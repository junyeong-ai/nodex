use chrono::NaiveDate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::kind::Kind;
use super::status::Status;

/// A document node in the graph.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Node {
    // === Identity ===
    pub id: String,
    #[serde(
        serialize_with = "serialize_path_forward",
        deserialize_with = "deserialize_path"
    )]
    #[schemars(with = "String")]
    pub path: PathBuf,
    pub title: String,
    pub kind: Kind,

    // === Lifecycle ===
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<NaiveDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated: Option<NaiveDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewed: Option<NaiveDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,

    // === Relations (from frontmatter) ===
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supersedes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub implements: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Source-code (or other out-of-graph) paths this document covers.
    /// Each entry becomes an outgoing edge with relation `"covers"`;
    /// the target stays [`crate::model::ResolvedTarget::Unresolved`]
    /// by design because code paths sit outside the doc graph.
    /// Consumed by `GitDriftRule` (drift signal against covered code)
    /// and `nodex query covered-by` (reverse lookup).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub covers: Vec<String>,

    // === Flags ===
    #[serde(default)]
    pub orphan_ok: bool,

    // === Extension point for project-specific frontmatter fields ===
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attrs: BTreeMap<String, serde_json::Value>,

    // === Body fingerprints (parser-computed; never authored) ===
    //
    // `body_hash` and `body_lines_hash` are the structural fingerprint
    // of the document body, computed once at parse time and stored on
    // the node so check-time rules stay pure functions of
    // `(graph, config)`. They drive [`crate::rules::body_immutable`]:
    // the `frozen` mode compares `body_hash`; the `append_only` mode
    // compares `body_lines_hash` for prefix equality. Both are SHA-256
    // hex digests via [`crate::hash::sha256_hex`] — same algorithm the
    // build cache uses, so swapping is a single-file edit.
    /// SHA-256 hex of the body text after frontmatter splitting,
    /// `""` for a body-less document.
    #[serde(default)]
    pub body_hash: String,
    /// Per-line SHA-256 hex of every body line (in order, frontmatter
    /// excluded). Vec length equals the number of `body.lines()`
    /// elements; collapse-detection (`append_only`) compares this
    /// vector for prefix equality. Empty for a body-less document.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub body_lines_hash: Vec<String>,
}

impl Node {
    /// True when this node passes the rule's `kinds` filter:
    /// empty list means no restriction; otherwise the node's `kind`
    /// must appear in the list. The single primitive every per-block
    /// rule (annotations, body_line, body_immutable,
    /// frontmatter_immutable) and the build-time materialiser
    /// delegate to.
    #[inline]
    pub fn matches_kinds(&self, kinds: &[String]) -> bool {
        kinds.is_empty() || kinds.iter().any(|k| k == self.kind.as_str())
    }
}

/// Validate an explicitly supplied node id at a write seam.
///
/// Inferred ids are slugs by construction; an explicit id is free-form,
/// but it must round-trip through every reference syntax nodex itself
/// writes: a body `[[wikilink]]` capture is trimmed on parse, and `[`,
/// `]`, `|`, and line breaks are wikilink / reference metacharacters.
/// An id that is not trim-stable — or empty, or carrying those
/// metacharacters — would be written by `scaffold` or repointed by
/// `retarget` into a reference the next build cannot resolve back to
/// the node: the tool would manufacture a dangling reference. Refuse at
/// the seam instead. Unicode ids remain fully legal.
pub fn validate_explicit_id(id: &str) -> crate::error::Result<()> {
    let trim_stable = id.trim() == id;
    let has_metachar = id
        .chars()
        .any(|c| matches!(c, '[' | ']' | '|' | '\n' | '\r'));
    if id.is_empty() || !trim_stable || has_metachar {
        return Err(crate::error::Error::Config(format!(
            "node id {id:?} is not reference-safe: an id must be non-empty, without \
             leading/trailing whitespace, and free of the reference metacharacters \
             `[`, `]`, `|`, and line breaks — references nodex writes for it could \
             not resolve back to the node"
        )));
    }
    Ok(())
}

/// Serialize a path with forward slashes so JSON output is stable
/// across Windows and Unix. Shared across modules that serialise
/// `PathBuf` fields to JSON.
pub fn serialize_path_forward<S: serde::Serializer>(path: &Path, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&crate::path_guard::forward_string(path))
}

/// Deserialize a path from a JSON string.
pub fn deserialize_path<'de, D: serde::Deserializer<'de>>(d: D) -> Result<PathBuf, D::Error> {
    let s = String::deserialize(d)?;
    Ok(PathBuf::from(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn node_with_kind(k: &str) -> Node {
        Node {
            id: "x".into(),
            path: PathBuf::from("x.md"),
            title: "x".into(),
            kind: Kind::new(k),
            status: Status::new("active"),
            created: None,
            updated: None,
            reviewed: None,
            owner: None,
            supersedes: vec![],
            superseded_by: None,
            implements: vec![],
            related: vec![],
            tags: vec![],
            covers: vec![],
            orphan_ok: false,
            attrs: BTreeMap::new(),
            body_hash: String::new(),
            body_lines_hash: Vec::new(),
        }
    }

    #[test]
    fn matches_kinds_empty_list_passes_every_node() {
        // The identity predicate: an empty filter means "no
        // restriction on kind" — every node passes regardless of
        // what `kind` it carries. This is the recommended default
        // while a project is still deciding which kinds to lock down.
        assert!(node_with_kind("anything").matches_kinds(&[]));
    }

    #[test]
    fn matches_kinds_populated_list_is_or_within() {
        // OR-within-category semantics: a node matches when its kind
        // appears in *any* of the listed kinds. A node whose kind is
        // absent is rejected — same convention `NodeFilter` uses, so
        // authors moving between query filters and rule kind lists
        // never relearn the rule.
        let allowed = vec!["spec".into(), "adr".into()];
        assert!(node_with_kind("spec").matches_kinds(&allowed));
        assert!(node_with_kind("adr").matches_kinds(&allowed));
        assert!(!node_with_kind("readme").matches_kinds(&allowed));
    }

    #[test]
    fn explicit_ids_must_be_reference_safe() {
        // Reference-safe ids round-trip through every syntax nodex
        // writes; everything else is refused at the write seams.
        for ok in ["adr-001", "유니코드-아이디", "emoji-😀", "a", "x_y.z"] {
            assert!(validate_explicit_id(ok).is_ok(), "{ok:?} must be legal");
        }
        for bad in [
            "",
            " padded",
            "padded ",
            " both ",
            "tab\t",
            "with]bracket",
            "with[bracket",
            "pipe|sep",
            "line\nbreak",
            "cr\rbreak",
        ] {
            assert!(
                validate_explicit_id(bad).is_err(),
                "{bad:?} must be refused"
            );
        }
    }
}
