use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::annotation::Annotation;
use super::body_line_match::BodyLineMatch;
use super::edge::Edge;
use super::node::Node;

/// An in-scope document that failed to parse and has no node —
/// first-class graph data, so `build` reports it structurally and the
/// node-less `parse_failure` check rule fires from it. `path` is the
/// forward-slash project-root-relative path; `message` carries the
/// full error chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ParseFailure {
    pub path: String,
    pub message: String,
    /// SHA-256 of the exact bytes the failed parse consumed (the same
    /// digest the build cache keys on), so a snapshot consumer can
    /// distinguish "same broken bytes" from "changed since build".
    /// When the build could not read the file at all (hard I/O
    /// failure) there are no bytes to hash and this is the empty
    /// string — a sentinel no real digest equals, so the status
    /// content probe can never confirm `current` for an unreadable
    /// file: its state stays unconfirmable until a build can read it.
    pub content_hash: String,
}

/// Build provenance recorded in `graph.json`: the binary version that
/// produced the snapshot and the hash of the graph-shaping config
/// surface (`builder::graph_config_hash`). The snapshot self-describes
/// — staleness probes need no oracle outside the file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphMeta {
    pub nodex_version: String,
    pub config_hash: String,
}

/// Immutable document graph with pre-built adjacency indices.
/// Indices are derived state — rebuilt by [`Graph::new`] and by the
/// `Deserialize` impl.
pub struct Graph {
    nodes: IndexMap<String, Node>,
    edges: Vec<Edge>,
    annotations: Vec<Annotation>,
    body_line_matches: Vec<BodyLineMatch>,
    parse_failures: Vec<ParseFailure>,
    meta: GraphMeta,
    incoming: BTreeMap<String, Vec<usize>>,
    outgoing: BTreeMap<String, Vec<usize>>,
    body_line_matches_by_rule: BTreeMap<String, Vec<usize>>,
}

impl Graph {
    /// Construct from canonical parts. Adjacency indices over edges and
    /// the per-rule body-line index are derived in one pass — callers
    /// never thread them in.
    pub fn new(
        nodes: IndexMap<String, Node>,
        edges: Vec<Edge>,
        annotations: Vec<Annotation>,
        body_line_matches: Vec<BodyLineMatch>,
        parse_failures: Vec<ParseFailure>,
        meta: GraphMeta,
    ) -> Self {
        let (incoming, outgoing) = build_edge_indices(&edges);
        let body_line_matches_by_rule = build_body_line_indices(&body_line_matches);
        Self {
            nodes,
            edges,
            annotations,
            body_line_matches,
            parse_failures,
            meta,
            incoming,
            outgoing,
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
            .ok_or_else(|| crate::error::Error::MissingNode {
                asked: crate::error::Lookup::Id(id.to_string()),
                corpus: self.corpus(),
            })
    }

    /// What this graph held, for a lookup that missed it. Derived here because
    /// the graph is what a lookup was made against: a miss over no nodes at
    /// all is not a fact about the id, and the remedy that clears it is not a
    /// corrected one.
    pub fn corpus(&self) -> crate::error::Corpus {
        if !self.nodes.is_empty() {
            crate::error::Corpus::Documents
        } else if self.parse_failures.is_empty() {
            crate::error::Corpus::Empty
        } else {
            crate::error::Corpus::OnlyParseFailures
        }
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
        self.node_by_path(path)
            .ok_or_else(|| crate::error::Error::MissingNode {
                asked: crate::error::Lookup::Path(path.to_path_buf()),
                corpus: self.corpus(),
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

    /// In-scope documents that failed to parse and have no node.
    /// Sorted by path for deterministic output. Consumed by the
    /// node-less `parse_failure` check rule and surfaced structurally
    /// on the build result.
    pub fn parse_failures(&self) -> &[ParseFailure] {
        &self.parse_failures
    }

    /// Build provenance recorded with the snapshot.
    pub fn meta(&self) -> &GraphMeta {
        &self.meta
    }

    /// Body-line matches against a specific `[[rules.body_line]]`
    /// block. Consumed by [`crate::rules::body_line::BodyLineRule`] so
    /// each per-block instance reads only its own match set.
    pub fn body_line_matches_for_rule(&self, name: &str) -> Vec<&BodyLineMatch> {
        self.body_line_matches_by_rule
            .get(name)
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

fn build_body_line_indices(matches: &[BodyLineMatch]) -> BTreeMap<String, Vec<usize>> {
    let mut by_rule: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (idx, m) in matches.iter().enumerate() {
        by_rule.entry(m.name.clone()).or_default().push(idx);
    }
    by_rule
}

/// Serialised schema revision. Every breaking change to the on-disk
/// shape of `graph.json` bumps this; readers refuse any file whose
/// recorded version does not equal `SCHEMA_VERSION`, with
/// `nodex build --full` as the escape hatch.
pub const SCHEMA_VERSION: u32 = 12;

/// Serialise meta + nodes + edges + annotations + body-line matches +
/// parse failures with a schema-version envelope. Indices are derived
/// state and intentionally omitted.
impl Serialize for Graph {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("Graph", 7)?;
        s.serialize_field("schema_version", &SCHEMA_VERSION)?;
        s.serialize_field("meta", &self.meta)?;
        s.serialize_field("nodes", &self.nodes)?;
        s.serialize_field("edges", &self.edges)?;
        s.serialize_field("annotations", &self.annotations)?;
        s.serialize_field("body_line_matches", &self.body_line_matches)?;
        s.serialize_field("parse_failures", &self.parse_failures)?;
        s.end()
    }
}

impl<'de> Deserialize<'de> for Graph {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            schema_version: u32,
            // `meta` and `parse_failures` default so a file from an
            // older schema still fails through the version-mismatch
            // message below — the version gate is the single designed
            // rejection seam, never a "missing field" error.
            #[serde(default)]
            meta: GraphMeta,
            nodes: IndexMap<String, Node>,
            edges: Vec<Edge>,
            #[serde(default)]
            annotations: Vec<Annotation>,
            #[serde(default)]
            body_line_matches: Vec<BodyLineMatch>,
            #[serde(default)]
            parse_failures: Vec<ParseFailure>,
        }

        let raw = Raw::deserialize(deserializer)?;
        if raw.schema_version != SCHEMA_VERSION {
            return Err(serde::de::Error::custom(format!(
                "graph.json schema_version {} does not match this binary ({}); \
                 run `nodex build --full` to regenerate",
                raw.schema_version, SCHEMA_VERSION
            )));
        }
        // The `nodes` map is keyed by id; every lookup and traversal
        // trusts key == `node.id`. A hand-edited or merge-mangled
        // `graph.json` where they differ would make a node findable by key
        // yet dereference a missing entry on its own-id traversal — a
        // panic on user data. Reject it as a parse error so every consumer
        // keeps the invariant for free.
        if let Some((key, node)) = raw.nodes.iter().find(|(key, node)| *key != &node.id) {
            return Err(serde::de::Error::custom(format!(
                "graph.json node keyed {key:?} carries id {:?}; the map key must equal the \
                 node id — regenerate with `nodex build --full`",
                node.id
            )));
        }
        Ok(Graph::new(
            raw.nodes,
            raw.edges,
            raw.annotations,
            raw.body_line_matches,
            raw.parse_failures,
            raw.meta,
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
            .field("parse_failures", &self.parse_failures.len())
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
        // message; consumers see a typed deserialisation error. The
        // payload deliberately carries no `meta` / `parse_failures`:
        // both default during deserialisation precisely so an older
        // file fails through this version message rather than a
        // "missing field" error.
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

    #[test]
    fn deserialize_rejects_node_keyed_under_a_mismatched_id() {
        // A `graph.json` whose `nodes` map key differs from the embedded
        // `node.id` would be findable by key but panic on its own-id
        // traversal. Reject it as a parse error, not a panic on user data.
        let raw = format!(
            r#"{{"schema_version": {SCHEMA_VERSION}, "nodes": {{"a": {{"id": "b", "path": "a.md", "title": "A", "kind": "generic", "status": "active"}}}}, "edges": []}}"#
        );
        let err = serde_json::from_str::<Graph>(&raw).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("keyed") && msg.contains("\"a\"") && msg.contains("\"b\""),
            "error must name the mismatched key and id: {msg}"
        );
    }

    #[test]
    fn meta_and_parse_failures_round_trip_through_serialisation() {
        // Provenance and recorded drops are canonical graph data: a
        // serialise → deserialise cycle must preserve both byte-for-byte
        // so snapshot consumers read exactly what the build recorded.
        let graph = Graph::new(
            IndexMap::new(),
            vec![],
            vec![],
            vec![],
            vec![ParseFailure {
                path: "docs/bad.md".into(),
                message: "parse error at docs/bad.md: yaml: …".into(),
                content_hash: "abc123".into(),
            }],
            GraphMeta {
                nodex_version: "0.15.0".into(),
                config_hash: "deadbeef".into(),
            },
        );
        let json = serde_json::to_string(&graph).unwrap();
        let back: Graph = serde_json::from_str(&json).unwrap();
        assert_eq!(back.meta().nodex_version, "0.15.0");
        assert_eq!(back.meta().config_hash, "deadbeef");
        assert_eq!(back.parse_failures(), graph.parse_failures());
    }
}
