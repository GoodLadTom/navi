//! AI through the command-line tools people already pay for, so no API key is needed: Claude Code
//! (signed in with a Claude plan) and Codex (signed in with a ChatGPT plan). Each runs in the
//! background with its own sign-in, gets the text on standard input and prints the reply.
//!
//! * Claude Code: `claude -p --output-format json --model <haiku|sonnet|opus> --system-prompt <s>
//!   --tools "" --no-session-persistence --strict-mcp-config --disable-slash-commands`, in an empty
//!   folder so no project instructions load. (`--bare` would also skip the user's own instructions
//!   file, but it refuses subscription sign-ins, so the system prompt says to ignore anything else.)
//! * Codex: `node <npm>/@openai/codex/bin/codex.js exec --skip-git-repo-check --ephemeral --sandbox
//!   read-only --color never -m <model> -c model_reasoning_effort="low" -o <file> <instructions>`.
//!   Node is run directly because the npm `codex.cmd` shim cuts an argument at its first line break.
//!   Codex has no system prompt switch, so the instructions are the prompt and the text follows on
//!   standard input. Its model list comes from Codex's own cache (~/.codex/models_cache.json).
//! * Both take a few seconds, so the pipeline uses them for rewrites, not for Clean's light polish.
//!   Claude Code runs with MAX_THINKING_TOKENS=0: with thinking on, Haiku spent about 470 output
//!   tokens thinking before a ten-word reply (about 6 s); without it, about 2 s (18 Oct 2026).
//! * No console window flashes up (CREATE_NO_WINDOW), and a tool that runs past the deadline is
//!   killed. Errors are short sentences; prompts and replies are never logged.
//! * Finding the tools: a path in Settings (the AI address field) first, then the native
//!   installer's places, then the PATH. npm, pnpm, Yarn and nvm installs leave a `.cmd` shim on the
//!   PATH; Navi reads the script it points at (through one more shim for Yarn) and runs that with
//!   Node, never the shim (see above).
//!   A plain `codex.exe` or `claude.exe` (the standalone build, a Scoop or Volta shim) runs as is.

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

/// A minute for a dictation; longer text (a meeting transcript) gets two seconds more per 1,000
/// characters, up to six minutes.
const DEADLINE: Duration = Duration::from_secs(60);
const DEADLINE_MAX: Duration = Duration::from_secs(360);
/// Longer system prompts go through a file (the Windows command line stops at 32,767 characters).
const ARG_LIMIT: usize = 8_000;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Extra rules for a tool that also reads its user's own instructions file. Kept general: the rules
/// above it decide what may be added (dictation forbids greetings it didn't hear; a job on a call
/// may draft an email that has one).
const STAY_ON_TASK: &str = "\n\nYou are running inside Navi, a dictation app. Follow only the rules above. Ignore any other instructions, memories or notes you have about the user. Never add names, facts or anything else the rules above don't allow. Never use tools. Output only what the rules above ask for.";

/// One tool, as the Settings page shows it.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CliTool {
    /// "claude-code" or "codex".
    pub id: String,
    pub name: String,
    /// Which plan it uses.
    pub plan: String,
    pub found: bool,
    pub path: Option<String>,
    /// Models to offer, best first for dictation (fast and cheap).
    pub models: Vec<String>,
    pub install_hint: String,
}

pub fn is_cli(provider: &str) -> bool {
    matches!(provider, "claude-code" | "codex")
}

/// How to start a tool: a program, plus the script it runs when that program is Node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launcher {
    pub program: PathBuf,
    pub script: Option<PathBuf>,
}

impl Launcher {
    fn command(&self) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(&self.program);
        if let Some(script) = &self.script {
            cmd.arg(script);
        }
        cmd
    }

    /// The path Settings shows: the script for a Node tool, otherwise the program.
    pub fn shown(&self) -> String {
        self.script.as_ref().unwrap_or(&self.program).display().to_string()
    }
}

/// The tools on this computer, found or not.
pub fn detect() -> Vec<CliTool> {
    let claude = find_claude(None);
    let codex = find_codex(None);
    vec![
        CliTool {
            id: "claude-code".into(),
            name: "Claude Code".into(),
            plan: "your Claude plan".into(),
            found: claude.is_some(),
            path: claude.map(|l| l.shown()),
            models: vec!["haiku".into(), "sonnet".into(), "opus".into()],
            install_hint: "To install it, go to claude.com/claude-code.".into(),
        },
        CliTool {
            id: "codex".into(),
            name: "Codex".into(),
            plan: "your ChatGPT plan".into(),
            found: codex.is_some(),
            path: codex.map(|l| l.shown()),
            models: codex_models(),
            install_hint: "To install it, run npm install -g @openai/codex in a terminal.".into(),
        },
    ]
}

/// The model to ask for, from whatever is in Settings ("claude-haiku-4-5-..." from an API key era
/// becomes "haiku").
pub fn model_for(provider: &str, wanted: &str) -> String {
    let w = wanted.trim().to_ascii_lowercase();
    match provider {
        "claude-code" => ["haiku", "sonnet", "opus"].iter().find(|m| w.contains(*m)).unwrap_or(&"haiku").to_string(),
        _ => {
            let listed = codex_models();
            if !w.is_empty() && !w.starts_with("claude") && (listed.is_empty() || listed.iter().any(|m| *m == w)) {
                w
            } else {
                listed.into_iter().next().unwrap_or_default()
            }
        }
    }
}

/// Runs `provider` on `user` with `system` as its instructions and returns the reply text.
pub async fn complete(provider: &str, model: &str, system: &str, user: &str, path_override: Option<&str>) -> Result<String> {
    let scratch = std::env::temp_dir().join("navi-ai");
    std::fs::create_dir_all(&scratch).context("Navi couldn't make a folder for the AI tool")?;
    match provider {
        "claude-code" => claude_code(model, system, user, path_override, &scratch).await,
        "codex" => codex(model, system, user, path_override, &scratch).await,
        other => bail!("Navi doesn't know the AI tool \"{other}\"."),
    }
}

async fn claude_code(model: &str, system: &str, user: &str, path_override: Option<&str>, scratch: &Path) -> Result<String> {
    let launcher = find_claude(path_override).ok_or_else(|| anyhow!("Claude Code isn't installed on this computer. Install it, or pick another AI in Settings."))?;
    let system = format!("{system}{STAY_ON_TASK}");
    let mut cmd = launcher.command();
    cmd.current_dir(scratch).args(["-p", "--output-format", "json", "--model", &model_for("claude-code", model)]);
    let prompt_file = scratch.join(format!("system-{}.txt", std::process::id()));
    if system.len() > ARG_LIMIT {
        std::fs::write(&prompt_file, &system)?;
        cmd.arg("--system-prompt-file").arg(&prompt_file);
    } else {
        cmd.arg("--system-prompt").arg(&system);
    }
    cmd.args(["--tools", "", "--no-session-persistence", "--strict-mcp-config", "--disable-slash-commands"]);
    // No hidden thinking before a tidy-up: it was most of the wait (about 6 s down to about 2 s on
    // Haiku, 18 Oct 2026) and adds nothing to rewording a few sentences.
    cmd.env("MAX_THINKING_TOKENS", "0");
    let out = run(cmd, user).await;
    let _ = std::fs::remove_file(&prompt_file);
    let out = out.map_err(|e| anyhow!("Claude Code didn't answer: {e}"))?;
    parse_claude(&out.stdout, &out.stderr, out.code)
}

/// The reply from `claude -p --output-format json`.
fn parse_claude(stdout: &str, stderr: &str, code: Option<i32>) -> Result<String> {
    let json = stdout.lines().rev().find_map(|line| serde_json::from_str::<serde_json::Value>(line.trim()).ok());
    if let Some(v) = json {
        let text = v.get("result").and_then(|r| r.as_str()).unwrap_or_default().trim().to_string();
        let is_error = v.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false);
        if !is_error && !text.is_empty() {
            return Ok(text);
        }
        bail!("{}", explain("Claude Code", if text.is_empty() { stderr } else { &text }));
    }
    let stopped = format!("it stopped (code {})", code.unwrap_or(-1));
    bail!("{}", explain("Claude Code", if stderr.trim().is_empty() { &stopped } else { stderr }))
}

async fn codex(model: &str, system: &str, user: &str, path_override: Option<&str>, scratch: &Path) -> Result<String> {
    let launcher = find_codex(path_override).ok_or_else(|| anyhow!("Codex isn't installed on this computer. Install it, or pick another AI in Settings."))?;
    let reply_file = scratch.join(format!("codex-reply-{}-{}.txt", std::process::id(), uuid::Uuid::new_v4()));
    let instructions = format!("{system}{STAY_ON_TASK}\n\nThe text to work on follows on standard input.");
    let mut cmd = launcher.command();
    cmd.current_dir(scratch).args(["exec", "--skip-git-repo-check", "--ephemeral", "--sandbox", "read-only", "--color", "never"]);
    let model = model_for("codex", model);
    if !model.is_empty() {
        cmd.args(["-m", &model]);
    }
    cmd.args(["-c", "model_reasoning_effort=\"low\"", "-o"]).arg(&reply_file).arg(&instructions);
    let out = run(cmd, user).await.map_err(|e| anyhow!("Codex didn't answer: {e}"))?;
    let reply = std::fs::read_to_string(&reply_file).unwrap_or_default();
    let _ = std::fs::remove_file(&reply_file);
    if out.code == Some(0) && !reply.trim().is_empty() {
        return Ok(reply.trim().to_string());
    }
    let error_line = out.stdout.lines().chain(out.stderr.lines()).find(|l| l.contains("ERROR")).unwrap_or_default().to_string();
    let message = serde_json::from_str::<serde_json::Value>(error_line.trim_start_matches("ERROR:").trim())
        .ok()
        .and_then(|v| v.pointer("/error/message").and_then(|m| m.as_str()).map(str::to_string))
        .unwrap_or(error_line);
    bail!("{}", explain("Codex", if message.trim().is_empty() { "it gave no reply" } else { &message }))
}

/// A short, useful sentence for the common failures.
fn explain(tool: &str, raw: &str) -> String {
    let lower = raw.to_lowercase();
    if lower.contains("login") || lower.contains("log in") || lower.contains("sign in") || lower.contains("not authenticated") || lower.contains("api key") {
        return format!("{tool} isn't signed in. Open Settings > AI and press Sign in on its card, then try again.");
    }
    if lower.contains("rate limit") || lower.contains("usage limit") || lower.contains("limit reached") || lower.contains("429") {
        return format!("{tool} says you've reached your plan's limit for now. Try again later, or pick another AI in Settings.");
    }
    if lower.contains("not supported") && lower.contains("model") {
        return format!("{tool} can't use that model with your plan. Pick another model in Settings > AI.");
    }
    let first = raw.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("it stopped without saying why");
    let short: String = first.chars().take(160).collect();
    format!("{tool}: {short}")
}

struct Output {
    stdout: String,
    stderr: String,
    code: Option<i32>,
}

/// Starts the tool with no window, writes `input` to it, waits (see DEADLINE) and collects what it
/// printed.
async fn run(cmd: tokio::process::Command, input: &str) -> Result<Output> {
    run_for(cmd, input, deadline_for(input.len())).await
}

async fn run_for(mut cmd: tokio::process::Command, input: &str, deadline: Duration) -> Result<Output> {
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| anyhow!("it couldn't start ({e})"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.as_bytes()).await.ok();
        // Dropping stdin closes it, so the tool knows the text has ended.
    }
    let output = tokio::time::timeout(deadline, child.wait_with_output())
        .await
        .map_err(|_| anyhow!("no reply within {} seconds", deadline.as_secs()))??;
    Ok(Output {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        code: output.status.code(),
    })
}

fn deadline_for(input_len: usize) -> Duration {
    (DEADLINE + Duration::from_secs(2 * (input_len / 1_000) as u64)).min(DEADLINE_MAX)
}

/// Claude Code: a path set in Settings, the native installer's places, claude.exe on the PATH, then
/// an npm (or pnpm, Yarn, nvm) install's claude.cmd.
pub fn find_claude(path_override: Option<&str>) -> Option<Launcher> {
    if let Some(l) = path_override.and_then(launcher_for_path) {
        return Some(l);
    }
    let home = dirs::home_dir();
    let candidates = [home.as_ref().map(|h| h.join(".local").join("bin").join("claude.exe")), home.as_ref().map(|h| h.join(".claude").join("local").join("claude.exe"))];
    candidates
        .into_iter()
        .flatten()
        .find(|p| p.is_file())
        .or_else(|| on_path("claude.exe"))
        .map(|program| Launcher { program, script: None })
        .or_else(|| from_shims("claude.cmd"))
}

/// Codex: a path set in Settings, the global npm install, an npm (or pnpm, Yarn, nvm) install's
/// codex.cmd anywhere on the PATH, then codex.exe on the PATH.
pub fn find_codex(path_override: Option<&str>) -> Option<Launcher> {
    if let Some(l) = path_override.and_then(launcher_for_path) {
        return Some(l);
    }
    let npm_js = dirs::data_dir().map(|d| d.join("npm")).and_then(|npm| {
        let js = npm.join("node_modules").join("@openai").join("codex").join("bin").join("codex.js");
        js.is_file().then(|| node_launcher(js, &npm)).flatten()
    });
    npm_js.or_else(|| from_shims("codex.cmd")).or_else(|| on_path("codex.exe").map(|program| Launcher { program, script: None }))
}

// ---- Settings' Find button: a proper search ----

/// A tool the Find button found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub launcher: Launcher,
    /// The file to keep in Settings, from which `launcher_for_path` starts the same thing. None
    /// when it is where Navi looks anyway.
    pub path: Option<PathBuf>,
}

/// What each tool's files are called.
struct Names {
    /// The program itself: claude.exe, or claude on a Mac.
    program: &'static str,
    /// An npm-style shim (Windows only).
    shim: &'static str,
    /// Its Node script in a global node_modules folder.
    script: &'static [&'static str],
    /// The folder-name start of the editor extension that carries a copy of it.
    extension: &'static str,
}

fn names(id: &str) -> Names {
    let exe = cfg!(windows);
    if id == "codex" {
        Names { program: if exe { "codex.exe" } else { "codex" }, shim: "codex.cmd", script: &["@openai", "codex", "bin", "codex.js"], extension: "openai.chatgpt-" }
    } else {
        Names { program: if exe { "claude.exe" } else { "claude" }, shim: "claude.cmd", script: &["@anthropic-ai", "claude-code", "cli.js"], extension: "anthropic.claude-code-" }
    }
}

/// Looks properly for `id` ("claude-code" or "codex"): where Navi looks anyway, then the PATH as
/// the system has it now (a tool installed after Navi started isn't on Navi's own copy, and an app
/// opened from the Mac's Finder gets a short one), then the folders installers and Node version
/// managers use, then the copies editor extensions carry (Claude Code's VS Code extension has its
/// own claude.exe). Blocking: a second or so at most.
pub fn search(id: &str) -> Option<Found> {
    let usual = if id == "codex" { find_codex(None) } else { find_claude(None) };
    if let Some(launcher) = usual {
        return Some(Found { launcher, path: None });
    }
    let n = names(id);
    let mut dirs = fresh_path();
    dirs.extend(install_dirs());
    search_dirs(&dirs, &n).or_else(|| search_extensions(&extension_roots(), &n))
}

/// The first of `dirs` holding the tool: the program, an npm shim that leads somewhere real, or
/// the Node script in the folder's node_modules (or, on a Mac, lib/node_modules).
fn search_dirs(dirs: &[PathBuf], n: &Names) -> Option<Found> {
    let found = |path: PathBuf| launcher_for_path(&path.to_string_lossy()).map(|launcher| Found { launcher, path: Some(path) });
    dirs.iter().find_map(|dir| {
        let program = dir.join(n.program);
        if program.is_file() {
            return found(program);
        }
        let shim = dir.join(n.shim);
        if cfg!(windows) && shim.is_file() {
            if let Some(f) = found(shim) {
                return Some(f);
            }
        }
        let modules = if cfg!(windows) { dir.join("node_modules") } else { dir.join("..").join("lib").join("node_modules") };
        let script = n.script.iter().fold(modules, |p, part| p.join(part));
        script.is_file().then(|| found(script)).flatten()
    })
}

/// The newest editor extension in `roots` that carries the tool (VS Code, Cursor, Windsurf),
/// searched a few folders deep. These move when the extension updates, so they come last.
fn search_extensions(roots: &[PathBuf], n: &Names) -> Option<Found> {
    let mut exts: Vec<(std::time::SystemTime, PathBuf)> = roots
        .iter()
        .filter_map(|r| std::fs::read_dir(r).ok())
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().to_ascii_lowercase().starts_with(n.extension))
        .map(|e| (e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), e.path()))
        .collect();
    exts.sort_by(|a, b| b.0.cmp(&a.0));
    exts.iter().find_map(|(_, dir)| find_file(dir, n.program, 4)).map(|program| Found { launcher: Launcher { program: program.clone(), script: None }, path: Some(program) })
}

/// `name` in `dir` or up to `depth` folders below it.
fn find_file(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
    let here = dir.join(name);
    if here.is_file() {
        return Some(here);
    }
    if depth == 0 {
        return None;
    }
    let mut subs: Vec<PathBuf> = std::fs::read_dir(dir).ok()?.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()).collect();
    subs.sort();
    subs.iter().find_map(|d| find_file(d, name, depth - 1))
}

/// Folders where VS Code, Cursor and Windsurf keep extensions.
fn extension_roots() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else { return Vec::new() };
    [".vscode", ".vscode-insiders", ".cursor", ".windsurf"].iter().map(|d| home.join(d).join("extensions")).collect()
}

/// Every folder in `parent` (a version manager's Node versions), newest name last as listed.
fn subfolders(parent: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(parent).map(|r| r.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()).collect()).unwrap_or_default();
    out.sort();
    out.reverse();
    out
}

/// Where installers and Node version managers put command-line tools on Windows.
#[cfg(windows)]
fn install_dirs() -> Vec<PathBuf> {
    let home = dirs::home_dir();
    let roaming = dirs::data_dir();
    let local = dirs::data_local_dir();
    let mut out: Vec<PathBuf> = [
        home.as_ref().map(|h| h.join(".local").join("bin")),
        home.as_ref().map(|h| h.join(".claude").join("local")),
        roaming.as_ref().map(|r| r.join("npm")),
        local.as_ref().map(|l| l.join("Volta").join("bin")),
        local.as_ref().map(|l| l.join("pnpm")),
        local.as_ref().map(|l| l.join("Yarn").join("bin")),
        home.as_ref().map(|h| h.join("scoop").join("shims")),
        local.as_ref().map(|l| l.join("Microsoft").join("WinGet").join("Links")),
        std::env::var_os("ProgramFiles").map(|p| PathBuf::from(p).join("nodejs")),
    ]
    .into_iter()
    .flatten()
    .collect();
    // Each Node version nvm-windows and fnm keep, newest first.
    if let Some(r) = &roaming {
        out.extend(subfolders(&r.join("nvm")));
        out.extend(subfolders(&r.join("fnm").join("node-versions")).into_iter().map(|v| v.join("installation")));
    }
    out
}

/// Where installers, Homebrew and Node version managers put command-line tools on a Mac or Linux.
#[cfg(not(windows))]
fn install_dirs() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else { return Vec::new() };
    let mut out: Vec<PathBuf> = vec![
        home.join(".local").join("bin"),
        home.join(".claude").join("local"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        home.join(".npm-global").join("bin"),
        home.join(".volta").join("bin"),
        home.join(".bun").join("bin"),
        home.join("Library").join("pnpm"),
        home.join(".yarn").join("bin"),
    ];
    out.extend(subfolders(&home.join(".nvm").join("versions").join("node")).into_iter().map(|v| v.join("bin")));
    for fnm in [home.join(".fnm"), home.join("Library").join("Application Support").join("fnm")] {
        out.extend(subfolders(&fnm.join("node-versions")).into_iter().map(|v| v.join("installation").join("bin")));
    }
    out
}

/// The PATH as the system has it now: on Windows the user's and the machine's from the registry;
/// elsewhere the one the user's login shell sets up (nvm and Homebrew add to it there).
#[cfg(windows)]
fn fresh_path() -> Vec<PathBuf> {
    win::fresh_path()
}

#[cfg(not(windows))]
fn fresh_path() -> Vec<PathBuf> {
    login_shell_path().clone()
}

/// The PATH from `$SHELL -ilc`, asked once (it can take a second) and remembered.
#[cfg(not(windows))]
fn login_shell_path() -> &'static Vec<PathBuf> {
    static PATH: std::sync::OnceLock<Vec<PathBuf>> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        const MARK: &str = "__NAVI_PATH__";
        let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".into());
        let Ok(mut child) = std::process::Command::new(shell)
            .args(["-ilc", &format!("printf '\\n{MARK}%s\\n' \"$PATH\"")])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            return Vec::new();
        };
        let started = std::time::Instant::now();
        while child.try_wait().ok().flatten().is_none() {
            if started.elapsed() > Duration::from_secs(3) {
                let _ = child.kill();
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let Ok(out) = child.wait_with_output() else { return Vec::new() };
        let text = String::from_utf8_lossy(&out.stdout);
        text.lines().find_map(|l| l.strip_prefix(MARK)).map(|p| std::env::split_paths(p).collect()).unwrap_or_default()
    })
}

/// Whether a tool is signed in, from its own status command (`claude auth status`, `codex login
/// status`), which asks no model and takes a moment. None when it couldn't tell (an older version
/// without the command, or it didn't start).
pub async fn signed_in(id: &str, launcher: &Launcher) -> Option<bool> {
    let mut cmd = launcher.command();
    cmd.current_dir(std::env::temp_dir());
    if id == "codex" {
        cmd.args(["login", "status"]);
    } else {
        cmd.args(["auth", "status"]);
    }
    let out = run_for(cmd, "", Duration::from_secs(15)).await.ok()?;
    if id == "codex" {
        codex_status(&out.stdout, &out.stderr, out.code)
    } else {
        claude_status(&out.stdout)
    }
}

/// `claude auth status` prints JSON with "loggedIn".
fn claude_status(stdout: &str) -> Option<bool> {
    let start = stdout.find('{')?;
    let v: serde_json::Value = serde_json::from_str(stdout[start..].trim()).ok()?;
    v.get("loggedIn").and_then(|b| b.as_bool())
}

/// `codex login status` says "Logged in using ChatGPT" (or an API key), or "Not logged in".
fn codex_status(stdout: &str, stderr: &str, code: Option<i32>) -> Option<bool> {
    let said = format!("{stdout}\n{stderr}").to_lowercase();
    if said.contains("not logged in") {
        Some(false)
    } else if said.contains("logged in") && code == Some(0) {
        Some(true)
    } else {
        None
    }
}

/// The command that signs a tool in, run in a terminal window the user can see.
fn sign_in_args(id: &str) -> &'static [&'static str] {
    if id == "codex" {
        &["login"]
    } else {
        &["auth", "login"]
    }
}

/// Opens a terminal window that runs the tool's own sign-in (`claude auth login`, `codex login`),
/// which opens the browser. Navi never sees the sign-in itself.
pub fn open_sign_in(id: &str, launcher: &Launcher) -> Result<()> {
    let name = if id == "codex" { "Codex" } else { "Claude Code" };
    let mut parts: Vec<String> = vec![launcher.program.display().to_string()];
    parts.extend(launcher.script.iter().map(|s| s.display().to_string()));
    parts.extend(sign_in_args(id).iter().map(|s| s.to_string()));
    let folder = std::env::temp_dir().join("navi-ai");
    std::fs::create_dir_all(&folder).context("Navi couldn't make a folder for the sign-in")?;
    open_terminal(name, &parts, &folder)
}

/// The batch file the Windows sign-in window runs: each part quoted (a batch file needs nothing
/// else escaped but %), and UTF-8 for any accents in the paths.
#[cfg_attr(not(windows), allow(dead_code))]
fn sign_in_batch(name: &str, parts: &[String]) -> String {
    let line = parts.iter().map(|p| format!("\"{}\"", p.replace('%', "%%"))).collect::<Vec<_>>().join(" ");
    format!(
        "@echo off\r\nchcp 65001 >nul\r\ntitle Sign in to {name}\r\necho Sign in to {name} below. It opens your browser.\r\necho When it says you're signed in, close this window and go back to Navi.\r\necho.\r\n{line}\r\necho.\r\npause\r\n"
    )
}

/// The script the Mac's Terminal runs for the sign-in, each part quoted for the shell.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn sign_in_shell(name: &str, parts: &[String], folder: &Path) -> String {
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    let line = parts.iter().map(|p| quote(p)).collect::<Vec<_>>().join(" ");
    format!(
        "#!/bin/bash\ncd {}\necho \"Sign in to {name} below. It opens your browser.\"\necho \"When it says you're signed in, close this window and go back to Navi.\"\necho\n{line}\n",
        quote(&folder.display().to_string())
    )
}

#[cfg(windows)]
fn open_terminal(name: &str, parts: &[String], folder: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
    let script = sign_in_batch(name, parts);
    let file = folder.join(format!("sign-in-{}.cmd", if name == "Codex" { "codex" } else { "claude" }));
    std::fs::write(&file, script).context("Navi couldn't write the sign-in script")?;
    std::process::Command::new("cmd.exe").arg("/c").arg(&file).current_dir(folder).creation_flags(CREATE_NEW_CONSOLE).spawn().context("Navi couldn't open a terminal")?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn open_terminal(name: &str, parts: &[String], folder: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let script = sign_in_shell(name, parts, folder);
    let file = folder.join(format!("sign-in-{}.command", if name == "Codex" { "codex" } else { "claude" }));
    std::fs::write(&file, script).context("Navi couldn't write the sign-in script")?;
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755))?;
    std::process::Command::new("open").args(["-a", "Terminal"]).arg(&file).spawn().context("Navi couldn't open Terminal")?;
    Ok(())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn open_terminal(name: &str, parts: &[String], _folder: &Path) -> Result<()> {
    bail!("Open a terminal and run {} to sign in to {name}.", parts.join(" "))
}

#[cfg(windows)]
mod win {
    use std::path::PathBuf;
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::System::Registry::{RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};

    /// The machine's PATH, then the user's, as the registry has them now (expanded).
    pub fn fresh_path() -> Vec<PathBuf> {
        let machine = read(HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment", "Path");
        let user = read(HKEY_CURRENT_USER, "Environment", "Path");
        [machine, user].into_iter().flatten().flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).collect()
    }

    fn read(root: HKEY, key: &str, value: &str) -> Option<String> {
        let (key, value) = (HSTRING::from(key), HSTRING::from(value));
        // Asked for as REG_SZ (without RRF_NOEXPAND), a REG_EXPAND_SZ value, as the user's PATH
        // usually is, comes back with %USERPROFILE% and the like filled in.
        let flags = RRF_RT_REG_SZ;
        let mut size = 0u32;
        for _ in 0..3 {
            let mut buf = vec![0u16; size as usize / 2 + 1];
            let mut got = (buf.len() * 2) as u32;
            let status = unsafe { RegGetValueW(root, PCWSTR(key.as_ptr()), PCWSTR(value.as_ptr()), flags, None, Some(buf.as_mut_ptr().cast()), Some(&mut got)) };
            if status.is_ok() {
                let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                return Some(String::from_utf16_lossy(&buf[..len]));
            }
            // Too small (the expanded text can be longer than the size first given): try again.
            size = got.max(size * 2).max(1024);
        }
        None
    }
}

/// A tool path typed into Settings: a program, a Node script, or an npm-style `.cmd` shim.
fn launcher_for_path(path: &str) -> Option<Launcher> {
    let p = PathBuf::from(path.trim());
    if path.trim().is_empty() || !p.is_file() {
        return None;
    }
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    match ext.as_str() {
        "js" | "cjs" | "mjs" => node_launcher(p.clone(), p.parent()?),
        "cmd" | "bat" => launcher_from_shim(&p),
        // npm's `claude` on a Mac is a link to cli.js, which starts with "#!/usr/bin/env node":
        // from the Finder there is no node on the PATH, so Navi runs it with Node itself.
        _ if !cfg!(windows) => match std::fs::canonicalize(&p) {
            Ok(real) if is_script(&real) => node_launcher(real, p.parent()?),
            _ => Some(Launcher { program: p, script: None }),
        },
        _ => Some(Launcher { program: p, script: None }),
    }
}

fn is_script(p: &Path) -> bool {
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    matches!(ext.as_str(), "js" | "cjs" | "mjs")
}

/// The first `shim` (claude.cmd, codex.cmd) in npm's global folder or on the PATH that leads to a
/// script or program that is really there.
fn from_shims(shim: &str) -> Option<Launcher> {
    let npm = dirs::data_dir().map(|d| d.join("npm"));
    let path_dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    npm.into_iter().chain(path_dirs).map(|dir| dir.join(shim)).filter(|p| p.is_file()).find_map(|p| launcher_from_shim(&p))
}

fn launcher_from_shim(shim: &Path) -> Option<Launcher> {
    launcher_from_shim_within(shim, 1)
}

/// `launcher_from_shim`, following at most `hops` shims that only call another shim (Yarn's global
/// codex.cmd calls the one in its node_modules\.bin, which names the script).
fn launcher_from_shim_within(shim: &Path, hops: u32) -> Option<Launcher> {
    let dir = shim.parent()?;
    let target = shim_target(&std::fs::read_to_string(shim).ok()?, dir)?;
    if !target.is_file() {
        return None;
    }
    let ext = target.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    match ext.as_str() {
        "exe" => Some(Launcher { program: target, script: None }),
        "cmd" | "bat" if hops > 0 => launcher_from_shim_within(&target, hops - 1),
        "cmd" | "bat" => None,
        _ => node_launcher(target, dir),
    }
}

/// Node runs `script`: the node.exe beside the shim, as the shim itself would pick, then the one
/// of the Node install whose node_modules holds the script (nvm and fnm keep one per version),
/// then the PATH's.
fn node_launcher(script: PathBuf, dir: &Path) -> Option<Launcher> {
    let node = if cfg!(windows) { "node.exe" } else { "node" };
    // <prefix>\node_modules\... on Windows, <prefix>/lib/node_modules/... with <prefix>/bin/node elsewhere.
    let prefix = script.ancestors().find(|a| a.file_name().is_some_and(|n| n == "node_modules")).and_then(Path::parent);
    let of_prefix = prefix.map(|p| if cfg!(windows) { p.join(node) } else { p.parent().unwrap_or(p).join("bin").join(node) });
    let node = Some(dir.join(node)).filter(|p| p.is_file()).or_else(|| of_prefix.filter(|p| p.is_file())).or_else(|| on_path(node))?;
    Some(Launcher { program: node, script: Some(script) })
}

/// The script or program an npm, pnpm or yarn `.cmd` shim in `dir` runs: the quoted path starting
/// %dp0% or %~dp0 (the shim's own folder) that ends in .js, .cjs, .mjs or .exe and is not node.exe.
/// Failing that, another shim it calls (.cmd or .bat), as Yarn's global shim does.
fn shim_target(shim: &str, dir: &Path) -> Option<PathBuf> {
    shim_path_ending(shim, dir, &[".js", ".cjs", ".mjs", ".exe"]).or_else(|| shim_path_ending(shim, dir, &[".cmd", ".bat"]))
}

/// The first quoted path in `shim` that starts %dp0% or %~dp0 and ends in one of `endings` (never
/// node.exe), resolved against the shim's folder `dir`.
fn shim_path_ending(shim: &str, dir: &Path, endings: &[&str]) -> Option<PathBuf> {
    shim.split('"').skip(1).step_by(2).find_map(|quoted| {
        let quoted = quoted.trim();
        let rest = quoted.strip_prefix("%dp0%").or_else(|| quoted.strip_prefix("%~dp0%")).or_else(|| quoted.strip_prefix("%~dp0"))?;
        let lower = rest.to_ascii_lowercase();
        let runnable = endings.iter().any(|e| lower.ends_with(e)) && !lower.ends_with("node.exe");
        if !runnable {
            return None;
        }
        let mut path = dir.to_path_buf();
        for part in rest.split(['\\', '/']).filter(|p| !p.is_empty() && *p != ".") {
            if part == ".." {
                path.pop();
            } else {
                path.push(part);
            }
        }
        Some(path)
    })
}

/// Codex's models for this account, small and fast ones first ("luna" is the lightest family).
pub fn codex_models() -> Vec<String> {
    let Some(path) = dirs::home_dir().map(|h| h.join(".codex").join("models_cache.json")) else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return Vec::new() };
    let mut models: Vec<(bool, i64, String)> = v
        .get("models")
        .and_then(|m| m.as_array())
        .map(|list| {
            list.iter()
                .filter(|m| m.get("visibility").and_then(|x| x.as_str()) == Some("list"))
                .filter_map(|m| {
                    let slug = m.get("slug")?.as_str()?.to_string();
                    let priority = m.get("priority").and_then(|p| p.as_i64()).unwrap_or(99);
                    Some((!slug.contains("luna"), priority, slug))
                })
                .collect()
        })
        .unwrap_or_default();
    models.sort();
    models.into_iter().map(|(_, _, slug)| slug).collect()
}

/// `exe` on Navi's PATH, else on the PATH as the system has it now (fresh_path).
fn on_path(exe: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .and_then(|paths| std::env::split_paths(&paths).map(|dir| dir.join(exe)).find(|p| p.is_file()))
        .or_else(|| fresh_path().into_iter().map(|dir| dir.join(exe)).find(|p| p.is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_models_map_from_anything() {
        assert_eq!(model_for("claude-code", "claude-haiku-4-5-20251001"), "haiku");
        assert_eq!(model_for("claude-code", "sonnet"), "sonnet");
        assert_eq!(model_for("claude-code", "claude-opus-5-5"), "opus");
        assert_eq!(model_for("claude-code", ""), "haiku");
        assert_eq!(model_for("claude-code", "gpt-6-luna"), "haiku");
    }

    #[test]
    fn claude_replies_and_errors_parse() {
        let ok = r#"{"type":"result","subtype":"success","is_error":false,"result":"Hi Priya, can we do Thursday?"}"#;
        assert_eq!(parse_claude(ok, "", Some(0)).unwrap(), "Hi Priya, can we do Thursday?");
        let not_signed_in = r#"{"type":"result","is_error":true,"result":"Invalid API key · Please run /login"}"#;
        let e = parse_claude(not_signed_in, "", Some(1)).unwrap_err().to_string();
        assert!(e.contains("isn't signed in"), "{e}");
        let e = parse_claude("", "error: something odd happened\nmore", Some(2)).unwrap_err().to_string();
        assert_eq!(e, "Claude Code: error: something odd happened");
        let e = parse_claude("", "", Some(3)).unwrap_err().to_string();
        assert!(e.contains("code 3"), "{e}");
    }

    #[test]
    fn errors_read_well() {
        assert!(explain("Codex", "You've hit your usage limit").contains("plan's limit"));
        assert!(explain("Codex", "The 'gpt-6.1-sol' model is not supported when using Codex with a ChatGPT account.").contains("can't use that model"));
        assert!(!explain("Codex", "x").contains('\u{2014}'));
    }

    #[test]
    fn shims_lead_to_their_scripts() {
        let dir = Path::new("C:\\Users\\you\\AppData\\Roaming\\npm");
        // npm's own shim (npm 7 and later).
        let npm = "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\r\nIF EXIST \"%dp0%\\node.exe\" (\r\n  SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  SET \"_prog=node\"\r\n  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n)\r\n\r\nendLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  \"%dp0%\\node_modules\\@anthropic-ai\\claude-code\\cli.js\" %*\r\n";
        assert_eq!(shim_target(npm, dir), Some(dir.join("node_modules").join("@anthropic-ai").join("claude-code").join("cli.js")));
        // pnpm's shim reaches into its global store.
        let pnpm_dir = Path::new("C:\\Users\\you\\AppData\\Local\\pnpm");
        let pnpm = "@SETLOCAL\r\n@IF NOT DEFINED NODE_PATH (\r\n  @SET \"NODE_PATH=C:\\x\\node_modules\"\r\n)\r\n@IF EXIST \"%~dp0\\node.exe\" (\r\n  \"%~dp0\\node.exe\"  \"%~dp0\\global\\5\\node_modules\\@openai\\codex\\bin\\codex.js\" %*\r\n) ELSE (\r\n  node  \"%~dp0\\global\\5\\node_modules\\@openai\\codex\\bin\\codex.js\" %*\r\n)\r\n";
        assert_eq!(shim_target(pnpm, pnpm_dir), Some(pnpm_dir.join("global").join("5").join("node_modules").join("@openai").join("codex").join("bin").join("codex.js")));
        // A shim for a native program, with a step up a folder.
        let native = "@\"%~dp0\\..\\tools\\codex.exe\" %*";
        assert_eq!(shim_target(native, dir), Some(Path::new("C:\\Users\\you\\AppData\\Roaming").join("tools").join("codex.exe")));
        assert_eq!(shim_target("@echo off\r\nnode \"%dp0%\\node.exe\"", dir), None);
        // Yarn's global shim only calls another shim.
        let yarn_dir = Path::new("C:\\Users\\you\\AppData\\Local\\Yarn\\bin");
        let yarn = "@\"%~dp0\\..\\Data\\global\\node_modules\\.bin\\codex.cmd\" %*\r\n";
        assert_eq!(
            shim_target(yarn, yarn_dir),
            Some(Path::new("C:\\Users\\you\\AppData\\Local\\Yarn").join("Data").join("global").join("node_modules").join(".bin").join("codex.cmd"))
        );
        assert_eq!(launcher_for_path("  "), None);
        assert_eq!(launcher_for_path("C:\\no\\such\\claude.exe"), None);
    }

    #[test]
    fn a_yarn_install_is_found_through_its_second_shim() {
        let root = std::env::temp_dir().join(format!("navi-yarn-shim-{}", uuid::Uuid::new_v4()));
        let bin = root.join("Yarn").join("bin");
        let inner = root.join("Yarn").join("Data").join("global").join("node_modules").join(".bin");
        let script = root.join("Yarn").join("Data").join("global").join("node_modules").join("@openai").join("codex").join("bin").join("codex.js");
        for dir in [bin.as_path(), inner.as_path(), script.parent().unwrap()] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(bin.join("codex.cmd"), "@\"%~dp0\\..\\Data\\global\\node_modules\\.bin\\codex.cmd\" %*\r\n").unwrap();
        std::fs::write(
            inner.join("codex.cmd"),
            "@IF EXIST \"%~dp0\\node.exe\" (\r\n  \"%~dp0\\node.exe\"  \"%~dp0\\..\\@openai\\codex\\bin\\codex.js\" %*\r\n) ELSE (\r\n  node  \"%~dp0\\..\\@openai\\codex\\bin\\codex.js\" %*\r\n)\r\n",
        )
        .unwrap();
        // A node.exe beside the inner shim, so the test doesn't depend on Node being installed.
        std::fs::write(inner.join("node.exe"), "").unwrap();
        std::fs::write(&script, "").unwrap();
        let found = launcher_from_shim(&bin.join("codex.cmd"));
        // A shim that calls itself is followed once, then given up on.
        std::fs::write(bin.join("loop.cmd"), "@\"%~dp0\\loop.cmd\" %*\r\n").unwrap();
        let looped = launcher_from_shim(&bin.join("loop.cmd"));
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(found, Some(Launcher { program: inner.join("node.exe"), script: Some(script) }));
        assert_eq!(looped, None);
    }

    #[test]
    fn only_the_two_tools_count() {
        assert!(is_cli("claude-code") && is_cli("codex"));
        assert!(!is_cli("anthropic") && !is_cli("off"));
        let tools = detect();
        assert_eq!(tools.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["claude-code", "codex"]);
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("navi-find-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_search_finds_programs_shims_and_scripts() {
        let n = names("codex");
        let (empty, with_program, with_script) = (scratch("empty"), scratch("program"), scratch("script"));
        std::fs::write(with_program.join(n.program), "").unwrap();
        let found = search_dirs(&[empty.clone(), with_program.clone()], &n).expect("the program");
        assert_eq!(found.launcher, Launcher { program: with_program.join(n.program), script: None });
        assert_eq!(found.path, Some(with_program.join(n.program)), "kept in Settings, as it isn't where Navi looks");
        // A Node version folder (nvm-windows) holding the package, with its own node beside it.
        if cfg!(windows) {
            let script = n.script.iter().fold(with_script.join("node_modules"), |p, part| p.join(part));
            std::fs::create_dir_all(script.parent().unwrap()).unwrap();
            std::fs::write(&script, "").unwrap();
            std::fs::write(with_script.join("node.exe"), "").unwrap();
            let found = search_dirs(&[empty.clone(), with_script.clone()], &n).expect("the script");
            assert_eq!(found.launcher, Launcher { program: with_script.join("node.exe"), script: Some(script.clone()) }, "run with that version's own Node");
            assert_eq!(launcher_for_path(&found.path.unwrap().to_string_lossy()), Some(found.launcher), "the kept path starts the same thing");
        }
        assert_eq!(search_dirs(&[empty.clone()], &n), None);
        for d in [empty, with_program, with_script] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn the_newest_editor_extension_wins() {
        let n = names("claude-code");
        let root = scratch("extensions");
        let old = root.join("anthropic.claude-code-2.1.278-win32-x64").join("resources").join("native-binary");
        let other = root.join("someone.else-1.0.0").join("bin");
        for d in [&old, &other] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join(n.program), "").unwrap();
        }
        std::thread::sleep(Duration::from_millis(20));
        let new = root.join("anthropic.claude-code-2.1.296-win32-x64").join("resources").join("native-binary");
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join(n.program), "").unwrap();
        let found = search_extensions(&[root.clone(), root.join("missing")], &n).expect("found");
        assert_eq!(found.launcher.program, new.join(n.program));
        assert_eq!(search_extensions(&[root.join("missing")], &n), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn node_comes_from_the_install_that_holds_the_script() {
        let prefix = scratch("prefix");
        let script = if cfg!(windows) { prefix.join("node_modules") } else { prefix.join("lib").join("node_modules") }.join("@openai").join("codex").join("bin").join("codex.js");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::write(&script, "").unwrap();
        let node = if cfg!(windows) { prefix.join("node.exe") } else { prefix.join("bin").join("node") };
        std::fs::create_dir_all(node.parent().unwrap()).unwrap();
        std::fs::write(&node, "").unwrap();
        let elsewhere = scratch("elsewhere");
        assert_eq!(node_launcher(script.clone(), &elsewhere), Some(Launcher { program: node, script: Some(script) }));
        let _ = std::fs::remove_dir_all(prefix);
        let _ = std::fs::remove_dir_all(elsewhere);
    }

    #[test]
    fn sign_in_status() {
        assert_eq!(claude_status("{\n  \"loggedIn\": true,\n  \"authMethod\": \"claude.ai\"\n}"), Some(true));
        assert_eq!(claude_status("{\"loggedIn\": false}"), Some(false));
        assert_eq!(claude_status("error: unknown command 'auth'"), None, "an older Claude Code");
        assert_eq!(codex_status("Logged in using ChatGPT\n", "", Some(0)), Some(true));
        assert_eq!(codex_status("", "Not logged in\n", Some(1)), Some(false));
        assert_eq!(codex_status("", "error: unrecognized subcommand 'status'", Some(2)), None);
    }

    #[test]
    fn sign_in_scripts_quote_every_path() {
        let parts: Vec<String> = [r"C:\Program Files\nodejs\node.exe", r"C:\Users\Zoë 100%\npm\codex.js", "login"].map(String::from).to_vec();
        let bat = sign_in_batch("Codex", &parts);
        assert!(bat.starts_with("@echo off\r\nchcp 65001 >nul\r\n"), "{bat}");
        assert!(bat.contains("\r\n\"C:\\Program Files\\nodejs\\node.exe\" \"C:\\Users\\Zoë 100%%\\npm\\codex.js\" \"login\"\r\n"), "{bat}");
        let parts: Vec<String> = ["/Users/zoe/.nvm/versions/node/v22.1.0/bin/node", "/Users/zoe/it's here/cli.js", "auth", "login"].map(String::from).to_vec();
        let sh = sign_in_shell("Claude Code", &parts, Path::new("/tmp/navi-ai"));
        assert!(sh.starts_with("#!/bin/bash\ncd '/tmp/navi-ai'\n"), "{sh}");
        assert!(sh.contains("\n'/Users/zoe/.nvm/versions/node/v22.1.0/bin/node' '/Users/zoe/it'\\''s here/cli.js' 'auth' 'login'\n"), "{sh}");
    }

    /// The real search and status checks on this computer. Run by hand:
    /// cargo test --lib text::cli_ai::tests::live_search -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_search() {
        #[cfg(windows)]
        println!("fresh PATH: {} folders, {} unexpanded", win::fresh_path().len(), win::fresh_path().iter().filter(|p| p.to_string_lossy().contains('%')).count());
        for id in ["claude-code", "codex"] {
            let t = std::time::Instant::now();
            let found = search(id);
            println!("{id}: {:?} in {} ms", found.as_ref().map(|f| (f.launcher.shown(), &f.path)), t.elapsed().as_millis());
            let n = names(id);
            let mut dirs = fresh_path();
            dirs.extend(install_dirs());
            let searched = search_dirs(&dirs, &n).map(|f| f.launcher.shown());
            let extension = search_extensions(&extension_roots(), &n).map(|f| f.launcher.shown());
            println!("  in folders: {searched:?}\n  in editors: {extension:?}");
            if let Some(f) = found {
                let t = std::time::Instant::now();
                println!("  signed in: {:?} in {} ms", signed_in(id, &f.launcher).await, t.elapsed().as_millis());
            }
        }
    }

    /// Asks the real Claude Code on this computer for a tiny rewrite (uses a sliver of the plan).
    /// Run by hand: cargo test --lib text::cli_ai::tests::live_claude_code -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_claude_code() {
        let t = std::time::Instant::now();
        let out = complete("claude-code", "haiku", "Rewrite the text as a short chat message in British English.", "ok so see you tomorrow then cheers", None).await;
        println!("{:?} in {} ms", out, t.elapsed().as_millis());
        assert!(out.is_ok());
    }

    /// The same through Codex. Run by hand: ... text::cli_ai::tests::live_codex -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_codex() {
        let t = std::time::Instant::now();
        let out = complete("codex", "", "Rewrite the text as a short chat message in British English.", "ok so see you tomorrow then cheers", None).await;
        println!("{:?} in {} ms", out, t.elapsed().as_millis());
        assert!(out.is_ok());
    }
}
