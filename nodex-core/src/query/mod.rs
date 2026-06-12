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

/// The five [`NodeRef`] field names, the vocabulary `--fields`
/// projection accepts. Kept next to the struct so the two cannot
/// drift.
pub const NODE_REF_FIELDS: &[&str] = &["id", "title", "kind", "status", "path"];

/// A ranking's ordered selection plus its structural exclusions. A
/// ranking is a total order over composite scores, so a node or
/// candidate with no composite (no positively-weighted signal present)
/// is not in the ranking's domain: it never occupies a top/bottom-N
/// slot, never satisfies a score cutoff, and never sorts as an
/// extreme. The exclusion is never silent — `unscored` counts the
/// excluded entries so the CLI can surface them as an envelope
/// warning. A count rather than an id list: identity is recoverable
/// (any node probes via the single-node form) and the payload stays
/// bounded.
#[derive(Debug, Clone)]
pub struct RankingOutcome<T> {
    pub entries: Vec<T>,
    pub unscored: usize,
}

/// Field-projected view of [`NodeRef`] for `query nodes --fields` —
/// the token-economy surface: an agent that only needs ids does not
/// pay for titles and paths. A sibling shape rather than an
/// `Option`-ised [`NodeRef`] on purpose: `NodeRef` is flattened into
/// every other list entry, and its five fields are non-null there by
/// contract — typed codegen clients must keep that guarantee.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct NodeRefProjection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl NodeRefProjection {
    /// Project `node_ref` onto `fields`. An empty list means "no
    /// projection" — all five fields are kept, so the flag's absence
    /// can never produce an empty object. Unknown field names are the
    /// caller's responsibility to reject up-front (the CLI validates
    /// against [`NODE_REF_FIELDS`]); this constructor only keeps or
    /// drops.
    pub fn from_node_ref(node_ref: NodeRef, fields: &[String]) -> Self {
        let keep = |name: &str| fields.is_empty() || fields.iter().any(|f| f == name);
        Self {
            id: keep("id").then_some(node_ref.id),
            title: keep("title").then_some(node_ref.title),
            kind: keep("kind").then_some(node_ref.kind),
            status: keep("status").then_some(node_ref.status),
            path: keep("path").then_some(node_ref.path),
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
