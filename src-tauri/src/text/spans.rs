//! Spans of text that the prose rules must leave alone: code, links, email addresses, file paths,
//! @mentions, hashtags and file names. Shared by the spelling, tidy, dash and dictionary passes.

use std::ops::Range;

/// Byte ranges of `text` holding code or machine text (sorted, may touch but never overlap).
pub(crate) fn protected_ranges(text: &str) -> Vec<Range<usize>> {
    let code = code_spans(text);
    let mut out: Vec<Range<usize>> = code.clone();
    for chunk in chunks(text) {
        if overlaps(&code, &chunk) {
            continue;
        }
        if is_machine_chunk(&text[chunk.clone()]) {
            out.push(chunk);
        }
    }
    out.sort_by_key(|r| r.start);
    out
}

/// Maximal runs of non-whitespace characters, as byte ranges.
pub(crate) fn chunks(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        match (c.is_whitespace(), start) {
            (false, None) => start = Some(i),
            (true, Some(s)) => {
                out.push(s..i);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push(s..text.len());
    }
    out
}

/// True for a whitespace-free chunk that is a link, email address, @mention, hashtag, path, file
/// name, version number or a piece of code rather than prose.
pub(crate) fn is_machine_chunk(chunk: &str) -> bool {
    if chunk.is_empty() {
        return false;
    }
    let lower = chunk.to_ascii_lowercase();
    if lower.contains("://") || lower.starts_with("www.") || lower.contains("(www.") {
        return true;
    }
    if chunk.contains('@') || chunk.contains('/') || chunk.contains('\\') || chunk.contains('`') {
        return true;
    }
    if chunk.starts_with('#') && chunk.chars().nth(1).is_some_and(|c| c.is_alphanumeric()) {
        return true;
    }
    if chunk.chars().any(|c| matches!(c, '=' | '<' | '>' | '{' | '}' | '$' | '|' | '^' | '~')) || chunk.contains("::") {
        return true;
    }
    // A dot directly followed by a letter or digit: colors.json, example.com, v1.2, main.rs.
    let chars: Vec<char> = chunk.chars().collect();
    chars.windows(2).any(|w| w[0] == '.' && w[1].is_alphanumeric())
}

/// Code spans: a run of backticks closed by the next run of the same length (```fences``` too).
fn code_spans(text: &str) -> Vec<Range<usize>> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'`' {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && b[i] == b'`' {
            i += 1;
        }
        let run = i - start;
        let mut j = i;
        let mut found = None;
        while j < b.len() {
            if b[j] == b'`' {
                let s = j;
                while j < b.len() && b[j] == b'`' {
                    j += 1;
                }
                if j - s == run {
                    found = Some(j);
                    break;
                }
            } else {
                j += 1;
            }
        }
        if let Some(end) = found {
            out.push(start..end);
            i = end;
        }
    }
    out
}

pub(crate) fn overlaps(ranges: &[Range<usize>], r: &Range<usize>) -> bool {
    ranges.iter().any(|p| p.start < r.end && r.start < p.end)
}

pub(crate) fn inside(ranges: &[Range<usize>], pos: usize) -> bool {
    ranges.iter().any(|p| p.start <= pos && pos < p.end)
}

/// Whole-word, ASCII-case-insensitive occurrences of `needle` in `hay`.
pub(crate) fn find_whole_ci(hay: &str, needle: &str) -> Vec<Range<usize>> {
    let needle = needle.trim();
    if needle.is_empty() {
        return Vec::new();
    }
    let h = hay.to_ascii_lowercase();
    let n = needle.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = h[from..].find(&n) {
        let start = from + rel;
        let end = start + n.len();
        let before_ok = hay[..start].chars().next_back().is_none_or(|c| !is_word_char(c));
        let after_ok = hay[end..].chars().next().is_none_or(|c| !is_word_char(c));
        if before_ok && after_ok {
            out.push(start..end);
        }
        from = start + n.chars().next().map_or(1, char::len_utf8);
        if from >= h.len() {
            break;
        }
    }
    out
}

pub(crate) fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn protected_texts(t: &str) -> Vec<&str> {
        protected_ranges(t).into_iter().map(|r| &t[r]).collect()
    }

    #[test]
    fn finds_machine_text() {
        let t = "see https://example.com/color and alex@colors.example, @colorbot, #color, src/color.ts, colors.json and `color` ok";
        let p = protected_texts(t);
        assert!(p.contains(&"https://example.com/color"));
        assert!(p.contains(&"alex@colors.example,"));
        assert!(p.contains(&"@colorbot,"));
        assert!(p.contains(&"#color,"));
        assert!(p.contains(&"src/color.ts,"));
        assert!(p.contains(&"colors.json"));
        assert!(p.contains(&"`color`"));
        assert!(!p.contains(&"and"));
    }

    #[test]
    fn fenced_code_spans_cover_spaces() {
        let t = "before ```let color = 1;\nlet x = 2;``` after";
        let p = protected_texts(t);
        assert_eq!(p, vec!["```let color = 1;\nlet x = 2;```"]);
    }

    #[test]
    fn unclosed_backtick_is_not_code() {
        assert!(protected_texts("it's a ` stray tick").iter().all(|s| s.contains('`')));
    }

    #[test]
    fn whole_word_search() {
        let r = find_whole_ci("Navi and navigate and NAVI.", "navi");
        assert_eq!(r.len(), 2);
    }
}
