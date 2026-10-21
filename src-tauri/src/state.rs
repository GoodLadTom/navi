//! The app-wide state shared by the controller, the commands and the tray.

use crate::audio::AudioService;
use crate::engine::Engine;
use crate::hotkey::HotkeyService;
use crate::paths::Paths;
use crate::sounds::SoundService;
use crate::store::Db;
use crate::types::{AppContext, EngineStatus, Mode, PillState, Settings};
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager};

pub struct App {
    pub handle: AppHandle,
    pub paths: Paths,
    pub settings: RwLock<Settings>,
    pub db: Arc<Db>,
    pub engine: tokio::sync::RwLock<Option<Arc<Engine>>>,
    /// Held while the engine is (re)starting, so two starts never overlap.
    pub engine_start: tokio::sync::Mutex<()>,
    pub engine_status: Mutex<EngineStatus>,
    pub audio: AudioService,
    pub sounds: SoundService,
    pub hotkeys: Mutex<Option<HotkeyService>>,
    pub session: Mutex<Session>,
    pub last_insert: Mutex<Option<LastInsert>>,
    /// The tray's Mode submenu items ("mode:auto", "mode:<id>"), kept so their ticks can follow
    /// the active mode however it changes.
    pub tray_mode_items: Mutex<Vec<tauri::menu::CheckMenuItem<tauri::Wry>>>,
    /// The tray's "Listen for “Navi”" (or “Hey Navi”) and "Watch my screen" ticks ("heynavi",
    /// "watch").
    pub tray_toggles: Mutex<Vec<tauri::menu::CheckMenuItem<tauri::Wry>>>,
    pub mic_test: AtomicBool,
    pub quitting: AtomicBool,
    pub paused: AtomicBool,
    /// Bumped on every pill change so stale hide timers do nothing.
    pub pill_gen: AtomicU64,
    pub project_index: Mutex<HashMap<String, (Instant, Arc<Vec<String>>)>>,
    /// While set and in the future, the shortcut picker on screen is shown the keys the hook sees
    /// (so they press down as you hold them). It never holds a take back, and it lapses by itself.
    pub key_monitor_until: Mutex<Option<Instant>>,
    /// The meeting Navi is taking notes on, if any (see meeting.rs).
    pub meeting: Mutex<Option<crate::meeting::Session>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Talk,
    HandsFree,
    Ramble,
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Talk => "talk",
            Kind::HandsFree => "handsFree",
            Kind::Ramble => "ramble",
        }
    }
}

pub struct Recording {
    pub kind: Kind,
    /// Counts takes, so a late timer can tell whether its take is still the current one.
    pub seq: u64,
    /// When the keys went down (double-tap timing).
    pub pressed: Instant,
    /// When the microphone opened: audio sample 0.
    pub started: Instant,
    pub ctx: AppContext,
    pub mode: Mode,
    /// The start sound played and the bubble shown. Until then the take can end silently.
    pub announced: bool,
    /// When the start cue began (ms after `started`) and how long it is; that stretch is trimmed
    /// off the front of the audio. 0 length when no cue played.
    pub cue_at_ms: u32,
    pub cue_ms: u32,
    pub cue_word: Option<&'static str>,
    /// Released quickly once: waiting to see whether a second tap makes it hands-free.
    pub pending_tap: Option<u64>,
    /// When the hook last saw the talk keys let go in this take, so a key bounce is told from a
    /// real press: in a take held to talk, the quick release that may begin a double tap (and its
    /// window counts from it); in a hands-free take or a ramble, any release.
    pub tap_up_at: Option<Instant>,
    /// 16 kHz sample range covered by the ramble cue when a take turned into a ramble.
    pub ramble_cue: Option<(usize, usize)>,
    pub ramble_started: Option<Instant>,
    /// On-device OCR of the window, started alongside the take when the setting is on.
    pub ocr: Option<tokio::task::JoinHandle<Option<String>>>,
    /// "Hey Navi" started this take (wake.rs): its audio opens with what was said while Navi woke
    /// up (`preroll_ms` of it), it finishes when you stop talking, and only real commands count.
    pub from_wake: bool,
    pub preroll_ms: u32,
}

pub enum Phase {
    Idle,
    Recording(Recording),
    Processing,
}

pub struct Session {
    pub phase: Phase,
    pub tap_seq: u64,
    pub take_seq: u64,
    /// A take is opening the microphone (the phase is still Idle). Claimed with the phase check,
    /// so the shortcut and "Navi" can never both start a take and fight over the microphone.
    pub starting: bool,
    /// The mode name the bubble shows while the last take is processed, so it can go back to
    /// that after a note.
    pub processing_mode: String,
    /// When the talk keys went down while the last take was still processing. Cleared when they
    /// are let go or another key joins them (another shortcut), so only a real hold gets a note.
    pub busy_press: Option<Instant>,
}

impl Default for Session {
    fn default() -> Self {
        Self { phase: Phase::Idle, tap_seq: 0, take_seq: 0, starting: false, processing_mode: String::new(), busy_press: None }
    }
}

#[derive(Clone, Debug)]
pub struct LastInsert {
    pub hwnd: isize,
    pub text: String,
    pub take_id: String,
    pub app_label: String,
    pub at: Instant,
    pub checked: bool,
    /// What Navi pasted over (the selection a spoken change replaced), so "scratch that" puts it
    /// back. None for dictation.
    pub replaced: Option<String>,
}

/// The last bubble state sent was Hidden. One pill window per app, so a static will do.
static PILL_HIDDEN: AtomicBool = AtomicBool::new(true);

impl App {
    pub fn settings(&self) -> Settings {
        self.settings.read().clone()
    }

    /// True while keys are being shown to the shortcut picker (see `key_monitor_until`).
    pub fn key_monitor_open(&self) -> bool {
        self.key_monitor_until.lock().is_some_and(|until| Instant::now() < until)
    }

    pub fn emit<S: serde::Serialize + Clone>(&self, event: &str, payload: S) {
        if let Err(e) = self.handle.emit(event, payload) {
            log::debug!("emit {event} failed: {e}");
        }
    }

    /// Sends the bubble its new state. Only Hidden touches the window (see `hide_pill_window`).
    pub fn pill(&self, state: PillState) -> u64 {
        let gen = self.pill_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let hidden = matches!(state, PillState::Hidden);
        PILL_HIDDEN.store(hidden, Ordering::SeqCst);
        if hidden {
            self.hide_pill_window();
        }
        self.emit("pill", state);
        gen
    }

    /// Hides the pill window, leaving the bubble's state alone. Like `show_pill_window`, never call
    /// it with the session lock held: Windows makes it wait for the main thread, and the main
    /// thread may be waiting for that lock (the tray, the Modes page), which would hang Navi.
    pub fn hide_pill_window(&self) {
        if let Some(w) = self.handle.get_webview_window("pill") {
            let _ = crate::overlay::hide_pill(&w);
        }
    }

    /// Shows the pill window for a bubble state already sent, once the session lock is let go. If
    /// the bubble was hidden meanwhile (the take was dropped while its window was being shown),
    /// the window is hidden again rather than left up over nothing.
    pub fn reveal_pill_window(&self, near_hwnd: isize) {
        self.show_pill_window(near_hwnd);
        if PILL_HIDDEN.load(Ordering::SeqCst) {
            self.hide_pill_window();
        }
    }

    /// Hide the pill window after `ms`, unless something else has happened to it since.
    pub fn hide_pill_later(self: &Arc<Self>, gen: u64, ms: u64) {
        let app = self.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            if app.pill_gen.load(Ordering::SeqCst) == gen {
                app.pill(PillState::Hidden);
            }
        });
    }

    /// Never with the session lock held (see `hide_pill_window`).
    pub fn show_pill_window(&self, near_hwnd: isize) {
        if !self.settings.read().general.show_pill {
            return;
        }
        if let Some(w) = self.handle.get_webview_window("pill") {
            if let Err(e) = crate::overlay::show_pill(&w, near_hwnd) {
                log::warn!("could not show the bubble: {e:#}");
            }
        }
    }

    pub fn show_main(&self) {
        // A Mac: back in the Dock while the window is open (see lib.rs close_main_window).
        #[cfg(target_os = "macos")]
        let _ = self.handle.set_dock_visibility(true);
        if let Some(w) = self.handle.get_webview_window("main") {
            let _ = w.show();
            let _ = w.unminimize();
            let _ = w.set_focus();
        }
    }

    pub fn navigate(&self, to: &str) {
        self.show_main();
        self.emit("navigate", to.to_string());
    }
}
