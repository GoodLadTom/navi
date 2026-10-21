//! Speech-to-text through a whisper.cpp server that Navi starts and keeps warm.
//!
//! OWNER: audio and engine agent. STATUS: implemented.
//!
//! Contract:
//! * `Engine::start` finds whisper-server.exe (models::find_server) and the model file
//!   (models::find_model), picks a free localhost port, and spawns the server hidden
//!   (CREATE_NO_WINDOW) with the model, threads ≈ physical cores / 2, language, `--no-gpu` when GPU
//!   is off, and the Silero VAD options when `vad` is on and the VAD model is installed. It waits
//!   until the server answers (poll for up to ~60 s, model loading takes a few seconds) and kills the
//!   child process when the Engine is dropped or `shutdown` runs. If the process exits, the next
//!   `transcribe` restarts it once before failing.
//! * `transcribe` posts the audio as a WAV (multipart field "file") to /inference with
//!   response_format=verbose_json (fall back to json), temperature 0, the language, and `prompt`
//!   (vocabulary biasing: names and terms the user cares about). Returns trimmed text and segments.
//!   Typical latency for 10 s of speech on an RTX 5070 with large-v3-turbo should be well under 1 s.
//! * Status changes are reported by calling the `on_status` callback given to `start`.
//! * Check the flags against `whisper-server --help` of the installed build (for example
//!   %USERPROFILE%\.local\tools\whisper-cuda\Release\whisper-server.exe) rather than guessing.
//!
//! Implementation notes:
//! * The child runs inside a Windows Job Object with KILL_ON_JOB_CLOSE, so if Navi crashes or is
//!   killed the server dies with it instead of holding gigabytes of VRAM.
//! * `-nlp` (no language probabilities) is always passed when supported: without it verbose_json
//!   runs an extra language-detection pass that costs ~150 ms per request on the RTX 5070.
//! * `start` sends one short warm-up request (sounds/warmup.wav) before reporting ready, because the
//!   first CUDA inference after loading is noticeably slower than the rest.
//! * Clips shorter than 1.05 s are padded with silence (some whisper.cpp builds ignore anything
//!   under a second). Whole segments that are bracketed non-speech tags ("[BLANK_AUDIO]") are dropped.
//! * If the server exits while loading on the GPU, start retries once on the CPU; an unknown
//!   optional flag (older builds) is dropped and retried; a port clash retries on a new port.

use crate::audio;
use crate::models::{self, GpuInfo};
use crate::paths::Paths;
use crate::types::{EngineSettings, EngineStatus};
use anyhow::{anyhow, bail, Context, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Clips up to this long go to the quick server (`transcribe_quick`), within its audio window.
#[cfg(target_os = "macos")]
const QUICK_MAX_SAMPLES: usize = audio::SAMPLE_RATE as usize * 7;
/// The quick server's audio window in Whisper's frames (1500 is the full 30 s): 384 is 7.68 s.
#[cfg(target_os = "macos")]
const QUICK_AUDIO_CTX: u32 = 384;
const READY_TIMEOUT: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Enough to hold the server's whole usage text, which follows an "unknown argument" error.
const LOG_TAIL_LINES: usize = 200;
const MAX_LAUNCH_ATTEMPTS: usize = 4;
/// 1.05 s at 16 kHz.
const MIN_SAMPLES: usize = audio::SAMPLE_RATE as usize * 21 / 20;
const MAX_PROMPT_CHARS: usize = 1_000;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Debug, Default)]
pub struct TranscribeOptions {
    /// "en" or "auto".
    pub language: String,
    /// Vocabulary prompt (at most ~800 characters).
    pub prompt: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transcript {
    pub text: String,
    pub segments: Vec<Segment>,
    /// Engine time only.
    pub ms: u64,
}

pub type StatusCallback = Arc<dyn Fn(EngineStatus) + Send + Sync>;

pub struct Engine {
    inner: Arc<Inner>,
}

struct Inner {
    /// A second server with a short audio window for voice-command checks (macOS; see
    /// `Engine::transcribe_quick`). Started on first use.
    quick: tokio::sync::Mutex<Option<Server>>,
    settings: EngineSettings,
    paths: Paths,
    on_status: StatusCallback,
    http: reqwest::Client,
    status: Mutex<EngineStatus>,
    server: tokio::sync::Mutex<Option<Server>>,
    exe: PathBuf,
    model: PathBuf,
    vad_model: Option<PathBuf>,
    gpu: Option<GpuInfo>,
    cuda_build: bool,
    threads: usize,
    /// Cleared if this server build cannot produce verbose_json; plain json is used from then on.
    verbose: AtomicBool,
    plan: Mutex<LaunchPlan>,
    stopped: AtomicBool,
}

/// What to ask of the server. Optional parts are switched off when a build rejects them.
#[derive(Clone, Copy, Debug)]
struct LaunchPlan {
    gpu: bool,
    no_language_probabilities: bool,
    vad: bool,
}

struct Server {
    child: Child,
    port: u16,
    log: Arc<Mutex<VecDeque<String>>>,
    readers_done: Arc<AtomicUsize>,
    _job: Option<job::JobHandle>,
}

impl Server {
    fn tail(&self) -> Vec<String> {
        self.log.lock().iter().cloned().collect()
    }

    /// After the process exits, give the log readers a moment to collect its last words.
    async fn wait_for_log(&self) {
        let deadline = Instant::now() + Duration::from_millis(500);
        while self.readers_done.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

enum StartFailure {
    Exited(Option<i32>),
    Timeout,
}

impl Engine {
    pub async fn start(settings: &EngineSettings, paths: &Paths, on_status: StatusCallback) -> Result<Self> {
        let mut status = EngineStatus {
            state: "starting".into(),
            message: "Starting the speech engine".into(),
            model: settings.model.clone(),
            gpu: None,
            device: String::new(),
        };
        on_status(status.clone());

        let Some(exe) = models::find_server(settings, paths) else {
            status.state = "missing-engine".into();
            status.message = "The speech engine is not installed yet".into();
            on_status(status);
            bail!("The speech engine is not installed yet");
        };
        let Some(model) = models::find_model(settings, paths) else {
            let message = format!("The {} model is not downloaded yet", models::display_name(&settings.model));
            status.state = "missing-model".into();
            status.message = message.clone();
            on_status(status);
            bail!(message);
        };

        let gpu = if settings.use_gpu {
            tokio::task::spawn_blocking(models::detect_gpu).await.ok().flatten()
        } else {
            None
        };
        status.gpu = gpu.as_ref().map(GpuInfo::label);
        let vad_model = if settings.vad { models::find_vad_model(paths) } else { None };
        if settings.vad && vad_model.is_none() {
            log::info!("silence trimming is on but the VAD model is not installed; running without it");
        }

        let http = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(3))
            .build()
            .context("Could not set up the connection to the speech engine")?;

        let inner = Arc::new(Inner {
            settings: settings.clone(),
            paths: paths.clone(),
            on_status,
            http,
            status: Mutex::new(status),
            server: tokio::sync::Mutex::new(None),
            quick: tokio::sync::Mutex::new(None),
            cuda_build: has_cuda_backend(&exe),
            exe,
            model,
            plan: Mutex::new(LaunchPlan {
                gpu: settings.use_gpu,
                no_language_probabilities: true,
                vad: vad_model.is_some(),
            }),
            vad_model,
            gpu,
            threads: default_threads(),
            verbose: AtomicBool::new(true),
            stopped: AtomicBool::new(false),
        });

        let server = inner.launch().await?;
        let port = server.port;
        *inner.server.lock().await = Some(server);
        inner.warm_up(port).await;
        inner.report_ready();
        Ok(Engine { inner })
    }

    /// For voice-command checks: short clips the user is waiting on. Whisper always works through a
    /// 30-second window, which on Apple silicon's GPU costs about 1.2 s with large-v3-turbo however
    /// short the clip, so on a Mac a clip of up to QUICK_MAX_SAMPLES goes to a second server
    /// started with a 7.68-second window (`-ac 384`), about 0.3 s a check. Takes keep the full
    /// window. Anything else, or a quick server that can't start, uses `transcribe`.
    pub async fn transcribe_quick(&self, samples_16k: &[f32], opts: &TranscribeOptions) -> Result<Transcript> {
        #[cfg(target_os = "macos")]
        if !samples_16k.is_empty() && samples_16k.len() <= QUICK_MAX_SAMPLES {
            match self.inner.quick_port().await {
                Ok(port) => {
                    let (wav, duration_ms) = wav_for(samples_16k)?;
                    match self.inner.infer(port, &wav, opts, duration_ms).await {
                        Ok(transcript) => return Ok(transcript),
                        Err(e) => log::debug!("the quick speech engine failed a check ({e:#}); using the main one"),
                    }
                }
                Err(e) => log::debug!("the quick speech engine isn't available ({e:#}); using the main one"),
            }
        }
        self.transcribe(samples_16k, opts).await
    }

    /// Starts the quick server ahead of the first check (no-op off macOS).
    pub fn warm_quick(&self) {
        #[cfg(target_os = "macos")]
        {
            let inner = self.inner.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = inner.quick_port().await {
                    log::info!("the quick speech engine didn't start ({e:#}); checks use the main one");
                }
            });
        }
    }

    pub async fn transcribe(&self, samples_16k: &[f32], opts: &TranscribeOptions) -> Result<Transcript> {
        if samples_16k.is_empty() {
            return Ok(Transcript::default());
        }
        let (wav, duration_ms) = wav_for(samples_16k)?;

        let (port, restarted) = self.inner.ensure_running().await?;
        let first = match self.inner.infer(port, &wav, opts, duration_ms).await {
            Ok(transcript) => return Ok(transcript),
            Err(e) => e,
        };
        if restarted {
            return Err(readable(first));
        }
        // A crash shows up as a dropped connection a moment before the process can be reaped.
        if first.downcast_ref::<reqwest::Error>().is_some() {
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        let (port, restarted) = self.inner.ensure_running().await?;
        if !restarted {
            return Err(readable(first));
        }
        self.inner.infer(port, &wav, opts, duration_ms).await.map_err(readable)
    }

    pub fn status(&self) -> EngineStatus {
        self.inner.status.lock().clone()
    }

    pub async fn shutdown(&self) {
        self.inner.stopped.store(true, Ordering::SeqCst);
        let quick = self.inner.quick.lock().await.take();
        if let Some(quick) = quick {
            let _ = tokio::task::spawn_blocking(move || drop(quick)).await;
        }
        let server = self.inner.server.lock().await.take();
        if let Some(server) = server {
            let _ = tokio::task::spawn_blocking(move || drop(server)).await;
        }
        self.inner.set_status("stopped", "The speech engine is stopped", None);
    }

    /// The localhost port the server is listening on, if it is running (diagnostics).
    pub async fn port(&self) -> Option<u16> {
        self.inner.server.lock().await.as_ref().map(|s| s.port)
    }
}

impl Inner {
    /// The quick server's port, starting it if it isn't running.
    #[cfg(target_os = "macos")]
    async fn quick_port(&self) -> Result<u16> {
        let mut guard = self.quick.lock().await;
        if let Some(server) = guard.as_mut() {
            if matches!(server.child.try_wait(), Ok(None)) {
                return Ok(server.port);
            }
        }
        if let Some(old) = guard.take() {
            let _ = tokio::task::spawn_blocking(move || drop(old)).await;
        }
        if self.stopped.load(Ordering::SeqCst) {
            bail!("The speech engine is stopped");
        }
        // Short clips need no silence trimming, and it would only add time.
        let plan = LaunchPlan { vad: false, ..*self.plan.lock() };
        let device = self.device_for(plan);
        let mut server = self.spawn(plan, device, Some(QUICK_AUDIO_CTX))?;
        if self.wait_ready(&mut server).await.is_err() {
            let _ = tokio::task::spawn_blocking(move || drop(server)).await;
            bail!("it did not start");
        }
        let port = server.port;
        // The first request on a new server sets up the GPU and is slow: get it over with here,
        // so the first "Navi" doesn't wait and the listener doesn't think checks are slow.
        self.warm_up(port).await;
        log::info!("quick speech engine ready on port {port} for voice commands");
        *guard = Some(server);
        Ok(port)
    }

    fn set_status(&self, state: &str, message: impl Into<String>, device: Option<&str>) {
        let snapshot = {
            let mut status = self.status.lock();
            status.state = state.to_string();
            status.message = message.into();
            if let Some(device) = device {
                status.device = device.to_string();
            }
            status.clone()
        };
        (self.on_status)(snapshot);
    }

    fn report_ready(&self) {
        let device = self.status.lock().device.clone();
        let message = match (&self.gpu, device.as_str()) {
            (Some(gpu), "cuda" | "metal") => format!("Ready on the {}", gpu.name),
            _ => "Ready on the CPU".to_string(),
        };
        self.set_status("ready", message, None);
    }

    fn device_for(&self, plan: LaunchPlan) -> &'static str {
        if plan.gpu && self.cuda_build && self.gpu.is_some() {
            if cfg!(target_os = "macos") { "metal" } else { "cuda" }
        } else {
            "cpu"
        }
    }

    async fn launch(&self) -> Result<Server> {
        let model_name = models::display_name(&self.settings.model);
        for attempt in 1..=MAX_LAUNCH_ATTEMPTS {
            let plan = *self.plan.lock();
            let device = self.device_for(plan);
            let place = match (&self.gpu, device) {
                (Some(gpu), "cuda" | "metal") => format!("the {}", gpu.name),
                _ => "the CPU".to_string(),
            };
            self.set_status("starting", format!("Loading {model_name} on {place}"), Some(device));

            let mut server = match self.spawn(plan, device, None) {
                Ok(server) => server,
                Err(e) => {
                    self.set_status("error", format!("{e:#}"), None);
                    return Err(e);
                }
            };
            let failure = match self.wait_ready(&mut server).await {
                Ok(()) => {
                    log::info!("speech engine ready on port {} ({device})", server.port);
                    return Ok(server);
                }
                Err(failure) => failure,
            };
            server.wait_for_log().await;
            let tail = server.tail();
            let _ = tokio::task::spawn_blocking(move || drop(server)).await;

            match failure {
                StartFailure::Timeout => {
                    let message = format!("The speech engine did not start within {} seconds", READY_TIMEOUT.as_secs());
                    log::warn!("{message}; last output:\n{}", tail.join("\n"));
                    self.set_status("error", message.clone(), None);
                    bail!(message);
                }
                StartFailure::Exited(code) => {
                    log::warn!("speech engine exited while starting (code {code:?}); last output:\n{}", tail.join("\n"));
                    let lower = tail.join("\n").to_lowercase();
                    let mut plan = self.plan.lock();
                    if attempt < MAX_LAUNCH_ATTEMPTS {
                        if let Some(flag) = rejected_flag(&tail) {
                            if plan.no_language_probabilities || plan.vad {
                                if flag == "-nlp" || flag == "--no-language-probabilities" {
                                    plan.no_language_probabilities = false;
                                } else if flag.starts_with("--vad") || flag == "-vm" {
                                    plan.vad = false;
                                } else {
                                    plan.no_language_probabilities = false;
                                    plan.vad = false;
                                }
                                log::warn!("this speech engine build does not know {flag}; starting without it");
                                continue;
                            }
                        }
                        if lower.contains("couldn't bind") || lower.contains("bind to server socket") {
                            continue; // port taken in the meantime; a new one is picked
                        }
                        if device != "cpu" && !model_failed_to_load(&lower) {
                            log::warn!("the graphics card could not run the speech engine; trying the CPU");
                            plan.gpu = false;
                            continue;
                        }
                    }
                    drop(plan);
                    let message = explain_exit(&lower, &model_name);
                    self.set_status("error", message.clone(), None);
                    bail!(message);
                }
            }
        }
        let message = "The speech engine could not be started".to_string();
        self.set_status("error", message.clone(), None);
        bail!(message)
    }

    /// `audio_ctx`: a shorter audio window than Whisper's 30 seconds (the quick server).
    fn spawn(&self, plan: LaunchPlan, device: &'static str, audio_ctx: Option<u32>) -> Result<Server> {
        let port = free_port()?;
        let language = match self.settings.language.trim() {
            "" => "en",
            other => other,
        };
        let mut cmd = Command::new(&self.exe);
        cmd.arg("-m")
            .arg(&self.model)
            .args(["--host", "127.0.0.1", "--port", &port.to_string()])
            .args(["-t", &self.threads.to_string()])
            .args(["-l", language]);
        if plan.no_language_probabilities {
            cmd.arg("-nlp");
        }
        if device == "cpu" {
            cmd.arg("--no-gpu");
        }
        if let Some(ctx) = audio_ctx {
            cmd.args(["-ac", &ctx.to_string()]);
        }
        if plan.vad {
            if let Some(vad) = &self.vad_model {
                cmd.arg("--vad").arg("--vad-model").arg(vad);
            }
        }
        if let Some(dir) = self.exe.parent() {
            cmd.current_dir(dir);
        }
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let job = match job::JobHandle::kill_on_close() {
            Ok(job) => Some(job),
            Err(e) => {
                log::warn!("could not create a job object for the speech engine: {e:#}");
                None
            }
        };
        let mut child = cmd
            .spawn()
            .with_context(|| format!("Could not start the speech engine ({})", self.exe.display()))?;
        if let Some(job) = &job {
            if let Err(e) = job.assign(&child) {
                log::warn!("could not tie the speech engine to Navi's lifetime: {e:#}");
            }
        }

        let log = Arc::new(Mutex::new(VecDeque::with_capacity(LOG_TAIL_LINES)));
        let readers_done = Arc::new(AtomicUsize::new(0));
        match child.stdout.take() {
            Some(out) => spawn_log_reader(out, log.clone(), readers_done.clone()),
            None => {
                readers_done.fetch_add(1, Ordering::SeqCst);
            }
        }
        match child.stderr.take() {
            Some(err) => spawn_log_reader(err, log.clone(), readers_done.clone()),
            None => {
                readers_done.fetch_add(1, Ordering::SeqCst);
            }
        }
        log::info!(
            "starting speech engine {} with {} on port {port} ({device}, {} threads, VAD {})",
            self.exe.display(),
            self.model.display(),
            self.threads,
            if plan.vad && self.vad_model.is_some() { "on" } else { "off" }
        );
        Ok(Server { child, port, log, readers_done, _job: job })
    }

    async fn wait_ready(&self, server: &mut Server) -> std::result::Result<(), StartFailure> {
        let url = format!("http://127.0.0.1:{}/health", server.port);
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            match server.child.try_wait() {
                Ok(Some(status)) => return Err(StartFailure::Exited(status.code())),
                Ok(None) => {}
                Err(_) => return Err(StartFailure::Exited(None)),
            }
            if let Ok(resp) = self.http.get(&url).timeout(Duration::from_secs(2)).send().await {
                // 503 means the model is still loading. Builds without /health (404) only start
                // listening once the model is loaded, so any other answer means ready.
                if resp.status() != reqwest::StatusCode::SERVICE_UNAVAILABLE {
                    return Ok(());
                }
            }
            if Instant::now() >= deadline {
                return Err(StartFailure::Timeout);
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// The server's port, restarting it first if it has died. `true` when it was restarted.
    async fn ensure_running(&self) -> Result<(u16, bool)> {
        let mut guard = self.server.lock().await;
        if let Some(server) = guard.as_mut() {
            match server.child.try_wait() {
                Ok(None) => return Ok((server.port, false)),
                Ok(Some(status)) => log::warn!("the speech engine exited ({status}); restarting it"),
                Err(e) => log::warn!("could not check on the speech engine ({e}); restarting it"),
            }
        } else if self.stopped.load(Ordering::SeqCst) {
            bail!("The speech engine is stopped");
        }
        if let Some(old) = guard.take() {
            log::warn!("last output from the speech engine:\n{}", old.tail().join("\n"));
            let _ = tokio::task::spawn_blocking(move || drop(old)).await;
        }
        self.set_status("starting", "The speech engine stopped unexpectedly; restarting it", None);
        let server = self.launch().await?;
        let port = server.port;
        *guard = Some(server);
        drop(guard);
        self.report_ready();
        Ok((port, true))
    }

    async fn infer(&self, port: u16, wav: &[u8], opts: &TranscribeOptions, duration_ms: u64) -> Result<Transcript> {
        if !self.verbose.load(Ordering::Relaxed) {
            return self.post(port, wav, opts, duration_ms, false).await;
        }
        match self.post(port, wav, opts, duration_ms, true).await {
            Ok(transcript) => Ok(transcript),
            Err(e) if e.downcast_ref::<reqwest::Error>().is_some() => Err(e),
            Err(verbose_error) => match self.post(port, wav, opts, duration_ms, false).await {
                Ok(transcript) => {
                    log::warn!("this speech engine cannot give timings ({verbose_error:#}); using plain text replies");
                    self.verbose.store(false, Ordering::Relaxed);
                    Ok(transcript)
                }
                Err(_) => Err(verbose_error),
            },
        }
    }

    async fn post(&self, port: u16, wav: &[u8], opts: &TranscribeOptions, duration_ms: u64, verbose: bool) -> Result<Transcript> {
        let language = match opts.language.trim() {
            "" => self.settings.language.trim().to_string(),
            other => other.to_string(),
        };
        let file = reqwest::multipart::Part::bytes(wav.to_vec())
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .context("Could not prepare the audio for the speech engine")?;
        let mut form = reqwest::multipart::Form::new()
            .part("file", file)
            .text("response_format", if verbose { "verbose_json" } else { "json" })
            .text("temperature", "0.0");
        if !language.is_empty() {
            form = form.text("language", language);
        }
        let prompt = truncate_chars(opts.prompt.trim(), MAX_PROMPT_CHARS);
        if !prompt.is_empty() {
            form = form.text("prompt", prompt.to_string());
        }
        // Generous: long ramble chunks on a CPU can take a while.
        let timeout = Duration::from_secs(30) + Duration::from_millis(duration_ms.saturating_mul(3));
        let started = Instant::now();
        let response = self
            .http
            .post(format!("http://127.0.0.1:{port}/inference"))
            .multipart(form)
            .timeout(timeout)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        let ms = started.elapsed().as_millis() as u64;
        if !status.is_success() {
            bail!("The speech engine could not transcribe this audio (HTTP {}): {}", status.as_u16(), truncate_chars(body.trim(), 200));
        }
        parse_reply(&body, ms, duration_ms)
    }

    /// A throwaway inference so the first real take does not pay for CUDA's first-run set-up.
    /// It uses a few seconds of real speech shipped for the purpose (sounds/warmup.wav, never
    /// played; VAD keeps speech) and a vocabulary prompt, so a normal take's decoder paths run too.
    async fn warm_up(&self, port: u16) {
        let mut speech = match audio::read_wav_16k(&self.paths.sounds.join("warmup.wav")) {
            Ok(samples) if !samples.is_empty() => samples,
            _ => {
                log::debug!("no warm-up clip in {}; skipping warm-up", self.paths.sounds.display());
                return;
            }
        };
        speech.resize(speech.len().max(MIN_SAMPLES), 0.0);
        let Ok(wav) = audio::wav_bytes(&speech) else { return };
        let duration_ms = speech.len() as u64 * 1000 / audio::SAMPLE_RATE as u64;
        let opts = TranscribeOptions { language: String::new(), prompt: "Navi, Claude Code.".into() };
        let started = Instant::now();
        match self.post(port, &wav, &opts, duration_ms, self.verbose.load(Ordering::Relaxed)).await {
            Ok(_) => log::debug!("speech engine warm-up took {} ms", started.elapsed().as_millis()),
            Err(e) => log::debug!("speech engine warm-up failed: {e:#}"),
        }
    }
}

#[derive(Deserialize)]
struct Reply {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    segments: Option<Vec<ReplySegment>>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
struct ReplySegment {
    #[serde(default)]
    start: f64,
    #[serde(default)]
    end: f64,
    #[serde(default)]
    text: String,
}

/// Parses a verbose_json or json reply from whisper-server.
/// The clip as WAV bytes (padded to MIN_SAMPLES, which the server needs) and its length in ms.
fn wav_for(samples_16k: &[f32]) -> Result<(Vec<u8>, u64)> {
    let duration_ms = samples_16k.len() as u64 * 1000 / audio::SAMPLE_RATE as u64;
    let wav = if samples_16k.len() < MIN_SAMPLES {
        let mut padded = samples_16k.to_vec();
        padded.resize(MIN_SAMPLES, 0.0);
        audio::wav_bytes(&padded)?
    } else {
        audio::wav_bytes(samples_16k)?
    };
    Ok((wav, duration_ms))
}

fn parse_reply(body: &str, ms: u64, duration_ms: u64) -> Result<Transcript> {
    let reply: Reply = serde_json::from_str(body)
        .map_err(|e| anyhow!("The speech engine sent a reply Navi could not read: {e}"))?;
    if let Some(error) = reply.error {
        bail!("The speech engine could not transcribe this audio: {error}");
    }
    match reply.segments {
        Some(segments) => {
            let kept: Vec<&ReplySegment> = segments.iter().filter(|s| !is_non_speech(&s.text)).collect();
            // Segment texts carry their own leading spaces (none for languages without spaces),
            // so concatenating keeps whisper's own spacing.
            let text = collapse_whitespace(&kept.iter().map(|s| s.text.as_str()).collect::<String>());
            let segments = kept
                .iter()
                .filter_map(|s| {
                    let text = collapse_whitespace(&s.text);
                    if text.is_empty() {
                        return None;
                    }
                    let start_ms = ((s.start.max(0.0) * 1000.0).round() as u64).min(duration_ms);
                    let end_ms = ((s.end.max(0.0) * 1000.0).round() as u64).clamp(start_ms, duration_ms.max(start_ms));
                    Some(Segment { start_ms, end_ms, text })
                })
                .collect();
            Ok(Transcript { text, segments, ms })
        }
        None => {
            let raw = reply.text.ok_or_else(|| anyhow!("The speech engine sent an empty reply"))?;
            // Plain json separates segments with newlines; each segment brings its own space.
            let text: String = raw.split('\n').filter(|line| !is_non_speech(line)).collect();
            Ok(Transcript { text: collapse_whitespace(&text), segments: Vec::new(), ms })
        }
    }
}

/// Puts a readable message in front of connection-level failures (the cause stays in the chain).
fn readable(e: anyhow::Error) -> anyhow::Error {
    let timed_out = match e.downcast_ref::<reqwest::Error>() {
        Some(transport) => transport.is_timeout(),
        None => return e,
    };
    if timed_out {
        e.context("The speech engine took too long to answer")
    } else {
        e.context("The speech engine did not answer")
    }
}

/// Whole segments such as "[BLANK_AUDIO]" or "[Music]" are whisper's non-speech tags.
fn is_non_speech(segment: &str) -> bool {
    let t = segment.trim();
    (t.starts_with('[') && t.ends_with(']')) || t.eq_ignore_ascii_case("(silence)")
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((index, _)) => &text[..index],
        None => text,
    }
}

/// The flag named in whisper-server's "error: unknown argument: -nlp" line, if there is one.
fn rejected_flag(log: &[String]) -> Option<String> {
    log.iter().find_map(|line| {
        // ASCII lowercasing keeps byte offsets, so `at` is valid in `line` too.
        let lower = line.to_ascii_lowercase();
        let at = lower.find("unknown argument:")?;
        let flag = line[at + "unknown argument:".len()..].trim();
        (!flag.is_empty()).then(|| flag.to_string())
    })
}

fn model_failed_to_load(log: &str) -> bool {
    log.contains("failed to load model")
        || log.contains("failed to initialize whisper context")
        || log.contains("failed to open")
        || log.contains("invalid model")
        || log.contains("bad magic")
}

fn explain_exit(log: &str, model_name: &str) -> String {
    if model_failed_to_load(log) {
        format!("The {model_name} model file could not be loaded. It may be damaged; download it again in Settings.")
    } else if log.contains("out of memory") {
        "The speech engine ran out of memory. Try a smaller model in Settings.".to_string()
    } else {
        let last = log.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
        if last.is_empty() {
            "The speech engine stopped while starting".to_string()
        } else {
            format!("The speech engine stopped while starting: {}", truncate_chars(&last, 160))
        }
    }
}

fn spawn_log_reader<R: Read + Send + 'static>(stream: R, log: Arc<Mutex<VecDeque<String>>>, done: Arc<AtomicUsize>) {
    let spawned = std::thread::Builder::new().name("navi-engine-log".into()).spawn({
        let done = done.clone();
        move || {
            let mut reader = BufReader::new(stream);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&buf).trim_end().to_string();
                        // What was said never goes in the tail, which is logged when the engine
                        // stops: some builds print every segment they transcribe.
                        if line.is_empty() || is_transcript_line(&line) {
                            continue;
                        }
                        log::trace!("whisper-server: {line}");
                        let mut tail = log.lock();
                        if tail.len() >= LOG_TAIL_LINES {
                            tail.pop_front();
                        }
                        tail.push_back(line);
                    }
                }
            }
            done.fetch_add(1, Ordering::SeqCst);
        }
    });
    if spawned.is_err() {
        done.fetch_add(1, Ordering::SeqCst);
    }
}

/// A transcribed segment as whisper.cpp prints it, "[00:00:01.000 --> 00:00:03.500]   Hello there",
/// which some server builds do for every request.
fn is_transcript_line(line: &str) -> bool {
    let Some((stamps, _)) = line.trim_start().strip_prefix('[').and_then(|rest| rest.split_once(']')) else { return false };
    let is_time = |t: &str| {
        let t = t.trim();
        t.contains(':') && t.chars().all(|c| c.is_ascii_digit() || matches!(c, ':' | '.' | ','))
    };
    stamps.split_once("-->").is_some_and(|(from, to)| is_time(from) && is_time(to))
}

fn free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).context("No free local port for the speech engine")?;
    Ok(listener.local_addr()?.port())
}

/// A CUDA build ships its CUDA backend (or the cuBLAS runtime) next to the executable. On a Mac
/// every whisper.cpp build (Homebrew's, or one made by scripts/mac-setup.sh) has Metal built in.
fn has_cuda_backend(exe: &Path) -> bool {
    if cfg!(target_os = "macos") {
        return true;
    }
    let Some(dir) = exe.parent() else { return false };
    ["ggml-cuda.dll", "cublas64_12.dll", "cublas64_11.dll", "cublas64_13.dll"]
        .iter()
        .any(|name| dir.join(name).is_file())
}

/// About half the physical cores, between 2 and 8: the GPU does the heavy lifting, and on a CPU
/// more threads than this mostly fight over memory bandwidth.
fn default_threads() -> usize {
    let logical = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let physical = physical_cores().unwrap_or((logical / 2).max(1));
    (physical / 2).clamp(2, 8)
}

#[cfg(windows)]
fn physical_cores() -> Option<usize> {
    use windows::Win32::System::SystemInformation::{
        GetLogicalProcessorInformationEx, RelationProcessorCore, SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
    };
    let mut len = 0u32;
    // The first call fails with ERROR_INSUFFICIENT_BUFFER and reports the size needed.
    let _ = unsafe { GetLogicalProcessorInformationEx(RelationProcessorCore, None, &mut len) };
    if len == 0 {
        return None;
    }
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    unsafe {
        GetLogicalProcessorInformationEx(
            RelationProcessorCore,
            Some(buf.as_mut_ptr().cast::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>()),
            &mut len,
        )
        .ok()?;
    }
    let bytes: &[u8] = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), (len as usize).min(buf.len() * 8)) };
    // Variable-length records, each starting with Relationship: u32 and Size: u32.
    let mut cores = 0;
    let mut offset = 0;
    while offset + 8 <= bytes.len() {
        let size = u32::from_ne_bytes([bytes[offset + 4], bytes[offset + 5], bytes[offset + 6], bytes[offset + 7]]) as usize;
        if size == 0 {
            break;
        }
        cores += 1;
        offset += size;
    }
    (cores > 0).then_some(cores)
}

/// The performance cores on Apple silicon (the efficiency cores slow whisper.cpp down), else every
/// physical core.
#[cfg(target_os = "macos")]
fn physical_cores() -> Option<usize> {
    ["hw.perflevel0.physicalcpu", "hw.physicalcpu"]
        .iter()
        .find_map(|name| models::sysctl_string(name).and_then(|v| v.parse::<usize>().ok()))
        .filter(|&n| n > 0)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn physical_cores() -> Option<usize> {
    None
}

#[cfg(windows)]
mod job {
    use anyhow::Result;
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// A job object that kills every process in it when its last handle closes, which includes
    /// Navi exiting or crashing.
    pub struct JobHandle(HANDLE);

    // A job handle is a kernel handle; it can be used and closed from any thread.
    unsafe impl Send for JobHandle {}
    unsafe impl Sync for JobHandle {}

    impl JobHandle {
        pub fn kill_on_close() -> Result<Self> {
            let handle = unsafe { CreateJobObjectW(None, windows::core::PCWSTR::null())? };
            let job = JobHandle(handle);
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )?;
            }
            Ok(job)
        }

        pub fn assign(&self, child: &std::process::Child) -> Result<()> {
            unsafe { AssignProcessToJobObject(self.0, HANDLE(child.as_raw_handle()))? };
            Ok(())
        }
    }

    impl Drop for JobHandle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

/// macOS has no job objects, so a speech engine left behind when Navi crashed (the release build
/// aborts on panic) is found by the process id noted at launch and stopped when the next Navi
/// starts its first engine.
#[cfg(target_os = "macos")]
mod job {
    use std::sync::Once;

    pub struct JobHandle;

    fn pid_file() -> std::path::PathBuf {
        std::env::temp_dir().join("com.goodlad.navi.whisper-server.pid")
    }

    fn stop_leftover() {
        let Ok(text) = std::fs::read_to_string(pid_file()) else { return };
        let _ = std::fs::remove_file(pid_file());
        for pid in text.lines().filter_map(|l| l.trim().parse::<i32>().ok()) {
            let Ok(out) = std::process::Command::new("/bin/ps").args(["-p", &pid.to_string(), "-o", "comm="]).output() else { continue };
            if String::from_utf8_lossy(&out.stdout).trim().ends_with("whisper-server") {
                log::info!("stopping a speech engine left running by an earlier Navi (pid {pid})");
                let _ = std::process::Command::new("/bin/kill").arg(pid.to_string()).status();
            }
        }
    }

    impl JobHandle {
        pub fn kill_on_close() -> anyhow::Result<Self> {
            static ONCE: Once = Once::new();
            ONCE.call_once(stop_leftover);
            Ok(JobHandle)
        }

        /// Notes the server's pid (the main and the quick server each add theirs).
        pub fn assign(&self, child: &std::process::Child) -> anyhow::Result<()> {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new().create(true).append(true).open(pid_file())?;
            writeln!(file, "{}", child.id())?;
            Ok(())
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod job {
    pub struct JobHandle;

    impl JobHandle {
        pub fn kill_on_close() -> anyhow::Result<Self> {
            Ok(JobHandle)
        }

        pub fn assign(&self, _child: &std::process::Child) -> anyhow::Result<()> {
            Ok(())
        }
    }
}

/// Compile-time checks: the services can be shared across threads and their futures spawned on
/// tokio. Never called.
#[allow(dead_code)]
fn assert_thread_safety(engine: &Engine, settings: &EngineSettings, paths: &Paths, on_status: StatusCallback) {
    fn is_send<T: Send>(_: &T) {}
    fn is_shareable<T: Send + Sync>() {}
    is_shareable::<Engine>();
    is_shareable::<crate::audio::AudioService>();
    is_shareable::<crate::sounds::SoundService>();
    is_send(&Engine::start(settings, paths, on_status));
    let opts = TranscribeOptions::default();
    is_send(&engine.transcribe(&[], &opts));
    is_send(&engine.shutdown());
    is_send(&models::download_model(models::VAD_ID, paths, Arc::new(|_: crate::types::DownloadProgress| {})));
    is_send(&models::download_engine(paths, true, Arc::new(|_: crate::types::DownloadProgress| {})));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_verbose_replies() {
        let body = r#"{"task":"transcribe","language":"english","duration":10.0,
            "text":" you don't have the code for\n and it works\n",
            "segments":[
                {"id":0,"text":" you don't have the code for","start":0.07,"end":3.17},
                {"id":1,"text":" [BLANK_AUDIO]","start":3.17,"end":4.0},
                {"id":2,"text":" and it works","start":4.0,"end":12.5}
            ]}"#;
        let t = parse_reply(body, 217, 10_000).expect("parse");
        assert_eq!(t.text, "you don't have the code for and it works");
        assert_eq!(t.segments.len(), 2);
        assert_eq!(t.segments[0].start_ms, 70);
        assert_eq!(t.segments[0].end_ms, 3_170);
        assert_eq!(t.segments[1].end_ms, 10_000, "padding beyond the real audio is clamped");
        assert_eq!(t.ms, 217);
    }

    #[test]
    fn parses_plain_replies_and_errors() {
        let t = parse_reply(r#"{"text":" Hello there,\n how are you?\n"}"#, 5, 2_000).expect("parse");
        assert_eq!(t.text, "Hello there, how are you?");
        assert!(t.segments.is_empty());
        let cjk = parse_reply(r#"{"text":"你好\n世界\n"}"#, 5, 2_000).expect("parse");
        assert_eq!(cjk.text, "你好世界", "no spaces invented between segments");
        let silent = parse_reply(r#"{"text":" [BLANK_AUDIO]\n"}"#, 5, 2_000).expect("parse");
        assert_eq!(silent.text, "");
        assert!(parse_reply(r#"{"error":"failed to read audio data"}"#, 5, 2_000).is_err());
        assert!(parse_reply("not json", 5, 2_000).is_err());
    }

    #[test]
    fn helpers_behave() {
        assert_eq!(truncate_chars("héllo", 2), "hé");
        assert_eq!(truncate_chars("hi", 10), "hi");
        assert!(is_non_speech(" [Music] "));
        assert!(!is_non_speech("[sic] is Latin"));
        let threads = default_threads();
        assert!((2..=8).contains(&threads));
        println!("physical cores {:?}, threads {threads}", physical_cores());
        assert!(explain_exit("error: failed to load model", "Large v3 Turbo").contains("damaged"));

        // The real server prints the error on line 5, then ~60 lines of usage (checked against
        // a real whisper.cpp build), so the flag must survive in the tail.
        let mut log: Vec<String> = vec!["ggml_cuda_init: found 1 CUDA devices".into(), "error: unknown argument: -nlp".into()];
        log.extend((0..60).map(|i| format!("  -o{i}, --option-{i} [false] mentions --vad and bind")));
        let tail: Vec<String> = log.iter().rev().take(LOG_TAIL_LINES).rev().cloned().collect();
        assert_eq!(rejected_flag(&tail).as_deref(), Some("-nlp"));
        assert_eq!(rejected_flag(&["all good".to_string()]), None);
    }

    #[test]
    fn transcribed_words_never_go_in_the_log_tail() {
        assert!(is_transcript_line("[00:00:00.000 --> 00:00:02.500]   See you on Tuesday."));
        assert!(is_transcript_line("  [00:01:02,040 --> 00:01:04,000]  Hi"));
        // The server's own lines stay, so a failure can still be explained.
        for line in [
            "whisper_init_from_file_with_params_no_state: loading model from 'ggml-base.en.bin'",
            "error: unknown argument: -nlp",
            "[INFO] listening on 127.0.0.1:8080",
            "Received request: audio.wav",
            "whisper_full_with_state: progress = 50%",
        ] {
            assert!(!is_transcript_line(line), "{line}");
        }
    }

    /// Closing the job handle (what happens when Navi exits or crashes) kills the child.
    #[cfg(windows)]
    #[test]
    fn job_object_kills_child_on_close() {
        use std::os::windows::process::CommandExt;
        let job = job::JobHandle::kill_on_close().expect("job object");
        let mut child = Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("spawn ping");
        job.assign(&child).expect("assign to job");
        assert!(child.try_wait().expect("try_wait").is_none(), "child should be running");
        drop(job);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut exited = false;
        while Instant::now() < deadline {
            if child.try_wait().expect("try_wait").is_some() {
                exited = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if !exited {
            let _ = child.kill();
        }
        assert!(exited, "closing the job did not kill the child");
    }

    fn test_settings() -> EngineSettings {
        let home = dirs::home_dir().expect("home");
        EngineSettings {
            model: "large-v3-turbo".into(),
            language: "en".into(),
            use_gpu: true,
            // A Mac uses whichever engine Navi finds (Homebrew's or scripts/mac-setup.sh's).
            server_path: if cfg!(target_os = "macos") { None } else { Some(home.join(r".local\tools\whisper-cuda\Release\whisper-server.exe").to_string_lossy().into_owned()) },
            model_path: Some(home.join(".local").join("share").join("whisper").join("ggml-large-v3-turbo.bin").to_string_lossy().into_owned()),
            vad: true,
        }
    }

    /// 10 s of a spoken recording (20 s to 30 s in), read with hound and resampled by
    /// audio::resample. Set NAVI_SAMPLE_WAV to any WAV of clear speech at least 30 s long.
    fn narration_slice() -> Vec<f32> {
        let path = std::env::var("NAVI_SAMPLE_WAV").expect("set NAVI_SAMPLE_WAV to a WAV of speech, 30 s or longer");
        let src = Path::new(&path);
        let mut reader = hound::WavReader::open(src).expect("open narration");
        let spec = reader.spec();
        reader.seek(spec.sample_rate * 20).expect("seek");
        let frames = (spec.sample_rate * 10) as usize;
        let channels = spec.channels as usize;
        let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f64;
        let interleaved: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Float => reader.samples::<f32>().take(frames * channels).map(|s| s.expect("sample")).collect(),
            hound::SampleFormat::Int => reader
                .samples::<i32>()
                .take(frames * channels)
                .map(|s| (s.expect("sample") as f64 * scale) as f32)
                .collect(),
        };
        let mono: Vec<f32> = interleaved.chunks_exact(channels).map(|f| f.iter().sum::<f32>() / channels as f32).collect();
        audio::resample(&mono, spec.sample_rate, audio::SAMPLE_RATE).expect("resample")
    }

    fn median(mut v: Vec<u64>) -> u64 {
        v.sort_unstable();
        v[v.len() / 2]
    }

    /// Starts the real server on the RTX 5070, transcribes 10 s of speech, checks the words and
    /// reports latency, then shuts it down. Run by hand:
    /// cargo test --lib engine::tests::end_to_end -- --ignored --nocapture
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn end_to_end() {
        let paths = Paths::for_tests("engine-e2e");
        // Silence trimming needs the Silero model: fetch it (885 KB) into the test folder.
        let vad = models::download_model("vad", &paths, Arc::new(|_: crate::types::DownloadProgress| {}))
            .await
            .expect("vad download");
        assert!(vad.is_file());

        let statuses = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = statuses.clone();
        let on_status: StatusCallback = Arc::new(move |s: EngineStatus| {
            println!("status: {} / {} / device {} / gpu {:?}", s.state, s.message, s.device, s.gpu);
            seen.lock().push(s.state);
        });
        let started = Instant::now();
        let engine = Engine::start(&test_settings(), &paths, on_status).await.expect("engine start");
        println!("start-up including warm-up: {} ms", started.elapsed().as_millis());
        assert_eq!(engine.status().state, "ready");
        assert_eq!(engine.status().device, if cfg!(target_os = "macos") { "metal" } else { "cuda" });

        let slice = narration_slice();
        assert_eq!(slice.len(), 160_000);
        let opts = TranscribeOptions { language: "en".into(), prompt: "Claude Code.".into() };

        let mut engine_ms = Vec::new();
        let mut wall_ms = Vec::new();
        let mut text = String::new();
        for _ in 0..6 {
            let t = Instant::now();
            let transcript = engine.transcribe(&slice, &opts).await.expect("transcribe");
            wall_ms.push(t.elapsed().as_millis() as u64);
            engine_ms.push(transcript.ms);
            text = transcript.text;
            assert!(!transcript.segments.is_empty(), "verbose_json gives segments");
        }
        println!("text: {text}");
        println!("10 s clip, engine ms per run: {engine_ms:?}; median {} ms (wall median {} ms)", median(engine_ms.clone()), median(wall_ms));
        let lower = text.to_lowercase();
        for word in ["code", "evidence", "six levels", "question"] {
            assert!(lower.contains(word), "missing '{word}' in: {text}");
        }

        // A sub-second clip and pure silence.
        let short = engine.transcribe(&slice[..12_000], &opts).await.expect("short");
        println!("0.75 s clip: '{}' in {} ms", short.text, short.ms);
        assert!(short.text.to_lowercase().contains("have"), "short clip: {}", short.text);
        let silence = engine.transcribe(&vec![0.0f32; 32_000], &opts).await.expect("silence");
        println!("2 s of silence: '{}' in {} ms", silence.text, silence.ms);
        assert!(silence.text.is_empty(), "silence should give no words with VAD on: {}", silence.text);

        let port = engine.port().await.expect("port");
        engine.shutdown().await;
        assert_eq!(engine.status().state, "stopped");
        assert!(engine.transcribe(&slice, &opts).await.is_err(), "no restart after an explicit shutdown");
        let client = reqwest::Client::builder().no_proxy().build().expect("client");
        assert!(client.get(format!("http://127.0.0.1:{port}/health")).send().await.is_err(), "server should be gone");
        let states = statuses.lock().clone();
        assert!(states.contains(&"starting".to_string()) && states.contains(&"ready".to_string()));
        let _ = std::fs::remove_dir_all(&paths.data);
    }

    /// Kills the server behind the engine's back and checks the next transcribe restarts it.
    /// cargo test --lib engine::tests::restarts_after_crash -- --ignored --nocapture
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn restarts_after_crash() {
        let paths = Paths::for_tests("engine-crash");
        let mut settings = test_settings();
        settings.vad = false;
        let engine = Engine::start(&settings, &paths, Arc::new(|_: EngineStatus| {})).await.expect("engine start");
        let first_port = engine.port().await.expect("port");
        {
            let mut guard = engine.inner.server.lock().await;
            let server = guard.as_mut().expect("server");
            server.child.kill().expect("kill");
            server.child.wait().expect("wait");
        }
        let slice = narration_slice();
        let t = Instant::now();
        let transcript = engine.transcribe(&slice, &TranscribeOptions::default()).await.expect("transcribe after crash");
        println!("restarted on port {:?} (was {first_port}) and transcribed in {} ms: {}", engine.port().await, t.elapsed().as_millis(), transcript.text);
        assert!(transcript.text.to_lowercase().contains("evidence"));
        engine.shutdown().await;
        let _ = std::fs::remove_dir_all(&paths.data);
    }
}
