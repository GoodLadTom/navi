//! Word list, snippets and learned suggestions.
//!
//! OWNER: data agent. STATUS: implemented, with unit tests.
//!
//! Contract:
//! * `upsert_word` / `upsert_snippet`: insert when id is empty (generate a uuid, set created_at),
//!   update otherwise; return the stored row. Terms are unique case-insensitively: adding an
//!   existing term merges its sounds_like lists.
//! * `record_suggestion(from, to, app)`: same (from, to) pair increments count and last_seen;
//!   ignores pairs that already exist as a word (term == to and from in sounds_like).
//! * `accept_suggestion(id)`: adds/merges a word {term: to, sounds_like: [from], source: "learned"},
//!   deletes the suggestion, returns the word. `dismiss_suggestion` deletes it and remembers the
//!   pair so it is never suggested again.
//! * `vocabulary_prompt(max_chars)`: the most-used terms first, joined as "Quikly, Tauri, ..."
//!   to fit max_chars, for the speech engine's prompt.
//!
//! The finer rules
//! * Case: terms and triggers are compared with Unicode lower-casing ("Émile" = "émile"), after
//!   trimming and squeezing runs of spaces to one. `list_words` is alphabetical on that key.
//! * Adding a word whose term already exists (no id, or an id that is not in the list) merges:
//!   the sounds_like lists are joined (duplicates dropped, ignoring case), a non-empty new note
//!   replaces the old one, and the existing spelling is kept. Editing a row by its id replaces its
//!   fields exactly as given instead; renaming it onto another word's term is an error.
//! * `uses` and `created_at` belong to the store: edits and merges keep what is stored. New rows
//!   take `uses` from the caller (0 for the UI).
//! * Ownership: "manual" and "learned" rows are the user's own; any other `source` is a pack id.
//!   When the user adds or edits a row that came from a pack, it becomes theirs (source manual),
//!   so removing the pack later cannot take their edit away. Manual beats learned.
//! * Suggestions are keyed by (the engine's text lower-cased, the corrected text as typed), so
//!   "Quickly" and "quickly" corrected to "Quikly" count as the same lesson.
//! * `vocabulary_prompt` skips a term that does not fit and carries on with shorter ones.

use super::db::{new_id, now_rfc3339, to_u32};
use super::Db;
use crate::types::{DictEntry, Snippet, Suggestion};
use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension, Row, TransactionBehavior};
use std::collections::HashSet;

const WORD_COLS: &str = "id, term, sounds_like, note, source, created_at, uses";
const SNIPPET_COLS: &str = "id, trigger_text, expansion, source";
const SUGGESTION_COLS: &str = "id, from_text, to_text, hits, app_name, last_seen";

/// The `source` value of words and snippets the user typed in.
pub const SOURCE_MANUAL: &str = "manual";
/// The `source` value of words added by accepting a learned suggestion.
pub const SOURCE_LEARNED: &str = "learned";

impl Db {
    // ---- words ----

    /// Every word, alphabetical ignoring case.
    pub fn list_words(&self) -> Result<Vec<DictEntry>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&format!("SELECT {WORD_COLS} FROM words ORDER BY term_key, id"))?;
        let rows = stmt.query_map([], word_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<DictEntry>>>()?)
    }

    /// Adds a word (empty id), merges it into the existing word with the same term, or edits the
    /// word with this id. Returns the row as stored. See the notes at the top of this file.
    pub fn upsert_word(&self, word: &DictEntry) -> Result<DictEntry> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored = upsert_word_in(&tx, word)?;
        tx.commit()?;
        Ok(stored)
    }

    /// Deleting something that is not there is not an error.
    pub fn delete_word(&self, id: &str) -> Result<()> {
        self.conn.lock().execute("DELETE FROM words WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Adds one to `uses` for each term in the list (matched ignoring case). Terms that are not in
    /// the dictionary are ignored.
    pub fn bump_word_uses(&self, terms: &[String]) -> Result<()> {
        if terms.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        {
            let mut stmt = tx.prepare("UPDATE words SET uses = uses + 1 WHERE term_key = ?1")?;
            for term in terms {
                let key = clean_phrase(term).to_lowercase();
                if !key.is_empty() {
                    stmt.execute([key])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    // ---- snippets ----

    /// Every snippet, alphabetical by trigger ignoring case.
    pub fn list_snippets(&self) -> Result<Vec<Snippet>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&format!("SELECT {SNIPPET_COLS} FROM snippets ORDER BY trigger_key, id"))?;
        let rows = stmt.query_map([], snippet_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<Snippet>>>()?)
    }

    /// Adds a snippet (empty id), replaces the expansion of the snippet with the same trigger, or
    /// edits the snippet with this id. Returns the row as stored.
    pub fn upsert_snippet(&self, snippet: &Snippet) -> Result<Snippet> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored = upsert_snippet_in(&tx, snippet)?;
        tx.commit()?;
        Ok(stored)
    }

    pub fn delete_snippet(&self, id: &str) -> Result<()> {
        self.conn.lock().execute("DELETE FROM snippets WHERE id = ?1", [id])?;
        Ok(())
    }

    // ---- learned suggestions ----

    /// Most-seen first, then most recent.
    pub fn list_suggestions(&self) -> Result<Vec<Suggestion>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&format!("SELECT {SUGGESTION_COLS} FROM suggestions ORDER BY hits DESC, last_seen DESC, id"))?;
        let rows = stmt.query_map([], suggestion_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<Suggestion>>>()?)
    }

    /// Notes that the user changed `from` (what the engine wrote) into `to` (what they typed) in
    /// `app`. Returns the suggestion with its updated count, or None when the pair is not worth
    /// suggesting: empty or unchanged text, already covered by a word, or dismissed before.
    pub fn record_suggestion(&self, from: &str, to: &str, app: &str) -> Result<Option<Suggestion>> {
        let from = clean_phrase(from);
        let to = clean_phrase(to);
        if from.is_empty() || to.is_empty() || from == to {
            return Ok(None);
        }
        let from_key = from.to_lowercase();
        let app = app.trim();

        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let dismissed = tx
            .query_row("SELECT 1 FROM dismissed_pairs WHERE from_key = ?1 AND to_text = ?2", params![from_key, to], |_| Ok(()))
            .optional()?
            .is_some();
        if dismissed {
            return Ok(None);
        }
        if let Some(word) = word_by_key(&tx, &to.to_lowercase())? {
            if word.sounds_like.iter().any(|s| s.to_lowercase() == from_key) {
                return Ok(None);
            }
        }

        let now = now_rfc3339();
        let existing: Option<String> = tx
            .query_row("SELECT id FROM suggestions WHERE from_key = ?1 AND to_text = ?2", params![from_key, to], |row| row.get(0))
            .optional()?;
        let id = match existing {
            Some(id) => {
                tx.execute(
                    "UPDATE suggestions SET hits = hits + 1, last_seen = ?2, app_name = CASE WHEN ?3 <> '' THEN ?3 ELSE app_name END WHERE id = ?1",
                    params![id, now, app],
                )?;
                id
            }
            None => {
                let id = new_id();
                tx.execute(
                    "INSERT INTO suggestions (id, from_text, from_key, to_text, hits, app_name, last_seen) VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6)",
                    params![id, from, from_key, to, app, now],
                )?;
                id
            }
        };
        let stored = suggestion_by_id(&tx, &id)?;
        tx.commit()?;
        Ok(stored)
    }

    /// Turns a suggestion into a dictionary word (or adds the sound-alike to the word that is
    /// already there) and removes the suggestion. None if the suggestion no longer exists.
    pub fn accept_suggestion(&self, id: &str) -> Result<Option<DictEntry>> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(suggestion) = suggestion_by_id(&tx, id)? else {
            return Ok(None);
        };
        let word = DictEntry {
            term: suggestion.to,
            sounds_like: vec![suggestion.from],
            source: SOURCE_LEARNED.to_string(),
            ..DictEntry::default()
        };
        let stored = upsert_word_in(&tx, &word)?;
        tx.execute("DELETE FROM suggestions WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(Some(stored))
    }

    /// Removes a suggestion and remembers the pair so it is never suggested again.
    pub fn dismiss_suggestion(&self, id: &str) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pair: Option<(String, String)> = tx
            .query_row("SELECT from_key, to_text FROM suggestions WHERE id = ?1", [id], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?;
        if let Some((from_key, to)) = pair {
            tx.execute(
                "INSERT OR IGNORE INTO dismissed_pairs (from_key, to_text, dismissed_at) VALUES (?1, ?2, ?3)",
                params![from_key, to, now_rfc3339()],
            )?;
            tx.execute("DELETE FROM suggestions WHERE id = ?1", [id])?;
        }
        tx.commit()?;
        Ok(())
    }

    // ---- the speech engine's prompt ----

    /// The dictionary terms as "Quikly, Tauri, ...", most used first (newest first among equals),
    /// at most `max_chars` characters long.
    pub fn vocabulary_prompt(&self, max_chars: usize) -> Result<String> {
        if max_chars == 0 {
            return Ok(String::new());
        }
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT term FROM words ORDER BY uses DESC, created_at DESC, term_key ASC")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = String::new();
        let mut used = 0_usize;
        for term in rows {
            let term = term?;
            let separator = if out.is_empty() { 0 } else { 2 };
            let needed = term.chars().count() + separator;
            if used + needed > max_chars {
                // Stop once not even a one-letter term (plus its ", ") could still fit.
                let smallest_possible = separator + 1;
                if used + smallest_possible > max_chars {
                    break;
                }
                continue;
            }
            if !out.is_empty() {
                out.push_str(", ");
            }
            out.push_str(&term);
            used += needed;
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------------------------
// Shared with packs.rs
// ---------------------------------------------------------------------------------------------

/// Trims and squeezes runs of whitespace to a single space.
pub(crate) fn clean_phrase(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Tidy sound-alike list: trimmed, no blanks, no repeats (ignoring case), and nothing identical to
/// the term itself.
pub(crate) fn clean_sounds_like(list: &[String], term: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for raw in list {
        let item = clean_phrase(raw);
        if item.is_empty() || item == term {
            continue;
        }
        if seen.insert(item.to_lowercase()) {
            out.push(item);
        }
    }
    out
}

/// "manual" and "learned" rows are the user's own; anything else is a pack id.
pub(crate) fn is_own_source(source: &str) -> bool {
    source == SOURCE_MANUAL || source == SOURCE_LEARNED
}

/// Which `source` a row ends up with when `incoming` is added or edited over an `existing` row.
/// The user's own rows stay theirs (manual beats learned); a pack's row is claimed by whoever edits
/// it, as "learned" only if that is what arrived and "manual" otherwise.
pub(crate) fn resolve_source(existing: &str, incoming: &str) -> String {
    let incoming = incoming.trim();
    let incoming = if incoming.is_empty() { SOURCE_MANUAL } else { incoming };
    let resolved = if is_own_source(existing) {
        if existing == SOURCE_MANUAL || incoming == SOURCE_MANUAL {
            SOURCE_MANUAL
        } else {
            SOURCE_LEARNED
        }
    } else if incoming == SOURCE_LEARNED {
        SOURCE_LEARNED
    } else {
        SOURCE_MANUAL
    };
    resolved.to_string()
}

pub(crate) fn sounds_to_json(list: &[String]) -> String {
    serde_json::to_string(list).unwrap_or_else(|_| "[]".to_string())
}

fn sounds_from_json(text: &str) -> Vec<String> {
    serde_json::from_str(text).unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------
// Words
// ---------------------------------------------------------------------------------------------

fn word_from_row(row: &Row<'_>) -> rusqlite::Result<DictEntry> {
    let sounds: String = row.get(2)?;
    Ok(DictEntry {
        id: row.get(0)?,
        term: row.get(1)?,
        sounds_like: sounds_from_json(&sounds),
        note: row.get(3)?,
        source: row.get(4)?,
        created_at: row.get(5)?,
        uses: to_u32(row.get(6)?),
    })
}

fn word_by_id(conn: &Connection, id: &str) -> Result<Option<DictEntry>> {
    Ok(conn.query_row(&format!("SELECT {WORD_COLS} FROM words WHERE id = ?1"), [id], word_from_row).optional()?)
}

pub(crate) fn word_by_key(conn: &Connection, key: &str) -> Result<Option<DictEntry>> {
    Ok(conn.query_row(&format!("SELECT {WORD_COLS} FROM words WHERE term_key = ?1"), [key], word_from_row).optional()?)
}

/// The body of `upsert_word`, run inside the caller's transaction.
pub(crate) fn upsert_word_in(conn: &Connection, word: &DictEntry) -> Result<DictEntry> {
    let term = clean_phrase(&word.term);
    if term.is_empty() {
        bail!("A word needs some text.");
    }
    let key = term.to_lowercase();
    let sounds = clean_sounds_like(&word.sounds_like, &term);
    let note = word.note.trim().to_string();
    let wanted_id = word.id.trim();

    let by_id = if wanted_id.is_empty() { None } else { word_by_id(conn, wanted_id)? };
    let by_key = word_by_key(conn, &key)?;

    let stored_id = match (by_id, by_key) {
        // Editing a row the user can see.
        (Some(existing), other) => {
            if let Some(other) = other {
                if other.id != existing.id {
                    bail!("\"{}\" is already in your word list.", other.term);
                }
            }
            conn.execute(
                "UPDATE words SET term = ?2, term_key = ?3, sounds_like = ?4, note = ?5, source = ?6 WHERE id = ?1",
                params![existing.id, term, key, sounds_to_json(&sounds), note, resolve_source(&existing.source, &word.source)],
            )?;
            existing.id
        }
        // Adding a term that is already there: merge into the existing row.
        (None, Some(existing)) => {
            let own = is_own_source(&existing.source);
            // The user's spelling stands; a pack's spelling gives way to whoever claims the word.
            let final_term = if own { existing.term.clone() } else { term.clone() };
            let joined: Vec<String> = existing.sounds_like.iter().chain(sounds.iter()).cloned().collect();
            let merged = clean_sounds_like(&joined, &final_term);
            let final_note = if note.is_empty() { existing.note.clone() } else { note };
            conn.execute(
                "UPDATE words SET term = ?2, sounds_like = ?3, note = ?4, source = ?5 WHERE id = ?1",
                params![existing.id, final_term, sounds_to_json(&merged), final_note, resolve_source(&existing.source, &word.source)],
            )?;
            existing.id
        }
        // A new word (an id we have not seen is kept, so imports can restore ids).
        (None, None) => {
            let id = if wanted_id.is_empty() { new_id() } else { wanted_id.to_string() };
            let created_at = if word.created_at.trim().is_empty() { now_rfc3339() } else { word.created_at.trim().to_string() };
            let source = word.source.trim();
            let source = if source.is_empty() { SOURCE_MANUAL } else { source };
            conn.execute(
                "INSERT INTO words (id, term, term_key, sounds_like, note, source, created_at, uses) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![id, term, key, sounds_to_json(&sounds), note, source, created_at, i64::from(word.uses)],
            )?;
            id
        }
    };
    match word_by_id(conn, &stored_id)? {
        Some(stored) => Ok(stored),
        None => bail!("The word could not be read back after saving."),
    }
}

// ---------------------------------------------------------------------------------------------
// Snippets
// ---------------------------------------------------------------------------------------------

fn snippet_from_row(row: &Row<'_>) -> rusqlite::Result<Snippet> {
    Ok(Snippet { id: row.get(0)?, trigger: row.get(1)?, expansion: row.get(2)?, source: row.get(3)? })
}

fn snippet_by_id(conn: &Connection, id: &str) -> Result<Option<Snippet>> {
    Ok(conn.query_row(&format!("SELECT {SNIPPET_COLS} FROM snippets WHERE id = ?1"), [id], snippet_from_row).optional()?)
}

pub(crate) fn snippet_by_key(conn: &Connection, key: &str) -> Result<Option<Snippet>> {
    Ok(conn.query_row(&format!("SELECT {SNIPPET_COLS} FROM snippets WHERE trigger_key = ?1"), [key], snippet_from_row).optional()?)
}

pub(crate) fn upsert_snippet_in(conn: &Connection, snippet: &Snippet) -> Result<Snippet> {
    let trigger = clean_phrase(&snippet.trigger);
    if trigger.is_empty() {
        bail!("A snippet needs a phrase to say.");
    }
    if snippet.expansion.trim().is_empty() {
        bail!("A snippet needs some text to write out.");
    }
    // The expansion is kept exactly as typed: line breaks and a trailing newline can be deliberate.
    let expansion = snippet.expansion.as_str();
    let key = trigger.to_lowercase();
    let wanted_id = snippet.id.trim();

    let by_id = if wanted_id.is_empty() { None } else { snippet_by_id(conn, wanted_id)? };
    let by_key = snippet_by_key(conn, &key)?;

    let stored_id = match (by_id, by_key) {
        (Some(existing), other) => {
            if let Some(other) = other {
                if other.id != existing.id {
                    bail!("\"{}\" is already one of your snippets.", other.trigger);
                }
            }
            conn.execute(
                "UPDATE snippets SET trigger_text = ?2, trigger_key = ?3, expansion = ?4, source = ?5 WHERE id = ?1",
                params![existing.id, trigger, key, expansion, resolve_source(&existing.source, &snippet.source)],
            )?;
            existing.id
        }
        // Same phrase again: the newer text wins.
        (None, Some(existing)) => {
            let final_trigger = if is_own_source(&existing.source) { existing.trigger.clone() } else { trigger.clone() };
            conn.execute(
                "UPDATE snippets SET trigger_text = ?2, expansion = ?3, source = ?4 WHERE id = ?1",
                params![existing.id, final_trigger, expansion, resolve_source(&existing.source, &snippet.source)],
            )?;
            existing.id
        }
        (None, None) => {
            let id = if wanted_id.is_empty() { new_id() } else { wanted_id.to_string() };
            let source = snippet.source.trim();
            let source = if source.is_empty() { SOURCE_MANUAL } else { source };
            conn.execute(
                "INSERT INTO snippets (id, trigger_text, trigger_key, expansion, source, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, trigger, key, expansion, source, now_rfc3339()],
            )?;
            id
        }
    };
    match snippet_by_id(conn, &stored_id)? {
        Some(stored) => Ok(stored),
        None => bail!("The snippet could not be read back after saving."),
    }
}

// ---------------------------------------------------------------------------------------------
// Suggestions
// ---------------------------------------------------------------------------------------------

fn suggestion_from_row(row: &Row<'_>) -> rusqlite::Result<Suggestion> {
    Ok(Suggestion {
        id: row.get(0)?,
        from: row.get(1)?,
        to: row.get(2)?,
        count: to_u32(row.get(3)?),
        app_name: row.get(4)?,
        last_seen: row.get(5)?,
    })
}

fn suggestion_by_id(conn: &Connection, id: &str) -> Result<Option<Suggestion>> {
    Ok(conn.query_row(&format!("SELECT {SUGGESTION_COLS} FROM suggestions WHERE id = ?1"), [id], suggestion_from_row).optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        Db::open_memory().unwrap()
    }

    fn word(term: &str, sounds: &[&str]) -> DictEntry {
        DictEntry { term: term.into(), sounds_like: sounds.iter().map(|s| s.to_string()).collect(), ..DictEntry::default() }
    }

    fn snippet(trigger: &str, expansion: &str) -> Snippet {
        Snippet { trigger: trigger.into(), expansion: expansion.into(), ..Snippet::default() }
    }

    fn terms(db: &Db) -> Vec<String> {
        db.list_words().unwrap().into_iter().map(|w| w.term).collect()
    }

    /// Plants a word that belongs to a pack, the way an install would.
    fn plant_pack_word(db: &Db, term: &str, pack: &str) -> DictEntry {
        let planted = DictEntry { source: pack.into(), ..word(term, &["pack sound"]) };
        db.upsert_word(&planted).unwrap()
    }

    // ---- words ----

    #[test]
    fn a_new_word_gets_an_id_a_date_and_the_manual_source() {
        let db = db();
        let stored = db.upsert_word(&DictEntry { note: "product name".into(), ..word("Quikly", &["quickly", "quick ly"]) }).unwrap();
        assert_eq!(stored.id.len(), 36, "a uuid");
        assert!(chrono::DateTime::parse_from_rfc3339(&stored.created_at).is_ok(), "{}", stored.created_at);
        assert_eq!(stored.term, "Quikly");
        assert_eq!(stored.sounds_like, ["quickly", "quick ly"]);
        assert_eq!(stored.note, "product name");
        assert_eq!(stored.source, "manual");
        assert_eq!(stored.uses, 0);
        assert_eq!(db.list_words().unwrap().len(), 1);
        assert_eq!(db.list_words().unwrap()[0].id, stored.id);
    }

    #[test]
    fn adding_an_existing_term_merges_instead_of_duplicating() {
        let db = db();
        let first = db.upsert_word(&DictEntry { note: "old note".into(), ..word("Quikly", &["quickly"]) }).unwrap();
        let second = db.upsert_word(&word("quikly", &["quick ly", "Quickly", "  "])).unwrap();
        assert_eq!(second.id, first.id, "same row");
        assert_eq!(second.term, "Quikly", "the existing spelling stays");
        assert_eq!(second.sounds_like, ["quickly", "quick ly"], "merged, 'Quickly' is the same as 'quickly'");
        assert_eq!(second.note, "old note", "an empty new note does not wipe the old one");
        assert_eq!(second.created_at, first.created_at);
        assert_eq!(db.list_words().unwrap().len(), 1);

        let third = db.upsert_word(&DictEntry { note: "new note".into(), ..word("QUIKLY", &[]) }).unwrap();
        assert_eq!(third.note, "new note", "a non-empty new note replaces the old one");
        assert_eq!(third.sounds_like, ["quickly", "quick ly"]);
    }

    #[test]
    fn term_matching_ignores_case_beyond_ascii_and_stray_spaces() {
        let db = db();
        db.upsert_word(&word("Émile", &[])).unwrap();
        db.upsert_word(&word("  émile ", &["amil"])).unwrap();
        db.upsert_word(&word("Claude   Code", &[])).unwrap();
        db.upsert_word(&word("claude code", &[])).unwrap();
        db.upsert_word(&word("STRASSE", &[])).unwrap();
        // Alphabetical on the lower-cased text, so the accented letter sorts after plain a to z.
        assert_eq!(terms(&db), ["Claude Code", "STRASSE", "Émile"]);
        assert_eq!(db.list_words().unwrap()[2].sounds_like, ["amil"]);
    }

    #[test]
    fn sounds_like_is_tidied() {
        let db = db();
        let stored = db.upsert_word(&word("Tauri", &["  tory ", "TORY", "", "tau ri", "Tauri", "tau  ri"])).unwrap();
        assert_eq!(stored.sounds_like, ["tory", "tau ri"], "trimmed, de-duplicated ignoring case, never the term itself");
        // A case-only variant of the term is a useful alias: the engine writes it lower case.
        let stored = db.upsert_word(&word("Quikly", &["quikly"])).unwrap();
        assert_eq!(stored.sounds_like, ["quikly"]);
    }

    #[test]
    fn a_word_needs_some_text() {
        let db = db();
        assert!(db.upsert_word(&word("", &["x"])).is_err());
        assert!(db.upsert_word(&word("   \t", &[])).is_err());
        assert!(db.list_words().unwrap().is_empty());
    }

    #[test]
    fn editing_by_id_replaces_the_fields_and_keeps_uses_and_date() {
        let db = db();
        let created = db.upsert_word(&DictEntry { uses: 3, ..word("Tauri", &["tory", "tar ee"]) }).unwrap();
        assert_eq!(created.uses, 3, "a new row takes the caller's count");

        let edited = db
            .upsert_word(&DictEntry { id: created.id.clone(), term: "Tauri v2".into(), sounds_like: vec!["tory".into()], note: "framework".into(), uses: 99, created_at: "1999-01-01T00:00:00+00:00".into(), ..DictEntry::default() })
            .unwrap();
        assert_eq!(edited.id, created.id);
        assert_eq!(edited.term, "Tauri v2");
        assert_eq!(edited.sounds_like, ["tory"], "an edit replaces the list instead of merging");
        assert_eq!(edited.note, "framework");
        assert_eq!(edited.uses, 3, "uses belong to the store");
        assert_eq!(edited.created_at, created.created_at);
        assert_eq!(db.list_words().unwrap().len(), 1);
    }

    #[test]
    fn changing_only_the_case_of_a_term_by_id_is_allowed() {
        let db = db();
        let created = db.upsert_word(&word("quikly", &[])).unwrap();
        let fixed = db.upsert_word(&DictEntry { id: created.id.clone(), term: "Quikly".into(), ..DictEntry::default() }).unwrap();
        assert_eq!(fixed.id, created.id);
        assert_eq!(fixed.term, "Quikly");
        assert_eq!(db.list_words().unwrap().len(), 1);
    }

    #[test]
    fn renaming_a_word_onto_another_words_term_is_refused() {
        let db = db();
        db.upsert_word(&word("Alpha", &[])).unwrap();
        let beta = db.upsert_word(&word("Beta", &["bayta"])).unwrap();
        let err = db.upsert_word(&DictEntry { id: beta.id.clone(), term: "ALPHA".into(), ..DictEntry::default() }).unwrap_err();
        assert!(format!("{err:#}").contains("Alpha"), "{err:#}");
        let after = db.list_words().unwrap();
        assert_eq!(terms(&db), ["Alpha", "Beta"]);
        assert_eq!(after[1].sounds_like, ["bayta"], "the refused edit changed nothing");
    }

    #[test]
    fn an_id_we_have_not_seen_is_inserted_with_that_id() {
        let db = db();
        let stored = db
            .upsert_word(&DictEntry { id: "restored-1".into(), created_at: "2025-05-05T05:05:05+00:00".into(), ..word("Restored", &[]) })
            .unwrap();
        assert_eq!(stored.id, "restored-1");
        assert_eq!(stored.created_at, "2025-05-05T05:05:05+00:00");
    }

    #[test]
    fn editing_a_pack_word_makes_it_the_users_own() {
        let db = db();
        let planted = plant_pack_word(&db, "Kubernetes", "devops-pack");
        assert_eq!(planted.source, "devops-pack");

        // The UI echoes back the row it was given, with the pack's source still on it.
        let edited = db.upsert_word(&DictEntry { note: "container thing".into(), ..planted.clone() }).unwrap();
        assert_eq!(edited.id, planted.id);
        assert_eq!(edited.source, "manual", "an edit claims the word");
        assert_eq!(edited.note, "container thing");
    }

    #[test]
    fn adding_a_word_by_hand_over_a_pack_word_claims_it_with_the_users_spelling() {
        let db = db();
        let planted = plant_pack_word(&db, "kubernetes", "devops-pack");
        let mine = db.upsert_word(&word("Kubernetes", &["cooper nettys"])).unwrap();
        assert_eq!(mine.id, planted.id);
        assert_eq!(mine.term, "Kubernetes");
        assert_eq!(mine.source, "manual");
        assert_eq!(mine.sounds_like, ["pack sound", "cooper nettys"]);
    }

    #[test]
    fn manual_beats_learned_in_both_directions_and_pack_rows_stay_put_without_a_claim() {
        let db = db();
        db.upsert_word(&word("Alpha", &[])).unwrap();
        let stays = db.upsert_word(&DictEntry { source: "learned".into(), ..word("alpha", &["al fa"]) }).unwrap();
        assert_eq!(stays.source, "manual");

        db.upsert_word(&DictEntry { source: "learned".into(), ..word("Beta", &[]) }).unwrap();
        let promoted = db.upsert_word(&word("beta", &[])).unwrap();
        assert_eq!(promoted.source, "manual");

        let learned = db.upsert_word(&DictEntry { source: "learned".into(), ..word("Gamma", &[]) }).unwrap();
        assert_eq!(learned.source, "learned");
    }

    #[test]
    fn words_list_alphabetically_ignoring_case() {
        let db = db();
        for t in ["banana", "Apple", "cherry", "Avocado"] {
            db.upsert_word(&word(t, &[])).unwrap();
        }
        assert_eq!(terms(&db), ["Apple", "Avocado", "banana", "cherry"]);
    }

    #[test]
    fn deleting_a_word_is_forgiving() {
        let db = db();
        let a = db.upsert_word(&word("Alpha", &[])).unwrap();
        db.upsert_word(&word("Beta", &[])).unwrap();
        db.delete_word(&a.id).unwrap();
        db.delete_word(&a.id).unwrap();
        db.delete_word("no-such-id").unwrap();
        assert_eq!(terms(&db), ["Beta"]);
        // The term can be added again afterwards.
        db.upsert_word(&word("Alpha", &[])).unwrap();
        assert_eq!(terms(&db), ["Alpha", "Beta"]);
    }

    #[test]
    fn bumping_uses_matches_ignoring_case_and_skips_unknown_terms() {
        let db = db();
        db.upsert_word(&word("Quikly", &[])).unwrap();
        db.upsert_word(&word("Tauri", &[])).unwrap();
        db.bump_word_uses(&["quikly".to_string(), "QUIKLY".to_string(), "  Tauri ".to_string(), "unknown".to_string(), "".to_string()]).unwrap();
        db.bump_word_uses(&[]).unwrap();
        let by_term = |t: &str| db.list_words().unwrap().into_iter().find(|w| w.term == t).unwrap().uses;
        assert_eq!(by_term("Quikly"), 2);
        assert_eq!(by_term("Tauri"), 1);
        assert_eq!(db.list_words().unwrap().len(), 2, "unknown terms are not created");
    }

    // ---- vocabulary prompt ----

    #[test]
    fn the_vocabulary_prompt_lists_the_most_used_terms_first() {
        let db = db();
        for (term, uses, created) in [
            ("Rare", 0, "2026-01-01T00:00:00+00:00"),
            ("Newer", 0, "2026-03-01T00:00:00+00:00"),
            ("Often", 9, "2026-02-01T00:00:00+00:00"),
            ("Sometimes", 4, "2026-02-01T00:00:00+00:00"),
        ] {
            db.upsert_word(&DictEntry { uses, created_at: created.into(), ..word(term, &[]) }).unwrap();
        }
        assert_eq!(db.vocabulary_prompt(1000).unwrap(), "Often, Sometimes, Newer, Rare");
    }

    #[test]
    fn the_vocabulary_prompt_fits_the_budget() {
        let db = db();
        for (term, uses) in [("Quikly", 5), ("Tauri", 4), ("Kubernetes", 3), ("Go", 2)] {
            db.upsert_word(&DictEntry { uses, ..word(term, &[]) }).unwrap();
        }
        assert_eq!(db.vocabulary_prompt(0).unwrap(), "");
        assert_eq!(db.vocabulary_prompt(1).unwrap(), "", "nothing is one character long");
        assert_eq!(db.vocabulary_prompt(2).unwrap(), "Go", "a short term still fits when longer ones cannot");
        assert_eq!(db.vocabulary_prompt(5).unwrap(), "Tauri", "Quikly (6) is too long, so the next one that fits is used");
        assert_eq!(db.vocabulary_prompt(6).unwrap(), "Quikly");
        assert_eq!(db.vocabulary_prompt(12).unwrap(), "Quikly, Go", "', Tauri' would make 13");
        assert_eq!(db.vocabulary_prompt(13).unwrap(), "Quikly, Tauri");
        // Kubernetes (10) does not fit after the first two (13 + 2 + 10 = 25), but 'Go' still does.
        assert_eq!(db.vocabulary_prompt(20).unwrap(), "Quikly, Tauri, Go");
        for budget in 0..60 {
            assert!(db.vocabulary_prompt(budget).unwrap().chars().count() <= budget, "budget {budget}");
        }
        assert_eq!(db.vocabulary_prompt(100).unwrap(), "Quikly, Tauri, Kubernetes, Go");
    }

    #[test]
    fn the_vocabulary_prompt_counts_characters_not_bytes() {
        let db = db();
        db.upsert_word(&DictEntry { uses: 2, ..word("Zoë", &[]) }).unwrap();
        db.upsert_word(&word("Ñandú", &[])).unwrap();
        assert_eq!(db.vocabulary_prompt(10).unwrap(), "Zoë, Ñandú");
        assert_eq!(Db::open_memory().unwrap().vocabulary_prompt(100).unwrap(), "");
    }

    // ---- snippets ----

    #[test]
    fn snippets_are_stored_exactly_and_listed_by_trigger() {
        let db = db();
        let signature = "Kind regards,\nAlex\n";
        let s = db.upsert_snippet(&snippet("My signature", signature)).unwrap();
        db.upsert_snippet(&snippet("calendar link", "https://cal.example/alex")).unwrap();
        assert_eq!(s.id.len(), 36);
        assert_eq!(s.source, "manual");
        assert_eq!(s.expansion, signature, "line breaks survive");
        let listed = db.list_snippets().unwrap();
        assert_eq!(listed.iter().map(|x| x.trigger.as_str()).collect::<Vec<_>>(), ["calendar link", "My signature"]);
    }

    #[test]
    fn saying_the_same_trigger_again_replaces_the_expansion() {
        let db = db();
        let first = db.upsert_snippet(&snippet("my calendar link", "https://old.example")).unwrap();
        let second = db.upsert_snippet(&snippet("  My  Calendar Link ", "https://new.example")).unwrap();
        assert_eq!(second.id, first.id);
        assert_eq!(second.trigger, "my calendar link", "existing spelling kept");
        assert_eq!(second.expansion, "https://new.example");
        assert_eq!(db.list_snippets().unwrap().len(), 1);
    }

    #[test]
    fn editing_a_snippet_by_id_and_refusing_a_clash() {
        let db = db();
        let a = db.upsert_snippet(&snippet("alpha", "one")).unwrap();
        db.upsert_snippet(&snippet("beta", "two")).unwrap();
        let edited = db.upsert_snippet(&Snippet { id: a.id.clone(), trigger: "Alpha!".into(), expansion: "uno".into(), source: String::new() }).unwrap();
        assert_eq!(edited.id, a.id);
        assert_eq!(edited.trigger, "Alpha!");
        assert_eq!(edited.expansion, "uno");
        let err = db.upsert_snippet(&Snippet { id: a.id.clone(), trigger: "BETA".into(), expansion: "x".into(), source: String::new() }).unwrap_err();
        assert!(format!("{err:#}").contains("beta"));
        assert_eq!(db.list_snippets().unwrap().len(), 2);
    }

    #[test]
    fn snippets_need_a_trigger_and_an_expansion_and_can_be_deleted() {
        let db = db();
        assert!(db.upsert_snippet(&snippet("", "x")).is_err());
        assert!(db.upsert_snippet(&snippet("x", "  \n ")).is_err());
        let s = db.upsert_snippet(&snippet("hello", "world")).unwrap();
        db.delete_snippet(&s.id).unwrap();
        db.delete_snippet(&s.id).unwrap();
        assert!(db.list_snippets().unwrap().is_empty());
    }

    #[test]
    fn editing_a_pack_snippet_makes_it_the_users_own() {
        let db = db();
        let planted = db.upsert_snippet(&Snippet { source: "team-pack".into(), ..snippet("standup", "yesterday, today, blockers") }).unwrap();
        assert_eq!(planted.source, "team-pack");
        let edited = db.upsert_snippet(&Snippet { expansion: "what I did".into(), ..planted.clone() }).unwrap();
        assert_eq!(edited.source, "manual");
        assert_eq!(edited.id, planted.id);
    }

    // ---- suggestions ----

    #[test]
    fn the_same_correction_counts_up() {
        let db = db();
        let first = db.record_suggestion("quickly", "Quikly", "Outlook").unwrap().expect("a suggestion");
        assert_eq!((first.from.as_str(), first.to.as_str(), first.count, first.app_name.as_str()), ("quickly", "Quikly", 1, "Outlook"));
        assert!(chrono::DateTime::parse_from_rfc3339(&first.last_seen).is_ok());

        // Pretend the first sighting was a while ago so we can see last_seen move.
        db.conn.lock().execute("UPDATE suggestions SET last_seen = '2020-01-01T00:00:00+00:00'", []).unwrap();
        let second = db.record_suggestion("quickly", "Quikly", "Slack").unwrap().unwrap();
        assert_eq!(second.id, first.id);
        assert_eq!(second.count, 2);
        assert_eq!(second.app_name, "Slack", "the latest app is kept");
        assert!(second.last_seen.as_str() > "2020-01-01T00:00:00+00:00");

        let third = db.record_suggestion("quickly", "Quikly", "").unwrap().unwrap();
        assert_eq!(third.count, 3);
        assert_eq!(third.app_name, "Slack", "a blank app does not erase the last one");
        assert_eq!(db.list_suggestions().unwrap().len(), 1);
    }

    #[test]
    fn the_engines_capitalisation_does_not_split_a_correction() {
        let db = db();
        db.record_suggestion("Quickly", "Quikly", "").unwrap();
        let again = db.record_suggestion("quickly", "Quikly", "").unwrap().unwrap();
        assert_eq!(again.count, 2);
        assert_eq!(again.from, "Quickly", "the first spelling is kept");
        // A different corrected spelling is a different suggestion.
        let other = db.record_suggestion("quickly", "quikly", "").unwrap().unwrap();
        assert_eq!(other.count, 1);
        assert_eq!(db.list_suggestions().unwrap().len(), 2);
    }

    #[test]
    fn pointless_corrections_are_ignored() {
        let db = db();
        assert!(db.record_suggestion("same", "same", "").unwrap().is_none());
        assert!(db.record_suggestion("", "x", "").unwrap().is_none());
        assert!(db.record_suggestion("x", "", "").unwrap().is_none());
        assert!(db.record_suggestion("  ", "x", "").unwrap().is_none());
        assert!(db.record_suggestion("x", " \t", "").unwrap().is_none());
        assert!(db.list_suggestions().unwrap().is_empty());
        // Only the case differs: that is a real correction.
        assert!(db.record_suggestion("tauri", "Tauri", "").unwrap().is_some());
    }

    #[test]
    fn corrections_the_dictionary_already_makes_are_not_suggested() {
        let db = db();
        db.upsert_word(&word("Quikly", &["quickly"])).unwrap();
        assert!(db.record_suggestion("quickly", "Quikly", "").unwrap().is_none());
        assert!(db.record_suggestion("QUICKLY", "quikly", "").unwrap().is_none(), "ignoring case on both sides");
        // A new way the engine mishears the word is worth suggesting.
        let new_alias = db.record_suggestion("quick lee", "Quikly", "").unwrap();
        assert!(new_alias.is_some());
        // A different target is a different story.
        assert!(db.record_suggestion("quickly", "Quickstep", "").unwrap().is_some());
    }

    #[test]
    fn accepting_adds_a_learned_word_and_removes_the_suggestion() {
        let db = db();
        let s = db.record_suggestion("quickly", "Quikly", "Outlook").unwrap().unwrap();
        let word = db.accept_suggestion(&s.id).unwrap().expect("the new word");
        assert_eq!(word.term, "Quikly");
        assert_eq!(word.sounds_like, ["quickly"]);
        assert_eq!(word.source, "learned");
        assert!(db.list_suggestions().unwrap().is_empty());
        assert_eq!(db.list_words().unwrap().len(), 1);
        assert!(db.accept_suggestion(&s.id).unwrap().is_none(), "already accepted");
        // Now the pair is covered by a word, so it is not suggested again.
        assert!(db.record_suggestion("quickly", "Quikly", "").unwrap().is_none());
    }

    #[test]
    fn accepting_merges_into_an_existing_word_and_keeps_it_manual() {
        let db = db();
        let mine = db.upsert_word(&DictEntry { note: "mine".into(), ..word("Quikly", &["quikly"]) }).unwrap();
        let s = db.record_suggestion("quick lee", "quikly", "").unwrap().unwrap();
        let merged = db.accept_suggestion(&s.id).unwrap().unwrap();
        assert_eq!(merged.id, mine.id);
        assert_eq!(merged.term, "Quikly", "the user's spelling stands");
        assert_eq!(merged.sounds_like, ["quikly", "quick lee"]);
        assert_eq!(merged.source, "manual");
        assert_eq!(merged.note, "mine");
        assert_eq!(db.list_words().unwrap().len(), 1);
    }

    #[test]
    fn dismissing_forgets_the_suggestion_for_good() {
        let db = db();
        let s = db.record_suggestion("quickly", "Quikly", "").unwrap().unwrap();
        db.dismiss_suggestion(&s.id).unwrap();
        assert!(db.list_suggestions().unwrap().is_empty());
        assert!(db.list_words().unwrap().is_empty(), "dismissing adds nothing to the dictionary");
        assert!(db.record_suggestion("quickly", "Quikly", "").unwrap().is_none());
        assert!(db.record_suggestion("QUICKLY", "Quikly", "").unwrap().is_none(), "the engine's capitalisation does not matter");
        assert!(db.record_suggestion("quickly", "Quickstep", "").unwrap().is_some(), "other targets are unaffected");
        assert!(db.record_suggestion("quick lie", "Quikly", "").unwrap().is_some(), "other mishearings are unaffected");
        db.dismiss_suggestion(&s.id).unwrap();
        db.dismiss_suggestion("no-such-id").unwrap();
    }

    #[test]
    fn suggestions_list_most_seen_first() {
        let db = db();
        db.record_suggestion("a", "A1", "").unwrap();
        for _ in 0..3 {
            db.record_suggestion("b", "B1", "").unwrap();
        }
        for _ in 0..2 {
            db.record_suggestion("c", "C1", "").unwrap();
        }
        let order: Vec<(String, u32)> = db.list_suggestions().unwrap().into_iter().map(|s| (s.to, s.count)).collect();
        assert_eq!(order, [("B1".to_string(), 3), ("C1".to_string(), 2), ("A1".to_string(), 1)]);
    }

    #[test]
    fn helper_rules_for_ownership() {
        assert_eq!(resolve_source("manual", ""), "manual");
        assert_eq!(resolve_source("manual", "learned"), "manual");
        assert_eq!(resolve_source("learned", "manual"), "manual");
        assert_eq!(resolve_source("learned", "learned"), "learned");
        assert_eq!(resolve_source("learned", ""), "manual");
        assert_eq!(resolve_source("some-pack", "some-pack"), "manual");
        assert_eq!(resolve_source("some-pack", "learned"), "learned");
        assert_eq!(resolve_source("some-pack", ""), "manual");
        assert!(is_own_source("manual") && is_own_source("learned") && !is_own_source("acme-team"));
        assert_eq!(clean_phrase("  a \t b\n c  "), "a b c");
    }
}
