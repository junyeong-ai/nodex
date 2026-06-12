//! Unified issue report — single query that surfaces every actionable
//! problem in the graph, so an AI agent can discover "what needs fixing"
//! in a single round-trip instead of composing four separate queries.
//!
//! All collectors defer to existing functions; this module is pure
//! composition and adds a summary aggregate.

use globset::GlobMatcher;
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

use crate::config::{Config, UnresolvedPolicyRuleConfig, UnresolvedSeverity};
use crate::model::{Edge, Graph, ParseFailure, ResolvedTarget, UnresolvedCause};
use crate::rules::{SkippedRule, Violation, check_with_unresolved};

use super::detect::{OrphanEntry, StaleEntry, find_orphans, find_stale};

/// Stable category keys used in [`IssueSummary::by_category`] —
/// re-exported from the model so the config validator (reserved
/// `[[detection.unresolved_policy]]` row names) and this report read
/// one vocabulary.
pub use crate::model::edge::categories;

/// A single unresolved outgoing edge. Surfaced so the agent can fix the
/// dangling reference (rename, create missing doc, or delete the link).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct UnresolvedEdge {
    pub source: String,
    pub source_path: String,
    pub relation: String,
    pub raw_target: String,
    /// Human prose for [`UnresolvedEdge::cause`] — its `Display`
    /// rendering, so the typed cause and the prose can never disagree.
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
    /// Severity the `[[detection.unresolved_policy]]` table assigns —
    /// per-edge attribution, so a downgrade is visible, never silent.
    /// Consumers branch on this instead of re-deriving the policy
    /// (actionable ⇔ not `info`).
    pub severity: UnresolvedSeverity,
    /// Name of the policy row that classified this edge; absent when
    /// the built-in fallthrough (`warning`) applied. Invariant:
    /// `severity == Info` ⇒ `policy_name` is `Some` — the fallthrough
    /// is always `warning`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_name: Option<String>,
}

/// Aggregate of all actionable problems in the graph.
///
/// Every unresolved edge appears in `unresolved_edges` whatever its
/// policy severity; an error-severity edge *also* appears in
/// `violations` (its gate record, `unresolved_reference/<name>`), and
/// only the violation increments `summary.total`.
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
    let unresolved_edges = find_unresolved_edges(graph, config, root);
    // The caller supplies the same diff context `check` runs under (the
    // CLI resolves `rules.immutable_baseline` exactly as `nodex check`
    // does), so the violations reported here and by a default `check`
    // never diverge; `None` leaves the diff-aware rules self-reporting
    // as skipped, same as a baseline-less `check`. The classification
    // computed above seeds the rule pass, so the per-row
    // `unresolved_reference` stat probes run once per report and the
    // violations derive from exactly the edges this report lists.
    let report = check_with_unresolved(graph, config, root, diff, unresolved_edges.clone());

    let mut by_category: BTreeMap<String, usize> = BTreeMap::new();
    if !orphans.is_empty() {
        by_category.insert(categories::ORPHAN.to_string(), orphans.len());
    }
    if !stale.is_empty() {
        by_category.insert(categories::STALE.to_string(), stale.len());
    }
    // Each unresolved edge increments exactly one counter — never
    // double-counted, never silently dropped. Warning-level edges (the
    // fallthrough plane) count under `unresolved_edge` in `total`;
    // info-level edges count under their policy row's name, out of
    // `total`; error-level edges are counted solely through their
    // `violation_unresolved_reference/<name>` category below.
    let mut warning_edges = 0usize;
    for edge in &unresolved_edges {
        match edge.severity {
            UnresolvedSeverity::Warning => warning_edges += 1,
            UnresolvedSeverity::Info => {
                let name = edge.policy_name.clone().expect(
                    "info severity is only assigned by a policy row, never the fallthrough",
                );
                *by_category.entry(name).or_insert(0) += 1;
            }
            UnresolvedSeverity::Error => {}
        }
    }
    if warning_edges > 0 {
        by_category.insert(categories::UNRESOLVED_EDGE.to_string(), warning_edges);
    }
    for v in &report.violations {
        let key = format!("{}{}", categories::VIOLATION_PREFIX, v.rule_id);
        *by_category.entry(key).or_insert(0) += 1;
    }

    let total = orphans.len() + stale.len() + warning_edges + report.violations.len();

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
/// Walks every unresolved edge, refines its resolver-recorded
/// [`UnresolvedCause`] — consulting [`Graph::parse_failures`]
/// (`TargetUnparsed`) and a filesystem stat (`ExcludedFromScope` vs.
/// `Missing`) for body-link targets — and assigns each edge its
/// `[[detection.unresolved_policy]]` severity: rows are tried in
/// declared order, the first whose `cause` matches and whose `glob`
/// (when present) matches a normalized resolution candidate wins, and
/// an unmatched edge falls through to `warning`. Row globs were
/// compiled once at `Config::load`; the recompile here cannot fail.
pub fn find_unresolved_edges(graph: &Graph, config: &Config, root: &Path) -> Vec<UnresolvedEdge> {
    let policy: Vec<(&UnresolvedPolicyRuleConfig, Option<GlobMatcher>)> = config
        .detection
        .unresolved_policy
        .iter()
        .map(|row| {
            let matcher = row.glob.as_deref().map(|glob| {
                globset::Glob::new(glob)
                    .expect("validated by Config::load")
                    .compile_matcher()
            });
            (row, matcher)
        })
        .collect();

    let mut entries: Vec<UnresolvedEdge> = graph
        .edges()
        .iter()
        .filter_map(|edge| unresolved_from(graph, edge, config, root, &policy))
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
    config: &Config,
    root: &Path,
    policy: &[(&UnresolvedPolicyRuleConfig, Option<GlobMatcher>)],
) -> Option<UnresolvedEdge> {
    let ResolvedTarget::Unresolved { raw, cause } = &edge.target else {
        return None;
    };
    let source_node = graph.nodes().get(&edge.source);
    let source_path = source_node
        .map(|n| crate::path_guard::forward_string(&n.path))
        .unwrap_or_default();
    // `covers` is a path-only out-of-graph relation; every other relation
    // is a document reference that resolves through the extension ladder.
    // The same closed-vocabulary dispatch as the resolver's: only the
    // frontmatter `covers:` field can produce the path-only relation.
    let document_ref = crate::model::edge::is_document_ref_relation(&edge.relation);
    // The one shared definition of "what could this link mean" —
    // consumed by the cause classifier's probes and the policy glob
    // matcher alike, so the two can never disagree with the resolver.
    // Pathless causes carry no resolution candidates: the same
    // `has_path_candidates` predicate that confines policy-row globs
    // at load gates the ladder here, so every consumer of the
    // candidate set reads it identically.
    let candidates = if cause.has_path_candidates() {
        crate::builder::resolver::normalized_resolution_candidates(
            raw,
            source_node.map(|n| n.path.as_path()),
            &config.parser.extensions,
            document_ref,
        )
    } else {
        Vec::new()
    };
    let cause = classify_unresolved(*cause, &candidates, graph.parse_failures(), root);
    let (severity, policy_name) = assign_policy(cause, &candidates, policy);
    Some(UnresolvedEdge {
        source: edge.source.clone(),
        source_path,
        relation: edge.relation.clone(),
        raw_target: raw.clone(),
        reason: cause.to_string(),
        location: edge.location.clone(),
        cause,
        severity,
        policy_name,
    })
}

/// First-match-wins policy assignment. A row matches when its `cause`
/// equals the edge's cause and its `glob` — compiled from the row,
/// matching any normalized root-relative resolution candidate — is
/// absent or matches (load-time validation confines globs to
/// path-carrying causes). The built-in fallthrough is `warning`,
/// unattributed.
fn assign_policy(
    cause: UnresolvedCause,
    candidates: &[String],
    policy: &[(&UnresolvedPolicyRuleConfig, Option<GlobMatcher>)],
) -> (UnresolvedSeverity, Option<String>) {
    for (row, matcher) in policy {
        if row.cause != cause {
            continue;
        }
        if let Some(matcher) = matcher
            && !candidates.iter().any(|c| matcher.is_match(c))
        {
            continue;
        }
        return (row.severity, Some(row.name.clone()));
    }
    (UnresolvedSeverity::Warning, None)
}

/// Refine the resolver-recorded [`UnresolvedCause`]. Only the
/// path-shaped [`UnresolvedCause::Missing`] — the build-time base for a
/// path the index does not contain — is refined, through two probes: a
/// candidate recorded in [`Graph::parse_failures`] is an in-scope
/// document that dropped (`TargetUnparsed`, never `ExcludedFromScope` —
/// the file is not excluded by design); a file that exists on disk is
/// exclusion (most commonly `conditional_exclude`); the fall-through
/// remains `Missing`. Every other cause is final at the refusal site.
fn classify_unresolved(
    cause: UnresolvedCause,
    candidates: &[String],
    parse_failures: &[ParseFailure],
    root: &Path,
) -> UnresolvedCause {
    match cause {
        UnresolvedCause::Missing => {
            if candidates
                .iter()
                .any(|c| parse_failures.iter().any(|f| &f.path == c))
            {
                UnresolvedCause::TargetUnparsed
            } else if target_exists_on_disk(candidates, root) {
                UnresolvedCause::ExcludedFromScope
            } else {
                UnresolvedCause::Missing
            }
        }
        final_cause => final_cause,
    }
}

/// True if any normalized resolution candidate is a regular file under
/// `root`. The candidates are the exact set the resolver tried
/// ([`crate::builder::resolver::normalized_resolution_candidates`]) —
/// already root-contained (escaping interpretations are dropped at
/// candidate generation, so a `../sibling.md` link can never stat a
/// file *outside* the project and be misclassified as
/// `ExcludedFromScope`), so an extension-less `[[guide]]` whose
/// `guide.md` is excluded from scope classifies as `ExcludedFromScope`,
/// not a generic `Missing`. The probe itself is the shared
/// case-sensitive ladder probe
/// ([`crate::builder::resolver::first_candidate_on_disk`]).
fn target_exists_on_disk(candidates: &[String], root: &Path) -> bool {
    crate::builder::resolver::first_candidate_on_disk(candidates, root).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GraphMeta, Kind, Node, Status};
    use indexmap::IndexMap;
    use std::path::PathBuf;

    fn node(id: &str) -> Node {
        node_at(id, &format!("{id}.md"))
    }

    fn node_at(id: &str, path: &str) -> Node {
        Node {
            id: id.to_string(),
            path: PathBuf::from(path),
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
            content_hash: String::new(),
            parse_issues: vec![],
        }
    }

    fn graph_of(nodes: Vec<Node>, edges: Vec<Edge>) -> Graph {
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n);
        }
        Graph::new(map, edges, vec![], vec![], vec![], GraphMeta::default())
    }

    fn dangling(source: &str, raw: &str, relation: &str) -> Edge {
        Edge {
            source: source.to_string(),
            target: ResolvedTarget::unresolved(raw, UnresolvedCause::Missing),
            relation: relation.to_string(),
            location: "L1".to_string(),
        }
    }

    fn row(
        name: &str,
        cause: UnresolvedCause,
        glob: Option<&str>,
        severity: UnresolvedSeverity,
    ) -> UnresolvedPolicyRuleConfig {
        UnresolvedPolicyRuleConfig {
            name: name.to_string(),
            cause,
            glob: glob.map(str::to_string),
            severity,
        }
    }

    fn policy_config(rows: Vec<UnresolvedPolicyRuleConfig>) -> Config {
        let mut config = Config::default();
        config.detection.unresolved_policy = rows;
        config
    }

    /// Whether `raw`, written from `source`, names a file on disk —
    /// builds the shared candidate ladder and runs the classifier's
    /// disk probe over it, so the tests below assert exactly what the
    /// classifier sees.
    fn on_disk(
        raw: &str,
        source: Option<&Path>,
        root: &Path,
        extensions: &[String],
        document_ref: bool,
    ) -> bool {
        let candidates = crate::builder::resolver::normalized_resolution_candidates(
            raw,
            source,
            extensions,
            document_ref,
        );
        target_exists_on_disk(&candidates, root)
    }

    fn classify(
        cause: UnresolvedCause,
        raw: &str,
        source: Option<&Path>,
        root: &Path,
        extensions: &[String],
        document_ref: bool,
    ) -> UnresolvedCause {
        let candidates = crate::builder::resolver::normalized_resolution_candidates(
            raw,
            source,
            extensions,
            document_ref,
        );
        classify_unresolved(cause, &candidates, &[], root)
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
            !on_disk(
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
            on_disk(
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
    fn target_exists_on_disk_is_case_sensitive() {
        // A link whose spelling differs only in letter case from a real
        // file is a broken link, not an excluded-from-scope one — on a
        // case-insensitive filesystem `is_file` would falsely match it
        // and the cause classifier would hide it from the broken count.
        let base = tempfile::tempdir().expect("tempdir");
        let root = base.path().join("proj");
        std::fs::create_dir_all(root.join("docs/guides")).unwrap();
        std::fs::write(root.join("docs/guides/intro.md"), "x").unwrap();

        // Exact spelling: present on disk.
        assert!(on_disk(
            "docs/guides/intro.md",
            None,
            &root,
            &[".md".to_string()],
            true,
        ));
        // Case-mismatched parent component: NOT a match (broken link),
        // on case-sensitive and case-insensitive filesystems alike.
        assert!(!on_disk(
            "docs/GUIDES/intro.md",
            None,
            &root,
            &[".md".to_string()],
            true,
        ));
        assert_eq!(
            classify(
                UnresolvedCause::Missing,
                "docs/GUIDES/intro.md",
                None,
                &root,
                &[".md".to_string()],
                true,
            ),
            UnresolvedCause::Missing,
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
            on_disk("docs/guide", None, &root, &[".md".to_string()], true),
            "an extension-less target must match `docs/guide.md` via the extension ladder"
        );
        assert_eq!(
            classify(
                UnresolvedCause::Missing,
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
            !on_disk("docs/guide", None, &root, &[".md".to_string()], false),
            "covers must not extension-append in the disk probe"
        );
    }

    #[test]
    fn finds_unresolved_edges() {
        let graph = graph_of(
            vec![node("a")],
            vec![Edge {
                source: "a".to_string(),
                target: ResolvedTarget::unresolved("missing.md", UnresolvedCause::Missing),
                relation: "references".to_string(),
                location: "L42".to_string(),
            }],
        );

        let unresolved = find_unresolved_edges(&graph, &Config::default(), Path::new("."));
        assert_eq!(unresolved.len(), 1);
        assert_eq!(unresolved[0].source, "a");
        assert_eq!(unresolved[0].raw_target, "missing.md");
        // The target doesn't exist on disk under the test root, so the
        // probes leave `Missing` standing; `reason` is its prose.
        assert_eq!(unresolved[0].cause, UnresolvedCause::Missing);
        assert_eq!(unresolved[0].reason, UnresolvedCause::Missing.to_string());
    }

    #[test]
    fn empty_graph_has_no_issues() {
        let graph = graph_of(vec![], vec![]);
        let report = find_issues(&graph, &Config::default(), Path::new("."), None);
        assert_eq!(report.summary.total, 0);
        assert!(report.summary.by_category.is_empty());
    }

    #[test]
    fn summary_counts_are_additive() {
        let graph = graph_of(
            vec![node("a")],
            vec![
                Edge {
                    source: "a".to_string(),
                    target: ResolvedTarget::unresolved("x.md", UnresolvedCause::Missing),
                    relation: "references".to_string(),
                    location: "L1".to_string(),
                },
                Edge {
                    source: "a".to_string(),
                    target: ResolvedTarget::unresolved("y.md", UnresolvedCause::Missing),
                    relation: "references".to_string(),
                    location: "L2".to_string(),
                },
            ],
        );
        let report = find_issues(&graph, &Config::default(), Path::new("."), None);
        assert_eq!(report.unresolved_edges.len(), 2);
        assert_eq!(report.summary.by_category[categories::UNRESOLVED_EDGE], 2);
    }

    #[test]
    fn policy_first_match_wins() {
        // Two rows match the same edge; declaration order decides.
        let root = tempfile::tempdir().expect("tempdir");
        let graph = graph_of(
            vec![node("a")],
            vec![dangling("a", "specs/x.md", "references")],
        );

        let narrow_first = policy_config(vec![
            row(
                "ephemeral-specs",
                UnresolvedCause::Missing,
                Some("specs/**"),
                UnresolvedSeverity::Info,
            ),
            row(
                "any-missing",
                UnresolvedCause::Missing,
                Some("**"),
                UnresolvedSeverity::Error,
            ),
        ]);
        let edges = find_unresolved_edges(&graph, &narrow_first, root.path());
        assert_eq!(edges[0].severity, UnresolvedSeverity::Info);
        assert_eq!(edges[0].policy_name.as_deref(), Some("ephemeral-specs"));

        let broad_first = policy_config(vec![
            row(
                "any-missing",
                UnresolvedCause::Missing,
                Some("**"),
                UnresolvedSeverity::Error,
            ),
            row(
                "ephemeral-specs",
                UnresolvedCause::Missing,
                Some("specs/**"),
                UnresolvedSeverity::Info,
            ),
        ]);
        let edges = find_unresolved_edges(&graph, &broad_first, root.path());
        assert_eq!(edges[0].severity, UnresolvedSeverity::Error);
        assert_eq!(edges[0].policy_name.as_deref(), Some("any-missing"));
    }

    #[test]
    fn policy_glob_matches_normalized_source_relative_candidate() {
        // `../docs/x.md` written from `designs/a.md` *means* `docs/x.md`
        // — the glob matches the normalized root-relative resolution
        // candidate, which raw-target prefix matching gets wrong.
        let root = tempfile::tempdir().expect("tempdir");
        let graph = graph_of(
            vec![node_at("a", "designs/a.md")],
            vec![dangling("a", "../docs/x.md", "references")],
        );
        let config = policy_config(vec![row(
            "ephemeral-docs",
            UnresolvedCause::Missing,
            Some("docs/**"),
            UnresolvedSeverity::Info,
        )]);

        let edges = find_unresolved_edges(&graph, &config, root.path());
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].cause, UnresolvedCause::Missing);
        assert_eq!(edges[0].severity, UnresolvedSeverity::Info);
        assert_eq!(edges[0].policy_name.as_deref(), Some("ephemeral-docs"));
    }

    #[test]
    fn policy_glob_applies_extension_ladder_and_skips_it_for_covers() {
        // An extension-less document reference expands through the
        // extension ladder, so `docs/guide` matches a `docs/guide.md`
        // glob; a `covers` target is path-only and must not.
        let root = tempfile::tempdir().expect("tempdir");
        let config = policy_config(vec![row(
            "guide-links",
            UnresolvedCause::Missing,
            Some("docs/guide.md"),
            UnresolvedSeverity::Info,
        )]);

        let reference = graph_of(
            vec![node("a")],
            vec![dangling("a", "docs/guide", "references")],
        );
        let edges = find_unresolved_edges(&reference, &config, root.path());
        assert_eq!(edges[0].policy_name.as_deref(), Some("guide-links"));
        assert_eq!(edges[0].severity, UnresolvedSeverity::Info);

        let covers = graph_of(vec![node("a")], vec![dangling("a", "docs/guide", "covers")]);
        let edges = find_unresolved_edges(&covers, &config, root.path());
        assert_eq!(
            edges[0].policy_name, None,
            "covers must not extension-append into the row's glob"
        );
        assert_eq!(edges[0].severity, UnresolvedSeverity::Warning);
    }

    #[test]
    fn fallthrough_is_warning_and_unattributed() {
        // Default policy has only the excluded_target row; a Missing
        // edge matches no row → warning, no policy_name, counted in
        // `summary.total` under `unresolved_edge`.
        let root = tempfile::tempdir().expect("tempdir");
        let graph = graph_of(
            vec![node("a")],
            vec![dangling("a", "docs/x.md", "references")],
        );
        let report = find_issues(&graph, &Config::default(), root.path(), None);

        assert_eq!(report.unresolved_edges.len(), 1);
        assert_eq!(
            report.unresolved_edges[0].severity,
            UnresolvedSeverity::Warning
        );
        assert_eq!(report.unresolved_edges[0].policy_name, None);
        assert_eq!(report.summary.by_category[categories::UNRESOLVED_EDGE], 1);
        assert_eq!(report.summary.total, 1);
    }

    #[test]
    fn info_edges_count_under_row_name_out_of_total() {
        let root = tempfile::tempdir().expect("tempdir");
        let graph = graph_of(
            vec![node("a")],
            vec![dangling("a", "specs/x.md", "references")],
        );
        let config = policy_config(vec![row(
            "ephemeral-specs",
            UnresolvedCause::Missing,
            Some("specs/**"),
            UnresolvedSeverity::Info,
        )]);
        let report = find_issues(&graph, &config, root.path(), None);

        assert_eq!(report.unresolved_edges.len(), 1, "edge stays visible");
        assert_eq!(report.summary.by_category["ephemeral-specs"], 1);
        assert!(
            !report
                .summary
                .by_category
                .contains_key(categories::UNRESOLVED_EDGE)
        );
        assert_eq!(report.summary.total, 0, "info edges stay out of total");
    }

    #[test]
    fn error_edges_count_once_via_violation() {
        // An error-classified edge is gated through its violation —
        // listed in BOTH `unresolved_edges` (detail record) and
        // `violations` (gate record), but only the violation
        // increments `total`.
        let root = tempfile::tempdir().expect("tempdir");
        let graph = graph_of(
            vec![node("a")],
            vec![dangling("a", "docs/x.md", "references")],
        );
        let config = policy_config(vec![row(
            "broken-docs-link",
            UnresolvedCause::Missing,
            Some("docs/**"),
            UnresolvedSeverity::Error,
        )]);
        let report = find_issues(&graph, &config, root.path(), None);

        assert_eq!(report.unresolved_edges.len(), 1);
        assert_eq!(
            report.unresolved_edges[0].severity,
            UnresolvedSeverity::Error
        );
        assert_eq!(
            report.unresolved_edges[0].policy_name.as_deref(),
            Some("broken-docs-link")
        );
        let gate: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.rule_id == "unresolved_reference/broken-docs-link")
            .collect();
        assert_eq!(gate.len(), 1);
        assert!(
            !report
                .summary
                .by_category
                .contains_key(categories::UNRESOLVED_EDGE),
            "the error edge must not also count as a warning edge"
        );
        assert_eq!(
            report.summary.by_category["violation_unresolved_reference/broken-docs-link"],
            1
        );
        assert_eq!(report.summary.total, 1, "counted once, via the violation");
    }

    #[test]
    fn parse_failed_target_classifies_target_unparsed() {
        // The target exists on disk AND is recorded in
        // `Graph::parse_failures` — it is an in-scope document that
        // dropped, never `ExcludedFromScope` (the default info row must
        // not file a genuinely broken reference out of the total).
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(root.path().join("docs")).unwrap();
        std::fs::write(root.path().join("docs/broken.md"), "---\nbad").unwrap();

        let mut map = IndexMap::new();
        let n = node("a");
        map.insert(n.id.clone(), n);
        let graph = Graph::new(
            map,
            vec![dangling("a", "docs/broken.md", "references")],
            vec![],
            vec![],
            vec![ParseFailure {
                path: "docs/broken.md".into(),
                message: "parse error".into(),
                content_hash: "abc".into(),
            }],
            GraphMeta::default(),
        );

        let report = find_issues(&graph, &Config::default(), root.path(), None);
        let edge = report
            .unresolved_edges
            .iter()
            .find(|e| e.raw_target == "docs/broken.md")
            .expect("edge reported");
        assert_eq!(edge.cause, UnresolvedCause::TargetUnparsed);
        // No default row matches it → counted fallthrough, like Missing.
        assert_eq!(edge.severity, UnresolvedSeverity::Warning);
        assert_eq!(edge.policy_name, None);
        assert_eq!(report.summary.by_category[categories::UNRESOLVED_EDGE], 1);
    }
}
