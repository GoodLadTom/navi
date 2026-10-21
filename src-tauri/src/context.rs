//! What the user is dictating into, read on this machine only.
//!
//! OWNER: Windows systems agent. STATUS: implemented.
//!
//! Contract:
//! * `capture` must be fast (target < 40 ms, hard cap ~120 ms, ~160 ms in a browser; give UI
//!   Automation calls a timeout or run them on a worker thread and abandon slow ones). It fills
//!   hwnd, process name (lower case), exe path, window title, a friendly `app_label`, a
//!   `category`, and, when `read_field` is on and
//!   the process is not excluded, `field_text` (up to ~1,500 chars before the caret, via UIA
//!   TextPattern/ValuePattern of the focused element) and `selection` (UIA TextPattern selection).
//!   Password fields (UIA IsPassword) are never read. In a browser, when `read_app` is on and the
//!   browser is not excluded, it also fills `host` from the address bar (`host_from_address`: the
//!   host alone, "chatgpt.com", so Auto can pick Prompt in a ChatGPT tab renamed after the chat).
//!   That read gets at most 150 ms; without it `host` stays empty and the title decides.
//! * Categories: terminal (windowsterminal, wt, cmd, powershell, pwsh, conhost, alacritty, wezterm,
//!   warp), editor (code, cursor, windsurf, zed, idea64, pycharm64, devenv, sublime_text, notepad++,
//!   bb / claude code style agent apps), chat (slack, teams, ms-teams, discord, whatsapp, telegram,
//!   signal, messenger), email (outlook, olk, thunderbird, mailbird; browsers whose title contains
//!   "Gmail" or "Outlook"), browser (chrome, msedge, firefox, brave, arc, opera), document (winword,
//!   notion, obsidian, onenote, wordpad, notepad, excel, powerpnt), else other.
//! * `read_focused_text(hwnd, settings)`: if `hwnd` is still the foreground window, the full text of
//!   its focused element (capped at 20,000 chars), for learning from corrections. None otherwise,
//!   and None when `read_field` is off or the window's app is excluded (`may_read_field`).
//! * `ocr_window(hwnd)`: optional on-device OCR (Windows.Media.Ocr) of the window; None if unavailable.
//!
//! Details worth knowing:
//! * UI Automation runs on a worker thread that owns COM (MTA) and one IUIAutomation; `capture`
//!   waits at most 100 ms for it and moves on without field text if the app is slow. Each UIA call
//!   also has a Windows-side timeout, and a worker stuck for over 3 s is abandoned and replaced.
//!   `read_focused_text` uses a second worker so a long read never delays a take, and a browser's
//!   address bar is read on a third, alongside the field. Finding the address bar walks the
//!   window's UIA tree, so it is done once per window and the element kept; after that each read
//!   is a single call. A search that outlasts the budget still finishes and is kept for next time,
//!   and a window with no address bar (an installed web app) is searched again only after a minute.
//!   The address itself is never logged or kept, only its host.
//! * Call `warm_up()` at startup: creating the UIA client costs tens of milliseconds and would
//!   otherwise eat the first take's budget.
//! * With `read_app` off, the window title is left empty; process name, label and category are still
//!   filled because mode picking and the excluded-apps check need them. An excluded app's title is
//!   never read at all (`kept_title`), so it can't reach the AI, History or a mode pick.
//! * Navi's own windows and elevated apps (UIA cannot reach them from a normal process) are skipped.
//! * UWP apps hosted by ApplicationFrameHost.exe are reported as the real app inside the frame.
//! * Newlines in returned text are "\n" only.

use crate::types::{AppContext, ContextSettings};
use std::time::Duration;

/// How long `capture` waits for UI Automation, from the start of the call.
const CAPTURE_UIA_BUDGET: Duration = Duration::from_millis(100);
/// How long `capture` waits for a browser's address, from the start of the call. It is read while
/// the field is, so this is the most a take in a browser waits, not an extra 150 ms.
const CAPTURE_ADDRESS_BUDGET: Duration = Duration::from_millis(150);
/// How long `read_focused_text` waits (it runs in the background, long after the take).
const READ_UIA_BUDGET: Duration = Duration::from_millis(1_500);
const FIELD_CHARS: usize = 1_500;
const SELECTION_CHARS: usize = 10_000;
const FULL_TEXT_CHARS: usize = 20_000;
const OCR_TIMEOUT: Duration = Duration::from_secs(5);
const OCR_MAX_CHARS: usize = 10_000;

pub fn capture(settings: &ContextSettings) -> AppContext {
    #[cfg(windows)]
    {
        win::capture(settings)
    }
    #[cfg(target_os = "macos")]
    {
        mac::capture(settings)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = settings;
        AppContext { category: "other".into(), ..Default::default() }
    }
}

pub fn foreground_hwnd() -> isize {
    #[cfg(windows)]
    {
        win::foreground_hwnd()
    }
    #[cfg(target_os = "macos")]
    {
        mac::foreground_hwnd()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    0
}

/// The process id of the app in front (macOS), for "is Navi in front?".
#[cfg(target_os = "macos")]
pub fn foreground_pid() -> Option<i32> {
    mac::foreground_pid()
}

pub fn read_focused_text(hwnd: isize, settings: &ContextSettings) -> Option<String> {
    #[cfg(windows)]
    {
        win::read_focused_text(hwnd, settings)
    }
    #[cfg(target_os = "macos")]
    {
        mac::read_focused_text(hwnd, settings)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (hwnd, settings);
        None
    }
}

/// On-device OCR of the window with Windows.Media.Ocr (lines joined with "\n"). Only when `hwnd` is
/// the foreground window, because it reads the window's pixels from the screen. None if Windows has
/// no OCR language installed or recognition fails. Blocks for up to 5 s.
pub fn ocr_window(hwnd: isize) -> Option<String> {
    #[cfg(windows)]
    {
        win::ocr_window(hwnd)
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd;
        None
    }
}

/// On-device OCR of a picture (top-down BGRA, opaque), for screenshots (screen.rs). Blocks for up to
/// 5 s; None when Windows has no OCR language or reads nothing.
pub fn ocr_bgra(width: u32, height: u32, pixels: Vec<u8>) -> Option<String> {
    #[cfg(windows)]
    {
        win::ocr_bgra(width, height, pixels)
    }
    #[cfg(not(windows))]
    {
        let _ = (width, height, pixels);
        None
    }
}

/// The foreground window's app (as the interface names it), title and process name.
pub fn foreground_app() -> (String, String, String) {
    #[cfg(windows)]
    {
        win::foreground_app()
    }
    #[cfg(target_os = "macos")]
    {
        mac::foreground_app()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    (String::new(), String::new(), String::new())
}

/// "Good morning, <first name>": the Windows display name's first word if available, else USERNAME.
pub fn user_first_name() -> String {
    #[cfg(windows)]
    let from_windows = win::display_name()
        .and_then(|n| first_name_from_display(&n))
        .or_else(|| win::local_full_name().and_then(|n| first_name_from_display(&n)));
    #[cfg(target_os = "macos")]
    let from_windows = mac::display_name().and_then(|n| first_name_from_display(&n));
    #[cfg(not(any(windows, target_os = "macos")))]
    let from_windows: Option<String> = None;
    from_windows
        .or_else(|| std::env::var("USERNAME").ok().and_then(|u| first_name_from_username(&u)))
        .or_else(|| std::env::var("USER").ok().and_then(|u| first_name_from_username(&u)))
        .unwrap_or_default()
}

/// Starts the UI Automation worker ahead of the first take.
pub fn warm_up() {
    #[cfg(windows)]
    win::warm_up();
    #[cfg(target_os = "macos")]
    mac::warm_up();
}

/// Category of whatever is in front right now (so `insert` can treat terminals differently).
pub(crate) fn foreground_category() -> &'static str {
    #[cfg(windows)]
    {
        win::foreground_category()
    }
    #[cfg(target_os = "macos")]
    {
        mac::foreground_category()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    "other"
}

/// Lower-case process name of the foreground window ("mintty.exe"), empty if unknown.
pub(crate) fn foreground_process_name() -> String {
    #[cfg(windows)]
    {
        win::foreground_process_name()
    }
    #[cfg(target_os = "macos")]
    {
        mac::foreground_process_name()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    String::new()
}

// ---------------------------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------------------------

/// "Code.exe" -> "code", "WhatsApp.Root.exe" -> "whatsapp.root".
pub fn process_stem(process_name: &str) -> String {
    let name = process_name.trim().rsplit(['\\', '/']).next().unwrap_or("").to_lowercase();
    match name.strip_suffix(".exe") {
        Some(stem) => stem.to_string(),
        None => name,
    }
}

const TERMINALS: &[&str] = &[
    "windowsterminal", "wt", "openconsole", "cmd", "powershell", "pwsh", "conhost", "alacritty", "wezterm", "wezterm-gui",
    "warp", "mintty", "tabby", "hyper", "putty", "kitty", "rio",
];
const EDITORS: &[&str] = &[
    "code", "code - insiders", "codium", "cursor", "windsurf", "zed", "idea", "idea64", "pycharm", "pycharm64", "webstorm64",
    "rider64", "clion64", "goland64", "phpstorm64", "rubymine64", "rustrover64", "datagrip64", "studio64", "fleet", "devenv",
    "sublime_text", "notepad++", "bb", "claude", "codex", "kiro", "trae",
];
const CHATS: &[&str] = &[
    "slack", "teams", "ms-teams", "discord", "whatsapp", "whatsapp.root", "telegram", "signal", "messenger", "element",
];
const EMAIL: &[&str] = &["outlook", "olk", "hxoutlook", "thunderbird", "mailbird", "mailspring", "mailclient"];
const BROWSERS: &[&str] = &["chrome", "msedge", "firefox", "brave", "arc", "opera", "vivaldi", "zen", "librewolf", "chromium"];
const DOCUMENTS: &[&str] = &[
    "winword", "notion", "obsidian", "onenote", "onenoteim", "wordpad", "notepad", "excel", "powerpnt", "soffice.bin",
    "typora", "logseq", "evernote",
];

// Apple's own apps and Mac-only ones, named as `mac::front` names them. Kept apart so the
// Windows lists stay exactly as they were.
#[cfg(target_os = "macos")]
const MAC_TERMINALS: &[&str] = &["terminal", "iterm2", "ghostty"];
#[cfg(target_os = "macos")]
const MAC_EDITORS: &[&str] = &["xcode"];
#[cfg(target_os = "macos")]
const MAC_CHATS: &[&str] = &["messages"];
#[cfg(target_os = "macos")]
const MAC_EMAIL: &[&str] = &["mail", "spark", "superhuman"];
#[cfg(target_os = "macos")]
const MAC_BROWSERS: &[&str] = &["safari", "orion", "dia"];
#[cfg(target_os = "macos")]
const MAC_DOCUMENTS: &[&str] = &["notes", "textedit", "pages", "keynote", "numbers"];
#[cfg(not(target_os = "macos"))]
const MAC_TERMINALS: &[&str] = &[];
#[cfg(not(target_os = "macos"))]
const MAC_EDITORS: &[&str] = &[];
#[cfg(not(target_os = "macos"))]
const MAC_CHATS: &[&str] = &[];
#[cfg(not(target_os = "macos"))]
const MAC_EMAIL: &[&str] = &[];
#[cfg(not(target_os = "macos"))]
const MAC_BROWSERS: &[&str] = &[];
#[cfg(not(target_os = "macos"))]
const MAC_DOCUMENTS: &[&str] = &[];

/// "terminal" | "editor" | "chat" | "email" | "browser" | "document" | "other".
pub fn categorise(process_name: &str, window_title: &str) -> &'static str {
    let stem = process_stem(process_name);
    let s = stem.as_str();
    if TERMINALS.contains(&s) || MAC_TERMINALS.contains(&s) {
        "terminal"
    } else if EDITORS.contains(&s) || MAC_EDITORS.contains(&s) {
        "editor"
    } else if CHATS.contains(&s) || MAC_CHATS.contains(&s) {
        "chat"
    } else if EMAIL.contains(&s) || MAC_EMAIL.contains(&s) {
        "email"
    } else if BROWSERS.contains(&s) || MAC_BROWSERS.contains(&s) {
        if title_mentions(window_title, "Gmail") || title_mentions(window_title, "Outlook") {
            "email"
        } else {
            "browser"
        }
    } else if DOCUMENTS.contains(&s) || MAC_DOCUMENTS.contains(&s) {
        "document"
    } else {
        "other"
    }
}

/// A web browser (chrome, msedge, firefox, brave and the rest of BROWSERS), whose address bar
/// `capture` reads.
pub fn is_browser(process_name: &str) -> bool {
    let stem = process_stem(process_name);
    BROWSERS.contains(&stem.as_str()) || MAC_BROWSERS.contains(&stem.as_str())
}

/// The host of what a browser's address bar shows, lower case and without "www.":
/// "chatgpt.com/c/123" and "https://www.chatgpt.com/c/123" both give "chatgpt.com". None for
/// anything that isn't a web address: a search being typed, "about:blank", "chrome://settings".
pub fn host_from_address(address: &str) -> Option<String> {
    let text = address.trim();
    if text.is_empty() || text.chars().any(char::is_whitespace) {
        return None;
    }
    // Browsers often hide "https://" while the address isn't being edited (Chrome does).
    let full = if text.contains("://") { text.to_string() } else { format!("https://{text}") };
    let url = url::Url::parse(&full).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?.trim_end_matches('.').to_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    // A single word ("weather") is a search, not a site.
    host.contains('.').then(|| host.to_string())
}

/// Whole-word, case-insensitive: "Inbox - Gmail" mentions Gmail, "Outlooking" does not mention Outlook.
pub fn title_mentions(title: &str, word: &str) -> bool {
    let title = title.to_lowercase();
    let word = word.to_lowercase();
    if word.is_empty() {
        return false;
    }
    let mut from = 0;
    while let Some(pos) = title[from..].find(&word) {
        let start = from + pos;
        let end = start + word.len();
        let before_ok = title[..start].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
        let after_ok = title[end..].chars().next().is_none_or(|c| !c.is_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
        from = start + word.len().max(1);
    }
    false
}

const APP_LABELS: &[(&str, &str)] = &[
    ("outlook", "Outlook"),
    ("olk", "Outlook"),
    ("hxoutlook", "Mail"),
    ("thunderbird", "Thunderbird"),
    ("mailbird", "Mailbird"),
    ("windowsterminal", "Windows Terminal"),
    ("wt", "Windows Terminal"),
    ("openconsole", "Windows Terminal"),
    ("cmd", "Command Prompt"),
    ("powershell", "PowerShell"),
    ("pwsh", "PowerShell"),
    ("conhost", "Console"),
    ("alacritty", "Alacritty"),
    ("wezterm", "WezTerm"),
    ("wezterm-gui", "WezTerm"),
    ("warp", "Warp"),
    ("mintty", "Git Bash"),
    ("code", "Visual Studio Code"),
    ("code - insiders", "VS Code Insiders"),
    ("cursor", "Cursor"),
    ("windsurf", "Windsurf"),
    ("zed", "Zed"),
    ("idea64", "IntelliJ IDEA"),
    ("pycharm64", "PyCharm"),
    ("webstorm64", "WebStorm"),
    ("rider64", "Rider"),
    ("devenv", "Visual Studio"),
    ("sublime_text", "Sublime Text"),
    ("notepad++", "Notepad++"),
    ("bb", "BB"),
    ("claude", "Claude"),
    ("slack", "Slack"),
    ("teams", "Teams"),
    ("ms-teams", "Teams"),
    ("discord", "Discord"),
    ("whatsapp", "WhatsApp"),
    ("whatsapp.root", "WhatsApp"),
    ("telegram", "Telegram"),
    ("signal", "Signal"),
    ("messenger", "Messenger"),
    ("chrome", "Chrome"),
    ("msedge", "Edge"),
    ("firefox", "Firefox"),
    ("brave", "Brave"),
    ("arc", "Arc"),
    ("opera", "Opera"),
    ("vivaldi", "Vivaldi"),
    ("winword", "Word"),
    ("excel", "Excel"),
    ("powerpnt", "PowerPoint"),
    ("onenote", "OneNote"),
    ("notion", "Notion"),
    ("obsidian", "Obsidian"),
    ("wordpad", "WordPad"),
    ("notepad", "Notepad"),
    ("explorer", "File Explorer"),
    ("searchhost", "Windows Search"),
    ("navi", "Navi"),
];

/// Friendly name for well-known apps.
pub fn known_app_label(process_name: &str) -> Option<&'static str> {
    let stem = process_stem(process_name);
    APP_LABELS.iter().find(|(s, _)| *s == stem).map(|(_, label)| *label)
}

/// A file description (from the exe's version info) good enough to show as an app name.
pub fn usable_description(description: &str) -> Option<String> {
    let d = description.trim();
    let generic = ["electron", "chromium", "application", "app", "launcher", "java(tm) platform se binary"];
    let ok = (2..=40).contains(&d.chars().count()) && !generic.iter().any(|g| d.eq_ignore_ascii_case(g));
    ok.then(|| d.to_string())
}

/// Last resort: "granola.exe" -> "Granola".
pub fn label_from_stem(process_name: &str) -> String {
    capitalise(&process_stem(process_name))
}

fn capitalise(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Case-insensitive, with or without ".exe": "1Password" excludes "1password.exe".
pub fn is_excluded(process_name: &str, excluded: &[String]) -> bool {
    let stem = process_stem(process_name);
    !stem.is_empty() && excluded.iter().any(|e| process_stem(e) == stem)
}

/// Whether the text in `process_name`'s fields may be read: "Read the text near my cursor" is on,
/// the app is known, and it isn't under "Never read these apps".
pub fn may_read_field(process_name: &str, settings: &ContextSettings) -> bool {
    settings.read_field && !process_name.trim().is_empty() && !is_excluded(process_name, &settings.excluded_apps)
}

/// The window title kept for a take or a screenshot: none with "Read the window title" off, and
/// never an excluded app's, which would otherwise go to the AI and into History.
pub fn kept_title(title: String, read_app: bool, excluded: bool) -> String {
    if read_app && !excluded {
        title
    } else {
        String::new()
    }
}

/// "\r\n" and "\r" become "\n"; NULs and U+FFFC (embedded-object placeholders) are dropped.
pub(crate) fn normalise_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\0' | '\u{FFFC}' => {}
            _ => out.push(c),
        }
    }
    out
}

/// The last `n` characters.
pub(crate) fn tail_chars(text: &str, n: usize) -> &str {
    let count = text.chars().count();
    if count <= n {
        return text;
    }
    let skip = count - n;
    let start = text.char_indices().nth(skip).map(|(i, _)| i).unwrap_or(text.len());
    &text[start..]
}

/// The first `n` characters.
pub(crate) fn head_chars(text: &str, n: usize) -> &str {
    match text.char_indices().nth(n) {
        Some((i, _)) => &text[..i],
        None => text,
    }
}

fn non_empty(text: &str) -> Option<String> {
    (!text.trim().is_empty()).then(|| text.to_string())
}

/// "Jane Doe" -> "Jane"; "Doe, Jane" -> "Jane"; "Dr Sam Smith" -> "Sam"; an email address -> None.
pub fn first_name_from_display(display: &str) -> Option<String> {
    let s = display.trim();
    if s.is_empty() || s.contains('@') {
        return None;
    }
    let given = match s.split_once(',') {
        Some((_, rest)) if !rest.trim().is_empty() => rest.trim(),
        _ => s,
    };
    const TITLES: [&str; 7] = ["dr", "mr", "mrs", "ms", "miss", "prof", "sir"];
    given
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '\''))
        .filter(|w| !w.is_empty())
        .find(|w| !TITLES.contains(&w.to_lowercase().as_str()))
        .map(|w| w.to_string())
}

/// "jdoe" -> "Jdoe"; "jane.doe" -> "Jane"; "DOMAIN\sam_jones" -> "Sam"; "JO" -> "Jo".
pub fn first_name_from_username(user: &str) -> Option<String> {
    let user = user.trim();
    let user = user.rsplit('\\').next().unwrap_or(user);
    let user = user.split('@').next().unwrap_or(user);
    let part = user.split(|c: char| matches!(c, '.' | '_' | '-') || c.is_whitespace()).find(|p| !p.is_empty())?;
    let part = part.trim_end_matches(|c: char| c.is_ascii_digit());
    if part.is_empty() || !part.chars().any(|c| c.is_alphabetic()) {
        return None;
    }
    let all_upper = part.chars().filter(|c| c.is_alphabetic()).all(|c| c.is_uppercase());
    let all_lower = part.chars().filter(|c| c.is_alphabetic()).all(|c| c.is_lowercase());
    if all_upper || all_lower {
        Some(capitalise(&part.to_lowercase()))
    } else {
        Some(part.to_string())
    }
}

/// Scales (w, h) down to fit within `max` on both sides, keeping the aspect ratio.
pub(crate) fn fit_within(w: i32, h: i32, max: i32) -> (i32, i32) {
    let longest = w.max(h);
    if longest <= max || longest <= 0 {
        return (w, h);
    }
    let scale = max as f64 / longest as f64;
    (((w as f64 * scale).round() as i32).max(1), ((h as f64 * scale).round() as i32).max(1))
}

// ---------------------------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------------------------

#[cfg(windows)]
mod win {
    use super::*;
    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc;
    use std::sync::{Arc, OnceLock};
    use std::time::Instant;
    use windows::core::{Interface, BOOL, HSTRING, PCWSTR, PWSTR};
    use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::DataWriter;
    use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, GetDC, ReleaseDC, SelectObject, SetStretchBltMode,
        StretchBlt, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HALFTONE, HGDIOBJ, SRCCOPY,
    };
    use windows::Win32::NetworkManagement::NetManagement::{NetApiBufferFree, NetUserGetInfo, USER_INFO_10};
    use windows::Win32::Security::Authentication::Identity::{GetUserNameExW, NameDisplay};
    use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
    use windows::Win32::System::Threading::{
        GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, CUIAutomation8, IUIAutomation, IUIAutomation2, IUIAutomationElement, IUIAutomationTextPattern,
        IUIAutomationTextPattern2, IUIAutomationTextRange, IUIAutomationValuePattern, PropertyConditionFlags_MatchSubstring,
        TextPatternRangeEndpoint_End, TextPatternRangeEndpoint_Start, TextUnit_Character, TreeScope_Descendants,
        UIA_AutomationIdPropertyId, UIA_ClassNamePropertyId, UIA_ControlTypePropertyId, UIA_EditControlTypeId, UIA_NamePropertyId,
        UIA_TextPattern2Id, UIA_TextPatternId, UIA_ValuePatternId,
    };
    use windows::Win32::UI::WindowsAndMessaging::{EnumChildWindows, GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId, IsIconic};

    // ----- the foreground window -----

    pub(super) fn foreground_hwnd() -> isize {
        unsafe { GetForegroundWindow() }.0 as isize
    }

    struct WindowInfo {
        pid: u32,
        exe_path: String,
        /// Lower case, e.g. "outlook.exe"; empty if the process could not be opened.
        process_name: String,
    }

    fn window_info(hwnd: HWND) -> WindowInfo {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        let info = process_info(pid);
        if info.process_name == "applicationframehost.exe" {
            if let Some(child) = hosted_app_pid(hwnd, pid).map(process_info) {
                if !child.process_name.is_empty() {
                    return child;
                }
            }
        }
        info
    }

    fn process_info(pid: u32) -> WindowInfo {
        let exe_path = if pid == 0 { None } else { process_path(pid) }.unwrap_or_default();
        let process_name = exe_path.rsplit(['\\', '/']).next().unwrap_or("").to_lowercase();
        WindowInfo { pid, exe_path, process_name }
    }

    fn process_path(pid: u32) -> Option<String> {
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut path = None;
            for capacity in [1_024usize, 32_768] {
                let mut buf = vec![0u16; capacity];
                let mut len = buf.len() as u32;
                if QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok() {
                    path = Some(String::from_utf16_lossy(&buf[..len as usize]));
                    break;
                }
            }
            let _ = CloseHandle(handle);
            path
        }
    }

    /// A UWP app's top-level window belongs to ApplicationFrameHost.exe; the app's own content is a
    /// child window owned by the real process.
    fn hosted_app_pid(frame: HWND, frame_pid: u32) -> Option<u32> {
        struct Search {
            frame_pid: u32,
            found: u32,
        }
        unsafe extern "system" fn visit(child: HWND, lparam: LPARAM) -> BOOL {
            // SAFETY: lparam is the &mut Search passed below, alive for the whole enumeration.
            let search = unsafe { &mut *(lparam.0 as *mut Search) };
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(child, Some(&mut pid)) };
            if pid != 0 && pid != search.frame_pid {
                search.found = pid;
                return BOOL(0);
            }
            BOOL(1)
        }
        let mut search = Search { frame_pid, found: 0 };
        unsafe {
            let _ = EnumChildWindows(Some(frame), Some(visit), LPARAM(&mut search as *mut Search as isize));
        }
        (search.found != 0).then_some(search.found)
    }

    fn window_title(hwnd: HWND) -> String {
        // For other processes' windows this reads the stored caption without sending WM_GETTEXT,
        // so a hung app cannot block it.
        let mut buf = [0u16; 512];
        let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
        if len <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..len as usize])
    }

    fn app_label(info: &WindowInfo) -> String {
        if info.process_name.is_empty() {
            return String::new();
        }
        if let Some(label) = known_app_label(&info.process_name) {
            return label.to_string();
        }
        static CACHE: Mutex<Option<HashMap<String, Option<String>>>> = parking_lot::const_mutex(None);
        let cached = CACHE.lock().as_ref().and_then(|m| m.get(&info.exe_path).cloned());
        let description = match cached {
            Some(d) => d,
            None => {
                let d = file_description(&info.exe_path).and_then(|d| usable_description(&d));
                CACHE.lock().get_or_insert_with(HashMap::new).insert(info.exe_path.clone(), d.clone());
                d
            }
        };
        description.unwrap_or_else(|| label_from_stem(&info.process_name))
    }

    fn file_description(path: &str) -> Option<String> {
        if path.is_empty() {
            return None;
        }
        let wide = HSTRING::from(path);
        unsafe {
            let size = GetFileVersionInfoSizeW(&wide, None);
            if size == 0 {
                return None;
            }
            let mut data = vec![0u8; size as usize];
            GetFileVersionInfoW(&wide, None, size, data.as_mut_ptr() as *mut c_void).ok()?;
            let mut ptr: *mut c_void = std::ptr::null_mut();
            let mut len = 0u32;
            if !VerQueryValueW(data.as_ptr() as *const c_void, windows::core::w!("\\VarFileInfo\\Translation"), &mut ptr, &mut len).as_bool()
                || ptr.is_null()
                || len < 4
            {
                return None;
            }
            let lang = std::ptr::read_unaligned(ptr as *const u16);
            let codepage = std::ptr::read_unaligned((ptr as *const u16).add(1));
            let key = HSTRING::from(format!("\\StringFileInfo\\{lang:04x}{codepage:04x}\\FileDescription"));
            let mut text: *mut c_void = std::ptr::null_mut();
            let mut chars = 0u32;
            if !VerQueryValueW(data.as_ptr() as *const c_void, &key, &mut text, &mut chars).as_bool() || text.is_null() || chars == 0 {
                return None;
            }
            let units: Vec<u16> = (0..chars as usize).map(|i| std::ptr::read_unaligned((text as *const u16).add(i))).collect();
            let s = String::from_utf16_lossy(&units).trim_end_matches('\0').trim().to_string();
            (!s.is_empty()).then_some(s)
        }
    }

    pub(super) fn foreground_category() -> &'static str {
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.0.is_null() {
            return "other";
        }
        let info = window_info(hwnd);
        categorise(&info.process_name, &window_title(hwnd))
    }

    pub(super) fn foreground_process_name() -> String {
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.0.is_null() {
            return String::new();
        }
        window_info(hwnd).process_name
    }

    pub(super) fn foreground_app() -> (String, String, String) {
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.0.is_null() {
            return (String::new(), String::new(), String::new());
        }
        let info = window_info(hwnd);
        (app_label(&info), window_title(hwnd), info.process_name)
    }

    pub(super) fn capture(settings: &ContextSettings) -> AppContext {
        let started = Instant::now();
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.0.is_null() {
            return AppContext { category: "other".into(), ..Default::default() };
        }
        let info = window_info(hwnd);
        let excluded = is_excluded(&info.process_name, &settings.excluded_apps);
        // Never read means never read: an excluded app's title isn't even used for its category.
        let title = if excluded { String::new() } else { window_title(hwnd) };
        let mut ctx = AppContext {
            hwnd: hwnd.0 as isize,
            category: categorise(&info.process_name, &title).to_string(),
            app_label: app_label(&info),
            window_title: kept_title(title, settings.read_app, excluded),
            process_name: info.process_name.clone(),
            exe_path: info.exe_path.clone(),
            field_text: None,
            selection: None,
            host: String::new(),
        };

        let own = info.pid == unsafe { GetCurrentProcessId() };
        let readable = !info.process_name.is_empty() && !excluded;
        // A browser's address is asked for first, so its worker reads it while the field is read.
        let address_deadline = started + CAPTURE_ADDRESS_BUDGET;
        let address = if settings.read_app && readable && !own && is_browser(&info.process_name) {
            ADDRESS_WORKER.send(Job::Address(hwnd.0 as isize), address_deadline)
        } else {
            None
        };
        if may_read_field(&info.process_name, settings) && !own {
            if let Some(Reply::Field { before, selection }) = CAPTURE_WORKER.call(Job::Field, started + CAPTURE_UIA_BUDGET) {
                // Only trust the answer if the same window is still in front.
                if unsafe { GetForegroundWindow() } == hwnd {
                    ctx.field_text = before;
                    ctx.selection = selection;
                }
            }
        }
        if let Some(reply) = address {
            // Too slow (the first take in a window can be) or unreadable: the title decides.
            if let Ok(Reply::Address(Some(host))) = reply.recv_timeout(address_deadline.saturating_duration_since(Instant::now())) {
                if unsafe { GetForegroundWindow() } == hwnd {
                    ctx.host = host;
                }
            }
        }
        ctx
    }

    pub(super) fn read_focused_text(hwnd: isize, settings: &ContextSettings) -> Option<String> {
        if hwnd == 0 || foreground_hwnd() != hwnd {
            return None;
        }
        // Checked against the app in that window now, with today's settings: an app put under
        // "Never read these apps" since the paste is not read either.
        if !may_read_field(&window_info(HWND(hwnd as *mut c_void)).process_name, settings) {
            return None;
        }
        let deadline = Instant::now() + READ_UIA_BUDGET;
        match READ_WORKER.call(Job::FullText, deadline) {
            Some(Reply::FullText(text)) if foreground_hwnd() == hwnd => text,
            _ => None,
        }
    }

    pub(super) fn warm_up() {
        CAPTURE_WORKER.ensure_started();
        ADDRESS_WORKER.ensure_started();
    }

    // ----- UI Automation worker -----

    #[derive(Clone, Copy)]
    enum Job {
        /// Text before the caret and the selection.
        Field,
        /// The whole focused element, capped.
        FullText,
        /// The host in the address bar of this browser window.
        Address(isize),
    }

    enum Reply {
        Field { before: Option<String>, selection: Option<String> },
        FullText(Option<String>),
        Address(Option<String>),
    }

    struct Request {
        job: Job,
        deadline: Instant,
        reply: mpsc::Sender<Reply>,
    }

    struct Worker {
        tx: mpsc::Sender<Request>,
        /// Milliseconds (see `now_ms`) when the current request started; 0 while idle.
        busy_since: Arc<AtomicU64>,
    }

    /// A worker busy this long on one request is presumed stuck in a frozen app.
    const STUCK_AFTER_MS: u64 = 3_000;

    fn now_ms() -> u64 {
        static EPOCH: OnceLock<Instant> = OnceLock::new();
        EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
    }

    struct WorkerSlot {
        name: &'static str,
        worker: Mutex<Option<Worker>>,
    }

    static CAPTURE_WORKER: WorkerSlot = WorkerSlot { name: "navi-uia", worker: parking_lot::const_mutex(None) };
    static READ_WORKER: WorkerSlot = WorkerSlot { name: "navi-uia-read", worker: parking_lot::const_mutex(None) };
    static ADDRESS_WORKER: WorkerSlot = WorkerSlot { name: "navi-uia-address", worker: parking_lot::const_mutex(None) };

    impl WorkerSlot {
        fn ensure_started(&self) {
            let mut slot = self.worker.lock();
            let stuck = slot.as_ref().is_some_and(|w| {
                let since = w.busy_since.load(Ordering::Relaxed);
                since != 0 && now_ms().saturating_sub(since) > STUCK_AFTER_MS
            });
            if stuck {
                // Its thread finishes (or not) on its own; requests go to a fresh one.
                log::warn!("UI Automation worker {} is stuck; starting a new one", self.name);
                *slot = None;
            }
            if slot.is_none() {
                *slot = spawn_worker(self.name);
            }
        }

        fn call(&self, job: Job, deadline: Instant) -> Option<Reply> {
            let reply_rx = self.send(job, deadline)?;
            reply_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).ok()
        }

        /// Hands `job` to the worker without waiting; the reply comes on the receiver.
        fn send(&self, job: Job, deadline: Instant) -> Option<mpsc::Receiver<Reply>> {
            let (reply_tx, reply_rx) = mpsc::channel();
            self.ensure_started();
            let mut slot = self.worker.lock();
            let worker = slot.as_ref()?;
            if worker.tx.send(Request { job, deadline, reply: reply_tx }).is_err() {
                *slot = None;
                return None;
            }
            Some(reply_rx)
        }
    }

    fn spawn_worker(name: &'static str) -> Option<Worker> {
        let (tx, rx) = mpsc::channel::<Request>();
        let busy_since = Arc::new(AtomicU64::new(0));
        let busy = busy_since.clone();
        match std::thread::Builder::new().name(name.into()).spawn(move || worker_main(rx, busy)) {
            Ok(_) => Some(Worker { tx, busy_since }),
            Err(e) => {
                log::warn!("Could not start the UI Automation worker: {e}");
                None
            }
        }
    }

    fn worker_main(rx: mpsc::Receiver<Request>, busy: Arc<AtomicU64>) {
        let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let automation = create_automation();
        if automation.is_none() {
            log::warn!("UI Automation is not available; Navi will work without reading the text field");
        }
        // Each browser window's address bar, once found (only the address worker fills it).
        let mut address_bars = AddressBars::default();
        for request in rx {
            // Abandoned requests are skipped rather than worked on for nobody.
            if Instant::now() >= request.deadline {
                continue;
            }
            busy.store(now_ms(), Ordering::Relaxed);
            let reply = match request.job {
                Job::Field => {
                    let (before, selection) = automation.as_ref().map(read_field).unwrap_or((None, None));
                    Reply::Field { before, selection }
                }
                Job::FullText => Reply::FullText(automation.as_ref().and_then(read_full)),
                Job::Address(hwnd) => Reply::Address(automation.as_ref().and_then(|a| read_address(a, hwnd, &mut address_bars))),
            };
            busy.store(0, Ordering::Relaxed);
            let _ = request.reply.send(reply);
        }
        drop(address_bars);
        drop(automation);
        if com.is_ok() {
            unsafe { CoUninitialize() };
        }
    }

    fn create_automation() -> Option<IUIAutomation> {
        unsafe {
            let automation: IUIAutomation = CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)
                .or_else(|_| CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER))
                .ok()?;
            if let Ok(automation2) = automation.cast::<IUIAutomation2>() {
                // Bound each cross-process call, so one frozen app cannot hold the worker for long.
                let _ = automation2.SetConnectionTimeout(500);
                let _ = automation2.SetTransactionTimeout(1_000);
            }
            Some(automation)
        }
    }

    /// The focused element, unless it is a password field or one of Navi's own.
    fn focused_element(automation: &IUIAutomation) -> Option<IUIAutomationElement> {
        unsafe {
            let element = automation.GetFocusedElement().ok()?;
            // If the property cannot be read, assume the worst.
            if element.CurrentIsPassword().map(|b| b.as_bool()).unwrap_or(true) {
                return None;
            }
            if element.CurrentProcessId().is_ok_and(|pid| pid as u32 == GetCurrentProcessId()) {
                return None;
            }
            Some(element)
        }
    }

    fn read_field(automation: &IUIAutomation) -> (Option<String>, Option<String>) {
        let Some(element) = focused_element(automation) else {
            return (None, None);
        };
        unsafe {
            if let Ok(pattern2) = element.GetCurrentPatternAs::<IUIAutomationTextPattern2>(UIA_TextPattern2Id) {
                let ranges = selection_ranges(&pattern2);
                let mut active = BOOL(0);
                let caret = pattern2.GetCaretRange(&mut active).ok().or_else(|| ranges.first().cloned());
                return (caret.and_then(|c| text_before(&c, FIELD_CHARS)), selection_text(&ranges));
            }
            if let Ok(pattern) = element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId) {
                let ranges = selection_ranges(&pattern);
                return (ranges.first().and_then(|c| text_before(c, FIELD_CHARS)), selection_text(&ranges));
            }
            if let Ok(value) = element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) {
                // No caret position here; assume the user is typing at the end.
                if let Ok(text) = value.CurrentValue() {
                    return (non_empty(tail_chars(&normalise_text(&text.to_string()), FIELD_CHARS)), None);
                }
            }
        }
        (None, None)
    }

    fn read_full(automation: &IUIAutomation) -> Option<String> {
        let element = focused_element(automation)?;
        unsafe {
            let pattern2 = element.GetCurrentPatternAs::<IUIAutomationTextPattern2>(UIA_TextPattern2Id).ok();
            let pattern = match &pattern2 {
                Some(p2) => p2.cast::<IUIAutomationTextPattern>().ok(),
                None => element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId).ok(),
            };
            if let Some(pattern) = pattern {
                let document = pattern.DocumentRange().ok()?;
                let text = normalise_text(&document.GetText((FULL_TEXT_CHARS + 1) as i32).ok()?.to_string());
                if text.chars().count() <= FULL_TEXT_CHARS {
                    return non_empty(&text);
                }
                // Too long: keep the stretch around the caret, where the user has just been editing.
                let caret = pattern2
                    .and_then(|p2| {
                        let mut active = BOOL(0);
                        p2.GetCaretRange(&mut active).ok()
                    })
                    .or_else(|| selection_ranges(&pattern).into_iter().next());
                if let Some(around) = caret.and_then(|c| text_around(&c, FULL_TEXT_CHARS * 3 / 4, FULL_TEXT_CHARS / 4)) {
                    return Some(around);
                }
                return non_empty(head_chars(&text, FULL_TEXT_CHARS));
            }
            if let Ok(value) = element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) {
                let text = normalise_text(&value.CurrentValue().ok()?.to_string());
                return non_empty(tail_chars(&text, FULL_TEXT_CHARS));
            }
        }
        None
    }

    /// A browser window with no address bar to find (an installed web app, full screen) is looked
    /// through again only after this long, so its takes don't each wait for a search.
    const NO_ADDRESS_BAR_RETRY: Duration = Duration::from_secs(60);

    /// The address bars found so far, by window, and the windows that had none.
    #[derive(Default)]
    struct AddressBars {
        found: HashMap<isize, IUIAutomationElement>,
        missing: HashMap<isize, Instant>,
    }

    /// The host in the address bar of the browser window `hwnd`. The bar is looked for once per
    /// window and kept in `bars`; reading a kept one is a single call.
    fn read_address(automation: &IUIAutomation, hwnd: isize, bars: &mut AddressBars) -> Option<String> {
        if let Some(bar) = bars.found.get(&hwnd) {
            match address_text(bar) {
                Some(text) => return host_from_address(&text),
                // The window has gone, or the browser made a new address bar: look again.
                None => {
                    bars.found.remove(&hwnd);
                }
            }
        }
        if bars.missing.get(&hwnd).is_some_and(|at| at.elapsed() < NO_ADDRESS_BAR_RETRY) {
            return None;
        }
        if bars.found.len() + bars.missing.len() >= 32 {
            bars.found.clear();
            bars.missing.clear();
        }
        let Some(bar) = find_address_bar(automation, HWND(hwnd as *mut c_void)) else {
            bars.missing.insert(hwnd, Instant::now());
            return None;
        };
        bars.missing.remove(&hwnd);
        let text = address_text(&bar);
        bars.found.insert(hwnd, bar);
        host_from_address(&text?)
    }

    /// What the address bar shows ("" when it is empty), or None when it can't be read.
    fn address_text(bar: &IUIAutomationElement) -> Option<String> {
        unsafe {
            let value = bar.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId).ok()?;
            Some(value.CurrentValue().ok()?.to_string())
        }
    }

    /// A browser window's address bar: an edit box whose class is Chromium's omnibox (Chrome's is
    /// "OmniboxViewViews", named "Address and search bar" in English), or whose id is Firefox's
    /// "urlbar-input".
    fn find_address_bar(automation: &IUIAutomation, window: HWND) -> Option<IUIAutomationElement> {
        unsafe {
            let root = automation.ElementFromHandle(window).ok()?;
            let edit = automation.CreatePropertyCondition(UIA_ControlTypePropertyId, &VARIANT::from(UIA_EditControlTypeId.0)).ok()?;
            let class = VARIANT::from("OmniboxViewViews");
            // Part of the class name, so a browser that renames Chromium's class for its own still
            // matches; where Windows can't match part of a name, the whole one.
            let omnibox = automation
                .CreatePropertyConditionEx(UIA_ClassNamePropertyId, &class, PropertyConditionFlags_MatchSubstring)
                .or_else(|_| automation.CreatePropertyCondition(UIA_ClassNamePropertyId, &class))
                .ok()?;
            let named = automation.CreatePropertyCondition(UIA_NamePropertyId, &VARIANT::from("Address and search bar")).ok()?;
            let firefox = automation.CreatePropertyCondition(UIA_AutomationIdPropertyId, &VARIANT::from("urlbar-input")).ok()?;
            let chromium = automation.CreateOrCondition(&omnibox, &named).ok()?;
            let which = automation.CreateOrCondition(&chromium, &firefox).ok()?;
            let wanted = automation.CreateAndCondition(&edit, &which).ok()?;
            root.FindFirst(TreeScope_Descendants, &wanted).ok()
        }
    }

    fn selection_ranges(pattern: &IUIAutomationTextPattern) -> Vec<IUIAutomationTextRange> {
        unsafe {
            let Ok(array) = pattern.GetSelection() else {
                return Vec::new();
            };
            let len = array.Length().unwrap_or(0).clamp(0, 32);
            (0..len).filter_map(|i| array.GetElement(i).ok()).collect()
        }
    }

    fn selection_text(ranges: &[IUIAutomationTextRange]) -> Option<String> {
        let mut out = String::new();
        for range in ranges {
            if let Ok(text) = unsafe { range.GetText(SELECTION_CHARS as i32) } {
                let text = normalise_text(&text.to_string());
                if !text.is_empty() {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(&text);
                }
            }
            if out.len() >= SELECTION_CHARS * 4 {
                break;
            }
        }
        non_empty(head_chars(&out, SELECTION_CHARS))
    }

    /// Up to `max` characters before the caret (or before the start of the selection).
    fn text_before(caret: &IUIAutomationTextRange, max: usize) -> Option<String> {
        unsafe {
            let range = caret.Clone().ok()?;
            // Collapse onto the caret's start, then stretch the start back.
            range.MoveEndpointByRange(TextPatternRangeEndpoint_End, caret, TextPatternRangeEndpoint_Start).ok()?;
            range.MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Character, -(max as i32)).ok()?;
            let text = range.GetText((max * 2) as i32).ok()?.to_string();
            non_empty(tail_chars(&normalise_text(&text), max))
        }
    }

    fn text_around(caret: &IUIAutomationTextRange, before: usize, after: usize) -> Option<String> {
        unsafe {
            let range = caret.Clone().ok()?;
            range.MoveEndpointByRange(TextPatternRangeEndpoint_End, caret, TextPatternRangeEndpoint_Start).ok()?;
            range.MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Character, -(before as i32)).ok()?;
            range.MoveEndpointByUnit(TextPatternRangeEndpoint_End, TextUnit_Character, after as i32).ok()?;
            let text = range.GetText(((before + after) * 2) as i32).ok()?.to_string();
            non_empty(head_chars(&normalise_text(&text), before + after))
        }
    }

    // ----- OCR -----

    pub(super) fn ocr_window(hwnd: isize) -> Option<String> {
        if hwnd == 0 {
            return None;
        }
        let (tx, rx) = mpsc::channel();
        // WinRT needs COM on the calling thread; a thread of its own also lets a slow OCR be abandoned.
        std::thread::Builder::new()
            .name("navi-ocr".into())
            .spawn(move || {
                let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
                let text = recognise(HWND(hwnd as *mut c_void));
                if com.is_ok() {
                    unsafe { CoUninitialize() };
                }
                let _ = tx.send(text);
            })
            .ok()?;
        rx.recv_timeout(OCR_TIMEOUT).ok().flatten()
    }

    fn recognise(hwnd: HWND) -> Option<String> {
        unsafe {
            if GetForegroundWindow() != hwnd || IsIconic(hwnd).as_bool() {
                return None;
            }
        }
        // Fails when Windows has no OCR language for the user's languages.
        let engine = OcrEngine::TryCreateFromUserProfileLanguages().ok()?;
        let max_side = OcrEngine::MaxImageDimension().unwrap_or(2_600).clamp(64, 16_384) as i32;
        let shot = capture_window(hwnd, max_side)?;
        recognise_pixels(&engine, shot.width, shot.height, &shot.pixels)
    }

    /// OCR of a screenshot on a thread of its own (WinRT needs COM there), waiting up to 5 s.
    pub(super) fn ocr_bgra(width: u32, height: u32, pixels: Vec<u8>) -> Option<String> {
        if width < 16 || height < 16 || pixels.len() < (width as usize) * (height as usize) * 4 {
            return None;
        }
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("navi-ocr".into())
            .spawn(move || {
                let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
                let text = OcrEngine::TryCreateFromUserProfileLanguages().ok().and_then(|engine| {
                    let max_side = OcrEngine::MaxImageDimension().unwrap_or(2_600).clamp(64, 16_384) as u32;
                    if width.max(height) > max_side {
                        return None;
                    }
                    recognise_pixels(&engine, width as i32, height as i32, &pixels)
                });
                if com.is_ok() {
                    unsafe { CoUninitialize() };
                }
                let _ = tx.send(text);
            })
            .ok()?;
        rx.recv_timeout(OCR_TIMEOUT).ok().flatten()
    }

    fn recognise_pixels(engine: &OcrEngine, width: i32, height: i32, pixels: &[u8]) -> Option<String> {
        let writer = DataWriter::new().ok()?;
        writer.WriteBytes(pixels).ok()?;
        let buffer = writer.DetachBuffer().ok()?;
        let bitmap = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
            &buffer,
            BitmapPixelFormat::Bgra8,
            width,
            height,
            BitmapAlphaMode::Premultiplied,
        )
        .ok()?;
        let result = engine.RecognizeAsync(&bitmap).ok()?.get().ok()?;
        let lines = result.Lines().ok()?;
        let mut out = String::new();
        for i in 0..lines.Size().ok()? {
            if let Ok(text) = lines.GetAt(i).and_then(|line| line.Text()) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&text.to_string());
            }
        }
        non_empty(head_chars(&out, OCR_MAX_CHARS))
    }

    struct Shot {
        width: i32,
        height: i32,
        /// Top-down BGRA, opaque.
        pixels: Vec<u8>,
    }

    /// Copies the window's area of the screen (it is the foreground window, so that is the window),
    /// scaled down to fit OCR's size limit.
    fn capture_window(hwnd: HWND, max_side: i32) -> Option<Shot> {
        let mut rect = RECT::default();
        let have_bounds = unsafe {
            DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut rect as *mut RECT as *mut c_void, std::mem::size_of::<RECT>() as u32)
        }
        .is_ok()
            || unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut rect) }.is_ok();
        let (src_w, src_h) = (rect.right - rect.left, rect.bottom - rect.top);
        if !have_bounds || src_w < 16 || src_h < 16 {
            return None;
        }
        let (width, height) = fit_within(src_w, src_h, max_side);

        unsafe {
            let screen = GetDC(None);
            if screen.is_invalid() {
                return None;
            }
            let memory = CreateCompatibleDC(Some(screen));
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    // Negative height: rows run top to bottom, as SoftwareBitmap expects.
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let mut pixels = None;
            if let Ok(dib) = CreateDIBSection(Some(memory), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
                if !bits.is_null() {
                    let previous = SelectObject(memory, HGDIOBJ(dib.0));
                    SetStretchBltMode(memory, HALFTONE);
                    if StretchBlt(memory, 0, 0, width, height, Some(screen), rect.left, rect.top, src_w, src_h, SRCCOPY).as_bool() {
                        let _ = GdiFlush();
                        let mut px = std::slice::from_raw_parts(bits as *const u8, (width as usize) * (height as usize) * 4).to_vec();
                        // GDI leaves the alpha byte at 0; OCR wants opaque pixels.
                        px.iter_mut().skip(3).step_by(4).for_each(|a| *a = 255);
                        pixels = Some(px);
                    }
                    SelectObject(memory, previous);
                }
                let _ = DeleteObject(HGDIOBJ(dib.0));
            }
            let _ = DeleteDC(memory);
            ReleaseDC(None, screen);
            pixels.map(|pixels| Shot { width, height, pixels })
        }
    }

    // ----- the user's name -----

    /// Works for Microsoft Entra / domain accounts; usually unavailable for local and Microsoft accounts.
    pub(super) fn display_name() -> Option<String> {
        unsafe {
            let mut size = 0u32;
            let _ = GetUserNameExW(NameDisplay, None, &mut size);
            if size == 0 {
                return None;
            }
            let mut buf = vec![0u16; size as usize + 1];
            let mut len = buf.len() as u32;
            if !GetUserNameExW(NameDisplay, Some(PWSTR(buf.as_mut_ptr())), &mut len) {
                return None;
            }
            let name = String::from_utf16_lossy(&buf[..(len as usize).min(buf.len())]);
            let name = name.trim_end_matches('\0').trim().to_string();
            (!name.is_empty()).then_some(name)
        }
    }

    /// The "Full name" of the local account (a Microsoft account's name is copied here).
    pub(super) fn local_full_name() -> Option<String> {
        let user = std::env::var("USERNAME").ok()?;
        let user = HSTRING::from(user.as_str());
        unsafe {
            let mut buf: *mut u8 = std::ptr::null_mut();
            let status = NetUserGetInfo(PCWSTR::null(), &user, 10, &mut buf);
            if buf.is_null() {
                return None;
            }
            let name = if status == 0 {
                let info = &*(buf as *const USER_INFO_10);
                let full = info.usri10_full_name;
                if full.is_null() {
                    None
                } else {
                    full.to_string().ok()
                }
            } else {
                None
            };
            NetApiBufferFree(Some(buf as *const c_void));
            name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, IsWindowVisible};

        /// Reads the address bar of every browser window open on this computer (they needn't be in
        /// front), timing the first read and one from the kept bar. Prints the host only for an AI
        /// chat site. Run by hand with a browser open:
        /// cargo test --lib context::win::tests::live_address_bar -- --ignored --nocapture
        #[test]
        #[ignore]
        fn live_address_bar() {
            unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
                // SAFETY: lparam is the &mut Vec passed below, alive for the whole enumeration.
                let found = unsafe { &mut *(lparam.0 as *mut Vec<(HWND, String)>) };
                if unsafe { IsWindowVisible(hwnd) }.as_bool() && !window_title(hwnd).is_empty() {
                    let info = window_info(hwnd);
                    if is_browser(&info.process_name) {
                        found.push((hwnd, info.process_name));
                    }
                }
                BOOL(1)
            }
            let mut found: Vec<(HWND, String)> = Vec::new();
            unsafe {
                let _ = EnumWindows(Some(visit), LPARAM(&mut found as *mut Vec<(HWND, String)> as isize));
            }
            println!("{} browser window(s)", found.len());
            // The UI Automation client is made as Navi starts, not on the first take.
            warm_up();
            std::thread::sleep(Duration::from_millis(500));
            for (hwnd, process) in found {
                let mut times = Vec::new();
                let mut host = None;
                for _ in 0..2 {
                    let t = Instant::now();
                    host = match ADDRESS_WORKER.call(Job::Address(hwnd.0 as isize), Instant::now() + Duration::from_secs(10)) {
                        Some(Reply::Address(host)) => host,
                        _ => None,
                    };
                    times.push(t.elapsed().as_millis());
                }
                let shown = match host.as_deref() {
                    Some(h) if crate::text::modes::site_mode(h).is_some() => h.to_string(),
                    Some(_) => "another site".into(),
                    None => "no host".into(),
                };
                println!("{process}: {shown}, first read {} ms, then {} ms", times[0], times[1]);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// macOS: NSWorkspace for the app in front, the Accessibility API for titles and fields
// ---------------------------------------------------------------------------------------------

/// The window identity on a Mac: there is no global window handle, so `hwnd` packs the app's pid
/// (high 32 bits) and the CGWindowID of its frontmost normal window (low 32 bits). Two different
/// windows of one app, or the same window id in another app, never compare equal.
#[cfg(target_os = "macos")]
pub(crate) mod mac {
    use super::*;
    use crate::mac::ax::{self, Element};
    use crate::mac::front;
    use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRange, CFRetained, CFString, CFType, CFURL};
    use objc2_core_graphics::{kCGNullWindowID, CGWindowListCopyWindowInfo, CGWindowListOption};
    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::sync::mpsc;
    use std::time::Instant;

    pub(crate) fn pack(pid: i32, window: u32) -> isize {
        (((pid as u32 as u64) << 32) | window as u64) as isize
    }

    pub(crate) fn unpack(hwnd: isize) -> (i32, u32) {
        let v = hwnd as u64;
        ((v >> 32) as u32 as i32, v as u32)
    }

    /// The CGWindowID of `pid`'s frontmost normal (layer 0) window on screen. Window numbers and
    /// owners are readable without the Screen Recording permission (titles are not).
    pub(crate) fn front_window_of(pid: i32) -> Option<u32> {
        let opts = CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements;
        let list = CGWindowListCopyWindowInfo(opts, kCGNullWindowID)?;
        let list: CFRetained<CFArray<CFDictionary<CFString, CFType>>> = unsafe { CFRetained::cast_unchecked(list) };
        let owner_key = CFString::from_static_str("kCGWindowOwnerPID");
        let layer_key = CFString::from_static_str("kCGWindowLayer");
        let number_key = CFString::from_static_str("kCGWindowNumber");
        let num = |d: &CFDictionary<CFString, CFType>, k: &CFString| -> Option<i64> {
            d.get(k).and_then(|v| v.downcast_ref::<CFNumber>().and_then(|n| n.as_i64()))
        };
        for dict in list.iter() {
            if num(&dict, &owner_key) == Some(pid as i64) && num(&dict, &layer_key) == Some(0) {
                if let Some(id) = num(&dict, &number_key) {
                    return Some(id as u32);
                }
            }
        }
        None
    }

    pub(super) fn foreground_hwnd() -> isize {
        let Some(pid) = front::front_pid() else { return 0 };
        // The focused window as the app sees it (needs Accessibility), else the frontmost one.
        let window = ax::trusted()
            .then(|| Element::application(pid).element("AXFocusedWindow").and_then(|w| w.window_id()))
            .flatten()
            .or_else(|| front_window_of(pid))
            .unwrap_or(0);
        pack(pid, window)
    }

    pub(crate) fn foreground_pid() -> Option<i32> {
        front::front_pid()
    }

    fn app_label(app: &front::FrontApp) -> String {
        if let Some(label) = front::mac_label(&app.process_name) {
            return label.to_string();
        }
        if !app.name.trim().is_empty() {
            return app.name.trim().to_string();
        }
        known_app_label(&app.process_name).map(str::to_string).unwrap_or_else(|| label_from_stem(&app.process_name))
    }

    fn window_title(pid: i32) -> String {
        if !ax::trusted() {
            return String::new();
        }
        Element::application(pid)
            .element("AXFocusedWindow")
            .and_then(|w| w.string("AXTitle"))
            .unwrap_or_default()
    }

    pub(super) fn foreground_category() -> &'static str {
        let Some(app) = front::front_app() else { return "other" };
        categorise(&app.process_name, &window_title(app.pid))
    }

    pub(super) fn foreground_process_name() -> String {
        front::front_app().map(|a| a.process_name).unwrap_or_default()
    }

    pub(super) fn foreground_app() -> (String, String, String) {
        let Some(app) = front::front_app() else { return (String::new(), String::new(), String::new()) };
        (app_label(&app), window_title(app.pid), app.process_name)
    }

    /// Runs `job` on a thread of its own and waits until `deadline`. A slow app (each AX call
    /// gives up after 0.25 s) only costs that thread, never the take.
    fn within<T: Send + 'static>(deadline: Instant, job: impl FnOnce() -> T + Send + 'static) -> Option<T> {
        let (tx, rx) = mpsc::channel();
        let _ = std::thread::Builder::new().name("navi-ax".into()).spawn(move || {
            let _ = tx.send(job());
        });
        rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).ok()
    }

    /// The text in `el` before index `end` (UTF-16 units), at most `max` characters.
    fn text_before(el: &Element, end: i64, max: usize) -> Option<String> {
        if end <= 0 {
            return None;
        }
        // Ask for a little more than needed: a range counts UTF-16 units, `max` counts chars.
        let start = (end - (max as i64) * 2).max(0);
        let text = el
            .string_for_range(CFRange { location: start as isize, length: (end - start) as isize })
            .or_else(|| {
                let value = el.string("AXValue")?;
                let units: Vec<u16> = value.encode_utf16().collect();
                let end = (end as usize).min(units.len());
                Some(String::from_utf16_lossy(&units[..end]))
            })?;
        non_empty(tail_chars(&normalise_text(&text), max))
    }

    /// The focused field of app `pid`: the text before the caret and the selection.
    fn read_field(pid: i32) -> (Option<String>, Option<String>) {
        let Some(el) = ax::focused_element() else { return (None, None) };
        if el.pid() != Some(pid) || el.is_secure() {
            return (None, None);
        }
        let selection = el
            .string("AXSelectedText")
            .map(|s| normalise_text(&s))
            .filter(|s| !s.trim().is_empty())
            .map(|s| head_chars(&s, SELECTION_CHARS).to_string());
        let before = match el.range("AXSelectedTextRange") {
            Some(range) => text_before(&el, range.location as i64, FIELD_CHARS),
            // No caret to go by: assume the user is typing at the end, as Windows does.
            None => el.string("AXValue").and_then(|v| non_empty(tail_chars(&normalise_text(&v), FIELD_CHARS))),
        };
        (before, selection)
    }

    fn read_full(pid: i32) -> Option<String> {
        let el = ax::focused_element()?;
        if el.pid() != Some(pid) || el.is_secure() {
            return None;
        }
        el.set_timeout(1.0);
        let count = el.number("AXNumberOfCharacters").unwrap_or(-1);
        if count >= 0 && count as usize > FULL_TEXT_CHARS {
            // Too long: keep the stretch around the caret, where the user has just been editing.
            if let Some(range) = el.range("AXSelectedTextRange") {
                let before = (FULL_TEXT_CHARS * 3 / 4) as i64;
                let start = (range.location as i64 - before).max(0);
                let len = (FULL_TEXT_CHARS as i64).min(count - start);
                if let Some(text) = el.string_for_range(CFRange { location: start as isize, length: len as isize }) {
                    return non_empty(&normalise_text(&text));
                }
            }
        }
        let text = normalise_text(&el.string("AXValue")?);
        if text.chars().count() <= FULL_TEXT_CHARS {
            non_empty(&text)
        } else {
            non_empty(tail_chars(&text, FULL_TEXT_CHARS))
        }
    }

    // ----- a browser's address -----

    /// Elements already found that hold a window's address (a web area with AXURL, or the address
    /// field), so later takes in that window make a single call.
    static ADDRESS_ELEMENTS: Mutex<Option<HashMap<isize, Element>>> = parking_lot::const_mutex(None);
    /// Windows searched with nothing found, and when, so they aren't searched on every take.
    static NO_ADDRESS: Mutex<Option<HashMap<isize, Instant>>> = parking_lot::const_mutex(None);
    const NO_ADDRESS_RETRY: Duration = Duration::from_secs(60);

    fn address_of(el: &Element) -> Option<String> {
        if let Some(value) = el.attr("AXURL") {
            if let Some(url) = value.downcast_ref::<CFURL>() {
                return Some(url.string().to_string());
            }
            if let Some(s) = value.downcast_ref::<CFString>() {
                return Some(s.to_string());
            }
        }
        if el.role() == "AXTextField" || el.role() == "AXComboBox" {
            return el.string("AXValue");
        }
        None
    }

    fn looks_like_address_field(el: &Element) -> bool {
        let role = el.role();
        if role != "AXTextField" && role != "AXComboBox" {
            return false;
        }
        let id = el.string("AXIdentifier").unwrap_or_default().to_lowercase();
        let desc = el.string("AXDescription").unwrap_or_default().to_lowercase();
        id.contains("address") || id.contains("url") || desc.contains("address") || desc.contains("url") || desc.contains("search or enter")
    }

    /// Breadth-first through the window for the address field or the page's web area, giving up
    /// after a few hundred elements.
    fn find_address_element(window: &Element) -> Option<Element> {
        let mut queue = std::collections::VecDeque::from([window.clone()]);
        let mut web_area: Option<Element> = None;
        let mut seen = 0;
        while let Some(el) = queue.pop_front() {
            seen += 1;
            if seen > 600 {
                break;
            }
            if looks_like_address_field(&el) && el.string("AXValue").is_some() {
                return Some(el);
            }
            if web_area.is_none() && el.role() == "AXWebArea" && el.attr("AXURL").is_some() {
                web_area = Some(el.clone());
                continue; // nothing of the browser's own is inside the page
            }
            queue.extend(el.children());
        }
        web_area
    }

    fn read_host(pid: i32, hwnd: isize) -> Option<String> {
        let kept = ADDRESS_ELEMENTS.lock().as_ref().and_then(|m| m.get(&hwnd).cloned());
        if let Some(el) = kept {
            match address_of(&el) {
                Some(text) => return host_from_address(&text),
                None => {
                    if let Some(m) = ADDRESS_ELEMENTS.lock().as_mut() {
                        m.remove(&hwnd);
                    }
                }
            }
        }
        if NO_ADDRESS.lock().as_ref().and_then(|m| m.get(&hwnd).copied()).is_some_and(|at| at.elapsed() < NO_ADDRESS_RETRY) {
            return None;
        }
        ax::wake_web_accessibility(pid);
        let window = Element::application(pid).element("AXFocusedWindow")?;
        match find_address_element(&window) {
            Some(el) => {
                let host = address_of(&el).and_then(|a| host_from_address(&a));
                let mut map = ADDRESS_ELEMENTS.lock();
                let map = map.get_or_insert_with(HashMap::new);
                if map.len() >= 32 {
                    map.clear();
                }
                map.insert(hwnd, el);
                host
            }
            None => {
                NO_ADDRESS.lock().get_or_insert_with(HashMap::new).insert(hwnd, Instant::now());
                None
            }
        }
    }

    pub(super) fn capture(settings: &ContextSettings) -> AppContext {
        let started = Instant::now();
        let Some(app) = front::front_app() else {
            return AppContext { category: "other".into(), ..Default::default() };
        };
        let own = app.pid == std::process::id() as i32;
        let excluded = is_excluded(&app.process_name, &settings.excluded_apps);
        let hwnd = foreground_hwnd();
        // Never read means never read: an excluded app's title isn't even used for its category.
        let title = if excluded { String::new() } else { window_title(app.pid) };
        let mut ctx = AppContext {
            hwnd,
            category: categorise(&app.process_name, &title).to_string(),
            app_label: app_label(&app),
            window_title: kept_title(title, settings.read_app, excluded),
            process_name: app.process_name.clone(),
            exe_path: app.path.clone(),
            field_text: None,
            selection: None,
            host: String::new(),
        };
        if own || !ax::trusted() {
            return ctx;
        }
        let pid = app.pid;
        let readable = !app.process_name.is_empty() && !excluded;
        // A browser's address is asked for first, so it is read while the field is.
        let address = if settings.read_app && readable && is_browser(&app.process_name) {
            let (tx, rx) = mpsc::channel();
            let _ = std::thread::Builder::new().name("navi-ax-address".into()).spawn(move || {
                let _ = tx.send(read_host(pid, hwnd));
            });
            Some(rx)
        } else {
            None
        };
        if may_read_field(&app.process_name, settings) {
            if let Some((before, selection)) = within(started + CAPTURE_UIA_BUDGET, move || read_field(pid)) {
                if front::front_pid() == Some(pid) {
                    ctx.field_text = before;
                    ctx.selection = selection;
                }
            }
        }
        if let Some(rx) = address {
            let wait = (started + CAPTURE_ADDRESS_BUDGET).saturating_duration_since(Instant::now());
            if let Ok(Some(host)) = rx.recv_timeout(wait) {
                ctx.host = host;
            }
        }
        ctx
    }

    pub(super) fn read_focused_text(hwnd: isize, settings: &ContextSettings) -> Option<String> {
        if hwnd == 0 || foreground_hwnd() != hwnd || !ax::trusted() {
            return None;
        }
        let (pid, _) = unpack(hwnd);
        let app = front::front_app()?;
        if app.pid != pid || !may_read_field(&app.process_name, settings) {
            return None;
        }
        within(Instant::now() + READ_UIA_BUDGET, move || read_full(pid)).flatten()
    }

    pub(super) fn warm_up() {
        let _ = front::front_app();
    }

    pub(super) fn display_name() -> Option<String> {
        let name = objc2_foundation::NSFullUserName().to_string();
        (!name.trim().is_empty()).then_some(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems() {
        assert_eq!(process_stem("Code.exe"), "code");
        assert_eq!(process_stem("C:\\Program Files\\Slack\\slack.exe"), "slack");
        assert_eq!(process_stem("WhatsApp.Root.exe"), "whatsapp.root");
        assert_eq!(process_stem("Code - Insiders.exe"), "code - insiders");
        assert_eq!(process_stem("noext"), "noext");
        assert_eq!(process_stem(""), "");
    }

    #[test]
    fn categories_follow_the_table() {
        let cases: &[(&str, &str, &str)] = &[
            ("WindowsTerminal.exe", "", "terminal"),
            ("wt.exe", "", "terminal"),
            ("cmd.exe", "", "terminal"),
            ("powershell.exe", "", "terminal"),
            ("pwsh.exe", "", "terminal"),
            ("conhost.exe", "", "terminal"),
            ("alacritty.exe", "", "terminal"),
            ("wezterm-gui.exe", "", "terminal"),
            ("warp.exe", "", "terminal"),
            ("mintty.exe", "", "terminal"),
            ("Code.exe", "", "editor"),
            ("Cursor.exe", "", "editor"),
            ("Windsurf.exe", "", "editor"),
            ("zed.exe", "", "editor"),
            ("idea64.exe", "", "editor"),
            ("pycharm64.exe", "", "editor"),
            ("devenv.exe", "", "editor"),
            ("sublime_text.exe", "", "editor"),
            ("notepad++.exe", "", "editor"),
            ("bb.exe", "", "editor"),
            ("claude.exe", "", "editor"),
            ("slack.exe", "", "chat"),
            ("Teams.exe", "", "chat"),
            ("ms-teams.exe", "", "chat"),
            ("Discord.exe", "", "chat"),
            ("WhatsApp.exe", "", "chat"),
            ("WhatsApp.Root.exe", "", "chat"),
            ("Telegram.exe", "", "chat"),
            ("Signal.exe", "", "chat"),
            ("Messenger.exe", "", "chat"),
            ("OUTLOOK.EXE", "", "email"),
            ("olk.exe", "", "email"),
            ("thunderbird.exe", "", "email"),
            ("Mailbird.exe", "", "email"),
            ("chrome.exe", "Inbox (3) - jane@example.com - Gmail - Google Chrome", "email"),
            ("msedge.exe", "Mail - Jane Doe - Outlook - Microsoft Edge", "email"),
            ("firefox.exe", "GitHub - Mozilla Firefox", "browser"),
            ("chrome.exe", "Outlooking ahead - Google Chrome", "browser"),
            ("brave.exe", "", "browser"),
            ("arc.exe", "", "browser"),
            ("opera.exe", "", "browser"),
            ("WINWORD.EXE", "", "document"),
            ("Notion.exe", "", "document"),
            ("Obsidian.exe", "", "document"),
            ("ONENOTE.EXE", "", "document"),
            ("wordpad.exe", "", "document"),
            ("notepad.exe", "", "document"),
            ("EXCEL.EXE", "", "document"),
            ("POWERPNT.EXE", "", "document"),
            ("granola.exe", "", "other"),
            ("", "", "other"),
        ];
        for (process, title, expected) in cases {
            assert_eq!(categorise(process, title), *expected, "{process} / {title}");
        }
    }

    #[test]
    fn title_matching_is_whole_word() {
        assert!(title_mentions("Inbox - Gmail - Google Chrome", "Gmail"));
        assert!(title_mentions("gmail", "Gmail"));
        assert!(title_mentions("(Outlook)", "outlook"));
        assert!(!title_mentions("Outlooking", "Outlook"));
        assert!(!title_mentions("MyGmail", "Gmail"));
        assert!(title_mentions("MyGmail and Gmail", "Gmail"), "a later whole-word match still counts");
        assert!(!title_mentions("", "Gmail"));
        assert!(!title_mentions("Gmail", ""));
    }

    #[test]
    fn browsers_are_known_by_process() {
        for p in ["chrome.exe", "MSEDGE.EXE", "firefox.exe", "brave.exe", "C:\\Program Files\\Vivaldi\\vivaldi.exe"] {
            assert!(is_browser(p), "{p}");
        }
        for p in ["chatgpt.exe", "code.exe", "explorer.exe", ""] {
            assert!(!is_browser(p), "{p}");
        }
    }

    #[test]
    fn only_the_host_is_taken_from_an_address() {
        // As Chrome, Edge and Firefox show it (no "https://"), and in full.
        assert_eq!(host_from_address("chatgpt.com/c/68f1a2b3-1234"), Some("chatgpt.com".into()));
        assert_eq!(host_from_address("https://chatgpt.com/c/68f1a2b3-1234?model=x#top"), Some("chatgpt.com".into()));
        assert_eq!(host_from_address(" https://www.Claude.ai/chat/abc "), Some("claude.ai".into()));
        assert_eq!(host_from_address("chat.openai.com"), Some("chat.openai.com".into()));
        assert_eq!(host_from_address("http://user:secret@example.co.uk:8080/path"), Some("example.co.uk".into()));
        // Not web addresses: a search being typed, an empty bar, the browser's own pages.
        for text in ["", "   ", "what is the weather", "weather", "about:blank", "chrome://settings", "edge://newtab", "file:///C:/notes.txt", "view-source:https://chatgpt.com"] {
            assert_eq!(host_from_address(text), None, "{text:?}");
        }
    }

    #[test]
    fn app_labels() {
        assert_eq!(known_app_label("Code.exe"), Some("Visual Studio Code"));
        assert_eq!(known_app_label("WindowsTerminal.exe"), Some("Windows Terminal"));
        assert_eq!(known_app_label("OUTLOOK.EXE"), Some("Outlook"));
        assert_eq!(known_app_label("olk.exe"), Some("Outlook"));
        assert_eq!(known_app_label("chrome.exe"), Some("Chrome"));
        assert_eq!(known_app_label("msedge.exe"), Some("Edge"));
        assert_eq!(known_app_label("ms-teams.exe"), Some("Teams"));
        assert_eq!(known_app_label("unknownapp.exe"), None);
        assert_eq!(label_from_stem("granola.exe"), "Granola");
        assert_eq!(label_from_stem(""), "");
        assert_eq!(usable_description("Granola"), Some("Granola".into()));
        assert_eq!(usable_description("Electron"), None);
        assert_eq!(usable_description(" "), None);
        assert_eq!(usable_description(&"x".repeat(41)), None);
    }

    #[test]
    fn exclusions_ignore_case_and_extension() {
        let excluded = vec!["1Password.exe".to_string(), "KeePass".to_string(), "Bitwarden.exe".to_string()];
        assert!(is_excluded("1password.exe", &excluded));
        assert!(is_excluded("keepass.exe", &excluded));
        assert!(is_excluded("BITWARDEN.EXE", &excluded));
        assert!(!is_excluded("code.exe", &excluded));
        assert!(!is_excluded("", &excluded));
        assert!(!is_excluded("code.exe", &[]));
    }

    #[test]
    fn excluded_apps_fields_are_never_read() {
        let settings = ContextSettings::default();
        assert!(may_read_field("code.exe", &settings));
        // On the list (any case), or nothing known about the window: not read.
        assert!(!may_read_field("1password.exe", &settings));
        assert!(!may_read_field("KEEPASS.EXE", &settings));
        assert!(!may_read_field("", &settings));
        // "Read the text near my cursor" off: no app is read.
        let off = ContextSettings { read_field: false, ..ContextSettings::default() };
        assert!(!may_read_field("code.exe", &off));
    }

    #[test]
    fn an_excluded_apps_title_is_never_kept() {
        let title = || "Bank login - 1Password".to_string();
        assert_eq!(kept_title(title(), true, false), "Bank login - 1Password");
        assert_eq!(kept_title(title(), true, true), "", "an app under Never read these apps");
        assert_eq!(kept_title(title(), false, false), "", "Read the window title off");
        assert_eq!(kept_title(title(), false, true), "");
    }

    #[test]
    fn text_normalising() {
        assert_eq!(normalise_text("a\r\nb\rc\u{FFFC}d\0e\n"), "a\nb\ncde\n");
        assert_eq!(normalise_text(""), "");
    }

    #[test]
    fn char_slicing_respects_multibyte_text() {
        assert_eq!(tail_chars("héllo wörld", 5), "wörld");
        assert_eq!(tail_chars("ab", 5), "ab");
        assert_eq!(tail_chars("ab", 0), "");
        assert_eq!(head_chars("héllo", 2), "hé");
        assert_eq!(head_chars("ab", 5), "ab");
        assert_eq!(tail_chars("😀😀😀", 2), "😀😀");
    }

    #[test]
    fn first_names_from_display_names() {
        assert_eq!(first_name_from_display("Jane Doe"), Some("Jane".into()));
        assert_eq!(first_name_from_display("Doe, Jane"), Some("Jane".into()));
        assert_eq!(first_name_from_display("  Jane  "), Some("Jane".into()));
        assert_eq!(first_name_from_display("Dr. Sam Smith"), Some("Sam".into()));
        assert_eq!(first_name_from_display("Jane (Work)"), Some("Jane".into()));
        assert_eq!(first_name_from_display("Mary-Jane O'Neil"), Some("Mary-Jane".into()));
        assert_eq!(first_name_from_display("jane@example.com"), None);
        assert_eq!(first_name_from_display(""), None);
    }

    #[test]
    fn first_names_from_usernames() {
        assert_eq!(first_name_from_username("jdoe"), Some("Jdoe".into()));
        assert_eq!(first_name_from_username("jane.doe"), Some("Jane".into()));
        assert_eq!(first_name_from_username("DOMAIN\\sam_jones"), Some("Sam".into()));
        assert_eq!(first_name_from_username("JO"), Some("Jo".into()));
        assert_eq!(first_name_from_username("McKay"), Some("McKay".into()));
        assert_eq!(first_name_from_username("user42"), Some("User".into()));
        assert_eq!(first_name_from_username("1234"), None);
        assert_eq!(first_name_from_username(""), None);
    }

    #[test]
    fn ocr_images_fit_the_size_limit() {
        assert_eq!(fit_within(800, 600, 2600), (800, 600));
        assert_eq!(fit_within(5200, 1000, 2600), (2600, 500));
        assert_eq!(fit_within(1000, 5200, 2600), (500, 2600));
        assert_eq!(fit_within(3840, 2160, 2600), (2600, 1463));
        assert_eq!(fit_within(0, 0, 2600), (0, 0));
    }

    #[test]
    #[ignore = "reads this machine's account name; run with --ignored"]
    fn user_first_name_is_never_empty_when_username_is_set() {
        // Read-only: only checks that something sensible comes back.
        if std::env::var("USERNAME").is_ok_and(|u| u.chars().any(|c| c.is_alphabetic())) {
            let name = user_first_name();
            assert!(!name.is_empty());
            assert!(!name.contains(' '));
        }
    }
}
