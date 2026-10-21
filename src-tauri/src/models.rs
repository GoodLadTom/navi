//! Speech models and the engine binaries: catalogue, discovery, downloads, GPU detection.
//!
//! OWNER: audio and engine agent. STATUS: implemented.
//!
//! Contract:
//! * `catalogue()`: whisper.cpp GGML models from huggingface.co/ggerganov/whisper.cpp (resolve URLs
//!   like https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin):
//!   large-v3-turbo (recommended, 99 languages), large-v3-turbo-q5_0 (smaller, nearly as good),
//!   small.en, base.en, plus the Silero VAD model (ggml-silero-v5.1.2.bin from
//!   huggingface.co/ggml-org/whisper-vad) under id "vad". Fill `installed`/`path` from disk.
//! * `find_model(settings, paths)`: settings.model_path if it exists (and is not the catalogue file
//!   of a model other than settings.model), else Paths::models/<file>, else known locations
//!   where other whisper.cpp tools may have left it: %USERPROFILE%\.local\share\whisper\<file>.
//! * `find_server(settings, paths)`: settings.server_path, else Paths::engine (recursive search for
//!   whisper-server.exe), else %USERPROFILE%\.local\tools\whisper-cuda\Release\whisper-server.exe,
//!   else whisper-server.exe on PATH.
//! * `download_model` / `download_engine`: stream to a .part file with progress callbacks (throttle
//!   to ~10 per second), verify size (and SHA-256 when known), then rename into place. The engine
//!   comes from the latest github.com/ggml-org/whisper.cpp release (GitHub API): the CUDA 12 x64 zip
//!   when an NVIDIA GPU is present, else the plain x64 (or BLAS) zip; extract into Paths::engine.
//! * `detect_gpu()`: nvidia-smi --query-gpu=name,memory.total --format=csv,noheader (hidden
//!   window). None if not available.
//!
//! Implementation notes:
//! * Every catalogue entry carries its exact size and SHA-256 (from the Hugging Face API), and
//!   both are checked before the file is renamed into place. An interrupted download resumes from
//!   its .part file with an HTTP Range request.
//! * whisper.cpp's newest "vX.Y.Z" release often has no binaries: Windows builds are attached to
//!   the "bNNNN" build releases, and the CUDA asset was renamed in 2026 from
//!   whisper-cublas-12.4.0-bin-x64.zip to whisper-bin-win-cuda-12.4.0-x64.zip. `find_engine_release`
//!   walks the recent releases newest first and accepts both names. GitHub's SHA-256 digest is
//!   verified when present.
//! * The engine is extracted into Paths::engine/<cuda|cpu>-<tag>, recorded in Paths::engine/
//!   current.txt (which `find_server` reads first); older builds are removed unless still running.
//! * `download_engine` returns the path of the new whisper-server.exe.

use crate::paths::Paths;
use crate::types::{DownloadProgress, EngineSettings, ModelInfo};
use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;

pub type ProgressCallback = Arc<dyn Fn(DownloadProgress) + Send + Sync>;

pub const VAD_ID: &str = "vad";
#[cfg(windows)]
const SERVER_EXE: &str = "whisper-server.exe";
#[cfg(not(windows))]
const SERVER_EXE: &str = "whisper-server";
const ENGINE_MARKER: &str = "current.txt";
const RELEASES_URL: &str = "https://api.github.com/repos/ggml-org/whisper.cpp/releases?per_page=20";
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Debug, Default)]
pub struct GpuInfo {
    pub name: String,
    pub vram_mb: u64,
}

impl GpuInfo {
    /// "NVIDIA GeForce RTX 5070 (12 GB)".
    pub fn label(&self) -> String {
        if self.vram_mb == 0 {
            return self.name.clone();
        }
        format!("{} ({} GB)", self.name, (self.vram_mb as f64 / 1024.0).round() as u64)
    }
}

struct Entry {
    id: &'static str,
    name: &'static str,
    file: &'static str,
    url: &'static str,
    bytes: u64,
    sha256: &'static str,
    languages: &'static str,
    speed: u8,
    accuracy: u8,
    recommended: bool,
}

const CATALOGUE: &[Entry] = &[
    Entry {
        id: "large-v3-turbo",
        name: "Large v3 Turbo",
        file: "ggml-large-v3-turbo.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin",
        bytes: 1_624_555_275,
        sha256: "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69",
        languages: "99 languages",
        speed: 3,
        accuracy: 5,
        recommended: true,
    },
    Entry {
        id: "large-v3-turbo-q5_0",
        name: "Large v3 Turbo (compact)",
        file: "ggml-large-v3-turbo-q5_0.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin",
        bytes: 574_041_195,
        sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
        languages: "99 languages",
        speed: 4,
        accuracy: 4,
        recommended: false,
    },
    Entry {
        id: "small.en",
        name: "Small (English)",
        file: "ggml-small.en.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin",
        bytes: 487_614_201,
        sha256: "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d",
        languages: "English",
        speed: 4,
        accuracy: 3,
        recommended: false,
    },
    Entry {
        id: "base.en",
        name: "Base (English)",
        file: "ggml-base.en.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin",
        bytes: 147_964_211,
        sha256: "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002",
        languages: "English",
        speed: 5,
        accuracy: 2,
        recommended: false,
    },
    Entry {
        id: VAD_ID,
        name: "Silence trimming (Silero VAD)",
        file: "ggml-silero-v5.1.2.bin",
        url: "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin",
        bytes: 885_098,
        sha256: "29940d98d42b91fbd05ce489f3ecf7c72f0a42f027e4875919a28fb4c04ea2cf",
        languages: "Any language",
        speed: 5,
        accuracy: 0,
        recommended: false,
    },
];

fn entry(id: &str) -> Option<&'static Entry> {
    CATALOGUE.iter().find(|e| e.id == id)
}

/// Friendly name for a catalogue id ("Large v3 Turbo"); unknown ids are returned as they are.
pub fn display_name(id: &str) -> String {
    entry(id).map(|e| e.name.to_string()).unwrap_or_else(|| id.to_string())
}

pub fn catalogue(paths: &Paths, settings: &EngineSettings) -> Vec<ModelInfo> {
    CATALOGUE
        .iter()
        .map(|e| {
            let path = if e.id == VAD_ID {
                find_vad_model(paths)
            } else if e.id == settings.model {
                find_model(settings, paths)
            } else {
                locate_file(e.file, paths)
            };
            ModelInfo {
                id: e.id.to_string(),
                name: e.name.to_string(),
                file: e.file.to_string(),
                url: e.url.to_string(),
                size_mb: ((e.bytes + 500_000) / 1_000_000).max(1) as u32,
                languages: e.languages.to_string(),
                speed: e.speed,
                accuracy: e.accuracy,
                recommended: e.recommended,
                installed: path.is_some(),
                path: path.map(|p| p.to_string_lossy().into_owned()),
            }
        })
        .collect()
}

/// %USERPROFILE%\.local\share\whisper, where some other whisper.cpp tools keep their models.
fn shared_model_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".local").join("share").join("whisper"))
}

fn locate_file(file: &str, paths: &Paths) -> Option<PathBuf> {
    let managed = paths.models.join(file);
    if managed.is_file() {
        return Some(managed);
    }
    shared_model_dir().map(|dir| dir.join(file)).filter(|p| p.is_file())
}

fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim).filter(|v| !v.is_empty())
}

pub fn find_model(settings: &EngineSettings, paths: &Paths) -> Option<PathBuf> {
    if let Some(explicit) = non_empty(&settings.model_path) {
        let path = PathBuf::from(explicit);
        if let Some(other) = catalogue_model_of(&path).filter(|id| *id != settings.model.trim()) {
            // Recorded for another model (setup saves the path it found); choosing a model in
            // Settings changes only the id, and the id is what the interface and History show.
            log::debug!("ignoring the model file {}: it is {other}, not {}", path.display(), settings.model);
        } else if path.is_file() {
            return Some(path);
        } else {
            log::warn!("the model file {} does not exist; looking in the usual places", path.display());
        }
    }
    let file = entry(&settings.model)
        .filter(|e| e.id != VAD_ID)
        .map(|e| e.file.to_string())
        .unwrap_or_else(|| format!("ggml-{}.bin", settings.model.trim()));
    locate_file(&file, paths)
}

/// The catalogue speech model whose file this is, going by its name. None for any other file.
fn catalogue_model_of(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.to_str()?;
    CATALOGUE.iter().find(|e| e.id != VAD_ID && e.file.eq_ignore_ascii_case(name)).map(|e| e.id)
}

pub fn find_vad_model(paths: &Paths) -> Option<PathBuf> {
    let file = entry(VAD_ID).map(|e| e.file).unwrap_or("ggml-silero-v5.1.2.bin");
    if let Some(path) = locate_file(file, paths) {
        return Some(path);
    }
    // Any other Silero release dropped into a known folder works the same way.
    let mut dirs = vec![paths.models.clone()];
    dirs.extend(shared_model_dir());
    for dir in dirs {
        let Ok(read) = std::fs::read_dir(&dir) else { continue };
        let mut found: Vec<PathBuf> = read
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with("ggml-silero-") && n.ends_with(".bin"))
                        .unwrap_or(false)
            })
            .collect();
        found.sort();
        if let Some(last) = found.pop() {
            return Some(last);
        }
    }
    None
}

pub fn find_server(settings: &EngineSettings, paths: &Paths) -> Option<PathBuf> {
    if let Some(explicit) = non_empty(&settings.server_path) {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
        log::warn!("the engine {} does not exist; looking in the usual places", path.display());
    }
    if let Some(managed) = managed_server(&paths.engine) {
        return Some(managed);
    }
    if let Some(home) = dirs::home_dir() {
        let known = home.join(".local").join("tools").join("whisper-cuda").join("Release").join(SERVER_EXE);
        if known.is_file() {
            return Some(known);
        }
    }
    // A Mac app opened from Finder gets a short PATH, so the usual install folders are checked by
    // name: the build scripts/mac-setup.sh makes first, then Homebrew's whisper-cpp.
    #[cfg(target_os = "macos")]
    {
        let mut known: Vec<PathBuf> = Vec::new();
        if let Some(home) = dirs::home_dir() {
            known.push(home.join(".local").join("tools").join("whisper.cpp").join("build").join("bin").join(SERVER_EXE));
            known.push(home.join(".local").join("bin").join(SERVER_EXE));
        }
        known.push(PathBuf::from("/opt/homebrew/bin").join(SERVER_EXE));
        known.push(PathBuf::from("/usr/local/bin").join(SERVER_EXE));
        if let Some(found) = known.into_iter().find(|p| p.is_file()) {
            return Some(found);
        }
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).map(|dir| dir.join(SERVER_EXE)).find(|p| p.is_file())
    })
}

/// The engine `download_engine` installed last, else the newest whisper-server.exe in the folder.
fn managed_server(engine_dir: &Path) -> Option<PathBuf> {
    if let Ok(recorded) = std::fs::read_to_string(engine_dir.join(ENGINE_MARKER)) {
        let path = engine_dir.join(recorded.trim());
        if !recorded.trim().is_empty() && path.is_file() {
            return Some(path);
        }
    }
    let mut found = Vec::new();
    collect_servers(engine_dir, 4, &mut found);
    found
        .into_iter()
        .max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
}

fn collect_servers(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    for item in read.flatten() {
        let path = item.path();
        let Ok(kind) = item.file_type() else { continue };
        if kind.is_dir() {
            let hidden = path.file_name().and_then(|n| n.to_str()).map(|n| n.starts_with('.')).unwrap_or(false);
            if depth > 0 && !hidden {
                collect_servers(&path, depth - 1, found);
            }
        } else if path.file_name().map(|n| n.eq_ignore_ascii_case(SERVER_EXE)).unwrap_or(false) {
            found.push(path);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Downloads
// ---------------------------------------------------------------------------------------------

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("Navi/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(60))
        .build()
        .context("Could not set up downloads")
}

fn report(progress: &ProgressCallback, id: &str, received: u64, total: u64, state: &str, message: impl Into<String>) {
    progress(DownloadProgress { id: id.to_string(), received, total, state: state.to_string(), message: message.into() });
}

pub async fn download_model(id: &str, paths: &Paths, progress: ProgressCallback) -> Result<PathBuf> {
    let result = download_model_inner(id, paths, &progress).await;
    if let Err(e) = &result {
        report(&progress, id, 0, 0, "error", format!("{e:#}"));
    }
    result
}

async fn download_model_inner(id: &str, paths: &Paths, progress: &ProgressCallback) -> Result<PathBuf> {
    let entry = entry(id).ok_or_else(|| anyhow!("Unknown model '{id}'"))?;
    tokio::fs::create_dir_all(&paths.models)
        .await
        .with_context(|| format!("Could not create {}", paths.models.display()))?;
    let dest = paths.models.join(entry.file);
    if tokio::fs::metadata(&dest).await.map(|m| m.len() == entry.bytes).unwrap_or(false) {
        report(progress, id, entry.bytes, entry.bytes, "done", format!("{} is already downloaded", entry.name));
        return Ok(dest);
    }
    let client = http_client()?;
    download_file(&client, entry.url, &dest, Some(entry.bytes), Some(entry.sha256), id, progress).await?;
    report(progress, id, entry.bytes, entry.bytes, "done", format!("{} is ready", entry.name));
    Ok(dest)
}

/// Streams `url` to `<dest>.part`, resuming an earlier partial download when the server allows,
/// checks size and SHA-256, then renames into place.
async fn download_file(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected_bytes: Option<u64>,
    sha256: Option<&str>,
    id: &str,
    progress: &ProgressCallback,
) -> Result<()> {
    let file_name = dest.file_name().and_then(|n| n.to_str()).unwrap_or("download");
    let part = dest.with_file_name(format!("{file_name}.part"));
    let mut have = tokio::fs::metadata(&part).await.map(|m| m.len()).unwrap_or(0);
    if expected_bytes.is_some_and(|n| have > n) {
        let _ = tokio::fs::remove_file(&part).await;
        have = 0;
    }

    let mut hasher = Sha256::new();
    let mut received = 0u64;
    let complete_part = expected_bytes == Some(have) && have > 0;
    if !complete_part {
        let mut request = client.get(url);
        if have > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
        }
        let response = request.send().await.map_err(|e| network_error(&e))?;
        let status = response.status();
        if !status.is_success() {
            bail!("The download failed (HTTP {}). Try again later.", status.as_u16());
        }
        let resuming = have > 0 && status == reqwest::StatusCode::PARTIAL_CONTENT;
        let total = expected_bytes
            .or_else(|| response.content_length().map(|len| len + if resuming { have } else { 0 }))
            .unwrap_or(0);

        let mut file = if resuming {
            hasher = hash_file(&part).await?;
            received = have;
            tokio::fs::OpenOptions::new().append(true).open(&part).await
        } else {
            tokio::fs::File::create(&part).await
        }
        .with_context(|| format!("Could not write {}", part.display()))?;

        report(progress, id, received, total, "downloading", "Downloading");
        let mut last_report = Instant::now();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| network_error(&e))?;
            file.write_all(&chunk).await.with_context(|| format!("Could not write {}", part.display()))?;
            hasher.update(&chunk);
            received += chunk.len() as u64;
            if expected_bytes.is_some_and(|n| received > n) {
                drop(file);
                let _ = tokio::fs::remove_file(&part).await;
                bail!("The download was larger than expected. Try again later.");
            }
            if last_report.elapsed() >= PROGRESS_INTERVAL {
                report(progress, id, received, total, "downloading", "Downloading");
                last_report = Instant::now();
            }
        }
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        report(progress, id, received, total, "downloading", "Downloading");
    } else {
        hasher = hash_file(&part).await?;
        received = have;
    }

    report(progress, id, received, expected_bytes.unwrap_or(received), "verifying", "Checking the download");
    if let Some(expected) = expected_bytes {
        if received != expected {
            // Short: keep the .part so the next attempt resumes.
            bail!("The download stopped early ({received} of {expected} bytes). Try again to resume it.");
        }
    }
    if let Some(want) = sha256 {
        let got = hex::encode(hasher.finalize());
        if !got.eq_ignore_ascii_case(want) {
            let _ = tokio::fs::remove_file(&part).await;
            bail!("The downloaded file was damaged (checksum mismatch). Try again.");
        }
    }
    tokio::fs::rename(&part, dest)
        .await
        .with_context(|| format!("Could not move the download into place at {}", dest.display()))?;
    Ok(())
}

async fn hash_file(path: &Path) -> Result<Sha256> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<Sha256> {
        use std::io::Read;
        let mut file = std::fs::File::open(&path).with_context(|| format!("Could not read {}", path.display()))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(hasher)
    })
    .await
    .map_err(|e| anyhow!("Checking the partial download failed: {e}"))?
}

fn network_error(e: &reqwest::Error) -> anyhow::Error {
    if e.is_timeout() {
        anyhow!("The download timed out. Check your internet connection and try again.")
    } else if e.is_connect() {
        anyhow!("Could not reach the download server. Check your internet connection.")
    } else {
        anyhow!("The download was interrupted ({e}). Try again to resume it.")
    }
}

/// A whisper.cpp Windows build on GitHub.
#[derive(Clone, Debug)]
pub struct EngineAsset {
    pub tag: String,
    pub name: String,
    pub url: String,
    pub size: u64,
    pub sha256: Option<String>,
    /// "cuda" or "cpu".
    pub flavour: &'static str,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    #[serde(default)]
    digest: Option<String>,
}

/// The newest whisper.cpp release that has a Windows x64 build of the wanted flavour.
pub async fn find_engine_release(gpu: bool) -> Result<EngineAsset> {
    let client = http_client()?;
    let response = client
        .get(RELEASES_URL)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| network_error(&e))?;
    let status = response.status();
    if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        bail!("GitHub is limiting requests right now. Try again in an hour.");
    }
    if !status.is_success() {
        bail!("Could not check for the latest engine (HTTP {})", status.as_u16());
    }
    let mut releases: Vec<GhRelease> = response.json().await.context("GitHub sent a release list Navi could not read")?;
    releases.retain(|r| !r.draft);
    // RFC 3339 timestamps sort correctly as text.
    releases.sort_by(|a, b| b.published_at.cmp(&a.published_at));
    for release in &releases {
        let names: Vec<&str> = release.assets.iter().map(|a| a.name.as_str()).collect();
        if let Some(index) = pick_engine_asset(&names, gpu) {
            let asset = &release.assets[index];
            return Ok(EngineAsset {
                tag: release.tag_name.clone(),
                name: asset.name.clone(),
                url: asset.browser_download_url.clone(),
                size: asset.size,
                sha256: asset
                    .digest
                    .as_deref()
                    .and_then(|d| d.strip_prefix("sha256:"))
                    .map(str::to_ascii_lowercase),
                flavour: if gpu && is_cuda_asset(&asset.name) { "cuda" } else { "cpu" },
            });
        }
    }
    bail!("No Windows build of the speech engine was found on GitHub")
}

fn is_cuda_asset(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("cuda") || n.contains("cublas")
}

/// Index of the best Windows x64 zip: CUDA 12 for a GPU (then CUDA 11), else OpenBLAS, else plain.
fn pick_engine_asset(names: &[&str], gpu: bool) -> Option<usize> {
    let lower: Vec<String> = names.iter().map(|n| n.to_ascii_lowercase()).collect();
    let windows_x64 = |n: &str| n.ends_with(".zip") && n.contains("x64") && !n.contains("arm64") && !n.contains("ubuntu");
    let find = |pred: &dyn Fn(&str) -> bool| lower.iter().position(|n| windows_x64(n) && pred(n));
    if gpu {
        let cuda = find(&|n| n.contains("cuda-12") || n.contains("cublas-12"))
            .or_else(|| find(&|n| n.contains("cuda-11") || n.contains("cublas-11")));
        if cuda.is_some() {
            return cuda;
        }
    }
    find(&|n| n.contains("blas") && !n.contains("cublas"))
        .or_else(|| find(&|n| n.starts_with("whisper-bin-x64") || n == "whisper-bin-win-x64.zip"))
}

pub async fn download_engine(paths: &Paths, gpu: bool, progress: ProgressCallback) -> Result<PathBuf> {
    let result = download_engine_inner(paths, gpu, &progress).await;
    if let Err(e) = &result {
        report(&progress, "engine", 0, 0, "error", format!("{e:#}"));
    }
    result
}

async fn download_engine_inner(paths: &Paths, gpu: bool, progress: &ProgressCallback) -> Result<PathBuf> {
    // whisper.cpp publishes Windows builds only. On a Mac the engine is built by
    // scripts/mac-setup.sh (with Metal), or comes from Homebrew.
    #[cfg(target_os = "macos")]
    {
        let _ = (paths, gpu, progress);
        bail!("Navi can't download the speech engine on a Mac. Run scripts/mac-setup.sh from the Navi folder, or install it with Homebrew: brew install whisper-cpp");
    }
    #[cfg(not(target_os = "macos"))]
    {
    let asset = find_engine_release(gpu).await?;
    let engine_dir = paths.engine.clone();
    tokio::fs::create_dir_all(&engine_dir)
        .await
        .with_context(|| format!("Could not create {}", engine_dir.display()))?;
    let zip_path = engine_dir.join(&asset.name);
    let client = http_client()?;
    download_file(&client, &asset.url, &zip_path, Some(asset.size), asset.sha256.as_deref(), "engine", progress).await?;

    report(progress, "engine", asset.size, asset.size, "extracting", "Unpacking the speech engine");
    let folder = format!("{}-{}", asset.flavour, sanitise(&asset.tag));
    let server = tokio::task::spawn_blocking(move || install_engine_zip(&engine_dir, &zip_path, &folder))
        .await
        .map_err(|e| anyhow!("Unpacking the speech engine failed: {e}"))??;
    report(progress, "engine", asset.size, asset.size, "done", format!("Speech engine {} is ready", asset.tag));
    Ok(server)
    }
}

fn sanitise(tag: &str) -> String {
    tag.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect()
}

/// Extracts into a staging folder, swaps it into place, records it and tidies older builds.
fn install_engine_zip(engine_dir: &Path, zip_path: &Path, folder: &str) -> Result<PathBuf> {
    let staging = engine_dir.join(format!(".staging-{folder}"));
    if staging.exists() {
        std::fs::remove_dir_all(&staging).with_context(|| format!("Could not clear {}", staging.display()))?;
    }
    let file = std::fs::File::open(zip_path).with_context(|| format!("Could not open {}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(file).context("The engine download is not a valid zip file")?;
    archive.extract(&staging).context("Could not unpack the speech engine")?;

    let mut found = Vec::new();
    collect_servers(&staging, 4, &mut found);
    let Some(server_in_staging) = found.into_iter().next() else {
        let _ = std::fs::remove_dir_all(&staging);
        bail!("The engine download does not contain {SERVER_EXE}");
    };
    let relative = server_in_staging
        .strip_prefix(&staging)
        .map(Path::to_path_buf)
        .map_err(|_| anyhow!("Unexpected layout in the engine download"))?;

    let mut target = engine_dir.join(folder);
    if target.exists() && std::fs::remove_dir_all(&target).is_err() {
        // In use (the server is running from it): install beside it instead.
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        target = engine_dir.join(format!("{folder}-{stamp}"));
    }
    std::fs::rename(&staging, &target).with_context(|| format!("Could not install the engine into {}", target.display()))?;
    let server = target.join(&relative);
    let marker = target
        .strip_prefix(engine_dir)
        .map(|p| p.join(&relative))
        .unwrap_or_else(|_| server.clone());
    std::fs::write(engine_dir.join(ENGINE_MARKER), marker.to_string_lossy().as_bytes())
        .context("Could not record the installed engine")?;
    let _ = std::fs::remove_file(zip_path);
    remove_old_engines(engine_dir, &target);
    Ok(server)
}

/// Deletes earlier engine folders, skipping any whose server is running (Windows will not delete
/// a running executable, so that delete failing is the signal to leave the folder alone).
fn remove_old_engines(engine_dir: &Path, keep: &Path) {
    let Ok(read) = std::fs::read_dir(engine_dir) else { return };
    for item in read.flatten() {
        let path = item.path();
        if path == keep || !path.is_dir() {
            continue;
        }
        let name = item.file_name().to_string_lossy().into_owned();
        if !(name.starts_with("cuda-") || name.starts_with("cpu-")) {
            continue;
        }
        let mut servers = Vec::new();
        collect_servers(&path, 4, &mut servers);
        if servers.iter().all(|s| std::fs::remove_file(s).is_ok()) {
            if let Err(e) = std::fs::remove_dir_all(&path) {
                log::debug!("could not remove old engine {}: {e}", path.display());
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// GPU
// ---------------------------------------------------------------------------------------------

pub fn detect_gpu() -> Option<GpuInfo> {
    #[cfg(target_os = "macos")]
    {
        mac_gpu()
    }
    #[cfg(not(target_os = "macos"))]
    {
        nvidia_gpu()
    }
}

/// Apple silicon runs the engine on its GPU through Metal. Its memory is shared with the
/// processor, so the whole of it counts towards which models fit. Intel Macs get None (CPU only).
#[cfg(target_os = "macos")]
fn mac_gpu() -> Option<GpuInfo> {
    let brand = sysctl_string("machdep.cpu.brand_string")?;
    if !brand.starts_with("Apple") {
        return None;
    }
    let vram_mb = sysctl_string("hw.memsize").and_then(|b| b.parse::<u64>().ok()).map(|b| b / (1024 * 1024)).unwrap_or(0);
    Some(GpuInfo { name: format!("{brand} GPU"), vram_mb })
}

#[cfg(target_os = "macos")]
pub(crate) fn sysctl_string(name: &str) -> Option<String> {
    let mut cmd = Command::new("/usr/sbin/sysctl");
    cmd.args(["-n", name]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    let out = run_with_timeout(cmd, Duration::from_secs(2))?;
    let out = out.trim();
    (!out.is_empty()).then(|| out.to_string())
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn nvidia_gpu() -> Option<GpuInfo> {
    let exe = nvidia_smi()?;
    let mut cmd = Command::new(exe);
    cmd.args(["--query-gpu=name,memory.total", "--format=csv,noheader,nounits"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let output = run_with_timeout(cmd, Duration::from_secs(5))?;
    parse_nvidia_smi(&output)
}

fn nvidia_smi() -> Option<PathBuf> {
    let on_path = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).map(|dir| dir.join("nvidia-smi.exe")).find(|p| p.is_file())
    });
    on_path.or_else(|| {
        let system_root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let program_files = std::env::var_os("ProgramFiles").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
        [
            system_root.join("System32").join("nvidia-smi.exe"),
            program_files.join("NVIDIA Corporation").join("NVSMI").join("nvidia-smi.exe"),
        ]
        .into_iter()
        .find(|p| p.is_file())
    })
}

/// Runs a command, giving up (and killing it) after `timeout`. Returns stdout on success.
fn run_with_timeout(mut cmd: Command, timeout: Duration) -> Option<String> {
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let text = reader.join().ok()?;
                return status.success().then_some(text);
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn parse_nvidia_smi(output: &str) -> Option<GpuInfo> {
    let line = output.lines().map(str::trim).find(|l| !l.is_empty())?;
    let (name, memory) = line.rsplit_once(',')?;
    let vram_mb = memory.trim().trim_end_matches("MiB").trim().parse::<u64>().unwrap_or(0);
    let name = name.trim();
    (!name.is_empty()).then(|| GpuInfo { name: name.to_string(), vram_mb })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::DownloadProgress;
    use parking_lot::Mutex;

    #[test]
    fn catalogue_is_complete_and_consistent() {
        let paths = Paths::for_tests("models-catalogue");
        let settings = EngineSettings { model_path: Some("Z:/definitely/missing.bin".into()), ..Default::default() };
        let list = catalogue(&paths, &settings);
        let ids: Vec<&str> = list.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["large-v3-turbo", "large-v3-turbo-q5_0", "small.en", "base.en", "vad"]);
        assert_eq!(list.iter().filter(|m| m.recommended).count(), 1);
        assert_eq!(list[0].size_mb, 1625);
        assert_eq!(list[4].size_mb, 1);
        for e in CATALOGUE {
            assert_eq!(e.sha256.len(), 64, "{}", e.id);
            assert!(e.url.ends_with(e.file), "{}", e.id);
        }
        for m in &list {
            assert_eq!(m.installed, m.path.is_some());
        }
        let _ = std::fs::remove_dir_all(&paths.data);
    }

    #[test]
    fn finds_models_in_order() {
        let paths = Paths::for_tests("models-find");
        let mut settings = EngineSettings { model: "base.en".into(), ..Default::default() };
        let managed = paths.models.join("ggml-base.en.bin");
        std::fs::write(&managed, b"model").expect("write");
        assert_eq!(find_model(&settings, &paths), Some(managed.clone()));

        let explicit = paths.data.join("custom.bin");
        std::fs::write(&explicit, b"model").expect("write");
        settings.model_path = Some(explicit.to_string_lossy().into_owned());
        assert_eq!(find_model(&settings, &paths), Some(explicit));
        settings.model_path = Some("   ".into());
        assert_eq!(find_model(&settings, &paths), Some(managed.clone()));

        // A path saved for another catalogue model (setup found it, then another model was chosen
        // in Settings) gives way to the model chosen; one saved for this model still counts.
        let elsewhere = paths.data.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("mkdir");
        let stale = elsewhere.join("ggml-small.en.bin");
        std::fs::write(&stale, b"model").expect("write");
        settings.model_path = Some(stale.to_string_lossy().into_owned());
        assert_eq!(find_model(&settings, &paths), Some(managed.clone()));
        let own = elsewhere.join("GGML-BASE.EN.BIN");
        std::fs::write(&own, b"model").expect("write");
        settings.model_path = Some(own.to_string_lossy().into_owned());
        assert_eq!(find_model(&settings, &paths), Some(own));
        assert_eq!(catalogue_model_of(Path::new("ggml-silero-v5.1.2.bin")), None, "the VAD file is not a speech model");
        settings.model_path = None;
        assert_eq!(find_model(&settings, &paths), Some(managed));

        // Another Silero release in the models folder is accepted; the catalogue's own file wins.
        let other_vad = paths.models.join("ggml-silero-v6.2.0.bin");
        std::fs::write(&other_vad, b"vad").expect("write");
        let shared_has_vad = shared_model_dir().map(|d| d.join("ggml-silero-v5.1.2.bin").is_file()).unwrap_or(false);
        if !shared_has_vad {
            assert_eq!(find_vad_model(&paths), Some(other_vad));
        }
        let own_vad = paths.models.join("ggml-silero-v5.1.2.bin");
        std::fs::write(&own_vad, b"vad").expect("write");
        assert_eq!(find_vad_model(&paths), Some(own_vad));
        let _ = std::fs::remove_dir_all(&paths.data);
    }

    // Windows engine folders and zips.
    #[cfg(windows)]
    #[test]
    fn finds_the_server() {
        let paths = Paths::for_tests("models-server");
        let settings = EngineSettings::default();
        // Nothing managed yet: falls back to a build already on the machine (or PATH, or nothing).
        let fallback = find_server(&settings, &paths);
        println!("fallback server: {fallback:?}");

        let nested = paths.engine.join("cpu-b1").join("Release");
        std::fs::create_dir_all(&nested).expect("mkdir");
        std::fs::write(nested.join(SERVER_EXE), b"exe").expect("write");
        assert_eq!(find_server(&settings, &paths), Some(nested.join(SERVER_EXE)));

        let other = paths.engine.join("cuda-b2");
        std::fs::create_dir_all(&other).expect("mkdir");
        std::fs::write(other.join(SERVER_EXE), b"exe").expect("write");
        std::fs::write(paths.engine.join(ENGINE_MARKER), "cuda-b2\\whisper-server.exe").expect("marker");
        assert_eq!(find_server(&settings, &paths), Some(paths.engine.join("cuda-b2\\whisper-server.exe")));

        let explicit = EngineSettings { server_path: Some(nested.join(SERVER_EXE).to_string_lossy().into_owned()), ..Default::default() };
        assert_eq!(find_server(&explicit, &paths), Some(nested.join(SERVER_EXE)));
        let _ = std::fs::remove_dir_all(&paths.data);
    }

    #[test]
    fn picks_the_right_engine_zip() {
        // b5454 naming (October 2026).
        let new = [
            "whisper-b5454-xcframework.zip",
            "whisper-bin-ubuntu-x64.tar.gz",
            "whisper-bin-win-cpu-arm64.zip",
            "whisper-bin-win-cuda-11.8.0-x64.zip",
            "whisper-bin-win-cuda-12.4.0-x64.zip",
            "whisper-bin-win-cuda-13.4-arm64.zip",
            "whisper-bin-Win32.zip",
            "whisper-bin-x64.zip",
            "whisper-blas-bin-Win32.zip",
            "whisper-blas-bin-x64.zip",
        ];
        assert_eq!(pick_engine_asset(&new, true).map(|i| new[i]), Some("whisper-bin-win-cuda-12.4.0-x64.zip"));
        assert_eq!(pick_engine_asset(&new, false).map(|i| new[i]), Some("whisper-blas-bin-x64.zip"));
        // b5130 naming (September 2026).
        let old = [
            "whisper-bin-x64.zip",
            "whisper-blas-bin-x64.zip",
            "whisper-cublas-11.8.0-bin-x64.zip",
            "whisper-cublas-12.4.0-bin-x64.zip",
        ];
        assert_eq!(pick_engine_asset(&old, true).map(|i| old[i]), Some("whisper-cublas-12.4.0-bin-x64.zip"));
        assert_eq!(pick_engine_asset(&old, false).map(|i| old[i]), Some("whisper-blas-bin-x64.zip"));
        assert_eq!(pick_engine_asset(&["whisper-bin-x64.zip"], true), Some(0), "CPU build when no CUDA build exists");
        assert_eq!(pick_engine_asset(&["notes.txt"], false), None);
    }

    #[test]
    fn parses_nvidia_smi_output() {
        let gpu = parse_nvidia_smi("NVIDIA GeForce RTX 5070, 12227\r\n").expect("gpu");
        assert_eq!(gpu.name, "NVIDIA GeForce RTX 5070");
        assert_eq!(gpu.vram_mb, 12_227);
        assert_eq!(gpu.label(), "NVIDIA GeForce RTX 5070 (12 GB)");
        assert_eq!(parse_nvidia_smi("Tesla T4, 15360 MiB").map(|g| g.vram_mb), Some(15_360));
        assert!(parse_nvidia_smi("").is_none());
    }

    // Windows engine folders and zips.
    #[cfg(windows)]
    #[test]
    fn engine_zip_installs_and_records_itself() {
        let paths = Paths::for_tests("models-engine-zip");
        let zip_path = paths.engine.join("whisper-test.zip");
        {
            let file = std::fs::File::create(&zip_path).expect("create zip");
            let mut zip = zip::ZipWriter::new(file);
            let options: zip::write::SimpleFileOptions = Default::default();
            zip.start_file("Release/whisper-server.exe", options).expect("entry");
            std::io::Write::write_all(&mut zip, b"exe").expect("write");
            zip.start_file("Release/ggml-cuda.dll", options).expect("entry");
            std::io::Write::write_all(&mut zip, b"dll").expect("write");
            zip.finish().expect("finish");
        }
        let old = paths.engine.join("cpu-b1");
        std::fs::create_dir_all(&old).expect("mkdir");
        std::fs::write(old.join(SERVER_EXE), b"old").expect("write");

        let server = install_engine_zip(&paths.engine, &zip_path, "cuda-b5454").expect("install");
        assert_eq!(server, paths.engine.join("cuda-b5454").join("Release").join(SERVER_EXE));
        assert!(server.is_file());
        assert!(!zip_path.exists(), "zip removed after install");
        assert!(!old.exists(), "older build tidied away");
        assert_eq!(find_server(&EngineSettings::default(), &paths), Some(server));
        let _ = std::fs::remove_dir_all(&paths.data);
    }

    /// Downloads the 885 KB Silero model into a temp folder through the real download path.
    /// cargo test --lib models::tests::download_vad_model -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn download_vad_model() {
        let paths = Paths::for_tests("models-download");
        let events = Arc::new(Mutex::new(Vec::<DownloadProgress>::new()));
        let sink = events.clone();
        let progress: ProgressCallback = Arc::new(move |p: DownloadProgress| sink.lock().push(p));

        // Leave half a file behind to exercise resume.
        let entry = entry(VAD_ID).expect("vad entry");
        let client = http_client().expect("client");
        let partial = client
            .get(entry.url)
            .header(reqwest::header::RANGE, "bytes=0-400000")
            .send()
            .await
            .expect("partial")
            .bytes()
            .await
            .expect("bytes");
        assert_eq!(partial.len(), 400_001, "server honours ranges");
        std::fs::write(paths.models.join(format!("{}.part", entry.file)), &partial).expect("write part");

        let path = download_model(VAD_ID, &paths, progress.clone()).await.expect("download");
        assert_eq!(std::fs::metadata(&path).expect("meta").len(), entry.bytes);
        let states: Vec<String> = events.lock().iter().map(|p| p.state.clone()).collect();
        println!("progress states: {states:?}");
        assert!(states.contains(&"verifying".to_string()));
        assert_eq!(states.last().map(String::as_str), Some("done"));
        assert!(events.lock().iter().any(|p| p.state == "downloading" && p.received >= 400_001), "resumed, not restarted");
        assert_eq!(find_vad_model(&paths), Some(path.clone()));

        // A second call notices the file is already there.
        download_model(VAD_ID, &paths, progress).await.expect("second call");
        let _ = std::fs::remove_dir_all(&paths.data);
    }

    /// Lists the GitHub release assets Navi would pick (no download).
    /// cargo test --lib models::tests::engine_release_assets -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn engine_release_assets() {
        for gpu in [true, false] {
            let asset = find_engine_release(gpu).await.expect("release");
            println!(
                "gpu={gpu}: {} {} ({} MB, sha256 {:?}, {})",
                asset.tag,
                asset.name,
                asset.size / 1_000_000,
                asset.sha256.as_deref().map(|s| &s[..12]),
                asset.flavour
            );
            assert!(asset.name.to_lowercase().contains("x64") && asset.name.ends_with(".zip"));
            assert_eq!(asset.flavour, if gpu { "cuda" } else { "cpu" });
        }
    }

    /// cargo test --lib models::tests::gpu_detection -- --ignored --nocapture
    #[test]
    #[ignore]
    fn gpu_detection() {
        let t = Instant::now();
        let gpu = detect_gpu();
        println!("detect_gpu: {:?} in {} ms", gpu.as_ref().map(GpuInfo::label), t.elapsed().as_millis());
    }
}
