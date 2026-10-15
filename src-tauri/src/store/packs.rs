//! Packs: shareable bundles of words, snippets and modes (a team's jargon in one file).
//!
//! OWNER: data agent. STATUS: implemented, with unit tests.
//!
//! Contract:
//! * File format: JSON of types::Pack, extension .navipack, plus "format": "navi-pack/1".
//! * `export_pack`: the user's own (source "manual" or "learned") words and snippets plus custom
//!   modes, with the given name/author/description; id = slug of the name.
//! * `install_pack`: validates, removes anything previously installed from the same pack id, then
//!   inserts its words/snippets/modes with source = pack id; returns the summary. Never overwrites
//!   the user's own entries (a term that already exists as manual is skipped).
//! * `install_pack_from_url`: downloads (https only, 5 MB cap), installs, remembers the URL.
//! * `refresh_subscriptions`: re-fetches every pack with a source_url, reinstalling when the version
//!   changed. Errors per pack are collected, not fatal.
//!
//! The finer rules
//! * A file is a pack only if it says `"format": "navi-pack/1"`. A newer format number is refused
//!   with a message to update Navi. A UTF-8 byte order mark (Notepad adds one) is tolerated, and a
//!   mode may leave out every field except its name.
//! * Exported files carry no internal ids, dates or use counts; just terms, sound-alikes, notes,
//!   trigger phrases, expansions and modes. The version is the export time (2026.10.14.1530) so a
//!   pack re-exported to the same address always looks newer to subscribers. Snippet expansions go
//!   into the file as written, so the UI should warn before sharing one with private details.
//! * Pack ids are lower-case slugs. "manual" and "learned" mean "the user's own" everywhere in the
//!   store, so they can never be pack ids.
//! * Installing is one transaction: a bad pack changes nothing, and the previous version of the same
//!   pack stays in place. A word or snippet that the user (or another pack) already has is skipped,
//!   so first come, first served. Reinstalling keeps the ids and use counts of words that were in the
//!   previous version. A pack's mode whose id clashes with a built-in or one of the user's modes is
//!   filed as `<pack-id>-<mode-id>` instead of replacing it.
//! * Counts in `PackSummary` are taken from the tables, so they show what the pack really has in
//!   Navi now (entries the user has since claimed by editing them are no longer counted).
//! * A `sourceUrl` written inside a file is ignored: only `install_pack_from_url` (the user typing an
//!   address) sets up a subscription, so a file cannot make Navi fetch from somewhere on its own. A
//!   pack that is already subscribed keeps its address when it is reinstalled from a file.
//! * `refresh_subscriptions` does the network work. It does not check `privacy.localOnly`; the
//!   caller decides whether it may run at all.

use super::db::{new_id, now_rfc3339};
use super::dictionary::{clean_phrase, clean_sounds_like, is_own_source, sounds_to_json, SOURCE_LEARNED, SOURCE_MANUAL};
use super::modes::{mode_id_taken, slugify, tidy_apps, unique_mode_id};
use super::Db;
use crate::types::{Mode, Pack, PackSummary};
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Row, TransactionBehavior};
use serde::Serialize;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;
use url::Url;

/// The value of the "format" field in a pack file.
pub const PACK_FORMAT: &str = "navi-pack/1";
/// File extension for pack files, without the dot.
pub const PACK_EXTENSION: &str = "navipack";
/// Largest pack file Navi will read or download.
pub const MAX_PACK_BYTES: usize = 5 * 1024 * 1024;

const MAX_WORDS: usize = 20_000;
const MAX_SNIPPETS: usize = 5_000;
const MAX_MODES: usize = 200;
const MAX_TERM_CHARS: usize = 200;
const MAX_SOUNDS_PER_WORD: usize = 100;
const MAX_NOTE_CHARS: usize = 2_000;
const MAX_EXPANSION_CHARS: usize = 50_000;
const MAX_INSTRUCTION_CHARS: usize = 50_000;
const MAX_REDIRECTS: usize = 5;

impl Db {
    /// Writes the user's own words, snippets and custom modes to `path` as a pack file and returns
    /// what went into it. Errors if there is nothing to share.
    pub fn export_pack(&self, path: &Path, name: &str, author: &str, description: &str) -> Result<PackSummary> {
        let name = clean_phrase(name);
        let mut id = slugify(&name);
        if id.is_empty() {
            bail!("Give the pack a name with some letters or numbers in it.");
        }
        if is_own_source(&id) {
            id.push_str("-pack"); // "manual" and "learned" are reserved
        }
        let author = clean_phrase(author);
        let description = description.trim().to_string();

        let (words, snippets, modes) = {
            let conn = self.conn.lock();
            (own_words(&conn)?, own_snippets(&conn)?, own_modes(&conn)?)
        };
        if words.is_empty() && snippets.is_empty() && modes.is_empty() {
            bail!("There is nothing to put in a pack yet. Add some words, snippets or modes of your own first.");
        }

        let version = chrono::Local::now().format("%Y.%m.%d.%H%M").to_string();
        let file = PackFile {
            format: PACK_FORMAT,
            id: &id,
            name: &name,
            author: &author,
            description: &description,
            version: &version,
            words: &words,
            snippets: &snippets,
            modes: &modes,
        };
        let mut text = serde_json::to_string_pretty(&file).context("writing the pack")?;
        text.push('\n');
        write_atomically(path, text.as_bytes()).with_context(|| format!("saving the pack to {}", path.display()))?;

        Ok(PackSummary {
            id,
            name,
            author,
            description,
            version,
            words: words.len(),
            snippets: snippets.len(),
            modes: modes.len(),
            source_url: None,
            installed_at: String::new(),
        })
    }

    /// Installs (or updates) a pack. See the notes at the top of this file for the rules.
    pub fn install_pack(&self, pack: &Pack) -> Result<PackSummary> {
        let prepared = prepare_pack(pack)?;
        let mut conn = self.conn.lock();
        install_prepared(&mut conn, prepared)
    }

    /// Reads a .navipack file and installs it. The file's own `sourceUrl` is ignored.
    pub fn install_pack_file(&self, path: &Path) -> Result<PackSummary> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .with_context(|| format!("opening {}", path.display()))?
            .take(MAX_PACK_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .with_context(|| format!("reading {}", path.display()))?;
        let mut pack = parse_pack_bytes(&bytes).with_context(|| format!("installing {}", path.display()))?;
        pack.source_url = None;
        self.install_pack(&pack)
    }

    /// Installed packs, A to Z by name.
    pub fn list_packs(&self) -> Result<Vec<PackSummary>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&format!("{SUMMARY_SQL} ORDER BY p.name COLLATE NOCASE, p.id"))?;
        let rows = stmt.query_map([], summary_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<PackSummary>>>()?)
    }

    /// Removes a pack and exactly the words, snippets and modes it installed. Anything the user
    /// added or claimed stays. Removing a pack that is not installed does nothing.
    pub fn remove_pack(&self, id: &str) -> Result<()> {
        let id = id.trim();
        if id.is_empty() || is_own_source(id) {
            return Ok(()); // never touch the user's own entries
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let installed = tx.query_row("SELECT 1 FROM packs WHERE id = ?1", [id], |_| Ok(())).optional()?.is_some();
        if installed {
            remove_rows(&tx, id)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Packs that were installed from an address and so can be refreshed.
    pub fn subscriptions(&self) -> Result<Vec<Subscription>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, version, source_url FROM packs WHERE source_url IS NOT NULL AND source_url <> '' ORDER BY name COLLATE NOCASE, id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Subscription { id: row.get(0)?, name: row.get(1)?, version: row.get(2)?, url: row.get(3)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<Subscription>>>()?)
    }
}

/// A pack installed from an address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subscription {
    pub id: String,
    pub name: String,
    pub version: String,
    pub url: String,
}

/// Downloads a pack from an https address (at most 5 MB), installs it and remembers the address so
/// `refresh_subscriptions` can keep it current.
pub async fn install_pack_from_url(db: &Db, url: &str) -> Result<PackSummary> {
    let client = download_client(true)?;
    install_from_url_with(db, &client, url, true).await
}

/// Re-fetches every pack that has an address and reinstalls those whose version changed. One pack
/// failing (offline, address gone, bad file) never stops the others; each failure is returned as a
/// line of text naming the pack.
pub async fn refresh_subscriptions(db: &Db) -> Vec<String> {
    match download_client(true) {
        Ok(client) => refresh_all(db, &client, true).await,
        Err(e) => vec![format!("{e:#}")],
    }
}

// ---------------------------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PackFile<'a> {
    format: &'a str,
    id: &'a str,
    name: &'a str,
    author: &'a str,
    description: &'a str,
    version: &'a str,
    words: &'a [WordOut],
    snippets: &'a [SnippetOut],
    modes: &'a [Mode],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WordOut {
    term: String,
    sounds_like: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    note: String,
}

#[derive(Serialize)]
struct SnippetOut {
    trigger: String,
    expansion: String,
}

fn own_words(conn: &Connection) -> Result<Vec<WordOut>> {
    let mut stmt = conn.prepare("SELECT term, sounds_like, note FROM words WHERE source IN (?1, ?2) ORDER BY term_key, id")?;
    let rows = stmt.query_map([SOURCE_MANUAL, SOURCE_LEARNED], |row| {
        let sounds: String = row.get(1)?;
        Ok(WordOut { term: row.get(0)?, sounds_like: serde_json::from_str(&sounds).unwrap_or_default(), note: row.get(2)? })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<WordOut>>>()?)
}

fn own_snippets(conn: &Connection) -> Result<Vec<SnippetOut>> {
    let mut stmt = conn.prepare("SELECT trigger_text, expansion FROM snippets WHERE source IN (?1, ?2) ORDER BY trigger_key, id")?;
    let rows = stmt.query_map([SOURCE_MANUAL, SOURCE_LEARNED], |row| Ok(SnippetOut { trigger: row.get(0)?, expansion: row.get(1)? }))?;
    Ok(rows.collect::<rusqlite::Result<Vec<SnippetOut>>>()?)
}

/// The user's custom modes (not pack modes, not overrides of built-ins).
fn own_modes(conn: &Connection) -> Result<Vec<Mode>> {
    let mut stmt = conn.prepare("SELECT json FROM modes WHERE source = ?1 ORDER BY seq")?;
    let rows = stmt.query_map([SOURCE_MANUAL], |row| row.get::<_, String>(0))?;
    let mut modes = Vec::new();
    for json in rows {
        if let Ok(mode) = serde_json::from_str::<Mode>(&json?) {
            if !mode.builtin {
                modes.push(mode);
            }
        }
    }
    Ok(modes)
}

/// Writes next to the target and renames into place, so a failed export never leaves a half file.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating the folder {}", dir.display()))?;
        }
    }
    let mut temp_name = path.as_os_str().to_owned();
    temp_name.push(".tmp");
    let temp = PathBuf::from(temp_name);
    std::fs::write(&temp, bytes)?;
    if let Err(e) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(e.into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Reading and checking a pack
// ---------------------------------------------------------------------------------------------

/// Turns the bytes of a .navipack file into a `Pack`, checking the "format" marker.
pub fn parse_pack_bytes(bytes: &[u8]) -> Result<Pack> {
    if bytes.len() > MAX_PACK_BYTES {
        bail!("That pack is bigger than {}, which is more than Navi will read.", size_text(MAX_PACK_BYTES));
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(bytes);
    let mut value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| anyhow!("That file isn't a Navi pack: it isn't valid JSON ({e})."))?;
    match value.get("format").and_then(|f| f.as_str()) {
        None => bail!("That file isn't a Navi pack: it has no \"format\" field."),
        Some(PACK_FORMAT) => {}
        Some(other) => match other.strip_prefix("navi-pack/").and_then(|n| n.parse::<u32>().ok()) {
            Some(n) if n > 1 => bail!("This pack uses a newer pack format ({other}). Update Navi to install it."),
            _ => bail!("That file isn't a Navi pack (its format is \"{other}\")."),
        },
    }
    fill_mode_defaults(&mut value);
    serde_json::from_value(value).map_err(|e| anyhow!("The pack file is damaged: {e}."))
}

/// A hand-written mode needs only a name; the other fields get the same defaults the UI would use.
fn fill_mode_defaults(value: &mut serde_json::Value) {
    use serde_json::json;
    let Some(serde_json::Value::Array(modes)) = value.get_mut("modes") else { return };
    for mode in modes {
        let serde_json::Value::Object(map) = mode else { continue };
        for (key, default) in [
            ("id", json!("")),
            ("name", json!("")),
            ("icon", json!("spark")),
            ("description", json!("")),
            ("kind", json!("rewrite")),
            ("instructions", json!("")),
            ("apps", json!([])),
            ("useAi", json!(true)),
            ("builtin", json!(false)),
        ] {
            map.entry(key).or_insert(default);
        }
    }
}

struct Prepared {
    id: String,
    name: String,
    author: String,
    description: String,
    version: String,
    source_url: Option<String>,
    words: Vec<PreparedWord>,
    snippets: Vec<PreparedSnippet>,
    modes: Vec<Mode>,
}

struct PreparedWord {
    term: String,
    key: String,
    sounds: Vec<String>,
    note: String,
}

struct PreparedSnippet {
    trigger: String,
    key: String,
    expansion: String,
}

/// The id a pack is stored under: the slug of its id, or of its name when it has no id.
pub(crate) fn pack_id_for(id: &str, name: &str) -> Result<String> {
    let from_id = slugify(id);
    let slug = if from_id.is_empty() { slugify(name) } else { from_id };
    if slug.is_empty() {
        bail!("This pack needs an id or a name with some letters or numbers in it.");
    }
    if is_own_source(&slug) {
        bail!("\"{slug}\" is a reserved name and cannot be used as a pack id.");
    }
    Ok(slug)
}

/// Checks and tidies a pack before anything is written. Any problem is an error, so a bad pack
/// never half installs.
fn prepare_pack(pack: &Pack) -> Result<Prepared> {
    let id = pack_id_for(&pack.id, &pack.name)?;
    let name = clean_phrase(&pack.name);
    let name = if name.is_empty() { id.clone() } else { name };

    if pack.words.len() > MAX_WORDS {
        bail!("This pack has {} words; Navi accepts up to {MAX_WORDS} in one pack.", pack.words.len());
    }
    if pack.snippets.len() > MAX_SNIPPETS {
        bail!("This pack has {} snippets; Navi accepts up to {MAX_SNIPPETS} in one pack.", pack.snippets.len());
    }
    if pack.modes.len() > MAX_MODES {
        bail!("This pack has {} modes; Navi accepts up to {MAX_MODES} in one pack.", pack.modes.len());
    }

    let mut words: Vec<PreparedWord> = Vec::new();
    let mut word_at: HashMap<String, usize> = HashMap::new();
    for (i, w) in pack.words.iter().enumerate() {
        let n = i + 1;
        let term = clean_phrase(&w.term);
        if term.is_empty() {
            bail!("Word {n} in this pack has no text.");
        }
        if term.chars().count() > MAX_TERM_CHARS {
            bail!("Word {n} in this pack is longer than {MAX_TERM_CHARS} characters.");
        }
        if w.sounds_like.len() > MAX_SOUNDS_PER_WORD || w.sounds_like.iter().any(|s| s.chars().count() > MAX_TERM_CHARS) {
            bail!("The sound-alikes for \"{term}\" are too many or too long.");
        }
        let note = w.note.trim().to_string();
        if note.chars().count() > MAX_NOTE_CHARS {
            bail!("The note for \"{term}\" is longer than {MAX_NOTE_CHARS} characters.");
        }
        let key = term.to_lowercase();
        let sounds = clean_sounds_like(&w.sounds_like, &term);
        match word_at.get(&key) {
            // The same word twice in one pack: keep one, with both sound-alike lists.
            Some(&at) => {
                let joined: Vec<String> = words[at].sounds.iter().chain(sounds.iter()).cloned().collect();
                words[at].sounds = clean_sounds_like(&joined, &words[at].term);
                if words[at].note.is_empty() {
                    words[at].note = note;
                }
            }
            None => {
                word_at.insert(key.clone(), words.len());
                words.push(PreparedWord { term, key, sounds, note });
            }
        }
    }

    let mut snippets: Vec<PreparedSnippet> = Vec::new();
    let mut snippet_at: HashMap<String, usize> = HashMap::new();
    for (i, s) in pack.snippets.iter().enumerate() {
        let n = i + 1;
        let trigger = clean_phrase(&s.trigger);
        if trigger.is_empty() {
            bail!("Snippet {n} in this pack has no trigger phrase.");
        }
        if s.expansion.trim().is_empty() {
            bail!("The snippet \"{trigger}\" in this pack has no text to write out.");
        }
        if trigger.chars().count() > MAX_TERM_CHARS || s.expansion.chars().count() > MAX_EXPANSION_CHARS {
            bail!("The snippet \"{trigger}\" in this pack is too long.");
        }
        let key = trigger.to_lowercase();
        let prepared = PreparedSnippet { trigger, key: key.clone(), expansion: s.expansion.clone() };
        match snippet_at.get(&key) {
            // The same phrase twice: the later one wins, as it would if typed in again.
            Some(&at) => snippets[at] = prepared,
            None => {
                snippet_at.insert(key, snippets.len());
                snippets.push(prepared);
            }
        }
    }

    let mut modes: Vec<Mode> = Vec::new();
    for (i, m) in pack.modes.iter().enumerate() {
        let n = i + 1;
        let mode_name = m.name.trim();
        if mode_name.is_empty() {
            bail!("Mode {n} in this pack has no name.");
        }
        if m.instructions.chars().count() > MAX_INSTRUCTION_CHARS {
            bail!("The instructions for the mode \"{mode_name}\" are too long.");
        }
        let icon = m.icon.trim();
        modes.push(Mode {
            id: slugify(&m.id),
            name: mode_name.to_string(),
            icon: if icon.is_empty() { "spark".to_string() } else { icon.to_string() },
            description: m.description.trim().to_string(),
            kind: m.kind.clone(),
            instructions: m.instructions.clone(),
            apps: tidy_apps(&m.apps),
            use_ai: m.use_ai,
            builtin: false, // a pack cannot claim to be a built-in
        });
    }

    let source_url = pack.source_url.as_deref().map(str::trim).filter(|u| !u.is_empty()).map(str::to_string);
    Ok(Prepared {
        id,
        name,
        author: clean_phrase(&pack.author),
        description: pack.description.trim().to_string(),
        version: pack.version.trim().to_string(),
        source_url,
        words,
        snippets,
        modes,
    })
}

// ---------------------------------------------------------------------------------------------
// Installing and removing
// ---------------------------------------------------------------------------------------------

fn remove_rows(conn: &Connection, id: &str) -> Result<()> {
    if is_own_source(id) {
        bail!("Refusing to remove \"{id}\": those entries belong to the user, not to a pack.");
    }
    conn.execute("DELETE FROM words WHERE source = ?1", [id])?;
    conn.execute("DELETE FROM snippets WHERE source = ?1", [id])?;
    conn.execute("DELETE FROM modes WHERE source = ?1", [id])?;
    conn.execute("DELETE FROM packs WHERE id = ?1", [id])?;
    Ok(())
}

fn install_prepared(conn: &mut Connection, p: Prepared) -> Result<PackSummary> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    // What this pack put here last time. Its words keep their ids, dates and use counts, and a
    // subscription address is not lost when the update comes from a file.
    let previous_url: Option<String> =
        tx.query_row("SELECT source_url FROM packs WHERE id = ?1", [&p.id], |row| row.get(0)).optional()?.flatten();
    let mut old_words: HashMap<String, (String, String, i64)> = HashMap::new();
    {
        let mut stmt = tx.prepare("SELECT term_key, id, created_at, uses FROM words WHERE source = ?1")?;
        let rows = stmt.query_map([&p.id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, i64>(3)?))
        })?;
        for row in rows {
            let (key, id, created_at, uses) = row?;
            old_words.insert(key, (id, created_at, uses));
        }
    }
    let mut old_snippets: HashMap<String, (String, String)> = HashMap::new();
    {
        let mut stmt = tx.prepare("SELECT trigger_key, id, created_at FROM snippets WHERE source = ?1")?;
        let rows = stmt.query_map([&p.id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))?;
        for row in rows {
            let (key, id, created_at) = row?;
            old_snippets.insert(key, (id, created_at));
        }
    }

    remove_rows(&tx, &p.id)?;

    let now = now_rfc3339();
    for w in &p.words {
        // The user's own word (or another pack's) stands.
        if tx.query_row("SELECT 1 FROM words WHERE term_key = ?1", [&w.key], |_| Ok(())).optional()?.is_some() {
            continue;
        }
        let (id, created_at, uses) = old_words.get(&w.key).cloned().unwrap_or_else(|| (new_id(), now.clone(), 0));
        tx.execute(
            "INSERT INTO words (id, term, term_key, sounds_like, note, source, created_at, uses) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![id, w.term, w.key, sounds_to_json(&w.sounds), w.note, p.id, created_at, uses],
        )?;
    }
    for s in &p.snippets {
        if tx.query_row("SELECT 1 FROM snippets WHERE trigger_key = ?1", [&s.key], |_| Ok(())).optional()?.is_some() {
            continue;
        }
        let (id, created_at) = old_snippets.get(&s.key).cloned().unwrap_or_else(|| (new_id(), now.clone()));
        tx.execute(
            "INSERT INTO snippets (id, trigger_text, trigger_key, expansion, source, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, s.trigger, s.key, s.expansion, p.id, created_at],
        )?;
    }
    for m in &p.modes {
        let wanted = if m.id.is_empty() { slugify(&m.name) } else { m.id.clone() };
        let wanted = if wanted.is_empty() { "mode".to_string() } else { wanted };
        // Never replace a built-in or one of the user's modes: file the pack's under its own name.
        let id = if mode_id_taken(&tx, &wanted)? { unique_mode_id(&tx, &format!("{}-{wanted}", p.id))? } else { wanted };
        let stored = Mode { id: id.clone(), ..m.clone() };
        tx.execute(
            "INSERT INTO modes (id, source, json) VALUES (?1, ?2, ?3)",
            params![id, p.id, serde_json::to_string(&stored)?],
        )?;
    }

    let source_url = p.source_url.clone().or(previous_url);
    tx.execute(
        "INSERT INTO packs (id, name, author, description, version, source_url, installed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![p.id, p.name, p.author, p.description, p.version, source_url, now],
    )?;
    let summary = match tx.query_row(&format!("{SUMMARY_SQL} WHERE p.id = ?1"), [&p.id], summary_from_row).optional()? {
        Some(summary) => summary,
        None => bail!("The pack could not be read back after installing."),
    };
    tx.commit()?;
    Ok(summary)
}

const SUMMARY_SQL: &str = "SELECT p.id, p.name, p.author, p.description, p.version, p.source_url, p.installed_at,
        (SELECT COUNT(*) FROM words WHERE source = p.id),
        (SELECT COUNT(*) FROM snippets WHERE source = p.id),
        (SELECT COUNT(*) FROM modes WHERE source = p.id)
     FROM packs p";

fn summary_from_row(row: &Row<'_>) -> rusqlite::Result<PackSummary> {
    let count = |index: usize| -> rusqlite::Result<usize> { Ok(usize::try_from(row.get::<_, i64>(index)?).unwrap_or(0)) };
    Ok(PackSummary {
        id: row.get(0)?,
        name: row.get(1)?,
        author: row.get(2)?,
        description: row.get(3)?,
        version: row.get(4)?,
        source_url: row.get(5)?,
        installed_at: row.get(6)?,
        words: count(7)?,
        snippets: count(8)?,
        modes: count(9)?,
    })
}

// ---------------------------------------------------------------------------------------------
// Downloading
// ---------------------------------------------------------------------------------------------

/// Checks an address typed by the user. Packs can only come from https, so the traffic is private
/// and cannot be rewritten on the way.
pub(crate) fn check_pack_url(raw: &str, require_https: bool) -> Result<Url> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("Enter the web address of the pack.");
    }
    let url = Url::parse(raw).map_err(|_| anyhow!("That doesn't look like a web address. It should start with https://"))?;
    match url.scheme() {
        "https" => {}
        "http" if !require_https => {}
        _ => bail!("Packs can only be downloaded from https:// addresses."),
    }
    if url.host_str().is_none_or(str::is_empty) {
        bail!("That web address has no website name in it.");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("Take the username and password out of the address; Navi does not store them.");
    }
    Ok(url)
}

/// Whether a redirect may be followed. Anything that would leave https is refused.
pub(crate) fn redirect_verdict(next: &Url, hops_so_far: usize, require_https: bool) -> std::result::Result<(), &'static str> {
    if hops_so_far >= MAX_REDIRECTS {
        return Err("the address redirected too many times");
    }
    if require_https && next.scheme() != "https" {
        return Err("the address redirected somewhere that is not https");
    }
    Ok(())
}

pub(crate) fn redirect_policy(require_https: bool) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| match redirect_verdict(attempt.url(), attempt.previous().len(), require_https) {
        Ok(()) => attempt.follow(),
        Err(why) => attempt.error(why),
    })
}

fn download_client(require_https: bool) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("Navi/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(redirect_policy(require_https))
        .build()
        .context("setting up the download")
}

fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut next = error.source();
    while let Some(cause) = next {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        next = cause.source();
    }
    text
}

/// "5 MB", "300 KB" or "12 bytes", for messages.
fn size_text(bytes: usize) -> String {
    const KB: usize = 1024;
    const MB: usize = 1024 * 1024;
    if bytes >= MB {
        format!("{} MB", bytes / MB)
    } else if bytes >= KB {
        format!("{} KB", bytes / KB)
    } else {
        format!("{bytes} bytes")
    }
}

/// Downloads `url`, refusing anything bigger than `max_bytes` (judged from the announced length
/// first, then by counting what actually arrives).
pub(crate) async fn fetch_limited(client: &reqwest::Client, url: &Url, max_bytes: usize) -> Result<Vec<u8>> {
    let host = url.host_str().unwrap_or("that server").to_string();
    let too_big = || anyhow!("That pack is bigger than {}, so Navi won't download it.", size_text(max_bytes));
    let explain = |e: reqwest::Error| {
        let e = e.without_url(); // the address may carry an access token, so keep it out of messages
        if e.is_timeout() {
            anyhow!("{host} took too long to answer.")
        } else if e.is_connect() {
            anyhow!("Couldn't connect to {host}.")
        } else {
            anyhow!("Couldn't download the pack from {host}: {}", error_chain(&e))
        }
    };

    let mut response = client
        .get(url.clone())
        .header(reqwest::header::ACCEPT, "application/json, */*;q=0.5")
        .send()
        .await
        .map_err(explain)?;
    let status = response.status();
    if !status.is_success() {
        bail!("{host} answered \"{status}\" instead of sending the pack.");
    }
    if let Some(length) = response.content_length() {
        if length > max_bytes as u64 {
            return Err(too_big());
        }
    }
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(explain)? {
        if body.len() + chunk.len() > max_bytes {
            return Err(too_big());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn install_from_url_with(db: &Db, client: &reqwest::Client, raw_url: &str, require_https: bool) -> Result<PackSummary> {
    let url = check_pack_url(raw_url, require_https)?;
    let bytes = fetch_limited(client, &url, MAX_PACK_BYTES).await?;
    let mut pack = parse_pack_bytes(&bytes)?;
    pack.source_url = Some(url.to_string());
    db.install_pack(&pack)
}

async fn refresh_all(db: &Db, client: &reqwest::Client, require_https: bool) -> Vec<String> {
    let subscriptions = match db.subscriptions() {
        Ok(subscriptions) => subscriptions,
        Err(e) => return vec![format!("Couldn't read the list of installed packs: {e:#}")],
    };
    let mut problems = Vec::new();
    for subscription in &subscriptions {
        match refresh_one(db, client, subscription, require_https).await {
            Ok(true) => log::info!("pack {} was updated", subscription.id),
            Ok(false) => {}
            Err(e) => problems.push(format!("{}: {e:#}", subscription.name)),
        }
    }
    problems
}

async fn refresh_one(db: &Db, client: &reqwest::Client, subscription: &Subscription, require_https: bool) -> Result<bool> {
    let url = check_pack_url(&subscription.url, require_https)?;
    let bytes = fetch_limited(client, &url, MAX_PACK_BYTES).await?;
    let fetched = parse_pack_bytes(&bytes)?;
    apply_fetched(db, subscription, fetched)
}

/// Installs `fetched` over the subscribed pack if it is the same pack with a different version.
/// Returns whether anything changed.
pub(crate) fn apply_fetched(db: &Db, subscription: &Subscription, mut fetched: Pack) -> Result<bool> {
    let fetched_id = pack_id_for(&fetched.id, &fetched.name)?;
    if fetched_id != subscription.id {
        bail!("The address now serves a pack called \"{fetched_id}\" instead of \"{}\", so nothing was changed.", subscription.id);
    }
    if fetched.version.trim() == subscription.version.trim() {
        return Ok(false);
    }
    fetched.source_url = Some(subscription.url.clone());
    db.install_pack(&fetched)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testutil::TempDir;
    use crate::types::{DictEntry, ModeKind, Snippet};
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn db() -> Db {
        Db::open_memory().unwrap()
    }

    fn word(term: &str, sounds: &[&str]) -> DictEntry {
        DictEntry { term: term.into(), sounds_like: sounds.iter().map(|s| s.to_string()).collect(), ..DictEntry::default() }
    }

    fn snippet(trigger: &str, expansion: &str) -> Snippet {
        Snippet { trigger: trigger.into(), expansion: expansion.into(), ..Snippet::default() }
    }

    fn mode(id: &str, name: &str) -> Mode {
        Mode {
            id: id.into(),
            name: name.into(),
            icon: "pen".into(),
            description: "A pack mode".into(),
            kind: ModeKind::Rewrite,
            instructions: "Keep it short.".into(),
            apps: vec!["outlook.exe".into()],
            use_ai: true,
            builtin: false,
        }
    }

    fn pack(id: &str, version: &str) -> Pack {
        Pack {
            id: id.into(),
            name: format!("Pack {id}"),
            author: "Alex".into(),
            description: "A test pack".into(),
            version: version.into(),
            words: vec![word("Quikly", &["quickly", "quick ly"]), word("Figma", &["fig ma"])],
            snippets: vec![snippet("my calendar link", "https://cal.example/alex")],
            modes: vec![mode("sales-email", "Sales email")],
            source_url: None,
        }
    }

    fn terms(db: &Db) -> Vec<String> {
        db.list_words().unwrap().into_iter().map(|w| w.term).collect()
    }

    fn triggers(db: &Db) -> Vec<String> {
        db.list_snippets().unwrap().into_iter().map(|s| s.trigger).collect()
    }

    fn mode_ids(db: &Db) -> Vec<String> {
        db.list_stored_modes().unwrap().into_iter().map(|m| m.id).collect()
    }

    fn source_of_word(db: &Db, term: &str) -> String {
        db.list_words().unwrap().into_iter().find(|w| w.term == term).unwrap().source
    }

    fn pack_json(id: &str, version: &str, word: &str) -> String {
        json!({
            "format": "navi-pack/1", "id": id, "name": id, "version": version,
            "words": [{"term": word, "soundsLike": ["x"]}]
        })
        .to_string()
    }

    // ---- export and round trip ----

    fn seeded_source_db() -> Db {
        let db = db();
        db.upsert_word(&DictEntry { note: "product name".into(), uses: 12, ..word("Quikly", &["quickly", "quick ly"]) }).unwrap();
        db.upsert_word(&word("Tauri", &["tory"])).unwrap();
        db.upsert_word(&DictEntry { source: "learned".into(), ..word("Whisper", &["whisker"]) }).unwrap();
        db.upsert_snippet(&snippet("my calendar link", "https://cal.example/alex")).unwrap();
        db.save_mode(&mode("", "Sales email")).unwrap();
        // Things that must NOT be exported: a built-in override and everything from another pack.
        db.save_mode(&Mode { builtin: true, ..mode("email", "Email") }).unwrap();
        db.install_pack(&pack("other", "1")).unwrap();
        db
    }

    #[test]
    fn export_then_install_into_a_fresh_database_round_trips() {
        let dir = TempDir::new("pack-roundtrip");
        let path = dir.join("team.navipack");
        let source = seeded_source_db();

        let exported = source.export_pack(&path, "Acme Team", "Alex", "Our jargon").unwrap();
        assert_eq!(exported.id, "acme-team");
        assert_eq!(exported.name, "Acme Team");
        assert_eq!(exported.author, "Alex");
        assert_eq!(exported.description, "Our jargon");
        assert_eq!((exported.words, exported.snippets, exported.modes), (3, 1, 1), "own words (manual + learned), own snippets, own custom modes");
        assert!(exported.source_url.is_none());
        assert!(!exported.version.is_empty());

        let fresh = db();
        let installed = fresh.install_pack_file(&path).unwrap();
        assert_eq!(installed.id, "acme-team");
        assert_eq!((installed.words, installed.snippets, installed.modes), (3, 1, 1));
        assert_eq!(installed.version, exported.version);
        assert!(!installed.installed_at.is_empty());

        let words = fresh.list_words().unwrap();
        assert_eq!(words.iter().map(|w| w.term.as_str()).collect::<Vec<_>>(), ["Quikly", "Tauri", "Whisper"]);
        let quikly = &words[0];
        assert_eq!(quikly.sounds_like, ["quickly", "quick ly"]);
        assert_eq!(quikly.note, "product name");
        assert_eq!(quikly.source, "acme-team");
        assert_eq!(quikly.uses, 0, "the author's use counts are not shared");
        assert!(words.iter().all(|w| w.source == "acme-team"));
        assert!(source.list_words().unwrap().iter().all(|w| w.id != quikly.id), "new ids on the receiving side");

        let snippets = fresh.list_snippets().unwrap();
        assert_eq!((snippets[0].trigger.as_str(), snippets[0].expansion.as_str(), snippets[0].source.as_str()), ("my calendar link", "https://cal.example/alex", "acme-team"));

        let modes = fresh.list_stored_modes().unwrap();
        assert_eq!(modes.len(), 1);
        assert_eq!((modes[0].id.as_str(), modes[0].name.as_str(), modes[0].instructions.as_str()), ("sales-email", "Sales email", "Keep it short."));
        assert!(!modes[0].builtin);

        let packs = fresh.list_packs().unwrap();
        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].id, "acme-team");
    }

    #[test]
    fn exported_files_are_tidy_hand_editable_json_without_internal_details() {
        let dir = TempDir::new("pack-file");
        let path = dir.join("tidy.navipack");
        let source = seeded_source_db();
        source.export_pack(&path, "Tidy", "", "").unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert_ne!(&bytes[..3], &[0xEF, 0xBB, 0xBF], "no byte order mark");
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.ends_with("}\n"));
        assert!(text.contains("\n  \"words\""), "indented for people to read");

        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["format"], "navi-pack/1");
        assert_eq!(value["id"], "tidy");
        assert!(value.get("sourceUrl").is_none());
        let first = &value["words"][0];
        assert_eq!(first["term"], "Quikly");
        assert_eq!(first["soundsLike"], json!(["quickly", "quick ly"]));
        assert_eq!(first["note"], "product name");
        for forbidden in ["id", "uses", "createdAt", "source"] {
            assert!(first.get(forbidden).is_none(), "word should not carry {forbidden}");
        }
        assert!(value["words"][1].get("note").is_none(), "an empty note is left out");
        assert_eq!(value["snippets"][0]["trigger"], "my calendar link");
        assert_eq!(value["modes"][0]["name"], "Sales email");
        assert_eq!(value["modes"][0]["builtin"], false);
        let names: Vec<&str> = value["words"].as_array().unwrap().iter().map(|w| w["term"].as_str().unwrap()).collect();
        assert!(!names.contains(&"Figma"), "words from another pack are not exported");
    }

    #[test]
    fn exporting_needs_a_name_and_something_to_share() {
        let dir = TempDir::new("pack-export-errors");
        let path = dir.join("x.navipack");
        let empty = db();
        let err = empty.export_pack(&path, "Team", "", "").unwrap_err();
        assert!(format!("{err:#}").contains("nothing to put in a pack"), "{err:#}");
        assert!(!path.exists());

        let some = seeded_source_db();
        assert!(some.export_pack(&path, "   ", "", "").is_err());
        assert!(some.export_pack(&path, "!!!", "", "").is_err());
        assert!(!path.exists());

        // Only pack content and built-in overrides: still nothing of the user's own.
        let only_pack = db();
        only_pack.install_pack(&pack("p", "1")).unwrap();
        only_pack.save_mode(&Mode { builtin: true, ..mode("email", "Email") }).unwrap();
        assert!(only_pack.export_pack(&path, "Team", "", "").is_err());
    }

    #[test]
    fn exporting_creates_folders_replaces_a_file_cleanly_and_leaves_no_temp_file() {
        let dir = TempDir::new("pack-export-paths");
        let path = dir.join("a").join("b").join("team.navipack");
        let source = seeded_source_db();
        source.export_pack(&path, "Team", "", "first").unwrap();
        source.upsert_word(&word("Extra", &[])).unwrap();
        source.export_pack(&path, "Team", "", "second").unwrap();
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["description"], "second");
        assert_eq!(value["words"].as_array().unwrap().len(), 4);
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(leftovers.len(), 1, "{leftovers:?}");
    }

    #[test]
    fn a_pack_named_manual_gets_a_safe_id() {
        let dir = TempDir::new("pack-reserved");
        let path = dir.join("m.navipack");
        let source = seeded_source_db();
        assert_eq!(source.export_pack(&path, "Manual", "", "").unwrap().id, "manual-pack");
        assert_eq!(source.export_pack(&path, "Learned", "", "").unwrap().id, "learned-pack");
        let fresh = db();
        assert_eq!(fresh.install_pack_file(&path).unwrap().id, "learned-pack");
    }

    #[test]
    fn re_exporting_gives_a_version_subscribers_can_tell_apart_over_time() {
        let dir = TempDir::new("pack-version");
        let source = seeded_source_db();
        let v = source.export_pack(&dir.join("v.navipack"), "Team", "", "").unwrap().version;
        let parts: Vec<&str> = v.split('.').collect();
        assert_eq!(parts.len(), 4, "{v}");
        assert_eq!(parts[0].len(), 4);
        assert_eq!(parts[3].len(), 4);
    }

    // ---- install ----

    #[test]
    fn installing_replaces_the_previous_version_of_the_same_pack() {
        let db = db();
        let v1 = Pack {
            words: vec![word("Alpha", &["al"]), word("Bravo", &["bra"])],
            snippets: vec![snippet("one", "1")],
            modes: vec![mode("m1", "Mode one")],
            ..pack("team", "1")
        };
        let first = db.install_pack(&v1).unwrap();
        assert_eq!((first.words, first.snippets, first.modes), (2, 1, 1));
        assert_eq!(first.version, "1");

        db.bump_word_uses(&["bravo".to_string(), "bravo".to_string()]).unwrap();
        let bravo_before = db.list_words().unwrap().into_iter().find(|w| w.term == "Bravo").unwrap();
        assert_eq!(bravo_before.uses, 2);

        let v2 = Pack {
            words: vec![word("Bravo", &["bra", "brah"]), word("Charlie", &[])],
            snippets: vec![snippet("two", "2")],
            modes: vec![],
            ..pack("team", "2")
        };
        let second = db.install_pack(&v2).unwrap();
        assert_eq!((second.words, second.snippets, second.modes), (2, 1, 0));
        assert_eq!(second.version, "2");

        assert_eq!(terms(&db), ["Bravo", "Charlie"], "Alpha left with version 1");
        assert_eq!(triggers(&db), ["two"]);
        assert!(mode_ids(&db).is_empty());
        let bravo = db.list_words().unwrap().into_iter().find(|w| w.term == "Bravo").unwrap();
        assert_eq!(bravo.id, bravo_before.id, "same word keeps its id across updates");
        assert_eq!(bravo.uses, 2, "and its use count");
        assert_eq!(bravo.created_at, bravo_before.created_at);
        assert_eq!(bravo.sounds_like, ["bra", "brah"], "but takes the new content");
        assert_eq!(db.list_packs().unwrap().len(), 1, "still one pack");
    }

    #[test]
    fn installing_never_overwrites_the_users_own_entries() {
        let db = db();
        db.upsert_word(&word("Tauri", &["mine"])).unwrap();
        db.upsert_word(&DictEntry { source: "learned".into(), ..word("Rust", &["rusty"]) }).unwrap();
        db.upsert_snippet(&snippet("Signature", "My own sign-off")).unwrap();

        let p = Pack {
            words: vec![word("tauri", &["pack"]), word("RUST", &["pack"]), word("Other", &["pack"])],
            snippets: vec![snippet("signature", "Pack sign-off"), snippet("extra", "More")],
            modes: vec![],
            ..pack("team", "1")
        };
        let summary = db.install_pack(&p).unwrap();
        assert_eq!((summary.words, summary.snippets), (1, 1), "only what was actually installed is counted");

        let words = db.list_words().unwrap();
        let find = |t: &str| words.iter().find(|w| w.term == t).unwrap();
        assert_eq!(find("Tauri").sounds_like, ["mine"]);
        assert_eq!(find("Tauri").source, "manual");
        assert_eq!(find("Rust").sounds_like, ["rusty"]);
        assert_eq!(find("Rust").source, "learned");
        assert_eq!(find("Other").source, "team");
        let snippets = db.list_snippets().unwrap();
        let sig = snippets.iter().find(|s| s.trigger == "Signature").unwrap();
        assert_eq!((sig.expansion.as_str(), sig.source.as_str()), ("My own sign-off", "manual"));
        assert!(snippets.iter().any(|s| s.trigger == "extra" && s.source == "team"));
        assert_eq!(words.len(), 3);
    }

    #[test]
    fn a_term_in_two_packs_belongs_to_the_first_one_installed() {
        let db = db();
        db.install_pack(&Pack { words: vec![word("Shared", &["one"])], snippets: vec![], modes: vec![], ..pack("first", "1") }).unwrap();
        let second = db.install_pack(&Pack { words: vec![word("shared", &["two"]), word("Mine", &[])], snippets: vec![], modes: vec![], ..pack("second", "1") }).unwrap();
        assert_eq!(second.words, 1);
        assert_eq!(source_of_word(&db, "Shared"), "first");
        db.remove_pack("second").unwrap();
        assert_eq!(terms(&db), ["Shared"]);
    }

    #[test]
    fn removing_a_pack_deletes_exactly_what_it_installed() {
        let db = db();
        db.upsert_word(&word("Mine", &[])).unwrap();
        db.upsert_snippet(&snippet("my own", "text")).unwrap();
        db.save_mode(&mode("", "My mode")).unwrap();
        db.install_pack(&pack("alpha", "1")).unwrap();
        let beta = Pack { words: vec![word("BetaWord", &[])], snippets: vec![snippet("beta phrase", "beta")], modes: vec![mode("beta-mode", "Beta mode")], ..pack("beta", "1") };
        db.install_pack(&beta).unwrap();
        assert_eq!(db.list_packs().unwrap().len(), 2);

        db.remove_pack("alpha").unwrap();
        assert_eq!(terms(&db), ["BetaWord", "Mine"]);
        assert_eq!(triggers(&db), ["beta phrase", "my own"]);
        assert_eq!(mode_ids(&db), ["my-mode", "beta-mode"]);
        let packs = db.list_packs().unwrap();
        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].id, "beta");

        db.remove_pack("alpha").unwrap();
        db.remove_pack("never-installed").unwrap();
        db.remove_pack("").unwrap();
        assert_eq!(terms(&db), ["BetaWord", "Mine"], "removing twice or removing nothing changes nothing");
    }

    #[test]
    fn removing_manual_or_learned_never_deletes_the_users_own_entries() {
        let db = db();
        db.upsert_word(&word("Mine", &[])).unwrap();
        db.upsert_word(&DictEntry { source: "learned".into(), ..word("Learned", &[]) }).unwrap();
        db.upsert_snippet(&snippet("mine", "text")).unwrap();
        db.save_mode(&mode("", "My mode")).unwrap();
        db.remove_pack("manual").unwrap();
        db.remove_pack("learned").unwrap();
        db.remove_pack("  manual ").unwrap();
        assert_eq!(terms(&db), ["Learned", "Mine"]);
        assert_eq!(triggers(&db), ["mine"]);
        assert_eq!(mode_ids(&db), ["my-mode"]);
    }

    #[test]
    fn entries_the_user_edited_survive_the_pack_being_removed() {
        let db = db();
        db.install_pack(&pack("team", "1")).unwrap();
        let quikly = db.list_words().unwrap().into_iter().find(|w| w.term == "Quikly").unwrap();
        db.upsert_word(&DictEntry { note: "I changed this".into(), ..quikly }).unwrap();
        let sig = db.list_snippets().unwrap().remove(0);
        db.upsert_snippet(&Snippet { expansion: "my link".into(), ..sig }).unwrap();
        let installed_mode = db.list_stored_modes().unwrap().remove(0);
        db.save_mode(&Mode { instructions: "my tweak".into(), ..installed_mode }).unwrap();

        assert_eq!(db.list_packs().unwrap()[0].words, 1, "the claimed word no longer counts for the pack");
        db.remove_pack("team").unwrap();
        assert_eq!(terms(&db), ["Quikly"]);
        assert_eq!(db.list_words().unwrap()[0].source, "manual");
        assert_eq!(triggers(&db), ["my calendar link"]);
        assert_eq!(mode_ids(&db), ["sales-email"]);
    }

    #[test]
    fn a_reinstall_does_not_take_back_a_word_the_user_claimed() {
        let db = db();
        db.install_pack(&pack("team", "1")).unwrap();
        let quikly = db.list_words().unwrap().into_iter().find(|w| w.term == "Quikly").unwrap();
        db.upsert_word(&DictEntry { sounds_like: vec!["my own".into()], ..quikly }).unwrap();
        db.install_pack(&pack("team", "2")).unwrap();
        let quikly = db.list_words().unwrap().into_iter().find(|w| w.term == "Quikly").unwrap();
        assert_eq!(quikly.sounds_like, ["my own"]);
        assert_eq!(quikly.source, "manual");
    }

    #[test]
    fn modes_that_clash_with_built_ins_or_the_users_modes_are_filed_under_the_pack() {
        let db = db();
        db.save_mode(&mode("", "Sales email")).unwrap(); // takes the id "sales-email"
        let p = Pack {
            words: vec![],
            snippets: vec![],
            modes: vec![mode("sales-email", "Sales email"), mode("email", "Pack email"), mode("", "Fresh Mode"), mode("Weird ID!", "Odd id")],
            ..pack("team", "1")
        };
        let summary = db.install_pack(&p).unwrap();
        assert_eq!(summary.modes, 4);
        assert_eq!(mode_ids(&db), ["sales-email", "team-sales-email", "team-email", "fresh-mode", "weird-id"]);
        let users = db.list_stored_modes().unwrap().remove(0);
        assert_eq!(users.instructions, "Keep it short.");

        // Reinstalling lands on the same ids, and removing takes only the pack's.
        db.install_pack(&p).unwrap();
        assert_eq!(mode_ids(&db), ["sales-email", "team-sales-email", "team-email", "fresh-mode", "weird-id"]);
        db.remove_pack("team").unwrap();
        assert_eq!(mode_ids(&db), ["sales-email"]);
    }

    #[test]
    fn a_pack_cannot_make_its_modes_built_in() {
        let db = db();
        let sneaky = Mode { builtin: true, ..mode("x", "Sneaky") };
        db.install_pack(&Pack { words: vec![], snippets: vec![], modes: vec![sneaky], ..pack("team", "1") }).unwrap();
        assert!(!db.list_stored_modes().unwrap()[0].builtin);
    }

    #[test]
    fn bad_packs_are_rejected_and_change_nothing() {
        let db = db();
        db.install_pack(&pack("team", "1")).unwrap();
        let before = (terms(&db), triggers(&db), mode_ids(&db));

        let broken = |change: &dyn Fn(&mut Pack)| {
            let mut p = pack("team", "2");
            change(&mut p);
            p
        };
        let cases: Vec<(&str, Pack)> = vec![
            ("no id or name", Pack { id: String::new(), name: String::new(), ..pack("team", "2") }),
            ("only symbols", Pack { id: "!!!".into(), name: "???".into(), ..pack("team", "2") }),
            ("reserved manual", Pack { id: "manual".into(), ..pack("team", "2") }),
            ("reserved learned", Pack { id: "Learned".into(), ..pack("team", "2") }),
            ("blank word", broken(&|p| p.words.push(word("   ", &["x"])))),
            ("long word", broken(&|p| p.words.push(word(&"x".repeat(MAX_TERM_CHARS + 1), &[])))),
            ("too many sounds", broken(&|p| p.words.push(DictEntry { sounds_like: vec!["s".into(); MAX_SOUNDS_PER_WORD + 1], ..word("Many", &[]) }))),
            ("no trigger", broken(&|p| p.snippets.push(snippet(" ", "text")))),
            ("no expansion", broken(&|p| p.snippets.push(snippet("phrase", "  \n")))),
            ("no mode name", broken(&|p| p.modes.push(mode("m", "  ")))),
            ("too many words", broken(&|p| p.words = (0..=MAX_WORDS).map(|i| word(&format!("w{i}"), &[])).collect())),
        ];
        for (label, bad) in cases {
            assert!(db.install_pack(&bad).is_err(), "{label} should be rejected");
            assert_eq!((terms(&db), triggers(&db), mode_ids(&db)), before, "{label} must leave the installed pack alone");
            assert_eq!(db.list_packs().unwrap()[0].version, "1", "{label}");
        }
    }

    #[test]
    fn pack_ids_are_normalised_and_fall_back_to_the_name() {
        let db = db();
        assert_eq!(db.install_pack(&Pack { id: "Acme_Team!".into(), ..pack("x", "1") }).unwrap().id, "acme-team");
        let from_name = db.install_pack(&Pack { id: "  ".into(), name: "Sales Crew".into(), ..pack("x", "1") }).unwrap();
        assert_eq!(from_name.id, "sales-crew");
        assert_eq!(from_name.name, "Sales Crew");
        let no_name = db.install_pack(&Pack { id: "only-id".into(), name: String::new(), ..pack("x", "1") }).unwrap();
        assert_eq!(no_name.name, "only-id");
    }

    #[test]
    fn duplicates_inside_one_pack_are_merged() {
        let db = db();
        let p = Pack {
            words: vec![DictEntry { note: "first note".into(), ..word("Tauri", &["tory"]) }, DictEntry { note: "second".into(), ..word("tauri", &["tar ee", "TORY"]) }],
            snippets: vec![snippet("sig", "one"), snippet("SIG", "two")],
            modes: vec![],
            ..pack("team", "1")
        };
        let summary = db.install_pack(&p).unwrap();
        assert_eq!((summary.words, summary.snippets), (1, 1));
        let w = &db.list_words().unwrap()[0];
        assert_eq!(w.sounds_like, ["tory", "tar ee"]);
        assert_eq!(w.note, "first note");
        assert_eq!(db.list_snippets().unwrap()[0].expansion, "two", "the later snippet wins");
    }

    #[test]
    fn packs_list_by_name_with_live_counts() {
        let db = db();
        let zebra = Pack { name: "zebra".into(), words: vec![word("Zed", &[])], snippets: vec![snippet("zed", "z")], modes: vec![], ..pack("z", "1") };
        db.install_pack(&zebra).unwrap();
        db.install_pack(&Pack { name: "Apple".into(), ..pack("a", "1") }).unwrap();
        let listed = db.list_packs().unwrap();
        assert_eq!(listed.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Apple", "zebra"]);
        assert_eq!((listed[0].words, listed[0].snippets, listed[0].modes), (2, 1, 1));
        db.delete_word(&db.list_words().unwrap().iter().find(|w| w.source == "a").unwrap().id).unwrap();
        assert_eq!(db.list_packs().unwrap()[0].words, 1, "counts follow the tables");
        assert_eq!(listed[0].author, "Alex");
        assert_eq!(listed[0].description, "A test pack");
    }

    // ---- files ----

    #[test]
    fn parsing_checks_the_format_marker() {
        let ok = pack_json("demo", "1", "Tauri");
        assert_eq!(parse_pack_bytes(ok.as_bytes()).unwrap().words[0].term, "Tauri");

        let mut with_bom = vec![0xEF, 0xBB, 0xBF];
        with_bom.extend_from_slice(ok.as_bytes());
        assert!(parse_pack_bytes(&with_bom).is_ok(), "Notepad's byte order mark is tolerated");

        let fails = |text: &str| format!("{:#}", parse_pack_bytes(text.as_bytes()).unwrap_err());
        assert!(fails("not json at all").contains("isn't valid JSON"));
        assert!(fails("").contains("isn't valid JSON"));
        assert!(fails("[1, 2]").contains("no \"format\""));
        assert!(fails(&json!({"id": "x", "words": []}).to_string()).contains("no \"format\""));
        assert!(fails(&json!({"format": "settings/1"}).to_string()).contains("isn't a Navi pack"));
        assert!(fails(&json!({"format": "navi-pack/2"}).to_string()).contains("Update Navi"));
        assert!(fails(&json!({"format": "navi-pack/17"}).to_string()).contains("newer"));
        assert!(fails(&json!({"format": "navi-pack/0"}).to_string()).contains("isn't a Navi pack"));
        assert!(fails(&json!({"format": 1}).to_string()).contains("no \"format\""), "a non-text format is no format");
        assert!(fails(&json!({"format": "navi-pack/1", "words": "oops"}).to_string()).contains("damaged"));
        assert!(fails(&json!({"format": "navi-pack/1", "modes": [{"name": "x", "kind": "magic"}]}).to_string()).contains("damaged"));
    }

    #[test]
    fn a_hand_written_pack_can_be_minimal() {
        let text = json!({
            "format": "navi-pack/1",
            "name": "Minimal",
            "extra": "ignored",
            "words": [{"term": "Tauri", "extra": 1}],
            "snippets": [{"trigger": "hi", "expansion": "hello"}],
            "modes": [{"name": "Brief"}]
        })
        .to_string();
        let parsed = parse_pack_bytes(text.as_bytes()).unwrap();
        assert_eq!(parsed.modes[0].icon, "spark");
        assert_eq!(parsed.modes[0].kind, ModeKind::Rewrite);
        assert!(parsed.modes[0].use_ai);
        assert!(!parsed.modes[0].builtin);

        let db = db();
        let summary = db.install_pack(&parsed).unwrap();
        assert_eq!(summary.id, "minimal");
        assert_eq!((summary.words, summary.snippets, summary.modes), (1, 1, 1));
        assert_eq!(mode_ids(&db), ["brief"]);
    }

    #[test]
    fn installing_from_a_file_reports_problems_with_the_file_name() {
        let dir = TempDir::new("pack-file-errors");
        let db = db();
        let missing = db.install_pack_file(&dir.join("nope.navipack")).unwrap_err();
        assert!(format!("{missing:#}").contains("nope.navipack"));

        let junk = dir.join("junk.navipack");
        std::fs::write(&junk, "hello").unwrap();
        let err = db.install_pack_file(&junk).unwrap_err();
        assert!(format!("{err:#}").contains("junk.navipack"));
        assert!(db.list_packs().unwrap().is_empty());
    }

    #[test]
    fn a_file_over_the_size_limit_is_refused() {
        let dir = TempDir::new("pack-file-big");
        let big = dir.join("big.navipack");
        let mut text = pack_json("big", "1", "Tauri");
        text.push_str(&" ".repeat(MAX_PACK_BYTES));
        std::fs::write(&big, text).unwrap();
        let err = db().install_pack_file(&big).unwrap_err();
        assert!(format!("{err:#}").contains("5 MB"), "{err:#}");
    }

    #[test]
    fn a_source_url_written_in_a_file_is_ignored() {
        let dir = TempDir::new("pack-file-url");
        let path = dir.join("p.navipack");
        let text = json!({"format": "navi-pack/1", "id": "sneaky", "name": "Sneaky", "version": "1", "sourceUrl": "https://evil.example/pack", "words": [{"term": "Tauri"}]});
        std::fs::write(&path, text.to_string()).unwrap();
        let db = db();
        let summary = db.install_pack_file(&path).unwrap();
        assert!(summary.source_url.is_none());
        assert!(db.subscriptions().unwrap().is_empty());
    }

    #[test]
    fn a_subscribed_pack_keeps_its_address_when_updated_from_a_file() {
        let dir = TempDir::new("pack-file-keepurl");
        let path = dir.join("p.navipack");
        std::fs::write(&path, pack_json("team", "2", "Tauri")).unwrap();
        let db = db();
        db.install_pack(&Pack { source_url: Some("https://packs.example/team.navipack".into()), ..pack("team", "1") }).unwrap();
        let updated = db.install_pack_file(&path).unwrap();
        assert_eq!(updated.version, "2");
        assert_eq!(updated.source_url.as_deref(), Some("https://packs.example/team.navipack"));
        // An explicit new address wins.
        let moved = db.install_pack(&Pack { source_url: Some("https://elsewhere.example/t".into()), ..pack("team", "3") }).unwrap();
        assert_eq!(moved.source_url.as_deref(), Some("https://elsewhere.example/t"));
        assert_eq!(db.subscriptions().unwrap().len(), 1);
    }

    // ---- addresses ----

    #[test]
    fn only_https_addresses_are_accepted() {
        assert!(check_pack_url("https://packs.example/team.navipack", true).is_ok());
        assert!(check_pack_url("  https://packs.example/team.navipack  ", true).is_ok());
        for bad in [
            "",
            "   ",
            "packs.example/team.navipack",
            "http://packs.example/team.navipack",
            "ftp://packs.example/team.navipack",
            "file:///C:/packs/team.navipack",
            "javascript:alert(1)",
            "https://",
            "https://user:secret@packs.example/team.navipack",
            "https://user@packs.example/team.navipack",
            "not a url",
        ] {
            assert!(check_pack_url(bad, true).is_err(), "{bad:?} should be refused");
        }
        assert!(check_pack_url("http://127.0.0.1:8080/x", false).is_ok(), "plain http only when a test asks for it");
        assert!(check_pack_url("ftp://127.0.0.1/x", false).is_err());
    }

    #[test]
    fn sizes_read_naturally_in_messages() {
        assert_eq!(size_text(MAX_PACK_BYTES), "5 MB");
        assert_eq!(size_text(300 * 1024), "300 KB");
        assert_eq!(size_text(12), "12 bytes");
    }

    #[test]
    fn redirects_may_not_leave_https_or_loop() {
        let https = Url::parse("https://other.example/pack").unwrap();
        let http = Url::parse("http://other.example/pack").unwrap();
        assert!(redirect_verdict(&https, 0, true).is_ok());
        assert!(redirect_verdict(&https, MAX_REDIRECTS - 1, true).is_ok());
        assert!(redirect_verdict(&https, MAX_REDIRECTS, true).is_err());
        assert!(redirect_verdict(&http, 0, true).is_err());
        assert!(redirect_verdict(&http, 0, false).is_ok());
    }

    // ---- network (a throwaway local server; nothing leaves the machine) ----

    struct TestServer {
        base: String,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    impl TestServer {
        fn url(&self, path: &str) -> String {
            format!("{}{path}", self.base)
        }
    }

    fn http_ok(body: &str) -> Vec<u8> {
        format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).into_bytes()
    }

    fn http_redirect(to: &str) -> Vec<u8> {
        format!("HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes()
    }

    async fn serve(routes: Vec<(&str, Vec<u8>)>) -> TestServer {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let routes: Vec<(String, Vec<u8>)> = routes.into_iter().map(|(path, response)| (path.to_string(), response)).collect();
        let task = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let routes = routes.clone();
                tokio::spawn(async move {
                    let mut buffer = vec![0_u8; 8192];
                    let read = socket.read(&mut buffer).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                    let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let response = routes
                        .iter()
                        .find(|(p, _)| *p == path)
                        .map(|(_, r)| r.clone())
                        .unwrap_or_else(|| b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec());
                    let _ = socket.write_all(&response).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        TestServer { base: format!("http://{addr}"), task }
    }

    fn test_client(require_https: bool) -> reqwest::Client {
        // No system proxy: the test server is on this machine.
        reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(10)).redirect(redirect_policy(require_https)).build().unwrap()
    }

    async fn within<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(20), future).await.expect("the test timed out")
    }

    #[tokio::test]
    async fn fetching_returns_the_body_and_enforces_the_size_cap() {
        let big = "x".repeat(1000);
        let server = serve(vec![
            ("/ok", http_ok("hello")),
            ("/big", http_ok(&big)),
            ("/announced", b"HTTP/1.1 200 OK\r\nContent-Length: 99999999\r\nConnection: close\r\n\r\n".to_vec()),
            ("/unannounced", [b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec(), big.clone().into_bytes()].concat()),
        ])
        .await;
        let client = test_client(false);
        let get = |path: &str, cap: usize| {
            let url = Url::parse(&server.url(path)).unwrap();
            let client = client.clone();
            async move { within(fetch_limited(&client, &url, cap)).await }
        };

        assert_eq!(get("/ok", 100).await.unwrap(), b"hello");
        assert_eq!(get("/big", 1000).await.unwrap().len(), 1000, "exactly at the cap is fine");
        let announced = get("/big", 999).await.unwrap_err();
        assert!(format!("{announced:#}").contains("bigger than"), "{announced:#}");
        let header_only = get("/announced", 1024 * 1024).await.unwrap_err();
        assert!(format!("{header_only:#}").contains("bigger than"), "refused from the headers alone: {header_only:#}");
        let counted = get("/unannounced", 500).await.unwrap_err();
        assert!(format!("{counted:#}").contains("bigger than"), "refused by counting what arrived: {counted:#}");
        assert_eq!(get("/unannounced", 1000).await.unwrap().len(), 1000);

        let missing = get("/nope", 100).await.unwrap_err();
        assert!(format!("{missing:#}").contains("404"), "{missing:#}");
    }

    #[tokio::test]
    async fn redirects_are_followed_but_never_out_of_https() {
        let server = serve(vec![("/start", http_redirect("/pack")), ("/pack", http_ok("moved ok"))]).await;
        let url = Url::parse(&server.url("/start")).unwrap();

        let relaxed = within(fetch_limited(&test_client(false), &url, 100)).await.unwrap();
        assert_eq!(relaxed, b"moved ok");

        // A client that insists on https refuses to follow this redirect to plain http.
        let strict = within(fetch_limited(&test_client(true), &url, 100)).await.unwrap_err();
        assert!(format!("{strict:#}").contains("not https"), "{strict:#}");
    }

    #[tokio::test]
    async fn a_redirect_loop_gives_up() {
        let server = serve(vec![("/a", http_redirect("/b")), ("/b", http_redirect("/a"))]).await;
        let url = Url::parse(&server.url("/a")).unwrap();
        let err = within(fetch_limited(&test_client(false), &url, 100)).await.unwrap_err();
        assert!(format!("{err:#}").contains("too many times"), "{err:#}");
    }

    #[tokio::test]
    async fn an_unreachable_server_gives_a_plain_message() {
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap().port()
        };
        let url = Url::parse(&format!("http://127.0.0.1:{port}/pack")).unwrap();
        let err = within(fetch_limited(&test_client(false), &url, 100)).await.unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("127.0.0.1"), "{text}");
        assert!(!text.contains("/pack"), "the address itself is kept out of the message: {text}");
    }

    #[tokio::test]
    async fn installing_from_an_address_installs_and_remembers_it() {
        let server = serve(vec![("/team.navipack", http_ok(&pack_json("team", "7", "Tauri")))]).await;
        let db = db();
        let address = server.url("/team.navipack");
        let summary = within(install_from_url_with(&db, &test_client(false), &address, false)).await.unwrap();
        assert_eq!((summary.id.as_str(), summary.version.as_str()), ("team", "7"));
        assert_eq!(summary.source_url.as_deref(), Some(address.as_str()));
        assert_eq!(terms(&db), ["Tauri"]);
        assert_eq!(db.subscriptions().unwrap(), vec![Subscription { id: "team".into(), name: "team".into(), version: "7".into(), url: address }]);
    }

    #[tokio::test]
    async fn installing_from_an_address_fails_cleanly() {
        let server = serve(vec![
            ("/junk", http_ok("<html>not a pack</html>")),
            ("/other", http_ok(&json!({"format": "settings/1"}).to_string())),
        ])
        .await;
        let db = db();
        let client = test_client(false);
        for path in ["/junk", "/other", "/missing"] {
            let err = within(install_from_url_with(&db, &client, &server.url(path), false)).await.unwrap_err();
            assert!(!format!("{err:#}").is_empty(), "{path}");
        }
        assert!(db.list_packs().unwrap().is_empty());
    }

    fn assert_send<T: Send>(_: &T) {}

    #[test]
    fn the_download_futures_are_send_so_they_can_be_spawned() {
        let db = db();
        assert_send(&install_pack_from_url(&db, "https://packs.example/p.navipack"));
        assert_send(&refresh_subscriptions(&db));
    }

    #[tokio::test]
    async fn the_public_download_function_refuses_anything_but_https() {
        let db = db();
        for bad in ["http://example.com/p.navipack", "", "ftp://example.com/p", "https://user:pw@example.com/p", "nonsense"] {
            let err = install_pack_from_url(&db, bad).await.unwrap_err();
            assert!(!format!("{err:#}").is_empty(), "{bad:?}");
        }
        assert!(db.list_packs().unwrap().is_empty());
    }

    // ---- refreshing subscriptions ----

    #[tokio::test]
    async fn refreshing_reinstalls_only_the_packs_whose_version_changed() {
        let server = serve(vec![
            ("/changed.navipack", http_ok(&pack_json("changed", "2", "NewWord"))),
            ("/same.navipack", http_ok(&pack_json("same", "1", "DifferentButSameVersion"))),
        ])
        .await;
        let db = db();
        let old = |id: &str, path: &str| Pack {
            words: vec![word("OldWord", &[])],
            snippets: vec![],
            modes: vec![],
            source_url: Some(server.url(path)),
            ..pack(id, "1")
        };
        db.install_pack(&old("changed", "/changed.navipack")).unwrap();
        db.install_pack(&Pack { words: vec![word("StaysPut", &[])], ..old("same", "/same.navipack") }).unwrap();

        let problems = within(refresh_all(&db, &test_client(false), false)).await;
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(terms(&db), ["NewWord", "StaysPut"]);
        let packs = db.list_packs().unwrap();
        let changed = packs.iter().find(|p| p.id == "changed").unwrap();
        assert_eq!(changed.version, "2");
        assert_eq!(changed.source_url.as_deref(), Some(server.url("/changed.navipack").as_str()), "still subscribed");
        assert_eq!(packs.iter().find(|p| p.id == "same").unwrap().version, "1");

        // Nothing new the second time round.
        assert!(within(refresh_all(&db, &test_client(false), false)).await.is_empty());
    }

    #[tokio::test]
    async fn one_failing_pack_does_not_stop_the_others() {
        let server = serve(vec![
            ("/good.navipack", http_ok(&pack_json("good", "2", "Fresh"))),
            ("/renamed.navipack", http_ok(&pack_json("something-else", "9", "X"))),
            ("/broken.navipack", http_ok("{{ nope")),
        ])
        .await;
        let db = db();
        let sub = |id: &str, path: &str| Pack { words: vec![word(&format!("Old{id}"), &[])], snippets: vec![], modes: vec![], source_url: Some(server.url(path)), ..pack(id, "1") };
        db.install_pack(&sub("good", "/good.navipack")).unwrap();
        db.install_pack(&sub("renamed", "/renamed.navipack")).unwrap();
        db.install_pack(&sub("broken", "/broken.navipack")).unwrap();
        db.install_pack(&sub("gone", "/missing.navipack")).unwrap();
        db.install_pack(&Pack { words: vec![word("Local", &[])], snippets: vec![], modes: vec![], ..pack("local-only", "1") }).unwrap();

        // The https-only rule is checked against every stored address too, not just on install.
        let strict = within(refresh_all(&db, &download_client(true).unwrap(), true)).await;
        assert_eq!(strict.len(), 4, "every http address is refused when https is required: {strict:?}");
        assert!(strict.iter().all(|p| p.contains("https://")), "{strict:?}");
        assert!(terms(&db).contains(&"Oldgood".to_string()), "nothing changed");

        let problems = within(refresh_all(&db, &test_client(false), false)).await;
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(problems.iter().any(|p| p.starts_with("Pack broken: ") && p.contains("isn't valid JSON")), "{problems:?}");
        assert!(problems.iter().any(|p| p.starts_with("Pack renamed: ") && p.contains("something-else")), "{problems:?}");
        assert!(problems.iter().any(|p| p.starts_with("Pack gone: ") && p.contains("404")), "{problems:?}");
        assert!(terms(&db).contains(&"Fresh".to_string()), "the good pack was still updated: {:?}", terms(&db));
        assert!(terms(&db).contains(&"Oldrenamed".to_string()), "a failed update leaves the old content");
        assert!(terms(&db).contains(&"Local".to_string()));
    }

    #[tokio::test]
    async fn refreshing_with_no_subscriptions_does_nothing() {
        let db = db();
        db.install_pack(&pack("local-only", "1")).unwrap();
        assert!(refresh_subscriptions(&db).await.is_empty());
        assert!(Db::open_memory().map(|d| d.subscriptions().unwrap().is_empty()).unwrap());
    }

    #[tokio::test]
    async fn a_connection_error_is_reported_per_pack() {
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap().port()
        };
        let db = db();
        db.install_pack(&Pack { source_url: Some(format!("https://127.0.0.1:{port}/a.navipack")), ..pack("one", "1") }).unwrap();
        db.install_pack(&Pack { source_url: Some(format!("https://127.0.0.1:{port}/b.navipack")), ..pack("two", "1") }).unwrap();
        let problems = within(refresh_subscriptions(&db)).await;
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].starts_with("Pack one: "));
        assert!(problems[1].starts_with("Pack two: "));
        assert_eq!(terms(&db).len(), 2, "nothing was lost");
    }

    #[test]
    fn applying_a_fetched_pack_checks_identity_and_version() {
        let db = db();
        db.install_pack(&Pack { source_url: Some("https://packs.example/t".into()), ..pack("team", "1") }).unwrap();
        let sub = db.subscriptions().unwrap().remove(0);
        assert_eq!(sub.id, "team");

        let same = Pack { words: vec![word("Different", &[])], ..pack("team", "1") };
        assert!(!apply_fetched(&db, &sub, same).unwrap());
        assert!(!terms(&db).contains(&"Different".to_string()));

        let renamed = Pack { id: "other".into(), ..pack("team", "2") };
        assert!(apply_fetched(&db, &sub, renamed).is_err());

        let reserved = Pack { id: "manual".into(), ..pack("team", "2") };
        assert!(apply_fetched(&db, &sub, reserved).is_err());

        let newer = Pack { words: vec![word("Different", &[])], source_url: Some("https://evil.example/x".into()), ..pack("team", "2") };
        assert!(apply_fetched(&db, &sub, newer).unwrap());
        assert_eq!(terms(&db), ["Different"]);
        assert_eq!(db.list_packs().unwrap()[0].source_url.as_deref(), Some("https://packs.example/t"), "the address stays the subscribed one");

        // An id given in a different spelling is still the same pack.
        let sub = db.subscriptions().unwrap().remove(0);
        let respelled = Pack { id: "TEAM".into(), ..pack("team", "3") };
        assert!(apply_fetched(&db, &sub, respelled).unwrap());
    }
}
