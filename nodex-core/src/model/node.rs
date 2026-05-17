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
