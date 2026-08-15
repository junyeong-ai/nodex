//! "The world I documented has moved on" detector.
//!
//! For each non-terminal document with a `reviewed` date, count the
//! git commits that landed on the documents (and code paths declared
//! via `covers`) it references since that review. When the total
//! crosses `detection.git_drift_threshold`, the review is treated as
//! stale relative to the artefacts it covers — the canonical
//! doc-gardening signal.
//!
//! Disabled when `git_drift_threshold` is `None`. The runtime
//! environment is verified by [`crate::rules::preflight`] before any
//! command runs and the resolved binding arrives on
//! [`RuleContext::repository`], so this rule measures the project's own
//! history without rediscovering a repository per document.

use std::path::Path;

use chrono::NaiveDate;

use crate::git::Repository;
use crate::model::ResolvedTarget;

use super::{
    Rule, RuleContext, RuleRun, Severity, SubjectUnit, Violation, ViolationDetails,
    detail::Evidence,
};

pub struct GitDriftRule;

impl Rule for GitDriftRule {
    fn id(&self) -> &str {
        "git_drift"
    }

    fn severity(&self) -> Severity {
        Severity::Warning
    }

    fn description(&self) -> &str {
        "Active docs are flagged when outgoing relation targets have accumulated \
         more than `detection.git_drift_threshold` git commits since `reviewed`"
    }

    fn params(&self, config: &crate::config::Config) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        m.insert(
            "threshold".into(),
            serde_json::json!(config.detection.git_drift_threshold),
        );
        m.insert(
            "relations".into(),
            serde_json::json!(config.detection.git_drift_relations),
        );
        m
    }

    /// The measurement needs the project's repository. `preflight`
    /// refuses a run whose threshold is set without one, so this only
    /// declines for a library caller that skipped it — and then it
    /// declines *visibly*, in `skipped_rules`, rather than reporting a
    /// corpus with no drift.
    fn is_applicable(&self, ctx: &RuleContext<'_>) -> bool {
        ctx.repository.is_some()
    }

    fn skip_reason(&self, _ctx: &RuleContext<'_>) -> String {
        "no git repository for the project — drift cannot be measured".to_string()
    }

    fn subject_unit(&self) -> SubjectUnit {
        SubjectUnit::Nodes
    }

    /// Drift is a reading of the documents a node points at over
    /// `detection.git_drift_relations`, so a diff that moved one of them
    /// moved the reading: the finding is the diff's when the reviewing
    /// document's own record moved or any document it measures against
    /// did. A `covers` path outside the graph is not a record a graph
    /// diff carries, so commits to it alone leave the finding to the
    /// whole-project check.
    fn touched_by(
        &self,
        ctx: &RuleContext<'_>,
        since: &crate::diff::Touched,
        violation: &Violation,
    ) -> bool {
        violation.node_id.as_deref().is_none_or(|id| {
            since.document(id)
                || drift_edges(ctx.graph, ctx.config, id)
                    .filter_map(|edge| edge.target.id())
                    .any(|target| since.document(target))
        })
    }

    fn check(&self, ctx: &RuleContext<'_>) -> RuleRun {
        let Some(threshold) = ctx.config.detection.git_drift_threshold else {
            return RuleRun::clean(0);
        };
        let Some(repository) = ctx.repository.as_ref() else {
            return RuleRun::clean(0);
        };
        let mut violations = Vec::new();
        let mut subjects = 0;
        let mut unjudged = 0;

        for node in ctx.graph.nodes().values() {
            if ctx.config.is_terminal(node.status.as_str()) {
                continue;
            }
            let Some(reviewed) = node.reviewed else {
                continue;
            };
            let mut total_commits: u32 = 0;
            let mut hottest: Option<(String, u32)> = None;
            // What the node offered to measure, and what could be. A node
            // offering nothing has no drift, and zero is its answer; one whose
            // every offer went unmeasured has no answer at all, and reporting
            // zero there would be the absence this rule refuses to read as
            // "no drift" — refused per edge just below, and refused for the
            // node here.
            let mut offered = 0usize;
            let mut measured = 0usize;
            // Named, not just counted: a document whose every offer went
            // unmeasured is one this rule does not gate, and the targets are
            // what a repair edits.
            let mut unmeasured: Vec<String> = Vec::new();

            for target in drift_targets(ctx.graph, ctx.config, ctx.files, node) {
                offered += 1;
                let (path, label) = match target {
                    DriftTarget::Resolved { path, label } => (path, label),
                    DriftTarget::Unresolvable { label } => {
                        unmeasured.push(label);
                        continue;
                    }
                };
                // The environment is already verified, so a residual
                // `None` is a per-path anomaly — skip that edge rather
                // than count it as zero drift.
                let Some(commits) = commits_since(repository, &path, reviewed) else {
                    unmeasured.push(label);
                    continue;
                };
                total_commits = total_commits.saturating_add(commits);
                measured += 1;
                if hottest.as_ref().is_none_or(|(_, c)| commits > *c) {
                    hottest = Some((label, commits));
                }
            }

            if offered > 0 && measured == 0 {
                unjudged += 1;
                unmeasured.sort();
                unmeasured.dedup();
                violations.push(Violation::new(
                    self.id(),
                    self.severity(),
                    Some(node.id.clone()),
                    Some(crate::path_guard::forward_string(&node.path)),
                    ViolationDetails::GitDriftUnmeasurable {
                        targets: unmeasured,
                        reviewed: reviewed.to_string(),
                    },
                ));
                continue;
            }
            subjects += 1;

            if total_commits > threshold {
                violations.push(Violation::new(
                    self.id(),
                    self.severity(),
                    Some(node.id.clone()),
                    Some(crate::path_guard::forward_string(&node.path)),
                    ViolationDetails::GitDrift {
                        total_commits: Evidence(total_commits),
                        threshold,
                        reviewed: reviewed.to_string(),
                        hottest: hottest
                            .map(|(id, commits)| Evidence(super::DriftHotspot { id, commits })),
                    },
                ));
            }
        }

        RuleRun::new(subjects, violations).unjudged(unjudged)
    }
}

/// One outgoing edge in a `detection.git_drift_relations` relation, and
/// what the project holds behind it. An unresolvable target is named
/// rather than dropped: a node whose every offer went unmeasured is one
/// the rule does not gate, and the target is what a repair repoints.
pub(crate) enum DriftTarget {
    Resolved {
        path: std::path::PathBuf,
        label: String,
    },
    Unresolvable {
        label: String,
    },
}

impl DriftTarget {
    /// The path when the project holds one — for a caller that measures
    /// drift and has nowhere to report a target it could not reach.
    pub(crate) fn path(self) -> Option<std::path::PathBuf> {
        match self {
            DriftTarget::Resolved { path, .. } => Some(path),
            DriftTarget::Unresolvable { .. } => None,
        }
    }
}

/// The edges drift is measured over: the node's outgoing edges in a
/// `detection.git_drift_relations` relation. One filter, read by the
/// resolution below and by the narrowing question a diff puts to the
/// rule.
fn drift_edges<'g>(
    graph: &'g crate::model::Graph,
    config: &'g crate::config::Config,
    id: &str,
) -> impl Iterator<Item = &'g crate::model::Edge> {
    let relations = &config.detection.git_drift_relations;
    graph
        .outgoing_edges(id)
        .into_iter()
        .filter(move |edge| relations.iter().any(|r| r == &edge.relation))
}

/// The subjects of `node`'s drift: one entry per edge `drift_edges`
/// selects, in graph order. `check` and `query trust` read the
/// resolution here, so the two readings of drift can never measure
/// different files — the discipline [`drift_binding`] already applies
/// to the repository, applied to the paths inside it.
///
/// `covers` typically points at code paths that live outside the doc
/// graph, so a target the graph has no node for still resolves. A
/// refused cause (absolute, source-escaping) carries no in-root
/// candidates and is unresolvable outright; the rest probe the same
/// normalized candidate ladder the resolver uses — never the raw
/// authored string, so the probe can never stat outside the project
/// root.
pub(crate) fn drift_targets(
    graph: &crate::model::Graph,
    config: &crate::config::Config,
    files: crate::builder::scanner::ProjectFiles<'_>,
    node: &crate::model::Node,
) -> Vec<DriftTarget> {
    drift_edges(graph, config, &node.id)
        .map(|edge| match &edge.target {
            ResolvedTarget::Resolved { id } => match graph.node(id) {
                Some(target) => DriftTarget::Resolved {
                    path: target.path.clone(),
                    label: id.clone(),
                },
                None => DriftTarget::Unresolvable { label: id.clone() },
            },
            ResolvedTarget::Unresolved { raw, cause } => {
                let candidate = cause.has_path_candidates().then(|| {
                    let candidates = crate::builder::resolver::normalized_resolution_candidates(
                        raw,
                        Some(node.path.as_path()),
                        &config.parser.extensions,
                        crate::model::edge::is_document_ref_relation(&edge.relation),
                    );
                    crate::builder::resolver::first_candidate_on_disk(
                        &candidates,
                        files,
                        crate::model::edge::is_path_only_relation(&edge.relation),
                    )
                });
                match candidate.flatten() {
                    Some(path) => DriftTarget::Resolved {
                        path,
                        label: raw.clone(),
                    },
                    None => DriftTarget::Unresolvable { label: raw.clone() },
                }
            }
        })
        .collect()
}

/// The binding the drift measurement needs, or `None` when the project
/// does not measure drift — the threshold is the gate, so a project
/// without `git_drift_threshold` never pays for a probe — or has no
/// repository to measure. `check` and `query trust` both resolve through
/// here, so the two readings of drift can never land on different
/// repositories.
pub(crate) fn drift_binding(config: &crate::config::Config, root: &Path) -> Option<Repository> {
    config.detection.git_drift_threshold?;
    Repository::discover(root).ok().flatten()
}

/// Commit count touching the project's `path` strictly *after* the
/// `reviewed` date, or `None` when git could not measure it. `None` is
/// "unmeasurable", distinct from `Some(0)` "no drift": callers must not
/// conflate absence of a signal with a zero signal — the check rule
/// guards the environment through [`crate::rules::preflight`] and treats
/// a residual `None` as a skipped edge; the trust query has no such
/// guard and drops the whole drift component on `None`, the same way
/// `backlinks` drops an absent signal rather than fabricating maximum
/// trust from it.
///
/// `path` is project-relative and reaches git as
/// [`Repository::tracked_path`] writes it, so a project in a
/// subdirectory of a larger repository counts commits on its own file
/// rather than on the repository root's same-named one.
///
/// The boundary is the day after `reviewed`, not `reviewed` itself: a
/// review records that the doc was current as of that day, so the commit
/// that performed the review (and any same-day change the reviewer
/// already saw) must not register as drift — otherwise a freshly-reviewed
/// document would report drift on day zero.
pub(crate) fn commits_since(
    repository: &Repository,
    path: &Path,
    reviewed: NaiveDate,
) -> Option<u32> {
    let Some(after) = reviewed.succ_opt() else {
        return Some(0); // reviewed == NaiveDate::MAX: no day after it
    };
    // `rev-list --count` reports the number itself. Counting the lines of
    // a `log` instead would fold in whatever a user's git configuration
    // adds to each entry (`log.showSignature` prepends verification
    // lines), turning a measurement into a config-dependent guess.
    let output = repository
        .command()
        .args(["rev-list", "--count", "--since"])
        .arg(after.to_string())
        .arg("HEAD")
        .arg("--")
        .arg(repository.tracked_path(path))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::model::{Edge, Graph, GraphMeta, Kind, Node, Status, UnresolvedCause};
    use crate::rules::{Rule, RuleContext};
    use indexmap::IndexMap;
    use std::path::PathBuf;

    #[test]
    fn rule_skips_absolute_raw_target_without_probing_disk() {
        // The check rule shares the trust query's probe discipline
        // (symmetric guards): an absolute authored target carries no
        // in-root resolution candidates, so the edge is skipped — its
        // commits are never counted, even when the absolute path names
        // a real, heavily-committed file.
        let dir = tempfile::TempDir::new().unwrap();
        let run = |args: &[&str]| {
            let out = crate::git::command(dir.path())
                .expect("git on PATH")
                .args(args)
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .expect("git ran");
            assert!(out.status.success(), "git {args:?} failed");
        };
        run(&["init"]);
        run(&["config", "commit.gpgsign", "false"]);
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/auth.rs"), "fn a() {}\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "one"]);
        std::fs::write(dir.path().join("src/auth.rs"), "fn b() {}\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "two"]);

        let mut config = Config::default();
        config.detection.git_drift_threshold = Some(1);
        let reviewed = chrono::Local::now().date_naive() - chrono::Duration::days(10);
        let node = Node {
            id: "doc-x".to_string(),
            path: PathBuf::from("docs/x.md"),
            title: "X".to_string(),
            kind: Kind::new("generic"),
            status: Status::new("active"),
            created: None,
            updated: None,
            reviewed: Some(reviewed),
            owner: None,
            supersedes: vec![],
            superseded_by: None,
            implements: vec![],
            related: vec![],
            tags: vec![],
            covers: vec![],
            orphan_ok: false,
            attrs: Default::default(),
            body_hash: String::new(),
            body_lines_hash: Vec::new(),
            content_hash: String::new(),
            parse_issues: vec![],
            inferred_fields: vec![],
        };
        let mut nodes = IndexMap::new();
        nodes.insert(node.id.clone(), node);
        let graph = Graph::new(
            nodes,
            vec![Edge {
                source: "doc-x".to_string(),
                target: crate::model::ResolvedTarget::unresolved(
                    dir.path().join("src/auth.rs").to_string_lossy(),
                    UnresolvedCause::Absolute,
                ),
                relation: "covers".to_string(),
                location: "frontmatter:covers".to_string(),
            }],
            vec![],
            vec![],
            vec![],
            GraphMeta::default(),
        );

        let violations = GitDriftRule
            .check(&RuleContext {
                today: crate::test_today(),
                graph: &graph,
                config: &config,
                files: crate::builder::scanner::ProjectFiles::working_tree(dir.path()),
                repository: drift_binding(&config, dir.path()),
                since: None,
            })
            .violations;
        assert!(
            !violations
                .iter()
                .any(|v| matches!(v.details, ViolationDetails::GitDrift { .. })),
            "an absolute raw target must never be counted as drift: {violations:?}"
        );
    }

    /// The finding sits on the reviewing document and the reading comes
    /// from the documents it points at, so a diff that touched a measured
    /// document answers for it; one that touched an unrelated document
    /// does not. No git involved: the question is about records.
    #[test]
    fn a_diff_that_moved_a_measured_document_answers_for_the_drift() {
        let node = |id: &str, title: &str| Node {
            id: id.to_string(),
            path: PathBuf::from(format!("docs/{id}.md")),
            title: title.to_string(),
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
            orphan_ok: false,
            attrs: Default::default(),
            body_hash: String::new(),
            body_lines_hash: Vec::new(),
            content_hash: String::new(),
            parse_issues: vec![],
            inferred_fields: vec![],
        };
        let graph_of = |nodes: Vec<Node>| {
            let mut map = IndexMap::new();
            for n in nodes {
                map.insert(n.id.clone(), n);
            }
            Graph::new(
                map,
                vec![Edge {
                    source: "reviewer".to_string(),
                    target: crate::model::ResolvedTarget::resolved("measured"),
                    relation: "implements".to_string(),
                    location: "frontmatter:implements".to_string(),
                }],
                vec![],
                vec![],
                vec![],
                GraphMeta::default(),
            )
        };
        let before = graph_of(vec![
            node("reviewer", "R"),
            node("measured", "M"),
            node("bystander", "B"),
        ]);
        let config = Config::default();
        let violation = Violation::new(
            "git_drift",
            Severity::Warning,
            Some("reviewer".to_string()),
            Some("docs/reviewer.md".to_string()),
            ViolationDetails::GitDrift {
                total_commits: Evidence(3),
                threshold: 2,
                reviewed: "2026-01-01".to_string(),
                hottest: None,
            },
        );
        let answers = |after: &Graph| {
            let touched = crate::diff::compute_diff(&before, after).touched();
            GitDriftRule.touched_by(
                &RuleContext {
                    today: crate::test_today(),
                    graph: after,
                    config: &config,
                    files: crate::builder::scanner::ProjectFiles::working_tree(Path::new(".")),
                    repository: None,
                    since: None,
                },
                &touched,
                &violation,
            )
        };

        let measured_moved = graph_of(vec![
            node("reviewer", "R"),
            node("measured", "M, revised"),
            node("bystander", "B"),
        ]);
        assert!(
            answers(&measured_moved),
            "the diff moved a document the reading counts"
        );
        let bystander_moved = graph_of(vec![
            node("reviewer", "R"),
            node("measured", "M"),
            node("bystander", "B, revised"),
        ]);
        assert!(
            !answers(&bystander_moved),
            "the diff moved nothing the reading counts"
        );
    }

    #[test]
    fn covers_directory_target_is_measured() {
        // `covers` names out-of-graph code, and git measures a
        // directory's history as readily as a file's — a covered
        // directory must count its commits, not be silently skipped
        // by a file-only disk probe.
        let dir = tempfile::TempDir::new().unwrap();
        let run = |args: &[&str]| {
            let out = crate::git::command(dir.path())
                .expect("git on PATH")
                .args(args)
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .expect("git ran");
            assert!(out.status.success(), "git {args:?} failed");
        };
        run(&["init"]);
        run(&["config", "commit.gpgsign", "false"]);
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        for i in 0..3 {
            std::fs::write(dir.path().join("src/auth.rs"), format!("// {i}\n")).unwrap();
            run(&["add", "-A"]);
            run(&["commit", "-m", &format!("churn {i}")]);
        }

        let mut config = Config::default();
        config.detection.git_drift_threshold = Some(1);
        let reviewed = chrono::Local::now().date_naive() - chrono::Duration::days(10);
        let node = Node {
            id: "doc-x".to_string(),
            path: PathBuf::from("docs/x.md"),
            title: "X".to_string(),
            kind: Kind::new("generic"),
            status: Status::new("active"),
            created: None,
            updated: None,
            reviewed: Some(reviewed),
            owner: None,
            supersedes: vec![],
            superseded_by: None,
            implements: vec![],
            related: vec![],
            tags: vec![],
            covers: vec![],
            orphan_ok: false,
            attrs: Default::default(),
            body_hash: String::new(),
            body_lines_hash: Vec::new(),
            content_hash: String::new(),
            parse_issues: vec![],
            inferred_fields: vec![],
        };
        let mut nodes = IndexMap::new();
        nodes.insert(node.id.clone(), node);
        let graph = Graph::new(
            nodes,
            vec![Edge {
                source: "doc-x".to_string(),
                target: crate::model::ResolvedTarget::unresolved("src", UnresolvedCause::Missing),
                relation: "covers".to_string(),
                location: "frontmatter:covers".to_string(),
            }],
            vec![],
            vec![],
            vec![],
            GraphMeta::default(),
        );

        let violations = GitDriftRule
            .check(&RuleContext {
                today: crate::test_today(),
                graph: &graph,
                config: &config,
                files: crate::builder::scanner::ProjectFiles::working_tree(dir.path()),
                repository: drift_binding(&config, dir.path()),
                since: None,
            })
            .violations;
        assert_eq!(
            violations.len(),
            1,
            "a covered directory's commits must be measured: {violations:?}"
        );
        assert!(
            violations[0].message.contains("3 commits"),
            "all three commits under src/ count: {}",
            violations[0].message
        );
    }
    #[test]
    fn a_node_whose_every_drift_edge_went_unmeasured_is_named_and_not_judged() {
        // Skipping an unmeasurable edge keeps absence from reading as no
        // drift — but a node whose edges were *all* skipped ends the loop at
        // zero commits, which is that same absence one level up. It is
        // reported as unjudged rather than as a record this rule stood over
        // and found clean, and named as a finding besides: the reach says how
        // many documents this rule does not gate, and only the finding says
        // which, and which target to repoint. A node offering no drift edge
        // at all is different: there is nothing to measure, so zero is its
        // answer and it is a subject like any other.
        let dir = tempfile::TempDir::new().unwrap();
        let out = crate::git::command(dir.path())
            .expect("git on PATH")
            .args(["init"])
            .output()
            .expect("git ran");
        assert!(out.status.success(), "git init failed");

        let mut config = Config::default();
        config.detection.git_drift_threshold = Some(1);
        let reviewed = chrono::Local::now().date_naive() - chrono::Duration::days(10);
        let node = |id: &str| Node {
            id: id.to_string(),
            path: PathBuf::from(format!("docs/{id}.md")),
            title: id.to_string(),
            kind: Kind::new("generic"),
            status: Status::new("active"),
            created: None,
            updated: None,
            reviewed: Some(reviewed),
            owner: None,
            supersedes: vec![],
            superseded_by: None,
            implements: vec![],
            related: vec![],
            tags: vec![],
            covers: vec![],
            orphan_ok: false,
            attrs: Default::default(),
            body_hash: String::new(),
            body_lines_hash: Vec::new(),
            content_hash: String::new(),
            parse_issues: vec![],
            inferred_fields: vec![],
        };
        let mut nodes = IndexMap::new();
        // Two of them offer nothing, so the two counters differ: read the
        // same, a fixture holding one of each cannot tell which is which.
        for id in [
            "doc-offers",
            "doc-offers-nothing",
            "doc-offers-nothing-either",
        ] {
            nodes.insert(id.to_string(), node(id));
        }
        let graph = Graph::new(
            nodes,
            // The only drift edge names a path that is not on disk, so
            // nothing about `doc-offers` can be measured.
            // Named out of order, and one of them twice under two relations
            // that both measure drift: the rustdoc calls `targets` sorted, and
            // a node offering one target reads the same however it is built.
            ["src/z.rs", "src/a.rs", "src/z.rs"]
                .iter()
                .zip(["covers", "covers", "references"])
                .map(|(target, relation)| Edge {
                    source: "doc-offers".to_string(),
                    target: crate::model::ResolvedTarget::unresolved(
                        *target,
                        UnresolvedCause::Missing,
                    ),
                    relation: relation.to_string(),
                    location: format!("frontmatter:{relation}"),
                })
                .collect(),
            vec![],
            vec![],
            vec![],
            GraphMeta::default(),
        );

        let run = GitDriftRule.check(&RuleContext {
            today: crate::test_today(),
            graph: &graph,
            config: &config,
            files: crate::builder::scanner::ProjectFiles::working_tree(dir.path()),
            repository: drift_binding(&config, dir.path()),
            since: None,
        });
        assert_eq!(run.subjects, 2, "the nodes with nothing to measure");
        assert_eq!(run.unjudged, 1, "the node whose measurements all failed");
        let named: Vec<_> = run
            .violations
            .iter()
            .map(|v| (v.node_id.as_deref(), &v.details))
            .collect();
        assert_eq!(
            named,
            vec![(
                Some("doc-offers"),
                &ViolationDetails::GitDriftUnmeasurable {
                    targets: vec!["src/a.rs".to_string(), "src/z.rs".to_string()],
                    reviewed: reviewed.to_string(),
                }
            )],
            "the unjudged node names itself and the target to repoint"
        );
    }
}
