//! settings.json: read with defaults for anything missing, written atomically.

use crate::paths::Paths;
use crate::types::Settings;
use anyhow::Result;

pub fn load(paths: &Paths) -> Settings {
    match std::fs::read_to_string(&paths.settings) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            log::warn!("settings.json unreadable ({e}); keeping a copy and starting from defaults");
            let _ = std::fs::copy(&paths.settings, paths.settings.with_extension("json.bad"));
            Settings::default()
        }),
        // First run: the name is filled in by setup from the Windows display name.
        Err(_) => Settings::default(),
    }
}

pub fn save(paths: &Paths, settings: &Settings) -> Result<()> {
    let tmp = paths.settings.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(settings)?)?;
    std::fs::rename(&tmp, &paths.settings)?;
    Ok(())
}
