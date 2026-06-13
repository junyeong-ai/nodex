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

// ─── Graph adjacency primitive ──────────────────────────────────────────

/// One edge direction for a reachability walk. The undirected projection
/// is the caller's union of the two — `outgoing(..).chain(incoming(..))`
/// — so this stays a minimal, symmetric pair with no "both" special case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    /// Resolved targets of outgoing edges (this node → others).
    Outgoing,
    /// Sources of incoming edges (others → this node).
    Incoming,
}

/// The node-key `&str`s adjacent to `id` along `direction`, restricted to
/// `relations` when `Some` (every relation when `None`). Each result is a
/// key borrowed from the graph — so it outlives the walk — and only
/// neighbours that resolve to a real node are returned: an unresolved
/// (dangling) outgoing target carries no node and is skipped. Self-loops
/// are kept (an `id`→`id` edge yields `id`); callers that exclude them do
/// so via their own `visited`/root filter.
///
/// The single definition of "one hop from a node" — every reachability
/// walk in this module (`find_components`, `find_neighborhood`) and the
/// supersession lineage (`query::traverse::find_chain`) expands through
/// it, so a fix to the hop step lands everywhere at once. Returns a `Vec`
/// rather than an iterator so the borrow of `graph` is not pinned across
/// the caller's `visited` mutation.
pub(crate) fn adjacent<'g>(
    graph: &'g Graph,
    id: &str,
    direction: Direction,
    relations: Option<&BTreeSet<&str>>,
) -> Vec<&'g str> {
    let allowed = |relation: &str| relations.is_none_or(|set| set.contains(relation));
    let mut out = Vec::new();
    match direction {
        Direction::Outgoing => {
            for edge in graph.outgoing_edges(id) {
                if allowed(&edge.relation)
                    && let Some(target) = edge.target.id()
                    && let Some((key, _)) = graph.nodes().get_key_value(target)
                {
                    out.push(key.as_str());
                }
            }
        }
        Direction::Incoming => {
            for edge in graph.incoming_edges(id) {
                if allowed(&edge.relation)
                    && let Some((key, _)) = graph.nodes().get_key_value(edge.source.as_str())
                {
                    out.push(key.as_str());
                }
            }
        }
    }
    out
}

/// Both directions' neighbours — the undirected one-hop set. The union of
/// [`adjacent`] over [`Direction::Outgoing`] and [`Direction::Incoming`],
/// the shared expansion for the direction-agnostic walks.
pub(crate) fn adjacent_undirected<'g>(
    graph: &'g Graph,
    id: &str,
    relations: Option<&BTreeSet<&str>>,
) -> Vec<&'g str> {
    let mut out = adjacent(graph, id, Direction::Outgoing, relations);
    out.extend(adjacent(graph, id, Direction::Incoming, relations));
    out
}

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
            for neighbour in adjacent_undirected(graph, id, None) {
                if visited.insert(neighbour) {
                    queue.push_back(neighbour);
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
        for neighbour in adjacent_undirected(graph, id, None) {
            if visited.insert(neighbour) {
                frontier.push_back((neighbour, d + 1));
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
    fn adjacent_returns_directed_neighbours_filtered_by_relation() {
        // a → b (references), a → c (supersedes), d → a (supersedes).
        let g = build(
            &["a", "b", "c", "d"],
            vec![
                edge("a", "b", "references"),
                edge("a", "c", "supersedes"),
                edge("d", "a", "supersedes"),
            ],
        );
        // Outgoing, unfiltered: both targets.
        assert_eq!(adjacent(&g, "a", Direction::Outgoing, None), vec!["b", "c"]);
        // Outgoing, relation-filtered: only the supersedes target.
        let sup = BTreeSet::from(["supersedes"]);
        assert_eq!(
            adjacent(&g, "a", Direction::Outgoing, Some(&sup)),
            vec!["c"]
        );
        // Incoming: the source that points at `a`.
        assert_eq!(adjacent(&g, "a", Direction::Incoming, None), vec!["d"]);
    }

    #[test]
    fn adjacent_skips_a_target_with_no_node() {
        // A target id that is not a graph node carries no node and is
        // dropped (the dangling/unresolved discipline).
        let g = build(&["a"], vec![edge("a", "ghost", "references")]);
        assert!(adjacent(&g, "a", Direction::Outgoing, None).is_empty());
    }

    #[test]
    fn adjacent_undirected_is_the_union_of_both_directions() {
        // a → b (outgoing), c → a (incoming): undirected one-hop = {b, c}.
        let g = build(
            &["a", "b", "c"],
            vec![edge("a", "b", "references"), edge("c", "a", "references")],
        );
        assert_eq!(adjacent_undirected(&g, "a", None), vec!["b", "c"]);
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
