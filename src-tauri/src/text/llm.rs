//! Language model calls for the AI modes. Optional: Navi works fully without one.
//!
//! OWNER: text agent.
//!
//! Contract:
//! * Providers: "anthropic" (POST https://api.anthropic.com/v1/messages, headers x-api-key and
//!   anthropic-version: 2023-06-01, body {model, max_tokens, temperature, system, messages}),
//!   "openai" (https://api.openai.com/v1/chat/completions), "ollama" (base_url or
//!   http://127.0.0.1:11434, POST /api/chat with stream false), "custom" (any OpenAI-compatible
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
//! * Ollama: Navi asks for 127.0.0.1, not localhost (Ollama listens on IPv4 only, and every new
//!   connection to localhost tried ::1 first). Calls send `keep_alive` (OLLAMA_KEEP_ALIVE) so the
//!   model stays loaded between takes, and quick jobs send `think: false` (see `think`), retried
//!   without it if the model refuses. `ollama_status` reads /api/tags for Settings.
//! * A server on this computer gets a one-second connection timeout: Windows takes about two
//!   seconds to refuse a connection to a port nobody listens on. When one didn't answer, calls to
//!   it for the next minute (DOWN_FOR) fail at once, so takes go straight to the offline tidy; the
//!   Settings Test button and status check always try again.
//! * `Debug` never prints the key.

use crate::types::AiSettings;
use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// A short reply (a dictation's clean-up, the Settings test) gets this long. Call notes and screen
/// watch write-ups ask for thousands of tokens, so each request also gets time per token of reply.
const TIMEOUT: Duration = Duration::from_secs(20);
const TIMEOUT_PER_TOKEN: Duration = Duration::from_millis(50);
/// A model on this computer may need loading first, and writes slowly without a big graphics card.
const LOCAL_TIMEOUT: Duration = Duration::from_secs(60);
const LOCAL_TIMEOUT_PER_TOKEN: Duration = Duration::from_millis(150);
const TIMEOUT_MAX: Duration = Duration::from_secs(600);
const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
const OPENAI_URL: &str = "https://api.openai.com/v1/chat/completions";
const OLLAMA_DEFAULT: &str = "http://127.0.0.1:11434";
/// A server on this computer takes the connection at once if it is running.
const LOCAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
/// After a server on this computer didn't answer, calls skip it for this long.
const DOWN_FOR: Duration = Duration::from_secs(60);
/// How long Ollama keeps the model loaded after a call. Its own default is five minutes, after
/// which the next take waits for the model to load again.
const OLLAMA_KEEP_ALIVE: &str = "30m";
/// Room for a model that thinks before it answers, when it is left to (see `think`).
const THINKING_TOKENS: u32 = 2048;
/// How long Settings waits for Ollama's list of models.
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct Llm {
    pub provider: String,
    pub model: String,
    pub base_url: Option<String>,
    /// Ollama's `think`: Some(false) for jobs someone waits on (a dictation, a voice edit), as a
    /// model that thinks first (qwen3, deepseek-r1) takes far longer. None leaves it to the model,
    /// for jobs nobody waits on (`for_long_jobs`).
    think: Option<bool>,
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
            think: (provider == "ollama").then_some(false),
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

    /// For jobs nobody sits waiting on (call notes, screen watches): a model that thinks before
    /// it answers may.
    pub fn for_long_jobs(mut self) -> Self {
        self.think = None;
        self
    }

    pub async fn complete(&self, system: &str, user: &str, max_tokens: u32, temperature: f32) -> Result<String> {
        if crate::text::cli_ai::is_cli(&self.provider) {
            let started = Instant::now();
            let result = crate::text::cli_ai::complete(&self.provider, &self.model, system, user, self.base_url.as_deref()).await;
            log::debug!("AI call to {} took {} ms ({})", self.label(), started.elapsed().as_millis(), if result.is_ok() { "ok" } else { "failed" });
            return result.map(|reply| strip_thinking(&reply));
        }
        let local = self.is_local();
        let client = http_client(local)?;
        let started = Instant::now();
        let temperature = temperature.clamp(0.0, 1.0);
        let max_tokens = max_tokens.max(1);
        let limit = self.request_timeout(max_tokens);
        // A server on this computer that didn't answer a moment ago isn't asked again yet.
        let endpoint = self.endpoint();
        if local && recently_down(&endpoint) {
            log::debug!("AI call to {} skipped: it didn't answer a moment ago", self.label());
            return Err(anyhow!(self.describe(CallError::Connect, limit)));
        }
        let mut opts = SendOpts { temperature: Some(temperature), think: self.think };
        let result = loop {
            match self.send(client, system, user, max_tokens, opts, limit).await {
                Err(CallError::TemperatureRejected) if opts.temperature.is_some() => opts.temperature = None,
                Err(CallError::ThinkRejected) if opts.think.is_some() => opts.think = None,
                other => break other,
            }
        };
        if local {
            note_reachable(&endpoint, !matches!(result, Err(CallError::Connect)));
        }
        log::debug!("AI call to {} took {} ms ({})", self.label(), started.elapsed().as_millis(), if result.is_ok() { "ok" } else { "failed" });
        result.map_err(|e| anyhow!(self.describe(e, limit)))
    }

    /// How long one request may take. A model writes a few dozen tokens a second, so a reply of
    /// thousands of tokens (call notes, jobs on calls) needs minutes where a dictation needs seconds.
    fn request_timeout(&self, max_tokens: u32) -> Duration {
        // Reasoning models think before they answer (see `body`).
        let tokens = if self.provider == "openai" && is_reasoning_model(&self.model) {
            max_tokens.saturating_add(2048)
        } else if self.provider == "ollama" {
            ollama_predict(max_tokens, self.think)
        } else {
            max_tokens
        };
        let (base, per_token) = if self.provider == "ollama" || self.is_local() { (LOCAL_TIMEOUT, LOCAL_TIMEOUT_PER_TOKEN) } else { (TIMEOUT, TIMEOUT_PER_TOKEN) };
        (base + per_token * tokens).min(TIMEOUT_MAX)
    }

    /// A tiny round trip for the Settings "Test" button. Returns the model's reply. It always
    /// asks, even a server that didn't answer a moment ago: it may have just been started.
    pub async fn test(&self) -> Result<String> {
        if self.is_local() {
            note_reachable(&self.endpoint(), true);
        }
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

    fn body(&self, system: &str, user: &str, max_tokens: u32, opts: SendOpts) -> Value {
        let temperature = opts.temperature;
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
                let predict = ollama_predict(max_tokens, opts.think);
                let mut options = json!({ "num_predict": predict });
                if let Some(t) = temperature {
                    options["temperature"] = json!(t);
                }
                // Ollama's default context would quietly cut a long transcript short.
                if let Some(n) = ollama_context(system.len() + user.len(), predict) {
                    options["num_ctx"] = json!(n);
                }
                let mut b = json!({ "model": self.model, "messages": messages, "stream": false, "keep_alive": OLLAMA_KEEP_ALIVE, "options": options });
                if let Some(think) = opts.think {
                    b["think"] = json!(think);
                }
                b
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

    async fn send(&self, client: &reqwest::Client, system: &str, user: &str, max_tokens: u32, opts: SendOpts, limit: Duration) -> std::result::Result<String, CallError> {
        let mut req = client.post(self.endpoint()).timeout(limit).json(&self.body(system, user, max_tokens, opts));
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
            let detail = api_error_message(&text).to_lowercase();
            if status == 400 && opts.temperature.is_some() && detail.contains("temperature") {
                return Err(CallError::TemperatureRejected);
            }
            // An Ollama too old for `think`, or a model that can't stop thinking.
            if status == 400 && opts.think.is_some() && detail.contains("think") {
                return Err(CallError::ThinkRejected);
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
            // The first sentence says what is wrong on its own: the bubble shows only that.
            CallError::Connect => {
                let at = self.base_url.as_deref().unwrap_or(OLLAMA_DEFAULT);
                match self.provider.as_str() {
                    "ollama" if self.is_local() => format!("Ollama isn't installed or isn't running. Nothing answered at {at}."),
                    "ollama" => format!("Couldn't reach Ollama. Is it running at {at}?"),
                    "custom" => format!("Couldn't reach the AI server. Is it running at {at}?"),
                    _ => format!("No connection to {who}. Check your internet connection."),
                }
            }
            CallError::Empty => format!("{who} sent back an empty reply."),
            CallError::BadReply => format!("{who} sent a reply Navi couldn't read."),
            CallError::Other(m) => format!("Couldn't contact {who} ({m})."),
            CallError::TemperatureRejected | CallError::ThinkRejected => format!("{who} refused the request settings."),
            CallError::Status { status, body } => status_message(who, &self.provider, &self.model, status, &body),
        }
    }
}

#[derive(Debug)]
enum CallError {
    Status { status: u16, body: String },
    TemperatureRejected,
    ThinkRejected,
    Timeout,
    Connect,
    Empty,
    BadReply,
    Other(String),
}

/// What one request asks for beyond the messages. Each is dropped in turn when the server refuses it.
#[derive(Clone, Copy, Debug)]
struct SendOpts {
    temperature: Option<f32>,
    /// Ollama only (see `Llm::think`).
    think: Option<bool>,
}

impl CallError {
    fn from_reqwest(e: reqwest::Error) -> Self {
        // A connection that timed out is a connection error (a server on this computer that isn't
        // running, see LOCAL_CONNECT_TIMEOUT), not a slow reply.
        if e.is_connect() {
            CallError::Connect
        } else if e.is_timeout() {
            CallError::Timeout
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
        // Only a missing model when the answer names it: a 404 that doesn't ("404 page not
        // found") came from a wrong address.
        404 if names_model(&lower, model) => missing_model_msg(),
        404 if matches!(provider, "ollama" | "custom") => format!("{who} found nothing at that address (status 404). Check the address in Settings > AI."),
        400 | 422 if model_missing => missing_model_msg(),
        400 if lower.contains("credit balance") || lower.contains("billing") => format!("{who} says the account is out of credit."),
        408 => format!("{who} took too long to reply."),
        503 | 529 => format!("{who} is overloaded right now. Try again shortly."),
        500..=599 => format!("{who} had a server error ({status}). Try again shortly."),
        _ if detail.is_empty() => format!("{who} refused the request (status {status})."),
        _ => format!("{who} refused the request: {detail}"),
    }
}

/// Does an error's text (lower case) name `model`? Ollama answers "model 'llama9' not found" for
/// "llama9" and for "llama9:latest" alike.
fn names_model(detail_lower: &str, model: &str) -> bool {
    let m = model.trim().to_lowercase();
    let name = m.strip_suffix(":latest").unwrap_or(&m);
    !name.is_empty() && detail_lower.contains(name)
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

/// Ollama's `num_predict`: a model left to think counts its thinking in it too.
fn ollama_predict(max_tokens: u32, think: Option<bool>) -> u32 {
    if think == Some(false) {
        max_tokens
    } else {
        max_tokens.saturating_add(THINKING_TOKENS)
    }
}

/// Servers on this computer that didn't take a connection, and when (see DOWN_FOR).
static DOWN: parking_lot::Mutex<Vec<(String, Instant)>> = parking_lot::const_mutex(Vec::new());

/// True when the server at `endpoint` didn't take a connection within the last DOWN_FOR.
fn recently_down(endpoint: &str) -> bool {
    let mut down = DOWN.lock();
    down.retain(|(_, at)| at.elapsed() < DOWN_FOR);
    down.iter().any(|(e, _)| e == endpoint)
}

/// Remembers whether the server at `endpoint` took the connection.
fn note_reachable(endpoint: &str, reachable: bool) {
    let mut down = DOWN.lock();
    down.retain(|(e, at)| e != endpoint && at.elapsed() < DOWN_FOR);
    if !reachable {
        down.push((endpoint.to_string(), Instant::now()));
    }
}

/// Settings > AI with Ollama: whether it answers, and the models it has pulled.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OllamaStatus {
    /// The address asked: the one in Settings, or 127.0.0.1:11434.
    pub address: String,
    pub running: bool,
    /// Models that can chat, newest first as Ollama lists them, ":latest" left off the name.
    pub models: Vec<OllamaModel>,
    /// Why Navi couldn't list them; empty while Ollama answers.
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OllamaModel {
    pub name: String,
    /// On disk, in bytes.
    pub size: u64,
}

/// Ollama's address from Settings (`ai.base_url`), or the default.
pub fn ollama_address(base_url: Option<&str>) -> String {
    base_url.map(normalise_base_url).filter(|u| !u.is_empty()).unwrap_or_else(|| OLLAMA_DEFAULT.to_string())
}

/// Asks Ollama at `base_url` for its models (GET /api/tags). With `local_only`, an address off
/// this computer isn't asked. Also tells the dictation calls whether Ollama is there.
pub async fn ollama_status(base_url: Option<&str>, local_only: bool) -> OllamaStatus {
    let address = ollama_address(base_url);
    let local = is_loopback(&address);
    if local_only && !local {
        return OllamaStatus { address, message: "Local only is on, so Navi won't ask Ollama at an address off this computer.".into(), ..Default::default() };
    }
    let client = match http_client(local) {
        Ok(c) => c,
        Err(e) => return OllamaStatus { address, message: e.to_string(), ..Default::default() },
    };
    let chat = format!("{address}/api/chat");
    let reply: std::result::Result<Vec<OllamaModel>, CallError> = async {
        let resp = client.get(format!("{address}/api/tags")).timeout(STATUS_TIMEOUT).send().await.map_err(CallError::from_reqwest)?;
        let status = resp.status().as_u16();
        let text = resp.text().await.map_err(CallError::from_reqwest)?;
        if !(200..300).contains(&status) {
            return Err(CallError::Status { status, body: text });
        }
        let value: Value = serde_json::from_str(&text).map_err(|_| CallError::BadReply)?;
        parse_tags(&value).ok_or(CallError::BadReply)
    }
    .await;
    if local {
        note_reachable(&chat, !matches!(reply, Err(CallError::Connect)));
    }
    match reply {
        Ok(models) => OllamaStatus { address, running: true, models, message: String::new() },
        Err(e) => {
            let message = match e {
                CallError::Connect if local => "Ollama isn't installed or isn't running.".to_string(),
                CallError::Connect => "Navi couldn't reach Ollama at that address.".to_string(),
                CallError::Timeout => "Ollama didn't answer in time.".to_string(),
                CallError::Status { .. } | CallError::BadReply => "Something answered at that address, but it isn't Ollama.".to_string(),
                _ => "Navi couldn't ask Ollama for its models.".to_string(),
            };
            OllamaStatus { address, message, ..Default::default() }
        }
    }
}

/// The models in an /api/tags reply. Embedding models (they can't chat) are left out when Ollama
/// says what each model can do. None when the reply isn't Ollama's.
fn parse_tags(v: &Value) -> Option<Vec<OllamaModel>> {
    let list = v.get("models")?.as_array()?;
    let models = list
        .iter()
        .filter(|m| {
            let caps = m.get("capabilities").and_then(Value::as_array);
            caps.is_none_or(|c| c.iter().any(|x| x.as_str() == Some("completion")))
        })
        .filter_map(|m| {
            let name = m.get("name").or_else(|| m.get("model")).and_then(Value::as_str)?.trim();
            let name = name.strip_suffix(":latest").unwrap_or(name);
            (!name.is_empty()).then(|| OllamaModel { name: name.to_string(), size: m.get("size").and_then(Value::as_u64).unwrap_or(0) })
        })
        .collect();
    Some(models)
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

/// The network client: `local` for a server on this computer, which gets LOCAL_CONNECT_TIMEOUT.
fn http_client(local: bool) -> Result<&'static reqwest::Client> {
    static REMOTE: OnceLock<reqwest::Client> = OnceLock::new();
    static LOCAL: OnceLock<reqwest::Client> = OnceLock::new();
    let cell = if local { &LOCAL } else { &REMOTE };
    if let Some(c) = cell.get() {
        return Ok(c);
    }
    // No overall timeout here: each request sets its own (see `request_timeout`).
    let mut builder = reqwest::Client::builder()
        .connect_timeout(if local { LOCAL_CONNECT_TIMEOUT } else { Duration::from_secs(8) })
        .user_agent(concat!("Navi/", env!("CARGO_PKG_VERSION")));
    if local {
        // A proxy set for the internet must never stand between Navi and this computer.
        builder = builder.no_proxy();
    }
    let client = builder.build().map_err(|_| anyhow!("Navi couldn't start its network client."))?;
    let _ = cell.set(client);
    cell.get().ok_or_else(|| anyhow!("Navi couldn't start its network client."))
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
        // 127.0.0.1, not localhost: Ollama listens on IPv4 only, and localhost tried ::1 first.
        assert_eq!(o.base_url.as_deref(), Some("http://127.0.0.1:11434"));
        assert_eq!(o.endpoint(), "http://127.0.0.1:11434/api/chat");
        assert!(o.is_local());
        assert_eq!(ollama_address(None), "http://127.0.0.1:11434");
        assert_eq!(ollama_address(Some(" localhost:11434/ ")), "http://localhost:11434");
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
        // Long replies (call notes ask for 3,000 tokens) get minutes, not 20 seconds.
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
        let opts = SendOpts { temperature: None, think: llm.think };
        assert!(llm.body("S", "short", 100, opts)["options"].get("num_ctx").is_none());
        let long = "word ".repeat(4000);
        assert_eq!(llm.body("S", &long, 3000, opts)["options"]["num_ctx"], 16384);
    }

    #[test]
    fn ollama_keeps_the_model_loaded_and_quick_jobs_dont_think() {
        let llm = Llm::from_settings(&ai("ollama", "qwen3:8b", None), None, false).expect("llm");
        let body = llm.body("S", "U", 300, SendOpts { temperature: Some(0.2), think: llm.think });
        assert_eq!(body["keep_alive"], "30m");
        assert_eq!(body["think"], false);
        assert_eq!(body["options"]["num_predict"], 300);
        // Jobs nobody waits on leave thinking to the model, with room for it.
        let long = llm.clone().for_long_jobs();
        let body = long.body("S", "U", 300, SendOpts { temperature: None, think: long.think });
        assert!(body.get("think").is_none());
        assert_eq!(body["keep_alive"], "30m");
        assert_eq!(body["options"]["num_predict"], 300 + THINKING_TOKENS);
        assert!(long.request_timeout(300) > llm.request_timeout(300));
        // Only Ollama is sent `think`.
        let custom = Llm::from_settings(&ai("custom", "m", Some("http://127.0.0.1:1234/v1")), None, false).expect("llm");
        assert!(custom.body("S", "U", 10, SendOpts { temperature: None, think: custom.think }).get("think").is_none());
    }

    #[test]
    fn error_messages_are_short_and_readable() {
        let body = |m: &str| json!({ "type": "error", "error": { "type": "x", "message": m } }).to_string();
        assert_eq!(status_message("Claude", "anthropic", "m", 401, &body("invalid x-api-key")), "Claude rejected the API key.");
        assert!(status_message("Claude", "anthropic", "m", 429, &body("slow down")).contains("rate limiting"));
        assert!(status_message("OpenAI", "openai", "m", 429, &body("You exceeded your current quota")).contains("out of credit"));
        assert_eq!(status_message("Claude", "anthropic", "claude-x", 404, &body("model: claude-x")), "Claude doesn't recognise the model \"claude-x\".");
        assert!(status_message("Ollama", "ollama", "llama9", 404, r#"{"error":"model 'llama9' not found"}"#).contains("ollama pull llama9"));
        assert!(status_message("Ollama", "ollama", "llama9:latest", 404, r#"{"error":"model 'llama9' not found"}"#).contains("ollama pull"));
        // A 404 that doesn't name the model is a wrong address (a /v1 left on the end), not a
        // model to pull.
        let wrong = status_message("Ollama", "ollama", "llama3.2", 404, "404 page not found");
        assert_eq!(wrong, "Ollama found nothing at that address (status 404). Check the address in Settings > AI.");
        assert!(!status_message("The AI server", "custom", "m", 404, r#"{"error":"not found"}"#).contains("recognise the model"));
        assert!(!status_message("Claude", "anthropic", "claude-x", 404, &body("not found")).contains("recognise the model"));
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
        assert_eq!(body["keep_alive"], "30m");
        assert_eq!(body["think"], false);
    }

    #[tokio::test]
    async fn a_model_that_refuses_think_is_asked_again_without_it() {
        let refusal = json!({ "error": "think value \"false\" is not supported for this model" }).to_string();
        let (url, seen) = mock_server(vec![(400, refusal), (200, json!({ "message": { "content": "ok" } }).to_string())]).await;
        let llm = Llm::from_settings(&ai("ollama", "gpt-oss", Some(&url)), None, true).expect("llm");
        assert_eq!(llm.complete("S", "U", 5, 0.2).await.expect("retry"), "ok");
        let reqs = seen.await.expect("server");
        assert_eq!(body_json(&reqs[0])["think"], false);
        assert!(body_json(&reqs[1]).get("think").is_none());
        assert!(body_json(&reqs[1])["options"].get("temperature").is_some(), "only think was dropped");
    }

    #[tokio::test]
    async fn ollama_not_running_fails_fast_and_later_takes_skip_it() {
        let url = closed_port_url().await;
        let llm = Llm::from_settings(&ai("ollama", "llama3.2", Some(&url)), None, true).expect("llm");
        // Windows takes about two seconds to refuse a connection to a closed port.
        let t = Instant::now();
        let err = llm.complete("S", "U", 5, 0.0).await.expect_err("nothing there").to_string();
        assert!(t.elapsed() < Duration::from_millis(1800), "gave up after {:?}", t.elapsed());
        assert_eq!(err, format!("Ollama isn't installed or isn't running. Nothing answered at {url}."));
        // The next take within the minute doesn't wait to be refused again.
        let t = Instant::now();
        let again = llm.complete("S", "U", 5, 0.0).await.expect_err("still nothing").to_string();
        assert!(t.elapsed() < Duration::from_millis(100), "waited {:?}", t.elapsed());
        assert_eq!(again, err);
    }

    #[tokio::test]
    async fn a_remembered_failure_is_skipped_until_the_test_button_asks_again() {
        let ok = |s: &str| (200, json!({ "message": { "content": s } }).to_string());
        let (url, seen) = mock_server(vec![ok("ready"), ok("Fine.")]).await;
        let llm = Llm::from_settings(&ai("ollama", "llama3.2", Some(&url)), None, true).expect("llm");
        note_reachable(&llm.endpoint(), false);
        assert!(llm.complete("S", "U", 5, 0.0).await.is_err(), "skipped without asking");
        // Ollama was just started: Test asks whatever happened a moment ago, and takes follow.
        assert_eq!(llm.test().await.expect("asked again"), "ready");
        assert_eq!(llm.complete("S", "U", 5, 0.0).await.expect("reachable again"), "Fine.");
        assert_eq!(seen.await.expect("server").len(), 2);
    }

    #[test]
    fn tags_list_the_models_that_can_chat() {
        let v = json!({ "models": [
            { "name": "llama3.2:latest", "size": 2019393189u64, "capabilities": ["completion", "tools"] },
            { "name": "nomic-embed-text:latest", "size": 274302450u64, "capabilities": ["embedding"] },
            { "name": "qwen3:8b", "size": 5225388164u64 },
        ] });
        let models = parse_tags(&v).expect("tags");
        assert_eq!(models, vec![OllamaModel { name: "llama3.2".into(), size: 2019393189 }, OllamaModel { name: "qwen3:8b".into(), size: 5225388164 }]);
        assert_eq!(parse_tags(&json!({ "models": [] })), Some(Vec::new()));
        assert_eq!(parse_tags(&json!({ "data": [] })), None, "not Ollama");
    }

    #[tokio::test]
    async fn ollama_status_says_whether_it_runs_and_what_it_has() {
        let tags = json!({ "models": [{ "name": "llama3.2:latest", "size": 2019393189u64 }] }).to_string();
        let (url, seen) = mock_server(vec![(200, tags), (404, "404 page not found".into())]).await;
        let s = ollama_status(Some(&url), true).await;
        assert!(s.running, "{s:?}");
        assert_eq!((s.address.as_str(), s.models.len(), s.models[0].name.as_str()), (url.as_str(), 1, "llama3.2"));
        assert!(s.message.is_empty());
        let not_ollama = ollama_status(Some(&url), true).await;
        assert!(!not_ollama.running);
        assert_eq!(not_ollama.message, "Something answered at that address, but it isn't Ollama.");
        let reqs = seen.await.expect("server");
        assert!(reqs[0].to_lowercase().starts_with("get /api/tags"));

        // Not running: said quickly, and takes skip it for a minute.
        let closed = closed_port_url().await;
        let t = Instant::now();
        let down = ollama_status(Some(&closed), false).await;
        assert!(t.elapsed() < Duration::from_millis(1800), "took {:?}", t.elapsed());
        assert!(!down.running && down.models.is_empty());
        assert_eq!(down.message, "Ollama isn't installed or isn't running.");
        assert!(recently_down(&format!("{closed}/api/chat")));

        // Local only never asks an address off this computer.
        let away = ollama_status(Some("http://192.168.1.20:11434"), true).await;
        assert!(!away.running);
        assert!(away.message.starts_with("Local only is on"), "{}", away.message);
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
        assert_eq!(err, format!("Couldn't reach the AI server. Is it running at {url}?"));
    }

    /// Settings' status check and one Test, against the Ollama on this computer. Run by hand:
    /// cargo test --lib text::llm::tests::live_ollama -- --ignored --nocapture
    /// (NAVI_LIVE_MODEL picks the model, llama3.2 by default).
    #[tokio::test]
    #[ignore]
    async fn live_ollama() {
        let model = std::env::var("NAVI_LIVE_MODEL").unwrap_or_else(|_| "llama3.2".into());
        let t = Instant::now();
        let status = ollama_status(None, true).await;
        println!("status in {} ms: {status:?}", t.elapsed().as_millis());
        assert!(status.running, "{}", status.message);
        let llm = Llm::from_settings(&ai("ollama", &model, None), None, true).expect("llm");
        for _ in 0..3 {
            let t = Instant::now();
            let reply = llm.test().await.expect("live reply");
            println!("test in {} ms: {reply}", t.elapsed().as_millis());
        }
        // Ramble notes, as Navi asks for them (a dictation's settings: no thinking).
        let said = "Right, so for the launch next week I need to finish the pricing page by Wednesday, ask Sam to check the Stripe products, and we decided to drop the Team plan for now. Oh and book the photographer for Friday.";
        let (system, user) = crate::text::ramble::notes_messages(said, &crate::types::AppContext::default(), &crate::types::Settings::default());
        let t = Instant::now();
        let reply = llm.complete(&system, &user, 2500, 0.2).await.expect("notes");
        let notes = crate::text::ramble::parse_notes(&reply);
        println!("ramble notes in {} ms ({}): {:?}", t.elapsed().as_millis(), if notes.is_ok() { "parsed" } else { "didn't parse" }, notes.map(|n| (n.title, n.tasks.len())));
        // Not running: a port nothing listens on, twice.
        let closed = Llm::from_settings(&ai("ollama", &model, Some(&closed_port_url().await)), None, true).expect("llm");
        for _ in 0..2 {
            let t = Instant::now();
            let err = closed.complete("S", "U", 5, 0.0).await.expect_err("nothing there");
            println!("not running, gave up in {} ms: {err}", t.elapsed().as_millis());
        }
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
