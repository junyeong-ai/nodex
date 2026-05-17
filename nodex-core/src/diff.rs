//! Structural diff between two graph snapshots.
//!
//! Pure transformation of two [`Graph`] values into a [`GraphDiff`]
//! describing every node addition / removal, edge addition / removal,
//! status transition, and frontmatter field change. No policy, no
//! heuristics — downstream callers (rule policies, CI gates, the CLI's
//! human-readable report) decide what to do with the delta.

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

use crate::model::{Edge, Graph, Node, ResolvedTarget};
use crate::query::NodeRef;

/// A structural delta between graph A (the "before" snapshot) and
/// graph B (the "after" snapshot).
#[derive(Debug, Clone, Serialize)]
pub struct GraphDiff {
    pub added_nodes: Vec<NodeRef>,
    pub removed_nodes: Vec<NodeRef>,
    pub added_edges: Vec<EdgeRef>,
    pub removed_edges: Vec<EdgeRef>,
    pub status_transitions: Vec<StatusTransition>,
    pub field_changes: Vec<FieldChange>,
}

/// A flat view of an [`Edge`] suitable for diff output. We re-emit the
/// target shape verbatim so downstream readers see exactly the same
/// `{ type, id }` / `{ type, raw, reason }` shape as in `graph.json`.
#[derive(Debug, Clone, Serialize)]
pub struct EdgeRef {
    pub source: String,
    pub target: ResolvedTarget,
    pub relation: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusTransition {
    pub id: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FieldChange {
    pub id: String,
    /// Frontmatter field name. Owned `String` rather than `&'static str`
    /// so dynamic `attrs` keys are first-class — the diff primitive must
    /// be safe for library callers that invoke it repeatedly in a
    /// long-running process.
    pub field: String,
    /// `None` when the field was unset in the "before" snapshot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<serde_json::Value>,
    /// `None` when the field was unset in the "after" snapshot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<serde_json::Value>,
}

/// Compute a deterministic structural diff. Every output collection is
/// sorted so two runs on the same inputs produce byte-identical JSON.
pub fn compute_diff(before: &Graph, after: &Graph) -> GraphDiff {
    let before_ids: BTreeSet<&str> = before.nodes().keys().map(String::as_str).collect();
    let after_ids: BTreeSet<&str> = after.nodes().keys().map(String::as_str).collect();

    let added: Vec<NodeRef> = after_ids
        .difference(&before_ids)
        .filter_map(|id| after.node(id))
        .map(NodeRef::from_node)
        .collect();
    let removed: Vec<NodeRef> = before_ids
        .difference(&after_ids)
        .filter_map(|id| before.node(id))
        .map(NodeRef::from_node)
        .collect();

    // Index each graph's edges by `EdgeKey` so the set-difference
    // operates on stable identity (source × target × relation, with
    // unresolved targets keyed on the raw user string) while the
    // output still recovers the full original [`Edge`] — including
    // the `Unresolved::reason` diagnostic. Keying on `EdgeKey` and
    // looking up via the map is a single seam where dedup semantics
    // (collapse `(source, raw, relation)` regardless of resolver
    // reason) and output semantics (preserve the reason for the
    // caller) are reconciled.
    let before_edges = edge_index(before);
    let after_edges = edge_index(after);
    let before_keys: BTreeSet<&EdgeKey> = before_edges.keys().collect();
    let after_keys: BTreeSet<&EdgeKey> = after_edges.keys().collect();

    let added_edges: Vec<EdgeRef> = after_keys
        .difference(&before_keys)
        .filter_map(|k| after_edges.get(*k))
        .map(|e| edge_ref_from(e))
        .collect();
    let removed_edges: Vec<EdgeRef> = before_keys
        .difference(&after_keys)
        .filter_map(|k| before_edges.get(*k))
        .map(|e| edge_ref_from(e))
        .collect();

    // Per-node field changes — only for ids present in both snapshots.
    let mut transitions = Vec::new();
    let mut field_changes = Vec::new();
    for id in before_ids.intersection(&after_ids) {
        let (Some(b), Some(a)) = (before.node(id), after.node(id)) else {
            continue;
        };
        if b.status.as_str() != a.status.as_str() {
            transitions.push(StatusTransition {
                id: (*id).to_string(),
                from: b.status.as_str().to_string(),
                to: a.status.as_str().to_string(),
            });
        }
        collect_field_changes(b, a, &mut field_changes);
    }

    field_changes.sort_by(|x, y| x.id.cmp(&y.id).then_with(|| x.field.cmp(&y.field)));

    GraphDiff {
        added_nodes: added,
        removed_nodes: removed,
        added_edges,
        removed_edges,
        status_transitions: transitions,
        field_changes,
    }
}

// ─── helpers ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct EdgeKey {
    source: String,
    target_kind: u8, // 0=resolved, 1=unresolved
    target_payload: String,
    relation: String,
}

impl EdgeKey {
    fn from_edge(e: &Edge) -> Self {
        let (target_kind, target_payload) = match &e.target {
            ResolvedTarget::Resolved { id } => (0u8, id.clone()),
            ResolvedTarget::Unresolved { raw, .. } => (1u8, raw.clone()),
        };
        Self {
            source: e.source.clone(),
            target_kind,
            target_payload,
            relation: e.relation.clone(),
        }
    }
}

/// Map each unique `EdgeKey` to a representative `Edge` from the
/// graph — first encounter wins on dedup, matching the build-time
/// `EdgeIdentity` dedup contract. Returning the original `Edge`
/// rather than a reconstructed view is what lets the diff output
/// preserve the full `ResolvedTarget::Unresolved { raw, reason }`
/// payload through set differencing.
fn edge_index(graph: &Graph) -> BTreeMap<EdgeKey, &Edge> {
    let mut idx = BTreeMap::new();
    for edge in graph.edges() {
        idx.entry(EdgeKey::from_edge(edge)).or_insert(edge);
    }
    idx
}

fn edge_ref_from(edge: &Edge) -> EdgeRef {
    EdgeRef {
        source: edge.source.clone(),
        target: edge.target.clone(),
        relation: edge.relation.clone(),
    }
}

/// Iterate over every tracked frontmatter field and emit a `FieldChange`
/// when before/after differ. Built-in fields are enumerated explicitly so
/// adding a new field to [`Node`] surfaces as a missed diff here.
fn collect_field_changes(b: &Node, a: &Node, out: &mut Vec<FieldChange>) {
    fn diff<T: PartialEq + serde::Serialize>(
        id: &str,
        field: &str,
        before: &T,
        after: &T,
        out: &mut Vec<FieldChange>,
    ) {
        if before != after {
            out.push(FieldChange {
                id: id.to_string(),
                field: field.to_string(),
                before: Some(serde_json::to_value(before).unwrap_or(serde_json::Value::Null)),
                after: Some(serde_json::to_value(after).unwrap_or(serde_json::Value::Null)),
            });
        }
    }

    let id = b.id.as_str();
    diff(id, "title", &b.title, &a.title, out);
    diff(
        id,
        "kind",
        &b.kind.as_str().to_string(),
        &a.kind.as_str().to_string(),
        out,
    );
    // status handled separately as a transition
    diff(id, "created", &b.created, &a.created, out);
    diff(id, "updated", &b.updated, &a.updated, out);
    diff(id, "reviewed", &b.reviewed, &a.reviewed, out);
    diff(id, "owner", &b.owner, &a.owner, out);
    diff(id, "supersedes", &b.supersedes, &a.supersedes, out);
    diff(id, "superseded_by", &b.superseded_by, &a.superseded_by, out);
    diff(id, "implements", &b.implements, &a.implements, out);
    diff(id, "related", &b.related, &a.related, out);
    diff(id, "tags", &b.tags, &a.tags, out);
    diff(id, "covers", &b.covers, &a.covers, out);
    diff(id, "orphan_ok", &b.orphan_ok, &a.orphan_ok, out);

    // attrs — per-key delta so callers see the field name precisely.
    let keys: BTreeSet<&str> = b
        .attrs
        .keys()
        .chain(a.attrs.keys())
        .map(String::as_str)
        .collect();
    for k in keys {
        let bv = b.attrs.get(k);
        let av = a.attrs.get(k);
        if bv != av {
            out.push(FieldChange {
                id: id.to_string(),
                field: k.to_string(),
                before: bv.cloned(),
                after: av.cloned(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Edge, Graph, Kind, ResolvedTarget, Status};
    use indexmap::IndexMap;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn n(id: &str, status: &str) -> Node {
        Node {
            id: id.into(),
            path: PathBuf::from(format!("{id}.md")),
            title: id.into(),
            kind: Kind::new("generic"),
            status: Status::new(status),
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
        }
    }

    fn build(nodes: &[Node], edges: Vec<Edge>) -> Graph {
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n.clone());
        }
        Graph::new(map, edges)
    }

    fn edge(source: &str, target: &str, relation: &str) -> Edge {
        Edge {
            source: source.into(),
            target: ResolvedTarget::Resolved { id: target.into() },
            relation: relation.into(),
            location: "L1".into(),
        }
    }

    #[test]
    fn identity_diff_is_empty() {
        let g = build(&[n("a", "active")], vec![]);
        let d = compute_diff(&g, &g);
        assert!(d.added_nodes.is_empty());
        assert!(d.removed_nodes.is_empty());
        assert!(d.status_transitions.is_empty());
        assert!(d.field_changes.is_empty());
    }

    #[test]
    fn detects_added_and_removed_nodes() {
        let before = build(&[n("a", "active")], vec![]);
        let after = build(&[n("a", "active"), n("b", "active")], vec![]);
        let d = compute_diff(&before, &after);
        assert_eq!(d.added_nodes.len(), 1);
        assert_eq!(d.added_nodes[0].id, "b");
        assert!(d.removed_nodes.is_empty());

        let d_rev = compute_diff(&after, &before);
        assert_eq!(d_rev.removed_nodes.len(), 1);
        assert_eq!(d_rev.removed_nodes[0].id, "b");
    }

    #[test]
    fn detects_status_transition() {
        let before = build(&[n("a", "active")], vec![]);
        let after = build(&[n("a", "archived")], vec![]);
        let d = compute_diff(&before, &after);
        assert_eq!(d.status_transitions.len(), 1);
        let t = &d.status_transitions[0];
        assert_eq!(t.id, "a");
        assert_eq!(t.from, "active");
        assert_eq!(t.to, "archived");
    }

    #[test]
    fn detects_field_change_on_title() {
        let mut before_n = n("a", "active");
        before_n.title = "Old".into();
        let mut after_n = n("a", "active");
        after_n.title = "New".into();
        let d = compute_diff(&build(&[before_n], vec![]), &build(&[after_n], vec![]));
        let titles: Vec<&str> = d
            .field_changes
            .iter()
            .filter(|c| c.field == "title")
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(titles, vec!["a"]);
    }

    #[test]
    fn detects_added_and_removed_edges() {
        let before = build(
            &[n("a", "active"), n("b", "active")],
            vec![edge("a", "b", "references")],
        );
        let after = build(
            &[n("a", "active"), n("b", "active")],
            vec![edge("a", "b", "related")],
        );
        let d = compute_diff(&before, &after);
        assert_eq!(d.added_edges.len(), 1);
        assert_eq!(d.added_edges[0].relation, "related");
        assert_eq!(d.removed_edges.len(), 1);
        assert_eq!(d.removed_edges[0].relation, "references");
    }

    #[test]
    fn diff_preserves_unresolved_reason_through_round_trip() {
        // Regression gate for the dedup-vs-output reconciliation: the
        // diff keys edges on `(source, raw, relation)` to collapse the
        // same logical reference regardless of resolver reason, but
        // the output must still carry the original `Unresolved.reason`
        // so the caller knows *why* the edge failed to resolve.
        let unresolved_edge = |source: &str, raw: &str, reason: &str| Edge {
            source: source.into(),
            target: ResolvedTarget::Unresolved {
                raw: raw.into(),
                reason: reason.into(),
            },
            relation: "references".into(),
            location: "L1".into(),
        };

        // `before` has no edges; `after` introduces an unresolved one
        // with a meaningful reason. The added edge in the diff must
        // carry that reason verbatim — not a blank placeholder.
        let before = build(&[n("a", "active")], vec![]);
        let after = build(
            &[n("a", "active")],
            vec![unresolved_edge(
                "a",
                "missing.md",
                "path not found in scope",
            )],
        );
        let d = compute_diff(&before, &after);
        assert_eq!(d.added_edges.len(), 1);
        match &d.added_edges[0].target {
            ResolvedTarget::Unresolved { raw, reason } => {
                assert_eq!(raw, "missing.md");
                assert_eq!(
                    reason, "path not found in scope",
                    "unresolved reason must survive the EdgeKey round-trip"
                );
            }
            other => panic!("expected unresolved target, got {other:?}"),
        }
    }
}
