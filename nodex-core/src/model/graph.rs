use indexmap::IndexMap;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::annotation::Annotation;
use super::edge::Edge;
use super::node::Node;

/// Immutable document graph with pre-built adjacency indices.
/// Indices are automatically rebuilt on deserialization.
pub struct Graph {
    nodes: IndexMap<String, Node>,
    edges: Vec<Edge>,
    annotations: Vec<Annotation>,
    incoming: BTreeMap<String, Vec<usize>>,
    outgoing: BTreeMap<String, Vec<usize>>,
    annotations_by_source: BTreeMap<String, Vec<usize>>,
}

impl Graph {
    /// Build a graph from nodes, edges, and annotations. Constructs
    /// every adjacency index in one pass.
    pub fn new(
        nodes: IndexMap<String, Node>,
        edges: Vec<Edge>,
        annotations: Vec<Annotation>,
    ) -> Self {
        let (incoming, outgoing) = build_edge_indices(&edges);
        let annotations_by_source = build_annotation_index(&annotations);
        Self {
            nodes,
            edges,
            annotations,
            incoming,
            outgoing,
            annotations_by_source,
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

    /// Edges pointing to `id`. Includes self-loops (a doc that
    /// references itself surfaces here) — callers that measure
    /// *external attention* (orphan detection, backlinks query, trust
    /// score) should use [`Self::external_incoming_edges`] instead,
    /// which filters self-loops out. Honest graph-structure callers
    /// (node detail, diff, component analysis) use this method.
    pub fn incoming_edges(&self, id: &str) -> Vec<&Edge> {
        self.incoming_indices(id)
            .iter()
            .filter_map(|&idx| self.edges.get(idx))
            .collect()
    }

    /// Edges pointing to `id` with self-loops filtered out. Returned
    /// for queries that ask "who else attends to this node?" — a
    /// self-reference (a→a) does not represent attention from outside
    /// and would otherwise mask a node that is structurally isolated.
    /// See [`Self::incoming_edges`] for the un-filtered view.
    pub fn external_incoming_edges(&self, id: &str) -> Vec<&Edge> {
        self.incoming_indices(id)
            .iter()
            .filter_map(|&idx| self.edges.get(idx))
            .filter(|e| e.source != id)
            .collect()
    }

    /// Edges originating from `id`.
    pub fn outgoing_edges(&self, id: &str) -> Vec<&Edge> {
        self.outgoing_indices(id)
            .iter()
            .filter_map(|&idx| self.edges.get(idx))
            .collect()
    }

    /// Every annotation extracted at build time. Sorted by
    /// `(pattern_name, key, source_id, line)` for deterministic output.
    pub fn annotations(&self) -> &[Annotation] {
        &self.annotations
    }

    /// Annotations whose source is `id`. Symmetric to
    /// [`Self::outgoing_edges`] but for annotation records.
    pub fn annotations_from(&self, id: &str) -> Vec<&Annotation> {
        self.annotations_by_source
            .get(id)
            .map(|idxs| {
                idxs.iter()
                    .filter_map(|&i| self.annotations.get(i))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    pub fn annotation_count(&self) -> usize {
        self.annotations.len()
    }
}

fn build_edge_indices(
    edges: &[Edge],
) -> (BTreeMap<String, Vec<usize>>, BTreeMap<String, Vec<usize>>) {
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

fn build_annotation_index(annotations: &[Annotation]) -> BTreeMap<String, Vec<usize>> {
    let mut by_source: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (idx, ann) in annotations.iter().enumerate() {
        by_source
            .entry(ann.source_id.clone())
            .or_default()
            .push(idx);
    }
    by_source
}

/// Serialised schema revision. Every breaking change to the on-disk
/// shape of `graph.json` bumps this; readers refuse any file whose
/// recorded version does not equal `SCHEMA_VERSION`, with
/// `nodex build --full` as the escape hatch.
pub const SCHEMA_VERSION: u32 = 3;

/// Serialise nodes + edges + annotations with a schema-version
/// envelope. Indices are derived state and intentionally omitted.
impl Serialize for Graph {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("Graph", 4)?;
        s.serialize_field("schema_version", &SCHEMA_VERSION)?;
        s.serialize_field("nodes", &self.nodes)?;
        s.serialize_field("edges", &self.edges)?;
        s.serialize_field("annotations", &self.annotations)?;
        s.end()
    }
}

/// Deserialise nodes + edges + annotations, then rebuild adjacency
/// indices. Any `schema_version` other than `SCHEMA_VERSION` is
/// rejected — the envelope is part of the contract, not optional
/// metadata.
impl<'de> Deserialize<'de> for Graph {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            schema_version: u32,
            nodes: IndexMap<String, Node>,
            edges: Vec<Edge>,
            #[serde(default)]
            annotations: Vec<Annotation>,
        }

        let raw = Raw::deserialize(deserializer)?;
        if raw.schema_version != SCHEMA_VERSION {
            return Err(serde::de::Error::custom(format!(
                "graph.json schema_version {} does not match this binary ({}); \
                 run `nodex build --full` to regenerate",
                raw.schema_version, SCHEMA_VERSION
            )));
        }
        Ok(Graph::new(raw.nodes, raw.edges, raw.annotations))
    }
}

impl std::fmt::Debug for Graph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Graph")
            .field("nodes", &self.nodes.len())
            .field("edges", &self.edges.len())
            .field("annotations", &self.annotations.len())
            .finish()
    }
}
