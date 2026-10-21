//! Shared types. Every module speaks in these, and the UI receives them as camelCase JSON.
//! Changing a field here changes the contract with the UI (ui/js/bridge.js mirrors these shapes).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------------------------
// Settings (persisted as settings.json in the app data folder)
// ---------------------------------------------------------------------------------------------

/// Fields no longer kept (`version`, `writing.styleSamples`, `speak.voiceName`: saved, never read)
/// are skipped when an older file is read and left out when it is next saved.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
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
    pub voice_commands: VoiceCommandSettings,
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
    /// "Navi, ..." (or "Hey Navi, ...", following the wake words in Settings > Voice commands) said
    /// on a call gives the AI a job, done while the call goes on.
    pub call_jobs: bool,
    /// Look at the screen every 15 seconds (meeting.rs SHOT_EVERY) and keep a screenshot when it
    /// has changed, so the notes can say what was shown.
    pub call_screenshots: bool,
    /// Where the actions from the notes go: "none" | "clickup" | "notion" (connections under
    /// RambleSettings).
    pub actions_to: String,
}

impl Default for NotesSettings {
    fn default() -> Self {
        Self {
            detect_calls: true,
            system_audio: true,
            auto_stop: true,
            call_jobs: true,
            call_screenshots: false,
            actions_to: "none".into(),
        }
    }
}

/// Settings > Voice commands (spoken.rs, wake.rs): the microphone stays open and Navi listens for
/// its wake word and commands. Off by default.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct VoiceCommandSettings {
    /// Listen all the time. Off by default (Windows shows its microphone icon while on).
    pub enabled: bool,
    /// One entry per action, in VOICE_ACTIONS order (unknown actions from a newer Navi after them).
    pub commands: Vec<VoiceCommandEntry>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct VoiceCommandEntry {
    /// One of VOICE_ACTIONS. A string, so a value from a newer Navi can't make settings.json
    /// unreadable.
    pub action: String,
    /// What you say, as typed ("Hey Navi", "don't worry"). Matched ignoring case and punctuation.
    /// Empty: the command is off (not allowed for "wake").
    pub phrases: Vec<String>,
}

/// Every action, in the order settings.json keeps them. Settings shows "colour" with the other
/// commands said after the wake word.
pub const VOICE_ACTIONS: &[&str] = &["wake", "scratch_that", "watch_on", "watch_off", "notes_on", "notes_off", "cancel", "finish", "colour"];

/// What Whisper writes for silence and noise. Listening for any of these would set Navi off in a
/// quiet room, so Settings refuses them.
const BLOCKED_PHRASES: &[&str] = &[
    "thank you", "thanks", "thank you very much", "thank you so much", "thanks for watching", "thank you for watching", "you", "bye", "bye bye", "ok", "okay", "yes", "no", "so", "um", "uh",
    "hmm", "the", "a", "i",
];

/// Wake phrases people type in front of a command ("Navi, take notes"), whatever the wake words
/// are. They are dropped from the command when it is saved.
const BUILTIN_WAKE_KEYS: &[&str] = &["navi", "hey navi", "hi navi", "ok navi", "okay navi"];

/// The longest phrase Settings accepts, in characters and in words. Commands are short; a long
/// phrase is a sentence, and a sentence said between pauses is dictation.
const MAX_PHRASE_CHARS: usize = 40;
const MAX_PHRASE_WORDS: usize = 6;
/// The most phrases one command can have.
const MAX_PHRASES: usize = 8;
/// Cancel and finish are listened for while you dictate, so a single word has to be at least this
/// long ("cancel" passes, "done" and "stop" don't): short words turn up in dictation all the time.
const MIN_IN_TAKE_LETTERS: usize = 6;

/// Words said around a command that don't change it ("please stop taking notes now").
const LEADING_FILLERS: &[&str] = &["please", "can you", "could you", "just", "now"];
const TRAILING_FILLERS: &[&str] = &["please", "now", "thanks", "thank you"];

impl Default for VoiceCommandSettings {
    fn default() -> Self {
        Self { enabled: false, commands: default_voice_commands() }
    }
}

/// The phrases an action ships with. ui/js/mock.js DEFAULT_VOICE_COMMANDS keeps in step.
fn default_phrases(action: &str) -> &'static [&'static str] {
    match action {
        "wake" => &["Navi", "Hey Navi"],
        "scratch_that" => &["scratch that"],
        "watch_on" => &["watch my screen"],
        "watch_off" => &["stop watching"],
        "notes_on" => &["take notes"],
        "notes_off" => &["stop taking notes"],
        "cancel" => &["don't worry"],
        // Two words, natural, and rarely a whole sentence on its own in the middle of dictation
        // (when it ends a message, finishing is what you wanted anyway).
        "finish" => &["that's all"],
        // Followed by a colour's name: "turn gold", "make it sky" (colours.rs, spoken.rs).
        "colour" => &["turn", "change to", "switch to", "make it"],
        _ => &[],
    }
}

fn default_entry(action: &str) -> VoiceCommandEntry {
    VoiceCommandEntry { action: action.to_string(), phrases: default_phrases(action).iter().map(|p| p.to_string()).collect() }
}

/// Every action with the phrases Navi ships with, in VOICE_ACTIONS order.
pub fn default_voice_commands() -> Vec<VoiceCommandEntry> {
    VOICE_ACTIONS.iter().map(|a| default_entry(a)).collect()
}

/// "Wake word", "Cancel" ... as Settings and error messages name them; the id itself for an
/// unknown action.
pub fn voice_action_label(action: &str) -> &str {
    match action {
        "wake" => "Wake word",
        "scratch_that" => "Scratch that",
        "watch_on" => "Watch my screen",
        "watch_off" => "Stop watching",
        "notes_on" => "Take notes",
        "notes_off" => "Stop taking notes",
        "cancel" => "Cancel",
        "finish" => "Finish",
        "colour" => "Change colour",
        other => other,
    }
}

/// The comparison key for a phrase: lower case; ' and ’ and ‘ removed; every other character that is
/// not a letter or digit becomes a space; runs of spaces collapsed; trimmed.
/// "Don't worry!" -> "dont worry", "Hey, Navi" -> "hey navi".
pub fn phrase_key(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut gap = false;
    for c in s.chars().flat_map(char::to_lowercase) {
        if is_apostrophe(c) {
            continue;
        }
        if c.is_alphanumeric() {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.push(c);
        } else {
            gap = true;
        }
    }
    out
}

/// Two words Whisper may write for one ("do not" for "don't"). The listener (spoken.rs) folds them
/// in phrases and in what it hears, so Settings treats them as the same phrase too (fold_key).
pub const PHRASE_FOLDS: &[(&str, &str, &str)] = &[
    ("do", "not", "dont"),
    ("that", "is", "thats"),
    ("it", "is", "its"),
    ("let", "us", "lets"),
    ("i", "am", "im"),
    ("what", "is", "whats"),
    ("all", "right", "alright"),
    ("can", "not", "cant"),
    ("will", "not", "wont"),
    ("o", "k", "ok"),
];

/// A phrase_key with PHRASE_FOLDS applied: "do not worry" and "don't worry" both give "dont worry".
pub fn fold_key(key: &str) -> String {
    let words: Vec<&str> = key.split(' ').filter(|w| !w.is_empty()).collect();
    let mut out: Vec<&str> = Vec::with_capacity(words.len());
    let mut i = 0;
    while i < words.len() {
        if let Some((_, _, folded)) = PHRASE_FOLDS.iter().find(|(a, b, _)| words[i] == *a && words.get(i + 1) == Some(b)) {
            out.push(folded);
            i += 2;
        } else {
            out.push(words[i]);
            i += 1;
        }
    }
    out.join(" ")
}

fn is_apostrophe(c: char) -> bool {
    matches!(c, '\'' | '\u{2019}' | '\u{2018}')
}

/// A phrase as typed, tidied: nothing but letters, digits or an apostrophe (' or ’) at either end,
/// and one space between words. Punctuation inside stays ("Hey, Navi").
fn clean_phrase(p: &str) -> String {
    let t = p.trim_matches(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '\u{2019}'));
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What follows the first `n` words of `text`, counting words as phrase_key does (runs of letters,
/// digits and apostrophes holding at least one letter or digit). Empty when the words run out.
fn after_words(text: &str, n: usize) -> &str {
    let (mut count, mut in_word, mut has_alnum) = (0, false, false);
    for (i, c) in text.char_indices() {
        if c.is_alphanumeric() || is_apostrophe(c) {
            if !in_word {
                in_word = true;
                has_alnum = false;
            }
            has_alnum |= c.is_alphanumeric();
        } else if in_word {
            in_word = false;
            if has_alnum {
                count += 1;
                if count == n {
                    return &text[i..];
                }
            }
        }
    }
    ""
}

/// `key` (a phrase_key) without the filler words around a command, unless that would leave
/// nothing.
fn without_fillers(key: &str) -> String {
    let mut words: Vec<&str> = key.split(' ').filter(|w| !w.is_empty()).collect();
    loop {
        let before = words.len();
        for filler in LEADING_FILLERS {
            let f: Vec<&str> = filler.split(' ').collect();
            if words.len() > f.len() && words.starts_with(&f) {
                words.drain(..f.len());
            }
        }
        for filler in TRAILING_FILLERS {
            let f: Vec<&str> = filler.split(' ').collect();
            if words.len() > f.len() && words.ends_with(&f) {
                words.truncate(words.len() - f.len());
            }
        }
        if words.len() == before {
            break;
        }
    }
    words.join(" ")
}

/// Where an action comes in VOICE_ACTIONS; unknown actions after all of them.
fn action_rank(action: &str) -> usize {
    VOICE_ACTIONS.iter().position(|a| *a == action).unwrap_or(VOICE_ACTIONS.len())
}

impl VoiceCommandSettings {
    /// The phrases for `action`, cleaned (clean_phrase), empty ones dropped, as typed.
    pub fn phrases(&self, action: &str) -> Vec<String> {
        self.commands.iter().filter(|e| e.action == action).flat_map(|e| e.phrases.iter()).map(|p| clean_phrase(p)).filter(|p| !phrase_key(p).is_empty()).collect()
    }

    /// The first of those, if any.
    pub fn first(&self, action: &str) -> Option<String> {
        self.phrases(action).into_iter().next()
    }

    /// The first wake phrase as typed, or "Navi" when there is none. For "Listen for “Navi”".
    pub fn wake_label(&self) -> String {
        self.first("wake").unwrap_or_else(|| "Navi".into())
    }

    /// How to say a command: "Navi, stop watching" (wake_label, ", ", first phrase). For "wake":
    /// wake_label alone. None when the action has no phrase.
    pub fn say(&self, action: &str) -> Option<String> {
        let first = self.first(action)?;
        if action == "wake" {
            return Some(first);
        }
        Some(format!("{}, {first}", self.wake_label()))
    }

    /// For call jobs (jobs.rs `name_only`): true when a wake phrase's key is exactly "navi".
    pub fn jobs_name_only(&self) -> bool {
        self.phrases("wake").iter().any(|p| phrase_key(p) == "navi")
    }

    /// The non-wake action whose phrase is all of `said`, comparing phrase_key, after dropping
    /// "please", "can you", "could you", "just", "now" from the front and "please", "now",
    /// "thanks", "thank you" from the end. "stop taking notes." -> Some("notes_off"). A colour
    /// phrase counts with a colour after it: "turn gold" -> Some("colour").
    pub fn action_for(&self, said: &str) -> Option<String> {
        let key = phrase_key(said);
        let bare = without_fillers(&key);
        if key.is_empty() {
            return None;
        }
        let with_colour = |p: &str| [&bare, &key].iter().any(|k| k.strip_prefix(p).and_then(|w| w.strip_prefix(' ')).is_some_and(|w| crate::colours::by_word(w).is_some()));
        VOICE_ACTIONS
            .iter()
            .filter(|a| **a != "wake")
            // A phrase that itself starts with a filler ("just stop") still matches as typed.
            .find(|a| self.phrases(a).iter().map(|p| phrase_key(p)).any(|p| if **a == "colour" { with_colour(&p) } else { p == bare || p == key }))
            .map(|a| a.to_string())
    }

    /// Puts `phrases` in place of `action`'s, adding the entry when there is none.
    pub fn set_phrases(&mut self, action: &str, phrases: &[&str]) {
        let phrases: Vec<String> = phrases.iter().map(|p| p.to_string()).collect();
        let mut seen = false;
        self.commands.retain_mut(|e| {
            if e.action != action {
                return true;
            }
            if seen {
                return false;
            }
            seen = true;
            e.phrases = phrases.clone();
            true
        });
        if !seen {
            self.commands.push(VoiceCommandEntry { action: action.to_string(), phrases });
            self.commands.sort_by_key(|e| action_rank(&e.action));
        }
    }

    /// Adds an entry with its default phrases for every VOICE_ACTIONS id that has none. An entry
    /// that exists with no phrases stays empty (the user turned that command off).
    pub fn complete(&mut self) {
        for action in VOICE_ACTIONS {
            if !self.commands.iter().any(|e| e.action == *action) {
                self.commands.push(default_entry(action));
            }
        }
        // Stable, so unknown actions keep their own order after the known ones.
        self.commands.sort_by_key(|e| action_rank(&e.action));
    }

    /// Validates and tidies what Settings saves. Err is a message for the bubble or a toast.
    /// ui/js/mock.js normaliseVoiceCommands is a port of this: keep the two in step.
    pub fn normalised(&self) -> Result<VoiceCommandSettings, String> {
        self.normalised_from(None)
    }

    /// normalised(), with the settings as they were before this save (`before`), so a phrase used
    /// twice is reported against the command that already had it, not the row it was just typed
    /// into.
    pub fn normalised_from(&self, before: Option<&VoiceCommandSettings>) -> Result<VoiceCommandSettings, String> {
        let mut vc = self.clone();
        // 1. Every action there. Then one entry per known action (a hand-edited file could have
        // two: the second joins the first) and unknown ones set aside, untouched.
        vc.complete();
        let mut known: Vec<VoiceCommandEntry> = Vec::new();
        let mut unknown: Vec<VoiceCommandEntry> = Vec::new();
        for e in vc.commands {
            if action_rank(&e.action) == VOICE_ACTIONS.len() {
                unknown.push(e);
            } else if let Some(k) = known.iter_mut().find(|k| k.action == e.action) {
                k.phrases.extend(e.phrases);
            } else {
                known.push(e);
            }
        }

        // 2. Tidy every phrase; drop the ones with nothing to say.
        for e in &mut known {
            e.phrases = e.phrases.iter().map(|p| clean_phrase(p)).filter(|p| !phrase_key(p).is_empty()).collect();
        }

        // 3. "Navi, take notes" typed into Take notes is saved as "take notes": the listener hears
        // the wake word first and matches the command on what follows it.
        let mut wake_keys: Vec<String> = known.iter().filter(|e| e.action == "wake").flat_map(|e| e.phrases.iter().map(|p| phrase_key(p))).collect();
        wake_keys.extend(BUILTIN_WAKE_KEYS.iter().map(|k| k.to_string()));
        wake_keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
        for e in known.iter_mut().filter(|e| e.action != "wake") {
            for p in e.phrases.iter_mut() {
                let key = phrase_key(p);
                for wake in &wake_keys {
                    if key == *wake {
                        return Err(format!("\u{201c}{p}\u{201d} is the wake word. Type what you say after it."));
                    }
                    if key.starts_with(&format!("{wake} ")) {
                        *p = clean_phrase(after_words(p, wake.split(' ').count()));
                        break;
                    }
                }
            }
        }

        // 3b. "turn gold" typed into Change colour is saved as "turn": the listener hears the colour
        // after the phrase. A colour on its own has nothing before it to listen for.
        for e in known.iter_mut().filter(|e| e.action == "colour") {
            for p in e.phrases.iter_mut() {
                let key = phrase_key(p);
                let words = key.split(' ').count();
                if key.rsplit(' ').next().is_some_and(|w| crate::colours::by_word(w).is_some()) {
                    if words == 1 {
                        return Err(format!("\u{201c}{p}\u{201d} is a colour. Type what you say before it, such as \u{201c}turn\u{201d}."));
                    }
                    let colour = after_words(p, words - 1);
                    *p = clean_phrase(&p[..p.len() - colour.len()]);
                }
            }
        }

        // 4. Each phrase on its own.
        for e in &known {
            let in_take = matches!(e.action.as_str(), "cancel" | "finish");
            for p in &e.phrases {
                let key = phrase_key(p);
                let words = key.split(' ').count();
                if p.chars().count() > MAX_PHRASE_CHARS || words > MAX_PHRASE_WORDS {
                    return Err(format!("\u{201c}{p}\u{201d} is too long for a voice command. Keep it to six words or fewer."));
                }
                if BLOCKED_PHRASES.contains(&key.as_str()) {
                    return Err(format!("Navi can\u{2019}t listen for \u{201c}{p}\u{201d}: Whisper often hears it in silence. Pick another phrase."));
                }
                if in_take && words == 1 && key.chars().count() < MIN_IN_TAKE_LETTERS {
                    return Err(format!("\u{201c}{p}\u{201d} is too short to listen for while you\u{2019}re dictating. Use two words or more, such as \u{201c}that\u{2019}s all\u{201d}."));
                }
            }
        }

        // 5. The same phrase twice in one command: the first stays.
        for e in &mut known {
            let mut seen: Vec<String> = Vec::new();
            e.phrases.retain(|p| {
                let key = phrase_key(p);
                if seen.contains(&key) {
                    return false;
                }
                seen.push(key);
                true
            });
        }

        // 6. A few phrases each.
        if known.iter().any(|e| e.phrases.len() > MAX_PHRASES) {
            return Err("Up to eight phrases for each command.".into());
        }

        // 7. One phrase, one action, or Navi couldn't know which you meant. Compared folded, as the
        // listener hears them: "do not worry" is "don't worry".
        let mut taken: Vec<(String, &str, &str)> = Vec::new();
        for e in &known {
            for p in &e.phrases {
                let key = fold_key(&phrase_key(p));
                if let Some((_, earlier, earlier_p)) = taken.iter().find(|(k, _, _)| *k == key) {
                    // Name the command that had it before this save; the phrase as typed in the other.
                    let had = |action: &str| before.is_some_and(|b| b.phrases(action).iter().any(|q| fold_key(&phrase_key(q)) == key));
                    let (owner, typed) = if had(&e.action) && !had(earlier) { (e.action.as_str(), *earlier_p) } else { (*earlier, p.as_str()) };
                    return Err(format!("\u{201c}{typed}\u{201d} is already used for {}.", voice_action_label(owner)));
                }
                taken.push((key, e.action.as_str(), p.as_str()));
            }
        }

        // 8. Something has to wake it.
        if known.iter().all(|e| e.action != "wake" || e.phrases.is_empty()) {
            return Err("Keep at least one wake word.".into());
        }

        // 9. In VOICE_ACTIONS order (complete() sorted them), unknown actions after.
        known.extend(unknown);
        Ok(VoiceCommandSettings { enabled: vc.enabled, commands: known })
    }
}

/// "Navi, watch": screenshots while you work, written up afterwards (see watch.rs).
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
    /// ElevenLabs model id, e.g. "eleven_v4_turbo".
    pub model: String,
}

impl Default for SpeakSettings {
    fn default() -> Self {
        Self { voice_id: String::new(), model: "eleven_v4_turbo".into() }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
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
            voice_commands: VoiceCommandSettings::default(),
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
            // A Mac: Right Option, which needs no change in System Settings (Fn does).
            talk: if cfg!(target_os = "macos") { vec!["RAlt".into()] } else { vec!["Ctrl".into(), "Win".into()] },
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
    /// Sound set id ("bubble", "hush", "felt"...; see ui/sounds/sets/index.json).
    pub set: String,
    /// 0.0 to 1.0.
    pub volume: f32,
    /// Cue name ("talk", "ramble", "stop", "done", "cancel", "error", "learn") -> path of a user WAV.
    pub custom: HashMap<String, String>,
    /// Other apps' sound while you talk: "keep", "pause", "mute" or "both" (see media.rs).
    pub while_talking: String,
}

impl Default for SoundSettings {
    fn default() -> Self {
        Self { enabled: true, set: "bubble".into(), volume: 0.7, custom: HashMap::new(), while_talking: "pause".into() }
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
    /// Required for "custom"; for "ollama", empty means http://127.0.0.1:11434. For "claude-code" and
    /// "codex": the path to the tool, only when Navi can't find it.
    pub base_url: Option<String>,
    /// The model for jobs nobody waits on (call notes, watch notes, call jobs).
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
    /// "How I write": typed, or written by the AI from files of your writing (Settings > Writing >
    /// Learn it from my writing). Fed to every AI mode.
    pub style_notes: String,
}

impl Default for WritingSettings {
    fn default() -> Self {
        Self {
            british: true,
            no_em_dashes: true,
            remove_fillers: true,
            style_notes: String::new(),
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
    /// Keeps Navi off the internet. Blocks AI that runs off this computer (Anthropic, OpenAI,
    /// Claude Code, Codex, and Ollama or a custom server at any other address), ElevenLabs, ClickUp
    /// and Notion (sending notes and tasks, and the Test buttons), packs fetched or refreshed from
    /// the web, the automatic update checks and the automatic download of the silence-trimming
    /// model. Still allowed: model and engine downloads you start, Check for updates and Update now
    /// when you press them, and Ollama or a custom server on this computer.
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
    /// Navi's colour, one of colours.rs COLOURS: "lilac" (the default), "gold", "teal", "sky",
    /// "rose" or "graphite".
    pub colour: String,
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
            colour: crate::colours::DEFAULT.into(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Modes
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ModeKind {
    /// Exactly what you said (spoken punctuation, dictionary and snippets still apply).
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
    /// "pasted" | "typed" | "clipboard" | "notes" | "command" | "spoken" (read aloud by the Speak
    /// mode older versions had) | "cancelled" | "error" | "rerun" (a failed take transcribed again
    /// from History)
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
    /// In a browser, the host in its address bar ("chatgpt.com"), for picking the mode. Only the
    /// host is ever kept, never the rest of the address. Empty when unknown.
    pub host: String,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The default voice commands with some actions' phrases replaced.
    fn vc_with(changes: &[(&str, &[&str])]) -> VoiceCommandSettings {
        let mut vc = VoiceCommandSettings::default();
        for (action, phrases) in changes {
            vc.set_phrases(action, phrases);
        }
        vc
    }

    fn table(vc: &VoiceCommandSettings) -> Vec<(String, Vec<String>)> {
        vc.commands.iter().map(|e| (e.action.clone(), e.phrases.clone())).collect()
    }

    #[test]
    fn voice_commands_start_off_with_the_shipped_phrases() {
        let vc = VoiceCommandSettings::default();
        assert!(!vc.enabled, "the microphone stays closed until you turn it on");
        let expected: Vec<(String, Vec<String>)> = [
            ("wake", vec!["Navi", "Hey Navi"]),
            ("scratch_that", vec!["scratch that"]),
            ("watch_on", vec!["watch my screen"]),
            ("watch_off", vec!["stop watching"]),
            ("notes_on", vec!["take notes"]),
            ("notes_off", vec!["stop taking notes"]),
            ("cancel", vec!["don't worry"]),
            ("finish", vec!["that's all"]),
            ("colour", vec!["turn", "change to", "switch to", "make it"]),
        ]
        .into_iter()
        .map(|(a, p)| (a.to_string(), p.into_iter().map(String::from).collect()))
        .collect();
        assert_eq!(table(&vc), expected);
        assert_eq!(vc.commands.iter().map(|e| e.action.as_str()).collect::<Vec<_>>(), VOICE_ACTIONS);
        assert_eq!(Settings::default().voice_commands, vc);
        assert_eq!(vc.normalised(), Ok(vc.clone()), "the defaults pass their own checks unchanged");
    }

    #[test]
    fn the_ui_sees_voice_commands_in_camel_case() {
        let json = serde_json::to_value(Settings::default()).unwrap();
        assert_eq!(json["voiceCommands"]["enabled"], false);
        assert_eq!(json["voiceCommands"]["commands"][0], serde_json::json!({ "action": "wake", "phrases": ["Navi", "Hey Navi"] }));
        assert_eq!(json["voiceCommands"]["commands"][7], serde_json::json!({ "action": "finish", "phrases": ["that's all"] }));
        assert!(json.get("heyNavi").is_none());
        // Half a section still reads, with the rest from the defaults.
        let s: Settings = serde_json::from_str(r#"{ "voiceCommands": { "enabled": true } }"#).unwrap();
        assert!(s.voice_commands.enabled);
        assert_eq!(s.voice_commands.commands, default_voice_commands());
        // An action from a newer Navi doesn't make the file unreadable.
        let s: Settings = serde_json::from_str(r#"{ "voiceCommands": { "commands": [{ "action": "open_app", "phrases": ["open mail"] }] } }"#).unwrap();
        assert_eq!(s.voice_commands.commands[0].action, "open_app");
    }

    #[test]
    fn phrase_keys_ignore_case_and_punctuation() {
        assert_eq!(phrase_key("Don't worry!"), "dont worry");
        assert_eq!(phrase_key("Hey, Navi"), "hey navi");
        assert_eq!(phrase_key("  Take   NOTES. "), "take notes");
        assert_eq!(phrase_key("That\u{2019}s all"), "thats all");
        assert_eq!(phrase_key("\u{2018}cause"), "cause");
        assert_eq!(phrase_key("watch-my-screen"), "watch my screen");
        assert_eq!(phrase_key("?! '"), "");
        assert_eq!(phrase_key(""), "");
    }

    #[test]
    fn actions_have_labels() {
        assert_eq!(voice_action_label("wake"), "Wake word");
        assert_eq!(voice_action_label("notes_off"), "Stop taking notes");
        assert_eq!(voice_action_label("cancel"), "Cancel");
        assert_eq!(voice_action_label("finish"), "Finish");
        assert_eq!(voice_action_label("colour"), "Change colour");
        assert_eq!(voice_action_label("open_app"), "open_app", "an unknown action is named by its id");
        for action in VOICE_ACTIONS {
            assert_ne!(voice_action_label(action), *action, "{action} has a label");
        }
    }

    #[test]
    fn saving_tidies_phrases_and_keeps_punctuation_inside() {
        let vc = vc_with(&[("wake", &["  Hey,   Navi!! ", "Navi"]), ("cancel", &["  don't worry!! ", "?!"])]).normalised().unwrap();
        assert_eq!(vc.phrases("wake"), ["Hey, Navi", "Navi"]);
        assert_eq!(vc.commands[0].phrases, ["Hey, Navi", "Navi"], "saved as tidied");
        assert_eq!(vc.phrases("cancel"), ["don't worry"], "a phrase with no words in it goes");
        assert_eq!(vc.normalised(), Ok(vc.clone()), "a second save changes nothing");
    }

    #[test]
    fn a_wake_word_typed_into_a_command_is_dropped() {
        let vc = vc_with(&[("notes_on", &["Navi, take notes"]), ("watch_on", &["Hey Navi watch my screen"]), ("notes_off", &["hey, navi - stop taking notes!"]), ("scratch_that", &["OK Navi scratch that"])])
            .normalised()
            .unwrap();
        assert_eq!(vc.phrases("notes_on"), ["take notes"]);
        assert_eq!(vc.phrases("watch_on"), ["watch my screen"]);
        assert_eq!(vc.phrases("notes_off"), ["stop taking notes"]);
        assert_eq!(vc.phrases("scratch_that"), ["scratch that"]);
        // Your own wake word counts too.
        let vc = vc_with(&[("wake", &["Computer"]), ("finish", &["computer, that's all"])]).normalised().unwrap();
        assert_eq!(vc.phrases("finish"), ["that's all"]);
    }

    #[test]
    fn a_colour_typed_into_change_colour_is_dropped() {
        // The listener hears the colour after the phrase, so the phrase is what comes before it.
        let vc = vc_with(&[("colour", &["turn gold", "Navi, go Yellow!", "make it"])]).normalised().unwrap();
        assert_eq!(vc.phrases("colour"), ["turn", "go", "make it"]);
        assert_eq!(vc.normalised(), Ok(vc.clone()), "a second save changes nothing");
        assert_eq!(vc_with(&[("colour", &["Gold!"])]).normalised(), Err("\u{201c}Gold\u{201d} is a colour. Type what you say before it, such as \u{201c}turn\u{201d}.".into()));
        assert_eq!(vc_with(&[("colour", &["Navi, teal"])]).normalised(), Err("\u{201c}teal\u{201d} is a colour. Type what you say before it, such as \u{201c}turn\u{201d}.".into()));
        // A colour word anywhere else is any other word.
        assert!(vc_with(&[("colour", &["go for gold now"])]).normalised().is_ok());
        assert!(vc_with(&[("watch_on", &["gold"])]).normalised().is_ok());
        assert_eq!(vc_with(&[("colour", &["take notes"])]).normalised(), Err("\u{201c}take notes\u{201d} is already used for Take notes.".into()));
    }

    #[test]
    fn a_command_that_is_only_the_wake_word_is_refused() {
        assert_eq!(vc_with(&[("notes_on", &["Hey Navi"])]).normalised(), Err("\u{201c}Hey Navi\u{201d} is the wake word. Type what you say after it.".into()));
        assert_eq!(vc_with(&[("watch_on", &["navi."])]).normalised(), Err("\u{201c}navi\u{201d} is the wake word. Type what you say after it.".into()));
        assert_eq!(vc_with(&[("wake", &["Computer"]), ("cancel", &["computer"])]).normalised(), Err("\u{201c}computer\u{201d} is the wake word. Type what you say after it.".into()));
    }

    #[test]
    fn long_phrases_are_refused() {
        let long = "\u{201c}please would you start watching my screen\u{201d} is too long for a voice command. Keep it to six words or fewer.";
        assert_eq!(vc_with(&[("watch_on", &["please would you start watching my screen"])]).normalised(), Err(long.into()));
        // Six words is fine; so is 40 characters, but not 41.
        assert!(vc_with(&[("watch_on", &["would you start watching my screen"])]).normalised().is_ok());
        assert!(vc_with(&[("watch_on", &["supercalifragilisticexpialidocious scree"])]).normalised().is_ok());
        assert_eq!(
            vc_with(&[("watch_on", &["supercalifragilisticexpialidocious screen"])]).normalised(),
            Err("\u{201c}supercalifragilisticexpialidocious screen\u{201d} is too long for a voice command. Keep it to six words or fewer.".into())
        );
    }

    #[test]
    fn what_whisper_hears_in_silence_is_refused() {
        assert_eq!(
            vc_with(&[("finish", &["Thank you."])]).normalised(),
            Err("Navi can\u{2019}t listen for \u{201c}Thank you\u{201d}: Whisper often hears it in silence. Pick another phrase.".into())
        );
        assert!(vc_with(&[("wake", &["Navi", "OK"])]).normalised().unwrap_err().contains("\u{201c}OK\u{201d}"));
        assert!(vc_with(&[("watch_off", &["bye bye"])]).normalised().is_err());
        // "Hey" on its own is allowed: Settings only warns about it.
        assert!(vc_with(&[("wake", &["Hey"])]).normalised().is_ok());
    }

    #[test]
    fn cancel_and_finish_need_two_words_or_a_long_one() {
        let short = |p: &str| format!("\u{201c}{p}\u{201d} is too short to listen for while you\u{2019}re dictating. Use two words or more, such as \u{201c}that\u{2019}s all\u{201d}.");
        assert_eq!(vc_with(&[("finish", &["done"])]).normalised(), Err(short("done")));
        assert_eq!(vc_with(&[("cancel", &["Stop!"])]).normalised(), Err(short("Stop")));
        assert!(vc_with(&[("cancel", &["cancel"])]).normalised().is_ok());
        assert!(vc_with(&[("cancel", &["stop it"])]).normalised().is_ok());
        // Commands after the wake word can be one short word.
        assert!(vc_with(&[("watch_off", &["stop"])]).normalised().is_ok());
    }

    #[test]
    fn repeats_go_and_eight_phrases_is_the_most() {
        let vc = vc_with(&[("wake", &["Navi", "navi!", "NAVI", "Hey Navi", "hey, navi"])]).normalised().unwrap();
        assert_eq!(vc.phrases("wake"), ["Navi", "Hey Navi"], "the first of each stays");
        let eight = ["scratch that", "delete that", "undo that", "remove that", "lose that", "bin that", "drop that", "cut that"];
        assert!(vc_with(&[("scratch_that", &eight)]).normalised().is_ok());
        let mut nine = eight.to_vec();
        nine.push("forget that");
        assert_eq!(vc_with(&[("scratch_that", &nine)]).normalised(), Err("Up to eight phrases for each command.".into()));
    }

    #[test]
    fn a_phrase_can_belong_to_one_action_only() {
        assert_eq!(vc_with(&[("notes_on", &["Stop watching"])]).normalised(), Err("\u{201c}Stop watching\u{201d} is already used for Stop watching.".into()));
        // The earlier action in VOICE_ACTIONS order is the one named.
        assert_eq!(vc_with(&[("scratch_that", &["take notes"])]).normalised(), Err("\u{201c}take notes\u{201d} is already used for Scratch that.".into()));
        // A wake phrase used for a command is caught earlier, as the wake word.
        assert_eq!(vc_with(&[("wake", &["Navi", "that's all"])]).normalised(), Err("\u{201c}that's all\u{201d} is the wake word. Type what you say after it.".into()));
        // A command typed with the wake word in front is compared without it.
        assert_eq!(vc_with(&[("notes_off", &["Navi, take notes"])]).normalised(), Err("\u{201c}take notes\u{201d} is already used for Take notes.".into()));
        // Saved from Settings, the command that already had the phrase is named, even when it
        // comes later than the row it was typed into.
        let before = VoiceCommandSettings::default();
        assert_eq!(vc_with(&[("scratch_that", &["Navi, take notes"])]).normalised_from(Some(&before)), Err("\u{201c}take notes\u{201d} is already used for Take notes.".into()));
        assert_eq!(vc_with(&[("notes_on", &["Stop watching"])]).normalised_from(Some(&before)), Err("\u{201c}Stop watching\u{201d} is already used for Stop watching.".into()));
        // Two-word forms the listener folds are the same phrase: "do not worry" is "don't worry".
        assert_eq!(vc_with(&[("finish", &["do not worry"])]).normalised_from(Some(&before)), Err("\u{201c}do not worry\u{201d} is already used for Cancel.".into()));
        assert_eq!(vc_with(&[("finish", &["do not worry"])]).normalised(), Err("\u{201c}do not worry\u{201d} is already used for Cancel.".into()));
        assert!(vc_with(&[("finish", &["do nothing more"])]).normalised().is_ok(), "only whole pairs fold");
        assert_eq!(fold_key("do not worry"), "dont worry");
        assert_eq!(fold_key("that is all"), "thats all");
        assert_eq!(fold_key("do"), "do");
    }

    #[test]
    fn something_has_to_wake_navi() {
        assert_eq!(vc_with(&[("wake", &[])]).normalised(), Err("Keep at least one wake word.".into()));
        assert_eq!(vc_with(&[("wake", &["  ", "!!"])]).normalised(), Err("Keep at least one wake word.".into()));
        // Any other command can be turned off by emptying it.
        let vc = vc_with(&[("finish", &[]), ("scratch_that", &[])]).normalised().unwrap();
        assert!(vc.phrases("finish").is_empty() && vc.phrases("scratch_that").is_empty());
        assert_eq!(vc.commands.len(), VOICE_ACTIONS.len(), "an empty command keeps its entry");
    }

    #[test]
    fn unknown_actions_are_kept_last_and_left_alone() {
        let mut vc = VoiceCommandSettings::default();
        vc.commands.insert(0, VoiceCommandEntry { action: "open_app".into(), phrases: vec!["  open mail  ".into(), "Navi".into()] });
        vc.commands.swap(3, 5);
        let out = vc.normalised().unwrap();
        assert_eq!(out.commands.iter().map(|e| e.action.as_str()).take(VOICE_ACTIONS.len()).collect::<Vec<_>>(), VOICE_ACTIONS);
        assert_eq!(out.commands.last(), Some(&VoiceCommandEntry { action: "open_app".into(), phrases: vec!["  open mail  ".into(), "Navi".into()] }));
    }

    #[test]
    fn complete_adds_missing_actions_but_leaves_an_empty_one_empty() {
        let mut vc = VoiceCommandSettings { enabled: true, commands: vec![VoiceCommandEntry { action: "finish".into(), phrases: vec![] }, VoiceCommandEntry { action: "wake".into(), phrases: vec!["Hey".into()] }] };
        vc.complete();
        assert_eq!(vc.commands.iter().map(|e| e.action.as_str()).collect::<Vec<_>>(), VOICE_ACTIONS);
        assert!(vc.phrases("finish").is_empty(), "you turned finish off");
        assert_eq!(vc.phrases("wake"), ["Hey"]);
        assert_eq!(vc.phrases("cancel"), ["don't worry"], "a missing action gets its defaults");
        assert!(vc.enabled);
    }

    #[test]
    fn how_to_say_each_command() {
        let vc = VoiceCommandSettings::default();
        assert_eq!(vc.wake_label(), "Navi");
        assert_eq!(vc.say("wake").as_deref(), Some("Navi"));
        assert_eq!(vc.say("watch_off").as_deref(), Some("Navi, stop watching"));
        assert_eq!(vc.first("cancel").as_deref(), Some("don't worry"));
        let vc = vc_with(&[("wake", &["Hey Navi"]), ("watch_off", &[])]);
        assert_eq!(vc.say("notes_off").as_deref(), Some("Hey Navi, stop taking notes"));
        assert_eq!(vc.say("watch_off"), None, "a command that is off can't be said");
        assert_eq!(vc_with(&[("wake", &["Hey"])]).wake_label(), "Hey");
        assert_eq!(vc_with(&[("wake", &[])]).wake_label(), "Navi");
        assert_eq!(vc_with(&[("wake", &[])]).say("wake"), None);
    }

    #[test]
    fn call_jobs_take_the_name_alone_only_when_it_wakes_navi() {
        assert!(VoiceCommandSettings::default().jobs_name_only());
        assert!(vc_with(&[("wake", &["navi!"])]).jobs_name_only());
        assert!(!vc_with(&[("wake", &["Hey Navi"])]).jobs_name_only());
        assert!(!vc_with(&[("wake", &["Hey"])]).jobs_name_only());
    }

    #[test]
    fn a_whole_line_can_be_a_command() {
        let vc = VoiceCommandSettings::default();
        assert_eq!(vc.action_for("stop taking notes.").as_deref(), Some("notes_off"));
        assert_eq!(vc.action_for("please stop taking notes now").as_deref(), Some("notes_off"));
        assert_eq!(vc.action_for("Can you watch my screen, thanks.").as_deref(), Some("watch_on"));
        assert_eq!(vc.action_for("Just, stop watching please").as_deref(), Some("watch_off"));
        assert_eq!(vc.action_for("draft the email"), None);
        assert_eq!(vc.action_for("stop taking notes on the budget"), None, "the whole line, not the start of it");
        assert_eq!(vc.action_for("navi"), None, "the wake word is not a command");
        assert_eq!(vc.action_for("please"), None);
        assert_eq!(vc.action_for(""), None);
        // A command that is off matches nothing; one that starts with a filler still matches.
        assert_eq!(vc_with(&[("notes_off", &[])]).action_for("stop taking notes"), None);
        assert_eq!(vc_with(&[("watch_off", &["just stop"])]).action_for("just stop").as_deref(), Some("watch_off"));
        // A colour phrase is a command with a colour after it, and only then.
        assert_eq!(vc.action_for("Change to teal.").as_deref(), Some("colour"));
        assert_eq!(vc.action_for("please turn gold").as_deref(), Some("colour"));
        assert_eq!(vc.action_for("make it pink, thanks").as_deref(), Some("colour"));
        for said in ["turn", "change to", "turn orange", "turn gold into cash", "make it shorter"] {
            assert_eq!(vc.action_for(said), None, "{said}");
        }
    }

    #[test]
    fn set_phrases_replaces_or_adds_an_entry() {
        let mut vc = VoiceCommandSettings { enabled: false, commands: vec![] };
        vc.set_phrases("cancel", &["never mind"]);
        vc.set_phrases("wake", &["Hey"]);
        assert_eq!(vc.commands.iter().map(|e| e.action.as_str()).collect::<Vec<_>>(), ["wake", "cancel"]);
        vc.set_phrases("cancel", &["forget it"]);
        assert_eq!(vc.phrases("cancel"), ["forget it"]);
    }
}
