use chrono::NaiveDate;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

use crate::error::{Error, ParseError, Result};
use crate::model::{Kind, Node, Status};

/// Raw frontmatter fields — flat deserialization target.
#[derive(Debug, Default, Deserialize)]
struct RawFrontmatter {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    created: Option<NaiveDate>,
    #[serde(default)]
    updated: Option<NaiveDate>,
    #[serde(default)]
    reviewed: Option<NaiveDate>,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    supersedes: Option<StringOrVec>,
    #[serde(default)]
    superseded_by: Option<String>,
    #[serde(default)]
    implements: Option<StringOrVec>,
    #[serde(default)]
    related: Option<StringOrVec>,
    #[serde(default)]
    tags: Option<StringOrVec>,
    #[serde(default)]
    covers: Option<StringOrVec>,
    #[serde(default)]
    orphan_ok: Option<bool>,

    /// Catch-all for project-specific fields.
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

/// Accepts both `"single"` and `["a", "b"]` in YAML.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum StringOrVec {
    Single(String),
    Multiple(Vec<String>),
}

impl StringOrVec {
    fn into_vec(self) -> Vec<String> {
        match self {
            Self::Single(s) => vec![s],
            Self::Multiple(v) => v,
        }
    }
}

/// Split a document into frontmatter YAML and body text.
/// Returns `(yaml_str, body_str)`. Returns `(None, full_content)` if no frontmatter.
///
/// A leading UTF-8 BOM (U+FEFF) is stripped before the `---` check so
/// files authored by Windows editors — which often write a BOM — parse
/// correctly instead of silently falling through to "no frontmatter".
pub fn split_frontmatter(content: &str) -> (Option<&str>, &str) {
    let content = content.strip_prefix('\u{FEFF}').unwrap_or(content);
    if !content.starts_with("---") {
        return (None, content);
    }

    // Find the closing `---` after the opening one.
    let after_open = &content[3..];
    // Skip optional whitespace + newline after opening ---
    let body_start = if after_open.starts_with('\n') {
        4 // "---\n"
    } else if after_open.starts_with("\r\n") {
        5
    } else {
        return (None, content);
    };

    let rest = &content[body_start..];
    if let Some(close_pos) = rest.find("\n---") {
        let yaml = &rest[..close_pos];
        let after_close = &rest[close_pos + 4..]; // skip "\n---"
        // Skip newline after closing ---
        let body = if let Some(stripped) = after_close.strip_prefix('\n') {
            stripped
        } else if let Some(stripped) = after_close.strip_prefix("\r\n") {
            stripped
        } else {
            after_close
        };
        (Some(yaml), body)
    } else {
        (None, content)
    }
}

/// Parse frontmatter YAML into a partial Node (id/kind may need inference).
/// Returns `(Node, body_text)`.
pub fn parse_frontmatter(path: &Path, content: &str) -> Result<(Node, String)> {
    let (yaml_opt, body) = split_frontmatter(content);

    let raw: RawFrontmatter = if let Some(yaml) = yaml_opt {
        yaml_serde::from_str(yaml).map_err(|e| Error::Parse {
            path: path.to_path_buf(),
            source: ParseError::Yaml(e),
        })?
    } else {
        RawFrontmatter::default()
    };

    let title = raw.title.unwrap_or_else(|| extract_h1(body, path));

    // Compute body fingerprints once, at the only place that owns the
    // body string. `body_hash` powers `body_immutable.frozen`;
    // `body_lines_hash` powers `body_immutable.append_only` (prefix
    // equality of the per-line vector). Stored on the node so rules
    // stay pure functions of `(graph, config)`.
    let body_hash = crate::hash::sha256_hex(body);
    let body_lines_hash: Vec<String> = body.lines().map(crate::hash::sha256_hex).collect();

    let node = Node {
        id: raw.id.unwrap_or_default(), // empty = needs inference
        path: path.to_path_buf(),
        title,
        kind: Kind::new(raw.kind.unwrap_or_default()), // empty = needs inference
        status: Status::new(raw.status.unwrap_or_default()), // empty = needs inference
        created: raw.created,
        updated: raw.updated,
        reviewed: raw.reviewed,
        owner: raw.owner,
        supersedes: raw.supersedes.map(|s| s.into_vec()).unwrap_or_default(),
        superseded_by: raw.superseded_by,
        implements: raw.implements.map(|s| s.into_vec()).unwrap_or_default(),
        related: raw.related.map(|s| s.into_vec()).unwrap_or_default(),
        tags: raw.tags.map(|s| s.into_vec()).unwrap_or_default(),
        covers: raw.covers.map(|s| s.into_vec()).unwrap_or_default(),
        orphan_ok: raw.orphan_ok.unwrap_or(false),
        attrs: raw.extra,
        body_hash,
        body_lines_hash,
    };

    Ok((node, body.to_string()))
}

/// Extract the first H1 heading via pulldown-cmark, concatenating its
/// text + inline-code children. Falls back to the filename stem when
/// no H1 exists. Re-exported by `parser` module for callers (migrate)
/// that need title inference without a full document parse.
pub fn extract_h1(body: &str, path: &Path) -> String {
    use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};

    let mut depth: u32 = 0;
    let mut buf = String::new();
    for event in Parser::new(body) {
        match event {
            Event::Start(Tag::Heading {
                level: HeadingLevel::H1,
                ..
            }) => {
                depth += 1;
            }
            Event::End(TagEnd::Heading(HeadingLevel::H1)) if depth > 0 => {
                let trimmed = buf.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
                depth = 0;
                buf.clear();
            }
            Event::Text(t) | Event::Code(t) if depth > 0 => buf.push_str(&t),
            _ => {}
        }
    }
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("untitled")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_basic_frontmatter() {
        let content = "---\ntitle: Hello\n---\nBody text";
        let (yaml, body) = split_frontmatter(content);
        assert_eq!(yaml, Some("title: Hello"));
        assert_eq!(body, "Body text");
    }

    #[test]
    fn split_no_frontmatter() {
        let content = "Just body text";
        let (yaml, body) = split_frontmatter(content);
        assert!(yaml.is_none());
        assert_eq!(body, "Just body text");
    }

    #[test]
    fn parse_full_frontmatter() {
        let content = "---\nid: test-doc\ntitle: Test\nkind: guide\nstatus: active\ncreated: 2026-01-01\ntags:\n  - foo\n  - bar\n---\n# Heading\n\nBody";
        let path = Path::new("docs/test.md");
        let (node, body) = parse_frontmatter(path, content).unwrap();
        assert_eq!(node.id, "test-doc");
        assert_eq!(node.title, "Test");
        assert_eq!(node.kind.as_str(), "guide");
        assert_eq!(node.tags, vec!["foo", "bar"]);
        assert!(body.contains("Body"));
    }

    #[test]
    fn parse_missing_fields_leaves_blanks_for_inference() {
        let content = "---\ntitle: Minimal\n---\nBody";
        let path = Path::new("readme.md");
        let (node, _) = parse_frontmatter(path, content).unwrap();
        assert_eq!(node.id, "");
        assert_eq!(node.kind.as_str(), "");
        assert_eq!(node.status.as_str(), "");
    }

    #[test]
    fn extract_h1_handles_setext_and_inline() {
        let content = "---\nid: x\n---\nMy `Title`\n=========\n\nBody";
        let path = Path::new("doc.md");
        let (node, _) = parse_frontmatter(path, content).unwrap();
        assert_eq!(node.title, "My Title");
    }

    #[test]
    fn title_fallback_to_h1() {
        let content = "# My Document\n\nSome text";
        let path = Path::new("doc.md");
        let (node, _) = parse_frontmatter(path, content).unwrap();
        assert_eq!(node.title, "My Document");
    }

    #[test]
    fn string_or_vec_single() {
        let content = "---\ntitle: T\nsupersedes: old-doc\n---\n";
        let path = Path::new("doc.md");
        let (node, _) = parse_frontmatter(path, content).unwrap();
        assert_eq!(node.supersedes, vec!["old-doc"]);
    }

    // ─── body fingerprint ──────────────────────────────────────────────
    //
    // The body's whole-document hash and per-line hash vector are the
    // primitive `body_immutable` consumes. These tests pin the
    // invariants the rule depends on: same body → same hash, different
    // body → different hash, line vector length matches `body.lines()`,
    // and identical line content produces byte-identical hash entries
    // (so prefix equality is well-defined for the append-only mode).

    #[test]
    fn body_hash_changes_with_body_text() {
        let path = Path::new("doc.md");
        let (a, _) = parse_frontmatter(path, "---\ntitle: T\n---\nfirst").unwrap();
        let (b, _) = parse_frontmatter(path, "---\ntitle: T\n---\nsecond").unwrap();
        assert_ne!(
            a.body_hash, b.body_hash,
            "different bodies must produce different hashes"
        );
    }

    #[test]
    fn body_hash_stable_for_identical_body() {
        let path = Path::new("doc.md");
        let (a, _) = parse_frontmatter(path, "---\ntitle: T\n---\nbody").unwrap();
        let (b, _) = parse_frontmatter(path, "---\ntitle: T\n---\nbody").unwrap();
        assert_eq!(
            a.body_hash, b.body_hash,
            "identical bodies must produce identical hashes"
        );
        assert_eq!(a.body_lines_hash, b.body_lines_hash);
    }

    #[test]
    fn body_lines_hash_matches_body_lines_iter() {
        // The per-line vector is what `append_only` mode compares
        // prefix-wise. Its length must equal `body.lines()` so a
        // reviewer reasoning about "the first N lines" can map
        // 1:1 between the source and the fingerprint.
        let path = Path::new("doc.md");
        let body = "alpha\nbeta\ngamma";
        let (n, _) = parse_frontmatter(path, &format!("---\nid: x\n---\n{body}")).unwrap();
        assert_eq!(n.body_lines_hash.len(), 3);
        // Identical lines produce identical hashes — append-only
        // prefix comparison relies on this byte-level stability.
        let (m, _) = parse_frontmatter(path, &format!("---\nid: x\n---\n{body}\nnew")).unwrap();
        assert_eq!(m.body_lines_hash[..3], n.body_lines_hash[..]);
        assert_eq!(m.body_lines_hash.len(), 4);
    }

    #[test]
    fn body_hash_for_empty_body_is_sha256_of_empty_string() {
        // A document with no body still gets a well-defined hash
        // (SHA-256 of the empty string). This means the body_immutable
        // rule treats "frontmatter-only doc" as a real state, never
        // a special case.
        let path = Path::new("doc.md");
        let (n, _) = parse_frontmatter(path, "---\nid: x\n---\n").unwrap();
        assert_eq!(
            n.body_hash,
            crate::hash::sha256_hex(""),
            "empty body must hash to SHA-256(\"\")"
        );
        assert!(n.body_lines_hash.is_empty());
    }
}
