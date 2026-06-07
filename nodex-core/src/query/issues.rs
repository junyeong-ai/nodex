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
use crate::rules::{SkippedRule, Violation, check};

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
    /// Links whose target exists on disk but sits outside scan scope
    /// (most commonly `[[scope.conditional_exclude]]`). Tracked
    /// separately and kept out of `summary.total`: the reference points
    /// at a real, intentionally-ungraphed file, so it is informational —
    /// not a broken link the operator must fix.
    pub const EXCLUDED_TARGET: &str = "excluded_target";
}

/// A single unresolved outgoing edge. Surfaced so the agent can fix the
/// dangling reference (rename, create missing doc, or delete the link).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct UnresolvedEdge {
    pub source: String,
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
    /// `IdNotFound` since they never had a path to stat. Named `cause`
    /// (not `kind`) to avoid colliding with a document's `kind`.
    pub cause: UnresolvedCause,
}

/// Why a target could not be resolved. Stable JSON surface so external
/// tooling can branch on the cause without string-matching `reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnresolvedCause {
    /// Frontmatter id relation (`supersedes` / `implements` /
    /// `related` / `superseded_by`) whose value isn't a known node id.
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

/// Build the full issue report — orphans, stale, unresolved edges, and
/// rule violations — in a single call. This composition exists so the
/// common AI-agent question "what's broken?" resolves in one round-trip
/// instead of four separate queries; every field can also be computed
/// by an external caller using the underlying APIs.
///
/// **Filesystem side effect.** Despite the "pure composition" framing,
/// this function is *not* pure over the graph alone: classifying
/// unresolved edges calls [`find_unresolved_edges`], which probes the
/// filesystem to distinguish "target file missing on disk" (`Missing`)
/// from "target file present but outside scan scope, most commonly via
/// `[[scope.conditional_exclude]]`" (`ExcludedFromScope`). The probe
/// joins paths against `root` (and the source file's parent directory)
/// and never reads file contents; it cannot reach beyond the project
/// root by construction. Callers that need a graph-only computation
/// can read the individual sub-reports (`find_orphans`, `find_stale`,
/// `rules::check`) directly.
pub fn find_issues(
    graph: &Graph,
    config: &Config,
    root: &Path,
    diff: Option<&crate::diff::GraphDiff>,
) -> IssueReport {
    let orphans = find_orphans(graph, config);
    let stale = find_stale(graph, config);
    let unresolved_edges = find_unresolved_edges(graph, root, &config.parser.extensions);
    // The caller supplies the same diff context `check` runs under (the
    // CLI resolves `rules.immutable_baseline` exactly as `nodex check`
    // does), so the violations reported here and by a default `check`
    // never diverge; `None` leaves the diff-aware rules self-reporting
    // as skipped, same as a baseline-less `check`.
    let report = check(graph, config, root, diff);

    let mut by_category: BTreeMap<String, usize> = BTreeMap::new();
    if !orphans.is_empty() {
        by_category.insert(categories::ORPHAN.to_string(), orphans.len());
    }
    if !stale.is_empty() {
        by_category.insert(categories::STALE.to_string(), stale.len());
    }
    // A link to an on-disk-but-excluded file is not a broken link — the
    // target exists, it just isn't graphed. Count those separately and
    // keep them out of `total` so "what's broken?" isn't inflated by
    // intentional out-of-scope references, while still surfacing them
    // (never a silent drop).
    let excluded = unresolved_edges
        .iter()
        .filter(|e| e.cause == UnresolvedCause::ExcludedFromScope)
        .count();
    let broken_edges = unresolved_edges.len() - excluded;
    if broken_edges > 0 {
        by_category.insert(categories::UNRESOLVED_EDGE.to_string(), broken_edges);
    }
    if excluded > 0 {
        by_category.insert(categories::EXCLUDED_TARGET.to_string(), excluded);
    }
    for v in &report.violations {
        let key = format!("{}{}", categories::VIOLATION_PREFIX, v.rule_id);
        *by_category.entry(key).or_insert(0) += 1;
    }

    let total = orphans.len() + stale.len() + broken_edges + report.violations.len();

    IssueReport {
        orphans,
        stale,
        unresolved_edges,
        violations: report.violations,
        skipped_rules: report.skipped_rules,
        summary: IssueSummary { total, by_category },
    }
}

/// Collect every edge whose target failed to resolve during build.
///
/// Walks every unresolved edge and classifies the cause into typed
/// [`UnresolvedCause`] — including a filesystem stat for body-link
/// targets so the common "looks missing but is actually excluded by
/// `[[scope.conditional_exclude]]`" case surfaces as
/// `ExcludedFromScope` instead of a generic `Missing`.
pub fn find_unresolved_edges(
    graph: &Graph,
    root: &Path,
    extensions: &[String],
) -> Vec<UnresolvedEdge> {
    let mut entries: Vec<UnresolvedEdge> = graph
        .edges()
        .iter()
        .filter_map(|edge| unresolved_from(graph, edge, root, extensions))
        .collect();

    entries.sort_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then_with(|| a.relation.cmp(&b.relation))
            .then_with(|| a.raw_target.cmp(&b.raw_target))
    });

    entries
}

fn unresolved_from(
    graph: &Graph,
    edge: &Edge,
    root: &Path,
    extensions: &[String],
) -> Option<UnresolvedEdge> {
    let ResolvedTarget::Unresolved { raw, reason } = &edge.target else {
        return None;
    };
    let source_node = graph.nodes().get(&edge.source);
    let source_path = source_node
        .map(|n| crate::path_guard::forward_string(&n.path))
        .unwrap_or_default();
    // `covers` is a path-only out-of-graph relation; every other relation
    // is a document reference that resolves through the extension ladder.
    // Mirrors the resolver's own `document_ref` split.
    let document_ref = edge.relation != "covers";
    let cause = classify_unresolved(
        reason,
        raw,
        source_node.map(|n| n.path.as_path()),
        root,
        extensions,
        document_ref,
    );
    Some(UnresolvedEdge {
        source: edge.source.clone(),
        source_path,
        relation: edge.relation.clone(),
        raw_target: raw.clone(),
        reason: reason.clone(),
        location: edge.location.clone(),
        cause,
    })
}

/// Map a resolver-emitted `reason` string into typed [`UnresolvedCause`].
/// For the path-based `path not found in scope` case, also probe the
/// filesystem — when the file actually exists, the cause is exclusion
/// (most commonly `conditional_exclude`), not absence. The fall-through
/// remains `Missing`.
fn classify_unresolved(
    reason: &str,
    raw: &str,
    source_path: Option<&Path>,
    root: &Path,
    extensions: &[String],
    document_ref: bool,
) -> UnresolvedCause {
    match reason {
        "node id not found in graph" => UnresolvedCause::IdNotFound,
        "absolute paths are not in scope" => UnresolvedCause::Absolute,
        "path escapes source scope" => UnresolvedCause::EscapesSource,
        "path not found in scope" => {
            if target_exists_on_disk(raw, source_path, root, extensions, document_ref) {
                UnresolvedCause::ExcludedFromScope
            } else {
                UnresolvedCause::Missing
            }
        }
        _ => UnresolvedCause::Missing,
    }
}

/// True if `raw` resolves to a regular file under `root`. Probes the exact
/// candidate set the resolver tried — [`crate::builder::resolver::reference_path_candidates`]
/// — under both the root-relative and source-relative interpretations, so
/// an extension-less `[[guide]]` whose `guide.md` is excluded from scope
/// classifies as `ExcludedFromScope`, not a generic `Missing`. Sharing the
/// candidate generator with the resolver is what keeps the two from
/// drifting apart. Symlinks are followed (consistent with the scanner).
fn target_exists_on_disk(
    raw: &str,
    source_path: Option<&Path>,
    root: &Path,
    extensions: &[String],
    document_ref: bool,
) -> bool {
    // Stat a candidate only after normalising it to a root-relative path
    // and confirming it doesn't escape — the same containment the
    // resolver applies (`path_guard::normalize_relative`). Without this,
    // a `../sibling.md` link could stat a file *outside* the project and
    // be misclassified as `ExcludedFromScope`, hiding a broken link from
    // the issue count.
    let in_root = |rel: &Path| {
        crate::path_guard::normalize_relative(rel).is_some_and(|n| root.join(n).is_file())
    };
    let bases = crate::builder::resolver::reference_path_candidates(raw, extensions, document_ref);
    for base in &bases {
        // Root-relative interpretation (mirrors the resolver's direct match).
        if in_root(Path::new(base)) {
            return true;
        }
        // Source-relative interpretation (mirrors the resolver's second try).
        if let Some(parent) = source_path.and_then(Path::parent)
            && in_root(&parent.join(base))
        {
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
    fn escaping_link_is_not_classified_as_on_disk() {
        // A `../secret.md` link must NOT be reported as on-disk (and so
        // `ExcludedFromScope`) just because a same-named file exists as a
        // sibling of the project root — that would hide a broken link
        // from the issue count. The escaping candidate is rejected; only
        // the in-root interpretation may stat.
        let base = tempfile::tempdir().expect("tempdir");
        let root = base.path().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(base.path().join("secret.md"), "x").unwrap();

        assert!(
            !target_exists_on_disk(
                "../secret.md",
                Some(Path::new("a.md")),
                &root,
                &[".md".to_string()],
                true,
            ),
            "a link escaping the project root must not count as on-disk"
        );

        // The in-root interpretation still resolves through `..`.
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs/real.md"), "x").unwrap();
        assert!(
            target_exists_on_disk(
                "../docs/real.md",
                Some(Path::new("guides/g.md")),
                &root,
                &[".md".to_string()],
                true,
            ),
            "an in-root `..` path must still resolve"
        );
    }

    #[test]
    fn target_exists_on_disk_applies_the_extension_ladder() {
        // An extension-less `[[guide]]` whose `guide.md` is on disk (but
        // excluded from the graph) must be seen as on-disk — the probe
        // mirrors the resolver's extension append, so the cause classifies
        // as `ExcludedFromScope`, not a generic `Missing`.
        let base = tempfile::tempdir().expect("tempdir");
        let root = base.path().join("proj");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs/guide.md"), "x").unwrap();

        assert!(
            target_exists_on_disk("docs/guide", None, &root, &[".md".to_string()], true),
            "an extension-less target must match `docs/guide.md` via the extension ladder"
        );
        assert_eq!(
            classify_unresolved(
                "path not found in scope",
                "docs/guide",
                None,
                &root,
                &[".md".to_string()],
                true,
            ),
            UnresolvedCause::ExcludedFromScope,
        );

        // A `covers` target (document_ref = false) is path-only: it must
        // NOT extension-append, so `docs/guide` does not match
        // `docs/guide.md` — mirroring the resolver's `covers` handling
        // exactly via the shared candidate generator.
        assert!(
            !target_exists_on_disk("docs/guide", None, &root, &[".md".to_string()], false),
            "covers must not extension-append in the disk probe"
        );
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
        let graph = Graph::new(map, edges, vec![], vec![]);

        let unresolved = find_unresolved_edges(&graph, Path::new("."), &[".md".to_string()]);
        assert_eq!(unresolved.len(), 1);
        assert_eq!(unresolved[0].source, "a");
        assert_eq!(unresolved[0].raw_target, "missing.md");
        assert_eq!(unresolved[0].reason, "path not in scope");
        // Reason string is unrecognised by the classifier → falls back
        // to `Missing` (target also doesn't exist on disk under the
        // test root).
        assert_eq!(unresolved[0].cause, UnresolvedCause::Missing);
    }

    #[test]
    fn empty_graph_has_no_issues() {
        let graph = Graph::new(IndexMap::new(), vec![], vec![], vec![]);
        let report = find_issues(&graph, &Config::default(), Path::new("."), None);
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
        let graph = Graph::new(map, edges, vec![], vec![]);
        let report = find_issues(&graph, &Config::default(), Path::new("."), None);
        assert_eq!(report.unresolved_edges.len(), 2);
        assert_eq!(report.summary.by_category[categories::UNRESOLVED_EDGE], 2);
    }
}
