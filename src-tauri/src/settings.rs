//! settings.json: read with defaults for anything missing, written atomically. Settings saved by
//! older versions are moved to today's shape as they are read (`migrate`).

use crate::paths::Paths;
use crate::types::{Settings, VoiceCommandSettings};
use anyhow::Result;

pub fn load(paths: &Paths) -> Settings {
    match std::fs::read_to_string(&paths.settings) {
        Ok(text) => parse(&text).unwrap_or_else(|e| {
            log::warn!("settings.json unreadable ({e}); keeping a copy and starting from defaults");
            let _ = std::fs::copy(&paths.settings, paths.settings.with_extension("json.bad"));
            Settings::default()
        }),
        // First run: the name is filled in by setup from the Windows display name.
        Err(_) => Settings::default(),
    }
}

/// settings.json's text as Settings, migrated. Nothing is written here: the next save writes the
/// new shape, and migrating again until then is harmless.
fn parse(text: &str) -> serde_json::Result<Settings> {
    let mut value: serde_json::Value = serde_json::from_str(text)?;
    if migrate(&mut value) {
        log::info!("settings: the wake word moved to Voice commands");
    }
    let mut settings: Settings = serde_json::from_value(value)?;
    // An action added since the file was saved starts with its own phrases.
    settings.voice_commands.complete();
    Ok(settings)
}

/// Moves settings saved by older versions to today's shape: `heyNavi` {enabled, phrase} became
/// `voiceCommands`. True when anything changed.
pub fn migrate(value: &mut serde_json::Value) -> bool {
    let Some(obj) = value.as_object_mut() else { return false };
    let Some(old) = obj.remove("heyNavi") else { return false };
    // Voice commands already saved (by a newer Navi, before going back to an older one) win.
    if obj.get("voiceCommands").is_none_or(|v| v.is_null()) {
        let mut vc = VoiceCommandSettings { enabled: old.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false), ..Default::default() };
        // "navi" (the default) woke to the name alone and to "Hey Navi", so both stay; only
        // "hey-navi" was the one phrase.
        if old.get("phrase").and_then(|v| v.as_str()).is_some_and(|p| p.trim().eq_ignore_ascii_case("hey-navi")) {
            vc.set_phrases("wake", &["Hey Navi"]);
        }
        match serde_json::to_value(&vc) {
            Ok(v) => {
                obj.insert("voiceCommands".into(), v);
            }
            Err(e) => log::warn!("settings: couldn't move the wake word to Voice commands ({e})"),
        }
    }
    true
}

pub fn save(paths: &Paths, settings: &Settings) -> Result<()> {
    let tmp = paths.settings.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(settings)?)?;
    std::fs::rename(&tmp, &paths.settings)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn wake_of(value: &serde_json::Value) -> Vec<String> {
        let vc: VoiceCommandSettings = serde_json::from_value(value["voiceCommands"].clone()).expect("voice commands");
        vc.phrases("wake")
    }

    #[test]
    fn the_name_alone_keeps_both_wake_words_and_stays_on() {
        let mut v = json!({ "heyNavi": { "enabled": true, "phrase": "navi" }, "watch": { "everySecs": 20 } });
        assert!(migrate(&mut v));
        assert!(v.get("heyNavi").is_none());
        assert_eq!(v["voiceCommands"]["enabled"], true);
        assert_eq!(wake_of(&v), ["Navi", "Hey Navi"]);
        assert_eq!(v["watch"]["everySecs"], 20, "the rest is left alone");
        let vc: VoiceCommandSettings = serde_json::from_value(v["voiceCommands"].clone()).unwrap();
        assert_eq!(vc.commands, crate::types::default_voice_commands(), "every other command gets its defaults");
    }

    #[test]
    fn hey_navi_keeps_only_hey_navi() {
        for phrase in ["hey-navi", "HEY-NAVI "] {
            let mut v = json!({ "heyNavi": { "enabled": false, "phrase": phrase } });
            assert!(migrate(&mut v));
            assert_eq!(v["voiceCommands"]["enabled"], false);
            assert_eq!(wake_of(&v), ["Hey Navi"], "{phrase}");
        }
        // Odd or missing values get the defaults: off, both wake words.
        let mut v = json!({ "heyNavi": {} });
        assert!(migrate(&mut v));
        assert_eq!(v["voiceCommands"]["enabled"], false);
        assert_eq!(wake_of(&v), ["Navi", "Hey Navi"]);
    }

    #[test]
    fn nothing_to_move_changes_nothing() {
        let mut v = json!({ "general": { "theme": "dark" } });
        assert!(!migrate(&mut v));
        assert_eq!(v, json!({ "general": { "theme": "dark" } }));
        assert!(!migrate(&mut json!([1, 2])));
    }

    #[test]
    fn saved_voice_commands_win_over_the_old_wake_word() {
        let vc = json!({ "enabled": false, "commands": [{ "action": "wake", "phrases": ["Hey"] }] });
        let mut v = json!({ "heyNavi": { "enabled": true, "phrase": "hey-navi" }, "voiceCommands": vc.clone() });
        assert!(migrate(&mut v), "heyNavi is still removed");
        assert!(v.get("heyNavi").is_none());
        assert_eq!(v["voiceCommands"], vc);
    }

    #[test]
    fn an_old_settings_file_loads_with_everything_else_kept() {
        let paths = Paths::for_tests("settings-migrate");
        let old = json!({
            "version": 1,
            "onboarded": true,
            "userName": "Tom",
            "hotkeys": { "talk": ["Ctrl", "Alt"], "rambleKey": "Space" },
            "general": { "theme": "dark" },
            "watch": { "everySecs": 30 },
            "heyNavi": { "enabled": true, "phrase": "navi" }
        });
        std::fs::write(&paths.settings, serde_json::to_string_pretty(&old).unwrap()).unwrap();
        let s = load(&paths);
        assert!(s.onboarded);
        assert_eq!(s.user_name, "Tom");
        assert_eq!(s.general.theme, "dark");
        assert_eq!(s.hotkeys.talk, ["Ctrl", "Alt"]);
        assert_eq!(s.watch.every_secs, 30);
        assert!(s.voice_commands.enabled);
        assert_eq!(s.voice_commands.phrases("wake"), ["Navi", "Hey Navi"]);
        assert!(!paths.settings.with_extension("json.bad").exists(), "read cleanly");
        // Nothing is written at load; a save writes the new shape, which loads the same.
        assert!(std::fs::read_to_string(&paths.settings).unwrap().contains("heyNavi"));
        save(&paths, &s).unwrap();
        let text = std::fs::read_to_string(&paths.settings).unwrap();
        assert!(text.contains("voiceCommands") && !text.contains("heyNavi"));
        assert_eq!(load(&paths).voice_commands, s.voice_commands);
        let _ = std::fs::remove_dir_all(&paths.data);
    }

    #[test]
    fn a_file_from_before_colours_loads_as_lilac() {
        let paths = Paths::for_tests("settings-colour");
        std::fs::write(&paths.settings, r#"{ "onboarded": true, "general": { "theme": "dark", "showFirefly": false } }"#).unwrap();
        let s = load(&paths);
        assert_eq!(s.general.colour, "lilac");
        assert_eq!((s.general.theme.as_str(), s.general.show_firefly), ("dark", false), "the rest of General is kept");
        assert_eq!(s.voice_commands.phrases("colour"), ["turn", "change to", "switch to", "make it"], "the colour command gets its phrases");
        // A colour picked since is kept; one Navi doesn't have still loads, and shows as Lilac.
        save(&paths, &Settings { general: crate::types::GeneralSettings { colour: "gold".into(), ..Default::default() }, ..Default::default() }).unwrap();
        assert_eq!(load(&paths).general.colour, "gold");
        std::fs::write(&paths.settings, r#"{ "general": { "colour": "orange" } }"#).unwrap();
        assert_eq!(crate::colours::find(&load(&paths).general.colour).id, "lilac");
        let _ = std::fs::remove_dir_all(&paths.data);
    }

    #[test]
    fn fields_navi_no_longer_keeps_still_load_and_go_on_the_next_save() {
        let paths = Paths::for_tests("settings-dead-fields");
        let old = json!({
            "version": 1,
            "onboarded": true,
            "writing": { "british": false, "styleNotes": "Short and warm.", "styleSamples": ["C:\\Users\\you\\Documents\\letter.docx"] },
            "speak": { "voiceId": "v2", "voiceName": "George", "model": "eleven_v4" }
        });
        std::fs::write(&paths.settings, serde_json::to_string_pretty(&old).unwrap()).unwrap();
        let s = load(&paths);
        assert!(!paths.settings.with_extension("json.bad").exists(), "read cleanly");
        assert!(s.onboarded && !s.writing.british);
        assert_eq!(s.writing.style_notes, "Short and warm.");
        assert_eq!((s.speak.voice_id.as_str(), s.speak.model.as_str()), ("v2", "eleven_v4"));
        save(&paths, &s).unwrap();
        let text = std::fs::read_to_string(&paths.settings).unwrap();
        for gone in ["\"version\"", "styleSamples", "voiceName", "letter.docx"] {
            assert!(!text.contains(gone), "{gone} is still saved");
        }
        let _ = std::fs::remove_dir_all(&paths.data);
    }

    #[test]
    fn a_file_missing_a_command_gets_it_back_on_load() {
        let paths = Paths::for_tests("settings-complete");
        let text = r#"{ "voiceCommands": { "enabled": true, "commands": [{ "action": "wake", "phrases": ["Hey"] }, { "action": "finish", "phrases": [] }] } }"#;
        std::fs::write(&paths.settings, text).unwrap();
        let vc = load(&paths).voice_commands;
        assert_eq!(vc.commands.len(), crate::types::VOICE_ACTIONS.len());
        assert_eq!(vc.phrases("wake"), ["Hey"]);
        assert!(vc.phrases("finish").is_empty(), "turned off stays off");
        assert_eq!(vc.phrases("cancel"), ["don't worry"]);
        let _ = std::fs::remove_dir_all(&paths.data);
    }
}
