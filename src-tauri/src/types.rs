//! Shared types. Every module speaks in these, and the UI receives them as camelCase JSON.
//! Changing a field here changes the contract with the UI (ui/js/bridge.js mirrors these shapes).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------------------------
// Settings (persisted as settings.json in the app data folder)
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub version: u32,
    pub onboarded: bool,
    pub user_name: String,
    pub hotkeys: HotkeySettings,
    pub sounds: SoundSettings,
    pub mic: MicSettings,
    pub engine: EngineSettings,
    pub ai: AiSettings,
    pub writing: WritingSettings,
    pub context: ContextSettings,
    pub modes: ModeSettings,
    pub learning: LearningSettings,
    pub agent: AgentSettings,
    pub ramble: RambleSettings,
    pub privacy: PrivacySettings,
    pub general: GeneralSettings,
    pub speak: SpeakSettings,
    pub notes: NotesSettings,
    pub hey_navi: HeyNaviSettings,
    pub watch: WatchSettings,
}

/// Meeting notes (see meeting.rs).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NotesSettings {
    /// Offer to take notes when a call app starts using the microphone.
    pub detect_calls: bool,
    /// Record what the computer plays (the other side of the call) as well as the microphone.
    pub system_audio: bool,
    /// Finish by itself when the call app lets go of the microphone.
    pub auto_stop: bool,
    /// Come up with ideas for Shorts while the call goes on (see ideas.rs).
    pub short_ideas: bool,
    /// What makes a good Short, in the user's words. Given to the AI with every pass.
    pub ideas_brief: String,
    /// "Hey Navi, ..." said on a call gives the AI a job, done while the call goes on.
    pub call_jobs: bool,
    /// Take a screenshot when the screen changes, so the notes can say what was shown.
    pub call_screenshots: bool,
    /// Where the actions from the notes go: "none" | "clickup" | "notion" (connections under
    /// RambleSettings).
    pub actions_to: String,
}

/// The starting brief for Short ideas: what makes a clip worth posting, in plain words.
pub const DEFAULT_IDEAS_BRIEF: &str = "A Short is under a minute and teaches one thing that someone scrolling past, who has never heard of us, would find useful.
- One clear lesson per Short, stated in a sentence.
- Built only from the main speaker's own words, in whole sentences.
- Open on a strong hook line taken from what was said.
- Never name or show the other people on the call. A question someone else asked can become a short on-screen setup line with no name.
- Flag anything that needs the speaker's OK before it goes out (clients, deals, prices, private details) and anything said somewhere noisy.
- Leave out topics that only make sense to people who were on the call.";

impl Default for NotesSettings {
    fn default() -> Self {
        Self {
            detect_calls: true,
            system_audio: true,
            auto_stop: true,
            short_ideas: false,
            ideas_brief: DEFAULT_IDEAS_BRIEF.into(),
            call_jobs: true,
            call_screenshots: false,
            actions_to: "none".into(),
        }
    }
}

/// "Hey Navi": listening all the time for the wake word (see wake.rs).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HeyNaviSettings {
    pub enabled: bool,
    /// What wakes it: "navi", the name on its own (the default), or "hey-navi".
    /// Also what starts a job on a call. A string rather than an enum, so an odd value can't make
    /// settings.json unreadable.
    pub phrase: String,
}

impl Default for HeyNaviSettings {
    fn default() -> Self {
        Self { enabled: false, phrase: "navi".into() }
    }
}

impl HeyNaviSettings {
    /// True when the name on its own wakes Navi ("Navi, email Sam"), not only "Hey Navi". Anything
    /// but "hey-navi" counts, so a missing or odd value gets the default.
    pub fn name_only(&self) -> bool {
        !self.phrase.trim().eq_ignore_ascii_case("hey-navi")
    }

    /// The wake phrase as people say it: "Hey Navi" or "Navi".
    pub fn label(&self) -> &'static str {
        if self.name_only() {
            "Navi"
        } else {
            "Hey Navi"
        }
    }
}

/// "Hey Navi, watch": screenshots while you work, written up afterwards (see watch.rs).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WatchSettings {
    /// Seconds between looks at the screen.
    pub every_secs: u32,
}

impl Default for WatchSettings {
    fn default() -> Self {
        Self { every_secs: 10 }
    }
}

/// ElevenLabs voice playback (the key lives in the Credential Manager as "elevenlabs").
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SpeakSettings {
    pub voice_id: String,
    pub voice_name: String,
    /// ElevenLabs model id, e.g. "eleven_v4_turbo".
    pub model: String,
}

impl Default for SpeakSettings {
    fn default() -> Self {
        Self { voice_id: String::new(), voice_name: String::new(), model: "eleven_v4_turbo".into() }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            onboarded: false,
            user_name: String::new(),
            hotkeys: HotkeySettings::default(),
            sounds: SoundSettings::default(),
            mic: MicSettings::default(),
            engine: EngineSettings::default(),
            ai: AiSettings::default(),
            writing: WritingSettings::default(),
            context: ContextSettings::default(),
            modes: ModeSettings::default(),
            learning: LearningSettings::default(),
            agent: AgentSettings::default(),
            ramble: RambleSettings::default(),
            privacy: PrivacySettings::default(),
            general: GeneralSettings::default(),
            speak: SpeakSettings::default(),
            notes: NotesSettings::default(),
            hey_navi: HeyNaviSettings::default(),
            watch: WatchSettings::default(),
        }
    }
}

/// Key names are logical: "Ctrl", "Shift", "Alt", "Win" match either side; "LCtrl"/"RCtrl",
/// "LShift"/"RShift", "LAlt"/"RAlt", "LWin"/"RWin" match one side; anything else is a key name
/// such as "F9", "Space", "CapsLock", "ScrollLock", "Pause", "Insert", "A".."Z", "0".."9".
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HotkeySettings {
    /// Hold to talk. Release to finish.
    pub talk: Vec<String>,
    /// Tapped while the talk chord is held: switches the take to ramble (hands-free notes).
    pub ramble_key: String,
    /// Double-tap the talk chord to lock hands-free; tap once more to finish.
    pub double_tap_hands_free: bool,
    /// Cancels the take while recording (swallowed so the app underneath never sees it).
    pub cancel_key: String,
    /// Keys that switch mode (F1 for Clean, F2 for Email...). The bubble says which mode is on.
    pub mode_keys: Vec<ModeKey>,
}

/// One mode key. `mode` is a mode id, "auto" (pick from the app) or "next" (the next mode along).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct ModeKey {
    pub mode: String,
    pub keys: Vec<String>,
}

impl Default for HotkeySettings {
    fn default() -> Self {
        Self {
            talk: vec!["Ctrl".into(), "Win".into()],
            ramble_key: "Space".into(),
            double_tap_hands_free: true,
            cancel_key: "Escape".into(),
            mode_keys: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SoundSettings {
    pub enabled: bool,
    /// Sound set id ("hush", "felt", "oak"...; see ui/sounds/sets/index.json).
    pub set: String,
    /// 0.0 to 1.0.
    pub volume: f32,
    /// Cue name ("talk", "ramble", "stop", "done", "cancel", "error", "learn") -> path of a user WAV.
    pub custom: HashMap<String, String>,
}

impl Default for SoundSettings {
    fn default() -> Self {
        Self { enabled: true, set: "hush".into(), volume: 0.7, custom: HashMap::new() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MicSettings {
    /// Device name; None = the Windows default input.
    pub device: Option<String>,
    /// Drop the audio recorded while the start cue plays, so speaker bleed never becomes text.
    pub trim_cue: bool,
}

impl Default for MicSettings {
    fn default() -> Self {
        Self { device: None, trim_cue: true }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EngineSettings {
    /// Catalogue id, see models::catalogue().
    pub model: String,
    /// "en" or "auto".
    pub language: String,
    pub use_gpu: bool,
    /// Explicit whisper-server.exe; otherwise the managed engine folder, then known locations.
    pub server_path: Option<String>,
    /// Explicit model file; otherwise the managed models folder, then known locations.
    pub model_path: Option<String>,
    /// Trim silence with the Silero VAD model when it is installed.
    pub vad: bool,
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            model: "large-v3-turbo".into(),
            language: "en".into(),
            use_gpu: true,
            server_path: None,
            model_path: None,
            vad: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AiSettings {
    /// "off" | "anthropic" | "openai" | "ollama" | "custom" (any OpenAI-compatible endpoint) |
    /// "claude-code" | "codex".
    pub provider: String,
    pub model: String,
    /// Required for "ollama" (default http://localhost:11434) and "custom". For "claude-code" and
    /// "codex": the path to the tool, only when Navi can't find it.
    pub base_url: Option<String>,
    /// The model for jobs nobody waits on (call notes, Short ideas, watch notes, call jobs).
    /// Empty, or one that doesn't suit the provider: Sonnet through Claude Code, otherwise the
    /// same model as dictation (controller.rs make_llm_long).
    pub long_model: String,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self { provider: "off".into(), model: "claude-haiku-4-5-20251001".into(), base_url: None, long_model: String::new() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WritingSettings {
    /// US spellings become British ones (colour, organise, travelled).
    pub british: bool,
    /// Em and en dashes used as punctuation become commas or full stops.
    pub no_em_dashes: bool,
    /// Drop "um", "uh", "er", "you know" style fillers.
    pub remove_fillers: bool,
    /// "How I write": typed, or generated from the samples. Fed to every AI mode.
    pub style_notes: String,
    /// Files of the user's own writing used to generate style_notes.
    pub style_samples: Vec<String>,
}

impl Default for WritingSettings {
    fn default() -> Self {
        Self {
            british: true,
            no_em_dashes: true,
            remove_fillers: true,
            style_notes: String::new(),
            style_samples: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ContextSettings {
    /// App name and window title.
    pub read_app: bool,
    /// Text near the cursor and the selection, read on this machine with UI Automation.
    pub read_field: bool,
    /// On-device OCR of the active window (Windows' own OCR, never uploaded).
    pub ocr: bool,
    /// Process names that are never read, e.g. "1Password.exe".
    pub excluded_apps: Vec<String>,
}

impl Default for ContextSettings {
    fn default() -> Self {
        Self {
            read_app: true,
            read_field: true,
            ocr: false,
            excluded_apps: vec!["1Password.exe".into(), "KeePass.exe".into(), "Bitwarden.exe".into()],
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ModeSettings {
    pub default_mode: String,
    /// Pick a mode from the app you are typing into (Outlook -> Email, Slack -> Message...).
    pub auto_by_app: bool,
    /// The mode every take uses, chosen on the Modes page, by a mode key or from the tray.
    /// Empty means Auto: pick from the app (if `auto_by_app`), else `default_mode`.
    pub active: String,
}

impl Default for ModeSettings {
    fn default() -> Self {
        Self { default_mode: "clean".into(), auto_by_app: true, active: String::new() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LearningSettings {
    pub enabled: bool,
    /// Add a correction to the word list automatically once it has been seen this many times.
    pub auto_add_after: u32,
    /// Seconds after pasting before Navi reads the field back to look for your fixes.
    pub check_after_secs: u32,
}

impl Default for LearningSettings {
    fn default() -> Self {
        Self { enabled: true, auto_add_after: 2, check_after_secs: 25 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ProjectRef {
    pub id: String,
    pub name: String,
    pub path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentSettings {
    pub projects: Vec<ProjectRef>,
    /// Project id used when the window title does not name one.
    pub active_project: Option<String>,
    /// "at" writes @path/to/file (Claude Code, Cursor). "plain" writes the bare path.
    pub mention_style: String,
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self { projects: Vec::new(), active_project: None, mention_style: "at".into() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RambleSettings {
    /// Folder for the Markdown notes; None = Documents\Navi Notes.
    pub notes_dir: Option<String>,
    /// Also paste the tidy notes into the app you were in.
    pub paste_notes: bool,
    /// "none" | "clickup" | "notion".
    pub tasks_to: String,
    pub clickup_list_id: Option<String>,
    pub notion_database_id: Option<String>,
}

impl Default for RambleSettings {
    fn default() -> Self {
        Self { notes_dir: None, paste_notes: false, tasks_to: "none".into(), clickup_list_id: None, notion_database_id: None }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PrivacySettings {
    /// Blocks every network call except model and engine downloads you start yourself.
    pub local_only: bool,
    /// Days to keep recordings; 0 keeps them forever.
    pub keep_audio_days: u32,
    pub save_history: bool,
}

impl Default for PrivacySettings {
    fn default() -> Self {
        Self { local_only: false, keep_audio_days: 30, save_history: true }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GeneralSettings {
    pub launch_at_login: bool,
    pub start_minimised: bool,
    /// "paste" (clipboard + Ctrl+V, restored afterwards) or "type" (keystrokes, slower).
    pub paste_method: String,
    pub restore_clipboard: bool,
    pub show_pill: bool,
    /// The firefly that flies in while you talk. Some people would rather not see it.
    pub show_firefly: bool,
    /// The window's close button hides it and Navi keeps listening in the tray. false: it quits.
    pub close_to_tray: bool,
    /// The one-time "Navi is still running" note after the first close has been shown.
    pub tray_tip_shown: bool,
    /// Look for a new version on GitHub now and then (see update.rs).
    pub check_updates: bool,
    /// "light", "dark" or "system" (follow Windows).
    pub theme: String,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            launch_at_login: true,
            start_minimised: true,
            paste_method: "paste".into(),
            restore_clipboard: true,
            show_pill: true,
            show_firefly: true,
            close_to_tray: true,
            tray_tip_shown: false,
            check_updates: true,
            theme: "light".into(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Modes
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ModeKind {
    /// Exactly what you said (dictionary and snippets still apply).
    Raw,
    /// Offline tidy: punctuation, capitals, fillers, British spelling. AI polish if enabled.
    Clean,
    /// AI rewrite to a register (email, message, formal...).
    Rewrite,
    /// A prompt for a coding agent, with file references from the active project.
    Prompt,
    /// Ramble: structured notes and tasks.
    Notes,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mode {
    pub id: String,
    pub name: String,
    /// Icon key the UI understands: "spark", "clean", "mail", "chat", "terminal", "notes", "pen".
    pub icon: String,
    pub description: String,
    pub kind: ModeKind,
    /// Extra instructions given to the AI for this mode.
    pub instructions: String,
    /// Process names (lower case, e.g. "outlook.exe") that select this mode automatically.
    pub apps: Vec<String>,
    /// false = never call the AI, even when a provider is set.
    pub use_ai: bool,
    pub builtin: bool,
}

// ---------------------------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Take {
    pub id: String,
    /// RFC 3339, local time with offset.
    pub created_at: String,
    pub duration_ms: u64,
    pub app_name: String,
    pub window_title: String,
    pub mode_id: String,
    pub raw_text: String,
    pub final_text: String,
    pub words: u32,
    pub audio_path: Option<String>,
    pub model: String,
    /// From key release to text delivered.
    pub latency_ms: u64,
    /// "pasted" | "typed" | "clipboard" | "notes" | "command" | "cancelled" | "error" | "rerun"
    /// (a failed take transcribed again from History)
    pub status: String,
    /// "talk" | "handsFree" | "ramble" | "command"
    pub kind: String,
    /// Extra data: notes JSON for rambles, the command for voice edits, warnings.
    pub meta: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct HistoryQuery {
    pub search: Option<String>,
    pub app: Option<String>,
    pub mode: Option<String>,
    pub kind: Option<String>,
    pub limit: u32,
    pub offset: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct DayStat {
    /// YYYY-MM-DD, local.
    pub date: String,
    pub words: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Stats {
    pub words_today: u64,
    pub words_week: u64,
    pub words_total: u64,
    pub takes_total: u64,
    /// Typing at 40 wpm versus speaking time.
    pub minutes_saved: f64,
    pub avg_wpm: f64,
    pub streak_days: u32,
    /// Last 14 days, oldest first.
    pub daily: Vec<DayStat>,
}

// ---------------------------------------------------------------------------------------------
// Dictionary, snippets, learned suggestions, packs
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct DictEntry {
    pub id: String,
    /// The correct form, e.g. "Quikly".
    pub term: String,
    /// What the engine tends to write instead, e.g. ["quickly", "quick ly"].
    pub sounds_like: Vec<String>,
    pub note: String,
    /// "manual" | "learned" | a pack id.
    pub source: String,
    pub created_at: String,
    pub uses: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Snippet {
    pub id: String,
    /// Spoken trigger, e.g. "my calendar link".
    pub trigger: String,
    pub expansion: String,
    pub source: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Suggestion {
    pub id: String,
    pub from: String,
    pub to: String,
    pub count: u32,
    pub app_name: String,
    pub last_seen: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Pack {
    /// Lower-case slug, e.g. "acme-team".
    pub id: String,
    pub name: String,
    pub author: String,
    pub description: String,
    pub version: String,
    pub words: Vec<DictEntry>,
    pub snippets: Vec<Snippet>,
    pub modes: Vec<Mode>,
    /// Set when installed from a URL; Navi re-fetches it to stay current.
    pub source_url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct PackSummary {
    pub id: String,
    pub name: String,
    pub author: String,
    pub description: String,
    pub version: String,
    pub words: usize,
    pub snippets: usize,
    pub modes: usize,
    pub source_url: Option<String>,
    pub installed_at: String,
}

// ---------------------------------------------------------------------------------------------
// Engine, models, devices
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct EngineStatus {
    /// "starting" | "ready" | "missing-model" | "missing-engine" | "error" | "stopped"
    pub state: String,
    pub message: String,
    pub model: String,
    /// e.g. "NVIDIA GeForce RTX 5070 (12 GB)".
    pub gpu: Option<String>,
    /// "cuda" | "cpu"
    pub device: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub file: String,
    pub url: String,
    pub size_mb: u32,
    /// "English" or "99 languages".
    pub languages: String,
    /// 1 (slowest) to 5 (fastest).
    pub speed: u8,
    /// 1 to 5.
    pub accuracy: u8,
    pub recommended: bool,
    pub installed: bool,
    pub path: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct DeviceInfo {
    pub name: String,
    pub is_default: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct DownloadProgress {
    /// Model id or "engine".
    pub id: String,
    pub received: u64,
    pub total: u64,
    /// "downloading" | "verifying" | "extracting" | "done" | "error"
    pub state: String,
    pub message: String,
}

// ---------------------------------------------------------------------------------------------
// Context and session
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct AppContext {
    /// HWND of the foreground window when the take started (0 if unknown).
    pub hwnd: isize,
    /// Lower case, e.g. "outlook.exe".
    pub process_name: String,
    pub exe_path: String,
    pub window_title: String,
    /// Up to ~1,500 characters before the caret in the focused field.
    pub field_text: Option<String>,
    /// Text selected in the focused field when the take started.
    pub selection: Option<String>,
    /// "terminal" | "editor" | "chat" | "email" | "browser" | "document" | "other"
    pub category: String,
    /// Friendly app name for the UI, e.g. "Outlook".
    pub app_label: String,
}

/// What the recording pill shows. Emitted as the "pill" event.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum PillState {
    Hidden,
    #[serde(rename_all = "camelCase")]
    Listening { kind: String, mode_name: String, app_label: String },
    #[serde(rename_all = "camelCase")]
    Processing { mode_name: String },
    #[serde(rename_all = "camelCase")]
    Done { preview: String, outcome: String },
    #[serde(rename_all = "camelCase")]
    Error { message: String },
    #[serde(rename_all = "camelCase")]
    Learned { from: String, to: String },
    /// A short note, such as "Navi is still running" after the window is closed.
    #[serde(rename_all = "camelCase")]
    Notice { message: String },
}

/// Events from the hotkey hook to the controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyEvent {
    /// Every key of the talk chord is now down.
    TalkDown,
    /// One key of the talk chord was released after TalkDown.
    TalkUp,
    /// The ramble key was pressed while the talk chord was held (swallowed).
    Ramble,
    /// The cancel key was pressed while recording (swallowed).
    Cancel,
    /// Mode key number n (an index into HotkeySettings::mode_keys) was pressed.
    ModeKey(u8),
    /// Enter was pressed while a hands-free or ramble take was recording (swallowed).
    Finish,
}

/// Spoken edit commands, recognised after the wake word ("Navi, scratch that").
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum VoiceCommand {
    /// "scratch that", "delete that", "undo that".
    Undo,
    /// "make that shorter", "make it more formal": rewrite the last insertion.
    Rewrite { instruction: String },
    /// "change Tuesday to Wednesday" in the last insertion.
    Replace { from: String, to: String },
    /// With text selected: "turn this into bullet points".
    Transform { instruction: String },
    /// "what's wrong with this?": the flaws in the selection (or the last insertion), shown in a
    /// card and never typed.
    Critique { instruction: String },
    /// "watch" / "stop watching": screenshots while you work (see watch.rs).
    Watch { on: bool },
}
