//! Impact analysis: what depends on what a diff changed.
//!
//! Combines the structural [`compute_diff`] with dependency lookups to
//! answer "what could break if I merge this?" in one shot — a
//! *modified* node is paired with its transitive dependents (the
//! [`find_dependents`] walk over the after graph), a *removed* node
//! with the direct referrers that still point at it and now dangle
//! (references the same change repointed elsewhere are correctly
//! absent). Pure graph computation: no heuristics, no mutation,
//! deterministic for a given pair of graphs.

use std::collections::BTreeSet;

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
    /// change are correctly absent.
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
    /// Removed nodes that the *after* graph still references (dangling id
    /// references). The sharpest "this will break" signal — a removal whose
    /// every referrer was repointed does not appear here.
    pub likely_breaking: Vec<String>,
}

/// Analyse what changing `before` into `after` affects: each modified node
/// paired with its transitive dependents, and each removed node paired with
/// the documents that still dangle on its id in `after`.
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

    let mut impacted = Vec::new();
    let mut likely_breaking = Vec::new();

    for id in diff.touched_ids() {
        if added.contains(id.as_str()) {
            continue; // a brand-new node had no prior dependents
        }
        let (change, dependents) = if removed.contains(id.as_str()) {
            // A removal breaks only the references that still point at it in
            // `after` — references repointed by the same change are gone.
            let removed_path = before
                .node(&id)
                .map(|n| crate::path_guard::forward_string(&n.path))
                .unwrap_or_default();
            (
                ChangeKind::Removed,
                dangling_referrers(after, &id, &removed_path, extensions, relations),
            )
        } else {
            // `find_dependents` only errors on a missing node; the id is in
            // `after` by construction.
            match find_dependents(after, &id, max_depth, relations) {
                Ok(report) => (ChangeKind::Modified, report.dependents),
                Err(_) => continue,
            }
        };
        if dependents.is_empty() {
            continue; // changed, but nothing is affected
        }
        if matches!(change, ChangeKind::Removed) {
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
        if raw == removed_id {
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

    fn graph(nodes: &[&str], edges: Vec<Edge>) -> Graph {
        let mut map = IndexMap::new();
        for id in nodes {
            map.insert(id.to_string(), node(id));
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
    fn added_node_is_not_impacted() {
        let before = graph(&["a"], vec![]);
        let after = graph(&["a", "b"], vec![implements_edge("a", "b")]);

        let report = compute_impact(&before, &after, &[], None, &[]);

        assert!(report.impacted.iter().all(|e| e.id != "b"));
        assert!(report.likely_breaking.is_empty());
    }
}
