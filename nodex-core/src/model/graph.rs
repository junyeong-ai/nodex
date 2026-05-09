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

    /// Borrow the node with the given id, or return
    /// [`crate::error::Error::MissingNode`] when it doesn't exist.
    /// Use this at the boundary between user input and graph
    /// operations so a typo surfaces with a typed error rather than
    /// as a silently-empty result.
    pub fn require_node(&self, id: &str) -> crate::error::Result<&Node> {
        self.node(id)
            .ok_or_else(|| crate::error::Error::MissingNode(id.to_string()))
    }

    pub fn nodes(&self) -> &IndexMap<String, Node> {
        &self.nodes
    }

    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    /// Edge indices where `target == id`.
    pub fn incoming_indices(&self, id: &str) -> &[usize] {
        self.incoming
            .get(id)
            .map(|v| v.as_slice())
            .unwrap_or_default()
    }

    fn outgoing_indices(&self, id: &str) -> &[usize] {
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

/// Serialised schema revision. Every breaking change to the on-disk
/// shape of `graph.json` bumps this; readers refuse any file whose
/// recorded version does not equal `SCHEMA_VERSION`, with
/// `nodex build --full` as the escape hatch.
pub const SCHEMA_VERSION: u32 = 2;

/// Serialise nodes + edges with a schema-version envelope. Indices
/// are derived state and intentionally omitted.
impl Serialize for Graph {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("Graph", 3)?;
        s.serialize_field("schema_version", &SCHEMA_VERSION)?;
        s.serialize_field("nodes", &self.nodes)?;
        s.serialize_field("edges", &self.edges)?;
        s.end()
    }
}

/// Deserialise nodes + edges, then rebuild adjacency indices. Any
/// `schema_version` other than `SCHEMA_VERSION` is rejected — the
/// envelope is part of the contract, not optional metadata.
impl<'de> Deserialize<'de> for Graph {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            schema_version: u32,
            nodes: IndexMap<String, Node>,
            edges: Vec<Edge>,
        }

        let raw = Raw::deserialize(deserializer)?;
        if raw.schema_version != SCHEMA_VERSION {
            return Err(serde::de::Error::custom(format!(
                "graph.json schema_version {} does not match this binary ({}); \
                 run `nodex build --full` to regenerate",
                raw.schema_version, SCHEMA_VERSION
            )));
        }
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
