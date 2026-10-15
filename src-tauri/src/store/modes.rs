//! Stored modes: custom modes, and edits to built-ins (instructions, apps, use_ai) by id.
//!
//! OWNER: data agent. STATUS: implemented, with unit tests.
//!
//! * `list_stored_modes` returns every stored row in the order it was first saved, which is the
//!   order custom modes appear in after the built-ins (`text::modes::merge` appends them).
//! * `save_mode` is an upsert by id. With an empty id it makes a new custom mode and gives it a
//!   slug from the name ("My Email Style" -> "my-email-style"), adding -2, -3... when that id is
//!   taken. A custom mode can never take a built-in id (it would silently become an override of
//!   that built-in); `BUILTIN_IDS` lists them and must follow `text::modes::builtin_modes`.
//! * Saving a built-in (builtin: true) stores an override; `delete_mode` on it restores the default.
//! * Process names in `apps` are stored lower case, trimmed and without repeats, because that is how
//!   `pick_mode` compares them.
//! * Modes installed from a pack carry the pack id as their `source`. If the user edits one it
//!   becomes theirs (source manual) so it survives the pack being updated or removed.

use super::Db;
use crate::types::Mode;
use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::HashSet;

/// Ids of the built-in modes, which the UI and settings refer to. Keep in step with
/// `text::modes::builtin_modes`.
pub const BUILTIN_IDS: [&str; 8] = ["raw", "clean", "message", "email", "prompt", "interview", "speak", "notes"];

/// Longest slug made from a name.
const MAX_SLUG_LEN: usize = 40;

impl Db {
    pub fn list_stored_modes(&self) -> Result<Vec<Mode>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT id, json FROM modes ORDER BY seq")?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
        let mut modes = Vec::new();
        for row in rows {
            let (id, json) = row?;
            match serde_json::from_str::<Mode>(&json) {
                Ok(mode) => modes.push(mode),
                // One unreadable row must not stop Navi from loading every other mode.
                Err(e) => log::warn!("skipping stored mode {id}: {e}"),
            }
        }
        Ok(modes)
    }

    /// The id of every stored row, including any `list_stored_modes` skips because it can't be
    /// read (saved by a newer Navi, or damaged). The settings tidy-up keeps those modes' keys.
    pub fn list_mode_ids(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT id FROM modes ORDER BY seq")?;
        let ids = stmt.query_map([], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    }

    /// Insert or replace by id. A new custom mode with an empty id gets a slug from its name.
    pub fn save_mode(&self, mode: &Mode) -> Result<Mode> {
        let name = mode.name.trim();
        if name.is_empty() {
            bail!("A mode needs a name.");
        }
        let mut stored = mode.clone();
        stored.name = name.to_string();
        stored.apps = tidy_apps(&mode.apps);

        let wanted_id = mode.id.trim();
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if wanted_id.is_empty() {
            stored.id = unique_mode_id(&tx, &slugify(name))?;
            stored.builtin = false;
        } else {
            if !mode.builtin && BUILTIN_IDS.contains(&wanted_id) {
                bail!("\"{wanted_id}\" is the id of a built-in mode. Choose a different name for your mode.");
            }
            stored.id = wanted_id.to_string();
        }
        let json = serde_json::to_string(&stored)?;
        // Whoever saves a mode through the app owns it from then on, including a pack's mode.
        tx.execute(
            "INSERT INTO modes (id, source, json) VALUES (?1, 'manual', ?2)
             ON CONFLICT(id) DO UPDATE SET json = excluded.json, source = 'manual'",
            params![stored.id, json],
        )?;
        tx.commit()?;
        Ok(stored)
    }

    /// Deletes a custom mode, or removes the override of a built-in (restoring its defaults).
    pub fn delete_mode(&self, id: &str) -> Result<()> {
        self.conn.lock().execute("DELETE FROM modes WHERE id = ?1", [id.trim()])?;
        Ok(())
    }
}

/// Lower-case URL-style id: letters and digits joined by single hyphens, accents folded to plain
/// letters, apostrophes dropped, at most 40 characters. Returns "" when the text has nothing
/// usable (for example only symbols).
pub fn slugify(text: &str) -> String {
    let mut slug = String::new();
    let mut gap = false;
    for original in text.chars().flat_map(char::to_lowercase) {
        if matches!(original, '\'' | '\u{2019}' | '\u{2018}' | '`') {
            continue; // "Alex's" -> "alexs"
        }
        let piece = if original.is_ascii_alphanumeric() { Some(original.to_string()) } else { fold_letter(original).map(str::to_string) };
        match piece {
            Some(piece) => {
                if gap && !slug.is_empty() {
                    slug.push('-');
                }
                gap = false;
                slug.push_str(&piece);
            }
            None => gap = true,
        }
    }
    if slug.len() > MAX_SLUG_LEN {
        slug.truncate(MAX_SLUG_LEN); // the slug is ASCII, so this cannot split a character
        while slug.ends_with('-') {
            slug.pop();
        }
    }
    slug
}

/// Plain-letter stand-ins for the accented characters likely to appear in names.
fn fold_letter(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'ç' | 'ć' | 'č' => "c",
        'ď' | 'đ' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => "e",
        'ì' | 'í' | 'î' | 'ï' | 'ī' | 'ı' => "i",
        'ł' => "l",
        'ñ' | 'ń' | 'ň' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => "o",
        'ř' => "r",
        'š' | 'ś' | 'ş' => "s",
        'ť' | 'ţ' => "t",
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' => "u",
        'ý' | 'ÿ' => "y",
        'ž' | 'ź' | 'ż' => "z",
        'ß' => "ss",
        'æ' => "ae",
        'œ' => "oe",
        'þ' => "th",
        _ => return None,
    })
}

/// Process names the way `pick_mode` compares them: lower case, trimmed, no blanks, no repeats.
pub(crate) fn tidy_apps(apps: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    apps.iter()
        .map(|a| a.trim().to_lowercase())
        .filter(|a| !a.is_empty())
        .filter(|a| seen.insert(a.clone()))
        .collect()
}

/// True when `id` is a built-in id or is already stored.
pub(crate) fn mode_id_taken(conn: &Connection, id: &str) -> Result<bool> {
    if BUILTIN_IDS.contains(&id) {
        return Ok(true);
    }
    Ok(conn.query_row("SELECT 1 FROM modes WHERE id = ?1", [id], |_| Ok(())).optional()?.is_some())
}

/// `base`, or `base-2`, `base-3`... whichever is free. An empty base becomes "mode".
pub(crate) fn unique_mode_id(conn: &Connection, base: &str) -> Result<String> {
    let base = if base.is_empty() { "mode" } else { base };
    if !mode_id_taken(conn, base)? {
        return Ok(base.to_string());
    }
    for n in 2..10_000 {
        let candidate = format!("{base}-{n}");
        if !mode_id_taken(conn, &candidate)? {
            return Ok(candidate);
        }
    }
    bail!("Could not find a free id for the mode \"{base}\".")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ModeKind;

    fn db() -> Db {
        Db::open_memory().unwrap()
    }

    fn mode(id: &str, name: &str) -> Mode {
        Mode {
            id: id.into(),
            name: name.into(),
            icon: "pen".into(),
            description: "A test mode".into(),
            kind: ModeKind::Rewrite,
            instructions: "Be brief.".into(),
            apps: vec!["outlook.exe".into()],
            use_ai: true,
            builtin: false,
        }
    }

    fn ids(db: &Db) -> Vec<String> {
        db.list_stored_modes().unwrap().into_iter().map(|m| m.id).collect()
    }

    // ---- slugs ----

    #[test]
    fn slugs_are_lower_case_ascii_words_joined_by_hyphens() {
        assert_eq!(slugify("My Email Style"), "my-email-style");
        assert_eq!(slugify("  Alex's  Notes!! "), "alexs-notes");
        assert_eq!(slugify("Alex\u{2019}s Notes"), "alexs-notes");
        assert_eq!(slugify("A/B  C_d"), "a-b-c-d");
        assert_eq!(slugify("Café Déjà-vu"), "cafe-deja-vu");
        assert_eq!(slugify("Straße"), "strasse");
        assert_eq!(slugify("ÆØÅ"), "aeoa");
        assert_eq!(slugify("Acme Team"), "acme-team");
        assert_eq!(slugify("Version 2.0"), "version-2-0");
    }

    #[test]
    fn slugs_of_nothing_usable_are_empty() {
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("---"), "");
        assert_eq!(slugify("!!! ???"), "");
        assert_eq!(slugify("日本語"), "");
    }

    #[test]
    fn slugs_are_capped_without_a_trailing_hyphen() {
        let long = "word ".repeat(30);
        let slug = slugify(&long);
        assert!(slug.len() <= MAX_SLUG_LEN, "{slug}");
        assert!(!slug.ends_with('-') && !slug.starts_with('-'));
        assert!(slug.starts_with("word-word"));
        assert_eq!(slugify(&"x".repeat(100)).len(), MAX_SLUG_LEN);
    }

    // ---- saving ----

    #[test]
    fn a_new_custom_mode_gets_a_slug_id_and_is_not_built_in() {
        let db = db();
        let saved = db.save_mode(&Mode { builtin: true, ..mode("", "  Sales Email ") }).unwrap();
        assert_eq!(saved.id, "sales-email");
        assert_eq!(saved.name, "Sales Email", "the name is trimmed");
        assert!(!saved.builtin, "a mode made from scratch cannot claim to be built in");
        assert_eq!(db.list_stored_modes().unwrap().len(), 1);
        let back = &db.list_stored_modes().unwrap()[0];
        assert_eq!(back.id, "sales-email");
        assert_eq!(back.instructions, "Be brief.");
        assert_eq!(back.kind, ModeKind::Rewrite);
        assert!(back.use_ai);
    }

    #[test]
    fn every_field_survives_a_round_trip() {
        let db = db();
        for kind in [ModeKind::Raw, ModeKind::Clean, ModeKind::Rewrite, ModeKind::Prompt, ModeKind::Notes] {
            let original = Mode { kind: kind.clone(), use_ai: false, ..mode("", &format!("Mode {kind:?}")) };
            let saved = db.save_mode(&original).unwrap();
            let back = db.list_stored_modes().unwrap().into_iter().find(|m| m.id == saved.id).unwrap();
            assert_eq!(back.kind, kind);
            assert!(!back.use_ai);
            assert_eq!(back.icon, "pen");
            assert_eq!(back.description, "A test mode");
            assert_eq!(back.apps, ["outlook.exe"]);
        }
    }

    #[test]
    fn clashing_names_get_numbered_ids() {
        let db = db();
        assert_eq!(db.save_mode(&mode("", "Sales Email")).unwrap().id, "sales-email");
        assert_eq!(db.save_mode(&mode("", "Sales email")).unwrap().id, "sales-email-2");
        assert_eq!(db.save_mode(&mode("", "SALES EMAIL")).unwrap().id, "sales-email-3");
        assert_eq!(db.save_mode(&mode("", "!!!")).unwrap().id, "mode", "a name with no letters falls back");
        assert_eq!(db.save_mode(&mode("", "???")).unwrap().id, "mode-2");
    }

    #[test]
    fn a_new_mode_named_like_a_built_in_does_not_take_its_id() {
        let db = db();
        assert_eq!(db.save_mode(&mode("", "Email")).unwrap().id, "email-2");
        assert_eq!(db.save_mode(&mode("", "Clean")).unwrap().id, "clean-2");
        assert_eq!(db.save_mode(&mode("", "Notes")).unwrap().id, "notes-2");
        assert_eq!(db.save_mode(&mode("", "Prompt")).unwrap().id, "prompt-2");
    }

    #[test]
    fn a_custom_mode_cannot_be_saved_under_a_built_in_id() {
        let db = db();
        let err = db.save_mode(&mode("email", "My email")).unwrap_err();
        assert!(format!("{err:#}").contains("built-in"), "{err:#}");
        assert!(db.list_stored_modes().unwrap().is_empty());
    }

    #[test]
    fn saving_a_built_in_stores_an_override_and_deleting_it_restores_the_default() {
        let db = db();
        let edited = Mode { instructions: "Always sign off with Cheers.".into(), builtin: true, use_ai: false, ..mode("email", "Email") };
        let saved = db.save_mode(&edited).unwrap();
        assert_eq!(saved.id, "email");
        assert!(saved.builtin);
        let stored = db.list_stored_modes().unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].instructions, "Always sign off with Cheers.");
        assert!(!stored[0].use_ai);

        db.delete_mode("email").unwrap();
        assert!(db.list_stored_modes().unwrap().is_empty(), "no override left, so the built-in defaults apply");
    }

    #[test]
    fn saving_with_an_existing_id_replaces_the_mode_and_keeps_its_place() {
        let db = db();
        db.save_mode(&mode("", "First")).unwrap();
        db.save_mode(&mode("", "Second")).unwrap();
        db.save_mode(&mode("", "Third")).unwrap();
        let edited = db.save_mode(&Mode { instructions: "New text".into(), ..mode("first", "First (edited)") }).unwrap();
        assert_eq!(edited.id, "first");
        assert_eq!(ids(&db), ["first", "second", "third"], "order is the order of creation");
        let stored = db.list_stored_modes().unwrap();
        assert_eq!(stored[0].name, "First (edited)");
        assert_eq!(stored[0].instructions, "New text");
    }

    #[test]
    fn an_explicit_new_id_is_kept_as_given() {
        let db = db();
        assert_eq!(db.save_mode(&mode(" my-id ", "Whatever")).unwrap().id, "my-id");
    }

    #[test]
    fn a_mode_needs_a_name() {
        let db = db();
        assert!(db.save_mode(&mode("", "")).is_err());
        assert!(db.save_mode(&mode("x", "   ")).is_err());
        assert!(db.list_stored_modes().unwrap().is_empty());
    }

    #[test]
    fn app_names_are_stored_lower_case_and_tidy() {
        let db = db();
        let apps = vec![" Outlook.EXE ".to_string(), "outlook.exe".to_string(), "".to_string(), "Slack.exe".to_string()];
        let saved = db.save_mode(&Mode { apps, ..mode("", "Apps") }).unwrap();
        assert_eq!(saved.apps, ["outlook.exe", "slack.exe"]);
    }

    #[test]
    fn deleting_is_forgiving() {
        let db = db();
        let m = db.save_mode(&mode("", "Temp")).unwrap();
        db.delete_mode(&m.id).unwrap();
        db.delete_mode(&m.id).unwrap();
        db.delete_mode("never-existed").unwrap();
        assert!(db.list_stored_modes().unwrap().is_empty());
        // The id is free again.
        assert_eq!(db.save_mode(&mode("", "Temp")).unwrap().id, "temp");
    }

    #[test]
    fn editing_a_pack_mode_makes_it_the_users_own() {
        let db = db();
        let json = serde_json::to_string(&mode("pack-mode", "From a pack")).unwrap();
        db.conn.lock().execute("INSERT INTO modes (id, source, json) VALUES ('pack-mode', 'team-pack', ?1)", [json]).unwrap();
        db.save_mode(&Mode { instructions: "My tweak".into(), ..mode("pack-mode", "From a pack") }).unwrap();
        let source: String = db.conn.lock().query_row("SELECT source FROM modes WHERE id = 'pack-mode'", [], |r| r.get(0)).unwrap();
        assert_eq!(source, "manual");
    }

    #[test]
    fn an_unreadable_row_is_skipped_not_fatal() {
        let db = db();
        db.save_mode(&mode("", "Good")).unwrap();
        db.conn.lock().execute("INSERT INTO modes (id, source, json) VALUES ('broken', 'manual', '{not json')", []).unwrap();
        db.save_mode(&mode("", "Also good")).unwrap();
        assert_eq!(ids(&db), ["good", "also-good"]);
        assert_eq!(db.list_mode_ids().unwrap(), ["good", "broken", "also-good"], "every row is still listed by id");
    }
}
