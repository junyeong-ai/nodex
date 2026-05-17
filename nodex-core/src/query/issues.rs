//! Unified issue report — single query that surfaces every actionable
//! problem in the graph, so an AI agent can discover "what needs fixing"
//! in a single round-trip instead of composing four separate queries.
//!
//! All collectors defer to existing functions; this module is pure
//! composition and adds a summary aggregate.

use schemars::JsonSchema;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

use crate::config::Config;
use crate::model::{Edge, Graph, ResolvedTarget};
use crate::rules::{SkippedRule, Violation, check_project};

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
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct UnresolvedEdge {
    pub source_id: String,
    pub source_path: String,
    pub relation: String,
    pub raw_target: String,
    pub reason: String,
    pub location: String,
    /// Typed classification of *why* the target failed to resolve.
    /// Lets consumers dispatch on cause without parsing
    /// [`UnresolvedEdge::reason`] strings — `Missing` vs.
    /// `ExcludedFromScope` drive different remediation (create vs.
    /// re-include / delete link), and frontmatter-id relations carry
    /// `IdNotFound` since they never had a path to stat.
    pub kind: UnresolvedKind,
}

/// Why a target could not be resolved. Stable JSON surface so external
/// tooling can branch on the cause without string-matching `reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnresolvedKind {
    /// Frontmatter id relation (`supersedes` / `implements` /
    /// `related`) whose value isn't a known node id.
    IdNotFound,
    /// Body-link path that doesn't correspond to any file on disk
    /// under the project root.
    Missing,
    /// Body-link path whose file exists on disk but isn't in the
    /// graph's scan scope — most commonly removed by
    /// `[[scope.conditional_exclude]]` on a terminal-status parent.
    ExcludedFromScope,
    /// Body-link path that walks above the source file's directory
    /// via `..` segments. Refused as a security guard, never resolved.
    EscapesSource,
    /// Body-link path written as an absolute path. Refused as out of
    /// project scope.
    Absolute,
}

/// Aggregate of all actionable problems in the graph.
#[derive(Debug, Serialize, JsonSchema)]
pub struct IssueReport {
    pub orphans: Vec<OrphanEntry>,
    pub stale: Vec<StaleEntry>,
    pub unresolved_edges: Vec<UnresolvedEdge>,
    pub violations: Vec<Violation>,
    /// Rules that the runner declined to evaluate, with their reason.
    /// Surfaced alongside violations so a consumer never has to guess
    /// "did this rule pass, or did it never run?".
    pub skipped_rules: Vec<SkippedRule>,
    pub summary: IssueSummary,
}

/// Counts by category for quick triage. Uses [`BTreeMap`] so the
/// serialized JSON key order is deterministic.
#[derive(Debug, Serialize, JsonSchema)]
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
pub fn collect_issues(graph: &Graph, config: &Config, root: &Path) -> IssueReport {
    let orphans = find_orphans(graph, config);
    let stale = find_stale(graph, config);
    let unresolved_edges = find_unresolved_edges(graph, root);
    let report = check_project(graph, config, root);

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
    for v in &report.violations {
        let key = format!("{}{}", categories::VIOLATION_PREFIX, v.rule_id);
        *by_category.entry(key).or_insert(0) += 1;
    }

    let total = orphans.len() + stale.len() + unresolved_edges.len() + report.violations.len();

    IssueReport {
        orphans,
        stale,
        unresolved_edges,
        violations: report.violations,
        skipped_rules: report.skipped,
        summary: IssueSummary { total, by_category },
    }
}

/// Collect every edge whose target failed to resolve during build.
///
/// Walks every unresolved edge and classifies the cause into typed
/// [`UnresolvedKind`] — including a filesystem stat for body-link
/// targets so the common "looks missing but is actually excluded by
/// `[[scope.conditional_exclude]]`" case surfaces as
/// `ExcludedFromScope` instead of a generic `Missing`.
pub fn find_unresolved_edges(graph: &Graph, root: &Path) -> Vec<UnresolvedEdge> {
    let mut entries: Vec<UnresolvedEdge> = graph
        .edges()
        .iter()
        .filter_map(|edge| unresolved_from(graph, edge, root))
        .collect();

    entries.sort_by(|a, b| {
        a.source_id
            .cmp(&b.source_id)
            .then_with(|| a.relation.cmp(&b.relation))
            .then_with(|| a.raw_target.cmp(&b.raw_target))
    });

    entries
}

fn unresolved_from(graph: &Graph, edge: &Edge, root: &Path) -> Option<UnresolvedEdge> {
    let ResolvedTarget::Unresolved { raw, reason } = &edge.target else {
        return None;
    };
    let source_node = graph.nodes().get(&edge.source);
    let source_path = source_node
        .map(|n| crate::path_guard::forward_string(&n.path))
        .unwrap_or_default();
    let kind = classify_unresolved(reason, raw, source_node.map(|n| n.path.as_path()), root);
    Some(UnresolvedEdge {
        source_id: edge.source.clone(),
        source_path,
        relation: edge.relation.clone(),
        raw_target: raw.clone(),
        reason: reason.clone(),
        location: edge.location.clone(),
        kind,
    })
}

/// Map a resolver-emitted `reason` string into typed [`UnresolvedKind`].
/// For the path-based `path not found in scope` case, also probe the
/// filesystem — when the file actually exists, the cause is exclusion
/// (most commonly `conditional_exclude`), not absence. The fall-through
/// remains `Missing`.
fn classify_unresolved(
    reason: &str,
    raw: &str,
    source_path: Option<&Path>,
    root: &Path,
) -> UnresolvedKind {
    match reason {
        "node id not found in graph" => UnresolvedKind::IdNotFound,
        "absolute paths are not in scope" => UnresolvedKind::Absolute,
        "path escapes source scope" => UnresolvedKind::EscapesSource,
        "path not found in scope" => {
            if target_exists_on_disk(raw, source_path, root) {
                UnresolvedKind::ExcludedFromScope
            } else {
                UnresolvedKind::Missing
            }
        }
        _ => UnresolvedKind::Missing,
    }
}

/// True if `raw` resolves to a regular file under `root`, either as a
/// project-root-relative path or relative to the source file's
/// directory — matching the two interpretations the resolver itself
/// attempts before giving up. Symlinks are followed (consistent with
/// the scanner) so a link-target that exists behind a symlink counts
/// as on-disk.
fn target_exists_on_disk(raw: &str, source_path: Option<&Path>, root: &Path) -> bool {
    let candidate_root = root.join(raw);
    if candidate_root.is_file() {
        return true;
    }
    if let Some(source) = source_path
        && let Some(parent) = source.parent()
    {
        let candidate_rel = root.join(parent).join(raw);
        if candidate_rel.is_file() {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Kind, Node, Status};
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
            covers: vec![],
            orphan_ok: true, // skip orphan detection
            attrs: Default::default(),
            body_hash: String::new(),
            body_lines_hash: Vec::new(),
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
            location: "L42".to_string(),
        }];
        let graph = Graph::new(map, edges, vec![], vec![], vec![]);

        let unresolved = find_unresolved_edges(&graph, Path::new("."));
        assert_eq!(unresolved.len(), 1);
        assert_eq!(unresolved[0].source_id, "a");
        assert_eq!(unresolved[0].raw_target, "missing.md");
        assert_eq!(unresolved[0].reason, "path not in scope");
        // Reason string is unrecognised by the classifier → falls back
        // to `Missing` (target also doesn't exist on disk under the
        // test root).
        assert_eq!(unresolved[0].kind, UnresolvedKind::Missing);
    }

    #[test]
    fn empty_graph_has_no_issues() {
        let graph = Graph::new(IndexMap::new(), vec![], vec![], vec![], vec![]);
        let report = collect_issues(&graph, &Config::default(), Path::new("."));
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
                location: "L1".to_string(),
            },
            Edge {
                source: "a".to_string(),
                target: ResolvedTarget::unresolved("y.md", "not found"),
                relation: "references".to_string(),
                location: "L2".to_string(),
            },
        ];
        let graph = Graph::new(map, edges, vec![], vec![], vec![]);
        let report = collect_issues(&graph, &Config::default(), Path::new("."));
        assert_eq!(report.unresolved_edges.len(), 2);
        assert_eq!(report.summary.by_category[categories::UNRESOLVED_EDGE], 2);
    }
}
