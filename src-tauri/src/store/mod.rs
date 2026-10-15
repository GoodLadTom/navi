//! Local storage: one SQLite database (navi.db) plus the Windows Credential Manager for keys.
//!
//! OWNER: data agent. Every method is on `Db` (split across files by topic) and is synchronous;
//! callers run them on a blocking thread when they might be slow. Unit tests use Db::open_memory().

pub mod db;
pub mod dictionary;
pub mod history;
pub mod modes;
pub mod notes;
pub mod packs;
pub mod secrets;

pub use db::Db;

/// Helpers shared by the unit tests in this folder.
#[cfg(test)]
pub(crate) mod testutil {
    use crate::types::Take;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A throwaway folder under the system temp directory, removed when dropped. Declare it before
    /// any `Db` that lives inside it so the database is closed first.
    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new(label: &str) -> TempDir {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("navi-store-{label}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("create the temp folder");
            TempDir(dir)
        }

        pub fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A pasted take with sensible defaults; override fields with struct update syntax.
    pub fn take(id: &str, created_at: &str, words: u32) -> Take {
        Take {
            id: id.to_string(),
            created_at: created_at.to_string(),
            duration_ms: 10_000,
            app_name: "Notepad".into(),
            window_title: "notes.txt".into(),
            mode_id: "clean".into(),
            raw_text: format!("raw {id}"),
            final_text: format!("final {id}"),
            words,
            status: "pasted".into(),
            kind: "talk".into(),
            ..Take::default()
        }
    }
}
