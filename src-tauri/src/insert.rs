//! Putting text into whatever app has focus.
//!
//! OWNER: Windows systems agent. STATUS: implemented.
//!
//! Contract:
//! * `insert_text` with method "paste": snapshot the clipboard (every format whose data is an
//!   HGLOBAL; GDI handle formats like CF_BITMAP can be skipped because CF_DIB carries the same
//!   image), put the text on the clipboard as CF_UNICODETEXT together with the registered format
//!   "ExcludeClipboardContentFromMonitorProcessing" (so Win+V history and clipboard managers skip
//!   it), send Ctrl+V with SendInput, wait ~250 ms, then restore the snapshot if `restore`.
//!   Before sending, wait (up to ~600 ms) until the user's Ctrl/Shift/Alt/Win keys are physically
//!   released (GetAsyncKeyState), so Ctrl+V is not turned into another shortcut.
//! * method "type": SendInput with KEYEVENTF_UNICODE per UTF-16 unit, newlines as Enter (Shift+Enter
//!   in chat apps and web chats, where Enter sends the message).
//! * If the foreground window belongs to an elevated process and Navi is not elevated, SendInput is
//!   blocked by Windows (UIPI): leave the text on the clipboard and return ClipboardOnly.
//! * `select_back_and_replace_with(n, text, method)`: Shift+Left n times (n = UTF-16 length of the
//!   last insert), then insert `text` with `method`. Used by voice edits right after an insertion.
//!   `select_back_and_replace(n, text)` is the same with "paste".
//! * `copy_selection`: snapshot clipboard, send Ctrl+C, read text (wait up to ~300 ms for the
//!   clipboard sequence number to change), restore. None if nothing was selected.
//!
//! Details worth knowing:
//! * All of these block the calling thread (sleeps of up to ~1 s in total, ~3 s when pasting into a
//!   busy app or a remote desktop): call them from a blocking task, never from the UI thread or the
//!   hook.
//! * Before the clipboard is put back, a busy target gets time to handle the Ctrl+V first (Navi
//!   waits, up to a limit, until its window responds again). Remote desktops and virtual machines
//!   fetch the text over the network when the far side pastes, so they get two seconds.
//! * The pasted text also carries "CanIncludeInClipboardHistory" and "CanUploadToCloudClipboard"
//!   set to 0. The restored snapshot is marked the same way, so restoring does not add a duplicate
//!   of the user's old clipboard to Win+V history.
//! * The restore is skipped if anything else changed the clipboard during the paste (sequence
//!   number check), so a copy the user or the target app made is never overwritten.
//! * Line breaks go on the clipboard as CRLF (classic Windows edit controls ignore a bare LF).
//! * If modifiers are still held after the wait, Navi releases them for Windows (key-ups, with the
//!   mask key first for Win and Alt) so the paste cannot become Win+Ctrl+V or Ctrl+Shift+V.
//! * Terminals: mintty (Git Bash) and PuTTY get Shift+Insert instead of Ctrl+V, which they pass to
//!   the shell. `select_back_and_replace` uses Backspace there (Shift+Left does not select), and
//!   `undo` and `copy_selection` refuse, because Ctrl+Z pauses and Ctrl+C interrupts the running
//!   program.
//! * If the clipboard cannot be opened (another app holding it), "paste" falls back to typing.

use anyhow::{anyhow, Result};
use serde::Serialize;
use std::time::Duration;

/// Written into dwExtraInfo of every keystroke Navi injects ("NAVI" in ASCII).
pub const NAVI_INPUT_TAG: usize = 0x4E41_5649;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum InsertOutcome {
    Pasted,
    Typed,
    /// Text left on the clipboard; the string says why (e.g. "That window is running as administrator").
    ClipboardOnly(String),
}

impl InsertOutcome {
    pub fn status(&self) -> &'static str {
        match self {
            InsertOutcome::Pasted => "pasted",
            InsertOutcome::Typed => "typed",
            InsertOutcome::ClipboardOnly(_) => "clipboard",
        }
    }
}

/// How long to wait for the user to let go of Ctrl/Shift/Alt/Win before sending keys.
const MODIFIER_WAIT: Duration = Duration::from_millis(600);
/// How long the target app gets to read the clipboard after Ctrl+V before it is restored.
const PASTE_SETTLE: Duration = Duration::from_millis(250);
/// The longest wait for a busy target window to respond again before that settle time starts.
const BUSY_TARGET_WAIT: Duration = Duration::from_millis(1500);
/// Remote desktops and virtual machines read the clipboard late (see `reads_clipboard_late`).
const REMOTE_PASTE_SETTLE: Duration = Duration::from_millis(2000);
/// How long to wait for Ctrl+C to change the clipboard.
const COPY_WAIT: Duration = Duration::from_millis(300);
/// OpenClipboard fails while another app has it open; retry this many times, this far apart.
const OPEN_ATTEMPTS: u32 = 10;
const OPEN_RETRY_GAP: Duration = Duration::from_millis(15);
/// Snapshot limits, so a huge or slow clipboard (Excel offers dozens of rendered-on-demand
/// formats) never stalls a take. Formats past a limit are left out of the restore.
const SNAPSHOT_BUDGET: Duration = Duration::from_millis(400);
const SNAPSHOT_MAX_FORMAT_BYTES: usize = 64 << 20;
const SNAPSHOT_MAX_TOTAL_BYTES: usize = 256 << 20;
/// UTF-16 units per SendInput call when typing, and the pause between batches, so the target's
/// message queue (10,000 messages) never overflows.
const TYPE_BATCH_UNITS: usize = 100;
const TYPE_BATCH_GAP: Duration = Duration::from_millis(5);
/// Shift+Left presses per SendInput call when selecting back.
const SELECT_BATCH: usize = 200;

const EXCLUDE_FORMAT: &str = "ExcludeClipboardContentFromMonitorProcessing";
const HISTORY_FORMAT: &str = "CanIncludeInClipboardHistory";
const CLOUD_FORMAT: &str = "CanUploadToCloudClipboard";

// Each fits the two-line bubble (80 characters at most).
const ELEVATED_REASON: &str = "That window runs as administrator. Your text is on the clipboard: press Ctrl+V.";
const BLOCKED_REASON: &str = "Windows blocked the typing. Your text is on the clipboard: press Ctrl+V.";
const TYPING_STOPPED_REASON: &str = "Windows stopped the typing part way. The full text is on the clipboard.";
#[cfg(not(any(windows, target_os = "macos")))]
const NOT_SUPPORTED: &str = "Inserting text only works on Windows and macOS for now.";

/// method: "paste" | "type".
pub fn insert_text(text: &str, method: &str, restore: bool) -> Result<InsertOutcome> {
    let typed = method.trim().eq_ignore_ascii_case("type");
    if text.is_empty() {
        return Ok(if typed { InsertOutcome::Typed } else { InsertOutcome::Pasted });
    }
    #[cfg(windows)]
    {
        if let Some(outcome) = win::clipboard_only_if_blocked(text)? {
            return Ok(outcome);
        }
        put_in(text, typed, restore)
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(outcome) = mac::clipboard_only_if_blocked(text)? {
            return Ok(outcome);
        }
        put_in(text, typed, restore)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = restore;
        Err(anyhow!(NOT_SUPPORTED))
    }
}

/// Pastes or types `text` where the cursor is (whatever is selected is replaced).
#[cfg(target_os = "macos")]
fn put_in(text: &str, typed: bool, restore: bool) -> Result<InsertOutcome> {
    if typed {
        let outcome = mac::type_text(text)?;
        if !restore {
            let _ = set_clipboard_text(text);
        }
        Ok(outcome)
    } else {
        mac::paste(text, restore)
    }
}

/// Pastes or types `text` where the cursor is (whatever is selected is replaced).
#[cfg(windows)]
fn put_in(text: &str, typed: bool, restore: bool) -> Result<InsertOutcome> {
    if typed {
        let outcome = win::type_text(text)?;
        // "Copy what I say to the clipboard" (restore off) applies when typing too.
        if !restore {
            let _ = set_clipboard_text(text);
        }
        Ok(outcome)
    } else {
        win::paste(text, restore)
    }
}

/// `select_back_and_replace_with` using paste.
pub fn select_back_and_replace(chars_utf16: usize, text: &str, restore: bool) -> Result<InsertOutcome> {
    select_back_and_replace_with(chars_utf16, text, "paste", restore)
}

/// Replaces the last `chars_utf16` characters before the cursor with `text`, put in with `method`
/// ("paste" | "type", as `insert_text`), so an app that ignores pasting still gets the new text.
/// In terminals, where Shift+Left does not select, the old text is removed with Backspace instead.
/// With an empty `text` this simply deletes the last insertion, which is the safe way to "scratch
/// that" in every app, terminals included. ClipboardOnly means nothing in the app changed.
pub fn select_back_and_replace_with(chars_utf16: usize, text: &str, method: &str, restore: bool) -> Result<InsertOutcome> {
    let typed = method.trim().eq_ignore_ascii_case("type");
    #[cfg(windows)]
    {
        if text.is_empty() && win::blocked_by_uipi() {
            return Err(anyhow!("That window runs as administrator, so Navi can't change the text there."));
        }
        if let Some(outcome) = win::clipboard_only_if_blocked(text)? {
            return Ok(outcome);
        }
        if crate::context::foreground_category() == "terminal" {
            win::backspace(chars_utf16)?;
        } else {
            win::select_back(chars_utf16)?;
            if text.is_empty() && chars_utf16 > 0 {
                win::delete_selection()?;
            }
        }
        if text.is_empty() {
            return Ok(if typed { InsertOutcome::Typed } else { InsertOutcome::Pasted });
        }
        put_in(text, typed, restore)
    }
    #[cfg(target_os = "macos")]
    {
        if text.is_empty() && mac::blocked() {
            return Err(anyhow!("macOS isn't letting Navi type there, so it can't change the text."));
        }
        if let Some(outcome) = mac::clipboard_only_if_blocked(text)? {
            return Ok(outcome);
        }
        if crate::context::foreground_category() == "terminal" {
            mac::backspace(chars_utf16)?;
        } else {
            mac::select_back(chars_utf16)?;
            if text.is_empty() && chars_utf16 > 0 {
                mac::delete_selection()?;
            }
        }
        if text.is_empty() {
            return Ok(if typed { InsertOutcome::Typed } else { InsertOutcome::Pasted });
        }
        put_in(text, typed, restore)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (chars_utf16, text, typed, restore);
        Err(anyhow!(NOT_SUPPORTED))
    }
}

/// Ctrl+Z in the focused app. Refused in terminals, where Ctrl+Z pauses the running program (use
/// `select_back_and_replace(n, "", ..)` there).
pub fn undo() -> Result<()> {
    #[cfg(windows)]
    {
        if win::blocked_by_uipi() {
            return Err(anyhow!("That window runs as administrator, so Navi can't undo there."));
        }
        if crate::context::foreground_category() == "terminal" {
            return Err(anyhow!("In a terminal, Ctrl+Z would pause the running program, so Navi didn't send it."));
        }
        win::undo()
    }
    #[cfg(target_os = "macos")]
    {
        if mac::blocked() {
            return Err(anyhow!("macOS isn't letting Navi type there, so it can't undo."));
        }
        if crate::context::foreground_category() == "terminal" {
            return Err(anyhow!("A terminal has no undo, so Navi didn't send ⌘Z."));
        }
        mac::undo()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    Err(anyhow!(NOT_SUPPORTED))
}

pub fn copy_selection() -> Result<Option<String>> {
    #[cfg(windows)]
    {
        win::copy_selection()
    }
    #[cfg(target_os = "macos")]
    {
        mac::copy_selection()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    Ok(None)
}

/// Plain clipboard write (used by "Copy" buttons when the UI cannot do it itself).
pub fn set_clipboard_text(text: &str) -> Result<()> {
    #[cfg(windows)]
    {
        win::set_text(text, false)
    }
    #[cfg(target_os = "macos")]
    {
        mac::set_text(text, false)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = text;
        Err(anyhow!(NOT_SUPPORTED))
    }
}

// ---------------------------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------------------------

/// False for formats whose data is not a plain HGLOBAL (GDI handles, metafiles, private and
/// owner-display formats) or that are OLE's own bookkeeping; copying their bytes would not survive
/// EmptyClipboard. Also false for the privacy markers, which are added back explicitly.
pub(crate) fn should_snapshot_format(format: u32, name: Option<&str>) -> bool {
    const CF_BITMAP: u32 = 2;
    const CF_METAFILEPICT: u32 = 3;
    const CF_PALETTE: u32 = 9;
    const CF_ENHMETAFILE: u32 = 14;
    const CF_OWNERDISPLAY: u32 = 0x80;
    const CF_DSPBITMAP: u32 = 0x82;
    const CF_DSPMETAFILEPICT: u32 = 0x83;
    const CF_DSPENHMETAFILE: u32 = 0x8E;
    const SKIPPED_NAMES: [&str; 5] = ["DataObject", "Ole Private Data", EXCLUDE_FORMAT, HISTORY_FORMAT, CLOUD_FORMAT];
    match format {
        0 | CF_BITMAP | CF_METAFILEPICT | CF_PALETTE | CF_ENHMETAFILE | CF_OWNERDISPLAY | CF_DSPBITMAP | CF_DSPMETAFILEPICT
        | CF_DSPENHMETAFILE => false,
        // CF_PRIVATEFIRST..=CF_PRIVATELAST and CF_GDIOBJFIRST..=CF_GDIOBJLAST.
        0x0200..=0x03FF => false,
        _ => !name.is_some_and(|n| SKIPPED_NAMES.iter().any(|s| s.eq_ignore_ascii_case(n))),
    }
}

/// Every line break (LF, CRLF or a lone CR) as CRLF, the Windows clipboard convention.
pub(crate) fn to_crlf(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 16);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("\r\n");
            }
            '\n' => out.push_str("\r\n"),
            _ => out.push(c),
        }
    }
    out
}

/// What CF_UNICODETEXT holds: UTF-16, CRLF line breaks, NUL-terminated.
pub(crate) fn clipboard_utf16(text: &str) -> Vec<u16> {
    let mut units: Vec<u16> = to_crlf(text).encode_utf16().collect();
    units.push(0);
    units
}

/// Text from CF_UNICODETEXT bytes, stopping at the first NUL, with CRLF folded to LF.
pub(crate) fn text_from_utf16_bytes(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|pair| u16::from_le_bytes(*pair)).take_while(|u| *u != 0).collect();
    String::from_utf16_lossy(&units).replace("\r\n", "\n")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TypeStep {
    /// One UTF-16 unit, sent as a Unicode keystroke.
    Unit(u16),
    Enter,
}

/// What "type" sends: each UTF-16 unit as a Unicode keystroke, each line break (LF, CRLF or a
/// lone CR) as one Enter.
pub(crate) fn typing_steps(text: &str) -> Vec<TypeStep> {
    let mut steps = Vec::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                steps.push(TypeStep::Enter);
            }
            '\n' => steps.push(TypeStep::Enter),
            _ => {
                let mut buf = [0u16; 2];
                steps.extend(c.encode_utf16(&mut buf).iter().map(|u| TypeStep::Unit(*u)));
            }
        }
    }
    steps
}

/// Splits typing steps into batches of about `size`, never between the two halves of a
/// surrogate pair (an emoji must arrive in one SendInput call).
pub(crate) fn typing_batches(steps: &[TypeStep], size: usize) -> Vec<&[TypeStep]> {
    let size = size.max(2);
    let mut batches = Vec::new();
    let mut start = 0;
    while start < steps.len() {
        let mut end = (start + size).min(steps.len());
        if end < steps.len() {
            if let TypeStep::Unit(u) = steps[end - 1] {
                if (0xD800..=0xDBFF).contains(&u) {
                    end += 1;
                }
            }
        }
        batches.push(&steps[start..end]);
        start = end;
    }
    batches
}

/// Terminals where Ctrl+V reaches the shell as a control character and Shift+Insert is the paste
/// key: mintty (Git Bash, MSYS2, Cygwin) and PuTTY/KiTTY. Everything else gets Ctrl+V.
pub(crate) fn paste_uses_shift_insert(process_name: &str) -> bool {
    matches!(crate::context::process_stem(process_name).as_str(), "mintty" | "putty" | "kitty")
}

/// Remote desktops, remote-control apps and virtual machine windows: the far side asks for the
/// clipboard over the network only when it handles the paste, which can take well over the usual
/// settle time, and putting the old clipboard back first would paste that instead.
pub(crate) fn reads_clipboard_late(process_name: &str) -> bool {
    matches!(
        crate::context::process_stem(process_name).as_str(),
        "mstsc" | "msrdc" | "msrdcw" | "rdcman" | "wfica32" | "cdviewer" | "vmconnect" | "vmware" | "vmware-view"
            | "vmware-remotemks" | "virtualboxvm" | "anydesk" | "teamviewer" | "parsecd" | "rustdesk"
    )
}

/// Chat sites where Enter sends the message, as their browser tab titles name them.
/// Browser tabs where Enter is a new line, paragraph or bullet, and Shift+Enter a soft break or,
/// in a notebook, "run this cell".
const WEB_EDITORS: &[&str] = &[
    "Google Docs", "Google Sheets", "Google Slides", "Notion", "Word", "Confluence", "Gmail", "Outlook", "Dropbox Paper", "Coda",
    "Colab", "Jupyter", "JupyterLab", "ipynb", "Kaggle", "Replit", "CodeSandbox", "StackBlitz", "github.dev", "Overleaf",
];

/// Apps where Enter sends the message, so a typed line break is Shift+Enter (a new line in the
/// same message): chat apps, the AI desktop apps, and browser tabs other than document editors.
/// Browser tabs are judged by what they aren't, because a chat's tab is often renamed after the
/// conversation ("Trip ideas - Google Chrome" in ChatGPT), and sending half a message is worse than
/// a soft break.
pub(crate) fn line_break_needs_shift(process_name: &str, window_title: &str) -> bool {
    match crate::context::categorise(process_name, window_title) {
        "chat" => true,
        "browser" => !window_title.trim().is_empty() && !WEB_EDITORS.iter().any(|name| crate::context::title_mentions(window_title, name)),
        _ => matches!(crate::context::process_stem(process_name).as_str(), "claude" | "chatgpt" | "codex"),
    }
}

/// UIPI stops input from a lower integrity level reaching a higher one. A target whose level
/// cannot be read is treated as higher: in practice that is an elevated or protected process.
pub(crate) fn uipi_blocks(target_level: Option<u32>, our_level: u32) -> bool {
    target_level.is_none_or(|level| level > our_level)
}

// ---------------------------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------------------------

#[cfg(windows)]
mod win {
    use super::*;
    use std::ffi::c_void;
    use std::thread::sleep;
    use std::time::Instant;
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{CloseHandle, GlobalFree, HANDLE, HGLOBAL, LPARAM, WPARAM};
    use windows::Win32::Security::{
        GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TokenIntegrityLevel, TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
    };
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardFormatNameW, GetClipboardSequenceNumber,
        OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
        KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC, VIRTUAL_KEY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId, SendMessageTimeoutW, SMTO_ABORTIFHUNG, WM_NULL};

    const CF_UNICODETEXT: u32 = 13;
    const VK_BACK: u16 = 0x08;
    const VK_RETURN: u16 = 0x0D;
    const VK_SHIFT: u16 = 0x10;
    const VK_CONTROL: u16 = 0x11;
    const VK_LEFT: u16 = 0x25;
    const VK_INSERT: u16 = 0x2D;
    const VK_DELETE: u16 = 0x2E;
    const VK_C: u16 = 0x43;
    const VK_V: u16 = 0x56;
    const VK_Z: u16 = 0x5A;
    const VK_LWIN: u16 = 0x5B;
    const VK_RWIN: u16 = 0x5C;
    const VK_LMENU: u16 = 0xA4;
    const VK_RMENU: u16 = 0xA5;
    const HELD_MODIFIERS: [u16; 8] = [0xA2, 0xA3, 0xA0, 0xA1, VK_LMENU, VK_RMENU, VK_LWIN, VK_RWIN];

    // ----- clipboard -----

    /// An open clipboard; closed on drop.
    struct Clipboard;

    impl Clipboard {
        fn open() -> Result<Self> {
            Self::open_with(OPEN_ATTEMPTS)
        }

        fn open_with(attempts: u32) -> Result<Self> {
            for attempt in 0..attempts {
                // No owner window: Navi never uses delayed rendering, so it needs no clipboard messages.
                if unsafe { OpenClipboard(None) }.is_ok() {
                    return Ok(Clipboard);
                }
                if attempt + 1 < attempts {
                    sleep(OPEN_RETRY_GAP);
                }
            }
            Err(anyhow!("Another app is holding the clipboard, so Navi couldn't use it. Please try again."))
        }

        fn empty(&self) -> Result<()> {
            unsafe { EmptyClipboard() }.map_err(|e| anyhow!("Windows wouldn't let Navi clear the clipboard ({e})."))
        }

        fn snapshot(&self) -> Snapshot {
            let started = Instant::now();
            let mut items = Vec::new();
            let mut total = 0usize;
            let mut format = 0u32;
            loop {
                format = unsafe { EnumClipboardFormats(format) };
                if format == 0 {
                    break;
                }
                if started.elapsed() > SNAPSHOT_BUDGET {
                    log::warn!("Clipboard snapshot stopped at its time limit; some formats will not be restored");
                    break;
                }
                if !should_snapshot_format(format, format_name(format).as_deref()) {
                    continue;
                }
                // For formats the owner renders on demand, this asks the owner to render now.
                let Ok(handle) = (unsafe { GetClipboardData(format) }) else {
                    continue;
                };
                if handle.is_invalid() {
                    continue;
                }
                let Some(bytes) = read_hglobal(HGLOBAL(handle.0), SNAPSHOT_MAX_FORMAT_BYTES) else {
                    continue;
                };
                if total + bytes.len() > SNAPSHOT_MAX_TOTAL_BYTES {
                    log::warn!("Clipboard snapshot is over its size limit; some formats will not be restored");
                    continue;
                }
                total += bytes.len();
                items.push((format, bytes));
            }
            Snapshot { items }
        }

        fn restore(&self, snapshot: &Snapshot) -> Result<()> {
            self.empty()?;
            for (format, bytes) in &snapshot.items {
                if let Err(e) = set_bytes(*format, bytes) {
                    log::debug!("Could not restore clipboard format {format}: {e}");
                }
            }
            if !snapshot.items.is_empty() {
                mark_private();
            }
            Ok(())
        }

        fn write_text(&self, text: &str, private: bool) -> Result<()> {
            self.empty()?;
            let units = clipboard_utf16(text);
            let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
            set_bytes(CF_UNICODETEXT, &bytes)?;
            if private {
                mark_private();
            }
            Ok(())
        }

        fn read_text(&self) -> Option<String> {
            let handle = unsafe { GetClipboardData(CF_UNICODETEXT) }.ok()?;
            if handle.is_invalid() {
                return None;
            }
            read_hglobal(HGLOBAL(handle.0), SNAPSHOT_MAX_FORMAT_BYTES).map(|bytes| text_from_utf16_bytes(&bytes))
        }
    }

    impl Drop for Clipboard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseClipboard();
            }
        }
    }

    struct Snapshot {
        items: Vec<(u32, Vec<u8>)>,
    }

    /// Registered formats have names; the predefined ones (below 0xC000) do not.
    fn format_name(format: u32) -> Option<String> {
        if format < 0xC000 {
            return None;
        }
        let mut buf = [0u16; 256];
        let len = unsafe { GetClipboardFormatNameW(format, &mut buf) };
        (len > 0).then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }

    fn read_hglobal(hglobal: HGLOBAL, max: usize) -> Option<Vec<u8>> {
        unsafe {
            let size = GlobalSize(hglobal);
            if size == 0 || size > max {
                return None;
            }
            let ptr = GlobalLock(hglobal);
            if ptr.is_null() {
                return None;
            }
            let bytes = std::slice::from_raw_parts(ptr as *const u8, size).to_vec();
            let _ = GlobalUnlock(hglobal);
            Some(bytes)
        }
    }

    fn set_bytes(format: u32, bytes: &[u8]) -> Result<()> {
        unsafe {
            let hglobal = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1))?;
            let ptr = GlobalLock(hglobal);
            if ptr.is_null() {
                let _ = GlobalFree(Some(hglobal));
                return Err(anyhow!("Navi couldn't get memory for the clipboard."));
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
            let _ = GlobalUnlock(hglobal);
            // On success the clipboard owns the memory; on failure it is still ours to free.
            if let Err(e) = SetClipboardData(format, Some(HANDLE(hglobal.0))) {
                let _ = GlobalFree(Some(hglobal));
                return Err(anyhow!("Windows refused the clipboard data ({e})."));
            }
            Ok(())
        }
    }

    /// Asks Win+V history, cloud clipboard and clipboard managers to ignore what is on the clipboard.
    fn mark_private() {
        let zero = 0u32.to_le_bytes();
        for name in [EXCLUDE_FORMAT, HISTORY_FORMAT, CLOUD_FORMAT] {
            let id = unsafe { RegisterClipboardFormatW(&HSTRING::from(name)) };
            if id != 0 {
                let _ = set_bytes(id, &zero);
            }
        }
    }

    fn sequence() -> u32 {
        unsafe { GetClipboardSequenceNumber() }
    }

    pub(super) fn set_text(text: &str, private: bool) -> Result<()> {
        Clipboard::open()?.write_text(text, private)
    }

    // ----- keyboard -----

    fn is_extended(vk: u16) -> bool {
        // Page Up/Down, End, Home, arrows, Insert, Delete, Win keys, Apps, Num /, Num Lock, right
        // Ctrl and Alt. Without the flag, Shift + an arrow with Num Lock on acts like the numpad.
        matches!(vk, 0x21..=0x28 | 0x2D | 0x2E | 0x5B..=0x5D | 0x6F | 0x90 | 0xA3 | 0xA5)
    }

    fn key(vk: u16, up: bool) -> INPUT {
        let mut flags = KEYBD_EVENT_FLAGS(0);
        if up {
            flags |= KEYEVENTF_KEYUP;
        }
        if is_extended(vk) {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16;
        keyboard_input(vk, scan, flags)
    }

    fn unicode(unit: u16, up: bool) -> INPUT {
        let mut flags = KEYEVENTF_UNICODE;
        if up {
            flags |= KEYEVENTF_KEYUP;
        }
        keyboard_input(0, unit, flags)
    }

    fn keyboard_input(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: NAVI_INPUT_TAG },
            },
        }
    }

    fn send(inputs: &[INPUT]) -> Result<()> {
        if inputs.is_empty() {
            return Ok(());
        }
        let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent as usize == inputs.len() {
            Ok(())
        } else {
            Err(anyhow!("Windows blocked Navi's keystrokes."))
        }
    }

    fn chord(modifier: u16, vk: u16) -> [INPUT; 4] {
        [key(modifier, false), key(vk, false), key(vk, true), key(modifier, true)]
    }

    fn is_down(vk: u16) -> bool {
        let state = unsafe { GetAsyncKeyState(vk as i32) };
        state < 0
    }

    /// Waits for the user to let go of every modifier. If some are still down after the wait
    /// (held on purpose, or stuck), releases them for Windows so our keystrokes stay what they are.
    fn wait_for_modifiers() {
        let started = Instant::now();
        loop {
            let held: Vec<u16> = HELD_MODIFIERS.iter().copied().filter(|vk| is_down(*vk)).collect();
            if held.is_empty() {
                return;
            }
            if started.elapsed() >= MODIFIER_WAIT {
                let mut inputs = Vec::with_capacity(held.len() + 2);
                // A lone Win or Alt release would open Start or the menu bar; the mask key prevents it.
                if held.iter().any(|vk| matches!(*vk, VK_LWIN | VK_RWIN | VK_LMENU | VK_RMENU)) {
                    inputs.push(key(crate::hotkey::MASK_VK as u16, false));
                    inputs.push(key(crate::hotkey::MASK_VK as u16, true));
                }
                inputs.extend(held.iter().map(|vk| key(*vk, true)));
                let _ = send(&inputs);
                log::debug!("Released {} modifier key(s) the user was still holding", held.len());
                return;
            }
            sleep(Duration::from_millis(10));
        }
    }

    // ----- integrity levels (UIPI) -----

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    /// The mandatory integrity RID of a process (0x2000 medium, 0x3000 high, 0x4000 system).
    fn integrity_level(process: HANDLE) -> Option<u32> {
        unsafe {
            let mut token = HANDLE::default();
            OpenProcessToken(process, TOKEN_QUERY, &mut token).ok()?;
            let token = OwnedHandle(token);
            let mut len = 0u32;
            let _ = GetTokenInformation(token.0, TokenIntegrityLevel, None, 0, &mut len);
            if len == 0 {
                return None;
            }
            // u64 storage keeps the pointer inside TOKEN_MANDATORY_LABEL aligned.
            let mut buf = vec![0u64; (len as usize).div_ceil(8)];
            GetTokenInformation(token.0, TokenIntegrityLevel, Some(buf.as_mut_ptr() as *mut c_void), len, &mut len).ok()?;
            let label = &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL);
            let sid = label.Label.Sid;
            let count_ptr = GetSidSubAuthorityCount(sid);
            if count_ptr.is_null() || *count_ptr == 0 {
                return None;
            }
            let rid_ptr = GetSidSubAuthority(sid, (*count_ptr - 1) as u32);
            (!rid_ptr.is_null()).then(|| *rid_ptr)
        }
    }

    /// True when the foreground window belongs to a process at a higher integrity level than Navi
    /// (usually "Run as administrator"), where Windows silently drops our keystrokes.
    pub(super) fn blocked_by_uipi() -> bool {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.0.is_null() {
                return false;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == 0 || pid == GetCurrentProcessId() {
                return false;
            }
            let Some(ours) = integrity_level(GetCurrentProcess()) else {
                return false;
            };
            let theirs = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok().and_then(|handle| {
                let process = OwnedHandle(handle);
                integrity_level(process.0)
            });
            uipi_blocks(theirs, ours)
        }
    }

    /// If keystrokes cannot reach the foreground window, puts `text` on the clipboard instead.
    pub(super) fn clipboard_only_if_blocked(text: &str) -> Result<Option<InsertOutcome>> {
        if !blocked_by_uipi() {
            return Ok(None);
        }
        set_text(text, false)?;
        Ok(Some(InsertOutcome::ClipboardOnly(ELEVATED_REASON.to_string())))
    }

    // ----- the operations -----

    pub(super) fn paste(text: &str, restore: bool) -> Result<InsertOutcome> {
        let clipboard = match Clipboard::open() {
            Ok(clipboard) => clipboard,
            Err(e) => {
                log::warn!("{e} Typing the text instead.");
                return type_text(text);
            }
        };
        let snapshot = restore.then(|| clipboard.snapshot());
        // Kept out of clipboard history only when it is about to be swapped back out.
        if let Err(e) = clipboard.write_text(text, restore) {
            if let Some(snapshot) = &snapshot {
                let _ = clipboard.restore(snapshot);
            }
            drop(clipboard);
            log::warn!("{e} Typing the text instead.");
            return type_text(text);
        }
        drop(clipboard);
        let written = sequence();

        let target = crate::context::foreground_process_name();
        let keys = if paste_uses_shift_insert(&target) {
            chord(VK_SHIFT, VK_INSERT)
        } else {
            chord(VK_CONTROL, VK_V)
        };
        wait_for_modifiers();
        if send(&keys).is_err() {
            // The text is still on the clipboard; restoring now would lose it.
            return Ok(InsertOutcome::ClipboardOnly(BLOCKED_REASON.to_string()));
        }
        if snapshot.is_some() {
            settle_before_restore(&target);
        } else {
            sleep(PASTE_SETTLE);
        }

        if let Some(snapshot) = snapshot {
            if sequence() != written {
                log::debug!("Clipboard changed during the paste; leaving it as it is");
            } else {
                match Clipboard::open() {
                    Ok(clipboard) => {
                        if let Err(e) = clipboard.restore(&snapshot) {
                            log::warn!("Could not restore the clipboard: {e}");
                        }
                    }
                    Err(e) => log::warn!("Could not restore the clipboard: {e}"),
                }
            }
        }
        Ok(InsertOutcome::Pasted)
    }

    /// Gives the target time to read the clipboard before the old contents go back. An app that is
    /// busy handles the Ctrl+V only when its window responds again, so wait for that first (up to
    /// BUSY_TARGET_WAIT). A remote desktop fetches the text when the far side pastes.
    fn settle_before_restore(target: &str) {
        if reads_clipboard_late(target) {
            sleep(REMOTE_PASTE_SETTLE);
            return;
        }
        let hwnd = unsafe { GetForegroundWindow() };
        if !hwnd.0.is_null() {
            let started = Instant::now();
            // Returns once the window's thread is taking messages again, or straight away if
            // Windows already counts it as hung.
            let mut result = 0usize;
            unsafe {
                let _ = SendMessageTimeoutW(hwnd, WM_NULL, WPARAM(0), LPARAM(0), SMTO_ABORTIFHUNG, BUSY_TARGET_WAIT.as_millis() as u32, Some(&mut result));
            }
            let waited = started.elapsed();
            if waited >= Duration::from_millis(100) {
                log::debug!("The target app was busy for {} ms before pasting", waited.as_millis());
            }
        }
        sleep(PASTE_SETTLE);
    }

    pub(super) fn type_text(text: &str) -> Result<InsertOutcome> {
        wait_for_modifiers();
        // Where Enter sends the message, a line break must not send half of it.
        let (_, title, process) = crate::context::foreground_app();
        let shift_enter = line_break_needs_shift(&process, &title);
        let steps = typing_steps(text);
        let batches = typing_batches(&steps, TYPE_BATCH_UNITS);
        let count = batches.len();
        for (index, batch) in batches.into_iter().enumerate() {
            let mut inputs = Vec::with_capacity(batch.len() * 2);
            for step in batch {
                match *step {
                    TypeStep::Unit(unit) => {
                        inputs.push(unicode(unit, false));
                        inputs.push(unicode(unit, true));
                    }
                    TypeStep::Enter if shift_enter => {
                        inputs.push(key(VK_SHIFT, false));
                        inputs.push(key(VK_RETURN, false));
                        inputs.push(key(VK_RETURN, true));
                        inputs.push(key(VK_SHIFT, true));
                    }
                    TypeStep::Enter => {
                        inputs.push(key(VK_RETURN, false));
                        inputs.push(key(VK_RETURN, true));
                    }
                }
            }
            if send(&inputs).is_err() {
                if shift_enter {
                    // Never leave Shift held down for Windows.
                    let _ = send(&[key(VK_SHIFT, true)]);
                }
                set_text(text, false)?;
                let reason = if index == 0 { BLOCKED_REASON } else { TYPING_STOPPED_REASON };
                return Ok(InsertOutcome::ClipboardOnly(reason.to_string()));
            }
            if index + 1 < count {
                sleep(TYPE_BATCH_GAP);
            }
        }
        Ok(InsertOutcome::Typed)
    }

    pub(super) fn select_back(chars_utf16: usize) -> Result<()> {
        if chars_utf16 == 0 {
            return Ok(());
        }
        wait_for_modifiers();
        let mut remaining = chars_utf16;
        let mut first = true;
        while remaining > 0 {
            let take = remaining.min(SELECT_BATCH);
            remaining -= take;
            let mut inputs = Vec::with_capacity(take * 2 + 2);
            if first {
                inputs.push(key(VK_SHIFT, false));
            }
            for _ in 0..take {
                inputs.push(key(VK_LEFT, false));
                inputs.push(key(VK_LEFT, true));
            }
            if remaining == 0 {
                inputs.push(key(VK_SHIFT, true));
            }
            if let Err(e) = send(&inputs) {
                // Never leave Shift held down for Windows.
                let _ = send(&[key(VK_SHIFT, true)]);
                return Err(e);
            }
            first = false;
            if remaining > 0 {
                sleep(TYPE_BATCH_GAP);
            }
        }
        // Let the app finish moving the selection before anything replaces it.
        sleep(Duration::from_millis(30));
        Ok(())
    }

    pub(super) fn delete_selection() -> Result<()> {
        send(&[key(VK_DELETE, false), key(VK_DELETE, true)])
    }

    /// Deletes `count` characters before the cursor (terminals, where Shift+Left does not select).
    pub(super) fn backspace(count: usize) -> Result<()> {
        if count == 0 {
            return Ok(());
        }
        wait_for_modifiers();
        let mut remaining = count;
        while remaining > 0 {
            let take = remaining.min(SELECT_BATCH);
            remaining -= take;
            let inputs: Vec<INPUT> = (0..take).flat_map(|_| [key(VK_BACK, false), key(VK_BACK, true)]).collect();
            send(&inputs)?;
            if remaining > 0 {
                sleep(TYPE_BATCH_GAP);
            }
        }
        sleep(Duration::from_millis(30));
        Ok(())
    }

    pub(super) fn undo() -> Result<()> {
        wait_for_modifiers();
        send(&chord(VK_CONTROL, VK_Z)).map_err(|_| anyhow!("Windows blocked Navi's keystrokes, so it couldn't undo."))
    }

    pub(super) fn copy_selection() -> Result<Option<String>> {
        if blocked_by_uipi() || crate::context::foreground_category() == "terminal" {
            return Ok(None);
        }
        let snapshot = Clipboard::open()?.snapshot();
        let before = sequence();

        wait_for_modifiers();
        if send(&chord(VK_CONTROL, VK_C)).is_err() {
            return Ok(None);
        }
        let started = Instant::now();
        while sequence() == before {
            if started.elapsed() >= COPY_WAIT {
                return Ok(None);
            }
            sleep(Duration::from_millis(10));
        }

        // The app may still be adding formats; give it a little longer to let go of the clipboard.
        let clipboard = Clipboard::open_with(OPEN_ATTEMPTS * 2)?;
        let text = clipboard.read_text();
        if let Err(e) = clipboard.restore(&snapshot) {
            log::warn!("Could not restore the clipboard after copying the selection: {e}");
        }
        Ok(text.filter(|t| !t.is_empty()))
    }
}

/// The Mac counterpart of `win`: NSPasteboard for the clipboard and Quartz keyboard events
/// (CGEventPost) for Command + V, typing and selecting. Every posted event carries
/// `NAVI_INPUT_TAG` in its user data, so Navi's own shortcut listener ignores it, and its flags are
/// set explicitly, so a modifier the user still holds can't turn Command + V into something else.
/// Posting keys needs the Accessibility permission; without it macOS drops them silently, so the
/// text is left on the clipboard instead, with a reason.
#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardTypeString, NSPasteboardWriting};
    use objc2_core_foundation::{CFRange, CFRetained};
    use objc2_core_graphics::{CGEvent, CGEventField, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation};
    use objc2_foundation::{NSArray, NSData, NSString};
    use std::thread::sleep;
    use std::time::Instant;

    const KC_RETURN: u16 = 0x24;
    const KC_DELETE: u16 = 0x33;
    const KC_LEFT: u16 = 0x7B;
    const KC_V: u16 = 0x09;
    const KC_C: u16 = 0x08;
    const KC_Z: u16 = 0x06;
    /// UTF-16 units per typed event: macOS takes at most 20.
    const UNITS_PER_EVENT: usize = 20;

    // Each fits the two-line bubble (80 characters at most).
    const SECURE_REASON: &str = "macOS hides typing in password fields. Your text is on the clipboard: press ⌘V.";
    const NO_PERMISSION_REASON: &str = "Navi needs the Accessibility permission to type. Your text is on the clipboard.";

    #[link(name = "Carbon", kind = "framework")]
    extern "C" {
        fn IsSecureEventInputEnabled() -> u8;
    }

    // ----- the clipboard -----

    fn pasteboard() -> Retained<NSPasteboard> {
        NSPasteboard::generalPasteboard()
    }

    /// Every item on the pasteboard, each as (type, bytes) pairs, within the snapshot limits.
    struct Snapshot(Vec<Vec<(String, Vec<u8>)>>);

    fn snapshot() -> Snapshot {
        let started = Instant::now();
        let mut total = 0usize;
        let mut items = Vec::new();
        let Some(list) = pasteboard().pasteboardItems() else { return Snapshot(items) };
        'items: for item in list.iter() {
            let mut kept = Vec::new();
            for kind in item.types().iter() {
                if started.elapsed() > SNAPSHOT_BUDGET {
                    break 'items;
                }
                let Some(data) = item.dataForType(&kind) else { continue };
                let bytes = data.to_vec();
                if bytes.len() > SNAPSHOT_MAX_FORMAT_BYTES || total + bytes.len() > SNAPSHOT_MAX_TOTAL_BYTES {
                    continue;
                }
                total += bytes.len();
                kept.push((kind.to_string(), bytes));
            }
            if !kept.is_empty() {
                items.push(kept);
            }
        }
        Snapshot(items)
    }

    fn restore(snapshot: &Snapshot) {
        let board = pasteboard();
        board.clearContents();
        if snapshot.0.is_empty() {
            return;
        }
        let items: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = snapshot
            .0
            .iter()
            .map(|pairs| {
                let item = NSPasteboardItem::new();
                for (kind, bytes) in pairs {
                    item.setData_forType(&NSData::with_bytes(bytes), &NSString::from_str(kind));
                }
                ProtocolObject::from_retained(item)
            })
            .collect();
        let array = NSArray::from_retained_slice(&items);
        if !board.writeObjects(&array) {
            log::warn!("Could not restore the clipboard");
        }
    }

    /// Puts `text` on the pasteboard. `private`: marked transient and auto-generated, so clipboard
    /// managers (Maccy, Raycast, Alfred, Paste) leave it out of their history.
    pub(super) fn set_text(text: &str, private: bool) -> Result<()> {
        let board = pasteboard();
        board.clearContents();
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let item = NSPasteboardItem::new();
        if !item.setString_forType(&NSString::from_str(&text), unsafe { NSPasteboardTypeString }) {
            return Err(anyhow!("Couldn't put the text on the clipboard."));
        }
        if private {
            for marker in ["org.nspasteboard.TransientType", "org.nspasteboard.AutoGeneratedType"] {
                item.setData_forType(&NSData::with_bytes(&[]), &NSString::from_str(marker));
            }
        }
        let array = NSArray::from_retained_slice(&[ProtocolObject::from_retained(item)]);
        if board.writeObjects(&array) {
            Ok(())
        } else {
            Err(anyhow!("Couldn't put the text on the clipboard."))
        }
    }

    fn read_text() -> Option<String> {
        pasteboard().stringForType(unsafe { NSPasteboardTypeString }).map(|s| s.to_string())
    }

    fn change_count() -> isize {
        pasteboard().changeCount()
    }

    // ----- keys -----

    fn source() -> Option<CFRetained<CGEventSource>> {
        CGEventSource::new(CGEventSourceStateID::Private)
    }

    fn post_key(code: u16, down: bool, flags: CGEventFlags) -> Result<()> {
        let event = CGEvent::new_keyboard_event(source().as_deref(), code, down).ok_or_else(|| anyhow!("macOS couldn't make a key event."))?;
        CGEvent::set_flags(Some(&event), flags);
        CGEvent::set_integer_value_field(Some(&event), CGEventField::EventSourceUserData, NAVI_INPUT_TAG as i64);
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
        Ok(())
    }

    fn tap(code: u16, flags: CGEventFlags) -> Result<()> {
        post_key(code, true, flags)?;
        post_key(code, false, flags)
    }

    fn command(code: u16) -> Result<()> {
        tap(code, CGEventFlags::MaskCommand)
    }

    /// Up to 20 UTF-16 units typed by one key event, whatever the keyboard layout.
    fn post_units(units: &[u16]) -> Result<()> {
        for down in [true, false] {
            let event = CGEvent::new_keyboard_event(source().as_deref(), 0, down).ok_or_else(|| anyhow!("macOS couldn't make a key event."))?;
            CGEvent::set_flags(Some(&event), CGEventFlags(0));
            unsafe { CGEvent::keyboard_set_unicode_string(Some(&event), units.len() as _, units.as_ptr()) };
            CGEvent::set_integer_value_field(Some(&event), CGEventField::EventSourceUserData, NAVI_INPUT_TAG as i64);
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
        }
        Ok(())
    }

    /// Waits (up to MODIFIER_WAIT) for the user to let go of Command, Shift, Option, Control and Fn.
    fn wait_for_modifiers() {
        let held = CGEventFlags::MaskCommand.0
            | CGEventFlags::MaskShift.0
            | CGEventFlags::MaskAlternate.0
            | CGEventFlags::MaskControl.0
            | CGEventFlags::MaskSecondaryFn.0;
        let started = Instant::now();
        while started.elapsed() < MODIFIER_WAIT {
            if CGEventSource::flags_state(CGEventSourceStateID::HIDSystemState).0 & held == 0 {
                return;
            }
            sleep(Duration::from_millis(10));
        }
    }

    fn secure_input() -> bool {
        unsafe { IsSecureEventInputEnabled() != 0 }
    }

    /// Leaves `text` on the clipboard when macOS would drop Navi's keys: a password field (Secure
    /// Event Input) or no Accessibility permission.
    pub(super) fn clipboard_only_if_blocked(text: &str) -> Result<Option<InsertOutcome>> {
        let reason = if !crate::mac::ax::trusted() {
            NO_PERMISSION_REASON
        } else if secure_input() {
            SECURE_REASON
        } else {
            return Ok(None);
        };
        set_text(text, false)?;
        Ok(Some(InsertOutcome::ClipboardOnly(reason.to_string())))
    }

    pub(super) fn blocked() -> bool {
        !crate::mac::ax::trusted() || secure_input()
    }

    // ----- the operations -----

    pub(super) fn paste(text: &str, restore_after: bool) -> Result<InsertOutcome> {
        let kept = restore_after.then(snapshot);
        if let Err(e) = set_text(text, restore_after) {
            if let Some(kept) = &kept {
                restore(kept);
            }
            log::warn!("{e} Typing the text instead.");
            return type_text(text);
        }
        let written = change_count();
        let target = crate::context::foreground_process_name();
        wait_for_modifiers();
        if command(KC_V).is_err() {
            return Ok(InsertOutcome::ClipboardOnly("macOS blocked the typing. Your text is on the clipboard: press ⌘V.".to_string()));
        }
        sleep(if reads_clipboard_late(&target) { REMOTE_PASTE_SETTLE } else { PASTE_SETTLE });
        if let Some(kept) = kept {
            if change_count() != written {
                log::debug!("Clipboard changed during the paste; leaving it as it is");
            } else {
                restore(&kept);
            }
        }
        Ok(InsertOutcome::Pasted)
    }

    pub(super) fn type_text(text: &str) -> Result<InsertOutcome> {
        wait_for_modifiers();
        let (_, title, process) = crate::context::foreground_app();
        let shift_enter = line_break_needs_shift(&process, &title);
        let mut pending: Vec<u16> = Vec::with_capacity(UNITS_PER_EVENT);
        let flush = |pending: &mut Vec<u16>| -> Result<()> {
            if !pending.is_empty() {
                post_units(pending)?;
                pending.clear();
                sleep(Duration::from_millis(2));
            }
            Ok(())
        };
        for step in typing_steps(text) {
            match step {
                TypeStep::Unit(unit) => {
                    // Never split a surrogate pair across two events.
                    let high = (0xD800..=0xDBFF).contains(&unit);
                    if pending.len() >= UNITS_PER_EVENT || (high && pending.len() + 2 > UNITS_PER_EVENT) {
                        flush(&mut pending)?;
                    }
                    pending.push(unit);
                }
                TypeStep::Enter => {
                    flush(&mut pending)?;
                    tap(KC_RETURN, if shift_enter { CGEventFlags::MaskShift } else { CGEventFlags(0) })?;
                }
            }
        }
        flush(&mut pending)?;
        Ok(InsertOutcome::Typed)
    }

    /// Selects the last `chars_utf16` characters before the cursor. Where the field says where its
    /// caret is, the selection is set exactly through Accessibility (ranges count UTF-16 units, as
    /// Navi does); otherwise Shift + Left is pressed that many times.
    pub(super) fn select_back(chars_utf16: usize) -> Result<()> {
        if chars_utf16 == 0 {
            return Ok(());
        }
        if let Some(el) = crate::mac::ax::focused_element() {
            if let Some(range) = el.range("AXSelectedTextRange") {
                let start = range.location - chars_utf16 as isize;
                if start >= 0 && el.set_range("AXSelectedTextRange", CFRange { location: start, length: chars_utf16 as isize }) {
                    sleep(Duration::from_millis(30));
                    if el.range("AXSelectedTextRange").is_some_and(|r| r.location == start && r.length == chars_utf16 as isize) {
                        return Ok(());
                    }
                }
            }
        }
        wait_for_modifiers();
        for _ in 0..chars_utf16 {
            tap(KC_LEFT, CGEventFlags::MaskShift)?;
        }
        sleep(Duration::from_millis(30));
        Ok(())
    }

    pub(super) fn delete_selection() -> Result<()> {
        tap(KC_DELETE, CGEventFlags(0))
    }

    pub(super) fn backspace(count: usize) -> Result<()> {
        if count == 0 {
            return Ok(());
        }
        wait_for_modifiers();
        for i in 0..count {
            tap(KC_DELETE, CGEventFlags(0))?;
            if i % SELECT_BATCH == SELECT_BATCH - 1 {
                sleep(TYPE_BATCH_GAP);
            }
        }
        sleep(Duration::from_millis(30));
        Ok(())
    }

    pub(super) fn undo() -> Result<()> {
        wait_for_modifiers();
        command(KC_Z).map_err(|_| anyhow!("macOS blocked Navi's keystrokes, so it couldn't undo."))
    }

    /// The selected text: read through Accessibility where the app says, else copied with
    /// Command + C and the clipboard put back.
    pub(super) fn copy_selection() -> Result<Option<String>> {
        if blocked() {
            return Ok(None);
        }
        if let Some(el) = crate::mac::ax::focused_element() {
            if !el.is_secure() {
                if let Some(text) = el.string("AXSelectedText").filter(|t| !t.is_empty()) {
                    return Ok(Some(normalise(&text)));
                }
            }
        }
        let kept = snapshot();
        let before = change_count();
        wait_for_modifiers();
        command(KC_C)?;
        let started = Instant::now();
        while change_count() == before && started.elapsed() < COPY_WAIT {
            sleep(Duration::from_millis(15));
        }
        if change_count() == before {
            return Ok(None);
        }
        let text = read_text();
        restore(&kept);
        Ok(text.filter(|t| !t.is_empty()).map(|t| normalise(&t)))
    }

    fn normalise(text: &str) -> String {
        text.replace("\r\n", "\n").replace('\r', "\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_status_strings() {
        assert_eq!(InsertOutcome::Pasted.status(), "pasted");
        assert_eq!(InsertOutcome::Typed.status(), "typed");
        assert_eq!(InsertOutcome::ClipboardOnly("x".into()).status(), "clipboard");
    }

    #[test]
    fn snapshot_skips_handle_and_private_formats() {
        assert!(should_snapshot_format(13, None), "CF_UNICODETEXT");
        assert!(should_snapshot_format(1, None), "CF_TEXT");
        assert!(should_snapshot_format(8, None), "CF_DIB");
        assert!(should_snapshot_format(17, None), "CF_DIBV5");
        assert!(should_snapshot_format(15, None), "CF_HDROP");
        assert!(should_snapshot_format(0xC123, Some("HTML Format")));
        assert!(should_snapshot_format(0xC124, Some("Rich Text Format")));
        assert!(!should_snapshot_format(0, None));
        assert!(!should_snapshot_format(2, None), "CF_BITMAP");
        assert!(!should_snapshot_format(3, None), "CF_METAFILEPICT");
        assert!(!should_snapshot_format(9, None), "CF_PALETTE");
        assert!(!should_snapshot_format(14, None), "CF_ENHMETAFILE");
        assert!(!should_snapshot_format(0x80, None), "CF_OWNERDISPLAY");
        assert!(!should_snapshot_format(0x8E, None), "CF_DSPENHMETAFILE");
        assert!(!should_snapshot_format(0x250, None), "private range");
        assert!(!should_snapshot_format(0x300, None), "GDI object range");
        assert!(!should_snapshot_format(0x3FF, None), "GDI object range end");
        assert!(!should_snapshot_format(0xC200, Some("DataObject")));
        assert!(!should_snapshot_format(0xC201, Some("Ole Private Data")));
        assert!(!should_snapshot_format(0xC202, Some(EXCLUDE_FORMAT)));
        assert!(!should_snapshot_format(0xC203, Some("canincludeinclipboardhistory")));
        assert!(!should_snapshot_format(0xC204, Some(CLOUD_FORMAT)));
    }

    #[test]
    fn line_breaks_become_crlf() {
        assert_eq!(to_crlf("a\nb"), "a\r\nb");
        assert_eq!(to_crlf("a\r\nb"), "a\r\nb");
        assert_eq!(to_crlf("a\rb"), "a\r\nb");
        assert_eq!(to_crlf("a\n\nb\n"), "a\r\n\r\nb\r\n");
        assert_eq!(to_crlf("plain"), "plain");
        assert_eq!(to_crlf(""), "");
    }

    #[test]
    fn clipboard_text_round_trips() {
        let units = clipboard_utf16("Hi\nthere 😀");
        assert_eq!(units.last(), Some(&0));
        assert!(units.windows(2).any(|w| w == [13, 10]));
        let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(text_from_utf16_bytes(&bytes), "Hi\nthere 😀");
    }

    #[test]
    fn clipboard_bytes_stop_at_nul_and_tolerate_odd_lengths() {
        let mut bytes: Vec<u8> = "ab".encode_utf16().chain([0, 0x63]).flat_map(|u| u.to_le_bytes()).collect();
        bytes.push(0x41);
        assert_eq!(text_from_utf16_bytes(&bytes), "ab");
        assert_eq!(text_from_utf16_bytes(&[]), "");
    }

    #[test]
    fn typing_turns_line_breaks_into_enter() {
        use TypeStep::*;
        let u = |c: char| Unit(c as u16);
        assert_eq!(typing_steps("hi\nyo"), vec![u('h'), u('i'), Enter, u('y'), u('o')]);
        assert_eq!(typing_steps("a\r\nb"), vec![u('a'), Enter, u('b')]);
        assert_eq!(typing_steps("a\rb"), vec![u('a'), Enter, u('b')]);
        assert_eq!(typing_steps("\n\n"), vec![Enter, Enter]);
        assert_eq!(typing_steps("é"), vec![Unit(0xE9)]);
        assert_eq!(typing_steps("😀"), vec![Unit(0xD83D), Unit(0xDE00)]);
        assert!(typing_steps("").is_empty());
    }

    #[test]
    fn typing_batches_keep_surrogate_pairs_together() {
        let steps = typing_steps("ab😀cd");
        // a b D83D | DE00 ... would split the emoji; the batch grows by one instead.
        let batches = typing_batches(&steps, 3);
        assert_eq!(batches[0].len(), 4);
        assert_eq!(batches.iter().map(|b| b.len()).sum::<usize>(), steps.len());
        for batch in &batches {
            if let Some(TypeStep::Unit(last)) = batch.last() {
                assert!(!(0xD800..=0xDBFF).contains(last), "batch ends inside a surrogate pair");
            }
        }
        assert!(typing_batches(&[], 10).is_empty());
        assert_eq!(typing_batches(&typing_steps("abcdef"), 4).len(), 2);
    }

    #[test]
    fn shift_insert_only_for_terminals_that_need_it() {
        assert!(paste_uses_shift_insert("mintty.exe"));
        assert!(paste_uses_shift_insert("C:\\Program Files\\Git\\usr\\bin\\mintty.exe"));
        assert!(paste_uses_shift_insert("PUTTY.EXE"));
        assert!(!paste_uses_shift_insert("WindowsTerminal.exe"));
        assert!(!paste_uses_shift_insert("code.exe"));
        assert!(!paste_uses_shift_insert(""));
    }

    #[test]
    fn remote_desktops_get_longer_to_read_the_clipboard() {
        for exe in ["mstsc.exe", "msrdc.exe", "wfica32.exe", "CDViewer.exe", "vmconnect.exe", "VirtualBoxVM.exe", "C:\\Program Files\\VMware\\vmware.exe"] {
            assert!(reads_clipboard_late(exe), "{exe}");
        }
        for exe in ["code.exe", "slack.exe", "EXCEL.EXE", ""] {
            assert!(!reads_clipboard_late(exe), "{exe}");
        }
    }

    #[test]
    fn line_breaks_never_send_a_chat_message_early() {
        for exe in ["slack.exe", "ms-teams.exe", "Discord.exe", "WhatsApp.Root.exe", "Claude.exe", "ChatGPT.exe"] {
            assert!(line_break_needs_shift(exe, ""), "{exe}");
        }
        for (exe, title) in [
            ("chrome.exe", "(3) WhatsApp - Google Chrome"),
            ("msedge.exe", "Chat | Microsoft Teams - Microsoft Edge"),
            ("firefox.exe", "general (Channel) - Acme - Slack — Mozilla Firefox"),
            ("chrome.exe", "Trip ideas - Claude - Google Chrome"),
            ("chrome.exe", "ChatGPT - Google Chrome"),
            ("brave.exe", "Google Gemini - Brave"),
            // ChatGPT renames the tab after the conversation once it has started.
            ("chrome.exe", "Trip ideas - Google Chrome"),
        ] {
            assert!(line_break_needs_shift(exe, title), "{exe} / {title}");
        }
        // Enter is a new line, paragraph or bullet there, so it stays.
        for exe in ["WINWORD.EXE", "notepad.exe", "OUTLOOK.EXE", "code.exe", "WindowsTerminal.exe", ""] {
            assert!(!line_break_needs_shift(exe, ""), "{exe}");
        }
        for (exe, title) in [
            ("chrome.exe", "Plan - Google Docs - Google Chrome"),
            ("msedge.exe", "Notes | Notion - Microsoft Edge"),
            ("chrome.exe", ""),
            ("chrome.exe", "Inbox (2) - alex@example.com - Gmail - Google Chrome"),
            ("chrome.exe", "Teamsheet - Google Sheets - Google Chrome"),
            // Shift+Enter runs the cell in a notebook.
            ("chrome.exe", "analysis.ipynb - Colab - Google Chrome"),
            ("msedge.exe", "Untitled.ipynb - JupyterLab - Microsoft Edge"),
        ] {
            assert!(!line_break_needs_shift(exe, title), "{exe} / {title}");
        }
    }

    #[test]
    fn uipi_compares_integrity_levels() {
        const MEDIUM: u32 = 0x2000;
        const HIGH: u32 = 0x3000;
        const LOW: u32 = 0x1000;
        assert!(uipi_blocks(Some(HIGH), MEDIUM));
        assert!(!uipi_blocks(Some(MEDIUM), MEDIUM));
        assert!(!uipi_blocks(Some(LOW), MEDIUM));
        assert!(!uipi_blocks(Some(HIGH), HIGH));
        assert!(uipi_blocks(None, MEDIUM), "unreadable means elevated or protected");
    }

    #[test]
    fn user_facing_reasons_avoid_em_dashes() {
        for reason in [ELEVATED_REASON, BLOCKED_REASON, TYPING_STOPPED_REASON] {
            assert!(!reason.contains('\u{2014}') && !reason.contains('\u{2013}'), "{reason}");
        }
    }

    /// Pastes and types into a new TextEdit document with the real Mac code, reads the document
    /// back through Accessibility, then closes it unsaved. Needs the Accessibility permission for
    /// whatever runs the test. Run by hand:
    /// cargo test --lib insert::tests::live_mac_paste_and_type -- --ignored --nocapture
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn live_mac_paste_and_type() {
        use std::process::Command;
        use std::time::Duration;
        assert!(crate::mac::ax::trusted(), "this needs the Accessibility permission");
        let script = |s: &str| Command::new("/usr/bin/osascript").args(["-e", s]).output().expect("osascript");
        script(r#"tell application "TextEdit" to activate
tell application "TextEdit" to make new document"#);
        std::thread::sleep(Duration::from_millis(1200));
        crate::insert::set_clipboard_text("what you had copied").unwrap();
        let pasted = insert_text("Pasted by Navi, on a Mac.", "paste", true).expect("paste");
        std::thread::sleep(Duration::from_millis(400));
        let typed = insert_text(" Typed too: café ✓.", "type", true).expect("type");
        std::thread::sleep(Duration::from_millis(600));
        let front = crate::mac::front::front_app().map(|a| a.name).unwrap_or_default();
        let doc = script(r#"tell application "TextEdit" to get text of front document"#);
        let field = String::from_utf8_lossy(&doc.stdout).trim_end_matches('\n').to_string();
        println!("in front: {front}");
        let ctx = crate::context::capture(&crate::types::ContextSettings { read_field: true, read_app: true, ..Default::default() });
        println!("capture: app {:?} title {:?} before the caret {:?}", ctx.app_label, ctx.window_title, ctx.field_text);
        assert_eq!(ctx.field_text.as_deref(), Some("Pasted by Navi, on a Mac. Typed too: café ✓."), "the text before the caret is read");
        let full = crate::context::read_focused_text(ctx.hwnd, &crate::types::ContextSettings { read_field: true, read_app: true, ..Default::default() });
        assert_eq!(full.as_deref(), Some("Pasted by Navi, on a Mac. Typed too: café ✓."), "the whole field is read for learning");
        // "Scratch that": select back over what was typed and delete it.
        select_back_and_replace_with(" Typed too: café ✓.".encode_utf16().count(), "", "paste", true).expect("scratch that");
        std::thread::sleep(Duration::from_millis(400));
        let after = script(r#"tell application "TextEdit" to get text of front document"#);
        assert_eq!(String::from_utf8_lossy(&after.stdout).trim_end_matches('\n'), "Pasted by Navi, on a Mac.", "scratch that removes the last insert");
        let clipboard = mac_clipboard();
        script(r#"tell application "TextEdit" to close front document saving no"#);
        println!("{pasted:?} {typed:?}; the document: {field:?}; the clipboard: {clipboard:?}");
        assert_eq!(pasted, InsertOutcome::Pasted);
        assert_eq!(typed, InsertOutcome::Typed);
        assert_eq!(field, "Pasted by Navi, on a Mac. Typed too: café ✓.");
        assert_eq!(clipboard.as_deref(), Some("what you had copied"), "the clipboard is put back");
    }

    #[cfg(target_os = "macos")]
    fn mac_clipboard() -> Option<String> {
        let out = std::process::Command::new("/usr/bin/pbpaste").output().ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }

}
