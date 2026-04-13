use indexmap::IndexMap;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::edge::Edge;
use super::node::Node;

/// Immutable document graph with pre-built adjacency indices.
/// Indices are automatically rebuilt on deserialization.
pub struct Graph {
    nodes: IndexMap<String, Node>,
    edges: Vec<Edge>,
    incoming: BTreeMap<String, Vec<usize>>,
    outgoing: BTreeMap<String, Vec<usize>>,
}

impl Graph {
    /// Build a graph from nodes and edges. Constructs adjacency indices.
    pub fn new(nodes: IndexMap<String, Node>, edges: Vec<Edge>) -> Self {
        let (incoming, outgoing) = build_indices(&edges);
        Self {
            nodes,
            edges,
            incoming,
            outgoing,
        }
    }

    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.get(id)
    }

    pub fn nodes(&self) -> &IndexMap<String, Node> {
        &self.nodes
    }

    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    pub fn edge(&self, idx: usize) -> Option<&Edge> {
        self.edges.get(idx)
    }

    /// Edge indices where `target == id`.
    pub fn incoming_indices(&self, id: &str) -> &[usize] {
        self.incoming
            .get(id)
            .map(|v| v.as_slice())
            .unwrap_or_default()
    }

    /// Edge indices where `source == id`.
    pub fn outgoing_indices(&self, id: &str) -> &[usize] {
        self.outgoing
            .get(id)
            .map(|v| v.as_slice())
            .unwrap_or_default()
    }

    /// Edges pointing to `id`.
    pub fn incoming_edges(&self, id: &str) -> Vec<&Edge> {
        self.incoming_indices(id)
            .iter()
            .filter_map(|&idx| self.edges.get(idx))
            .collect()
    }

    /// Edges originating from `id`.
    pub fn outgoing_edges(&self, id: &str) -> Vec<&Edge> {
        self.outgoing_indices(id)
            .iter()
            .filter_map(|&idx| self.edges.get(idx))
            .collect()
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }
}

fn build_indices(edges: &[Edge]) -> (BTreeMap<String, Vec<usize>>, BTreeMap<String, Vec<usize>>) {
    let mut incoming: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut outgoing: BTreeMap<String, Vec<usize>> = BTreeMap::new();

    for (idx, edge) in edges.iter().enumerate() {
        outgoing.entry(edge.source.clone()).or_default().push(idx);
        if let Some(target_id) = edge.target.id() {
            incoming.entry(target_id.to_string()).or_default().push(idx);
        }
    }

    (incoming, outgoing)
}

/// Serialize only nodes + edges. Indices are derived state.
impl Serialize for Graph {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("Graph", 2)?;
        s.serialize_field("nodes", &self.nodes)?;
        s.serialize_field("edges", &self.edges)?;
        s.end()
    }
}

/// Deserialize nodes + edges, then automatically rebuild indices.
impl<'de> Deserialize<'de> for Graph {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            nodes: IndexMap<String, Node>,
            edges: Vec<Edge>,
        }

        let raw = Raw::deserialize(deserializer)?;
        Ok(Graph::new(raw.nodes, raw.edges))
    }
}

impl std::fmt::Debug for Graph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Graph")
            .field("nodes", &self.nodes.len())
            .field("edges", &self.edges.len())
            .finish()
    }
}
