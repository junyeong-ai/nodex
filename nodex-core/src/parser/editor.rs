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
                        expected: "exactly one declaration".into(),
                    },
                });
            }
        }
        Ok(Self { lines, key_index })
    }

    /// Look up a top-level scalar, distinguishing absent from
    /// present-but-non-scalar so callers can react to each case
    /// without a second probe. A key whose value spans more than its
    /// own line is `NonScalar`: a `key:` / `key: # comment` header over
    /// a block collection (even across interior comment / blank runs),
    /// **or** a non-empty plain scalar that folds across more-indented
    /// continuation lines (`key: first` then `  more` reads as one
    /// value `"first more"`). Both are symmetric with the flow forms
    /// (`[a]` / `{a}`) and block scalars `parse_scalar_value` already
    /// rejects, so an editing caller refuses instead of acting on a
    /// truncated first-line value or overwriting only the key line and
    /// orphaning the rest into invalid YAML. A bare `key:` with no
    /// members is `Value("")`, exactly as the build parser reads it.
    pub fn scalar(&self, key: &str) -> Scalar<'_> {
        match self.key_index.get(key) {
            None => Scalar::Absent,
            Some(&idx) => match yaml_text::parse_scalar_value(&self.lines[idx]) {
                Some(_) if self.find_block_end(idx) > idx + 1 => Scalar::NonScalar,
                Some(v) => Scalar::Value(v),
                None => Scalar::NonScalar,
            },
        }
    }

    /// Set a top-level scalar value. In-place when the key exists,
    /// appended at the end otherwise. Always written as a quoted
    /// string scalar. Replacing a key that held a block-style
    /// collection or block scalar removes the whole block — including
    /// interior comment and blank lines, which YAML reads as part of
    /// the collection — never just the `key:` line, so the result is
    /// never orphaned child lines and never a split block. Trailing
    /// trivia after the block is preserved verbatim.
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
    /// block — flow `[a, b]` or block-style, including interior
    /// comment and blank lines between its members — is removed
    /// entirely before the new block is written, never split, so a
    /// removed item can never re-attach behind a comment. Trailing
    /// trivia after the block is preserved verbatim. An empty `items`
    /// slice removes the key altogether so the rendered output stays
    /// clean.
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

    /// First line index *after* the value of the key at `start`,
    /// dispatched on the value's shape. A block scalar (`|` / `>`,
    /// with any chomping / indent indicator) owns its indented body. A
    /// bare `key:` or `key: # comment` heads a block collection (a
    /// key-line trailing comment leaves the value empty — the comment
    /// is trivia, never the value) and owns its members plus interior
    /// trivia. A non-empty inline plain scalar may still fold across
    /// more-indented continuation lines (`key: first` then `  more`),
    /// which it owns. A flow collection (`[a]` / `{a}`) is the line
    /// itself — its continuation lines, if any, are never reached by an
    /// editing caller because `parse_scalar_value` classifies it
    /// `NonScalar` first.
    fn find_block_end(&self, start: usize) -> usize {
        let line = &self.lines[start];
        let colon = match line.find(':') {
            Some(c) => c,
            None => return start + 1,
        };
        let value = line[colon + 1..].trim_start();
        if value.starts_with('|') || value.starts_with('>') {
            return self.find_indented_body_end(start);
        }
        if value.is_empty() || value.starts_with('#') {
            return self.find_block_collection_end(start);
        }
        // A non-empty inline value: a plain scalar that may fold across
        // more-indented continuation lines. Owning them keeps `set` /
        // `set_list` from orphaning a continuation into invalid YAML (a
        // single-line value owns just its line). Flow collections reach
        // here too but `scalar()` has already classified them NonScalar,
        // so no editing caller acts on the over-owned span.
        self.find_indented_body_end(start)
    }

    /// Block-collection extent. Members are indented lines and `-`
    /// sequence entries (at any column). A run of trivia lines is
    /// owned only when a member line follows it — YAML legally threads
    /// comments and blank lines between sequence / mapping entries —
    /// so a trailing trivia run (a comment documenting the next key,
    /// trailing blanks) is left unowned and survives a replacement
    /// untouched.
    fn find_block_collection_end(&self, start: usize) -> usize {
        let mut end = start + 1;
        while end < self.lines.len() {
            if is_collection_member(&self.lines[end]) {
                end += 1;
            } else if is_block_trivia(&self.lines[end]) {
                let mut probe = end + 1;
                while probe < self.lines.len() && is_block_trivia(&self.lines[probe]) {
                    probe += 1;
                }
                if probe < self.lines.len() && is_collection_member(&self.lines[probe]) {
                    end = probe + 1;
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        end
    }

    /// Indented continuation-body extent, shared by a block scalar (`|`
    /// / `>`) and a folded plain scalar (`key: first` continued on
    /// more-indented lines) — their extent rules are identical: the body
    /// is the run of indented lines, and any column-0 non-blank line,
    /// including a `#` comment, terminates it per YAML. So the member test
    /// is first-char-whitespace, never comment-excluding. That is safe in
    /// both reachable shapes for different reasons: in a block scalar an
    /// indented `# text` line is *literal content* and must be owned; in a
    /// folded plain scalar an indented comment is *invalid YAML*
    /// (`yaml_serde` rejects it with "did not find expected key"), so such
    /// a document is dropped at build time and never reaches the editor as
    /// a `Node` — only comment-free plain folds arrive here. An interior
    /// blank run followed by an indented line is body; a trailing blank
    /// run is left unowned.
    fn find_indented_body_end(&self, start: usize) -> usize {
        let mut end = start + 1;
        while end < self.lines.len() {
            let line = &self.lines[end];
            if line.chars().all(char::is_whitespace) {
                let mut probe = end + 1;
                while probe < self.lines.len() && self.lines[probe].chars().all(char::is_whitespace)
                {
                    probe += 1;
                }
                if self
                    .lines
                    .get(probe)
                    .is_some_and(|l| l.starts_with(|c: char| c.is_whitespace()))
                {
                    end = probe + 1;
                } else {
                    break;
                }
            } else if line.starts_with(|c: char| c.is_whitespace()) {
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

/// A line that *is* block-collection content: indented, or a
/// (possibly column-0) `-` sequence entry — and never blank and never
/// a comment at any indentation, which are trivia between members.
fn is_collection_member(line: &str) -> bool {
    let Some(first) = line.chars().next() else {
        return false;
    };
    if !first.is_whitespace() && first != '-' {
        return false;
    }
    let trimmed = line.trim_start();
    !trimmed.is_empty() && !trimmed.starts_with('#')
}

/// A line YAML reads as trivia inside or after a block collection:
/// blank (empty or all-whitespace) or a comment at any indentation.
fn is_block_trivia(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.is_empty() || trimmed.starts_with('#')
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
    fn block_scalar_and_alias_values_are_non_scalar() {
        // A block scalar (`|` / `>`) and a YAML alias (`*ref`) are not
        // values the line editor can reason about — reading them must
        // report `NonScalar` so a caller refuses instead of misreading
        // (e.g. an aliased status bypassing the lifecycle terminal gate).
        let e = editor("note: |\n  body line\nstatus: *ref\nid: foo\n");
        assert_eq!(e.scalar("note"), Scalar::NonScalar);
        assert_eq!(e.scalar("status"), Scalar::NonScalar);
        assert_eq!(e.scalar("id"), Scalar::Value("foo".into()));
    }

    #[test]
    fn set_replaces_block_scalar_without_orphaning_body() {
        // Overwriting a key that held a block scalar must remove the
        // indented body too, never leave it dangling as invalid YAML.
        let mut e = editor("id: foo\nnote: |\n  first line\n  second line\nstatus: active\n");
        e.set("note", "short");
        let out = e.render();
        assert!(out.contains("note: \"short\""), "key replaced: {out}");
        assert!(!out.contains("first line"), "block body removed: {out}");
        assert!(!out.contains("second line"), "block body removed: {out}");
        assert!(out.contains("status: active"), "sibling preserved: {out}");
        assert!(out.contains("id: foo"), "sibling preserved: {out}");
    }

    #[test]
    fn folded_plain_scalar_is_non_scalar() {
        // A plain scalar that folds across a more-indented continuation
        // line reads, in yaml_serde, as one value ("first continuation").
        // The line editor sees only the first line, so it must refuse
        // (NonScalar) rather than act on the truncated "first" — the build
        // parser and the editor agree the value is not a clean single line.
        let yaml = "title: first\n  continuation\nstatus: active\n";
        let e = editor(yaml);
        assert_eq!(e.scalar("title"), Scalar::NonScalar);
        assert_eq!(e.scalar("status"), Scalar::Value("active".into()));
        // yaml_serde confirms the value really folds (it is one string,
        // not the truncated first line).
        let parsed: BTreeMap<String, yaml_serde::Value> =
            yaml_serde::from_str(yaml).expect("folded scalar parses");
        assert_eq!(
            parsed["title"],
            yaml_serde::Value::String("first continuation".into())
        );
    }

    #[test]
    fn set_replaces_folded_plain_scalar_without_orphaning_continuation() {
        // Overwriting a key that held a folded plain scalar must remove the
        // continuation line too, never leave it behind as invalid YAML.
        let mut e = editor("title: first\n  continuation\nstatus: active\n");
        e.set("title", "short");
        let out = e.render();
        assert!(out.contains("title: \"short\""), "key replaced: {out}");
        assert!(!out.contains("continuation"), "continuation removed: {out}");
        assert!(out.contains("status: active"), "sibling preserved: {out}");
        let parsed: BTreeMap<String, yaml_serde::Value> =
            yaml_serde::from_str(&out).expect("edited block parses");
        assert_eq!(parsed["title"], yaml_serde::Value::String("short".into()));
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

    #[test]
    fn set_list_replaces_block_interrupted_by_column0_comment() {
        // A column-0 comment between members is interior trivia: the
        // block is replaced whole, so a removed item can never
        // re-attach to the new list behind the comment.
        let mut e = editor("related:\n  - keep-1\n# note\n  - old-id\nstatus: active\n");
        e.set_list("related", &["keep-1", "new-id"]);
        let out = e.render();
        assert!(!out.contains("old-id"), "stale item removed: {out}");
        assert!(
            !out.contains("# note"),
            "interior comment removed with the block: {out}"
        );
        assert!(out.contains("status: active"), "sibling preserved: {out}");
        let parsed: BTreeMap<String, yaml_serde::Value> =
            yaml_serde::from_str(&out).expect("edited block parses");
        assert_eq!(
            parsed["related"],
            yaml_serde::Value::Sequence(vec!["keep-1".into(), "new-id".into()]),
            "yaml_serde reads exactly the new list: {out}"
        );
    }

    #[test]
    fn scalar_treats_comment_separated_block_as_non_scalar() {
        // Comment and blank runs followed by a member line are part of
        // the block, so both header shapes classify NonScalar — while
        // a key-line comment with no children stays an empty scalar.
        let e = editor("id:\n# c\n  - weird\n");
        assert_eq!(e.scalar("id"), Scalar::NonScalar);
        let e = editor("id: # c\n  - weird\n");
        assert_eq!(e.scalar("id"), Scalar::NonScalar);
        let e = editor("id: # only comment\nnext: 1\n");
        assert_eq!(e.scalar("id"), Scalar::Value("".into()));
    }

    #[test]
    fn set_list_leaves_trailing_comment_documenting_next_key() {
        let mut e = editor("related:\n  - a\n# about status\nstatus: active\n");
        e.set_list("related", &["b"]);
        let out = e.render();
        assert_eq!(
            out, "related:\n  - \"b\"\n# about status\nstatus: active\n",
            "a trailing comment is unowned and survives the replacement verbatim"
        );
    }

    #[test]
    fn set_preserves_trailing_blank_and_indented_comment_after_replaced_block() {
        let yaml = "related:\n  - a\n   \n  # note\nstatus: active\n";
        let mut e = editor(yaml);
        e.set("related", "x");
        assert_eq!(
            e.render(),
            "related: \"x\"\n   \n  # note\nstatus: active\n",
            "trailing blank and indented comment survive set() verbatim"
        );
        let mut e = editor(yaml);
        e.set_list("related", &["b"]);
        assert_eq!(
            e.render(),
            "related:\n  - \"b\"\n   \n  # note\nstatus: active\n",
            "trailing blank and indented comment survive set_list() verbatim"
        );
    }

    #[test]
    fn empty_value_before_blank_line_is_a_value() {
        // `key:` followed by a blank line and another top-level key is
        // an empty scalar, exactly as the build parser reads it.
        let e = editor("id:\n\ntitle: A\n");
        assert_eq!(e.scalar("id"), Scalar::Value("".into()));
    }

    #[test]
    fn block_scalar_ends_at_column0_comment() {
        // A column-0 `#` line terminates a block-scalar body per YAML
        // — it is never consumed with the replaced block.
        let mut e = editor("note: |\n  body\n# c\nstatus: a\n");
        e.set("note", "x");
        let out = e.render();
        assert!(out.contains("note: \"x\""), "key replaced: {out}");
        assert!(!out.contains("body"), "block body removed: {out}");
        assert!(out.contains("# c"), "terminating comment preserved: {out}");
        assert!(out.contains("status: a"), "sibling preserved: {out}");
    }

    mod properties {
        use std::collections::BTreeMap;
        use std::path::Path;

        use proptest::prelude::*;

        use super::super::{FrontmatterEditor, Scalar};
        use crate::yaml_text::{self, strategies};

        /// One generated top-level frontmatter entry. Each variant
        /// knows exactly the lines it owns — the key line plus member
        /// lines and interior trivia. Block segments always end with a
        /// member, so trailing trivia is modeled as separate top-level
        /// `Comment` / `Blank` entries that no entry owns.
        #[derive(Debug, Clone)]
        enum GenEntry {
            Scalar {
                key: String,
                line: String,
                value: String,
            },
            EmptyScalar {
                key: String,
                key_line_comment: Option<String>,
            },
            /// A plain scalar that folds across more-indented continuation
            /// lines (`key: head` then `  cont`) — one value to yaml_serde,
            /// but multi-line, so the editor classifies it `NonScalar`.
            PlainMultiline {
                key: String,
                head: String,
                continuations: Vec<String>,
            },
            FlowList {
                key: String,
                items: Vec<String>,
            },
            BlockList {
                key: String,
                key_line_comment: Option<String>,
                indent: &'static str,
                segments: Vec<Segment>,
            },
            BlockMap {
                key: String,
                key_line_comment: Option<String>,
                segments: Vec<Segment>,
            },
            BlockScalar {
                key: String,
                header: &'static str,
                body: Vec<Segment>,
            },
            Comment(String),
            Blank,
        }

        /// One line inside a block entry's owned region.
        #[derive(Debug, Clone)]
        enum Segment {
            Member(String),
            Comment0(String),
            CommentIndented(String),
            Blank,
        }

        #[derive(Debug, Clone)]
        struct GenDoc {
            entries: Vec<GenEntry>,
        }

        #[derive(Debug, Clone)]
        enum GenOp {
            Set { key: String, value: String },
            SetList { key: String, items: Vec<String> },
        }

        impl GenEntry {
            fn key(&self) -> Option<&str> {
                match self {
                    GenEntry::Scalar { key, .. }
                    | GenEntry::EmptyScalar { key, .. }
                    | GenEntry::PlainMultiline { key, .. }
                    | GenEntry::FlowList { key, .. }
                    | GenEntry::BlockList { key, .. }
                    | GenEntry::BlockMap { key, .. }
                    | GenEntry::BlockScalar { key, .. } => Some(key),
                    GenEntry::Comment(_) | GenEntry::Blank => None,
                }
            }

            fn lines(&self) -> Vec<String> {
                fn key_line(key: &str, comment: &Option<String>) -> String {
                    match comment {
                        Some(c) => format!("{key}: # {c}"),
                        None => format!("{key}:"),
                    }
                }
                match self {
                    GenEntry::Scalar { line, .. } => vec![line.clone()],
                    GenEntry::EmptyScalar {
                        key,
                        key_line_comment,
                    } => vec![key_line(key, key_line_comment)],
                    GenEntry::PlainMultiline {
                        key,
                        head,
                        continuations,
                    } => {
                        let mut lines = vec![format!("{key}: {head}")];
                        for cont in continuations {
                            lines.push(format!("  {cont}"));
                        }
                        lines
                    }
                    GenEntry::FlowList { key, items } => {
                        let rendered: Vec<String> =
                            items.iter().map(|i| yaml_text::quote(i)).collect();
                        vec![format!("{key}: [{}]", rendered.join(", "))]
                    }
                    GenEntry::BlockList {
                        key,
                        key_line_comment,
                        indent,
                        segments,
                    } => {
                        let mut lines = vec![key_line(key, key_line_comment)];
                        for segment in segments {
                            lines.push(match segment {
                                Segment::Member(item) => {
                                    format!("{indent}- {}", yaml_text::quote(item))
                                }
                                Segment::Comment0(c) => format!("# {c}"),
                                Segment::CommentIndented(c) => format!("  # {c}"),
                                Segment::Blank => String::new(),
                            });
                        }
                        lines
                    }
                    GenEntry::BlockMap {
                        key,
                        key_line_comment,
                        segments,
                    } => {
                        let mut lines = vec![key_line(key, key_line_comment)];
                        for (i, segment) in segments.iter().enumerate() {
                            lines.push(match segment {
                                Segment::Member(value) => {
                                    format!("  m{i}: {}", yaml_text::quote(value))
                                }
                                Segment::Comment0(c) => format!("# {c}"),
                                Segment::CommentIndented(c) => format!("  # {c}"),
                                Segment::Blank => String::new(),
                            });
                        }
                        lines
                    }
                    GenEntry::BlockScalar { key, header, body } => {
                        let mut lines = vec![format!("{key}: {header}")];
                        for segment in body {
                            lines.push(match segment {
                                Segment::Member(text) => format!("  {text}"),
                                Segment::CommentIndented(c) => format!("  # {c}"),
                                Segment::Blank => String::new(),
                                Segment::Comment0(_) => {
                                    unreachable!("a block-scalar body has no column-0 comments")
                                }
                            });
                        }
                        lines
                    }
                    GenEntry::Comment(c) => vec![format!("# {c}")],
                    GenEntry::Blank => vec![String::new()],
                }
            }
        }

        impl GenDoc {
            fn render(&self) -> String {
                let mut lines: Vec<String> = Vec::new();
                for entry in &self.entries {
                    lines.extend(entry.lines());
                }
                let mut out = lines.join("\n");
                out.push('\n');
                out
            }

            /// The document with the op target's owned lines replaced
            /// whole by the canonical rendering (a fresh key appends,
            /// an emptied list disappears), everything else
            /// byte-identical — the structural spec the editor must
            /// match exactly.
            fn expected_after(&self, op: &GenOp) -> String {
                let replacement = op.rendered_lines();
                let mut lines: Vec<String> = Vec::new();
                let mut replaced = false;
                for entry in &self.entries {
                    if entry.key() == Some(op.key()) {
                        lines.extend(replacement.iter().cloned());
                        replaced = true;
                    } else {
                        lines.extend(entry.lines());
                    }
                }
                if !replaced {
                    lines.extend(replacement);
                }
                let mut out = lines.join("\n");
                out.push('\n');
                out
            }

            /// Whether `key` holds a keep-chomped (`|+`) block scalar,
            /// whose parsed value keeps the trailing newlines of the
            /// blank run after its body — lines no entry owns.
            fn keeps_trailing_newlines(&self, key: &str) -> bool {
                self.entries.iter().any(|entry| {
                    matches!(entry, GenEntry::BlockScalar { key: k, header: "|+", .. } if k == key)
                })
            }

            fn expected_scalar(&self, key: &str) -> Scalar<'static> {
                for entry in &self.entries {
                    if entry.key() == Some(key) {
                        return match entry {
                            GenEntry::Scalar { value, .. } => Scalar::Value(value.clone().into()),
                            GenEntry::EmptyScalar { .. } => Scalar::Value("".into()),
                            GenEntry::PlainMultiline { .. }
                            | GenEntry::FlowList { .. }
                            | GenEntry::BlockList { .. }
                            | GenEntry::BlockMap { .. }
                            | GenEntry::BlockScalar { .. } => Scalar::NonScalar,
                            GenEntry::Comment(_) | GenEntry::Blank => {
                                unreachable!("trivia entries carry no key")
                            }
                        };
                    }
                }
                Scalar::Absent
            }
        }

        impl GenOp {
            fn key(&self) -> &str {
                match self {
                    GenOp::Set { key, .. } | GenOp::SetList { key, .. } => key,
                }
            }

            /// The canonical lines the editor writes for this op; an
            /// empty list removes the entry.
            fn rendered_lines(&self) -> Vec<String> {
                match self {
                    GenOp::Set { key, value } => {
                        vec![yaml_text::render_scalar_line(key, value)]
                    }
                    GenOp::SetList { items, .. } if items.is_empty() => Vec::new(),
                    GenOp::SetList { key, items } => {
                        let mut lines = vec![format!("{key}:")];
                        for item in items {
                            lines.push(format!("  - {}", yaml_text::quote(item)));
                        }
                        lines
                    }
                }
            }

            fn apply(&self, editor: &mut FrontmatterEditor) {
                match self {
                    GenOp::Set { key, value } => editor.set(key, value),
                    GenOp::SetList { key, items } => {
                        let refs: Vec<&str> = items.iter().map(String::as_str).collect();
                        editor.set_list(key, &refs);
                    }
                }
            }

            /// The mutation expressed against the yaml_serde document
            /// map: `set` writes a string, a non-empty `set_list` a
            /// string sequence, an empty `set_list` removes the key.
            fn apply_to_map(&self, map: &mut BTreeMap<String, yaml_serde::Value>) {
                match self {
                    GenOp::Set { key, value } => {
                        map.insert(key.clone(), yaml_serde::Value::String(value.clone()));
                    }
                    GenOp::SetList { key, items } if items.is_empty() => {
                        map.remove(key);
                    }
                    GenOp::SetList { key, items } => {
                        map.insert(
                            key.clone(),
                            yaml_serde::Value::Sequence(
                                items
                                    .iter()
                                    .cloned()
                                    .map(yaml_serde::Value::String)
                                    .collect(),
                            ),
                        );
                    }
                }
            }
        }

        /// Parse a frontmatter block as a top-level mapping. An empty
        /// or comment-only block parses as Null, which reads as the
        /// empty mapping (a `set_list` that removed the last key
        /// renders exactly that).
        fn parse_doc_map(
            yaml: &str,
        ) -> Result<BTreeMap<String, yaml_serde::Value>, yaml_serde::Error> {
            match yaml_serde::from_str::<yaml_serde::Value>(yaml)? {
                yaml_serde::Value::Null => Ok(BTreeMap::new()),
                value => yaml_serde::from_value(value),
            }
        }

        fn comment_text() -> impl Strategy<Value = String> {
            proptest::string::string_regex("[a-z0-9 ]{0,6}").expect("hardcoded regex compiles")
        }

        /// A literal block-scalar body line: never starting with
        /// whitespace (a deeper first line would shift the parser's
        /// detected indent), but free to look like a comment or a
        /// sequence entry — inside a block scalar both are content.
        fn body_text() -> impl Strategy<Value = String> {
            proptest::string::string_regex("[a-z#-][a-z0-9 #:-]{0,6}")
                .expect("hardcoded regex compiles")
                .prop_filter("no trailing whitespace", |s| s.trim_end() == s.as_str())
        }

        /// Block-collection segments: interior members and trivia in
        /// any order, always ending with a member so the entry owns
        /// every generated line. `Comment0` produces the column-0
        /// comment between members.
        fn collection_segments() -> impl Strategy<Value = Vec<Segment>> {
            let interior = prop_oneof![
                strategies::any_value().prop_map(Segment::Member),
                comment_text().prop_map(Segment::Comment0),
                comment_text().prop_map(Segment::CommentIndented),
                Just(Segment::Blank),
            ];
            (
                prop::collection::vec(interior, 0..4),
                strategies::any_value(),
            )
                .prop_map(|(mut segments, last)| {
                    segments.push(Segment::Member(last));
                    segments
                })
        }

        /// Block-scalar body segments: indented content lines (an
        /// indented comment is content) and interior blanks, always
        /// ending with a content line.
        fn scalar_body_segments() -> impl Strategy<Value = Vec<Segment>> {
            let interior = prop_oneof![
                body_text().prop_map(Segment::Member),
                comment_text().prop_map(Segment::CommentIndented),
                Just(Segment::Blank),
            ];
            (prop::collection::vec(interior, 0..4), body_text()).prop_map(|(mut body, last)| {
                body.push(Segment::Member(last));
                body
            })
        }

        fn keyed_entry(key: String) -> impl Strategy<Value = GenEntry> {
            let scalar = {
                let key = key.clone();
                strategies::scalar_line().prop_map(move |(line, value)| {
                    let colon = line.find(':').expect("scalar lines carry a key separator");
                    GenEntry::Scalar {
                        line: format!("{key}{}", &line[colon..]),
                        key: key.clone(),
                        value,
                    }
                })
            };
            let empty_scalar = {
                let key = key.clone();
                prop::option::of(comment_text()).prop_map(move |key_line_comment| {
                    GenEntry::EmptyScalar {
                        key: key.clone(),
                        key_line_comment,
                    }
                })
            };
            let plain_multiline = {
                let key = key.clone();
                (
                    strategies::plain_value(),
                    prop::collection::vec(strategies::plain_value(), 1..3),
                )
                    .prop_map(move |(head, continuations)| {
                        GenEntry::PlainMultiline {
                            key: key.clone(),
                            head,
                            continuations,
                        }
                    })
            };
            let flow_list = {
                let key = key.clone();
                prop::collection::vec(strategies::any_value(), 0..3).prop_map(move |items| {
                    GenEntry::FlowList {
                        key: key.clone(),
                        items,
                    }
                })
            };
            let block_list = {
                let key = key.clone();
                (
                    prop::option::of(comment_text()),
                    prop_oneof![Just(""), Just("  ")],
                    collection_segments(),
                )
                    .prop_map(move |(key_line_comment, indent, segments)| {
                        GenEntry::BlockList {
                            key: key.clone(),
                            key_line_comment,
                            indent,
                            segments,
                        }
                    })
            };
            let block_map = {
                let key = key.clone();
                (prop::option::of(comment_text()), collection_segments()).prop_map(
                    move |(key_line_comment, segments)| GenEntry::BlockMap {
                        key: key.clone(),
                        key_line_comment,
                        segments,
                    },
                )
            };
            let block_scalar = {
                let key = key.clone();
                (
                    prop_oneof![Just("|"), Just(">"), Just("|-"), Just(">-"), Just("|+")],
                    scalar_body_segments(),
                )
                    .prop_map(move |(header, body)| GenEntry::BlockScalar {
                        key: key.clone(),
                        header,
                        body,
                    })
            };
            prop_oneof![
                scalar,
                empty_scalar,
                plain_multiline,
                flow_list,
                block_list,
                block_map,
                block_scalar
            ]
        }

        fn trivia_entry() -> impl Strategy<Value = GenEntry> {
            prop_oneof![
                comment_text().prop_map(GenEntry::Comment),
                Just(GenEntry::Blank),
            ]
        }

        /// A whole document plus one mutation against it. Keys are
        /// drawn as a set (the editor rejects duplicates), with one
        /// extra key reserved for fresh-key ops; trivia runs are
        /// interleaved between entries and trailed at the end.
        fn doc_and_op() -> impl Strategy<Value = (GenDoc, GenOp)> {
            (1usize..=4)
                .prop_flat_map(|keyed| {
                    (
                        prop::collection::btree_set(strategies::key(), keyed + 1),
                        Just(keyed),
                    )
                })
                .prop_flat_map(|(keys, keyed)| {
                    let keys: Vec<String> = keys.into_iter().collect();
                    let fresh_key = keys[keyed].clone();
                    let entries: Vec<_> = keys[..keyed].iter().cloned().map(keyed_entry).collect();
                    let trivia_runs = prop::collection::vec(
                        prop::collection::vec(trivia_entry(), 0..3),
                        keyed + 1,
                    );
                    (entries, trivia_runs, Just(fresh_key))
                })
                .prop_flat_map(|(keyed_entries, trivia_runs, fresh_key)| {
                    let n = keyed_entries.len();
                    let mut entries: Vec<GenEntry> = Vec::new();
                    for (i, keyed) in keyed_entries.into_iter().enumerate() {
                        entries.extend(trivia_runs[i].iter().cloned());
                        entries.push(keyed);
                    }
                    entries.extend(trivia_runs[n].iter().cloned());
                    let doc = GenDoc { entries };
                    let mut target_keys: Vec<String> = doc
                        .entries
                        .iter()
                        .filter_map(|e| e.key().map(String::from))
                        .collect();
                    target_keys.push(fresh_key);
                    let op = (
                        prop::sample::select(target_keys),
                        any::<bool>(),
                        strategies::any_value(),
                        prop::collection::vec(strategies::any_value(), 0..3),
                    )
                        .prop_map(|(key, is_set, value, items)| {
                            if is_set {
                                GenOp::Set { key, value }
                            } else {
                                GenOp::SetList { key, items }
                            }
                        });
                    (Just(doc), op)
                })
        }

        proptest! {
            /// The edited render parses through `yaml_serde` (the
            /// output is never invalid YAML) and equals the input
            /// document with exactly the one intended mutation.
            #[test]
            fn edits_agree_with_yaml_serde_on_generated_documents((doc, op) in doc_and_op()) {
                let input = doc.render();
                let mut expected = parse_doc_map(&input).expect("generated document parses");
                op.apply_to_map(&mut expected);
                let mut editor = FrontmatterEditor::parse(&input, Path::new("gen.md"))
                    .expect("generated keys are unique");
                op.apply(&mut editor);
                let output = editor.render();
                let actual = match parse_doc_map(&output) {
                    Ok(map) => map,
                    Err(e) => {
                        return Err(TestCaseError::fail(format!(
                            "edited document is not valid YAML: {e}\ninput:\n{input}\noutput:\n{output}"
                        )));
                    }
                };
                let actual_keys: Vec<&String> = actual.keys().collect();
                let expected_keys: Vec<&String> = expected.keys().collect();
                prop_assert_eq!(actual_keys, expected_keys, "input:\n{}\noutput:\n{}", input, output);
                for (key, expected_value) in &expected {
                    let actual_value = &actual[key];
                    if key != op.key() && doc.keeps_trailing_newlines(key) {
                        // Keep chomping (`|+`) reads the blank run after
                        // the body — lines no entry owns and the editor
                        // preserves verbatim — into the value, so a
                        // removal next to the block legitimately changes
                        // the kept newline count. Everything before the
                        // trailing newlines must still match exactly.
                        let (yaml_serde::Value::String(a), yaml_serde::Value::String(e)) =
                            (actual_value, expected_value)
                        else {
                            return Err(TestCaseError::fail(format!(
                                "a block scalar parses as a string: {key}\ninput:\n{input}"
                            )));
                        };
                        prop_assert_eq!(
                            a.trim_end_matches('\n'),
                            e.trim_end_matches('\n'),
                            "input:\n{}\noutput:\n{}",
                            input,
                            output
                        );
                    } else {
                        prop_assert_eq!(
                            actual_value,
                            expected_value,
                            "diverged on {}\ninput:\n{}\noutput:\n{}",
                            key,
                            input,
                            output
                        );
                    }
                }
            }

            /// Byte-level spec: only the target entry's owned lines
            /// change, replaced whole by the canonical rendering;
            /// every unowned line — including trailing trivia —
            /// survives verbatim.
            #[test]
            fn edits_touch_only_the_target_entry_lines((doc, op) in doc_and_op()) {
                let input = doc.render();
                let mut editor = FrontmatterEditor::parse(&input, Path::new("gen.md"))
                    .expect("generated keys are unique");
                op.apply(&mut editor);
                prop_assert_eq!(editor.render(), doc.expected_after(&op), "input:\n{}", input);
            }

            /// scalar() classifies every generated entry exactly as
            /// its shape dictates, and an absent key reports Absent.
            #[test]
            fn scalar_classification_matches_generated_shape((doc, op) in doc_and_op()) {
                let input = doc.render();
                let editor = FrontmatterEditor::parse(&input, Path::new("gen.md"))
                    .expect("generated keys are unique");
                for entry in &doc.entries {
                    if let Some(key) = entry.key() {
                        prop_assert_eq!(
                            editor.scalar(key),
                            doc.expected_scalar(key),
                            "misclassified {} in:\n{}",
                            key,
                            input
                        );
                    }
                }
                prop_assert_eq!(
                    editor.scalar(op.key()),
                    doc.expected_scalar(op.key()),
                    "misclassified op target {} in:\n{}",
                    op.key(),
                    input
                );
            }

            /// Parsing a rendered document and rendering it again is
            /// the identity — the editor never normalizes lines it
            /// was not asked to touch.
            #[test]
            fn parse_render_is_identity_on_generated_documents((doc, _op) in doc_and_op()) {
                let input = doc.render();
                let editor = FrontmatterEditor::parse(&input, Path::new("gen.md"))
                    .expect("generated keys are unique");
                prop_assert_eq!(editor.render(), input);
            }
        }
    }
}
