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

use crate::state::{App, Phase};
use anyhow::{bail, Result};
use parking_lot::Mutex;
use serde::Serialize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri_plugin_updater::{Update, UpdaterExt};

const FIRST_CHECK: Duration = Duration::from_secs(20);
const EVERY: Duration = Duration::from_secs(6 * 3600);

/// What the interface shows about updates.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    /// "idle" | "checking" | "current" | "available" | "downloading" | "error"
    pub state: String,
    /// The version running now.
    pub current: String,
    /// The version on offer, when there is one.
    pub version: String,
    pub notes: String,
    /// 0 to 1 while downloading.
    pub progress: f32,
    pub message: String,
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
        set(app, UpdateStatus { state: "error".into(), current: current_version(app), version: update.version.clone(), message: explain(&e.to_string()), ..Default::default() });
        bail!(explain(&e.to_string()));
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

/// The first check soon after start, then one every six hours, when Settings allows.
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

/// Plain words for what went wrong.
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
}
