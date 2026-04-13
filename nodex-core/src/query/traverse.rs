use crate::model::Graph;
use std::collections::BTreeSet;

/// Find all nodes that link TO the given node.
pub fn find_backlinks(graph: &Graph, target_id: &str) -> Vec<BacklinkEntry> {
    graph
        .incoming_edges(target_id)
        .iter()
        .filter_map(|edge| {
            let source = graph.node(&edge.source)?;
            Some(BacklinkEntry {
                id: source.id.clone(),
                title: source.title.clone(),
                relation: edge.relation.clone(),
                location: edge.location.clone(),
            })
        })
        .collect()
}

#[derive(Debug, serde::Serialize)]
pub struct BacklinkEntry {
    pub id: String,
    pub title: String,
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
            id: node.id.clone(),
            title: node.title.clone(),
            status: node.status.to_string(),
        });

        match &node.superseded_by {
            Some(next) => current_id = next.clone(),
            None => break,
        }
    }

    chain
}

#[derive(Debug, serde::Serialize)]
pub struct ChainEntry {
    pub id: String,
    pub title: String,
    pub status: String,
}

/// Get full node detail with incoming and outgoing edges.
pub fn node_detail(graph: &Graph, id: &str) -> Option<NodeDetail> {
    let node = graph.node(id)?;

    let incoming: Vec<EdgeSummary> = graph
        .incoming_edges(id)
        .iter()
        .map(|e| EdgeSummary {
            node_id: e.source.clone(),
            relation: e.relation.clone(),
            confidence: e.confidence.to_string(),
        })
        .collect();

    let outgoing: Vec<EdgeSummary> = graph
        .outgoing_edges(id)
        .iter()
        .filter_map(|e| {
            Some(EdgeSummary {
                node_id: e.target.id()?.to_string(),
                relation: e.relation.clone(),
                confidence: e.confidence.to_string(),
            })
        })
        .collect();

    Some(NodeDetail {
        node: node.clone(),
        incoming,
        outgoing,
    })
}

#[derive(Debug, serde::Serialize)]
pub struct NodeDetail {
    pub node: crate::model::Node,
    pub incoming: Vec<EdgeSummary>,
    pub outgoing: Vec<EdgeSummary>,
}

#[derive(Debug, serde::Serialize)]
pub struct EdgeSummary {
    pub node_id: String,
    pub relation: String,
    pub confidence: String,
}
