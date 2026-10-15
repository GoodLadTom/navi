//! Prompt mode for coding agents: which project you are in, and which files you probably mean.
//!
//! OWNER: text agent.
//!
//! Contract:
//! * `detect_project`: match the window title against the configured projects (folder name or
//!   project name appears in the title, case-insensitive; VS Code titles look like
//!   "file.ts - navi - Visual Studio Code"); else the active project; else None.
//! * `index_files`: relative paths (forward slashes) under `root`, honouring .gitignore and skipping
//!   node_modules, target, dist, build, .git, binaries and files over 1 MB; stop at `max`.
//! * `candidates`: rank files by how well their path parts match the nouns in the utterance
//!   ("the header component" -> src/components/Header.tsx), using token overlap plus fuzzy matching
//!   (strsim), best first, at most `max`.
//!
//! Details:
//! * Title matches are whole words ("navi" never matches "navigation.ts"); when several projects
//!   match, the longest name wins.
//! * `index_files` also skips hidden files and folders (.env, .next, .venv), common build and
//!   dependency folders (out, coverage, vendor, __pycache__ and similar) and walks in sorted order.
//!   A .gitignore is honoured even when the folder is not a git repository.
//! * `candidates` only returns files that actually match something in the utterance, so it can be
//!   empty. File-name matches count more than folder matches; "app dot ts" finds src/app.ts.

use crate::text::spans;
use crate::types::{AgentSettings, AppContext, ProjectRef};
use std::cmp::Ordering;
use std::path::Path;

pub fn detect_project(ctx: &AppContext, settings: &AgentSettings) -> Option<ProjectRef> {
    let title = ctx.window_title.trim();
    if !title.is_empty() {
        let mut best: Option<(usize, &ProjectRef)> = None;
        for p in &settings.projects {
            for name in project_names(p) {
                let len = name.chars().count();
                if len >= 2 && !spans::find_whole_ci(title, &name).is_empty() && best.is_none_or(|(l, _)| len > l) {
                    best = Some((len, p));
                }
            }
        }
        if let Some((_, p)) = best {
            return Some(p.clone());
        }
    }
    settings.active_project.as_ref().and_then(|id| settings.projects.iter().find(|p| &p.id == id).cloned())
}

fn project_names(p: &ProjectRef) -> Vec<String> {
    let mut names = Vec::new();
    if !p.name.trim().is_empty() {
        names.push(p.name.trim().to_string());
    }
    let path = p.path.trim().trim_end_matches(['/', '\\']);
    if let Some(folder) = path.rsplit(['/', '\\']).next().filter(|f| !f.is_empty() && !f.ends_with(':')) {
        names.push(folder.to_string());
    }
    names
}

const SKIP_DIRS: &[&str] = &[
    "node_modules", "target", "dist", "build", "out", ".git", ".next", ".nuxt", ".svelte-kit", ".turbo", ".cache",
    ".parcel-cache", "coverage", "__pycache__", ".venv", "venv", "vendor", "bin", "obj", ".idea", ".gradle", "pods",
    "deriveddata", ".pytest_cache", ".mypy_cache", ".tox", "bower_components", "jspm_packages", ".terraform",
    ".serverless",
];

const BINARY_EXT: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "ico", "icns", "bmp", "tif", "tiff", "psd", "ai", "sketch", "fig", "heic", "avif",
    "mp3", "wav", "ogg", "flac", "m4a", "aac", "mp4", "mov", "avi", "mkv", "webm", "zip", "gz", "tgz", "7z", "rar", "tar",
    "bz2", "xz", "exe", "dll", "so", "dylib", "lib", "a", "o", "obj", "pdb", "bin", "dat", "db", "sqlite", "sqlite3",
    "class", "jar", "war", "pyc", "pyo", "woff", "woff2", "ttf", "otf", "eot", "pdf", "doc", "docx", "xls", "xlsx",
    "ppt", "pptx", "iso", "dmg", "msi", "apk", "ipa", "wasm", "node", "lockb", "onnx", "gguf", "safetensors", "pt",
    "ckpt", "h5", "npy", "npz", "parquet", "blend", "fbx", "glb", "stl", "prproj", "aep",
];

const MAX_FILE_BYTES: u64 = 1024 * 1024;

pub fn index_files(root: &Path, max: usize) -> Vec<String> {
    if max == 0 || !root.is_dir() {
        return Vec::new();
    }
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .ignore(true)
        .require_git(false)
        .follow_links(false)
        .max_depth(Some(16))
        .sort_by_file_name(|a, b| a.cmp(b))
        .filter_entry(|e| {
            let is_dir = e.file_type().is_some_and(|t| t.is_dir());
            !(is_dir && e.depth() > 0 && SKIP_DIRS.contains(&e.file_name().to_string_lossy().to_lowercase().as_str()))
        });
    let mut out = Vec::new();
    for entry in builder.build() {
        let Ok(entry) = entry else {
            continue;
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let binary = path.extension().map(|e| e.to_string_lossy().to_lowercase()).is_some_and(|e| BINARY_EXT.contains(&e.as_str()));
        if binary || entry.metadata().map_or(true, |m| m.len() > MAX_FILE_BYTES) {
            continue;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if rel.is_empty() {
            continue;
        }
        out.push(rel);
        if out.len() >= max {
            break;
        }
    }
    out
}

const STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "but", "in", "on", "at", "to", "for", "of", "with", "from", "into", "onto", "by",
    "about", "as", "is", "are", "was", "were", "be", "been", "being", "it", "its", "it's", "this", "that", "these",
    "those", "there", "here", "my", "our", "your", "their", "his", "her", "we", "i", "you", "they", "he", "she", "me",
    "us", "them", "can", "could", "would", "should", "will", "shall", "please", "just", "also", "so", "then", "now",
    "make", "makes", "making", "change", "changes", "changing", "fix", "fixes", "fixing", "update", "updates",
    "updating", "add", "adds", "adding", "remove", "removes", "removing", "delete", "create", "rename", "move",
    "refactor", "write", "rewrite", "check", "look", "looks", "open", "use", "using", "need", "needs", "want",
    "wants", "like", "get", "gets", "set", "put", "take", "let", "do", "does", "did", "done", "have", "has", "had",
    "all", "any", "some", "more", "less", "new", "old", "sure", "way", "thing", "things", "stuff", "bit", "little",
    "bug", "bugs", "issue", "issues", "problem", "problems", "file", "files", "folder", "folders", "directory", "code",
    "method", "line", "lines", "dot", "slash", "which", "what", "when", "where", "why", "how", "not", "no", "yes",
    "out", "up", "down", "over", "under", "again", "still", "too", "very", "really", "one", "two", "first", "last",
    "next", "same", "other", "don't", "doesn't", "isn't", "can't", "won't", "instead", "because", "if", "else",
    "than", "there's", "that's", "what's", "let's", "okay", "ok", "right", "currently", "should", "properly",
    "working", "work", "works", "broken", "wrong", "correct", "correctly", "so", "is", "go", "goes", "going", "see",
];

pub fn candidates(utterance: &str, files: &[String], max: usize) -> Vec<String> {
    if max == 0 || files.is_empty() {
        return Vec::new();
    }
    let query = query_tokens(utterance);
    if query.is_empty() {
        return Vec::new();
    }
    let joined: Vec<String> = query.windows(2).map(|w| format!("{}{}", w[0], w[1])).collect();
    let mut scored: Vec<(f64, &String)> = files
        .iter()
        .filter_map(|f| {
            let s = score(f, &query, &joined);
            (s >= THRESHOLD).then_some((s, f))
        })
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal).then_with(|| a.1.len().cmp(&b.1.len())).then_with(|| a.1.cmp(b.1)));
    scored.into_iter().take(max).map(|(_, f)| f.clone()).collect()
}

/// A single exact file-name match passes; a single folder match does not.
const THRESHOLD: f64 = 0.75;

/// "@src/app.ts" or "src/app.ts" depending on the mention style.
pub fn mention(path: &str, style: &str) -> String {
    if style == "plain" { path.to_string() } else { format!("@{path}") }
}

fn query_tokens(utterance: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in utterance.split(|c: char| !c.is_alphanumeric() && c != '\'') {
        for part in split_camel(raw.trim_matches('\'')) {
            let t = part.to_lowercase();
            if t.chars().count() < 2 || STOPWORDS.contains(&t.as_str()) || out.contains(&t) {
                continue;
            }
            out.push(t);
        }
    }
    out
}

/// "HeaderNav" -> ["Header", "Nav"], "parseJSONFile" -> ["parse", "JSON", "File"].
fn split_camel(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        let boundary = i > 0
            && c.is_uppercase()
            && (chars[i - 1].is_lowercase()
                || chars[i - 1].is_ascii_digit()
                || (chars[i - 1].is_uppercase() && chars.get(i + 1).is_some_and(|n| n.is_lowercase())));
        if boundary && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

struct PathParts {
    dirs: Vec<String>,
    base: Vec<String>,
    ext: Option<String>,
    /// File name without extension, lower case, separators removed: "user_profile" -> "userprofile".
    stem: String,
    depth: usize,
}

fn path_parts(path: &str) -> PathParts {
    let segments: Vec<&str> = path.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    let file = segments.last().copied().unwrap_or("");
    let (stem_raw, ext) = match file.rfind('.') {
        Some(i) if i > 0 => (&file[..i], Some(file[i + 1..].to_lowercase())),
        _ => (file, None),
    };
    let words = |s: &str| -> Vec<String> {
        s.split(|c: char| !c.is_alphanumeric()).flat_map(split_camel).map(|w| w.to_lowercase()).filter(|w| !w.is_empty()).collect()
    };
    let dirs = segments.iter().take(segments.len().saturating_sub(1)).flat_map(|s| words(s)).collect();
    PathParts { dirs, base: words(stem_raw), ext, stem: stem_raw.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase(), depth: segments.len() }
}

fn token_sim(q: &str, p: &str) -> f64 {
    if q == p {
        return 1.0;
    }
    if format!("{q}s") == p || format!("{p}s") == q || format!("{q}es") == p || format!("{p}es") == q {
        return 0.9;
    }
    let (ql, pl) = (q.chars().count(), p.chars().count());
    if ql >= 3 && p.starts_with(q) {
        return 0.8;
    }
    if pl >= 3 && q.starts_with(p) {
        return 0.75;
    }
    if ql >= 4 && pl >= 4 {
        let jw = strsim::jaro_winkler(q, p);
        if jw >= 0.88 {
            return jw * 0.85;
        }
    }
    0.0
}

fn score(path: &str, query: &[String], joined: &[String]) -> f64 {
    let parts = path_parts(path);
    let mut total = 0.0;
    let mut base_matched = vec![false; parts.base.len()];
    for q in query {
        let mut best = 0.0_f64;
        for (i, p) in parts.base.iter().enumerate() {
            let s = token_sim(q, p);
            if s >= 0.75 {
                base_matched[i] = true;
            }
            best = best.max(s);
        }
        for p in &parts.dirs {
            best = best.max(token_sim(q, p) * 0.6);
        }
        if parts.ext.as_deref() == Some(q.as_str()) {
            best = best.max(0.5);
        }
        total += best;
    }
    if joined.contains(&parts.stem) {
        total += 1.0;
    }
    if !parts.base.is_empty() && total > 0.0 {
        let covered = base_matched.iter().filter(|m| **m).count() as f64 / parts.base.len() as f64;
        total += 0.3 * covered;
    }
    total - 0.01 * parts.depth as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn project(id: &str, name: &str, path: &str) -> ProjectRef {
        ProjectRef { id: id.into(), name: name.into(), path: path.into() }
    }

    fn title(t: &str) -> AppContext {
        AppContext { window_title: t.into(), ..Default::default() }
    }

    #[test]
    fn detects_project_from_title() {
        let settings = AgentSettings {
            projects: vec![
                project("1", "Navi", "D:\\Code\\Navi"),
                project("2", "Website", "D:\\Sites\\acme-site\\"),
                project("3", "Navi Docs", "D:\\Docs\\navi-docs"),
            ],
            active_project: None,
            mention_style: "at".into(),
        };
        let id = |t: &str| detect_project(&title(t), &settings).map(|p| p.id);
        assert_eq!(id("pipeline.rs - Navi - Visual Studio Code"), Some("1".into()));
        assert_eq!(id("index.html - acme-site - Cursor"), Some("2".into()));
        assert_eq!(id("C:\\Users\\you\\Projects\\Navi"), Some("1".into()));
        assert_eq!(id("intro.md - Navi Docs - Visual Studio Code"), Some("3".into()));
        assert_eq!(id("navigation.ts - other - Visual Studio Code"), None);
        let with_active = AgentSettings { active_project: Some("2".into()), ..settings.clone() };
        assert_eq!(detect_project(&title("navigation.ts - other"), &with_active).map(|p| p.id), Some("2".into()));
        assert_eq!(detect_project(&title(""), &with_active).map(|p| p.id), Some("2".into()));
    }

    struct TempTree(PathBuf);

    impl TempTree {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("navi-agent-{name}-{}-{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
            fs::create_dir_all(&root).expect("temp root");
            TempTree(root)
        }
        fn file(&self, rel: &str, contents: &[u8]) {
            let p = self.0.join(rel);
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent).expect("parent");
            }
            fs::write(p, contents).expect("write");
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn indexes_a_project_tree() {
        let t = TempTree::new("index");
        t.file("src/components/Header.tsx", b"export {}");
        t.file("src/components/Footer.tsx", b"export {}");
        t.file("src/app.ts", b"export {}");
        t.file("README.md", b"# hi");
        t.file("node_modules/react/index.js", b"x");
        t.file("target/debug/navi.d", b"x");
        t.file("dist/bundle.js", b"x");
        t.file("build/out.js", b"x");
        t.file(".git/config", b"x");
        t.file(".env", b"SECRET=1");
        t.file("assets/logo.png", b"\x89PNG");
        t.file("big.txt", &vec![b'a'; 1_100_000]);
        t.file("debug.log", b"x");
        t.file("notes/keep.md", b"x");
        t.file(".gitignore", b"*.log\nnotes/\n");
        let mut files = index_files(&t.0, 100);
        files.sort();
        assert_eq!(files, vec!["README.md", "src/app.ts", "src/components/Footer.tsx", "src/components/Header.tsx"]);
        assert_eq!(index_files(&t.0, 2).len(), 2);
        assert!(index_files(&t.0.join("missing"), 10).is_empty());
    }

    fn files() -> Vec<String> {
        [
            "src/components/Header.tsx",
            "src/components/HeaderNav.tsx",
            "src/components/Footer.tsx",
            "src/pages/UserProfile.tsx",
            "src/app.ts",
            "src/lib/auth_service.rs",
            "README.md",
            "docs/setup-guide.md",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    #[test]
    fn ranks_candidates() {
        let f = files();
        assert_eq!(candidates("fix the header component", &f, 3).first().map(String::as_str), Some("src/components/Header.tsx"));
        assert_eq!(candidates("open app dot ts", &f, 3).first().map(String::as_str), Some("src/app.ts"));
        assert_eq!(candidates("the user profile page is slow", &f, 3).first().map(String::as_str), Some("src/pages/UserProfile.tsx"));
        assert_eq!(candidates("the auth service", &f, 3).first().map(String::as_str), Some("src/lib/auth_service.rs"));
        assert_eq!(candidates("update the setup guide", &f, 3).first().map(String::as_str), Some("docs/setup-guide.md"));
        assert_eq!(candidates("the hedder looks off", &f, 3).first().map(String::as_str), Some("src/components/Header.tsx"));
        assert_eq!(candidates("the HeaderNav spacing", &f, 1), vec!["src/components/HeaderNav.tsx".to_string()]);
    }

    #[test]
    fn candidates_only_return_real_matches() {
        let f = files();
        assert!(candidates("make the wording friendlier", &f, 5).is_empty());
        assert!(candidates("", &f, 5).is_empty());
        assert!(candidates("header", &[], 5).is_empty());
        assert!(candidates("header", &f, 0).is_empty());
        assert!(candidates("components", &f, 10).len() <= 3);
    }

    #[test]
    fn mention_styles() {
        assert_eq!(mention("src/app.ts", "at"), "@src/app.ts");
        assert_eq!(mention("src/app.ts", "plain"), "src/app.ts");
    }

    #[test]
    fn camel_splitting() {
        assert_eq!(split_camel("HeaderNav"), vec!["Header", "Nav"]);
        assert_eq!(split_camel("parseJSONFile"), vec!["parse", "JSON", "File"]);
        assert_eq!(split_camel("user"), vec!["user"]);
    }
}
