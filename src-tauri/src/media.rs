//! Other apps' sound while you talk (Settings > Sound > "While you talk", `sounds.whileTalking`).
//!
//! * "keep": nothing changes. "pause": what's playing is paused when the take is announced (the
//!   start cue) and played again when the take stops recording. "mute": every other app's audio is
//!   muted and unmuted again. "both": pause what can be paused, then mute the rest.
//! * Pausing uses Windows' media sessions (the players behind the volume flyout's media controls:
//!   Spotify, a YouTube tab, most media apps), and only sessions Navi paused are played again.
//!   Navi keeps the session objects themselves, not their app ids: every tab of a browser shares
//!   one app id, and a tab you had paused yourself must stay paused.
//! * Muting sets the mute flag on each other app's audio session, on every active output device,
//!   and clears it only on sessions Navi muted. The volume level is never touched, so every app
//!   comes back exactly as loud as it was. Navi's own sessions (its cues) and the system sounds
//!   session are left alone.
//! * Nothing changes while Navi takes call notes (it is recording the other side of the call).
//! * One state for the whole app: a take that starts before the last one's sound came back takes
//!   it over, so nothing is paused twice or played again under a new take.
//! * What Navi muted is written to `media-muted.json` in the data folder until it is unmuted, so a
//!   crash mid-take can't leave other apps silent: the next start unmutes them (`recover`).
//! * On a Mac, muting mutes the output device itself and pausing mutes too (see `mac`). Muting
//!   waits for the start cue to finish, and the sound comes back while any later cue plays.

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
#[derive(Default)]
struct Held {
    owner: u64,
    /// The media sessions Navi paused, as the sessions themselves.
    paused: win::Paused,
    /// Audio sessions Navi muted (their session instance ids).
    muted: Vec<String>,
}

/// A player behind one of Windows' media sessions (win.rs gives the real ones; the tests, fakes).
trait Player {
    fn playing(&self) -> bool;
    fn paused(&self) -> bool;
    /// True when it paused.
    fn pause(&self) -> bool;
    fn play(&self);
}

/// Pauses every player that is playing. Returns those it paused: the only ones `play_again` will
/// touch, so one already paused (a YouTube tab you paused yourself) is never started.
fn pause_all<P: Player>(players: Vec<P>) -> Vec<P> {
    players.into_iter().filter(|p| p.playing() && p.pause()).collect()
}

/// Plays again the players Navi paused that are still paused (one you started again yourself
/// meanwhile, or that has gone, is left alone). Returns how many.
fn play_again<P: Player>(paused: &[P]) -> usize {
    let mut played = 0;
    for p in paused.iter().filter(|p| p.paused()) {
        p.play();
        played += 1;
    }
    played
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
    // macOS mutes the whole output, Navi's start cue with it: the cue (just played) ends first.
    #[cfg(target_os = "macos")]
    let cue_wait = Duration::from_millis(app.sounds.duration_ms(crate::sounds::Cue::Talk, &settings.sounds) as u64 + 50);
    tauri::async_runtime::spawn(async move {
        {
            let mut state = STATE.lock().await;
            match state.as_mut() {
                // The last take's sound hasn't come back yet: this take carries on holding it.
                Some(held) => held.owner = seq,
                None => {
                    #[cfg(target_os = "macos")]
                    {
                        tokio::time::sleep(cue_wait).await;
                        if !matches!(&app.session.lock().phase, Phase::Recording(rec) if rec.seq == seq) {
                            return;
                        }
                    }
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

/// A Navi cue of `duration_ms` is about to play (sounds.rs). On a Mac, where muting silences the
/// whole output, the sound comes back for it and is muted again afterwards while a take holds it.
#[cfg(target_os = "macos")]
pub fn cue_playing(duration_ms: u32) {
    mac::cue_playing(duration_ms);
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
    // Nothing can pause another app's player on a Mac (see `mac`), so a pause mutes there.
    #[cfg(target_os = "macos")]
    let choice = if choice.pauses() {
        static SAID: std::sync::Once = std::sync::Once::new();
        SAID.call_once(|| log::info!("while you talk: macOS lets no app pause another's player, so Navi mutes the sound instead"));
        Choice::Mute
    } else {
        choice
    };
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
    // The sessions are let go here, and only then COM's hold on their apartment (win::Paused).
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
    use windows::Win32::System::Com::{
        CoCreateInstance, CoDecrementMTAUsage, CoIncrementMTAUsage, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
        CO_MTA_USAGE_COOKIE,
    };

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

    #[cfg(test)]
    fn app_id(session: &MediaSession) -> String {
        session.SourceAppUserModelId().map(|id| id.to_string()).unwrap_or_default()
    }

    impl super::Player for MediaSession {
        fn playing(&self) -> bool {
            status(self) == Some(PlaybackStatus::Playing)
        }
        fn paused(&self) -> bool {
            status(self) == Some(PlaybackStatus::Paused)
        }
        fn pause(&self) -> bool {
            self.TryPauseAsync().and_then(|op| op.get()).unwrap_or(false)
        }
        fn play(&self) {
            let _ = self.TryPlayAsync().and_then(|op| op.get());
        }
    }

    /// Keeps COM's multithreaded apartment open, where the sessions Navi holds live, after the
    /// thread that paused them lets COM go (`Com`): without it, the last thread leaving would close
    /// it under them.
    struct MtaHold(CO_MTA_USAGE_COOKIE);

    // The cookie is only handed back to COM, from whichever thread lets the hold go.
    unsafe impl Send for MtaHold {}

    impl Drop for MtaHold {
        fn drop(&mut self) {
            let _ = unsafe { CoDecrementMTAUsage(self.0) };
        }
    }

    /// The media sessions Navi paused, kept as the sessions themselves (a browser's tabs all share
    /// one app id). Declared before `_mta`, so the sessions are let go first.
    #[derive(Default)]
    pub struct Paused {
        sessions: Vec<MediaSession>,
        _mta: Option<MtaHold>,
    }

    impl Paused {
        pub fn len(&self) -> usize {
            self.sessions.len()
        }
        pub fn is_empty(&self) -> bool {
            self.sessions.is_empty()
        }
    }

    /// Pauses every playing media session and keeps the ones it paused.
    pub fn pause_playing() -> Paused {
        let _com = Com::init();
        let sessions = super::pause_all(media_sessions());
        let mta = if sessions.is_empty() { None } else { unsafe { CoIncrementMTAUsage() }.ok().map(MtaHold) };
        Paused { sessions, _mta: mta }
    }

    /// Plays again the sessions Navi paused, if they are still paused. Never another session of
    /// the same app.
    pub fn resume(paused: &Paused) {
        let _com = Com::init();
        let played = super::play_again(&paused.sessions);
        if played < paused.sessions.len() {
            log::info!("while you talk: played {played} of {} paused player(s) again; the rest were playing or gone", paused.sessions.len());
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

/// The Mac half stands in for `win`, so the shared code above calls one set of names.
#[cfg(target_os = "macos")]
use mac as win;

/// macOS has no per-app volume, so "mute" mutes the default output device itself (Core Audio), or
/// turns its volume to nothing where it has no mute, and puts back exactly what it changed.
/// Pausing isn't possible: macOS 15.4 closed MediaRemote (the private framework behind Now
/// Playing) to other apps, so `apply` mutes instead. A muted device silences Navi's own cues too,
/// so the sound comes back while a cue plays (`cue_playing`) and is muted again after it, for as
/// long as the take holds it.
#[cfg(target_os = "macos")]
mod mac {
    use objc2_core_audio::{
        kAudioDevicePropertyDeviceUID, kAudioDevicePropertyMute, kAudioDevicePropertyVolumeScalar, kAudioHardwarePropertyDefaultOutputDevice,
        kAudioHardwarePropertyDevices, kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeOutput,
        kAudioObjectSystemObject, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectHasProperty, AudioObjectID,
        AudioObjectIsPropertySettable, AudioObjectPropertyAddress, AudioObjectSetPropertyData,
    };
    use objc2_core_foundation::{CFRetained, CFString};
    use parking_lot::Mutex;
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::time::Duration;

    /// A volume at or below this counts as the nothing Navi set.
    const SILENT: f32 = 0.001;
    /// How long after a cue's end the sound is muted again.
    const CUE_TAIL: Duration = Duration::from_millis(150);

    #[derive(Default)]
    pub struct Paused;

    impl Paused {
        pub fn len(&self) -> usize {
            0
        }
        pub fn is_empty(&self) -> bool {
            true
        }
    }

    pub fn pause_playing() -> Paused {
        Paused
    }
    pub fn resume(_paused: &Paused) {}

    /// One change Navi made, written as a string for `media-muted.json`:
    /// "mac-mute:<element>:<device uid>" or "mac-volume:<element>:<level before>:<device uid>"
    /// (the uid last, as it may hold colons).
    #[derive(Clone, Debug, PartialEq)]
    enum Change {
        Mute { element: u32, uid: String },
        Volume { element: u32, level: f32, uid: String },
    }

    impl Change {
        fn key(&self) -> String {
            match self {
                Change::Mute { element, uid } => format!("mac-mute:{element}:{uid}"),
                Change::Volume { element, level, uid } => format!("mac-volume:{element}:{level}:{uid}"),
            }
        }

        fn parse(key: &str) -> Option<Change> {
            if let Some(rest) = key.strip_prefix("mac-mute:") {
                let (element, uid) = rest.split_once(':')?;
                return Some(Change::Mute { element: element.parse().ok()?, uid: uid.to_string() }).filter(|_| !uid.is_empty());
            }
            let rest = key.strip_prefix("mac-volume:")?;
            let mut parts = rest.splitn(3, ':');
            let (element, level, uid) = (parts.next()?.parse().ok()?, parts.next()?.parse::<f32>().ok()?, parts.next()?);
            (!uid.is_empty() && level.is_finite()).then(|| Change::Volume { element, level: level.clamp(0.0, 1.0), uid: uid.to_string() })
        }
    }

    /// What Navi has muted and still holds, and whether the sound is back for a cue just now.
    struct Hold {
        changes: Vec<Change>,
        given_back: bool,
        /// Counts cues, so only the last one's timer mutes again.
        cue: u64,
    }

    static HOLD: Mutex<Option<Hold>> = parking_lot::const_mutex(None);

    fn address(selector: u32, scope: u32, element: u32) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress { mSelector: selector, mScope: scope, mElement: element }
    }

    fn has(id: AudioObjectID, addr: &AudioObjectPropertyAddress) -> bool {
        unsafe { AudioObjectHasProperty(id, NonNull::from(addr)) }
    }

    fn settable(id: AudioObjectID, addr: &AudioObjectPropertyAddress) -> bool {
        let mut yes = 0u8;
        has(id, addr) && unsafe { AudioObjectIsPropertySettable(id, NonNull::from(addr), NonNull::from(&mut yes)) } == 0 && yes != 0
    }

    /// A plain value (u32, f32, an object id) of a property.
    fn get<T: Copy + Default>(id: AudioObjectID, addr: &AudioObjectPropertyAddress) -> Option<T> {
        let mut value = T::default();
        let mut size = std::mem::size_of::<T>() as u32;
        let status = unsafe { AudioObjectGetPropertyData(id, NonNull::from(addr), 0, std::ptr::null(), NonNull::from(&mut size), NonNull::from(&mut value).cast::<c_void>()) };
        (status == 0 && size as usize == std::mem::size_of::<T>()).then_some(value)
    }

    fn set<T: Copy>(id: AudioObjectID, addr: &AudioObjectPropertyAddress, value: T) -> bool {
        let mut value = value;
        let size = std::mem::size_of::<T>() as u32;
        unsafe { AudioObjectSetPropertyData(id, NonNull::from(addr), 0, std::ptr::null(), size, NonNull::from(&mut value).cast::<c_void>()) == 0 }
    }

    fn default_output() -> Option<AudioObjectID> {
        let addr = address(kAudioHardwarePropertyDefaultOutputDevice, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain);
        get::<AudioObjectID>(kAudioObjectSystemObject as AudioObjectID, &addr).filter(|id| *id != 0)
    }

    fn devices() -> Vec<AudioObjectID> {
        let system = kAudioObjectSystemObject as AudioObjectID;
        let addr = address(kAudioHardwarePropertyDevices, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain);
        let mut size = 0u32;
        if unsafe { AudioObjectGetPropertyDataSize(system, NonNull::from(&addr), 0, std::ptr::null(), NonNull::from(&mut size)) } != 0 || size == 0 {
            return Vec::new();
        }
        let mut ids = vec![0 as AudioObjectID; size as usize / std::mem::size_of::<AudioObjectID>()];
        let status = unsafe { AudioObjectGetPropertyData(system, NonNull::from(&addr), 0, std::ptr::null(), NonNull::from(&mut size), NonNull::new(ids.as_mut_ptr().cast::<c_void>()).unwrap()) };
        if status != 0 {
            return Vec::new();
        }
        ids.truncate(size as usize / std::mem::size_of::<AudioObjectID>());
        ids
    }

    /// A device's persistent id, which survives a restart (its object id may not).
    fn uid(id: AudioObjectID) -> Option<String> {
        let addr = address(kAudioDevicePropertyDeviceUID, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain);
        let raw = get::<usize>(id, &addr)?;
        // SAFETY: the property is a CFStringRef that the caller owns (+1), released when dropped.
        let text: CFRetained<CFString> = unsafe { CFRetained::from_raw(NonNull::new(raw as *mut CFString)?) };
        Some(text.to_string()).filter(|s| !s.is_empty())
    }

    fn device_with_uid(wanted: &str) -> Option<AudioObjectID> {
        devices().into_iter().find(|id| uid(*id).as_deref() == Some(wanted))
    }

    fn mute_address(element: u32) -> AudioObjectPropertyAddress {
        address(kAudioDevicePropertyMute, kAudioObjectPropertyScopeOutput, element)
    }

    fn volume_address(element: u32) -> AudioObjectPropertyAddress {
        address(kAudioDevicePropertyVolumeScalar, kAudioObjectPropertyScopeOutput, element)
    }

    /// Mutes the default output: its mute switch (the whole device, else each of the first two
    /// channels), or failing that its volume. Returns what it changed; nothing if it was silent
    /// already.
    fn mute_default_output() -> Vec<Change> {
        let Some(device) = default_output() else { return Vec::new() };
        let Some(uid) = uid(device) else { return Vec::new() };
        let main = [kAudioObjectPropertyElementMain];
        let channels = [1, 2];
        let mut changes = Vec::new();
        let mute_elements: &[u32] = if settable(device, &mute_address(main[0])) { &main } else { &channels };
        for &element in mute_elements {
            let addr = mute_address(element);
            if !settable(device, &addr) {
                continue;
            }
            if get::<u32>(device, &addr) == Some(0) && set(device, &addr, 1u32) {
                changes.push(Change::Mute { element, uid: uid.clone() });
            }
        }
        if mute_elements.iter().any(|e| settable(device, &mute_address(*e))) {
            return changes;
        }
        let volume_elements: &[u32] = if settable(device, &volume_address(main[0])) { &main } else { &channels };
        for &element in volume_elements {
            let addr = volume_address(element);
            if !settable(device, &addr) {
                continue;
            }
            if let Some(level) = get::<f32>(device, &addr).filter(|l| *l > SILENT) {
                if set(device, &addr, 0f32) {
                    changes.push(Change::Volume { element, level, uid: uid.clone() });
                }
            }
        }
        if changes.is_empty() && !volume_elements.iter().any(|e| settable(device, &volume_address(*e))) {
            log::info!("while you talk: the sound output has no mute or volume Navi can set, so it plays on");
        }
        changes
    }

    /// Puts one change back, if it is still as Navi left it (one you unmuted or turned up yourself
    /// meanwhile is left alone). True when it was put back.
    fn restore(change: &Change) -> bool {
        match change {
            Change::Mute { element, uid } => {
                let Some(device) = device_with_uid(uid) else { return false };
                let addr = mute_address(*element);
                get::<u32>(device, &addr).is_some_and(|m| m != 0) && set(device, &addr, 0u32)
            }
            Change::Volume { element, level, uid } => {
                let Some(device) = device_with_uid(uid) else { return false };
                let addr = volume_address(*element);
                get::<f32>(device, &addr).is_some_and(|l| l <= SILENT) && set(device, &addr, *level)
            }
        }
    }

    /// Makes one change again after a cue.
    fn reapply(change: &Change) {
        match change {
            Change::Mute { element, uid } => {
                if let Some(device) = device_with_uid(uid) {
                    set(device, &mute_address(*element), 1u32);
                }
            }
            Change::Volume { element, uid, .. } => {
                if let Some(device) = device_with_uid(uid) {
                    set(device, &volume_address(*element), 0f32);
                }
            }
        }
    }

    pub fn mute_others() -> Vec<String> {
        let changes = mute_default_output();
        let keys = changes.iter().map(Change::key).collect();
        if !changes.is_empty() {
            *HOLD.lock() = Some(Hold { changes, given_back: false, cue: 0 });
        }
        keys
    }

    /// Puts back the changes with these keys (from this run, or one that crashed). Returns how many.
    pub fn unmute(ids: &[String]) -> usize {
        let mut hold = HOLD.lock();
        let changes: Vec<Change> = ids.iter().filter_map(|k| Change::parse(k)).collect();
        if let Some(h) = hold.as_mut() {
            h.changes.retain(|c| !changes.contains(c));
            if h.changes.is_empty() {
                *hold = None;
            }
        }
        changes.iter().filter(|c| restore(c)).count()
    }

    /// A cue of `duration_ms` is about to play: the sound comes back now, and is muted again just
    /// after the cue if Navi still holds it then (a take's last cues find it already let go).
    pub fn cue_playing(duration_ms: u32) {
        let cue = {
            let mut hold = HOLD.lock();
            let Some(h) = hold.as_mut() else { return };
            if !h.given_back {
                h.changes.iter().for_each(|c| {
                    restore(c);
                });
                h.given_back = true;
            }
            h.cue += 1;
            h.cue
        };
        let _ = std::thread::Builder::new().name("navi-media-cue".into()).spawn(move || {
            std::thread::sleep(Duration::from_millis(duration_ms as u64) + CUE_TAIL);
            let mut hold = HOLD.lock();
            if let Some(h) = hold.as_mut().filter(|h| h.cue == cue && h.given_back) {
                h.changes.iter().for_each(reapply);
                h.given_back = false;
            }
        });
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn changes_round_trip_as_text() {
            let mute = Change::Mute { element: 0, uid: "AppleUSBAudioEngine:Apple:Speakers:1".into() };
            let volume = Change::Volume { element: 2, level: 0.62, uid: "BuiltInSpeakerDevice".into() };
            assert_eq!(Change::parse(&mute.key()), Some(mute.clone()));
            assert_eq!(Change::parse(&volume.key()), Some(volume));
            // A Windows session id, an empty uid or rubbish is ignored, never a panic.
            assert_eq!(Change::parse("{0.0.0.00000000}.{not-a-real-session}"), None);
            assert_eq!(Change::parse("mac-mute:0:"), None);
            assert_eq!(Change::parse("mac-volume:x:0.5:dev"), None);
            assert_eq!(Change::parse("mac-volume:1:NaN:dev"), None);
        }

        /// Mutes the default output and puts it back, silently, then checks it is exactly as it
        /// was. Run by hand:
        /// cargo test --lib media::mac::tests::live_mute_round_trip -- --ignored --nocapture
        #[test]
        #[ignore]
        fn live_mute_round_trip() {
            let device = default_output().expect("a default output device");
            let uid = uid(device).expect("its uid");
            let main = kAudioObjectPropertyElementMain;
            let state = || {
                [main, 1, 2].map(|e| (get::<u32>(device, &mute_address(e)), get::<f32>(device, &volume_address(e))))
            };
            let before = state();
            println!("default output {device} ({uid}): mute and volume by element {before:?}");
            // Only the mute switch is tried here, so the volume never moves during the test.
            if !settable(device, &mute_address(main)) && !settable(device, &mute_address(1)) {
                println!("this output has no mute switch; nothing was changed");
                return;
            }
            let keys = mute_others();
            println!("muted: {keys:?}");
            assert!(keys.iter().all(|k| k.starts_with("mac-mute:")), "only the mute switch should be used here: {keys:?}");
            let muted = || keys.iter().filter_map(|k| Change::parse(k)).all(|c| matches!(c, Change::Mute { element, .. } if get::<u32>(device, &mute_address(element)) == Some(1)));
            if keys.is_empty() {
                println!("the output was muted already; nothing was changed");
                return;
            }
            assert!(muted());
            // A cue: the sound is back while it plays, muted again after it.
            cue_playing(50);
            assert_eq!(state(), before, "the sound is back for the cue");
            std::thread::sleep(Duration::from_millis(500));
            assert!(muted(), "muted again after the cue");
            // A crash: nothing held in memory, the file's keys put it back on the next start.
            *HOLD.lock() = None;
            let dir = std::env::temp_dir().join(format!("navi-mac-mute-test-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(super::super::MUTED_FILE), serde_json::to_string(&keys).unwrap()).unwrap();
            super::super::recover(dir.clone());
            assert!(!dir.join(super::super::MUTED_FILE).exists());
            let _ = std::fs::remove_dir_all(&dir);
            assert_eq!(state(), before, "the output is back as it was");
            // Putting back again changes nothing.
            assert_eq!(unmute(&keys), 0);
            assert_eq!(state(), before);
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod win {
    #[derive(Default)]
    pub struct Paused;

    impl Paused {
        pub fn len(&self) -> usize {
            0
        }
        pub fn is_empty(&self) -> bool {
            true
        }
    }

    pub fn pause_playing() -> Paused {
        Paused
    }
    pub fn resume(_paused: &Paused) {}
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

    /// A pretend media session: its state is shared between clones, as a real session's is.
    #[derive(Clone)]
    struct Fake {
        state: std::rc::Rc<std::cell::Cell<&'static str>>,
        /// Refuses to pause (some players can't).
        stubborn: bool,
    }

    impl Fake {
        fn new(state: &'static str) -> Self {
            Fake { state: std::rc::Rc::new(std::cell::Cell::new(state)), stubborn: false }
        }
        fn get(&self) -> &'static str {
            self.state.get()
        }
    }

    impl Player for Fake {
        fn playing(&self) -> bool {
            self.get() == "playing"
        }
        fn paused(&self) -> bool {
            self.get() == "paused"
        }
        fn pause(&self) -> bool {
            if self.stubborn {
                return false;
            }
            self.state.set("paused");
            true
        }
        fn play(&self) {
            self.state.set("playing");
        }
    }

    #[test]
    fn only_the_players_navi_paused_play_again() {
        // Two YouTube tabs in one browser (one app id): one playing, one you paused yourself.
        let (playing_tab, your_paused_tab) = (Fake::new("playing"), Fake::new("paused"));
        let (music, stopped, live_radio) = (Fake::new("playing"), Fake::new("stopped"), Fake { stubborn: true, ..Fake::new("playing") });
        let kept = pause_all(vec![playing_tab.clone(), your_paused_tab.clone(), music.clone(), stopped.clone(), live_radio.clone()]);
        assert_eq!(kept.len(), 2, "only what Navi really paused is kept");
        assert_eq!((playing_tab.get(), music.get(), live_radio.get()), ("paused", "paused", "playing"));
        // While you talk you start the music again yourself.
        music.play();
        assert_eq!(play_again(&kept), 1);
        assert_eq!(playing_tab.get(), "playing");
        assert_eq!(your_paused_tab.get(), "paused", "the tab you paused stays paused");
        assert_eq!(stopped.get(), "stopped");
        assert_eq!(music.get(), "playing");
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
