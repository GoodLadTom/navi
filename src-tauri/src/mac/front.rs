//! The app in front: its pid, bundle id, name and the "process name" Navi keys modes and
//! categories on. Known Mac apps get the name their Windows version runs as ("code.exe" for VS
//! Code), so modes, categories and settings carry over from Windows unchanged; any other app is
//! its executable's name in lower case ("granola").

use objc2_app_kit::NSWorkspace;

#[derive(Clone, Debug, Default)]
pub struct FrontApp {
    pub pid: i32,
    pub bundle_id: String,
    /// The app's own name ("Visual Studio Code").
    pub name: String,
    /// What Navi keys on: "code.exe", "terminal", "granola".
    pub process_name: String,
    /// The .app bundle's path.
    pub path: String,
}

pub fn front_app() -> Option<FrontApp> {
    let app = front_pid()
        .and_then(|pid| objc2_app_kit::NSRunningApplication::runningApplicationWithProcessIdentifier(pid))
        .or_else(|| NSWorkspace::sharedWorkspace().frontmostApplication())?;
    let pid = app.processIdentifier();
    let bundle_id = app.bundleIdentifier().map(|s| s.to_string()).unwrap_or_default();
    let name = app.localizedName().map(|s| s.to_string()).unwrap_or_default();
    let exec = app.executableURL()
        .and_then(|u| u.lastPathComponent())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let path = app.bundleURL().and_then(|u| u.path()).map(|s| s.to_string()).unwrap_or_default();
    Some(FrontApp { pid, process_name: process_name_for(&bundle_id, &exec, &name), bundle_id, name, path })
}

/// The owner of the frontmost normal window on screen, from the window server.
#[cfg(test)]
fn first_window_owner() -> Option<i32> {
    use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType};
    use objc2_core_graphics::{kCGNullWindowID, CGWindowListCopyWindowInfo, CGWindowListOption};
    let list = CGWindowListCopyWindowInfo(CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements, kCGNullWindowID)?;
    let list: CFRetained<CFArray<CFDictionary<CFString, CFType>>> = unsafe { CFRetained::cast_unchecked(list) };
    let num = |d: &CFDictionary<CFString, CFType>, k: &str| d.get(&CFString::from_str(k)).and_then(|v| v.downcast_ref::<CFNumber>().and_then(|n| n.as_i64()));
    list.iter().find(|d| num(d, "kCGWindowLayer") == Some(0)).and_then(|d| num(&d, "kCGWindowOwnerPID")).map(|p| p as i32)
}

/// The front process as the Process Manager says (deprecated by Apple, but asks the system each
/// time rather than waiting on notifications).
pub fn process_manager_front() -> Option<i32> {
    #[repr(C)]
    struct Psn {
        high: u32,
        low: u32,
    }
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn GetFrontProcess(psn: *mut Psn) -> i32;
        fn GetProcessPID(psn: *const Psn, pid: *mut i32) -> i32;
    }
    let mut psn = Psn { high: 0, low: 0 };
    let mut pid = 0;
    unsafe { (GetFrontProcess(&mut psn) == 0 && GetProcessPID(&psn, &mut pid) == 0 && pid > 0).then_some(pid) }
}

/// The pid of the app in front, from the Process Manager (see `process_manager_front`), else
/// NSWorkspace's. NSWorkspace's answer only moves on while a run loop handles its notifications,
/// so off the main thread it can be out of date; Accessibility's focused app sometimes gives
/// nothing; the window list follows windows, not focus. The Process Manager matched macOS's own
/// record (`lsappinfo front`) every time (live_front_app_methods).
pub fn front_pid() -> Option<i32> {
    process_manager_front().or_else(|| NSWorkspace::sharedWorkspace().frontmostApplication().map(|a| a.processIdentifier()))
}

/// Bundle ids of Mac apps whose Windows version Navi already knows, with that Windows process
/// name. Matching is on the bundle id's start, so "com.microsoft.teams2" matches
/// "com.microsoft.teams".
const KNOWN: &[(&str, &str)] = &[
    ("com.microsoft.VSCodeInsiders", "code - insiders.exe"),
    ("com.microsoft.VSCode", "code.exe"),
    ("com.vscodium", "codium.exe"),
    ("com.todesktop.230313mzl4w4u92", "cursor.exe"),
    ("com.exafunction.windsurf", "windsurf.exe"),
    ("dev.zed.Zed", "zed.exe"),
    ("com.sublimetext", "sublime_text.exe"),
    ("com.jetbrains.intellij", "idea64.exe"),
    ("com.jetbrains.pycharm", "pycharm64.exe"),
    ("com.jetbrains.WebStorm", "webstorm64.exe"),
    ("com.jetbrains.rider", "rider64.exe"),
    ("com.jetbrains.rustrover", "rustrover64.exe"),
    ("com.google.android.studio", "studio64.exe"),
    ("com.anthropic.claudefordesktop", "claude.exe"),
    ("com.openai.chat", "chatgpt.exe"),
    ("com.openai.codex", "codex.exe"),
    ("dev.warp.Warp", "warp.exe"),
    ("io.alacritty", "alacritty.exe"),
    ("com.github.wez.wezterm", "wezterm-gui.exe"),
    ("net.kovidgoyal.kitty", "kitty.exe"),
    ("co.zeit.hyper", "hyper.exe"),
    ("org.tabby", "tabby.exe"),
    ("com.tinyspeck.slackmacgap", "slack.exe"),
    ("com.microsoft.teams", "ms-teams.exe"),
    ("com.hnc.Discord", "discord.exe"),
    ("net.whatsapp.WhatsApp", "whatsapp.exe"),
    ("desktop.WhatsApp", "whatsapp.exe"),
    ("ru.keepcoder.Telegram", "telegram.exe"),
    ("org.telegram.desktop", "telegram.exe"),
    ("org.whispersystems.signal-desktop", "signal.exe"),
    ("com.facebook.archon", "messenger.exe"),
    ("im.riot.app", "element.exe"),
    ("com.microsoft.Outlook", "outlook.exe"),
    ("org.mozilla.thunderbird", "thunderbird.exe"),
    ("com.mailspring", "mailspring.exe"),
    ("com.google.Chrome", "chrome.exe"),
    ("org.chromium.Chromium", "chromium.exe"),
    ("com.microsoft.edgemac", "msedge.exe"),
    ("org.mozilla.firefox", "firefox.exe"),
    ("com.brave.Browser", "brave.exe"),
    ("company.thebrowser.Browser", "arc.exe"),
    ("com.operasoftware.Opera", "opera.exe"),
    ("com.vivaldi.Vivaldi", "vivaldi.exe"),
    ("app.zen-browser.zen", "zen.exe"),
    ("com.microsoft.Word", "winword.exe"),
    ("com.microsoft.Excel", "excel.exe"),
    ("com.microsoft.Powerpoint", "powerpnt.exe"),
    ("com.microsoft.onenote.mac", "onenote.exe"),
    ("notion.id", "notion.exe"),
    ("md.obsidian", "obsidian.exe"),
    ("abnerworks.Typora", "typora.exe"),
    ("com.electron.logseq", "logseq.exe"),
    ("com.evernote.Evernote", "evernote.exe"),
    ("org.libreoffice.script", "soffice.bin"),
    ("us.zoom.xos", "zoom.exe"),
    ("com.goodlad.navi", "navi.exe"),
    // Apple's own apps, which have no Windows version: named for the Mac lists in `context`.
    ("com.apple.Terminal", "terminal"),
    ("com.googlecode.iterm2", "iterm2"),
    ("com.mitchellh.ghostty", "ghostty"),
    ("com.apple.Safari", "safari"),
    ("com.apple.SafariTechnologyPreview", "safari"),
    ("com.kagi.kagimacOS", "orion"),
    ("company.thebrowser.dia", "dia"),
    ("com.apple.mail", "mail"),
    ("com.readdle.smartemail-Mac", "spark"),
    ("com.superhuman.electron", "superhuman"),
    ("com.apple.MobileSMS", "messages"),
    ("com.apple.Notes", "notes"),
    ("com.apple.TextEdit", "textedit"),
    ("com.apple.iWork.Pages", "pages"),
    ("com.apple.iWork.Keynote", "keynote"),
    ("com.apple.iWork.Numbers", "numbers"),
    ("com.apple.dt.Xcode", "xcode"),
    ("com.apple.finder", "finder"),
    ("com.apple.Spotlight", "spotlight"),
];

/// The name Navi keys an app on (see the module comment).
pub fn process_name_for(bundle_id: &str, executable: &str, name: &str) -> String {
    let id = bundle_id.to_ascii_lowercase();
    if !id.is_empty() {
        if let Some((_, known)) = KNOWN.iter().find(|(b, _)| id.starts_with(&b.to_ascii_lowercase())) {
            return known.to_string();
        }
    }
    let base = if executable.trim().is_empty() { name } else { executable };
    base.trim().to_lowercase()
}

/// The friendly label of a Mac app Navi named in `KNOWN` by its Mac name ("terminal" -> Terminal).
pub fn mac_label(process_name: &str) -> Option<&'static str> {
    Some(match process_name {
        "terminal" => "Terminal",
        "iterm2" => "iTerm2",
        "ghostty" => "Ghostty",
        "safari" => "Safari",
        "orion" => "Orion",
        "dia" => "Dia",
        "mail" => "Mail",
        "spark" => "Spark",
        "superhuman" => "Superhuman",
        "messages" => "Messages",
        "notes" => "Notes",
        "textedit" => "TextEdit",
        "pages" => "Pages",
        "keynote" => "Keynote",
        "numbers" => "Numbers",
        "xcode" => "Xcode",
        "finder" => "Finder",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_apps_take_their_windows_names() {
        assert_eq!(process_name_for("com.microsoft.VSCode", "Electron", "Code"), "code.exe");
        assert_eq!(process_name_for("com.microsoft.VSCodeInsiders", "Electron", "Code - Insiders"), "code - insiders.exe");
        assert_eq!(process_name_for("com.microsoft.teams2", "MSTeams", "Microsoft Teams"), "ms-teams.exe");
        assert_eq!(process_name_for("com.apple.Terminal", "Terminal", "Terminal"), "terminal");
        assert_eq!(process_name_for("com.google.Chrome", "Google Chrome", "Google Chrome"), "chrome.exe");
    }

    #[test]
    fn other_apps_are_their_executable() {
        assert_eq!(process_name_for("so.granola.app", "Granola", "Granola"), "granola");
        assert_eq!(process_name_for("", "", "Some App"), "some app");
    }

    /// Compares the ways of finding the app in front right after switching apps.
    #[test]
    #[ignore]
    fn live_front_app_methods() {
        use std::process::Command;
        for app in ["Safari", "Terminal", "TextEdit"] {
            Command::new("/usr/bin/osascript").args(["-e", &format!("tell application \"{app}\" to activate")]).output().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1500));
            let truth = String::from_utf8_lossy(&Command::new("/usr/bin/lsappinfo").args(["info", "-only", "pid", &String::from_utf8_lossy(&Command::new("/usr/bin/lsappinfo").arg("front").output().unwrap().stdout).trim().to_string()]).output().unwrap().stdout).to_string();
            let ax = crate::mac::ax::Element::system_wide().element("AXFocusedApplication").and_then(|a| a.pid());
            let ws = NSWorkspace::sharedWorkspace().frontmostApplication().map(|a| a.processIdentifier());
            let cg = first_window_owner();
            let pm = process_manager_front();
            println!("{app}: lsappinfo {} ax {ax:?} workspace {ws:?} window list {cg:?} process manager {pm:?}", truth.trim());
        }
    }

}
