//! Body-text annotations — config-declared regex markers extracted at
//! build time and surfaced by `nodex query annotations`.
//!
//! Annotations are a deliberately *narrow* parallel to edges: both come
//! from body scans, both are config-driven, but only edges participate
//! in graph membership (`mentions` / `references` resolve to other
//! nodes). Annotations intentionally do not resolve — they capture a
//! grouping key for callers that ask "every doc that mentions X" where
//! X is a pre-graph identifier (e.g. a promotion candidate, an open
//! research question) that may never become a node.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One annotation extracted from a document body during parsing,
/// before the source kind is known. `Annotation` is the resolved
/// shape stored on `Graph`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RawAnnotation {
    /// The `[[annotations]].name` whose pattern matched.
    pub name: String,
    /// The captured value of the configured `key` named capture.
    pub key: String,
    /// 1-based line number inside the body (frontmatter excluded).
    pub line: usize,
}

/// One annotation resolved against the project graph. The grouping
/// key, plus the source node id and the matched body location.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Annotation {
    pub source: String,
    pub name: String,
    pub key: String,
    pub line: usize,
}
