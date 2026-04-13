use chrono::NaiveDate;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

use crate::error::{Error, Result};
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
pub fn split_frontmatter(content: &str) -> (Option<&str>, &str) {
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
        yaml_serde::from_str(yaml).map_err(|e| Error::Yaml {
            path: path.to_path_buf(),
            source: e,
        })?
    } else {
        RawFrontmatter::default()
    };

    let title = raw.title.unwrap_or_else(|| extract_h1(body, path));

    let node = Node {
        id: raw.id.unwrap_or_default(), // empty = needs inference
        path: path.to_path_buf(),
        title,
        kind: Kind::new(raw.kind.unwrap_or_default()), // empty = needs inference
        status: Status::new(raw.status.unwrap_or_else(|| "active".to_string())),
        created: raw.created,
        updated: raw.updated,
        reviewed: raw.reviewed,
        owner: raw.owner,
        supersedes: raw.supersedes.map(|s| s.into_vec()).unwrap_or_default(),
        superseded_by: raw.superseded_by,
        implements: raw.implements.map(|s| s.into_vec()).unwrap_or_default(),
        related: raw.related.map(|s| s.into_vec()).unwrap_or_default(),
        tags: raw.tags.map(|s| s.into_vec()).unwrap_or_default(),
        orphan_ok: raw.orphan_ok.unwrap_or(false),
        attrs: raw.extra,
    };

    Ok((node, body.to_string()))
}

/// Extract the first H1 heading from markdown body as a fallback title.
fn extract_h1(body: &str, path: &Path) -> String {
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix("# ") {
            return heading.trim().to_string();
        }
    }
    // Last resort: use filename stem
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
    fn parse_missing_fields_uses_defaults() {
        let content = "---\ntitle: Minimal\n---\nBody";
        let path = Path::new("readme.md");
        let (node, _) = parse_frontmatter(path, content).unwrap();
        assert_eq!(node.id, ""); // needs inference
        assert_eq!(node.kind.as_str(), ""); // needs inference
        assert_eq!(node.status.as_str(), "active");
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
}
