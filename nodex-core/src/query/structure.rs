//! Whole-graph structural primitives — no policy, no thresholds.
//!
//! Each function takes a [`Graph`] and returns a deterministic
//! partitioning or expansion. Callers (CLI, downstream agents) apply
//! their own judgment to the results; nothing here decides what is
//! "good" or "bad".

use schemars::JsonSchema;
use serde::Serialize;
use std::collections::{BTreeSet, VecDeque};

use crate::error::Result;
use crate::model::Graph;

use super::NodeRef;

// ─── Connected components ───────────────────────────────────────────────

/// One connected component of the graph (undirected projection of all
/// edges). Members are sorted by node id for determinism.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Component {
    /// Stable 1-based ordinal. Components are emitted largest-first;
    /// among components of equal size, the one whose smallest member id
    /// is alphabetically lower comes first.
    pub component_id: u32,
    pub size: usize,
    pub members: Vec<NodeRef>,
}

/// Partition the graph into connected components by ignoring edge
/// direction. Two nodes belong to the same component iff a path of
/// edges (in either direction) connects them.
///
/// Pure structural output — leaf-by-design singletons appear as
/// size-1 components without any "this is bad" labelling; downstream
/// policy lives elsewhere.
pub fn find_components(graph: &Graph) -> Vec<Component> {
    // Sort node ids upfront so the BFS visit order is deterministic
    // regardless of `IndexMap` insertion order.
    let mut all_ids: Vec<&str> = graph.nodes().keys().map(|s| s.as_str()).collect();
    all_ids.sort_unstable();

    let mut visited: BTreeSet<&str> = BTreeSet::new();
    let mut components: Vec<Vec<&str>> = Vec::new();

    for &seed in &all_ids {
        if visited.contains(seed) {
            continue;
        }
        let mut members: Vec<&str> = Vec::new();
        let mut queue: VecDeque<&str> = VecDeque::new();
        queue.push_back(seed);
        visited.insert(seed);

        while let Some(id) = queue.pop_front() {
            members.push(id);
            for edge in graph.outgoing_edges(id) {
                if let Some(t) = edge.target.id()
                    && !visited.contains(t)
                    && let Some((k, _)) = graph.nodes().get_key_value(t)
                {
                    visited.insert(k.as_str());
                    queue.push_back(k.as_str());
                }
            }
            for edge in graph.incoming_edges(id) {
                let src = edge.source.as_str();
                if !visited.contains(src)
                    && let Some((k, _)) = graph.nodes().get_key_value(src)
                {
                    visited.insert(k.as_str());
                    queue.push_back(k.as_str());
                }
            }
        }

        members.sort_unstable();
        components.push(members);
    }

    // Largest-first, then alphabetic by smallest member id for tie-break.
    components.sort_by(|a, b| {
        b.len()
            .cmp(&a.len())
            .then_with(|| a.first().cmp(&b.first()))
    });

    components
        .into_iter()
        .enumerate()
        .map(|(idx, members)| Component {
            component_id: (idx + 1) as u32,
            size: members.len(),
            members: members
                .into_iter()
                .filter_map(|id| graph.node(id))
                .map(NodeRef::from_node)
                .collect(),
        })
        .collect()
}

// ─── Bounded neighbourhood ──────────────────────────────────────────────

/// One node reached during a neighbourhood walk, annotated with the
/// shortest BFS distance from the seed.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct NeighborhoodEntry {
    #[serde(flatten)]
    pub node: NodeRef,
    pub depth: u32,
}

/// All nodes within `depth` hops of `seed`, ignoring edge direction.
/// Depth `0` returns the seed alone; depth `N` returns the seed plus
/// every node reachable by ≤ N edges. The result is sorted by
/// `(depth, id)` so callers can stream it without further work.
///
/// Pure structural output — no token counting, no priority heuristics,
/// no "healthy first" reordering. Downstream consumers that want a
/// budget-bound subset slice the result themselves.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Neighborhood {
    pub seed: String,
    pub depth: u32,
    pub nodes: Vec<NeighborhoodEntry>,
}

pub fn find_neighborhood(graph: &Graph, seed: &str, depth: u32) -> Result<Neighborhood> {
    graph.require_node(seed)?;

    let mut visited: BTreeSet<&str> = BTreeSet::new();
    let mut frontier: VecDeque<(&str, u32)> = VecDeque::new();

    let seed_key = graph
        .nodes()
        .get_key_value(seed)
        .map(|(k, _)| k.as_str())
        .expect("require_node already established presence");
    frontier.push_back((seed_key, 0));
    visited.insert(seed_key);

    let mut collected: Vec<NeighborhoodEntry> = Vec::new();

    while let Some((id, d)) = frontier.pop_front() {
        if let Some(n) = graph.node(id) {
            collected.push(NeighborhoodEntry {
                node: NodeRef::from_node(n),
                depth: d,
            });
        }
        if d == depth {
            continue;
        }
        for edge in graph.outgoing_edges(id) {
            if let Some(t) = edge.target.id()
                && !visited.contains(t)
                && let Some((k, _)) = graph.nodes().get_key_value(t)
            {
                visited.insert(k.as_str());
                frontier.push_back((k.as_str(), d + 1));
            }
        }
        for edge in graph.incoming_edges(id) {
            let src = edge.source.as_str();
            if !visited.contains(src)
                && let Some((k, _)) = graph.nodes().get_key_value(src)
            {
                visited.insert(k.as_str());
                frontier.push_back((k.as_str(), d + 1));
            }
        }
    }

    collected.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then_with(|| a.node.id.cmp(&b.node.id))
    });

    Ok(Neighborhood {
        seed: seed.to_string(),
        depth,
        nodes: collected,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Edge, Graph, Kind, Node, ResolvedTarget, Status};
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

    fn edge(source: &str, target: &str, relation: &str) -> Edge {
        Edge {
            source: source.into(),
            target: ResolvedTarget::Resolved { id: target.into() },
            relation: relation.into(),
            location: "test".into(),
        }
    }

    fn build(node_ids: &[&str], edges: Vec<Edge>) -> Graph {
        let mut nodes = IndexMap::new();
        for id in node_ids {
            nodes.insert((*id).to_string(), n(id));
        }
        Graph::new(
            nodes,
            edges,
            vec![],
            vec![],
            vec![],
            crate::model::GraphMeta::default(),
        )
    }

    #[test]
    fn singleton_node_is_its_own_component() {
        let g = build(&["a"], vec![]);
        let cs = find_components(&g);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].size, 1);
        assert_eq!(cs[0].component_id, 1);
    }

    #[test]
    fn two_disconnected_nodes_are_two_components() {
        let g = build(&["a", "b"], vec![]);
        let cs = find_components(&g);
        assert_eq!(cs.len(), 2);
    }

    #[test]
    fn directed_edge_still_connects_undirected_components() {
        let g = build(&["a", "b"], vec![edge("a", "b", "references")]);
        let cs = find_components(&g);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].size, 2);
    }

    #[test]
    fn components_ordered_largest_first_then_alphabetic() {
        // {a,b,c} (size 3) and {x,y} (size 2)
        let g = build(
            &["a", "b", "c", "x", "y"],
            vec![
                edge("a", "b", "references"),
                edge("b", "c", "references"),
                edge("x", "y", "references"),
            ],
        );
        let cs = find_components(&g);
        assert_eq!(cs[0].size, 3);
        assert_eq!(cs[1].size, 2);
        assert_eq!(cs[0].component_id, 1);
        assert_eq!(cs[1].component_id, 2);
    }

    #[test]
    fn neighborhood_depth_zero_returns_seed_only() {
        let g = build(&["a", "b"], vec![edge("a", "b", "references")]);
        let nb = find_neighborhood(&g, "a", 0).unwrap();
        assert_eq!(nb.nodes.len(), 1);
        assert_eq!(nb.nodes[0].depth, 0);
        assert_eq!(nb.nodes[0].node.id, "a");
    }

    #[test]
    fn neighborhood_depth_one_includes_both_directions() {
        // a → b, c → a   (depth 1 from a should include a, b, c)
        let g = build(
            &["a", "b", "c"],
            vec![edge("a", "b", "references"), edge("c", "a", "references")],
        );
        let nb = find_neighborhood(&g, "a", 1).unwrap();
        let ids: Vec<&str> = nb.nodes.iter().map(|n| n.node.id.as_str()).collect();
        assert!(ids.contains(&"a"));
        assert!(ids.contains(&"b"));
        assert!(ids.contains(&"c"));
        assert_eq!(nb.nodes.len(), 3);
    }

    #[test]
    fn neighborhood_unknown_seed_errors() {
        let g = build(&["a"], vec![]);
        assert!(find_neighborhood(&g, "ghost", 1).is_err());
    }
}
