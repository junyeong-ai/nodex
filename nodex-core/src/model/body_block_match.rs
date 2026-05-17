//! Multi-line body blocks — extracted at parse time, validated by
//! [`crate::rules::body_block::BodyBlockRule`] at check time.
//!
//! A *block* is a contiguous span of body lines whose first line
//! matches the configured `start_pattern` and whose terminator is the
//! first subsequent line matching `end_pattern` (or another
//! `start_pattern` match — which closes the previous block and opens
//! a new one — or end-of-body).
//!
//! Symmetric with [`crate::model::BodyLineMatch`]: the parser produces
//! raw matches (no kind filter, no enum check), the builder applies
//! `applies_to_kind` when materialising onto the graph, and consumers
//! (the rule) read from the graph without re-touching the filesystem.
//! Captures come from the *start* line's regex — body_block is a
//! framing primitive, not a per-line scanner. A project that needs
//! per-line conformance inside a block already has
//! [`crate::model::BodyLineMatch`].

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One raw block match before the source kind is known. Mirrors
/// [`crate::model::RawBodyLineMatch`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawBodyBlockMatch {
    /// The `[[rules.body_block]].name` whose start_pattern matched.
    pub rule_name: String,
    /// 1-based line number of the line that matched `start_pattern`.
    pub start_line: usize,
    /// 1-based line number of the last body line that belongs to
    /// this block. Equal to `start_line` when the block has no
    /// content lines (`end_pattern` matched the line right after
    /// the header). Past the last body line when end-of-body
    /// closed the block.
    pub end_line: usize,
    /// Named-capture values from the `start_pattern` match.
    pub captures: BTreeMap<String, String>,
}

/// One block resolved against the project graph. Same shape as
/// `RawBodyBlockMatch` plus the source node id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BodyBlockMatch {
    pub source_id: String,
    pub rule_name: String,
    pub start_line: usize,
    pub end_line: usize,
    pub captures: BTreeMap<String, String>,
}
