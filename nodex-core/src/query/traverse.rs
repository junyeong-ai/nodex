use crate::model::Graph;
use schemars::JsonSchema;
use std::collections::{BTreeMap, BTreeSet};

use super::NodeRef;

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

    #[test]
    fn node_entry_names_each_edge_end_honestly() {
        // Z → X (incoming on X), X → Y (outgoing on X). Each summary
        // names the *other* end of the edge — `source` for incoming,
        // `target` for outgoing — instead of overloading one field.
        let mut nodes = IndexMap::new();
        for id in ["x", "y", "z"] {
            nodes.insert(id.to_string(), node(id));
        }
        let edges = vec![
            Edge {
                source: "z".into(),
                target: ResolvedTarget::resolved("x"),
                relation: "references".into(),
                location: "L1".into(),
            },
            Edge {
                source: "x".into(),
                target: ResolvedTarget::resolved("y"),
                relation: "references".into(),
                location: "L2".into(),
            },
        ];
        let graph = Graph::new(
            nodes,
            edges,
            vec![],
            vec![],
            vec![],
            crate::model::GraphMeta::default(),
        );
        let detail = find_node_entry(&graph, "x").unwrap();
        let json = serde_json::to_value(&detail).unwrap();
        assert_eq!(json["incoming"][0]["source"], "z");
        assert!(json["incoming"][0].get("target").is_none());
        assert_eq!(json["outgoing"][0]["target"], "y");
        assert!(json["outgoing"][0].get("source").is_none());
    }

    /// `supersedes`-authored chains must traverse identically to
    /// `superseded_by`-authored ones: the builder materialises both into
    /// `supersedes` edges, and `find_chain` reads only those edges.
    /// v1 ← v2 ← v3 authored purely as `v3.supersedes=[v2]`,
    /// `v2.supersedes=[v1]`. Two contracts: the result is chronological
    /// (oldest → newest), and it is anchor-agnostic — naming *any* member
    /// returns the whole line, so anchoring on the current head no longer
    /// truncates it.
    #[test]
    fn chain_is_full_lineage_chronological_from_any_anchor() {
        let mut nodes = IndexMap::new();
        for id in ["v1", "v2", "v3"] {
            nodes.insert(id.to_string(), node(id));
        }
        let edges = vec![
            Edge {
                source: "v2".into(),
                target: ResolvedTarget::resolved("v1"),
                relation: "supersedes".into(),
                location: "frontmatter:supersedes".into(),
            },
            Edge {
                source: "v3".into(),
                target: ResolvedTarget::resolved("v2"),
                relation: "supersedes".into(),
                location: "frontmatter:supersedes".into(),
            },
        ];
        let graph = Graph::new(
            nodes,
            edges,
            vec![],
            vec![],
            vec![],
            crate::model::GraphMeta::default(),
        );
        let chain_ids = |start: &str| -> Vec<String> {
            find_chain(&graph, start)
                .iter()
                .map(|e| {
                    serde_json::to_value(&e.node).unwrap()["id"]
                        .as_str()
                        .unwrap()
                        .to_string()
                })
                .collect()
        };
        // oldest → newest, and identical from every anchor.
        assert_eq!(chain_ids("v1"), vec!["v1", "v2", "v3"], "from oldest");
        assert_eq!(chain_ids("v2"), vec!["v1", "v2", "v3"], "from middle");
        assert_eq!(chain_ids("v3"), vec!["v1", "v2", "v3"], "from current head");
        // the live head is the last entry, whatever member you anchor on.
        assert_eq!(chain_ids("v1").last().map(String::as_str), Some("v3"));
        assert!(chain_ids("nonexistent").is_empty());
    }

    /// `supersedes` is a DAG, not a list: a fork (one document superseded
    /// by several) and a consolidation (one document superseding several)
    /// must both return the *whole* lineage, never a single branch. The
    /// previous lex-min walk collapsed forks and silently dropped the
    /// other branches — this is the regression guard for that data loss.
    #[test]
    fn chain_includes_every_branch_of_a_fork_or_consolidation() {
        // Helper: build a graph from `(newer, older)` supersedes pairs.
        let chain_of = |ids: &[&str], pairs: &[(&str, &str)], anchor: &str| -> Vec<String> {
            let mut nodes = IndexMap::new();
            for id in ids {
                nodes.insert(id.to_string(), node(id));
            }
            let edges = pairs
                .iter()
                .map(|(newer, older)| Edge {
                    source: (*newer).into(),
                    target: ResolvedTarget::resolved(*older),
                    relation: "supersedes".into(),
                    location: "frontmatter:supersedes".into(),
                })
                .collect();
            let graph = Graph::new(
                nodes,
                edges,
                vec![],
                vec![],
                vec![],
                crate::model::GraphMeta::default(),
            );
            find_chain(&graph, anchor)
                .iter()
                .map(|e| {
                    serde_json::to_value(&e.node).unwrap()["id"]
                        .as_str()
                        .unwrap()
                        .to_string()
                })
                .collect()
        };

        // Consolidation: `x` supersedes both `a` and `b`. The lineage is
        // {a, b, x}; oldest-first with the lex tie-break puts the two
        // roots before the tip. Neither branch may be dropped.
        let consolidation = chain_of(&["a", "b", "x"], &[("x", "a"), ("x", "b")], "x");
        assert_eq!(
            consolidation,
            vec!["a", "b", "x"],
            "consolidation keeps both roots"
        );
        // Anchor-agnostic: naming a root returns the whole component too.
        assert_eq!(
            chain_of(&["a", "b", "x"], &[("x", "a"), ("x", "b")], "a"),
            consolidation
        );

        // Fork: `a` is superseded by both `x` and `y`. The lineage is
        // {a, x, y}; `a` is the lone root, the two tips follow in id order.
        let fork = chain_of(&["a", "x", "y"], &[("x", "a"), ("y", "a")], "a");
        assert_eq!(fork, vec!["a", "x", "y"], "fork keeps both tips");
    }
}

/// Find all nodes that link TO the given node — "backlinks" in the
/// "what attends to this from elsewhere" sense, so self-references
/// (a→a) are filtered out. Use [`crate::model::Graph::incoming_edges`]
/// for the honest, self-inclusive view (`query node` does).
pub fn find_backlinks(graph: &Graph, target_id: &str) -> Vec<BacklinkEntry> {
    graph
        .external_incoming_edges(target_id)
        .iter()
        .filter_map(|edge| {
            let source = graph.node(&edge.source)?;
            Some(BacklinkEntry {
                node: NodeRef::from_node(source),
                relation: edge.relation.clone(),
                location: edge.location.clone(),
            })
        })
        .collect()
}

#[derive(Debug, serde::Serialize, JsonSchema)]
pub struct BacklinkEntry {
    #[serde(flatten)]
    pub node: NodeRef,
    pub relation: String,
    pub location: String,
}

/// The full supersession lineage containing `start_id`: every document
/// reachable from the anchor through `supersedes` edges in either
/// direction (the connected component of the supersession graph),
/// ordered oldest → newest.
///
/// Both directions are walked from the anchor — backward to the older
/// documents it supersedes, forward to the newer documents that supersede
/// it — so naming *any* member returns the whole lineage, not just the
/// part after the anchor. `supersedes` is a DAG, not a list: forks,
/// consolidations (one document superseding several), and diamonds are
/// permitted (only cycles are rejected, at build time by
/// `builder::validate_supersedes_dag`). Every branch is reported — a fork
/// is never collapsed to one neighbour — so no document in the lineage is
/// ever silently dropped.
///
/// Order is a topological sort (Kahn's algorithm) of the chronological
/// edge direction: an `A supersedes B` edge means `B` is older than `A`,
/// so `B` precedes `A`. Ties — branches with no ordering between them —
/// break on the lexicographically smallest id, so the order is total and
/// deterministic. The current head(s) are the tip(s): documents
/// superseded by nothing. A clean linear lineage has exactly one tip and
/// it is the last entry; a fork has several, and the consumer reads
/// "what's current" from the tips (or from `status`, which the
/// supersession lifecycle leaves non-terminal only on a live document),
/// not from position alone.
///
/// The chain reads from the resolved `supersedes` edge graph — the single
/// representation the builder materialises from both the `supersedes:`
/// and `superseded_by:` authoring styles — so traversal is identical
/// regardless of which side authored the relation. A dangling
/// `supersedes` (unresolved target) carries no node and is skipped.
pub fn find_chain(graph: &Graph, start_id: &str) -> Vec<ChainEntry> {
    if graph.node(start_id).is_none() {
        return Vec::new();
    }

    // Lineage runs along `supersedes` edges only.
    let supersedes: BTreeSet<&str> = BTreeSet::from(["supersedes"]);

    // 1. Collect the connected component: walk every node reachable from
    //    the anchor through `supersedes` edges in EITHER direction (older
    //    targets and newer sources are both neighbours). Order is
    //    irrelevant — only the member set matters — so a LIFO frontier
    //    (stack) is used.
    let mut component: BTreeSet<String> = BTreeSet::new();
    component.insert(start_id.to_string());
    let mut frontier = vec![start_id.to_string()];
    while let Some(id) = frontier.pop() {
        for neighbour in super::structure::adjacent_undirected(graph, &id, Some(&supersedes)) {
            if component.insert(neighbour.to_string()) {
                frontier.push(neighbour.to_string());
            }
        }
    }

    // 2. Topologically sort the component oldest → newest. An
    //    `A supersedes B` edge orders the older `B` before the newer `A`,
    //    so `in_degree[A]` counts the documents `A` supersedes within the
    //    component; the roots (in-degree 0) are the oldest. Kahn's
    //    algorithm drains them with the lex-smallest ready id as a total
    //    tie-break, so forks emit every branch in a stable order.
    let mut newer_of: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut in_degree: BTreeMap<String, usize> =
        component.iter().map(|id| (id.clone(), 0)).collect();
    for older in &component {
        // `newer` supersedes `older` (incoming supersedes edge). The
        // component walk closed over these, so every `newer` is already a
        // member — the `expect` asserts that invariant rather than
        // silently skipping an edge.
        for newer in super::structure::adjacent(
            graph,
            older,
            super::structure::Direction::Incoming,
            Some(&supersedes),
        ) {
            newer_of
                .entry(older.clone())
                .or_default()
                .push(newer.to_string());
            *in_degree
                .get_mut(newer)
                .expect("component walk closes over supersedes sources") += 1;
        }
    }

    let mut ready: BTreeSet<String> = in_degree
        .iter()
        .filter(|(_, deg)| **deg == 0)
        .map(|(id, _)| id.clone())
        .collect();
    let mut ordered: Vec<String> = Vec::with_capacity(component.len());
    while let Some(id) = ready.iter().next().cloned() {
        ready.remove(&id);
        if let Some(newer_ids) = newer_of.get(&id) {
            for newer in newer_ids {
                let deg = in_degree.get_mut(newer).expect("component member");
                *deg -= 1;
                if *deg == 0 {
                    ready.insert(newer.clone());
                }
            }
        }
        ordered.push(id);
    }
    // Defensive: a cycle (which `validate_supersedes_dag` forbids in a
    // built graph) would leave members unprocessed — append them in id
    // order so the lineage is reported in full rather than truncated.
    let placed: BTreeSet<&String> = ordered.iter().collect();
    let leftover: Vec<String> = component
        .iter()
        .filter(|id| !placed.contains(id))
        .cloned()
        .collect();
    ordered.extend(leftover);

    ordered
        .into_iter()
        .filter_map(|id| {
            graph.node(&id).map(|node| ChainEntry {
                node: NodeRef::from_node(node),
            })
        })
        .collect()
}

#[derive(Debug, serde::Serialize, JsonSchema)]
pub struct ChainEntry {
    #[serde(flatten)]
    pub node: NodeRef,
}

/// Find a node's full detail with incoming and outgoing edges,
/// or `None` if the id is not in the graph.
pub fn find_node_entry(graph: &Graph, id: &str) -> Option<NodeEntry> {
    let node = graph.node(id)?;

    let incoming: Vec<IncomingEdgeRef> = graph
        .incoming_edges(id)
        .iter()
        .map(|e| IncomingEdgeRef {
            source: e.source.clone(),
            relation: e.relation.clone(),
        })
        .collect();

    let outgoing: Vec<OutgoingEdgeRef> = graph
        .outgoing_edges(id)
        .iter()
        .map(|e| OutgoingEdgeRef {
            target: match &e.target {
                crate::model::ResolvedTarget::Resolved { id } => id.clone(),
                crate::model::ResolvedTarget::Unresolved { raw, .. } => raw.clone(),
            },
            relation: e.relation.clone(),
        })
        .collect();

    Some(NodeEntry {
        node: node.clone(),
        incoming,
        outgoing,
        body: None,
    })
}

/// One node's full detail view: the node itself plus every edge that
/// touches it, split into honest `incoming` / `outgoing` halves. Single
/// `*Entry` shape returned by `query node` — the project-wide convention
/// is that every queryable single-node row ends in `*Entry`.
#[derive(Debug, serde::Serialize, JsonSchema)]
pub struct NodeEntry {
    pub node: crate::model::Node,
    pub incoming: Vec<IncomingEdgeRef>,
    pub outgoing: Vec<OutgoingEdgeRef>,
    /// Body text, attached only on request (`query node --with-body`).
    /// The graph stores body *fingerprints*, never text, so the CLI
    /// reads the file through the canonical parse seam and fills this
    /// in — `Some("")` for a body-less document ("asked and empty" is
    /// distinct from "not asked"). Absent (and omitted from JSON)
    /// otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

/// One edge pointing **into** the queried node. `source` is the other
/// end — the node that links to us. Split from [`OutgoingEdgeRef`] so the
/// JSON shape names each end honestly instead of overloading "target"
/// to also mean "source for incoming".
#[derive(Debug, serde::Serialize, JsonSchema)]
pub struct IncomingEdgeRef {
    pub source: String,
    pub relation: String,
}

/// One edge originating **from** the queried node. `target` is the
/// resolved node id when the edge points into the graph, or the raw
/// user string for out-of-graph references (e.g. `covers` pointing at
/// source files).
#[derive(Debug, serde::Serialize, JsonSchema)]
pub struct OutgoingEdgeRef {
    pub target: String,
    pub relation: String,
}

/// Reverse lookup: which doc nodes cover the given source-code path?
/// Reads `covers` edges from the graph (frontmatter-declared coverage
/// of out-of-graph artefacts).
///
/// Both the query input and each `covers` entry are run through the
/// same path normalisation (forward slashes, leading `./` stripped,
/// `.` / `..` segments resolved) so equivalent paths match regardless
/// of authoring style. Absolute paths supplied at query time are
/// compared as-is — `covers` entries are project-relative by
/// convention, so an absolute query simply won't match.
pub fn find_covered_by(
    graph: &Graph,
    code_path: &str,
    extensions: &[String],
) -> Vec<CoveredByEntry> {
    use crate::model::ResolvedTarget;
    let needle = normalize_query_path(code_path);
    graph
        .edges()
        .iter()
        .filter(|edge| edge.relation == "covers")
        .filter_map(|edge| Some((edge, graph.node(&edge.source)?)))
        .filter(|(edge, source)| match &edge.target {
            ResolvedTarget::Resolved { id } => graph.node(id).is_some_and(|target| {
                normalize_query_path(&crate::path_guard::forward_string(&target.path)) == needle
            }),
            // What the covering document's own text could name, by the
            // ladder the build binds with — the frame a `covers` value
            // opening `./` says out loud included. Folded away here and
            // honoured there, the same value meant two paths, and the one
            // this answered for existed nowhere.
            ResolvedTarget::Unresolved { raw, .. } => {
                crate::builder::resolver::normalized_resolution_candidates(
                    raw,
                    Some(source.path.as_path()),
                    extensions,
                    crate::model::edge::is_document_ref_relation(&edge.relation),
                )
                .contains(&needle)
            }
        })
        .map(|(edge, source)| CoveredByEntry {
            node: NodeRef::from_node(source),
            relation: edge.relation.clone(),
        })
        .collect()
}

/// Canonicalise the *needle* for equality comparison: forward slashes,
/// no leading `./`, no `.` / `..` segments. Pure string-and-`Path`-
/// component operations — never touches disk.
///
/// A needle is what the caller is looking for and has no frame of its
/// own, so folding `./` off it is right. The edge's own text is the
/// opposite: there `./` is the document saying which directory it meant,
/// and the ladder above answers for it.
fn normalize_query_path(input: &str) -> String {
    use std::path::{Component, PathBuf};
    let forward = crate::path_guard::forward_str(input);
    let stripped = forward.strip_prefix("./").unwrap_or(&forward);
    let mut parts: Vec<Component<'_>> = Vec::new();
    for component in std::path::Path::new(stripped).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    crate::path_guard::forward_string(&parts.iter().collect::<PathBuf>())
}

#[derive(Debug, serde::Serialize, JsonSchema)]
pub struct CoveredByEntry {
    #[serde(flatten)]
    pub node: NodeRef,
    pub relation: String,
}
