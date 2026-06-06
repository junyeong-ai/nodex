use indexmap::IndexMap;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::annotation::Annotation;
use super::body_line_match::BodyLineMatch;
use super::edge::Edge;
use super::node::Node;

/// Immutable document graph with pre-built adjacency indices.
/// Indices are derived state — rebuilt by [`Graph::new`] and by the
/// `Deserialize` impl.
pub struct Graph {
    nodes: IndexMap<String, Node>,
    edges: Vec<Edge>,
    annotations: Vec<Annotation>,
    body_line_matches: Vec<BodyLineMatch>,
    incoming: BTreeMap<String, Vec<usize>>,
    outgoing: BTreeMap<String, Vec<usize>>,
    annotations_by_source: BTreeMap<String, Vec<usize>>,
    body_line_matches_by_source: BTreeMap<String, Vec<usize>>,
    body_line_matches_by_rule: BTreeMap<String, Vec<usize>>,
}

impl Graph {
    /// Construct from canonical parts. Adjacency indices over edges,
    /// annotations, and body-line matches are derived in one pass —
    /// callers never thread them in.
    pub fn new(
        nodes: IndexMap<String, Node>,
        edges: Vec<Edge>,
        annotations: Vec<Annotation>,
        body_line_matches: Vec<BodyLineMatch>,
    ) -> Self {
        let (incoming, outgoing) = build_edge_indices(&edges);
        let annotations_by_source = build_annotation_index(&annotations);
        let (body_line_matches_by_source, body_line_matches_by_rule) =
            build_body_line_indices(&body_line_matches);
        Self {
            nodes,
            edges,
            annotations,
            body_line_matches,
            incoming,
            outgoing,
            annotations_by_source,
            body_line_matches_by_source,
            body_line_matches_by_rule,
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

    /// Reverse lookup: find the node whose on-disk path matches.
    /// Path comparison is forward-slash-normalised on both sides; the
    /// caller is expected to have pre-normalised user input (e.g. via
    /// `path_guard::normalize_for_lookup`) to handle `./` prefixes
    /// and absolute paths. Returns `None` for any path not in the
    /// scanned set — including paths excluded by `[scope]`.
    ///
    /// Linear over `nodes()`. The graph never indexes by path because
    /// (a) id is the canonical identifier everywhere else in the API
    /// and (b) the secondary index would have to be rebuilt on every
    /// rename. Reverse lookup is rare (only at user-input boundaries
    /// like `query node --path`) so the O(n) scan is acceptable.
    pub fn node_by_path(&self, path: &std::path::Path) -> Option<&Node> {
        let needle = crate::path_guard::forward_string(path);
        self.nodes
            .values()
            .find(|n| crate::path_guard::forward_string(&n.path) == needle)
    }

    /// Like [`Self::node_by_path`] but returns a typed
    /// [`crate::error::Error::MissingNode`] for the not-found case —
    /// symmetric with [`Self::require_node`]. CLI handlers consume
    /// this to surface a single canonical error shape regardless of
    /// which lookup key the user supplied.
    pub fn require_node_by_path(&self, path: &std::path::Path) -> crate::error::Result<&Node> {
        self.node_by_path(path).ok_or_else(|| {
            crate::error::Error::MissingNode(format!(
                "path={}",
                crate::path_guard::forward_string(path)
            ))
        })
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
    /// `(name, key, source, line)` for deterministic output.
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

    /// Every body-line regex match extracted at build time. Sorted by
    /// `(rule_name, source, line)` for deterministic output.
    pub fn body_line_matches(&self) -> &[BodyLineMatch] {
        &self.body_line_matches
    }

    /// Body-line matches against a specific `[[rules.body_line]]`
    /// block. Consumed by [`crate::rules::body_line::BodyLineRule`] so
    /// each per-block instance reads only its own match set.
    pub fn body_line_matches_for_rule(&self, rule_name: &str) -> Vec<&BodyLineMatch> {
        self.body_line_matches_by_rule
            .get(rule_name)
            .map(|idxs| {
                idxs.iter()
                    .filter_map(|&i| self.body_line_matches.get(i))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Body-line matches whose source is `id`. Symmetric to
    /// [`Self::annotations_from`].
    pub fn body_line_matches_from(&self, id: &str) -> Vec<&BodyLineMatch> {
        self.body_line_matches_by_source
            .get(id)
            .map(|idxs| {
                idxs.iter()
                    .filter_map(|&i| self.body_line_matches.get(i))
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
        by_source.entry(ann.source.clone()).or_default().push(idx);
    }
    by_source
}

fn build_body_line_indices(
    matches: &[BodyLineMatch],
) -> (BTreeMap<String, Vec<usize>>, BTreeMap<String, Vec<usize>>) {
    let mut by_source: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut by_rule: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (idx, m) in matches.iter().enumerate() {
        by_source.entry(m.source.clone()).or_default().push(idx);
        by_rule.entry(m.rule_name.clone()).or_default().push(idx);
    }
    (by_source, by_rule)
}

/// Serialised schema revision. Every breaking change to the on-disk
/// shape of `graph.json` bumps this; readers refuse any file whose
/// recorded version does not equal `SCHEMA_VERSION`, with
/// `nodex build --full` as the escape hatch.
pub const SCHEMA_VERSION: u32 = 9;

/// Serialise nodes + edges + annotations + body-line matches with a
/// schema-version envelope. Indices are derived state and
/// intentionally omitted.
impl Serialize for Graph {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("Graph", 5)?;
        s.serialize_field("schema_version", &SCHEMA_VERSION)?;
        s.serialize_field("nodes", &self.nodes)?;
        s.serialize_field("edges", &self.edges)?;
        s.serialize_field("annotations", &self.annotations)?;
        s.serialize_field("body_line_matches", &self.body_line_matches)?;
        s.end()
    }
}

impl<'de> Deserialize<'de> for Graph {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            schema_version: u32,
            nodes: IndexMap<String, Node>,
            edges: Vec<Edge>,
            #[serde(default)]
            annotations: Vec<Annotation>,
            #[serde(default)]
            body_line_matches: Vec<BodyLineMatch>,
        }

        let raw = Raw::deserialize(deserializer)?;
        if raw.schema_version != SCHEMA_VERSION {
            return Err(serde::de::Error::custom(format!(
                "graph.json schema_version {} does not match this binary ({}); \
                 run `nodex build --full` to regenerate",
                raw.schema_version, SCHEMA_VERSION
            )));
        }
        Ok(Graph::new(
            raw.nodes,
            raw.edges,
            raw.annotations,
            raw.body_line_matches,
        ))
    }
}

impl std::fmt::Debug for Graph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Graph")
            .field("nodes", &self.nodes.len())
            .field("edges", &self.edges.len())
            .field("annotations", &self.annotations.len())
            .field("body_line_matches", &self.body_line_matches.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_rejects_outdated_schema_version() {
        // A graph.json produced by an older nodex binary must not
        // silently load — the schema envelope is part of the
        // contract. Operators see the "run nodex build --full"
        // message; consumers see a typed deserialisation error.
        let raw = format!(
            r#"{{"schema_version": {}, "nodes": {{}}, "edges": []}}"#,
            SCHEMA_VERSION - 1
        );
        let err = serde_json::from_str::<Graph>(&raw).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("schema_version"),
            "error must mention schema_version: {msg}"
        );
        assert!(
            msg.contains("nodex build --full"),
            "error must hint at the regeneration command: {msg}"
        );
    }
}
