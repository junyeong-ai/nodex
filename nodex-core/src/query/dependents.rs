//! Transitive reverse traversal: every doc that ultimately reaches
//! the root via an incoming edge chain.
//!
//! Distinct from `query backlinks` (1-hop reverse) and `query
//! neighborhood` (undirected BFS) — this query answers the "what
//! breaks if I change X?" question, which is *directional* (we want
//! ancestors-of-X in the dependency direction) and *transitive*
//! (depth N, not just 1). Pure graph traversal; no policy, no
//! heuristics.

use schemars::JsonSchema;
use serde::Serialize;
use std::collections::{BTreeSet, VecDeque};

use super::NodeRef;
use crate::diff::EdgeRef;
use crate::error::Result;
use crate::model::{Graph, ResolvedTarget};

/// One node whose dependency chain ultimately includes the root, with
/// the shortest path (in BFS hops) and a witness chain.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DependentEntry {
    #[serde(flatten)]
    pub node: NodeRef,
    pub hops: u32,
    /// One edge per hop, from the dependent down to the root.
    /// `via[0].source == self.node.id`; `via.last().target` resolves to
    /// `root_id`. Captures *one* shortest path — when multiple exist
    /// (a node reaches the root through several distinct ancestors)
    /// the first BFS-discovered one wins, which is deterministic for
    /// a given graph by virtue of the sorted-id seeding.
    pub via: Vec<EdgeRef>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DependentsReport {
    pub root_id: String,
    /// `None` when the caller did not bound the depth.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    /// Empty when unrestricted. Carries the *requested* filter so the
    /// envelope is self-describing — a `dependents = []` result reads
    /// differently when relations restricted the search vs when the
    /// graph genuinely has nothing.
    pub relations: Vec<String>,
    pub dependents: Vec<DependentEntry>,
}

/// Every node that transitively depends on `id`, expanding only along
/// incoming edges (the reverse direction). Self-loops are skipped —
/// they don't represent external dependency.
///
/// - `id` must be a known node; returns [`crate::error::Error::MissingNode`]
///   otherwise.
/// - `max_depth = None` walks until the frontier is exhausted; with a
///   bound, expansion stops after exactly that many hops.
/// - `relations` filters which edge relations the traversal follows.
///   Empty list = follow every relation. The caller is expected to have
///   validated the values against [`crate::config::Config::known_relations`]
///   before calling — this function does *not* re-validate, the same
///   way `find_neighborhood` doesn't re-check `depth >= 0`.
pub fn find_dependents(
    graph: &Graph,
    id: &str,
    max_depth: Option<u32>,
    relations: &[String],
) -> Result<DependentsReport> {
    graph.require_node(id)?;

    let relation_filter: Option<BTreeSet<&str>> = if relations.is_empty() {
        None
    } else {
        Some(relations.iter().map(String::as_str).collect())
    };

    // BFS over reverse edges. Visited tracks reach to avoid revisits
    // (also defends against any unsupervised cycle). `predecessor`
    // records, for each reached node, the (parent_in_BFS, edge) that
    // first discovered it — used to reconstruct the via chain.
    let mut visited: BTreeSet<String> = BTreeSet::new();
    visited.insert(id.to_string());
    let mut predecessor: std::collections::BTreeMap<String, (String, EdgeRef)> =
        std::collections::BTreeMap::new();
    let mut hops: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    let mut frontier: VecDeque<(String, u32)> = VecDeque::new();
    frontier.push_back((id.to_string(), 0));

    while let Some((current, depth)) = frontier.pop_front() {
        if let Some(bound) = max_depth
            && depth >= bound
        {
            continue;
        }
        // Reverse step: every external incoming edge points *from*
        // a dependent *to* `current`. The dependent is the edge's
        // source.
        for edge in graph.external_incoming_edges(&current) {
            if relation_filter
                .as_ref()
                .is_some_and(|f| !f.contains(edge.relation.as_str()))
            {
                continue;
            }
            let parent = edge.source.clone();
            if visited.contains(&parent) {
                continue;
            }
            visited.insert(parent.clone());
            let edge_ref = EdgeRef {
                source: edge.source.clone(),
                target: ResolvedTarget::resolved(&current),
                relation: edge.relation.clone(),
            };
            predecessor.insert(parent.clone(), (current.clone(), edge_ref));
            hops.insert(parent.clone(), depth + 1);
            frontier.push_back((parent, depth + 1));
        }
    }

    let mut dependents: Vec<DependentEntry> = visited
        .iter()
        // Drop the root (it isn't a dependent of itself) and any
        // visited string that turned out not to be a graph node —
        // the build pipeline guarantees every `Edge.source` resolves,
        // but the rule is defensive against any future partial-state
        // input, matching the discipline `BodyLineRule` uses on
        // `body_line_matches_for_rule`.
        .filter(|n| *n != id)
        .filter_map(|node| {
            let resolved = graph.node(node)?;
            Some(DependentEntry {
                node: NodeRef::from_node(resolved),
                hops: *hops
                    .get(node)
                    .expect("invariant: every visited non-root node was assigned a hop count"),
                via: reconstruct_via(node, &predecessor),
            })
        })
        .collect();

    dependents.sort_by(|a, b| a.hops.cmp(&b.hops).then_with(|| a.node.id.cmp(&b.node.id)));

    Ok(DependentsReport {
        root_id: id.to_string(),
        max_depth,
        relations: relations.to_vec(),
        dependents,
    })
}

fn reconstruct_via(
    start: &str,
    predecessor: &std::collections::BTreeMap<String, (String, EdgeRef)>,
) -> Vec<EdgeRef> {
    let mut path = Vec::new();
    let mut cursor = start.to_string();
    while let Some((next, edge)) = predecessor.get(&cursor) {
        path.push(edge.clone());
        cursor = next.clone();
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Edge, Kind, Node, ResolvedTarget, Status};
    use indexmap::IndexMap;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn n(id: &str) -> Node {
        Node {
            id: id.into(),
            path: PathBuf::from(format!("{id}.md")),
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

    fn edge(src: &str, dst: &str, rel: &str) -> Edge {
        Edge {
            source: src.into(),
            target: ResolvedTarget::resolved(dst),
            relation: rel.into(),
            location: "test".into(),
        }
    }

    fn graph(ids: &[&str], edges: Vec<Edge>) -> Graph {
        let mut map = IndexMap::new();
        for id in ids {
            map.insert((*id).to_string(), n(id));
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
    fn unknown_root_errors() {
        let g = graph(&["a"], vec![]);
        let err = find_dependents(&g, "ghost", None, &[]).unwrap_err();
        assert!(matches!(err, crate::error::Error::MissingNode(_)));
    }

    #[test]
    fn isolated_node_has_no_dependents() {
        let g = graph(&["a"], vec![]);
        let r = find_dependents(&g, "a", None, &[]).unwrap();
        assert!(r.dependents.is_empty());
        assert_eq!(r.root_id, "a");
    }

    #[test]
    fn single_hop_dependent() {
        // b → a (b depends on a)
        let g = graph(&["a", "b"], vec![edge("b", "a", "implements")]);
        let r = find_dependents(&g, "a", None, &[]).unwrap();
        assert_eq!(r.dependents.len(), 1);
        assert_eq!(r.dependents[0].node.id, "b");
        assert_eq!(r.dependents[0].hops, 1);
        assert_eq!(r.dependents[0].via.len(), 1);
        assert_eq!(r.dependents[0].via[0].source, "b");
        assert_eq!(r.dependents[0].via[0].relation, "implements");
    }

    #[test]
    fn transitive_dependent_two_hops() {
        // c → b → a
        let g = graph(
            &["a", "b", "c"],
            vec![edge("b", "a", "implements"), edge("c", "b", "implements")],
        );
        let r = find_dependents(&g, "a", None, &[]).unwrap();
        assert_eq!(r.dependents.len(), 2);
        let by_id: BTreeMap<_, _> = r
            .dependents
            .iter()
            .map(|d| (d.node.id.as_str(), d))
            .collect();
        assert_eq!(by_id["b"].hops, 1);
        assert_eq!(by_id["c"].hops, 2);
        assert_eq!(by_id["c"].via.len(), 2);
        // via chain reads c → b → a.
        assert_eq!(by_id["c"].via[0].source, "c");
        assert_eq!(by_id["c"].via[1].source, "b");
    }

    #[test]
    fn depth_bound_stops_expansion() {
        // c → b → a; depth=1 returns only b.
        let g = graph(
            &["a", "b", "c"],
            vec![edge("b", "a", "implements"), edge("c", "b", "implements")],
        );
        let r = find_dependents(&g, "a", Some(1), &[]).unwrap();
        assert_eq!(r.dependents.len(), 1);
        assert_eq!(r.dependents[0].node.id, "b");
    }

    #[test]
    fn relation_filter_skips_unmatched_relations() {
        // b → a (implements), c → a (related). Filter on "implements"
        // only — c shouldn't appear.
        let g = graph(
            &["a", "b", "c"],
            vec![edge("b", "a", "implements"), edge("c", "a", "related")],
        );
        let r = find_dependents(&g, "a", None, &["implements".to_string()]).unwrap();
        assert_eq!(r.dependents.len(), 1);
        assert_eq!(r.dependents[0].node.id, "b");
        assert_eq!(r.relations, vec!["implements".to_string()]);
    }

    #[test]
    fn self_loop_is_ignored() {
        // a → a self-reference; "a" must not appear as its own dependent.
        let g = graph(&["a"], vec![edge("a", "a", "references")]);
        let r = find_dependents(&g, "a", None, &[]).unwrap();
        assert!(r.dependents.is_empty());
    }

    #[test]
    fn cycle_does_not_infinite_loop() {
        // a → b → a (illegal as a supersedes-DAG but possible for
        // other relations). Defensive visited check guarantees BFS
        // terminates.
        let g = graph(
            &["a", "b"],
            vec![edge("a", "b", "references"), edge("b", "a", "references")],
        );
        let r = find_dependents(&g, "a", None, &[]).unwrap();
        assert_eq!(r.dependents.len(), 1);
        assert_eq!(r.dependents[0].node.id, "b");
    }

    #[test]
    fn dependents_sorted_by_hops_then_id() {
        // a is reached by b (hops 1), c (hops 1), d (hops 2 via c).
        let g = graph(
            &["a", "b", "c", "d"],
            vec![
                edge("b", "a", "implements"),
                edge("c", "a", "implements"),
                edge("d", "c", "implements"),
            ],
        );
        let r = find_dependents(&g, "a", None, &[]).unwrap();
        let ids: Vec<&str> = r.dependents.iter().map(|d| d.node.id.as_str()).collect();
        // hops 1 first (b, c alphabetic), then hops 2 (d).
        assert_eq!(ids, vec!["b", "c", "d"]);
    }
}
