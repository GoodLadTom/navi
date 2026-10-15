//! The SQLite connection and schema.
//!
//! OWNER: data agent. STATUS: implemented, with unit tests.
//!
//! Contract: WAL mode, foreign keys on, a `meta` table with schema_version and forward-only
//! migrations. Tables: takes, words, snippets, suggestions, dismissed_pairs, modes (custom and
//! overridden built-ins as JSON), packs (summary + installed_at) and notes (meeting notes, schema
//! version 2; their ideas, jobs and screenshots in `extras` since version 3). Words/snippets/modes carry
//! `source` so removing a pack removes exactly what it installed. Full-text search over takes (FTS5 on
//! raw_text, final_text, app_name, window_title) when the bundled SQLite has it, else LIKE.
//!
//! How it is put together
//! * One connection behind a mutex. Every `Db` method takes the lock, so it is safe to share the
//!   `Db` between threads (wrap it in an `Arc`). Statements are short; nothing holds the lock while
//!   waiting on the network.
//! * Schema changes are numbered entries in `MIGRATIONS`. Never edit a shipped entry: add a new one.
//!   Each step runs in its own transaction together with the bump of `meta.schema_version`, so a
//!   failed step leaves the database exactly as it was. A database written by a newer Navi is
//!   refused with a clear message instead of being half understood.
//! * The FTS5 index is not a numbered migration because it depends on what the SQLite build can do.
//!   `ensure_fts` creates it (and fills it from existing takes) whenever it is missing and FTS5 is
//!   present, on every start.
//! * `secure_delete` is on, so deleted dictation text is overwritten rather than left in free pages.

use anyhow::{bail, Context, Result};
use chrono::{Local, SecondsFormat};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use std::path::Path;
use std::time::Duration;

pub struct Db {
    pub(crate) conn: Mutex<Connection>,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Nothing from the database goes into debug output: it holds the user's dictation history.
        f.debug_struct("Db").finish_non_exhaustive()
    }
}

/// Schema version this build reads and writes.
pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// Forward-only migrations. Entry 0 takes an empty database to schema version 1, entry 1 to
/// version 2, and so on.
const MIGRATIONS: &[&str] = &[MIGRATION_1, MIGRATION_2, MIGRATION_3];

const MIGRATION_1: &str = r#"
CREATE TABLE takes (
    seq          INTEGER PRIMARY KEY,
    id           TEXT NOT NULL UNIQUE,
    created_at   TEXT NOT NULL,
    ts           INTEGER NOT NULL,
    day          TEXT NOT NULL,
    duration_ms  INTEGER NOT NULL DEFAULT 0,
    app_name     TEXT NOT NULL DEFAULT '' COLLATE NOCASE,
    window_title TEXT NOT NULL DEFAULT '',
    mode_id      TEXT NOT NULL DEFAULT '',
    raw_text     TEXT NOT NULL DEFAULT '',
    final_text   TEXT NOT NULL DEFAULT '',
    words        INTEGER NOT NULL DEFAULT 0,
    audio_path   TEXT,
    model        TEXT NOT NULL DEFAULT '',
    latency_ms   INTEGER NOT NULL DEFAULT 0,
    status       TEXT NOT NULL DEFAULT '',
    kind         TEXT NOT NULL DEFAULT '',
    meta         TEXT NOT NULL DEFAULT 'null'
);
CREATE INDEX takes_by_time ON takes (ts DESC, seq DESC);
-- Covers every statistics query, so they read this small index instead of the full rows (which can
-- hold long ramble transcripts).
CREATE INDEX takes_by_day ON takes (day, status, words, duration_ms);
CREATE INDEX takes_by_app ON takes (app_name);

CREATE TABLE words (
    id          TEXT PRIMARY KEY,
    term        TEXT NOT NULL,
    term_key    TEXT NOT NULL UNIQUE,
    sounds_like TEXT NOT NULL DEFAULT '[]',
    note        TEXT NOT NULL DEFAULT '',
    source      TEXT NOT NULL DEFAULT 'manual',
    created_at  TEXT NOT NULL,
    uses        INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX words_by_source ON words (source);

CREATE TABLE snippets (
    id           TEXT PRIMARY KEY,
    trigger_text TEXT NOT NULL,
    trigger_key  TEXT NOT NULL UNIQUE,
    expansion    TEXT NOT NULL,
    source       TEXT NOT NULL DEFAULT 'manual',
    created_at   TEXT NOT NULL
);
CREATE INDEX snippets_by_source ON snippets (source);

CREATE TABLE suggestions (
    id        TEXT PRIMARY KEY,
    from_text TEXT NOT NULL,
    from_key  TEXT NOT NULL,
    to_text   TEXT NOT NULL,
    hits      INTEGER NOT NULL DEFAULT 1,
    app_name  TEXT NOT NULL DEFAULT '',
    last_seen TEXT NOT NULL,
    UNIQUE (from_key, to_text)
);

CREATE TABLE dismissed_pairs (
    from_key     TEXT NOT NULL,
    to_text      TEXT NOT NULL,
    dismissed_at TEXT NOT NULL,
    PRIMARY KEY (from_key, to_text)
) WITHOUT ROWID;

CREATE TABLE modes (
    seq    INTEGER PRIMARY KEY AUTOINCREMENT,
    id     TEXT NOT NULL UNIQUE,
    source TEXT NOT NULL DEFAULT 'manual',
    json   TEXT NOT NULL
);
CREATE INDEX modes_by_source ON modes (source);

CREATE TABLE packs (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    author       TEXT NOT NULL DEFAULT '',
    description  TEXT NOT NULL DEFAULT '',
    version      TEXT NOT NULL DEFAULT '',
    source_url   TEXT,
    installed_at TEXT NOT NULL
);

INSERT OR IGNORE INTO meta (key, value) VALUES ('created_at', strftime('%Y-%m-%dT%H:%M:%SZ', 'now'));
"#;

/// Meeting notes (see store/notes.rs): one row per call, the transcript as a JSON list of lines.
const MIGRATION_2: &str = r#"
CREATE TABLE notes (
    seq          INTEGER PRIMARY KEY,
    id           TEXT NOT NULL UNIQUE,
    created_at   TEXT NOT NULL,
    ts           INTEGER NOT NULL,
    title        TEXT NOT NULL DEFAULT '',
    app          TEXT NOT NULL DEFAULT '',
    duration_ms  INTEGER NOT NULL DEFAULT 0,
    status       TEXT NOT NULL DEFAULT 'done',
    transcript   TEXT NOT NULL DEFAULT '[]',
    my_notes     TEXT NOT NULL DEFAULT '',
    notes_md     TEXT NOT NULL DEFAULT '',
    file_path    TEXT
);
CREATE INDEX notes_ts ON notes (ts DESC);
"#;

/// Short ideas, call jobs and screenshots for each note, as one JSON object (see store/notes.rs).
const MIGRATION_3: &str = r#"
ALTER TABLE notes ADD COLUMN extras TEXT NOT NULL DEFAULT '{}';
"#;

/// The full-text index over takes: an external-content FTS5 table (the text stays in `takes`, the
/// index only holds tokens) kept in step by triggers. `seq` is a real INTEGER PRIMARY KEY so the
/// row numbers survive VACUUM.
const FTS_SCHEMA: &str = r#"
CREATE VIRTUAL TABLE takes_fts USING fts5(
    raw_text, final_text, app_name, window_title,
    content = 'takes', content_rowid = 'seq',
    tokenize = 'unicode61 remove_diacritics 2'
);
CREATE TRIGGER takes_fts_ai AFTER INSERT ON takes BEGIN
    INSERT INTO takes_fts (rowid, raw_text, final_text, app_name, window_title)
    VALUES (new.seq, new.raw_text, new.final_text, new.app_name, new.window_title);
END;
CREATE TRIGGER takes_fts_ad AFTER DELETE ON takes BEGIN
    INSERT INTO takes_fts (takes_fts, rowid, raw_text, final_text, app_name, window_title)
    VALUES ('delete', old.seq, old.raw_text, old.final_text, old.app_name, old.window_title);
END;
CREATE TRIGGER takes_fts_au AFTER UPDATE OF raw_text, final_text, app_name, window_title ON takes BEGIN
    INSERT INTO takes_fts (takes_fts, rowid, raw_text, final_text, app_name, window_title)
    VALUES ('delete', old.seq, old.raw_text, old.final_text, old.app_name, old.window_title);
    INSERT INTO takes_fts (rowid, raw_text, final_text, app_name, window_title)
    VALUES (new.seq, new.raw_text, new.final_text, new.app_name, new.window_title);
END;
"#;

impl Db {
    /// Opens (creating the folder and file if needed) and migrates the database at `path`.
    pub fn open(path: &Path) -> Result<Db> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir).with_context(|| format!("creating the folder {}", dir.display()))?;
            }
        }
        let conn = Connection::open(path).with_context(|| format!("opening the database {}", path.display()))?;
        Self::init(conn, true).with_context(|| format!("preparing the database {}", path.display()))
    }

    /// A private in-memory database with the full schema. Used by unit tests.
    pub fn open_memory() -> Result<Db> {
        Self::init(Connection::open_in_memory()?, false)
    }

    fn init(mut conn: Connection, on_disk: bool) -> Result<Db> {
        // Wait up to five seconds for another writer instead of failing straight away.
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.pragma_update(None, "secure_delete", "ON")?;
        if on_disk {
            // WAL lets readers and the writer work side by side and survives a crash. Where the
            // folder cannot do WAL (some network shares) SQLite keeps its default journal.
            let _ = conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0));
            conn.pragma_update(None, "synchronous", "NORMAL")?;
        }
        migrate(&mut conn, MIGRATIONS)?;
        ensure_fts(&mut conn)?;
        Ok(Db { conn: Mutex::new(conn) })
    }

    /// The schema version stored in the database (equal to `SCHEMA_VERSION` after `open`).
    pub fn schema_version(&self) -> Result<i64> {
        read_schema_version(&self.conn.lock())
    }

    /// Whether history search runs on the FTS5 index (true with the bundled SQLite) or on LIKE.
    pub fn fts_enabled(&self) -> bool {
        has_fts(&self.conn.lock())
    }

    /// Small bits of app state that do not deserve their own table (for example the time of the
    /// last pack refresh). `schema_version` is reserved.
    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock();
        Ok(conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| row.get::<_, String>(0))
            .optional()?)
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        let key = key.trim();
        if key.is_empty() {
            bail!("A meta key cannot be empty.");
        }
        if key == "schema_version" {
            bail!("schema_version is managed by Navi's migrations and cannot be set by hand.");
        }
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }

    /// Folds the write-ahead log back into the main file and truncates it. Best effort: call it
    /// when the app is quitting; failure is harmless.
    pub fn checkpoint(&self) {
        let conn = self.conn.lock();
        let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
    }
}

// ---------------------------------------------------------------------------------------------
// Helpers shared by the other store files
// ---------------------------------------------------------------------------------------------

/// Now as RFC 3339 in local time with the offset, e.g. "2026-10-14T13:05:09+01:00".
pub fn now_rfc3339() -> String {
    Local::now().to_rfc3339_opts(SecondsFormat::Secs, false)
}

/// A fresh random id.
pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub(crate) fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

pub(crate) fn to_u64(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

pub(crate) fn to_u32(v: i64) -> u32 {
    u32::try_from(v.max(0)).unwrap_or(u32::MAX)
}

pub(crate) fn has_fts(conn: &Connection) -> bool {
    conn.query_row("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'takes_fts'", [], |_| Ok(()))
        .optional()
        .map(|found| found.is_some())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------------------------
// Migrations and the FTS index
// ---------------------------------------------------------------------------------------------

fn read_schema_version(conn: &Connection) -> Result<i64> {
    let stored: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |row| row.get(0))
        .optional()?;
    Ok(stored.and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0).max(0))
}

/// Applies every migration the database has not seen yet, each in its own transaction.
fn migrate(conn: &mut Connection, migrations: &[&str]) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;")?;
    let latest = migrations.len() as i64;
    loop {
        // IMMEDIATE takes the write lock up front, so two copies of Navi starting together
        // cannot both run the same step. The version is re-read once the lock is held.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = read_schema_version(&tx)?;
        if current > latest {
            bail!(
                "This data was saved by a newer version of Navi (database version {current}, this copy understands up to {latest}). Update Navi to open it."
            );
        }
        if current == latest {
            return Ok(());
        }
        let next = current + 1;
        let sql = migrations[usize::try_from(current).unwrap_or(0)];
        tx.execute_batch(sql).with_context(|| format!("upgrading the database to version {next}"))?;
        tx.execute(
            "INSERT INTO meta (key, value) VALUES ('schema_version', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [next.to_string()],
        )?;
        tx.commit()?;
    }
}

/// True when this SQLite build can create FTS5 tables. A temp table is used so the probe never
/// touches the user's data.
fn fts5_compiled_in(conn: &Connection) -> bool {
    if conn.execute_batch("CREATE VIRTUAL TABLE temp.navi_fts5_probe USING fts5(x)").is_err() {
        return false;
    }
    let _ = conn.execute_batch("DROP TABLE temp.navi_fts5_probe");
    true
}

/// Makes the search index match what this SQLite build can do.
fn ensure_fts(conn: &mut Connection) -> Result<()> {
    let present = has_fts(conn);
    if !fts5_compiled_in(conn) {
        if present {
            // The index cannot be maintained here, so stop the triggers from touching it. Inserts
            // must keep working; search falls back to LIKE.
            conn.execute_batch(
                "DROP TRIGGER IF EXISTS takes_fts_ai; DROP TRIGGER IF EXISTS takes_fts_ad; DROP TRIGGER IF EXISTS takes_fts_au;",
            )?;
        }
        return Ok(());
    }
    if present {
        return Ok(());
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(FTS_SCHEMA).context("creating the search index")?;
    // Index whatever is already in the table (a database made before the index existed).
    tx.execute_batch("INSERT INTO takes_fts (takes_fts) VALUES ('rebuild');").context("filling the search index")?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testutil::TempDir;

    fn table_exists(conn: &Connection, name: &str) -> bool {
        conn.query_row("SELECT 1 FROM sqlite_master WHERE name = ?1", [name], |_| Ok(())).optional().unwrap().is_some()
    }

    #[test]
    fn memory_db_is_at_the_latest_schema_version() {
        let db = Db::open_memory().unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        let conn = db.conn.lock();
        for table in ["meta", "takes", "words", "snippets", "suggestions", "dismissed_pairs", "modes", "packs"] {
            assert!(table_exists(&conn, table), "missing table {table}");
        }
    }

    #[test]
    fn bundled_sqlite_has_fts5_and_the_index_is_created() {
        let db = Db::open_memory().unwrap();
        assert!(fts5_compiled_in(&db.conn.lock()), "the bundled SQLite should include FTS5");
        assert!(db.fts_enabled());
    }

    #[test]
    fn open_creates_missing_folders_and_keeps_data_across_reopens() {
        let dir = TempDir::new("db-reopen");
        let path = dir.join("deep").join("er").join("navi.db");
        {
            let db = Db::open(&path).unwrap();
            db.meta_set("hello", "world").unwrap();
        }
        assert!(path.exists());
        let db = Db::open(&path).unwrap();
        assert_eq!(db.meta_get("hello").unwrap().as_deref(), Some("world"));
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn file_db_uses_wal_foreign_keys_and_secure_delete() {
        let dir = TempDir::new("db-pragmas");
        let db = Db::open(&dir.join("navi.db")).unwrap();
        let conn = db.conn.lock();
        let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
        let fk: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap();
        assert_eq!(fk, 1);
        let secure: i64 = conn.query_row("PRAGMA secure_delete", [], |r| r.get(0)).unwrap();
        assert_eq!(secure, 1);
    }

    #[test]
    fn a_database_from_a_newer_navi_is_refused_with_a_clear_message() {
        let dir = TempDir::new("db-newer");
        let path = dir.join("navi.db");
        {
            let db = Db::open(&path).unwrap();
            db.conn
                .lock()
                .execute("UPDATE meta SET value = ?1 WHERE key = 'schema_version'", [(SCHEMA_VERSION + 5).to_string()])
                .unwrap();
        }
        let err = Db::open(&path).expect_err("opening a newer database must fail");
        let text = format!("{err:#}");
        assert!(text.contains("newer version of Navi"), "unexpected message: {text}");
    }

    #[test]
    fn migrations_apply_in_order_and_only_once() {
        let mut conn = Connection::open_in_memory().unwrap();
        let v1 = "CREATE TABLE a (x INTEGER);";
        let v2 = "CREATE TABLE b (y INTEGER); INSERT INTO a (x) VALUES (1);";
        migrate(&mut conn, &[v1]).unwrap();
        assert_eq!(read_schema_version(&conn).unwrap(), 1);
        assert!(!table_exists(&conn, "b"));
        migrate(&mut conn, &[v1, v2]).unwrap();
        assert_eq!(read_schema_version(&conn).unwrap(), 2);
        assert!(table_exists(&conn, "b"));
        // Running again changes nothing (v2 would insert a second row if it ran twice).
        migrate(&mut conn, &[v1, v2]).unwrap();
        let rows: i64 = conn.query_row("SELECT COUNT(*) FROM a", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn a_failing_migration_leaves_the_database_untouched() {
        let mut conn = Connection::open_in_memory().unwrap();
        let v1 = "CREATE TABLE a (x INTEGER);";
        let bad = "CREATE TABLE half_done (z INTEGER); THIS IS NOT SQL;";
        migrate(&mut conn, &[v1]).unwrap();
        let err = migrate(&mut conn, &[v1, bad]).unwrap_err();
        assert!(format!("{err:#}").contains("version 2"), "error should name the step: {err:#}");
        assert_eq!(read_schema_version(&conn).unwrap(), 1);
        assert!(!table_exists(&conn, "half_done"), "the failed step must roll back completely");
    }

    #[test]
    fn meta_round_trip_and_reserved_key() {
        let db = Db::open_memory().unwrap();
        assert_eq!(db.meta_get("missing").unwrap(), None);
        db.meta_set("last_refresh", "2026-10-14").unwrap();
        db.meta_set("last_refresh", "2026-10-15").unwrap();
        assert_eq!(db.meta_get("last_refresh").unwrap().as_deref(), Some("2026-10-15"));
        assert!(db.meta_set("schema_version", "99").is_err());
        assert!(db.meta_set("  ", "x").is_err());
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn the_search_index_is_rebuilt_when_it_is_missing() {
        let dir = TempDir::new("db-fts-rebuild");
        let path = dir.join("navi.db");
        {
            let db = Db::open(&path).unwrap();
            db.insert_take(&crate::store::testutil::take("a", "2026-10-14T09:00:00+01:00", 3)).unwrap();
            // Pretend this database was made before the index existed.
            db.conn
                .lock()
                .execute_batch(
                    "DROP TRIGGER takes_fts_ai; DROP TRIGGER takes_fts_ad; DROP TRIGGER takes_fts_au; DROP TABLE takes_fts;",
                )
                .unwrap();
            assert!(!db.fts_enabled());
        }
        let db = Db::open(&path).unwrap();
        assert!(db.fts_enabled(), "reopening should recreate the index");
        let hits = db
            .list_takes(&crate::types::HistoryQuery { search: Some("final".into()), ..Default::default() })
            .unwrap();
        assert_eq!(hits.len(), 1, "existing takes must be searchable after the rebuild");
    }

    #[test]
    fn db_can_be_shared_between_threads_and_prints_nothing_private() {
        fn needs_send_and_sync<T: Send + Sync>() {}
        needs_send_and_sync::<Db>();
        let db = Db::open_memory().unwrap();
        db.insert_take(&crate::store::testutil::take("secret-id", "2026-10-14T09:00:00+01:00", 1)).unwrap();
        let shown = format!("{db:?}");
        assert!(shown.starts_with("Db"), "{shown}");
        assert!(!shown.contains("secret-id") && !shown.contains("final"), "{shown}");
    }

    #[test]
    fn a_shared_db_can_be_used_from_many_threads_at_once() {
        use crate::store::testutil::take;
        use crate::types::HistoryQuery;
        let db = std::sync::Arc::new(Db::open_memory().unwrap());
        let workers: Vec<_> = (0..4)
            .map(|t| {
                let db = std::sync::Arc::clone(&db);
                std::thread::spawn(move || {
                    for i in 0..50 {
                        db.insert_take(&take(&format!("t{t}-{i}"), "2026-10-14T09:00:00+01:00", 3)).unwrap();
                        db.list_takes(&HistoryQuery { search: Some("final".into()), ..Default::default() }).unwrap();
                        db.stats().unwrap();
                        db.bump_word_uses(&["nothing".to_string()]).unwrap();
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().expect("a worker thread panicked");
        }
        let all = db.list_takes(&HistoryQuery { limit: 1_000, ..Default::default() }).unwrap();
        assert_eq!(all.len(), 200);
    }

    #[test]
    fn two_connections_to_one_file_do_not_lock_each_other_out() {
        use crate::store::testutil::take;
        use crate::types::HistoryQuery;
        let dir = TempDir::new("db-two-connections");
        let path = dir.join("navi.db");
        let first = Db::open(&path).unwrap();
        let second = Db::open(&path).unwrap();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for i in 0..100 {
                    first.insert_take(&take(&format!("a{i}"), "2026-10-14T09:00:00+01:00", 1)).unwrap();
                }
            });
            scope.spawn(|| {
                for i in 0..100 {
                    second.insert_take(&take(&format!("b{i}"), "2026-10-14T09:00:00+01:00", 1)).unwrap();
                    second.list_takes(&HistoryQuery::default()).unwrap();
                }
            });
        });
        let seen_by_first = first.list_takes(&HistoryQuery { limit: 1_000, ..Default::default() }).unwrap();
        let seen_by_second = second.list_takes(&HistoryQuery { limit: 1_000, ..Default::default() }).unwrap();
        assert_eq!((seen_by_first.len(), seen_by_second.len()), (200, 200));
    }

    #[test]
    fn everything_survives_a_restart() {
        use crate::store::testutil::take;
        use crate::types::{DictEntry, HistoryQuery, Mode, ModeKind, Pack, Snippet};
        let dir = TempDir::new("db-restart");
        let path = dir.join("navi.db");
        {
            let db = Db::open(&path).unwrap();
            db.insert_take(&take("t1", "2026-10-14T09:00:00+01:00", 4)).unwrap();
            db.upsert_word(&DictEntry { term: "Quikly".into(), sounds_like: vec!["quickly".into()], ..Default::default() }).unwrap();
            db.upsert_snippet(&Snippet { trigger: "hello".into(), expansion: "world".into(), ..Default::default() }).unwrap();
            db.record_suggestion("tory", "Tauri", "Code").unwrap();
            let dismissed = db.record_suggestion("whisker", "Whisper", "").unwrap().unwrap();
            db.dismiss_suggestion(&dismissed.id).unwrap();
            db.save_mode(&Mode {
                id: String::new(),
                name: "Brief".into(),
                icon: "pen".into(),
                description: String::new(),
                kind: ModeKind::Rewrite,
                instructions: "Short.".into(),
                apps: vec![],
                use_ai: true,
                builtin: false,
            })
            .unwrap();
            db.install_pack(&Pack {
                id: "team".into(),
                name: "Team".into(),
                version: "1".into(),
                words: vec![DictEntry { term: "Figma".into(), ..Default::default() }],
                ..Default::default()
            })
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        assert_eq!(db.list_takes(&HistoryQuery { search: Some("final".into()), ..Default::default() }).unwrap().len(), 1);
        assert_eq!(db.list_words().unwrap().len(), 2);
        assert_eq!(db.list_snippets().unwrap().len(), 1);
        assert_eq!(db.list_suggestions().unwrap().len(), 1);
        assert!(db.record_suggestion("whisker", "Whisper", "").unwrap().is_none(), "the dismissal was remembered");
        assert_eq!(db.list_stored_modes().unwrap()[0].name, "Brief");
        assert_eq!(db.list_packs().unwrap()[0].words, 1);
        assert_eq!(db.stats_on(chrono::NaiveDate::from_ymd_opt(2026, 10, 14).unwrap()).unwrap().words_today, 4);
    }

    #[test]
    fn helpers_clamp_numbers_instead_of_wrapping() {
        assert_eq!(to_i64(u64::MAX), i64::MAX);
        assert_eq!(to_u64(-5), 0);
        assert_eq!(to_u32(-1), 0);
        assert_eq!(to_u32(i64::MAX), u32::MAX);
        assert_eq!(to_u32(42), 42);
    }

    #[test]
    fn now_is_rfc3339_with_an_offset() {
        let now = now_rfc3339();
        assert!(chrono::DateTime::parse_from_rfc3339(&now).is_ok(), "{now}");
        assert_eq!(new_id().len(), 36);
    }
}
