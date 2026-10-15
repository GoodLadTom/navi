//! "Hey Navi": the microphone stays open and Navi listens for its name, so you can start talking
//! without touching the keyboard (Settings > Hey Navi). Off by default.
//!
//! * Its own microphone stream (WASAPI shared mode, alongside anything else using the mic) feeds a
//!   few seconds of 16 kHz audio into a ring.
//! * A simple voice detector (loudness over a slowly adapting noise floor) spots where each
//!   utterance starts. Its first moments (up to 1.8 s, less if it ends sooner) go to the speech
//!   engine, and if the words open with the wake phrase ("Hey Navi", any spelling, or "Navi" with a
//!   pause after it when Settings choose the name alone, see voice::is_wake_phrase) a hands-free
//!   take starts straight away with the audio from the start of
//!   the utterance included, so "Hey Navi, email Sam about Friday" keeps every word. The take ends
//!   by itself when you stop talking (controller::start_from_wake).
//! * Paused while a take is running, while Navi is taking call notes (the call's own transcript
//!   hears "Hey Navi, ..." as a job instead, see jobs.rs), while the shortcut is paused from the
//!   tray, and during a microphone test.
//! * A microphone that won't open (the one named in Settings unplugged, say) is never swapped for
//!   another. The bubble says why once, and it is tried again every 30 seconds, or at once when
//!   Settings name another.
//! * Each check is one short transcription on this computer. Nothing is kept or sent anywhere, and
//!   what was heard is never logged.

use crate::audio::{self, To16k};
use crate::engine::TranscribeOptions;
use crate::state::{App, Phase};
use crate::text::voice;
use crate::types::PillState;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const RATE: usize = audio::SAMPLE_RATE as usize;
/// 20 ms frames for the voice detector.
const FRAME: usize = RATE / 50;
/// Audio kept in the ring.
const RING_SECS: usize = 10;
/// Speech frames in a row that start an utterance, and quiet frames that end one.
const START_FRAMES: usize = 3;
const END_FRAMES: usize = 30;
/// Audio kept from before an utterance starts, so its first sound is whole.
const LEAD_IN: usize = RATE / 5;
/// How much of an utterance is checked for the wake phrase.
const CHECK_LEN: usize = RATE * 18 / 10;
/// Shorter utterances than this aren't worth checking.
const MIN_CHECK: usize = RATE * 4 / 10;
/// Louder than the noise floor by this much counts as speech, and never below the absolute floor.
const OVER_FLOOR_DB: f32 = 10.0;
const MIN_SPEECH_DB: f32 = -52.0;
const TICK: Duration = Duration::from_millis(60);

/// 16 kHz audio with the absolute index of its first sample.
struct Ring {
    samples: Vec<f32>,
    first: u64,
}

impl Ring {
    fn end(&self) -> u64 {
        self.first + self.samples.len() as u64
    }

    fn push(&mut self, new: &[f32]) {
        self.samples.extend_from_slice(new);
        let max = RATE * RING_SECS;
        if self.samples.len() > max {
            let drop = self.samples.len() - max;
            self.samples.drain(..drop);
            self.first += drop as u64;
        }
    }

    /// Audio from absolute index `from` (clamped to what is still held) to the end.
    fn since(&self, from: u64) -> Vec<f32> {
        let start = from.saturating_sub(self.first).min(self.samples.len() as u64) as usize;
        self.samples[start..].to_vec()
    }

    fn range(&self, from: u64, len: usize) -> Vec<f32> {
        let start = from.saturating_sub(self.first).min(self.samples.len() as u64) as usize;
        let end = (start + len).min(self.samples.len());
        self.samples[start..end].to_vec()
    }

    fn clear(&mut self) {
        self.first = self.end();
        self.samples.clear();
    }
}

/// The voice detector: where the current utterance started, if one is under way.
#[derive(Default)]
struct Detector {
    floor_db: Option<f32>,
    loud_run: usize,
    quiet_run: usize,
    utterance: Option<Utterance>,
    /// Absolute index of the next frame to look at.
    next: u64,
}

#[derive(Clone, Copy)]
struct Utterance {
    start: u64,
    checked: bool,
}

/// What the detector wants done after looking at new audio.
#[derive(Debug, PartialEq)]
enum Check {
    /// Check `len` samples from `start`; `ended`: the utterance was over before the check.
    Now { start: u64, len: usize, ended: bool },
}

impl Detector {
    /// Looks at every whole frame not yet seen. Returns the check to make, if one is due.
    fn feed(&mut self, ring: &Ring) -> Option<Check> {
        let mut due = None;
        if self.next < ring.first {
            self.next = ring.first;
        }
        while self.next + FRAME as u64 <= ring.end() {
            let at = (self.next - ring.first) as usize;
            let frame = &ring.samples[at..at + FRAME];
            let rms = (frame.iter().map(|x| x * x).sum::<f32>() / FRAME as f32).sqrt();
            let db = if rms > 1e-7 { 20.0 * rms.log10() } else { -140.0 };
            let floor = self.floor_db.get_or_insert(db);
            // Falls fast to the quiet between words; rises slowly (about 1 dB a second).
            *floor = if db < *floor { db } else { *floor + 0.02 };
            let loud = db > (*floor + OVER_FLOOR_DB).max(MIN_SPEECH_DB);
            if loud {
                self.loud_run += 1;
                self.quiet_run = 0;
            } else {
                self.quiet_run += 1;
                self.loud_run = 0;
            }
            match self.utterance.as_mut() {
                None if self.loud_run >= START_FRAMES => {
                    let start = (self.next + FRAME as u64).saturating_sub((START_FRAMES * FRAME + LEAD_IN) as u64).max(ring.first);
                    self.utterance = Some(Utterance { start, checked: false });
                }
                Some(u) => {
                    let len = (self.next + FRAME as u64 - u.start) as usize;
                    let ended = self.quiet_run >= END_FRAMES;
                    if !u.checked && (len >= CHECK_LEN || (ended && len >= MIN_CHECK + END_FRAMES * FRAME)) {
                        u.checked = true;
                        let len = if ended { len - (END_FRAMES * FRAME) / 2 } else { len };
                        due = Some(Check::Now { start: u.start, len: len.min(CHECK_LEN), ended });
                    }
                    if ended {
                        self.utterance = None;
                    }
                }
                None => {}
            }
            self.next += FRAME as u64;
        }
        due
    }

    fn reset(&mut self) {
        let floor = self.floor_db;
        *self = Detector { floor_db: floor, ..Default::default() };
    }
}

/// Starts the listener thread. It opens and closes the microphone as Settings and Navi's state
/// allow.
pub fn spawn(app: Arc<App>) {
    let spawned = std::thread::Builder::new().name("navi-wake".into()).spawn(move || listen(app));
    if let Err(e) = spawned {
        log::warn!("couldn't start listening for Hey Navi: {e}");
    }
}

/// True when the listener should have the microphone open.
fn wanted(app: &App) -> bool {
    // The settings lock is let go before the meeting lock is taken.
    let enabled = app.settings.read().hey_navi.enabled;
    enabled && !app.paused.load(Ordering::SeqCst) && !app.quitting.load(Ordering::SeqCst) && app.meeting.lock().is_none()
}

/// True while audio heard now must not wake Navi (a take, or a microphone test, is running).
fn busy(app: &App) -> bool {
    !matches!(app.session.lock().phase, Phase::Idle) || app.mic_test.load(Ordering::SeqCst) || app.audio.is_recording()
}

fn listen(app: Arc<App>) {
    let ring = Arc::new(Mutex::new(Ring { samples: Vec::new(), first: 0 }));
    let checking = Arc::new(AtomicBool::new(false));
    let mut mic: Option<Mic> = None;
    // The microphone the stream was last opened (or tried) on, so a change in Settings reopens
    // it, straight away even while waiting to retry one that wouldn't open.
    let mut opened_on: Option<String> = None;
    let mut detector = Detector::default();
    let mut retry_at = Instant::now();
    // The last reason the microphone wouldn't open, so a mic left unplugged is logged and shown
    // once, not every 30 seconds.
    let mut open_error: Option<String> = None;
    loop {
        std::thread::sleep(TICK);
        if app.quitting.load(Ordering::SeqCst) {
            break;
        }
        if !wanted(&app) {
            if mic.take().is_some() {
                log::info!("stopped listening for Hey Navi");
            }
            ring.lock().clear();
            detector.reset();
            // Back on (switched on again, or a call or pause over): try the mic at once, and say
            // again if it still won't open.
            retry_at = Instant::now();
            open_error = None;
            continue;
        }
        let device = app.settings.read().mic.device.clone();
        if mic.is_some() && opened_on != device {
            log::info!("the microphone changed in Settings; listening for Hey Navi on the new one");
            mic = None;
            ring.lock().clear();
            detector.reset();
        }
        if mic.is_none() {
            if opened_on != device {
                retry_at = Instant::now();
            }
            if Instant::now() < retry_at {
                continue;
            }
            let opened = Mic::open(device.as_deref());
            opened_on = device.clone();
            match opened {
                Ok(m) => {
                    log::info!("listening for Hey Navi");
                    mic = Some(m);
                    open_error = None;
                }
                Err(e) => {
                    if open_error.as_deref() != Some(e.as_str()) {
                        log::warn!("couldn't open the microphone for Hey Navi: {e}");
                        tell(&app, open_notice(device.as_deref(), &e));
                    }
                    open_error = Some(e);
                    retry_at = Instant::now() + Duration::from_secs(30);
                    continue;
                }
            }
        }
        let Some(m) = mic.as_mut() else { continue };
        let fresh = m.take_16k();
        if m.failed() {
            log::warn!("the microphone for Hey Navi stopped; opening it again shortly");
            mic = None;
            retry_at = Instant::now() + Duration::from_secs(3);
            continue;
        }
        // Read before the ring is locked, so the ring is never held while the session lock is taken.
        let (busy_now, checking_now) = (busy(&app), checking.load(Ordering::SeqCst));
        let due = on_tick(&mut ring.lock(), &mut detector, &fresh, busy_now, checking_now);
        if let Some(Check::Now { start, len, ended }) = due {
            log::debug!("checking {} ms of speech for Hey Navi{}", len * 1000 / RATE, if ended { " (a short phrase)" } else { "" });
            // One check at a time: a second utterance while one is being checked is skipped.
            if checking.swap(true, Ordering::SeqCst) {
                continue;
            }
            let audio = ring.lock().range(start, len);
            let (app2, ring2, checking2) = (app.clone(), ring.clone(), checking.clone());
            tauri::async_runtime::spawn(async move {
                check(&app2, audio, start, &ring2).await;
                checking2.store(false, Ordering::SeqCst);
            });
        }
    }
}

/// One listener tick: new audio goes into the ring and the detector, unless Navi is busy. A busy
/// tick drops the new audio and empties the ring, so nothing said during a take can wake Navi
/// afterwards, except while a check is starting a take: what the ring heard as the take's
/// microphone opened is still to be joined onto it (check() takes it), so it is kept as it is.
fn on_tick(ring: &mut Ring, detector: &mut Detector, fresh: &[f32], busy: bool, checking: bool) -> Option<Check> {
    if busy {
        if !checking {
            ring.clear();
        }
        detector.reset();
        return None;
    }
    ring.push(fresh);
    detector.feed(ring)
}

/// Shows a short note in the bubble, unless a take or a microphone test is using it.
fn tell(app: &Arc<App>, message: String) {
    if busy(app) {
        return;
    }
    app.show_pill_window(crate::context::foreground_hwnd());
    let gen = app.pill(PillState::Notice { message });
    app.hide_pill_later(gen, 4000);
}

/// What the bubble says when the listener can't open the microphone.
fn open_notice(device: Option<&str>, error: &str) -> String {
    match device.map(str::trim).filter(|n| !n.is_empty()) {
        // Listening starts by itself once it is plugged in.
        Some(name) if error.ends_with("is not connected") => format!("Hey Navi is waiting for the microphone '{name}'"),
        _ => format!("Hey Navi can't listen. {error}"),
    }
}

/// Transcribes the start of an utterance and wakes Navi if it opens with the wake phrase.
async fn check(app: &Arc<App>, audio: Vec<f32>, start: u64, ring: &Arc<Mutex<Ring>>) {
    let Some(engine) = app.engine.read().await.clone() else { return };
    let language = app.settings().engine.language;
    let heard = match engine.transcribe(&audio, &TranscribeOptions { language, prompt: String::new() }).await {
        Ok(t) => t.text,
        Err(e) => {
            log::debug!("Hey Navi check failed: {e:#}");
            return;
        }
    };
    let wake = app.settings().hey_navi;
    if !voice::is_wake_phrase(&heard, wake.name_only()) || busy(app) || !wanted(app) {
        return;
    }
    let (preroll, heard_to) = {
        let r = ring.lock();
        (r.since(start), r.end())
    };
    log::info!("heard {}", wake.label());
    match crate::controller::start_from_wake(app, preroll).await {
        Ok(()) => {
            let gap = ring.lock().since(heard_to);
            join_gap(app, gap);
        }
        Err(e) => {
            // The error sound has played; say why, or "Navi" just beeps.
            log::warn!("Hey Navi couldn't start a take: {e:#}");
            let message = e.to_string().lines().next().unwrap_or("Navi couldn't start listening").trim().to_string();
            app.show_pill_window(crate::context::foreground_hwnd());
            let gen = app.pill(PillState::Error { message });
            app.hide_pill_later(gen, 3000);
        }
    }
    ring.lock().clear();
}

/// Joins `gap`, what the listener heard while the take's own microphone was opening, onto the
/// take "Navi" has just started, between the preroll and the take's audio. Also skips the start
/// cue trim for it: you are mid-sentence when the cue plays, so that stretch is speech.
fn join_gap(app: &App, gap: Vec<f32>) {
    let mut s = app.session.lock();
    if mark_gap(&mut s.phase, gap.len()) && !gap.is_empty() {
        app.audio.prepend(&gap);
    }
}

/// Notes a joined gap of `gap_len` samples on the take, if it is a live "Hey Navi" take. False
/// (nothing to join) when it isn't: the shortcut won the microphone, or the take has already ended.
fn mark_gap(phase: &mut Phase, gap_len: usize) -> bool {
    let Phase::Recording(rec) = phase else { return false };
    if !rec.from_wake {
        return false;
    }
    rec.cue_ms = 0;
    rec.preroll_ms += (gap_len * 1000 / RATE) as u32;
    true
}

/// The listener's own microphone stream: mono at the device rate, converted to 16 kHz on demand.
struct Mic {
    _stream: cpal::Stream,
    raw: Arc<Mutex<Vec<f32>>>,
    failed: Arc<AtomicBool>,
    converter: To16k,
}

impl Mic {
    fn open(name: Option<&str>) -> Result<Self, String> {
        let host = cpal::default_host();
        // The same rule as a take (audio.rs): a named microphone that isn't there is not swapped
        // for the default, or "Navi" would be heard and then the take couldn't open its mic.
        let (device, label) = match name.map(str::trim).filter(|n| !n.is_empty()) {
            Some(n) => {
                let found = host.input_devices().ok().and_then(|mut it| it.find(|d| d.name().map(|x| x == n).unwrap_or(false)));
                (found.ok_or_else(|| format!("The microphone '{n}' is not connected"))?, n.to_string())
            }
            None => {
                let device = host.default_input_device().ok_or_else(|| audio::NO_MICROPHONE.to_string())?;
                let label = device.name().unwrap_or_else(|_| "default microphone".into());
                (device, label)
            }
        };
        // Worded as a take's would be: the bubble may show it.
        let readable = |e: &dyn std::fmt::Display, unplugged: bool| audio::describe_open_error(e, unplugged, &label).to_string();
        let supported = device
            .default_input_config()
            .map_err(|e| readable(&e, matches!(e, cpal::DefaultStreamConfigError::DeviceNotAvailable)))?;
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.config();
        let channels = (config.channels as usize).max(1);
        let raw = Arc::new(Mutex::new(Vec::new()));
        let failed = Arc::new(AtomicBool::new(false));
        let stream = match format {
            cpal::SampleFormat::F32 => build::<f32>(&device, &config, channels, raw.clone(), failed.clone()),
            cpal::SampleFormat::I16 => build::<i16>(&device, &config, channels, raw.clone(), failed.clone()),
            cpal::SampleFormat::I32 => build::<i32>(&device, &config, channels, raw.clone(), failed.clone()),
            cpal::SampleFormat::U16 => build::<u16>(&device, &config, channels, raw.clone(), failed.clone()),
            other => return Err(format!("The microphone '{label}' uses an audio format Navi cannot read ({other})")),
        }
        .map_err(|e| readable(&e, matches!(e, cpal::BuildStreamError::DeviceNotAvailable)))?;
        stream.play().map_err(|e| readable(&e, matches!(e, cpal::PlayStreamError::DeviceNotAvailable)))?;
        let converter = To16k::new(config.sample_rate.0).map_err(|e| e.to_string())?;
        Ok(Self { _stream: stream, raw, failed, converter })
    }

    fn take_16k(&mut self) -> Vec<f32> {
        let raw = std::mem::take(&mut *self.raw.lock());
        self.converter.push(&raw).unwrap_or_default()
    }

    fn failed(&self) -> bool {
        self.failed.load(Ordering::SeqCst)
    }
}

fn build<T>(device: &cpal::Device, config: &cpal::StreamConfig, channels: usize, raw: Arc<Mutex<Vec<f32>>>, failed: Arc<AtomicBool>) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let inv = 1.0 / channels as f32;
    device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            let mut out = raw.lock();
            // Ten seconds at most, should the listener stall.
            if out.len() > 48_000 * 10 {
                out.clear();
            }
            out.extend(data.chunks_exact(channels).map(|f| f.iter().map(|&s| s.to_sample::<f32>()).sum::<f32>() * inv));
        },
        move |err| {
            log::warn!("Hey Navi microphone: {err}");
            failed.store(true, Ordering::SeqCst);
        },
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(secs: f32, amp: f32) -> Vec<f32> {
        (0..(secs * RATE as f32) as usize).map(|i| (i as f32 * 0.07).sin() * amp).collect()
    }

    fn feed_all(d: &mut Detector, ring: &mut Ring, audio: &[f32]) -> Vec<Check> {
        let mut out = Vec::new();
        for chunk in audio.chunks(RATE / 20) {
            ring.push(chunk);
            if let Some(c) = d.feed(ring) {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn a_short_phrase_is_checked_once_it_ends() {
        let mut ring = Ring { samples: Vec::new(), first: 0 };
        let mut d = Detector::default();
        let mut audio = tone(1.0, 0.0005); // room noise
        audio.extend(tone(0.8, 0.2)); // "Hey Navi"
        audio.extend(tone(1.0, 0.0005));
        let checks = feed_all(&mut d, &mut ring, &audio);
        assert_eq!(checks.len(), 1, "{checks:?}");
        let Check::Now { start, len, ended } = checks[0];
        assert!(ended);
        // Starts just before the speech, with a little lead-in.
        let speech_at = RATE as u64;
        assert!(start <= speech_at && start + (LEAD_IN as u64) * 2 >= speech_at, "start {start}");
        assert!(len >= RATE * 7 / 10 && len <= CHECK_LEN, "len {len}");
    }

    #[test]
    fn a_long_utterance_is_checked_after_its_first_moments() {
        let mut ring = Ring { samples: Vec::new(), first: 0 };
        let mut d = Detector::default();
        let mut audio = tone(0.5, 0.0005);
        audio.extend(tone(4.0, 0.2));
        let checks = feed_all(&mut d, &mut ring, &audio);
        assert_eq!(checks.len(), 1, "checked once, not again as it goes on");
        let Check::Now { len, ended, .. } = checks[0];
        assert_eq!((len, ended), (CHECK_LEN, false));
    }

    #[test]
    fn quiet_and_clicks_are_ignored() {
        let mut ring = Ring { samples: Vec::new(), first: 0 };
        let mut d = Detector::default();
        let mut audio = tone(2.0, 0.0005);
        // A 30 ms click.
        audio.extend(tone(0.03, 0.5));
        audio.extend(tone(2.0, 0.0005));
        assert!(feed_all(&mut d, &mut ring, &audio).is_empty());
    }

    #[test]
    fn the_gap_is_only_what_came_after_the_preroll() {
        let mut ring = Ring { samples: Vec::new(), first: 0 };
        let mut d = Detector::default();
        on_tick(&mut ring, &mut d, &vec![0.1; RATE], false, false);
        // check() found the wake phrase and takes the preroll; start_from_wake opens the take's mic.
        let (preroll, heard_to) = (ring.since(0), ring.end());
        on_tick(&mut ring, &mut d, &vec![0.2; RATE / 10], false, true); // heard while it opened
        // The take's microphone is open (busy) and the check hasn't finished: the ring is held,
        // and what the listener hears now is the take's own audio, so it isn't added.
        assert_eq!(on_tick(&mut ring, &mut d, &vec![0.3; RATE / 10], true, true), None);
        let gap = ring.since(heard_to);
        assert_eq!((preroll.len(), gap.len()), (RATE, RATE / 10));
        assert!(gap.iter().all(|v| *v == 0.2), "none of the preroll, nor the take's audio, is repeated");

        // Busy with no check under way (a take from the shortcut): the ring is emptied.
        on_tick(&mut ring, &mut d, &vec![0.3; RATE / 10], true, false);
        assert!(ring.samples.is_empty() && ring.since(heard_to).is_empty());
        assert_eq!(ring.end(), heard_to + (RATE / 10) as u64, "positions still count on");
    }

    #[test]
    fn a_busy_tick_forgets_the_utterance_under_way() {
        let mut ring = Ring { samples: Vec::new(), first: 0 };
        let mut d = Detector::default();
        let mut audio = tone(1.0, 0.0005);
        audio.extend(tone(0.3, 0.2)); // speech starts...
        for chunk in audio.chunks(RATE / 20) {
            on_tick(&mut ring, &mut d, chunk, false, false);
        }
        assert!(d.utterance.is_some());
        on_tick(&mut ring, &mut d, &tone(0.05, 0.2), true, true); // ...and a take starts
        assert!(d.utterance.is_none(), "speech from before the take is never checked afterwards");
    }

    fn recording(from_wake: bool) -> crate::state::Recording {
        let now = Instant::now();
        crate::state::Recording {
            kind: crate::state::Kind::HandsFree,
            seq: 1,
            pressed: now,
            started: now,
            ctx: Default::default(),
            mode: crate::text::modes::builtin_modes().remove(0),
            announced: true,
            cue_at_ms: 0,
            cue_ms: 180,
            cue_word: None,
            pending_tap: None,
            ramble_cue: None,
            ramble_started: None,
            ocr: None,
            from_wake,
            preroll_ms: 1_000,
        }
    }

    #[test]
    fn the_gap_joins_only_a_live_wake_take() {
        let mut phase = Phase::Recording(recording(true));
        assert!(mark_gap(&mut phase, RATE / 10));
        let Phase::Recording(rec) = &phase else { unreachable!() };
        assert_eq!((rec.cue_ms, rec.preroll_ms), (0, 1_100), "no cue trim; the gap counts as preroll");

        let mut phase = Phase::Recording(recording(false));
        assert!(!mark_gap(&mut phase, RATE / 10), "the shortcut won the microphone");
        let Phase::Recording(rec) = &phase else { unreachable!() };
        assert_eq!((rec.cue_ms, rec.preroll_ms), (180, 1_000), "a shortcut take is left alone");

        assert!(!mark_gap(&mut Phase::Idle, RATE / 10), "the take has already ended");
        assert!(!mark_gap(&mut Phase::Processing, RATE / 10));
    }

    #[test]
    fn a_missing_named_microphone_is_not_swapped_for_the_default() {
        let err = Mic::open(Some("Navi test: no such microphone")).err().expect("must not open another mic");
        assert!(err.contains("is not connected"), "{err}");
        assert_eq!(open_notice(Some("Navi test: no such microphone"), &err), "Hey Navi is waiting for the microphone 'Navi test: no such microphone'");
    }

    #[test]
    fn says_why_hey_navi_cant_listen() {
        assert_eq!(open_notice(Some(" USB Mic "), "The microphone 'USB Mic' is not connected"), "Hey Navi is waiting for the microphone 'USB Mic'");
        assert_eq!(open_notice(None, audio::NO_MICROPHONE), "Hey Navi can't listen. No microphone found");
        let blocked = audio::describe_open_error(&"Access is denied. (0x80070005)", false, "USB Mic").to_string();
        assert_eq!(open_notice(Some("USB Mic"), &blocked), format!("Hey Navi can't listen. {blocked}"));
    }

    #[test]
    fn the_ring_keeps_ten_seconds_and_finds_old_audio() {
        let mut ring = Ring { samples: Vec::new(), first: 0 };
        ring.push(&vec![0.1; RATE * 12]);
        assert_eq!(ring.samples.len(), RATE * RING_SECS);
        assert_eq!(ring.first, (RATE * 2) as u64);
        assert_eq!(ring.since(0).len(), RATE * RING_SECS, "asking for audio that has gone gives what is left");
        assert_eq!(ring.range((RATE * 11) as u64, RATE * 5).len(), RATE);
        ring.clear();
        assert!(ring.samples.is_empty());
        assert_eq!(ring.end(), (RATE * 12) as u64);
    }
}
