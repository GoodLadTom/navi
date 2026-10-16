//! Microphone capture (cpal, WASAPI shared mode) and WAV helpers.
//!
//! OWNER: audio and engine agent. STATUS: implemented.
//!
//! Contract:
//! * cpal streams are not Send on every platform, so the stream lives on a dedicated audio thread
//!   that owns it; `AudioService` talks to that thread over channels.
//! * `start(device)` opens the named input device (None = default), any sample format, any rate,
//!   downmixes to mono f32 and appends to the take buffer. It should be ready in well under 150 ms.
//!   Every ~20 ms it sends a level (RMS mapped to 0..1 on a perceptual curve, e.g. dBFS -60..-10)
//!   on the level channel given to `new`.
//! * `start_meter(device)` opens the microphone the same way for a microphone test: levels are
//!   sent but no audio is kept, so a test left running doesn't fill memory.
//! * `stop()` ends the take and returns the audio resampled to 16 kHz mono (rubato, good quality).
//! * `snapshot_16k(from)` returns 16 kHz audio recorded so far starting at sample `from` (16 kHz
//!   index) without stopping, so long rambles can be transcribed in chunks while they happen.
//! * `cancel()` stops and discards. `is_recording()` is cheap.
//! * Errors are readable for the UI: "No microphone found", "Microphone access is off in Windows
//!   Settings > Privacy & security > Microphone", "The microphone 'X' is not connected".
//!
//! Implementation notes:
//! * The cpal callback only downmixes, removes DC offset, meters and appends to a raw buffer at the
//!   device rate. The audio thread folds that buffer into the 16 kHz take every 250 ms, so memory
//!   stays small on long rambles and `stop()` only has the last fraction of a second left to do.
//! * Resampling is rubato's FFT resampler with its delay compensated, so 16 kHz sample `n` is
//!   exactly `n / 16000` seconds into the take, and `snapshot_16k` output is a prefix of `stop()`.
//! * Extra public items: `last_error()`, `decode_wav_mono()`, `resample()`, `level_from_rms()`.

use crate::types::DeviceInfo;
use anyhow::{anyhow, bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use rubato::{FftFixedIn, Resampler};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

pub const SAMPLE_RATE: u32 = 16_000;

/// How often the audio thread folds raw device audio into the 16 kHz take while recording.
const DRAIN_INTERVAL: Duration = Duration::from_millis(250);
/// Level meter window.
const LEVEL_WINDOW_SECS: f64 = 0.02;
const LEVEL_FLOOR_DB: f32 = -60.0;
const LEVEL_CEIL_DB: f32 = -10.0;
/// Input frames per resampler call.
const RESAMPLER_CHUNK: usize = 1024;
/// A driver that takes longer than this to open is treated as not responding.
const START_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_TIMEOUT: Duration = Duration::from_secs(3);

pub(crate) const NO_MICROPHONE: &str = "No microphone found";
const PRIVACY_BLOCKED: &str = "Microphone access is off in Windows Settings > Privacy & security > Microphone";

pub struct Recording {
    /// 16 kHz mono, -1..1.
    pub samples: Vec<f32>,
    pub duration_ms: u64,
}

pub struct AudioService {
    commands: mpsc::Sender<Command>,
    shared: Arc<Shared>,
}

enum Command {
    /// `keep`: false for a microphone test, which only needs the level.
    Start { device: Option<String>, keep: bool, reply: mpsc::SyncSender<Result<()>> },
    Stop { reply: mpsc::SyncSender<()> },
    Shutdown,
}

/// State shared between the controller's thread, the audio thread and the cpal callback.
/// Lock order is always `conv` then `raw`; the callback only ever takes `raw`.
struct Shared {
    recording: AtomicBool,
    /// Mono samples at the device rate, not yet converted.
    raw: Mutex<Vec<f32>>,
    conv: Mutex<Converter>,
    /// Set when the stream fails during a take (for example the microphone was unplugged).
    error: Mutex<Option<String>>,
}

impl Shared {
    fn drain_into(&self, conv: &mut Converter) -> Result<()> {
        let mut batch = std::mem::take(&mut conv.spare);
        {
            let mut raw = self.raw.lock();
            std::mem::swap(&mut *raw, &mut batch);
        }
        let result = conv.push(&batch);
        batch.clear();
        conv.spare = batch;
        result
    }
}

impl AudioService {
    pub fn new(level_tx: UnboundedSender<f32>) -> Self {
        let shared = Arc::new(Shared {
            recording: AtomicBool::new(false),
            raw: Mutex::new(Vec::new()),
            conv: Mutex::new(Converter::passthrough(SAMPLE_RATE)),
            error: Mutex::new(None),
        });
        let (commands, rx) = mpsc::channel();
        let thread_shared = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("navi-audio".into())
            .spawn(move || capture_thread(rx, thread_shared, level_tx));
        if let Err(e) = spawned {
            log::error!("could not start the audio thread: {e}");
        }
        Self { commands, shared }
    }

    pub fn start(&self, device: Option<&str>) -> Result<()> {
        self.open(device, true)
    }

    /// Opens the microphone for a level meter only (a microphone test): levels are sent as for a
    /// take, but nothing is recorded, so a test left running uses no memory. `stop()` returns no
    /// audio; `cancel()` or `start()` ends it.
    pub fn start_meter(&self, device: Option<&str>) -> Result<()> {
        self.open(device, false)
    }

    fn open(&self, device: Option<&str>, keep: bool) -> Result<()> {
        let (reply, reply_rx) = mpsc::sync_channel(1);
        self.commands
            .send(Command::Start { device: device.map(str::to_owned), keep, reply })
            .map_err(|_| anyhow!("The audio system is not running. Restart Navi."))?;
        match reply_rx.recv_timeout(START_TIMEOUT) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => {
                // If the driver eventually opens, close it again so the take is not left running.
                let (stop_reply, _ignored) = mpsc::sync_channel(1);
                let _ = self.commands.send(Command::Stop { reply: stop_reply });
                Err(anyhow!("The microphone did not respond. Try reconnecting it or choose another one in Settings."))
            }
            Err(RecvTimeoutError::Disconnected) => Err(anyhow!("The audio system stopped unexpectedly. Restart Navi.")),
        }
    }

    pub fn stop(&self) -> Result<Recording> {
        if !self.shared.recording.load(Ordering::SeqCst) {
            bail!("Navi is not recording");
        }
        self.close_stream();
        let samples = {
            let mut conv = self.shared.conv.lock();
            self.shared.drain_into(&mut conv)?;
            conv.finish()?;
            std::mem::take(&mut conv.out)
        };
        if samples.is_empty() {
            if let Some(message) = self.last_error() {
                bail!(message);
            }
        }
        let duration_ms = samples.len() as u64 * 1000 / SAMPLE_RATE as u64;
        Ok(Recording { samples, duration_ms })
    }

    /// Puts audio recorded just before this take (by the "Hey Navi" listener, wake.rs) in front of
    /// it, so the words said while Navi was waking up are kept. Called again, the new audio goes
    /// after what was put in front before, so call it oldest first.
    pub fn prepend(&self, samples_16k: &[f32]) {
        self.shared.conv.lock().prepend(samples_16k);
    }

    pub fn snapshot_16k(&self, from: usize) -> Vec<f32> {
        let mut conv = self.shared.conv.lock();
        if let Err(e) = self.shared.drain_into(&mut conv) {
            log::warn!("audio snapshot: {e:#}");
        }
        conv.out.get(from..).map(<[f32]>::to_vec).unwrap_or_default()
    }

    pub fn cancel(&self) {
        if !self.shared.recording.load(Ordering::SeqCst) {
            return;
        }
        self.close_stream();
        let mut conv = self.shared.conv.lock();
        conv.clear();
        self.shared.raw.lock().clear();
    }

    pub fn is_recording(&self) -> bool {
        self.shared.recording.load(Ordering::Relaxed)
    }

    /// What went wrong with the microphone during the current or last take, if anything
    /// (for example "The microphone 'X' was disconnected"). `stop()` still returns the audio
    /// captured before the failure; show this alongside it.
    pub fn last_error(&self) -> Option<String> {
        self.shared.error.lock().clone()
    }

    fn close_stream(&self) {
        let (reply, reply_rx) = mpsc::sync_channel(1);
        if self.commands.send(Command::Stop { reply }).is_ok() && reply_rx.recv_timeout(STOP_TIMEOUT).is_err() {
            log::warn!("the microphone took too long to close");
        }
        self.shared.recording.store(false, Ordering::SeqCst);
    }
}

impl Drop for AudioService {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
    }
}

fn capture_thread(rx: mpsc::Receiver<Command>, shared: Arc<Shared>, level_tx: UnboundedSender<f32>) {
    let host = cpal::default_host();
    let mut stream: Option<cpal::Stream> = None;
    loop {
        let command = if stream.is_some() {
            match rx.recv_timeout(DRAIN_INTERVAL) {
                Ok(command) => command,
                Err(RecvTimeoutError::Timeout) => {
                    let mut conv = shared.conv.lock();
                    if let Err(e) = shared.drain_into(&mut conv) {
                        log::warn!("audio conversion: {e:#}");
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(command) => command,
                Err(_) => break,
            }
        };
        match command {
            Command::Start { device, keep, reply } => {
                if stream.take().is_some() {
                    log::warn!("a take started while another was still recording; the earlier one was discarded");
                }
                let result = match open_stream(&host, device.as_deref(), keep, &shared, &level_tx) {
                    Ok(opened) => {
                        stream = Some(opened);
                        Ok(())
                    }
                    Err(e) => {
                        shared.recording.store(false, Ordering::SeqCst);
                        Err(e)
                    }
                };
                let _ = reply.send(result);
            }
            Command::Stop { reply } => {
                // Dropping the stream stops capture; no callback runs after this returns.
                drop(stream.take());
                shared.recording.store(false, Ordering::SeqCst);
                let _ = level_tx.send(0.0);
                let _ = reply.send(());
            }
            Command::Shutdown => break,
        }
    }
    drop(stream);
}

fn open_stream(
    host: &cpal::Host,
    wanted: Option<&str>,
    keep: bool,
    shared: &Arc<Shared>,
    level_tx: &UnboundedSender<f32>,
) -> Result<cpal::Stream> {
    let (device, label) = find_input_device(host, wanted)?;
    let supported = device.default_input_config().map_err(|e| {
        let unplugged = matches!(e, cpal::DefaultStreamConfigError::DeviceNotAvailable);
        describe_open_error(&e, unplugged, &label)
    })?;
    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.config();
    let rate = config.sample_rate.0;
    let channels = config.channels.max(1) as usize;

    {
        let mut conv = shared.conv.lock();
        *conv = Converter::new(rate, SAMPLE_RATE)?;
        shared.raw.lock().clear();
    }
    *shared.error.lock() = None;

    let state = CaptureState::new(rate, channels, keep, shared.clone(), level_tx.clone());
    let built = match sample_format {
        cpal::SampleFormat::F32 => build_input::<f32>(&device, &config, state, &label, shared, level_tx),
        cpal::SampleFormat::I16 => build_input::<i16>(&device, &config, state, &label, shared, level_tx),
        cpal::SampleFormat::I32 => build_input::<i32>(&device, &config, state, &label, shared, level_tx),
        cpal::SampleFormat::I24 => build_input::<cpal::I24>(&device, &config, state, &label, shared, level_tx),
        cpal::SampleFormat::U8 => build_input::<u8>(&device, &config, state, &label, shared, level_tx),
        cpal::SampleFormat::U16 => build_input::<u16>(&device, &config, state, &label, shared, level_tx),
        cpal::SampleFormat::I8 => build_input::<i8>(&device, &config, state, &label, shared, level_tx),
        cpal::SampleFormat::U32 => build_input::<u32>(&device, &config, state, &label, shared, level_tx),
        cpal::SampleFormat::F64 => build_input::<f64>(&device, &config, state, &label, shared, level_tx),
        cpal::SampleFormat::I64 => build_input::<i64>(&device, &config, state, &label, shared, level_tx),
        cpal::SampleFormat::U64 => build_input::<u64>(&device, &config, state, &label, shared, level_tx),
        other => bail!("The microphone '{label}' uses an audio format Navi cannot read ({other})"),
    };
    let stream = built.map_err(|e| {
        let unplugged = matches!(e, cpal::BuildStreamError::DeviceNotAvailable);
        describe_open_error(&e, unplugged, &label)
    })?;
    shared.recording.store(true, Ordering::SeqCst);
    if let Err(e) = stream.play() {
        shared.recording.store(false, Ordering::SeqCst);
        let unplugged = matches!(e, cpal::PlayStreamError::DeviceNotAvailable);
        return Err(describe_open_error(&e, unplugged, &label));
    }
    log::debug!("microphone '{label}' open: {rate} Hz, {channels} channel(s), {sample_format}");
    Ok(stream)
}

fn build_input<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut state: CaptureState,
    label: &str,
    shared: &Arc<Shared>,
    level_tx: &UnboundedSender<f32>,
) -> std::result::Result<cpal::Stream, cpal::BuildStreamError>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let error_shared = shared.clone();
    let error_tx = level_tx.clone();
    let error_label = label.to_string();
    device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| state.push(data),
        move |err: cpal::StreamError| {
            let message = match err {
                cpal::StreamError::DeviceNotAvailable => format!("The microphone '{error_label}' was disconnected"),
                other => format!("The microphone '{error_label}' stopped working: {other}"),
            };
            log::warn!("{message}");
            *error_shared.error.lock() = Some(message);
            let _ = error_tx.send(0.0);
        },
        None,
    )
}

/// Everything the real-time callback needs. Owned by the callback closure.
struct CaptureState {
    channels: usize,
    /// False for a microphone test: the level is all it needs, so no audio is kept.
    keep: bool,
    dc_r: f32,
    dc_x: f32,
    dc_y: f32,
    level_acc: f64,
    level_n: usize,
    level_window: usize,
    level_tx: UnboundedSender<f32>,
    scratch: Vec<f32>,
    shared: Arc<Shared>,
}

impl CaptureState {
    fn new(rate: u32, channels: usize, keep: bool, shared: Arc<Shared>, level_tx: UnboundedSender<f32>) -> Self {
        Self {
            channels,
            keep,
            // One-pole DC blocker at about 20 Hz: cheap mics often carry an offset that would
            // otherwise read as noise on the level meter.
            dc_r: 1.0 - (2.0 * std::f32::consts::PI * 20.0 / rate.max(1) as f32),
            dc_x: 0.0,
            dc_y: 0.0,
            level_acc: 0.0,
            level_n: 0,
            level_window: ((rate as f64 * LEVEL_WINDOW_SECS) as usize).max(1),
            level_tx,
            scratch: Vec::with_capacity(4096),
            shared,
        }
    }

    fn push<T>(&mut self, data: &[T])
    where
        T: cpal::Sample,
        f32: cpal::FromSample<T>,
    {
        self.scratch.clear();
        let channels = self.channels;
        let inv = 1.0 / channels as f32;
        for frame in data.chunks_exact(channels) {
            let mut sum = 0.0f32;
            for s in frame {
                sum += s.to_sample::<f32>();
            }
            let x = sum * inv;
            let mut y = x - self.dc_x + self.dc_r * self.dc_y;
            if y.abs() < 1e-20 {
                y = 0.0; // keep the filter out of denormals during digital silence
            }
            self.dc_x = x;
            self.dc_y = y;
            self.scratch.push(y);

            self.level_acc += (y as f64) * (y as f64);
            self.level_n += 1;
            if self.level_n >= self.level_window {
                let rms = (self.level_acc / self.level_n as f64).sqrt() as f32;
                let _ = self.level_tx.send(level_from_rms(rms));
                self.level_acc = 0.0;
                self.level_n = 0;
            }
        }
        if self.keep {
            self.shared.raw.lock().extend_from_slice(&self.scratch);
        }
    }
}

/// RMS (linear, 0..1) to a 0..1 meter value: dBFS -60 maps to 0, -10 to 1.
pub fn level_from_rms(rms: f32) -> f32 {
    if rms.is_nan() || rms <= 1e-9 {
        return 0.0;
    }
    let db = 20.0 * rms.log10();
    ((db - LEVEL_FLOOR_DB) / (LEVEL_CEIL_DB - LEVEL_FLOOR_DB)).clamp(0.0, 1.0)
}

fn find_input_device(host: &cpal::Host, wanted: Option<&str>) -> Result<(cpal::Device, String)> {
    match wanted.map(str::trim).filter(|name| !name.is_empty()) {
        None => {
            let device = host.default_input_device().ok_or_else(|| anyhow!(NO_MICROPHONE))?;
            let name = device.name().unwrap_or_else(|_| "default microphone".into());
            Ok((device, name))
        }
        Some(name) => {
            let devices = host.input_devices().map_err(|e| describe_open_error(&e, false, name))?;
            for device in devices {
                if device.name().map(|n| n == name).unwrap_or(false) {
                    return Ok((device, name.to_string()));
                }
            }
            if host.default_input_device().is_none() {
                bail!(NO_MICROPHONE);
            }
            bail!("The microphone '{name}' is not connected")
        }
    }
}

/// Turns a cpal/WASAPI error into something the pill can show (wake.rs uses it too).
pub(crate) fn describe_open_error(err: &dyn std::fmt::Display, unplugged: bool, label: &str) -> anyhow::Error {
    let text = err.to_string();
    let lower = text.to_lowercase();
    if lower.contains("0x80070005") || lower.contains("access is denied") || lower.contains("access denied") {
        anyhow!(PRIVACY_BLOCKED)
    } else if unplugged || lower.contains("0x88890004") || lower.contains("0x80070490") {
        // AUDCLNT_E_DEVICE_INVALIDATED / E_NOTFOUND
        anyhow!("The microphone '{label}' is not connected")
    } else if lower.contains("0x8889000a") {
        anyhow!("The microphone '{label}' is being used exclusively by another app")
    } else if lower.contains("0x88890010") {
        anyhow!("The Windows Audio service is not running")
    } else if lower.contains("not supported") {
        anyhow!("The microphone '{label}' does not offer an audio format Navi can use")
    } else {
        anyhow!("Could not open the microphone '{label}': {text}")
    }
}

pub fn list_devices() -> Vec<DeviceInfo> {
    // Enumerate on a short-lived thread so cpal's COM set-up never touches the caller's thread.
    std::thread::Builder::new()
        .name("navi-audio-devices".into())
        .spawn(enumerate_devices)
        .ok()
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default()
}

fn enumerate_devices() -> Vec<DeviceInfo> {
    let host = cpal::default_host();
    let default_name = host.default_input_device().and_then(|d| d.name().ok());
    let mut out: Vec<DeviceInfo> = Vec::new();
    match host.input_devices() {
        Ok(devices) => {
            for device in devices {
                let Ok(name) = device.name() else { continue };
                if out.iter().any(|d| d.name == name) {
                    continue;
                }
                let is_default = default_name.as_deref() == Some(name.as_str());
                out.push(DeviceInfo { name, is_default });
            }
        }
        Err(e) => log::warn!("could not list microphones: {e}"),
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Resampling
// ---------------------------------------------------------------------------------------------

/// Streaming mono resampler with its delay removed: output sample `n` is input time `n / rate_out`.
struct Converter {
    rate_in: u32,
    rate_out: u32,
    resampler: Option<FftFixedIn<f32>>,
    /// Input not yet consumed (always shorter than one chunk between calls).
    pending: Vec<f32>,
    out: Vec<f32>,
    /// Resampler delay still to drop from the front of the output.
    skip: usize,
    delay: usize,
    total_in: u64,
    /// Samples put in front of `out` from elsewhere (`prepend`), which `total_in` doesn't count.
    prepended: usize,
    out_buf: Vec<Vec<f32>>,
    /// Swapped with `Shared::raw` so the callback keeps reusing an allocation.
    spare: Vec<f32>,
}

impl Converter {
    fn passthrough(rate: u32) -> Self {
        Self {
            rate_in: rate,
            rate_out: rate,
            resampler: None,
            pending: Vec::new(),
            out: Vec::new(),
            skip: 0,
            delay: 0,
            total_in: 0,
            prepended: 0,
            out_buf: Vec::new(),
            spare: Vec::new(),
        }
    }

    fn new(rate_in: u32, rate_out: u32) -> Result<Self> {
        if rate_in == 0 || rate_out == 0 {
            bail!("Invalid sample rate ({rate_in} Hz to {rate_out} Hz)");
        }
        if rate_in == rate_out {
            return Ok(Self::passthrough(rate_in));
        }
        let resampler = FftFixedIn::<f32>::new(rate_in as usize, rate_out as usize, RESAMPLER_CHUNK, 2, 1)
            .map_err(|e| anyhow!("Could not convert audio from {rate_in} Hz: {e}"))?;
        let delay = resampler.output_delay();
        let out_buf = resampler.output_buffer_allocate(true);
        Ok(Self {
            rate_in,
            rate_out,
            resampler: Some(resampler),
            pending: Vec::with_capacity(RESAMPLER_CHUNK * 2),
            out: Vec::new(),
            skip: delay,
            delay,
            total_in: 0,
            prepended: 0,
            out_buf,
            spare: Vec::new(),
        })
    }

    /// Inserts already converted audio after anything prepended before and ahead of the take.
    fn prepend(&mut self, samples: &[f32]) {
        let at = self.prepended.min(self.out.len());
        self.out.splice(at..at, samples.iter().copied());
        self.prepended = at + samples.len();
    }

    fn push(&mut self, input: &[f32]) -> Result<()> {
        self.total_in += input.len() as u64;
        let Some(resampler) = self.resampler.as_mut() else {
            self.out.extend_from_slice(input);
            return Ok(());
        };
        self.pending.extend_from_slice(input);
        let mut offset = 0;
        while self.pending.len() - offset >= RESAMPLER_CHUNK {
            let chunk = &self.pending[offset..offset + RESAMPLER_CHUNK];
            process_chunk(resampler, chunk, &mut self.out_buf, &mut self.skip, &mut self.out)?;
            offset += RESAMPLER_CHUNK;
        }
        self.pending.drain(..offset);
        Ok(())
    }

    /// Flushes the tail: pads with silence until the output covers the whole input, then trims
    /// to exactly `input length * rate_out / rate_in` samples (plus anything prepended, which
    /// must not be cut off the end instead).
    fn finish(&mut self) -> Result<()> {
        let Some(resampler) = self.resampler.as_mut() else { return Ok(()) };
        let expected = self.prepended + ((self.total_in as f64) * self.rate_out as f64 / self.rate_in as f64).round() as usize;
        let mut chunk = vec![0.0f32; RESAMPLER_CHUNK];
        let mut rounds = 0;
        while self.out.len() < expected && rounds < 1024 {
            let n = self.pending.len().min(RESAMPLER_CHUNK);
            chunk[..n].copy_from_slice(&self.pending[..n]);
            chunk[n..].fill(0.0);
            self.pending.drain(..n);
            process_chunk(resampler, &chunk, &mut self.out_buf, &mut self.skip, &mut self.out)?;
            rounds += 1;
        }
        self.out.truncate(expected);
        self.pending.clear();
        Ok(())
    }

    fn clear(&mut self) {
        if let Some(resampler) = self.resampler.as_mut() {
            resampler.reset();
        }
        self.pending.clear();
        self.out = Vec::new();
        self.skip = self.delay;
        self.total_in = 0;
        self.prepended = 0;
    }
}

fn process_chunk(
    resampler: &mut FftFixedIn<f32>,
    chunk: &[f32],
    out_buf: &mut [Vec<f32>],
    skip: &mut usize,
    out: &mut Vec<f32>,
) -> Result<()> {
    let (_, produced) = resampler
        .process_into_buffer(&[chunk], out_buf, None)
        .map_err(|e| anyhow!("Audio conversion failed: {e}"))?;
    let mut frames = &out_buf[0][..produced];
    if *skip > 0 {
        let k = (*skip).min(frames.len());
        *skip -= k;
        frames = &frames[k..];
    }
    out.extend_from_slice(frames);
    Ok(())
}

/// Streaming conversion to 16 kHz for audio captured outside a take (the "Hey Navi" listener).
pub struct To16k(Converter);

impl To16k {
    pub fn new(rate_in: u32) -> Result<Self> {
        Ok(Self(Converter::new(rate_in, SAMPLE_RATE)?))
    }

    /// Converts `input` and returns the 16 kHz samples now ready.
    pub fn push(&mut self, input: &[f32]) -> Result<Vec<f32>> {
        self.0.push(input)?;
        Ok(std::mem::take(&mut self.0.out))
    }
}

/// Resamples mono audio between any two rates (FFT resampler, delay compensated, exact length).
pub fn resample(input: &[f32], rate_in: u32, rate_out: u32) -> Result<Vec<f32>> {
    let mut converter = Converter::new(rate_in, rate_out)?;
    converter.push(input)?;
    converter.finish()?;
    Ok(std::mem::take(&mut converter.out))
}

// ---------------------------------------------------------------------------------------------
// WAV files
// ---------------------------------------------------------------------------------------------

/// 16 kHz mono 16-bit PCM WAV.
pub fn write_wav(path: &Path, samples_16k: &[f32]) -> Result<()> {
    let spec = hound::WavSpec { channels: 1, sample_rate: SAMPLE_RATE, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(path, spec)?;
    for s in samples_16k {
        w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)?;
    }
    w.finalize()?;
    Ok(())
}

/// Reads any WAV and returns 16 kHz mono f32 (resampling if needed). Used by re-runs from history.
pub fn read_wav_16k(path: &Path) -> Result<Vec<f32>> {
    let (mono, rate) = decode_wav_mono(path)?;
    resample(&mono, rate, SAMPLE_RATE)
}

/// Decodes a PCM or float WAV (any bit depth, any channel count) to mono f32 at its own rate.
/// A file cut short (for example by a crash while recording) yields the samples before the damage.
pub fn decode_wav_mono(path: &Path) -> Result<(Vec<f32>, u32)> {
    let mut reader = hound::WavReader::open(path)
        .with_context(|| format!("Could not read the audio file {}", path.display()))?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let interleaved: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Float, 32) => collect_samples(reader.samples::<f32>(), |v| v),
        (hound::SampleFormat::Int, bits @ 1..=8) => {
            let scale = 1.0 / (1u32 << (bits - 1)) as f32;
            collect_samples(reader.samples::<i8>(), |v| v as f32 * scale)
        }
        (hound::SampleFormat::Int, bits @ 9..=16) => {
            let scale = 1.0 / (1u32 << (bits - 1)) as f32;
            collect_samples(reader.samples::<i16>(), |v| v as f32 * scale)
        }
        (hound::SampleFormat::Int, bits @ 17..=32) => {
            let scale = 1.0 / (1u64 << (bits - 1)) as f64;
            collect_samples(reader.samples::<i32>(), |v| (v as f64 * scale) as f32)
        }
        (format, bits) => bail!("{} uses an unsupported WAV format ({bits}-bit {format:?})", path.display()),
    };
    if interleaved.is_empty() && reader.duration() > 0 {
        bail!("{} could not be decoded", path.display());
    }
    let mono = if channels == 1 {
        interleaved
    } else {
        let inv = 1.0 / channels as f32;
        interleaved.chunks_exact(channels).map(|frame| frame.iter().sum::<f32>() * inv).collect()
    };
    Ok((mono, spec.sample_rate))
}

fn collect_samples<S, I>(samples: I, convert: impl Fn(S) -> f32) -> Vec<f32>
where
    I: Iterator<Item = hound::Result<S>>,
{
    let (lower, _) = samples.size_hint();
    let mut out = Vec::with_capacity(lower);
    for sample in samples {
        match sample {
            Ok(v) => out.push(convert(v)),
            Err(e) => {
                log::warn!("audio file ends early or is damaged after {} samples: {e}", out.len());
                break;
            }
        }
    }
    out
}

/// In-memory 16 kHz mono 16-bit WAV, for sending to the engine.
pub fn wav_bytes(samples_16k: &[f32]) -> Result<Vec<u8>> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let spec = hound::WavSpec { channels: 1, sample_rate: SAMPLE_RATE, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::new(&mut cursor, spec)?;
        for s in samples_16k {
            w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)?;
        }
        w.finalize()?;
    }
    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    fn sine(freq: f32, rate: u32, secs: f32, amp: f32) -> Vec<f32> {
        let n = (rate as f32 * secs) as usize;
        (0..n).map(|i| amp * (2.0 * PI * freq * i as f32 / rate as f32).sin()).collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt() as f32
    }

    /// Correlation with a reference sine at the target rate; ~1.0 means same frequency, phase
    /// (so no leftover resampler delay) and shape.
    fn similarity(x: &[f32], reference: &[f32]) -> f32 {
        let dot: f64 = x.iter().zip(reference).map(|(a, b)| *a as f64 * *b as f64).sum();
        let na: f64 = x.iter().map(|a| (*a as f64).powi(2)).sum::<f64>().sqrt();
        let nb: f64 = reference.iter().map(|b| (*b as f64).powi(2)).sum::<f64>().sqrt();
        (dot / (na * nb)) as f32
    }

    #[test]
    fn resample_keeps_length_level_and_timing() {
        for rate in [8_000u32, 22_050, 44_100, 48_000, 96_000] {
            let input = sine(440.0, rate, 1.0, 0.5);
            let out = resample(&input, rate, SAMPLE_RATE).expect("resample");
            assert_eq!(out.len(), SAMPLE_RATE as usize, "length at {rate} Hz");
            let reference = sine(440.0, SAMPLE_RATE, 1.0, 0.5);
            // Ignore the edges, where the filter sees the silence beyond the signal.
            let mid = 800..15_200;
            // The resampler delay is removed to the nearest whole sample (a residue of up to half a
            // sample, about 30 µs, remains), so the best alignment must be at lag 0.
            let at_lag = |lag: isize| {
                let shifted = (800 + lag) as usize..(15_200 + lag) as usize;
                similarity(&out[shifted], &reference[mid.clone()])
            };
            let best = (-3..=3).max_by(|a, b| at_lag(*a).total_cmp(&at_lag(*b))).unwrap_or(99);
            assert_eq!(best, 0, "output is shifted at {rate} Hz");
            assert!(at_lag(0) > 0.995, "shape off at {rate} Hz: {}", at_lag(0));
            let level = rms(&out[mid.clone()]) / rms(&reference[mid.clone()]);
            assert!((level - 1.0).abs() < 0.01, "level off at {rate} Hz: {level}");
        }
    }

    #[test]
    fn resample_removes_content_above_8k() {
        // 12 kHz cannot exist at 16 kHz; it must be filtered, not folded back to 4 kHz.
        let input = sine(12_000.0, 48_000, 1.0, 0.5);
        let out = resample(&input, 48_000, SAMPLE_RATE).expect("resample");
        assert!(rms(&out[800..15_200]) < 0.005, "aliasing: {}", rms(&out[800..15_200]));
    }

    #[test]
    fn streaming_matches_offline_and_snapshots_are_prefixes() {
        let input = sine(300.0, 44_100, 2.3, 0.4);
        let offline = resample(&input, 44_100, SAMPLE_RATE).expect("offline");

        let mut conv = Converter::new(44_100, SAMPLE_RATE).expect("converter");
        let mut fed = 0;
        let mut step = 1;
        let mut snapshot_len = 0;
        while fed < input.len() {
            let n = (step * 97 % 3000 + 1).min(input.len() - fed);
            conv.push(&input[fed..fed + n]).expect("push");
            fed += n;
            step += 1;
            assert!(conv.out.len() >= snapshot_len, "snapshot shrank");
            assert_eq!(&conv.out[..], &offline[..conv.out.len()], "snapshot is not a prefix");
            snapshot_len = conv.out.len();
        }
        conv.finish().expect("finish");
        assert_eq!(conv.out.len(), offline.len());
        let max_diff = conv.out.iter().zip(&offline).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(max_diff < 1e-6, "streaming differs from offline by {max_diff}");
    }

    #[test]
    fn prepended_audio_does_not_cut_the_end_of_the_take() {
        // A "Hey Navi" take: the take's own audio at 48 kHz, with 16 kHz audio from the listener
        // put in front of it in two pieces while it records.
        let input = sine(300.0, 48_000, 2.0, 0.4);
        let offline = resample(&input, 48_000, SAMPLE_RATE).expect("offline");
        let mut conv = Converter::new(48_000, SAMPLE_RATE).expect("converter");
        conv.push(&input[..30_000]).expect("push");
        conv.prepend(&[0.25; 8_000]);
        conv.prepend(&[-0.25; 1_600]);
        conv.push(&input[30_000..]).expect("push");
        conv.finish().expect("finish");
        assert_eq!(conv.out.len(), 8_000 + 1_600 + offline.len(), "the whole take is kept");
        assert!(conv.out[..8_000].iter().all(|v| *v == 0.25), "the first piece comes first");
        assert!(conv.out[8_000..9_600].iter().all(|v| *v == -0.25), "the later piece follows it");
        let max_diff = conv.out[9_600..].iter().zip(&offline).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(max_diff < 1e-6, "the take follows the prepended audio unchanged ({max_diff})");

        conv.clear();
        conv.push(&input).expect("push");
        conv.finish().expect("finish");
        assert_eq!(conv.out.len(), offline.len(), "a cleared converter forgets what was prepended");
    }

    #[test]
    fn a_microphone_test_meters_but_keeps_no_audio() {
        let shared = Arc::new(Shared {
            recording: AtomicBool::new(true),
            raw: Mutex::new(Vec::new()),
            conv: Mutex::new(Converter::passthrough(SAMPLE_RATE)),
            error: Mutex::new(None),
        });
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let speech = sine(440.0, 48_000, 0.1, 0.5);

        let mut meter = CaptureState::new(48_000, 1, false, shared.clone(), tx.clone());
        meter.push(&speech[..]);
        assert!(shared.raw.lock().is_empty(), "a test left running must not pile up audio");
        let mut levels = Vec::new();
        while let Ok(level) = rx.try_recv() {
            levels.push(level);
        }
        assert_eq!(levels.len(), 5, "a level every 20 ms, as for a take");
        assert!(levels.iter().all(|l| *l > 0.5), "{levels:?}");

        let mut take = CaptureState::new(48_000, 1, true, shared.clone(), tx);
        take.push(&speech[..]);
        assert_eq!(shared.raw.lock().len(), speech.len(), "a take keeps every sample");
    }

    #[test]
    fn passthrough_at_16k_is_exact() {
        let input = sine(1000.0, SAMPLE_RATE, 0.5, 0.3);
        let out = resample(&input, SAMPLE_RATE, SAMPLE_RATE).expect("resample");
        assert_eq!(out, input);
    }

    #[test]
    fn levels_follow_a_perceptual_curve() {
        assert_eq!(level_from_rms(0.0), 0.0);
        assert_eq!(level_from_rms(f32::NAN), 0.0);
        assert_eq!(level_from_rms(0.0005), 0.0); // about -66 dBFS
        assert!((level_from_rms(10f32.powf(-35.0 / 20.0)) - 0.5).abs() < 1e-3);
        assert_eq!(level_from_rms(1.0), 1.0);
    }

    #[test]
    fn wav_round_trip_through_any_format() {
        let dir = std::env::temp_dir().join(format!("navi-audio-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");

        // 44.1 kHz stereo 24-bit with the tone on both channels.
        let path24 = dir.join("stereo24.wav");
        let spec = hound::WavSpec { channels: 2, sample_rate: 44_100, bits_per_sample: 24, sample_format: hound::SampleFormat::Int };
        let tone = sine(500.0, 44_100, 1.0, 0.5);
        let mut w = hound::WavWriter::create(&path24, spec).expect("create");
        for s in &tone {
            let v = (s * 8_388_607.0) as i32;
            w.write_sample(v).expect("write");
            w.write_sample(v).expect("write");
        }
        w.finalize().expect("finalize");
        let out = read_wav_16k(&path24).expect("read 24-bit");
        assert_eq!(out.len(), 16_000);
        let reference = sine(500.0, SAMPLE_RATE, 1.0, 0.5);
        assert!(similarity(&out[800..15_200], &reference[800..15_200]) > 0.999);

        // 8-bit mono and 32-bit float mono.
        let path8 = dir.join("mono8.wav");
        let spec = hound::WavSpec { channels: 1, sample_rate: 16_000, bits_per_sample: 8, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&path8, spec).expect("create");
        for s in &reference {
            w.write_sample((s * 127.0) as i8).expect("write");
        }
        w.finalize().expect("finalize");
        let out8 = read_wav_16k(&path8).expect("read 8-bit");
        assert!(similarity(&out8, &reference) > 0.99);

        let pathf = dir.join("monof.wav");
        let spec = hound::WavSpec { channels: 1, sample_rate: 48_000, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
        let mut w = hound::WavWriter::create(&pathf, spec).expect("create");
        for s in sine(500.0, 48_000, 1.0, 0.5) {
            w.write_sample(s).expect("write");
        }
        w.finalize().expect("finalize");
        let outf = read_wav_16k(&pathf).expect("read float");
        assert_eq!(outf.len(), 16_000);

        // Our own 16 kHz writer round-trips.
        let path16 = dir.join("take.wav");
        write_wav(&path16, &reference).expect("write take");
        let back = read_wav_16k(&path16).expect("read take");
        assert_eq!(back.len(), reference.len());
        assert!(similarity(&back, &reference) > 0.9999);

        assert!(read_wav_16k(&dir.join("missing.wav")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn errors_read_well() {
        let e = describe_open_error(&"Access is denied. (0x80070005)", false, "Mic");
        assert_eq!(e.to_string(), PRIVACY_BLOCKED);
        let e = describe_open_error(&"whatever", true, "USB Mic");
        assert_eq!(e.to_string(), "The microphone 'USB Mic' is not connected");
        let e = describe_open_error(&"Device in use (0x8889000A)", false, "USB Mic");
        assert!(e.to_string().contains("exclusively"));
    }

    /// Opens the default microphone for about 1.5 seconds. Run by hand:
    /// cargo test --lib audio::tests::mic_capture_smoke -- --ignored --nocapture
    #[test]
    #[ignore]
    fn mic_capture_smoke() {
        let t = std::time::Instant::now();
        let devices = list_devices();
        println!("devices ({} ms): {devices:?}", t.elapsed().as_millis());

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let audio = AudioService::new(tx);
        let t = std::time::Instant::now();
        audio.start(None).expect("start");
        println!("start took {} ms", t.elapsed().as_millis());
        assert!(audio.is_recording());
        std::thread::sleep(Duration::from_millis(700));
        let early = audio.snapshot_16k(0);
        std::thread::sleep(Duration::from_millis(800));
        let rec = audio.stop().expect("stop");
        assert!(!audio.is_recording());

        let mut levels = Vec::new();
        while let Ok(level) = rx.try_recv() {
            levels.push(level);
        }
        let peak = levels.iter().cloned().fold(0.0f32, f32::max);
        println!(
            "captured {} samples ({} ms), snapshot after 0.7 s had {}, {} levels (peak {peak:.2}), rms {:.4}, error {:?}",
            rec.samples.len(),
            rec.duration_ms,
            early.len(),
            levels.len(),
            rms(&rec.samples),
            audio.last_error()
        );
        assert!(rec.duration_ms > 1300 && rec.duration_ms < 1800, "duration {}", rec.duration_ms);
        assert!(levels.len() > 50, "levels arrive every ~20 ms");
        assert_eq!(&rec.samples[..early.len()], &early[..], "snapshot is a prefix of the take");
        assert!(audio.stop().is_err(), "second stop is an error");
    }
}
