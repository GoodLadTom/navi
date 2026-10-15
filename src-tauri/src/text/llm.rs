//! Language model calls for the AI modes. Optional: Navi works fully without one.
//!
//! OWNER: text agent.
//!
//! Contract:
//! * Providers: "anthropic" (POST https://api.anthropic.com/v1/messages, headers x-api-key and
//!   anthropic-version: 2023-06-01, body {model, max_tokens, temperature, system, messages}),
//!   "openai" (https://api.openai.com/v1/chat/completions), "ollama" (base_url or
//!   http://localhost:11434, POST /api/chat with stream false), "custom" (any OpenAI-compatible
//!   base_url + /chat/completions, optional key).
//! * `from_settings` returns None when the provider is "off", when a cloud provider has no key, or
//!   when `local_only` is on and the provider is not local (ollama, or custom on localhost).
//! * Timeouts: 20 s for a short completion, more for a long reply (see `request_timeout`), and the
//!   callers' own deadlines on top. Errors are short and readable ("Claude rejected the API key").
//! * Never log keys or full prompts.
//!
//! Details:
//! * With `local_only`, Ollama and custom endpoints only count as local when their address is this
//!   machine (localhost, 127.x, ::1). An Ollama server elsewhere on the network is blocked too.
//! * A model left over from another provider (a "claude-..." model with OpenAI, say) is swapped for
//!   that provider's default, so switching provider in Settings never leaves a broken model.
//! * If a model refuses the temperature setting (some newer models only allow the default), the
//!   call is retried once without it. `<think>` blocks from reasoning models are removed.
//! * `Debug` never prints the key.

use crate::types::AiSettings;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// A short reply (a dictation's clean-up, the Settings test) gets this long. Call notes and Short
/// ideas ask for thousands of tokens, so each request also gets time per token of reply.
const TIMEOUT: Duration = Duration::from_secs(20);
const TIMEOUT_PER_TOKEN: Duration = Duration::from_millis(50);
/// A model on this computer may need loading first, and writes slowly without a big graphics card.
const LOCAL_TIMEOUT: Duration = Duration::from_secs(60);
const LOCAL_TIMEOUT_PER_TOKEN: Duration = Duration::from_millis(150);
const TIMEOUT_MAX: Duration = Duration::from_secs(600);
const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
const OPENAI_URL: &str = "https://api.openai.com/v1/chat/completions";
const OLLAMA_DEFAULT: &str = "http://localhost:11434";

#[derive(Clone)]
pub struct Llm {
    pub provider: String,
    pub model: String,
    pub base_url: Option<String>,
    key: Option<String>,
    /// Tests only: send anthropic/openai calls here instead of the real endpoint.
    endpoint_override: Option<String>,
}

impl std::fmt::Debug for Llm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Llm")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("key", &self.key.as_ref().map(|_| "<hidden>"))
            .finish()
    }
}

impl Llm {
    pub fn from_settings(ai: &AiSettings, key: Option<String>, local_only: bool) -> Option<Llm> {
        let provider = ai.provider.trim().to_ascii_lowercase();
        let key = key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty());
        let base_url = ai.base_url.as_deref().map(normalise_base_url).filter(|u| !u.is_empty());
        let model = ai.model.trim();
        let llm = |provider: &str, model: String, base_url: Option<String>, key: Option<String>| Llm {
            provider: provider.to_string(),
            model,
            base_url,
            key,
            endpoint_override: None,
        };
        match provider.as_str() {
            "anthropic" => {
                if local_only || key.is_none() {
                    return None;
                }
                let model = if model.starts_with("claude") { model.to_string() } else { default_model("anthropic").to_string() };
                Some(llm("anthropic", model, None, key))
            }
            "openai" => {
                if local_only || key.is_none() {
                    return None;
                }
                let model = if model.is_empty() || model.starts_with("claude") { default_model("openai").to_string() } else { model.to_string() };
                Some(llm("openai", model, None, key))
            }
            "ollama" => {
                let url = base_url.unwrap_or_else(|| OLLAMA_DEFAULT.to_string());
                if local_only && !is_loopback(&url) {
                    return None;
                }
                let foreign = model.is_empty() || model.starts_with("claude") || model.starts_with("gpt-");
                let model = if foreign { default_model("ollama").to_string() } else { model.to_string() };
                Some(llm("ollama", model, Some(url), None))
            }
            "custom" => {
                let url = base_url?;
                if model.is_empty() || (local_only && !is_loopback(&url)) {
                    return None;
                }
                Some(llm("custom", model.to_string(), Some(url), key))
            }
            // A subscription tool on this computer (no key): Claude Code or Codex. base_url may
            // hold a path to the tool if it lives somewhere unusual.
            p if crate::text::cli_ai::is_cli(p) => {
                if local_only {
                    return None;
                }
                Some(llm(p, crate::text::cli_ai::model_for(p, model), ai.base_url.clone().filter(|u| !u.trim().is_empty()), None))
            }
            _ => None,
        }
    }

    /// Subscription tools take a few seconds a call, so quick jobs (Clean's polish) skip them.
    pub fn is_slow(&self) -> bool {
        crate::text::cli_ai::is_cli(&self.provider)
    }

    pub async fn complete(&self, system: &str, user: &str, max_tokens: u32, temperature: f32) -> Result<String> {
        if crate::text::cli_ai::is_cli(&self.provider) {
            let started = Instant::now();
            let result = crate::text::cli_ai::complete(&self.provider, &self.model, system, user, self.base_url.as_deref()).await;
            log::debug!("AI call to {} took {} ms ({})", self.label(), started.elapsed().as_millis(), if result.is_ok() { "ok" } else { "failed" });
            return result.map(|reply| strip_thinking(&reply));
        }
        let client = http_client()?;
        let started = Instant::now();
        let temperature = temperature.clamp(0.0, 1.0);
        let max_tokens = max_tokens.max(1);
        let limit = self.request_timeout(max_tokens);
        let result = match self.send(client, system, user, max_tokens, Some(temperature), limit).await {
            Err(CallError::TemperatureRejected) => self.send(client, system, user, max_tokens, None, limit).await,
            other => other,
        };
        log::debug!("AI call to {} took {} ms ({})", self.label(), started.elapsed().as_millis(), if result.is_ok() { "ok" } else { "failed" });
        result.map_err(|e| anyhow!(self.describe(e, limit)))
    }

    /// How long one request may take. A model writes a few dozen tokens a second, so a reply of
    /// thousands of tokens (call notes, Short ideas) needs minutes where a dictation needs seconds.
    fn request_timeout(&self, max_tokens: u32) -> Duration {
        // Reasoning models think before they answer (see `body`).
        let tokens = if self.provider == "openai" && is_reasoning_model(&self.model) { max_tokens.saturating_add(2048) } else { max_tokens };
        let (base, per_token) = if self.provider == "ollama" || self.is_local() { (LOCAL_TIMEOUT, LOCAL_TIMEOUT_PER_TOKEN) } else { (TIMEOUT, TIMEOUT_PER_TOKEN) };
        (base + per_token * tokens).min(TIMEOUT_MAX)
    }

    /// A tiny round trip for the Settings "Test" button. Returns the model's reply.
    pub async fn test(&self) -> Result<String> {
        self.complete("Reply with the single word: ready", "Say it.", 10, 0.0).await
    }

    pub fn label(&self) -> String {
        format!("{} · {}", self.provider, self.model)
    }

    /// The name used in messages: "Claude", "OpenAI", "Ollama" or "The AI server".
    pub fn display_name(&self) -> &'static str {
        match self.provider.as_str() {
            "anthropic" => "Claude",
            "openai" => "OpenAI",
            "ollama" => "Ollama",
            "claude-code" => "Claude Code",
            "codex" => "Codex",
            _ => "The AI server",
        }
    }

    /// True when calls stay on this machine.
    pub fn is_local(&self) -> bool {
        matches!(self.provider.as_str(), "ollama" | "custom") && self.base_url.as_deref().is_some_and(is_loopback)
    }

    #[cfg(test)]
    pub(crate) fn with_endpoint(mut self, url: &str) -> Self {
        self.endpoint_override = Some(url.to_string());
        self
    }

    fn endpoint(&self) -> String {
        if let Some(url) = &self.endpoint_override {
            return url.clone();
        }
        match self.provider.as_str() {
            "anthropic" => ANTHROPIC_URL.to_string(),
            "openai" => OPENAI_URL.to_string(),
            "ollama" => format!("{}/api/chat", self.base_url.as_deref().unwrap_or(OLLAMA_DEFAULT)),
            _ => {
                let base = self.base_url.as_deref().unwrap_or_default();
                if base.ends_with("/chat/completions") {
                    base.to_string()
                } else {
                    format!("{base}/chat/completions")
                }
            }
        }
    }

    fn body(&self, system: &str, user: &str, max_tokens: u32, temperature: Option<f32>) -> Value {
        let messages = json!([{ "role": "system", "content": system }, { "role": "user", "content": user }]);
        match self.provider.as_str() {
            "anthropic" => {
                let mut b = json!({
                    "model": self.model,
                    "max_tokens": max_tokens,
                    "system": system,
                    "messages": [{ "role": "user", "content": user }],
                });
                if let Some(t) = temperature {
                    b["temperature"] = json!(t);
                }
                b
            }
            "openai" => {
                let reasoning = is_reasoning_model(&self.model);
                let mut b = json!({
                    "model": self.model,
                    "messages": messages,
                    "max_completion_tokens": if reasoning { max_tokens + 2048 } else { max_tokens },
                });
                if reasoning {
                    b["reasoning_effort"] = json!("low");
                } else if let Some(t) = temperature {
                    b["temperature"] = json!(t);
                }
                b
            }
            "ollama" => {
                let mut options = json!({ "num_predict": max_tokens });
                if let Some(t) = temperature {
                    options["temperature"] = json!(t);
                }
                // Ollama's default context would quietly cut a long transcript short.
                if let Some(n) = ollama_context(system.len() + user.len(), max_tokens) {
                    options["num_ctx"] = json!(n);
                }
                json!({ "model": self.model, "messages": messages, "stream": false, "options": options })
            }
            _ => {
                let mut b = json!({ "model": self.model, "messages": messages, "max_tokens": max_tokens, "stream": false });
                if let Some(t) = temperature {
                    b["temperature"] = json!(t);
                }
                b
            }
        }
    }

    async fn send(&self, client: &reqwest::Client, system: &str, user: &str, max_tokens: u32, temperature: Option<f32>, limit: Duration) -> std::result::Result<String, CallError> {
        let mut req = client.post(self.endpoint()).timeout(limit).json(&self.body(system, user, max_tokens, temperature));
        match (self.provider.as_str(), self.key.as_deref()) {
            ("anthropic", Some(key)) => {
                req = req.header("x-api-key", key).header("anthropic-version", "2023-06-01");
            }
            ("openai" | "custom", Some(key)) => req = req.bearer_auth(key),
            _ => {}
        }
        let resp = req.send().await.map_err(CallError::from_reqwest)?;
        let status = resp.status().as_u16();
        let text = resp.text().await.map_err(CallError::from_reqwest)?;
        if !(200..300).contains(&status) {
            if status == 400 && temperature.is_some() && api_error_message(&text).to_lowercase().contains("temperature") {
                return Err(CallError::TemperatureRejected);
            }
            return Err(CallError::Status { status, body: text });
        }
        let value: Value = serde_json::from_str(&text).map_err(|_| CallError::BadReply)?;
        let out = parse_reply(&self.provider, &value).ok_or(CallError::BadReply)?;
        let out = strip_thinking(&out);
        let out = out.trim();
        if out.is_empty() {
            return Err(CallError::Empty);
        }
        Ok(out.to_string())
    }

    fn describe(&self, e: CallError, limit: Duration) -> String {
        let who = self.display_name();
        match e {
            CallError::Timeout => format!("{who} took longer than {} seconds to reply.", limit.as_secs()),
            CallError::Connect => {
                if matches!(self.provider.as_str(), "ollama" | "custom") {
                    let at = self.base_url.as_deref().unwrap_or(OLLAMA_DEFAULT);
                    format!("Couldn't reach {who} at {at}. Is it running?")
                } else {
                    format!("No connection to {who}. Check your internet connection.")
                }
            }
            CallError::Empty => format!("{who} sent back an empty reply."),
            CallError::BadReply => format!("{who} sent a reply Navi couldn't read."),
            CallError::Other(m) => format!("Couldn't contact {who} ({m})."),
            CallError::TemperatureRejected => format!("{who} refused the request settings."),
            CallError::Status { status, body } => status_message(who, &self.provider, &self.model, status, &body),
        }
    }
}

#[derive(Debug)]
enum CallError {
    Status { status: u16, body: String },
    TemperatureRejected,
    Timeout,
    Connect,
    Empty,
    BadReply,
    Other(String),
}

impl CallError {
    fn from_reqwest(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            CallError::Timeout
        } else if e.is_connect() {
            CallError::Connect
        } else if e.is_builder() {
            CallError::Other("the address or key is not valid".into())
        } else if e.is_request() {
            CallError::Connect
        } else {
            CallError::Other("network error".into())
        }
    }
}

fn status_message(who: &str, provider: &str, model: &str, status: u16, body: &str) -> String {
    let detail = api_error_message(body);
    let lower = detail.to_lowercase();
    let model_missing = (lower.contains("model") || provider == "ollama")
        && (lower.contains("not found") || lower.contains("does not exist") || lower.contains("not_found") || lower.contains("unknown model") || lower.contains("invalid model"));
    let missing_model_msg = || {
        if provider == "ollama" {
            format!("Ollama doesn't have the model \"{model}\". Run: ollama pull {model}")
        } else {
            format!("{who} doesn't recognise the model \"{model}\".")
        }
    };
    match status {
        401 | 403 if provider == "ollama" => format!("{who} refused the request (status {status})."),
        401 | 403 => format!("{who} rejected the API key."),
        402 => format!("{who} says the account is out of credit."),
        429 if lower.contains("quota") || lower.contains("credit") || lower.contains("billing") => format!("{who} says the account is out of credit."),
        429 => format!("{who} is rate limiting requests. Wait a moment and try again."),
        404 => missing_model_msg(),
        400 | 422 if model_missing => missing_model_msg(),
        400 if lower.contains("credit balance") || lower.contains("billing") => format!("{who} says the account is out of credit."),
        408 => format!("{who} took too long to reply."),
        503 | 529 => format!("{who} is overloaded right now. Try again shortly."),
        500..=599 => format!("{who} had a server error ({status}). Try again shortly."),
        _ if detail.is_empty() => format!("{who} refused the request (status {status})."),
        _ => format!("{who} refused the request: {detail}"),
    }
}

/// The error text from a provider's JSON error body, shortened. Never includes the request.
fn api_error_message(body: &str) -> String {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let msg = v
        .pointer("/error/message")
        .and_then(Value::as_str)
        .or_else(|| v.get("error").and_then(Value::as_str))
        .or_else(|| v.get("message").and_then(Value::as_str))
        .or_else(|| v.get("detail").and_then(Value::as_str))
        .unwrap_or("");
    let one_line = msg.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() > 160 {
        one_line.chars().take(157).collect::<String>() + "..."
    } else {
        one_line
    }
}

fn parse_reply(provider: &str, v: &Value) -> Option<String> {
    match provider {
        "anthropic" => {
            let blocks = v.get("content")?.as_array()?;
            let text: String = blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect();
            Some(text)
        }
        "ollama" => v.pointer("/message/content").and_then(Value::as_str).map(str::to_string),
        _ => {
            let content = v.pointer("/choices/0/message/content")?;
            match content {
                Value::String(s) => Some(s.clone()),
                Value::Array(parts) => Some(parts.iter().filter_map(|p| p.get("text").and_then(Value::as_str)).collect()),
                _ => None,
            }
        }
    }
}

/// Removes `<think>...</think>` blocks that some local reasoning models put in their reply.
fn strip_thinking(s: &str) -> String {
    let mut out = s.to_string();
    while let Some(start) = out.find("<think>") {
        match out[start..].find("</think>") {
            Some(rel) => out.replace_range(start..start + rel + "</think>".len(), ""),
            None => {
                out.truncate(start);
                break;
            }
        }
    }
    out
}

fn is_reasoning_model(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") || m.starts_with("gpt-5")
}

/// The context window to ask Ollama for, or None when its default (4,096 tokens) is room enough.
/// A token is three or four characters; sizes go up in steps so the model is not reloaded for
/// every call.
fn ollama_context(prompt_bytes: usize, max_tokens: u32) -> Option<u32> {
    let needed = prompt_bytes / 3 + max_tokens as usize + 256;
    (needed > 4096).then(|| needed.next_power_of_two().clamp(8192, 32768) as u32)
}

fn default_model(provider: &str) -> &'static str {
    match provider {
        "anthropic" => "claude-haiku-4-5-20251001",
        "openai" => "gpt-4.1-mini",
        _ => "llama3.2",
    }
}

fn normalise_base_url(url: &str) -> String {
    let u = url.trim().trim_end_matches('/');
    if u.is_empty() || u.contains("://") {
        u.to_string()
    } else {
        format!("http://{u}")
    }
}

/// True when `url` points at this machine.
fn is_loopback(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(&normalise_base_url(url)) else {
        return false;
    };
    match parsed.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost") || d.to_ascii_lowercase().ends_with(".localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback() || ip.is_unspecified(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

fn http_client() -> Result<&'static reqwest::Client> {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    if let Some(c) = CLIENT.get() {
        return Ok(c);
    }
    // No overall timeout here: each request sets its own (see `request_timeout`).
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(8))
        .user_agent(concat!("Navi/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| anyhow!("Navi couldn't start its network client."))?;
    let _ = CLIENT.set(client);
    CLIENT.get().ok_or_else(|| anyhow!("Navi couldn't start its network client."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::testkit::{body_json, closed_port_url, mock_server};

    fn ai(provider: &str, model: &str, base_url: Option<&str>) -> AiSettings {
        AiSettings { provider: provider.into(), model: model.into(), base_url: base_url.map(str::to_string), ..Default::default() }
    }

    #[test]
    fn from_settings_rules() {
        let key = || Some("sk-test".to_string());
        assert!(Llm::from_settings(&ai("off", "x", None), key(), false).is_none());
        assert!(Llm::from_settings(&ai("anthropic", "claude-haiku-4-5-20251001", None), None, false).is_none());
        assert!(Llm::from_settings(&ai("anthropic", "claude-haiku-4-5-20251001", None), Some("  ".into()), false).is_none());
        assert!(Llm::from_settings(&ai("anthropic", "claude-haiku-4-5-20251001", None), key(), true).is_none());
        assert!(Llm::from_settings(&ai("openai", "gpt-4.1-mini", None), key(), true).is_none());
        assert!(Llm::from_settings(&ai("mystery", "x", None), key(), false).is_none());
        let a = Llm::from_settings(&ai("Anthropic", "claude-haiku-4-5-20251001", None), key(), false).expect("anthropic");
        assert_eq!(a.provider, "anthropic");
        assert!(!a.is_local());
        // Ollama is local by default and needs no key.
        let o = Llm::from_settings(&ai("ollama", "llama3.2", None), None, true).expect("ollama");
        assert_eq!(o.base_url.as_deref(), Some("http://localhost:11434"));
        assert!(o.is_local());
        assert!(Llm::from_settings(&ai("ollama", "llama3.2", Some("http://192.168.1.20:11434")), None, true).is_none());
        assert!(Llm::from_settings(&ai("ollama", "llama3.2", Some("http://192.168.1.20:11434")), None, false).is_some());
        // Custom needs a base URL and a model; local only when on this machine.
        assert!(Llm::from_settings(&ai("custom", "m", None), None, false).is_none());
        assert!(Llm::from_settings(&ai("custom", "", Some("http://localhost:1234/v1")), None, false).is_none());
        assert!(Llm::from_settings(&ai("custom", "m", Some("localhost:1234/v1/")), None, true).is_some());
        assert!(Llm::from_settings(&ai("custom", "m", Some("http://127.0.0.1:8080/v1")), None, true).is_some());
        assert!(Llm::from_settings(&ai("custom", "m", Some("https://api.groq.com/openai/v1")), key(), true).is_none());
    }

    #[test]
    fn leftover_models_are_swapped() {
        let key = || Some("k".to_string());
        assert_eq!(Llm::from_settings(&ai("openai", "claude-haiku-4-5-20251001", None), key(), false).map(|l| l.model), Some("gpt-4.1-mini".into()));
        assert_eq!(Llm::from_settings(&ai("anthropic", "gpt-4.1-mini", None), key(), false).map(|l| l.model), Some("claude-haiku-4-5-20251001".into()));
        assert_eq!(Llm::from_settings(&ai("ollama", "claude-haiku-4-5-20251001", None), None, false).map(|l| l.model), Some("llama3.2".into()));
    }

    #[test]
    fn debug_hides_the_key() {
        let l = Llm::from_settings(&ai("anthropic", "claude-haiku-4-5-20251001", None), Some("sk-secret-123".into()), false).expect("llm");
        let shown = format!("{l:?}");
        assert!(!shown.contains("sk-secret-123"));
        assert!(shown.contains("<hidden>"));
    }

    #[test]
    fn replies_are_parsed() {
        let a = json!({ "content": [{ "type": "text", "text": "Hello " }, { "type": "text", "text": "there." }] });
        assert_eq!(parse_reply("anthropic", &a).as_deref(), Some("Hello there."));
        let o = json!({ "choices": [{ "message": { "content": "Hi." } }] });
        assert_eq!(parse_reply("openai", &o).as_deref(), Some("Hi."));
        let parts = json!({ "choices": [{ "message": { "content": [{ "type": "text", "text": "A" }, { "type": "text", "text": "B" }] } }] });
        assert_eq!(parse_reply("custom", &parts).as_deref(), Some("AB"));
        let ol = json!({ "message": { "role": "assistant", "content": "Yo." } });
        assert_eq!(parse_reply("ollama", &ol).as_deref(), Some("Yo."));
        assert_eq!(strip_thinking("<think>hmm</think>Answer"), "Answer");
        assert_eq!(strip_thinking("Answer<think>unfinished"), "Answer");
    }

    #[test]
    fn long_replies_get_longer_timeouts() {
        let key = || Some("k".to_string());
        let claude = Llm::from_settings(&ai("anthropic", "claude-haiku-4-5-20251001", None), key(), false).expect("llm");
        // A dictation keeps the old 20 seconds, give or take its reply.
        assert!(claude.request_timeout(10) < Duration::from_secs(21));
        // Call notes and Short ideas (3,000 and 6,000 tokens) get minutes, not 20 seconds.
        assert!(claude.request_timeout(3000) >= Duration::from_secs(150));
        assert!(claude.request_timeout(6000) >= Duration::from_secs(300));
        assert!(claude.request_timeout(u32::MAX) <= TIMEOUT_MAX);
        // Reasoning models think first; a model on this computer is slower still.
        let o4 = Llm::from_settings(&ai("openai", "o4-mini", None), key(), false).expect("llm");
        let mini = Llm::from_settings(&ai("openai", "gpt-4.1-mini", None), key(), false).expect("llm");
        assert!(o4.request_timeout(100) > mini.request_timeout(100));
        let ollama = Llm::from_settings(&ai("ollama", "llama3.2", None), None, false).expect("llm");
        assert!(ollama.request_timeout(10) >= Duration::from_secs(60));
        assert!(ollama.request_timeout(3000) > claude.request_timeout(3000));
    }

    #[test]
    fn ollama_gets_room_for_long_transcripts() {
        assert_eq!(ollama_context(2_000, 500), None, "a dictation fits the default");
        assert_eq!(ollama_context(30_000, 3000), Some(16384));
        assert_eq!(ollama_context(200_000, 6000), Some(32768), "capped");
        let llm = Llm::from_settings(&ai("ollama", "llama3.2", None), None, false).expect("llm");
        assert!(llm.body("S", "short", 100, None)["options"].get("num_ctx").is_none());
        let long = "word ".repeat(4000);
        assert_eq!(llm.body("S", &long, 3000, None)["options"]["num_ctx"], 16384);
    }

    #[test]
    fn error_messages_are_short_and_readable() {
        let body = |m: &str| json!({ "type": "error", "error": { "type": "x", "message": m } }).to_string();
        assert_eq!(status_message("Claude", "anthropic", "m", 401, &body("invalid x-api-key")), "Claude rejected the API key.");
        assert!(status_message("Claude", "anthropic", "m", 429, &body("slow down")).contains("rate limiting"));
        assert!(status_message("OpenAI", "openai", "m", 429, &body("You exceeded your current quota")).contains("out of credit"));
        assert_eq!(status_message("Claude", "anthropic", "claude-x", 404, &body("model: claude-x")), "Claude doesn't recognise the model \"claude-x\".");
        assert!(status_message("Ollama", "ollama", "llama9", 404, r#"{"error":"model 'llama9' not found"}"#).contains("ollama pull llama9"));
        assert!(status_message("Claude", "anthropic", "m", 529, "").contains("overloaded"));
        assert!(status_message("Claude", "anthropic", "m", 400, &body("Your credit balance is too low")).contains("out of credit"));
        let long = "x ".repeat(200);
        assert!(status_message("Claude", "anthropic", "m", 400, &body(&long)).chars().count() < 220);
    }

    #[tokio::test]
    async fn anthropic_request_shape() {
        let (url, seen) = mock_server(vec![(200, json!({ "content": [{ "type": "text", "text": "ready" }] }).to_string())]).await;
        let llm = Llm::from_settings(&ai("anthropic", "claude-haiku-4-5-20251001", None), Some("test-key".into()), false)
            .expect("llm")
            .with_endpoint(&format!("{url}/v1/messages"));
        assert_eq!(llm.complete("SYS", "USER", 50, 0.2).await.expect("reply"), "ready");
        let reqs = seen.await.expect("server");
        let lower = reqs[0].to_lowercase();
        assert!(lower.starts_with("post /v1/messages"));
        assert!(lower.contains("x-api-key: test-key"));
        assert!(lower.contains("anthropic-version: 2023-06-01"));
        let body = body_json(&reqs[0]);
        assert_eq!(body["system"], "SYS");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "USER");
        assert_eq!(body["max_tokens"], 50);
        assert!(body.get("temperature").is_some());
    }

    #[tokio::test]
    async fn custom_endpoint_request_shape_and_auth() {
        let (url, seen) = mock_server(vec![(200, json!({ "choices": [{ "message": { "content": "Done." } }] }).to_string())]).await;
        let llm = Llm::from_settings(&ai("custom", "local-model", Some(&format!("{url}/v1"))), Some("abc".into()), true).expect("llm");
        assert_eq!(llm.complete("S", "U", 20, 0.0).await.expect("reply"), "Done.");
        let reqs = seen.await.expect("server");
        let lower = reqs[0].to_lowercase();
        assert!(lower.starts_with("post /v1/chat/completions"));
        assert!(lower.contains("authorization: bearer abc"));
        let body = body_json(&reqs[0]);
        assert_eq!(body["model"], "local-model");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "U");
        assert_eq!(body["max_tokens"], 20);
    }

    #[tokio::test]
    async fn ollama_request_shape_and_missing_model() {
        let (url, seen) = mock_server(vec![
            (200, json!({ "message": { "content": "<think>x</think>Fine." } }).to_string()),
            (404, json!({ "error": "model 'nope' not found, try pulling it first" }).to_string()),
        ])
        .await;
        let llm = Llm::from_settings(&ai("ollama", "qwen3", Some(&url)), None, true).expect("llm");
        assert_eq!(llm.complete("S", "U", 30, 0.1).await.expect("reply"), "Fine.");
        let missing = Llm::from_settings(&ai("ollama", "nope", Some(&url)), None, true).expect("llm");
        let err = missing.complete("S", "U", 30, 0.1).await.expect_err("missing model").to_string();
        assert!(err.contains("ollama pull nope"), "{err}");
        let reqs = seen.await.expect("server");
        assert!(reqs[0].to_lowercase().starts_with("post /api/chat"));
        let body = body_json(&reqs[0]);
        assert_eq!(body["stream"], false);
        assert_eq!(body["options"]["num_predict"], 30);
    }

    #[tokio::test]
    async fn bad_key_and_rate_limit_errors() {
        let err_body = json!({ "type": "error", "error": { "type": "authentication_error", "message": "invalid x-api-key" } }).to_string();
        let (url, _seen) = mock_server(vec![(401, err_body), (429, json!({ "error": { "message": "slow" } }).to_string())]).await;
        let llm = Llm::from_settings(&ai("anthropic", "claude-haiku-4-5-20251001", None), Some("bad".into()), false)
            .expect("llm")
            .with_endpoint(&url);
        let e1 = llm.complete("S", "U", 5, 0.0).await.expect_err("401").to_string();
        assert_eq!(e1, "Claude rejected the API key.");
        let e2 = llm.complete("S", "U", 5, 0.0).await.expect_err("429").to_string();
        assert!(e2.contains("rate limiting"), "{e2}");
    }

    #[tokio::test]
    async fn temperature_refusal_is_retried_without_it() {
        let refusal = json!({ "error": { "message": "Unsupported value: 'temperature' does not support 0.2 with this model." } }).to_string();
        let (url, seen) = mock_server(vec![(400, refusal), (200, json!({ "choices": [{ "message": { "content": "ok" } }] }).to_string())]).await;
        let llm = Llm::from_settings(&ai("custom", "m", Some(&url)), None, false).expect("llm");
        assert_eq!(llm.complete("S", "U", 5, 0.2).await.expect("retry"), "ok");
        let reqs = seen.await.expect("server");
        assert!(body_json(&reqs[0]).get("temperature").is_some());
        assert!(body_json(&reqs[1]).get("temperature").is_none());
    }

    #[tokio::test]
    async fn unreachable_local_server() {
        let url = closed_port_url().await;
        let llm = Llm::from_settings(&ai("custom", "m", Some(&url)), None, true).expect("llm");
        let err = llm.complete("S", "U", 5, 0.0).await.expect_err("no server").to_string();
        assert!(err.starts_with("Couldn't reach The AI server at"), "{err}");
    }

    /// One real call, only when NAVI_LIVE_TESTS=1 and ANTHROPIC_API_KEY are both set.
    #[tokio::test]
    async fn live_anthropic_round_trip() {
        let (Ok(flag), Ok(key)) = (std::env::var("NAVI_LIVE_TESTS"), std::env::var("ANTHROPIC_API_KEY")) else {
            return;
        };
        if flag != "1" || key.trim().is_empty() {
            return;
        }
        let llm = Llm::from_settings(&ai("anthropic", "claude-haiku-4-5-20251001", None), Some(key), false).expect("llm");
        let reply = llm.test().await.expect("live reply");
        assert!(reply.to_lowercase().contains("ready"), "unexpected reply: {reply}");
    }
}
