//! Minimal-diff editor for top-level YAML frontmatter scalars.
//!
//! Only touches lines that match a key being changed. The user's key
//! order, comments, blank lines, and quoting style are preserved
//! verbatim everywhere else — a one-field lifecycle transition
//! produces a one-line diff.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::Path;

use crate::error::{Error, ParseError, Result};
use crate::yaml_text;

/// Result of looking up a top-level scalar by key.
///
/// The three states are distinguished because callers (lifecycle,
/// migrate) react differently to each: `Absent` is a fresh-document
/// case, `Value` is the happy path, `NonScalar` is an authoring error
/// the editor cannot safely reason about. The value is a [`Cow`]
/// because quoted scalars decode their escapes — an escape-free value
/// still borrows from the underlying line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scalar<'a> {
    Absent,
    Value(Cow<'a, str>),
    NonScalar,
}

/// Editable view of a frontmatter YAML block. Construct with [`parse`],
/// query with [`scalar`], mutate with [`set`], serialise with [`render`].
#[derive(Debug)]
pub struct FrontmatterEditor {
    lines: Vec<String>,
    key_index: BTreeMap<String, usize>,
}

impl FrontmatterEditor {
    /// Parse a frontmatter YAML block (without the surrounding `---`
    /// delimiters). Rejects duplicate top-level keys — they make
    /// "update one scalar" semantics undefined.
    pub fn parse(yaml: &str, path: &Path) -> Result<Self> {
        let lines: Vec<String> = yaml.lines().map(String::from).collect();
        let mut key_index = BTreeMap::new();
        for (i, line) in lines.iter().enumerate() {
            if let Some(key) = yaml_text::parse_scalar_key(line)
                && key_index.insert(key.to_string(), i).is_some()
            {
                return Err(Error::Parse {
                    path: path.to_path_buf(),
                    source: ParseError::InvalidField {
                        field: key.to_string(),
                        expected: "exactly one declaration",
                    },
                });
            }
        }
        Ok(Self { lines, key_index })
    }

    /// Look up a top-level scalar, distinguishing absent from
    /// present-but-non-scalar so callers can react to each case
    /// without a second probe. A key whose value is a block-style
    /// collection (`key:` followed by indented `-` / `key:` children)
    /// is `NonScalar` — symmetric with the flow forms (`[a]` / `{a}`)
    /// `parse_scalar_value` already rejects — so an editing caller
    /// refuses it instead of overwriting only the key line and
    /// orphaning the children into invalid YAML.
    pub fn scalar(&self, key: &str) -> Scalar<'_> {
        match self.key_index.get(key) {
            None => Scalar::Absent,
            Some(&idx) => match yaml_text::parse_scalar_value(&self.lines[idx]) {
                Some(v) if v.is_empty() && self.find_block_end(idx) > idx + 1 => Scalar::NonScalar,
                Some(v) => Scalar::Value(v),
                None => Scalar::NonScalar,
            },
        }
    }

    /// Set a top-level scalar value. In-place when the key exists,
    /// appended at the end otherwise. Always written as a quoted
    /// string scalar. Replacing a key that held a block-style
    /// collection removes the whole block (not just the `key:` line),
    /// so the result is never orphaned child lines.
    pub fn set(&mut self, key: &str, value: &str) {
        let new_line = yaml_text::render_scalar_line(key, value);
        if let Some(&idx) = self.key_index.get(key) {
            let end = self.find_block_end(idx);
            if end == idx + 1 {
                self.lines[idx] = new_line;
            } else {
                let _ = self.lines.splice(idx..end, [new_line]);
                self.rebuild_key_index();
            }
        } else {
            self.lines.push(new_line);
            self.key_index.insert(key.to_string(), self.lines.len() - 1);
        }
    }

    /// Replace (or insert) a top-level YAML list under `key`. Items
    /// are written in canonical block style (`key:` followed by
    /// indented `- "item"` lines, all values quoted). An existing
    /// block — flow `[a, b]` or block-style — is removed entirely
    /// before the new block is written. An empty `items` slice removes
    /// the key altogether so the rendered output stays clean.
    pub fn set_list(&mut self, key: &str, items: &[&str]) {
        let block = if items.is_empty() {
            Vec::new()
        } else {
            let mut out = Vec::with_capacity(items.len() + 1);
            out.push(format!("{key}:"));
            for item in items {
                out.push(format!("  - {}", yaml_text::quote(item)));
            }
            out
        };

        if let Some(&idx) = self.key_index.get(key) {
            let end = self.find_block_end(idx);
            let _ = self.lines.splice(idx..end, block);
        } else if !block.is_empty() {
            self.lines.extend(block);
        }
        self.rebuild_key_index();
    }

    /// First line index *after* the value of the key at `start`.
    /// Handles three shapes: scalar `key: value`, inline list
    /// `key: [a, b]`, and block list `key:\n  - a\n  - b`.
    fn find_block_end(&self, start: usize) -> usize {
        let line = &self.lines[start];
        let colon = match line.find(':') {
            Some(c) => c,
            None => return start + 1,
        };
        let value = line[colon + 1..].trim_start();
        if !value.is_empty() {
            // Scalar or inline collection — block is just this line.
            return start + 1;
        }
        // Block-style: consume subsequent indented `-` items until a
        // top-level key, comment that starts at column 0, or EOF.
        let mut end = start + 1;
        while end < self.lines.len() {
            let l = &self.lines[end];
            if l.is_empty() {
                end += 1;
                continue;
            }
            let first = l.chars().next().unwrap();
            if first.is_whitespace() || first == '-' {
                end += 1;
            } else {
                break;
            }
        }
        end
    }

    fn rebuild_key_index(&mut self) {
        self.key_index.clear();
        for (i, line) in self.lines.iter().enumerate() {
            if let Some(key) = yaml_text::parse_scalar_key(line) {
                self.key_index.insert(key.to_string(), i);
            }
        }
    }

    /// Serialise as a YAML block without the `---` delimiters. The
    /// trailing newline lets callers concatenate with `---\n` cleanly.
    pub fn render(&self) -> String {
        let mut out = self.lines.join("\n");
        out.push('\n');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(yaml: &str) -> FrontmatterEditor {
        FrontmatterEditor::parse(yaml, Path::new("test.md")).unwrap()
    }

    #[test]
    fn scalar_distinguishes_absent_value_and_non_scalar() {
        let e = editor("id: foo\ntitle: \"Hello\"\ntags: [a, b]\n");
        assert_eq!(e.scalar("id"), Scalar::Value("foo".into()));
        assert_eq!(e.scalar("title"), Scalar::Value("Hello".into()));
        assert_eq!(e.scalar("tags"), Scalar::NonScalar);
        assert_eq!(e.scalar("missing"), Scalar::Absent);
    }

    #[test]
    fn scalar_treats_block_collection_as_non_scalar() {
        // A key whose value is a block list / map is NOT a scalar —
        // symmetric with the flow forms (`[a]` / `{a}`). Reporting it
        // as `Value("")` would let an editing caller overwrite only the
        // `key:` line and orphan the indented children into invalid YAML.
        let e = editor("id:\n  - weird\ntitle: A\nattrs:\n  nested: yes\nstatus: active\n");
        assert_eq!(e.scalar("id"), Scalar::NonScalar);
        assert_eq!(e.scalar("attrs"), Scalar::NonScalar);
        assert_eq!(e.scalar("status"), Scalar::Value("active".into()));
        // A genuinely empty scalar (no block children) stays a value.
        let e2 = editor("id:\ntitle: A\n");
        assert_eq!(e2.scalar("id"), Scalar::Value("".into()));
    }

    #[test]
    fn set_replaces_a_block_collection_without_orphaning_children() {
        // Overwriting a key that held a block list must remove the whole
        // block, never leave the indented `- item` lines behind as
        // invalid YAML.
        let mut e = editor("id:\n  - weird\n  - more\ntitle: A\nstatus: active\n");
        e.set("id", "doc-a");
        let out = e.render();
        assert!(out.contains("id: \"doc-a\""), "id replaced: {out}");
        assert!(!out.contains("- weird"), "block children removed: {out}");
        assert!(!out.contains("- more"), "block children removed: {out}");
        assert!(out.contains("title: A"), "sibling preserved: {out}");
        assert!(out.contains("status: active"), "sibling preserved: {out}");
    }

    #[test]
    fn set_in_place_preserves_other_lines() {
        let mut e = editor("id: foo\n# comment line\nstatus: active\nupdated: 2026-01-01\n");
        e.set("status", "archived");
        let out = e.render();
        assert!(out.contains("# comment line"));
        assert!(out.contains("status: \"archived\""));
        assert!(out.contains("updated: 2026-01-01"));
        let status_pos = out.find("status:").unwrap();
        let comment_pos = out.find("# comment line").unwrap();
        let updated_pos = out.find("updated:").unwrap();
        assert!(comment_pos < status_pos);
        assert!(status_pos < updated_pos);
    }

    #[test]
    fn set_new_key_appends() {
        let mut e = editor("id: foo\n");
        e.set("status", "archived");
        let out = e.render();
        assert!(out.contains("id: foo"));
        assert!(out.contains("status: \"archived\""));
    }

    #[test]
    fn duplicate_keys_rejected() {
        let err = FrontmatterEditor::parse("id: a\nid: b\n", Path::new("dup.md")).unwrap_err();
        match err {
            Error::Parse {
                source: ParseError::InvalidField { field, .. },
                ..
            } => assert_eq!(field, "id"),
            _ => panic!("expected InvalidField, got {err:?}"),
        }
    }

    #[test]
    fn nested_keys_are_ignored() {
        let e = editor("id: foo\nattrs:\n  nested: yes\n");
        assert_eq!(e.scalar("id"), Scalar::Value("foo".into()));
        assert_eq!(e.scalar("nested"), Scalar::Absent);
    }

    #[test]
    fn set_list_writes_block_style_for_new_key() {
        let mut e = editor("id: foo\n");
        e.set_list("related", &["adr-001", "adr-002"]);
        let out = e.render();
        assert!(out.contains("related:\n  - \"adr-001\"\n  - \"adr-002\""));
        assert!(out.contains("id: foo"));
    }

    #[test]
    fn set_list_replaces_existing_block_style_list() {
        let mut e = editor("id: foo\nrelated:\n  - old-1\n  - old-2\nstatus: active\n");
        e.set_list("related", &["new-1"]);
        let out = e.render();
        assert!(out.contains("related:\n  - \"new-1\""));
        assert!(!out.contains("old-1"));
        assert!(!out.contains("old-2"));
        assert!(out.contains("status: active"));
    }

    #[test]
    fn set_list_replaces_existing_inline_list() {
        let mut e = editor("id: foo\ntags: [a, b, c]\nstatus: active\n");
        e.set_list("tags", &["x"]);
        let out = e.render();
        assert!(out.contains("tags:\n  - \"x\""));
        assert!(!out.contains("[a, b, c]"));
        assert!(out.contains("status: active"));
    }

    #[test]
    fn set_list_with_empty_items_removes_key() {
        let mut e = editor("id: foo\ntags:\n  - one\n  - two\nstatus: active\n");
        e.set_list("tags", &[]);
        let out = e.render();
        assert!(!out.contains("tags"));
        assert!(out.contains("id: foo"));
        assert!(out.contains("status: active"));
    }

    #[test]
    fn set_list_quotes_values_with_special_chars() {
        let mut e = editor("id: foo\n");
        e.set_list("notes", &["with \"quote\"", "with \\ backslash"]);
        let out = e.render();
        assert!(out.contains("notes:\n  - \"with \\\"quote\\\"\"\n  - \"with \\\\ backslash\""));
    }
}
