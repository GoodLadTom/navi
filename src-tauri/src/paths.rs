//! Where Navi keeps its files. Everything lives under the per-user app data folder
//! (%APPDATA%\com.goodlad.navi), apart from notes, which default to Documents\Navi Notes.
//! Older installs keep their %APPDATA%\com.navaigate.navi folder (see `choose_data_dir`).

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

/// The data folder's name before the app identifier became com.goodlad.navi.
const OLD_DATA_FOLDER: &str = "com.navaigate.navi";

/// The data folder to use, given `new`, the app data folder for the current identifier
/// (…\com.goodlad.navi). Installs from before the identifier changed keep using their old folder
/// beside it (…\com.navaigate.navi), because the database stores the absolute paths of its
/// recordings, notes and screenshots: if `new` is missing (or empty) and the old folder exists,
/// the old one is used. Otherwise `new` is.
fn choose_data_dir(new: &Path) -> PathBuf {
    let new_is_unused = match std::fs::read_dir(new) {
        Ok(mut entries) => entries.next().is_none(),
        Err(_) => !new.exists(),
    };
    if new_is_unused {
        if let Some(old) = new.parent().map(|parent| parent.join(OLD_DATA_FOLDER)) {
            if old != new && old.is_dir() {
                return old;
            }
        }
    }
    new.to_path_buf()
}

#[derive(Clone, Debug)]
pub struct Paths {
    pub data: PathBuf,
    pub settings: PathBuf,
    pub db: PathBuf,
    pub audio: PathBuf,
    pub models: PathBuf,
    pub engine: PathBuf,
    pub logs: PathBuf,
    pub packs: PathBuf,
    pub notes_default: PathBuf,
    /// Bundled sounds (ui/sounds in development, the app resources once installed).
    pub sounds: PathBuf,
}

impl Paths {
    pub fn init(app: &AppHandle) -> Result<Self> {
        let new = app.path().app_data_dir().context("no app data folder")?;
        let data = choose_data_dir(&new);
        if data == new {
            log::info!("data folder: {}", data.display());
        } else {
            log::info!("data folder: {} (kept from an older install)", data.display());
            // The asset protocol's scope ($APPDATA) only covers the new folder; recordings and
            // screenshots in the old one must still load in the window.
            if let Err(e) = app.asset_protocol_scope().allow_directory(&data, true) {
                log::warn!("could not allow {} for the window: {e}", data.display());
            }
        }
        let p = Self {
            settings: data.join("settings.json"),
            db: data.join("navi.db"),
            audio: data.join("audio"),
            models: data.join("models"),
            engine: data.join("engine"),
            logs: data.join("logs"),
            packs: data.join("packs"),
            notes_default: dirs::document_dir().unwrap_or_else(|| data.clone()).join("Navi Notes"),
            sounds: Self::find_sounds(app),
            data,
        };
        for dir in [&p.data, &p.audio, &p.models, &p.engine, &p.logs, &p.packs] {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        Ok(p)
    }

    /// Paths for unit tests: a throwaway folder under the system temp directory.
    #[cfg(test)]
    pub fn for_tests(name: &str) -> Self {
        let data = std::env::temp_dir().join(format!("navi-test-{name}-{}", std::process::id()));
        let p = Self {
            settings: data.join("settings.json"),
            db: data.join("navi.db"),
            audio: data.join("audio"),
            models: data.join("models"),
            engine: data.join("engine"),
            logs: data.join("logs"),
            packs: data.join("packs"),
            notes_default: data.join("notes"),
            sounds: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("ui").join("sounds"),
            data,
        };
        for dir in [&p.data, &p.audio, &p.models, &p.engine, &p.logs, &p.packs, &p.notes_default] {
            let _ = std::fs::create_dir_all(dir);
        }
        p
    }

    fn find_sounds(app: &AppHandle) -> PathBuf {
        // Installed: sounds are bundled next to the frontend; in development, read the source folder.
        if let Ok(res) = app.path().resource_dir() {
            for candidate in [res.join("sounds"), res.join("ui").join("sounds"), res.join("_up_").join("ui").join("sounds")] {
                // Files every build ships. (It once looked for pop-start.wav, long since retired:
                // only leftovers from an older install made it work, and a fresh install fell back
                // to the build machine's source folder, so it had no sounds and no warm-up.)
                if candidate.join("sets").join("index.json").exists() || candidate.join("warmup.wav").exists() {
                    return candidate;
                }
            }
            log::warn!("no bundled sounds found in {}", res.display());
        }
        // Development builds read the source folder. Release builds never do: the path is the
        // build machine's own and would otherwise be baked into every installer.
        #[cfg(debug_assertions)]
        {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("ui").join("sounds")
        }
        #[cfg(not(debug_assertions))]
        {
            app.path().resource_dir().map(|res| res.join("sounds")).unwrap_or_default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_installs_keep_their_data_folder() {
        let base = std::env::temp_dir().join(format!("navi-test-data-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let new = base.join("com.goodlad.navi");
        let old = base.join(OLD_DATA_FOLDER);

        // A fresh install: neither folder yet.
        std::fs::create_dir_all(&base).expect("base");
        assert_eq!(choose_data_dir(&new), new);

        // An older install: only the old folder, with data in it.
        std::fs::create_dir_all(&old).expect("old");
        std::fs::write(old.join("navi.db"), b"x").expect("old db");
        assert_eq!(choose_data_dir(&new), old);

        // An empty new folder holds nothing to lose, so the old one still wins.
        std::fs::create_dir_all(&new).expect("new");
        assert_eq!(choose_data_dir(&new), old);

        // Once the new folder has data, it is used.
        std::fs::write(new.join("settings.json"), b"{}").expect("new settings");
        assert_eq!(choose_data_dir(&new), new);

        // Only the new folder.
        std::fs::remove_dir_all(&old).expect("remove old");
        assert_eq!(choose_data_dir(&new), new);

        // The identifier unchanged: the old folder is the new one.
        assert_eq!(choose_data_dir(&old), old);

        let _ = std::fs::remove_dir_all(&base);
    }
}
