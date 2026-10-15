//! Meeting notes: a call recorded on both sides (your microphone and what the computer played),
//! its labelled transcript, the notes you typed during it and the notes Navi wrote afterwards.
//!
//! * Stored in the `notes` table (schema version 2). The transcript is JSON (a list of lines).
//!   Short ideas, call jobs and screenshots live together as JSON in `extras` (version 3).
//! * `save_note` is an upsert by id, so a note being taken can be saved again as it grows.
//! * Newest first in `list_notes`; `list_notes` leaves the transcript and extras out (they can be
//!   long) but counts the ideas, jobs and kept screenshots for the list.

use super::Db;
use anyhow::Result;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

/// One line of a meeting transcript.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Line {
    /// Milliseconds from the start of the meeting.
    pub at_ms: u64,
    /// "you" (your microphone) or "them" (what the computer played).
    pub who: String,
    pub text: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct MeetingNote {
    pub id: String,
    /// RFC 3339, local time.
    pub created_at: String,
    pub title: String,
    /// The app the call was in ("Microsoft Teams"), when known.
    pub app: String,
    pub duration_ms: u64,
    /// "recording" while it is being taken, "writing" while the notes are made, "done".
    pub status: String,
    pub transcript: Vec<Line>,
    /// What you typed during the meeting.
    pub my_notes: String,
    /// The finished notes, in Markdown.
    pub notes_md: String,
    pub file_path: Option<String>,
    /// Ideas for Shorts that came up on the call (ideas.rs).
    pub ideas: Vec<ShortIdea>,
    /// Jobs given on the call with "Hey Navi, ..." (jobs.rs).
    pub jobs: Vec<CallJob>,
    /// Screenshots taken on the call or while watching (screen.rs, watch.rs).
    pub shots: Vec<Shot>,
    /// In `list_notes` only, where the lists above are left out: how many of each there are.
    pub counts: Option<Counts>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Counts {
    pub ideas: usize,
    pub jobs: usize,
    pub shots: usize,
}

/// An idea for a Short: what it teaches and the exact lines to cut it from.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct ShortIdea {
    pub id: String,
    pub title: String,
    /// The line it opens on.
    pub hook: String,
    /// What a viewer learns, in a sentence.
    pub lesson: String,
    /// The lines to cut, in the order the Short uses them, with where they are in the call.
    pub quotes: Vec<IdeaQuote>,
    /// Why it would work, briefly.
    pub why: String,
    /// Things to check first ("needs the speaker's OK: a client is named").
    pub flags: Vec<String>,
    /// Rough length in seconds.
    pub seconds: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct IdeaQuote {
    pub at_ms: u64,
    pub text: String,
}

/// A job given on a call ("Hey Navi, draft the proposal") and what the AI made of it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct CallJob {
    pub id: String,
    /// When in the call it was asked.
    pub at_ms: u64,
    pub ask: String,
    /// "working" | "done" | "failed".
    pub status: String,
    pub result_md: String,
    pub error: String,
}

/// A screenshot kept with a note: a JPEG in the app data folder.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Shot {
    pub at_ms: u64,
    pub path: String,
    /// The app and window it showed.
    pub app: String,
    pub window: String,
    /// Text read off it on this computer (Windows OCR), shortened.
    pub text: String,
    /// What the AI said it shows, once the screenshots have been written up.
    pub caption: String,
}

/// The JSON kept in the `extras` column.
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Extras {
    ideas: Vec<ShortIdea>,
    jobs: Vec<CallJob>,
    shots: Vec<Shot>,
}

impl Db {
    pub fn save_note(&self, note: &MeetingNote) -> Result<()> {
        let ts = chrono::DateTime::parse_from_rfc3339(&note.created_at).map(|d| d.timestamp()).unwrap_or_else(|_| chrono::Local::now().timestamp());
        let transcript = serde_json::to_string(&note.transcript)?;
        let extras = serde_json::to_string(&Extras { ideas: note.ideas.clone(), jobs: note.jobs.clone(), shots: note.shots.clone() })?;
        self.conn.lock().execute(
            "INSERT INTO notes (id, created_at, ts, title, app, duration_ms, status, transcript, my_notes, notes_md, file_path, extras)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO UPDATE SET title = excluded.title, app = excluded.app, duration_ms = excluded.duration_ms,
                status = excluded.status, transcript = excluded.transcript, my_notes = excluded.my_notes,
                notes_md = excluded.notes_md, file_path = excluded.file_path, extras = excluded.extras",
            params![note.id, note.created_at, ts, note.title, note.app, note.duration_ms as i64, note.status, transcript, note.my_notes, note.notes_md, note.file_path, extras],
        )?;
        Ok(())
    }

    /// Newest first, without transcripts or extras (counted instead).
    pub fn list_notes(&self, limit: u32) -> Result<Vec<MeetingNote>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, created_at, title, app, duration_ms, status, my_notes, notes_md, file_path, extras FROM notes ORDER BY ts DESC, seq DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit.max(1) as i64], |r| {
            let extras: Extras = serde_json::from_str(&r.get::<_, String>(9)?).unwrap_or_default();
            Ok(MeetingNote {
                id: r.get(0)?,
                created_at: r.get(1)?,
                title: r.get(2)?,
                app: r.get(3)?,
                duration_ms: r.get::<_, i64>(4)?.max(0) as u64,
                status: r.get(5)?,
                transcript: Vec::new(),
                my_notes: r.get(6)?,
                notes_md: r.get(7)?,
                file_path: r.get(8)?,
                counts: Some(Counts { ideas: extras.ideas.len(), jobs: extras.jobs.len(), shots: extras.shots.len() }),
                ..Default::default()
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn get_note(&self, id: &str) -> Result<Option<MeetingNote>> {
        let conn = self.conn.lock();
        let note = conn
            .query_row(
                "SELECT id, created_at, title, app, duration_ms, status, transcript, my_notes, notes_md, file_path, extras FROM notes WHERE id = ?1",
                [id],
                |r| {
                    let transcript: String = r.get(6)?;
                    let extras: Extras = serde_json::from_str(&r.get::<_, String>(10)?).unwrap_or_default();
                    Ok(MeetingNote {
                        id: r.get(0)?,
                        created_at: r.get(1)?,
                        title: r.get(2)?,
                        app: r.get(3)?,
                        duration_ms: r.get::<_, i64>(4)?.max(0) as u64,
                        status: r.get(5)?,
                        transcript: serde_json::from_str(&transcript).unwrap_or_default(),
                        my_notes: r.get(7)?,
                        notes_md: r.get(8)?,
                        file_path: r.get(9)?,
                        ideas: extras.ideas,
                        jobs: extras.jobs,
                        shots: extras.shots,
                        counts: None,
                    })
                },
            )
            .optional()?;
        Ok(note)
    }

    pub fn delete_note(&self, id: &str) -> Result<()> {
        self.conn.lock().execute("DELETE FROM notes WHERE id = ?1", [id])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(id: &str, at: &str) -> MeetingNote {
        MeetingNote {
            id: id.into(),
            created_at: at.into(),
            title: format!("Call {id}"),
            app: "Microsoft Teams".into(),
            duration_ms: 61_000,
            status: "done".into(),
            transcript: vec![Line { at_ms: 0, who: "them".into(), text: "Hello?".into() }, Line { at_ms: 1500, who: "you".into(), text: "Hi, can you hear me?".into() }],
            my_notes: "budget".into(),
            notes_md: "# Call\n\n- Budget agreed".into(),
            file_path: None,
            ..Default::default()
        }
    }

    #[test]
    fn ideas_jobs_and_screenshots_are_kept() {
        let db = Db::open_memory().unwrap();
        let mut n = note("a", "2026-10-14T10:00:00+01:00");
        n.ideas.push(ShortIdea { id: "i1".into(), title: "Open later on Fridays".into(), quotes: vec![IdeaQuote { at_ms: 61_000, text: "Half our Friday trade comes in after six.".into() }], ..Default::default() });
        n.jobs.push(CallJob { id: "j1".into(), ask: "draft the proposal".into(), status: "done".into(), result_md: "# Proposal".into(), ..Default::default() });
        n.shots.push(Shot { at_ms: 5_000, path: "C:\\shots\\1.jpg".into(), text: "Pricing".into(), ..Default::default() });
        db.save_note(&n).unwrap();
        let back = db.get_note("a").unwrap().unwrap();
        assert_eq!((back.ideas.len(), back.jobs.len(), back.shots.len()), (1, 1, 1));
        assert_eq!(back.ideas[0].quotes[0].at_ms, 61_000);
        assert_eq!(back.jobs[0].result_md, "# Proposal");
        let listed = &db.list_notes(5).unwrap()[0];
        assert!(listed.ideas.is_empty(), "the list leaves extras out");
        assert_eq!(listed.counts, Some(Counts { ideas: 1, jobs: 1, shots: 1 }));
    }

    #[test]
    fn notes_round_trip_newest_first() {
        let db = Db::open_memory().unwrap();
        db.save_note(&note("a", "2026-10-14T10:00:00+01:00")).unwrap();
        db.save_note(&note("b", "2026-10-14T11:00:00+01:00")).unwrap();
        let list = db.list_notes(10).unwrap();
        assert_eq!(list.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["b", "a"]);
        assert!(list[0].transcript.is_empty(), "the list leaves transcripts out");
        let full = db.get_note("a").unwrap().unwrap();
        assert_eq!(full.transcript.len(), 2);
        assert_eq!(full.transcript[1].who, "you");

        // Saving again updates in place (a note grows while it is taken).
        let mut a = full;
        a.status = "writing".into();
        a.transcript.push(Line { at_ms: 3000, who: "them".into(), text: "Yes.".into() });
        db.save_note(&a).unwrap();
        let again = db.get_note("a").unwrap().unwrap();
        assert_eq!((again.status.as_str(), again.transcript.len()), ("writing", 3));
        assert_eq!(db.list_notes(10).unwrap().len(), 2);

        db.delete_note("a").unwrap();
        assert!(db.get_note("a").unwrap().is_none());
        assert_eq!(db.list_notes(10).unwrap().len(), 1);
    }
}
