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
//! command runs and the reading arrives on [`RuleContext::history`], so
//! this rule measures the project's own history in one walk rather than
//! putting a question to git per document.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::git::{History, HistoryRecord, Repository};
use crate::model::ResolvedTarget;
use crate::warning::{Warning, WarningCode};

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
        ctx.history.measures()
    }

    fn skip_reason(&self, _ctx: &RuleContext<'_>) -> String {
        "no git repository for the project — drift cannot be measured".to_string()
    }

    fn subject_unit(&self) -> SubjectUnit {
        SubjectUnit::Nodes
    }

    /// Drift is git's reading, so the question is put to git: the
    /// finding is the diff's when the reviewing document's own record
    /// moved, or when the range the diff arrived in added a commit the
    /// reading counts — one dated after `reviewed`, on any path the
    /// document measures against, a covered code path outside the graph
    /// included. Asked of the graph diff instead, a covered path would
    /// have no record to be touched by, and a measured document edited
    /// without a commit would read as moving a count it did not move.
    fn touched_by(
        &self,
        ctx: &RuleContext<'_>,
        since: &crate::diff::Touched,
        violation: &Violation,
    ) -> bool {
        violation.node_id.as_deref().is_none_or(|id| {
            since.document(id) || {
                let Some(node) = ctx.graph.node(id) else {
                    return false;
                };
                let Some(reviewed) = node.reviewed else {
                    return false;
                };
                drift_targets(ctx.graph, ctx.config, ctx.files, node)
                    .into_iter()
                    .filter_map(DriftTarget::path)
                    .any(|path| {
                        ctx.history
                            .commits_added(since.since(), &path, reviewed)
                            .is_some_and(|added| added > 0)
                    })
            }
        })
    }

    fn check(&self, ctx: &RuleContext<'_>) -> RuleRun {
        let Some(threshold) = ctx.config.detection.git_drift_threshold else {
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
                // `None` is git having gone unread — skip the edge rather
                // than count it as zero drift.
                let Some(commits) = ctx.history.commits_since(&path, reviewed) else {
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

/// The subjects of `node`'s drift: one entry per outgoing edge in a
/// `detection.git_drift_relations` relation, in graph order. `check` and `query trust` read the
/// resolution here, so the two readings of drift can never measure
/// different files — the discipline [`DriftHistory`] already applies to
/// the repository, applied to the paths inside it.
///
/// `covers` typically points at code paths that live outside the doc
/// graph, so a target the graph has no node for still resolves. What an
/// unresolved edge is probed against is the path half of what its
/// resolution sought (`resolver::Sought::paths`): an id names no file and
/// a refused spelling (absolute, source-escaping) names nothing in root,
/// so both are unresolvable outright, and everything else probes the same
/// normalized candidate ladder the resolver walked — never the raw
/// authored string, so the probe can never stat outside the project root.
pub(crate) fn drift_targets(
    graph: &crate::model::Graph,
    config: &crate::config::Config,
    files: crate::builder::scanner::ProjectFiles<'_>,
    node: &crate::model::Node,
) -> Vec<DriftTarget> {
    let relations = &config.detection.git_drift_relations;
    graph
        .outgoing_edges(&node.id)
        .into_iter()
        .filter(|edge| relations.iter().any(|r| r == &edge.relation))
        .map(|edge| match &edge.target {
            ResolvedTarget::Resolved { id } => match graph.node(id) {
                Some(target) => DriftTarget::Resolved {
                    path: target.path.clone(),
                    label: id.clone(),
                },
                None => DriftTarget::Unresolvable { label: id.clone() },
            },
            ResolvedTarget::Unresolved { raw, .. } => {
                let sought = crate::builder::resolver::sought_names(
                    raw,
                    &edge.relation,
                    Some(node.path.as_path()),
                    &config.parser.extensions,
                );
                let candidate = crate::builder::resolver::first_candidate_on_disk(
                    sought.paths(),
                    files,
                    crate::model::edge::is_path_only_relation(&edge.relation),
                );
                match candidate {
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

/// What git says about the project, read on demand and remembered.
///
/// The measurement is per `(path, review date)` pair, but one revision
/// walk holds every pair's answer for a range, and putting the question
/// to git per pair spends a process on each: the cost of a pass would
/// then track the size of the corpus rather than of the repository it
/// reads. A command running more than one pass over one project — a
/// `--content` gate judges the working tree and the proposal it would
/// become — holds one of these across them, because a repository's
/// history is one reading for every pass of one command.
///
/// Across commands the reading at `HEAD` is kept in the output directory
/// (`history.json`), keyed by the commit it was taken at. A walk of a
/// commit is a function of that commit, so a kept reading whose commit
/// the current `HEAD` reaches is completed by walking only the range
/// between them, and the whole history is walked only when there is no
/// such reading — a command's cost is then the commits since the last one
/// rather than every commit the repository holds. Only a working-tree
/// `check` stores it ([`Self::refreshing`]); every other reader consults it
/// and writes nothing, so `check --content` stays the read-only gate it is.
///
/// Nothing is read until something asks. A project without
/// `detection.git_drift_threshold` measures no drift and never reaches
/// git at all; a pass whose rules do not measure it never asks; and
/// `check` and `query trust` both ask through here, so the two readings
/// of drift can never land on different repositories.
pub struct DriftHistory {
    /// The project root, when the project measures drift at all.
    measured: Option<PathBuf>,
    /// Where the reading at `HEAD` is kept, and whether this command
    /// stores it.
    kept: Option<Kept>,
    repository: OnceLock<Option<Repository>>,
    /// The reading at `HEAD`, taken once however many passes ask for it.
    head: OnceLock<Option<Arc<History>>>,
    /// One reading per revision range, kept because a pass asks the same
    /// range once per document and a command asks it once per pass.
    readings: RwLock<BTreeMap<String, Option<Arc<History>>>>,
    /// What keeping the reading ran into, for the command that stores it.
    warnings: Mutex<Vec<Warning>>,
}

/// The file a project's reading at `HEAD` is kept in, and whether this
/// command refreshes it.
struct Kept {
    path: PathBuf,
    refresh: bool,
}

/// `history.json` as it is written. The key is everything a walk of `head`
/// depends on besides the commit itself: the binary that walked it and the
/// project's prefix, which bounds the walk.
#[derive(Serialize, Deserialize)]
struct KeptReading {
    schema_version: u32,
    nodex: String,
    prefix: String,
    head: String,
    history: HistoryRecord,
}

/// On-disk shape version of `history.json`. Bump on any change to
/// [`KeptReading`] or [`HistoryRecord`]. A mismatch discards the file
/// without a word, as a foreign `cache.json` is discarded: it is the
/// expected invalidation after an upgrade, and the walk it forces answers
/// the same.
const KEPT_SCHEMA_VERSION: u32 = 1;

impl DriftHistory {
    /// The reading of a project that measures no drift: no threshold, so
    /// nothing to read and nothing that could read it.
    pub const fn unmeasured() -> Self {
        Self {
            measured: None,
            kept: None,
            repository: OnceLock::new(),
            head: OnceLock::new(),
            readings: RwLock::new(BTreeMap::new()),
            warnings: Mutex::new(Vec::new()),
        }
    }

    /// The reading a project's own config asks for. Cheap: the threshold
    /// is the gate, and everything past it happens on demand. It consults
    /// the kept reading and never stores one.
    pub fn of(config: &crate::config::Config, root: &Path) -> Self {
        Self::keeping(config, root, false)
    }

    /// [`Self::of`] for the command that owns the kept reading: it stores
    /// the reading it takes whenever the kept one did not already answer,
    /// and reports on [`Self::warnings`] what kept it from doing so.
    pub fn refreshing(config: &crate::config::Config, root: &Path) -> Self {
        Self::keeping(config, root, true)
    }

    fn keeping(config: &crate::config::Config, root: &Path, refresh: bool) -> Self {
        let measured = config
            .detection
            .git_drift_threshold
            .map(|_| root.to_path_buf());
        let kept = measured.as_ref().map(|root| Kept {
            path: root.join(&config.output.dir).join("history.json"),
            refresh,
        });
        Self {
            measured,
            kept,
            ..Self::unmeasured()
        }
    }

    /// Whether this project has a repository to measure drift against —
    /// the `git_drift` rule's applicability, and the question that first
    /// reaches git.
    pub fn measures(&self) -> bool {
        self.repository().is_some()
    }

    fn repository(&self) -> Option<&Repository> {
        self.repository
            .get_or_init(|| {
                let root = self.measured.as_ref()?;
                Repository::discover(root).ok().flatten()
            })
            .as_ref()
    }

    /// What a refreshing reading could not do with the kept file: read one
    /// that was there, or store the one it took. Always empty for a reading
    /// that only consults the file — it neither owns nor repairs it, and
    /// whatever it met is met again by the next refresh, which says so.
    pub fn warnings(&self) -> Vec<Warning> {
        self.warnings
            .lock()
            .expect("no reader panics while holding this")
            .clone()
    }

    /// Commits touching the project's `path` strictly *after* the
    /// `reviewed` date, or `None` when git could not be read. `None` is
    /// "unmeasurable", distinct from `Some(0)` "no drift": callers must
    /// not conflate absence of a signal with a zero signal — the check
    /// rule guards the environment through [`crate::rules::preflight`]
    /// and treats a residual `None` as a skipped edge; the trust query
    /// has no such guard and drops the whole drift component on `None`,
    /// the same way `backlinks` drops an absent signal rather than
    /// fabricating maximum trust from it.
    ///
    /// The boundary is the day after `reviewed`, not `reviewed` itself: a
    /// review records that the doc was current as of that day, so the
    /// commit that performed the review (and any same-day change the
    /// reviewer already saw) must not register as drift — otherwise a
    /// freshly-reviewed document would report drift on day zero.
    pub fn commits_since(&self, path: &Path, reviewed: NaiveDate) -> Option<u32> {
        Some(self.at_head()?.commits_since(path, reviewed))
    }

    /// The part of [`Self::commits_since`]'s count that arrived in
    /// `since..HEAD`, so a narrowed report can tell a drift the range
    /// moved from one that stood before it. The range travels with the
    /// diff it came from ([`crate::diff::Touched::since`]), so the
    /// reading and the narrowing can never be taken against different
    /// refs.
    pub fn commits_added(&self, since: &str, path: &Path, reviewed: NaiveDate) -> Option<u32> {
        Some(
            self.reading(&format!("{since}..HEAD"))?
                .commits_since(path, reviewed),
        )
    }

    /// The walk of the commit `HEAD` names, taken once. A failed walk is
    /// remembered as a failure, as [`Self::reading`] remembers one.
    fn at_head(&self) -> Option<Arc<History>> {
        self.head
            .get_or_init(|| self.walk_head().map(Arc::new))
            .clone()
    }

    fn walk_head(&self) -> Option<History> {
        let repository = self.repository()?;
        let head = repository.head().ok()??;
        // A history git can reshape in place has no reading its head
        // commit names, so there is nothing to consult or keep for it; an
        // unanswered question about it is read the same way.
        let kept = self
            .kept
            .as_ref()
            .filter(|_| matches!(repository.reshapes_history(), Ok(false)));
        let Some(kept) = kept else {
            return History::read(repository, &head).ok();
        };
        let prefix = repository.prefix().to_str();
        let reading = match prefix.and_then(|prefix| self.load(kept, prefix)) {
            Some((at, history)) if at == head => return Some(history),
            Some((at, history)) if matches!(repository.is_ancestor(&at, &head), Ok(true)) => {
                let since = History::read(repository, &format!("{at}..{head}")).ok()?;
                history.joined(since).ok()?
            }
            _ => History::read(repository, &head).ok()?,
        };
        if kept.refresh {
            self.store(kept, prefix, &head, &reading);
        }
        Some(reading)
    }

    /// The reading kept for this project, with the commit it was taken at —
    /// `None` when there is none, or it was taken by another binary or of
    /// another prefix, or it cannot be read as one.
    fn load(&self, kept: &Kept, prefix: &str) -> Option<(String, History)> {
        let raw = match std::fs::read_to_string(&kept.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => return self.unreadable(kept, e),
        };
        let value: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(value) => value,
            Err(e) => return self.unreadable(kept, e),
        };
        // Every shape this file has had is an object, so anything else is
        // damage rather than a version to read past.
        if !value.is_object() {
            return self.unreadable(kept, "not a JSON object");
        }
        if value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            != Some(u64::from(KEPT_SCHEMA_VERSION))
        {
            return None;
        }
        let reading: KeptReading = match serde_json::from_value(value) {
            Ok(reading) => reading,
            Err(e) => return self.unreadable(kept, e),
        };
        if reading.nodex != env!("CARGO_PKG_VERSION") || reading.prefix != prefix {
            return None;
        }
        match History::from_record(reading.history) {
            Some(history) => Some((reading.head, history)),
            None => self.unreadable(kept, "a record no walk could have produced"),
        }
    }

    fn unreadable<T>(&self, kept: &Kept, reason: impl std::fmt::Display) -> Option<T> {
        self.report(
            kept,
            format!(
                "kept history unreadable at {}: {reason}; walking the whole history",
                kept.path.display()
            ),
        );
        None
    }

    /// Keep `reading` as the reading at `head`. The write goes through the
    /// guarded primitive, so a crash leaves the previous file whole and a
    /// path escaping the project is refused rather than followed.
    fn store(&self, kept: &Kept, prefix: Option<&str>, head: &str, reading: &History) {
        let root = self
            .measured
            .as_ref()
            .expect("a kept reading belongs to a measured project");
        let stored = prefix
            .ok_or("the project's path inside the repository is not UTF-8")
            .and_then(|prefix| {
                let history = reading
                    .record()
                    .ok_or("a path in the history is not UTF-8")?;
                Ok(KeptReading {
                    schema_version: KEPT_SCHEMA_VERSION,
                    nodex: env!("CARGO_PKG_VERSION").to_owned(),
                    prefix: prefix.to_owned(),
                    head: head.to_owned(),
                    history,
                })
            })
            .map_err(str::to_owned)
            .and_then(|reading| {
                let json =
                    serde_json::to_string(&reading).expect("a kept reading is JSON-serialisable");
                crate::path_guard::write_atomic_in_root(root, &kept.path, &json)
                    .map_err(|e| e.to_string())
            });
        if let Err(reason) = stored {
            self.report(
                kept,
                format!(
                    "history not kept at {}: {reason}; the next command walks the whole history again",
                    kept.path.display()
                ),
            );
        }
    }

    fn report(&self, kept: &Kept, message: String) {
        if kept.refresh {
            self.warnings
                .lock()
                .expect("no reader panics while holding this")
                .push(Warning::new(WarningCode::Cache, message));
        }
    }

    /// The walk of `revisions`, taken once. A failed walk is remembered
    /// as a failure: retrying it per document would restore the cost the
    /// reading exists to remove, and a repository that cannot be walked
    /// does not become walkable mid-pass.
    fn reading(&self, revisions: &str) -> Option<Arc<History>> {
        if let Some(reading) = self
            .readings
            .read()
            .expect("no reader panics while holding this")
            .get(revisions)
        {
            return reading.clone();
        }
        let reading = History::read(self.repository()?, revisions)
            .ok()
            .map(Arc::new);
        self.readings
            .write()
            .expect("no reader panics while holding this")
            .insert(revisions.to_string(), reading.clone());
        reading
    }
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
            body_structure: Default::default(),
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
                history: &DriftHistory::of(&config, dir.path()),
                since: None,
                steps: None,
            })
            .violations;
        assert!(
            !violations
                .iter()
                .any(|v| matches!(v.details, ViolationDetails::GitDrift { .. })),
            "an absolute raw target must never be counted as drift: {violations:?}"
        );
    }

    /// The finding sits on the reviewing document and the reading is
    /// git's, so the range a diff arrived in answers for the drift when
    /// it added a commit the reading counts: on a measured document, on a
    /// covered code path outside the graph alike. A commit to a bystander
    /// document moves nothing the reading counts.
    #[test]
    fn a_range_that_added_a_counted_commit_answers_for_the_drift() {
        let dir = tempfile::TempDir::new().unwrap();
        let run = |args: &[&str]| -> String {
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
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        run(&["init"]);
        run(&["config", "commit.gpgsign", "false"]);
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        for (path, body) in [
            ("docs/reviewer.md", "r\n"),
            ("docs/measured.md", "m\n"),
            ("docs/bystander.md", "b\n"),
            ("src/covered.rs", "fn a() {}\n"),
        ] {
            std::fs::write(dir.path().join(path), body).unwrap();
        }
        run(&["add", "."]);
        run(&["commit", "-m", "base"]);
        let base = run(&["rev-parse", "HEAD"]);

        let node = |id: &str| Node {
            id: id.to_string(),
            path: PathBuf::from(format!("docs/{id}.md")),
            title: id.to_string(),
            kind: Kind::new("generic"),
            status: Status::new("active"),
            created: None,
            updated: None,
            reviewed: (id == "reviewer").then(|| crate::test_today() - chrono::Duration::days(365)),
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
            body_structure: Default::default(),
            content_hash: String::new(),
            parse_issues: vec![],
            inferred_fields: vec![],
        };
        let mut nodes = IndexMap::new();
        for id in ["reviewer", "measured", "bystander"] {
            nodes.insert(id.to_string(), node(id));
        }
        let graph = Graph::new(
            nodes,
            vec![
                Edge {
                    source: "reviewer".to_string(),
                    target: crate::model::ResolvedTarget::resolved("measured"),
                    relation: "implements".to_string(),
                    location: "frontmatter:implements".to_string(),
                },
                Edge {
                    source: "reviewer".to_string(),
                    target: crate::model::ResolvedTarget::unresolved(
                        "src/covered.rs",
                        UnresolvedCause::Missing,
                    ),
                    relation: "covers".to_string(),
                    location: "frontmatter:covers".to_string(),
                },
            ],
            vec![],
            vec![],
            vec![],
            GraphMeta::default(),
        );
        let mut config = Config::default();
        config.detection.git_drift_threshold = Some(1);
        let violation = Violation::new(
            "git_drift",
            Severity::Warning,
            Some("reviewer".to_string()),
            Some("docs/reviewer.md".to_string()),
            ViolationDetails::GitDrift {
                total_commits: Evidence(2),
                threshold: 1,
                reviewed: "2025-01-01".to_string(),
                hottest: None,
            },
        );
        let history = DriftHistory::of(&config, dir.path());
        let answers = |since: &str| {
            let touched = crate::diff::compute_diff(&graph, &graph).touched(since);
            GitDriftRule.touched_by(
                &RuleContext {
                    today: crate::test_today(),
                    graph: &graph,
                    config: &config,
                    files: crate::builder::scanner::ProjectFiles::working_tree(dir.path()),
                    history: &history,
                    since: None,
                    steps: None,
                },
                &touched,
                &violation,
            )
        };

        std::fs::write(dir.path().join("docs/bystander.md"), "b2\n").unwrap();
        run(&["commit", "-am", "bystander"]);
        assert!(
            !answers(&base),
            "a commit to a document the reading does not count moves nothing"
        );
        let after_bystander = run(&["rev-parse", "HEAD"]);

        std::fs::write(dir.path().join("docs/measured.md"), "m2\n").unwrap();
        run(&["commit", "-am", "measured"]);
        assert!(
            answers(&after_bystander),
            "the range added a commit on a measured document"
        );
        let after_measured = run(&["rev-parse", "HEAD"]);

        std::fs::write(dir.path().join("src/covered.rs"), "fn b() {}\n").unwrap();
        run(&["commit", "-am", "covered"]);
        assert!(
            answers(&after_measured),
            "the range added a commit on a covered code path outside the graph"
        );
        assert!(!answers("HEAD"), "an empty range added nothing");
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
            body_structure: Default::default(),
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
                history: &DriftHistory::of(&config, dir.path()),
                since: None,
                steps: None,
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
            body_structure: Default::default(),
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
            history: &DriftHistory::of(&config, dir.path()),
            since: None,
            steps: None,
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

    mod kept {
        use super::super::DriftHistory;
        use crate::config::Config;
        use crate::git::fixture::{commit_on, history_repo, run_git};
        use crate::git::{History, Repository};
        use crate::warning::WarningCode;
        use chrono::NaiveDate;
        use std::path::{Path, PathBuf};

        fn measured() -> Config {
            let mut config = Config::default();
            config.detection.git_drift_threshold = Some(1);
            config
        }

        fn kept_file(root: &Path) -> PathBuf {
            root.join(measured().output.dir).join("history.json")
        }

        fn day(d: u32) -> NaiveDate {
            NaiveDate::from_ymd_opt(2024, 3, d).unwrap()
        }

        fn head_of(root: &Path) -> String {
            Repository::discover(root)
                .unwrap()
                .unwrap()
                .head()
                .unwrap()
                .unwrap()
        }

        /// The kept file's JSON, edited by `edit` and written back.
        fn edit_kept(root: &Path, edit: impl FnOnce(&mut serde_json::Value)) {
            let path = kept_file(root);
            let mut kept: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            edit(&mut kept);
            std::fs::write(&path, serde_json::to_string(&kept).unwrap()).unwrap();
        }

        /// Record in the kept reading a commit no walk reports: its first
        /// commit changing `ghost.rs`. A reading that counts the ghost read
        /// the kept file; one that does not walked without it.
        fn plant_ghost(root: &Path) {
            edit_kept(root, |kept| {
                let history = &mut kept["history"];
                history["paths"]
                    .as_array_mut()
                    .unwrap()
                    .push("ghost.rs".into());
                history["commits"]
                    .as_array_mut()
                    .unwrap()
                    .push(serde_json::json!([0]));
            });
        }

        fn ghost(history: &DriftHistory) -> Option<u32> {
            history.commits_since(Path::new("ghost.rs"), day(1) - chrono::Duration::days(1))
        }

        /// Only the command that owns the kept reading stores it; a reading
        /// that consults it leaves the output directory as it found it.
        #[test]
        fn only_a_refreshing_reading_keeps_the_head_it_walked() {
            let dir = history_repo();
            let root = dir.path();
            commit_on(root, day(2), 9, &[("src/a.rs", "1\n")]);
            let consulting = DriftHistory::of(&measured(), root);
            assert_eq!(
                consulting.commits_since(Path::new("src/a.rs"), day(1)),
                Some(1)
            );
            assert!(
                !kept_file(root).exists(),
                "a consulting reading writes nothing"
            );

            let refreshing = DriftHistory::refreshing(&measured(), root);
            assert_eq!(
                refreshing.commits_since(Path::new("src/a.rs"), day(1)),
                Some(1)
            );
            let kept: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(kept_file(root)).unwrap()).unwrap();
            assert_eq!(kept["head"], head_of(root).as_str());
            assert!(refreshing.warnings().is_empty());
        }

        /// A kept reading whose commit `HEAD` reaches is consulted and
        /// completed by the commits since — the ghost it was planted with
        /// is counted — and everything else counts as a fresh walk does,
        /// including a side branch forked before the kept head and merged
        /// after it.
        #[test]
        fn a_kept_reading_the_head_reaches_is_completed_by_the_commits_since() {
            let dir = history_repo();
            let root = dir.path();
            commit_on(root, day(2), 9, &[("src/a.rs", "1\n")]);
            run_git(root, &["branch", "-M", "trunk"]);
            run_git(root, &["checkout", "-q", "-b", "side"]);
            commit_on(root, day(3), 9, &[("src/a.rs", "side\n")]);
            run_git(root, &["checkout", "-q", "trunk"]);
            commit_on(root, day(4), 9, &[("src/b.rs", "b\n")]);
            DriftHistory::refreshing(&measured(), root).commits_since(Path::new("src"), day(1));
            plant_ghost(root);
            run_git(
                root,
                &["merge", "-q", "--no-ff", "side", "-m", "merge side"],
            );
            commit_on(root, day(5), 9, &[("src/b.rs", "b2\n")]);

            let consulting = DriftHistory::of(&measured(), root);
            assert_eq!(
                ghost(&consulting),
                Some(1),
                "the kept reading was consulted"
            );
            let repository = Repository::discover(root).unwrap().unwrap();
            let walked = History::read(&repository, "HEAD").unwrap();
            for path in ["src", "src/a.rs", "src/b.rs"] {
                for reviewed in [day(1), day(2), day(3), day(4)] {
                    assert_eq!(
                        consulting.commits_since(Path::new(path), reviewed),
                        Some(walked.commits_since(Path::new(path), reviewed)),
                        "{path} since {reviewed}"
                    );
                }
            }
        }

        /// A kept reading that does not answer for this walk is passed over
        /// without a word: taken at a commit `HEAD` does not reach, by
        /// another binary, of another prefix, or in another shape.
        #[test]
        fn a_kept_reading_that_does_not_answer_for_this_walk_is_passed_over() {
            let dir = history_repo();
            let root = dir.path();
            commit_on(root, day(2), 9, &[("src/a.rs", "1\n")]);
            let first = head_of(root);
            run_git(root, &["checkout", "-q", "-b", "side"]);
            commit_on(root, day(3), 9, &[("src/a.rs", "side\n")]);
            DriftHistory::refreshing(&measured(), root).commits_since(Path::new("src"), day(1));
            plant_ghost(root);
            run_git(root, &["checkout", "-q", &first]);
            let unrelated = DriftHistory::refreshing(&measured(), root);
            assert_eq!(
                ghost(&unrelated),
                Some(0),
                "kept at a commit HEAD does not reach"
            );
            assert!(unrelated.warnings().is_empty());

            let foreign: [(&str, serde_json::Value); 3] = [
                ("nodex", "0.0.0".into()),
                ("prefix", "elsewhere".into()),
                ("schema_version", 0.into()),
            ];
            for (field, value) in foreign {
                plant_ghost(root);
                edit_kept(root, |kept| kept[field] = value);
                let passed_over = DriftHistory::refreshing(&measured(), root);
                assert_eq!(
                    ghost(&passed_over),
                    Some(0),
                    "a kept reading with a foreign {field}"
                );
                assert!(passed_over.warnings().is_empty());
            }
        }

        /// A history git can reshape in place has no reading its head names:
        /// the kept file is neither consulted nor written.
        #[test]
        fn a_history_git_can_reshape_is_neither_consulted_nor_kept() {
            let dir = history_repo();
            let root = dir.path();
            commit_on(root, day(2), 9, &[("src/a.rs", "1\n")]);
            let first = head_of(root);
            commit_on(root, day(3), 9, &[("src/a.rs", "2\n")]);
            DriftHistory::refreshing(&measured(), root).commits_since(Path::new("src"), day(1));
            plant_ghost(root);
            let planted = std::fs::read(kept_file(root)).unwrap();
            run_git(root, &["replace", &first, &head_of(root)]);
            let reshaped = DriftHistory::refreshing(&measured(), root);
            assert_eq!(ghost(&reshaped), Some(0));
            assert_eq!(
                std::fs::read(kept_file(root)).unwrap(),
                planted,
                "nothing kept"
            );
        }

        /// A kept file that cannot be read as one is walked past either way;
        /// only the refreshing reading, which owns the file, says so — and
        /// replaces it.
        #[test]
        fn an_unreadable_kept_reading_is_reported_by_the_reading_that_owns_it() {
            let dir = history_repo();
            let root = dir.path();
            commit_on(root, day(2), 9, &[("src/a.rs", "1\n")]);
            std::fs::create_dir_all(kept_file(root).parent().unwrap()).unwrap();
            std::fs::write(kept_file(root), "not json").unwrap();

            let consulting = DriftHistory::of(&measured(), root);
            assert_eq!(
                consulting.commits_since(Path::new("src/a.rs"), day(1)),
                Some(1)
            );
            assert!(consulting.warnings().is_empty());

            let refreshing = DriftHistory::refreshing(&measured(), root);
            assert_eq!(
                refreshing.commits_since(Path::new("src/a.rs"), day(1)),
                Some(1)
            );
            let warnings = refreshing.warnings();
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert_eq!(warnings[0].code, WarningCode::Cache);
            let kept: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(kept_file(root)).unwrap()).unwrap();
            assert_eq!(
                kept["head"],
                head_of(root).as_str(),
                "the owner replaced it"
            );
        }

        /// A kept file the guarded write refuses — here a symlink out of the
        /// project — is reported, and what it points at is left alone.
        #[cfg(unix)]
        #[test]
        fn a_reading_the_guard_refuses_to_keep_is_reported_and_not_written() {
            let dir = history_repo();
            let root = dir.path();
            commit_on(root, day(2), 9, &[("src/a.rs", "1\n")]);
            let outside = tempfile::TempDir::new().unwrap();
            let target = outside.path().join("history.json");
            std::fs::create_dir_all(kept_file(root).parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(&target, kept_file(root)).unwrap();

            let refreshing = DriftHistory::refreshing(&measured(), root);
            assert_eq!(
                refreshing.commits_since(Path::new("src/a.rs"), day(1)),
                Some(1)
            );
            let warnings = refreshing.warnings();
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert_eq!(warnings[0].code, WarningCode::Cache);
            assert!(!target.exists(), "the write did not follow the link");
        }
    }
}
