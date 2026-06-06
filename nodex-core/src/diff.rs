//! Structural diff between two graph snapshots.
//!
//! Pure transformation of two [`Graph`] values into a [`GraphDiff`]
//! describing every node addition / removal, edge addition / removal,
//! status transition, and frontmatter field change. No policy, no
//! heuristics — downstream callers (rule policies, CI gates, the CLI's
//! human-readable report) decide what to do with the delta.

use schemars::JsonSchema;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

use crate::model::{Annotation, Edge, Graph, Node, ResolvedTarget};
use crate::query::NodeRef;

/// A structural delta between graph A (the "before" snapshot) and
/// graph B (the "after" snapshot).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct GraphDiff {
    pub added_nodes: Vec<NodeRef>,
    pub removed_nodes: Vec<NodeRef>,
    pub added_edges: Vec<EdgeRef>,
    pub removed_edges: Vec<EdgeRef>,
    pub status_transitions: Vec<StatusTransition>,
    pub field_changes: Vec<FieldChange>,
    /// Body-text annotations that appear in `after` but not `before`.
    /// Identity = `(name, key, source, line)` — a moved
    /// marker (same pattern + key, different line) shows as removed
    /// from the old line and added on the new one, which is the
    /// honest delta for reviewers.
    pub added_annotations: Vec<Annotation>,
    pub removed_annotations: Vec<Annotation>,
    /// Per-node body-text deltas — one entry whenever the body
    /// fingerprint changed between `before` and `after`. Powers
    /// [`crate::rules::body_immutable`]: the rule consumes this slice
    /// instead of re-reading files. New / removed nodes never appear
    /// here (those are captured by `added_nodes` / `removed_nodes`);
    /// only ids present in both snapshots produce a [`BodyChange`].
    ///
    /// Not serialised — `git diff` already shows body text changes for
    /// any external reviewer that wants them, and the rule layer is
    /// the only in-tree consumer. Keeping the field internal-only
    /// avoids advertising an envelope axis that has no audience.
    #[serde(skip)]
    pub body_changes: Vec<BodyChange>,
}

/// A flat view of an [`Edge`] suitable for diff output. We re-emit the
/// target shape verbatim so downstream readers see exactly the same
/// `{ type, id }` / `{ type, raw, reason }` shape as in `graph.json`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EdgeRef {
    pub source: String,
    pub target: ResolvedTarget,
    pub relation: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StatusTransition {
    pub id: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
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

/// One per-node body delta — emitted only when the body fingerprint
/// changed between the "before" and "after" snapshots. Carries both
/// the whole-body hash (so `frozen`-mode rules just compare two
/// strings) and the per-line hash vectors (so `append_only`-mode
/// rules can decide prefix-equality without re-reading files).
///
/// Storing the hash vectors rather than the body text is the
/// principled trade-off: rules stay pure functions of
/// `(graph, config)`, and a corpus of 10k docs with 100 lines each
/// costs ~3 MB of fingerprint data — acceptable for the immutability
/// guarantees it underwrites.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BodyChange {
    pub id: String,
    pub before_hash: String,
    pub after_hash: String,
    pub before_lines_hash: Vec<String>,
    pub after_lines_hash: Vec<String>,
}

impl GraphDiff {
    /// The status a still-present node held in the "before" snapshot.
    ///
    /// Derived from the transition stream: a status change is a
    /// [`StatusTransition`] whose `from` is the prior status; absent a
    /// transition the status was unchanged, so `current` (the after
    /// status) is also the prior one. Lets a diff-aware rule reason about
    /// "what was true before this edit" — e.g. immutability that applies
    /// only to a doc that was *already* terminal, so the very write that
    /// first makes it terminal is not retroactively rejected.
    pub fn before_status<'a>(&'a self, id: &str, current: &'a str) -> &'a str {
        self.status_transitions
            .iter()
            .find(|t| t.id == id)
            .map_or(current, |t| t.from.as_str())
    }

    /// The kind a still-present node held in the "before" snapshot.
    /// `kind` is a tracked frontmatter field, so a kind change surfaces as
    /// a [`FieldChange`]; absent one the kind was unchanged and `current`
    /// is also the prior kind. Lets kind-scoped rules gate on the kind the
    /// node had *before* the edit, not after.
    pub fn before_kind<'a>(&'a self, id: &str, current: &'a str) -> &'a str {
        self.field_changes
            .iter()
            .find(|c| c.id == id && c.field == "kind")
            .and_then(|c| c.before.as_ref())
            .and_then(serde_json::Value::as_str)
            .unwrap_or(current)
    }
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
    // Body changes share the same intersection: the whole-body hash
    // changing on a node that exists in both snapshots is what
    // `body_immutable` reacts to.
    let mut transitions = Vec::new();
    let mut field_changes = Vec::new();
    let mut body_changes = Vec::new();
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
        if b.body_hash != a.body_hash {
            body_changes.push(BodyChange {
                id: (*id).to_string(),
                before_hash: b.body_hash.clone(),
                after_hash: a.body_hash.clone(),
                before_lines_hash: b.body_lines_hash.clone(),
                after_lines_hash: a.body_lines_hash.clone(),
            });
        }
    }

    field_changes.sort_by(|x, y| x.id.cmp(&y.id).then_with(|| x.field.cmp(&y.field)));
    body_changes.sort_by(|x, y| x.id.cmp(&y.id));

    let (added_annotations, removed_annotations) =
        diff_annotations(before.annotations(), after.annotations());

    GraphDiff {
        added_nodes: added,
        removed_nodes: removed,
        added_edges,
        removed_edges,
        status_transitions: transitions,
        field_changes,
        added_annotations,
        removed_annotations,
        body_changes,
    }
}

/// Set-difference annotations on the 4-tuple identity
/// `(name, key, source, line)`. Output is sorted by the
/// same key so two runs on the same inputs produce byte-identical
/// JSON, in line with the rest of `compute_diff`.
fn diff_annotations(
    before: &[Annotation],
    after: &[Annotation],
) -> (Vec<Annotation>, Vec<Annotation>) {
    let before_set: BTreeSet<AnnotationKey> = before.iter().map(AnnotationKey::from_ref).collect();
    let after_set: BTreeSet<AnnotationKey> = after.iter().map(AnnotationKey::from_ref).collect();

    let mut added: Vec<Annotation> = after
        .iter()
        .filter(|a| !before_set.contains(&AnnotationKey::from_ref(a)))
        .cloned()
        .collect();
    let mut removed: Vec<Annotation> = before
        .iter()
        .filter(|a| !after_set.contains(&AnnotationKey::from_ref(a)))
        .cloned()
        .collect();

    let sort_key = |a: &Annotation, b: &Annotation| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.key.cmp(&b.key))
            .then_with(|| a.source.cmp(&b.source))
            .then_with(|| a.line.cmp(&b.line))
    };
    added.sort_by(sort_key);
    removed.sort_by(sort_key);
    (added, removed)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct AnnotationKey {
    name: String,
    key: String,
    source: String,
    line: usize,
}

impl AnnotationKey {
    fn from_ref(a: &Annotation) -> Self {
        Self {
            name: a.name.clone(),
            key: a.key.clone(),
            source: a.source.clone(),
            line: a.line,
        }
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

/// Serialised `Node` keys that never become a [`FieldChange`]. `id`
/// surfaces as a node add/remove (it is the snapshot join key, so a
/// changed id is a different node) and `status` as a [`StatusTransition`];
/// `path` is path-derived; the body fingerprints are reported as a
/// [`BodyChange`]; and `attrs` is expanded per-key below so callers get
/// the precise field name. Every *other* serialised field is authored
/// frontmatter — so adding a new frontmatter field to [`Node`] is diffed
/// automatically, and the only thing a new *internal* field must do is
/// join this list (guarded by `field_change_field_universe_is_exhaustive`).
const FIELDS_EXCLUDED_FROM_FIELD_CHANGES: &[&str] = &[
    "id",
    "path",
    "status",
    "attrs",
    "body_hash",
    "body_lines_hash",
];

/// Project a node onto its authored frontmatter fields as a JSON map.
/// Derived from `Node`'s own serialisation, so the field set is whatever
/// the model declares minus [`FIELDS_EXCLUDED_FROM_FIELD_CHANGES`] — no hand-kept
/// parallel list to drift. Empty collections / unset options are absent
/// (the model's `skip_serializing_if`), exactly as everywhere else a
/// node serialises.
fn frontmatter_fields(node: &Node) -> serde_json::Map<String, serde_json::Value> {
    let serde_json::Value::Object(mut map) =
        serde_json::to_value(node).expect("Node is JSON-serialisable")
    else {
        unreachable!("Node serialises as a JSON object");
    };
    for field in FIELDS_EXCLUDED_FROM_FIELD_CHANGES {
        map.remove(*field);
    }
    map
}

/// Emit a `FieldChange` for every authored frontmatter field whose value
/// differs between snapshots. `attrs` (project-specific frontmatter) is
/// expanded per-key so callers see the exact field name rather than one
/// opaque `attrs` blob.
fn collect_field_changes(b: &Node, a: &Node, out: &mut Vec<FieldChange>) {
    let id = b.id.as_str();

    let bf = frontmatter_fields(b);
    let af = frontmatter_fields(a);
    let fields: BTreeSet<&str> = bf.keys().chain(af.keys()).map(String::as_str).collect();
    for field in fields {
        let before = bf.get(field);
        let after = af.get(field);
        if before != after {
            out.push(FieldChange {
                id: id.to_string(),
                field: field.to_string(),
                before: before.cloned(),
                after: after.cloned(),
            });
        }
    }

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
            body_hash: String::new(),
            body_lines_hash: Vec::new(),
        }
    }

    fn build(nodes: &[Node], edges: Vec<Edge>) -> Graph {
        build_with_anns(nodes, edges, vec![])
    }

    fn build_with_anns(nodes: &[Node], edges: Vec<Edge>, anns: Vec<Annotation>) -> Graph {
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n.clone());
        }
        Graph::new(map, edges, anns, vec![])
    }

    fn ann(source: &str, pattern: &str, key: &str, line: usize) -> Annotation {
        Annotation {
            source: source.into(),
            name: pattern.into(),
            key: key.into(),
            line,
        }
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

    #[test]
    fn detects_added_and_removed_annotations() {
        // `before` has marker (promotes, spec-x) at line 1; `after`
        // moves it to line 5 and adds (research, q-1). The line change
        // must surface as removed-from-1 + added-at-5 (4-tuple identity
        // includes line); the new pattern shows as a pure addition.
        let before = build_with_anns(
            &[n("a", "active")],
            vec![],
            vec![ann("a", "promotes", "spec-x", 1)],
        );
        let after = build_with_anns(
            &[n("a", "active")],
            vec![],
            vec![
                ann("a", "promotes", "spec-x", 5),
                ann("a", "research", "q-1", 9),
            ],
        );
        let d = compute_diff(&before, &after);
        let added: Vec<(&str, &str, usize)> = d
            .added_annotations
            .iter()
            .map(|a| (a.name.as_str(), a.key.as_str(), a.line))
            .collect();
        let removed: Vec<(&str, &str, usize)> = d
            .removed_annotations
            .iter()
            .map(|a| (a.name.as_str(), a.key.as_str(), a.line))
            .collect();
        assert_eq!(
            added,
            vec![("promotes", "spec-x", 5), ("research", "q-1", 9)]
        );
        assert_eq!(removed, vec![("promotes", "spec-x", 1)]);
    }

    #[test]
    fn identical_annotations_produce_empty_diff() {
        let g = build_with_anns(
            &[n("a", "active")],
            vec![],
            vec![ann("a", "promotes", "x", 1)],
        );
        let d = compute_diff(&g, &g);
        assert!(d.added_annotations.is_empty());
        assert!(d.removed_annotations.is_empty());
    }

    // ─── body_changes ──────────────────────────────────────────────────
    //
    // `body_immutable` consumes `body_changes` directly; the diff must
    // produce one entry per node whose body fingerprint changed, never
    // for added / removed nodes (those are captured by the node-level
    // sets), and the entry must carry both whole-body and per-line
    // hashes so frozen / append-only modes can dispatch without
    // re-reading files.

    fn n_with_body(id: &str, body_hash: &str, lines: &[&str]) -> Node {
        let mut node = n(id, "active");
        node.body_hash = body_hash.to_string();
        node.body_lines_hash = lines.iter().map(|s| s.to_string()).collect();
        node
    }

    #[test]
    fn body_change_emitted_when_whole_body_hash_differs() {
        let before = build(&[n_with_body("a", "h-old", &["l1-old"])], vec![]);
        let after = build(&[n_with_body("a", "h-new", &["l1-new"])], vec![]);
        let d = compute_diff(&before, &after);
        assert_eq!(d.body_changes.len(), 1);
        let c = &d.body_changes[0];
        assert_eq!(c.id, "a");
        assert_eq!(c.before_hash, "h-old");
        assert_eq!(c.after_hash, "h-new");
        assert_eq!(c.before_lines_hash, vec!["l1-old"]);
        assert_eq!(c.after_lines_hash, vec!["l1-new"]);
    }

    #[test]
    fn body_change_omitted_when_hash_unchanged() {
        // Identical fingerprint → no entry. Other fields can still
        // change without inflating `body_changes`.
        let same = build(&[n_with_body("a", "h", &["l1"])], vec![]);
        let d = compute_diff(&same, &same);
        assert!(
            d.body_changes.is_empty(),
            "identical body must not emit a BodyChange entry"
        );
    }

    #[test]
    fn body_change_skips_added_and_removed_nodes() {
        // A node that appears only in `after` is captured by
        // `added_nodes`; it must not also appear in `body_changes`,
        // which would double-count and confuse downstream rules.
        let before = build(&[], vec![]);
        let after = build(&[n_with_body("a", "h", &["l1"])], vec![]);
        let d = compute_diff(&before, &after);
        assert_eq!(d.added_nodes.len(), 1);
        assert!(
            d.body_changes.is_empty(),
            "new node must not appear in body_changes"
        );
    }

    // ─── field-change completeness ─────────────────────────────────────
    //
    // `collect_field_changes` derives the diffable field set from
    // `Node`'s own serialisation minus `FIELDS_EXCLUDED_FROM_FIELD_CHANGES`, so a new
    // *frontmatter* field is diffed automatically. The only way to get it
    // wrong is to add an *internal* field and forget to exclude it. This
    // test fails the instant any `Node` field is added without being
    // classified — frontmatter (diffed) or internal (excluded).

    #[test]
    fn field_change_field_universe_is_exhaustive() {
        let date = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let full = Node {
            id: "x".into(),
            path: PathBuf::from("docs/x.md"),
            title: "t".into(),
            kind: Kind::new("adr"),
            status: Status::new("active"),
            created: Some(date),
            updated: Some(date),
            reviewed: Some(date),
            owner: Some("o".into()),
            supersedes: vec!["a".into()],
            superseded_by: Some("b".into()),
            implements: vec!["c".into()],
            related: vec!["d".into()],
            tags: vec!["e".into()],
            covers: vec!["f".into()],
            orphan_ok: true,
            attrs: BTreeMap::from([("priority".to_string(), serde_json::json!("high"))]),
            body_hash: "h".into(),
            body_lines_hash: vec!["l".into()],
        };

        let serde_json::Value::Object(map) = serde_json::to_value(&full).unwrap() else {
            panic!("Node serialises as an object");
        };
        let serialised: BTreeSet<&str> = map.keys().map(String::as_str).collect();

        // The authored frontmatter fields the diff is expected to surface.
        let expected_frontmatter: BTreeSet<&str> = [
            "title",
            "kind",
            "created",
            "updated",
            "reviewed",
            "owner",
            "supersedes",
            "superseded_by",
            "implements",
            "related",
            "tags",
            "covers",
            "orphan_ok",
        ]
        .into_iter()
        .collect();
        let internal: BTreeSet<&str> = FIELDS_EXCLUDED_FROM_FIELD_CHANGES.iter().copied().collect();

        // Every serialised field must be classified as exactly one of the
        // two — no overlap, nothing left over.
        assert!(
            expected_frontmatter.is_disjoint(&internal),
            "a field cannot be both frontmatter and internal"
        );
        let classified: BTreeSet<&str> = expected_frontmatter.union(&internal).copied().collect();
        assert_eq!(
            serialised, classified,
            "every serialised Node field must be classified frontmatter-vs-internal; \
             a new field here means updating expected_frontmatter or FIELDS_EXCLUDED_FROM_FIELD_CHANGES"
        );

        // And the projection used by the diff must be exactly the
        // frontmatter set — not leaking an internal field.
        let projection = frontmatter_fields(&full);
        let projected: BTreeSet<&str> = projection.keys().map(String::as_str).collect();
        assert_eq!(
            projected, expected_frontmatter,
            "frontmatter_fields must project exactly the authored frontmatter set"
        );
    }
}
