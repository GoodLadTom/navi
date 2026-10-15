//! Takes: every dictation, kept with its audio so nothing is ever lost.
//!
//! OWNER: data agent. STATUS: implemented, with unit tests.
//!
//! Contract:
//! * `list_takes`: newest first; `search` matches raw/final text, app and title; filters combine;
//!   default limit 50.
//! * `stats`: words today / last 7 days / total (sum of `words` for takes whose status is not
//!   cancelled or error), takes total, minutes_saved = words/40 - speaking minutes (floor at 0),
//!   avg_wpm over the last 30 days (words / speaking minutes), streak_days = consecutive local days
//!   up to today (or yesterday) with at least one take, daily = last 14 days oldest first (zeros
//!   included).
//! * `delete_take` returns the audio path so the caller can delete the file.
//! * `purge_audio_older_than(days)` clears audio_path on old takes and returns the files to delete.
//!
//! Details worth knowing
//! * Ordering uses `ts` (UTC milliseconds parsed from `created_at`), not the text of the timestamp,
//!   so takes either side of a clock change or a time zone move still sort correctly.
//! * Days (`day`, for stats) are the calendar date written in the take's own `created_at`, which the
//!   controller sets in local time with the offset. A take stamped in UTC would be filed under the
//!   UTC date.
//! * "Today", "last 7 days" and "last 30 days" are whole calendar days counted back from today, so
//!   the 7-day window is today plus the six days before it.
//! * Everything that feeds the numbers (words, takes total, minutes saved, average speed and the
//!   streak) ignores takes whose status is cancelled or error. The average speed also leaves out
//!   takes with no recorded length, so a take without a duration cannot inflate it.
//! * Search runs on the FTS5 index: every word typed must match the start of a word in the raw
//!   text, final text, app name or window title, case and accents ignored ("inv" finds "Invoice").
//!   If the index is missing, the query is only punctuation, or nothing starts with the words typed,
//!   it falls back to LIKE (text that contains every typed word anywhere, even inside a word).
//! * `purge_audio_older_than(0)` does nothing: 0 means "keep recordings forever" in the privacy
//!   settings, so passing it straight through can never wipe the audio.

use super::db::{has_fts, now_rfc3339, to_i64, to_u32, to_u64};
use super::Db;
use crate::types::{DayStat, HistoryQuery, Stats, Take};
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Days, Local, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone};
use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row, TransactionBehavior};
use std::collections::HashMap;

/// Page size when the query does not ask for one.
const DEFAULT_LIMIT: u32 = 50;
/// More words than this in the search box are ignored.
const MAX_SEARCH_TERMS: usize = 16;
/// Typing at this speed is the baseline for "minutes saved".
const TYPING_WPM: f64 = 40.0;

/// One day's totals while the statistics are put together.
struct DayTotals {
    words: i64,
    speaking_ms: i64,
    timed_words: i64,
}

/// The statistics queries. They only touch the columns of the `takes_by_day` index, so SQLite reads
/// that small index instead of the full rows (which can hold long ramble transcripts). A test checks
/// the query plans, so adding a column here without extending the index is caught.
/// Takes with the status cancelled or error never count toward any statistic.
const TOTALS_SQL: &str =
    "SELECT COALESCE(SUM(words), 0), COUNT(*), COALESCE(SUM(duration_ms), 0) FROM takes WHERE status NOT IN ('cancelled', 'error')";
const PER_DAY_SQL: &str = "SELECT day, COALESCE(SUM(words), 0), COALESCE(SUM(duration_ms), 0),
            COALESCE(SUM(CASE WHEN duration_ms > 0 THEN words ELSE 0 END), 0)
     FROM takes WHERE status NOT IN ('cancelled', 'error') AND day >= ?1 AND day <= ?2 GROUP BY day";
const STREAK_SQL: &str = "SELECT DISTINCT day FROM takes WHERE status NOT IN ('cancelled', 'error') AND day <= ?1 ORDER BY day DESC";

const TAKE_COLS: &str =
    "id, created_at, duration_ms, app_name, window_title, mode_id, raw_text, final_text, words, audio_path, model, latency_ms, status, kind, meta";

impl Db {
    /// Saves a new take. The id must be set; a blank `created_at` means "now". Saving the same id
    /// twice is an error.
    pub fn insert_take(&self, take: &Take) -> Result<()> {
        let id = take.id.trim();
        if id.is_empty() {
            bail!("A take needs an id before it can be saved.");
        }
        let (created_at, ts, day) = when(&take.created_at);
        let meta = serde_json::to_string(&take.meta).context("encoding the take's extra data")?;
        let conn = self.conn.lock();
        with_index_repair(&conn, || {
            conn.execute(
                "INSERT INTO takes (id, created_at, ts, day, duration_ms, app_name, window_title, mode_id, raw_text, final_text,
                                    words, audio_path, model, latency_ms, status, kind, meta)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                params![
                    id,
                    created_at,
                    ts,
                    day,
                    to_i64(take.duration_ms),
                    take.app_name,
                    take.window_title,
                    take.mode_id,
                    take.raw_text,
                    take.final_text,
                    i64::from(take.words),
                    audio_value(take),
                    take.model,
                    to_i64(take.latency_ms),
                    take.status,
                    take.kind,
                    meta,
                ],
            )
        })
        .with_context(|| format!("saving take {id}"))?;
        Ok(())
    }

    /// Rewrites every field of the take with this id (a corrected final text, a changed audio
    /// path). A blank `created_at` keeps the stored one. Errors if there is no such take, for example
    /// because it was deleted meanwhile.
    pub fn update_take(&self, take: &Take) -> Result<()> {
        let id = take.id.trim();
        if id.is_empty() {
            bail!("A take needs an id before it can be updated.");
        }
        // A blank or unusable timestamp leaves the stored one alone (NULL keeps it, see COALESCE).
        let stamp = parse_when(&take.created_at);
        let created_at = stamp.as_ref().map(|w| w.0.as_str());
        let ts = stamp.as_ref().map(|w| w.1);
        let day = stamp.as_ref().map(|w| w.2.as_str());
        let meta = serde_json::to_string(&take.meta).context("encoding the take's extra data")?;
        let conn = self.conn.lock();
        let changed = with_index_repair(&conn, || {
            conn.execute(
                "UPDATE takes SET created_at = COALESCE(?2, created_at), ts = COALESCE(?3, ts), day = COALESCE(?4, day),
                                  duration_ms = ?5, app_name = ?6, window_title = ?7,
                                  mode_id = ?8, raw_text = ?9, final_text = ?10, words = ?11, audio_path = ?12,
                                  model = ?13, latency_ms = ?14, status = ?15, kind = ?16, meta = ?17
                 WHERE id = ?1",
                params![
                    id,
                    created_at,
                    ts,
                    day,
                    to_i64(take.duration_ms),
                    take.app_name,
                    take.window_title,
                    take.mode_id,
                    take.raw_text,
                    take.final_text,
                    i64::from(take.words),
                    audio_value(take),
                    take.model,
                    to_i64(take.latency_ms),
                    take.status,
                    take.kind,
                    meta,
                ],
            )
        })
        .with_context(|| format!("updating take {id}"))?;
        if changed == 0 {
            bail!("Take {id} no longer exists.");
        }
        Ok(())
    }

    /// One take by id, or None.
    pub fn get_take(&self, id: &str) -> Result<Option<Take>> {
        let conn = self.conn.lock();
        Ok(conn
            .query_row(&format!("SELECT {TAKE_COLS} FROM takes WHERE id = ?1"), [id], take_from_row)
            .optional()?)
    }

    /// Takes newest first, narrowed by the search text and the app, mode and kind filters (blank
    /// filters are ignored). `limit` 0 means the default page of 50.
    pub fn list_takes(&self, q: &HistoryQuery) -> Result<Vec<Take>> {
        let conn = self.conn.lock();
        let limit = if q.limit == 0 { DEFAULT_LIMIT } else { q.limit };
        let search = q.search.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let Some(text) = search else {
            return run_list(&conn, q, limit, Match::Nothing);
        };
        if has_fts(&conn) {
            if let Some(fts) = fts_query(text) {
                match run_list(&conn, q, limit, Match::Fts(&fts)) {
                    Ok(rows) if !rows.is_empty() => return Ok(rows),
                    Ok(_) => {
                        // Nothing has a word starting with what was typed. If that is only because
                        // this page is past the end of the matches, we are done; if there are no
                        // matches at all, look for the text inside words instead ("kly" finds
                        // "Quikly", and so does Chinese or Japanese text, which has no spaces).
                        if q.offset > 0 {
                            let first_page = HistoryQuery { offset: 0, ..q.clone() };
                            if !run_list(&conn, &first_page, 1, Match::Fts(&fts))?.is_empty() {
                                return Ok(Vec::new());
                            }
                        }
                    }
                    // A damaged index must not make history unsearchable.
                    Err(e) => log::warn!("full-text search failed, using LIKE instead: {e:#}"),
                }
            }
        }
        run_list(&conn, q, limit, Match::Like(text))
    }

    /// Deletes one take and returns its audio path (if it had one) so the caller can delete the file.
    pub fn delete_take(&self, id: &str) -> Result<Option<String>> {
        let mut conn = self.conn.lock();
        match delete_one(&mut conn, id) {
            Err(e) if is_index_damage(&e) && has_fts(&conn) => {
                log::warn!("the history search index was damaged, rebuilding it: {e}");
                rebuild_index(&conn)?;
                Ok(delete_one(&mut conn, id)?)
            }
            other => Ok(other?),
        }
    }

    /// Deletes every take and returns all audio paths so the caller can delete the files. The
    /// search index and the write-ahead log are cleaned too, so the text really leaves the disk.
    pub fn clear_history(&self) -> Result<Vec<String>> {
        let mut conn = self.conn.lock();
        let paths = match delete_everything(&mut conn) {
            Err(e) if is_index_damage(&e) && has_fts(&conn) => {
                rebuild_index(&conn)?;
                delete_everything(&mut conn)?
            }
            other => other?,
        };
        if has_fts(&conn) {
            // Drops the leftover index segments (the delete triggers only leave tombstones).
            if let Err(e) = rebuild_index(&conn) {
                log::warn!("could not rebuild the search index after clearing history: {e}");
            }
        }
        let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        Ok(paths)
    }

    /// The numbers for the Home page, counted back from today on this machine's clock.
    pub fn stats(&self) -> Result<Stats> {
        self.stats_on(Local::now().date_naive())
    }

    /// `stats` for a given "today". Exposed so the maths can be checked against fixed dates.
    pub fn stats_on(&self, today: NaiveDate) -> Result<Stats> {
        let conn = self.conn.lock();

        let (words_total, takes_total, speaking_ms_total): (i64, i64, i64) =
            conn.query_row(TOTALS_SQL, [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;

        // Per-day totals for the last 30 days feed today, the week, the chart and the average speed.
        // `timed_words` leaves out takes with no recorded length, which would inflate words per minute.
        let window_start = days_before(today, 29);
        let mut per_day: HashMap<String, DayTotals> = HashMap::new();
        {
            let mut stmt = conn.prepare(PER_DAY_SQL)?;
            let rows = stmt.query_map([day_string(window_start), day_string(today)], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    DayTotals { words: row.get(1)?, speaking_ms: row.get(2)?, timed_words: row.get(3)? },
                ))
            })?;
            for row in rows {
                let (day, totals) = row?;
                per_day.insert(day, totals);
            }
        }
        let words_on = |date: NaiveDate| per_day.get(&day_string(date)).map_or(0, |d| d.words).max(0);

        let words_today = words_on(today);
        let words_week: i64 = (0..7).map(|back| words_on(days_before(today, back))).sum();

        let daily: Vec<DayStat> = (0..14)
            .rev()
            .map(|back| {
                let date = days_before(today, back);
                DayStat { date: day_string(date), words: to_u64(words_on(date)) }
            })
            .collect();

        let (timed_words_30, ms_30) = per_day.values().fold((0_i64, 0_i64), |(w, m), d| (w + d.timed_words, m + d.speaking_ms));
        let avg_wpm = if ms_30 > 0 { timed_words_30 as f64 / (ms_30 as f64 / 60_000.0) } else { 0.0 };

        let typing_minutes = words_total as f64 / TYPING_WPM;
        let speaking_minutes = speaking_ms_total as f64 / 60_000.0;
        let minutes_saved = (typing_minutes - speaking_minutes).max(0.0);

        Ok(Stats {
            words_today: to_u64(words_today),
            words_week: to_u64(words_week),
            words_total: to_u64(words_total),
            takes_total: to_u64(takes_total),
            minutes_saved,
            avg_wpm,
            streak_days: streak(&conn, today)?,
            daily,
        })
    }

    /// Distinct app names seen in history, for the filter list. Case-insensitive A to Z.
    pub fn app_names(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT DISTINCT app_name FROM takes WHERE app_name <> '' ORDER BY app_name COLLATE NOCASE")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
    }

    /// Clears `audio_path` on takes older than `days` days and returns those files so the caller
    /// can delete them. The text of the takes is kept. `days == 0` means "keep forever" and does
    /// nothing.
    pub fn purge_audio_older_than(&self, days: u32) -> Result<Vec<String>> {
        if days == 0 {
            return Ok(Vec::new());
        }
        let cutoff = Local::now().timestamp_millis().saturating_sub(i64::from(days).saturating_mul(86_400_000));
        self.purge_audio_before(cutoff)
    }

    /// `purge_audio_older_than` with an explicit cutoff (UTC milliseconds).
    pub fn purge_audio_before(&self, cutoff_ms: i64) -> Result<Vec<String>> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let paths = {
            let mut stmt =
                tx.prepare("SELECT audio_path FROM takes WHERE audio_path IS NOT NULL AND audio_path <> '' AND ts < ?1")?;
            let rows = stmt.query_map([cutoff_ms], |row| row.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<String>>>()?
        };
        tx.execute("UPDATE takes SET audio_path = NULL WHERE audio_path IS NOT NULL AND ts < ?1", [cutoff_ms])?;
        tx.commit()?;
        Ok(paths)
    }
}

// ---------------------------------------------------------------------------------------------
// Keeping the search index healthy
// ---------------------------------------------------------------------------------------------

/// SQLite's "database disk image is malformed". For a write to `takes` it nearly always means the
/// full-text index has drifted from its own bookkeeping, which is repairable.
fn is_index_damage(error: &rusqlite::Error) -> bool {
    error.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseCorrupt)
}

/// Rebuilds the search index from the takes table (where the text lives, so nothing is lost).
fn rebuild_index(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("INSERT INTO takes_fts (takes_fts) VALUES ('rebuild');")
}

/// Runs a write on `takes`. If it fails because the search index is damaged, rebuilds the index and
/// tries once more, so a bad index can never stop a dictation from being saved.
fn with_index_repair<T>(conn: &Connection, mut write: impl FnMut() -> rusqlite::Result<T>) -> rusqlite::Result<T> {
    match write() {
        Err(e) if is_index_damage(&e) && has_fts(conn) => {
            log::warn!("the history search index was damaged, rebuilding it: {e}");
            rebuild_index(conn)?;
            write()
        }
        other => other,
    }
}

/// Deletes one take in a transaction and returns its audio path. The commit is explicit so that a
/// failure at that point is reported rather than lost (a lone `DELETE ... RETURNING` would hide it).
fn delete_one(conn: &mut Connection, id: &str) -> rusqlite::Result<Option<String>> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let audio: Option<Option<String>> =
        tx.query_row("SELECT audio_path FROM takes WHERE id = ?1", [id], |row| row.get(0)).optional()?;
    tx.execute("DELETE FROM takes WHERE id = ?1", [id])?;
    tx.commit()?;
    Ok(audio.flatten().filter(|p| !p.is_empty()))
}

/// Deletes every take in one transaction and returns the audio files that belonged to them.
fn delete_everything(conn: &mut Connection) -> rusqlite::Result<Vec<String>> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let paths = {
        let mut stmt = tx.prepare("SELECT audio_path FROM takes WHERE audio_path IS NOT NULL AND audio_path <> ''")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<String>>>()?
    };
    tx.execute("DELETE FROM takes", [])?;
    tx.commit()?;
    Ok(paths)
}

// ---------------------------------------------------------------------------------------------
// Listing and searching
// ---------------------------------------------------------------------------------------------

enum Match<'a> {
    Nothing,
    /// A ready-made FTS5 query (see `fts_query`).
    Fts(&'a str),
    /// The raw text typed in the search box.
    Like(&'a str),
}

/// Query parameters collected while the SQL is built, numbered ?1, ?2, ...
#[derive(Default)]
struct Args {
    values: Vec<Value>,
}

impl Args {
    fn add(&mut self, value: Value) -> String {
        self.values.push(value);
        format!("?{}", self.values.len())
    }

    fn text(&mut self, value: &str) -> String {
        self.add(Value::Text(value.to_string()))
    }
}

fn filled(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

fn run_list(conn: &Connection, q: &HistoryQuery, limit: u32, search: Match<'_>) -> Result<Vec<Take>> {
    let mut args = Args::default();
    let mut clauses: Vec<String> = Vec::new();

    match search {
        Match::Nothing => {}
        Match::Fts(query) => {
            let p = args.text(query);
            clauses.push(format!("seq IN (SELECT rowid FROM takes_fts WHERE takes_fts MATCH {p})"));
        }
        Match::Like(text) => {
            for term in text.split_whitespace().take(MAX_SEARCH_TERMS) {
                let p = args.text(&like_pattern(term));
                clauses.push(format!(
                    "(raw_text LIKE {p} ESCAPE '\\' OR final_text LIKE {p} ESCAPE '\\' OR app_name LIKE {p} ESCAPE '\\' OR window_title LIKE {p} ESCAPE '\\')"
                ));
            }
        }
    }
    if let Some(app) = filled(&q.app) {
        let p = args.text(app);
        clauses.push(format!("app_name = {p}"));
    }
    if let Some(mode) = filled(&q.mode) {
        let p = args.text(mode);
        clauses.push(format!("mode_id = {p}"));
    }
    if let Some(kind) = filled(&q.kind) {
        let p = args.text(kind);
        clauses.push(format!("kind = {p}"));
    }

    let mut sql = format!("SELECT {TAKE_COLS} FROM takes");
    if !clauses.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&clauses.join(" AND "));
    }
    let limit_p = args.add(Value::Integer(i64::from(limit)));
    let offset_p = args.add(Value::Integer(i64::from(q.offset)));
    sql.push_str(&format!(" ORDER BY ts DESC, seq DESC LIMIT {limit_p} OFFSET {offset_p}"));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(args.values.iter()), take_from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<Take>>>()?)
}

/// Turns what was typed into an FTS5 query: every word becomes a quoted prefix term, so "inv ali"
/// matches takes that contain a word starting "inv" and a word starting "ali". Quoting keeps
/// FTS5 syntax characters ( " * - : AND OR NEAR ) from being interpreted. Returns None when a word
/// has no letters or digits (punctuation cannot be indexed), which sends the search to LIKE.
fn fts_query(search: &str) -> Option<String> {
    let mut parts = Vec::new();
    for token in search.split_whitespace().take(MAX_SEARCH_TERMS) {
        if !token.chars().any(char::is_alphanumeric) {
            return None;
        }
        parts.push(format!("\"{}\"*", token.replace('"', "\"\"")));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

/// `%word%` with LIKE's own wildcards escaped (the query uses ESCAPE '\').
fn like_pattern(term: &str) -> String {
    let mut out = String::with_capacity(term.len() + 2);
    out.push('%');
    for c in term.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

fn take_from_row(row: &Row<'_>) -> rusqlite::Result<Take> {
    let meta_text: String = row.get(14)?;
    Ok(Take {
        id: row.get(0)?,
        created_at: row.get(1)?,
        duration_ms: to_u64(row.get(2)?),
        app_name: row.get(3)?,
        window_title: row.get(4)?,
        mode_id: row.get(5)?,
        raw_text: row.get(6)?,
        final_text: row.get(7)?,
        words: to_u32(row.get(8)?),
        audio_path: row.get(9)?,
        model: row.get(10)?,
        latency_ms: to_u64(row.get(11)?),
        status: row.get(12)?,
        kind: row.get(13)?,
        // A damaged value must not stop the whole list from loading.
        meta: serde_json::from_str(&meta_text).unwrap_or(serde_json::Value::Null),
    })
}

fn audio_value(take: &Take) -> Option<&str> {
    take.audio_path.as_deref().filter(|p| !p.is_empty())
}

// ---------------------------------------------------------------------------------------------
// Dates and streaks
// ---------------------------------------------------------------------------------------------

fn day_string(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

fn days_before(date: NaiveDate, days: u64) -> NaiveDate {
    date.checked_sub_days(Days::new(days)).unwrap_or(NaiveDate::MIN)
}

/// A timestamp ready to store: (created_at text, UTC milliseconds, "YYYY-MM-DD" on the take's own clock).
type When = (String, i64, String);

/// Reads a take's `created_at`. A proper RFC 3339 stamp is kept exactly as written. A local time with
/// no offset is accepted and given one. Anything else is None, because it would corrupt ordering and
/// the statistics.
fn parse_when(created_at: &str) -> Option<When> {
    let raw = created_at.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Some((raw.to_string(), dt.timestamp_millis(), day_string(dt.date_naive())));
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(raw, format) {
            if let Some(local) = Local.from_local_datetime(&naive).earliest() {
                return Some((
                    local.to_rfc3339_opts(SecondsFormat::Secs, false),
                    local.timestamp_millis(),
                    day_string(local.date_naive()),
                ));
            }
        }
    }
    None
}

/// For a new take: its own timestamp if usable, otherwise now, so a take is filed rather than lost.
fn when(created_at: &str) -> When {
    parse_when(created_at).unwrap_or_else(|| {
        if !created_at.trim().is_empty() {
            log::warn!("a take had a timestamp that is not RFC 3339, so it was filed at the current time");
        }
        let now = Local::now();
        (now_rfc3339(), now.timestamp_millis(), day_string(now.date_naive()))
    })
}

/// Consecutive days with at least one counted take, ending today or, if there is nothing yet
/// today, yesterday.
fn streak(conn: &Connection, today: NaiveDate) -> Result<u32> {
    let yesterday = days_before(today, 1);
    let mut stmt = conn.prepare(STREAK_SQL)?;
    let mut rows = stmt.query([day_string(today)])?;
    let mut expected: Option<NaiveDate> = None;
    let mut count = 0_u32;
    while let Some(row) = rows.next()? {
        let text: String = row.get(0)?;
        let Ok(day) = NaiveDate::parse_from_str(&text, "%Y-%m-%d") else { continue };
        match expected {
            None => {
                if day != today && day != yesterday {
                    break;
                }
            }
            Some(next) => {
                if day != next {
                    break;
                }
            }
        }
        count += 1;
        expected = Some(days_before(day, 1));
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testutil::take;
    use serde_json::json;

    fn db() -> Db {
        Db::open_memory().unwrap()
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn query() -> HistoryQuery {
        HistoryQuery::default()
    }

    fn search(text: &str) -> HistoryQuery {
        HistoryQuery { search: Some(text.to_string()), ..HistoryQuery::default() }
    }

    fn ids(takes: &[Take]) -> Vec<&str> {
        takes.iter().map(|t| t.id.as_str()).collect()
    }

    /// Asks FTS5 to compare its index with the takes table; errors if they have drifted apart.
    fn assert_index_in_step(db: &Db) {
        db.conn
            .lock()
            .execute("INSERT INTO takes_fts (takes_fts, rank) VALUES ('integrity-check', 1)", [])
            .expect("the search index should match the takes table");
    }

    fn add_at(db: &Db, id: &str, when: &str, words: u32, duration_ms: u64) {
        db.insert_take(&Take { duration_ms, ..take(id, when, words) }).unwrap();
    }

    // ---- storing and fetching ----

    #[test]
    fn a_take_round_trips_every_field() {
        let db = db();
        let original = Take {
            id: "t1".into(),
            created_at: "2026-10-14T09:15:30+01:00".into(),
            duration_ms: 4_321,
            app_name: "Outlook".into(),
            window_title: "Re: Budget".into(),
            mode_id: "email".into(),
            raw_text: "um send the budget".into(),
            final_text: "Send the budget.".into(),
            words: 3,
            audio_path: Some("C:\\audio\\t1.wav".into()),
            model: "large-v3-turbo".into(),
            latency_ms: 812,
            status: "pasted".into(),
            kind: "talk".into(),
            meta: json!({"warnings": ["clipped"], "notes": {"tasks": 2}}),
        };
        db.insert_take(&original).unwrap();
        let back = db.get_take("t1").unwrap().expect("take should exist");
        assert_eq!(back.id, original.id);
        assert_eq!(back.created_at, original.created_at);
        assert_eq!(back.duration_ms, 4_321);
        assert_eq!(back.app_name, "Outlook");
        assert_eq!(back.window_title, "Re: Budget");
        assert_eq!(back.mode_id, "email");
        assert_eq!(back.raw_text, "um send the budget");
        assert_eq!(back.final_text, "Send the budget.");
        assert_eq!(back.words, 3);
        assert_eq!(back.audio_path.as_deref(), Some("C:\\audio\\t1.wav"));
        assert_eq!(back.model, "large-v3-turbo");
        assert_eq!(back.latency_ms, 812);
        assert_eq!(back.status, "pasted");
        assert_eq!(back.kind, "talk");
        assert_eq!(back.meta, original.meta);
    }

    #[test]
    fn missing_audio_and_null_meta_come_back_as_none_and_null() {
        let db = db();
        db.insert_take(&take("t1", "2026-10-14T09:00:00+01:00", 1)).unwrap();
        db.insert_take(&Take { audio_path: Some(String::new()), ..take("t2", "2026-10-14T09:01:00+01:00", 1) }).unwrap();
        for id in ["t1", "t2"] {
            let back = db.get_take(id).unwrap().unwrap();
            assert_eq!(back.audio_path, None, "{id}: no audio should be None");
            assert!(back.meta.is_null());
        }
        assert!(db.get_take("nope").unwrap().is_none());
    }

    #[test]
    fn inserting_without_an_id_or_with_a_duplicate_id_is_an_error() {
        let db = db();
        assert!(db.insert_take(&take("  ", "2026-10-14T09:00:00+01:00", 1)).is_err());
        db.insert_take(&take("dup", "2026-10-14T09:00:00+01:00", 1)).unwrap();
        let err = db.insert_take(&take("dup", "2026-10-14T09:00:00+01:00", 1)).unwrap_err();
        assert!(format!("{err:#}").contains("dup"));
    }

    #[test]
    fn huge_numbers_are_clamped_not_wrapped() {
        let db = db();
        db.insert_take(&Take { duration_ms: u64::MAX, latency_ms: u64::MAX, ..take("big", "2026-10-14T09:00:00+01:00", 1) })
            .unwrap();
        let back = db.get_take("big").unwrap().unwrap();
        assert_eq!(back.duration_ms, i64::MAX as u64);
    }

    #[test]
    fn a_bad_timestamp_is_replaced_not_stored() {
        let db = db();
        db.insert_take(&take("bad", "yesterday-ish", 2)).unwrap();
        db.insert_take(&take("empty", "", 2)).unwrap();
        for id in ["bad", "empty"] {
            let back = db.get_take(id).unwrap().unwrap();
            assert!(DateTime::parse_from_rfc3339(&back.created_at).is_ok(), "{id}: {}", back.created_at);
        }
    }

    #[test]
    fn a_local_time_without_an_offset_is_accepted() {
        let db = db();
        db.insert_take(&take("naive", "2026-10-14T09:00:00", 2)).unwrap();
        db.insert_take(&take("spaced", "2026-10-14 09:00:00.250", 2)).unwrap();
        for id in ["naive", "spaced"] {
            let back = db.get_take(id).unwrap().unwrap();
            let parsed = DateTime::parse_from_rfc3339(&back.created_at).unwrap();
            assert_eq!(parsed.date_naive(), date(2026, 10, 14), "{id}");
        }
    }

    #[test]
    fn updating_a_take_changes_the_row_and_the_search_index() {
        let db = db();
        db.insert_take(&Take { raw_text: "alpha bravo".into(), final_text: "Alpha bravo.".into(), ..take("t1", "2026-10-14T09:00:00+01:00", 2) })
            .unwrap();
        assert_eq!(db.list_takes(&search("bravo")).unwrap().len(), 1);

        let mut edited = db.get_take("t1").unwrap().unwrap();
        edited.final_text = "Alpha charlie.".into();
        edited.raw_text = "alpha charlie".into();
        edited.words = 5;
        edited.audio_path = Some("a.wav".into());
        edited.status = "typed".into();
        db.update_take(&edited).unwrap();

        let back = db.get_take("t1").unwrap().unwrap();
        assert_eq!(back.final_text, "Alpha charlie.");
        assert_eq!(back.words, 5);
        assert_eq!(back.audio_path.as_deref(), Some("a.wav"));
        assert_eq!(back.status, "typed");
        assert!(db.list_takes(&search("bravo")).unwrap().is_empty(), "old text must leave the index");
        assert_eq!(ids(&db.list_takes(&search("charlie")).unwrap()), ["t1"]);
        assert_index_in_step(&db);
    }

    #[test]
    fn updating_a_take_that_was_deleted_is_an_error() {
        let db = db();
        let err = db.update_take(&take("ghost", "2026-10-14T09:00:00+01:00", 1)).unwrap_err();
        assert!(format!("{err:#}").contains("ghost"));
        assert!(db.update_take(&take("  ", "2026-10-14T09:00:00+01:00", 1)).is_err());
    }

    #[test]
    fn updating_with_a_blank_or_unusable_timestamp_keeps_the_stored_one() {
        let db = db();
        db.insert_take(&take("t", "2026-10-14T09:00:00+01:00", 2)).unwrap();
        let mut edited = db.get_take("t").unwrap().unwrap();
        edited.final_text = "changed".into();
        for blank in ["", "   ", "not a date"] {
            edited.created_at = blank.to_string();
            db.update_take(&edited).unwrap();
            let back = db.get_take("t").unwrap().unwrap();
            assert_eq!(back.created_at, "2026-10-14T09:00:00+01:00", "{blank:?}");
            assert_eq!(back.final_text, "changed");
            assert_eq!(db.stats_on(date(2026, 10, 14)).unwrap().words_today, 2, "the day is unchanged too");
        }
        // A usable new timestamp does move the take, and the statistics follow.
        edited.created_at = "2026-10-12T08:00:00+01:00".into();
        db.update_take(&edited).unwrap();
        let stats = db.stats_on(date(2026, 10, 14)).unwrap();
        assert_eq!((stats.words_today, stats.words_week), (0, 2));
        assert_eq!(db.get_take("t").unwrap().unwrap().created_at, "2026-10-12T08:00:00+01:00");
    }

    // ---- listing ----

    #[test]
    fn lists_newest_first_by_real_time_not_by_text() {
        let db = db();
        // The clocks go back at 02:00 BST on 25 Oct 2026. 01:30 BST happens an hour BEFORE 01:10 GMT,
        // even though "01:30" sorts after "01:10" as text.
        db.insert_take(&take("earlier", "2026-10-25T01:30:00+01:00", 1)).unwrap();
        db.insert_take(&take("later", "2026-10-25T01:10:00+00:00", 1)).unwrap();
        db.insert_take(&take("oldest", "2026-10-24T23:00:00+01:00", 1)).unwrap();
        assert_eq!(ids(&db.list_takes(&query()).unwrap()), ["later", "earlier", "oldest"]);
    }

    #[test]
    fn takes_with_the_same_stamp_list_in_reverse_insertion_order() {
        let db = db();
        for id in ["a", "b", "c"] {
            db.insert_take(&take(id, "2026-10-14T09:00:00+01:00", 1)).unwrap();
        }
        assert_eq!(ids(&db.list_takes(&query()).unwrap()), ["c", "b", "a"]);
    }

    #[test]
    fn paging_uses_limit_offset_and_a_default_of_fifty() {
        let db = db();
        for i in 0..120 {
            let minutes = i % 60;
            let hours = i / 60;
            db.insert_take(&take(&format!("t{i:03}"), &format!("2026-10-14T{:02}:{minutes:02}:00+01:00", 8 + hours), 1)).unwrap();
        }
        let first = db.list_takes(&query()).unwrap();
        assert_eq!(first.len(), 50, "default page size");
        assert_eq!(first[0].id, "t119");
        assert_eq!(first[49].id, "t070");

        let second = db.list_takes(&HistoryQuery { offset: 50, ..query() }).unwrap();
        assert_eq!(second.len(), 50);
        assert_eq!(second[0].id, "t069");

        let tail = db.list_takes(&HistoryQuery { limit: 10, offset: 115, ..query() }).unwrap();
        assert_eq!(ids(&tail), ["t004", "t003", "t002", "t001", "t000"]);

        let everything = db.list_takes(&HistoryQuery { limit: 1000, ..query() }).unwrap();
        assert_eq!(everything.len(), 120);
        assert!(db.list_takes(&HistoryQuery { offset: 500, ..query() }).unwrap().is_empty());
    }

    #[test]
    fn filters_combine_and_blank_filters_are_ignored() {
        let db = db();
        db.insert_take(&Take { app_name: "Outlook".into(), mode_id: "email".into(), kind: "talk".into(), ..take("a", "2026-10-14T09:00:00+01:00", 1) }).unwrap();
        db.insert_take(&Take { app_name: "Outlook".into(), mode_id: "clean".into(), kind: "ramble".into(), ..take("b", "2026-10-14T09:01:00+01:00", 1) }).unwrap();
        db.insert_take(&Take { app_name: "Slack".into(), mode_id: "email".into(), kind: "talk".into(), ..take("c", "2026-10-14T09:02:00+01:00", 1) }).unwrap();

        let by = |app: Option<&str>, mode: Option<&str>, kind: Option<&str>| HistoryQuery {
            app: app.map(Into::into),
            mode: mode.map(Into::into),
            kind: kind.map(Into::into),
            ..query()
        };
        assert_eq!(ids(&db.list_takes(&by(Some("Outlook"), None, None)).unwrap()), ["b", "a"]);
        assert_eq!(ids(&db.list_takes(&by(Some("outlook"), None, None)).unwrap()), ["b", "a"], "app match ignores case");
        assert_eq!(ids(&db.list_takes(&by(None, Some("email"), None)).unwrap()), ["c", "a"]);
        assert_eq!(ids(&db.list_takes(&by(None, None, Some("ramble"))).unwrap()), ["b"]);
        assert_eq!(ids(&db.list_takes(&by(Some("Outlook"), Some("email"), Some("talk"))).unwrap()), ["a"]);
        assert!(db.list_takes(&by(Some("Slack"), Some("clean"), None)).unwrap().is_empty());
        assert_eq!(db.list_takes(&by(Some(""), Some("  "), Some(""))).unwrap().len(), 3, "blank filters mean no filter");
    }

    // ---- search ----

    fn search_fixture() -> Db {
        let db = db();
        let rows = [
            ("a", "send the invoice to Alice", "Send the invoice to Alice.", "Outlook", "Re: Invoice 42"),
            ("b", "meeting notes for tomorrow", "Meeting notes for tomorrow.", "OneNote", "Notes"),
            ("c", "open the quickly settings page", "Open the Quikly settings page.", "chrome.exe", "Settings - Navi"),
            ("d", "what time is it", "What time is it?", "Slack", "general"),
            ("e", "a lovely cafe near the station", "A lovely café near the station.", "Notepad", "plans.txt"),
        ];
        for (i, (id, raw, fin, app, title)) in rows.iter().enumerate() {
            db.insert_take(&Take {
                raw_text: raw.to_string(),
                final_text: fin.to_string(),
                app_name: app.to_string(),
                window_title: title.to_string(),
                ..take(id, &format!("2026-10-14T09:0{i}:00+01:00"), 5)
            })
            .unwrap();
        }
        db
    }

    fn found(db: &Db, text: &str) -> Vec<String> {
        db.list_takes(&search(text)).unwrap().into_iter().map(|t| t.id).collect()
    }

    #[test]
    fn search_matches_raw_text_final_text_app_and_title() {
        let db = search_fixture();
        assert_eq!(found(&db, "invoice"), ["a"], "raw/final text and title");
        assert_eq!(found(&db, "outlook"), ["a"], "app name");
        assert_eq!(found(&db, "onenote"), ["b"]);
        assert_eq!(found(&db, "settings"), ["c"], "title and text");
        assert_eq!(found(&db, "general"), ["d"], "title only");
        assert_eq!(found(&db, "chrome"), ["c"], "app name with an extension");
        assert_eq!(found(&db, "chrome.exe"), ["c"]);
        assert_eq!(found(&db, "plans.txt"), ["e"]);
    }

    #[test]
    fn search_finds_words_by_their_start_and_ignores_case() {
        let db = search_fixture();
        assert_eq!(found(&db, "inv"), ["a"]);
        assert_eq!(found(&db, "ALI"), ["a"]);
        assert_eq!(found(&db, "  Tomor  "), ["b"]);
        assert_eq!(found(&db, "qui"), ["c"], "one take, not one hit per matching column");
    }

    #[test]
    fn search_needs_every_word_and_accepts_them_in_any_order() {
        let db = search_fixture();
        assert_eq!(found(&db, "meeting tomorrow"), ["b"]);
        assert_eq!(found(&db, "tomorrow meeting"), ["b"]);
        assert_eq!(found(&db, "invoice settings"), Vec::<String>::new());
        assert_eq!(found(&db, "invoice outlook"), ["a"], "words can come from different columns");
    }

    #[test]
    fn search_sees_the_corrected_text_as_well_as_the_raw_text() {
        let db = search_fixture();
        // The engine heard "quickly"; the dictionary turned it into "Quikly".
        assert_eq!(found(&db, "quikly"), ["c"]);
        assert_eq!(found(&db, "quickly"), ["c"]);
    }

    #[test]
    fn when_no_word_starts_with_the_text_it_is_looked_for_inside_words() {
        let db = search_fixture();
        assert_eq!(found(&db, "kly"), ["c"], "inside 'quickly' and 'Quikly'");
        assert_eq!(found(&db, "oice"), ["a"], "inside 'invoice'");
        assert_eq!(found(&db, "oice utlo"), ["a"], "every typed word still has to match: 'invoice' and 'Outlook'");
        assert_eq!(found(&db, "oice zzz"), Vec::<String>::new());
        assert_eq!(found(&db, "nvoice"), ["a"], "inside 'invoice'");
        assert_eq!(found(&db, "voise"), Vec::<String>::new(), "a typo is still a miss");

        // Paging a fallback search keeps working; a page past the end is simply empty.
        let q = |offset| HistoryQuery { search: Some("oice".into()), offset, limit: 1, ..query() };
        assert_eq!(ids(&db.list_takes(&q(0)).unwrap()), ["a"]);
        assert!(db.list_takes(&q(1)).unwrap().is_empty());
    }

    #[test]
    fn a_page_past_the_end_of_indexed_matches_does_not_fall_back() {
        let db = db();
        for i in 0..3 {
            db.insert_take(&Take {
                raw_text: format!("budget {i}"),
                final_text: format!("Budget {i}"),
                ..take(&format!("b{i}"), &format!("2026-10-14T09:0{i}:00+01:00"), 2)
            })
            .unwrap();
        }
        db.insert_take(&Take { raw_text: "overbudgeted".into(), final_text: "Overbudgeted".into(), ..take("inside", "2026-10-14T10:00:00+01:00", 1) })
            .unwrap();
        let page = |offset| HistoryQuery { search: Some("budget".into()), limit: 2, offset, ..query() };
        assert_eq!(ids(&db.list_takes(&page(0)).unwrap()), ["b2", "b1"], "word-start matches only, newest first");
        assert_eq!(ids(&db.list_takes(&page(2)).unwrap()), ["b0"]);
        assert!(db.list_takes(&page(4)).unwrap().is_empty(), "the 'overbudgeted' take must not appear on a later page");
    }

    #[test]
    fn text_without_spaces_can_be_searched_from_the_middle() {
        let db = db();
        db.insert_take(&Take { raw_text: "你好世界".into(), final_text: "你好世界。".into(), ..take("zh", "2026-10-14T09:00:00+01:00", 1) })
            .unwrap();
        assert_eq!(found(&db, "你好"), ["zh"]);
        assert_eq!(found(&db, "好世"), ["zh"]);
        assert_eq!(found(&db, "再见"), Vec::<String>::new());
    }

    #[test]
    fn search_ignores_accents() {
        let db = search_fixture();
        assert_eq!(found(&db, "cafe"), ["e"]);
        assert_eq!(found(&db, "café"), ["e"]);
    }

    #[test]
    fn search_survives_characters_that_mean_something_to_fts5() {
        let db = search_fixture();
        for nasty in [
            "\"", "\"\"", "\"invoice", "invoice\"", "*", "invoice*", "-invoice", "AND", "OR", "NOT", "NEAR(", "NEAR(a b)", "raw_text:invoice", "(", ")", "^invoice", "a OR b", "{x}", "'", "\\", "%", "_",
        ] {
            db.list_takes(&search(nasty)).unwrap_or_else(|e| panic!("search for {nasty:?} failed: {e:#}"));
        }
        assert_eq!(found(&db, "\"invoice\""), ["a"], "quotes around a word are harmless");
        assert_eq!(found(&db, "-invoice"), ["a"], "a leading minus is just punctuation, not 'NOT'");
        assert_eq!(found(&db, "AND invoice"), Vec::<String>::new(), "AND is a plain word here");
    }

    #[test]
    fn punctuation_only_search_falls_back_to_like() {
        let db = search_fixture();
        assert_eq!(found(&db, "?"), ["d"], "the question mark in 'What time is it?'");
        assert_eq!(found(&db, " - "), ["c"], "the dash in a window title");
        assert_eq!(found(&db, "- navi"), ["c"], "one punctuation word sends every word to LIKE");
        assert_eq!(found(&db, "- zzz"), Vec::<String>::new());
    }

    #[test]
    fn like_wildcards_typed_by_the_user_are_literal() {
        let db = db();
        db.insert_take(&Take { raw_text: "100% sure".into(), final_text: "100% sure".into(), ..take("pct", "2026-10-14T09:00:00+01:00", 2) }).unwrap();
        db.insert_take(&Take { raw_text: "a_b".into(), final_text: "a_b".into(), ..take("under", "2026-10-14T09:01:00+01:00", 1) }).unwrap();
        db.insert_take(&Take { raw_text: "axb".into(), final_text: "axb".into(), ..take("x", "2026-10-14T09:02:00+01:00", 1) }).unwrap();
        assert_eq!(found(&db, "%"), ["pct"]);
        assert_eq!(found(&db, "_"), ["under"], "an underscore must not match any single character");
        assert_eq!(found(&db, "100%"), ["pct"]);
    }

    #[test]
    fn like_fallback_works_when_there_is_no_index() {
        let db = search_fixture();
        db.conn
            .lock()
            .execute_batch("DROP TRIGGER takes_fts_ai; DROP TRIGGER takes_fts_ad; DROP TRIGGER takes_fts_au; DROP TABLE takes_fts;")
            .unwrap();
        assert!(!db.fts_enabled());
        assert_eq!(found(&db, "invoice"), ["a"]);
        assert_eq!(found(&db, "vo"), ["a"], "LIKE matches inside words too");
        assert_eq!(found(&db, "meeting tomorrow"), ["b"]);
        assert_eq!(found(&db, "chrome"), ["c"]);
        assert_eq!(found(&db, "INVOICE"), ["a"], "case-insensitive");
        // New takes still save with the triggers gone.
        db.insert_take(&take("after", "2026-10-14T10:00:00+01:00", 1)).unwrap();
        assert_eq!(found(&db, "final after"), ["after"]);
    }

    #[test]
    fn search_combines_with_filters_and_paging() {
        let db = db();
        for i in 0..6 {
            let app = if i % 2 == 0 { "Outlook" } else { "Slack" };
            db.insert_take(&Take { app_name: app.into(), raw_text: format!("budget item {i}"), final_text: format!("Budget item {i}"), ..take(&format!("t{i}"), &format!("2026-10-14T09:0{i}:00+01:00"), 3) }).unwrap();
        }
        let q = HistoryQuery { search: Some("budget".into()), app: Some("Outlook".into()), ..query() };
        assert_eq!(ids(&db.list_takes(&q).unwrap()), ["t4", "t2", "t0"]);
        let paged = HistoryQuery { limit: 1, offset: 1, ..q };
        assert_eq!(ids(&db.list_takes(&paged).unwrap()), ["t2"]);
    }

    #[test]
    fn the_index_stays_in_step_through_inserts_updates_deletes_and_purges() {
        let db = search_fixture();
        assert_index_in_step(&db);
        db.delete_take("a").unwrap();
        assert_index_in_step(&db);
        let mut c = db.get_take("c").unwrap().unwrap();
        c.window_title = "Preferences".into();
        db.update_take(&c).unwrap();
        assert_index_in_step(&db);
        assert_eq!(found(&db, "preferences"), ["c"]);
        db.purge_audio_before(i64::MAX).unwrap();
        assert_index_in_step(&db);
        db.clear_history().unwrap();
        assert_index_in_step(&db);
    }

    /// Small deterministic random numbers, so a failure can be reproduced.
    struct Dice(u64);

    impl Dice {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n as u64) as usize
        }
    }

    #[test]
    fn the_search_index_agrees_with_a_simple_model_after_many_random_changes() {
        const VOCAB: [&str; 4] = ["alpha", "bravo", "charlie", "delta"];
        let db = db();
        let mut dice = Dice(0x2545_F491_4F6C_DD1D);
        let random_text = |dice: &mut Dice| -> String {
            let count = 1 + dice.below(3);
            (0..count).map(|_| VOCAB[dice.below(VOCAB.len())]).collect::<Vec<_>>().join(" ")
        };
        let mut model: Vec<(String, String)> = Vec::new(); // (take id, its text)
        let mut created = 0;
        for step in 0..400_usize {
            match dice.below(4) {
                0 | 1 => {
                    created += 1;
                    let id = format!("t{created}");
                    let text = random_text(&mut dice);
                    let stamp = format!("2026-10-14T09:{:02}:{:02}+01:00", step / 60 % 60, step % 60);
                    db.insert_take(&Take { raw_text: text.clone(), final_text: text.clone(), ..take(&id, &stamp, 1) }).unwrap();
                    model.push((id, text));
                }
                2 if !model.is_empty() => {
                    let at = dice.below(model.len());
                    let text = random_text(&mut dice);
                    let mut edited = db.get_take(&model[at].0).unwrap().unwrap();
                    edited.raw_text = text.clone();
                    edited.final_text = text.clone();
                    db.update_take(&edited).unwrap();
                    model[at].1 = text;
                }
                3 if !model.is_empty() => {
                    let at = dice.below(model.len());
                    db.delete_take(&model[at].0).unwrap();
                    model.remove(at);
                }
                _ => {}
            }
            if step % 50 == 0 {
                assert_index_in_step(&db);
            }
        }
        assert_index_in_step(&db);
        assert!(model.len() > 20, "the run should leave a decent number of takes ({})", model.len());
        for word in VOCAB {
            let mut expected: Vec<&str> =
                model.iter().filter(|(_, text)| text.split(' ').any(|w| w == word)).map(|(id, _)| id.as_str()).collect();
            expected.sort_unstable();
            let hits = db.list_takes(&HistoryQuery { search: Some(word.into()), limit: 10_000, ..query() }).unwrap();
            let mut actual: Vec<&str> = hits.iter().map(|t| t.id.as_str()).collect();
            actual.sort_unstable();
            assert_eq!(actual, expected, "searching for {word}");
        }
    }

    /// Garbles the index's own bookkeeping, the way an interrupted write or a bad edit might.
    fn damage_index(db: &Db) {
        db.conn.lock().execute("UPDATE takes_fts_data SET block = x'0102030405' WHERE id = 10", []).unwrap();
    }

    #[test]
    fn a_damaged_search_index_never_stops_a_take_being_saved() {
        let db = search_fixture();
        damage_index(&db);
        db.insert_take(&take("new", "2026-10-14T10:00:00+01:00", 2)).expect("saving must repair the index and carry on");
        assert_eq!(found(&db, "final new"), ["new"]);
        assert_eq!(found(&db, "invoice"), ["a"], "old takes are searchable again after the rebuild");
        assert_eq!(db.list_takes(&query()).unwrap().len(), 6);
        assert_index_in_step(&db);
    }

    #[test]
    fn a_damaged_search_index_does_not_block_edits_deletes_or_clearing() {
        let db = search_fixture();
        damage_index(&db);
        let mut c = db.get_take("c").unwrap().unwrap();
        c.final_text = "Edited after damage".into();
        db.update_take(&c).expect("update repairs the index");
        assert_eq!(found(&db, "damage"), ["c"]);
        assert_index_in_step(&db);

        damage_index(&db);
        db.insert_take(&Take { audio_path: Some("x.wav".into()), ..take("gone", "2026-10-14T11:00:00+01:00", 1) }).unwrap();
        damage_index(&db);
        assert_eq!(db.delete_take("gone").expect("delete repairs the index").as_deref(), Some("x.wav"));
        assert_index_in_step(&db);

        damage_index(&db);
        db.clear_history().expect("clearing repairs the index");
        assert!(db.list_takes(&query()).unwrap().is_empty());
        assert_index_in_step(&db);
    }

    #[test]
    fn searching_a_damaged_index_falls_back_to_like() {
        let db = search_fixture();
        damage_index(&db);
        assert_eq!(found(&db, "invoice"), ["a"]);
        assert_eq!(found(&db, "meeting tomorrow"), ["b"]);
    }

    // ---- delete and clear ----

    #[test]
    fn deleting_a_take_returns_its_audio_and_removes_it_from_search() {
        let db = db();
        db.insert_take(&Take { audio_path: Some("C:\\a\\one.wav".into()), ..take("one", "2026-10-14T09:00:00+01:00", 1) }).unwrap();
        db.insert_take(&take("two", "2026-10-14T09:01:00+01:00", 1)).unwrap();

        assert_eq!(db.delete_take("one").unwrap().as_deref(), Some("C:\\a\\one.wav"));
        assert!(db.get_take("one").unwrap().is_none());
        assert!(found(&db, "one").is_empty(), "the deleted take must leave the search index");
        assert_eq!(found(&db, "two"), ["two"]);
        assert_eq!(db.delete_take("two").unwrap(), None, "a take without audio gives None");
        assert_eq!(db.delete_take("two").unwrap(), None, "deleting twice is harmless");
        assert_eq!(db.delete_take("never-existed").unwrap(), None);
        assert!(db.list_takes(&query()).unwrap().is_empty());
    }

    #[test]
    fn clearing_history_returns_every_audio_path_and_empties_everything() {
        let db = db();
        db.insert_take(&Take { audio_path: Some("a.wav".into()), ..take("a", "2026-10-14T09:00:00+01:00", 4) }).unwrap();
        db.insert_take(&take("b", "2026-10-14T09:01:00+01:00", 4)).unwrap();
        db.insert_take(&Take { audio_path: Some("c.wav".into()), ..take("c", "2026-10-14T09:02:00+01:00", 4) }).unwrap();

        let mut paths = db.clear_history().unwrap();
        paths.sort();
        assert_eq!(paths, ["a.wav", "c.wav"]);
        assert!(db.list_takes(&query()).unwrap().is_empty());
        assert!(found(&db, "final").is_empty());
        let stats = db.stats_on(date(2026, 10, 14)).unwrap();
        assert_eq!((stats.words_total, stats.takes_total, stats.streak_days), (0, 0, 0));
        assert_index_in_step(&db);
        assert!(db.clear_history().unwrap().is_empty(), "clearing an empty history is fine");
        // The table is usable afterwards.
        db.insert_take(&take("d", "2026-10-14T09:03:00+01:00", 1)).unwrap();
        assert_eq!(found(&db, "final d"), ["d"]);
    }

    // ---- app names ----

    #[test]
    fn app_names_are_distinct_sorted_and_skip_blanks() {
        let db = db();
        for (i, app) in ["slack", "Outlook", "Slack", "", "notepad", "Outlook"].iter().enumerate() {
            db.insert_take(&Take { app_name: app.to_string(), ..take(&format!("t{i}"), "2026-10-14T09:00:00+01:00", 1) }).unwrap();
        }
        let names = db.app_names().unwrap();
        assert_eq!(names.len(), 3, "{names:?}");
        assert_eq!(names[0].to_lowercase(), "notepad");
        assert_eq!(names[1], "Outlook");
        assert_eq!(names[2].to_lowercase(), "slack");
        assert!(Db::open_memory().unwrap().app_names().unwrap().is_empty());
    }

    // ---- stats ----

    #[test]
    fn stats_on_an_empty_history_are_all_zero_with_a_full_chart() {
        let stats = db().stats_on(date(2026, 10, 14)).unwrap();
        assert_eq!(stats.words_today, 0);
        assert_eq!(stats.words_week, 0);
        assert_eq!(stats.words_total, 0);
        assert_eq!(stats.takes_total, 0);
        assert_eq!(stats.minutes_saved, 0.0);
        assert_eq!(stats.avg_wpm, 0.0);
        assert_eq!(stats.streak_days, 0);
        assert_eq!(stats.daily.len(), 14);
        assert!(stats.daily.iter().all(|d| d.words == 0));
    }

    /// Wednesday 14 Oct 2026 is "today" in these tests.
    fn stats_fixture() -> Db {
        let db = db();
        add_at(&db, "today-am", "2026-10-14T09:00:00+01:00", 100, 60_000);
        add_at(&db, "today-pm", "2026-10-14T18:30:00+01:00", 50, 30_000);
        add_at(&db, "four-days-ago", "2026-10-10T12:00:00+01:00", 200, 120_000);
        add_at(&db, "six-days-ago", "2026-10-08T12:00:00+01:00", 10, 6_000);
        add_at(&db, "seven-days-ago", "2026-10-07T12:00:00+01:00", 1_000, 600_000);
        add_at(&db, "edge-of-30", "2026-09-15T12:00:00+01:00", 5, 3_000);
        add_at(&db, "just-outside-30", "2026-09-14T12:00:00+01:00", 7, 4_000);
        db.insert_take(&Take { status: "cancelled".into(), ..take("cancelled", "2026-10-14T10:00:00+01:00", 999) }).unwrap();
        db.insert_take(&Take { status: "error".into(), ..take("failed", "2026-10-14T11:00:00+01:00", 888) }).unwrap();
        db
    }

    #[test]
    fn stats_count_words_for_today_the_week_and_in_total() {
        let stats = stats_fixture().stats_on(date(2026, 10, 14)).unwrap();
        assert_eq!(stats.words_today, 150, "cancelled and error takes are ignored");
        assert_eq!(stats.words_week, 150 + 200 + 10, "today plus six days back; 7 days ago is out");
        assert_eq!(stats.words_total, 150 + 200 + 10 + 1_000 + 5 + 7);
        assert_eq!(stats.takes_total, 7, "cancelled and error takes are not takes");
    }

    #[test]
    fn stats_average_speed_covers_exactly_the_last_thirty_days() {
        let stats = stats_fixture().stats_on(date(2026, 10, 14)).unwrap();
        // 30-day window is 15 Sep to 14 Oct: 1,365 words in 819,000 ms (13.65 minutes) = 100 wpm.
        assert!((stats.avg_wpm - 100.0).abs() < 1e-9, "avg_wpm = {}", stats.avg_wpm);
    }

    #[test]
    fn takes_with_no_recorded_length_do_not_inflate_the_average_speed() {
        let db = db();
        add_at(&db, "timed", "2026-10-14T09:00:00+01:00", 100, 60_000);
        add_at(&db, "untimed", "2026-10-14T10:00:00+01:00", 500, 0);
        let stats = db.stats_on(date(2026, 10, 14)).unwrap();
        assert!((stats.avg_wpm - 100.0).abs() < 1e-9, "avg_wpm = {}", stats.avg_wpm);
        assert_eq!(stats.words_today, 600, "but the words still count");
    }

    #[test]
    fn stats_minutes_saved_is_typing_time_minus_speaking_time() {
        let stats = stats_fixture().stats_on(date(2026, 10, 14)).unwrap();
        // 1,372 words / 40 wpm = 34.3 min typing; (819,000 + 4,000) ms = 13.7167 min speaking.
        let expected = 1_372.0 / 40.0 - 823_000.0 / 60_000.0;
        assert!((stats.minutes_saved - expected).abs() < 1e-9, "{} vs {expected}", stats.minutes_saved);
    }

    #[test]
    fn stats_minutes_saved_never_goes_below_zero() {
        let db = db();
        // 10 words would take 15 seconds to type, but this take lasted a minute.
        add_at(&db, "slow", "2026-10-14T09:00:00+01:00", 10, 60_000);
        assert_eq!(db.stats_on(date(2026, 10, 14)).unwrap().minutes_saved, 0.0);
    }

    #[test]
    fn stats_daily_is_fourteen_days_oldest_first_with_zeros() {
        let stats = stats_fixture().stats_on(date(2026, 10, 14)).unwrap();
        assert_eq!(stats.daily.len(), 14);
        assert_eq!(stats.daily.first().unwrap().date, "2026-10-01");
        assert_eq!(stats.daily.last().unwrap().date, "2026-10-14");
        let words = |d: &str| stats.daily.iter().find(|x| x.date == d).unwrap().words;
        assert_eq!(words("2026-10-14"), 150);
        assert_eq!(words("2026-10-10"), 200);
        assert_eq!(words("2026-10-08"), 10);
        assert_eq!(words("2026-10-07"), 1_000);
        assert_eq!(words("2026-10-13"), 0);
        assert_eq!(words("2026-10-01"), 0);
        assert!(stats.daily.windows(2).all(|w| w[0].date < w[1].date), "oldest first");
    }

    #[test]
    fn stats_daily_crosses_month_and_year_ends() {
        let db = db();
        add_at(&db, "ny", "2027-01-01T08:00:00+00:00", 3, 1_000);
        add_at(&db, "nye", "2026-12-31T23:30:00+00:00", 4, 1_000);
        let stats = db.stats_on(date(2027, 1, 2)).unwrap();
        let dates: Vec<&str> = stats.daily.iter().map(|d| d.date.as_str()).collect();
        assert_eq!(dates[0], "2026-12-20");
        assert_eq!(dates[13], "2027-01-02");
        assert_eq!(stats.daily[12].words, 3);
        assert_eq!(stats.daily[11].words, 4);
        assert_eq!(stats.words_week, 7);
    }

    #[test]
    fn a_take_belongs_to_the_day_on_its_own_clock() {
        let db = db();
        // 23:30 in UTC+1 is 22:30 UTC, still the 14th; 00:30 in UTC+1 on the 15th is 23:30 UTC on the 14th.
        add_at(&db, "late", "2026-10-14T23:30:00+01:00", 5, 1_000);
        add_at(&db, "after-midnight", "2026-10-15T00:30:00+01:00", 7, 1_000);
        let stats = db.stats_on(date(2026, 10, 15)).unwrap();
        assert_eq!(stats.words_today, 7);
        assert_eq!(stats.words_week, 12);
        assert_eq!(stats.streak_days, 2);
    }

    fn streak_for(days: &[(i32, u32, u32)], today: NaiveDate) -> u32 {
        let db = db();
        for (i, (y, m, d)) in days.iter().enumerate() {
            add_at(&db, &format!("t{i}"), &format!("{y}-{m:02}-{d:02}T12:00:00+01:00"), 5, 1_000);
        }
        db.stats_on(today).unwrap().streak_days
    }

    #[test]
    fn streak_counts_consecutive_days_ending_today() {
        let today = date(2026, 10, 14);
        assert_eq!(streak_for(&[], today), 0);
        assert_eq!(streak_for(&[(2026, 10, 14)], today), 1);
        assert_eq!(streak_for(&[(2026, 10, 14), (2026, 10, 13), (2026, 10, 12)], today), 3);
        assert_eq!(streak_for(&[(2026, 10, 14), (2026, 10, 14), (2026, 10, 13)], today), 2, "two takes on a day count once");
    }

    #[test]
    fn streak_survives_until_the_end_of_today_if_yesterday_had_a_take() {
        let today = date(2026, 10, 14);
        assert_eq!(streak_for(&[(2026, 10, 13), (2026, 10, 12), (2026, 10, 11)], today), 3, "nothing yet today");
        assert_eq!(streak_for(&[(2026, 10, 13)], today), 1);
    }

    #[test]
    fn streak_is_broken_by_a_gap_or_by_old_activity() {
        let today = date(2026, 10, 14);
        assert_eq!(streak_for(&[(2026, 10, 14), (2026, 10, 13), (2026, 10, 11)], today), 2, "gap on the 12th");
        assert_eq!(streak_for(&[(2026, 10, 12), (2026, 10, 11)], today), 0, "last take two days ago");
        assert_eq!(streak_for(&[(2026, 10, 14), (2026, 10, 12), (2026, 10, 11), (2026, 10, 10)], today), 1);
    }

    #[test]
    fn streak_ignores_future_days_and_crosses_month_year_and_leap_boundaries() {
        assert_eq!(streak_for(&[(2026, 10, 15), (2026, 10, 14)], date(2026, 10, 14)), 1, "a take dated tomorrow is not today");
        assert_eq!(streak_for(&[(2026, 11, 1), (2026, 10, 31), (2026, 10, 30)], date(2026, 11, 1)), 3);
        assert_eq!(streak_for(&[(2027, 1, 1), (2026, 12, 31)], date(2027, 1, 1)), 2);
        assert_eq!(streak_for(&[(2028, 3, 1), (2028, 2, 29), (2028, 2, 28)], date(2028, 3, 1)), 3);
        assert_eq!(streak_for(&[(2027, 3, 1), (2027, 2, 28)], date(2027, 3, 1)), 2, "no 29 Feb in 2027");
    }

    #[test]
    fn streak_does_not_count_cancelled_or_failed_takes() {
        let db = db();
        add_at(&db, "yesterday", "2026-10-13T12:00:00+01:00", 5, 1_000);
        db.insert_take(&Take { status: "cancelled".into(), ..take("cancelled-today", "2026-10-14T12:00:00+01:00", 5) }).unwrap();
        db.insert_take(&Take { status: "error".into(), ..take("failed-two-days-ago", "2026-10-12T12:00:00+01:00", 5) }).unwrap();
        assert_eq!(db.stats_on(date(2026, 10, 14)).unwrap().streak_days, 1, "only yesterday really counts");
    }

    #[test]
    fn stats_use_the_real_clock_when_asked_for_today() {
        let db = db();
        db.insert_take(&Take { created_at: now_rfc3339(), ..take("now", "", 12) }).unwrap();
        let stats = db.stats().unwrap();
        assert_eq!(stats.words_today, 12);
        assert_eq!(stats.streak_days, 1);
        assert_eq!(stats.daily.last().unwrap().words, 12);
    }

    // ---- audio retention ----

    #[test]
    fn purging_audio_clears_old_paths_and_keeps_the_text() {
        let db = db();
        let now = Local::now();
        let stamp = |days_ago: i64| (now - chrono::Duration::days(days_ago)).to_rfc3339_opts(SecondsFormat::Secs, false);
        db.insert_take(&Take { audio_path: Some("old.wav".into()), ..take("old", &stamp(40), 2) }).unwrap();
        db.insert_take(&Take { audio_path: Some("older.wav".into()), ..take("older", &stamp(400), 2) }).unwrap();
        db.insert_take(&Take { audio_path: Some("recent.wav".into()), ..take("recent", &stamp(2), 2) }).unwrap();
        db.insert_take(&take("no-audio", &stamp(90), 2)).unwrap();

        let mut purged = db.purge_audio_older_than(30).unwrap();
        purged.sort();
        assert_eq!(purged, ["old.wav", "older.wav"]);

        assert_eq!(db.get_take("old").unwrap().unwrap().audio_path, None);
        assert_eq!(db.get_take("old").unwrap().unwrap().final_text, "final old", "the text stays");
        assert_eq!(db.get_take("recent").unwrap().unwrap().audio_path.as_deref(), Some("recent.wav"));
        assert!(db.purge_audio_older_than(30).unwrap().is_empty(), "nothing left to purge the second time");
        assert_eq!(db.stats().unwrap().takes_total, 4, "no take was deleted");
    }

    #[test]
    fn purging_with_zero_days_means_keep_forever() {
        let db = db();
        db.insert_take(&Take { audio_path: Some("keep.wav".into()), ..take("keep", "2020-01-01T09:00:00+00:00", 2) }).unwrap();
        assert!(db.purge_audio_older_than(0).unwrap().is_empty());
        assert_eq!(db.get_take("keep").unwrap().unwrap().audio_path.as_deref(), Some("keep.wav"));
    }

    #[test]
    fn purge_boundary_is_exact_to_the_cutoff() {
        let db = db();
        db.insert_take(&Take { audio_path: Some("a.wav".into()), ..take("a", "2026-10-14T09:00:00+00:00", 1) }).unwrap();
        let ts = DateTime::parse_from_rfc3339("2026-10-14T09:00:00+00:00").unwrap().timestamp_millis();
        assert!(db.purge_audio_before(ts).unwrap().is_empty(), "a take exactly at the cutoff is not older");
        assert_eq!(db.purge_audio_before(ts + 1).unwrap(), ["a.wav"]);
    }

    // ---- indexes ----

    fn query_plan(db: &Db, sql: &str, params: &[&str]) -> String {
        let conn = db.conn.lock();
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let rows = stmt.query_map(params_from_iter(params.iter()), |row| row.get::<_, String>(3)).unwrap();
        rows.map(|row| row.unwrap()).collect::<Vec<_>>().join(" | ")
    }

    /// A history with enough rows that the planner has a real choice to make.
    fn busy_history() -> Db {
        let db = db();
        for i in 0..300 {
            db.insert_take(&Take {
                app_name: format!("App {}", i % 7),
                kind: if i % 5 == 0 { "ramble".into() } else { "talk".into() },
                ..take(&format!("t{i:03}"), &format!("2026-09-{:02}T{:02}:00:00+01:00", 1 + i % 28, 8 + i % 10), 5)
            })
            .unwrap();
        }
        db.conn.lock().execute_batch("ANALYZE").unwrap();
        db
    }

    #[test]
    fn the_statistics_queries_read_only_the_covering_index() {
        let db = busy_history();
        for (name, sql, params) in [
            ("totals", TOTALS_SQL, vec![]),
            ("per day", PER_DAY_SQL, vec!["2026-09-01", "2026-09-30"]),
            ("streak", STREAK_SQL, vec!["2026-09-30"]),
        ] {
            let plan = query_plan(&db, sql, &params);
            assert!(plan.contains("COVERING INDEX takes_by_day"), "{name} should use the covering index, but the plan is: {plan}");
        }
    }

    #[test]
    fn the_newest_first_list_is_read_in_index_order_without_sorting() {
        let db = busy_history();
        let plan = query_plan(&db, &format!("SELECT {TAKE_COLS} FROM takes ORDER BY ts DESC, seq DESC LIMIT 50 OFFSET 0"), &[]);
        assert!(plan.contains("takes_by_time"), "{plan}");
        assert!(!plan.contains("TEMP B-TREE"), "{plan}");
        let filtered = query_plan(&db, &format!("SELECT {TAKE_COLS} FROM takes WHERE kind = 'ramble' ORDER BY ts DESC, seq DESC LIMIT 50"), &[]);
        assert!(!filtered.contains("TEMP B-TREE"), "{filtered}");
    }

    // ---- helpers ----

    #[test]
    fn fts_queries_are_quoted_prefix_terms() {
        assert_eq!(fts_query("invoice").unwrap(), "\"invoice\"*");
        assert_eq!(fts_query("  inv   ali ").unwrap(), "\"inv\"* \"ali\"*");
        assert_eq!(fts_query("say \"hi\"").unwrap(), "\"say\"* \"\"\"hi\"\"\"*");
        assert_eq!(fts_query("chrome.exe").unwrap(), "\"chrome.exe\"*");
        assert!(fts_query("???").is_none());
        assert!(fts_query("ok ???").is_none(), "a punctuation-only word sends the whole search to LIKE");
        assert!(fts_query("   ").is_none());
    }

    #[test]
    fn like_patterns_escape_wildcards() {
        assert_eq!(like_pattern("abc"), "%abc%");
        assert_eq!(like_pattern("50%_off\\"), "%50\\%\\_off\\\\%");
    }
}
