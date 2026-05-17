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
        }
    }

    #[test]
    fn node_detail_names_each_edge_end_honestly() {
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
        let graph = Graph::new(nodes, edges, vec![], vec![]);
        let detail = find_node_detail(&graph, "x").unwrap();
        let json = serde_json::to_value(&detail).unwrap();
        assert_eq!(json["incoming"][0]["source"], "z");
        assert!(json["incoming"][0].get("target").is_none());
        assert_eq!(json["outgoing"][0]["target"], "y");
        assert!(json["outgoing"][0].get("source").is_none());
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

/// Walk the supersession chain forward from a node (oldest → newest).
pub fn find_chain(graph: &Graph, start_id: &str) -> Vec<ChainEntry> {
    let mut chain = Vec::new();
    let mut visited = BTreeSet::new();
    let mut current_id = start_id.to_string();

    loop {
        if visited.contains(&current_id) {
            break; // Cycle guard (shouldn't happen — DAG validated at build)
        }
        visited.insert(current_id.clone());

        let Some(node) = graph.node(&current_id) else {
            break;
        };

        chain.push(ChainEntry {
            node: NodeRef::from_node(node),
        });

        match &node.superseded_by {
            Some(next) => current_id = next.clone(),
            None => break,
        }
    }

    chain
}

#[derive(Debug, serde::Serialize, JsonSchema)]
pub struct ChainEntry {
    #[serde(flatten)]
    pub node: NodeRef,
}

/// Find a node's full detail with incoming and outgoing edges,
/// or `None` if the id is not in the graph.
pub fn find_node_detail(graph: &Graph, id: &str) -> Option<NodeDetail> {
    let node = graph.node(id)?;

    let incoming: Vec<IncomingEdge> = graph
        .incoming_edges(id)
        .iter()
        .map(|e| IncomingEdge {
            source: e.source.clone(),
            relation: e.relation.clone(),
        })
        .collect();

    let outgoing: Vec<OutgoingEdge> = graph
        .outgoing_edges(id)
        .iter()
        .map(|e| OutgoingEdge {
            target: match &e.target {
                crate::model::ResolvedTarget::Resolved { id } => id.clone(),
                crate::model::ResolvedTarget::Unresolved { raw, .. } => raw.clone(),
            },
            relation: e.relation.clone(),
        })
        .collect();

    Some(NodeDetail {
        node: node.clone(),
        incoming,
        outgoing,
    })
}

#[derive(Debug, serde::Serialize, JsonSchema)]
pub struct NodeDetail {
    pub node: crate::model::Node,
    pub incoming: Vec<IncomingEdge>,
    pub outgoing: Vec<OutgoingEdge>,
}

/// One edge pointing **into** the queried node. `source` is the other
/// end — the node that links to us. Split from [`OutgoingEdge`] so the
/// JSON shape names each end honestly instead of overloading "target"
/// to also mean "source for incoming".
#[derive(Debug, serde::Serialize, JsonSchema)]
pub struct IncomingEdge {
    pub source: String,
    pub relation: String,
}

/// One edge originating **from** the queried node. `target` is the
/// resolved node id when the edge points into the graph, or the raw
/// user string for out-of-graph references (e.g. `covers` pointing at
/// source files).
#[derive(Debug, serde::Serialize, JsonSchema)]
pub struct OutgoingEdge {
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
