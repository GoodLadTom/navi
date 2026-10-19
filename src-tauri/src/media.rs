//! Other apps' sound while you talk (Settings > Sound > "While you talk", `sounds.whileTalking`).
//!
//! * "keep": nothing changes. "pause": what's playing is paused when the take is announced (the
//!   start cue) and played again when the take stops recording. "mute": every other app's audio is
//!   muted and unmuted again. "both": pause what can be paused, then mute the rest.
//! * Pausing uses Windows' media sessions (the players behind the volume flyout's media controls:
//!   Spotify, a YouTube tab, most media apps), and only sessions Navi paused are played again.
//! * Muting sets the mute flag on each other app's audio session, on every active output device,
//!   and clears it only on sessions Navi muted. The volume level is never touched, so every app
//!   comes back exactly as loud as it was. Navi's own sessions (its cues) and the system sounds
//!   session are left alone.
//! * Nothing changes while Navi takes call notes (it is recording the other side of the call).
//! * One state for the whole app: a take that starts before the last one's sound came back takes
//!   it over, so nothing is paused twice or played again under a new take.
//! * What Navi muted is written to `media-muted.json` in the data folder until it is unmuted, so a
//!   crash mid-take can't leave other apps silent: the next start unmutes them (`recover`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::state::{App, Phase};

const MUTED_FILE: &str = "media-muted.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Keep,
    Pause,
    Mute,
    Both,
}

impl Choice {
    /// Unknown values (a newer settings file, a typo) keep playing, the safe choice.
    pub fn parse(value: &str) -> Choice {
        match value.trim().to_ascii_lowercase().as_str() {
            "pause" => Choice::Pause,
            "mute" => Choice::Mute,
            "both" => Choice::Both,
            _ => Choice::Keep,
        }
    }

    fn pauses(self) -> bool {
        matches!(self, Choice::Pause | Choice::Both)
    }

    fn mutes(self) -> bool {
        matches!(self, Choice::Mute | Choice::Both)
    }
}

/// What Navi changed for the take that owns it.
#[derive(Debug, Default)]
struct Held {
    owner: u64,
    /// Media sessions Navi paused (their app ids).
    paused: Vec<String>,
    /// Audio sessions Navi muted (their session instance ids).
    muted: Vec<String>,
}

static STATE: tokio::sync::Mutex<Option<Held>> = tokio::sync::Mutex::const_new(None);

/// Called when take `seq` is announced. Pauses or mutes in the background, then puts it back once
/// that take is no longer recording. Never blocks the take.
pub fn hold(app: &Arc<App>, seq: u64) {
    let settings = app.settings();
    let choice = Choice::parse(&settings.sounds.while_talking);
    if choice == Choice::Keep || app.meeting.lock().is_some() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        {
            let mut state = STATE.lock().await;
            match state.as_mut() {
                // The last take's sound hasn't come back yet: this take carries on holding it.
                Some(held) => held.owner = seq,
                None => {
                    let dir = app.paths.data.clone();
                    let held = tokio::task::spawn_blocking(move || apply(choice, &dir, seq)).await.unwrap_or_default();
                    if !held.paused.is_empty() || !held.muted.is_empty() {
                        log::info!("while you talk: paused {} player(s), muted {} app sound(s)", held.paused.len(), held.muted.len());
                    }
                    *state = Some(held);
                }
            }
        }
        // Wait for the take to stop recording.
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let recording = matches!(&app.session.lock().phase, Phase::Recording(rec) if rec.seq == seq);
            if !recording {
                break;
            }
        }
        let mut state = STATE.lock().await;
        if state.as_ref().is_some_and(|held| held.owner == seq) {
            if let Some(held) = state.take() {
                let dir = app.paths.data.clone();
                let _ = tokio::task::spawn_blocking(move || release(held, &dir)).await;
            }
        }
    });
}

/// At start: unmutes whatever a previous run muted and never got to unmute.
pub fn recover(data_dir: PathBuf) {
    let file = data_dir.join(MUTED_FILE);
    let Ok(text) = std::fs::read_to_string(&file) else { return };
    let ids: Vec<String> = serde_json::from_str(&text).unwrap_or_default();
    if !ids.is_empty() {
        let back = win::unmute(&ids);
        log::info!("unmuted {back} app sound(s) left muted by the last run");
    }
    let _ = std::fs::remove_file(&file);
}

fn apply(choice: Choice, data_dir: &Path, owner: u64) -> Held {
    let mut held = Held { owner, ..Default::default() };
    if choice.pauses() {
        held.paused = win::pause_playing();
    }
    if choice.mutes() {
        held.muted = win::mute_others();
        if !held.muted.is_empty() {
            if let Ok(json) = serde_json::to_string(&held.muted) {
                let _ = std::fs::write(data_dir.join(MUTED_FILE), json);
            }
        }
    }
    held
}

fn release(held: Held, data_dir: &Path) {
    if !held.muted.is_empty() {
        win::unmute(&held.muted);
        let _ = std::fs::remove_file(data_dir.join(MUTED_FILE));
    }
    if !held.paused.is_empty() {
        win::resume(&held.paused);
    }
}

#[cfg(windows)]
mod win {
    use windows::core::{Interface, PWSTR};
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSession as MediaSession,
        GlobalSystemMediaTransportControlsSessionManager as MediaManager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
    };
    use windows::Win32::Media::Audio::{
        eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator,
        DEVICE_STATE_ACTIVE,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED};

    /// COM for the length of one call on this (blocking pool) thread.
    struct Com(bool);

    impl Com {
        fn init() -> Com {
            Com(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
        }
    }

    impl Drop for Com {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }

    fn media_sessions() -> Vec<MediaSession> {
        let Ok(manager) = MediaManager::RequestAsync().and_then(|op| op.get()) else { return Vec::new() };
        let Ok(list) = manager.GetSessions() else { return Vec::new() };
        let count = list.Size().unwrap_or(0);
        (0..count).filter_map(|i| list.GetAt(i).ok()).collect()
    }

    fn status(session: &MediaSession) -> Option<PlaybackStatus> {
        session.GetPlaybackInfo().and_then(|info| info.PlaybackStatus()).ok()
    }

    fn app_id(session: &MediaSession) -> String {
        session.SourceAppUserModelId().map(|id| id.to_string()).unwrap_or_default()
    }

    /// Pauses every playing media session. Returns the app ids of those it paused.
    pub fn pause_playing() -> Vec<String> {
        let _com = Com::init();
        let mut paused = Vec::new();
        for session in media_sessions() {
            if status(&session) != Some(PlaybackStatus::Playing) {
                continue;
            }
            if session.TryPauseAsync().and_then(|op| op.get()).unwrap_or(false) {
                paused.push(app_id(&session));
            }
        }
        paused
    }

    /// Plays again the sessions Navi paused, if they are still paused.
    pub fn resume(ids: &[String]) {
        let _com = Com::init();
        for session in media_sessions() {
            if status(&session) == Some(PlaybackStatus::Paused) && ids.contains(&app_id(&session)) {
                let _ = session.TryPlayAsync().and_then(|op| op.get());
            }
        }
    }

    /// Every audio session on every active output device, with its control and volume.
    fn for_each_audio_session(mut f: impl FnMut(&IAudioSessionControl2, &ISimpleAudioVolume)) {
        let Ok(enumerator) = (unsafe { CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) }) else { return };
        let Ok(devices) = (unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }) else { return };
        let count = unsafe { devices.GetCount() }.unwrap_or(0);
        for d in 0..count {
            let Ok(device) = (unsafe { devices.Item(d) }) else { continue };
            let Ok(manager) = (unsafe { device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) }) else { continue };
            let Ok(sessions) = (unsafe { manager.GetSessionEnumerator() }) else { continue };
            let n = unsafe { sessions.GetCount() }.unwrap_or(0);
            for s in 0..n {
                let Ok(control) = (unsafe { sessions.GetSession(s) }) else { continue };
                let (Ok(control2), Ok(volume)) = (control.cast::<IAudioSessionControl2>(), control.cast::<ISimpleAudioVolume>()) else { continue };
                f(&control2, &volume);
            }
        }
    }

    fn instance_id(control: &IAudioSessionControl2) -> String {
        match unsafe { control.GetSessionInstanceIdentifier() } {
            Ok(p) => take_pwstr(p),
            Err(_) => String::new(),
        }
    }

    /// One line per media session and audio session, for the live test. Changes nothing.
    #[cfg(test)]
    pub fn describe() -> Vec<String> {
        let _com = Com::init();
        let mut lines: Vec<String> = media_sessions().iter().map(|s| format!("media: {} {:?}", app_id(s), status(s))).collect();
        for_each_audio_session(|control, volume| {
            let pid = unsafe { control.GetProcessId() }.unwrap_or(0);
            let muted = unsafe { volume.GetMute() }.map(|m| m.as_bool()).unwrap_or(false);
            lines.push(format!("audio: pid {pid} muted {muted}"));
        });
        lines
    }

    fn take_pwstr(p: PWSTR) -> String {
        if p.is_null() {
            return String::new();
        }
        let s = unsafe { p.to_string() }.unwrap_or_default();
        unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
        s
    }

    /// Mutes every other app's audio session that isn't muted already. Returns their instance ids.
    pub fn mute_others() -> Vec<String> {
        let _com = Com::init();
        let me = std::process::id();
        let mut muted = Vec::new();
        for_each_audio_session(|control, volume| {
            // pid 0 is the system sounds session: leave Windows' own sounds alone.
            let pid = unsafe { control.GetProcessId() }.unwrap_or(0);
            if pid == 0 || pid == me {
                return;
            }
            if unsafe { volume.GetMute() }.map(|m| m.as_bool()).unwrap_or(true) {
                return;
            }
            let id = instance_id(control);
            if id.is_empty() {
                return;
            }
            if unsafe { volume.SetMute(true, std::ptr::null()) }.is_ok() {
                muted.push(id);
            }
        });
        muted
    }

    /// Unmutes the sessions with these instance ids that are still muted. Returns how many.
    pub fn unmute(ids: &[String]) -> usize {
        let _com = Com::init();
        let mut back = 0;
        for_each_audio_session(|control, volume| {
            if !unsafe { volume.GetMute() }.map(|m| m.as_bool()).unwrap_or(false) {
                return;
            }
            if ids.contains(&instance_id(control)) && unsafe { volume.SetMute(false, std::ptr::null()) }.is_ok() {
                back += 1;
            }
        });
        back
    }
}

#[cfg(not(windows))]
mod win {
    pub fn pause_playing() -> Vec<String> {
        Vec::new()
    }
    pub fn resume(_ids: &[String]) {}
    pub fn mute_others() -> Vec<String> {
        Vec::new()
    }
    pub fn unmute(_ids: &[String]) -> usize {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_parse_and_unknown_keeps_playing() {
        assert_eq!(Choice::parse("pause"), Choice::Pause);
        assert_eq!(Choice::parse(" Mute "), Choice::Mute);
        assert_eq!(Choice::parse("both"), Choice::Both);
        assert_eq!(Choice::parse("keep"), Choice::Keep);
        assert_eq!(Choice::parse(""), Choice::Keep);
        assert_eq!(Choice::parse("louder"), Choice::Keep);
        assert!(Choice::Both.pauses() && Choice::Both.mutes());
        assert!(Choice::Pause.pauses() && !Choice::Pause.mutes());
        assert!(!Choice::Mute.pauses() && Choice::Mute.mutes());
        assert!(!Choice::Keep.pauses() && !Choice::Keep.mutes());
    }

    #[test]
    fn settings_default_to_pause_and_old_files_load() {
        let s = crate::types::SoundSettings::default();
        assert_eq!(Choice::parse(&s.while_talking), Choice::Pause);
        // A settings file from before this setting existed.
        let old: crate::types::SoundSettings = serde_json::from_str(r#"{"enabled":true,"set":"bubble","volume":0.5,"custom":{}}"#).unwrap();
        assert_eq!(old.while_talking, "pause");
        assert_eq!(old.set, "bubble");
    }

    #[test]
    fn recover_clears_a_leftover_file() {
        let dir = std::env::temp_dir().join(format!("navi-media-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MUTED_FILE), r#"["{0.0.0.00000000}.{not-a-real-session}"]"#).unwrap();
        recover(dir.clone());
        assert!(!dir.join(MUTED_FILE).exists());
        recover(dir.clone()); // nothing to do, no panic
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Lists the media and audio sessions on this PC without changing anything.
    /// cargo test --lib media::tests::live_list_sessions -- --ignored --nocapture
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn live_list_sessions() {
        // Pausing and muting for real would interrupt whatever is playing, so this only proves the
        // calls work: it lists and changes nothing.
        for line in win::describe() {
            println!("{line}");
        }
        assert_eq!(win::unmute(&[]), 0);
    }
}
