//! Body-line regex matches — extracted at parse time, validated by
//! [`crate::rules::body_line::BodyLineRule`] at check time.
//!
//! Symmetric with annotations: the parser produces raw matches (no
//! kind filter, no enum check), the builder applies `kinds`
//! when materialising onto the graph, and consumers (the rule) read
//! from the graph without re-touching the filesystem. Storing matches
//! in the graph keeps every check-time rule a pure function of
//! `(graph, config)` — the same discipline schema / freshness /
//! naming rules already follow.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One regex match against a `[[rules.body_line]]` pattern, before
/// the source kind is known. `BodyLineMatch` is the resolved shape
/// stored on `Graph`. Every named capture in the matching regex is
/// recorded — the rule consumes whichever ones it has enums for, and
/// future inspection surfaces can lean on the full set without
/// requiring a re-parse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawBodyLineMatch {
    /// The `[[rules.body_line]].name` whose pattern matched.
    pub rule_name: String,
    /// 1-based line number inside the body (frontmatter excluded).
    pub line: usize,
    /// Named-capture values, keyed by capture name.
    pub captures: BTreeMap<String, String>,
}

/// One regex match resolved against the project graph. Same shape as
/// `RawBodyLineMatch` plus the source node id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BodyLineMatch {
    pub source: String,
    pub rule_name: String,
    pub line: usize,
    pub captures: BTreeMap<String, String>,
}
