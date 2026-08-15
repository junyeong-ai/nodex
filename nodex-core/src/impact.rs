//! Impact analysis: what depends on what a diff changed.
//!
//! Combines the structural [`compute_diff`] with dependency lookups to
//! answer "what could break if I merge this?" in one shot — a
//! *modified* node is paired with its transitive dependents (the
//! [`find_dependents`] walk over the after graph), a *removed* node with
//! the direct referrers that still point at it and now dangle
//! (references the same change repointed elsewhere are correctly
//! absent), and a *moved* node with both: what still depends on it where
//! it is, and what still points at where it was. Pure graph computation:
//! no heuristics, no mutation, deterministic for a given pair of graphs.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::Serialize;

use crate::diff::{EdgeRef, GraphDiff, compute_diff};
use crate::model::{Graph, ResolvedTarget};
use crate::query::NodeRef;
use crate::query::dependents::{DependentEntry, find_dependents};

/// How a node changed between the two snapshots, as far as impact goes.
#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// Present in `before`, gone in `after` — its dependents now dangle.
    Removed,
    /// Present in both under a different path — a move that kept the
    /// record's id. A path is how every relative reference to a document
    /// resolves, so a move reaches the dependents an edit does, and a
    /// path-bound reference the move did not carry along now dangles.
    Moved,
    /// Status, a frontmatter field, or the body changed in place.
    Modified,
}

/// One changed node paired with the documents that depend on it.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ImpactEntry {
    pub id: String,
    pub change: ChangeKind,
    /// For a **modified** node: its transitive dependents in the *after*
    /// graph. For a **removed** node: the documents in the *after* graph
    /// that still reference its id and now dangle (each a direct,
    /// single-hop referrer) — references repointed elsewhere by the same
    /// change are correctly absent. For a **moved** node: both — its
    /// transitive dependents where it now is, then the direct referrers
    /// still pointing at where it was; a document in both is listed once,
    /// under the edge that still binds it.
    pub dependents: Vec<DependentEntry>,
}

/// The result of [`compute_impact`].
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ImpactReport {
    pub diff: GraphDiff,
    /// Per changed node with at least one affected dependent. Added nodes,
    /// and changes that affect nobody, are omitted — the report is the
    /// *impact*; the full delta is in `diff`.
    pub impacted: Vec<ImpactEntry>,
    /// Removed nodes the *after* graph still references, and moved nodes
    /// still referenced at their old path — a reference that now dangles
    /// either way. The sharpest "this will break" signal: a removal or move
    /// whose every referrer was repointed does not appear here.
    pub likely_breaking: Vec<String>,
}

/// Analyse what changing `before` into `after` affects: each modified node
/// paired with its transitive dependents, each removed node paired with
/// the documents that still dangle on its id in `after`, and each moved
/// node paired with both.
///
/// `relations` restricts which edge relations are followed (empty = every
/// relation); `max_depth` bounds the transitive reach for modified nodes
/// (`None` = until exhausted). `extensions` is the project's
/// `parser.extensions`, used to recognise extension-less references to a
/// removed file.
pub fn compute_impact(
    before: &Graph,
    after: &Graph,
    relations: &[String],
    max_depth: Option<u32>,
    extensions: &[String],
) -> ImpactReport {
    let diff = compute_diff(before, after);

    let removed: BTreeSet<&str> = diff.removed_nodes.iter().map(|n| n.id.as_str()).collect();
    let added: BTreeSet<&str> = diff.added_nodes.iter().map(|n| n.id.as_str()).collect();
    let moved_from: BTreeMap<&str, &str> = diff
        .path_changes
        .iter()
        .map(|c| (c.id.as_str(), c.from.as_str()))
        .collect();

    let mut impacted = Vec::new();
    let mut likely_breaking = Vec::new();

    for id in diff.touched_ids() {
        if added.contains(id.as_str()) {
            continue; // a brand-new node had no prior dependents
        }
        let (change, dependents, breaking) = if removed.contains(id.as_str()) {
            // A removal breaks only the references that still point at it in
            // `after` — references repointed by the same change are gone.
            let removed_path = before
                .node(&id)
                .map(|n| crate::path_guard::forward_string(&n.path))
                .unwrap_or_default();
            (
                ChangeKind::Removed,
                dangling_referrers(after, &id, &removed_path, extensions, relations),
                true,
            )
        } else {
            // `find_dependents` only errors on a missing node; the id is in
            // `after` by construction.
            let Ok(report) = find_dependents(after, &id, max_depth, relations) else {
                continue;
            };
            match moved_from.get(id.as_str()) {
                // A move keeps what depends on the document where it now is
                // and strands what still points at where it was. A document
                // that does both — an id relation and a stranded link — is
                // listed once, under the edge that still binds it; the
                // stranded edge is what makes the move breaking either way.
                Some(from) => {
                    let mut dependents = report.dependents;
                    let stranded = dangling_referrers(after, &id, from, extensions, relations);
                    let breaking = !stranded.is_empty();
                    for referrer in stranded {
                        if !dependents.iter().any(|d| d.node.id == referrer.node.id) {
                            dependents.push(referrer);
                        }
                    }
                    (ChangeKind::Moved, dependents, breaking)
                }
                None => (ChangeKind::Modified, report.dependents, false),
            }
        };
        if dependents.is_empty() {
            continue; // changed, but nothing is affected
        }
        if breaking {
            likely_breaking.push(id.clone());
        }
        impacted.push(ImpactEntry {
            id,
            change,
            dependents,
        });
    }

    ImpactReport {
        diff,
        impacted,
        likely_breaking,
    }
}

/// Documents in `after` whose unresolved edges still reference the removed
/// node — by id (frontmatter relation) or by path (a body link to the gone
/// file) — and therefore dangle. Each is a direct, single-hop referrer.
/// `relations` (empty = all) filters by edge relation.
fn dangling_referrers(
    after: &Graph,
    removed_id: &str,
    removed_path: &str,
    extensions: &[String],
    relations: &[String],
) -> Vec<DependentEntry> {
    let still_references = |edge: &crate::model::Edge| {
        let ResolvedTarget::Unresolved { raw, .. } = &edge.target else {
            return false;
        };
        // An id reference (a frontmatter relation) to the removed node —
        // gated on resolution mode. `covers` is path-only and never binds by
        // id, so an id-equal `covers` token is a coincidence, not a dangle;
        // `is_document_ref_relation` excludes exactly that one relation,
        // mirroring the path branch below so both branches agree on which
        // relations resolve by id.
        if crate::model::edge::is_document_ref_relation(&edge.relation) && raw == removed_id {
            return true;
        }
        // A body link to the removed file: resolve `raw` against the linking
        // document's directory and compare to the removed node's path, using
        // the same candidate ladder the build does. `covers` is path-only —
        // a closed, code-owned dispatch, since only the frontmatter field
        // can produce the relation.
        let source_dir = after
            .node(&edge.source)
            .and_then(|n| n.path.parent())
            .unwrap_or_else(|| std::path::Path::new(""));
        crate::builder::resolver::reference_resolves_to(
            raw,
            source_dir,
            removed_path,
            extensions,
            crate::model::edge::is_document_ref_relation(&edge.relation),
        )
    };
    after
        .edges()
        .iter()
        .filter(|edge| still_references(edge))
        .filter(|edge| relations.is_empty() || relations.iter().any(|r| r == &edge.relation))
        .filter_map(|edge| {
            let referrer = after.node(&edge.source)?;
            Some(DependentEntry {
                node: NodeRef::from_node(referrer),
                hops: 1,
                via: vec![EdgeRef {
                    source: edge.source.clone(),
                    target: edge.target.clone(),
                    relation: edge.relation.clone(),
                }],
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Edge, Kind, Node, ResolvedTarget, Status};
    use indexmap::IndexMap;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn node(id: &str) -> Node {
        Node {
            id: id.into(),
            path: PathBuf::from(format!("docs/{id}.md")),
            title: id.into(),
            kind: Kind::new("generic"),
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
            content_hash: String::new(),
            parse_issues: vec![],
            inferred_fields: vec![],
        }
    }

    fn implements_edge(source: &str, target: &str) -> Edge {
        Edge {
            source: source.into(),
            target: ResolvedTarget::resolved(target),
            relation: "implements".into(),
            location: "frontmatter:implements".into(),
        }
    }

    fn dangling_implements(source: &str, raw: &str) -> Edge {
        Edge {
            source: source.into(),
            target: ResolvedTarget::unresolved(raw, crate::model::UnresolvedCause::IdNotFound),
            relation: "implements".into(),
            location: "frontmatter:implements".into(),
        }
    }

    fn dangling_reference(source: &str, raw: &str) -> Edge {
        Edge {
            source: source.into(),
            target: ResolvedTarget::unresolved(raw, crate::model::UnresolvedCause::Missing),
            relation: "references".into(),
            location: "L1".into(),
        }
    }

    fn dangling_covers(source: &str, raw: &str) -> Edge {
        Edge {
            source: source.into(),
            target: ResolvedTarget::unresolved(raw, crate::model::UnresolvedCause::Missing),
            relation: "covers".into(),
            location: "frontmatter:covers".into(),
        }
    }

    fn node_at(id: &str, path: &str) -> Node {
        Node {
            path: PathBuf::from(path),
            ..node(id)
        }
    }

    fn graph(nodes: &[&str], edges: Vec<Edge>) -> Graph {
        graph_with(nodes.iter().map(|id| node(id)).collect(), edges)
    }

    fn graph_with(nodes: Vec<Node>, edges: Vec<Edge>) -> Graph {
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n);
        }
        Graph::new(
            map,
            edges,
            vec![],
            vec![],
            vec![],
            crate::model::GraphMeta::default(),
        )
    }

    #[test]
    fn removed_file_referenced_by_path_is_likely_breaking() {
        // `b` (docs/b.md) body-links to `a` by path (`a.md`). `a` (docs/a.md)
        // is removed but the link remains — a *path* dangler that must be
        // flagged even though the raw target is a path, not the node id.
        let before = graph(&["a", "b"], vec![]);
        let after = graph(&["b"], vec![dangling_reference("b", "a.md")]);

        let report = compute_impact(&before, &after, &[], None, &[".md".to_string()]);

        assert_eq!(report.likely_breaking, vec!["a".to_string()]);
        assert_eq!(
            report.impacted.iter().find(|e| e.id == "a").map(|e| e
                .dependents
                .iter()
                .map(|d| d.node.id.as_str())
                .collect::<Vec<_>>()),
            Some(vec!["b"])
        );
    }

    #[test]
    fn removed_node_still_referenced_is_likely_breaking() {
        // before: impl implements spec. after: spec removed, impl still
        // declares `implements: [spec]` → a dangling reference in `after`.
        let before = graph(&["spec", "impl"], vec![implements_edge("impl", "spec")]);
        let after = graph(&["impl"], vec![dangling_implements("impl", "spec")]);

        let report = compute_impact(&before, &after, &[], None, &[]);

        assert_eq!(report.likely_breaking, vec!["spec".to_string()]);
        let entry = report
            .impacted
            .iter()
            .find(|e| e.id == "spec")
            .expect("spec impacted");
        assert!(matches!(entry.change, ChangeKind::Removed));
        assert_eq!(
            entry
                .dependents
                .iter()
                .map(|d| d.node.id.as_str())
                .collect::<Vec<_>>(),
            vec!["impl"]
        );
    }

    #[test]
    fn removed_but_retargeted_is_not_breaking() {
        // The same change that removes `spec` repoints impl onto `spec2`,
        // so nothing dangles on `spec` in `after` — it must NOT be flagged.
        let before = graph(
            &["spec", "spec2", "impl"],
            vec![implements_edge("impl", "spec")],
        );
        let after = graph(&["spec2", "impl"], vec![implements_edge("impl", "spec2")]);

        let report = compute_impact(&before, &after, &[], None, &[]);

        assert!(
            report.likely_breaking.is_empty(),
            "retargeted removal must not be likely_breaking: {:?}",
            report.likely_breaking
        );
        assert!(report.impacted.iter().all(|e| e.id != "spec"));
    }

    #[test]
    fn removed_leaf_with_no_dependents_is_not_flagged() {
        let before = graph(&["a", "b"], vec![]);
        let after = graph(&["a"], vec![]);

        let report = compute_impact(&before, &after, &[], None, &[]);

        assert!(report.likely_breaking.is_empty());
        assert!(report.impacted.is_empty());
    }

    #[test]
    fn covers_id_collision_is_not_breaking_but_covers_path_dangle_is() {
        // `covers` is path-only: it never binds by id. `b` (src/b.md) covers
        // a code path whose token coincides with the removed node id `foo`,
        // so removing the node `foo` breaks nothing about b's covered path —
        // it must NOT be flagged (the id-match branch is gated on resolution
        // mode). `d` (docs/d.md) covers the removed FILE by path
        // (`bar.md` → docs/bar.md): a real path dangle that stays flagged via
        // the path probe.
        let before = graph_with(
            vec![
                node_at("foo", "docs/foo.md"),
                node_at("bar", "docs/bar.md"),
                node_at("b", "src/b.md"),
                node_at("d", "docs/d.md"),
            ],
            vec![],
        );
        let after = graph_with(
            vec![node_at("b", "src/b.md"), node_at("d", "docs/d.md")],
            vec![dangling_covers("b", "foo"), dangling_covers("d", "bar.md")],
        );

        let report = compute_impact(&before, &after, &[], None, &[".md".to_string()]);

        assert_eq!(report.likely_breaking, vec!["bar".to_string()]);
        assert!(
            !report.likely_breaking.contains(&"foo".to_string()),
            "a covers token coinciding with a removed id is not a dangle: {:?}",
            report.likely_breaking
        );
    }

    /// A move the references followed — an id relation, or a link the
    /// rename rewrote — keeps its dependents where the document now is,
    /// and strands nothing: reported, not breaking.
    #[test]
    fn a_move_its_references_followed_keeps_its_dependents_and_breaks_nothing() {
        let before = graph_with(
            vec![node_at("a", "docs/a.md"), node("b")],
            vec![implements_edge("b", "a")],
        );
        let after = graph_with(
            vec![node_at("a", "docs/moved/a.md"), node("b")],
            vec![implements_edge("b", "a")],
        );

        let report = compute_impact(&before, &after, &[], None, &[]);

        let moved = report
            .impacted
            .iter()
            .find(|e| e.id == "a")
            .expect("the moved node is impacted");
        assert!(matches!(moved.change, ChangeKind::Moved));
        let ids: Vec<&str> = moved
            .dependents
            .iter()
            .map(|d| d.node.id.as_str())
            .collect();
        assert_eq!(ids, vec!["b"]);
        assert!(
            report.likely_breaking.is_empty(),
            "nothing points at where it was"
        );
    }

    /// A bare move strands the path-bound reference to where the document
    /// was: the referrer is a dependent of the move, and the move is
    /// likely breaking — the same reading a removal gets, because to that
    /// reference the document is gone from where it looked.
    #[test]
    fn a_move_that_strands_a_path_reference_is_likely_breaking() {
        let before = graph_with(
            vec![node_at("a", "docs/a.md"), node_at("b", "docs/b.md")],
            vec![Edge {
                source: "b".into(),
                target: ResolvedTarget::resolved("a"),
                relation: "references".into(),
                location: "L1".into(),
            }],
        );
        let after = graph_with(
            vec![node_at("a", "docs/moved/a.md"), node_at("b", "docs/b.md")],
            vec![dangling_reference("b", "a.md")],
        );

        let report = compute_impact(&before, &after, &[], None, &["md".into()]);

        let moved = report
            .impacted
            .iter()
            .find(|e| e.id == "a")
            .expect("the moved node is impacted");
        assert!(matches!(moved.change, ChangeKind::Moved));
        let ids: Vec<&str> = moved
            .dependents
            .iter()
            .map(|d| d.node.id.as_str())
            .collect();
        assert_eq!(ids, vec!["b"], "the stranded referrer is the dependent");
        assert_eq!(report.likely_breaking, vec!["a"]);
    }

    #[test]
    fn added_node_is_not_impacted() {
        let before = graph(&["a"], vec![]);
        let after = graph(&["a", "b"], vec![implements_edge("a", "b")]);

        let report = compute_impact(&before, &after, &[], None, &[]);

        assert!(report.impacted.iter().all(|e| e.id != "b"));
        assert!(report.likely_breaking.is_empty());
    }
}
