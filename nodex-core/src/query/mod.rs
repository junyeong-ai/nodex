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
/// candidate with no composite is not in the ranking's domain: it
/// never occupies a top/bottom-N slot, never satisfies a score cutoff,
/// and never sorts as an extreme. No composite has two causes — no
/// positively-weighted signal present to rank by, or a
/// positively-weighted component the run can measure that the document
/// left undeclared (`trust`) — and both leave through here. The
/// exclusion is never silent — `unscored` counts the excluded entries
/// so the CLI can surface them as an envelope warning. A count rather
/// than an id list: a trust ranking's node probes via the single-node
/// form, and a similarity query carrying no signal excludes every
/// candidate alike, so the count says what the ids would and the
/// payload stays bounded.
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
    /// Project `node_ref` onto exactly `fields` — a name absent from
    /// `fields` is dropped (its `Option` is `None`, omitted by
    /// `skip_serializing_if`). An empty `fields` keeps no spine field at
    /// all; the default `query nodes` listing (no `--fields`) passes the
    /// full [`NODE_REF_FIELDS`] set explicitly, so it always emits the
    /// five-field identity and never a bare object. Unknown names are the
    /// caller's responsibility to reject up-front (the CLI validates
    /// against the spine ∪ declared fields); this constructor only keeps
    /// or drops.
    pub fn from_node_ref(node_ref: NodeRef, fields: &[String]) -> Self {
        let keep = |name: &str| fields.iter().any(|f| f == name);
        Self {
            id: keep("id").then_some(node_ref.id),
            title: keep("title").then_some(node_ref.title),
            kind: keep("kind").then_some(node_ref.kind),
            status: keep("status").then_some(node_ref.status),
            path: keep("path").then_some(node_ref.path),
        }
    }
}

/// A `query nodes` listing entry: the projected [`NodeRef`] spine plus
/// any requested non-spine frontmatter fields (other built-ins like
/// `owner` / `created` / `tags`, and project-declared `attrs` keys)
/// under `attrs`. The spine stays top-level so the five identity fields
/// keep their non-null contract for typed clients; enrichment lands in
/// a nested map so a project-declared key can never collide with an
/// entry's structural fields. `attrs` is omitted when empty (no
/// non-spine field was requested), so the default `query nodes` shape is
/// byte-identical to the bare spine.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct NodeListingEntry {
    #[serde(flatten)]
    pub node: NodeRefProjection,
    // `default + skip_serializing_if = empty` (the convention `Node` and
    // `AnnotationSourceRef.frontmatter` use): an entry without a declared
    // non-spine field projected omits `attrs` entirely, and the schema
    // marks it optional so a default listing validates.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub attrs: std::collections::BTreeMap<String, serde_json::Value>,
}

impl NodeListingEntry {
    /// Project a node onto a listing entry: `spine_fields` selects which
    /// of the five identity fields appear (empty = none), `extra_fields`
    /// names non-spine frontmatter fields to enrich `attrs` (empty =
    /// none). Vocabularies are validated by the CLI before this is reached.
    ///
    /// Today this is the seam behind `query nodes --fields` only — the one
    /// caller is `find_nodes_projected`. The other NodeRef-bearing
    /// listings (`search`, `backlinks`, `orphans`, `stale`, `chain`,
    /// `covered-by`) flatten the full [`NodeRef`] and always emit the
    /// five-field spine; none accepts `--fields`. Were projection extended
    /// to them, the long-term shape is a CLI-layer projector that drops
    /// fields at serialization for opted-in listings — *not* swapping
    /// [`NodeRef`] for [`NodeRefProjection`] inside the shared entry
    /// types, which are double-duty (a JSON contract *and* in-process data
    /// read as non-`Option` by `output/markdown.rs` and embedded in
    /// [`crate::query::issues::IssueReport`]) and each carry per-entry
    /// fields beyond the spine.
    pub fn project(node: &Node, spine_fields: &[String], extra_fields: &[String]) -> Self {
        Self {
            node: NodeRefProjection::from_node_ref(NodeRef::from_node(node), spine_fields),
            attrs: if extra_fields.is_empty() {
                std::collections::BTreeMap::new()
            } else {
                crate::query::annotations::collect_frontmatter(node, extra_fields)
            },
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
