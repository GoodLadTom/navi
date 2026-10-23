//! Updates from GitHub releases (tauri-plugin-updater). Each release on github.com/GoodLadTom/navi
//! carries the signed installer and a latest.json describing it. Navi reads latest.json and, when it
//! names a newer version, offers it: a card in the window and Settings > About. Updating downloads
//! the installer, checks its signature against the public key in tauri.conf.json (the private key
//! never leaves the publisher's PC), runs it quietly and restarts Navi.
//!
//! * Checked 20 seconds after start and every six hours, unless Settings turns that off or Local
//!   only is on; "Check for updates" always checks, because you asked.
//! * Never installs while a take or a meeting is being recorded, the screen is being watched, or
//!   notes are being written up.
//! * scripts/release.py builds, signs and publishes a release.
//!
//! A Mac copy is built on the Mac from a clone of the repository (scripts/mac-setup.sh), so there
//! is no signed download for it. There, the check fetches GitHub's main branch into that clone and
//! counts the changes this copy doesn't have, and Update runs `git pull` and the setup script in
//! Terminal, which builds the new copy, puts it in Applications and opens it again.

use crate::state::{App, Phase};
use anyhow::{bail, Context, Result};
use parking_lot::Mutex;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::Manager;
use tauri_plugin_updater::{Update, UpdaterExt};

const FIRST_CHECK: Duration = Duration::from_secs(20);
const EVERY: Duration = Duration::from_secs(6 * 3600);

/// What the interface shows about updates.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    /// "idle" | "checking" | "current" | "available" | "downloading" | "building" (a Mac copy
    /// rebuilding in Terminal) | "error"
    pub state: String,
    /// The version running now.
    pub current: String,
    /// The version on offer, when there is one.
    pub version: String,
    pub notes: String,
    /// 0 to 1 while downloading.
    pub progress: f32,
    pub message: String,
    /// On a Mac: how many changes on GitHub this copy doesn't have yet.
    pub changes: u32,
}

static STATUS: Mutex<Option<UpdateStatus>> = parking_lot::const_mutex(None);
static PENDING: Mutex<Option<Update>> = parking_lot::const_mutex(None);

fn current_version(app: &App) -> String {
    app.handle.package_info().version.to_string()
}

pub fn status(app: &App) -> UpdateStatus {
    STATUS.lock().clone().unwrap_or_else(|| UpdateStatus { state: "idle".into(), current: current_version(app), ..Default::default() })
}

fn set(app: &App, status: UpdateStatus) {
    *STATUS.lock() = Some(status.clone());
    app.emit("update", status);
}

/// Asks GitHub whether there is a newer version. `manual`: you pressed "Check for updates", so a
/// failure is shown rather than kept quiet.
pub async fn check(app: &Arc<App>, manual: bool) -> UpdateStatus {
    let current = current_version(app);
    // An update already downloading is left alone.
    if status(app).state == "downloading" {
        return status(app);
    }
    // A Mac copy is built from source on the Mac itself: it updates from the clone it was built from.
    if cfg!(target_os = "macos") {
        return check_source(app, manual).await;
    }
    set(app, UpdateStatus { state: "checking".into(), current: current.clone(), ..Default::default() });
    let found = async { app.handle.updater()?.check().await }.await;
    let next = match found {
        Ok(Some(update)) => {
            log::info!("update available: {} (running {current})", update.version);
            let next = UpdateStatus {
                state: "available".into(),
                current,
                version: update.version.clone(),
                notes: update.body.clone().unwrap_or_default().trim().to_string(),
                ..Default::default()
            };
            *PENDING.lock() = Some(update);
            next
        }
        Ok(None) => {
            *PENDING.lock() = None;
            UpdateStatus { state: "current".into(), current, ..Default::default() }
        }
        Err(e) => {
            log::info!("update check failed: {e}");
            UpdateStatus { state: if manual { "error" } else { "idle" }.into(), current, message: explain(&e.to_string()), ..Default::default() }
        }
    };
    set(app, next.clone());
    next
}

/// Downloads the update, checks its signature and runs the installer, which closes Navi and opens
/// the new version. Only returns if something went wrong.
pub async fn install(app: &Arc<App>) -> Result<()> {
    if let Some(why) = busy(app) {
        bail!(why);
    }
    if cfg!(target_os = "macos") {
        return install_source(app);
    }
    let Some(update) = PENDING.lock().clone() else { bail!("Check for updates first.") };
    let current = current_version(app);
    let version = update.version.clone();
    log::info!("downloading update {version}");
    let downloading = |progress: f32| UpdateStatus { state: "downloading".into(), current: current.clone(), version: version.clone(), progress, ..Default::default() };
    set(app, downloading(0.0));
    let (mut received, mut last_sent) = (0u64, Instant::now());
    let progress_app = app.clone();
    let progress_status = downloading(0.0);
    let result = update
        .download_and_install(
            move |chunk, length| {
                received += chunk as u64;
                // A few updates a second is plenty for a progress bar.
                if let Some(total) = length.filter(|t| *t > 0) {
                    if last_sent.elapsed() >= Duration::from_millis(250) {
                        last_sent = Instant::now();
                        set(&progress_app, UpdateStatus { progress: (received as f32 / total as f32).min(1.0), ..progress_status.clone() });
                    }
                }
            },
            || log::info!("update downloaded; starting the installer"),
        )
        .await;
    // On Windows a successful install exits Navi before this point.
    if let Err(e) = result {
        log::warn!("update failed: {e}");
        let why = explain_install(&e.to_string());
        set(app, UpdateStatus { state: "error".into(), current: current_version(app), version: update.version.clone(), message: why.clone(), ..Default::default() });
        bail!(why);
    }
    Ok(())
}

/// Why not to install now: a take or a meeting is being recorded, the screen is being watched, or
/// notes are still being written up. The installer closes Navi, which would lose them.
fn busy(app: &App) -> Option<&'static str> {
    if !matches!(app.session.lock().phase, Phase::Idle) || app.meeting.lock().is_some() {
        Some("Navi is recording. Finish first, then update.")
    } else if crate::watch::is_watching() {
        Some("Navi is watching your screen. Stop watching first, then update.")
    } else if crate::meeting::busy(app) {
        Some("Navi is still writing up your notes. Update once they're saved.")
    } else {
        None
    }
}

/// The first check soon after start, then one every six hours, when Settings allows (on a Mac,
/// a fetch of GitHub's main branch into the clone Navi was built from).
pub fn spawn_auto_check(app: Arc<App>) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK).await;
        loop {
            let settings = app.settings();
            if settings.general.check_updates && !settings.privacy.local_only {
                check(&app, false).await;
            }
            tokio::time::sleep(EVERY).await;
        }
    });
}

// ------------------------------------------------------------------ a Mac copy built from source

const NO_FOLDER: &str = "Navi can't find the folder it was built from. Run git pull and scripts/mac-setup.sh in that folder once more, and Update works from then on.";
const BUILDING: &str = "Building the new version in Terminal. Navi closes and opens again when it's ready, in a few minutes.";

/// What GitHub's main branch has that this copy's clone doesn't.
#[derive(Debug, PartialEq)]
struct SourceChanges {
    count: u32,
    /// Their subjects, newest first, at most five.
    subjects: Vec<String>,
    /// The version the new code builds (tauri.conf.json on GitHub).
    version: Option<String>,
}

/// The clone Navi was built from: the folder scripts/mac-setup.sh recorded in `recorded`, or else
/// the clone Navi is running from (its build sits at src-tauri/target/release/bundle/macos/Navi.app).
fn source_folder(recorded: Option<&Path>, exe: Option<&Path>) -> Option<PathBuf> {
    let is_clone = |dir: &Path| dir.join(".git").exists() && dir.join("scripts").join("mac-setup.sh").is_file();
    if let Some(dir) = recorded.and_then(|f| std::fs::read_to_string(f).ok()).map(|t| PathBuf::from(t.trim())) {
        if !dir.as_os_str().is_empty() && is_clone(&dir) {
            return Some(dir);
        }
    }
    exe.and_then(|e| e.ancestors().find(|d| is_clone(d)).map(Path::to_path_buf))
}

fn source_folder_for(app: &App) -> Option<PathBuf> {
    let recorded = app.handle.path().app_data_dir().ok().map(|d| d.join("source-folder"));
    let exe = std::env::current_exe().ok();
    source_folder(recorded.as_deref(), exe.as_deref())
}

/// Runs git in `dir`, never prompting for a password, and gives up after `limit`.
fn git(dir: &Path, args: &[&str], limit: Duration) -> Result<String> {
    let mut child = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("git isn't installed")?;
    let started = Instant::now();
    while child.try_wait()?.is_none() {
        if started.elapsed() > limit {
            let _ = child.kill();
            bail!("git took too long");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Fetches GitHub's main branch into the clone at `dir` (when `fetch`) and says what it has that the
/// clone's checked-out code doesn't.
fn source_changes(dir: &Path, fetch: bool) -> Result<SourceChanges> {
    const QUICK: Duration = Duration::from_secs(10);
    if fetch {
        git(dir, &["fetch", "--quiet", "origin", "main"], Duration::from_secs(60))?;
    }
    let count = git(dir, &["rev-list", "--count", "HEAD..origin/main"], QUICK)?.trim().parse().unwrap_or(0);
    let subjects = if count == 0 {
        Vec::new()
    } else {
        git(dir, &["log", "--format=%s", "-n", "5", "HEAD..origin/main"], QUICK)?.lines().filter(|l| !l.trim().is_empty()).map(str::to_string).collect()
    };
    let version = git(dir, &["show", "origin/main:src-tauri/tauri.conf.json"], QUICK)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("version").and_then(|v| v.as_str()).map(str::to_string));
    Ok(SourceChanges { count, subjects, version })
}

/// What Update runs in Terminal: bring in the new code, then build and install it with the setup
/// script (which quits this copy, replaces the one in Applications) and open the new one.
fn update_script(dir: &Path) -> String {
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    let wait = "  read -r -p \"Press Return to close this window. \" _\n  exit 1\n";
    format!(
        "#!/bin/bash\n\
         # Written by Navi when you pressed Update (Settings > About).\n\
         cd {dir} || exit 1\n\
         echo \"Updating Navi from GitHub...\"\n\
         echo\n\
         if ! git pull --ff-only origin main; then\n  echo\n  echo \"Navi couldn't bring in the new code (see above), so nothing was changed.\"\n{wait}fi\n\
         echo\n\
         if ! scripts/mac-setup.sh -y; then\n  echo\n  echo \"The new build didn't finish (see above). The Navi you had is still there.\"\n{wait}fi\n\
         open /Applications/Navi.app\n\
         echo\n\
         echo \"Navi is up to date and open again. You can close this window.\"\n",
        dir = quote(&dir.display().to_string()),
    )
}

/// The check on a Mac: the changes on GitHub this copy doesn't have.
async fn check_source(app: &Arc<App>, manual: bool) -> UpdateStatus {
    let current = current_version(app);
    let quiet_or_error = if manual { "error" } else { "idle" };
    let Some(dir) = source_folder_for(app) else {
        let next = UpdateStatus { state: quiet_or_error.into(), current, message: NO_FOLDER.into(), ..Default::default() };
        set(app, next.clone());
        return next;
    };
    set(app, UpdateStatus { state: "checking".into(), current: current.clone(), ..Default::default() });
    let found = tauri::async_runtime::spawn_blocking(move || source_changes(&dir, true)).await;
    let next = match found {
        Ok(Ok(c)) if c.count > 0 => {
            log::info!("update available: {} new changes on GitHub", c.count);
            UpdateStatus { state: "available".into(), current, version: c.version.unwrap_or_default(), notes: c.subjects.join("\n"), changes: c.count, ..Default::default() }
        }
        Ok(Ok(_)) => UpdateStatus { state: "current".into(), current, ..Default::default() },
        Ok(Err(e)) => {
            log::info!("update check failed: {e}");
            UpdateStatus { state: quiet_or_error.into(), current, message: explain_source(&e.to_string()), ..Default::default() }
        }
        Err(e) => UpdateStatus { state: quiet_or_error.into(), current, message: format!("Navi couldn't check for updates ({e})."), ..Default::default() },
    };
    set(app, next.clone());
    next
}

/// Update on a Mac: runs `update_script` in Terminal and says so. The setup script closes this copy
/// when the new one is built, minutes later.
fn install_source(app: &Arc<App>) -> Result<()> {
    let Some(dir) = source_folder_for(app) else { bail!(NO_FOLDER) };
    let folder = std::env::temp_dir().join("navi-update");
    std::fs::create_dir_all(&folder).context("Navi couldn't make a folder for the update")?;
    let file = folder.join("update-navi.command");
    std::fs::write(&file, update_script(&dir)).context("Navi couldn't write the update script")?;
    make_runnable(&file)?;
    std::process::Command::new("open").args(["-a", "Terminal"]).arg(&file).spawn().context("Navi couldn't open Terminal")?;
    log::info!("update: rebuilding in Terminal from {}", dir.display());
    let before = status(app);
    set(app, UpdateStatus { state: "building".into(), current: current_version(app), version: before.version, changes: before.changes, message: BUILDING.into(), ..Default::default() });
    Ok(())
}

#[cfg(unix)]
fn make_runnable(file: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).context("Navi couldn't make the update script runnable")
}

#[cfg(not(unix))]
fn make_runnable(_file: &Path) -> Result<()> {
    Ok(())
}

/// Plain words for a Mac check that failed.
fn explain_source(error: &str) -> String {
    let lower = error.to_lowercase();
    if lower.contains("isn't installed") {
        "Navi needs git to check for updates. Run scripts/mac-setup.sh once to install Apple's command-line tools.".into()
    } else if lower.contains("could not resolve") || lower.contains("unable to access") || lower.contains("took too long") || lower.contains("network") {
        "Navi couldn't reach GitHub. Check your connection and try again.".into()
    } else {
        format!("Navi couldn't check for updates ({error}).")
    }
}

/// Plain words for an update that was found and offered but couldn't be downloaded or installed.
/// (`explain` is for the check, so it talks about checking and about nothing being published.)
fn explain_install(error: &str) -> String {
    let lower = error.to_lowercase();
    if lower.contains("signature") {
        "The download didn't match Navi's signature, so it wasn't installed.".into()
    } else if lower.contains("dns") || lower.contains("connect") || lower.contains("timed out") || lower.contains("network") || lower.contains("request") {
        "Navi couldn't download the update. Check your connection and try again.".into()
    } else {
        format!("The update couldn't be installed ({error}). Try again later.")
    }
}

/// Plain words for what went wrong while checking.
fn explain(error: &str) -> String {
    let lower = error.to_lowercase();
    if lower.contains("404") || lower.contains("valid release json") || lower.contains("not found") {
        "No updates have been published yet.".into()
    } else if lower.contains("signature") {
        "The download didn't match Navi's signature, so it wasn't installed.".into()
    } else if lower.contains("dns") || lower.contains("connect") || lower.contains("timed out") || lower.contains("network") || lower.contains("request") {
        "Navi couldn't reach GitHub. Check your connection and try again.".into()
    } else {
        format!("Navi couldn't check for updates ({error}).")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_are_explained_plainly() {
        assert_eq!(explain("Could not fetch a valid release JSON from the remote"), "No updates have been published yet.");
        assert_eq!(explain("error sending request for url: dns error"), "Navi couldn't reach GitHub. Check your connection and try again.");
        assert_eq!(explain("signature verification failed"), "The download didn't match Navi's signature, so it wasn't installed.");
    }

    #[test]
    fn a_failed_install_is_not_called_a_failed_check() {
        assert_eq!(explain_install("signature verification failed"), "The download didn't match Navi's signature, so it wasn't installed.");
        assert_eq!(explain_install("error sending request for url: dns error"), "Navi couldn't download the update. Check your connection and try again.");
        let other = explain_install("the installer exited with code 2");
        assert_eq!(other, "The update couldn't be installed (the installer exited with code 2). Try again later.");
        assert!(!other.contains("check for updates") && !other.contains("published"));
    }

    /// This repository: the clone a Mac copy would be built from.
    fn repo() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
    }

    #[test]
    fn a_mac_copy_finds_the_clone_it_was_built_from() {
        let repo = repo();
        if !repo.join(".git").exists() {
            return;
        }
        let exe = repo.join("src-tauri/target/release/bundle/macos/Navi.app/Contents/MacOS/navi");
        assert_eq!(source_folder(None, Some(&exe)), Some(repo.clone()), "from the build inside the clone");
        let tmp = std::env::temp_dir().join(format!("navi-source-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let recorded = tmp.join("source-folder");
        std::fs::write(&recorded, format!("{}\n", repo.display())).unwrap();
        assert_eq!(source_folder(Some(&recorded), Some(Path::new("/Applications/Navi.app/Contents/MacOS/navi"))), Some(repo.clone()), "the folder the setup script recorded");
        std::fs::write(&recorded, "/nowhere/navi\n").unwrap();
        assert_eq!(source_folder(Some(&recorded), Some(Path::new("/Applications/Navi.app/Contents/MacOS/navi"))), None, "a stale record and a copy in Applications");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_clone_behind_github_counts_the_changes_it_is_missing() {
        let repo = repo();
        if !repo.join(".git").exists() {
            return;
        }
        // A clone of this repository (its origin/main is this repository's main), two changes behind.
        let tmp = std::env::temp_dir().join(format!("navi-behind-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let ok = std::process::Command::new("git").args(["clone", "--quiet", "--branch", "main"]).arg(&repo).arg(&tmp).status().map(|s| s.success()).unwrap_or(false);
        if !ok {
            return;
        }
        git(&tmp, &["reset", "--quiet", "--hard", "HEAD~2"], Duration::from_secs(30)).unwrap();
        let c = source_changes(&tmp, false).unwrap();
        assert_eq!(c.count, 2);
        assert_eq!(c.subjects.len(), 2);
        assert!(c.version.is_some(), "the version the new code builds");
        git(&tmp, &["reset", "--quiet", "--hard", "origin/main"], Duration::from_secs(30)).unwrap();
        assert_eq!(source_changes(&tmp, false).unwrap().count, 0, "up to date");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn the_update_script_pulls_builds_and_reopens() {
        let script = update_script(Path::new("/Users/dan/Tom's code/navi"));
        assert!(script.starts_with("#!/bin/bash\n"));
        assert!(script.contains("cd '/Users/dan/Tom'\\''s code/navi' || exit 1\n"), "the folder quoted for the shell");
        let (pull, build, open) = (script.find("git pull --ff-only origin main").unwrap(), script.find("scripts/mac-setup.sh -y").unwrap(), script.find("open /Applications/Navi.app").unwrap());
        assert!(pull < build && build < open, "in that order");
        assert_eq!(script.matches("read -r -p").count(), 2, "a failed step waits so you can read why");
    }

    #[test]
    fn mac_check_failures_are_explained_plainly() {
        assert!(explain_source("fatal: unable to access 'https://github.com/GoodLadTom/navi.git/': Could not resolve host: github.com").starts_with("Navi couldn't reach GitHub"));
        assert!(explain_source("git isn't installed").contains("command-line tools"));
    }
}
