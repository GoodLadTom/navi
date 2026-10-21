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
    let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;
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

/// The pid of the app in front.
pub fn front_pid() -> Option<i32> {
    NSWorkspace::sharedWorkspace().frontmostApplication().map(|a| a.processIdentifier())
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
}
