//! raw transcript -> final text, for one take.
//!
//! OWNER: text agent.
//!
//! Order:
//!  1. cleanup::spoken_punctuation, then snippets, then dictionary (record the terms used).
//!  2. Raw mode stops here (after tidy spacing only).
//!  3. Offline: remove fillers (if enabled), tidy.
//!  4. AI step when `llm` is Some and the mode allows AI (Clean uses AI only for a light polish with
//!     a strict "change as little as possible" prompt; Rewrite/Prompt always use it). If the AI call
//!     fails or times out, fall back to the offline text and add a warning; never lose the words.
//!     Prompt mode without AI: offline text plus "Relevant files: @a @b" from the candidates.
//!  5. Guard against the AI answering instead of rewriting: if the output looks like a reply to the
//!     text (starts with "Sure", "Here's", "Certainly", or is far longer than the input for a
//!     non-prompt mode), use the offline text instead and warn.
//!  6. no_em_dashes and britishise last (when enabled), then the dictionary again so AI changes
//!     cannot undo a term's spelling. Prompt mode is never britishised: a bare identifier or
//!     command ("npm run analyze") has nothing to mark it as code.
//!
//! Details:
//! * "The mode allows AI" means `mode.use_ai` is true and the kind is Clean, Rewrite or Prompt.
//!   Notes is handled by `text::ramble`, so here it gets the offline tidy only.
//! * A reply that only wraps good text in a preamble line ("Here's the cleaned text:") or quotes is
//!   unwrapped rather than thrown away. Clean, Message and Email must keep a question a question.
//! * A heading or closing line the speaker said ("So here are the dates", "let me know when...")
//!   is theirs, wherever they said it, so it is never taken for the AI's wrapping.
//! * Dictionary terms are protected from britishise (a brand spelt the American way stays).
//! * When the text before the cursor stops mid-sentence ("I went to the shop and"), a common first
//!   word loses its capital ("then we left"); names keep theirs. Raw mode is never changed.
//! * Warnings are short British-English sentences for the history view.

use crate::text::llm::Llm;
use crate::text::{agent, britishise, cleanup, modes};
use crate::types::{AppContext, DictEntry, Mode, ModeKind, Settings, Snippet};
use anyhow::Result;
use std::time::Duration;

pub struct PipelineInput<'a> {
    pub raw: &'a str,
    pub mode: &'a Mode,
    pub ctx: &'a AppContext,
    pub settings: &'a Settings,
    pub words: &'a [DictEntry],
    pub snippets: &'a [Snippet],
    pub llm: Option<&'a Llm>,
    /// Candidate files for Prompt mode (relative paths), empty otherwise.
    pub project_files: &'a [String],
}

#[derive(Clone, Debug, Default)]
pub struct PipelineOutput {
    pub text: String,
    pub used_ai: bool,
    pub terms_used: Vec<String>,
    pub warnings: Vec<String>,
}

/// Hard stop for the AI step, a little above the client's own 20 second timeout.
const AI_DEADLINE: Duration = Duration::from_secs(25);
/// Most candidate files shown to the AI in Prompt mode.
const MAX_PROMPT_FILES: usize = 60;

pub async fn run(input: PipelineInput<'_>) -> Result<PipelineOutput> {
    let s = input.settings;
    let mut warnings = Vec::new();

    // 1. Spoken punctuation, snippets, dictionary.
    let text = cleanup::spoken_punctuation(input.raw);
    let text = cleanup::apply_snippets(&text, input.snippets);
    let (text, mut terms) = cleanup::apply_dictionary(&text, input.words);

    // 2. Raw mode: exactly what was said.
    if input.mode.kind == ModeKind::Raw {
        return Ok(PipelineOutput { text: cleanup::tidy_spacing(&text), used_ai: false, terms_used: terms, warnings });
    }

    // 3. Offline tidy.
    let mut offline = text;
    if s.writing.remove_fillers {
        offline = cleanup::remove_fillers(&offline);
    }
    offline = cleanup::tidy(&offline);

    // 4 and 5. Optional AI step with a guard.
    let mut out = offline.clone();
    let mut used_ai = false;
    let wants_ai = input.mode.use_ai && matches!(input.mode.kind, ModeKind::Clean | ModeKind::Rewrite | ModeKind::Prompt);
    // A subscription tool takes seconds a call: worth it for a rewrite, not for Clean's light polish.
    let wants_ai = wants_ai && !(input.mode.kind == ModeKind::Clean && input.llm.is_some_and(|l| l.is_slow()));
    if let (Some(llm), true) = (input.llm, wants_ai) {
        if !offline.trim().is_empty() {
            let files = if input.mode.kind == ModeKind::Prompt { prompt_files(&offline, input.project_files) } else { Vec::new() };
            let (system, user) = modes::build_messages(input.mode, &offline, input.ctx, s, &terms, &files);
            let (max_tokens, temperature) = budget(&input.mode.kind, &offline);
            match tokio::time::timeout(AI_DEADLINE, llm.complete(&system, &user, max_tokens, temperature)).await {
                Ok(Ok(reply)) => {
                    let reply = unwrap_reply(&reply, &offline);
                    match reply_problem(&reply, &offline, input.mode) {
                        None => {
                            out = reply;
                            used_ai = true;
                        }
                        Some(why) => warnings.push(why),
                    }
                }
                Ok(Err(e)) => warnings.push(format!("AI step skipped: {e} Your words were tidied offline instead.")),
                Err(_) => warnings.push("AI step skipped: no reply in time. Your words were tidied offline instead.".to_string()),
            }
        }
    }

    // Prompt mode without AI: point at the files the speaker probably meant.
    if input.mode.kind == ModeKind::Prompt && !used_ai {
        let picks = agent::candidates(&offline, input.project_files, 3);
        if !picks.is_empty() {
            let mentions: Vec<String> = picks.iter().map(|p| agent::mention(p, &s.agent.mention_style)).collect();
            out = format!("{}\n\nRelevant files: {}", out.trim_end(), mentions.join(" "));
        }
    }

    // 6. House style last, then the dictionary again.
    if s.writing.no_em_dashes {
        out = cleanup::no_em_dashes(&out);
    }
    // A prompt for a coding agent is full of identifiers and commands that must stay as said
    // ("npm run analyze", "initialize"); the AI is already told to keep those exactly.
    if s.writing.british && input.mode.kind != ModeKind::Prompt {
        let keep: Vec<String> = input.words.iter().map(|w| w.term.trim().to_string()).filter(|t| !t.is_empty()).collect();
        out = britishise::britishise_keeping(&out, &keep);
    }
    if continues_sentence(input.ctx.field_text.as_deref()) {
        out = cleanup::decapitalise_common_start(&out);
    }
    let (final_text, more) = cleanup::apply_dictionary(&out, input.words);
    for t in more {
        if !terms.contains(&t) {
            terms.push(t);
        }
    }
    Ok(PipelineOutput { text: final_text.trim().to_string(), used_ai, terms_used: terms, warnings })
}

/// For voice rewrites and transforms (`voice::rewrite_messages` replies): removes wrapping the model
/// added around the text (a "Here's the text:" line, code fences, quotes, a closing "Let me know..."
/// line). Returns None when the reply is empty or opens like a chat reply ("Sure", "Certainly")
/// although `original` did not, so the caller can keep the original instead.
pub fn clean_ai_reply(reply: &str, original: &str) -> Option<String> {
    let r = unwrap_reply(reply, original);
    if r.trim().is_empty() {
        return None;
    }
    let opener = starts_with_phrase(&r, REPLY_OPENERS);
    if opener.is_some() && starts_with_phrase(original, REPLY_OPENERS) != opener && !opens_with_their_words(&r, original) {
        return None;
    }
    Some(r)
}

/// Does the text before the cursor stop mid-sentence (a lower-case letter, digit, comma, colon,
/// opening bracket or dash, not a line break)?
fn continues_sentence(field: Option<&str>) -> bool {
    let Some(f) = field else {
        return false;
    };
    let t = f.trim_end_matches([' ', '\t']);
    t.chars().last().is_some_and(|c| c.is_lowercase() || c.is_ascii_digit() || matches!(c, ',' | ';' | ':' | '(' | '-' | '–' | '—'))
}

/// (max_tokens, temperature) for the AI step.
fn budget(kind: &ModeKind, text: &str) -> (u32, f32) {
    let words = u32::try_from(text.split_whitespace().count()).unwrap_or(u32::MAX / 8);
    match kind {
        ModeKind::Clean => (words.saturating_mul(2).saturating_add(64).clamp(128, 4096), 0.0),
        ModeKind::Prompt => (words.saturating_mul(4).saturating_add(400).clamp(512, 4096), 0.2),
        _ => (words.saturating_mul(3).saturating_add(200).clamp(256, 4096), 0.3),
    }
}

/// The files most likely meant first, then the shallowest others for orientation.
fn prompt_files(text: &str, files: &[String]) -> Vec<String> {
    let mut picked = agent::candidates(text, files, MAX_PROMPT_FILES);
    if picked.len() < MAX_PROMPT_FILES {
        let mut rest: Vec<&String> = files.iter().filter(|f| !picked.contains(f)).collect();
        rest.sort_by_key(|f| (f.matches('/').count(), f.len()));
        let room = MAX_PROMPT_FILES - picked.len();
        picked.extend(rest.into_iter().take(room).cloned());
    }
    picked
}

const REPLY_OPENERS: &[&str] = &[
    "sure", "here's", "here is", "here are", "certainly", "of course", "absolutely", "i'd be happy", "i would be happy",
    "i'm happy to", "happy to help", "i'm sorry", "i am sorry", "sorry, i", "i can't", "i cannot", "i can not",
    "as an ai", "as a language model", "okay, here", "ok, here", "great question", "good question", "below is",
    "the rewritten", "rewritten text", "the cleaned", "cleaned-up text", "cleaned up text", "the corrected",
    "corrected text", "i've rewritten", "i have rewritten", "i've cleaned", "i've corrected", "i've tidied",
    "the polished", "polished text", "the revised", "revised text",
];

const CLOSERS: &[&str] = &["let me know", "i hope this helps", "hope this helps", "feel free to", "would you like me to", "do you want me to"];

/// Lower case, curly apostrophes straightened, leading quotes and markdown stripped.
fn opening(s: &str) -> String {
    s.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '“' | '‘' | '*' | '_' | '#' | '>'))
        .chars()
        .take(80)
        .collect::<String>()
        .replace('’', "'")
        .to_lowercase()
}

/// The phrase from `phrases` that `text` opens with, as a whole word or words.
fn starts_with_phrase(text: &str, phrases: &[&'static str]) -> Option<&'static str> {
    let o = opening(text);
    phrases.iter().copied().find(|p| o.strip_prefix(p).is_some_and(|rest| !rest.chars().next().is_some_and(char::is_alphanumeric)))
}

/// Lower-case words with straight apostrophes, single-spaced and padded with a space at each end,
/// so a phrase can be found as whole words whatever the punctuation.
fn plain_words(s: &str) -> String {
    let spaced: String = s.replace('’', "'").to_lowercase().chars().map(|c| if c.is_alphanumeric() || c == '\'' { c } else { ' ' }).collect();
    let words: Vec<&str> = spaced.split_whitespace().map(|w| w.trim_matches('\'')).filter(|w| !w.is_empty()).collect();
    format!(" {} ", words.join(" "))
}

/// Did the speaker say `phrase` anywhere in `input` (whole words, any case or punctuation)?
fn said(input: &str, phrase: &str) -> bool {
    let p = plain_words(phrase);
    !p.trim().is_empty() && plain_words(input).contains(&p)
}

/// True when the reply's first few words are the speaker's own ("So here are the dates..." comes
/// back as "Here are the dates:"), so an opening like "Here are" is theirs, not the AI's.
fn opens_with_their_words(reply: &str, input: &str) -> bool {
    let first: Vec<String> = plain_words(reply).split_whitespace().take(4).map(str::to_string).collect();
    first.len() >= 2 && said(input, &first.join(" "))
}

/// Removes wrapping the model added around otherwise good text: a "Here's the text:" line, a
/// closing "Let me know..." line, code fences or quotes that the speaker never dictated.
fn unwrap_reply(reply: &str, input: &str) -> String {
    let mut r = reply.trim().to_string();
    // Code fences around the whole reply.
    if r.starts_with("```") && r.ends_with("```") && r.len() > 6 && !input.contains("```") {
        let inner = &r[3..r.len() - 3];
        let inner = match inner.find('\n') {
            Some(nl) if !inner[..nl].trim().contains(' ') => &inner[nl + 1..],
            _ => inner,
        };
        r = inner.trim().to_string();
    }
    // A preamble line ending in a colon, when the speaker did not say it.
    if let Some(nl) = r.find('\n') {
        let first = r[..nl].trim();
        if first.ends_with(':') && starts_with_phrase(first, REPLY_OPENERS).is_some() && !opens_with_their_words(first, input) {
            r = r[nl + 1..].trim().to_string();
        }
    }
    // A closing offer of more help as the last paragraph. The speaker's own "Let me know when..."
    // often lands there too (Email puts each point in its own paragraph), so only strip an offer
    // they never made, wherever in the dictation they made it.
    if let Some(pos) = r.rfind("\n\n") {
        let last = r[pos..].trim();
        if let Some(closer) = starts_with_phrase(last, CLOSERS) {
            let core = closer.strip_prefix("i ").unwrap_or(closer);
            if !said(input, core) {
                r = r[..pos].trim().to_string();
            }
        }
    }
    // Quotes around the whole reply.
    for (open, close) in [('"', '"'), ('“', '”'), ('\'', '\'')] {
        if r.chars().count() >= 2 && r.starts_with(open) && r.ends_with(close) && !input.trim_start().starts_with(open) {
            let inner: String = r.chars().skip(1).take(r.chars().count() - 2).collect();
            if !inner.contains(open) && !inner.contains(close) {
                r = inner.trim().to_string();
            }
        }
    }
    r
}

/// Why the reply cannot be used, as a warning for the user, or None when it is fine.
fn reply_problem(reply: &str, input: &str, mode: &Mode) -> Option<String> {
    const KEPT: &str = "so Navi kept the offline version.";
    if reply.trim().is_empty() {
        return Some(format!("The AI returned nothing, {KEPT}"));
    }
    if let Some(p) = starts_with_phrase(reply, REPLY_OPENERS) {
        let input_same = starts_with_phrase(input, REPLY_OPENERS) == Some(p);
        if !input_same && !opens_with_their_words(reply, input) {
            return Some(format!("The AI replied to your words instead of rewriting them, {KEPT}"));
        }
    }
    let words_in = input.split_whitespace().count();
    let words_out = reply.split_whitespace().count();
    if mode.kind == ModeKind::Prompt {
        if reply.contains("```") && !input.contains("```") {
            return Some(format!("The AI wrote code instead of a prompt, {KEPT}"));
        }
        if words_out > words_in * 5 + 120 {
            return Some(format!("The AI added far more than you said, {KEPT}"));
        }
        return None;
    }
    if words_out > words_in * 2 + 20 {
        return Some(format!("The AI added far more than you said, {KEPT}"));
    }
    let keeps_questions = mode.kind == ModeKind::Clean || mode.id == "message" || mode.id == "email";
    if keeps_questions && input.contains('?') && !reply.contains('?') {
        return Some(format!("The AI answered your question instead of writing it down, {KEPT}"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::testkit::{body_json, mock_server};
    use crate::types::AiSettings;
    use serde_json::json;

    fn mode(id: &str) -> Mode {
        modes::builtin_modes().into_iter().find(|m| m.id == id).expect("mode")
    }

    fn entry(term: &str, sounds: &[&str]) -> DictEntry {
        DictEntry { term: term.into(), sounds_like: sounds.iter().map(|s| s.to_string()).collect(), ..Default::default() }
    }

    async fn run_offline(raw: &str, m: &Mode, settings: &Settings, words: &[DictEntry], snippets: &[Snippet], files: &[String]) -> PipelineOutput {
        let ctx = AppContext::default();
        run(PipelineInput { raw, mode: m, ctx: &ctx, settings, words, snippets, llm: None, project_files: files }).await.expect("pipeline")
    }

    #[tokio::test]
    async fn clean_mode_offline() {
        let s = Settings::default();
        let out = run_offline("Um, so I was thinking we could meet on Tuesday, full stop.", &mode("clean"), &s, &[], &[], &[]).await;
        assert_eq!(out.text, "So I was thinking we could meet on Tuesday.");
        assert!(!out.used_ai);
        assert!(out.warnings.is_empty());
    }

    #[tokio::test]
    async fn raw_mode_keeps_what_was_said() {
        let s = Settings::default();
        let words = vec![entry("Quikly", &["quickly"])];
        let out = run_offline("um the color of quickly is nice comma right", &mode("raw"), &s, &words, &[], &[]).await;
        assert_eq!(out.text, "um the color of Quikly is nice, right");
        assert_eq!(out.terms_used, vec!["Quikly".to_string()]);
    }

    #[tokio::test]
    async fn house_style_and_dictionary() {
        let s = Settings::default();
        let words = vec![entry("Quikly", &["quickly"]), entry("colorway", &[])];
        let out = run_offline("The color — it's gray. I love quickly's colorway", &mode("clean"), &s, &words, &[], &[]).await;
        assert_eq!(out.text, "The colour, it's grey. I love Quikly's colorway.");
        assert!(out.terms_used.contains(&"Quikly".to_string()));
        assert!(out.terms_used.contains(&"colorway".to_string()));
    }

    #[tokio::test]
    async fn settings_switch_rules_off() {
        let mut s = Settings::default();
        s.writing.british = false;
        s.writing.no_em_dashes = false;
        s.writing.remove_fillers = false;
        let out = run_offline("um the color — gray", &mode("clean"), &s, &[], &[], &[]).await;
        assert_eq!(out.text, "Um the color — gray.");
    }

    #[tokio::test]
    async fn snippets_apply() {
        let s = Settings::default();
        let snippets = vec![Snippet { trigger: "my calendar link".into(), expansion: "https://cal.example/alex".into(), ..Default::default() }];
        let out = run_offline("Book a slot here: my calendar link.", &mode("clean"), &s, &[], &snippets, &[]).await;
        assert_eq!(out.text, "Book a slot here: https://cal.example/alex");
    }

    #[tokio::test]
    async fn prompt_mode_without_ai_lists_relevant_files() {
        let s = Settings::default();
        let files = vec!["src/components/Header.tsx".to_string(), "src/app.ts".to_string(), "README.md".to_string()];
        let out = run_offline("fix the header component so the logo is centered", &mode("prompt"), &s, &[], &[], &files).await;
        assert_eq!(out.text, "Fix the header component so the logo is centered.\n\nRelevant files: @src/components/Header.tsx");
        let none = run_offline("tidy up the wording", &mode("prompt"), &s, &[], &[], &files).await;
        assert!(!none.text.contains("Relevant files"));
    }

    #[tokio::test]
    async fn prompt_mode_keeps_commands_as_said() {
        let s = Settings::default();
        // Nothing marks "analyze" as a command, so Prompt mode is never britishised.
        let out = run_offline("run npm run analyze then initialize the color picker", &mode("prompt"), &s, &[], &[], &[]).await;
        assert_eq!(out.text, "Run npm run analyze then initialize the color picker.");
        // Every other mode still is.
        let out = run_offline("run npm run analyze then initialize the color picker", &mode("clean"), &s, &[], &[], &[]).await;
        assert!(out.text.contains("colour"), "{}", out.text);
    }

    #[tokio::test]
    async fn continuing_a_sentence_drops_a_common_capital() {
        let s = Settings::default();
        let m = mode("clean");
        let run_in = |field: &'static str, raw: &'static str| {
            let s = s.clone();
            let m = m.clone();
            async move {
                let ctx = AppContext { field_text: Some(field.to_string()), ..Default::default() };
                run(PipelineInput { raw, mode: &m, ctx: &ctx, settings: &s, words: &[], snippets: &[], llm: None, project_files: &[] })
                    .await
                    .expect("pipeline")
                    .text
            }
        };
        assert_eq!(run_in("I went to the shop and ", "Then we left.").await, "then we left.");
        assert_eq!(run_in("I went to the shop and", "Sam came too.").await, "Sam came too.");
        assert_eq!(run_in("I went to the shop and", "I bought milk.").await, "I bought milk.");
        assert_eq!(run_in("That was the end.", "Then we left.").await, "Then we left.");
        assert_eq!(run_in("first line\n", "Then we left.").await, "Then we left.");
        assert!(!continues_sentence(None));
        assert!(continues_sentence(Some("Dear Sam,")));
    }

    #[tokio::test]
    async fn empty_and_filler_only_input() {
        let s = Settings::default();
        assert_eq!(run_offline("", &mode("clean"), &s, &[], &[], &[]).await.text, "");
        assert_eq!(run_offline("Um.", &mode("clean"), &s, &[], &[], &[]).await.text, "");
    }

    fn custom_llm(url: &str) -> Llm {
        let ai = AiSettings { provider: "custom".into(), model: "m".into(), base_url: Some(url.into()), ..Default::default() };
        Llm::from_settings(&ai, None, false).expect("llm")
    }

    fn reply(content: &str) -> (u16, String) {
        (200, json!({ "choices": [{ "message": { "content": content } }] }).to_string())
    }

    async fn run_with_ai(raw: &str, m: &Mode, responses: Vec<(u16, String)>) -> (PipelineOutput, Vec<String>) {
        let (url, seen) = mock_server(responses).await;
        let llm = custom_llm(&url);
        let s = Settings::default();
        let ctx = AppContext::default();
        let words = vec![entry("Quikly", &["quickly"])];
        let out = run(PipelineInput { raw, mode: m, ctx: &ctx, settings: &s, words: &words, snippets: &[], llm: Some(&llm), project_files: &[] })
            .await
            .expect("pipeline");
        (out, seen.await.expect("server"))
    }

    #[tokio::test]
    async fn ai_reply_is_used_and_house_style_still_applies() {
        let (out, reqs) = run_with_ai("hi sam can you send the report by friday thanks", &mode("message"), vec![reply("Hi Sam, can you send the colour report by Friday? Thanks — quickly")]).await;
        assert!(out.used_ai);
        assert_eq!(out.text, "Hi Sam, can you send the colour report by Friday? Thanks, Quikly");
        let body = body_json(&reqs[0]);
        assert!(body["messages"][1]["content"].as_str().unwrap_or("").contains("<transcript>"));
    }

    #[tokio::test]
    async fn ai_failure_falls_back_with_a_warning() {
        let (out, _) = run_with_ai("um hello there", &mode("message"), vec![(500, "{}".into())]).await;
        assert!(!out.used_ai);
        assert_eq!(out.text, "Hello there.");
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0].starts_with("AI step skipped: The AI server had a server error"), "{}", out.warnings[0]);
    }

    #[tokio::test]
    async fn ai_answering_is_rejected() {
        let (out, _) = run_with_ai("what's the capital of France?", &mode("message"), vec![reply("Sure! The capital of France is Paris.")]).await;
        assert!(!out.used_ai);
        assert_eq!(out.text, "What's the capital of France?");
        assert!(out.warnings[0].contains("replied to your words"));
        let (out, _) = run_with_ai("what's the capital of France?", &mode("email"), vec![reply("The capital of France is Paris.")]).await;
        assert!(!out.used_ai);
        assert!(out.warnings[0].contains("answered your question"));
    }

    #[tokio::test]
    async fn ai_wrapping_is_unwrapped_not_rejected() {
        let (out, _) = run_with_ai("hello there", &mode("message"), vec![reply("Here's the cleaned-up text:\n\n\"Hello there.\"")]).await;
        assert!(out.used_ai);
        assert_eq!(out.text, "Hello there.");
    }

    #[tokio::test]
    async fn speaker_starting_with_sure_is_not_a_reply() {
        let (out, _) = run_with_ai("sure thing see you at five", &mode("message"), vec![reply("Sure thing, see you at five.")]).await;
        assert!(out.used_ai);
        assert_eq!(out.text, "Sure thing, see you at five.");
    }

    #[tokio::test]
    async fn far_longer_reply_is_rejected() {
        let long = "word ".repeat(80);
        let (out, _) = run_with_ai("ok thanks", &mode("message"), vec![reply(&long)]).await;
        assert!(!out.used_ai);
        assert!(out.warnings[0].contains("far more"));
    }

    #[tokio::test]
    async fn clean_without_use_ai_never_calls() {
        // No server responses queued: a call would fail and leave a warning.
        let (out, reqs) = run_with_ai("hello there", &mode("clean"), vec![]).await;
        assert!(!out.used_ai);
        assert!(out.warnings.is_empty());
        assert!(reqs.is_empty());
    }

    /// Awkward input must never panic anywhere in the text module.
    #[tokio::test]
    async fn awkward_input_never_panics() {
        use crate::text::{learn, ramble, voice};
        let long = "word ".repeat(3000);
        let samples = [
            "", " ", "\n\n", ".", "...", "—", "–", "--", "-", "\"", "'", "’", "(", ")", "`", "```", "😀 hello 😀", "café crème brûlée",
            "日本語のテキスト", "مرحبا بالعالم", "e\u{301}cole", "İstanbul", "ß ẞ", "K\u{212A}", "a\u{0}b", "Navi, İstanbul should be Ankara",
            "Navi, Please make that shorter Please", "Don’t forget the invoice.", "Hey!", "um", "you know", "I mean", ", like,", "full stop",
            "new line new line new line", "open quote close quote", "colon", "period period period", "😀 — 😀", "2–3–4", "a—b—c",
            "– – –", "#", "@", "@@", "http://", "x://y", "C:\\", "\\\\server\\share", long.as_str(),
        ];
        let words = vec![entry("Quikly", &["quickly", "quick ly"]), entry("ß", &["ss"])];
        let snippets = vec![Snippet { trigger: "my link".into(), expansion: "https://x.y\nz".into(), ..Default::default() }];
        let s = Settings::default();
        let ctx = AppContext { field_text: Some("ends mid".into()), ..Default::default() };
        for raw in samples {
            let _ = cleanup::spoken_punctuation(raw);
            let _ = cleanup::remove_fillers(raw);
            let _ = cleanup::tidy(raw);
            let _ = cleanup::tidy_spacing(raw);
            let _ = cleanup::no_em_dashes(raw);
            let _ = cleanup::apply_dictionary(raw, &words);
            let _ = cleanup::apply_snippets(raw, &snippets);
            let _ = cleanup::strip_cue_echo(raw, Some("Hey"));
            let _ = cleanup::is_hallucination(raw);
            let _ = britishise::britishise(raw);
            let _ = voice::parse(raw, false);
            let _ = voice::parse(raw, true);
            let _ = voice::apply_replace(raw, raw, "x");
            let _ = learn::find_corrections(raw, raw);
            let _ = learn::find_corrections(raw, "café Café crème brûlées");
            let _ = agent::candidates(raw, &["src/café.rs".to_string(), "日本/テスト.ts".to_string()], 3);
            let _ = ramble::offline_notes(raw);
            let _ = ramble::parse_notes(raw);
            for m in modes::builtin_modes() {
                let _ = modes::build_messages(&m, raw, &ctx, &s, &[raw.to_string()], &[raw.to_string()]);
                let out = run(PipelineInput { raw, mode: &m, ctx: &ctx, settings: &s, words: &words, snippets: &snippets, llm: None, project_files: &[] })
                    .await
                    .expect("pipeline");
                assert!(out.warnings.is_empty());
            }
        }
    }

    #[test]
    fn non_ascii_regressions() {
        use crate::text::{learn, ramble, voice};
        assert_eq!(learn::find_corrections("We met at the café today.", "We met at the Café today."), vec![("café".to_string(), "Café".to_string())]);
        assert!(learn::find_corrections("I love crème brûlée here", "I love crème brûlées here").is_empty());
        assert_eq!(voice::parse("Navi, Please make that shorter Please", false), Some(crate::types::VoiceCommand::Rewrite { instruction: "make that shorter".into() }));
        let n = ramble::offline_notes("Don’t forget the invoice. Then lunch.");
        assert_eq!(n.tasks.first().map(|t| t.text.as_str()), Some("The invoice"));
    }

    #[test]
    fn clean_ai_reply_for_voice_rewrites() {
        assert_eq!(clean_ai_reply("Here's the shorter version:\n\nSee you at five.", "I'll see you at five o'clock then.").as_deref(), Some("See you at five."));
        assert_eq!(clean_ai_reply("\"See you at five.\"", "see you at five o'clock").as_deref(), Some("See you at five."));
        assert_eq!(clean_ai_reply("Sure! I've made it shorter.", "see you at five o'clock"), None);
        assert_eq!(clean_ai_reply("Sure, see you then.", "Sure, see you at five then."), Some("Sure, see you then.".to_string()));
        assert_eq!(clean_ai_reply("   ", "x"), None);
    }

    #[test]
    fn guard_unit_checks() {
        let m = mode("message");
        assert!(reply_problem("Certainly. Hello.", "hello", &m).is_some());
        assert!(reply_problem("Hello.", "hello", &m).is_none());
        assert!(reply_problem("Surely not.", "surely not", &m).is_none());
        let p = mode("prompt");
        assert!(reply_problem("```rust\nfn main() {}\n```", "write a main function", &p).is_some());
        assert!(reply_problem("Goal: Fix the header.\nContext: It overlaps the logo.", "fix the header it overlaps the logo", &p).is_none());
        assert_eq!(unwrap_reply("```\nHello there.\n```", "hello there"), "Hello there.");
        assert_eq!(unwrap_reply("Hello there.\n\nLet me know if you'd like changes.", "hello there"), "Hello there.");
        assert_eq!(budget(&ModeKind::Clean, "a b c").1, 0.0);
    }

    #[test]
    fn the_speakers_own_closing_line_is_kept() {
        // "let me know" comes well past the first 80 characters of what was said.
        let said = "hi sam thanks for sending the proposal over I've had a look and it all seems fine let me know when you want to talk it through";
        let email = "Hi Sam,\n\nThanks for sending the proposal over. I've had a look and it all seems fine.\n\nLet me know when you want to talk it through.";
        assert_eq!(unwrap_reply(email, said), email);
        let help = "Here's the link to the new deck and the notes from Tuesday's meeting in case you missed it, hope this helps";
        let tidy = "Here's the link to the new deck and the notes from Tuesday's meeting, in case you missed it.\n\nI hope this helps.";
        assert_eq!(unwrap_reply(tidy, help), tidy);
        let ask = "the room is free on Thursday afternoon and Friday morning, do you want me to book it";
        let asked = "The room is free on Thursday afternoon and Friday morning.\n\nDo you want me to book it?";
        assert_eq!(unwrap_reply(asked, ask), asked);
        // An offer the speaker never made still goes.
        assert_eq!(unwrap_reply(&format!("{email}\n\nFeel free to ask for any changes."), said), email);
    }

    #[test]
    fn a_heading_the_speaker_said_is_kept() {
        let said = "so here are the dates we agreed monday the third and friday the seventh";
        let reply = "Here are the dates we agreed:\n- Monday the 3rd\n- Friday the 7th";
        assert_eq!(unwrap_reply(reply, said), reply);
        assert!(reply_problem(reply, said, &mode("message")).is_none());
        assert_eq!(clean_ai_reply(reply, "So here are the dates we agreed: Monday the 3rd and Friday the 7th.").as_deref(), Some(reply));
        // A preamble the AI added is still taken off, and an AI reply is still refused.
        assert_eq!(unwrap_reply("Here's the cleaned-up text:\nHere are the dates.", "here are the dates"), "Here are the dates.");
        assert!(reply_problem("Sure! The dates are Monday and Friday.", "what are the dates", &mode("message")).is_some());
        assert!(reply_problem("I can't make Friday.", "so I can't make Friday", &mode("message")).is_none());
    }

    #[tokio::test]
    async fn email_keeps_a_closing_paragraph_end_to_end() {
        let said = "hi sam thanks for sending the proposal over I've had a look and it all seems fine let me know when you want to talk it through";
        let email = "Hi Sam,\n\nThanks for sending the proposal over. I've had a look and it all seems fine.\n\nLet me know when you want to talk it through.";
        let (out, _) = run_with_ai(said, &mode("email"), vec![reply(email)]).await;
        assert!(out.used_ai, "{:?}", out.warnings);
        assert_eq!(out.text, email);
    }

    /// Every built-in mode on the same sentences, through the real AI step (Claude Code, Haiku, as
    /// the owner runs it), printing what each one gives back and whether the AI's answer was kept.
    /// Run by hand: cargo test --lib text::pipeline::tests::live_every_mode -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_every_mode() {
        let provider = std::env::var("NAVI_LIVE_PROVIDER").unwrap_or_else(|_| "claude-code".into());
        let model = std::env::var("NAVI_LIVE_MODEL").unwrap_or_else(|_| "haiku".into());
        let ai = AiSettings { provider: provider.clone(), model: model.clone(), ..Default::default() };
        let llm = Llm::from_settings(&ai, None, false).expect("an AI provider");
        let mut settings = Settings::default();
        settings.user_name = "Alex".into();
        // As whisper writes them: already punctuated, with the ums and the thinking aloud left in.
        let said = [
            ("casual", "Hi Sam, um, just wanted to say thanks for yesterday. The meeting went really well, and, er, can we do Thursday instead of Friday for the follow-up? Cheers."),
            ("coding", "Can you fix the login page so that when the password is wrong it shows a proper error message instead of just going blank? And keep the styling the same."),
            ("plan", "So I want to plan a birthday party for my mum. She's turning sixty in March, and she likes jazz and doesn't like big crowds, and we've got about a thousand pounds."),
        ];
        println!("provider {provider} / {model}");
        for (label, raw) in said {
            println!("\n=============== {label}: {raw}");
            for id in ["raw", "clean", "message", "email", "prompt", "notes"] {
                let m = mode(id);
                let t = std::time::Instant::now();
                let out = run(PipelineInput { raw, mode: &m, ctx: &AppContext::default(), settings: &settings, words: &[], snippets: &[], llm: Some(&llm), project_files: &[] })
                    .await
                    .expect("pipeline");
                println!("\n--- {id} ({:?}, ai {}, {} ms){}", m.kind, out.used_ai, t.elapsed().as_millis(), if out.warnings.is_empty() { String::new() } else { format!(" WARNINGS: {:?}", out.warnings) });
                println!("{}", out.text);
            }
        }
    }
}
