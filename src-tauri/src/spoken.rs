//! Voice commands (Settings > Voice commands): the wake word and the phrases that act straight
//! away, worked out as plain state machines with no I/O, so the unit tests and the live
//! measurement run exactly the code the app runs. wake.rs and controller.rs only feed audio in,
//! send the checks to the speech engine Navi already runs, and carry out what these decide.
//!
//! * `Segmenter`: where each utterance starts and ends (loudness over a slowly adapting noise
//!   floor, frame by frame).
//! * `Matcher`: what the words mean. Whisper spells the name many ways ("Navi", "Navie", "Naavi",
//!   "Navy"), so wake phrases and commands are matched token by token, with fuzzy spelling, and
//!   with no punctuation rule for the name at the start of an utterance. A colour phrase ("turn")
//!   is a command only with one of the colours' words after it ("turn gold"; colours.rs), heard
//!   exactly.
//! * `IdleListener` (wake.rs, no take running): sliding checks every 0.3 s of new speech over the
//!   first 3 s of each utterance, one at a time, the first with the wake words as Whisper's prompt
//!   and the rest without. Only a check that heard the whole utterance gives it up on words that
//!   are not yet a command. Once a check has heard all of "Navi" or "Navi, watch my screen",
//!   `poll` acts the moment the quiet after it is long enough (300 ms, or 200 ms for a command):
//!   "Navi" alone pops the bubble about 0.3 s after you stop, "Navi, [short pause] watch my
//!   screen" runs with no take at all, and "Navi, email Sam..." starts dictation while you are
//!   still talking. On a call, a command may follow a bare "Navi" after a pause.
//! * `TakeListener` (controller.rs, inside a take): short utterances between pauses, for the
//!   cancel and finish phrases ("don't worry", "that's all", word for word), and the first thing
//!   said after the wake word in a take it started (with or without the name in front).
//!
//! Privacy: what the listeners hear is never logged or kept. Only which command fired is logged.

use crate::colours;
use crate::text::cleanup;
use crate::types::{phrase_key, VoiceCommandSettings, PHRASE_FOLDS as FOLDS};
use std::collections::VecDeque;

pub const RATE: usize = 16_000; // audio::SAMPLE_RATE
pub const FRAME: usize = RATE / 50; // 20 ms frames
pub const RING_SECS: usize = 10;
// Segmenter (the same loudness rule as the wake word listener has always used)
pub const OVER_FLOOR_DB: f32 = 10.0;
pub const MIN_SPEECH_DB: f32 = -52.0;
pub const FLOOR_RISE_DB: f32 = 0.02; // per frame: about 1 dB a second; falls at once
pub const START_FRAMES: usize = 3; // 60 ms of speech starts an utterance
pub const END_FRAMES: usize = 15; // 300 ms of quiet ends it (was 30)
pub const LEAD_IN: usize = RATE / 5; // 200 ms kept before the first loud frame
pub const TAIL: usize = RATE / 10; // 100 ms kept after the last loud frame
pub const MIN_LOUD_FRAMES: usize = 8; // 160 ms of loud frames before any check
// Idle listener
pub const FIRST_CHECK: usize = RATE * 2 / 5; // first check 0.40 s after the first loud frame
pub const STEP: usize = RATE * 3 / 10; // then every 0.30 s of new audio
pub const WINDOW: usize = RATE * 3; // checks cover at most 3.0 s after the first loud frame
// Frames (300 ms) of quiet that make "Navi" alone final: as long as the pause that ends an
// utterance, so "Navi, [200 ms] watch my screen" runs the command with no take and no bubble.
pub const QUIET_FOR_WAKE: usize = END_FRAMES;
pub const QUIET_FOR_COMMAND: usize = 10; // frames (200 ms) of quiet that make a command final
pub const MAX_EMPTY_SLIDING: usize = 2; // empty results in a row stop sliding checks
/// Still talking this long after the check that heard the bare name ("Navi, um..."): start the take.
pub const LONG_AFTER_NAME: u64 = RATE as u64;
/// On a call, an utterance that starts within this of a bare wake word is heard as what follows it.
pub const CALL_WAKE_GAP: u64 = (RATE * 3 / 2) as u64;
// In-take listener
pub const MIN_CMD_UTT: usize = RATE / 4; // shorter utterances (coughs) are not checked
pub const MAX_CMD_UTT: usize = RATE * 11 / 5; // 2.2 s: longer is dictation
pub const MAX_FIRST_UTT: usize = RATE * 3; // a wake take's first utterance, wake word included
pub const FIRST_WINDOW: usize = RATE * 4; // the first command can start up to 4 s after the preroll
pub const CONFIRM_QUIET: usize = RATE / 2; // 0.5 s of quiet after the phrase before acting
pub const CUT_MARGIN: usize = RATE * 3 / 20; // finish cuts 150 ms before the phrase
pub const QUEUE_MAX: usize = 2;
// Whisper prompt: only with an utterance's first sliding check (about 0.6 s of audio), where it
// helps Whisper spell a name it has barely heard. With any longer window it drops the name ("Watch
// my screen." for "Navi, watch my screen"), and on a short unclear sound it may write the prompt
// back ("Hey Navi." for "Hmm"), so a bare name heard with it is never acted on alone (judge).
pub const USE_PROMPT: bool = true;
pub const PROMPT_MAX_CHARS: usize = 200;

/// A check that stopped short of the utterance's last loud frame by more than this did not hear
/// the end of it, so quiet alone can't make its result final (a word may be missing).
const HEARD_SLACK: u64 = (FRAME * 6) as u64;
/// How many of the latest loud frames the segmenter remembers (Segmenter::loud_frames_after).
const LOUD_KEPT: usize = 16;

// ---------------------------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Wake,
    ScratchThat,
    WatchOn,
    WatchOff,
    NotesOn,
    NotesOff,
    Cancel,
    Finish,
    /// "Navi, turn gold": the id of the colour heard (colours.rs). In the lists below, and in the
    /// `allowed` lists Matcher takes, ANY_COLOUR stands for all of them (Action::is_in).
    Colour(&'static str),
}

/// The colour command, whatever the colour, in lists of actions.
pub const ANY_COLOUR: Action = Action::Colour("");

/// Said straight after the wake word.
pub const AFTER_WAKE: &[Action] = &[Action::ScratchThat, Action::WatchOn, Action::WatchOff, Action::NotesOn, Action::NotesOff, ANY_COLOUR];
/// Said on their own inside a take.
pub const IN_TAKE: &[Action] = &[Action::Cancel, Action::Finish];
/// All Navi listens for while it takes call notes.
pub const IN_CALL: &[Action] = &[Action::WatchOn, Action::WatchOff, Action::NotesOff];

impl Action {
    /// "wake", "scratch_that", ... as in types::VOICE_ACTIONS.
    pub fn id(self) -> &'static str {
        match self {
            Action::Wake => "wake",
            Action::ScratchThat => "scratch_that",
            Action::WatchOn => "watch_on",
            Action::WatchOff => "watch_off",
            Action::NotesOn => "notes_on",
            Action::NotesOff => "notes_off",
            Action::Cancel => "cancel",
            Action::Finish => "finish",
            Action::Colour(_) => "colour",
        }
    }

    /// ANY_COLOUR for "colour".
    pub fn from_id(id: &str) -> Option<Action> {
        ALL_ACTIONS.iter().copied().find(|a| a.id() == id.trim())
    }

    /// Whether this action is one of `list`, any colour counting as the colour command.
    pub fn is_in(self, list: &[Action]) -> bool {
        list.iter().any(|a| a.id() == self.id())
    }

    pub fn after_wake(self) -> bool {
        self.is_in(AFTER_WAKE)
    }

    pub fn in_take(self) -> bool {
        self.is_in(IN_TAKE)
    }

    pub fn in_call(self) -> bool {
        self.is_in(IN_CALL)
    }
}

/// In types::VOICE_ACTIONS order, which is also the order ties are settled in.
const ALL_ACTIONS: &[Action] = &[Action::Wake, Action::ScratchThat, Action::WatchOn, Action::WatchOff, Action::NotesOn, Action::NotesOff, Action::Cancel, Action::Finish, ANY_COLOUR];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListenMode {
    Normal,
    /// Call notes are running: only the commands that stop the notes or watch the screen.
    Call,
}

// ---------------------------------------------------------------------------------------------
// Audio ring and utterances
// ---------------------------------------------------------------------------------------------

/// 16 kHz audio with the absolute index of its first sample. Holds the last RING_SECS.
#[derive(Default)]
pub struct Ring {
    samples: Vec<f32>,
    first: u64,
}

impl Ring {
    pub fn new() -> Self {
        Self::default()
    }

    /// Absolute index of the oldest sample still held.
    pub fn first(&self) -> u64 {
        self.first
    }

    /// Absolute index just past the newest sample.
    pub fn end(&self) -> u64 {
        self.first + self.samples.len() as u64
    }

    pub fn push(&mut self, new: &[f32]) {
        self.samples.extend_from_slice(new);
        let max = RATE * RING_SECS;
        if self.samples.len() > max {
            let drop = self.samples.len() - max;
            self.samples.drain(..drop);
            self.first += drop as u64;
        }
    }

    /// Audio from absolute index `from` (clamped to what is still held) to the end.
    pub fn since(&self, from: u64) -> Vec<f32> {
        self.slice(from, self.end())
    }

    pub fn range(&self, from: u64, len: usize) -> Vec<f32> {
        self.slice(from, from.saturating_add(len as u64))
    }

    /// Audio from `start` to `end` (absolute indices), clamped to what is held.
    pub fn slice(&self, start: u64, end: u64) -> Vec<f32> {
        let a = start.clamp(self.first, self.end());
        let b = end.clamp(a, self.end());
        self.samples[(a - self.first) as usize..(b - self.first) as usize].to_vec()
    }

    /// Empties the ring; positions carry on counting from where they were.
    pub fn clear(&mut self) {
        self.first = self.end();
        self.samples.clear();
    }
}

/// One utterance, in absolute sample indices.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seg {
    /// First loud frame minus LEAD_IN (clamped to the ring).
    pub start: u64,
    /// First loud frame of the START_FRAMES run.
    pub speech_at: u64,
    /// End of the latest loud frame.
    pub last_loud_end: u64,
    pub loud_frames: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SegEvent {
    Started(Seg),
    Ended(Seg),
}

/// Spots utterances: START_FRAMES loud frames in a row start one, END_FRAMES quiet ones end it.
/// Loud means 10 dB over a noise floor that falls at once to the quiet between words and rises
/// slowly (about 1 dB a second), and never below -52 dBFS.
#[derive(Default)]
pub struct Segmenter {
    floor_db: Option<f32>,
    loud_run: usize,
    quiet_run: usize,
    cur: Option<Seg>,
    /// Absolute index of the next frame to look at.
    next: u64,
    last_loud_end: u64,
    /// Ends of the latest loud frames, newest last (LOUD_KEPT at most).
    recent_loud: VecDeque<u64>,
    /// After a reset no utterance starts until END_FRAMES of quiet have been heard, so speech
    /// already under way (someone mid-sentence as a take ends) is never taken for a new start.
    need_quiet: bool,
}

impl Segmenter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Looks at every whole frame not yet seen.
    pub fn feed(&mut self, ring: &Ring) -> Vec<SegEvent> {
        let mut events = Vec::new();
        if self.next < ring.first {
            self.next = ring.first;
        }
        while self.next + FRAME as u64 <= ring.end() {
            let at = (self.next - ring.first) as usize;
            let db = frame_db(&ring.samples[at..at + FRAME]);
            let floor = self.floor_db.get_or_insert(db);
            *floor = if db < *floor { db } else { *floor + FLOOR_RISE_DB };
            let loud = db > (*floor + OVER_FLOOR_DB).max(MIN_SPEECH_DB);
            let end = self.next + FRAME as u64;
            if loud {
                self.loud_run += 1;
                self.quiet_run = 0;
                self.last_loud_end = end;
                if self.recent_loud.len() >= LOUD_KEPT {
                    self.recent_loud.pop_front();
                }
                self.recent_loud.push_back(end);
            } else {
                self.quiet_run += 1;
                self.loud_run = 0;
                if self.need_quiet && self.quiet_run >= END_FRAMES {
                    self.need_quiet = false;
                }
            }
            match self.cur.as_mut() {
                None if self.loud_run >= START_FRAMES && !self.need_quiet => {
                    let speech_at = end.saturating_sub((START_FRAMES * FRAME) as u64);
                    let start = speech_at.saturating_sub(LEAD_IN as u64).max(ring.first);
                    let seg = Seg { start, speech_at, last_loud_end: end, loud_frames: self.loud_run };
                    self.cur = Some(seg);
                    events.push(SegEvent::Started(seg));
                }
                Some(seg) => {
                    if loud {
                        seg.last_loud_end = end;
                        seg.loud_frames += 1;
                    }
                    if self.quiet_run >= END_FRAMES {
                        events.push(SegEvent::Ended(*seg));
                        self.cur = None;
                    }
                }
                None => {}
            }
            self.next = end;
        }
        events
    }

    /// The utterance under way.
    pub fn current(&self) -> Option<Seg> {
        self.cur
    }

    /// Quiet frames since the last loud one.
    pub fn quiet_run(&self) -> usize {
        self.quiet_run
    }

    /// End of the latest loud frame, across utterances.
    pub fn last_loud_end(&self) -> u64 {
        self.last_loud_end
    }

    /// Loud frames (of the latest LOUD_KEPT) that end after absolute index `pos`.
    pub fn loud_frames_after(&self, pos: u64) -> usize {
        self.recent_loud.iter().rev().take_while(|end| **end > pos).count()
    }

    /// The end of the audio looked at so far.
    pub fn position(&self) -> u64 {
        self.next
    }

    /// Forgets the utterance under way; keeps the noise floor and the position.
    pub fn reset(&mut self) {
        *self = Segmenter { floor_db: self.floor_db, next: self.next, need_quiet: true, ..Default::default() };
    }
}

fn frame_db(frame: &[f32]) -> f32 {
    let rms = (frame.iter().map(|x| x * x).sum::<f32>() / frame.len().max(1) as f32).sqrt();
    if rms > 1e-7 {
        20.0 * rms.log10()
    } else {
        -140.0
    }
}

// ---------------------------------------------------------------------------------------------
// Matching words
// ---------------------------------------------------------------------------------------------

/// Whisper's spellings of the name that are not English words.
const NAVI_STRONG: &[&str] = &["navi", "naavi", "navie", "nevi", "navee", "navii", "nahvi", "naavie", "navvi", "knavi", "nahvee", "naavee"];
/// Spellings that are also real words, so they need more to go on. "Nav" never counts: it is a
/// word and a name.
const NAVI_WEAK: &[&str] = &["navy", "navvy", "navey", "nervy", "nervi"];
/// Greetings Whisper swaps for one another.
const GREETING_GROUPS: &[&[&str]] = &[&["hey", "hay", "hi", "hei", "heya", "hiya"], &["ok", "okay"]];
/// Greetings before the bare name ("OK Navi", "Hello Navi").
const GREETINGS: &[&str] = &["hey", "hay", "hi", "hei", "ok", "okay", "oi", "hello"];
/// Padding dropped from the front of a command ("can you take notes").
const LEAD_FILLERS: &[&[&str]] = &[
    &["please"],
    &["can", "you"],
    &["could", "you"],
    &["would", "you"],
    &["will", "you"],
    &["just"],
    &["now"],
    &["ok"],
    &["okay"],
    &["and"],
    &["um"],
    &["uh"],
    &["er"],
    &["so"],
];
/// Padding dropped from the end of a command ("take notes now please").
const TRAIL_FILLERS: &[&[&str]] = &[&["please"], &["now"], &["thanks"], &["thank", "you"], &["for", "me"]];
/// The first word of a two-word filler: "Navi, can" may yet be "Navi, can you take notes".
const FILLER_STARTS: &[&str] = &["can", "could", "would", "will"];
/// After the bare name with no pause, these make it talk about Navi ("Navi is our new app", "Navi
/// can't hear me", "Navi just crashed"). A word ending in a single "s" ("Navi types what you say")
/// counts too (talks_about_it).
const TALK_WORDS: &[&str] = &[
    "is", "was", "isnt", "wasnt", "has", "hasnt", "had", "does", "doesnt", "did", "didnt", "s", "seems", "looks", "works", "worked", "keeps", "kept", "said",
    "says", "and", "or", "but", "of", "from", "cant", "cannot", "couldnt", "wont", "wouldnt", "should", "shouldnt", "might", "may", "must", "just", "also",
    "really", "never", "always", "still", "only",
];
/// Talk about Navi unless "you" follows: "Navi can't hear me" and "Navi will do that" are talk,
/// "Navi can you take notes" is not.
const MODALS: &[&str] = &["can", "could", "will", "would"];

#[derive(Clone, Debug, PartialEq)]
struct Tok {
    key: String,
    start: usize,
    end: usize,
    /// Whisper wrote a pause (a comma, full stop, spaced dash...) straight after this word.
    pause_after: bool,
}

/// Words (runs of letters, digits and apostrophes) with their byte spans and comparison keys,
/// two-word forms folded ("do not" -> "dont").
fn tokens(text: &str) -> Vec<Tok> {
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        let word = c.is_alphanumeric() || matches!(c, '\'' | '\u{2019}' | '\u{2018}');
        match (word, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                spans.push((s, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        spans.push((s, text.len()));
    }
    let mut toks: Vec<Tok> = spans
        .into_iter()
        .filter_map(|(s, e)| {
            let key = phrase_key(&text[s..e]).replace(' ', "");
            (!key.is_empty()).then_some(Tok { key, start: s, end: e, pause_after: false })
        })
        .collect();
    for i in 0..toks.len() {
        let next_start = toks.get(i + 1).map_or(text.len(), |t| t.start);
        toks[i].pause_after = is_pause(&text[toks[i].end..next_start]);
    }
    let mut out: Vec<Tok> = Vec::with_capacity(toks.len());
    let mut i = 0;
    while i < toks.len() {
        if i + 1 < toks.len() && !toks[i].pause_after {
            if let Some((_, _, folded)) = FOLDS.iter().find(|(a, b, _)| toks[i].key == *a && toks[i + 1].key == *b) {
                out.push(Tok { key: folded.to_string(), start: toks[i].start, end: toks[i + 1].end, pause_after: toks[i + 1].pause_after });
                i += 2;
                continue;
            }
        }
        out.push(toks[i].clone());
        i += 1;
    }
    out
}

/// True when the text between two words holds a pause: `, . ! ? ; : … — –` or a hyphen with a
/// space after it ("Navy-blue" stays one thought).
fn is_pause(between: &str) -> bool {
    if between.chars().any(|c| matches!(c, ',' | '.' | '!' | '?' | ';' | ':' | '\u{2026}' | '\u{2014}' | '\u{2013}')) {
        return true;
    }
    let chars: Vec<char> = between.chars().collect();
    chars.windows(2).any(|w| w[0] == '-' && w[1].is_whitespace())
}

fn keys_of(text: &str) -> Vec<String> {
    tokens(text).into_iter().map(|t| t.key).collect()
}

/// `text` without its last word (empty with one word or none).
fn without_last_word(text: &str) -> &str {
    let t = tokens(text);
    if t.len() < 2 {
        return "";
    }
    &text[..t[t.len() - 1].start]
}

/// Whether heard word `heard` is phrase word `phrase`: Some(weak) when it is, weak when the name
/// was heard as a real word ("navy").
fn tok_eq(phrase: &str, heard: &str) -> Option<bool> {
    if phrase == heard {
        return Some(false);
    }
    if phrase == "navi" {
        return if NAVI_STRONG.contains(&heard) {
            Some(false)
        } else if NAVI_WEAK.contains(&heard) {
            Some(true)
        } else {
            None
        };
    }
    if GREETING_GROUPS.iter().any(|g| g.contains(&phrase) && g.contains(&heard)) {
        return Some(false);
    }
    // Fuzzy, for longer words only, and never a longer word that merely starts with the phrase
    // word: "computers" is not "computer", "watches" is not "watch". Never a colour's name either:
    // "Greek" is not "green", "graphic" is not "graphite".
    let len = phrase.chars().count();
    if len < 5 || heard.chars().next() != phrase.chars().next() || heard.starts_with(phrase) || colours::by_word(phrase).is_some() {
        return None;
    }
    let max = if len >= 8 { 2 } else { 1 };
    (strsim::levenshtein(phrase, heard) <= max).then_some(false)
}

/// Whether heard word `heard` is wake-phrase word `phrase`, Some(weak) as tok_eq. Stricter than
/// tok_eq, because a wake word is listened for in everything said: the name keeps its own spellings;
/// greetings swap within their group only inside a longer phrase ("Hi Navi" for "Hey Navi", but
/// "Hi Sam" never wakes a one-word "Hey"); any other word must be exact, so "Alex" never wakes
/// "Alexa" and "Commuter" never wakes "Computer".
fn wake_tok_eq(phrase: &str, heard: &str, longer_phrase: bool) -> Option<bool> {
    if phrase == "navi" || phrase == heard {
        return tok_eq(phrase, heard);
    }
    (longer_phrase && GREETING_GROUPS.iter().any(|g| g.contains(&phrase) && g.contains(&heard))).then_some(false)
}

/// True when `heard` could be the start of phrase word `phrase`, cut off by the end of a check:
/// "wat" for "watch", "nav" or "nah" for "navi".
fn is_partial(phrase: &str, heard: &str) -> bool {
    let fits = |word: &str| heard.len() >= 2 && heard.len() < word.len() && word.starts_with(heard);
    if phrase == "navi" {
        return NAVI_STRONG.iter().chain(NAVI_WEAK).any(|w| fits(w));
    }
    fits(phrase)
}

/// The wake phrase `phrase` at `heard[at..]`, word for word: Some(weak) when it matches.
fn match_seq(phrase: &[String], heard: &[Tok], at: usize) -> Option<bool> {
    if phrase.is_empty() || heard.len() < at + phrase.len() {
        return None;
    }
    let mut weak = false;
    for (p, h) in phrase.iter().zip(&heard[at..]) {
        weak |= wake_tok_eq(p, &h.key, phrase.len() > 1)?;
    }
    Some(weak)
}

fn is_name(key: &str) -> bool {
    tok_eq("navi", key).is_some()
}

/// The words left once the padding is dropped from the front and back (never all of them).
fn strip_fillers(keys: &[String], leading_only: bool) -> &[String] {
    let starts = |s: &[String], f: &[&str]| s.len() > f.len() && s.iter().zip(f).all(|(a, b)| a == b);
    let ends = |s: &[String], f: &[&str]| s.len() > f.len() && s[s.len() - f.len()..].iter().zip(f).all(|(a, b)| a == b);
    let mut s = keys;
    while let Some(f) = LEAD_FILLERS.iter().find(|f| starts(s, f)) {
        s = &s[f.len()..];
    }
    if !leading_only {
        loop {
            if let Some(f) = TRAIL_FILLERS.iter().find(|f| ends(s, f)) {
                s = &s[..s.len() - f.len()];
            } else if s.len() > 1 && is_name(&s[s.len() - 1]) {
                s = &s[..s.len() - 1];
            } else {
                break;
            }
        }
    }
    s
}

/// True when every word is padding, or the start of it ("please", "can you", "can").
fn only_fillers(keys: &[String]) -> bool {
    let mut s = keys;
    while let Some(f) = LEAD_FILLERS.iter().find(|f| s.len() >= f.len() && s.iter().zip(f.iter()).all(|(a, b)| a == b)) {
        s = &s[f.len()..];
    }
    s.is_empty() || (s.len() == 1 && FILLER_STARTS.contains(&s[0].as_str()))
}

/// What a wake hit found.
#[derive(Clone, Debug, PartialEq)]
pub struct WakeHit {
    /// The name was heard as a real word ("navy", "navvy", "nervy").
    pub weak: bool,
    /// Whisper wrote punctuation straight after the wake phrase.
    pub paused: bool,
    /// The matched phrase is a single word: Navi's name on its own, or a one-word wake word such
    /// as "Computer". Those are the ones people also talk about ("Navi is our new app").
    pub single_name: bool,
    /// The raw text after the phrase and its punctuation, trimmed.
    pub rest: String,
}

/// Wake phrases and commands from Settings > Voice commands. Rebuild it when they change.
#[derive(Clone, Debug)]
pub struct Matcher {
    /// Word keys of each wake phrase, longest first.
    wake: Vec<Vec<String>>,
    /// Word keys of each command phrase, in VOICE_ACTIONS order.
    commands: Vec<(Action, Vec<String>)>,
    prompt: String,
}

impl Matcher {
    pub fn new(vc: &VoiceCommandSettings) -> Self {
        let mut typed = vc.phrases(Action::Wake.id());
        if typed.is_empty() {
            // Settings always keep one (normalised); never listen for nothing.
            typed.push("Navi".to_string());
        }
        let mut wake: Vec<Vec<String>> = typed.iter().map(|p| keys_of(p)).filter(|k| !k.is_empty()).collect();
        wake.sort_by(|a, b| b.len().cmp(&a.len()).then(b.join(" ").len().cmp(&a.join(" ").len())));
        wake.dedup();
        let mut commands = Vec::new();
        for &action in ALL_ACTIONS.iter().filter(|a| **a != Action::Wake) {
            for p in vc.phrases(action.id()) {
                let keys = keys_of(&p);
                if keys.is_empty() {
                    continue;
                }
                if action == ANY_COLOUR {
                    // A colour phrase is said with a colour after it: "turn" becomes "turn gold",
                    // "turn yellow", "turn teal" and so on, each a command with its colour.
                    for c in colours::COLOURS {
                        for w in c.words {
                            commands.push((Action::Colour(c.id), [keys.clone(), vec![w.to_string()]].concat()));
                        }
                    }
                } else {
                    commands.push((action, keys));
                }
            }
        }
        Self { wake, commands, prompt: build_prompt(&typed, USE_PROMPT) }
    }

    fn has_bare_name(&self) -> bool {
        self.wake.iter().any(|p| p.len() == 1 && p[0] == "navi")
    }

    /// A wake phrase at `heard[at..]`: (where the words after it start, weak, single word).
    fn wake_at(&self, heard: &[Tok], at: usize) -> Option<(usize, bool, bool)> {
        for p in &self.wake {
            if let Some(weak) = match_seq(p, heard, at) {
                return Some((at + p.len(), weak, p.len() == 1));
            }
        }
        // "OK Navi", "Hello Navi": a greeting before the bare name. Said to Navi, not about it.
        if heard.get(at).is_some_and(|t| GREETINGS.contains(&t.key.as_str())) && self.has_bare_name() {
            if let Some(weak) = match_seq(&["navi".to_string()], heard, at + 1) {
                return Some((at + 2, weak, false));
            }
        }
        None
    }

    /// The wake phrase at the very start of `text`, any spelling, with or without a pause after it.
    /// A repeated name ("Navi. Navi, email Sam") is stripped too.
    pub fn match_wake(&self, text: &str) -> Option<WakeHit> {
        let heard = tokens(text);
        let (mut end, mut weak, single_name) = self.wake_at(&heard, 0)?;
        for _ in 0..2 {
            match self.wake_at(&heard, end) {
                Some((next, w, _)) => {
                    weak &= w;
                    end = next;
                }
                None => break,
            }
        }
        let paused = heard[end - 1].pause_after;
        let rest = heard.get(end).map_or("", |t| text[t.start..].trim()).to_string();
        Some(WakeHit { weak, paused, single_name, rest })
    }

    /// True when `text` may be a wake phrase not yet finished: nothing yet, part of a phrase
    /// ("Hey" for "Hey Navi", "Nav" cut off mid-word), or a lone greeting while the bare name wakes.
    pub fn could_be_wake_start(&self, text: &str) -> bool {
        let heard = tokens(text);
        let n = heard.len();
        if n == 0 {
            return true;
        }
        let begins = |p: &[String]| {
            let longer = p.len() > 1;
            n <= p.len()
                && heard[..n - 1].iter().zip(p).all(|(h, w)| wake_tok_eq(w, &h.key, longer).is_some())
                && ((n < p.len() && wake_tok_eq(&p[n - 1], &heard[n - 1].key, longer).is_some()) || is_partial(&p[n - 1], &heard[n - 1].key))
        };
        if self.wake.iter().any(|p| begins(p)) {
            return true;
        }
        // One short word on the same letter: Whisper's guess at a name cut off mid-sound ("Now",
        // "Ne" for "Navi"). Only ever a reason to look again, never to wake.
        let first_letter = |s: &str| s.chars().next();
        if n == 1 && heard[0].key.chars().count() <= 3 && self.wake.iter().any(|p| first_letter(&p[0]) == first_letter(&heard[0].key)) {
            return true;
        }
        let greeting = |t: &Tok| GREETINGS.contains(&t.key.as_str());
        self.has_bare_name() && greeting(&heard[0]) && (n == 1 || (n == 2 && is_partial("navi", &heard[1].key)))
    }

    /// The command among `allowed` that is all of `rest`, word for word, once padding ("please",
    /// "can you", "now") is dropped. An exact match wins; otherwise the first in VOICE_ACTIONS order.
    pub fn match_command(&self, rest: &str, allowed: &[Action]) -> Option<Action> {
        let keys = keys_of(rest);
        if keys.is_empty() {
            return None;
        }
        let trimmed = strip_fillers(&keys, false);
        let candidates: Vec<&[String]> = if trimmed.len() == keys.len() { vec![&keys[..]] } else { vec![&keys[..], trimmed] };
        let allowed_phrases = || self.commands.iter().filter(move |(a, _)| a.is_in(allowed));
        for cand in &candidates {
            if let Some((a, _)) = allowed_phrases().find(|(_, p)| p.as_slice() == *cand) {
                return Some(*a);
            }
        }
        for cand in &candidates {
            let fits = |p: &[String]| p.len() == cand.len() && p.iter().zip(cand.iter()).all(|(w, h)| tok_eq(w, h).is_some());
            if let Some((a, _)) = allowed_phrases().find(|(_, p)| fits(p.as_slice())) {
                return Some(*a);
            }
        }
        None
    }

    /// True when `rest` may be the start of a command among `allowed` ("watch my" for "watch my
    /// screen", "wat" cut off mid-word), or only padding so far ("please", "can you").
    pub fn is_command_prefix(&self, rest: &str, allowed: &[Action]) -> bool {
        let keys = keys_of(rest);
        if keys.is_empty() {
            return false;
        }
        if only_fillers(&keys) {
            return true;
        }
        let cand = strip_fillers(&keys, true);
        let n = cand.len();
        self.commands.iter().filter(|(a, _)| a.is_in(allowed)).any(|(_, p)| {
            n <= p.len()
                && cand[..n - 1].iter().zip(p).all(|(h, w)| tok_eq(w, h).is_some())
                && ((n < p.len() && tok_eq(&p[n - 1], &cand[n - 1]).is_some()) || is_partial(&p[n - 1], &cand[n - 1]))
        })
    }

    /// The phrase among `allowed` that is all of `rest`, word for word, with no padding dropped:
    /// only a trailing name ("don't worry, Navi") may be left over. For cancel and finish, said
    /// while you dictate, where "Okay, don't worry" or "Please cancel" may well be for the reader.
    pub fn match_exact(&self, rest: &str, allowed: &[Action]) -> Option<Action> {
        let mut keys = keys_of(rest);
        if keys.len() > 1 && keys.last().is_some_and(|k| is_name(k)) {
            keys.pop();
        }
        if keys.is_empty() {
            return None;
        }
        let allowed_phrases = || self.commands.iter().filter(move |(a, _)| a.is_in(allowed));
        if let Some((a, _)) = allowed_phrases().find(|(_, p)| *p == keys) {
            return Some(*a);
        }
        let fits = |p: &[String]| p.len() == keys.len() && p.iter().zip(&keys).all(|(w, h)| tok_eq(w, h).is_some());
        allowed_phrases().find(|(_, p)| fits(p)).map(|(a, _)| *a)
    }

    /// For a window that stopped mid-speech: is_command_prefix with the last word taken as cut off
    /// ("stop walking" for "stop watch[ing]"), or run on into the next word ("scratching" for
    /// "scratch th[at]", "stopwatching"). Only ever a reason to wait for the next window.
    pub fn may_start_command(&self, rest: &str, allowed: &[Action]) -> bool {
        if self.is_command_prefix(rest, allowed) || self.is_command_prefix(without_last_word(rest), allowed) {
            return true;
        }
        let keys = keys_of(rest);
        let Some((last, before)) = keys.split_last() else { return false };
        let before = strip_fillers(before, true);
        let n = before.len();
        self.commands.iter().filter(|(a, _)| a.is_in(allowed)).any(|(_, p)| {
            n < p.len() && before.iter().zip(p).all(|(h, w)| tok_eq(w, h).is_some()) && last.len() > p[n].len() && last.starts_with(p[n].as_str())
        })
    }

    /// Cancel or Finish, said on its own inside a take, optionally after the wake word.
    pub fn match_in_take(&self, text: &str) -> Option<Action> {
        if self.is_echo(text) {
            return None;
        }
        let target = match self.match_wake(text) {
            Some(hit) if keys_of(&hit.rest).is_empty() => return None,
            Some(hit) => hit.rest,
            None => text.to_string(),
        };
        self.match_exact(&target, IN_TAKE)
    }

    /// The words after the wake phrase (weak spellings too), or None without one.
    pub fn strip_wake(&self, text: &str) -> Option<String> {
        self.match_wake(text).map(|hit| hit.rest)
    }

    /// `text` without a last sentence that is one of `action`'s phrases ("See you Friday. That's
    /// all." -> "See you Friday."). Empty when the whole text is the phrase.
    pub fn strip_trailing(&self, text: &str, action: Action) -> String {
        // Word for word, as the take's listener matches it while you talk.
        let is_phrase = |s: &str| {
            let target = self.match_wake(s).map(|hit| hit.rest).unwrap_or_else(|| s.to_string());
            self.match_exact(&target, &[action]).is_some()
        };
        let t = text.trim_end();
        if is_phrase(t) {
            return String::new();
        }
        let mut last_start = None;
        for (i, c) in t.char_indices() {
            let after = i + c.len_utf8();
            if matches!(c, '.' | '!' | '?') && t[after..].chars().any(char::is_alphanumeric) {
                last_start = Some(after);
            }
        }
        match last_start {
            Some(s) if is_phrase(&t[s..]) => t[..s].trim_end().to_string(),
            _ => text.to_string(),
        }
    }

    /// True for talk about Navi rather than to it: the bare name, no pause, then "is", "was",
    /// "can't", "just", a word ending in a single "s" and so on ("Navi is our new app", "Navi types
    /// what you say"). "Navi, is it raining?" and "Navi can you take notes" are not. A command or
    /// the start of one never counts as talk (judge checks that first).
    pub fn talking_about(&self, hit: &WakeHit) -> bool {
        if !hit.single_name || hit.paused {
            return false;
        }
        let keys = keys_of(&hit.rest);
        let Some(first) = keys.first().map(String::as_str) else { return false };
        if MODALS.contains(&first) {
            return keys.get(1).is_none_or(|k| k != "you");
        }
        TALK_WORDS.contains(&first) || (first.len() >= 3 && first.ends_with('s') && !first.ends_with("ss") && !first.ends_with("us"))
    }

    /// True when Whisper echoed its prompt or reeled off several command phrases: counts as
    /// nothing heard.
    pub fn is_echo(&self, text: &str) -> bool {
        let heard = keys_of(text);
        let has_run = |p: &[String]| heard.windows(p.len()).any(|w| w.iter().zip(p).all(|(h, k)| tok_eq(k, h).is_some()));
        let mut found: Vec<&Vec<String>> = Vec::new();
        for (_, p) in &self.commands {
            if !found.contains(&p) && has_run(p) {
                found.push(p);
            }
        }
        if found.len() >= 2 {
            return true;
        }
        // The prompt read back: with two or more wake phrases it holds the name twice ("navi hey
        // navi"), which nobody says. With one phrase it is the phrase itself, so never an echo.
        let prompt = keys_of(&self.prompt);
        let n = prompt.len().min(6);
        self.wake.len() > 1 && n >= 2 && heard.windows(n).any(|w| w == &prompt[..n])
    }

    /// The prompt for an utterance's first sliding check (IdleCheck::first), and no other: the wake
    /// phrases ("Navi, Hey Navi."), so Whisper spells a name it has barely heard the way it is
    /// typed. The commands are plain English and left out, so an echoed prompt can never be a
    /// command; a bare name heard with it needs a check without it too (judge).
    pub fn prompt(&self) -> String {
        self.prompt.clone()
    }
}

fn build_prompt(wake: &[String], on: bool) -> String {
    if !on || wake.is_empty() {
        return String::new();
    }
    let p = format!("{}.", wake.join(", "));
    if p.chars().count() > PROMPT_MAX_CHARS {
        p.chars().take(PROMPT_MAX_CHARS).collect()
    } else {
        p
    }
}

/// What both listeners treat as nothing heard: Whisper's silence phrases, noise tags, echoes.
fn heard_nothing(text: &str, m: &Matcher) -> bool {
    cleanup::is_noise_marker(text) || cleanup::is_hallucination(text) || m.is_echo(text)
}

// ---------------------------------------------------------------------------------------------
// The idle listener (no take running)
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdleCheck {
    pub id: u64,
    /// Which utterance it is for.
    pub utt: u64,
    pub start: u64,
    pub end: u64,
    /// The last check of the utterance: it ended, or reached WINDOW.
    pub final_check: bool,
    /// The utterance's first sliding check: the only one sent with the Whisper prompt
    /// (Matcher::prompt), never a final check.
    pub first: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum IdleDecision {
    /// Keep listening to this utterance.
    Wait,
    /// Not for Navi: no more checks.
    Ignore,
    /// Start a take with the audio from `start`. The listener has judged the audio up to
    /// `heard_to` (the wake word): the take's own listener looks at what comes after it.
    Wake { start: u64, heard_to: u64, heard: String, after_ms: u32 },
    /// Run `action` with no take. `start..end` is the utterance, for History.
    Command { action: Action, start: u64, end: u64, heard: String, after_ms: u32 },
}

/// What one result means, before positions are filled in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    Wait,
    Ignore,
    Wake,
    Command(Action),
}

#[derive(Clone, Debug)]
struct Utt {
    id: u64,
    seg: Seg,
    ended: bool,
    /// End of the last check sent.
    checked_to: Option<u64>,
    decided: bool,
    /// End of the first check that heard the wake word with nothing after it.
    name_at: Option<u64>,
    /// A check without the prompt heard the wake word alone, its strong form (call mode remembers
    /// it for the next utterance).
    bare_wake: bool,
    /// Some check heard the wake word at the start of this utterance.
    heard_wake: bool,
    /// Send the next check at once (a bare name heard with the prompt wants a second opinion).
    check_now: bool,
    /// A decision that only needs more quiet (IdleListener::poll): what, the end of the check that
    /// heard it, the quiet frames it needs, and the words.
    when_quiet: Option<(Verdict, u64, usize, String)>,
    /// Empty results in a row.
    empties: usize,
    final_sent: bool,
}

impl Utt {
    fn new(id: u64, seg: Seg) -> Self {
        Self { id, seg, ended: false, checked_to: None, decided: false, name_at: None, bare_wake: false, heard_wake: false, check_now: false, when_quiet: None, empties: 0, final_sent: false }
    }
}

/// What on_result knows about the check behind a result, for judge.
#[derive(Clone, Copy, Debug)]
struct Seen {
    /// The check's audio held the whole utterance.
    covered: bool,
    /// Quiet frames since the utterance's last loud one, now (usize::MAX once it has ended).
    quiet: usize,
    /// The check reached (nearly) the last loud frame, so quiet can make it final.
    heard_all: bool,
    /// The check went with the Whisper prompt.
    prompted: bool,
    /// The utterance's first sliding check.
    first: bool,
    /// Another utterance has started since this one ended.
    superseded: bool,
    /// Where the check's audio ends.
    end: u64,
    /// On a call: the utterance began just after a bare wake word.
    after_call_wake: bool,
}

impl Seen {
    /// One ended utterance, heard in full by an unprompted final check.
    #[cfg(test)]
    fn ended() -> Self {
        Self { covered: true, quiet: usize::MAX, heard_all: true, prompted: false, first: false, superseded: false, end: RATE as u64 * 2, after_call_wake: false }
    }

    /// A sliding check, unprompted, while the utterance goes on.
    #[cfg(test)]
    fn sliding(quiet: usize, heard_all: bool) -> Self {
        Self { covered: false, quiet, heard_all, ..Self::ended() }
    }
}

/// Listens for the wake word (and a command after it) at the start of each utterance while no
/// take is running. One check in flight at a time.
pub struct IdleListener {
    sliding: bool,
    seg: Segmenter,
    /// The utterance under way (or the last one).
    cur: Option<Utt>,
    /// An utterance that ended undecided while another began: its final check comes first.
    prev: Option<Utt>,
    in_flight: Option<IdleCheck>,
    next_check_id: u64,
    next_utt: u64,
    /// Call mode: the utterance that was the wake word alone, and how late the next may start to
    /// count as what follows it ("Navi." [pause] "stop taking notes").
    call_wake: Option<(u64, u64)>,
}

impl IdleListener {
    /// `sliding` false: only end-of-utterance checks (the CPU engine, or a busy GPU, is too slow
    /// for more).
    pub fn new(sliding: bool) -> Self {
        Self { sliding, seg: Segmenter::new(), cur: None, prev: None, in_flight: None, next_check_id: 0, next_utt: 0, call_wake: None }
    }

    pub fn set_sliding(&mut self, on: bool) {
        self.sliding = on;
    }

    /// Segments every new frame in the ring. Call once per tick before results and next_check.
    pub fn advance(&mut self, ring: &Ring) {
        for ev in self.seg.feed(ring) {
            match ev {
                SegEvent::Started(seg) => {
                    if let Some(old) = self.cur.take() {
                        if !old.decided && old.seg.loud_frames >= MIN_LOUD_FRAMES {
                            self.prev = Some(old);
                        }
                    }
                    self.next_utt += 1;
                    self.cur = Some(Utt::new(self.next_utt, seg));
                }
                SegEvent::Ended(seg) => {
                    if let Some(u) = self.cur.as_mut() {
                        u.seg = seg;
                        u.ended = true;
                        // A click or a short cough: never worth a check.
                        if seg.loud_frames < MIN_LOUD_FRAMES {
                            u.decided = true;
                        }
                    }
                }
            }
        }
        if let (Some(u), Some(s)) = (self.cur.as_mut(), self.seg.current()) {
            if !u.ended {
                u.seg = s;
            }
        }
    }

    /// The check to send now, if one is due and none is in flight.
    pub fn next_check(&mut self, ring: &Ring) -> Option<IdleCheck> {
        if self.in_flight.is_some() {
            return None;
        }
        let now = ring.end();
        let sliding = self.sliding;
        let mut due: Option<(u64, u64, u64, bool, bool)> = None;
        for u in [self.prev.as_mut(), self.cur.as_mut()].into_iter().flatten() {
            if u.decided || u.seg.loud_frames < MIN_LOUD_FRAMES {
                continue;
            }
            let cap = u.seg.speech_at + WINDOW as u64;
            let start = u.seg.start.max(ring.first());
            if u.ended || now >= cap {
                if u.final_sent {
                    continue;
                }
                let end = (u.seg.last_loud_end + TAIL as u64).min(cap).min(now);
                if end <= start {
                    u.decided = true;
                    continue;
                }
                u.final_sent = true;
                u.checked_to = Some(end);
                due = Some((u.id, start, end, true, false));
                break;
            }
            if sliding && u.empties < MAX_EMPTY_SLIDING {
                let ready = match u.checked_to {
                    None => now >= u.seg.speech_at + FIRST_CHECK as u64,
                    Some(to) => (u.check_now && now > to) || now >= to + STEP as u64,
                };
                let end = now.min(cap);
                if ready && end > start {
                    let first = u.checked_to.is_none();
                    u.checked_to = Some(end);
                    u.check_now = false;
                    due = Some((u.id, start, end, false, first));
                    break;
                }
            }
        }
        let (utt, start, end, final_check, first) = due?;
        self.next_check_id += 1;
        let check = IdleCheck { id: self.next_check_id, utt, start, end, final_check, first };
        self.in_flight = Some(check);
        Some(check)
    }

    /// What the words heard by `check` mean.
    pub fn on_result(&mut self, check: &IdleCheck, text: &str, m: &Matcher, mode: ListenMode) -> IdleDecision {
        if self.in_flight.map(|c| c.id) != Some(check.id) {
            return IdleDecision::Wait;
        }
        self.in_flight = None;
        let now = self.seg.position();
        let quiet_now = self.seg.quiet_run();
        let call = mode == ListenMode::Call;
        let call_wake = if call { self.call_wake } else { None };
        let superseded = self.prev.as_ref().is_some_and(|p| p.id == check.utt);
        let Some(u) = [self.prev.as_mut(), self.cur.as_mut()].into_iter().flatten().find(|u| u.id == check.utt && !u.decided) else {
            return IdleDecision::Wait;
        };
        let seen = Seen {
            // This check's audio held the whole utterance.
            covered: check.final_check || (u.ended && check.end >= u.seg.last_loud_end),
            quiet: if u.ended { usize::MAX } else { quiet_now },
            heard_all: check.end + HEARD_SLACK >= u.seg.last_loud_end,
            prompted: check.first && !m.prompt.is_empty(),
            first: check.first,
            superseded,
            end: check.end,
            after_call_wake: call_wake.is_some_and(|(wake_utt, until)| u.id > wake_utt && u.seg.speech_at <= until),
        };
        let mut verdict = judge(u, text, m, mode, seen);
        if check.final_check && verdict == Verdict::Wait {
            verdict = Verdict::Ignore;
        }
        if verdict != Verdict::Wait {
            u.decided = true;
        }
        let after_ms = (now.saturating_sub(u.seg.last_loud_end) * 1000 / RATE as u64) as u32;
        let (start, end) = (u.seg.start, u.seg.last_loud_end + TAIL as u64);
        // The take starts after all the audio this utterance has had judged: what comes after it
        // (a command said after a pause, or more of the same breath) is the take's to hear.
        let heard_to = check.end.max(u.seg.last_loud_end);
        // A bare wake word on a call: the next utterance may be the command, after a pause.
        let new_call_wake = (call && u.decided && u.bare_wake).then(|| (u.id, u.seg.last_loud_end + CALL_WAKE_GAP));
        if seen.after_call_wake && u.decided {
            self.call_wake = None;
        }
        if new_call_wake.is_some() {
            self.call_wake = new_call_wake;
        }
        match verdict {
            Verdict::Wait => IdleDecision::Wait,
            Verdict::Ignore => IdleDecision::Ignore,
            Verdict::Wake => IdleDecision::Wake { start, heard_to, heard: text.to_string(), after_ms },
            Verdict::Command(action) => IdleDecision::Command { action, start, end, heard: text.to_string(), after_ms },
        }
    }

    /// A decision that needed only more quiet: a check without the prompt heard the whole wake
    /// word, or a whole command, and nothing has been said since. Call once per tick, after
    /// advance. Saves waiting for another check (about 150 to 250 ms) after you stop.
    pub fn poll(&mut self) -> IdleDecision {
        let quiet_now = self.seg.quiet_run();
        let now = self.seg.position();
        for u in [self.prev.as_mut(), self.cur.as_mut()].into_iter().flatten() {
            if u.decided {
                continue;
            }
            let Some((verdict, end, needed, heard)) = u.when_quiet.clone() else { continue };
            // More was said after what the check heard: the next check decides.
            if end + HEARD_SLACK < u.seg.last_loud_end || !(u.ended || quiet_now >= needed) {
                continue;
            }
            u.decided = true;
            let after_ms = (now.saturating_sub(u.seg.last_loud_end) * 1000 / RATE as u64) as u32;
            let (start, utt_end) = (u.seg.start, u.seg.last_loud_end + TAIL as u64);
            return match verdict {
                Verdict::Wake => IdleDecision::Wake { start, heard_to: end.max(u.seg.last_loud_end), heard, after_ms },
                Verdict::Command(action) => IdleDecision::Command { action, start, end: utt_end, heard, after_ms },
                Verdict::Wait | Verdict::Ignore => IdleDecision::Wait,
            };
        }
        IdleDecision::Wait
    }

    /// The engine was missing or the check failed: a final check gives the utterance up.
    pub fn on_failed(&mut self, check: &IdleCheck) {
        if self.in_flight.map(|c| c.id) != Some(check.id) {
            return;
        }
        self.in_flight = None;
        if check.final_check {
            if let Some(u) = [self.prev.as_mut(), self.cur.as_mut()].into_iter().flatten().find(|u| u.id == check.utt) {
                u.decided = true;
            }
        }
    }

    /// Forgets the utterance under way and any check in flight; keeps the noise floor.
    pub fn reset(&mut self) {
        self.seg.reset();
        self.cur = None;
        self.prev = None;
        self.in_flight = None;
        self.call_wake = None;
    }
}

/// The decision table for one result, as tuned by the live measurement. Only a final check, or a check that heard the whole utterance (`covered`), may
/// give an utterance up on words that are not yet a command: a partial window is often cut off
/// mid-word ("Navi, stop taking Navi.") or misheard ("9." for "Navi").
fn judge(u: &mut Utt, text: &str, m: &Matcher, mode: ListenMode, s: Seen) -> Verdict {
    let call = mode == ListenMode::Call;
    let wait_or_ignore = if s.covered { Verdict::Ignore } else { Verdict::Wait };
    // Every result decides afresh: an earlier one that only needed more quiet is forgotten.
    u.when_quiet = None;
    // A whole command: run it once 200 ms of quiet follow. A check that heard it all leaves that
    // to IdleListener::poll, so it runs on time without another check.
    let command_when_final = |u: &mut Utt, action: Action| {
        if s.covered || (s.quiet >= QUIET_FOR_COMMAND && s.heard_all) {
            return Verdict::Command(action);
        }
        if s.heard_all {
            u.when_quiet = Some((Verdict::Command(action), s.end, QUIET_FOR_COMMAND, text.to_string()));
        }
        Verdict::Wait
    };
    if heard_nothing(text, m) {
        u.empties += 1;
        return wait_or_ignore;
    }
    u.empties = 0;
    let Some(hit) = m.match_wake(text) else {
        // On a call, "Navi." [pause] "stop taking notes": the words after a bare wake word.
        if s.after_call_wake {
            if let Some(action) = m.match_command(text, IN_CALL) {
                return command_when_final(u, action);
            }
            if m.is_command_prefix(text, IN_CALL) {
                return wait_or_ignore;
            }
        }
        // A short window may have caught the name badly ("9.", "Hey, novel."): the first one, or
        // one after a window that did hear the wake word, is never the last word on it.
        return if m.could_be_wake_start(text) || (!s.covered && (s.first || u.heard_wake)) { wait_or_ignore } else { Verdict::Ignore };
    };
    u.heard_wake = true;
    let allowed = if call { IN_CALL } else { AFTER_WAKE };
    let command = m.match_command(&hit.rest, allowed);
    // A window that stops mid-speech may have cut its last word: if what it heard may still be the
    // start of a command, the next window decides.
    let prefix = command.is_none() && (m.is_command_prefix(&hit.rest, allowed) || (!s.covered && m.may_start_command(&hit.rest, allowed)));
    if command.is_none() && !prefix && m.talking_about(&hit) {
        // "Navi was" may be "Navi wa[tch]" cut off by the window: only a talk word with more after
        // it, or the whole utterance, settles it.
        return if s.covered || keys_of(&hit.rest).len() > 1 { Verdict::Ignore } else { Verdict::Wait };
    }
    if keys_of(&hit.rest).is_empty() {
        u.name_at.get_or_insert(s.end);
        // Heard with the prompt, a bare name may be Whisper reading the prompt back over a "hmm"
        // or a cough: a check without it must hear the name too. Ask for one straight away.
        if s.prompted {
            u.check_now = true;
            return Verdict::Wait;
        }
        if !hit.weak {
            u.bare_wake = true;
        }
        if call {
            // Nothing starts a take on a call; on_result remembers it for the next utterance.
            return wait_or_ignore;
        }
        // "Navy" may be "navy blue": only the whole utterance, with nothing said after it.
        if hit.weak {
            return if s.covered && !s.superseded { Verdict::Wake } else { wait_or_ignore };
        }
        // Still talking well past the name ("Navi, um..."): start the take now, so nothing is lost.
        let long_past = !u.ended && u.name_at.is_some_and(|at| s.end >= at + LONG_AFTER_NAME);
        if s.covered || (s.quiet >= QUIET_FOR_WAKE && s.heard_all) || long_past {
            return Verdict::Wake;
        }
        // Heard to the end: wake the moment the quiet is long enough, unless more is said first.
        if s.heard_all {
            u.when_quiet = Some((Verdict::Wake, s.end, QUIET_FOR_WAKE, text.to_string()));
        }
        return Verdict::Wait;
    }
    if let Some(action) = command {
        return command_when_final(u, action);
    }
    if m.match_command(&hit.rest, IN_TAKE).is_some() {
        // Nothing to cancel or finish at idle.
        return wait_or_ignore;
    }
    // On a call only a whole command acts; "Navi, ..." is otherwise a job for the call's own
    // transcript (jobs.rs), never a take. The final check decides.
    if call {
        return wait_or_ignore;
    }
    // "Navy blue is my favourite colour", "Navy, black or grey?": a weak spelling needs a command.
    if hit.weak {
        return if prefix { wait_or_ignore } else { Verdict::Ignore };
    }
    if prefix {
        return if s.covered { Verdict::Wake } else { Verdict::Wait };
    }
    // Dictation: start now, while they keep talking.
    Verdict::Wake
}

// ---------------------------------------------------------------------------------------------
// The in-take listener
// ---------------------------------------------------------------------------------------------

/// Positions are take-audio sample indices: 0 is the first sample `audio.snapshot_16k(0)` returns,
/// which is also index 0 of what `audio.stop()` returns (preroll included).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TakeCheck {
    pub id: u64,
    pub start: u64,
    pub end: u64,
    /// The first utterance after the wake word, checked from the start of the take.
    pub first: bool,
    pub speech_at: u64,
    pub utt_end: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TakeAction {
    Cancel { heard: String },
    /// Keep the take's audio before `cut`.
    Finish { cut: u64, heard: String },
    /// A wake take's first command: drop the take, run it.
    Command { action: Action, heard: String },
}

#[derive(Clone, Debug)]
struct Pending {
    action: TakeAction,
    /// The end of the phrase's utterance: speech after this means it was dictation.
    after: u64,
    confirm_at: u64,
}

/// Listens to a take's own audio for the cancel and finish phrases, and (in a take the wake word
/// started) a command said straight after the wake word.
pub struct TakeListener {
    preroll: u64,
    /// Utterances that end by here were the wake word, which the idle listener already heard.
    heard: u64,
    armed: Option<u64>,
    seg: Segmenter,
    /// Still waiting for the first thing said after the wake word.
    window_open: bool,
    queue: VecDeque<TakeCheck>,
    in_flight: Option<TakeCheck>,
    pending: Option<Pending>,
    /// The first loud frame of the latest utterance to start.
    latest_speech_at: Option<u64>,
    next_id: u64,
    done: bool,
    fired_after: u64,
}

impl TakeListener {
    /// `preroll`: samples of preroll at the front of a wake take's audio (Recording.preroll_ms × 16).
    pub fn new(from_wake: bool, preroll: u64) -> Self {
        let preroll = if from_wake { preroll } else { 0 };
        Self {
            preroll,
            heard: preroll,
            armed: None,
            seg: Segmenter::new(),
            window_open: from_wake,
            queue: VecDeque::new(),
            in_flight: None,
            pending: None,
            latest_speech_at: None,
            next_id: 0,
            done: false,
            fired_after: 0,
        }
    }

    /// Start acting on utterances that end after `at` (0 for a wake take; the switch point when a
    /// push-to-talk take turns hands-free).
    pub fn arm(&mut self, at: u64) {
        if self.armed.is_none() {
            self.armed = Some(at);
        }
    }

    /// A wake take: the idle listener judged its audio up to `heard` (IdleDecision::Wake
    /// `heard_to`, as a position in the take's audio). Only utterances that end by then are the
    /// wake word itself. The default, the whole preroll, would also skip a command said while a
    /// slow check came back or a Bluetooth microphone opened.
    pub fn skip_to(&mut self, heard: u64) {
        if self.window_open {
            self.heard = heard;
        }
    }

    /// Listens again after an action it returned was not carried out (a cancel waiting to be
    /// said a second time).
    pub fn resume(&mut self) {
        self.done = false;
        self.pending = None;
    }

    pub fn advance(&mut self, ring: &Ring) {
        for ev in self.seg.feed(ring) {
            match ev {
                SegEvent::Started(s) => self.latest_speech_at = Some(s.speech_at),
                SegEvent::Ended(s) => self.on_ended(s),
            }
        }
        let first_queued = self.queue.iter().any(|c| c.first) || self.in_flight.is_some_and(|c| c.first);
        if self.window_open && !first_queued && ring.end() > self.preroll + (FIRST_WINDOW + MAX_FIRST_UTT) as u64 {
            self.window_open = false;
        }
    }

    fn on_ended(&mut self, s: Seg) {
        let Some(at) = self.armed else { return };
        if self.done || s.last_loud_end < at {
            return;
        }
        let dur = s.last_loud_end.saturating_sub(s.speech_at);
        let end = s.last_loud_end + TAIL as u64;
        if self.window_open {
            // The wake word itself, which the idle listener already heard.
            if s.last_loud_end <= self.heard {
                return;
            }
            if s.speech_at < self.preroll + FIRST_WINDOW as u64 {
                if dur <= MAX_FIRST_UTT as u64 {
                    self.enqueue(0, end, true, s);
                } else {
                    // A long first utterance is dictation.
                    self.window_open = false;
                }
                return;
            }
            self.window_open = false;
        }
        if dur >= MIN_CMD_UTT as u64 && dur <= MAX_CMD_UTT as u64 {
            self.enqueue(s.start, end, false, s);
        }
    }

    fn enqueue(&mut self, start: u64, end: u64, first: bool, s: Seg) {
        if self.queue.len() >= QUEUE_MAX {
            self.queue.pop_front();
        }
        self.next_id += 1;
        self.queue.push_back(TakeCheck { id: self.next_id, start, end, first, speech_at: s.speech_at, utt_end: s.last_loud_end });
    }

    pub fn next_check(&mut self) -> Option<TakeCheck> {
        if self.in_flight.is_some() || self.done || self.armed.is_none() {
            return None;
        }
        let check = self.queue.pop_front()?;
        self.in_flight = Some(check);
        Some(check)
    }

    pub fn on_result(&mut self, check: &TakeCheck, text: &str, m: &Matcher, audio_end: u64) -> Option<TakeAction> {
        if self.in_flight.map(|c| c.id) != Some(check.id) {
            return None;
        }
        self.in_flight = None;
        if self.done {
            return None;
        }
        let nothing = heard_nothing(text, m);
        if check.first {
            if !nothing {
                match m.match_wake(text) {
                    // The name again, or a pause: still waiting for what comes after it.
                    Some(hit) if keys_of(&hit.rest).is_empty() => {}
                    Some(hit) => {
                        self.window_open = false;
                        self.first_command(check, &hit.rest, text, m);
                    }
                    // The audio starts at the wake word the idle listener heard, but Whisper may
                    // leave the name out ("Watch my screen."): it is still the first thing said
                    // after it.
                    None => {
                        self.window_open = false;
                        self.first_command(check, text, text, m);
                    }
                }
            }
        } else if !nothing {
            self.judge_alone(check, text, m);
        }
        self.poll(audio_end)
    }

    /// The first thing said after the wake word (`rest`): a command, or cancel. Finish counts as
    /// cancel too, as nothing has been dictated yet.
    fn first_command(&mut self, check: &TakeCheck, rest: &str, text: &str, m: &Matcher) {
        if let Some(action) = m.match_command(rest, AFTER_WAKE) {
            self.set_pending(TakeAction::Command { action, heard: text.to_string() }, check);
        } else if m.match_exact(rest, IN_TAKE).is_some() {
            self.set_pending(TakeAction::Cancel { heard: text.to_string() }, check);
        }
    }

    fn judge_alone(&mut self, check: &TakeCheck, text: &str, m: &Matcher) {
        match m.match_in_take(text) {
            Some(Action::Cancel) => self.set_pending(TakeAction::Cancel { heard: text.to_string() }, check),
            Some(Action::Finish) => self.set_pending(TakeAction::Finish { cut: check.speech_at.saturating_sub(CUT_MARGIN as u64), heard: text.to_string() }, check),
            _ => {}
        }
    }

    fn set_pending(&mut self, action: TakeAction, check: &TakeCheck) {
        self.pending = Some(Pending { action, after: check.utt_end, confirm_at: check.utt_end + CONFIRM_QUIET as u64 });
    }

    pub fn on_failed(&mut self, check: &TakeCheck) {
        if self.in_flight.map(|c| c.id) == Some(check.id) {
            self.in_flight = None;
        }
    }

    /// Fires a matched command once CONFIRM_QUIET has passed, or drops it if speech resumed.
    pub fn poll(&mut self, audio_end: u64) -> Option<TakeAction> {
        let p = self.pending.as_ref()?;
        // Speech after the phrase: a new utterance, or two loud frames that may be one starting
        // (a Started event takes three, which could land just after confirm_at).
        if self.latest_speech_at.is_some_and(|s| s > p.after) || self.seg.loud_frames_after(p.after) >= 2 {
            log::info!("voice: a command phrase was followed by more speech, so it was dictation");
            self.pending = None;
            return None;
        }
        if audio_end < p.confirm_at {
            return None;
        }
        let p = self.pending.take()?;
        self.done = true;
        self.fired_after = audio_end.saturating_sub(p.after);
        Some(p.action)
    }

    /// How long after the end of the phrase the last action fired, for the log.
    pub fn fired_after_ms(&self) -> u32 {
        (self.fired_after * 1000 / RATE as u64) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::VoiceCommandEntry;

    fn vc_with_wake(wake: &[&str]) -> VoiceCommandSettings {
        let mut vc = VoiceCommandSettings::default();
        for e in vc.commands.iter_mut() {
            if e.action == "wake" {
                e.phrases = wake.iter().map(|s| s.to_string()).collect();
            }
        }
        vc
    }

    fn m() -> Matcher {
        Matcher::new(&VoiceCommandSettings::default())
    }

    fn utt() -> Utt {
        Utt::new(1, Seg { start: 0, speech_at: 0, last_loud_end: RATE as u64, loud_frames: 40 })
    }

    /// The decision for one ended utterance heard in full.
    fn ended(m: &Matcher, text: &str, mode: ListenMode) -> Verdict {
        judge(&mut utt(), text, m, mode, Seen::ended())
    }

    /// The decision for a sliding check, unprompted, while the utterance goes on.
    fn sliding(m: &Matcher, text: &str, mode: ListenMode, quiet: usize, heard_all: bool) -> Verdict {
        judge(&mut utt(), text, m, mode, Seen::sliding(quiet, heard_all))
    }

    // ---- matching ----

    #[test]
    fn wake_spellings() {
        let m = m();
        for t in ["Navi", "Navi.", "navi", "Navie,", "Naavi", "Nahvee", "Hey Navi", "Hey, Navi.", "Hi Navi", "Hay Navi", "OK Navi", "Okay, Navi", "NAVI"] {
            assert!(m.match_wake(t).is_some(), "{t}");
        }
        for t in ["Nav", "Navigate", "Navigation", "Naval", "Heaven", "Hey there", ""] {
            assert!(m.match_wake(t).is_none(), "{t}");
        }
        assert!(m.match_wake("Navy").is_some_and(|h| h.weak), "navy is a weak hit");
        assert!(m.match_wake("Navi, watch").is_some_and(|h| h.paused && h.rest == "watch"));
        assert!(m.match_wake("Navi watch").is_some_and(|h| !h.paused && h.single_name));
        assert!(m.match_wake("OK Navi").is_some_and(|h| !h.single_name), "a greeting makes it an address");
    }

    #[test]
    fn one_breath_commands() {
        let m = m();
        for (t, a) in [
            ("Navi watch my screen", Action::WatchOn),
            ("Hey Navi, take notes.", Action::NotesOn),
            ("Navi stop taking notes", Action::NotesOff),
            ("Navi, stop watching", Action::WatchOff),
            ("Navi, scratch that", Action::ScratchThat),
            ("Navi, please take notes now", Action::NotesOn),
            ("Navi can you stop watching please", Action::WatchOff),
            ("Navi. Navi, take notes.", Action::NotesOn),
        ] {
            assert_eq!(ended(&m, t, ListenMode::Normal), Verdict::Command(a), "{t}");
        }
    }

    #[test]
    fn wake_and_dictation() {
        let m = m();
        for t in ["Navi, email Sam about Friday", "Navi email Sam", "Navi, take the bins out", "Navi", "Hey Navi."] {
            assert_eq!(ended(&m, t, ListenMode::Normal), Verdict::Wake, "{t}");
        }
    }

    #[test]
    fn talk_about_navi_is_ignored() {
        let m = m();
        for t in [
            "Navi is our new app",
            "Navy blue is my favourite colour",
            "Navi was great",
            "Navigate to the settings page",
            "I told Navi to take notes",
            "Take notes on the meeting",
            "Watch my screen please",
        ] {
            assert_eq!(ended(&m, t, ListenMode::Normal), Verdict::Ignore, "{t}");
        }
        assert_eq!(ended(&m, "Navy.", ListenMode::Normal), Verdict::Wake);
        assert_eq!(ended(&m, "Navy, watch my screen", ListenMode::Normal), Verdict::Command(Action::WatchOn));
        assert_eq!(ended(&m, "Navi, is it raining?", ListenMode::Normal), Verdict::Wake);
        assert_eq!(ended(&m, "Navi can you take notes", ListenMode::Normal), Verdict::Command(Action::NotesOn));
        // Talk about Navi at the start of a sentence, as its maker does all day.
        for t in [
            "Navi can't hear me",
            "Navi will do that",
            "Navi should be faster",
            "Navi just crashed",
            "Navi also writes notes",
            "Navi needs a key",
            "Navi types what you say",
            "Navi could be better",
            "Navi never wakes up",
        ] {
            assert_eq!(ended(&m, t, ListenMode::Normal), Verdict::Ignore, "{t}");
        }
        // A command, or its start, is never talk.
        assert_eq!(ended(&m, "Navi just take notes", ListenMode::Normal), Verdict::Command(Action::NotesOn));
        assert_eq!(ended(&m, "Navi would you stop watching", ListenMode::Normal), Verdict::Command(Action::WatchOff));
        assert_eq!(sliding(&m, "Navi just", ListenMode::Normal, 0, true), Verdict::Wait);
        assert_eq!(sliding(&m, "Navi can", ListenMode::Normal, 0, true), Verdict::Wait);
        // "Navy" is a colour as often as it is the name.
        assert_eq!(ended(&m, "Navy, black or grey?", ListenMode::Normal), Verdict::Ignore);
        assert_eq!(judge(&mut utt(), "Navy.", &m, ListenMode::Normal, Seen { superseded: true, ..Seen::ended() }), Verdict::Ignore, "more was said straight after it");
        assert_eq!(sliding(&m, "Navy.", ListenMode::Normal, QUIET_FOR_WAKE, true), Verdict::Wait, "only the whole utterance");
    }

    #[test]
    fn waits_while_talking() {
        let m = m();
        let n = ListenMode::Normal;
        // "Navi", still talking: wait, and only well past the name ("Navi, um...") start the take.
        let mut u = utt();
        assert_eq!(judge(&mut u, "Navi", &m, n, Seen { end: RATE as u64 / 2, ..Seen::sliding(0, true) }), Verdict::Wait);
        assert_eq!(judge(&mut u, "Navi", &m, n, Seen { end: RATE as u64, ..Seen::sliding(0, true) }), Verdict::Wait, "a comma's pause is not enough");
        assert_eq!(judge(&mut u, "Navi", &m, n, Seen { end: RATE as u64 * 3 / 2, ..Seen::sliding(0, true) }), Verdict::Wake);
        // A weak spelling never wakes while still talking ("navy blue").
        let mut u = utt();
        for end in [RATE as u64 / 2, RATE as u64 * 2] {
            assert_eq!(judge(&mut u, "Navy", &m, n, Seen { end, ..Seen::sliding(0, true) }), Verdict::Wait);
        }
        // 300 ms of quiet after the name makes it final, if the check heard up to the end; 160 ms
        // (a comma) does not, so "Navi, [pause] watch my screen" runs with no take.
        assert_eq!(sliding(&m, "Navi", n, QUIET_FOR_WAKE, true), Verdict::Wake);
        assert_eq!(sliding(&m, "Navi.", n, 8, true), Verdict::Wait);
        assert_eq!(sliding(&m, "Navi", n, QUIET_FOR_WAKE, false), Verdict::Wait);
        // Part of a command: wait; at the end it is dictation.
        assert_eq!(sliding(&m, "Navi, watch", n, 0, true), Verdict::Wait);
        assert_eq!(ended(&m, "Navi, watch", n), Verdict::Wake);
        assert_eq!(sliding(&m, "Navi, wat", n, 0, true), Verdict::Wait);
        assert_eq!(sliding(&m, "Navi, can you", n, 0, true), Verdict::Wait, "padding so far");
        // A whole command needs 200 ms of quiet (or the end) before it runs.
        assert_eq!(sliding(&m, "Navi, watch my screen", n, 5, true), Verdict::Wait);
        assert_eq!(sliding(&m, "Navi, watch my screen", n, 10, true), Verdict::Command(Action::WatchOn));
        assert_eq!(sliding(&m, "Navi, take notes", n, 10, false), Verdict::Wait, "words may follow that the check didn't hear");
        // A name cut off mid-word is not the end of it.
        assert_eq!(sliding(&m, "Nav", n, 0, true), Verdict::Wait);
        assert_eq!(ended(&m, "Nav", n), Verdict::Ignore);
        assert_eq!(sliding(&m, "Hey", n, 0, true), Verdict::Wait);
        assert_eq!(sliding(&m, "Now", n, 0, true), Verdict::Wait, "may be the name cut off");
        assert_eq!(ended(&m, "Now.", n), Verdict::Ignore);
        assert_eq!(sliding(&m, "Have a", n, 0, true), Verdict::Ignore, "ordinary speech is let go at once");
    }

    #[test]
    fn the_first_window_never_gives_an_utterance_up() {
        // About 0.6 s of audio: Whisper may write "9." or "Hey, no." for the name. The next decides.
        let m = m();
        let n = ListenMode::Normal;
        let first = Seen { first: true, prompted: true, ..Seen::sliding(0, true) };
        for heard in ["9.", "Hey, no.", "Naviwa", "paying off.", "Have a"] {
            let mut u = utt();
            assert_eq!(judge(&mut u, heard, &m, n, first), Verdict::Wait, "{heard}");
            assert_eq!(judge(&mut u, "Navi watch my screen", &m, n, Seen::ended()), Verdict::Command(Action::WatchOn), "{heard}");
        }
        // Covered (a short utterance that ended before its answer came back): nothing more to hear.
        assert_eq!(judge(&mut utt(), "9.", &m, n, Seen { first: true, ..Seen::ended() }), Verdict::Ignore);
        // Once a window has heard the wake word, a later short one that mishears it waits too
        // (live clip 05: "Hey Navi." then "Hey, novel.").
        let mut u = utt();
        assert_eq!(judge(&mut u, "Hey Navi.", &m, n, first), Verdict::Wait);
        assert_eq!(judge(&mut u, "Hey, novel.", &m, n, Seen::sliding(0, true)), Verdict::Wait);
        assert_eq!(judge(&mut u, "Hey Navi, take notes.", &m, n, Seen::ended()), Verdict::Command(Action::NotesOn));
        // A talk word cut off by the window ("Navi was" for "Navi wa[tch]", live clip 04) waits;
        // with more words after it, it is talk.
        let mut u = utt();
        assert_eq!(judge(&mut u, "Navi was", &m, n, Seen::sliding(0, true)), Verdict::Wait);
        assert_eq!(judge(&mut u, "Navi watch my screen", &m, n, Seen::ended()), Verdict::Command(Action::WatchOn));
        assert_eq!(sliding(&m, "Navi was great", n, 0, true), Verdict::Ignore);
        assert_eq!(ended(&m, "Navi was", n), Verdict::Ignore);
        // The last word cut off by the window (live clip 07: "Navi, stop walking." for "stop
        // watch[ing]"): the start of a command, so the next window decides.
        assert_eq!(sliding(&m, "Navi, stop walking.", n, 0, true), Verdict::Wait);
        assert_eq!(sliding(&m, "Navi, scratching", n, 0, true), Verdict::Wait);
        assert_eq!(sliding(&m, "Navi, take the", n, 0, true), Verdict::Wait);
        assert_eq!(ended(&m, "Navi, stop walking.", n), Verdict::Wake, "the whole of it: dictation");
        // Dictation that doesn't start like a command still starts the take at once.
        assert_eq!(sliding(&m, "Navi, email Sam about", n, 0, true), Verdict::Wake);
        assert_eq!(sliding(&m, "Navi, take the bins", n, 0, true), Verdict::Wake);
        assert_eq!(sliding(&m, "Navi, stopwatching.", n, 0, true), Verdict::Wait);
        assert_eq!(sliding(&m, "Navi, stopped by the shop", n, 0, true), Verdict::Wake);
        assert_eq!(without_last_word("stop walking."), "stop ");
        assert_eq!(without_last_word("do not worry"), "do not ");
        assert_eq!(without_last_word("watch"), "");
    }

    #[test]
    fn a_bare_name_heard_with_the_prompt_needs_a_check_without_it() {
        // "Hmm" came back as "Hey Navi." with the prompt, and as "home." without it (live probe).
        let m = m();
        let n = ListenMode::Normal;
        let prompted = Seen { first: true, prompted: true, ..Seen::ended() };
        let mut u = utt();
        assert_eq!(judge(&mut u, "Hey Navi.", &m, n, prompted), Verdict::Wait, "even when it heard the whole sound");
        assert!(u.check_now, "the second opinion is asked for at once");
        assert_eq!(judge(&mut u, "Home.", &m, n, Seen::ended()), Verdict::Ignore);
        // The real name is heard without the prompt too.
        let mut u = utt();
        assert_eq!(judge(&mut u, "Navi.", &m, n, prompted), Verdict::Wait);
        assert_eq!(judge(&mut u, "Na'vi.", &m, n, Seen::ended()), Verdict::Wake);
        // A command needs no second opinion: the prompt holds no command words.
        assert_eq!(judge(&mut utt(), "Navi, take notes.", &m, n, prompted), Verdict::Command(Action::NotesOn));
        // The whole prompt read back counts as nothing heard.
        assert!(m.is_echo("Navi, Hey Navi."));
        assert!(!m.is_echo("Hey Navi.") && !m.is_echo("Navi."));
        let one = Matcher::new(&vc_with_wake(&["Navi"]));
        assert!(!one.is_echo("Navi."), "with one wake phrase the prompt is the phrase itself");
    }

    #[test]
    fn a_late_answer_after_the_end_leaves_it_to_the_final_check() {
        // The utterance ended while a sliding check that heard only "Navi" was out: "Navi, stop
        // taking notes" must be decided by the final check, not woken on the stale "Navi".
        let m = m();
        let mut u = utt();
        u.ended = true;
        u.name_at = Some(0);
        let stale = Seen { covered: false, quiet: usize::MAX, heard_all: false, end: RATE as u64 / 2, ..Seen::ended() };
        assert_eq!(judge(&mut u, "Navi.", &m, ListenMode::Normal, stale), Verdict::Wait);
        assert_eq!(judge(&mut u, "Navi, stop taking notes.", &m, ListenMode::Normal, Seen::ended()), Verdict::Command(Action::NotesOff));
    }

    #[test]
    fn custom_wake_words() {
        let hey = Matcher::new(&vc_with_wake(&["Hey"]));
        assert_eq!(ended(&hey, "Hey, take notes.", ListenMode::Normal), Verdict::Command(Action::NotesOn));
        assert_eq!(ended(&hey, "Hey Sam, how are you", ListenMode::Normal), Verdict::Wake);
        assert_eq!(ended(&hey, "Hello", ListenMode::Normal), Verdict::Ignore);
        let computer = Matcher::new(&vc_with_wake(&["Computer"]));
        assert_eq!(ended(&computer, "Computer, watch my screen", ListenMode::Normal), Verdict::Command(Action::WatchOn));
        assert_eq!(ended(&computer, "Computers are", ListenMode::Normal), Verdict::Ignore);
        assert_eq!(ended(&computer, "Navi", ListenMode::Normal), Verdict::Ignore, "only the words in Settings wake it");
        // Hey Navi only: the bare name doesn't wake it.
        let hey_navi = Matcher::new(&vc_with_wake(&["Hey Navi"]));
        assert_eq!(ended(&hey_navi, "Navi, email Sam", ListenMode::Normal), Verdict::Ignore);
        assert_eq!(ended(&hey_navi, "Hey Navi, email Sam", ListenMode::Normal), Verdict::Wake);
        assert_eq!(ended(&hey_navi, "Hi Navi, email Sam", ListenMode::Normal), Verdict::Wake, "greetings swap inside a longer phrase");
        // A one-word wake word is matched exactly: no near spellings, no other greetings.
        assert_eq!(ended(&hey, "Hi Sam, how are you", ListenMode::Normal), Verdict::Ignore);
        assert_eq!(ended(&hey, "Hiya, take notes.", ListenMode::Normal), Verdict::Ignore);
        let alexa = Matcher::new(&vc_with_wake(&["Alexa"]));
        assert_eq!(ended(&alexa, "Alex, can you send that over?", ListenMode::Normal), Verdict::Ignore);
        assert_eq!(ended(&alexa, "Alexa, take notes", ListenMode::Normal), Verdict::Command(Action::NotesOn));
        for t in ["Compiler errors again", "Commuter trains", "Compute the total", "Computed it", "Composer notes"] {
            assert_eq!(ended(&computer, t, ListenMode::Normal), Verdict::Ignore, "{t}");
        }
        assert!(!computer.could_be_wake_start("Commuter"));
    }

    #[test]
    fn custom_commands() {
        let mut vc = VoiceCommandSettings::default();
        vc.commands.retain(|e| e.action != "watch_on");
        vc.commands.push(VoiceCommandEntry { action: "watch_on".into(), phrases: vec!["keep an eye on things".into()] });
        let m = Matcher::new(&vc);
        assert_eq!(ended(&m, "Navi, keep an eye on things.", ListenMode::Normal), Verdict::Command(Action::WatchOn));
        assert_eq!(ended(&m, "Navi, watch my screen", ListenMode::Normal), Verdict::Wake, "the old phrase is dictation now");
        // Whisper's spelling slips are forgiven on longer words.
        assert_eq!(m.match_command("stop takin notes", AFTER_WAKE), Some(Action::NotesOff));
        assert_eq!(m.match_command("dont wory", IN_TAKE), Some(Action::Cancel));
    }

    #[test]
    fn turn_gold() {
        // The launch film says "Navi, turn gold". Every way to say it gives the colour heard.
        let m = m();
        let n = ListenMode::Normal;
        for (t, id) in [
            ("Navi, turn gold", "gold"),
            ("Navi, turn gold.", "gold"),
            ("Navi turn gold", "gold"),
            ("Hey Navi, change to teal.", "teal"),
            ("Navi, switch to rose", "rose"),
            ("Navi, make it sky.", "sky"),
            ("Navi, turn graphite please", "graphite"),
            ("Navi, turn lilac", "lilac"),
            ("Navi, make it yellow", "gold"),
            ("Navi, can you turn pink", "rose"),
            ("Navi. Navi, turn gold.", "gold"),
        ] {
            assert_eq!(ended(&m, t, n), Verdict::Command(Action::Colour(id)), "{t}");
        }
        // Heard while still talking, "Navi, turn" may yet be "Navi, turn gold"; the colour runs once
        // 200 ms of quiet follow it.
        assert_eq!(sliding(&m, "Navi, turn", n, 0, true), Verdict::Wait);
        assert_eq!(sliding(&m, "Navi, turn go", n, 0, true), Verdict::Wait);
        assert_eq!(sliding(&m, "Navi, turn gold", n, 5, true), Verdict::Wait);
        assert_eq!(sliding(&m, "Navi, turn gold", n, QUIET_FOR_COMMAND, true), Verdict::Command(Action::Colour("gold")));
        // Said straight after the wake word, never alone in a take or on a call.
        assert_eq!(m.match_command("Turn gold.", AFTER_WAKE), Some(Action::Colour("gold")));
        let gold = Action::Colour("gold");
        assert!(gold.after_wake() && !gold.in_take() && !gold.in_call());
        assert_eq!(gold.id(), "colour");
        assert_eq!(Action::from_id("colour"), Some(ANY_COLOUR));
        // Phrases from Settings: a colour after each.
        let mut vc = VoiceCommandSettings::default();
        vc.set_phrases("colour", &["go"]);
        let go = Matcher::new(&vc);
        assert_eq!(ended(&go, "Navi, go gold", n), Verdict::Command(Action::Colour("gold")));
        assert_eq!(ended(&go, "Navi, turn gold", n), Verdict::Wake, "the old phrase is dictation now");
        vc.set_phrases("colour", &[]);
        assert_eq!(ended(&Matcher::new(&vc), "Navi, turn gold", n), Verdict::Wake, "turned off");
    }

    #[test]
    fn an_unknown_colour_changes_nothing() {
        // Not a colour Navi has: the words are dictation, as anything else after the wake word.
        let m = m();
        let n = ListenMode::Normal;
        for t in ["Navi, turn orange", "Navi, make it brown", "Navi, switch to purple rain", "Navi, turn"] {
            assert_eq!(ended(&m, t, n), Verdict::Wake, "{t}");
            assert_eq!(m.match_command(&m.strip_wake(t).unwrap(), AFTER_WAKE), None, "{t}");
        }
        // A colour's name is heard exactly: "make it Greek" is never green.
        assert_eq!(m.match_command("make it greek", AFTER_WAKE), None);
        assert_eq!(m.match_command("make it graphic", AFTER_WAKE), None);
        assert_eq!(ended(&m, "Navi, make it shorter", n), Verdict::Wake);
        // A colour saved by hand that Navi doesn't have shows as Lilac.
        assert_eq!(colours::find("orange").id, "lilac");
    }

    #[test]
    fn dictation_with_gold_in_it_never_changes_the_colour() {
        let m = m();
        for t in [
            "Turn gold.",
            "The gold price is up again",
            "We should turn gold into cash",
            "Navi, the gold price is up again",
            "Navi, turn gold into cash",
            "Navi turns gold in the film",
            "Navi, gold",
            "Gold",
        ] {
            assert!(!matches!(ended(&m, t, ListenMode::Normal), Verdict::Command(_)), "{t}");
        }
        // Inside a take only cancel and finish count, and on a call only the call's commands.
        assert_eq!(m.match_in_take("Turn gold."), None);
        assert_eq!(m.match_in_take("Navi, turn gold."), None);
        assert_eq!(ended(&m, "Navi, turn gold", ListenMode::Call), Verdict::Ignore);
        // Not mistaken for Whisper reading out a list of commands.
        assert!(!m.is_echo("Navi, turn gold.") && !m.is_echo("Navi, switch to teal."));
    }

    #[test]
    fn call_mode() {
        let m = m();
        assert_eq!(ended(&m, "Navi, stop taking notes", ListenMode::Call), Verdict::Command(Action::NotesOff));
        assert_eq!(ended(&m, "Navi, watch my screen", ListenMode::Call), Verdict::Command(Action::WatchOn));
        assert_eq!(ended(&m, "Navi", ListenMode::Call), Verdict::Ignore);
        assert_eq!(ended(&m, "Navi, draft the follow-up email", ListenMode::Call), Verdict::Ignore);
        assert_eq!(ended(&m, "Navi, take notes", ListenMode::Call), Verdict::Ignore);
        assert_eq!(sliding(&m, "Navi", ListenMode::Call, QUIET_FOR_WAKE, true), Verdict::Wait);
        // A window cut off mid-word is never the end of it: the final check decides (live clips 19, 39).
        for t in ["Navi, stop taking Navi.", "Navi, Sky.", "Na'vi, stopwatching.", "Navi, stop what", "Navi, draft the"] {
            assert_eq!(sliding(&m, t, ListenMode::Call, 0, true), Verdict::Wait, "{t}");
            assert_eq!(ended(&m, t, ListenMode::Call), Verdict::Ignore, "{t}");
        }
        // Straight after a bare wake word on a call, the next utterance needs no name.
        let after = Seen { after_call_wake: true, ..Seen::ended() };
        assert_eq!(judge(&mut utt(), "Stop taking notes.", &m, ListenMode::Call, after), Verdict::Command(Action::NotesOff));
        assert_eq!(judge(&mut utt(), "Stop watching.", &m, ListenMode::Call, after), Verdict::Command(Action::WatchOff));
        assert_eq!(judge(&mut utt(), "stop taking", &m, ListenMode::Call, Seen { after_call_wake: true, ..Seen::sliding(0, true) }), Verdict::Wait);
        assert_eq!(judge(&mut utt(), "Take notes.", &m, ListenMode::Call, after), Verdict::Ignore, "only the call's commands");
        assert_eq!(judge(&mut utt(), "Draft the email to Sam.", &m, ListenMode::Call, after), Verdict::Ignore);
        assert_eq!(ended(&m, "Stop taking notes.", ListenMode::Call), Verdict::Ignore, "without the wake word just before, nothing");
    }

    #[test]
    fn noise_and_echo() {
        let m = m();
        for t in ["Thank you.", "[BLANK_AUDIO]", "", " ... "] {
            assert!(heard_nothing(t, &m), "{t}");
            assert_eq!(ended(&m, t, ListenMode::Normal), Verdict::Ignore, "{t}");
        }
        assert!(m.is_echo("Navi, watch my screen. Navi, stop watching."));
        assert_eq!(ended(&m, "Navi, watch my screen. Navi, stop watching.", ListenMode::Normal), Verdict::Ignore);
        assert!(!m.is_echo("Navi, watch my screen."));
        let long = Matcher::new(&vc_with_wake(&["Navi", "Hey Navi", "OK Navi", "Hi Navi"]));
        assert!(long.is_echo("Navi, Hey Navi, OK Navi, Hi Navi."), "the prompt read back");
        // Empty results stop sliding checks after two in a row.
        let mut u = utt();
        assert_eq!(judge(&mut u, "", &m, ListenMode::Normal, Seen::sliding(0, true)), Verdict::Wait);
        assert_eq!(u.empties, 1);
    }

    #[test]
    fn in_take_phrases() {
        let m = m();
        for t in ["Don't worry.", "dont worry", "Do not worry!", "Navi, don't worry", "Don\u{2019}t worry.", "Don't worry, Navi.", "Don't wory."] {
            assert_eq!(m.match_in_take(t), Some(Action::Cancel), "{t}");
        }
        for t in ["That's all.", "that is all", "That\u{2019}s all!"] {
            assert_eq!(m.match_in_take(t), Some(Action::Finish), "{t}");
        }
        // Word for word: a short reply in a chat is still typed.
        for t in [
            "don't worry about the deadline",
            "that's all for now",
            "thank you",
            "Navi",
            "worry",
            "Okay, don't worry.",
            "Don't worry, thanks.",
            "So don't worry.",
            "And don't worry.",
            "Please don't worry.",
            "Don't worry, thank you.",
            "So that's all.",
            "And that's all.",
            "That's all, thanks.",
        ] {
            assert_eq!(m.match_in_take(t), None, "{t}");
        }
        let mut vc = VoiceCommandSettings::default();
        vc.commands.retain(|e| e.action != "cancel");
        vc.commands.push(VoiceCommandEntry { action: "cancel".into(), phrases: vec!["cancel".into()] });
        let one_word = Matcher::new(&vc);
        assert_eq!(one_word.match_in_take("Cancel."), Some(Action::Cancel));
        assert_eq!(one_word.match_in_take("Please cancel."), None);
        assert_eq!(one_word.match_in_take("Cancel, thanks."), None);
    }

    #[test]
    fn strip_wake_and_trailing() {
        let m = m();
        assert_eq!(m.strip_wake("Hey Navi, email Sam").as_deref(), Some("email Sam"));
        assert_eq!(m.strip_wake("Navi. Navi, email Sam").as_deref(), Some("email Sam"));
        assert_eq!(m.strip_wake("Navy, email Sam").as_deref(), Some("email Sam"), "weak hits allowed");
        assert_eq!(m.strip_wake("Hello there"), None);
        let hey = Matcher::new(&vc_with_wake(&["Hey"]));
        assert_eq!(hey.strip_wake("Hey, email Sam").as_deref(), Some("email Sam"));
        assert_eq!(m.strip_trailing("See you Friday. That's all.", Action::Finish), "See you Friday.");
        assert_eq!(m.strip_trailing("See you Friday! That is all", Action::Finish), "See you Friday!");
        assert_eq!(m.strip_trailing("That's all I wanted", Action::Finish), "That's all I wanted");
        assert_eq!(m.strip_trailing("That's all.", Action::Finish), "");
        assert_eq!(m.strip_trailing("It costs 3.5 pounds. Navi, that's all.", Action::Finish), "It costs 3.5 pounds.");
    }

    #[test]
    fn prompt() {
        assert_eq!(m().prompt(), if USE_PROMPT { "Navi, Hey Navi." } else { "" });
        assert_eq!(build_prompt(&["Navi".into(), "Hey Navi".into()], true), "Navi, Hey Navi.");
        assert_eq!(build_prompt(&["Navi".into()], false), "");
        assert_eq!(build_prompt(&["x".repeat(300)], true).chars().count(), PROMPT_MAX_CHARS);
    }

    #[test]
    fn tokens_fold_and_find_pauses() {
        let keys = |t: &str| tokens(t).into_iter().map(|t| t.key).collect::<Vec<_>>();
        assert_eq!(keys("Do not worry. That is all"), ["dont", "worry", "thats", "all"]);
        assert_eq!(keys("Do. Not"), ["do", "not"], "a pause keeps them apart");
        assert_eq!(keys("Hey-Navi"), ["hey", "navi"]);
        let t = tokens("Navi - watch, Navy-blue");
        assert!(t[0].pause_after && t[1].pause_after && !t[2].pause_after);
        assert_eq!(tok_eq("navi", "nav"), None);
        assert_eq!(tok_eq("navi", "navy"), Some(true));
        assert_eq!(tok_eq("watching", "watchin"), Some(false));
        assert_eq!(tok_eq("watch", "watches"), None);
        assert_eq!(tok_eq("notes", "note"), Some(false));
        assert_eq!(tok_eq("take", "make"), None, "short words must be exact");
        assert!(Action::from_id("notes_off").is_some_and(|a| a.in_call() && a.after_wake() && !a.in_take()));
        assert_eq!(ALL_ACTIONS.iter().map(|a| a.id()).collect::<Vec<_>>(), crate::types::VOICE_ACTIONS);
    }

    // ---- the segmenter and the idle listener, on synthetic audio ----

    fn tone(secs: f32, amp: f32) -> Vec<f32> {
        (0..(secs * RATE as f32) as usize).map(|i| (i as f32 * 0.07).sin() * amp).collect()
    }

    fn quiet(secs: f32) -> Vec<f32> {
        tone(secs, 0.0005)
    }

    fn segment_all(audio: &[f32]) -> Vec<SegEvent> {
        let (mut ring, mut s) = (Ring::new(), Segmenter::new());
        let mut out = Vec::new();
        for chunk in audio.chunks(RATE / 20) {
            ring.push(chunk);
            out.extend(s.feed(&ring));
        }
        out
    }

    #[test]
    fn a_short_phrase_ends_after_300_ms_of_quiet_with_its_lead_in() {
        let mut audio = quiet(1.0);
        audio.extend(tone(0.8, 0.2));
        audio.extend(quiet(1.0));
        let events = segment_all(&audio);
        assert_eq!(events.len(), 2, "{events:?}");
        let SegEvent::Ended(seg) = events[1] else { panic!("{events:?}") };
        let speech_at = RATE as u64;
        assert!(seg.speech_at >= speech_at && seg.speech_at <= speech_at + FRAME as u64, "{seg:?}");
        assert_eq!(seg.start, seg.speech_at - LEAD_IN as u64);
        let speech_end = speech_at + (RATE * 4 / 5) as u64;
        assert!(seg.last_loud_end.abs_diff(speech_end) <= FRAME as u64, "{seg:?}");
    }

    #[test]
    fn a_long_utterance_does_not_end() {
        let mut audio = quiet(0.5);
        audio.extend(tone(4.0, 0.2));
        let events = segment_all(&audio);
        assert!(matches!(events.as_slice(), [SegEvent::Started(_)]), "{events:?}");
    }

    #[test]
    fn a_click_never_starts_an_utterance() {
        let mut audio = quiet(2.0);
        audio.extend(tone(0.03, 0.5));
        audio.extend(quiet(2.0));
        assert!(segment_all(&audio).is_empty());
    }

    #[test]
    fn after_a_reset_speech_already_under_way_is_not_a_new_start() {
        let (mut ring, mut s) = (Ring::new(), Segmenter::new());
        ring.push(&quiet(1.0));
        s.feed(&ring);
        s.reset();
        ring.push(&tone(0.5, 0.2));
        assert!(s.feed(&ring).is_empty(), "mid-sentence");
        ring.push(&quiet(0.4));
        ring.push(&tone(0.5, 0.2));
        assert!(matches!(s.feed(&ring).as_slice(), [SegEvent::Started(_)]), "after a pause it is a start");
    }

    #[test]
    fn the_ring_keeps_ten_seconds_and_finds_old_audio() {
        let mut ring = Ring::new();
        ring.push(&vec![0.1; RATE * 12]);
        assert_eq!(ring.end() - ring.first(), (RATE * RING_SECS) as u64);
        assert_eq!(ring.first(), (RATE * 2) as u64);
        assert_eq!(ring.since(0).len(), RATE * RING_SECS, "asking for audio that has gone gives what is left");
        assert_eq!(ring.range((RATE * 11) as u64, RATE * 5).len(), RATE);
        assert_eq!(ring.slice((RATE * 3) as u64, (RATE * 4) as u64).len(), RATE);
        assert!(ring.slice((RATE * 4) as u64, (RATE * 3) as u64).is_empty());
        ring.clear();
        assert!(ring.since(0).is_empty());
        assert_eq!(ring.end(), (RATE * 12) as u64);
    }

    /// Feeds `audio` 40 ms at a time and collects every check, answering none (they stay in
    /// flight until `answer` says otherwise).
    struct Sim {
        ring: Ring,
        idle: IdleListener,
        checks: Vec<(u64, IdleCheck)>,
        mode: ListenMode,
    }

    impl Sim {
        fn new(sliding: bool) -> Self {
            Self { ring: Ring::new(), idle: IdleListener::new(sliding), checks: Vec::new(), mode: ListenMode::Normal }
        }

        /// Plays `audio`; every check due is recorded with the position it was sent at and then
        /// answered with `reply(check)` straight away (None: left in flight).
        fn play(&mut self, audio: &[f32], m: &Matcher, mut reply: impl FnMut(&IdleCheck) -> Option<String>) -> Vec<IdleDecision> {
            let mut decisions = Vec::new();
            for chunk in audio.chunks(RATE / 25) {
                self.ring.push(chunk);
                self.idle.advance(&self.ring);
                let d = self.idle.poll();
                if d != IdleDecision::Wait {
                    decisions.push(d);
                }
                if let Some(c) = self.idle.next_check(&self.ring) {
                    self.checks.push((self.ring.end(), c));
                    if let Some(text) = reply(&c) {
                        let d = self.idle.on_result(&c, &text, m, self.mode);
                        if d != IdleDecision::Wait {
                            decisions.push(d);
                        }
                    }
                }
            }
            decisions
        }
    }

    #[test]
    fn idle_scheduling() {
        let m = m();
        let lead = RATE as u64;
        // A 2 s utterance, answered with nothing useful: sliding checks then a final one.
        let mut sim = Sim::new(true);
        let mut audio = quiet(1.0);
        audio.extend(tone(2.0, 0.2));
        audio.extend(quiet(1.0));
        sim.play(&audio, &m, |_| Some("Hey".into()));
        let first = sim.checks[0];
        assert!(!first.1.final_check);
        let speech_at = first.1.end - FIRST_CHECK as u64;
        assert!(speech_at.abs_diff(lead) <= (FRAME * 2) as u64, "first check 0.40 s after the first loud frame: {first:?}");
        for w in sim.checks.windows(2) {
            if !w[1].1.final_check {
                assert!(w[1].1.end >= w[0].1.end + STEP as u64, "{w:?}");
            }
        }
        let last = *sim.checks.last().unwrap();
        assert!(last.1.final_check, "{:?}", sim.checks);
        assert!(last.1.end <= speech_at + WINDOW as u64);
        assert!(sim.checks.iter().all(|(_, c)| c.end <= speech_at + WINDOW as u64), "nothing beyond 3.0 s");

        // No second check while one is in flight.
        let mut sim = Sim::new(true);
        let mut audio = quiet(1.0);
        audio.extend(tone(2.0, 0.2));
        sim.play(&audio, &m, |_| None);
        assert_eq!(sim.checks.len(), 1);

        // A short phrase: the final check covers it, with its tail, after 300 ms of quiet.
        let mut sim = Sim::new(false);
        let mut audio = quiet(1.0);
        audio.extend(tone(0.5, 0.2));
        audio.extend(quiet(1.0));
        sim.play(&audio, &m, |_| Some("Thank you.".into()));
        assert_eq!(sim.checks.len(), 1, "sliding off: only the final check");
        let (sent_at, c) = sim.checks[0];
        assert!(c.final_check);
        let speech_end = lead + (RATE / 2) as u64;
        assert!(c.end.abs_diff(speech_end + TAIL as u64) <= FRAME as u64, "{c:?}");
        assert!(c.start <= lead && c.start + (LEAD_IN + FRAME) as u64 >= lead, "{c:?}");
        assert!(sent_at >= speech_end + (END_FRAMES * FRAME) as u64, "sent once it ended");

        // A 100 ms burst gets no check at all.
        let mut sim = Sim::new(true);
        let mut audio = quiet(1.0);
        audio.extend(tone(0.1, 0.2));
        audio.extend(quiet(1.0));
        sim.play(&audio, &m, |_| Some(String::new()));
        assert!(sim.checks.is_empty(), "{:?}", sim.checks);

        // Two empty results stop sliding, but the final check still happens.
        let mut sim = Sim::new(true);
        let mut audio = quiet(1.0);
        audio.extend(tone(2.5, 0.2));
        audio.extend(quiet(1.0));
        sim.play(&audio, &m, |_| Some(String::new()));
        assert_eq!(sim.checks.len(), 3, "{:?}", sim.checks);
        assert!(sim.checks[2].1.final_check);
    }

    #[test]
    fn idle_decides_on_the_listener_s_own_timeline() {
        let m = m();
        // "Navi" alone: the first sliding check (with the prompt) hears it 170 ms after the speech
        // stopped; a second check without the prompt goes at once, and its answer, 300 ms after,
        // makes it final.
        let mut sim = Sim::new(true);
        let mut audio = quiet(1.0);
        audio.extend(tone(0.45, 0.2));
        let mut decisions = sim.play(&audio, &m, |_| None);
        assert!(decisions.is_empty());
        let (_, c) = sim.checks[0];
        assert!(c.first);
        sim.ring.push(&quiet(0.17));
        sim.idle.advance(&sim.ring);
        assert_eq!(sim.idle.on_result(&c, "Navi.", &m, ListenMode::Normal), IdleDecision::Wait, "heard with the prompt: not on its own");
        sim.ring.push(&quiet(0.04));
        sim.idle.advance(&sim.ring);
        let again = sim.idle.next_check(&sim.ring).expect("a second check at once, not 0.3 s later");
        assert!(!again.first && !again.final_check);
        sim.ring.push(&quiet(0.13));
        sim.idle.advance(&sim.ring);
        let d = sim.idle.on_result(&again, "Navi.", &m, ListenMode::Normal);
        assert!(matches!(d, IdleDecision::Wake { start, heard_to, .. } if start < RATE as u64 && heard_to >= RATE as u64 * 29 / 20), "{d:?}");

        // A sliding check that comes back empty after the end leads to the final check.
        let mut sim = Sim::new(true);
        let mut audio = quiet(1.0);
        audio.extend(tone(0.6, 0.2));
        sim.play(&audio, &m, |_| None);
        let (_, c) = sim.checks[0];
        sim.ring.push(&quiet(0.4));
        sim.idle.advance(&sim.ring);
        assert_eq!(sim.idle.on_result(&c, "", &m, ListenMode::Normal), IdleDecision::Wait);
        let fin = sim.idle.next_check(&sim.ring).expect("a final check");
        assert!(fin.final_check);
        decisions.push(sim.idle.on_result(&fin, "Navi, take notes.", &m, ListenMode::Normal));
        assert!(matches!(decisions[0], IdleDecision::Command { action: Action::NotesOn, .. }), "{decisions:?}");

        // A stale result changes nothing.
        let mut sim = Sim::new(true);
        let mut audio = quiet(1.0);
        audio.extend(tone(1.0, 0.2));
        sim.play(&audio, &m, |_| None);
        let (_, c) = sim.checks[0];
        let old = IdleCheck { id: c.id + 10, ..c };
        assert_eq!(sim.idle.on_result(&old, "Navi", &m, ListenMode::Normal), IdleDecision::Wait);
        sim.idle.reset();
        assert_eq!(sim.idle.on_result(&c, "Navi", &m, ListenMode::Normal), IdleDecision::Wait, "forgotten by the reset");
    }

    #[test]
    fn an_utterance_that_ends_undecided_keeps_its_final_check() {
        // "Navi." [350 ms] "email Sam": the name's final check is still due when the next starts.
        let m = m();
        let mut sim = Sim::new(false);
        let mut audio = quiet(1.0);
        audio.extend(tone(0.4, 0.2));
        audio.extend(quiet(0.35));
        audio.extend(tone(0.2, 0.2));
        sim.play(&audio, &m, |_| None);
        let (_, c) = sim.checks[0];
        assert!(c.final_check);
        let d = sim.idle.on_result(&c, "Navi.", &m, ListenMode::Normal);
        assert!(matches!(d, IdleDecision::Wake { start, .. } if start < RATE as u64), "{d:?}");
    }

    #[test]
    fn only_the_first_sliding_check_goes_with_the_prompt() {
        let m = m();
        let mut audio = quiet(1.0);
        audio.extend(tone(1.5, 0.2));
        audio.extend(quiet(1.0));
        let mut sim = Sim::new(true);
        sim.play(&audio, &m, |_| Some("Navi, watch".into()));
        let firsts: Vec<bool> = sim.checks.iter().map(|(_, c)| c.first).collect();
        assert!(firsts.len() >= 3 && firsts[0] && firsts[1..].iter().all(|f| !f), "{firsts:?}");
        assert!(sim.checks.last().is_some_and(|(_, c)| c.final_check && !c.first));
        // Sliding off (the CPU engine, a busy GPU): the one check is a final one, with no prompt.
        let mut sim = Sim::new(false);
        sim.play(&audio, &m, |_| Some("Navi, watch".into()));
        assert!(sim.checks.iter().all(|(_, c)| c.final_check && !c.first), "{:?}", sim.checks);
    }

    #[test]
    fn heard_in_full_it_acts_as_soon_as_the_quiet_is_long_enough() {
        // "Navi watch my screen" (1.3 s): a check that heard it all comes back while you are still
        // in the 200 ms of quiet; it runs when that quiet is reached, with no further check.
        let m = m();
        let mut sim = Sim::new(true);
        let mut audio = quiet(1.0);
        audio.extend(tone(1.3, 0.2));
        sim.play(&audio, &m, |_| None);
        // The check in flight, as if it had covered everything up to now.
        let c = IdleCheck { end: sim.ring.end(), ..sim.checks.last().expect("a check").1 };
        sim.idle.in_flight = Some(c);
        sim.ring.push(&quiet(0.08));
        sim.idle.advance(&sim.ring);
        assert_eq!(sim.idle.on_result(&c, "Navi watch my screen.", &m, ListenMode::Normal), IdleDecision::Wait, "80 ms of quiet");
        let mut at = None;
        for i in 0..10 {
            sim.ring.push(&quiet(0.02));
            sim.idle.advance(&sim.ring);
            if let IdleDecision::Command { action: Action::WatchOn, .. } = sim.idle.poll() {
                at = Some(i);
                break;
            }
        }
        // 80 ms + 6 × 20 ms = 200 ms of quiet.
        assert_eq!(at, Some(5), "runs at 200 ms of quiet, not before");
        assert!(sim.idle.next_check(&sim.ring).is_none(), "decided: no more checks");

        // More speech after what the check heard: no decision until the next check.
        let mut sim = Sim::new(true);
        sim.play(&audio, &m, |_| None);
        let c = IdleCheck { end: sim.ring.end(), ..sim.checks.last().unwrap().1 };
        sim.idle.in_flight = Some(c);
        sim.ring.push(&quiet(0.1));
        sim.idle.advance(&sim.ring);
        assert_eq!(sim.idle.on_result(&c, "Navi, take notes.", &m, ListenMode::Normal), IdleDecision::Wait);
        sim.ring.push(&tone(0.5, 0.2)); // "...on the budget"
        sim.ring.push(&quiet(0.4));
        sim.idle.advance(&sim.ring);
        assert_eq!(sim.idle.poll(), IdleDecision::Wait, "it was dictation, not the command");
    }

    #[test]
    fn a_hmm_echoed_as_the_name_never_wakes() {
        // A 0.3 s "hmm": the prompted first check comes back "Hey Navi.", the check without the
        // prompt "home.".
        let m = m();
        let mut audio = quiet(1.0);
        audio.extend(tone(0.3, 0.2));
        audio.extend(quiet(1.0));
        let mut sim = Sim::new(true);
        let decisions = sim.play(&audio, &m, |c| Some(if c.first { "Hey Navi." } else { "home." }.into()));
        assert_eq!(decisions, [IdleDecision::Ignore], "{:?}", sim.checks);
        assert!(sim.checks.len() >= 2);
    }

    #[test]
    fn on_a_call_the_command_may_follow_the_wake_word_after_a_pause() {
        // "Navi." [500 ms] "Stop taking notes." during call notes (live clips 19 and 39).
        let m = m();
        let mut audio = quiet(1.0);
        audio.extend(tone(0.4, 0.2));
        audio.extend(quiet(0.5));
        audio.extend(tone(0.9, 0.2));
        audio.extend(quiet(1.0));
        let mut sim = Sim::new(true);
        sim.mode = ListenMode::Call;
        let second_starts = (RATE as f32 * 1.9) as u64;
        let decisions = sim.play(&audio, &m, |c| Some(if c.end < second_starts { "Navi." } else { "Stop taking notes." }.into()));
        assert!(matches!(decisions.last(), Some(IdleDecision::Command { action: Action::NotesOff, .. })), "{decisions:?}");
        // Too long a pause, and the second is just talk.
        let mut audio = quiet(1.0);
        audio.extend(tone(0.4, 0.2));
        audio.extend(quiet(2.5));
        audio.extend(tone(0.9, 0.2));
        audio.extend(quiet(1.0));
        let mut sim = Sim::new(true);
        sim.mode = ListenMode::Call;
        let second_starts = (RATE as f32 * 3.9) as u64;
        let decisions = sim.play(&audio, &m, |c| Some(if c.end < second_starts { "Navi." } else { "Stop taking notes." }.into()));
        assert!(decisions.iter().all(|d| *d == IdleDecision::Ignore), "{decisions:?}");
        // In Normal mode the bare name starts a take instead (the take hears the command).
        let mut audio = quiet(1.0);
        audio.extend(tone(0.4, 0.2));
        audio.extend(quiet(1.0));
        let mut sim = Sim::new(true);
        let decisions = sim.play(&audio, &m, |_| Some("Navi.".into()));
        assert!(matches!(decisions.as_slice(), [IdleDecision::Wake { .. }]), "{decisions:?}");
    }

    // ---- the in-take listener ----

    fn take_feed(l: &mut TakeListener, ring: &mut Ring, audio: &[f32]) -> Vec<TakeCheck> {
        let mut out = Vec::new();
        for chunk in audio.chunks(RATE / 20) {
            ring.push(chunk);
            l.advance(ring);
            if let Some(c) = l.next_check() {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn take_listener() {
        let m = m();
        // Dictation 2.5 s, quiet 0.6 s, a 0.6 s utterance: one alone check over that utterance.
        let (mut ring, mut l) = (Ring::new(), TakeListener::new(false, 0));
        l.arm(0);
        let mut audio = quiet(0.3);
        audio.extend(tone(2.5, 0.2));
        audio.extend(quiet(0.6));
        audio.extend(tone(0.6, 0.2));
        audio.extend(quiet(0.35));
        let checks = take_feed(&mut l, &mut ring, &audio);
        assert_eq!(checks.len(), 1, "{checks:?}");
        let c = checks[0];
        let speech_at = (RATE as f32 * 3.4) as u64;
        assert!(!c.first && c.speech_at.abs_diff(speech_at) <= FRAME as u64, "{c:?}");
        assert!(c.start + (LEAD_IN + FRAME) as u64 >= speech_at && c.start <= speech_at, "{c:?}");
        assert_eq!(c.end, c.utt_end + TAIL as u64);
        // A match then quiet: fires at utt_end + CONFIRM_QUIET, and Finish cuts before the phrase.
        assert_eq!(l.on_result(&c, "That's all.", &m, ring.end()), None, "not yet");
        ring.push(&quiet(0.1));
        l.advance(&ring);
        assert_eq!(l.poll(c.utt_end + CONFIRM_QUIET as u64 - 1), None);
        assert_eq!(l.poll(c.utt_end + CONFIRM_QUIET as u64), Some(TakeAction::Finish { cut: c.speech_at - CUT_MARGIN as u64, heard: "That's all.".into() }));
        assert_eq!(l.fired_after_ms(), 500);
        assert_eq!(l.next_check(), None, "done after one action");

        // A 3 s utterance is dictation: no check.
        let (mut ring, mut l) = (Ring::new(), TakeListener::new(false, 0));
        l.arm(0);
        let mut audio = quiet(0.3);
        audio.extend(tone(3.0, 0.2));
        audio.extend(quiet(0.5));
        assert!(take_feed(&mut l, &mut ring, &audio).is_empty());

        // A match then speech 0.35 s later: dropped.
        let (mut ring, mut l) = (Ring::new(), TakeListener::new(false, 0));
        l.arm(0);
        let mut audio = quiet(0.3);
        audio.extend(tone(0.6, 0.2));
        audio.extend(quiet(0.32));
        let c = take_feed(&mut l, &mut ring, &audio)[0];
        assert_eq!(l.on_result(&c, "Don't worry.", &m, ring.end()), None);
        let mut more = quiet(0.03);
        more.extend(tone(0.3, 0.2));
        take_feed(&mut l, &mut ring, &more);
        assert_eq!(l.poll(ring.end() + RATE as u64), None, "speech resumed: it was dictation");

        // Cancel fires once the quiet holds.
        let (mut ring, mut l) = (Ring::new(), TakeListener::new(false, 0));
        l.arm(0);
        let mut audio = quiet(0.3);
        audio.extend(tone(0.6, 0.2));
        audio.extend(quiet(0.6));
        let c = take_feed(&mut l, &mut ring, &audio)[0];
        assert!(matches!(l.on_result(&c, "Don't worry.", &m, ring.end()), Some(TakeAction::Cancel { .. })), "the result arrives after the confirm point");

        // Utterances before arm(at) are ignored (push-to-talk turned hands-free).
        let (mut ring, mut l) = (Ring::new(), TakeListener::new(false, 0));
        let mut audio = quiet(0.3);
        audio.extend(tone(0.6, 0.2));
        audio.extend(quiet(0.5));
        assert!(take_feed(&mut l, &mut ring, &audio).is_empty(), "not armed");
        l.arm(ring.end());
        assert!(take_feed(&mut l, &mut ring, &quiet(0.5)).is_empty());
        let mut audio = tone(0.6, 0.2);
        audio.extend(quiet(0.5));
        assert_eq!(take_feed(&mut l, &mut ring, &audio).len(), 1, "after the switch it listens");
    }

    #[test]
    fn take_listener_after_the_wake_word() {
        let m = m();
        // Preroll 0.8 s holding "Navi"; then "watch my screen".
        let preroll = (RATE * 4 / 5) as u64;
        let (mut ring, mut l) = (Ring::new(), TakeListener::new(true, preroll));
        l.arm(0);
        let mut audio = quiet(0.2);
        audio.extend(tone(0.35, 0.2)); // "Navi", inside the preroll
        audio.extend(quiet(0.7));
        audio.extend(tone(0.8, 0.2)); // "watch my screen"
        audio.extend(quiet(0.35));
        let checks = take_feed(&mut l, &mut ring, &audio);
        assert_eq!(checks.len(), 1, "the wake word itself is skipped: {checks:?}");
        let c = checks[0];
        assert!(c.first && c.start == 0 && c.end == c.utt_end + TAIL as u64, "{c:?}");
        ring.push(&quiet(0.3));
        l.advance(&ring);
        assert_eq!(l.on_result(&c, "Navi. Watch my screen.", &m, ring.end()), Some(TakeAction::Command { action: Action::WatchOn, heard: "Navi. Watch my screen.".into() }));

        // "Navi" [pause] "don't worry": cancel; "that's all" with nothing dictated also cancels.
        for said in ["Navi. Don't worry.", "Navi, that's all."] {
            let (mut ring, mut l) = (Ring::new(), TakeListener::new(true, preroll));
            l.arm(0);
            let c = take_feed(&mut l, &mut ring, &audio)[0];
            ring.push(&quiet(0.3));
            l.advance(&ring);
            assert!(matches!(l.on_result(&c, said, &m, ring.end()), Some(TakeAction::Cancel { .. })), "{said}");
        }

        // A first check that hears dictation closes the window: no command later.
        let (mut ring, mut l) = (Ring::new(), TakeListener::new(true, preroll));
        l.arm(0);
        let c = take_feed(&mut l, &mut ring, &audio)[0];
        assert_eq!(l.on_result(&c, "Navi. Email Sam about Friday.", &m, ring.end()), None);
        let mut next = tone(0.6, 0.2);
        next.extend(quiet(0.4));
        let later = take_feed(&mut l, &mut ring, &next);
        assert!(later.iter().all(|c| !c.first), "{later:?}");

        // Whisper left the name out: the first thing heard is still the command after it (live
        // clips 28, 38, 41), and finishing with nothing dictated cancels.
        for (said, want) in [("Watch my screen.", "command:watch_on"), ("Stop watching.", "command:watch_off"), ("That's all.", "cancel"), ("Don't worry.", "cancel")] {
            let (mut ring, mut l) = (Ring::new(), TakeListener::new(true, preroll));
            l.arm(0);
            let c = take_feed(&mut l, &mut ring, &audio)[0];
            ring.push(&quiet(0.3));
            l.advance(&ring);
            let got = match l.on_result(&c, said, &m, ring.end()) {
                Some(TakeAction::Command { action, .. }) => format!("command:{}", action.id()),
                Some(TakeAction::Cancel { .. }) => "cancel".into(),
                other => format!("{other:?}"),
            };
            assert_eq!(got, want, "{said}");
        }
        let (mut ring, mut l) = (Ring::new(), TakeListener::new(true, preroll));
        l.arm(0);
        let c = take_feed(&mut l, &mut ring, &audio)[0];
        assert_eq!(l.on_result(&c, "Watch my screen for a second.", &m, ring.end()), None, "the whole of it, or dictation");
    }

    #[test]
    fn a_command_said_while_the_take_was_starting_is_not_skipped() {
        // A slow check, or a Bluetooth microphone opening: by the time the take began, "Navi" and
        // "stop taking notes" were both in its preroll. The idle listener had judged only "Navi".
        let m = m();
        let preroll = (RATE * 2) as u64;
        let mut audio = quiet(0.2);
        audio.extend(tone(0.35, 0.2)); // "Navi"
        audio.extend(quiet(0.4));
        audio.extend(tone(0.8, 0.2)); // "stop taking notes", ends at 1.75 s
        audio.extend(quiet(0.6));
        let (mut ring, mut l) = (Ring::new(), TakeListener::new(true, preroll));
        l.arm(0);
        assert!(take_feed(&mut l, &mut ring, &audio).is_empty(), "without skip_to the whole preroll counts as the wake word");
        let (mut ring, mut l) = (Ring::new(), TakeListener::new(true, preroll));
        l.skip_to((RATE * 7 / 10) as u64);
        l.arm(0);
        let checks = take_feed(&mut l, &mut ring, &audio);
        assert!(matches!(checks.as_slice(), [c] if c.first && c.start == 0), "{checks:?}");
        ring.push(&quiet(0.3));
        l.advance(&ring);
        assert!(matches!(l.on_result(&checks[0], "Navi. Stop taking notes.", &m, ring.end()), Some(TakeAction::Command { action: Action::NotesOff, .. })));
    }

    #[test]
    fn a_colour_said_after_a_pause_ends_the_take_and_changes_colour() {
        // "Navi" [pause] "turn gold": the take the wake word started gives way to the colour.
        let m = m();
        let preroll = (RATE * 4 / 5) as u64;
        let mut audio = quiet(0.2);
        audio.extend(tone(0.35, 0.2)); // "Navi", inside the preroll
        audio.extend(quiet(0.7));
        audio.extend(tone(0.6, 0.2)); // "turn gold"
        audio.extend(quiet(0.35));
        for said in ["Navi. Turn gold.", "Turn gold."] {
            let (mut ring, mut l) = (Ring::new(), TakeListener::new(true, preroll));
            l.arm(0);
            let c = take_feed(&mut l, &mut ring, &audio)[0];
            ring.push(&quiet(0.3));
            l.advance(&ring);
            assert_eq!(l.on_result(&c, said, &m, ring.end()), Some(TakeAction::Command { action: Action::Colour("gold"), heard: said.into() }), "{said}");
        }
        // Later in a take it is dictation: only cancel and finish are listened for there.
        let (mut ring, mut l) = (Ring::new(), TakeListener::new(false, 0));
        l.arm(0);
        let mut audio = quiet(0.3);
        audio.extend(tone(0.6, 0.2));
        audio.extend(quiet(0.6));
        let c = take_feed(&mut l, &mut ring, &audio)[0];
        assert_eq!(l.on_result(&c, "Turn gold.", &m, ring.end()), None);
    }

    #[test]
    fn speech_starting_again_drops_a_pending_cancel() {
        let m = m();
        let (mut ring, mut l) = (Ring::new(), TakeListener::new(false, 0));
        l.arm(0);
        let mut audio = quiet(0.3);
        audio.extend(tone(0.6, 0.2));
        audio.extend(quiet(0.32));
        let c = take_feed(&mut l, &mut ring, &audio)[0];
        assert_eq!(l.on_result(&c, "Don't worry.", &m, ring.end()), None);
        // 120 ms later two loud frames: speech may be starting, too soon for a whole new utterance.
        let mut more = quiet(0.08);
        more.extend(tone(0.04, 0.2));
        take_feed(&mut l, &mut ring, &more);
        assert_eq!(l.seg.loud_frames_after(c.utt_end), 2);
        assert_eq!(l.poll(ring.end() + RATE as u64), None, "dropped: it was dictation");

        // One click is not speech.
        let (mut ring, mut l) = (Ring::new(), TakeListener::new(false, 0));
        l.arm(0);
        let c = take_feed(&mut l, &mut ring, &audio)[0];
        assert_eq!(l.on_result(&c, "Don't worry.", &m, ring.end()), None);
        let mut click = quiet(0.08);
        click.extend(tone(0.02, 0.2));
        click.extend(quiet(0.2));
        take_feed(&mut l, &mut ring, &click);
        assert!(matches!(l.poll(ring.end()), Some(TakeAction::Cancel { .. })));

        // Resumed after an action it returned: it listens again.
        l.resume();
        let mut again = tone(0.6, 0.2);
        again.extend(quiet(0.35));
        assert_eq!(take_feed(&mut l, &mut ring, &again).len(), 1);
    }

    // ---- live measurement (run by hand) ----

    #[derive(serde::Deserialize, Clone, Debug)]
    #[serde(rename_all = "camelCase")]
    struct Clip {
        file: String,
        text: String,
        #[serde(default)]
        voice: String,
        #[serde(default)]
        rate: serde_json::Value,
        kind: String,
        expect: String,
        #[serde(default)]
        wake_only: Option<String>,
    }

    struct SimOutcome {
        got: String,
        at: u64,
        texts: Vec<(String, u64)>,
    }

    fn with_noise(audio: &[f32], seed: u32) -> Vec<f32> {
        let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
        audio
            .iter()
            .map(|s| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                s + ((x >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0) * 0.002
            })
            .collect()
    }

    /// (first, last) 20 ms frames above -45 dBFS in a clean clip, as sample positions.
    fn speech_span(audio: &[f32]) -> Option<(u64, u64)> {
        let loud: Vec<usize> = audio.chunks(FRAME).enumerate().filter(|(_, f)| frame_db(f) > -45.0).map(|(i, _)| i).collect();
        Some(((*loud.first()? * FRAME) as u64, ((*loud.last()? + 1) * FRAME) as u64))
    }

    fn idle_got(d: &IdleDecision) -> String {
        match d {
            IdleDecision::Wake { .. } => "wake".into(),
            IdleDecision::Command { action, .. } => format!("command:{}", action.id()),
            _ => "ignore".into(),
        }
    }

    async fn simulate_idle(engine: &crate::engine::Engine, m: &Matcher, audio: &[f32], mode: ListenMode, prompt: &str) -> SimOutcome {
        let (mut ring, mut idle) = (Ring::new(), IdleListener::new(true));
        let mut pending: Option<(IdleCheck, String, u64)> = None;
        let mut texts = Vec::new();
        let tick = RATE / 25;
        let mut padded = audio.to_vec();
        padded.extend(with_noise(&vec![0.0; RATE * 2], 7));
        for chunk in padded.chunks(tick) {
            ring.push(chunk);
            idle.advance(&ring);
            let d = idle.poll();
            if d != IdleDecision::Wait {
                return SimOutcome { got: idle_got(&d), at: ring.end(), texts };
            }
            if let Some((c, text, ready)) = pending.clone() {
                if ring.end() >= ready {
                    pending = None;
                    let d = idle.on_result(&c, &text, m, mode);
                    if d != IdleDecision::Wait {
                        return SimOutcome { got: idle_got(&d), at: ring.end(), texts };
                    }
                }
            }
            if pending.is_none() {
                if let Some(c) = idle.next_check(&ring) {
                    // As wake.rs: the prompt only with the first sliding check.
                    let opts = crate::engine::TranscribeOptions { language: "en".into(), prompt: if c.first { prompt.to_string() } else { String::new() } };
                    let clip = ring.slice(c.start, c.end);
                    let t0 = std::time::Instant::now();
                    let tr = engine.transcribe(&clip, &opts).await.expect("transcribe");
                    let wall = t0.elapsed().as_millis() as u64;
                    texts.push((tr.text.clone(), tr.ms));
                    pending = Some((c, tr.text, ring.end() + wall * (RATE as u64 / 1000)));
                }
            }
        }
        SimOutcome { got: "ignore".into(), at: ring.end(), texts }
    }

    async fn simulate_take(engine: &crate::engine::Engine, m: &Matcher, audio: &[f32], from_wake: bool, preroll: u64) -> SimOutcome {
        let (mut ring, mut l) = (Ring::new(), TakeListener::new(from_wake, preroll));
        l.arm(0);
        let mut pending: Option<(TakeCheck, String, u64)> = None;
        let mut texts = Vec::new();
        let mut padded = audio.to_vec();
        padded.extend(with_noise(&vec![0.0; RATE * 2], 11));
        // The preroll arrives all at once, as it is prepended before the listener starts.
        let head = (preroll as usize).min(padded.len());
        let mut chunks: Vec<&[f32]> = vec![&padded[..head]];
        chunks.extend(padded[head..].chunks(RATE / 20));
        let got = |a: &TakeAction| match a {
            TakeAction::Cancel { .. } => "cancel".to_string(),
            TakeAction::Finish { .. } => "finish".to_string(),
            TakeAction::Command { action, .. } => format!("command:{}", action.id()),
        };
        for chunk in chunks {
            ring.push(chunk);
            l.advance(&ring);
            if let Some((c, text, ready)) = pending.clone() {
                if ring.end() >= ready {
                    pending = None;
                    if let Some(a) = l.on_result(&c, &text, m, ring.end()) {
                        return SimOutcome { got: got(&a), at: ring.end(), texts };
                    }
                }
            }
            if let Some(a) = l.poll(ring.end()) {
                return SimOutcome { got: got(&a), at: ring.end(), texts };
            }
            if pending.is_none() {
                if let Some(c) = l.next_check() {
                    // As controller::spawn_take_listener: take checks never carry the prompt.
                    let opts = crate::engine::TranscribeOptions { language: "en".into(), prompt: String::new() };
                    let clip = ring.slice(c.start, c.end);
                    let t0 = std::time::Instant::now();
                    let tr = engine.transcribe(&clip, &opts).await.expect("transcribe");
                    let wall = t0.elapsed().as_millis() as u64;
                    texts.push((tr.text.clone(), tr.ms));
                    pending = Some((c, tr.text, ring.end() + wall * (RATE as u64 / 1000)));
                }
            }
        }
        SimOutcome { got: "none".into(), at: ring.end(), texts }
    }

    fn percentile(v: &[i64], p: f32) -> i64 {
        if v.is_empty() {
            return 0;
        }
        let mut s = v.to_vec();
        s.sort_unstable();
        s[((s.len() - 1) as f32 * p).round() as usize]
    }

    /// cargo test --lib spoken::tests::live_detection -- --ignored --nocapture
    /// Env: NAVI_VOICE_CLIPS (folder with clips.json), optional NAVI_WHISPER_SERVER, NAVI_WHISPER_MODEL,
    /// NAVI_VOICE_PROMPT=0 to run without the prompt.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn live_detection() {
        use crate::engine::{Engine, StatusCallback};
        use std::path::PathBuf;
        use std::sync::Arc;

        let dir = PathBuf::from(std::env::var("NAVI_VOICE_CLIPS").expect("set NAVI_VOICE_CLIPS to the folder holding clips.json"));
        let mut clips: Vec<Clip> = serde_json::from_str(&std::fs::read_to_string(dir.join("clips.json")).expect("read clips.json")).expect("parse clips.json");
        let use_prompt = std::env::var("NAVI_VOICE_PROMPT").map(|v| v.trim() != "0").unwrap_or(true);
        let paths = crate::paths::Paths::for_tests("voice-live");

        // Silence trimming, as the app runs it: Navi's own copy of the Silero model, else download.
        let vad_file = "ggml-silero-v5.1.2.bin";
        let vad_target = paths.models.join(vad_file);
        if !vad_target.is_file() {
            let found = std::env::var_os("APPDATA")
                .map(PathBuf::from)
                .into_iter()
                .flat_map(|a| ["com.navaigate.navi", "com.goodlad.navi"].map(|id| a.join(id).join("models").join(vad_file)))
                .find(|p| p.is_file());
            let copied = found.is_some_and(|p| std::fs::copy(&p, &vad_target).is_ok());
            if !copied {
                crate::models::download_model("vad", &paths, Arc::new(|_: crate::types::DownloadProgress| {})).await.expect("vad download");
            }
        }
        let home = dirs::home_dir().expect("home");
        let settings = crate::types::EngineSettings {
            model: "large-v3-turbo".into(),
            language: "en".into(),
            use_gpu: true,
            server_path: Some(std::env::var("NAVI_WHISPER_SERVER").unwrap_or_else(|_| home.join(r".local\tools\whisper-cuda\Release\whisper-server.exe").to_string_lossy().into_owned())),
            model_path: Some(std::env::var("NAVI_WHISPER_MODEL").unwrap_or_else(|_| home.join(r".local\share\whisper\ggml-large-v3-turbo.bin").to_string_lossy().into_owned())),
            vad: true,
        };
        let on_status: StatusCallback = Arc::new(|_: crate::types::EngineStatus| {});
        let engine = Engine::start(&settings, &paths, on_status).await.expect("engine start");
        assert_eq!(engine.status().state, "ready");
        assert_eq!(engine.status().device, "cuda", "the live test measures the GPU engine");

        let vc = VoiceCommandSettings::default();
        let m = Matcher::new(&vc);
        let prompt = build_prompt(&vc.phrases("wake"), use_prompt);
        println!("prompt: {prompt:?}");

        // Three made-up negatives: white noise at -30 dBFS, near silence, a 1 kHz tone.
        let lead = with_noise(&vec![0.0; RATE * 3 / 5], 3);
        let tail = with_noise(&vec![0.0; RATE * 3 / 2], 5);
        let mut generated: Vec<(String, Vec<f32>)> = Vec::new();
        let mut x: u32 = 12_345;
        let noise: Vec<f32> = (0..RATE * 3)
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((x >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0) * 0.0316 * 1.732
            })
            .collect();
        generated.push(("white noise".into(), noise));
        generated.push(("near silence".into(), vec![0.0; RATE * 3]));
        generated.push(("1 kHz tone".into(), (0..RATE).map(|i| (i as f32 * std::f32::consts::TAU * 1000.0 / RATE as f32).sin() * 0.3).collect()));
        for (name, body) in &generated {
            let mut audio = lead.clone();
            audio.extend(body);
            audio.extend(&tail);
            let file = paths.data.join(format!("generated-{}.wav", name.replace(' ', "-")));
            crate::audio::write_wav(&file, &audio).expect("write generated clip");
            clips.push(Clip { file: file.to_string_lossy().into_owned(), text: name.clone(), voice: "generated".into(), rate: serde_json::Value::Null, kind: "idle".into(), expect: "ignore".into(), wake_only: None });
        }

        // Speech lengths of the wake-alone clips, for where the name ends inside dictation clips.
        let mut wake_len: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        for c in &clips {
            if let Ok(a) = crate::audio::read_wav_16k(&dir.join(&c.file)) {
                if let Some((s, e)) = speech_span(&a) {
                    wake_len.insert(c.file.clone(), e - s);
                }
            }
        }

        struct Row {
            text: String,
            kind: String,
            expect: String,
            got: String,
            latency: Option<i64>,
        }
        let mut rows: Vec<Row> = Vec::new();
        for (i, c) in clips.iter().enumerate() {
            let clean = crate::audio::read_wav_16k(&dir.join(&c.file)).expect("read clip");
            let audio = with_noise(&clean, i as u32 + 1);
            let span = speech_span(&clean);
            let outcome = match c.kind.as_str() {
                "idle" => simulate_idle(&engine, &m, &audio, ListenMode::Normal, &prompt).await,
                "call" => simulate_idle(&engine, &m, &audio, ListenMode::Call, &prompt).await,
                "take" => simulate_take(&engine, &m, &audio, false, 0).await,
                "wake-take" => {
                    // The preroll ends 300 ms after the first run of speech (the wake word).
                    let frames: Vec<bool> = clean.chunks(FRAME).map(|f| frame_db(f) > -45.0).collect();
                    let first_loud = frames.iter().position(|l| *l).unwrap_or(0);
                    let run_end = frames[first_loud..].windows(10).position(|w| w.iter().all(|l| !l)).map_or(frames.len(), |p| first_loud + p);
                    let preroll = (run_end * FRAME + RATE * 3 / 10) as u64;
                    simulate_take(&engine, &m, &audio, true, preroll).await
                }
                other => panic!("unknown clip kind {other}"),
            };
            let expect_wake = c.expect == "dictation";
            let correct = outcome.got == c.expect || (expect_wake && outcome.got == "wake");
            let phrase_end = if expect_wake {
                c.wake_only.as_ref().and_then(|w| wake_len.get(w)).map(|len| (RATE * 3 / 5) as u64 + len)
            } else {
                span.map(|(_, e)| e)
            };
            let latency = phrase_end.filter(|_| correct && !matches!(c.expect.as_str(), "ignore" | "none")).map(|e| (outcome.at as i64 - e as i64) * 1000 / RATE as i64);
            let checks: Vec<String> = outcome.texts.iter().map(|(t, ms)| format!("{ms} ms {t:?}")).collect();
            println!(
                "{:>2} {:<10} {:<5} {:<22} got {:<22} {} checks {:>6} ms | {}",
                i + 1,
                c.voice,
                c.rate,
                c.expect,
                outcome.got,
                outcome.texts.len(),
                latency.map_or("-".to_string(), |l| l.to_string()),
                checks.join(" | ")
            );
            rows.push(Row { text: c.text.clone(), kind: c.kind.clone(), expect: c.expect.clone(), got: outcome.got, latency });
        }
        engine.shutdown().await;
        let _ = std::fs::remove_dir_all(&paths.data);

        let negative = |r: &Row| matches!(r.expect.as_str(), "ignore" | "none");
        let correct = |r: &Row| r.got == r.expect || (r.expect == "dictation" && r.got == "wake");
        let false_triggers: Vec<&Row> = rows.iter().filter(|r| negative(r) && !correct(r)).collect();
        let positives: Vec<&Row> = rows.iter().filter(|r| !negative(r)).collect();
        let right = positives.iter().filter(|r| correct(r)).count();
        println!("\npositives correct: {right} of {}", positives.len());
        type Member = fn(&Row) -> bool;
        let groups: [(&str, Member); 4] = [
            ("wake only", |r| r.expect == "wake"),
            ("idle commands", |r| (r.kind == "idle" || r.kind == "call") && r.expect.starts_with("command:")),
            ("in-take", |r| (r.kind == "take" || r.kind == "wake-take") && !matches!(r.expect.as_str(), "ignore" | "none")),
            ("dictation (from the name)", |r| r.expect == "dictation"),
        ];
        let mut too_slow = Vec::new();
        for (name, member) in &groups {
            let lat: Vec<i64> = rows.iter().filter(|r| member(r)).filter_map(|r| r.latency).collect();
            let n = rows.iter().filter(|r| member(r)).count();
            let ok = rows.iter().filter(|r| member(r) && correct(r)).count();
            let median = percentile(&lat, 0.5);
            println!("{name}: {ok} of {n} correct, median {median} ms, 90th percentile {} ms (target 500 ms)", percentile(&lat, 0.9));
            if !lat.is_empty() && median > 800 && *name != "dictation (from the name)" {
                too_slow.push(format!("{name} median {median} ms"));
            }
        }
        let mut by_text: std::collections::BTreeMap<&str, (usize, usize)> = std::collections::BTreeMap::new();
        for r in &positives {
            let e = by_text.entry(r.text.as_str()).or_default();
            e.1 += 1;
            if correct(r) {
                e.0 += 1;
            }
        }
        let weak_texts: Vec<String> = by_text.iter().filter(|(_, (ok, n))| *n >= 4 && *ok < 3).map(|(t, (ok, n))| format!("{t} ({ok}/{n})")).collect();
        for r in &false_triggers {
            println!("FALSE TRIGGER: {:?} ({}) gave {}", r.text, r.kind, r.got);
        }
        assert!(false_triggers.is_empty(), "{} negative clips triggered", false_triggers.len());
        assert!(right * 10 >= positives.len() * 9, "only {right} of {} positive clips were right", positives.len());
        assert!(weak_texts.is_empty(), "right in fewer than 3 of 4 variants: {weak_texts:?}");
        assert!(too_slow.is_empty(), "slower than the 800 ms guard: {too_slow:?}");
    }

    // ---- live measurement on the speech engine a running Navi already has (tester) ----
    //
    // The same simulation as live_detection, with two differences: the checks go to the
    // whisper-server Navi itself is running (so Navi need not be quit), and a wake is followed end
    // to end. The idle listener's Wake hands the audio from the utterance's start to a TakeListener
    // as its preroll, as wake.rs (act) and controller::spawn_take_listener do, so "Navi. [pause]
    // Watch my screen" is decided by the same chain as in the app, and every wake or dictation clip
    // also shows that its take stays quiet.

    #[derive(serde::Deserialize, Clone, Debug)]
    #[serde(rename_all = "camelCase")]
    struct LiveClip {
        file: String,
        text: String,
        #[serde(default)]
        voice: String,
        #[serde(default)]
        rate: serde_json::Value,
        kind: String,
        expect: String,
        #[serde(default)]
        wake_only: Option<String>,
        /// The wake words for this clip, when not the defaults.
        #[serde(default)]
        wake: Option<Vec<String>>,
    }

    /// The whisper-server a running Navi started. Posts what Engine::transcribe posts.
    struct RunningEngine {
        http: reqwest::Client,
        port: u16,
        /// NAVI_VOICE_PROMPT_MAX_SECS: send the prompt only with checks this short or shorter.
        prompt_max_secs: Option<f32>,
    }

    impl RunningEngine {
        /// NAVI_WHISPER_PORT, else the last "speech engine ready on port N" line in Navi's log.
        async fn find() -> Result<Self, String> {
            let port: u16 = match std::env::var("NAVI_WHISPER_PORT") {
                Ok(p) => p.trim().parse().map_err(|e| format!("NAVI_WHISPER_PORT: {e}"))?,
                Err(_) => {
                    let local = std::env::var_os("LOCALAPPDATA").ok_or("no LOCALAPPDATA")?;
                    let log = ["com.goodlad.navi", "com.navaigate.navi"]
                        .iter()
                        .map(|id| std::path::PathBuf::from(&local).join(id).join("logs").join("Navi.log"))
                        .find(|p| p.is_file())
                        .ok_or("no Navi.log found; set NAVI_WHISPER_PORT")?;
                    let text = std::fs::read_to_string(&log).map_err(|e| e.to_string())?;
                    text.lines()
                        .rev()
                        .find_map(|l| l.split("speech engine ready on port ").nth(1))
                        .and_then(|rest| rest.split_whitespace().next())
                        .and_then(|n| n.parse().ok())
                        .ok_or("no \"speech engine ready on port\" line in Navi.log")?
                }
            };
            let http = reqwest::Client::new();
            let health = http.get(format!("http://127.0.0.1:{port}/health")).send().await.map_err(|e| format!("port {port}: {e}"))?;
            if !health.status().is_success() {
                return Err(format!("port {port} answered {}", health.status()));
            }
            let prompt_max_secs = std::env::var("NAVI_VOICE_PROMPT_MAX_SECS").ok().and_then(|s| s.trim().parse().ok());
            Ok(Self { http, port, prompt_max_secs })
        }

        /// (words, wall time in ms) as Engine::transcribe gives them: padded to 1.05 s,
        /// verbose_json, temperature 0, English, the prompt if any, non-speech segments dropped.
        async fn transcribe(&self, samples: &[f32], prompt: &str) -> (String, u64) {
            if samples.is_empty() {
                return (String::new(), 0);
            }
            let t0 = std::time::Instant::now();
            let mut padded = samples.to_vec();
            padded.resize(padded.len().max(RATE * 21 / 20), 0.0);
            let wav = crate::audio::wav_bytes(&padded).expect("wav");
            let file = reqwest::multipart::Part::bytes(wav).file_name("audio.wav").mime_str("audio/wav").expect("mime");
            let mut form = reqwest::multipart::Form::new().part("file", file).text("response_format", "verbose_json").text("temperature", "0.0").text("language", "en");
            let too_long = self.prompt_max_secs.is_some_and(|max| samples.len() as f32 > max * RATE as f32);
            if !prompt.trim().is_empty() && !too_long {
                form = form.text("prompt", prompt.trim().to_string());
            }
            let url = format!("http://127.0.0.1:{}/inference", self.port);
            let body = self.http.post(url).multipart(form).send().await.expect("post").text().await.expect("reply");
            let ms = t0.elapsed().as_millis() as u64;
            let reply: serde_json::Value = serde_json::from_str(&body).expect("reply json");
            let non_speech = |s: &str| {
                let t = s.trim();
                (t.starts_with('[') && t.ends_with(']')) || t.eq_ignore_ascii_case("(silence)")
            };
            let joined: String = match reply.get("segments").and_then(|s| s.as_array()) {
                Some(segs) => segs.iter().filter_map(|s| s.get("text").and_then(|t| t.as_str())).filter(|t| !non_speech(t)).collect(),
                None => reply.get("text").and_then(|t| t.as_str()).unwrap_or("").split('\n').filter(|l| !non_speech(l)).collect(),
            };
            (joined.split_whitespace().collect::<Vec<_>>().join(" "), ms)
        }
    }

    struct LiveCheck {
        what: &'static str,
        secs: f32,
        text: String,
        ms: u64,
    }

    struct LiveRun {
        got: String,
        /// Sample position of the decision.
        at: u64,
        /// The listener's own "after you stopped" figure.
        after_ms: Option<u32>,
        /// The utterance's start (Wake and Command), where a wake take's audio begins.
        start: u64,
        /// A Wake's heard_to: what the idle listener judged, for the take's skip_to.
        heard_to: u64,
        checks: Vec<LiveCheck>,
    }

    /// wake.rs's loop in simulated real time: a 40 ms tick, one check in flight, a result read on
    /// the first tick after it would have come back, poll() every tick, the prompt only with an
    /// utterance's first sliding check. Plays the whole clip, past an Ignore, and returns the
    /// first Wake or Command.
    async fn live_idle(e: &RunningEngine, m: &Matcher, padded: &[f32], mode: ListenMode, prompt: &str) -> LiveRun {
        let (mut ring, mut idle) = (Ring::new(), IdleListener::new(true));
        let mut pending: Option<(IdleCheck, String, u64)> = None;
        let mut checks = Vec::new();
        // After the clip, quiet keeps coming (as from a real microphone) while a check is out.
        let quiet_tick = with_noise(&[0.0; RATE / 25], 99);
        let mut chunks = padded.chunks(RATE / 25);
        let mut extra = 0;
        loop {
            let chunk = match chunks.next() {
                Some(c) => c,
                None if pending.is_some() && extra < 25 * 30 => {
                    extra += 1;
                    &quiet_tick[..]
                }
                None => break,
            };
            ring.push(chunk);
            idle.advance(&ring);
            let mut decision = idle.poll();
            if let Some((c, text, ready)) = pending.clone() {
                if decision == IdleDecision::Wait && ring.end() >= ready {
                    pending = None;
                    decision = idle.on_result(&c, &text, m, mode);
                }
            }
            match decision {
                IdleDecision::Wake { start, heard_to, after_ms, .. } => return LiveRun { got: "wake".into(), at: ring.end(), after_ms: Some(after_ms), start, heard_to, checks },
                IdleDecision::Command { action, start, after_ms, .. } => {
                    return LiveRun { got: format!("command:{}", action.id()), at: ring.end(), after_ms: Some(after_ms), start, heard_to: 0, checks };
                }
                IdleDecision::Wait | IdleDecision::Ignore => {}
            }
            if pending.is_none() {
                if let Some(c) = idle.next_check(&ring) {
                    let clip = ring.slice(c.start, c.end);
                    let (text, ms) = e.transcribe(&clip, if c.first { prompt } else { "" }).await;
                    checks.push(LiveCheck { what: if c.final_check { "final" } else if c.first { "first" } else { "slide" }, secs: clip.len() as f32 / RATE as f32, text: text.clone(), ms });
                    pending = Some((c, text, ring.end() + ms * (RATE as u64 / 1000)));
                }
            }
        }
        LiveRun { got: "ignore".into(), at: ring.end(), after_ms: None, start: 0, heard_to: 0, checks }
    }

    /// controller::spawn_take_listener in simulated real time: the preroll arrives at once, then a
    /// 50 ms tick; no check carries the prompt. `heard`: skip_to, as wake.rs passes it.
    async fn live_take(e: &RunningEngine, m: &Matcher, take: &[f32], from_wake: bool, preroll: usize, heard: Option<u64>) -> LiveRun {
        let (mut ring, mut l) = (Ring::new(), TakeListener::new(from_wake, preroll as u64));
        if let Some(heard) = heard {
            l.skip_to(heard);
        }
        l.arm(0);
        let mut pending: Option<(TakeCheck, String, u64)> = None;
        let mut checks = Vec::new();
        let head = preroll.min(take.len());
        let mut chunks: Vec<&[f32]> = Vec::new();
        if head > 0 {
            chunks.push(&take[..head]);
        }
        chunks.extend(take[head..].chunks(RATE / 20));
        let name = |a: &TakeAction| match a {
            TakeAction::Cancel { .. } => "cancel".to_string(),
            TakeAction::Finish { .. } => "finish".to_string(),
            TakeAction::Command { action, .. } => format!("command:{}", action.id()),
        };
        // After the clip, quiet keeps coming while a check is out or a phrase awaits confirmation.
        let quiet_tick = with_noise(&[0.0; RATE / 20], 98);
        let mut chunks = chunks.into_iter();
        let mut extra = 0;
        loop {
            let chunk = match chunks.next() {
                Some(c) => c,
                None if (pending.is_some() || l.pending.is_some()) && extra < 20 * 30 => {
                    extra += 1;
                    &quiet_tick[..]
                }
                None => break,
            };
            ring.push(chunk);
            l.advance(&ring);
            let mut action = None;
            if let Some((c, text, ready)) = pending.clone() {
                if ring.end() >= ready {
                    pending = None;
                    action = l.on_result(&c, &text, m, ring.end());
                }
            }
            if let Some(a) = action.or_else(|| l.poll(ring.end())) {
                return LiveRun { got: name(&a), at: ring.end(), after_ms: Some(l.fired_after_ms()), start: 0, heard_to: 0, checks };
            }
            if pending.is_none() {
                if let Some(c) = l.next_check() {
                    let clip = ring.slice(c.start, c.end);
                    let (text, ms) = e.transcribe(&clip, "").await;
                    checks.push(LiveCheck { what: if c.first { "take-first" } else { "alone" }, secs: clip.len() as f32 / RATE as f32, text: text.clone(), ms });
                    pending = Some((c, text, ring.end() + ms * (RATE as u64 / 1000)));
                }
            }
        }
        LiveRun { got: "none".into(), at: ring.end(), after_ms: None, start: 0, heard_to: 0, checks }
    }

    /// End of the first run of speech (followed by 200 ms below -45 dBFS) in a clean clip.
    fn first_run_end(audio: &[f32]) -> Option<u64> {
        let loud: Vec<bool> = audio.chunks(FRAME).map(|f| frame_db(f) > -45.0).collect();
        let first = loud.iter().position(|l| *l)?;
        let run_end = loud[first..].windows(10).position(|w| w.iter().all(|l| !l)).map_or(loud.len(), |p| first + p);
        Some((run_end * FRAME) as u64)
    }

    /// cargo test --lib spoken::tests::live_detection_on_the_running_engine -- --ignored --nocapture
    /// Env: NAVI_VOICE_CLIPS (the folder scripts/make_voice_clips.ps1 wrote); optional
    /// NAVI_WHISPER_PORT (else read from Navi's log), NAVI_VOICE_PROMPT=0 to run without the prompt.
    /// Navi keeps running: its own whisper-server answers the checks.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn live_detection_on_the_running_engine() {
        let dir = std::path::PathBuf::from(std::env::var("NAVI_VOICE_CLIPS").expect("set NAVI_VOICE_CLIPS to the folder holding clips.json"));
        let clips: Vec<LiveClip> = serde_json::from_str(&std::fs::read_to_string(dir.join("clips.json")).expect("read clips.json")).expect("parse clips.json");
        let use_prompt = std::env::var("NAVI_VOICE_PROMPT").map(|v| v.trim() != "0").unwrap_or(true);
        let engine = RunningEngine::find().await.expect("Navi's running speech engine");
        if let Some(max) = engine.prompt_max_secs {
            println!("the prompt goes only with checks of {max} s or less");
        }
        match std::env::var("NAVI_VOICE_PROMPT_TEXT") {
            Ok(t) => println!("engine: the running Navi's whisper-server, port {}; prompt replaced with {t:?}", engine.port),
            Err(_) => println!("engine: the running Navi's whisper-server, port {}; prompt {}", engine.port, if use_prompt { "on" } else { "off" }),
        }

        let mut all: Vec<(String, LiveClip, Vec<f32>)> = clips
            .iter()
            .map(|c| {
                let n = c.file.split('-').next().unwrap_or("?").to_string();
                (n, c.clone(), crate::audio::read_wav_16k(&dir.join(&c.file)).expect("read clip"))
            })
            .collect();
        // Three made-up negatives: white noise at -30 dBFS, near silence, a 1 kHz tone, each with
        // 0.6 s before and 1.5 s after (the low noise is added to every clip below).
        let mut x: u32 = 12_345;
        let noise: Vec<f32> = (0..RATE * 3)
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((x >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0) * 0.0316 * 1.732
            })
            .collect();
        let tone: Vec<f32> = (0..RATE).map(|i| (i as f32 * std::f32::consts::TAU * 1000.0 / RATE as f32).sin() * 0.3).collect();
        for (n, name, body) in [("G1", "white noise", noise), ("G2", "near silence", vec![0.0; RATE * 3]), ("G3", "1 kHz tone", tone)] {
            let mut audio = vec![0.0; RATE * 3 / 5];
            audio.extend(body);
            audio.extend(vec![0.0; RATE * 3 / 2]);
            let clip = LiveClip { file: name.into(), text: name.into(), voice: "gen".into(), rate: serde_json::Value::Null, kind: "idle".into(), expect: "ignore".into(), wake_only: None, wake: None };
            all.push((n.into(), clip, audio));
        }
        let wake_len: std::collections::HashMap<String, u64> = all.iter().filter_map(|(_, c, a)| speech_span(a).map(|(s, e)| (c.file.clone(), e - s))).collect();

        // The engine has sat idle; two throwaway checks so the first clip does not pay for it.
        for _ in 0..2 {
            let _ = engine.transcribe(&all[0].2, "Navi, Hey Navi.").await;
        }

        struct Row {
            n: String,
            text: String,
            kind: String,
            expect: String,
            got: String,
            then: Option<String>,
            correct: bool,
            latency: Option<i64>,
        }
        let ms_of = |samples: i64| samples * 1000 / RATE as i64;
        let mut rows: Vec<Row> = Vec::new();
        let mut engine_ms: Vec<i64> = Vec::new();
        for (i, (n, c, clean)) in all.iter().enumerate() {
            let mut padded = with_noise(clean, i as u32 + 1);
            padded.extend(with_noise(&vec![0.0; RATE * 2], 1_000 + i as u32));
            let vc = match &c.wake {
                Some(w) => vc_with_wake(&w.iter().map(String::as_str).collect::<Vec<_>>()),
                None => VoiceCommandSettings::default(),
            };
            let m = Matcher::new(&vc);
            // NAVI_VOICE_PROMPT_TEXT tries other wording for the prompt (the Matcher's own echo
            // check still knows only the shipped one).
            let prompt = std::env::var("NAVI_VOICE_PROMPT_TEXT").unwrap_or_else(|_| build_prompt(&vc.phrases("wake"), use_prompt));
            let mut checks: Vec<LiveCheck> = Vec::new();
            // (got, decision position, what the wake's take then did, when the wake fired, the listener's own figure)
            let (got, at, then, woke_at, after_ms) = match c.kind.as_str() {
                "idle" | "call" | "wake-take" => {
                    let mode = if c.kind == "call" { ListenMode::Call } else { ListenMode::Normal };
                    let idle = live_idle(&engine, &m, &padded, mode, &prompt).await;
                    checks.extend(idle.checks);
                    if idle.got == "wake" {
                        // As wake.rs: the take opens with the audio from the utterance's start up
                        // to the decision, and skips only what the idle listener judged.
                        let s = idle.start as usize;
                        let take = live_take(&engine, &m, &padded[s..], true, (idle.at - idle.start) as usize, Some(idle.heard_to.saturating_sub(idle.start))).await;
                        checks.extend(take.checks);
                        // What the app ends up doing: the take's action if it fired, else the take.
                        if take.got != "none" {
                            (take.got.clone(), take.at + idle.start, Some(format!("via take ({})", take.got)), Some(idle.at), take.after_ms)
                        } else if c.kind == "wake-take" {
                            ("none".to_string(), take.at + idle.start, Some("none".to_string()), Some(idle.at), None)
                        } else {
                            ("wake".to_string(), idle.at, Some("none".to_string()), Some(idle.at), idle.after_ms)
                        }
                    } else {
                        (idle.got, idle.at, None, None, idle.after_ms)
                    }
                }
                "take" => {
                    let take = live_take(&engine, &m, &padded, false, 0, None).await;
                    checks.extend(take.checks);
                    (take.got, take.at, None, None, take.after_ms)
                }
                other => panic!("unknown clip kind {other}"),
            };
            engine_ms.extend(checks.iter().map(|k| k.ms as i64));
            let fired = !matches!(got.as_str(), "ignore" | "none");
            let negative = matches!(c.expect.as_str(), "ignore" | "none");
            let take_quiet = then.as_deref().is_none_or(|t| t == "none");
            let correct = match c.expect.as_str() {
                "ignore" | "none" => !fired,
                "wake" | "dictation" => got == "wake" && take_quiet,
                e => got == e,
            };
            let span = speech_span(clean);
            let phrase_end = if c.expect == "dictation" {
                c.wake_only.as_ref().and_then(|w| wake_len.get(w)).zip(span).map(|(len, (s, _))| s + len)
            } else {
                span.map(|(_, e)| e)
            };
            let latency = phrase_end.filter(|_| correct && !negative).map(|e| ms_of(at as i64 - e as i64));
            let bubble = if c.kind == "wake-take" { woke_at.zip(first_run_end(clean)).map(|(w, e)| ms_of(w as i64 - e as i64)) } else { None };
            let shown: Vec<String> = checks.iter().map(|k| format!("{} {:.2}s {}ms {:?}", k.what, k.secs, k.ms, k.text)).collect();
            println!(
                "{n:>2} {:<5} r{:<4} {:<9} {:<20} got {:<20}{} {} latency {:>5} | listener {:>4} | bubble {:>4} | {}",
                c.voice,
                c.rate,
                c.kind,
                c.expect,
                got,
                then.as_ref().map_or(String::new(), |t| format!(" then {t}")),
                if correct { "ok   " } else { "WRONG" },
                latency.map_or("-".into(), |l| l.to_string()),
                after_ms.map_or("-".into(), |a| a.to_string()),
                bubble.map_or("-".into(), |b| b.to_string()),
                shown.join(" | ")
            );
            rows.push(Row { n: n.clone(), text: c.text.clone(), kind: c.kind.clone(), expect: c.expect.clone(), got, then, correct, latency });
        }

        // Per clip text: right in how many variants, and the latencies.
        println!("\nper clip:");
        let mut numbers: Vec<&str> = rows.iter().map(|r| r.n.as_str()).collect();
        numbers.dedup();
        for n in &numbers {
            let these: Vec<&Row> = rows.iter().filter(|r| r.n == *n).collect();
            let lat: Vec<i64> = these.iter().filter_map(|r| r.latency).collect();
            let gots: Vec<String> = these.iter().map(|r| format!("{}{}", r.got, r.then.as_ref().map_or(String::new(), |t| format!("+{t}")))).collect();
            println!(
                "SUMMARY {n} | {} | {} | expect {} | {}/{} right | got {} | latencies {:?} | median {}",
                these[0].text,
                these[0].kind,
                these[0].expect,
                these.iter().filter(|r| r.correct).count(),
                these.len(),
                gots.join(", "),
                lat,
                if lat.is_empty() { "-".to_string() } else { percentile(&lat, 0.5).to_string() }
            );
        }

        let num = |r: &Row| r.n.parse::<u32>().unwrap_or(0);
        type Member = fn(u32) -> bool;
        let groups: [(&str, Member); 7] = [
            ("wake only (1, 2)", |n| n == 1 || n == 2),
            ("idle commands (3-8, 19)", |n| (3..=8).contains(&n) || n == 19),
            ("in-take (22, 23, 27-29)", |n| n == 22 || n == 23 || (27..=29).contains(&n)),
            ("dictation from the name (9, 10, 31)", |n| n == 9 || n == 10 || n == 31),
            ("custom wake word Hey (32, 33)", |n| n == 32 || n == 33),
            ("stop watching (7, 38, 39)", |n| n == 7 || n == 38 || n == 39),
            ("short pause after the name (41, 42)", |n| n == 41 || n == 42),
        ];
        let mut too_slow = Vec::new();
        println!();
        for (name, member) in &groups {
            let these: Vec<&Row> = rows.iter().filter(|r| member(num(r))).collect();
            let lat: Vec<i64> = these.iter().filter_map(|r| r.latency).collect();
            let median = percentile(&lat, 0.5);
            println!(
                "{name}: {} of {} right, median {median} ms, 90th percentile {} ms (target 500 ms)",
                these.iter().filter(|r| r.correct).count(),
                these.len(),
                percentile(&lat, 0.9)
            );
            if !lat.is_empty() && median > 800 && !name.starts_with("dictation") {
                too_slow.push(format!("{name} median {median} ms"));
            }
        }
        println!("engine time per check: median {} ms, 90th percentile {} ms, {} checks", percentile(&engine_ms, 0.5), percentile(&engine_ms, 0.9), engine_ms.len());

        let negative = |r: &Row| matches!(r.expect.as_str(), "ignore" | "none");
        let quiet_or_wake = |r: &Row| matches!(r.got.as_str(), "ignore" | "none" | "wake");
        // Fired on ordinary speech, or a wake or dictation clip's take then cancelled, finished or ran a command.
        let false_triggers: Vec<&Row> = rows.iter().filter(|r| (negative(r) && !r.correct) || (matches!(r.expect.as_str(), "wake" | "dictation") && !quiet_or_wake(r))).collect();
        let wrong_action: Vec<&Row> = rows.iter().filter(|r| !negative(r) && !r.correct && !quiet_or_wake(r) && !matches!(r.expect.as_str(), "wake" | "dictation")).collect();
        let positives: Vec<&Row> = rows.iter().filter(|r| !negative(r)).collect();
        let right = positives.iter().filter(|r| r.correct).count();
        println!("positives right: {right} of {}", positives.len());
        for r in &false_triggers {
            println!("FALSE TRIGGER: {} {:?} ({}) gave {}{}", r.n, r.text, r.kind, r.got, r.then.as_ref().map_or(String::new(), |t| format!(" then {t}")));
        }
        for r in &wrong_action {
            println!("WRONG ACTION: {} {:?} ({}) expected {} gave {}", r.n, r.text, r.kind, r.expect, r.got);
        }
        let mut by_text: std::collections::BTreeMap<&str, (usize, usize)> = std::collections::BTreeMap::new();
        for r in &positives {
            let e = by_text.entry(r.n.as_str()).or_default();
            e.1 += 1;
            if r.correct {
                e.0 += 1;
            }
        }
        let weak: Vec<String> = by_text.iter().filter(|(_, (ok, n))| *n >= 4 && *ok < 3).map(|(t, (ok, n))| format!("{t} ({ok}/{n})")).collect();
        assert!(false_triggers.is_empty(), "{} clips triggered when they should not", false_triggers.len());
        assert!(wrong_action.is_empty(), "{} clips ran the wrong action", wrong_action.len());
        assert!(right * 10 >= positives.len() * 9, "only {right} of {} positive clips were right", positives.len());
        assert!(weak.is_empty(), "right in fewer than 3 of 4 variants: {weak:?}");
        assert!(too_slow.is_empty(), "slower than the 800 ms guard: {too_slow:?}");
    }
}
