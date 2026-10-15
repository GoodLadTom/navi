//! Where ramble notes and tasks can go: a Markdown file, ClickUp, Notion.
//!
//! OWNER: text agent.
//!
//! Contract:
//! * `save_markdown`: <dir>/<YYYY-MM-DD HHmm> <title>.md (title made filename-safe), creates dir.
//! * ClickUp: POST https://api.clickup.com/api/v2/list/{list_id}/task with header Authorization:
//!   <token>, body {"name": task.text, "description": "From Navi notes: <title>"}. Returns count.
//!   `test_clickup`: GET https://api.clickup.com/api/v2/user -> username.
//! * Notion: POST https://api.notion.com/v1/pages, headers Authorization: Bearer <token>,
//!   Notion-Version: 2022-06-28, parent {database_id}, a title property found by reading the
//!   database first (GET /v1/databases/{id}, the property whose type is "title"), children as
//!   paragraph / bulleted_list_item / to_do blocks. Returns the page URL.
//!   `test_notion`: GET https://api.notion.com/v1/users/me -> bot name.
//!
//! Details:
//! * `save_markdown` never overwrites: a clash gets " (2)", " (3)"... Windows-reserved characters
//!   and names (CON, NUL, COM1...) are made safe and long titles are cut to 80 characters.
//! * ClickUp task descriptions also carry the owner and due date as spoken, when there are any.
//!   ClickUp returns the number of tasks created; it only fails outright when none could be made.
//! * Notion accepts a database id with or without dashes, or a pasted database link. Text is split
//!   into Notion's 2,000-character pieces and a page is capped at Notion's 100 blocks.
//! * Errors are short and readable ("ClickUp rejected the token"); tokens are never logged.

use crate::text::ramble::{Notes, Task};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

const CLICKUP_API: &str = "https://api.clickup.com/api/v2";
const NOTION_API: &str = "https://api.notion.com/v1";
const NOTION_VERSION: &str = "2022-06-28";

pub fn save_markdown(dir: &Path, title: &str, markdown: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).map_err(|e| anyhow!("Couldn't create the notes folder {} ({e}).", dir.display()))?;
    let stamp = chrono::Local::now().format("%Y-%m-%d %H%M").to_string();
    let base = format!("{stamp} {}", safe_file_title(title));
    for n in 1..1000 {
        let name = if n == 1 { format!("{base}.md") } else { format!("{base} ({n}).md") };
        let path = dir.join(name);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                use std::io::Write;
                file.write_all(markdown.as_bytes()).map_err(|e| anyhow!("Couldn't write the note {} ({e}).", path.display()))?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(anyhow!("Couldn't save the note in {} ({e}).", dir.display())),
        }
    }
    Err(anyhow!("Couldn't find a free file name for the note in {}.", dir.display()))
}

/// A title that is safe as a Windows file name: no reserved characters or names, no trailing dots
/// or spaces, at most 80 characters, never empty.
pub(crate) fn safe_file_title(title: &str) -> String {
    let cleaned: String = title.chars().map(|c| if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') { ' ' } else { c }).collect();
    let mut s = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > 80 {
        s = s.chars().take(80).collect();
    }
    let s = s.trim_end_matches(['.', ' ']).trim_start_matches(['.', ' ']).to_string();
    if s.is_empty() {
        return "Notes".to_string();
    }
    let upper = s.to_ascii_uppercase();
    let stem = upper.split('.').next().unwrap_or("");
    let reserved = matches!(stem, "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.chars().nth(3).is_some_and(|c| c.is_ascii_digit()));
    if reserved {
        format!("{s} notes")
    } else {
        s
    }
}

fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(8))
        .user_agent(concat!("Navi/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| anyhow!("Navi couldn't start its network client."))
}

/// Short, readable message for a failed call to `service`.
fn http_error(service: &str, status: u16, body: &str, not_found: &str) -> anyhow::Error {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let detail = ["err", "message", "error"].iter().find_map(|k| v.get(*k).and_then(Value::as_str)).unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ");
    let detail: String = detail.chars().take(160).collect();
    anyhow!(match status {
        401 | 403 => format!("{service} rejected the token. Check it in Settings."),
        404 => not_found.to_string(),
        429 => format!("{service} is rate limiting requests. Try again in a minute."),
        500..=599 => format!("{service} had a server error ({status}). Try again shortly."),
        _ if detail.is_empty() => format!("{service} refused the request (status {status})."),
        _ => format!("{service} refused the request: {detail}"),
    })
}

fn network_error(service: &str, e: &reqwest::Error) -> anyhow::Error {
    if e.is_timeout() {
        anyhow!("{service} took too long to reply.")
    } else {
        anyhow!("No connection to {service}. Check your internet connection.")
    }
}

// ---------------------------------------------------------------------------------------------
// ClickUp
// ---------------------------------------------------------------------------------------------

pub async fn clickup_create_tasks(token: &str, list_id: &str, title: &str, tasks: &[Task]) -> Result<usize> {
    clickup_create_tasks_at(CLICKUP_API, token, list_id, title, tasks).await
}

async fn clickup_create_tasks_at(api: &str, token: &str, list_id: &str, title: &str, tasks: &[Task]) -> Result<usize> {
    let token = token.trim();
    let list_id = list_id.trim();
    if token.is_empty() {
        return Err(anyhow!("Add your ClickUp token in Settings first."));
    }
    if list_id.is_empty() || !list_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(anyhow!("The ClickUp list id doesn't look right. Copy it from the list's address in ClickUp."));
    }
    let tasks: Vec<&Task> = tasks.iter().filter(|t| !t.text.trim().is_empty()).collect();
    if tasks.is_empty() {
        return Ok(0);
    }
    let client = client()?;
    let url = format!("{api}/list/{list_id}/task");
    let mut created = 0;
    let mut first_error: Option<anyhow::Error> = None;
    for task in tasks {
        let body = json!({ "name": task.text.trim(), "description": clickup_description(title, task) });
        let result = client.post(&url).header("Authorization", token).json(&body).send().await;
        match result {
            Ok(resp) if resp.status().is_success() => created += 1,
            Ok(resp) => {
                let status = resp.status().as_u16();
                let text = resp.text().await.unwrap_or_default();
                let err = http_error("ClickUp", status, &text, "ClickUp couldn't find that list. Check the list id in Settings.");
                first_error.get_or_insert(err);
                if matches!(status, 401 | 403 | 404) {
                    break; // the same answer would come back for every task
                }
            }
            Err(e) => {
                first_error.get_or_insert(network_error("ClickUp", &e));
                break;
            }
        }
    }
    match (created, first_error) {
        (0, Some(e)) => Err(e),
        (n, Some(_)) => {
            log::warn!("ClickUp: created {n} task(s); some could not be created");
            Ok(n)
        }
        (n, None) => Ok(n),
    }
}

fn clickup_description(title: &str, task: &Task) -> String {
    let title = if title.trim().is_empty() { "Notes" } else { title.trim() };
    let mut d = format!("From Navi notes: {title}");
    if let Some(owner) = task.owner.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        d.push_str(&format!("\nOwner (as said): {owner}"));
    }
    if let Some(due) = task.due.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        d.push_str(&format!("\nDue (as said): {due}"));
    }
    d
}

pub async fn test_clickup(token: &str) -> Result<String> {
    test_clickup_at(CLICKUP_API, token).await
}

async fn test_clickup_at(api: &str, token: &str) -> Result<String> {
    let token = token.trim();
    if token.is_empty() {
        return Err(anyhow!("Add your ClickUp token in Settings first."));
    }
    let resp = client()?.get(format!("{api}/user")).header("Authorization", token).send().await.map_err(|e| network_error("ClickUp", &e))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(http_error("ClickUp", status, &text, "ClickUp couldn't find your account."));
    }
    let v: Value = serde_json::from_str(&text).map_err(|_| anyhow!("ClickUp sent a reply Navi couldn't read."))?;
    v.pointer("/user/username")
        .and_then(Value::as_str)
        .or_else(|| v.pointer("/user/email").and_then(Value::as_str))
        .map(str::to_string)
        .ok_or_else(|| anyhow!("ClickUp sent a reply Navi couldn't read."))
}

// ---------------------------------------------------------------------------------------------
// Notion
// ---------------------------------------------------------------------------------------------

pub async fn notion_create_page(token: &str, database_id: &str, notes: &Notes) -> Result<String> {
    notion_create_page_at(NOTION_API, token, database_id, notes).await
}

async fn notion_create_page_at(api: &str, token: &str, database_id: &str, notes: &Notes) -> Result<String> {
    let token = token.trim();
    if token.is_empty() {
        return Err(anyhow!("Add your Notion token in Settings first."));
    }
    let db = normalise_notion_id(database_id).ok_or_else(|| anyhow!("The Notion database id doesn't look right. Paste the database link or its id."))?;
    let client = client()?;
    let not_found = "Notion can't see that database. Open it in Notion and add your Navi connection under Connections.";

    // 1. Which property is the title?
    let resp = client
        .get(format!("{api}/databases/{db}"))
        .bearer_auth(token)
        .header("Notion-Version", NOTION_VERSION)
        .send()
        .await
        .map_err(|e| network_error("Notion", &e))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(http_error("Notion", status, &text, not_found));
    }
    let schema: Value = serde_json::from_str(&text).map_err(|_| anyhow!("Notion sent a reply Navi couldn't read."))?;
    let title_prop = schema
        .get("properties")
        .and_then(Value::as_object)
        .and_then(|props| props.iter().find(|(_, p)| p.get("type").and_then(Value::as_str) == Some("title")).map(|(name, _)| name.clone()))
        .ok_or_else(|| anyhow!("That Notion database has no title column."))?;

    // 2. Create the page.
    let title = if notes.title.trim().is_empty() { "Notes" } else { notes.title.trim() };
    let mut properties = serde_json::Map::new();
    properties.insert(title_prop, json!({ "title": rich_text(title) }));
    let body = json!({
        "parent": { "database_id": db },
        "properties": Value::Object(properties),
        "children": notion_blocks(notes),
    });
    let resp = client
        .post(format!("{api}/pages"))
        .bearer_auth(token)
        .header("Notion-Version", NOTION_VERSION)
        .json(&body)
        .send()
        .await
        .map_err(|e| network_error("Notion", &e))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(http_error("Notion", status, &text, not_found));
    }
    let page: Value = serde_json::from_str(&text).map_err(|_| anyhow!("Notion sent a reply Navi couldn't read."))?;
    page.get("url").and_then(Value::as_str).map(str::to_string).ok_or_else(|| anyhow!("Notion made the page but didn't say where it is."))
}

/// 32 hex characters from an id with or without dashes, or from a pasted Notion link.
fn normalise_notion_id(input: &str) -> Option<String> {
    let s = input.trim();
    let path = s.split(['?', '#']).next().unwrap_or(s);
    let last = path.rsplit('/').next().unwrap_or(path);
    let hex: String = last.chars().filter(|c| *c != '-').collect();
    let tail: String = hex.chars().rev().take(32).collect::<Vec<_>>().into_iter().rev().collect();
    (tail.len() == 32 && tail.chars().all(|c| c.is_ascii_hexdigit())).then_some(tail.to_lowercase())
}

/// Notion rich text, split into its 2,000-character pieces.
fn rich_text(text: &str) -> Value {
    let chars: Vec<char> = text.chars().collect();
    let parts: Vec<Value> = chars.chunks(2000).map(|c| json!({ "type": "text", "text": { "content": c.iter().collect::<String>() } })).collect();
    Value::Array(parts)
}

fn bold_label(text: &str) -> Value {
    json!({ "object": "block", "type": "paragraph", "paragraph": { "rich_text": [{ "type": "text", "text": { "content": text }, "annotations": { "bold": true } }] } })
}

fn notion_blocks(notes: &Notes) -> Vec<Value> {
    let mut blocks = Vec::new();
    let block = |kind: &str, text: &str| {
        let mut b = serde_json::Map::new();
        b.insert("object".into(), json!("block"));
        b.insert("type".into(), json!(kind));
        b.insert(kind.to_string(), json!({ "rich_text": rich_text(text) }));
        Value::Object(b)
    };
    if !notes.summary.trim().is_empty() {
        blocks.push(block("paragraph", notes.summary.trim()));
    }
    let sections: [(&str, &Vec<String>); 2] = [("Key points", &notes.points), ("Decisions", &notes.decisions)];
    for (label, items) in sections {
        let items: Vec<&str> = items.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        if items.is_empty() {
            continue;
        }
        blocks.push(bold_label(label));
        blocks.extend(items.into_iter().map(|i| block("bulleted_list_item", i)));
    }
    let tasks: Vec<&Task> = notes.tasks.iter().filter(|t| !t.text.trim().is_empty()).collect();
    if !tasks.is_empty() {
        blocks.push(bold_label("Tasks"));
        for t in tasks {
            let mut text = t.text.trim().to_string();
            match (t.owner.as_deref().filter(|s| !s.trim().is_empty()), t.due.as_deref().filter(|s| !s.trim().is_empty())) {
                (Some(o), Some(d)) => text.push_str(&format!(" ({o}, due {d})")),
                (Some(o), None) => text.push_str(&format!(" ({o})")),
                (None, Some(d)) => text.push_str(&format!(" (due {d})")),
                (None, None) => {}
            }
            blocks.push(json!({ "object": "block", "type": "to_do", "to_do": { "rich_text": rich_text(&text), "checked": false } }));
        }
    }
    if blocks.len() > 100 {
        let extra = blocks.len() - 99;
        blocks.truncate(99);
        blocks.push(block("paragraph", &format!("...and {extra} more items in the Markdown note.")));
    }
    blocks
}

pub async fn test_notion(token: &str) -> Result<String> {
    test_notion_at(NOTION_API, token).await
}

async fn test_notion_at(api: &str, token: &str) -> Result<String> {
    let token = token.trim();
    if token.is_empty() {
        return Err(anyhow!("Add your Notion token in Settings first."));
    }
    let resp = client()?
        .get(format!("{api}/users/me"))
        .bearer_auth(token)
        .header("Notion-Version", NOTION_VERSION)
        .send()
        .await
        .map_err(|e| network_error("Notion", &e))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(http_error("Notion", status, &text, "Notion couldn't find your integration."));
    }
    let v: Value = serde_json::from_str(&text).map_err(|_| anyhow!("Notion sent a reply Navi couldn't read."))?;
    Ok(v.get("name").and_then(Value::as_str).filter(|s| !s.trim().is_empty()).unwrap_or("your Notion integration").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::testkit::{body_json, mock_server};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("navi-notes-test-{}-{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
            TempDir(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn saves_markdown_with_a_safe_dated_name() {
        let dir = TempDir::new();
        let notes_dir = dir.0.join("Navi Notes");
        let p1 = save_markdown(&notes_dir, "Plan: Q4 / launch?", "# Plan\n").expect("save");
        assert!(p1.exists());
        let name = p1.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        let re = regex::Regex::new(r"^\d{4}-\d{2}-\d{2} \d{4} Plan Q4 launch\.md$").expect("regex");
        assert!(re.is_match(&name), "{name}");
        assert_eq!(std::fs::read_to_string(&p1).expect("read"), "# Plan\n");
        // Never overwrites.
        let p2 = save_markdown(&notes_dir, "Plan: Q4 / launch?", "second").expect("save again");
        assert_ne!(p1, p2);
        assert!(p2.to_string_lossy().ends_with("Plan Q4 launch (2).md"));
        assert_eq!(std::fs::read_to_string(&p1).expect("read"), "# Plan\n");
    }

    #[test]
    fn file_titles_are_windows_safe() {
        assert_eq!(safe_file_title("  a<b>c:d\"e/f\\g|h?i*j  "), "a b c d e f g h i j");
        assert_eq!(safe_file_title("..."), "Notes");
        assert_eq!(safe_file_title(""), "Notes");
        assert_eq!(safe_file_title("CON"), "CON notes");
        assert_eq!(safe_file_title("com1"), "com1 notes");
        assert_eq!(safe_file_title("Console plan"), "Console plan");
        assert_eq!(safe_file_title(&"x".repeat(200)).chars().count(), 80);
        assert_eq!(safe_file_title("Ends with dots..."), "Ends with dots");
    }

    #[test]
    fn notion_ids_from_links() {
        let id = "0123456789abcdef0123456789abcdef";
        assert_eq!(normalise_notion_id(id).as_deref(), Some(id));
        assert_eq!(normalise_notion_id("01234567-89ab-cdef-0123-456789abcdef").as_deref(), Some(id));
        assert_eq!(normalise_notion_id("https://www.notion.so/team/My-Tasks-0123456789abcdef0123456789abcdef?v=99").as_deref(), Some(id));
        assert_eq!(normalise_notion_id("not an id"), None);
    }

    #[test]
    fn notion_blocks_shape() {
        let notes = Notes {
            title: "T".into(),
            summary: "Sum.".into(),
            points: vec!["P1".into()],
            decisions: vec![],
            tasks: vec![Task { text: "Do it".into(), owner: Some("Sam".into()), due: Some("Friday".into()) }],
        };
        let blocks = notion_blocks(&notes);
        let kinds: Vec<&str> = blocks.iter().filter_map(|b| b["type"].as_str()).collect();
        assert_eq!(kinds, vec!["paragraph", "paragraph", "bulleted_list_item", "paragraph", "to_do"]);
        assert_eq!(blocks[4]["to_do"]["rich_text"][0]["text"]["content"], "Do it (Sam, due Friday)");
        assert_eq!(blocks[4]["to_do"]["checked"], false);
        let many = Notes { points: (0..150).map(|i| format!("p{i}")).collect(), ..Default::default() };
        assert_eq!(notion_blocks(&many).len(), 100);
        let long = rich_text(&"y".repeat(4500));
        assert_eq!(long.as_array().map(Vec::len), Some(3));
    }

    #[tokio::test]
    async fn clickup_creates_tasks() {
        let ok = json!({ "id": "abc" }).to_string();
        let (url, seen) = mock_server(vec![(200, ok.clone()), (200, ok)]).await;
        let tasks = vec![Task { text: "Email Sam".into(), owner: Some("Alex".into()), due: None }, Task { text: "Book room".into(), ..Default::default() }, Task::default()];
        let n = clickup_create_tasks_at(&url, "pk_test", "901", "Launch plan", &tasks).await.expect("created");
        assert_eq!(n, 2);
        let reqs = seen.await.expect("server");
        assert!(reqs[0].to_lowercase().starts_with("post /list/901/task"));
        assert!(reqs[0].to_lowercase().contains("authorization: pk_test"));
        let body = body_json(&reqs[0]);
        assert_eq!(body["name"], "Email Sam");
        assert_eq!(body["description"], "From Navi notes: Launch plan\nOwner (as said): Alex");
        assert_eq!(body_json(&reqs[1])["description"], "From Navi notes: Launch plan");
    }

    #[tokio::test]
    async fn clickup_errors_are_readable() {
        let (url, _seen) = mock_server(vec![(401, json!({ "err": "Token invalid", "ECODE": "OAUTH_025" }).to_string())]).await;
        let tasks = vec![Task { text: "A".into(), ..Default::default() }, Task { text: "B".into(), ..Default::default() }];
        let e = clickup_create_tasks_at(&url, "bad", "901", "T", &tasks).await.expect_err("401").to_string();
        assert_eq!(e, "ClickUp rejected the token. Check it in Settings.");
        assert!(clickup_create_tasks_at(&url, "", "901", "T", &tasks).await.is_err());
        assert!(clickup_create_tasks_at(&url, "tok", "../x", "T", &tasks).await.is_err());
        assert_eq!(clickup_create_tasks_at(&url, "tok", "901", "T", &[]).await.expect("nothing to do"), 0);
    }

    #[tokio::test]
    async fn clickup_user_test() {
        let (url, _seen) = mock_server(vec![(200, json!({ "user": { "id": 1, "username": "Jane Doe" } }).to_string())]).await;
        assert_eq!(test_clickup_at(&url, "pk").await.expect("user"), "Jane Doe");
    }

    #[tokio::test]
    async fn notion_reads_title_property_then_creates_page() {
        let schema = json!({ "object": "database", "properties": { "Status": { "type": "select" }, "Task name": { "id": "title", "type": "title" } } }).to_string();
        let page = json!({ "object": "page", "id": "p1", "url": "https://www.notion.so/T-p1" }).to_string();
        let (url, seen) = mock_server(vec![(200, schema), (200, page)]).await;
        let notes = Notes { title: "Weekly plan".into(), summary: "Short.".into(), ..Default::default() };
        let page_url = notion_create_page_at(&url, "secret_x", "0123456789abcdef0123456789abcdef", &notes).await.expect("page");
        assert_eq!(page_url, "https://www.notion.so/T-p1");
        let reqs = seen.await.expect("server");
        let get = reqs[0].to_lowercase();
        assert!(get.starts_with("get /databases/0123456789abcdef0123456789abcdef"));
        assert!(get.contains("authorization: bearer secret_x"));
        assert!(get.contains("notion-version: 2022-06-28"));
        let body = body_json(&reqs[1]);
        assert_eq!(body["parent"]["database_id"], "0123456789abcdef0123456789abcdef");
        assert_eq!(body["properties"]["Task name"]["title"][0]["text"]["content"], "Weekly plan");
        assert_eq!(body["children"][0]["type"], "paragraph");
    }

    #[tokio::test]
    async fn notion_missing_database_message() {
        let (url, _seen) = mock_server(vec![(404, json!({ "object": "error", "code": "object_not_found", "message": "Could not find database" }).to_string())]).await;
        let e = notion_create_page_at(&url, "secret_x", "0123456789abcdef0123456789abcdef", &Notes::default()).await.expect_err("404").to_string();
        assert!(e.contains("add your Navi connection"), "{e}");
        assert!(notion_create_page_at(&url, "secret_x", "nope", &Notes::default()).await.is_err());
    }

    #[tokio::test]
    async fn notion_user_test() {
        let (url, _seen) = mock_server(vec![(200, json!({ "object": "user", "type": "bot", "name": "Navi" }).to_string())]).await;
        assert_eq!(test_notion_at(&url, "secret").await.expect("bot"), "Navi");
    }
}
