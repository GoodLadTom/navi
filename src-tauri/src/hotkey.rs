//! Global hotkeys through a low-level keyboard hook (WH_KEYBOARD_LL) on its own thread.
//!
//! OWNER: Windows systems agent. STATUS: implemented.
//!
//! Contract (the controller relies on all of it):
//! * The talk chord is a set of logical keys (see types::HotkeySettings). TalkDown fires once when
//!   every key of the chord is down (key repeat must not re-fire it); TalkUp fires once when any of
//!   them is released after that.
//! * While the chord is held, the ramble key (default Space) fires `Ramble` and is SWALLOWED.
//! * While `set_recording(true)`, the cancel key (default Escape) fires `Cancel` and is SWALLOWED.
//! * While a hands-free, ramble or wake take records with Enter allowed to finish it
//!   (`set_enter_finishes(true)`, from Settings, off by default), Enter fires `Finish` and is
//!   SWALLOWED, whether or not the talk keys are still held. Otherwise Enter passes untouched and a
//!   tap of the shortcut is the way to finish.
//! * Chords that include Win must not open the Start menu when released: inject a harmless key
//!   (VK 0xE8, down then up) before letting the Win key-up through, the way AutoHotkey does.
//!   Navi's own keystrokes (dwExtraInfo == NAVI_INPUT_TAG) are ignored by the hook. Keys other
//!   software injects (a keyboard app's macro key set to F13, the on-screen keyboard) can press
//!   the shortcut, but a stray injected key never cancels one that is held.
//! * The hook callback must return quickly (Windows silently removes a hook that keeps it waiting
//!   ~300 ms): it waits at most LOCK_WAIT for the shared state and lets the key through if it is
//!   busy; send events over the channel and return.
//! * Self-repair: when the hook has seen no key for a minute, it is reinstalled, so a hook Windows
//!   removed comes back without restarting Navi. A press of the shortcut made by software (a macro
//!   key sending F13 sends no repeats) survives that; any other held state is dropped.
//! * `set_monitor` streams every key the hook sees to the live shortcut tester in the interface.
//! * `keys_pressed_since(t)` says whether a key that reached an app (not swallowed, not a lone
//!   modifier, not Navi's own) went down since `t`, for spoken edits in a field Navi can't read.
//! * `pause(true)` stops the shortcut and mode keys firing (the tray's "Pause the shortcut", and
//!   while the user records a new shortcut in Settings). A take already recording can still be
//!   cancelled with the cancel key, or finished with Enter if it is hands-free.
//! * `capture_next_chord()` resolves with the chord the user presses next (all keys down, then
//!   released), swallowing those keys, so Settings can record a new shortcut.
//!
//! How it behaves in detail (all of it lives in `HookMachine`, a pure state machine the hook feeds):
//! * The chord fires only when no other key is held, so Ctrl + Shift + Win is not Ctrl + Win.
//! * Any other key pressed while the chord is held means the user is doing a different shortcut
//!   (Ctrl + Win + Left switches desktops): that key passes through and `Cancel` is sent once so the
//!   controller drops the take. Until the chord is released every key then passes untouched
//!   (no Ramble), and `TalkUp` still follows on release.
//! * The cancel key also cancels while the chord is held, even before `set_recording(true)`.
//! * Non-modifier chord keys (F9, CapsLock...) are swallowed while they complete or belong to the
//!   held chord, so CapsLock does not toggle and F9 never reaches the app. Modifiers always pass.
//! * The mask key is injected at TalkDown (right after the key-down that completed the chord), which
//!   puts it between the Win/Alt key-down and its later key-up without ever holding back a real
//!   key-up. Chords with two or more modifiers get it too (Ctrl + Shift would switch keyboard layout).
//! * A key-up is swallowed exactly when its key-down was, so Windows never ends up with a stuck key.
//! * Keys whose release the hook never saw (lock screen, UAC prompt) are spotted with
//!   GetAsyncKeyState and forgotten, so a stale Win cannot turn the next Ctrl press into a take.
//!   If the whole chord was let go unseen mid-take, the missing TalkUp is sent on the next key
//!   press (before any TalkDown that press causes). Polling `chord_is_down()` while recording
//!   catches it sooner.

use crate::types::HotkeyEvent;
use anyhow::{anyhow, Result};
use std::time::Instant;
use tokio::sync::{mpsc::UnboundedSender, oneshot};

/// A hotkey event and when the hook saw it, so the controller times taps from the keyboard, not
/// from whenever it got round to the event.
pub type HotkeyMsg = (HotkeyEvent, Instant);

// Virtual-key codes the pure logic needs (local, so the logic compiles and tests on any platform).
const VK_CANCEL: u8 = 0x03;
const VK_SHIFT: u8 = 0x10;
const VK_CONTROL: u8 = 0x11;
const VK_MENU: u8 = 0x12;
const VK_PAUSE: u8 = 0x13;
const VK_LWIN: u8 = 0x5B;
const VK_RWIN: u8 = 0x5C;
const VK_LSHIFT: u8 = 0xA0;
const VK_RSHIFT: u8 = 0xA1;
const VK_LCONTROL: u8 = 0xA2;
const VK_RCONTROL: u8 = 0xA3;
const VK_LMENU: u8 = 0xA4;
const VK_RMENU: u8 = 0xA5;
/// An unassigned virtual key, injected so a released Win or Alt key is not seen as a lone tap.
pub const MASK_VK: u8 = 0xE8;
const VK_ESCAPE: u8 = 0x1B;
const VK_CAPITAL: u8 = 0x14;
/// The Mac's Fn (globe) key. Windows has no virtual key for it, so an unassigned code stands in;
/// nothing on Windows ever sends it.
const VK_FN: u8 = 0x07;

/// A key event as the hook saw it, for the live shortcut tester.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeySeen {
    pub vk: u8,
    pub up: bool,
    /// Sent by software (a keyboard app's macro key, the on-screen keyboard), not typed.
    pub injected: bool,
}

// ---------------------------------------------------------------------------------------------
// Logical keys
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Modifier {
    Ctrl,
    Shift,
    Alt,
    Win,
}

impl Modifier {
    /// The order chords are written in.
    pub const ALL: [Modifier; 4] = [Modifier::Ctrl, Modifier::Shift, Modifier::Alt, Modifier::Win];

    /// (left, right) virtual-key codes.
    pub fn sided_vks(self) -> (u8, u8) {
        match self {
            Modifier::Ctrl => (VK_LCONTROL, VK_RCONTROL),
            Modifier::Shift => (VK_LSHIFT, VK_RSHIFT),
            Modifier::Alt => (VK_LMENU, VK_RMENU),
            Modifier::Win => (VK_LWIN, VK_RWIN),
        }
    }

    /// The side-less code some sources report (there is none for Win).
    pub fn generic_vk(self) -> Option<u8> {
        match self {
            Modifier::Ctrl => Some(VK_CONTROL),
            Modifier::Shift => Some(VK_SHIFT),
            Modifier::Alt => Some(VK_MENU),
            Modifier::Win => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Modifier::Ctrl => "Ctrl",
            Modifier::Shift => "Shift",
            Modifier::Alt => "Alt",
            Modifier::Win => "Win",
        }
    }

    /// The modifier a virtual key belongs to, either side.
    pub fn of_vk(vk: u8) -> Option<Modifier> {
        match vk {
            VK_CONTROL | VK_LCONTROL | VK_RCONTROL => Some(Modifier::Ctrl),
            VK_SHIFT | VK_LSHIFT | VK_RSHIFT => Some(Modifier::Shift),
            VK_MENU | VK_LMENU | VK_RMENU => Some(Modifier::Alt),
            VK_LWIN | VK_RWIN => Some(Modifier::Win),
            _ => None,
        }
    }
}

/// One key of a chord.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// Either side of a modifier: "Ctrl", "Shift", "Alt", "Win".
    Mod(Modifier),
    /// Exactly one virtual key: "LCtrl", "F9", "A".
    Vk(u8),
}

impl Key {
    pub fn matches(self, vk: u8) -> bool {
        match self {
            Key::Mod(m) => Modifier::of_vk(vk) == Some(m),
            Key::Vk(k) => k == vk,
        }
    }

    pub fn modifier(self) -> Option<Modifier> {
        match self {
            Key::Mod(m) => Some(m),
            Key::Vk(vk) => Modifier::of_vk(vk),
        }
    }

    pub fn is_modifier(self) -> bool {
        self.modifier().is_some()
    }

    /// Canonical settings name.
    pub fn name(self) -> String {
        match self {
            Key::Mod(m) => m.name().to_string(),
            Key::Vk(vk) => vk_name(vk),
        }
    }
}

/// Canonical names, as stored in settings.
const NAMED_KEYS: &[(&str, u8)] = &[
    ("Fn", VK_FN),
    ("Backspace", 0x08),
    ("Tab", 0x09),
    ("Enter", 0x0D),
    ("Pause", 0x13),
    ("CapsLock", 0x14),
    ("Escape", 0x1B),
    ("Space", 0x20),
    ("PageUp", 0x21),
    ("PageDown", 0x22),
    ("End", 0x23),
    ("Home", 0x24),
    ("Left", 0x25),
    ("Up", 0x26),
    ("Right", 0x27),
    ("Down", 0x28),
    ("PrintScreen", 0x2C),
    ("Insert", 0x2D),
    ("Delete", 0x2E),
    ("LWin", 0x5B),
    ("RWin", 0x5C),
    ("Menu", 0x5D),
    ("NumpadMultiply", 0x6A),
    ("NumpadAdd", 0x6B),
    ("NumpadSubtract", 0x6D),
    ("NumpadDecimal", 0x6E),
    ("NumpadDivide", 0x6F),
    ("NumLock", 0x90),
    ("ScrollLock", 0x91),
    ("LShift", 0xA0),
    ("RShift", 0xA1),
    ("LCtrl", 0xA2),
    ("RCtrl", 0xA3),
    ("LAlt", 0xA4),
    ("RAlt", 0xA5),
    ("VolumeMute", 0xAD),
    ("VolumeDown", 0xAE),
    ("VolumeUp", 0xAF),
    ("MediaNext", 0xB0),
    ("MediaPrevious", 0xB1),
    ("MediaStop", 0xB2),
    ("MediaPlayPause", 0xB3),
    ("Semicolon", 0xBA),
    ("Equal", 0xBB),
    ("Comma", 0xBC),
    ("Minus", 0xBD),
    ("Period", 0xBE),
    ("Slash", 0xBF),
    ("Backquote", 0xC0),
    ("BracketLeft", 0xDB),
    ("Backslash", 0xDC),
    ("BracketRight", 0xDD),
    ("Quote", 0xDE),
    // The ` key on a UK keyboard (top left); other layouts put other symbols here.
    ("Oem8", 0xDF),
    ("IntlBackslash", 0xE2),
];

/// Other spellings (including the browser's KeyboardEvent.code names), lower case with spaces,
/// hyphens and underscores removed, mapped to a canonical name above.
const ALIASES: &[(&str, &str)] = &[
    ("esc", "Escape"),
    ("return", "Enter"),
    ("numpadenter", "Enter"),
    ("del", "Delete"),
    ("ins", "Insert"),
    ("pgup", "PageUp"),
    ("pgdn", "PageDown"),
    ("pgdown", "PageDown"),
    ("caps", "CapsLock"),
    ("scroll", "ScrollLock"),
    ("scrlk", "ScrollLock"),
    ("break", "Pause"),
    ("pausebreak", "Pause"),
    ("prtsc", "PrintScreen"),
    ("prtscn", "PrintScreen"),
    ("print", "PrintScreen"),
    ("snapshot", "PrintScreen"),
    ("apps", "Menu"),
    ("contextmenu", "Menu"),
    ("application", "Menu"),
    ("spacebar", "Space"),
    ("bksp", "Backspace"),
    ("arrowleft", "Left"),
    ("arrowright", "Right"),
    ("arrowup", "Up"),
    ("arrowdown", "Down"),
    ("leftarrow", "Left"),
    ("rightarrow", "Right"),
    ("uparrow", "Up"),
    ("downarrow", "Down"),
    ("grave", "Backquote"),
    ("backtick", "Backquote"),
    ("tilde", "Backquote"),
    ("hyphen", "Minus"),
    ("dash", "Minus"),
    ("equals", "Equal"),
    ("leftbracket", "BracketLeft"),
    ("rightbracket", "BracketRight"),
    ("apostrophe", "Quote"),
    ("dot", "Period"),
    ("fullstop", "Period"),
    ("forwardslash", "Slash"),
    ("lcontrol", "LCtrl"),
    ("leftctrl", "LCtrl"),
    ("leftcontrol", "LCtrl"),
    ("controlleft", "LCtrl"),
    ("ctrlleft", "LCtrl"),
    ("rcontrol", "RCtrl"),
    ("rightctrl", "RCtrl"),
    ("rightcontrol", "RCtrl"),
    ("controlright", "RCtrl"),
    ("ctrlright", "RCtrl"),
    ("leftshift", "LShift"),
    ("shiftleft", "LShift"),
    ("rightshift", "RShift"),
    ("shiftright", "RShift"),
    ("leftalt", "LAlt"),
    ("altleft", "LAlt"),
    ("lmenu", "LAlt"),
    ("rightalt", "RAlt"),
    ("altright", "RAlt"),
    ("rmenu", "RAlt"),
    ("altgr", "RAlt"),
    ("leftwin", "LWin"),
    ("winleft", "LWin"),
    ("metaleft", "LWin"),
    ("osleft", "LWin"),
    ("leftwindows", "LWin"),
    ("rightwin", "RWin"),
    ("winright", "RWin"),
    ("metaright", "RWin"),
    ("osright", "RWin"),
    ("rightwindows", "RWin"),
    ("playpause", "MediaPlayPause"),
    ("mediaplay", "MediaPlayPause"),
    ("mute", "VolumeMute"),
    ("globe", "Fn"),
    ("function", "Fn"),
    ("leftoption", "LAlt"),
    ("rightoption", "RAlt"),
    ("loption", "LAlt"),
    ("roption", "RAlt"),
    ("leftcommand", "LWin"),
    ("rightcommand", "RWin"),
    ("lcommand", "LWin"),
    ("rcommand", "RWin"),
    ("lcmd", "LWin"),
    ("rcmd", "RWin"),
];

/// Single characters that name a key.
const SYMBOLS: &[(char, &str)] = &[
    ('`', "Backquote"),
    ('-', "Minus"),
    ('=', "Equal"),
    ('[', "BracketLeft"),
    (']', "BracketRight"),
    ('\\', "Backslash"),
    (';', "Semicolon"),
    ('\'', "Quote"),
    (',', "Comma"),
    ('.', "Period"),
    ('/', "Slash"),
];

fn named(canonical: &str) -> Option<Key> {
    NAMED_KEYS.iter().find(|(n, _)| *n == canonical).map(|(_, vk)| Key::Vk(*vk))
}

/// Parses one logical key name. Case, spaces, hyphens and underscores do not matter.
pub fn parse_key(name: &str) -> Option<Key> {
    let trimmed = name.trim();
    let mut chars = trimmed.chars();
    let first = chars.next()?;
    if chars.next().is_none() {
        if first.is_ascii_alphanumeric() {
            return Some(Key::Vk(first.to_ascii_uppercase() as u8));
        }
        return SYMBOLS.iter().find(|(c, _)| *c == first).and_then(|(_, n)| named(n));
    }

    let norm: String = trimmed
        .chars()
        .filter(|c| !matches!(c, ' ' | '_' | '-'))
        .collect::<String>()
        .to_ascii_lowercase();
    match norm.as_str() {
        "ctrl" | "control" => return Some(Key::Mod(Modifier::Ctrl)),
        "shift" => return Some(Key::Mod(Modifier::Shift)),
        "alt" | "option" => return Some(Key::Mod(Modifier::Alt)),
        "win" | "windows" | "super" | "meta" | "cmd" | "command" | "os" => {
            return Some(Key::Mod(Modifier::Win))
        }
        _ => {}
    }
    if let Some((_, vk)) = NAMED_KEYS.iter().find(|(n, _)| n.eq_ignore_ascii_case(&norm)) {
        return Some(Key::Vk(*vk));
    }
    if let Some((_, canonical)) = ALIASES.iter().find(|(a, _)| *a == norm) {
        return named(canonical);
    }
    if let Some(n) = norm.strip_prefix('f').and_then(|d| d.parse::<u8>().ok()) {
        return (1..=24).contains(&n).then(|| Key::Vk(0x70 + n - 1));
    }
    if let Some(d) = norm.strip_prefix("numpad").or_else(|| norm.strip_prefix("num")) {
        if let Ok(n) = d.parse::<u8>() {
            return (n <= 9).then(|| Key::Vk(0x60 + n));
        }
    }
    let single = |rest: &str, ok: fn(char) -> bool| {
        let mut it = rest.chars();
        match (it.next(), it.next()) {
            (Some(c), None) if ok(c) => Some(Key::Vk(c.to_ascii_uppercase() as u8)),
            _ => None,
        }
    };
    if let Some(rest) = norm.strip_prefix("key") {
        return single(rest, |c| c.is_ascii_lowercase());
    }
    if let Some(rest) = norm.strip_prefix("digit") {
        return single(rest, |c| c.is_ascii_digit());
    }
    if let Some(hex) = norm.strip_prefix("vk") {
        let hex = hex.strip_prefix("0x").unwrap_or(hex);
        if let Ok(vk) = u8::from_str_radix(hex, 16) {
            return (vk != 0 && vk != 0xFF).then_some(Key::Vk(vk));
        }
    }
    None
}

/// Canonical name for a virtual key; unknown codes become "VKxx" (which `parse_key` reads back).
pub fn vk_name(vk: u8) -> String {
    match vk {
        b'0'..=b'9' | b'A'..=b'Z' => (vk as char).to_string(),
        0x60..=0x69 => format!("Numpad{}", vk - 0x60),
        0x70..=0x87 => format!("F{}", vk - 0x70 + 1),
        VK_CONTROL => "Ctrl".into(),
        VK_SHIFT => "Shift".into(),
        VK_MENU => "Alt".into(),
        _ => NAMED_KEYS
            .iter()
            .find(|(_, v)| *v == vk)
            .map(|(n, _)| n.to_string())
            .unwrap_or_else(|| format!("VK{vk:02X}")),
    }
}

/// Parses a chord; the error is a sentence the Settings screen can show.
pub fn parse_chord(names: &[String]) -> std::result::Result<Vec<Key>, String> {
    let mut keys = Vec::new();
    for name in names.iter().map(|n| n.trim()).filter(|n| !n.is_empty()) {
        let key = parse_key(name).ok_or_else(|| format!("Navi doesn't recognise the key \"{name}\" in that shortcut."))?;
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    if keys.is_empty() {
        return Err("A shortcut needs at least one key.".into());
    }
    Ok(keys)
}

/// What Settings shows about a talk shortcut before saving it.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChordCheck {
    /// "Right Ctrl", "Ctrl + Win".
    pub label: String,
    /// One per key, for the keycaps.
    pub labels: Vec<String>,
    /// It cannot be used; nothing is saved.
    pub error: Option<String>,
    /// It works, with a catch worth knowing.
    pub warning: Option<String>,
}

/// `check_chord` with the layout-independent labels (used by the tests and when saving).
pub fn check_chord(names: &[String], ramble_key: &str) -> ChordCheck {
    check_chord_with(names, ramble_key, &key_label)
}

/// Checks a talk shortcut, naming keys with `label`. Errors: it does not parse; it is one key that
/// would break normal typing (a letter, digit, Space, Enter...), a modifier that matches either
/// side, or Esc; it is a shortcut Windows or most apps already use (Ctrl + V, Alt + Tab,
/// Win + E, Shift + A); or it is two typing keys with no Ctrl, Alt or Win (J + K), which fast
/// typing would press. Warnings: one key that types a symbol (it can no longer be typed), a left-hand
/// modifier or Shift on its own (part of everyday shortcuts and capitals), Caps Lock, or a
/// shortcut that swallows the ramble key.
pub fn check_chord_with(names: &[String], ramble_key: &str, label: &dyn Fn(&str) -> String) -> ChordCheck {
    let cleaned: Vec<&String> = names.iter().filter(|n| !n.trim().is_empty()).collect();
    let labels: Vec<String> = cleaned.iter().map(|n| label(n)).collect();
    let chord_text = labels.join(" + ");
    let keys = match parse_chord(names) {
        Ok(keys) => keys,
        Err(e) => return ChordCheck { label: chord_text, labels, error: Some(e), warning: None },
    };
    let mut check = ChordCheck { label: chord_text.clone(), labels, ..ChordCheck::default() };
    if let Some(what) = everyday_shortcut(&keys) {
        check.error = Some(for_this_mac(format!("{chord_text} is {what}, so Navi would take it over. Pick something else.")));
        return check;
    }
    // Two keys that type, with no Ctrl, Alt or Win: typing them quickly together ("jk") would
    // start a take and swallow the second key.
    let typing_keys = keys.iter().filter(|k| matches!(k, Key::Vk(vk) if types_text(*vk))).count();
    let command_key = keys.iter().any(|k| matches!(k.modifier(), Some(Modifier::Ctrl | Modifier::Alt | Modifier::Win)));
    if typing_keys >= 2 && !command_key {
        check.error = Some(for_this_mac(format!("{chord_text} would start Navi whenever you type those keys together. Add Ctrl or Alt, or pick something else.")));
        return check;
    }
    if let [key] = keys.as_slice() {
        match *key {
            Key::Mod(m) => {
                check.error = Some(format!(
                    "{} on its own is part of too many shortcuts. Pick one side, such as Right Ctrl, or add another key.",
                    label(m.name())
                ))
            }
            Key::Vk(VK_ESCAPE) => check.error = Some("Esc cancels a take, so it can't start one too. Pick another key.".into()),
            Key::Vk(vk) if essential_for_typing(vk) => {
                check.error = Some(format!(
                    "{} on its own would stop you typing it. Pick a key you never type with, such as Right Ctrl or Caps Lock.",
                    label(&vk_name(vk))
                ))
            }
            Key::Vk(vk) if types_text(vk) => {
                check.warning = Some(format!("You won't be able to type {} while Navi is running.", label(&vk_name(vk))))
            }
            Key::Vk(VK_LCONTROL) if cfg!(target_os = "macos") => {
                check.warning = Some("Left Control is part of shortcuts like Control + Tab, and moves the cursor with letters in text fields, so Navi may start listening when you don't mean it to. Right Option is a safer pick.".into())
            }
            Key::Vk(VK_LMENU) if cfg!(target_os = "macos") => {
                check.warning = Some("Left Option types accents and symbols with other keys, so Navi may start listening when you don't mean it to. Right Option is a safer pick.".into())
            }
            Key::Vk(VK_LCONTROL) => {
                check.warning = Some("Left Ctrl is part of everyday shortcuts like Ctrl + C, so Navi may start listening when you don't mean it to. Right Ctrl is a safer pick.".into())
            }
            Key::Vk(VK_LMENU) => {
                check.warning = Some("Left Alt is part of everyday shortcuts like Alt + Tab, so Navi may start listening when you don't mean it to. Right Alt is a safer pick.".into())
            }
            Key::Vk(VK_LWIN) => {
                check.warning = Some("The Windows key starts lots of shortcuts, so Navi may start listening when you don't mean it to. Right Ctrl is a safer pick.".into())
            }
            Key::Vk(VK_LSHIFT | VK_RSHIFT) => {
                check.warning = Some("Shift is used for capital letters, so Navi may start listening while you type. Right Ctrl or Caps Lock is a safer pick.".into())
            }
            Key::Vk(VK_CAPITAL) if cfg!(target_os = "macos") => {
                check.error = Some("A Mac only tells apps when Caps Lock switches on or off, so it can't be held to talk. Try Right Option or Fn.".into())
            }
            // Navi swallows it, so it no longer toggles capitals; with Shift held it passes through.
            Key::Vk(VK_CAPITAL) => check.warning = Some("Caps Lock won't switch capitals on by itself any more. Shift + Caps Lock still does.".into()),
            Key::Vk(_) => {}
        }
    }
    check.error = check.error.map(for_this_mac);
    check.warning = check.warning.map(for_this_mac);
    if check.error.is_none() && check.warning.is_none() {
        if let Some(ramble) = parse_optional_key(ramble_key, "ramble") {
            if keys.iter().any(|k| keys_overlap(*k, ramble)) {
                check.warning = Some(format!(
                    "{} is part of this shortcut, so it can't switch a take to ramble. Choose another ramble key.",
                    label(ramble_key)
                ));
            }
        }
    }
    check
}

/// What a shortcut already does, if Windows or most apps use it: Navi swallows the last key of its
/// shortcut, so taking one of these would break it everywhere.
fn everyday_shortcut(keys: &[Key]) -> Option<&'static str> {
    let mods: Vec<Modifier> = keys.iter().filter_map(|k| k.modifier()).collect();
    let others: Vec<u8> = keys
        .iter()
        .filter_map(|k| match *k {
            Key::Vk(vk) if Modifier::of_vk(vk).is_none() => Some(vk),
            _ => None,
        })
        .collect();
    let [key] = others.as_slice() else { return None };
    let key = *key;
    let only = |set: &[Modifier]| mods.len() == set.len() && set.iter().all(|m| mods.contains(m));
    let letter_or_digit = matches!(key, 0x30..=0x39 | 0x41..=0x5A);
    // Control + Space switches the input source on a Mac.
    if cfg!(target_os = "macos") && only(&[Modifier::Ctrl]) && key == 0x20 {
        return Some("a Mac shortcut");
    }
    // A Mac's copy, paste and the rest are Command + a letter, and Control + a letter moves the
    // cursor in text fields.
    let copy_family = if cfg!(target_os = "macos") { Modifier::Win } else { Modifier::Ctrl };
    if cfg!(target_os = "macos") && only(&[Modifier::Ctrl]) && (letter_or_digit || key == 0x09) {
        return Some("an everyday shortcut in many apps");
    }
    // (Command + Space and the like go on to the Mac's own shortcuts below.)
    if only(&[copy_family]) && (!cfg!(target_os = "macos") || letter_or_digit || key == 0x09) {
        return Some(match key {
            b'C' => "Copy in most apps",
            b'V' => "Paste in most apps",
            b'X' => "Cut in most apps",
            b'Z' => "Undo in most apps",
            b'Y' => "Redo in most apps",
            b'A' => "Select all in most apps",
            b'S' => "Save in most apps",
            b'F' => "Find in most apps",
            b'P' => "Print in most apps",
            b'W' => "Close in most apps",
            b'T' => "New tab in most apps",
            0x09 => "Switch tab in most apps",
            _ if letter_or_digit => "an everyday shortcut in many apps",
            _ => return None,
        });
    }
    if only(&[Modifier::Shift]) && types_text(key) {
        // Shift + A types a capital; Shift + Enter is a new line in chat apps.
        return Some(if letter_or_digit || matches!(key, 0xBA..=0xC0 | 0xDB..=0xDF | 0xE2) { "how you type a capital or symbol" } else { "an everyday shortcut in many apps" });
    }
    if only(&[Modifier::Alt]) && matches!(key, 0x09 | 0x1B | 0x20 | 0x73) {
        return Some("a Windows shortcut"); // Alt + Tab, Alt + Esc, Alt + Space, Alt + F4
    }
    if mods.contains(&Modifier::Win) && (letter_or_digit || matches!(key, 0x09 | 0x0D | 0x20 | 0x25..=0x28)) {
        return Some("a Windows shortcut");
    }
    None
}

/// `key_label`, except that a symbol key shows the character it types on this computer's keyboard
/// layout (a UK keyboard's top-left key is VK 0xDF and types `), so no key is ever shown as "VKDF".
pub fn display_label(name: &str) -> String {
    if let Some(Key::Vk(vk)) = parse_key(name) {
        if matches!(vk, 0xBA..=0xC0 | 0xDB..=0xDF | 0xE1..=0xE4) {
            if let Some(c) = layout_char(vk) {
                return c.to_string();
            }
        }
    }
    key_label(name)
}

/// The character a key types (unshifted) with the current keyboard layout.
fn layout_char(vk: u8) -> Option<char> {
    #[cfg(windows)]
    {
        use windows::Win32::UI::Input::KeyboardAndMouse::{MapVirtualKeyW, MAPVK_VK_TO_CHAR};
        let value = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_CHAR) };
        // The low word is the character; the top bit marks a dead key (still a fine label).
        let c = char::from_u32(value & 0xFFFF)?;
        (c != '\0' && !c.is_control() && !c.is_whitespace()).then_some(c)
    }
    #[cfg(target_os = "macos")]
    {
        mac::layout_char(vk)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = vk;
        None
    }
}

/// Reads the keyboard layout's symbols for `display_label` (macOS: call on the main thread, at
/// start-up; the layout API may only be used there). A no-op elsewhere.
pub fn refresh_layout() {
    #[cfg(target_os = "macos")]
    mac::refresh_layout();
}

/// For saving a talk chord: `check_chord` without the warnings. (`start` and `set_chord` only
/// need it to parse.)
pub fn validate_chord(names: &[String]) -> std::result::Result<(), String> {
    match check_chord(names, "").error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Keys that produce text or edit it when pressed alone.
fn types_text(vk: u8) -> bool {
    matches!(vk, 0x08 | 0x09 | 0x0D | 0x20 | 0x2E | 0x30..=0x39 | 0x41..=0x5A | 0x60..=0x6F | 0xBA..=0xC0 | 0xDB..=0xDF | 0xE2)
}

/// Keys nobody can do without while typing: letters, digits, Space, Enter, Tab, Backspace, Delete.
fn essential_for_typing(vk: u8) -> bool {
    matches!(vk, 0x08 | 0x09 | 0x0D | 0x20 | 0x2E | 0x30..=0x39 | 0x41..=0x5A)
}

fn parse_optional_key(name: &str, what: &str) -> Option<Key> {
    if name.trim().is_empty() {
        return None;
    }
    let key = parse_key(name);
    if key.is_none() {
        log::warn!("Ignoring the {what} key {name:?}: not a key name Navi knows");
    }
    key
}

/// `chord_label` in this computer's keyboard layout, for anything the user reads.
pub fn display_chord_label(chord: &[String]) -> String {
    chord.iter().filter(|n| !n.trim().is_empty()).map(|n| display_label(n)).collect::<Vec<_>>().join(" + ")
}

/// "Ctrl + Win" style label (layout-independent; see `display_chord_label`).
pub fn chord_label(chord: &[String]) -> String {
    chord
        .iter()
        .filter(|n| !n.trim().is_empty())
        .map(|n| key_label(n))
        .collect::<Vec<_>>()
        .join(" + ")
}

/// Friendly label for one key name ("PageUp" -> "Page Up", "RCtrl" -> "Right Ctrl"). On a Mac
/// the keys take their Mac names ("RCtrl" -> "Right Control", "Win" -> "Command").
pub fn key_label(name: &str) -> String {
    let label = windows_key_label(name);
    if cfg!(target_os = "macos") {
        mac_key_label(&label)
    } else {
        label
    }
}

/// A key label in the words a Mac keyboard uses.
pub fn mac_key_label(label: &str) -> String {
    match label {
        "Ctrl" => "Control".into(),
        "Alt" => "Option".into(),
        "Win" => "Command".into(),
        "Left Ctrl" => "Left Control".into(),
        "Right Ctrl" => "Right Control".into(),
        "Left Alt" => "Left Option".into(),
        "Right Alt" => "Right Option".into(),
        "Left Win" => "Left Command".into(),
        "Right Win" => "Right Command".into(),
        "Enter" => "Return".into(),
        "Backspace" => "Delete".into(),
        "Delete" => "Forward Delete".into(),
        "Insert" => "Help".into(),
        other => other.to_string(),
    }
}

/// The sentence-level Windows words in a message, in Mac words (for the chord checks below).
fn for_this_mac(text: String) -> String {
    if !cfg!(target_os = "macos") {
        return text;
    }
    text.replace("Right Ctrl or Caps Lock", "Right Option or Fn")
        .replace("Right Ctrl", "Right Option")
        .replace("Right Alt", "Right Option")
        .replace("Left Ctrl", "Left Control")
        .replace("Left Alt", "Left Option")
        .replace("Add Ctrl or Alt", "Add Control, Option or Command")
        .replace("The Windows key", "Command")
        .replace("a Windows shortcut", "a Mac shortcut")
}

fn windows_key_label(name: &str) -> String {
    let Some(key) = parse_key(name) else {
        return name.trim().to_string();
    };
    let vk = match key {
        Key::Mod(m) => return m.name().to_string(),
        Key::Vk(vk) => vk,
    };
    let label = match vk {
        VK_LCONTROL => "Left Ctrl",
        VK_RCONTROL => "Right Ctrl",
        VK_LSHIFT => "Left Shift",
        VK_RSHIFT => "Right Shift",
        VK_LMENU => "Left Alt",
        VK_RMENU => "Right Alt",
        VK_LWIN => "Left Win",
        VK_RWIN => "Right Win",
        0x1B => "Esc",
        0x14 => "Caps Lock",
        0x91 => "Scroll Lock",
        0x90 => "Num Lock",
        0x2C => "Print Screen",
        0x21 => "Page Up",
        0x22 => "Page Down",
        0x25 => "Left Arrow",
        0x26 => "Up Arrow",
        0x27 => "Right Arrow",
        0x28 => "Down Arrow",
        0x6A => "Num *",
        0x6B => "Num +",
        0x6D => "Num -",
        0x6E => "Num .",
        0x6F => "Num /",
        0xBA => ";",
        0xBB => "=",
        0xBC => ",",
        0xBD => "-",
        0xBE => ".",
        0xBF => "/",
        0xC0 => "`",
        0xDB => "[",
        0xDC => "\\",
        0xDD => "]",
        0xDE => "'",
        0xE2 => "Intl \\",
        0xAD => "Mute",
        0xAE => "Volume Down",
        0xAF => "Volume Up",
        0xB0 => "Next Track",
        0xB1 => "Previous Track",
        0xB2 => "Stop",
        0xB3 => "Play/Pause",
        0x60..=0x69 => return format!("Num {}", vk - 0x60),
        _ => return vk_name(vk),
    };
    label.to_string()
}

// ---------------------------------------------------------------------------------------------
// The state machine (pure; the hook glue feeds it)
// ---------------------------------------------------------------------------------------------

/// What the hook should do with one key event.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Decision {
    /// Return 1 from the hook so no app sees the event.
    pub swallow: bool,
    /// Send TalkUp before `event`: the chord was released where the hook could not see it.
    pub talk_up_first: bool,
    pub event: Option<HotkeyEvent>,
    /// Inject MASK_VK down + up once this event has passed.
    pub inject_mask: bool,
    /// A capture finished with this chord.
    pub captured: Option<Vec<String>>,
}

struct Capture {
    /// Keys pressed during the capture, in order, without repeats.
    order: Vec<u8>,
    held: [bool; 256],
}

pub struct HookMachine {
    chord: Vec<Key>,
    ramble: Option<Key>,
    cancel: Option<Key>,
    /// Mode keys and then action keys, by index (an empty chord never fires, keeping the indices
    /// of the rest).
    extras: Vec<Vec<Key>>,
    down: [bool; 256],
    /// The key's current press was injected by software rather than typed.
    injected_down: [bool; 256],
    swallowed: [bool; 256],
    chord_active: bool,
    /// A foreign key already cancelled this press of the chord.
    chord_spent: bool,
    recording: bool,
    /// The take being recorded is hands-free or a ramble: Enter finishes it.
    enter_finishes: bool,
    paused: bool,
    capture: Option<Capture>,
    /// Trust the hook's own record of which keys are down when the system's says a key is up. On a
    /// Mac the event tap sees every press and release, but macOS's key state (CGEventSourceKeyState)
    /// can report a held key as up, which ended every hold-to-talk take a moment after it began
    /// while double taps (which never ask) worked. On Windows the system's state is kept as the
    /// judge, so a release the hook missed (lock screen, UAC prompt) is still caught.
    trust_own_view: bool,
}

const VK_RETURN: u8 = 0x0D;

fn keys_overlap(a: Key, b: Key) -> bool {
    (1..=254u8).any(|vk| a.matches(vk) && b.matches(vk))
}

/// Every key is Ctrl, Shift, Alt or Win (either side or one side).
fn modifiers_only(keys: &[Key]) -> bool {
    !keys.is_empty() && keys.iter().all(|k| k.is_modifier())
}

/// Chords with Win or Alt, or two or more modifiers, need the mask key so releasing them does not
/// open the Start menu, focus a menu bar or switch keyboard layout.
fn chord_needs_mask(chord: &[Key]) -> bool {
    let mut modifiers = 0;
    let mut win_or_alt = false;
    for m in chord.iter().filter_map(|k| k.modifier()) {
        modifiers += 1;
        win_or_alt |= matches!(m, Modifier::Win | Modifier::Alt);
    }
    win_or_alt || modifiers >= 2
}

/// Checks a mode key before saving it: what `check_chord` checks, plus that it is not made only of
/// Ctrl, Shift, Alt or Win (a mode key fires on its key-down, so Right Ctrl would switch mode at
/// every Ctrl + C), shares no key with the talk shortcut (holding that to talk would turn the mode
/// key into "another shortcut") and is not already another mode's key.
pub fn check_mode_key(keys: &[String], talk: &[String], others: &[Vec<String>], label: &dyn Fn(&str) -> String) -> ChordCheck {
    let mut check = check_extra_key(keys, talk, "switch modes", label);
    if check.error.is_none() && others.iter().any(|o| same_chord(keys, o)) {
        check.error = Some(format!("{} already switches to another mode.", check.label));
    }
    check
}

/// Checks an action key before saving it: the same rules as a mode key, and that no other key of
/// Navi's is the same. `taken`: those keys, each with what it does ("Scratch that", "switching to
/// Email").
pub fn check_action_key(keys: &[String], talk: &[String], taken: &[(Vec<String>, String)], label: &dyn Fn(&str) -> String) -> ChordCheck {
    let mut check = check_extra_key(keys, talk, "go off", label);
    if check.error.is_none() {
        if let Some((_, what)) = taken.iter().find(|(k, _)| same_chord(keys, k)) {
            check.error = Some(format!("{} is already your key for {what}.", check.label));
        }
    }
    check
}

/// What a mode key and an action key both have to pass: `check_chord`, not only Ctrl, Shift, Alt
/// or Win (it fires on its key-down, so Right Ctrl would `does` at every Ctrl + C), and no key of
/// the talk shortcut (holding that to talk would turn the key into "another shortcut").
fn check_extra_key(keys: &[String], talk: &[String], does: &str, label: &dyn Fn(&str) -> String) -> ChordCheck {
    let mut check = check_chord_with(keys, "", label);
    if mode_key_never_fires(keys) {
        check.error = Some(format!("{} is used with other keys all the time, so it would {does} while you work. Pick a key of its own, such as {}.", check.label, if cfg!(target_os = "macos") { "F13" } else { "F1" }));
        check.warning = None;
        return check;
    }
    if check.error.is_some() {
        return check;
    }
    let (Ok(mine), Ok(talk_keys)) = (parse_chord(keys), parse_chord(talk)) else { return check };
    if mine.iter().any(|k| talk_keys.iter().any(|t| keys_overlap(*k, *t))) {
        // Never suggest a key of the talk shortcut itself (a talk shortcut of F1).
        // A Mac's F1 to F12 are brightness and volume unless fn is held: suggest F13 to F19 there.
        let spares = if cfg!(target_os = "macos") { 0x7Cu8..=0x82 } else { 0x70u8..=0x7B };
        let spare = spares.clone().find(|vk| !talk_keys.iter().any(|t| t.matches(*vk))).map_or_else(|| vk_name(*spares.start()), vk_name);
        check.error = Some(format!("{} is part of your talk shortcut. Pick a key of its own, such as {spare}.", check.label));
    }
    check
}

/// The same keys, in any order. Keys that don't parse are never the same as anything.
fn same_chord(a: &[String], b: &[String]) -> bool {
    match (parse_chord(a), parse_chord(b)) {
        (Ok(a), Ok(b)) => a.len() == b.len() && b.iter().all(|k| a.contains(k)),
        _ => false,
    }
}

/// A mode key the hook never fires: one made only of Ctrl, Shift, Alt or Win. `check_mode_key`
/// refuses these, but settings saved before it did may still hold one.
pub fn mode_key_never_fires(keys: &[String]) -> bool {
    parse_chord(keys).is_ok_and(|mine| modifiers_only(&mine))
}

/// Removes the mode keys that never fire (see `mode_key_never_fires`), so Settings stops showing
/// a key that does nothing. Returns what to tell the user, once, or None if there were none.
pub fn drop_dead_mode_keys(mode_keys: &mut Vec<crate::types::ModeKey>) -> Option<String> {
    let dead: Vec<String> = mode_keys.iter().filter(|k| mode_key_never_fires(&k.keys)).map(|k| display_chord_label(&k.keys)).collect();
    let (last, rest) = dead.split_last()?;
    mode_keys.retain(|k| !mode_key_never_fires(&k.keys));
    Some(if rest.is_empty() {
        format!("{last} no longer switches modes, because it's used with other keys all the time. Pick another key in Settings.")
    } else {
        format!("{} and {last} no longer switch modes, because they're used with other keys all the time. Pick other keys in Settings.", rest.join(", "))
    })
}

impl HookMachine {
    pub fn new(chord: Vec<Key>, ramble: Option<Key>, cancel: Option<Key>) -> Self {
        let mut machine = Self {
            chord: Vec::new(),
            ramble: None,
            cancel: None,
            extras: Vec::new(),
            down: [false; 256],
            injected_down: [false; 256],
            swallowed: [false; 256],
            chord_active: false,
            chord_spent: false,
            recording: false,
            enter_finishes: false,
            paused: false,
            capture: None,
            // Tests model the system's key state explicitly, so they choose (`trusting_own_view`).
            trust_own_view: cfg!(target_os = "macos") && !cfg!(test),
        };
        machine.set_keys(chord, ramble, cancel);
        machine
    }

    /// Whether `vk` counts as down for the stale-key and still-held checks: the system's view
    /// (`probe`), or on a Mac also the hook's own (see `trust_own_view`).
    fn held_now(&self, vk: u8, probe: &dyn Fn(u8) -> bool) -> bool {
        probe(vk) || (self.trust_own_view && self.down[vk as usize])
    }

    #[cfg(test)]
    fn trusting_own_view(mut self) -> Self {
        self.trust_own_view = true;
        self
    }

    pub fn set_keys(&mut self, chord: Vec<Key>, ramble: Option<Key>, cancel: Option<Key>) {
        // A ramble or cancel key that is part of the chord could never fire; drop it.
        let clashes = |k: &Key| chord.iter().any(|c| keys_overlap(*c, *k));
        let ramble = ramble.filter(|k| !clashes(k));
        let cancel = cancel.filter(|k| !clashes(k));
        if chord != self.chord {
            self.chord_active = false;
            self.chord_spent = false;
        }
        self.chord = chord;
        self.ramble = ramble;
        self.cancel = cancel;
    }

    /// The mode keys and then the action keys (lib.rs extra_key_chords). One made only of
    /// modifiers (saved before `check_mode_key` refused them) never fires: it would switch mode on
    /// every Ctrl + C. `drop_dead_mode_keys` takes such keys out of the settings; this is the backstop.
    pub fn set_extras(&mut self, extras: Vec<Vec<Key>>) {
        self.extras = extras
            .into_iter()
            .map(|extra| {
                if modifiers_only(&extra) {
                    log::warn!("Ignoring a mode or action key made only of Ctrl, Shift, Alt or Win; pick another key in Settings");
                    Vec::new()
                } else {
                    extra
                }
            })
            .collect();
    }

    pub fn set_recording(&mut self, on: bool) {
        self.recording = on;
        if !on {
            self.enter_finishes = false;
        }
    }

    pub fn set_enter_finishes(&mut self, on: bool) {
        self.enter_finishes = on;
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        if paused {
            self.chord_active = false;
            self.chord_spent = false;
        }
    }

    pub fn start_capture(&mut self) {
        self.capture = Some(Capture { order: Vec::new(), held: [false; 256] });
        self.chord_active = false;
        self.chord_spent = false;
    }

    pub fn cancel_capture(&mut self) {
        self.capture = None;
    }

    pub fn is_capturing(&self) -> bool {
        self.capture.is_some()
    }

    pub fn chord_active(&self) -> bool {
        self.chord_active
    }

    fn in_chord(&self, vk: u8) -> bool {
        self.chord.iter().any(|k| k.matches(vk))
    }

    fn key_down(&self, key: Key) -> bool {
        match key {
            Key::Vk(vk) => self.down[vk as usize],
            Key::Mod(m) => {
                let (l, r) = m.sided_vks();
                self.down[l as usize] || self.down[r as usize] || m.generic_vk().is_some_and(|g| self.down[g as usize])
            }
        }
    }

    fn chord_held(&self) -> bool {
        !self.chord.is_empty() && self.chord.iter().all(|k| self.key_down(*k))
    }

    fn foreign_key_down(&self) -> bool {
        (1..=254u8).any(|vk| self.down[vk as usize] && !self.in_chord(vk))
    }

    fn needs_mask(&self) -> bool {
        chord_needs_mask(&self.chord)
    }

    /// The mode key completed by this press of `vk`: every key of it held, nothing else.
    fn extra_completed(&self, vk: u8) -> Option<usize> {
        self.extras.iter().position(|extra| {
            !extra.is_empty()
                && extra.iter().any(|k| k.matches(vk))
                && extra.iter().all(|k| self.key_down(*k))
                && !(1..=254u8).any(|other| self.down[other as usize] && !extra.iter().any(|k| k.matches(other)))
        })
    }

    /// Forgets keys we think are down but Windows says are up: their release happened where the
    /// hook could not see it (lock screen, UAC prompt, another hook swallowed it). Keys we
    /// swallowed never reach Windows' key state, so they are trusted as they are.
    fn forget_stale_keys(&mut self, except: u8, probe: &dyn Fn(u8) -> bool) {
        for vk in 1..=254u8 {
            let i = vk as usize;
            if vk != except && self.down[i] && !self.swallowed[i] && !self.held_now(vk, probe) {
                self.down[i] = false;
                self.injected_down[i] = false;
            }
        }
    }

    /// Drops everything the machine believes is held. Used after a long silence from the hook: a
    /// key really held down keeps sending repeats, so after a quiet minute nothing is, whatever the
    /// machine last saw (its release may have happened while Windows had removed the hook).
    /// Returns true if a press of the shortcut was in progress, which now needs a TalkUp.
    pub fn forget_held(&mut self) -> bool {
        let was_active = self.chord_active;
        self.down = [false; 256];
        self.injected_down = [false; 256];
        self.swallowed = [false; 256];
        self.chord_active = false;
        self.chord_spent = false;
        if let Some(cap) = self.capture.as_mut() {
            cap.held = [false; 256];
        }
        was_active
    }

    pub fn is_swallowed(&self, vk: u8) -> bool {
        self.swallowed[vk as usize]
    }

    /// The key's down reached Windows after all (it was let through while the hook's state was
    /// busy), so its up must reach Windows too.
    pub fn mark_passed(&mut self, vk: u8) {
        self.swallowed[vk as usize] = false;
    }

    /// Some key of the shortcut's current press was injected by software.
    fn chord_has_injected(&self) -> bool {
        (1..=254u8).any(|vk| {
            let i = vk as usize;
            self.down[i] && self.injected_down[i] && self.in_chord(vk)
        })
    }

    /// The shortcut is being held by software (a macro key or mouse button sending F13). Such a
    /// press sends no key repeats, so a quiet hook does not mean it was let go.
    pub fn held_by_software(&self) -> bool {
        self.chord_active && self.chord_has_injected()
    }

    /// Feeds one physical key event. `probe(vk)` says whether Windows thinks `vk` is down
    /// (GetAsyncKeyState); inside the hook it reflects the state before this event.
    pub fn on_key(&mut self, vk: u8, up: bool, probe: &dyn Fn(u8) -> bool) -> Decision {
        self.on_event(vk, up, false, probe)
    }

    /// `on_key` for any event. An `injected` one came from other software (a keyboard app's macro
    /// key, the on-screen keyboard, a mouse gesture, a script): it can press and release the
    /// shortcut, the ramble key and the cancel key, and a capture records it, so a macro key set to
    /// send F13 works as the shortcut. Any other injected key passes untouched and untracked, so a
    /// key some app injects in the background never cancels a shortcut you are holding; but when
    /// the shortcut itself came from software (a gesture sending Ctrl + Win + Right), its extra keys
    /// mean "another shortcut" just as typed ones do. An injected release of a key you are
    /// physically holding (a script restoring modifiers) is ignored.
    pub fn on_event(&mut self, vk: u8, up: bool, injected: bool, probe: &dyn Fn(u8) -> bool) -> Decision {
        let i = vk as usize;
        if injected {
            if up && self.down[i] && !self.injected_down[i] {
                return Decision::default();
            }
            if !self.cares_about(vk) {
                if !up && self.chord_active && !self.chord_spent && self.chord_has_injected() {
                    self.chord_spent = true;
                    return Decision { event: Some(HotkeyEvent::Cancel), ..Decision::default() };
                }
                return Decision::default();
            }
        }
        let mut d = Decision::default();

        if up {
            self.down[i] = false;
            self.injected_down[i] = false;
            d.swallow = std::mem::take(&mut self.swallowed[i]);
            let mut finished = false;
            if let Some(cap) = self.capture.as_mut() {
                if cap.held[i] {
                    cap.held[i] = false;
                    finished = !cap.held.iter().any(|h| *h);
                }
            }
            if finished {
                if let Some(cap) = self.capture.take() {
                    d.captured = Some(captured_chord_names(&cap.order));
                }
            }
            if self.chord_active && self.in_chord(vk) && !self.chord_held() {
                self.chord_active = false;
                self.chord_spent = false;
                d.event = Some(HotkeyEvent::TalkUp);
            }
            return d;
        }

        // A key-down for a key already down is auto-repeat, unless Windows says the key is up, in
        // which case its release went unseen and this is a fresh press.
        let repeat = self.down[i] && (self.swallowed[i] || self.held_now(vk, probe));
        if repeat {
            d.swallow = self.swallowed[i];
            return d;
        }
        // A fresh press: whatever we believed about this key until now was stale.
        self.down[i] = false;
        if self.chord_active {
            self.forget_stale_keys(vk, probe);
            if !self.chord_held() {
                // The chord was let go on the lock screen or a UAC prompt; end that press now.
                self.chord_active = false;
                self.chord_spent = false;
                d.talk_up_first = true;
            }
        }
        self.down[i] = true;
        self.injected_down[i] = injected;

        if let Some(cap) = self.capture.as_mut() {
            if !cap.order.contains(&vk) {
                cap.order.push(vk);
            }
            cap.held[i] = true;
            self.swallowed[i] = true;
            d.swallow = true;
            return d;
        }

        // Never true while paused: pausing ends the press and nothing starts one until unpaused.
        if self.chord_active {
            if self.chord_spent {
                // Another shortcut already claimed this press; everything passes until release.
                return d;
            }
            if self.recording && self.enter_finishes && vk == VK_RETURN {
                // Enter finishes a ramble or hands-free take even with the talk keys still held:
                // it is not another shortcut (and Ctrl + Win + Enter would start Narrator).
                self.swallowed[i] = true;
                d.swallow = true;
                d.event = Some(HotkeyEvent::Finish);
            } else if self.ramble.is_some_and(|k| k.matches(vk)) {
                self.swallowed[i] = true;
                d.swallow = true;
                d.event = Some(HotkeyEvent::Ramble);
            } else if self.cancel.is_some_and(|k| k.matches(vk)) {
                self.swallowed[i] = true;
                d.swallow = true;
                d.event = Some(HotkeyEvent::Cancel);
            } else if self.in_chord(vk) {
                // The other side of a chord modifier: still the chord.
            } else {
                self.chord_spent = true;
                d.event = Some(HotkeyEvent::Cancel);
            }
            return d;
        }

        if self.recording && self.cancel.is_some_and(|k| k.matches(vk)) {
            self.swallowed[i] = true;
            d.swallow = true;
            d.event = Some(HotkeyEvent::Cancel);
            return d;
        }

        if self.recording && self.enter_finishes && vk == VK_RETURN {
            self.swallowed[i] = true;
            d.swallow = true;
            d.event = Some(HotkeyEvent::Finish);
            return d;
        }

        // Paused, a take already under way can still be cancelled or finished (above), so it is
        // never left recording with no way out; nothing new starts and mode keys do nothing.
        if self.paused {
            return d;
        }

        if self.in_chord(vk) {
            self.forget_stale_keys(vk, probe);
            if self.chord_held() && !self.foreign_key_down() {
                self.chord_active = true;
                self.chord_spent = false;
                d.event = Some(HotkeyEvent::TalkDown);
                d.inject_mask = self.needs_mask();
                if Modifier::of_vk(vk).is_none() {
                    self.swallowed[i] = true;
                    d.swallow = true;
                }
                return d;
            }
        }

        if self.extras.iter().any(|extra| extra.iter().any(|k| k.matches(vk))) {
            self.forget_stale_keys(vk, probe);
            if let Some(n) = self.extra_completed(vk) {
                d.event = Some(HotkeyEvent::ExtraKey(n.min(u8::MAX as usize) as u8));
                d.inject_mask = chord_needs_mask(&self.extras[n]);
                if Modifier::of_vk(vk).is_none() {
                    self.swallowed[i] = true;
                    d.swallow = true;
                }
            }
        }
        d
    }

    fn cares_about(&self, vk: u8) -> bool {
        let i = vk as usize;
        self.capture.is_some()
            || self.down[i]
            || self.swallowed[i]
            || self.in_chord(vk)
            || self.ramble.is_some_and(|k| k.matches(vk))
            || self.cancel.is_some_and(|k| k.matches(vk))
            || (self.enter_finishes && vk == VK_RETURN)
            || self.extras.iter().any(|extra| extra.iter().any(|k| k.matches(vk)))
    }

    /// Nothing in progress and no key held (keys Windows says are up count as released).
    pub fn is_idle(&self, probe: &dyn Fn(u8) -> bool) -> bool {
        !self.chord_active
            && self.capture.is_none()
            && !(1..=254u8).any(|vk| {
                let i = vk as usize;
                self.swallowed[i] || (self.down[i] && self.held_now(vk, probe))
            })
    }

    /// True if every chord key is down: Windows' view for keys that reach it, the hook's own view
    /// for keys it swallowed (Windows never sees those).
    pub fn chord_is_down(&self, probe: &dyn Fn(u8) -> bool) -> bool {
        let down = |vk: u8| self.held_now(vk, probe) || (self.swallowed[vk as usize] && self.down[vk as usize]);
        !self.chord.is_empty()
            && self.chord.iter().all(|k| match *k {
                Key::Vk(vk) => down(vk),
                Key::Mod(m) => {
                    let (l, r) = m.sided_vks();
                    down(l) || down(r)
                }
            })
    }
}

/// Names for a captured chord: modifiers first (Ctrl, Shift, Alt, Win, either side), then the
/// other keys in the order pressed. A lone modifier keeps its side ("RCtrl"), as do both sides of
/// the same modifier pressed together.
pub fn captured_chord_names(order: &[u8]) -> Vec<String> {
    let lone = order.len() == 1;
    let mut out = Vec::new();
    for m in Modifier::ALL {
        let (l, r) = m.sided_vks();
        let left = order.contains(&l);
        let right = order.contains(&r);
        let generic = m.generic_vk().is_some_and(|g| order.contains(&g));
        match (left, right) {
            (true, true) => {
                out.push(vk_name(l));
                out.push(vk_name(r));
            }
            (true, false) if lone => out.push(vk_name(l)),
            (false, true) if lone => out.push(vk_name(r)),
            (true, false) | (false, true) => out.push(m.name().to_string()),
            (false, false) if generic => out.push(m.name().to_string()),
            (false, false) => {}
        }
    }
    out.extend(order.iter().filter(|vk| Modifier::of_vk(**vk).is_none()).map(|vk| vk_name(*vk)));
    out
}

// ---------------------------------------------------------------------------------------------
// Keys pressed since Navi typed (for spoken edits in a field Navi can't read)
// ---------------------------------------------------------------------------------------------

/// A key-down that reached the app in front and may have changed its text or moved the caret:
/// not one Navi swallowed (the shortcut's own keys, the ramble, cancel and mode keys, Enter
/// finishing a take), not Ctrl, Shift, Alt or Win on their own, and not a volume or media key.
/// Navi's own keystrokes never get this far (`NAVI_INPUT_TAG`).
fn reaches_the_text(vk: u8, up: bool, swallowed: bool) -> bool {
    !up && !swallowed && Modifier::of_vk(vk).is_none() && !matches!(vk, 0xAD..=0xB3) && vk != VK_FN
}

/// When the hook last saw such a key, readable from any thread without a lock, so the hook never
/// waits for it.
struct TypingClock {
    base: std::sync::OnceLock<Instant>,
    /// Nanoseconds after `base`, plus one; 0 while nothing has been seen.
    last: std::sync::atomic::AtomicU64,
}

impl TypingClock {
    const fn new() -> Self {
        Self { base: std::sync::OnceLock::new(), last: std::sync::atomic::AtomicU64::new(0) }
    }

    fn note(&self, at: Instant) {
        let base = *self.base.get_or_init(|| at);
        // An `at` from before the base counts as the base: later than it was, never earlier.
        let ns = at.saturating_duration_since(base).as_nanos().min(u64::MAX as u128 - 1) as u64 + 1;
        self.last.fetch_max(ns, std::sync::atomic::Ordering::Relaxed);
    }

    fn since(&self, t: Instant) -> bool {
        let ns = self.last.load(std::sync::atomic::Ordering::Relaxed);
        match self.base.get() {
            Some(base) if ns > 0 => *base + std::time::Duration::from_nanos(ns - 1) > t,
            _ => false,
        }
    }
}

static TYPING: TypingClock = TypingClock::new();

/// True if a key that could change the text in front was pressed since `t` (`reaches_the_text`),
/// as the hook saw it. True as well while the hook isn't running, as nothing is known then. Mouse
/// clicks are never seen.
pub fn keys_pressed_since(t: Instant) -> bool {
    #[cfg(windows)]
    let running = win::running();
    #[cfg(target_os = "macos")]
    let running = mac::running();
    #[cfg(not(any(windows, target_os = "macos")))]
    let running = false;
    !running || TYPING.since(t)
}

// ---------------------------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------------------------

pub struct HotkeyService {
    #[cfg(windows)]
    thread_id: u32,
    #[cfg(any(windows, target_os = "macos"))]
    thread: Option<std::thread::JoinHandle<()>>,
}

impl HotkeyService {
    /// Installs the hook on a dedicated thread with its own message loop.
    pub fn start(chord: Vec<String>, ramble_key: String, cancel_key: String, tx: UnboundedSender<HotkeyMsg>) -> Result<Self> {
        let chord_keys = parse_chord(&chord).map_err(|e| anyhow!(e))?;
        let ramble = parse_optional_key(&ramble_key, "ramble");
        let cancel = parse_optional_key(&cancel_key, "cancel");
        let machine = HookMachine::new(chord_keys, ramble, cancel);
        #[cfg(windows)]
        {
            let (thread_id, thread) = win::start(machine, tx)?;
            Ok(Self { thread_id, thread: Some(thread) })
        }
        #[cfg(target_os = "macos")]
        {
            let thread = mac::start(machine, tx)?;
            Ok(Self { thread: Some(thread) })
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let _ = (machine, tx);
            Err(anyhow!("Global shortcuts only work on Windows and macOS for now."))
        }
    }

    /// An unknown key name in `chord` leaves the current chord in place (see `validate_chord`).
    pub fn set_chord(&self, chord: Vec<String>, ramble_key: String, cancel_key: String) {
        let chord_keys = match parse_chord(&chord) {
            Ok(keys) => keys,
            Err(e) => {
                log::warn!("Keeping the current talk shortcut: {e}");
                return;
            }
        };
        let ramble = parse_optional_key(&ramble_key, "ramble");
        let cancel = parse_optional_key(&cancel_key, "cancel");
        with_machine(|m| m.set_keys(chord_keys, ramble, cancel));
    }

    /// The mode keys and then the action keys (lib.rs extra_key_chords). One that does not parse
    /// (or has no keys yet) is kept as a key that never fires, so the others keep their numbers.
    pub fn set_extras(&self, extras: &[Vec<String>]) {
        let parsed: Vec<Vec<Key>> = extras.iter().map(|names| parse_chord(names).unwrap_or_default()).collect();
        with_machine(|m| m.set_extras(parsed));
    }

    /// While true, the cancel key is reported and swallowed.
    pub fn set_recording(&self, on: bool) {
        with_machine(|m| m.set_recording(on));
    }

    pub fn pause(&self, paused: bool) {
        with_machine(|m| m.set_paused(paused));
    }

    /// The take being recorded went hands-free (or became a ramble): Enter now finishes it.
    pub fn set_enter_finishes(&self, on: bool) {
        with_machine(|m| m.set_enter_finishes(on));
    }

    /// The next chord the user presses, as logical key names (e.g. ["Ctrl", "Win"]). Keys pressed
    /// while capturing are swallowed and fire no events. Escape is captured like any other key, so
    /// the caller decides whether ["Escape"] means "cancel". A newer capture replaces an older one
    /// (the older receiver then sees the sender dropped).
    pub fn capture_next_chord(&self) -> oneshot::Receiver<Vec<String>> {
        let (tx, rx) = oneshot::channel();
        #[cfg(any(windows, target_os = "macos"))]
        {
            let mut guard = plat::SHARED.lock();
            if let Some(shared) = guard.as_mut() {
                shared.machine.start_capture();
                shared.capture_tx = Some(tx);
            }
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        drop(tx);
        rx
    }

    /// Stops a capture started by `capture_next_chord` (its receiver sees the sender dropped).
    pub fn cancel_capture(&self) {
        #[cfg(any(windows, target_os = "macos"))]
        {
            let mut guard = plat::SHARED.lock();
            if let Some(shared) = guard.as_mut() {
                shared.machine.cancel_capture();
                shared.capture_tx = None;
            }
        }
    }

    /// True if every key of the talk chord is physically down right now (GetAsyncKeyState).
    pub fn chord_is_down(&self) -> bool {
        #[cfg(any(windows, target_os = "macos"))]
        {
            let guard = plat::SHARED.lock();
            guard.as_ref().is_some_and(|s| s.machine.chord_is_down(&plat::probe))
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        false
    }

    /// While set, every key the hook sees (except Navi's own keystrokes) is also sent on `tx`, for
    /// the live shortcut tester. None stops it.
    pub fn set_monitor(&self, tx: Option<UnboundedSender<KeySeen>>) {
        #[cfg(any(windows, target_os = "macos"))]
        {
            if let Some(shared) = plat::SHARED.lock().as_mut() {
                shared.monitor = tx;
            }
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        drop(tx);
    }

    /// Reinstalls the hook now and forgets any keys it thinks are held (normally done by itself
    /// after a quiet minute).
    pub fn repair_now(&self) {
        #[cfg(windows)]
        win::request_repair(self.thread_id);
        #[cfg(target_os = "macos")]
        mac::request_repair();
    }

    /// How the listener is doing, for the log and the tester.
    pub fn health(&self) -> HookHealth {
        #[cfg(any(windows, target_os = "macos"))]
        {
            plat::health()
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        HookHealth::default()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookHealth {
    /// Seconds since the hook last saw a key.
    pub quiet_secs: u64,
    /// Times the hook was reinstalled after a quiet spell.
    pub reinstalls: u64,
    /// Keys let through because the shared state was busy (should stay 0).
    pub busy_misses: u64,
}

/// Other dictation apps that are running, by name ("Superwhisper"), since they may listen for
/// the same keys: whichever hook Windows asks first can keep a key from the other.
pub fn other_dictation_apps() -> Vec<String> {
    #[cfg(any(windows, target_os = "macos"))]
    let running = plat::running_exe_names();
    #[cfg(not(any(windows, target_os = "macos")))]
    let running: Vec<String> = Vec::new();
    let mut found: Vec<String> = Vec::new();
    for exe in running {
        if let Some(name) = dictation_app_name(&exe) {
            if !found.iter().any(|f| f == name) {
                found.push(name.to_string());
            }
        }
    }
    found
}

/// The product name for a known dictation app's executable ("Superwhisper.exe" -> "Superwhisper").
fn dictation_app_name(exe: &str) -> Option<&'static str> {
    let stem: String = exe
        .trim()
        .to_lowercase()
        .trim_end_matches(".exe")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    const KNOWN: &[(&str, &str)] = &[
        ("superwhisper", "Superwhisper"),
        ("wisprflow", "Wispr Flow"),
        ("wispr", "Wispr Flow"),
        ("willowvoice", "Willow"),
        ("willow", "Willow"),
        ("aquavoice", "Aqua Voice"),
        ("whispertyping", "WhisperTyping"),
        ("talon", "Talon"),
        ("natspeak", "Dragon"),
        ("dragonbar", "Dragon"),
    ];
    KNOWN.iter().find(|(key, _)| stem == *key).map(|(_, name)| *name)
}

#[cfg(windows)]
impl Drop for HotkeyService {
    fn drop(&mut self) {
        win::stop(self.thread_id, self.thread.take());
    }
}

#[cfg(target_os = "macos")]
impl Drop for HotkeyService {
    fn drop(&mut self) {
        mac::stop(self.thread.take());
    }
}

// The platform's listener, where the code is the same on both.
#[cfg(windows)]
use win as plat;
#[cfg(target_os = "macos")]
use mac as plat;

fn with_machine(f: impl FnOnce(&mut HookMachine)) {
    #[cfg(any(windows, target_os = "macos"))]
    {
        let mut guard = plat::SHARED.lock();
        if let Some(shared) = guard.as_mut() {
            f(&mut shared.machine);
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    let _ = f;
}

#[cfg(windows)]
mod win {
    use super::*;
    use parking_lot::Mutex;
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::Duration;
    use windows::Win32::Foundation::{CloseHandle, HINSTANCE, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::SystemInformation::GetTickCount64;
    use windows::Win32::System::Threading::{GetCurrentThread, GetCurrentThreadId, SetThreadPriority, THREAD_PRIORITY_HIGHEST};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VIRTUAL_KEY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetMessageW, KillTimer, PeekMessageW, PostThreadMessageW, SetTimer, SetWindowsHookExW,
        TranslateMessage, UnhookWindowsHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLKHF_LOWER_IL_INJECTED, LLKHF_UP,
        MSG, PM_NOREMOVE, WH_KEYBOARD_LL, WM_QUIT, WM_TIMER, WM_USER,
    };

    /// AltGr arrives as this fake left Ctrl, then right Alt. It is not Ctrl.
    const ALTGR_FAKE_CTRL_SCAN: u32 = 0x21D;
    /// Longest the hook waits for the shared state before letting a key through untouched.
    const LOCK_WAIT: Duration = Duration::from_millis(20);
    /// How often the hook thread checks whether the hook needs reinstalling.
    const REPAIR_CHECK_MS: u32 = 5_000;
    /// A removed hook sees no keys at all, so reinstall after this long without one.
    const REPAIR_AFTER_QUIET_MS: u64 = 60_000;
    /// Posted to the hook thread to reinstall straight away (`HotkeyService::repair_now`).
    const WM_REPAIR_NOW: u32 = WM_USER + 1;

    pub(super) fn request_repair(thread_id: u32) {
        unsafe {
            let _ = PostThreadMessageW(thread_id, WM_REPAIR_NOW, WPARAM(0), LPARAM(0));
        }
    }

    pub(super) struct Shared {
        pub machine: HookMachine,
        pub tx: UnboundedSender<HotkeyMsg>,
        pub capture_tx: Option<oneshot::Sender<Vec<String>>>,
        pub monitor: Option<UnboundedSender<KeySeen>>,
    }

    /// The hook procedure has no user-data pointer, so its state is global. Every holder keeps the
    /// lock for microseconds.
    pub(super) static SHARED: Mutex<Option<Shared>> = parking_lot::const_mutex(None);
    static RUNNING: AtomicBool = AtomicBool::new(false);
    /// GetTickCount64 when the hook last saw a key.
    static LAST_EVENT: AtomicU64 = AtomicU64::new(0);
    static REINSTALLS: AtomicU64 = AtomicU64::new(0);
    static BUSY_MISSES: AtomicU64 = AtomicU64::new(0);
    /// A copy of the machine's swallowed flags, readable without the lock: when the state is busy,
    /// a key-up whose key-down was swallowed is still swallowed, so Windows never sees a lone up.
    static SWALLOWED: [AtomicBool; 256] = [const { AtomicBool::new(false) }; 256];

    thread_local! {
        /// Events let through while the state was busy, fed to the machine (oldest first) on the
        /// next event that gets the lock. Only the hook thread runs the hook, so no lock is needed.
        static MISSED: RefCell<Vec<(u8, bool, bool, Instant)>> = const { RefCell::new(Vec::new()) };
    }

    fn tick() -> u64 {
        unsafe { GetTickCount64() }
    }

    pub(super) fn health() -> HookHealth {
        HookHealth {
            quiet_secs: tick().saturating_sub(LAST_EVENT.load(Ordering::Relaxed)) / 1000,
            reinstalls: REINSTALLS.load(Ordering::Relaxed),
            busy_misses: BUSY_MISSES.load(Ordering::Relaxed),
        }
    }

    pub(super) fn probe(vk: u8) -> bool {
        let state = unsafe { GetAsyncKeyState(vk as i32) };
        state < 0
    }

    /// Executable names of every running process ("Superwhisper.exe").
    pub(super) fn running_exe_names() -> Vec<String> {
        let mut out = Vec::new();
        unsafe {
            let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
                return out;
            };
            let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
            let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
            while more {
                let len = entry.szExeFile.iter().position(|c| *c == 0).unwrap_or(entry.szExeFile.len());
                out.push(String::from_utf16_lossy(&entry.szExeFile[..len]));
                more = Process32NextW(snapshot, &mut entry).is_ok();
            }
            let _ = CloseHandle(snapshot);
        }
        out
    }

    pub(super) fn start(machine: HookMachine, tx: UnboundedSender<HotkeyMsg>) -> Result<(u32, std::thread::JoinHandle<()>)> {
        if RUNNING.swap(true, Ordering::SeqCst) {
            return Err(anyhow!("Navi's shortcut listener is already running."));
        }
        *SHARED.lock() = Some(Shared { machine, tx, capture_tx: None, monitor: None });

        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<std::result::Result<u32, String>>();
        let spawned = std::thread::Builder::new().name("navi-hotkey".into()).spawn(move || hook_thread(ready_tx));
        let thread = match spawned {
            Ok(t) => t,
            Err(e) => {
                reset();
                return Err(anyhow!("Navi couldn't start its shortcut listener: {e}"));
            }
        };
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(thread_id)) => Ok((thread_id, thread)),
            Ok(Err(message)) => {
                reset();
                Err(anyhow!(message))
            }
            Err(_) => {
                reset();
                Err(anyhow!("Navi's shortcut listener didn't start in time."))
            }
        }
    }

    fn reset() {
        *SHARED.lock() = None;
        RUNNING.store(false, Ordering::SeqCst);
    }

    pub(super) fn running() -> bool {
        RUNNING.load(Ordering::SeqCst)
    }

    pub(super) fn stop(thread_id: u32, thread: Option<std::thread::JoinHandle<()>>) {
        unsafe {
            let _ = PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(thread) = thread {
            let started = Instant::now();
            while !thread.is_finished() && started.elapsed() < Duration::from_secs(1) {
                std::thread::sleep(Duration::from_millis(5));
            }
            if thread.is_finished() {
                let _ = thread.join();
            } else {
                log::warn!("The shortcut listener thread did not stop within a second");
            }
        }
        reset();
    }

    fn install() -> windows::core::Result<HHOOK> {
        unsafe {
            let module = GetModuleHandleW(None).ok().map(|m| HINSTANCE(m.0));
            SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), module, 0)
        }
    }

    fn hook_thread(ready: std::sync::mpsc::Sender<std::result::Result<u32, String>>) {
        unsafe {
            // Windows waits for this thread on every keystroke system-wide; keep it responsive.
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
            // Create the message queue now, so a WM_QUIT posted early is not lost.
            let mut msg = MSG::default();
            let _ = PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE);

            let mut hook = match install() {
                Ok(hook) => Some(hook),
                Err(e) => {
                    let _ = ready.send(Err(format!("Windows wouldn't let Navi listen for its shortcut ({e}).")));
                    return;
                }
            };
            LAST_EVENT.store(tick(), Ordering::Relaxed);
            // Keys pressed before the hook was in place were never seen: count them as pressed.
            TYPING.note(Instant::now());
            let _ = ready.send(Ok(GetCurrentThreadId()));
            let timer = SetTimer(None, 0, REPAIR_CHECK_MS, None);

            // 0 means WM_QUIT, -1 an error; either way, stop.
            while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                if msg.message == WM_TIMER && msg.hwnd.is_invalid() {
                    repair(&mut hook, false);
                    continue;
                }
                if msg.message == WM_REPAIR_NOW && msg.hwnd.is_invalid() {
                    repair(&mut hook, true);
                    continue;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if timer != 0 {
                let _ = KillTimer(None, timer);
            }
            if let Some(hook) = hook {
                let _ = UnhookWindowsHookEx(hook);
            }
        }
    }

    /// Windows removes a low-level hook that once keeps it waiting too long, and tells nobody. A
    /// removed hook sees no keys at all, so after a quiet minute put a fresh hook in place. A live
    /// hook that hears typing is never touched. A key really held keeps sending repeats, so after a
    /// quiet minute nothing is held, whatever the machine last saw: its release may have come while
    /// the hook was gone, so that state is dropped (ending a press of the shortcut if one was open).
    /// The exception is a press made by software, which sends no repeats: it is kept, and its own
    /// key-up ends it. `now` (the tester's repair button) drops everything.
    fn repair(hook: &mut Option<HHOOK>, now: bool) {
        let quiet = tick().saturating_sub(LAST_EVENT.load(Ordering::Relaxed));
        if !now && hook.is_some() && quiet < REPAIR_AFTER_QUIET_MS {
            return;
        }
        let keep_state = {
            let Some(mut guard) = SHARED.try_lock() else {
                return; // busy; the next check tries again
            };
            match guard.as_mut() {
                Some(shared) if !now && shared.machine.held_by_software() => true,
                Some(shared) => {
                    if shared.machine.forget_held() {
                        let _ = shared.tx.send((HotkeyEvent::TalkUp, Instant::now()));
                    }
                    false
                }
                None => false,
            }
        };
        if !keep_state {
            for flag in SWALLOWED.iter() {
                flag.store(false, Ordering::Relaxed);
            }
            MISSED.with(|m| m.borrow_mut().clear());
        }
        if let Some(old) = hook.take() {
            unsafe {
                let _ = UnhookWindowsHookEx(old);
            }
        }
        match install() {
            Ok(new) => {
                *hook = Some(new);
                REINSTALLS.fetch_add(1, Ordering::Relaxed);
                LAST_EVENT.store(tick(), Ordering::Relaxed);
                // Repaired by hand (the tester), the old hook was likely deaf: count the keys it
                // may have missed as pressed. Not after a quiet minute, which is most often just
                // nobody typing (talking to "Navi" instead), and would refuse every spoken edit.
                if now {
                    TYPING.note(Instant::now());
                }
            }
            // Tried again on the next check.
            Err(e) => log::warn!("could not reinstall the shortcut listener: {e}"),
        }
    }

    unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 && lparam.0 != 0 {
            // SAFETY: for HC_ACTION, lparam points at a KBDLLHOOKSTRUCT valid for this call.
            let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            if handle(info) {
                return LRESULT(1);
            }
        }
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }

    /// True to swallow the event.
    fn handle(info: &KBDLLHOOKSTRUCT) -> bool {
        // Navi's own keystrokes (pasting, the mask key) are never part of a shortcut.
        if info.dwExtraInfo == crate::insert::NAVI_INPUT_TAG {
            return false;
        }
        LAST_EVENT.store(tick(), Ordering::Relaxed);
        let Ok(mut vk) = u8::try_from(info.vkCode) else {
            return false;
        };
        if vk == 0 || vk == 0xFF || (vk == VK_LCONTROL && info.scanCode == ALTGR_FAKE_CTRL_SCAN) {
            return false;
        }
        // Ctrl + Pause reports VK_CANCEL; fold it in so the key's down and up always agree.
        if vk == VK_CANCEL {
            vk = VK_PAUSE;
        }
        let up = info.flags.0 & LLKHF_UP.0 != 0;
        let injected = info.flags.0 & (LLKHF_INJECTED.0 | LLKHF_LOWER_IL_INJECTED.0) != 0;
        let at = Instant::now();

        let (swallow, mask) = {
            // Never keep Windows waiting: if the state is busy, decide from the lock-free copy of
            // the swallowed flags (an up is swallowed exactly when its down was) and let the
            // machine catch up on the next event.
            let Some(mut guard) = SHARED.try_lock_for(LOCK_WAIT) else {
                BUSY_MISSES.fetch_add(1, Ordering::Relaxed);
                let swallow = up && SWALLOWED[vk as usize].load(Ordering::Relaxed);
                MISSED.with(|m| {
                    let mut m = m.borrow_mut();
                    if m.len() < 64 {
                        m.push((vk, up, injected, at));
                    }
                });
                if reaches_the_text(vk, up, swallow) {
                    TYPING.note(at);
                }
                return swallow;
            };
            let Some(shared) = guard.as_mut() else {
                return false;
            };
            let mut mask = false;
            for (m_vk, m_up, m_injected, m_at) in MISSED.with(|m| std::mem::take(&mut *m.borrow_mut())) {
                let d = feed(shared, m_vk, m_up, m_injected, m_at);
                // Already passed to Windows: never treat that down as swallowed.
                if !m_up && d.swallow {
                    shared.machine.mark_passed(m_vk);
                    SWALLOWED[m_vk as usize].store(false, Ordering::Relaxed);
                }
                mask |= d.inject_mask;
            }
            if let Some(monitor) = &shared.monitor {
                let _ = monitor.send(KeySeen { vk, up, injected });
            }
            let d = feed(shared, vk, up, injected, at);
            (d.swallow, mask || d.inject_mask)
        };
        if reaches_the_text(vk, up, swallow) {
            TYPING.note(at);
        }
        if mask {
            send_mask();
        }
        swallow
    }

    /// One event through the machine, with its events and capture result sent on.
    fn feed(shared: &mut Shared, vk: u8, up: bool, injected: bool, at: Instant) -> Decision {
        let mut d = shared.machine.on_event(vk, up, injected, &probe);
        SWALLOWED[vk as usize].store(shared.machine.is_swallowed(vk), Ordering::Relaxed);
        if d.talk_up_first {
            let _ = shared.tx.send((HotkeyEvent::TalkUp, at));
        }
        if let Some(event) = d.event {
            let _ = shared.tx.send((event, at));
        }
        if let Some(names) = d.captured.take() {
            if let Some(tx) = shared.capture_tx.take() {
                let _ = tx.send(names);
            }
        }
        d
    }

    /// Injected from inside the hook: Windows queues it behind the event being handled, so it lands
    /// after the key-down that completed the chord and before the user's eventual key-up.
    fn send_mask() {
        let key = |up: bool| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(MASK_VK as u16),
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                    time: 0,
                    dwExtraInfo: crate::insert::NAVI_INPUT_TAG,
                },
            },
        };
        let inputs = [key(false), key(true)];
        unsafe {
            SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        }
    }
}

/// The Mac counterpart of `win`: a Quartz event tap (CGEventTap) on its own thread with a run
/// loop. Mac key codes are turned into the Windows virtual-key numbers `HookMachine` works in at
/// the edge (`vk_of_keycode`), so the machine and its tests are shared. Modifiers arrive as
/// flagsChanged events: the key code says which key, and the device flags say whether it went
/// down or up. Needs the Accessibility permission; until it is granted the thread keeps trying,
/// so the shortcut starts working the moment the user allows it, without a restart.
#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use objc2_core_foundation::{kCFRunLoopDefaultMode, CFMachPort, CFRetained, CFRunLoop};
    use objc2_core_graphics::{
        CGEvent, CGEventField, CGEventFlags, CGEventMask, CGEventSource, CGEventSourceStateID, CGEventTapLocation,
        CGEventTapOptions, CGEventTapPlacement, CGEventTapProxy, CGEventType,
    };
    use parking_lot::Mutex;
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::Duration;

    /// Longest the tap waits for the shared state before letting a key through untouched.
    const LOCK_WAIT: Duration = Duration::from_millis(20);
    /// How often the tap thread checks the tap is still on (macOS turns off a tap that is slow).
    const CHECK_EVERY_S: f64 = 3.0;
    /// While there's no permission, how often to try making the tap again.
    const RETRY_EVERY: Duration = Duration::from_secs(2);

    pub(super) struct Shared {
        pub machine: HookMachine,
        pub tx: UnboundedSender<HotkeyMsg>,
        pub capture_tx: Option<oneshot::Sender<Vec<String>>>,
        pub monitor: Option<UnboundedSender<KeySeen>>,
    }

    pub(super) static SHARED: Mutex<Option<Shared>> = parking_lot::const_mutex(None);
    static RUNNING: AtomicBool = AtomicBool::new(false);
    static STOP: AtomicBool = AtomicBool::new(false);
    static REPAIR: AtomicBool = AtomicBool::new(false);
    static STARTED: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    /// Milliseconds since STARTED when the tap last saw a key.
    static LAST_EVENT: AtomicU64 = AtomicU64::new(0);
    static REINSTALLS: AtomicU64 = AtomicU64::new(0);
    static BUSY_MISSES: AtomicU64 = AtomicU64::new(0);
    static SWALLOWED: [AtomicBool; 256] = [const { AtomicBool::new(false) }; 256];
    /// The live tap, so the callback can turn it back on after macOS turned it off.
    static TAP: Mutex<Option<TapPort>> = parking_lot::const_mutex(None);

    struct TapPort(CFRetained<CFMachPort>);
    // A mach port is a kernel object; enabling it from the tap thread's callback is safe.
    unsafe impl Send for TapPort {}

    fn now_ms() -> u64 {
        STARTED.get_or_init(Instant::now).elapsed().as_millis() as u64
    }

    pub(super) fn health() -> HookHealth {
        HookHealth {
            quiet_secs: now_ms().saturating_sub(LAST_EVENT.load(Ordering::Relaxed)) / 1000,
            reinstalls: REINSTALLS.load(Ordering::Relaxed),
            busy_misses: BUSY_MISSES.load(Ordering::Relaxed),
        }
    }

    pub(super) fn running() -> bool {
        RUNNING.load(Ordering::SeqCst) && TAP.lock().is_some()
    }

    // ----- key codes -----

    /// Mac key code (kVK_*) -> Windows virtual key, for every key Navi can use or must see.
    const KEYCODES: &[(u16, u8)] = &[
        (0x00, b'A'), (0x01, b'S'), (0x02, b'D'), (0x03, b'F'), (0x04, b'H'), (0x05, b'G'), (0x06, b'Z'), (0x07, b'X'),
        (0x08, b'C'), (0x09, b'V'), (0x0A, 0xE2), (0x0B, b'B'), (0x0C, b'Q'), (0x0D, b'W'), (0x0E, b'E'), (0x0F, b'R'),
        (0x10, b'Y'), (0x11, b'T'), (0x12, b'1'), (0x13, b'2'), (0x14, b'3'), (0x15, b'4'), (0x16, b'6'), (0x17, b'5'),
        (0x18, 0xBB), (0x19, b'9'), (0x1A, b'7'), (0x1B, 0xBD), (0x1C, b'8'), (0x1D, b'0'), (0x1E, 0xDD), (0x1F, b'O'),
        (0x20, b'U'), (0x21, 0xDB), (0x22, b'I'), (0x23, b'P'), (0x24, 0x0D), (0x25, b'L'), (0x26, b'J'), (0x27, 0xDE),
        (0x28, b'K'), (0x29, 0xBA), (0x2A, 0xDC), (0x2B, 0xBC), (0x2C, 0xBF), (0x2D, b'N'), (0x2E, b'M'), (0x2F, 0xBE),
        (0x30, 0x09), (0x31, 0x20), (0x32, 0xC0), (0x33, 0x08), (0x35, 0x1B),
        (0x36, VK_RWIN), (0x37, VK_LWIN), (0x38, VK_LSHIFT), (0x39, VK_CAPITAL), (0x3A, VK_LMENU), (0x3B, VK_LCONTROL),
        (0x3C, VK_RSHIFT), (0x3D, VK_RMENU), (0x3E, VK_RCONTROL), (0x3F, VK_FN),
        (0x40, 0x80), (0x41, 0x6E), (0x43, 0x6A), (0x45, 0x6B), (0x47, 0x90), (0x48, 0xAF), (0x49, 0xAE), (0x4A, 0xAD),
        (0x4B, 0x6F), (0x4C, 0x0D), (0x4E, 0x6D), (0x4F, 0x81), (0x50, 0x82),
        (0x52, 0x60), (0x53, 0x61), (0x54, 0x62), (0x55, 0x63), (0x56, 0x64), (0x57, 0x65), (0x58, 0x66), (0x59, 0x67),
        (0x5A, 0x83), (0x5B, 0x68), (0x5C, 0x69),
        (0x60, 0x74), (0x61, 0x75), (0x62, 0x76), (0x63, 0x72), (0x64, 0x77), (0x65, 0x78), (0x67, 0x7A), (0x69, 0x7C),
        (0x6A, 0x7F), (0x6B, 0x7D), (0x6D, 0x79), (0x6F, 0x7B), (0x71, 0x7E), (0x72, 0x2D), (0x73, 0x24), (0x74, 0x21),
        (0x75, 0x2E), (0x76, 0x73), (0x77, 0x23), (0x78, 0x71), (0x79, 0x22), (0x7A, 0x70), (0x7B, 0x25), (0x7C, 0x27),
        (0x7D, 0x28), (0x7E, 0x26),
    ];

    // ----- the keyboard layout's symbols -----

    /// The character each symbol key types (unshifted) with the layout read at start-up.
    static LAYOUT: Mutex<Option<std::collections::HashMap<u8, char>>> = parking_lot::const_mutex(None);

    #[link(name = "Carbon", kind = "framework")]
    extern "C" {
        fn TISCopyCurrentKeyboardLayoutInputSource() -> *mut c_void;
        fn TISGetInputSourceProperty(source: *mut c_void, key: *const c_void) -> *const c_void;
        static kTISPropertyUnicodeKeyLayoutData: *const c_void;
        fn LMGetKbdType() -> u8;
        fn UCKeyTranslate(
            layout: *const u8, code: u16, action: u16, modifiers: u32, keyboard: u32, options: u32,
            dead: *mut u32, max: usize, len: *mut usize, chars: *mut u16,
        ) -> i32;
        fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
        fn CFRelease(object: *const c_void);
    }

    pub(super) fn refresh_layout() {
        const DISPLAY: u16 = 3; // kUCKeyActionDisplay
        const NO_DEAD_KEYS: u32 = 1; // kUCKeyTranslateNoDeadKeysMask
        let mut map = std::collections::HashMap::new();
        unsafe {
            let source = TISCopyCurrentKeyboardLayoutInputSource();
            if source.is_null() {
                return;
            }
            let data = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
            if !data.is_null() {
                let layout = CFDataGetBytePtr(data);
                let keyboard = LMGetKbdType() as u32;
                for &(code, vk) in KEYCODES {
                    if !matches!(vk, 0xBA..=0xC0 | 0xDB..=0xDF | 0xE1..=0xE4) {
                        continue;
                    }
                    let (mut dead, mut len, mut chars) = (0u32, 0usize, [0u16; 4]);
                    let ok = UCKeyTranslate(layout, code, DISPLAY, 0, keyboard, NO_DEAD_KEYS, &mut dead, chars.len(), &mut len, chars.as_mut_ptr());
                    if ok == 0 && len == 1 {
                        if let Some(c) = char::from_u32(chars[0] as u32).filter(|c| !c.is_control() && !c.is_whitespace()) {
                            map.insert(vk, c);
                        }
                    }
                }
            }
            CFRelease(source);
        }
        *LAYOUT.lock() = Some(map);
    }

    pub(super) fn layout_char(vk: u8) -> Option<char> {
        LAYOUT.lock().as_ref().and_then(|m| m.get(&vk).copied())
    }

    pub(crate) fn vk_of_keycode(code: u16) -> Option<u8> {
        KEYCODES.iter().find(|(c, _)| *c == code).map(|(_, vk)| *vk)
    }

    pub(crate) fn keycode_of_vk(vk: u8) -> Option<u16> {
        KEYCODES.iter().find(|(_, v)| *v == vk).map(|(c, _)| *c)
    }

    /// Device-dependent flag bits (IOLLEvent.h) saying which side of a modifier is down.
    fn modifier_down(vk: u8, flags: u64) -> Option<bool> {
        let bit: u64 = match vk {
            VK_LCONTROL => 0x0000_0001,
            VK_LSHIFT => 0x0000_0002,
            VK_RSHIFT => 0x0000_0004,
            VK_LWIN => 0x0000_0008,
            VK_RWIN => 0x0000_0010,
            VK_LMENU => 0x0000_0020,
            VK_RMENU => 0x0000_0040,
            VK_RCONTROL => 0x0000_2000,
            VK_FN => CGEventFlags::MaskSecondaryFn.0,
            VK_CAPITAL => CGEventFlags::MaskAlphaShift.0,
            _ => return None,
        };
        Some(flags & bit != 0)
    }

    /// Whether `vk` is physically down now (the Mac's GetAsyncKeyState).
    pub(super) fn probe(vk: u8) -> bool {
        let state = CGEventSourceStateID::HIDSystemState;
        match vk {
            VK_CONTROL => probe(VK_LCONTROL) || probe(VK_RCONTROL),
            VK_SHIFT => probe(VK_LSHIFT) || probe(VK_RSHIFT),
            VK_MENU => probe(VK_LMENU) || probe(VK_RMENU),
            VK_FN => CGEventSource::flags_state(state).0 & CGEventFlags::MaskSecondaryFn.0 != 0,
            _ => keycode_of_vk(vk).is_some_and(|code| CGEventSource::key_state(state, code)),
        }
    }

    /// Names of the apps running ("Superwhisper"), for spotting other dictation apps.
    pub(super) fn running_exe_names() -> Vec<String> {
        let apps = objc2_app_kit::NSWorkspace::sharedWorkspace().runningApplications();
        apps.iter().filter_map(|a| a.localizedName().map(|n| n.to_string())).collect()
    }

    // ----- the tap thread -----

    pub(super) fn start(machine: HookMachine, tx: UnboundedSender<HotkeyMsg>) -> Result<std::thread::JoinHandle<()>> {
        if RUNNING.swap(true, Ordering::SeqCst) {
            return Err(anyhow!("Navi's shortcut listener is already running."));
        }
        STOP.store(false, Ordering::SeqCst);
        *SHARED.lock() = Some(Shared { machine, tx, capture_tx: None, monitor: None });
        // Ask once, so macOS shows its prompt (and lists Navi in System Settings) straight away.
        if !crate::mac::ax::ask_for_trust() {
            log::info!("the shortcut needs the Accessibility permission; waiting for it");
        }
        std::thread::Builder::new().name("navi-hotkey".into()).spawn(tap_thread).map_err(|e| {
            *SHARED.lock() = None;
            RUNNING.store(false, Ordering::SeqCst);
            anyhow!("Navi couldn't start its shortcut listener: {e}")
        })
    }

    pub(super) fn stop(thread: Option<std::thread::JoinHandle<()>>) {
        STOP.store(true, Ordering::SeqCst);
        if let Some(thread) = thread {
            let started = Instant::now();
            while !thread.is_finished() && started.elapsed() < Duration::from_secs(CHECK_EVERY_S as u64 + 1) {
                std::thread::sleep(Duration::from_millis(5));
            }
            if thread.is_finished() {
                let _ = thread.join();
            } else {
                log::warn!("The shortcut listener thread did not stop in time");
            }
        }
        *TAP.lock() = None;
        *SHARED.lock() = None;
        RUNNING.store(false, Ordering::SeqCst);
    }

    pub(super) fn request_repair() {
        REPAIR.store(true, Ordering::SeqCst);
    }

    fn make_tap() -> Option<CFRetained<CFMachPort>> {
        let mask: CGEventMask = (1 << CGEventType::KeyDown.0) | (1 << CGEventType::KeyUp.0) | (1 << CGEventType::FlagsChanged.0);
        unsafe {
            CGEvent::tap_create(
                CGEventTapLocation::SessionEventTap,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::Default,
                mask,
                Some(callback),
                std::ptr::null_mut(),
            )
        }
    }

    fn tap_thread() {
        let _ = STARTED.get_or_init(Instant::now);
        let mut logged = false;
        let tap = loop {
            if STOP.load(Ordering::SeqCst) {
                return;
            }
            if let Some(tap) = make_tap() {
                break tap;
            }
            if !logged {
                log::warn!("macOS hasn't allowed Navi to hear its shortcut yet (Accessibility permission)");
                logged = true;
            }
            std::thread::sleep(RETRY_EVERY);
        };
        let Some(source) = CFMachPort::new_run_loop_source(None, Some(&tap), 0) else {
            log::warn!("could not attach the shortcut listener to its run loop");
            return;
        };
        let Some(run_loop) = CFRunLoop::current() else { return };
        run_loop.add_source(Some(&source), unsafe { kCFRunLoopDefaultMode });
        CGEvent::tap_enable(&tap, true);
        *TAP.lock() = Some(TapPort(tap.clone()));
        LAST_EVENT.store(now_ms(), Ordering::Relaxed);
        TYPING.note(Instant::now());
        log::info!("shortcut listener running");

        while !STOP.load(Ordering::SeqCst) {
            CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, CHECK_EVERY_S, false);
            let repair_now = REPAIR.swap(false, Ordering::SeqCst);
            if repair_now {
                if let Some(mut guard) = SHARED.try_lock_for(LOCK_WAIT) {
                    if let Some(shared) = guard.as_mut() {
                        if shared.machine.forget_held() {
                            let _ = shared.tx.send((HotkeyEvent::TalkUp, Instant::now()));
                        }
                    }
                }
                for flag in SWALLOWED.iter() {
                    flag.store(false, Ordering::Relaxed);
                }
                TYPING.note(Instant::now());
            }
            if repair_now || !CGEvent::tap_is_enabled(&tap) {
                CGEvent::tap_enable(&tap, true);
                REINSTALLS.fetch_add(1, Ordering::Relaxed);
                log::info!("shortcut listener switched back on");
            }
        }
        *TAP.lock() = None;
    }

    unsafe extern "C-unwind" fn callback(_proxy: CGEventTapProxy, kind: CGEventType, event: NonNull<CGEvent>, _info: *mut c_void) -> *mut CGEvent {
        let pass = event.as_ptr();
        if kind == CGEventType::TapDisabledByTimeout || kind == CGEventType::TapDisabledByUserInput {
            if let Some(tap) = TAP.try_lock().and_then(|t| t.as_ref().map(|p| p.0.clone())) {
                CGEvent::tap_enable(&tap, true);
                REINSTALLS.fetch_add(1, Ordering::Relaxed);
            }
            return pass;
        }
        let event = unsafe { event.as_ref() };
        if handle(kind, event) {
            std::ptr::null_mut()
        } else {
            pass
        }
    }

    /// True to swallow the event.
    fn handle(kind: CGEventType, event: &CGEvent) -> bool {
        let ev = Some(event);
        // Navi's own keystrokes (pasting, selecting back) are never part of a shortcut.
        if CGEvent::integer_value_field(ev, CGEventField::EventSourceUserData) == crate::insert::NAVI_INPUT_TAG as i64 {
            return false;
        }
        LAST_EVENT.store(now_ms(), Ordering::Relaxed);
        let code = CGEvent::integer_value_field(ev, CGEventField::KeyboardEventKeycode) as u16;
        let Some(vk) = vk_of_keycode(code) else { return false };
        let up = if kind == CGEventType::FlagsChanged {
            match modifier_down(vk, CGEvent::flags(ev).0) {
                Some(down) => !down,
                None => return false,
            }
        } else if kind == CGEventType::KeyDown {
            false
        } else if kind == CGEventType::KeyUp {
            true
        } else {
            return false;
        };
        // Caps Lock reaches a tap only as it toggles, never as a press and release.
        if vk == VK_CAPITAL {
            return false;
        }
        // Events from the keyboard come from the HID system; anything posted by software doesn't.
        let injected = CGEvent::integer_value_field(ev, CGEventField::EventSourceStateID) != CGEventSourceStateID::HIDSystemState.0 as i64;
        let at = Instant::now();

        let swallow = {
            let Some(mut guard) = SHARED.try_lock_for(LOCK_WAIT) else {
                BUSY_MISSES.fetch_add(1, Ordering::Relaxed);
                let swallow = up && SWALLOWED[vk as usize].load(Ordering::Relaxed);
                if reaches_the_text(vk, up, swallow) {
                    TYPING.note(at);
                }
                return swallow;
            };
            let Some(shared) = guard.as_mut() else { return false };
            if let Some(monitor) = &shared.monitor {
                let _ = monitor.send(KeySeen { vk, up, injected });
            }
            let mut d = shared.machine.on_event(vk, up, injected, &probe);
            SWALLOWED[vk as usize].store(shared.machine.is_swallowed(vk), Ordering::Relaxed);
            if d.talk_up_first {
                let _ = shared.tx.send((HotkeyEvent::TalkUp, at));
            }
            if let Some(event) = d.event {
                let _ = shared.tx.send((event, at));
            }
            if let Some(names) = d.captured.take() {
                if let Some(tx) = shared.capture_tx.take() {
                    let _ = tx.send(names);
                }
            }
            // No mask key on a Mac: a lone Command or Option tap does nothing there.
            d.swallow
        };
        if reaches_the_text(vk, up, swallow) {
            TYPING.note(at);
        }
        // A modifier's flagsChanged is never held back: the app would see a stuck modifier.
        swallow && (kind != CGEventType::FlagsChanged || vk == VK_FN)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashSet;

    /// Dan wanted a key for rewriting the selection, and Tom a key for everything only a voice
    /// command could do. Those keys are checked like mode keys, and against every key Navi has.
    #[test]
    fn action_keys_are_checked_against_the_talk_key_and_every_other_key() {
        let label = |n: &str| key_label(n);
        let taken = vec![(names(&["F8"]), "Ask AI".to_string()), (names(&["F2"]), "switching to Email".to_string())];
        assert!(check_action_key(&names(&["F9"]), &names(&["RCtrl"]), &taken, &label).error.is_none());
        let clash = check_action_key(&names(&["F8"]), &names(&["RCtrl"]), &taken, &label).error.expect("F8 is Ask AI's");
        assert!(clash.contains("already your key for Ask AI"), "{clash}");
        let clash = check_action_key(&names(&["F2"]), &names(&["RCtrl"]), &taken, &label).error.expect("F2 switches mode");
        assert!(clash.contains("switching to Email"), "{clash}");
        let lone = check_action_key(&names(&["Ctrl", "Shift"]), &names(&["F9"]), &[], &label).error.expect("modifiers only");
        assert!(lone.contains("go off while you work"), "{lone}");
        let talk = check_action_key(&names(&["RAlt", "F7"]), &names(&["RAlt"]), &[], &label).error.expect("shares the talk key");
        assert!(talk.contains("talk shortcut"), "{talk}");
        // The same keys in another order are the same key.
        let taken = vec![(names(&["Ctrl", "Shift", "F7"]), "Rewrite with AI".to_string())];
        assert!(check_action_key(&names(&["Shift", "Ctrl", "F7"]), &names(&["F9"]), &taken, &label).error.is_some_and(|e| e.contains("Rewrite with AI")));
        assert!(check_action_key(&names(&["Ctrl", "Shift", "F8"]), &names(&["F9"]), &taken, &label).error.is_none());
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn chord(list: &[&str]) -> Vec<Key> {
        parse_chord(&names(list)).expect("valid chord")
    }

    #[test]
    fn only_keys_that_reach_the_text_count_as_typing() {
        assert!(reaches_the_text(b'A', false, false));
        assert!(reaches_the_text(0x08, false, false), "Backspace");
        assert!(reaches_the_text(0x25, false, false), "an arrow moves the caret");
        assert!(!reaches_the_text(b'A', true, false), "a release");
        assert!(!reaches_the_text(0x14, false, true), "swallowed: the shortcut's own Caps Lock");
        for vk in [VK_LCONTROL, VK_RCONTROL, VK_LWIN, VK_RWIN, VK_LSHIFT, VK_SHIFT, VK_MENU] {
            assert!(!reaches_the_text(vk, false, false), "the talk keys and other modifiers on their own: {vk:#x}");
        }
        assert!(!reaches_the_text(0xAF, false, false), "Volume Up");
        assert!(!reaches_the_text(0xB3, false, false), "Play/Pause");
    }

    #[test]
    fn the_typing_clock_says_whether_a_key_came_after() {
        let clock = TypingClock::new();
        let t0 = Instant::now();
        assert!(!clock.since(t0), "nothing seen yet");
        let typed = t0 + std::time::Duration::from_millis(500);
        clock.note(typed);
        assert!(clock.since(t0));
        assert!(!clock.since(typed), "not after itself");
        assert!(!clock.since(typed + std::time::Duration::from_millis(1)));
        // A key fed late with its own older time never moves the clock back.
        clock.note(t0 + std::time::Duration::from_millis(100));
        assert!(clock.since(t0 + std::time::Duration::from_millis(400)));
    }

    /// A machine plus a fake view of Windows' key state that follows every event the machine
    /// lets through (swallowed keys never reach Windows).
    struct Rig {
        m: HookMachine,
        os_down: RefCell<HashSet<u8>>,
    }

    impl Rig {
        fn new(chord_names: &[&str]) -> Self {
            Self::with(chord_names, Some("Space"), Some("Escape"))
        }

        fn with(chord_names: &[&str], ramble: Option<&str>, cancel: Option<&str>) -> Self {
            Rig {
                m: HookMachine::new(chord(chord_names), ramble.and_then(parse_key), cancel.and_then(parse_key)),
                os_down: RefCell::new(HashSet::new()),
            }
        }

        fn ev(&mut self, vk: u8, up: bool) -> Decision {
            let os = self.os_down.borrow().clone();
            let d = self.m.on_key(vk, up, &|k| os.contains(&k));
            if !d.swallow {
                if up {
                    self.os_down.borrow_mut().remove(&vk);
                } else {
                    self.os_down.borrow_mut().insert(vk);
                }
            }
            d
        }

        fn down(&mut self, vk: u8) -> Decision {
            self.ev(vk, false)
        }

        fn up(&mut self, vk: u8) -> Decision {
            self.ev(vk, true)
        }
    }

    const SPACE: u8 = 0x20;
    const ESC: u8 = 0x1B;
    const LEFT: u8 = 0x25;
    const RIGHT: u8 = 0x27;
    const F9: u8 = 0x78;
    const CAPS: u8 = 0x14;
    const KEY_A: u8 = 0x41;

    #[test]
    fn parses_modifiers_and_sides() {
        assert_eq!(parse_key("Ctrl"), Some(Key::Mod(Modifier::Ctrl)));
        assert_eq!(parse_key("control"), Some(Key::Mod(Modifier::Ctrl)));
        assert_eq!(parse_key("Shift"), Some(Key::Mod(Modifier::Shift)));
        assert_eq!(parse_key("Alt"), Some(Key::Mod(Modifier::Alt)));
        assert_eq!(parse_key("Win"), Some(Key::Mod(Modifier::Win)));
        assert_eq!(parse_key("Windows"), Some(Key::Mod(Modifier::Win)));
        assert_eq!(parse_key("LCtrl"), Some(Key::Vk(VK_LCONTROL)));
        assert_eq!(parse_key("RCtrl"), Some(Key::Vk(VK_RCONTROL)));
        assert_eq!(parse_key("LShift"), Some(Key::Vk(VK_LSHIFT)));
        assert_eq!(parse_key("RShift"), Some(Key::Vk(VK_RSHIFT)));
        assert_eq!(parse_key("LAlt"), Some(Key::Vk(VK_LMENU)));
        assert_eq!(parse_key("RAlt"), Some(Key::Vk(VK_RMENU)));
        assert_eq!(parse_key("LWin"), Some(Key::Vk(VK_LWIN)));
        assert_eq!(parse_key("RWin"), Some(Key::Vk(VK_RWIN)));
        assert_eq!(parse_key("ControlLeft"), Some(Key::Vk(VK_LCONTROL)));
        assert_eq!(parse_key("MetaRight"), Some(Key::Vk(VK_RWIN)));
        assert_eq!(parse_key("Right Ctrl"), Some(Key::Vk(VK_RCONTROL)));
    }

    #[test]
    fn parses_named_keys() {
        let cases: &[(&str, u8)] = &[
            ("F1", 0x70),
            ("F9", 0x78),
            ("f24", 0x87),
            ("Space", 0x20),
            ("space", 0x20),
            ("Escape", 0x1B),
            ("Esc", 0x1B),
            ("CapsLock", 0x14),
            ("Caps Lock", 0x14),
            ("ScrollLock", 0x91),
            ("Pause", 0x13),
            ("Insert", 0x2D),
            ("Home", 0x24),
            ("End", 0x23),
            ("PageUp", 0x21),
            ("PgDn", 0x22),
            ("A", 0x41),
            ("z", 0x5A),
            ("0", 0x30),
            ("7", 0x37),
            ("Backquote", 0xC0),
            ("`", 0xC0),
            ("-", 0xBD),
            ("KeyQ", 0x51),
            ("Digit3", 0x33),
            ("ArrowLeft", 0x25),
            ("Numpad5", 0x65),
            ("VK5B", 0x5B),
            ("vk0xE8", 0xE8),
            ("PrintScreen", 0x2C),
            ("Menu", 0x5D),
        ];
        for (name, vk) in cases {
            assert_eq!(parse_key(name), Some(Key::Vk(*vk)), "{name}");
        }
    }

    #[test]
    fn rejects_unknown_names() {
        for name in ["", "  ", "F25", "F0", "Nonsense", "KeyAB", "Digit12", "VKZZ", "VK00", "Numpad10", "€"] {
            assert_eq!(parse_key(name), None, "{name:?}");
        }
    }

    #[test]
    fn every_key_name_reads_back() {
        for vk in 1..=254u8 {
            let name = vk_name(vk);
            let expected = match Modifier::of_vk(vk) {
                Some(m) if m.generic_vk() == Some(vk) => Key::Mod(m),
                _ => Key::Vk(vk),
            };
            assert_eq!(parse_key(&name), Some(expected), "vk {vk:#04x} named {name}");
        }
    }

    #[test]
    fn chord_parsing_and_validation() {
        assert_eq!(chord(&["Ctrl", "Win"]), vec![Key::Mod(Modifier::Ctrl), Key::Mod(Modifier::Win)]);
        assert_eq!(chord(&["Ctrl", "ctrl", "Win"]).len(), 2);
        let err = parse_chord(&names(&["Ctrl", "Bogus"])).unwrap_err();
        assert!(err.contains("Bogus"), "{err}");
        assert!(parse_chord(&[]).is_err());
        assert!(parse_chord(&names(&["", " "])).is_err());
    }

    #[test]
    fn validation_keeps_normal_typing_safe() {
        // A Mac can't hold Caps Lock, and Control + Space switches its input source.
        let mac_only_bad: [&[&str]; 2] = [&["CapsLock"], &["Ctrl", "Space"]];
        for ok in [&["Ctrl", "Win"][..], &["F9"], &["RCtrl"], &["CapsLock"], &["Ctrl", "Space"], &["Alt", "A"], &["ScrollLock"], &["Backslash"], &["RAlt"]] {
            if cfg!(target_os = "macos") && mac_only_bad.contains(&ok) {
                assert!(validate_chord(&names(ok)).is_err(), "{ok:?}");
                continue;
            }
            assert!(validate_chord(&names(ok)).is_ok(), "{ok:?}");
        }
        for bad in [&["A"][..], &["Space"], &["7"], &["Enter"], &["Tab"], &["Escape"], &["Ctrl"], &["Shift"], &["Win"], &["Bogus"]] {
            let err = validate_chord(&names(bad)).unwrap_err();
            assert!(!err.contains('\u{2014}'), "{err}");
        }
        assert!(validate_chord(&names(&["Space"])).unwrap_err().starts_with("Space on its own"));
        assert!(validate_chord(&names(&["Shift"])).unwrap_err().starts_with("Shift on its own"));
    }

    // Windows key names; a Mac shows its own (see mac_labels_use_mac_words).
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn one_key_shortcuts_get_plain_warnings() {
        let check = |keys: &[&str]| check_chord(&names(keys), "Space");
        // Good single keys: no error, no warning.
        for good in [&["RCtrl"][..], &["RAlt"], &["F9"], &["F13"], &["ScrollLock"], &["Pause"], &["Insert"], &["RWin"], &["Menu"]] {
            let c = check(good);
            assert_eq!((c.error.as_deref(), c.warning.as_deref()), (None, None), "{good:?}");
        }
        // Caps Lock works and says how to switch capitals on now.
        let c = check(&["CapsLock"]);
        assert!(c.error.is_none());
        assert!(c.warning.as_deref().is_some_and(|w| w.contains("Shift + Caps Lock")), "{c:?}");
        // And Shift + Caps Lock really does pass through when Caps Lock is the shortcut.
        let mut r = Rig::new(&["CapsLock"]);
        r.down(VK_LSHIFT);
        let d = r.down(CAPS);
        assert_eq!(d.event, None);
        assert!(!d.swallow);
        // A symbol key works, but can no longer be typed (Superwhisper users pick Backslash).
        let c = check(&["Backslash"]);
        assert!(c.error.is_none());
        assert_eq!(c.warning.as_deref(), Some("You won't be able to type \\ while Navi is running."));
        assert_eq!(c.label, "\\");
        assert!(check(&["Backquote"]).warning.is_some());
        assert!(check(&["Numpad5"]).warning.is_some());
        // Everyday modifiers on their own work, with a nudge towards a safer key.
        for nudge in [&["LCtrl"][..], &["LAlt"], &["LWin"], &["LShift"], &["RShift"]] {
            let c = check(nudge);
            assert!(c.error.is_none(), "{nudge:?}");
            assert!(c.warning.as_deref().is_some_and(|w| w.contains("safer pick")), "{nudge:?}: {c:?}");
        }
        // A shortcut that contains the ramble key is allowed, with a note.
        let c = check(&["Ctrl", "Space"]);
        assert!(c.error.is_none());
        assert!(c.warning.as_deref().is_some_and(|w| w.starts_with("Space is part of this shortcut")), "{c:?}");
        assert_eq!(check(&["Ctrl", "Win"]), ChordCheck { label: "Ctrl + Win".into(), labels: names(&["Ctrl", "Win"]), error: None, warning: None });
        for c in [check(&["Escape"]), check(&["A"]), check(&["Win"]), check(&["Bogus"])] {
            assert!(c.error.is_some(), "{c:?}");
        }
        // No message uses dashes as punctuation.
        for keys in [&["Backslash"][..], &["LCtrl"], &["LShift"], &["Escape"], &["A"], &["Ctrl"], &["Ctrl", "Space"]] {
            let c = check(keys);
            let text = format!("{:?}{:?}", c.error, c.warning);
            assert!(!text.contains('\u{2014}') && !text.contains('\u{2013}'), "{text}");
        }
    }

    #[test]
    fn mode_keys_fire_and_never_reach_the_app() {
        let mut r = Rig::new(&["RCtrl"]);
        r.m.set_extras(vec![chord(&["F1"]), Vec::new(), chord(&["Ctrl", "Alt", "3"])]);
        let d = r.down(0x70);
        assert_eq!(d.event, Some(HotkeyEvent::ExtraKey(0)));
        assert!(d.swallow, "F1 never opens Help underneath");
        assert_eq!(r.down(0x70), Decision { swallow: true, ..Decision::default() }, "a repeat fires nothing");
        assert!(r.up(0x70).swallow);
        // A chord: the index counts the empty (unparseable) entry too.
        r.down(VK_LCONTROL);
        r.down(VK_LMENU);
        let d = r.down(b'3');
        assert_eq!(d.event, Some(HotkeyEvent::ExtraKey(2)));
        assert!(d.swallow && d.inject_mask);
        assert!(r.up(b'3').swallow);
        r.up(VK_LMENU);
        r.up(VK_LCONTROL);
        // While you hold the talk key, a mode key is "another shortcut": it cancels, as any key does.
        assert_eq!(r.down(VK_RCONTROL).event, Some(HotkeyEvent::TalkDown));
        assert_eq!(r.down(0x70).event, Some(HotkeyEvent::Cancel));
        r.up(0x70);
        r.up(VK_RCONTROL);
        // Mid hands-free (keys released, still recording) a mode key works.
        r.m.set_recording(true);
        assert_eq!(r.down(0x70).event, Some(HotkeyEvent::ExtraKey(0)));
    }

    #[test]
    fn mode_keys_are_checked_against_the_talk_key_and_each_other() {
        let label = |n: &str| key_label(n);
        assert!(check_mode_key(&names(&["F1"]), &names(&["RCtrl"]), &[], &label).error.is_none());
        assert!(check_mode_key(&names(&["F9"]), &names(&["F9"]), &[], &label).error.as_deref().is_some_and(|e| e.contains("talk shortcut")));
        assert!(check_mode_key(&names(&["Ctrl", "1"]), &names(&["Ctrl", "Win"]), &[], &label).error.is_some(), "shares Ctrl with the talk keys");
        assert!(check_mode_key(&names(&["F2"]), &names(&["RCtrl"]), &[names(&["F2"])], &label).error.as_deref().is_some_and(|e| e.contains("another mode")));
        assert!(check_mode_key(&names(&["Ctrl", "V"]), &names(&["RCtrl"]), &[], &label).error.is_some());
    }

    #[test]
    fn f1_to_f5_get_the_same_check_as_a_key_set_by_hand() {
        // What Settings' "Use F1 to F5" asks for each key, against the keys as they will be.
        let label = |n: &str| key_label(n);
        let fkeys: Vec<Vec<String>> = (1..=5).map(|i| names(&[&format!("F{i}")])).collect();
        let others = |i: usize| fkeys.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, k)| k.clone()).collect::<Vec<_>>();
        for i in 0..5 {
            assert!(check_mode_key(&fkeys[i], &names(&["Ctrl", "Win"]), &others(i), &label).error.is_none(), "F{}", i + 1);
        }
        // A talk shortcut of F3 refuses F3, and the advice never names a talk key.
        let refused = check_mode_key(&fkeys[2], &names(&["F3"]), &others(2), &label).error.expect("F3 is the talk key");
        // A Mac suggests F13 and up (its F1 to F12 are brightness and volume).
        let (first, second) = if cfg!(target_os = "macos") { ("F13", "F13") } else { ("F1", "F2") };
        assert!(refused.contains("talk shortcut") && refused.ends_with(&format!("such as {first}.")), "{refused}");
        let talk_f1 = check_mode_key(&fkeys[0], &names(&["F1"]), &others(0), &label).error.expect("F1 is the talk key");
        assert!(talk_f1.ends_with(&format!("such as {second}.")), "{talk_f1}");
    }

    // Windows key names; a Mac shows its own (see mac_labels_use_mac_words).
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn everyday_shortcuts_are_refused() {
        let check = |keys: &[&str]| check_chord(&names(keys), "Space");
        let c = check(&["Ctrl", "V"]);
        assert_eq!(c.error.as_deref(), Some("Ctrl + V is Paste in most apps, so Navi would take it over. Pick something else."));
        assert!(check(&["Ctrl", "C"]).error.as_deref().is_some_and(|e| e.contains("Copy")));
        assert!(check(&["Ctrl", "K"]).error.as_deref().is_some_and(|e| e.contains("everyday shortcut")));
        assert!(check(&["Alt", "Tab"]).error.as_deref().is_some_and(|e| e.contains("Windows shortcut")));
        assert!(check(&["Alt", "F4"]).error.is_some());
        assert!(check(&["Win", "E"]).error.as_deref().is_some_and(|e| e.contains("Windows shortcut")));
        assert!(check(&["Ctrl", "Win", "Right"]).error.is_some(), "switches desktops");
        assert!(validate_chord(&names(&["Ctrl", "V"])).is_err(), "never saved, even if the interface asks");
        // Close relatives that nothing uses are fine.
        for ok in [&["Ctrl", "Shift", "Space"][..], &["Ctrl", "F9"], &["Ctrl", "Win"], &["Alt", "A"], &["Ctrl", "Alt", "D"]] {
            assert!(check(ok).error.is_none(), "{ok:?}: {:?}", check(ok).error);
        }
    }

    // Windows key names; a Mac shows its own (see mac_labels_use_mac_words).
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn checks_carry_a_label_per_key() {
        let c = check_chord(&names(&["Ctrl", "Win"]), "Space");
        assert_eq!(c.labels, names(&["Ctrl", "Win"]));
        assert_eq!(check_chord(&names(&["RCtrl"]), "Space").labels, names(&["Right Ctrl"]));
        // The UK ` key has a name now, and reads back.
        assert_eq!(vk_name(0xDF), "Oem8");
        assert_eq!(parse_key("Oem8"), Some(Key::Vk(0xDF)));
        assert_eq!(parse_key("VKDF"), Some(Key::Vk(0xDF)), "settings saved before still work");
        // Layout labels never fall back to "VKxx" for a symbol key, and leave named keys alone.
        assert!(!display_label("Oem8").starts_with("VK"));
        assert_eq!(display_label("RCtrl"), "Right Ctrl");
        assert_eq!(display_label("F9"), "F9");
        assert_eq!(display_chord_label(&names(&["Ctrl", "Win"])), "Ctrl + Win");
    }

    #[test]
    fn injected_keys_can_press_the_shortcut() {
        // A keyboard app's macro key set to send F13.
        let mut r = Rig::new(&["F13"]);
        let f13 = 0x7C;
        let d = r.m.on_event(f13, false, true, &|_| false);
        assert_eq!(d.event, Some(HotkeyEvent::TalkDown));
        assert!(d.swallow);
        let d = r.m.on_event(f13, true, true, &|_| false);
        assert_eq!(d.event, Some(HotkeyEvent::TalkUp));
        assert!(d.swallow, "its down was swallowed, so its up is too");
    }

    #[test]
    fn a_stray_injected_key_never_cancels_a_held_shortcut() {
        let mut r = Rig::new(&["Ctrl", "Win"]);
        r.down(VK_LCONTROL);
        assert_eq!(r.down(VK_LWIN).event, Some(HotkeyEvent::TalkDown));
        // Another app injects keys in the background (AutoHotkey's mask key, a keep-awake F15).
        let os = r.os_down.borrow().clone();
        for vk in [MASK_VK, 0x7E, KEY_A] {
            assert_eq!(r.m.on_event(vk, false, true, &|k| os.contains(&k)), Decision::default());
            assert_eq!(r.m.on_event(vk, true, true, &|k| os.contains(&k)), Decision::default());
        }
        assert!(r.m.chord_active(), "the take goes on");
        // An injected ramble key still counts.
        let d = r.m.on_event(SPACE, false, true, &|k| os.contains(&k));
        assert_eq!(d.event, Some(HotkeyEvent::Ramble));
        assert!(r.m.on_event(SPACE, true, true, &|k| os.contains(&k)).swallow);
        assert_eq!(r.up(VK_LWIN).event, Some(HotkeyEvent::TalkUp));
    }

    #[test]
    fn a_capture_records_injected_keys() {
        let mut r = Rig::new(&["Ctrl", "Win"]);
        r.m.start_capture();
        let f13 = 0x7C;
        assert!(r.m.on_event(f13, false, true, &|_| false).swallow);
        let d = r.m.on_event(f13, true, true, &|_| false);
        assert_eq!(d.captured, Some(names(&["F13"])));
    }

    #[test]
    fn a_gesture_that_sends_more_than_the_shortcut_is_another_shortcut() {
        // A mouse gesture injects Ctrl + Win + Right to switch desktops.
        let mut r = Rig::new(&["Ctrl", "Win"]);
        let none = |_: u8| false;
        assert_eq!(r.m.on_event(VK_LCONTROL, false, true, &none).event, None);
        assert_eq!(r.m.on_event(VK_LWIN, false, true, &|k| k == VK_LCONTROL).event, Some(HotkeyEvent::TalkDown));
        let d = r.m.on_event(RIGHT, false, true, &|k| k == VK_LCONTROL || k == VK_LWIN);
        assert_eq!(d.event, Some(HotkeyEvent::Cancel), "the gesture's arrow spends the press");
        assert!(!d.swallow);
        assert_eq!(r.m.on_event(RIGHT, true, true, &none), Decision::default());
        assert_eq!(r.m.on_event(VK_LWIN, true, true, &none).event, Some(HotkeyEvent::TalkUp));
    }

    #[test]
    fn a_script_releasing_a_key_you_hold_does_not_end_the_take() {
        let mut r = Rig::new(&["Ctrl", "Win"]);
        r.down(VK_LCONTROL);
        assert_eq!(r.down(VK_LWIN).event, Some(HotkeyEvent::TalkDown));
        // AutoHotkey restores modifier state by injecting Ctrl up while you still hold it.
        let os = r.os_down.borrow().clone();
        assert_eq!(r.m.on_event(VK_LCONTROL, true, true, &|k| os.contains(&k)), Decision::default());
        assert!(r.m.chord_active(), "you are still holding the keys");
        assert_eq!(r.up(VK_LCONTROL).event, Some(HotkeyEvent::TalkUp), "your own release still ends it");
    }

    #[test]
    fn after_a_long_silence_held_state_is_forgotten() {
        // The hook was removed while F9 was held, so its release was never seen.
        let mut r = Rig::new(&["F9"]);
        assert_eq!(r.down(F9).event, Some(HotkeyEvent::TalkDown));
        assert!(!r.m.is_idle(&|_| false));
        assert!(r.m.forget_held(), "a press was in progress, so the caller sends TalkUp");
        assert!(r.m.is_idle(&|_| false));
        assert!(!r.m.chord_is_down(&|_| false), "the watchdog no longer thinks F9 is held");
        // The next real press works straight away (not mistaken for a repeat).
        assert_eq!(r.down(F9).event, Some(HotkeyEvent::TalkDown));
        let mut quiet = Rig::new(&["Ctrl", "Win"]);
        assert!(!quiet.m.forget_held(), "nothing in progress, nothing to end");
    }

    #[test]
    fn a_down_let_through_while_busy_never_swallows_its_up() {
        let mut r = Rig::new(&["F9"]);
        // The hook was busy when F9 went down, so Windows saw it; the machine catches up later.
        let d = r.m.on_event(F9, false, false, &|_| false);
        assert!(d.swallow);
        r.m.mark_passed(F9);
        assert!(!r.m.is_swallowed(F9));
        let d = r.m.on_event(F9, true, false, &|_| false);
        assert_eq!(d.event, Some(HotkeyEvent::TalkUp));
        assert!(!d.swallow, "Windows saw the down, so it must see the up");
    }

    #[test]
    fn idle_means_nothing_held_or_in_progress() {
        let mut r = Rig::new(&["Ctrl", "Win"]);
        assert!(r.m.is_idle(&|_| false));
        r.down(VK_LCONTROL);
        let os = r.os_down.borrow().clone();
        assert!(!r.m.is_idle(&|k| os.contains(&k)), "Ctrl is held");
        // Its release went unseen: Windows says it is up, so it no longer counts.
        assert!(r.m.is_idle(&|_| false));
        r.up(VK_LCONTROL);
        r.m.start_capture();
        assert!(!r.m.is_idle(&|_| false), "a capture is waiting");
        r.m.cancel_capture();
        let mut f = Rig::new(&["F9"]);
        f.down(F9);
        assert!(!f.m.is_idle(&|_| false), "a swallowed key is trusted until its release");
    }

    #[test]
    fn known_dictation_apps_are_named() {
        assert_eq!(dictation_app_name("Superwhisper.exe"), Some("Superwhisper"));
        assert_eq!(dictation_app_name("Wispr Flow.exe"), Some("Wispr Flow"));
        assert_eq!(dictation_app_name("AquaVoice.exe"), Some("Aqua Voice"));
        assert_eq!(dictation_app_name("Aqua-Voice.exe"), Some("Aqua Voice"));
        assert_eq!(dictation_app_name("navi.exe"), None);
        assert_eq!(dictation_app_name("whisper-server.exe"), None);
        assert_eq!(dictation_app_name("flow.exe"), None);
    }

    /// Installs the real hook with F24 as the shortcut and injects F24 and F23 (keys no keyboard
    /// has; the hook swallows them, so no app sees them) to prove the Windows side works end to end.
    /// Run by hand: cargo test --lib hotkey::tests::live_hook_hears_injected_keys -- --ignored --nocapture
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn live_hook_hears_injected_keys() {
        use std::time::{Duration, Instant};
        use windows::Win32::UI::Input::KeyboardAndMouse::{SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VIRTUAL_KEY};

        fn send(vk: u8, up: bool, tag: usize) {
            let input = INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk as u16), wScan: 0, dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, time: 0, dwExtraInfo: tag },
                },
            };
            let sent = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
            assert_eq!(sent, 1, "SendInput refused the key");
        }
        fn next<T>(rx: &mut tokio::sync::mpsc::UnboundedReceiver<T>, within: Duration) -> Option<T> {
            let deadline = Instant::now() + within;
            while Instant::now() < deadline {
                if let Ok(v) = rx.try_recv() {
                    return Some(v);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            None
        }
        const F23: u8 = 0x86;
        const F24: u8 = 0x87;

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let t = Instant::now();
        let service = HotkeyService::start(names(&["F24"]), "F23".into(), "Escape".into(), tx).expect("hook installs");
        println!("hook installed in {} ms", t.elapsed().as_millis());
        let (mtx, mut mrx) = tokio::sync::mpsc::unbounded_channel();
        service.set_monitor(Some(mtx));

        let t = Instant::now();
        send(F24, false, 0);
        let (event, at) = next(&mut rx, Duration::from_secs(1)).expect("TalkDown");
        assert_eq!(event, HotkeyEvent::TalkDown);
        assert!(at >= t, "events carry the time the hook saw them");
        println!("TalkDown {} ms after the key", t.elapsed().as_millis());
        assert!(service.chord_is_down());
        // The ramble key while the shortcut is held.
        send(F23, false, 0);
        assert_eq!(next(&mut rx, Duration::from_secs(1)).map(|m| m.0), Some(HotkeyEvent::Ramble));
        send(F23, true, 0);
        send(F24, true, 0);
        assert_eq!(next(&mut rx, Duration::from_secs(1)).map(|m| m.0), Some(HotkeyEvent::TalkUp));
        assert!(!service.chord_is_down());
        assert_eq!(next(&mut mrx, Duration::from_secs(1)), Some(KeySeen { vk: F24, up: false, injected: true }));

        // Navi's own keystrokes are invisible to it.
        send(F24, false, crate::insert::NAVI_INPUT_TAG);
        send(F24, true, crate::insert::NAVI_INPUT_TAG);
        std::thread::sleep(Duration::from_millis(150));
        while let Ok(seen) = mrx.try_recv() {
            assert_ne!(seen, KeySeen { vk: F24, up: false, injected: true }, "a tagged key reached the monitor");
        }
        assert_eq!(rx.try_recv().ok().map(|m| m.0), None, "a tagged key fired an event");

        // Recording a new shortcut.
        let mut captured = service.capture_next_chord();
        send(F23, false, 0);
        send(F23, true, 0);
        let deadline = Instant::now() + Duration::from_secs(1);
        let names_got = loop {
            if let Ok(v) = captured.try_recv() {
                break v;
            }
            assert!(Instant::now() < deadline, "the capture never finished");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(names_got, names(&["F23"]));

        // Self-repair: hold F24, reinstall the hook mid-press (as if Windows had removed it), and
        // check the open press is ended and the new hook hears the next press.
        send(F24, false, 0);
        assert_eq!(next(&mut rx, Duration::from_secs(1)).map(|m| m.0), Some(HotkeyEvent::TalkDown));
        service.repair_now();
        assert_eq!(next(&mut rx, Duration::from_secs(1)).map(|m| m.0), Some(HotkeyEvent::TalkUp), "the stale press is ended");
        assert!(!service.chord_is_down(), "nothing is believed held after a repair");
        send(F24, true, 0); // the late release of that press is harmless
        std::thread::sleep(Duration::from_millis(50));
        while rx.try_recv().is_ok() {}
        send(F24, false, 0);
        assert_eq!(next(&mut rx, Duration::from_secs(1)).map(|m| m.0), Some(HotkeyEvent::TalkDown), "the new hook works");
        send(F24, true, 0);
        assert_eq!(next(&mut rx, Duration::from_secs(1)).map(|m| m.0), Some(HotkeyEvent::TalkUp));

        let health = service.health();
        println!("health: {health:?}");
        assert_eq!(health.busy_misses, 0);
        assert_eq!(health.reinstalls, 1);
        drop(service);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_labels_use_mac_words() {
        assert_eq!(chord_label(&names(&["Ctrl", "Win"])), "Control + Command");
        assert_eq!(key_label("RAlt"), "Right Option");
        assert_eq!(key_label("RWin"), "Right Command");
        assert_eq!(key_label("Fn"), "Fn");
        assert_eq!(key_label("Enter"), "Return");
        assert_eq!(parse_key("Right Option"), Some(Key::Vk(VK_RMENU)));
        assert_eq!(parse_key("globe"), Some(Key::Vk(VK_FN)));
        let c = check_chord(&names(&["Win", "V"]), "Space");
        assert_eq!(c.error.as_deref(), Some("Command + V is Paste in most apps, so Navi would take it over. Pick something else."));
        assert_eq!(check_chord(&names(&["Ctrl", "A"]), "Space").error.as_deref(), Some("Control + A is an everyday shortcut in many apps, so Navi would take it over. Pick something else."));
        assert!(check_chord(&names(&["Win", "Space"]), "Space").error.as_deref().is_some_and(|e| e.contains("a Mac shortcut")));
        assert!(check_chord(&names(&["LCtrl"]), "Space").warning.as_deref().is_some_and(|w| w.contains("Right Option") && !w.contains("Ctrl")));
        assert!(check_mode_key(&names(&["RAlt"]), &names(&["RAlt"]), &[], &|n: &str| key_label(n)).error.as_deref().is_some_and(|e| e.contains("F13")));
        assert!(check_chord(&names(&["Fn"]), "Space").error.is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_layout_names_symbol_keys() {
        // Test threads aren't the main thread, but reading the layout there works in practice.
        refresh_layout();
        assert!(display_label("Slash") == "/" || !display_label("Slash").is_empty());
        assert_eq!(mac::layout_char(b'A'), None, "letters keep their own names");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_key_codes_round_trip() {
        for code in [0x00u16, 0x31, 0x24, 0x35, 0x36, 0x37, 0x3A, 0x3D, 0x3B, 0x3E, 0x3F, 0x7A, 0x69] {
            let vk = mac::vk_of_keycode(code).expect("mapped");
            assert_eq!(mac::keycode_of_vk(vk), Some(code));
        }
        assert_eq!(mac::vk_of_keycode(0x3F), Some(VK_FN));
        assert_eq!(mac::vk_of_keycode(0x36), Some(VK_RWIN));
    }

    // Windows key names; a Mac shows its own (see mac_labels_use_mac_words).
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn labels_are_friendly() {
        assert_eq!(chord_label(&names(&["Ctrl", "Win"])), "Ctrl + Win");
        assert_eq!(chord_label(&names(&["RCtrl"])), "Right Ctrl");
        assert_eq!(chord_label(&names(&["Ctrl", "Shift", "PageUp"])), "Ctrl + Shift + Page Up");
        assert_eq!(chord_label(&names(&["Escape"])), "Esc");
        assert_eq!(chord_label(&names(&["Alt", "Backquote"])), "Alt + `");
        assert_eq!(chord_label(&names(&["Weird"])), "Weird");
        assert_eq!(chord_label(&names(&["Numpad5", "F13"])), "Num 5 + F13");
    }

    #[test]
    fn ctrl_win_fires_once_with_mask_and_passes_through() {
        let mut r = Rig::new(&["Ctrl", "Win"]);
        assert_eq!(r.down(VK_LCONTROL), Decision::default());
        let d = r.down(VK_LWIN);
        assert_eq!(d.event, Some(HotkeyEvent::TalkDown));
        assert!(d.inject_mask);
        assert!(!d.swallow);
        // Auto-repeat of the held keys.
        for _ in 0..5 {
            assert_eq!(r.down(VK_LWIN), Decision::default());
            assert_eq!(r.down(VK_LCONTROL), Decision::default());
        }
        let d = r.up(VK_LWIN);
        assert_eq!(d.event, Some(HotkeyEvent::TalkUp));
        assert!(!d.swallow);
        assert_eq!(r.up(VK_LCONTROL), Decision::default());
        // And again.
        r.down(VK_RWIN);
        assert_eq!(r.down(VK_RCONTROL).event, Some(HotkeyEvent::TalkDown));
        assert_eq!(r.up(VK_RCONTROL).event, Some(HotkeyEvent::TalkUp));
    }

    #[test]
    fn order_does_not_matter_and_either_side_counts() {
        let mut r = Rig::new(&["Ctrl", "Win"]);
        r.down(VK_RWIN);
        assert_eq!(r.down(VK_LCONTROL).event, Some(HotkeyEvent::TalkDown));
    }

    #[test]
    fn sided_keys_only_match_their_side() {
        let mut r = Rig::new(&["LCtrl", "Win"]);
        r.down(VK_RCONTROL);
        assert_eq!(r.down(VK_LWIN).event, None);
        r.up(VK_LWIN);
        r.up(VK_RCONTROL);
        r.down(VK_LCONTROL);
        assert_eq!(r.down(VK_LWIN).event, Some(HotkeyEvent::TalkDown));
    }

    #[test]
    fn both_sides_held_releases_only_when_the_last_goes() {
        let mut r = Rig::new(&["Ctrl", "Win"]);
        r.down(VK_LCONTROL);
        r.down(VK_RCONTROL);
        assert_eq!(r.down(VK_LWIN).event, Some(HotkeyEvent::TalkDown));
        assert_eq!(r.up(VK_LCONTROL).event, None);
        assert_eq!(r.up(VK_RCONTROL).event, Some(HotkeyEvent::TalkUp));
    }

    #[test]
    fn a_superset_chord_does_not_fire() {
        let mut r = Rig::new(&["Ctrl", "Win"]);
        r.down(VK_LSHIFT);
        r.down(VK_LCONTROL);
        assert_eq!(r.down(VK_LWIN).event, None);
    }

    #[test]
    fn stale_keys_are_forgotten() {
        let mut r = Rig::new(&["Ctrl", "Win"]);
        // Shift went down, then its release happened on the secure desktop: Windows says it is up.
        r.down(VK_LSHIFT);
        r.os_down.borrow_mut().remove(&VK_LSHIFT);
        r.down(VK_LCONTROL);
        assert_eq!(r.down(VK_LWIN).event, Some(HotkeyEvent::TalkDown));
    }

    #[test]
    fn a_stale_chord_key_cannot_complete_the_chord() {
        let mut r = Rig::new(&["Ctrl", "Win"]);
        // Win + L: the hook saw Win go down but never saw it come up.
        r.down(VK_LWIN);
        r.os_down.borrow_mut().remove(&VK_LWIN);
        // Later, a plain Ctrl press (Ctrl + C) must not start a take.
        assert_eq!(r.down(VK_LCONTROL).event, None);
        // A real Win press afterwards still works, even though the hook thought Win was down.
        assert_eq!(r.down(VK_LWIN).event, Some(HotkeyEvent::TalkDown));
    }

    #[test]
    fn ramble_key_is_swallowed_only_while_the_chord_is_held() {
        let mut r = Rig::new(&["Ctrl", "Win"]);
        assert_eq!(r.down(SPACE), Decision::default());
        r.up(SPACE);
        r.down(VK_LCONTROL);
        r.down(VK_LWIN);
        let d = r.down(SPACE);
        assert_eq!(d.event, Some(HotkeyEvent::Ramble));
        assert!(d.swallow);
        let repeat = r.down(SPACE);
        assert_eq!(repeat.event, None);
        assert!(repeat.swallow);
        assert!(r.up(SPACE).swallow);
        // The chord is still held and still owns its release.
        assert_eq!(r.up(VK_LCONTROL).event, Some(HotkeyEvent::TalkUp));
    }

    #[test]
    fn ramble_key_up_is_swallowed_even_after_the_chord_ends() {
        let mut r = Rig::new(&["Ctrl", "Win"]);
        r.down(VK_LCONTROL);
        r.down(VK_LWIN);
        assert!(r.down(SPACE).swallow);
        assert_eq!(r.up(VK_LWIN).event, Some(HotkeyEvent::TalkUp));
        assert!(r.up(SPACE).swallow, "the up must match its swallowed down");
    }

    #[test]
    fn enter_finishes_a_hands_free_take_only() {
        let mut r = Rig::new(&["Ctrl", "Win"]);
        r.m.set_recording(true);
        // A take being held down: Enter is just a key.
        assert_eq!(r.down(VK_RETURN), Decision::default());
        assert!(!r.up(VK_RETURN).swallow);
        r.m.set_enter_finishes(true);
        let d = r.down(VK_RETURN);
        assert_eq!(d.event, Some(HotkeyEvent::Finish));
        assert!(d.swallow);
        assert!(r.up(VK_RETURN).swallow, "its release is swallowed too");
        // Once the take has finished, Enter types again.
        r.m.set_recording(false);
        assert_eq!(r.down(VK_RETURN), Decision::default());
        assert!(!r.up(VK_RETURN).swallow);
    }

    /// The default (Settings off): in a hands-free take Enter reaches the app you're typing in, and
    /// a tap of the talk keys is what finishes the take.
    #[test]
    fn enter_types_normally_in_a_hands_free_take_by_default() {
        let mut r = Rig::new(&["Ctrl", "Win"]);
        r.m.set_recording(true);
        r.m.set_enter_finishes(crate::types::HotkeySettings::default().enter_finishes);
        assert_eq!(r.down(VK_RETURN), Decision::default(), "no Finish, not swallowed");
        assert!(!r.up(VK_RETURN).swallow);
        r.down(VK_LCONTROL);
        assert_eq!(r.down(VK_LWIN).event, Some(HotkeyEvent::TalkDown), "the tap that finishes it");
    }

    #[test]
    fn enter_finishes_a_ramble_while_the_talk_keys_are_still_held() {
        let mut r = Rig::new(&["Ctrl", "Win"]);
        r.down(VK_LCONTROL);
        assert_eq!(r.down(VK_LWIN).event, Some(HotkeyEvent::TalkDown));
        r.m.set_recording(true);
        assert_eq!(r.down(SPACE).event, Some(HotkeyEvent::Ramble));
        r.up(SPACE);
        r.m.set_enter_finishes(true);
        // Still holding Ctrl + Win out of habit: Enter finishes, never cancels, and never reaches
        // Windows as Ctrl + Win + Enter.
        let d = r.down(VK_RETURN);
        assert_eq!(d.event, Some(HotkeyEvent::Finish));
        assert!(d.swallow);
        assert!(r.down(VK_RETURN).swallow, "its repeats are swallowed too");
        assert!(r.up(VK_RETURN).swallow);
        assert_eq!(r.up(VK_LWIN).event, Some(HotkeyEvent::TalkUp));
        // A held take (not hands-free) still treats Enter as another shortcut.
        r.m.set_recording(false);
        r.down(VK_LWIN);
        r.m.set_recording(true);
        let d = r.down(VK_RETURN);
        assert_eq!(d.event, Some(HotkeyEvent::Cancel));
        assert!(!d.swallow);
    }

    #[test]
    fn enter_as_the_ramble_key_starts_a_ramble_then_finishes_it() {
        let mut r = Rig::with(&["Ctrl", "Win"], Some("Enter"), Some("Escape"));
        r.down(VK_LCONTROL);
        r.down(VK_LWIN);
        r.m.set_recording(true);
        assert_eq!(r.down(VK_RETURN).event, Some(HotkeyEvent::Ramble));
        r.up(VK_RETURN);
        r.m.set_enter_finishes(true);
        assert_eq!(r.down(VK_RETURN).event, Some(HotkeyEvent::Finish));
    }

    #[test]
    fn a_take_running_when_paused_can_still_be_ended() {
        let mut r = Rig::new(&["Ctrl", "Win"]);
        r.m.set_recording(true);
        r.m.set_enter_finishes(true);
        r.m.set_paused(true);
        let d = r.down(VK_RETURN);
        assert_eq!(d.event, Some(HotkeyEvent::Finish), "Enter still finishes it");
        assert!(d.swallow);
        r.up(VK_RETURN);
        let d = r.down(ESC);
        assert_eq!(d.event, Some(HotkeyEvent::Cancel), "Esc still cancels it");
        assert!(d.swallow);
        r.up(ESC);
        // Nothing new starts while paused.
        r.down(VK_LCONTROL);
        assert_eq!(r.down(VK_LWIN), Decision::default());
        r.up(VK_LWIN);
        r.up(VK_LCONTROL);
        // With no take running, Enter and Esc are just keys.
        r.m.set_recording(false);
        assert_eq!(r.down(VK_RETURN), Decision::default());
        assert_eq!(r.down(ESC), Decision::default());
    }

    #[test]
    fn a_press_made_by_software_is_held_by_software() {
        let f13 = 0x7C;
        let mut r = Rig::new(&["F13"]);
        assert!(!r.m.held_by_software());
        assert_eq!(r.m.on_event(f13, false, true, &|_| false).event, Some(HotkeyEvent::TalkDown));
        assert!(r.m.held_by_software(), "a quiet minute does not mean a macro key was let go");
        assert_eq!(r.m.on_event(f13, true, true, &|_| false).event, Some(HotkeyEvent::TalkUp));
        assert!(!r.m.held_by_software());
        // A press typed on the keyboard is not.
        let mut k = Rig::new(&["F9"]);
        k.down(F9);
        assert!(!k.m.held_by_software());
    }

    #[test]
    fn mode_keys_made_only_of_modifiers_are_refused_and_never_fire() {
        let label = |n: &str| key_label(n);
        for keys in [&["RCtrl"][..], &["RAlt"], &["RShift"], &["Ctrl", "Shift"], &["Ctrl"]] {
            let c = check_mode_key(&names(keys), &names(&["CapsLock"]), &[], &label);
            assert!(c.error.as_deref().is_some_and(|e| e.contains("switch modes")), "{keys:?}: {c:?}");
            assert!(c.warning.is_none());
        }
        assert!(check_mode_key(&names(&["Ctrl", "Shift", "M"]), &names(&["CapsLock"]), &[], &label).error.is_none());
        // One saved before the check refused it never switches mode on Ctrl + C.
        let mut r = Rig::new(&["CapsLock"]);
        r.m.set_extras(vec![chord(&["RCtrl"]), chord(&["F1"])]);
        assert_eq!(r.down(VK_RCONTROL).event, None);
        assert_eq!(r.down(b'C').event, None);
        r.up(b'C');
        r.up(VK_RCONTROL);
        assert_eq!(r.down(0x70).event, Some(HotkeyEvent::ExtraKey(1)), "the others keep their numbers");
    }

    // Windows key names; a Mac shows its own (see mac_labels_use_mac_words).
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn dead_mode_keys_are_dropped_with_one_notice() {
        use crate::types::ModeKey;
        let key = |mode: &str, keys: &[&str]| ModeKey { mode: mode.into(), keys: names(keys) };
        let mut keys = vec![key("email", &["RCtrl"]), key("clean", &["F1"]), key("next", &["Ctrl", "Shift", "M"])];
        let notice = drop_dead_mode_keys(&mut keys).expect("a notice");
        assert_eq!(notice, "Right Ctrl no longer switches modes, because it's used with other keys all the time. Pick another key in Settings.");
        assert_eq!(keys.iter().map(|k| k.mode.as_str()).collect::<Vec<_>>(), ["clean", "next"]);
        assert_eq!(drop_dead_mode_keys(&mut keys), None, "nothing left to drop, so nothing to say");

        let mut keys = vec![key("email", &["RAlt"]), key("raw", &["Ctrl", "Shift"]), key("brief", &["RShift"])];
        let notice = drop_dead_mode_keys(&mut keys).expect("a notice");
        assert!(notice.starts_with("Right Alt, Ctrl + Shift and Right Shift no longer switch modes"), "{notice}");
        assert!(keys.is_empty());
        assert!(!notice.contains('\u{2014}') && !notice.contains('\u{2013}'));
        // A key that doesn't parse is left for the hook to ignore, as before.
        let mut keys = vec![key("email", &["NoSuchKey"])];
        assert_eq!(drop_dead_mode_keys(&mut keys), None);
        assert_eq!(keys.len(), 1);
    }

    #[test]
    fn shortcuts_that_typing_would_press_are_refused() {
        let check = |keys: &[&str]| check_chord(&names(keys), "Space");
        let c = check(&["Shift", "A"]);
        assert_eq!(c.error.as_deref(), Some("Shift + A is how you type a capital or symbol, so Navi would take it over. Pick something else."));
        for bad in [&["LShift", "2"][..], &["RShift", "Slash"], &["Shift", "Enter"], &["Shift", "Space"], &["J", "K"], &["Space", "J"], &["Shift", "J", "K"]] {
            assert!(check(bad).error.is_some(), "{bad:?}");
        }
        for ok in [&["Shift", "F9"][..], &["Ctrl", "Shift", "A"], &["Alt", "J", "K"], &["Ctrl", "Space"], &["F9", "F10"], &["J", "F9"]] {
            // Control + Space switches a Mac's input source (see everyday_shortcut).
            if cfg!(target_os = "macos") && ok == ["Ctrl", "Space"] {
                continue;
            }
            assert!(check(ok).error.is_none(), "{ok:?}: {:?}", check(ok).error);
        }
        // Mode keys get the same check.
        assert!(check_mode_key(&names(&["Shift", "A"]), &names(&["RCtrl"]), &[], &|n: &str| key_label(n)).error.is_some());
    }

    #[test]
    fn cancel_key_only_while_recording_or_holding() {
        let mut r = Rig::new(&["Ctrl", "Win"]);
        assert_eq!(r.down(ESC), Decision::default());
        assert!(!r.up(ESC).swallow);
        r.m.set_recording(true);
        let d = r.down(ESC);
        assert_eq!(d.event, Some(HotkeyEvent::Cancel));
        assert!(d.swallow);
        let repeat = r.down(ESC);
        assert_eq!(repeat.event, None);
        assert!(repeat.swallow);
        assert!(r.up(ESC).swallow);
        r.m.set_recording(false);
        // Held chord, not yet recording: Escape still cancels and is swallowed.
        r.down(VK_LCONTROL);
        r.down(VK_LWIN);
        let d = r.down(ESC);
        assert_eq!(d.event, Some(HotkeyEvent::Cancel));
        assert!(d.swallow);
    }

    #[test]
    fn another_shortcut_during_the_chord_cancels_once_and_passes_through() {
        let mut r = Rig::new(&["Ctrl", "Win"]);
        r.down(VK_LCONTROL);
        assert_eq!(r.down(VK_LWIN).event, Some(HotkeyEvent::TalkDown));
        let d = r.down(LEFT);
        assert_eq!(d.event, Some(HotkeyEvent::Cancel));
        assert!(!d.swallow);
        r.up(LEFT);
        assert_eq!(r.down(RIGHT).event, None);
        r.up(RIGHT);
        assert_eq!(r.up(VK_LWIN).event, Some(HotkeyEvent::TalkUp));
        // The next press is a normal take again.
        assert_eq!(r.down(VK_LWIN).event, Some(HotkeyEvent::TalkDown));
        assert_eq!(r.down(KEY_A).event, Some(HotkeyEvent::Cancel));
    }

    #[test]
    fn a_spent_press_lets_everything_through() {
        let mut r = Rig::new(&["Ctrl", "Win"]);
        r.down(VK_LCONTROL);
        r.down(VK_LWIN);
        assert_eq!(r.down(LEFT).event, Some(HotkeyEvent::Cancel));
        let d = r.down(SPACE);
        assert_eq!(d, Decision::default(), "no Ramble after the press was cancelled");
        assert!(!r.up(SPACE).swallow);
        assert!(!r.down(ESC).swallow);
        assert_eq!(r.up(VK_LCONTROL).event, Some(HotkeyEvent::TalkUp));
    }

    #[test]
    fn a_chord_released_unseen_ends_on_the_next_press() {
        let mut r = Rig::new(&["Ctrl", "Win"]);
        r.down(VK_LCONTROL);
        assert_eq!(r.down(VK_LWIN).event, Some(HotkeyEvent::TalkDown));
        // A UAC prompt: both keys are let go on the secure desktop, out of the hook's sight.
        r.os_down.borrow_mut().clear();
        // The next press of the chord ends the old press and starts a new one.
        let d = r.down(VK_LCONTROL);
        assert!(d.talk_up_first);
        assert_eq!(d.event, None);
        let d = r.down(VK_LWIN);
        assert!(!d.talk_up_first);
        assert_eq!(d.event, Some(HotkeyEvent::TalkDown));
    }

    #[test]
    fn swallowed_chord_keys_are_trusted_until_their_release() {
        let mut r = Rig::new(&["F9"]);
        assert_eq!(r.down(F9).event, Some(HotkeyEvent::TalkDown));
        // Windows never saw the swallowed F9, so its state cannot prove F9 was let go: a later F9
        // down counts as a repeat, and another key is a foreign key on a held chord.
        assert_eq!(r.down(F9), Decision { swallow: true, ..Decision::default() });
        assert_eq!(r.down(KEY_A).event, Some(HotkeyEvent::Cancel));
        assert_eq!(r.up(F9).event, Some(HotkeyEvent::TalkUp));
    }

    #[test]
    fn the_other_side_of_a_held_modifier_is_not_a_stale_release() {
        let mut r = Rig::new(&["Ctrl", "Win"]);
        r.down(VK_LCONTROL);
        r.down(VK_LWIN);
        let d = r.down(VK_RCONTROL);
        assert_eq!(d, Decision::default());
        assert!(r.m.chord_active());
    }

    #[test]
    fn single_function_key_is_swallowed_without_a_mask() {
        let mut r = Rig::new(&["F9"]);
        let d = r.down(F9);
        assert_eq!(d.event, Some(HotkeyEvent::TalkDown));
        assert!(d.swallow);
        assert!(!d.inject_mask);
        let repeat = r.down(F9);
        assert_eq!(repeat, Decision { swallow: true, ..Decision::default() });
        let d = r.up(F9);
        assert_eq!(d.event, Some(HotkeyEvent::TalkUp));
        assert!(d.swallow);
    }

    #[test]
    fn caps_lock_pressed_first_is_not_swallowed_halfway() {
        let mut r = Rig::new(&["Ctrl", "CapsLock"]);
        assert!(!r.down(CAPS).swallow);
        let d = r.down(VK_LCONTROL);
        assert_eq!(d.event, Some(HotkeyEvent::TalkDown));
        assert!(!d.swallow);
        let d = r.up(CAPS);
        assert_eq!(d.event, Some(HotkeyEvent::TalkUp));
        assert!(!d.swallow, "its down went through, so its up must too");
    }

    #[test]
    fn caps_lock_completing_the_chord_is_swallowed() {
        let mut r = Rig::new(&["Ctrl", "CapsLock"]);
        r.down(VK_LCONTROL);
        let d = r.down(CAPS);
        assert_eq!(d.event, Some(HotkeyEvent::TalkDown));
        assert!(d.swallow);
        assert!(!d.inject_mask, "one modifier and no Win or Alt needs no mask");
        assert!(r.up(CAPS).swallow);
    }

    #[test]
    fn mask_rules() {
        let needs = |c: &[&str]| HookMachine::new(chord(c), None, None).needs_mask();
        assert!(needs(&["Ctrl", "Win"]));
        assert!(needs(&["Win"]));
        assert!(needs(&["RAlt"]));
        assert!(needs(&["Alt", "F9"]));
        assert!(needs(&["Ctrl", "Shift"]));
        assert!(!needs(&["F9"]));
        assert!(!needs(&["Ctrl", "F9"]));
        assert!(!needs(&["RCtrl"]));
    }

    #[test]
    fn paused_fires_nothing_but_keeps_ups_consistent() {
        let mut r = Rig::new(&["Ctrl", "Win"]);
        r.down(VK_LCONTROL);
        r.down(VK_LWIN);
        assert!(r.down(SPACE).swallow);
        r.m.set_paused(true);
        assert!(r.up(SPACE).swallow, "swallowed before the pause, so its up is too");
        assert_eq!(r.up(VK_LWIN).event, None, "pausing ended the chord silently");
        assert_eq!(r.down(VK_LWIN), Decision::default());
        r.up(VK_LWIN);
        r.up(VK_LCONTROL);
        r.m.set_paused(false);
        r.down(VK_LCONTROL);
        assert_eq!(r.down(VK_LWIN).event, Some(HotkeyEvent::TalkDown));
    }

    #[test]
    fn capture_collects_until_everything_is_released() {
        let mut r = Rig::new(&["Ctrl", "Win"]);
        r.m.start_capture();
        assert!(r.down(VK_LCONTROL).swallow);
        let d = r.down(VK_LWIN);
        assert!(d.swallow);
        assert_eq!(d.event, None, "capturing never fires the talk chord");
        assert!(r.down(VK_LWIN).swallow, "repeats are swallowed too");
        let d = r.up(VK_LWIN);
        assert!(d.swallow);
        assert_eq!(d.captured, None);
        let d = r.up(VK_LCONTROL);
        assert!(d.swallow);
        assert_eq!(d.captured, Some(names(&["Ctrl", "Win"])));
        assert!(!r.m.is_capturing());
    }

    #[test]
    fn capture_orders_modifiers_first() {
        let mut r = Rig::new(&["Ctrl", "Win"]);
        r.m.start_capture();
        r.down(VK_RWIN);
        r.down(VK_LSHIFT);
        r.down(KEY_A);
        r.up(KEY_A);
        r.up(VK_LSHIFT);
        assert_eq!(r.up(VK_RWIN).captured, Some(names(&["Shift", "Win", "A"])));
    }

    #[test]
    fn capture_keeps_the_side_of_a_lone_modifier() {
        let mut r = Rig::new(&["Ctrl", "Win"]);
        r.m.start_capture();
        r.down(VK_RCONTROL);
        assert_eq!(r.up(VK_RCONTROL).captured, Some(names(&["RCtrl"])));
    }

    #[test]
    fn capture_ignores_keys_held_before_it_started() {
        let mut r = Rig::new(&["Ctrl", "Win"]);
        r.down(VK_LCONTROL);
        r.m.start_capture();
        r.down(F9);
        assert!(!r.up(VK_LCONTROL).swallow, "its down went through before the capture");
        assert_eq!(r.up(F9).captured, Some(names(&["F9"])));
    }

    #[test]
    fn captured_names_cover_both_sides() {
        assert_eq!(captured_chord_names(&[VK_LCONTROL, VK_RCONTROL]), names(&["LCtrl", "RCtrl"]));
        assert_eq!(captured_chord_names(&[VK_LMENU, 0x70]), names(&["Alt", "F1"]));
        assert_eq!(captured_chord_names(&[0x70, VK_LCONTROL, VK_RWIN]), names(&["Ctrl", "Win", "F1"]));
        assert_eq!(captured_chord_names(&[CAPS]), names(&["CapsLock"]));
    }

    #[test]
    fn chord_is_down_uses_windows_state_and_swallowed_keys() {
        let m = HookMachine::new(chord(&["Ctrl", "Win"]), None, None);
        assert!(m.chord_is_down(&|vk| vk == VK_RCONTROL || vk == VK_LWIN));
        assert!(!m.chord_is_down(&|vk| vk == VK_RCONTROL));

        let mut r = Rig::new(&["F9"]);
        r.down(F9);
        let os = r.os_down.borrow().clone();
        assert!(!os.contains(&F9), "Windows never saw the swallowed F9");
        assert!(r.m.chord_is_down(&|k| os.contains(&k)));
        r.up(F9);
        assert!(!r.m.chord_is_down(&|_| false));
    }

    /// macOS can report a held key as up (CGEventSourceKeyState). Trusting the event tap's own
    /// record, as the Mac does, a held shortcut stays held (so hold to talk is announced and the
    /// release watchdog leaves it alone), a two-key chord still fires, and letting go ends it.
    #[test]
    fn a_mac_trusts_the_tap_when_the_system_says_a_held_key_is_up() {
        let never = |_: u8| false;
        // Right Option on its own, the Mac's default shortcut.
        let mut m = HookMachine::new(chord(&["RAlt"]), None, None).trusting_own_view();
        assert_eq!(m.on_key(VK_RMENU, false, &never).event, Some(HotkeyEvent::TalkDown));
        assert!(m.chord_is_down(&never), "held, whatever macOS says");
        assert!(!m.is_idle(&never));
        assert_eq!(m.on_key(VK_RMENU, false, &never).event, None, "a second key-down is not a new press");
        assert_eq!(m.on_key(VK_RMENU, true, &never).event, Some(HotkeyEvent::TalkUp));
        assert!(!m.chord_is_down(&never), "let go");
        assert!(m.is_idle(&never));

        // Two modifiers: the first is not forgotten as stale when the second goes down.
        let mut m = HookMachine::new(chord(&["Ctrl", "Alt"]), None, None).trusting_own_view();
        assert_eq!(m.on_key(VK_LCONTROL, false, &never).event, None);
        assert_eq!(m.on_key(VK_LMENU, false, &never).event, Some(HotkeyEvent::TalkDown));
        assert!(m.chord_is_down(&never));
        assert_eq!(m.on_key(VK_LCONTROL, true, &never).event, Some(HotkeyEvent::TalkUp));
        assert!(!m.chord_is_down(&never));
    }

    /// Windows keeps its own key state as the judge, so a release the hook missed is still caught.
    #[test]
    fn windows_judges_held_keys_by_the_system() {
        let never = |_: u8| false;
        let mut m = HookMachine::new(chord(&["RAlt"]), None, None);
        assert_eq!(m.on_key(VK_RMENU, false, &never).event, Some(HotkeyEvent::TalkDown));
        assert!(!m.chord_is_down(&never), "Windows says it is up, so it is treated as let go");
    }

    #[test]
    fn ramble_or_cancel_inside_the_chord_is_dropped() {
        let mut r = Rig::with(&["Ctrl", "Space"], Some("Space"), Some("Ctrl"));
        r.down(VK_LCONTROL);
        let d = r.down(SPACE);
        assert_eq!(d.event, Some(HotkeyEvent::TalkDown), "Space completes the chord instead");
        assert!(d.swallow);
    }

    #[test]
    fn changing_the_chord_mid_press_starts_clean() {
        let mut r = Rig::new(&["Ctrl", "Win"]);
        r.down(VK_LCONTROL);
        r.down(VK_LWIN);
        assert!(r.m.chord_active());
        r.m.set_keys(chord(&["F9"]), None, None);
        assert!(!r.m.chord_active());
        assert_eq!(r.up(VK_LWIN).event, None);
    }
}
