pub mod annotations;
pub mod dependents;
pub mod detect;
pub mod issues;
pub mod listing;
pub mod recent;
pub mod search;
pub mod similar;
pub mod structure;
pub mod traverse;
pub mod trust;

use chrono::NaiveDate;
use schemars::JsonSchema;
use serde::Serialize;

use crate::model::Node;

/// Common identifying view of a node embedded in every query result.
/// Flattened by serde so the JSON shape is `{ id, title, kind, status,
/// path, ...query-specific fields }` — uniform across every query.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct NodeRef {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub status: String,
    pub path: String,
}

impl NodeRef {
    pub fn from_node(node: &Node) -> Self {
        Self {
            id: node.id.clone(),
            title: node.title.clone(),
            kind: node.kind.to_string(),
            status: node.status.to_string(),
            path: crate::path_guard::forward_string(&node.path),
        }
    }
}

/// Whole days from `date` to `today`. Clamps negatives (future dates
/// from clock skew or post-dating) to 0 and saturates at `u32::MAX`,
/// so every "days ago" / "days since" surface computes the same value
/// the same way.
pub(crate) fn days_between_clamped(today: NaiveDate, date: NaiveDate) -> u32 {
    (today - date).num_days().max(0).min(u32::MAX as i64) as u32
}
