use crate::model::Graph;
use schemars::JsonSchema;
use std::collections::BTreeSet;

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

/// The full supersession lineage containing `start_id`, ordered oldest →
/// newest (chronological).
///
/// Both directions are walked from the anchor — backward to the older
/// documents it supersedes, forward to the newer documents that supersede
/// it — so naming *any* member returns the whole line, not just the part
/// after the anchor. (Anchoring on the current head used to return just
/// that node.) The order is chronological, which reads as the lineage's
/// natural forward sequence; the live document is the last entry, and —
/// independent of order — the only non-terminal status in the line, so a
/// consumer identifies "what's current" by either.
///
/// The chain reads from the resolved `supersedes` edge graph — the single
/// representation the builder materialises from both the `supersedes:`
/// and `superseded_by:` authoring styles — so traversal is identical
/// regardless of which side authored the relation. A fork (a node
/// superseded by, or superseding, more than one document — which the
/// supersedes-DAG permits) collapses to its lexicographically smallest
/// neighbour at each hop, so the result stays a single deterministic
/// line.
pub fn find_chain(graph: &Graph, start_id: &str) -> Vec<ChainEntry> {
    if graph.node(start_id).is_none() {
        return Vec::new();
    }
    let mut visited = BTreeSet::new();
    visited.insert(start_id.to_string());

    // Older side: the documents this one supersedes, walked oldest-ward.
    let mut older = Vec::new();
    let mut cursor = start_id.to_string();
    while let Some(prev) = predecessor(graph, &cursor) {
        if !visited.insert(prev.clone()) {
            break;
        }
        older.push(prev.clone());
        cursor = prev;
    }

    // Newer side: the documents that supersede this one, walked newest-ward.
    let mut newer = Vec::new();
    cursor = start_id.to_string();
    while let Some(next) = successor(graph, &cursor) {
        if !visited.insert(next.clone()) {
            break;
        }
        newer.push(next.clone());
        cursor = next;
    }

    // Assemble oldest → newest: reverse(older) + anchor + newer.
    let mut ids: Vec<String> = older.into_iter().rev().collect();
    ids.push(start_id.to_string());
    ids.extend(newer);

    ids.into_iter()
        .filter_map(|id| {
            graph.node(&id).map(|node| ChainEntry {
                node: NodeRef::from_node(node),
            })
        })
        .collect()
}

/// The newer document that supersedes `id` — the `source` of an incoming
/// `supersedes` edge (`newer --supersedes--> id`). Lexicographically
/// smallest source on a fork, so the chain stays a single deterministic
/// line.
fn successor(graph: &Graph, id: &str) -> Option<String> {
    graph
        .incoming_edges(id)
        .iter()
        .filter(|e| e.relation == "supersedes")
        .map(|e| e.source.clone())
        .min()
}

/// The older document `id` supersedes — the resolved `target` of an
/// outgoing `supersedes` edge (`id --supersedes--> older`). Lex-smallest
/// target on a fork. A dangling `supersedes` (unresolved target) carries
/// no node to continue from and is skipped.
fn predecessor(graph: &Graph, id: &str) -> Option<String> {
    graph
        .outgoing_edges(id)
        .iter()
        .filter(|e| e.relation == "supersedes")
        .filter_map(|e| e.target.id().map(str::to_string))
        .min()
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
pub fn find_covered_by(graph: &Graph, code_path: &str) -> Vec<CoveredByEntry> {
    use crate::model::ResolvedTarget;
    let needle = normalize_query_path(code_path);
    graph
        .edges()
        .iter()
        .filter(|e| e.relation == "covers")
        .filter_map(|e| match &e.target {
            ResolvedTarget::Resolved { id } => graph
                .node(id)
                .map(|n| (e, crate::path_guard::forward_string(&n.path))),
            ResolvedTarget::Unresolved { raw, .. } => {
                Some((e, crate::path_guard::forward_str(raw)))
            }
        })
        .filter(|(_, target_str)| normalize_query_path(target_str) == needle)
        .filter_map(|(edge, _)| {
            let source = graph.node(&edge.source)?;
            Some(CoveredByEntry {
                node: NodeRef::from_node(source),
                relation: edge.relation.clone(),
            })
        })
        .collect()
}

/// Canonicalise a project-relative path for equality comparison:
/// forward slashes, no leading `./`, no `.` / `..` segments. Pure
/// string-and-`Path`-component operations — never touches disk — so
/// the same logic applies to both authored frontmatter values and
/// runtime query input without I/O.
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
