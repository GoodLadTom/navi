//! Sound cues. Low latency matters: the start cue must sound within ~30 ms of the key press.
//!
//! OWNER: audio and engine agent. STATUS: implemented.
//!
//! Contract:
//! * Sound sets live in Paths::sounds/sets/<set>/<cue>.wav, listed with their names in
//!   sets/index.json ({"default": "bubble", "sets": [{"id", "name", "description"}]}). Every set has
//!   start, stop, done, ramble, cancel, error and learn (soft, low, calm sounds made by
//!   scripts/make_sounds.py).
//! * Cue -> file: Talk = start, Ramble = ramble, Stop = stop, Done = done, Cancel = cancel,
//!   Error = error, Learn = learn, from SoundSettings::set (the default set if that one is
//!   missing). A path in SoundSettings::custom for the cue name ("talk", "ramble", "stop", "done",
//!   "cancel", "error", "learn") overrides the built-in file.
//! * Decode every built-in file once at startup and keep the samples in memory; keep one output
//!   stream open (rodio) so playing never waits for device start-up. Apply SoundSettings::volume.
//!   `enabled == false` silences everything.
//! * `duration_ms` returns the cue length so the controller can trim speaker bleed from the start.
//!
//! Implementation notes:
//! * `play` never blocks on audio: it hands the decoded clip to the "navi-sounds" thread, which
//!   owns the rodio stream (not Send) and mixes the clip in on the next device period.
//! * Volume is applied as amplitude = volume², which tracks perceived loudness better than a
//!   straight line. A volume of 0 plays nothing and reports a duration of 0.
//! * Clips are kept mono and pre-resampled to the output device's rate (rubato, not rodio's
//!   linear interpolation). Custom cues are decoded on first use, cached until the file changes,
//!   and capped at 3 seconds so a long file can never eat the start of a dictation. Readings
//!   (`play_samples`) are resampled as they play and not kept.
//! * The output is reopened when the device fails or the Windows default output changes
//!   (checked every 3 seconds while idle, and before playing after a failure).

use crate::audio;
use crate::types::SoundSettings;
use anyhow::{anyhow, bail, Result};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// Used when the index is missing or does not name a default.
const FALLBACK_SET: &str = "bubble";
const MAX_CUSTOM_MS: u64 = 3_000;
const FADE_MS: u64 = 10;
const HOUSEKEEPING_INTERVAL: Duration = Duration::from_secs(3);

static NEXT_CLIP_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cue {
    Talk,
    Ramble,
    Stop,
    Done,
    Cancel,
    Error,
    Learn,
}

impl Cue {
    pub const ALL: [Cue; 7] = [Cue::Talk, Cue::Ramble, Cue::Stop, Cue::Done, Cue::Cancel, Cue::Error, Cue::Learn];

    pub fn key(&self) -> &'static str {
        match self {
            Cue::Talk => "talk",
            Cue::Ramble => "ramble",
            Cue::Stop => "stop",
            Cue::Done => "done",
            Cue::Cancel => "cancel",
            Cue::Error => "error",
            Cue::Learn => "learn",
        }
    }

    pub fn from_key(key: &str) -> Option<Cue> {
        Some(match key {
            "talk" => Cue::Talk,
            "ramble" => Cue::Ramble,
            "stop" => Cue::Stop,
            "done" => Cue::Done,
            "cancel" => Cue::Cancel,
            "error" => Cue::Error,
            "learn" => Cue::Learn,
            _ => return None,
        })
    }

    /// The cue's file name inside a set.
    pub fn file(&self) -> &'static str {
        match self {
            Cue::Talk => "start",
            Cue::Ramble => "ramble",
            Cue::Stop => "stop",
            Cue::Done => "done",
            Cue::Cancel => "cancel",
            Cue::Error => "error",
            Cue::Learn => "learn",
        }
    }
}

/// One sound set, for the picker.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SoundSet {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(serde::Deserialize, Default)]
struct SetIndex {
    #[serde(default)]
    default: String,
    #[serde(default)]
    sets: Vec<SoundSet>,
}

/// A decoded mono cue.
struct Clip {
    id: u64,
    samples: Arc<[f32]>,
    rate: u32,
    /// Played again and again (a cue), so worth keeping at the device rate. A reading is played
    /// once and never asked for again.
    reused: bool,
}

impl Clip {
    fn new(samples: Vec<f32>, rate: u32) -> Self {
        Self { id: NEXT_CLIP_ID.fetch_add(1, Ordering::Relaxed), samples: samples.into(), rate, reused: true }
    }

    fn one_off(samples: Vec<f32>, rate: u32) -> Self {
        Self { reused: false, ..Self::new(samples, rate) }
    }

    fn duration_ms(&self) -> u32 {
        (self.samples.len() as u64 * 1000 / self.rate.max(1) as u64) as u32
    }
}

struct CustomEntry {
    modified: Option<SystemTime>,
    len: u64,
    /// None when the file could not be decoded; retried once the file changes.
    clip: Option<Arc<Clip>>,
}

enum PlayerCommand {
    Play { clip: Arc<Clip>, gain: f32 },
    Shutdown,
}

pub struct SoundService {
    dir: PathBuf,
    /// "<set>/<cue file>" -> clip.
    builtin: Arc<HashMap<String, Arc<Clip>>>,
    sets: Vec<SoundSet>,
    default_set: String,
    custom: Mutex<HashMap<PathBuf, CustomEntry>>,
    player: Option<mpsc::Sender<PlayerCommand>>,
}

impl SoundService {
    pub fn new(dir: PathBuf) -> Self {
        Self::build(dir, true)
    }

    /// Decodes the cues without opening an audio output (tests, or a machine with no speakers).
    pub fn without_output(dir: PathBuf) -> Self {
        Self::build(dir, false)
    }

    fn build(dir: PathBuf, with_output: bool) -> Self {
        let sets_dir = dir.join("sets");
        let index: SetIndex = std::fs::read_to_string(sets_dir.join("index.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_else(|| {
                log::warn!("sound sets index missing in {}", sets_dir.display());
                SetIndex::default()
            });
        let mut builtin = HashMap::new();
        let mut sets = Vec::new();
        for set in index.sets {
            let mut complete = true;
            for cue in Cue::ALL {
                let path = sets_dir.join(&set.id).join(format!("{}.wav", cue.file()));
                match load_clip(&path, None) {
                    Ok(clip) => {
                        builtin.insert(format!("{}/{}", set.id, cue.file()), Arc::new(clip));
                    }
                    Err(e) => {
                        complete = false;
                        log::warn!("sound cue {} could not be loaded: {e:#}", path.display());
                    }
                }
            }
            if complete {
                sets.push(set);
            }
        }
        let default_set = if sets.iter().any(|s| s.id == index.default) {
            index.default
        } else {
            sets.first().map(|s| s.id.clone()).unwrap_or_else(|| FALLBACK_SET.to_string())
        };
        let builtin = Arc::new(builtin);
        let player = if with_output { spawn_player(builtin.clone()) } else { None };
        Self { dir, builtin, sets, default_set, custom: Mutex::new(HashMap::new()), player }
    }

    /// The sound sets that loaded completely, in the index's order.
    pub fn sets(&self) -> &[SoundSet] {
        &self.sets
    }

    pub fn default_set(&self) -> &str {
        &self.default_set
    }

    pub fn play(&self, cue: Cue, settings: &SoundSettings) {
        let gain = gain_for(settings);
        if gain <= 0.0 {
            return;
        }
        let Some(clip) = self.resolve(cue, settings) else { return };
        // On a Mac, "While you talk" mutes the whole output, cues included: it lets this one through.
        #[cfg(target_os = "macos")]
        crate::media::cue_playing(clip.duration_ms());
        if let Some(player) = &self.player {
            if player.send(PlayerCommand::Play { clip, gain }).is_err() {
                log::warn!("the sound player has stopped; cue '{}' not played", cue.key());
            }
        }
    }

    /// Plays any mono audio (an ElevenLabs reading, say) at full volume, mixed with the cues.
    pub fn play_samples(&self, samples: Vec<f32>, rate: u32) {
        if samples.is_empty() || rate == 0 {
            return;
        }
        if let Some(player) = &self.player {
            let clip = Arc::new(Clip::one_off(samples, rate));
            if player.send(PlayerCommand::Play { clip, gain: 1.0 }).is_err() {
                log::warn!("the sound player has stopped; the reading was not played");
            }
        }
    }

    pub fn duration_ms(&self, cue: Cue, settings: &SoundSettings) -> u32 {
        if gain_for(settings) <= 0.0 {
            return 0;
        }
        self.resolve(cue, settings).map(|clip| clip.duration_ms()).unwrap_or(0)
    }

    /// Decodes any custom cues in `settings` now, so the first press after a settings change
    /// does not wait for a file read. Optional.
    pub fn preload(&self, settings: &SoundSettings) {
        for cue in Cue::ALL {
            let _ = self.resolve(cue, settings);
        }
    }

    /// The folder the built-in cues were loaded from.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn resolve(&self, cue: Cue, settings: &SoundSettings) -> Option<Arc<Clip>> {
        if let Some(path) = settings.custom.get(cue.key()).map(|p| p.trim()).filter(|p| !p.is_empty()) {
            if let Some(clip) = self.custom_clip(Path::new(path)) {
                return Some(clip);
            }
        }
        self.builtin_clip(cue, settings)
    }

    fn builtin_clip(&self, cue: Cue, settings: &SoundSettings) -> Option<Arc<Clip>> {
        let wanted = settings.set.trim();
        self.builtin
            .get(&format!("{wanted}/{}", cue.file()))
            .or_else(|| self.builtin.get(&format!("{}/{}", self.default_set, cue.file())))
            .cloned()
    }

    fn custom_clip(&self, path: &Path) -> Option<Arc<Clip>> {
        let meta = std::fs::metadata(path).ok()?;
        let modified = meta.modified().ok();
        let len = meta.len();
        let mut cache = self.custom.lock();
        if let Some(entry) = cache.get(path) {
            if entry.modified == modified && entry.len == len {
                return entry.clip.clone();
            }
        }
        let clip = match load_clip(path, Some(MAX_CUSTOM_MS)) {
            Ok(clip) => Some(Arc::new(clip)),
            Err(e) => {
                log::warn!("custom sound {} cannot be used, keeping the built-in cue: {e:#}", path.display());
                None
            }
        };
        cache.insert(path.to_path_buf(), CustomEntry { modified, len, clip: clip.clone() });
        clip
    }
}

impl Drop for SoundService {
    fn drop(&mut self) {
        if let Some(player) = &self.player {
            let _ = player.send(PlayerCommand::Shutdown);
        }
    }
}

fn gain_for(settings: &SoundSettings) -> f32 {
    if !settings.enabled || !settings.volume.is_finite() {
        return 0.0;
    }
    settings.volume.clamp(0.0, 1.0).powi(2)
}

fn load_clip(path: &Path, max_ms: Option<u64>) -> Result<Clip> {
    let (mut samples, rate) = audio::decode_wav_mono(path)?;
    if samples.is_empty() || rate == 0 {
        bail!("{} has no audio", path.display());
    }
    if let Some(max_ms) = max_ms {
        let max = (rate as u64 * max_ms / 1000) as usize;
        if samples.len() > max {
            samples.truncate(max);
            let fade = ((rate as u64 * FADE_MS / 1000) as usize).min(samples.len());
            let start = samples.len() - fade;
            for (i, s) in samples[start..].iter_mut().enumerate() {
                *s *= 1.0 - (i + 1) as f32 / fade as f32;
            }
        }
    }
    Ok(Clip::new(samples, rate))
}

// ---------------------------------------------------------------------------------------------
// Output thread
// ---------------------------------------------------------------------------------------------

fn spawn_player(builtin: Arc<HashMap<String, Arc<Clip>>>) -> Option<mpsc::Sender<PlayerCommand>> {
    let (tx, rx) = mpsc::channel();
    match std::thread::Builder::new().name("navi-sounds".into()).spawn(move || player_thread(rx, builtin)) {
        Ok(_) => Some(tx),
        Err(e) => {
            log::error!("could not start the sound thread: {e}");
            None
        }
    }
}

fn player_thread(rx: mpsc::Receiver<PlayerCommand>, builtin: Arc<HashMap<String, Arc<Clip>>>) {
    let mut opener = Opener::default();
    let mut output = opener.open(&builtin);
    loop {
        match rx.recv_timeout(HOUSEKEEPING_INTERVAL) {
            Ok(PlayerCommand::Play { clip, gain }) => {
                if output.as_ref().is_none_or(Output::is_broken) {
                    drop(output.take()); // release the old device before opening the new one
                    output = opener.open(&builtin);
                }
                if let Some(out) = output.as_mut() {
                    out.play(&clip, gain);
                }
            }
            Ok(PlayerCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {
                let stale = match &output {
                    None => true,
                    Some(out) => out.is_broken() || out.default_device_changed(),
                };
                if stale {
                    drop(output.take());
                    output = opener.open(&builtin);
                }
            }
        }
    }
}

/// Opens outputs and logs failures once per run of failures rather than every few seconds.
#[derive(Default)]
struct Opener {
    failing: bool,
}

impl Opener {
    fn open(&mut self, builtin: &HashMap<String, Arc<Clip>>) -> Option<Output> {
        match Output::open(builtin) {
            Ok(out) => {
                if self.failing {
                    log::info!("sound output available again");
                }
                self.failing = false;
                Some(out)
            }
            Err(e) => {
                if !self.failing {
                    log::warn!("no sound output: {e:#}");
                }
                self.failing = true;
                None
            }
        }
    }
}

struct Output {
    stream: rodio::OutputStream,
    device: Option<String>,
    broken: Arc<AtomicBool>,
    clips: DeviceClips,
}

/// Clips converted to the output device's rate. Only cues are kept; a reading is converted as it
/// plays and dropped, or every reading would stay in memory until Navi quits.
struct DeviceClips {
    rate: u32,
    /// Clip id -> samples at `rate`.
    converted: HashMap<u64, Arc<[f32]>>,
}

impl DeviceClips {
    fn new(rate: u32) -> Self {
        Self { rate, converted: HashMap::new() }
    }

    /// Samples ready for the mixer and their rate.
    fn prepare(&mut self, clip: &Clip) -> (Arc<[f32]>, u32) {
        if clip.rate == self.rate {
            return (clip.samples.clone(), clip.rate);
        }
        if let Some(samples) = self.converted.get(&clip.id) {
            return (samples.clone(), self.rate);
        }
        match audio::resample(&clip.samples, clip.rate, self.rate) {
            Ok(samples) => {
                let samples: Arc<[f32]> = samples.into();
                if clip.reused {
                    if self.converted.len() >= 64 {
                        self.converted.clear();
                    }
                    self.converted.insert(clip.id, samples.clone());
                }
                (samples, self.rate)
            }
            Err(e) => {
                log::warn!("could not resample a sound cue, letting rodio convert it: {e:#}");
                (clip.samples.clone(), clip.rate)
            }
        }
    }
}

impl Output {
    fn open(builtin: &HashMap<String, Arc<Clip>>) -> Result<Self> {
        let broken = Arc::new(AtomicBool::new(false));
        let flag = broken.clone();
        let builder = rodio::OutputStreamBuilder::from_default_device()
            .map_err(|e| anyhow!("{e}"))?
            .with_error_callback(move |err: cpal::StreamError| {
                log::warn!("sound output error: {err}");
                flag.store(true, Ordering::SeqCst);
            });
        let mut stream = builder.open_stream_or_fallback().map_err(|e| anyhow!("{e}"))?;
        stream.log_on_drop(false);
        let rate = stream.config().sample_rate();
        let mut out = Self { stream, device: default_output_name(), broken, clips: DeviceClips::new(rate) };
        for clip in builtin.values() {
            out.clips.prepare(clip);
        }
        log::debug!("sound output open on {:?} at {rate} Hz", out.device);
        Ok(out)
    }

    fn is_broken(&self) -> bool {
        self.broken.load(Ordering::SeqCst)
    }

    fn default_device_changed(&self) -> bool {
        let now = default_output_name();
        now.is_some() && now != self.device
    }

    fn play(&mut self, clip: &Clip, gain: f32) {
        let (data, rate) = self.clips.prepare(clip);
        self.stream.mixer().add(ClipSource { data, pos: 0, rate, gain });
    }
}

fn default_output_name() -> Option<String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    cpal::default_host().default_output_device().and_then(|d| d.name().ok())
}

/// A mono clip played straight from shared memory, with the volume applied per sample.
struct ClipSource {
    data: Arc<[f32]>,
    pos: usize,
    rate: u32,
    gain: f32,
}

impl Iterator for ClipSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        let sample = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(sample * self.gain)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.data.len().saturating_sub(self.pos);
        (left, Some(left))
    }
}

impl rodio::Source for ClipSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> rodio::ChannelCount {
        1
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        self.rate
    }

    fn total_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs_f64(self.data.len() as f64 / self.rate.max(1) as f64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Paths;

    fn service() -> SoundService {
        SoundService::without_output(Paths::for_tests("sounds").sounds)
    }

    fn clip_ms(s: &SoundService, set: &str, cue: Cue) -> u32 {
        s.builtin.get(&format!("{set}/{}", cue.file())).unwrap_or_else(|| panic!("{set}/{} missing", cue.file())).duration_ms()
    }

    #[test]
    fn decodes_every_set_completely() {
        let s = service();
        assert!(s.sets().len() >= 10, "loads of options: {}", s.sets().len());
        assert_eq!(s.default_set(), "bubble");
        assert_eq!(s.sets()[0].id, "bubble", "the default is listed first");
        assert!(!s.sets().iter().any(|set| set.id == "bowl"), "Bowl was taken out");
        for set in s.sets() {
            assert!(!set.name.is_empty() && !set.description.is_empty(), "{set:?}");
            for cue in Cue::ALL {
                let clip = s.builtin.get(&format!("{}/{}", set.id, cue.file())).unwrap_or_else(|| panic!("{}/{} missing", set.id, cue.file()));
                assert_eq!(clip.rate, 48_000, "{}/{}", set.id, cue.file());
                let ms = clip.duration_ms();
                assert!((40..1_500).contains(&ms), "{}/{}: {ms} ms", set.id, cue.file());
                // Soft: never louder than -6 dBFS.
                let peak = clip.samples.iter().fold(0.0f32, |m, v| m.max(v.abs()));
                assert!(peak < 0.5, "{}/{} peaks at {peak}", set.id, cue.file());
            }
        }
    }

    #[test]
    fn maps_cues_to_the_chosen_set() {
        let s = service();
        let mut settings = SoundSettings::default();
        assert_eq!(settings.set, "bubble");
        assert_eq!(s.duration_ms(Cue::Talk, &settings), clip_ms(&s, "bubble", Cue::Talk));
        assert_eq!(s.duration_ms(Cue::Ramble, &settings), clip_ms(&s, "bubble", Cue::Ramble));
        settings.set = "pebble".into();
        assert_eq!(s.duration_ms(Cue::Talk, &settings), clip_ms(&s, "pebble", Cue::Talk));
        assert_eq!(s.duration_ms(Cue::Error, &settings), clip_ms(&s, "pebble", Cue::Error));
        settings.set = "fern".into(); // the retired voices
        assert_eq!(s.duration_ms(Cue::Talk, &settings), clip_ms(&s, "bubble", Cue::Talk), "an unknown set falls back to the default");
        settings.set = "bowl".into(); // taken out on 21 Oct 2026
        assert_eq!(s.duration_ms(Cue::Talk, &settings), clip_ms(&s, "bubble", Cue::Talk), "a removed set falls back to the default");
        settings.volume = 0.0;
        assert_eq!(s.duration_ms(Cue::Talk, &settings), 0, "silent cue needs no trimming");
        settings.volume = 0.7;
        settings.enabled = false;
        assert_eq!(s.duration_ms(Cue::Talk, &settings), 0);
        for cue in Cue::ALL {
            assert_eq!(Cue::from_key(cue.key()), Some(cue));
        }
    }

    #[test]
    fn custom_cues_override_and_fall_back() {
        let s = service();
        let dir = std::env::temp_dir().join(format!("navi-sounds-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let mut settings = SoundSettings::default();

        // A 250 ms 44.1 kHz stereo file overrides "done".
        let custom = dir.join("ding.wav");
        let spec = hound::WavSpec { channels: 2, sample_rate: 44_100, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&custom, spec).expect("create");
        for i in 0..(44_100 / 4) {
            let v = ((i as f32 * 0.05).sin() * 10_000.0) as i16;
            w.write_sample(v).expect("write");
            w.write_sample(v).expect("write");
        }
        w.finalize().expect("finalize");
        settings.custom.insert("done".into(), custom.to_string_lossy().into_owned());
        assert_eq!(s.duration_ms(Cue::Done, &settings), 250);

        // A long file is capped at 3 seconds.
        let long = dir.join("long.wav");
        let spec = hound::WavSpec { channels: 1, sample_rate: 16_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&long, spec).expect("create");
        for _ in 0..16_000 * 10 {
            w.write_sample(1000i16).expect("write");
        }
        w.finalize().expect("finalize");
        settings.custom.insert("talk".into(), long.to_string_lossy().into_owned());
        assert_eq!(s.duration_ms(Cue::Talk, &settings), 3_000);

        // Missing or broken files fall back to the built-in cue.
        let builtin_talk = clip_ms(&s, "bubble", Cue::Talk);
        settings.custom.insert("talk".into(), dir.join("missing.wav").to_string_lossy().into_owned());
        assert_eq!(s.duration_ms(Cue::Talk, &settings), builtin_talk);
        let broken = dir.join("broken.wav");
        std::fs::write(&broken, b"not a wav file").expect("write");
        settings.custom.insert("talk".into(), broken.to_string_lossy().into_owned());
        assert_eq!(s.duration_ms(Cue::Talk, &settings), builtin_talk);
        settings.custom.insert("talk".into(), "   ".into());
        assert_eq!(s.duration_ms(Cue::Talk, &settings), builtin_talk);

        s.preload(&settings);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_cues_are_kept_at_the_device_rate() {
        let s = service();
        assert!(s.builtin.values().all(|c| c.reused), "built-in cues are kept at the device rate");
        let mut clips = DeviceClips::new(44_100);

        let reading = Clip::one_off(vec![0.1; 2_400], 24_000);
        assert_eq!(reading.duration_ms(), 100);
        let (samples, rate) = clips.prepare(&reading);
        assert_eq!((samples.len(), rate), (4_410, 44_100), "converted to play");
        assert!(clips.converted.is_empty(), "a reading is played once, then dropped");

        let cue = s.builtin.values().next().expect("a cue");
        let (first, _) = clips.prepare(cue);
        let (again, _) = clips.prepare(cue);
        assert_eq!(clips.converted.len(), 1);
        assert!(Arc::ptr_eq(&first, &again), "a cue is converted once and reused");

        let native = Clip::one_off(vec![0.1; 441], 44_100);
        let (same, _) = clips.prepare(&native);
        assert!(Arc::ptr_eq(&same, &native.samples), "nothing to convert at the device rate");
        assert_eq!(clips.converted.len(), 1);
    }

    #[test]
    fn clip_source_applies_gain_and_reports_length() {
        let src = ClipSource { data: vec![0.5f32; 4_800].into(), pos: 0, rate: 48_000, gain: 0.49 };
        assert_eq!(rodio::Source::total_duration(&src), Some(Duration::from_millis(100)));
        let samples: Vec<f32> = src.collect();
        assert_eq!(samples.len(), 4_800);
        assert!((samples[0] - 0.245).abs() < 1e-6);
    }
}
