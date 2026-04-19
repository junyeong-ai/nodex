//! Unified issue report — single query that surfaces every actionable
//! problem in the graph, so an AI agent can discover "what needs fixing"
//! in a single round-trip instead of composing four separate queries.
//!
//! All collectors defer to existing functions; this module is pure
//! composition and adds a summary aggregate.

use serde::Serialize;
use std::collections::BTreeMap;

use crate::config::Config;
use crate::model::{Edge, Graph, ResolvedTarget};
use crate::rules::{Violation, check_all};

use super::detect::{OrphanEntry, StaleEntry, find_orphans, find_stale};

/// Stable category keys used in [`IssueSummary::by_category`].
///
/// Exposed as `const` so command-line consumers and tests reference the
/// same identifiers; violations are reported as `violation_<rule_id>`.
pub mod categories {
    pub const ORPHAN: &str = "orphan";
    pub const STALE: &str = "stale";
    pub const UNRESOLVED_EDGE: &str = "unresolved_edge";
    pub const VIOLATION_PREFIX: &str = "violation_";
}

/// A single unresolved outgoing edge. Surfaced so the agent can fix the
/// dangling reference (rename, create missing doc, or delete the link).
#[derive(Debug, Clone, Serialize)]
pub struct UnresolvedEdge {
    pub source_id: String,
    pub source_path: String,
    pub relation: String,
    pub raw_target: String,
    pub reason: String,
    pub location: String,
}

/// Aggregate of all actionable problems in the graph.
#[derive(Debug, Serialize)]
pub struct IssueReport {
    pub orphans: Vec<OrphanEntry>,
    pub stale: Vec<StaleEntry>,
    pub unresolved_edges: Vec<UnresolvedEdge>,
    pub violations: Vec<Violation>,
    pub summary: IssueSummary,
}

/// Counts by category for quick triage. Uses [`BTreeMap`] so the
/// serialized JSON key order is deterministic.
#[derive(Debug, Serialize)]
pub struct IssueSummary {
    pub total: usize,
    pub by_category: BTreeMap<String, usize>,
}

/// Build the full issue report.
///
/// This is intentionally a pure function over the graph — every field
/// can be computed by an external caller using existing APIs; this
/// exists so the common AI-agent question "what's broken?" resolves in
/// a single call.
pub fn collect_issues(graph: &Graph, config: &Config) -> IssueReport {
    let orphans = find_orphans(graph, config);
    let stale = find_stale(graph, config);
    let unresolved_edges = find_unresolved_edges(graph);
    let violations = check_all(graph, config);

    let mut by_category: BTreeMap<String, usize> = BTreeMap::new();
    if !orphans.is_empty() {
        by_category.insert(categories::ORPHAN.to_string(), orphans.len());
    }
    if !stale.is_empty() {
        by_category.insert(categories::STALE.to_string(), stale.len());
    }
    if !unresolved_edges.is_empty() {
        by_category.insert(
            categories::UNRESOLVED_EDGE.to_string(),
            unresolved_edges.len(),
        );
    }
    for v in &violations {
        let key = format!("{}{}", categories::VIOLATION_PREFIX, v.rule_id);
        *by_category.entry(key).or_insert(0) += 1;
    }

    let total = orphans.len() + stale.len() + unresolved_edges.len() + violations.len();

    IssueReport {
        orphans,
        stale,
        unresolved_edges,
        violations,
        summary: IssueSummary { total, by_category },
    }
}

/// Collect every edge whose target failed to resolve during build.
pub fn find_unresolved_edges(graph: &Graph) -> Vec<UnresolvedEdge> {
    let mut entries: Vec<UnresolvedEdge> = graph
        .edges()
        .iter()
        .filter_map(|edge| unresolved_from(graph, edge))
        .collect();

    entries.sort_by(|a, b| {
        a.source_id
            .cmp(&b.source_id)
            .then_with(|| a.relation.cmp(&b.relation))
            .then_with(|| a.raw_target.cmp(&b.raw_target))
    });

    entries
}

fn unresolved_from(graph: &Graph, edge: &Edge) -> Option<UnresolvedEdge> {
    let ResolvedTarget::Unresolved { raw, reason } = &edge.target else {
        return None;
    };
    let source_path = graph
        .nodes()
        .get(&edge.source)
        .map(|n| n.path.to_string_lossy().to_string())
        .unwrap_or_default();
    Some(UnresolvedEdge {
        source_id: edge.source.clone(),
        source_path,
        relation: edge.relation.clone(),
        raw_target: raw.clone(),
        reason: reason.clone(),
        location: edge.location.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Confidence, Kind, Node, Status};
    use indexmap::IndexMap;
    use std::path::PathBuf;

    fn node(id: &str) -> Node {
        Node {
            id: id.to_string(),
            path: PathBuf::from(format!("{id}.md")),
            title: id.to_string(),
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
            orphan_ok: true, // skip orphan detection
            attrs: Default::default(),
        }
    }

    #[test]
    fn finds_unresolved_edges() {
        let mut map = IndexMap::new();
        map.insert("a".into(), node("a"));
        let edges = vec![Edge {
            source: "a".to_string(),
            target: ResolvedTarget::unresolved("missing.md", "path not in scope"),
            relation: "references".to_string(),
            confidence: Confidence::Extracted,
            location: "L42".to_string(),
        }];
        let graph = Graph::new(map, edges);

        let unresolved = find_unresolved_edges(&graph);
        assert_eq!(unresolved.len(), 1);
        assert_eq!(unresolved[0].source_id, "a");
        assert_eq!(unresolved[0].raw_target, "missing.md");
        assert_eq!(unresolved[0].reason, "path not in scope");
    }

    #[test]
    fn empty_graph_has_no_issues() {
        let graph = Graph::new(IndexMap::new(), vec![]);
        let report = collect_issues(&graph, &Config::default());
        assert_eq!(report.summary.total, 0);
        assert!(report.summary.by_category.is_empty());
    }

    #[test]
    fn summary_counts_are_additive() {
        let mut map = IndexMap::new();
        map.insert("a".into(), node("a"));
        let edges = vec![
            Edge {
                source: "a".to_string(),
                target: ResolvedTarget::unresolved("x.md", "not found"),
                relation: "references".to_string(),
                confidence: Confidence::Extracted,
                location: "L1".to_string(),
            },
            Edge {
                source: "a".to_string(),
                target: ResolvedTarget::unresolved("y.md", "not found"),
                relation: "references".to_string(),
                confidence: Confidence::Extracted,
                location: "L2".to_string(),
            },
        ];
        let graph = Graph::new(map, edges);
        let report = collect_issues(&graph, &Config::default());
        assert_eq!(report.unresolved_edges.len(), 2);
        assert_eq!(report.summary.by_category[categories::UNRESOLVED_EDGE], 2);
    }
}
