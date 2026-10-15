//! Offline text rules applied to every take, before and after any AI step.
//!
//! OWNER: text agent.
//!
//! Every rule here is conservative: when a spoken word could be meant literally ("a dash of salt",
//! "the trial period", "colon cancer") it is left as spoken. A missed command is visible and easy to
//! fix; a wrongly deleted word changes what the user said. None of the rules touch code spans,
//! links, email addresses, paths or file names.

use crate::text::spans;
use crate::types::{DictEntry, Snippet};
use regex::Regex;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::LazyLock;

// =============================================================================================
// Spoken punctuation
// =============================================================================================

/// "full stop", "comma", "question mark", "exclamation mark", "new line", "new paragraph",
/// "open quote"/"close quote", "colon", "semicolon", "open bracket"/"close bracket", "dash",
/// "hyphen" (only as a spoken command between words), and "period" (US). Case-insensitive.
///
/// Also accepted: "exclamation point", "newline", "end quote", "unquote", "open/close paren",
/// "semi-colon", "em dash", "dot dot dot"/"ellipsis". A command is skipped when the word is clearly
/// used as a noun ("came to a full stop", "the trial period", "the Oxford comma", "a dash of salt",
/// "colon cancer", "a new line of products"). Punctuation the engine already put around a command
/// ("Hello, comma, how") is absorbed, and the word after a spoken full stop gets a capital.
pub fn spoken_punctuation(text: &str) -> String {
    let Some(re) = SPOKEN.as_ref() else {
        return text.to_string();
    };
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    let mut flags = CaseFlags::default();
    for m in re.find_iter(text) {
        if m.start() < cursor {
            continue;
        }
        let Some(kind) = classify_spoken(m.as_str()) else {
            continue;
        };
        if !is_spoken_command(text, m.start(), m.end(), kind) {
            continue;
        }
        push_segment(&mut out, &text[cursor..m.start()], &mut flags);
        cursor = apply_spoken(&mut out, text, m.end(), kind, &mut flags);
    }
    push_segment(&mut out, &text[cursor..], &mut flags);
    out.trim().to_string()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Spoken {
    FullStop,
    Period,
    Comma,
    Question,
    Exclamation,
    NewLine,
    NewParagraph,
    OpenQuote,
    CloseQuote,
    Colon,
    Semicolon,
    OpenBracket,
    CloseBracket,
    Dash,
    Hyphen,
    Ellipsis,
}

static SPOKEN: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)\b(?:full[ \t-]+stop|period|comma|question[ \t]+mark|exclamation[ \t]+(?:mark|point)",
        r"|new[ \t]+paragraph|new[ \t]+line|newline",
        r"|(?:open|begin|start)[ \t]+quotes?|(?:close|end)[ \t]+quotes?|unquote",
        r"|semi[ \t-]*colon|colon",
        r"|(?:open|left)[ \t]+(?:brackets?|parenthesis|parentheses|parens?)",
        r"|(?:close|end|right)[ \t]+(?:brackets?|parenthesis|parentheses|parens?)",
        r"|(?:em[ \t]+|en[ \t]+)?dash|hyphen|dot[ \t]+dot[ \t]+dot|ellipsis)\b"
    ))
    .ok()
});

fn classify_spoken(m: &str) -> Option<Spoken> {
    let lower = m.to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| c.is_whitespace() || c == '-').filter(|w| !w.is_empty()).collect();
    let first = *words.first()?;
    let last = *words.last()?;
    Some(match first {
        "full" => Spoken::FullStop,
        "period" => Spoken::Period,
        "comma" => Spoken::Comma,
        "question" => Spoken::Question,
        "exclamation" => Spoken::Exclamation,
        "new" if last == "paragraph" => Spoken::NewParagraph,
        "new" | "newline" => Spoken::NewLine,
        "unquote" => Spoken::CloseQuote,
        "semi" | "semicolon" => Spoken::Semicolon,
        "colon" => Spoken::Colon,
        "dash" | "em" | "en" => Spoken::Dash,
        "hyphen" => Spoken::Hyphen,
        "dot" | "ellipsis" => Spoken::Ellipsis,
        "open" | "begin" | "start" | "left" => {
            if last.starts_with("quote") {
                Spoken::OpenQuote
            } else {
                Spoken::OpenBracket
            }
        }
        "close" | "end" | "right" => {
            if last.starts_with("quote") {
                Spoken::CloseQuote
            } else {
                Spoken::CloseBracket
            }
        }
        _ => return None,
    })
}

const DETERMINERS: &[&str] = &[
    "a", "an", "the", "this", "that", "these", "those", "my", "your", "his", "her", "its", "our", "their", "no", "any",
    "each", "every", "another", "some", "which", "what", "whose", "extra", "missing", "single", "same",
];

const PERIOD_MODIFIERS: &[&str] = &[
    "trial", "grace", "time", "waiting", "notice", "cooling", "cooling-off", "probation", "probationary", "testing",
    "test", "transition", "transitional", "holding", "billing", "reporting", "rest", "sample", "study", "review",
    "payment", "free", "initial", "final", "short", "long", "brief", "extended", "first", "second", "third", "last",
    "next", "previous", "prior", "entire", "whole", "half", "quiet", "busy", "peak", "school", "exam", "exams",
    "revision", "historical", "victorian", "edwardian", "georgian", "elizabethan", "tudor", "medieval", "renaissance",
    "jurassic", "cretaceous", "triassic", "interwar", "war", "postwar", "post-war", "golden", "dark", "early", "late",
    "middle", "recovery", "recession", "election", "campaign", "accounting", "financial", "fiscal", "tax",
    "reference", "base", "baseline", "introductory", "honeymoon", "adjustment", "settling", "downtime", "blackout",
    "menstrual", "orbital", "rotation", "refractory", "latency", "incubation", "gestation", "retention", "vesting",
    "lockup", "lock-up", "evaluation", "assessment", "consultation", "implementation", "launch", "rollout", "beta",
    "pilot", "contract", "rental", "lease", "cycle", "sprint", "quarter", "month", "week", "year", "day", "hour",
    "minute", "return", "refund", "cancellation", "warranty", "guarantee", "summer", "winter", "spring", "autumn",
    "fall", "christmas", "holiday", "festive", "slow", "lean", "dry", "wet", "rainy", "growth", "boom", "crisis",
    "covid", "lockdown", "pandemic", "modern", "contemporary", "classical", "baroque", "romantic", "ancient",
    "colonial", "imperial", "edo", "meiji", "ming",
];

const COLON_BEFORE: &[&str] = &["sigmoid", "transverse", "ascending", "descending", "spastic", "irritable", "inflamed", "healthy"];
const COLON_AFTER: &[&str] = &["cancer", "polyp", "polyps", "health", "cleanse", "cleansing", "surgery", "wall", "lining", "screening", "removal"];
const DASH_AFTER: &[&str] = &[
    "of", "off", "out", "away", "to", "for", "through", "into", "across", "back", "around", "over", "down", "up", "home",
    "cam", "cams", "board", "boards", "app", "button", "dash",
];

/// Up to `n` words directly before byte `pos` (nearest first, lower case), stopping at punctuation
/// or a line break.
fn words_before(text: &str, pos: usize, n: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = text[..pos].chars().rev().peekable();
    while out.len() < n {
        while chars.peek().is_some_and(|&c| c == ' ' || c == '\t') {
            chars.next();
        }
        let mut word = Vec::new();
        while let Some(&c) = chars.peek() {
            if c.is_alphanumeric() || c == '\'' || c == '’' || c == '-' {
                word.push(c);
                chars.next();
            } else {
                break;
            }
        }
        if word.is_empty() {
            break;
        }
        out.push(word.into_iter().rev().collect::<String>().to_lowercase());
    }
    out
}

/// The word right after byte `pos` when only spaces separate them (original case).
fn word_after(text: &str, pos: usize) -> Option<String> {
    let rest = text[pos..].trim_start_matches([' ', '\t']);
    let w: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '\'' || *c == '’').collect();
    (!w.is_empty()).then_some(w)
}

fn is_spoken_command(text: &str, start: usize, end: usize, kind: Spoken) -> bool {
    let before = words_before(text, start, 3);
    let prev = before.first().map(String::as_str);
    let next = word_after(text, end);
    let next_lower = next.as_deref().map(str::to_lowercase);
    let has_before = !text[..start].trim().is_empty();
    let det = prev.is_some_and(|p| DETERMINERS.contains(&p));
    let prev_in = |list: &[&str]| prev.is_some_and(|p| list.contains(&p));
    let next_in = |list: &[&str]| next_lower.as_deref().is_some_and(|n| list.contains(&n));
    // Joined to the next word by a hyphen, it is part of a compound ("comma-separated"), not a command.
    let mut after = text[end..].chars();
    if after.next() == Some('-') && after.next().is_some_and(char::is_alphanumeric) {
        return false;
    }
    match kind {
        Spoken::FullStop | Spoken::Semicolon | Spoken::Ellipsis => has_before && !det,
        Spoken::Question | Spoken::Exclamation => {
            has_before && !det && !prev_in(&["big", "huge", "real", "giant", "massive", "little"])
        }
        Spoken::Comma => {
            has_before
                && !det
                && !prev_in(&["oxford", "serial", "inverted"])
                && !next_in(&["splice", "splices", "separated", "delimited", "seperated"])
        }
        Spoken::Period => {
            has_before
                && !before.iter().any(|w| DETERMINERS.contains(&w.as_str()))
                && !prev_in(PERIOD_MODIFIERS)
                && match &next {
                    None => true,
                    Some(n) => n.chars().next().is_some_and(char::is_uppercase) || next_in(&["new", "newline"]),
                }
        }
        Spoken::Colon => has_before && !det && !prev_in(COLON_BEFORE) && !next_in(COLON_AFTER),
        Spoken::NewLine => !det && !next_in(&["of", "manager", "managers", "item", "items", "up"]),
        Spoken::NewParagraph | Spoken::OpenQuote | Spoken::OpenBracket => !det,
        Spoken::CloseQuote | Spoken::CloseBracket => has_before && !det,
        Spoken::Dash => {
            // The engine often sets a spoken "dash" off with commas ("genius, dash, honestly").
            let next_soft = word_after(text, skip_chars(text, end, &[' ', '\t', ',', ';', ':']));
            let next_soft_lower = next_soft.as_deref().map(str::to_lowercase);
            has_before
                && !det
                && next_soft.is_some()
                && !next_soft_lower.as_deref().is_some_and(|n| DASH_AFTER.contains(&n))
                // "dash dash watch" is a command-line flag being read out: leave it as said.
                && !prev_in(&["mad", "quick", "last", "final", "little", "short", "hyphen", "dash", "double"])
        }
        Spoken::Hyphen => prev.is_some() && next.is_some() && !det,
    }
}

const SOFT: &[char] = &['.', ',', ';', ':', '!', '?'];

/// Writes the punctuation for `kind` into `out`, absorbing punctuation the engine already added,
/// and returns the byte position in `text` to continue from.
fn apply_spoken(out: &mut String, text: &str, end: usize, kind: Spoken, flags: &mut CaseFlags) -> usize {
    match kind {
        Spoken::FullStop | Spoken::Period | Spoken::Question | Spoken::Exclamation => {
            let mark = match kind {
                Spoken::Question => '?',
                Spoken::Exclamation => '!',
                _ => '.',
            };
            trim_trailing(out, SOFT);
            out.push(mark);
            let i = skip_soft(text, end);
            push_gap(out, text, i);
            flags.cap_next = true;
            flags.lower_next = false;
            i
        }
        Spoken::Comma | Spoken::Semicolon | Spoken::Colon => {
            let mark = match kind {
                Spoken::Semicolon => ';',
                Spoken::Colon => ':',
                _ => ',',
            };
            trim_trailing(out, SOFT);
            out.push(mark);
            let i = skip_soft(text, end);
            push_gap(out, text, i);
            flags.lower_next = true;
            flags.cap_next = false;
            i
        }
        Spoken::Ellipsis => {
            trim_trailing(out, SOFT);
            out.push_str("...");
            let i = skip_soft(text, end);
            push_gap(out, text, i);
            i
        }
        Spoken::NewLine | Spoken::NewParagraph => {
            trim_trailing(out, &[]);
            out.push_str(if kind == Spoken::NewLine { "\n" } else { "\n\n" });
            flags.cap_next = true;
            flags.lower_next = false;
            skip_chars(text, end, &[' ', '\t', '.', ',', ';', ':'])
        }
        Spoken::OpenQuote | Spoken::OpenBracket => {
            if !out.is_empty() && !out.ends_with([' ', '\n', '(', '"']) {
                out.push(' ');
            }
            out.push(if kind == Spoken::OpenQuote { '"' } else { '(' });
            skip_chars(text, end, &[' ', '\t', ',', ':'])
        }
        Spoken::CloseQuote | Spoken::CloseBracket => {
            trim_trailing(out, &[',', ';', ':']);
            out.push(if kind == Spoken::CloseQuote { '"' } else { ')' });
            let i = skip_chars(text, end, &[' ', '\t']);
            if text[i..].chars().next().is_some_and(char::is_alphanumeric) {
                out.push(' ');
            }
            i
        }
        Spoken::Dash => {
            trim_trailing(out, &[',', ';', ':']);
            out.push_str(" - ");
            flags.lower_next = true;
            skip_chars(text, end, &[' ', '\t', ',', ';', ':'])
        }
        Spoken::Hyphen => {
            trim_trailing(out, &[]);
            out.push('-');
            skip_chars(text, end, &[' ', '\t'])
        }
    }
}

/// Drops trailing spaces, then trailing characters from `set`, then spaces again.
fn trim_trailing(out: &mut String, set: &[char]) {
    loop {
        match out.chars().last() {
            Some(c) if c == ' ' || c == '\t' || set.contains(&c) => {
                out.pop();
            }
            _ => break,
        }
    }
}

fn skip_chars(text: &str, mut i: usize, set: &[char]) -> usize {
    while let Some(c) = text[i..].chars().next() {
        if set.contains(&c) {
            i += c.len_utf8();
        } else {
            break;
        }
    }
    i
}

fn skip_soft(text: &str, i: usize) -> usize {
    let i = skip_chars(text, i, &[' ', '\t']);
    let i = skip_chars(text, i, SOFT);
    skip_chars(text, i, &[' ', '\t'])
}

fn push_gap(out: &mut String, text: &str, i: usize) {
    if let Some(c) = text[i..].chars().next() {
        if !matches!(c, '\n' | '\r' | ')' | '.' | ',' | ';' | ':' | '!' | '?') {
            out.push(' ');
        }
    }
}

#[derive(Default)]
struct CaseFlags {
    /// Capitalise the next word (after a spoken full stop or new line).
    cap_next: bool,
    /// Lower-case the next word if the engine capitalised a common word after its own full stop
    /// that we turned into a comma ("Hello. Comma, How are you" -> "Hello, how are you").
    lower_next: bool,
}

/// Words that are never proper nouns, so they can safely lose a capital the engine gave them.
const NEVER_PROPER: &[&str] = &[
    "how", "what", "when", "where", "why", "who", "which", "the", "a", "an", "and", "but", "or", "so", "it", "it's", "its",
    "we", "we're", "we'll", "you", "you're", "they", "they're", "he", "she", "this", "that", "these", "those", "there",
    "here", "is", "are", "was", "were", "be", "can", "could", "would", "should", "please", "thanks", "thank", "if",
    "then", "because", "also", "just", "let's", "my", "your", "our", "their", "his", "her", "in", "on", "at", "to",
    "for", "with", "from", "of", "as", "do", "did", "does", "have", "has", "had", "not", "no", "yes", "maybe", "see",
    "sorry", "some", "all", "any", "now", "today", "tomorrow", "yesterday", "though", "although", "however", "plus",
    "like", "even", "only", "still", "really", "very", "too", "well",
];

/// Lower-cases the first word when it is a common word that is never a name, for text that carries
/// on a sentence already in the field ("...and" + "Then we left" -> "then we left"). Names, "I"
/// and anything unusual keep their capital.
pub(crate) fn decapitalise_common_start(text: &str) -> String {
    let mut flags = CaseFlags { cap_next: false, lower_next: true };
    let mut out = String::with_capacity(text.len());
    push_segment(&mut out, text, &mut flags);
    out
}

fn push_segment(out: &mut String, seg: &str, flags: &mut CaseFlags) {
    if !flags.cap_next && !flags.lower_next {
        out.push_str(seg);
        return;
    }
    let mut decided = false;
    for (i, c) in seg.char_indices() {
        if !decided && c.is_alphanumeric() {
            decided = true;
            let word: String = seg[i..].chars().take_while(|c| c.is_alphanumeric() || *c == '\'' || *c == '’').collect();
            let cap = flags.cap_next && c.is_lowercase() && word.chars().all(|c| !c.is_uppercase());
            let lower = flags.lower_next
                && c.is_uppercase()
                && word.chars().skip(1).all(|c| !c.is_uppercase())
                && NEVER_PROPER.contains(&word.to_lowercase().as_str());
            flags.cap_next = false;
            flags.lower_next = false;
            if cap {
                out.extend(c.to_uppercase());
                continue;
            }
            if lower {
                out.extend(c.to_lowercase());
                continue;
            }
        }
        out.push(c);
    }
}

// =============================================================================================
// Fillers and repetitions
// =============================================================================================

/// Drops um, uh, er, erm, hmm, mm, "you know" and "I mean" when used as fillers, and immediate
/// word repetitions ("the the"). Keeps meaning words ("like" is only dropped as ", like,").
///
/// "you know" and "I mean" only go when set off by commas or sentence edges ("It was, you know,
/// hard"), never in "Do you know the way?" or "I mean it". Repetitions are only removed for short
/// function words, so "had had", "that that", "very very" and "bye bye" stay. "mm" after a number
/// (millimetres) and capitals such as "ER" stay. "err" only goes when set off by punctuation.
pub fn remove_fillers(text: &str) -> String {
    let mut toks = tokenise(text);
    let n = toks.len();
    let mut del = vec![false; n];
    let mut cap = vec![false; n];

    for i in 0..n {
        if toks[i].kind == Kind::Word && !toks[i].protected && !del[i] && is_single_filler(&toks, &del, i) {
            drop_filler(&toks, &mut del, &mut cap, i, i);
        }
    }

    for i in 0..n {
        if toks[i].kind != Kind::Word || toks[i].protected || del[i] {
            continue;
        }
        let w = toks[i].text.to_lowercase();
        if w == "like" {
            if filler_set_off(&toks, &del, i, i, Phrase::Like) {
                drop_filler(&toks, &mut del, &mut cap, i, i);
            }
            continue;
        }
        if w != "you" && w != "i" {
            continue;
        }
        let Some(j) = adjacent_word(&toks, &del, i) else {
            continue;
        };
        let w2 = toks[j].text.to_lowercase();
        let phrase = match (w.as_str(), w2.as_str()) {
            ("you", "know") => Phrase::YouKnow,
            ("i", "mean") => Phrase::IMean,
            _ => continue,
        };
        if phrase == Phrase::YouKnow && sentence_has(&toks, i, "if you know") {
            continue;
        }
        if !filler_set_off(&toks, &del, i, j, phrase) {
            continue;
        }
        // "It was hard, you know?" -> "It was hard." (the question mark belonged to the tag).
        let right = next_sig(&toks, &del, j);
        if phrase == Phrase::YouKnow && ctx(&toks, right) == Ctx::Question {
            if let Some(q) = right {
                toks[q].text = ".".to_string();
            }
        }
        drop_filler(&toks, &mut del, &mut cap, i, j);
    }

    remove_repetitions(&mut toks, &mut del, &mut cap);
    rebuild(&toks, &del, &cap)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Word,
    Space,
    Newline,
    Punct,
}

#[derive(Clone, Debug)]
struct Tok {
    kind: Kind,
    text: String,
    protected: bool,
}

fn tokenise(text: &str) -> Vec<Tok> {
    let protected = spans::protected_ranges(text);
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let end_of = |k: usize| chars.get(k).map_or(text.len(), |&(p, _)| p);
    while i < chars.len() {
        let (start, c) = chars[i];
        let kind = if c == '\n' || c == '\r' {
            while i < chars.len() && (chars[i].1 == '\n' || chars[i].1 == '\r') {
                i += 1;
            }
            Kind::Newline
        } else if c.is_whitespace() {
            while i < chars.len() && chars[i].1.is_whitespace() && chars[i].1 != '\n' && chars[i].1 != '\r' {
                i += 1;
            }
            Kind::Space
        } else if c.is_alphanumeric() {
            while i < chars.len() {
                let ch = chars[i].1;
                let joins = matches!(ch, '\'' | '’' | '-') && chars.get(i + 1).is_some_and(|&(_, n)| n.is_alphanumeric());
                if ch.is_alphanumeric() || (joins && i > 0) {
                    i += 1;
                } else {
                    break;
                }
            }
            Kind::Word
        } else if c == '.' && chars.get(i + 1).is_some_and(|&(_, n)| n == '.') {
            while i < chars.len() && chars[i].1 == '.' {
                i += 1;
            }
            Kind::Punct
        } else {
            i += 1;
            Kind::Punct
        };
        let end = end_of(i);
        out.push(Tok { kind, text: text[start..end].to_string(), protected: spans::overlaps(&protected, &(start..end)) });
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ctx {
    /// Start or end of the text, or a line break.
    Edge,
    Comma,
    Stop,
    Question,
    Ellipsis,
    Dash,
    Word,
    Other,
}

fn ctx(toks: &[Tok], idx: Option<usize>) -> Ctx {
    let Some(k) = idx else {
        return Ctx::Edge;
    };
    let t = &toks[k];
    match t.kind {
        Kind::Newline => Ctx::Edge,
        Kind::Word => Ctx::Word,
        Kind::Space => Ctx::Other,
        Kind::Punct => match t.text.as_str() {
            "," => Ctx::Comma,
            "." | "!" => Ctx::Stop,
            "?" => Ctx::Question,
            "…" => Ctx::Ellipsis,
            s if s.starts_with("..") => Ctx::Ellipsis,
            "-" | "–" | "—" => Ctx::Dash,
            _ => Ctx::Other,
        },
    }
}

fn prev_sig(toks: &[Tok], del: &[bool], i: usize) -> Option<usize> {
    (0..i).rev().find(|&k| !del[k] && toks[k].kind != Kind::Space)
}

fn next_sig(toks: &[Tok], del: &[bool], j: usize) -> Option<usize> {
    (j + 1..toks.len()).find(|&k| !del[k] && toks[k].kind != Kind::Space)
}

/// The next word when only spaces separate it from token `i`.
fn adjacent_word(toks: &[Tok], del: &[bool], i: usize) -> Option<usize> {
    let mut k = i + 1;
    while k < toks.len() && (toks[k].kind == Kind::Space || del[k]) {
        k += 1;
    }
    (k < toks.len() && k > i + 1 && toks[k].kind == Kind::Word && !toks[k].protected).then_some(k)
}

fn is_single_filler(toks: &[Tok], del: &[bool], i: usize) -> bool {
    let w = &toks[i].text;
    let lw = w.to_lowercase();
    if lw == "err" {
        let l = ctx(toks, prev_sig(toks, del, i));
        let r = ctx(toks, next_sig(toks, del, i));
        return matches!(l, Ctx::Edge | Ctx::Stop | Ctx::Question | Ctx::Comma)
            && matches!(r, Ctx::Comma | Ctx::Stop | Ctx::Edge | Ctx::Ellipsis | Ctx::Dash);
    }
    let filler = matches!(
        lw.as_str(),
        "um" | "umm" | "ummm" | "uh" | "uhh" | "uhhh" | "uhm" | "er" | "erm" | "errm" | "hmm" | "hmmm" | "hm" | "mm" | "mmm"
    );
    if !filler {
        return false;
    }
    if w.chars().count() <= 2 && w.chars().all(char::is_uppercase) {
        return false; // "ER", "UM", "MM" are abbreviations
    }
    if lw.starts_with("mm") {
        if let Some(p) = prev_sig(toks, del, i) {
            if toks[p].kind == Kind::Word && toks[p].text.chars().any(|c| c.is_ascii_digit()) {
                return false; // "5 mm"
            }
        }
    }
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phrase {
    YouKnow,
    IMean,
    Like,
}

fn filler_set_off(toks: &[Tok], del: &[bool], i: usize, j: usize, phrase: Phrase) -> bool {
    let l = ctx(toks, prev_sig(toks, del, i));
    let r = ctx(toks, next_sig(toks, del, j));
    let l_open = matches!(l, Ctx::Edge | Ctx::Stop | Ctx::Question);
    match phrase {
        // "You know." or "You know?" on its own is a real sentence: keep it.
        Phrase::YouKnow => {
            (l_open || l == Ctx::Comma)
                && matches!(r, Ctx::Comma | Ctx::Stop | Ctx::Question | Ctx::Edge)
                && !(l_open && matches!(r, Ctx::Stop | Ctx::Question | Ctx::Edge))
        }
        Phrase::IMean => ((l_open || l == Ctx::Comma) && r == Ctx::Comma) || (l == Ctx::Comma && matches!(r, Ctx::Stop | Ctx::Edge)),
        Phrase::Like => l == Ctx::Comma && r == Ctx::Comma,
    }
}

/// Does the sentence around token `i` contain `phrase` (lower case, single spaces)?
fn sentence_has(toks: &[Tok], i: usize, phrase: &str) -> bool {
    let is_end = |t: &Tok| t.kind == Kind::Newline || (t.kind == Kind::Punct && matches!(t.text.as_str(), "." | "!" | "?"));
    let start = (0..i).rev().find(|&k| is_end(&toks[k])).map_or(0, |k| k + 1);
    let end = (i..toks.len()).find(|&k| is_end(&toks[k])).unwrap_or(toks.len());
    let words: Vec<String> = toks[start..end].iter().filter(|t| t.kind == Kind::Word).map(|t| t.text.to_lowercase()).collect();
    format!(" {} ", words.join(" ")).contains(&format!(" {phrase} "))
}

/// Words that open a sentence and keep their comma ("Yes, I agree", "So, what now?").
const INTRO_WORDS: &[&str] = &[
    "yes", "yeah", "yep", "no", "nope", "so", "well", "okay", "ok", "right", "now", "anyway", "actually", "honestly",
    "basically", "look", "listen", "oh", "ah", "hey", "hi", "hello", "sure", "also", "however", "still", "plus",
    "then", "first", "firstly", "second", "secondly", "finally", "again", "alright", "fine", "thanks", "sorry",
    "obviously", "personally", "luckily", "sadly", "frankly", "anyhow", "besides", "meanwhile", "otherwise",
];

/// Is the text between the previous sentence edge and token `comma` a single introductory word?
fn introductory(toks: &[Tok], del: &[bool], comma: Option<usize>) -> bool {
    let Some(c) = comma else {
        return false;
    };
    let mut words = Vec::new();
    for k in (0..c).rev() {
        if del[k] {
            continue;
        }
        match ctx(toks, Some(k)) {
            Ctx::Edge | Ctx::Stop | Ctx::Question => break,
            Ctx::Word => words.push(k),
            Ctx::Comma => return false,
            _ => {}
        }
    }
    words.len() == 1 && INTRO_WORDS.contains(&toks[words[0]].text.to_lowercase().as_str())
}

fn mark(del: &mut [bool], idx: Option<usize>) {
    if let Some(k) = idx {
        del[k] = true;
    }
}

/// Deletes tokens i..=j and tidies the commas that only existed to set them off.
fn drop_filler(toks: &[Tok], del: &mut [bool], cap: &mut [bool], i: usize, j: usize) {
    let first_cap = toks[i].text.chars().next().is_some_and(char::is_uppercase);
    for d in del.iter_mut().take(j + 1).skip(i) {
        *d = true;
    }
    let left = prev_sig(toks, del, i);
    let mut right = next_sig(toks, del, j);
    if matches!(ctx(toks, right), Ctx::Ellipsis | Ctx::Dash) {
        mark(del, right);
        right = next_sig(toks, del, j);
    }
    let (l, r) = (ctx(toks, left), ctx(toks, right));
    match (l, r) {
        (Ctx::Edge | Ctx::Stop | Ctx::Question, Ctx::Comma) => mark(del, right),
        // The filler was a whole sentence ("Um." / "Hmm."): its full stop goes too.
        (Ctx::Edge | Ctx::Stop | Ctx::Question, Ctx::Stop) => mark(del, right),
        (Ctx::Comma, Ctx::Comma) => {
            mark(del, right);
            if !introductory(toks, del, left) {
                mark(del, left);
            }
        }
        (Ctx::Comma, Ctx::Edge | Ctx::Stop | Ctx::Question) => mark(del, left),
        (Ctx::Word, Ctx::Comma) => mark(del, right),
        (Ctx::Comma, Ctx::Word) if !introductory(toks, del, left) => mark(del, left),
        _ => {}
    }
    if matches!(l, Ctx::Edge | Ctx::Stop | Ctx::Question) && first_cap {
        if let Some(k) = (j + 1..toks.len()).find(|&k| !del[k] && toks[k].kind == Kind::Word) {
            cap[k] = true;
        }
    }
}

/// Short function words that are almost never doubled on purpose.
const STUTTER: &[&str] = &[
    "the", "a", "an", "i", "i'm", "i've", "i'll", "i'd", "to", "of", "and", "in", "on", "at", "for", "with", "it", "it's",
    "we", "we're", "you", "he", "she", "they", "my", "our", "your", "but", "or", "if", "this", "be", "are", "will",
    "can", "just", "me", "his", "her", "their", "its", "as", "by", "from",
];

fn remove_repetitions(toks: &mut [Tok], del: &mut [bool], cap: &mut [bool]) {
    for i in 0..toks.len() {
        if toks[i].kind != Kind::Word || toks[i].protected || del[i] {
            continue;
        }
        // "I-I think", "the-the": a hyphenated stutter collapses to one word.
        let parts: Vec<String> = toks[i].text.split('-').map(str::to_string).collect();
        if parts.len() >= 2 {
            let first = parts[0].to_lowercase();
            if STUTTER.contains(&first.as_str()) && parts.iter().all(|p| p.to_lowercase() == first) {
                toks[i].text = parts[0].clone();
            }
        }
        // Next word, separated by spaces and at most one comma or hyphen ("I, I think", "I- I").
        let mut k = i + 1;
        let mut between = Vec::new();
        let mut punct = 0;
        while k < toks.len() {
            if del[k] {
                k += 1;
                continue;
            }
            match toks[k].kind {
                Kind::Space => between.push(k),
                Kind::Punct if (toks[k].text == "," || toks[k].text == "-") && punct == 0 => {
                    punct += 1;
                    between.push(k);
                }
                _ => break,
            }
            k += 1;
        }
        if k >= toks.len() || toks[k].kind != Kind::Word || toks[k].protected || between.is_empty() {
            continue;
        }
        let a = toks[i].text.to_lowercase();
        if a == toks[k].text.to_lowercase() && STUTTER.contains(&a.as_str()) {
            del[i] = true;
            for b in between {
                del[b] = true;
            }
            let i_cap = toks[i].text.chars().next().is_some_and(char::is_uppercase);
            if i_cap && toks[k].text.chars().next().is_some_and(char::is_lowercase) {
                cap[k] = true;
            }
            if cap[i] {
                cap[k] = true;
            }
        }
    }
}

fn rebuild(toks: &[Tok], del: &[bool], cap: &[bool]) -> String {
    let mut s = String::new();
    for (k, t) in toks.iter().enumerate() {
        if del[k] {
            continue;
        }
        match t.kind {
            Kind::Space => s.push(' '),
            Kind::Word if cap[k] => s.push_str(&capitalise_first(&t.text)),
            _ => s.push_str(&t.text),
        }
    }
    normalise_spaces(&s)
}

/// Single spaces, no space before closing punctuation, no stray comma at a line start, trimmed.
fn normalise_spaces(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (k, &c) in chars.iter().enumerate() {
        if c == ' ' {
            if out.is_empty() || out.ends_with(' ') || out.ends_with('\n') {
                continue;
            }
            match chars.get(k + 1) {
                None => continue,
                Some(',' | '.' | ';' | ':' | '!' | '?' | ')' | '…' | '\n' | ' ') => continue,
                _ => {}
            }
        }
        if matches!(c, ',' | ';') && (out.is_empty() || out.ends_with('\n')) {
            continue;
        }
        if c == '\n' {
            while out.ends_with(' ') {
                out.pop();
            }
        }
        out.push(c);
    }
    out.trim().to_string()
}

fn capitalise_first(w: &str) -> String {
    let mut c = w.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

// =============================================================================================
// Tidy
// =============================================================================================

/// Spacing around punctuation, a capital at each sentence start, " i " -> " I ", collapse spaces,
/// trim. Adds a final full stop to a multi-word sentence that has no ending punctuation.
///
/// Never capitalises after "e.g.", "Mr." and similar, or after an ellipsis; never adds a full stop
/// after a link, path or list item; leaves fenced code blocks exactly as they are.
pub fn tidy(text: &str) -> String {
    let spaced = tidy_spacing(text);
    if spaced.is_empty() {
        return spaced;
    }
    let capped = capitalise_sentences(&spaced);
    let with_i = fix_pronoun_i(&capped);
    add_final_full_stop(&with_i)
}

/// The spacing part of [`tidy`] only (used by Raw mode): line endings, collapsed spaces, blank
/// lines, no space before , . ; : ! ? ), a space after , and ;, doubled punctuation collapsed.
/// No capitals and no final full stop.
pub fn tidy_spacing(text: &str) -> String {
    let collapsed = collapse_whitespace(text);
    let protected = spans::protected_ranges(&collapsed);
    let spaced = fix_punct_spacing(&collapsed, &protected);
    let protected = spans::protected_ranges(&spaced);
    collapse_punct(&spaced, &protected).trim().to_string()
}

fn collapse_whitespace(text: &str) -> String {
    let t = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<String> = Vec::new();
    let mut in_fence = false;
    let mut blank_run = 0;
    for line in t.split('\n') {
        let trimmed = line.trim();
        let is_fence = trimmed.starts_with("```");
        if in_fence || is_fence {
            lines.push(line.trim_end().to_string());
            let opens_and_closes = trimmed.len() >= 6 && trimmed.ends_with("```");
            if is_fence && !opens_and_closes {
                in_fence = !in_fence;
            }
            blank_run = 0;
            continue;
        }
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        lines.push(collapsed);
    }
    lines.join("\n").trim().to_string()
}

fn fix_punct_spacing(s: &str, protected: &[Range<usize>]) -> String {
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let mut out = String::with_capacity(s.len() + 8);
    for k in 0..chars.len() {
        let (pos, c) = chars[k];
        let prot = spans::inside(protected, pos);
        if c == ' ' && !prot {
            if let Some(&(npos, n)) = chars.get(k + 1) {
                if matches!(n, ',' | '.' | ';' | ':' | '!' | '?' | ')' | '…') && !spans::inside(protected, npos) {
                    continue;
                }
            }
            if out.ends_with('(') {
                continue;
            }
        }
        out.push(c);
        if !prot && matches!(c, ',' | ';') {
            if let Some(&(npos, n)) = chars.get(k + 1) {
                if n.is_alphabetic() && !spans::inside(protected, npos) {
                    out.push(' ');
                }
            }
        }
    }
    out
}

/// ",," -> ",", ",." -> ".", ".," -> ".", "?." -> "?", ".." -> "." (an ellipsis "..." is kept).
fn collapse_punct(s: &str, protected: &[Range<usize>]) -> String {
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let mut out = String::with_capacity(s.len());
    let mut k = 0;
    while k < chars.len() {
        let (pos, c) = chars[k];
        if spans::inside(protected, pos) {
            out.push(c);
            k += 1;
            continue;
        }
        let next = chars.get(k + 1).map(|&(_, n)| n);
        if c == '.' {
            let mut run = 1;
            while chars.get(k + run).is_some_and(|&(_, n)| n == '.') {
                run += 1;
            }
            if run >= 3 {
                out.push_str(&".".repeat(run));
                k += run;
                continue;
            }
            out.push('.');
            k += run;
            while chars.get(k).is_some_and(|&(_, n)| matches!(n, ',' | ';' | ':')) {
                k += 1;
            }
            continue;
        }
        if matches!(c, ',' | ';' | ':') && next.is_some_and(|n| matches!(n, '.' | '!' | '?' | ',' | ';' | ':')) {
            k += 1;
            continue;
        }
        if matches!(c, '!' | '?') && next.is_some_and(|n| matches!(n, ',' | ';' | ':' | '.')) {
            out.push(c);
            k += 2;
            continue;
        }
        out.push(c);
        k += 1;
    }
    out
}

const ABBREVIATIONS: &[&str] = &[
    "etc", "vs", "approx", "incl", "mr", "mrs", "ms", "dr", "prof", "st", "fig", "cf", "inc", "ltd", "jr", "sr", "dept",
    "misc", "ref", "vol", "nb", "ext", "tel",
];

fn capitalise_sentences(s: &str) -> String {
    let protected = spans::protected_ranges(s);
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let mut out = String::with_capacity(s.len());
    let mut want = true;
    for k in 0..chars.len() {
        let (pos, c) = chars[k];
        if want {
            if c.is_alphabetic() {
                want = false;
                if !spans::inside(&protected, pos) && c.is_lowercase() && word_all_lower(&chars, k) {
                    out.extend(c.to_uppercase());
                    continue;
                }
            } else if c.is_numeric() {
                want = false;
            }
        }
        out.push(c);
        match c {
            '\n' => want = true,
            '.' | '!' | '?' if ends_sentence(s, &chars, k) => want = true,
            _ => {}
        }
    }
    out
}

fn word_all_lower(chars: &[(usize, char)], k: usize) -> bool {
    chars[k..].iter().map(|&(_, c)| c).take_while(|c| c.is_alphabetic() || *c == '\'' || *c == '’').all(|c| !c.is_uppercase())
}

fn ends_sentence(s: &str, chars: &[(usize, char)], k: usize) -> bool {
    let (pos, c) = chars[k];
    let mut j = k + 1;
    while chars.get(j).is_some_and(|&(_, n)| matches!(n, '"' | '\'' | '”' | '’' | ')')) {
        j += 1;
    }
    match chars.get(j) {
        None => {}
        Some(&(_, n)) if n.is_whitespace() => {}
        _ => return false,
    }
    if c != '.' {
        return true;
    }
    if k > 0 && chars[k - 1].1 == '.' {
        return false; // ellipsis: the thought carries on
    }
    let start = s[..pos].char_indices().rev().find(|(_, c)| c.is_whitespace()).map_or(0, |(i, c)| i + c.len_utf8());
    let chunk = &s[start..pos];
    if chunk.contains('.') {
        return false; // e.g. i.e. U.S.
    }
    let word = chunk.trim_start_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
    !ABBREVIATIONS.contains(&word.as_str())
}

fn fix_pronoun_i(s: &str) -> String {
    let protected = spans::protected_ranges(s);
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let mut out = String::with_capacity(s.len());
    for k in 0..chars.len() {
        let (pos, c) = chars[k];
        if c == 'i' && !spans::inside(&protected, pos) && standalone_i(&chars, k) {
            out.push('I');
        } else {
            out.push(c);
        }
    }
    out
}

fn standalone_i(chars: &[(usize, char)], k: usize) -> bool {
    let prev = k.checked_sub(1).map(|j| chars[j].1);
    if prev.is_some_and(|p| p.is_alphanumeric() || matches!(p, '.' | '_' | '-' | '\'' | '’' | '/' | '\\' | '@' | '#' | '$')) {
        return false;
    }
    match chars.get(k + 1).map(|&(_, n)| n) {
        None => true,
        Some(n) if n.is_whitespace() => true,
        Some(',' | '!' | '?' | ';' | ':' | ')' | '"' | '”') => true,
        Some('.') => !chars.get(k + 2).is_some_and(|&(_, n)| n.is_alphanumeric()),
        Some('\'' | '’') => {
            let rest: String =
                chars.get(k + 2..).unwrap_or(&[]).iter().map(|&(_, c)| c).take_while(|c| c.is_alphabetic()).collect::<String>().to_lowercase();
            matches!(rest.as_str(), "m" | "ve" | "ll" | "d")
        }
        _ => false,
    }
}

fn add_final_full_stop(s: &str) -> String {
    let t = s.trim_end();
    if t.split_whitespace().count() < 2 {
        return t.to_string();
    }
    let mut lines = t.rsplit('\n');
    let last_line = lines.next().unwrap_or(t).trim();
    if is_list_line(last_line) || last_line.starts_with('#') || last_line.starts_with("```") {
        return t.to_string();
    }
    // A sign-off ("Kind regards,\nJane Doe") or a lone word on the last line is not a sentence.
    if let Some(prev_line) = lines.next() {
        let last_words: Vec<&str> = last_line.split_whitespace().collect();
        let looks_like_name = last_words.len() <= 3 && last_words.iter().all(|w| w.chars().next().is_some_and(char::is_uppercase));
        let after_comma_line = prev_line.trim_end().ends_with(',') && last_words.len() <= 3;
        if last_words.len() < 2 || after_comma_line || looks_like_name {
            return t.to_string();
        }
    }
    let last_chunk = t.split_whitespace().last().unwrap_or("");
    if spans::is_machine_chunk(last_chunk) {
        return t.to_string();
    }
    let mut rev = t.chars().rev();
    let needs = match rev.next() {
        Some(c) if c.is_alphanumeric() => true,
        Some('"' | '\'' | '”' | '’' | ')') => rev.next().is_some_and(char::is_alphanumeric),
        _ => false,
    };
    if needs {
        format!("{t}.")
    } else {
        t.to_string()
    }
}

fn is_list_line(line: &str) -> bool {
    let l = line.trim_start();
    if l.starts_with("- ") || l.starts_with("* ") || l.starts_with("• ") {
        return true;
    }
    let digits = l.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && (l[digits..].starts_with(". ") || l[digits..].starts_with(") "))
}

// =============================================================================================
// Dashes
// =============================================================================================

/// Em/en dashes used as punctuation (" — ", " – ", "--") become ", " (or ". " when they join two
/// full sentences). Hyphens inside words and number ranges (2–3) are kept, ranges become "2-3".
///
/// Also: a pair of dashes around an aside becomes a pair of commas; an unspaced en dash between
/// words becomes a hyphen ("Monday–Friday" -> "Monday-Friday"); a dash used as a list bullet
/// becomes "- "; a dangling dash at the end of a line is dropped. "--help" style flags, "---" rules
/// and code are left alone.
pub fn no_em_dashes(text: &str) -> String {
    let protected = spans::protected_ranges(text);
    let hits = find_dashes(text, &protected);
    if hits.is_empty() {
        return text.to_string();
    }
    // Which hits are punctuation, so paired dashes in one sentence can become commas.
    let punct: Vec<bool> = hits.iter().map(|h| h.action == DashAction::Punct).collect();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    let mut flags = CaseFlags::default();
    for (n, h) in hits.iter().enumerate() {
        // Dashes separated only by spaces share those spaces: never step backwards.
        let from = h.from.max(last).min(h.start);
        push_segment(&mut out, &text[last.min(from)..from], &mut flags);
        last = h.to;
        match h.action {
            DashAction::Bullet => {
                out.push_str(&text[from..h.start]);
                out.push_str("- ");
            }
            DashAction::Remove => trim_trailing(&mut out, &[]),
            DashAction::Hyphen => {
                out.push_str(&text[from..h.start]);
                out.push('-');
                last = h.end;
            }
            DashAction::Range => out.push('-'),
            DashAction::Punct => {
                let left = out.chars().last();
                let right = text[h.to..].chars().next();
                let right_is_punct = right.is_some_and(|c| matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | ')'));
                if left.is_some_and(|c| matches!(c, ',' | ';' | ':' | '.' | '!' | '?' | '(')) {
                    if !right_is_punct && left != Some('(') {
                        out.push(' ');
                    }
                } else if right_is_punct || right.is_none() {
                    // nothing: the following punctuation ends the clause
                } else if joins_sentences(text, &hits, &punct, n) {
                    out.push_str(". ");
                    flags.cap_next = true;
                } else {
                    out.push_str(", ");
                }
            }
        }
    }
    push_segment(&mut out, &text[last..], &mut flags);
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DashAction {
    /// "– item" at a line start.
    Bullet,
    /// A dangling dash at the end of a line.
    Remove,
    /// Unspaced en dash between words: "Monday–Friday".
    Hyphen,
    /// Between two numbers: "2–3", "10 – 12".
    Range,
    /// Used as punctuation.
    Punct,
}

#[derive(Clone, Debug)]
struct DashHit {
    /// The dash characters.
    start: usize,
    end: usize,
    /// Including the spaces around the dash.
    from: usize,
    to: usize,
    action: DashAction,
}

fn find_dashes(text: &str, protected: &[Range<usize>]) -> Vec<DashHit> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = Vec::new();
    let mut k = 0;
    while k < chars.len() {
        let (pos, c) = chars[k];
        let (len_chars, is_double) = match c {
            '—' | '―' | '‒' | '–' => (1, false),
            '-' if chars.get(k + 1).is_some_and(|&(_, n)| n == '-')
                && !chars.get(k + 2).is_some_and(|&(_, n)| n == '-')
                && !(k > 0 && chars[k - 1].1 == '-') =>
            {
                (2, true)
            }
            _ => {
                k += 1;
                continue;
            }
        };
        if spans::inside(protected, pos) {
            k += len_chars;
            continue;
        }
        let start = pos;
        let end = chars.get(k + len_chars).map_or(text.len(), |&(p, _)| p);
        let from = start - text[..start].chars().rev().take_while(|&c| c == ' ' || c == '\t').map(char::len_utf8).sum::<usize>();
        let to = end + text[end..].chars().take_while(|&c| c == ' ' || c == '\t').map(char::len_utf8).sum::<usize>();
        let spaced = from < start || to > end;
        let line_start = text[..from].chars().next_back().is_none_or(|c| c == '\n');
        let line_end = text[to..].chars().next().is_none_or(|c| c == '\n' || c == '\r');
        let left = text[..from].chars().next_back();
        let right = text[to..].chars().next();
        let en = c == '–';
        let action = if is_double {
            // "--help" (no space after) or "a--b" with only one side spaced is code, not a dash.
            let letters_both = left.is_some_and(char::is_alphabetic) && right.is_some_and(char::is_alphabetic) && !spaced;
            let spaced_both = from < start && to > end;
            // "build -- --watch": a separator before more flags is a command line.
            if line_start || !(letters_both || spaced_both) || right == Some('-') {
                k += len_chars;
                continue;
            }
            if line_end {
                DashAction::Remove
            } else {
                DashAction::Punct
            }
        } else if line_start && !line_end {
            DashAction::Bullet
        } else if line_end {
            DashAction::Remove
        } else if left.is_some_and(|c| c.is_ascii_digit()) && right.is_some_and(|c| c.is_ascii_digit()) {
            DashAction::Range
        } else if en && !spaced && left.is_some_and(char::is_alphanumeric) && right.is_some_and(char::is_alphanumeric) {
            DashAction::Hyphen
        } else {
            DashAction::Punct
        };
        out.push(DashHit { start, end, from, to, action });
        k += len_chars;
    }
    out
}

const SUBJECT_STARTERS: &[&str] = &[
    "i", "you", "he", "she", "it", "we", "they", "there", "this", "that", "these", "those", "it's", "that's", "there's",
    "i'm", "we're", "they're", "you're", "he's", "she's", "i've", "we've", "they've", "you've", "i'll", "we'll",
    "they'll", "you'll", "i'd", "we'd", "they'd", "you'd", "let's", "here's", "what's",
];

/// A lone punctuation dash between two full sentences ("I went home — it was late") becomes a
/// full stop; everything else becomes a comma.
fn joins_sentences(text: &str, hits: &[DashHit], punct: &[bool], n: usize) -> bool {
    let h = &hits[n];
    let is_boundary = |c: char| matches!(c, '.' | '!' | '?' | '\n');
    let s0 = text[..h.from].char_indices().rev().find(|&(_, c)| is_boundary(c)).map_or(0, |(i, c)| i + c.len_utf8());
    let s1 = text[h.to..].find(is_boundary).map_or(text.len(), |i| h.to + i);
    let paired = hits.iter().enumerate().any(|(m, o)| m != n && punct[m] && o.start >= s0 && o.end <= s1);
    if paired {
        return false;
    }
    let before_words = text[s0..h.from].split_whitespace().count();
    let after = &text[h.to..s1];
    let after_words = after.split_whitespace().count();
    let first = after
        .split_whitespace()
        .next()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '’').replace('’', "'").to_lowercase())
        .unwrap_or_default();
    before_words >= 3 && after_words >= 3 && SUBJECT_STARTERS.contains(&first.as_str())
}

// =============================================================================================
// Dictionary and snippets
// =============================================================================================

/// Replaces each entry's `sounds_like` variants with its `term` (whole words, case-insensitive)
/// and fixes the casing of the term itself ("quikly" -> "Quikly"). Returns the terms used.
///
/// Multi-word variants match across any spacing or hyphens ("quick ly", "quick-ly"). The
/// longest variant wins where several overlap, replacements never cascade, and links, email
/// addresses, paths and code are left alone. "Terms used" are the terms that appear in the result
/// through a match (a variant, or the term itself in any casing), in order of first appearance.
pub fn apply_dictionary(text: &str, words: &[DictEntry]) -> (String, Vec<String>) {
    if text.is_empty() || words.is_empty() {
        return (text.to_string(), Vec::new());
    }
    let lower_text = text.to_lowercase();
    let mut map: HashMap<String, String> = HashMap::new();
    let mut keys: Vec<String> = Vec::new();
    for e in words {
        let term = e.term.trim();
        if term.is_empty() {
            continue;
        }
        let variants = e.sounds_like.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).chain(std::iter::once(term));
        for v in variants {
            let key = norm_key(v);
            if key.is_empty() || map.contains_key(&key) {
                continue;
            }
            let first = key.split(' ').next().unwrap_or_default();
            if !lower_text.contains(first) {
                continue; // cheap pre-filter keeps the pattern small for big dictionaries
            }
            map.insert(key.clone(), term.to_string());
            keys.push(key);
        }
    }
    let Some(re) = alternation(&mut keys) else {
        return (text.to_string(), Vec::new());
    };
    let protected = spans::protected_ranges(text);
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    let mut used: Vec<String> = Vec::new();
    for m in re.find_iter(text) {
        if spans::overlaps(&protected, &m.range()) {
            continue;
        }
        let Some(term) = map.get(&norm_key(m.as_str())) else {
            continue;
        };
        out.push_str(&text[last..m.start()]);
        out.push_str(term);
        last = m.end();
        if !used.contains(term) {
            used.push(term.clone());
        }
    }
    out.push_str(&text[last..]);
    (out, used)
}

/// Whole-phrase, case-insensitive trigger -> expansion. Trailing punctuation the engine added after
/// the trigger is dropped when the expansion is a block (contains a newline or URL).
pub fn apply_snippets(text: &str, snippets: &[Snippet]) -> String {
    if text.is_empty() || snippets.is_empty() {
        return text.to_string();
    }
    let mut map: HashMap<String, &Snippet> = HashMap::new();
    let mut keys: Vec<String> = Vec::new();
    for s in snippets {
        let key = norm_key(&s.trigger);
        if key.is_empty() || map.contains_key(&key) {
            continue;
        }
        map.insert(key.clone(), s);
        keys.push(key);
    }
    let Some(re) = alternation(&mut keys) else {
        return text.to_string();
    };
    let protected = spans::protected_ranges(text);
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for m in re.find_iter(text) {
        if m.start() < last || spans::overlaps(&protected, &m.range()) {
            continue;
        }
        let Some(snippet) = map.get(&norm_key(m.as_str())) else {
            continue;
        };
        out.push_str(&text[last..m.start()]);
        out.push_str(&snippet.expansion);
        last = m.end();
        if is_block(&snippet.expansion) {
            let rest = &text[last..];
            let trimmed = rest.trim_start_matches(['.', ',', '!', '?', ';', ':']);
            last += rest.len() - trimmed.len();
        }
    }
    out.push_str(&text[last..]);
    out
}

fn is_block(s: &str) -> bool {
    s.contains('\n') || s.contains("://") || s.to_ascii_lowercase().contains("www.")
}

/// Lower case, words separated by single spaces (hyphens count as spaces).
fn norm_key(s: &str) -> String {
    s.to_lowercase().split(|c: char| c.is_whitespace() || c == '-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ")
}

/// One case-insensitive regex matching any key, longest first, with word boundaries where the key
/// starts or ends with a word character.
fn alternation(keys: &mut [String]) -> Option<Regex> {
    if keys.is_empty() {
        return None;
    }
    keys.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    let parts: Vec<String> = keys
        .iter()
        .map(|k| {
            let body = k.split(' ').map(regex::escape).collect::<Vec<_>>().join(r"[\s\-]+");
            let pre = if k.chars().next().is_some_and(spans::is_word_char) { r"\b" } else { "" };
            let post = if k.chars().last().is_some_and(spans::is_word_char) { r"\b" } else { "" };
            format!("{pre}{body}{post}")
        })
        .collect();
    Regex::new(&format!("(?i)(?:{})", parts.join("|"))).ok()
}

// =============================================================================================
// Cue echo and hallucinations
// =============================================================================================

/// Removes a leading cue word picked up from the speakers ("Hey!", "Hey.", "Listen,") when
/// `cue_word` is Some. Only at the very start, only that word.
///
/// The cue word must be followed by punctuation (or be the whole text): "Hey Sarah, ..." is the
/// user's own greeting and stays, and so does "Hey, Sarah" (a capitalised name after the comma).
pub fn strip_cue_echo(text: &str, cue_word: Option<&str>) -> String {
    let Some(cue) = cue_word.map(|c| c.trim_matches(|ch: char| !ch.is_alphanumeric())).filter(|c| !c.is_empty()) else {
        return text.to_string();
    };
    let t = text.trim_start();
    let head_len: usize = t.chars().take(cue.chars().count()).map(char::len_utf8).sum();
    let head = &t[..head_len];
    if head.to_lowercase() != cue.to_lowercase() {
        return text.to_string();
    }
    let rest = &t[head_len..];
    if rest.chars().next().is_some_and(|c| c.is_alphanumeric() || matches!(c, '\'' | '’' | '-')) {
        return text.to_string(); // "Heyday", "Hey's"
    }
    let after_punct = rest.trim_start_matches(['!', '.', '?', ',', '…', ';', ':']);
    let punct = &rest[..rest.len() - after_punct.len()];
    let after = after_punct.trim_start();
    if punct.is_empty() {
        return if after.is_empty() { String::new() } else { text.to_string() };
    }
    if punct.starts_with(',') {
        let first: String = after.chars().take_while(|c| c.is_alphanumeric()).collect();
        if first != "I" && first.chars().next().is_some_and(char::is_uppercase) {
            return text.to_string();
        }
    }
    let first_word: String = after.chars().take_while(|c| c.is_alphabetic()).collect();
    if !first_word.is_empty() && first_word.chars().all(char::is_lowercase) {
        capitalise_first(after)
    } else {
        after.to_string()
    }
}

/// Whisper's habit of returning things like "[BLANK_AUDIO]", "(silence)", "Thank you." on silence:
/// returns true when the transcript is one of those and nothing else.
///
/// Note: "Thank you." and "you" on their own are treated as hallucinations, as the contract asks.
/// If the audio clearly held speech, the caller may prefer [`is_noise_marker`], which only matches
/// bracketed markers, music notes and punctuation.
pub fn is_hallucination(text: &str) -> bool {
    if is_noise_marker(text) {
        return true;
    }
    HALLUCINATIONS.contains(&normalise_phrase(text).as_str())
}

/// True for empty text, punctuation only, music notes, or nothing but bracketed markers such as
/// "[BLANK_AUDIO]", "(silence)", "[Music] [Applause]", "*coughs*".
pub fn is_noise_marker(text: &str) -> bool {
    let t = text.trim();
    if !t.chars().any(char::is_alphanumeric) {
        return true;
    }
    const NOTES: [char; 4] = ['♪', '♫', '♬', '♩'];
    if t.starts_with(NOTES) || t.ends_with(NOTES) {
        return true;
    }
    match MARKERS.as_ref() {
        Some(re) => re.is_match(t) && !re.replace_all(t, "").chars().any(char::is_alphanumeric),
        None => false,
    }
}

static MARKERS: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"\[[^\]]*\]|\([^)]*\)|\*[^*]*\*").ok());

const HALLUCINATIONS: &[&str] = &[
    "thank you",
    "thank you very much",
    "thank you so much",
    "thanks for watching",
    "thank you for watching",
    "thanks for watching and see you next time",
    "thank you for watching and see you next time",
    "thank you so much for watching",
    "thanks so much for watching",
    "please subscribe",
    "subscribe to my channel",
    "like and subscribe",
    "please like and subscribe",
    "don't forget to like and subscribe",
    "see you in the next video",
    "you",
    "subtitles by the amara org community",
    "sous titres réalisés par la communauté d'amara org",
    "untertitel der amara org community",
];

fn normalise_phrase(text: &str) -> String {
    let mapped: String = text
        .to_lowercase()
        .chars()
        .map(|c| match c {
            '’' => '\'',
            c if c.is_alphanumeric() || c == '\'' => c,
            _ => ' ',
        })
        .collect();
    mapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- spoken punctuation ----

    #[test]
    fn spoken_basic_marks() {
        assert_eq!(spoken_punctuation("hello comma how are you question mark"), "hello, how are you?");
        assert_eq!(spoken_punctuation("I went home full stop it was late"), "I went home. It was late");
        assert_eq!(spoken_punctuation("that's amazing exclamation mark"), "that's amazing!");
        assert_eq!(spoken_punctuation("wow exclamation point"), "wow!");
        assert_eq!(spoken_punctuation("I came semicolon I saw"), "I came; I saw");
        assert_eq!(spoken_punctuation("three things colon eggs comma milk and bread"), "three things: eggs, milk and bread");
        assert_eq!(spoken_punctuation("I'm done period"), "I'm done.");
        assert_eq!(spoken_punctuation("and then dot dot dot nothing"), "and then... nothing");
    }

    #[test]
    fn spoken_absorbs_engine_punctuation() {
        assert_eq!(spoken_punctuation("Hello, comma, how are you?"), "Hello, how are you?");
        assert_eq!(spoken_punctuation("Hello. Full stop."), "Hello.");
        assert_eq!(spoken_punctuation("Is it ready question mark."), "Is it ready?");
        assert_eq!(spoken_punctuation("Hello. Comma. How are you?"), "Hello, how are you?");
        assert_eq!(spoken_punctuation("I went home. Period. It was late."), "I went home. It was late.");
    }

    #[test]
    fn spoken_lines_and_paragraphs() {
        assert_eq!(
            spoken_punctuation("Dear Sam comma new line thanks for the update full stop"),
            "Dear Sam,\nThanks for the update."
        );
        assert_eq!(spoken_punctuation("first point new paragraph second point"), "first point\n\nSecond point");
        assert_eq!(spoken_punctuation("Hello. New line. How are you?"), "Hello.\nHow are you?");
    }

    #[test]
    fn spoken_quotes_brackets_dash_hyphen() {
        assert_eq!(spoken_punctuation("he said open quote hello close quote"), "he said \"hello\"");
        assert_eq!(spoken_punctuation("he said open quote hello close quote and left"), "he said \"hello\" and left");
        assert_eq!(spoken_punctuation("it's free open bracket for now close bracket"), "it's free (for now)");
        assert_eq!(spoken_punctuation("I went home dash it was late"), "I went home - it was late");
        assert_eq!(spoken_punctuation("a well hyphen known fact"), "a well-known fact");
    }

    #[test]
    fn spoken_words_used_as_nouns_stay() {
        for s in [
            "the car came to a full stop",
            "over a period of time",
            "we are still in the trial period.",
            "it was a difficult period",
            "the Oxford comma is useful",
            "there's a question mark over it",
            "a dash of salt",
            "I need to dash off now",
            "colon cancer screening",
            "a new line of products",
            "add a hyphen here",
            "Period.",
            "export it as comma-separated values",
            "the values are comma separated",
            "the fields are colon-separated",
            "save it as newline-delimited JSON",
            "npm run build dash dash watch",
        ] {
            assert_eq!(spoken_punctuation(s), s.trim(), "{s}");
        }
    }

    #[test]
    fn spoken_period_rules() {
        assert_eq!(spoken_punctuation("I went home period I ate"), "I went home. I ate");
        assert_eq!(spoken_punctuation("the testing period ended"), "the testing period ended");
        assert_eq!(spoken_punctuation("a two week period."), "a two week period.");
    }

    // ---- fillers ----

    #[test]
    fn fillers_basic() {
        assert_eq!(remove_fillers("Um, I think we should go."), "I think we should go.");
        assert_eq!(remove_fillers("I was, um, thinking about it."), "I was thinking about it.");
        assert_eq!(remove_fillers("Yes, um, I agree."), "Yes, I agree.");
        assert_eq!(remove_fillers("So, uh, what now?"), "So, what now?");
        assert_eq!(remove_fillers("I think um we should erm go"), "I think we should go");
        assert_eq!(remove_fillers("Hmm."), "");
        assert_eq!(remove_fillers("It's done, um."), "It's done.");
        assert_eq!(remove_fillers("I was um... thinking"), "I was thinking");
    }

    #[test]
    fn fillers_keep_meaningful_like_and_so() {
        assert_eq!(remove_fillers("It was, like, huge."), "It was huge.");
        assert_eq!(remove_fillers("I like it."), "I like it.");
        assert_eq!(remove_fillers("It's like a dream."), "It's like a dream.");
        assert_eq!(remove_fillers("So we left early."), "So we left early.");
        assert_eq!(remove_fillers("So, like, I went home."), "So, I went home.");
    }

    #[test]
    fn fillers_you_know_i_mean() {
        assert_eq!(remove_fillers("It was, you know, really hard."), "It was really hard.");
        assert_eq!(remove_fillers("You know, I think it works."), "I think it works.");
        assert_eq!(remove_fillers("It was hard, you know?"), "It was hard.");
        assert_eq!(remove_fillers("Do you know the way?"), "Do you know the way?");
        assert_eq!(remove_fillers("You know?"), "You know?");
        assert_eq!(remove_fillers("If you know, you know."), "If you know, you know.");
        assert_eq!(remove_fillers("I mean, it's fine."), "It's fine.");
        assert_eq!(remove_fillers("I mean it."), "I mean it.");
        assert_eq!(remove_fillers("What I mean is simple."), "What I mean is simple.");
    }

    #[test]
    fn fillers_repetitions() {
        assert_eq!(remove_fillers("the the cat"), "the cat");
        assert_eq!(remove_fillers("I I think so"), "I think so");
        assert_eq!(remove_fillers("I-I think so"), "I think so");
        assert_eq!(remove_fillers("I, I think so"), "I think so");
        assert_eq!(remove_fillers("The the end."), "The end.");
        for s in ["I had had enough.", "I know that that is true.", "It was very very good.", "Bye bye.", "No no no.", "What it is is fine."] {
            assert_eq!(remove_fillers(s), s, "{s}");
        }
    }

    #[test]
    fn fillers_leave_abbreviations_and_links() {
        assert_eq!(remove_fillers("It's 5 mm wide."), "It's 5 mm wide.");
        assert_eq!(remove_fillers("The ER was busy."), "The ER was busy.");
        assert_eq!(remove_fillers("To err is human."), "To err is human.");
        assert_eq!(remove_fillers("Check https://um.example.com/uh now."), "Check https://um.example.com/uh now.");
        assert_eq!(remove_fillers("Uh-huh, sounds good."), "Uh-huh, sounds good.");
    }

    #[test]
    fn fillers_keep_newlines() {
        assert_eq!(remove_fillers("Dear Sam,\num, thanks for this."), "Dear Sam,\nthanks for this.");
    }

    // ---- tidy ----

    #[test]
    fn tidy_basics() {
        assert_eq!(tidy("hello world"), "Hello world.");
        assert_eq!(tidy("  hello ,  world  "), "Hello, world.");
        assert_eq!(tidy("i think i'm right and i've said so"), "I think I'm right and I've said so.");
        assert_eq!(tidy("this is it. then we go"), "This is it. Then we go.");
        assert_eq!(tidy("is it done?"), "Is it done?");
        assert_eq!(tidy("Thanks"), "Thanks");
        assert_eq!(tidy("hello.."), "Hello.");
        assert_eq!(tidy("really?."), "Really?");
        assert_eq!(tidy("one,, two"), "One, two.");
        assert_eq!(tidy("hello,world"), "Hello, world.");
    }

    #[test]
    fn tidy_respects_abbreviations_ellipses_and_machine_text() {
        assert_eq!(tidy("e.g. apples are good"), "e.g. apples are good.");
        assert_eq!(tidy("wait... what"), "Wait... what.");
        assert_eq!(tidy("see https://example.com"), "See https://example.com");
        assert_eq!(tidy("it costs 3.50 pounds"), "It costs 3.50 pounds.");
        assert_eq!(tidy("ask Mr. smith today"), "Ask Mr. smith today.");
        assert_eq!(tidy("std::io is great"), "std::io is great.");
        assert_eq!(tidy("the iPhone is here. iPhones are fine"), "The iPhone is here. iPhones are fine.");
        assert_eq!(tidy("use i.e. this"), "Use i.e. this.");
    }

    #[test]
    fn tidy_lines_lists_and_code() {
        assert_eq!(tidy("dear sam,\nthanks for the update"), "Dear sam,\nThanks for the update.");
        assert_eq!(tidy("list:\n- eggs\n- milk"), "List:\n- Eggs\n- Milk");
        assert_eq!(tidy("one\n\n\n\ntwo words"), "One\n\nTwo words.");
        let code = "here it is:\n```\nlet  x = 1 ;\n```";
        assert_eq!(tidy(code), "Here it is:\n```\nlet  x = 1 ;\n```");
    }

    #[test]
    fn tidy_spacing_only() {
        assert_eq!(tidy_spacing("hello ,  world  ."), "hello, world.");
        assert_eq!(tidy_spacing("i said ( maybe )"), "i said (maybe)");
    }

    // ---- dashes ----

    #[test]
    fn dashes_become_commas_or_full_stops() {
        assert_eq!(no_em_dashes("I went home — it was late."), "I went home. It was late.");
        assert_eq!(no_em_dashes("My brother — who lives in Leeds — is visiting."), "My brother, who lives in Leeds, is visiting.");
        assert_eq!(no_em_dashes("It was late—very late."), "It was late, very late.");
        assert_eq!(no_em_dashes("There's one answer – money."), "There's one answer, money.");
        assert_eq!(no_em_dashes("word -- another word"), "word, another word");
    }

    #[test]
    fn dashes_ranges_hyphens_bullets() {
        assert_eq!(no_em_dashes("Pages 2–3"), "Pages 2-3");
        assert_eq!(no_em_dashes("10 – 12 people"), "10-12 people");
        assert_eq!(no_em_dashes("Monday–Friday"), "Monday-Friday");
        assert_eq!(no_em_dashes("a well-known fact"), "a well-known fact");
        assert_eq!(no_em_dashes("– first item\n– second item"), "- first item\n- second item");
        assert_eq!(no_em_dashes("I was going to say —"), "I was going to say");
    }

    #[test]
    fn dashes_leave_code_and_flags() {
        for s in ["run it with --help", "npm run build -- --watch", "`a — b`", "---", "a -- b --c"] {
            if s == "a -- b --c" {
                assert_eq!(no_em_dashes(s), "a, b --c");
            } else {
                assert_eq!(no_em_dashes(s), s, "{s}");
            }
        }
    }

    // ---- dictionary and snippets ----

    fn entry(term: &str, sounds: &[&str]) -> DictEntry {
        DictEntry { term: term.into(), sounds_like: sounds.iter().map(|s| s.to_string()).collect(), ..Default::default() }
    }

    #[test]
    fn dictionary_variants_and_casing() {
        let words = vec![entry("Quikly", &["quickly", "quick ly"]), entry("Tori", &["tory"])];
        let (out, used) = apply_dictionary("I use quickly and Quick ly with tory", &words);
        assert_eq!(out, "I use Quikly and Quikly with Tori");
        assert_eq!(used, vec!["Quikly".to_string(), "Tori".to_string()]);
        let (out, used) = apply_dictionary("quikly is great", &words);
        assert_eq!(out, "Quikly is great");
        assert_eq!(used, vec!["Quikly".to_string()]);
    }

    #[test]
    fn dictionary_whole_words_links_and_hyphens() {
        let words = vec![entry("Quikly", &["quickly", "quick ly"])];
        assert_eq!(apply_dictionary("it went quicklier than planned", &words).0, "it went quicklier than planned");
        assert_eq!(apply_dictionary("a quick lyric sheet", &words).0, "a quick lyric sheet");
        assert_eq!(apply_dictionary("see https://quickly.example", &words).0, "see https://quickly.example");
        assert_eq!(apply_dictionary("quickly's plan", &words).0, "Quikly's plan");
        assert_eq!(apply_dictionary("quick-ly rocks", &words).0, "Quikly rocks");
        let (out, used) = apply_dictionary("nothing here", &words);
        assert_eq!(out, "nothing here");
        assert!(used.is_empty());
    }

    #[test]
    fn dictionary_longest_match_wins_without_cascading() {
        let words = vec![entry("Claude", &["clawed"]), entry("Claude Code", &["clawed code"])];
        assert_eq!(apply_dictionary("clawed code and clawed", &words).0, "Claude Code and Claude");
    }

    fn snip(trigger: &str, expansion: &str) -> Snippet {
        Snippet { trigger: trigger.into(), expansion: expansion.into(), ..Default::default() }
    }

    #[test]
    fn snippets_expand() {
        let s = vec![snip("my calendar link", "https://cal.example/alex"), snip("my email", "alex@example.com"), snip("my address", "1 High St\nLondon")];
        assert_eq!(apply_snippets("Book here: my calendar link.", &s), "Book here: https://cal.example/alex");
        assert_eq!(apply_snippets("Email me at my email.", &s), "Email me at alex@example.com.");
        assert_eq!(apply_snippets("Send it to my address.", &s), "Send it to 1 High St\nLondon");
        assert_eq!(apply_snippets("My  Calendar   Link please", &s), "https://cal.example/alex please");
        assert_eq!(apply_snippets("my calendar linking", &s), "my calendar linking");
    }

    // ---- cue echo ----

    #[test]
    fn cue_echo() {
        assert_eq!(strip_cue_echo("Hey! I wanted to ask", Some("Hey")), "I wanted to ask");
        assert_eq!(strip_cue_echo("Hey. so it works", Some("Hey!")), "So it works");
        assert_eq!(strip_cue_echo("Listen, I need to plan", Some("Listen")), "I need to plan");
        assert_eq!(strip_cue_echo("Hey!", Some("Hey")), "");
        assert_eq!(strip_cue_echo("Hey Sarah, how are you", Some("Hey")), "Hey Sarah, how are you");
        assert_eq!(strip_cue_echo("Hey, Sarah, how are you", Some("Hey")), "Hey, Sarah, how are you");
        assert_eq!(strip_cue_echo("Heyday is here", Some("Hey")), "Heyday is here");
        assert_eq!(strip_cue_echo("I said hey!", Some("Hey")), "I said hey!");
        assert_eq!(strip_cue_echo("Hey! Hello", None), "Hey! Hello");
    }

    // ---- hallucinations ----

    #[test]
    fn hallucinations() {
        for s in ["", "  ", "[BLANK_AUDIO]", " (silence) ", "Thank you.", "Thanks for watching!", "you", "You.", "♪♪", "♪ la la la ♪", "[Music] [Applause]", "...", "*coughs*"] {
            assert!(is_hallucination(s), "{s:?} should be a hallucination");
        }
        for s in ["Thank you for the update.", "You should come.", "Hello", "Thanks.", "Hello (laughs)"] {
            assert!(!is_hallucination(s), "{s:?} is real speech");
        }
        assert!(is_noise_marker("[BLANK_AUDIO]"));
        assert!(!is_noise_marker("Thank you."));
    }
}
