//! Snapshot introspection: every read of `graph.json` and every
//! freshness judgement about it lives here.
//!
//! [`load_graph`] is the single snapshot-read seam — a missing file is
//! the typed [`Error::MissingGraph`] (`GRAPH_MISSING`), and every
//! successful read carries an exact membership+config divergence
//! warning when the snapshot no longer matches the working tree (a
//! warning, never a gate; a probe failure degrades to a warning).
//! [`compute_status`] is the full content probe behind `nodex status`:
//! it classifies the snapshot into one of five machine-coded states and
//! par-hashes the corpus against the per-node `content_hash` the build
//! recorded.
//!
//! Coverage is nodes ∪ [`Graph::parse_failures`]: a path the build saw
//! but could not parse is *covered* by the snapshot — it never counts
//! as membership divergence, because a rebuild alone cannot clear it.
//! `check`'s `parse_failure` rule owns that signal; `nodex status`
//! surfaces the same paths distinctly as [`StatusReport::unbuildable_paths`].

use rayon::prelude::*;
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::model::Graph;
use crate::model::graph::SCHEMA_VERSION;

/// Machine-coded state of the project's graph snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GraphState {
    /// No `graph.json` exists — the project has never been built.
    Absent,
    /// The file exists but cannot be read as a graph (invalid JSON or
    /// invalid shape); [`StatusReport::unreadable_reason`] carries the cause.
    Unreadable,
    /// The recorded `schema_version` differs from this binary's
    /// [`SCHEMA_VERSION`]; regenerate with `nodex build --full`.
    SchemaMismatch,
    /// The snapshot parsed but diverges from the working tree (content,
    /// membership, or graph-shaping config).
    Outdated,
    /// The snapshot faithfully reflects the working tree under the
    /// current config.
    Current,
}

/// `nodex status` payload: the snapshot's state plus everything needed
/// to act on it without a second probe.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StatusReport {
    pub state: GraphState,
    /// Root-relative snapshot location (`<output.dir>/graph.json`).
    pub graph_path: String,
    /// The schema version this binary reads and writes.
    pub supported_schema_version: u32,
    /// The schema version the snapshot records — absent when no
    /// readable version probe exists (`absent` / unparseable file).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_schema_version: Option<u32>,
    /// The nodex version that produced the snapshot. A binary upgrade
    /// flags the snapshot `outdated` (the config hash carries a version
    /// salt, mirroring the cache policy); this field names the cause.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_nodex_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unreadable_reason: Option<String>,
    /// In-scope paths the snapshot recorded as parse failures. They are
    /// covered by the snapshot — never membership divergence, so they
    /// cannot hold `state` at `outdated` — and the remedy is fixing the
    /// document, not a rebuild: `check`'s `parse_failure` rule reds the
    /// same paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unbuildable_paths: Vec<String>,
    /// Present exactly when `state` is `outdated`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub divergence: Option<SnapshotDivergence>,
}

impl StatusReport {
    fn new(state: GraphState, graph_path: String) -> Self {
        Self {
            state,
            graph_path,
            supported_schema_version: SCHEMA_VERSION,
            snapshot_schema_version: None,
            snapshot_nodex_version: None,
            unreadable_reason: None,
            unbuildable_paths: Vec::new(),
            divergence: None,
        }
    }
}

/// Exact delta between a snapshot and the working tree.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SnapshotDivergence {
    /// The graph-shaping config surface (`builder::graph_config_hash`)
    /// no longer matches the recorded `meta.config_hash`. Also true
    /// after a binary upgrade (the hash carries the version salt).
    pub config_changed: bool,
    /// In-scope paths the snapshot does not cover.
    pub added_paths: Vec<String>,
    /// Covered paths no longer in scope on disk.
    pub removed_paths: Vec<String>,
    /// Covered paths whose current bytes hash differently from the
    /// recorded `content_hash`. `None` = not probed (the membership
    /// probe never reads document content) — unmeasured, never "fresh".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_paths: Option<Vec<String>>,
}

impl SnapshotDivergence {
    /// True when any probed dimension diverges.
    pub fn is_divergent(&self) -> bool {
        self.config_changed
            || !self.added_paths.is_empty()
            || !self.removed_paths.is_empty()
            || self.changed_paths.as_ref().is_some_and(|c| !c.is_empty())
    }
}

/// How much of the working tree a divergence probe measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DivergenceProbe {
    /// Config-hash comparison plus one scope walk — no document content
    /// is read or hashed (`conditional_exclude` parent-glob matches are
    /// the lone frontmatter reads the scope authority itself performs).
    /// `changed_paths` stays `None`.
    Membership,
    /// Membership plus a parallel content hash of every covered
    /// in-scope file against the recorded `content_hash`.
    Content,
}

/// Diff a snapshot against the working tree. Membership is computed
/// from the single scope authority (`scanner::scan_scope`, honouring
/// `conditional_exclude` exactly as a build would) against the
/// snapshot's coverage — nodes ∪ recorded parse failures. Under
/// [`DivergenceProbe::Content`], an unreadable covered file counts as
/// changed: the snapshot cannot be confirmed faithful for it. The same
/// holds from the other side: a hard-I/O parse failure records the
/// empty-string sentinel hash, which no byte digest equals, so a file
/// the *build* could not read also never confirms `current` — even if
/// it is readable (or empty) at probe time.
pub fn compute_divergence(
    graph: &Graph,
    config: &Config,
    root: &Path,
    probe: DivergenceProbe,
) -> Result<SnapshotDivergence> {
    let scan = crate::builder::scanner::scan_scope(root, config)?;
    let scanned: BTreeSet<String> = scan
        .paths
        .iter()
        .map(|p| crate::path_guard::forward_string(p))
        .collect();

    // Coverage = nodes ∪ recorded parse failures, each carrying the
    // content digest the build saw. A recorded failure is covered: the
    // snapshot honestly states "seen, unbuildable", and only a byte
    // change (or its removal) makes the snapshot stale for that path.
    let mut covered: BTreeMap<String, &str> = BTreeMap::new();
    for node in graph.nodes().values() {
        covered.insert(
            crate::path_guard::forward_string(&node.path),
            node.content_hash.as_str(),
        );
    }
    for failure in graph.parse_failures() {
        covered.insert(failure.path.clone(), failure.content_hash.as_str());
    }

    let added_paths: Vec<String> = scanned
        .iter()
        .filter(|p| !covered.contains_key(*p))
        .cloned()
        .collect();
    let removed_paths: Vec<String> = covered
        .keys()
        .filter(|p| !scanned.contains(*p))
        .cloned()
        .collect();
    let config_changed = graph.meta().config_hash != crate::builder::graph_config_hash(config);

    let changed_paths = match probe {
        DivergenceProbe::Membership => None,
        DivergenceProbe::Content => {
            let intersecting: Vec<(&String, &&str)> = covered
                .iter()
                .filter(|(path, _)| scanned.contains(*path))
                .collect();
            let mut changed: Vec<String> = intersecting
                .par_iter()
                .filter_map(|(path, recorded)| {
                    let abs = root.join(Path::new(path));
                    // Raw bytes, matching what the build digested — so a
                    // recorded non-UTF-8 parse failure with unchanged
                    // bytes confirms faithful instead of reading stale.
                    match std::fs::read(&abs) {
                        Ok(bytes) => {
                            (crate::hash::sha256_hex(&bytes) != **recorded).then(|| (*path).clone())
                        }
                        // Unreadable now ⇒ the snapshot cannot be
                        // confirmed faithful for this path.
                        Err(_) => Some((*path).clone()),
                    }
                })
                .collect();
            changed.sort();
            Some(changed)
        }
    };

    Ok(SnapshotDivergence {
        config_changed,
        added_paths,
        removed_paths,
        changed_paths,
    })
}

/// Probe `<output.dir>/graph.json` and classify it into one of the five
/// [`GraphState`]s, running the full content probe on a readable
/// snapshot. A probe, not a gate: every reachable file state is a
/// successful report (the `query issues` precedent) — only a broken
/// `nodex.toml` or a scope-walk failure is an `Err`.
pub fn compute_status(root: &Path, config: &Config) -> Result<StatusReport> {
    let rel_path = format!("{}/graph.json", config.output.dir.trim_end_matches('/'));
    let graph_path = root.join(&config.output.dir).join("graph.json");

    let content = match std::fs::read_to_string(&graph_path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(StatusReport::new(GraphState::Absent, rel_path));
        }
        Err(e) => {
            let mut report = StatusReport::new(GraphState::Unreadable, rel_path);
            report.unreadable_reason = Some(format!("io error: {e}"));
            return Ok(report);
        }
    };

    // The version pre-probe runs BEFORE the full Graph parse so a
    // schema mismatch classifies as its own state instead of blurring
    // into the deserializer's error string. `schema_version` is the
    // only typed discriminator; `meta` is probed as raw JSON — a
    // snapshot whose meta shape differs from this binary's is exactly
    // the situation the probe classifies, so its shape must not be
    // able to fail the probe into `unreadable`.
    #[derive(serde::Deserialize)]
    struct VersionProbe {
        schema_version: u32,
        #[serde(default)]
        meta: serde_json::Value,
    }
    let probe: VersionProbe = match serde_json::from_str(&content) {
        Ok(probe) => probe,
        Err(e) => {
            let mut report = StatusReport::new(GraphState::Unreadable, rel_path);
            report.unreadable_reason = Some(e.to_string());
            return Ok(report);
        }
    };
    if probe.schema_version != SCHEMA_VERSION {
        let mut report = StatusReport::new(GraphState::SchemaMismatch, rel_path);
        report.snapshot_schema_version = Some(probe.schema_version);
        report.snapshot_nodex_version = probe
            .meta
            .get("nodex_version")
            .and_then(serde_json::Value::as_str)
            .filter(|v| !v.is_empty())
            .map(str::to_string);
        return Ok(report);
    }

    let graph: Graph = match serde_json::from_str(&content) {
        Ok(graph) => graph,
        Err(e) => {
            let mut report = StatusReport::new(GraphState::Unreadable, rel_path);
            report.snapshot_schema_version = Some(probe.schema_version);
            report.unreadable_reason = Some(e.to_string());
            return Ok(report);
        }
    };

    let divergence = compute_divergence(&graph, config, root, DivergenceProbe::Content)?;
    let state = if divergence.is_divergent() {
        GraphState::Outdated
    } else {
        GraphState::Current
    };
    let mut report = StatusReport::new(state, rel_path);
    report.snapshot_schema_version = Some(probe.schema_version);
    report.snapshot_nodex_version = Some(graph.meta().nodex_version.clone());
    report.unbuildable_paths = graph
        .parse_failures()
        .iter()
        .map(|f| f.path.clone())
        .collect();
    if divergence.is_divergent() {
        report.divergence = Some(divergence);
    }
    Ok(report)
}

/// Read the project's graph snapshot — the only seam through which a
/// command consumes `graph.json`. Returns the graph plus any staleness
/// warnings for the caller's envelope:
///
/// - a missing file is the typed [`Error::MissingGraph`] (`GRAPH_MISSING`),
///   so "unbuilt" is machine-distinguishable from a permissions failure;
/// - a readable snapshot gets the cheap exact probe
///   ([`DivergenceProbe::Membership`]); divergence renders one prose
///   warning — advisory only, never a gate;
/// - a probe failure also degrades to a warning (the `BuildCache::load`
///   precedent): staleness advice must never block a read.
///
/// Absence of the warning asserts membership + config fidelity only;
/// content is deliberately not hashed on this path, so that reading the
/// graph costs one scope walk. [`Snapshot::require`] escalates to the
/// content probe on the one question the cheap probe cannot answer.
pub fn load_graph(root: &Path, config: &Config) -> Result<Snapshot> {
    let graph_path = root.join(&config.output.dir).join("graph.json");
    let content = match std::fs::read_to_string(&graph_path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::MissingGraph { path: graph_path });
        }
        Err(source) => {
            return Err(Error::Io {
                path: graph_path,
                source,
            });
        }
    };
    let graph: Graph = serde_json::from_str(&content).map_err(|e| Error::Parse {
        path: graph_path,
        source: crate::error::ParseError::Json(e),
    })?;

    let mut warnings = Vec::new();
    match compute_divergence(&graph, config, root, DivergenceProbe::Membership) {
        Ok(divergence) if divergence.is_divergent() => {
            warnings.push(crate::Warning::new(
                crate::WarningCode::SnapshotDivergence,
                divergence_advisory(&divergence),
            ));
        }
        Ok(_) => {}
        Err(e) => {
            warnings.push(crate::Warning::new(
                crate::WarningCode::SnapshotDivergence,
                format!(
                    "graph staleness probe failed: {} — results may not reflect the working tree",
                    crate::error::chain(&e)
                ),
            ));
        }
    }
    Ok(Snapshot { graph, warnings })
}

/// A graph read from `graph.json`, together with what is known about how far
/// it has drifted from the working tree.
///
/// A lookup that misses is a question about the project rather than about
/// this reading of it: absence from a snapshot is only absence from the
/// project once the snapshot is known to match. Routing every missed lookup
/// through [`require`](Self::require) is what keeps the two apart — the
/// alternative is a confident `NOT_FOUND` about a document sitting on disk,
/// which a consumer dispatching on the code cannot tell from the real thing.
///
/// The working tree that settles the question is passed to `require` rather
/// than stored here, so this type owns everything it holds and stays free of
/// lifetime parameters — the snapshot is a value a caller may keep, while
/// the measurement it may need is a borrow taken at the moment of asking.
#[derive(Debug)]
pub struct Snapshot {
    graph: Graph,
    warnings: Vec<crate::Warning>,
}

impl Snapshot {
    /// The graph as the snapshot holds it.
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    /// The staleness advisories this read produced, for the caller's
    /// envelope. Advisory only, never a gate.
    pub fn warnings(&self) -> Vec<crate::Warning> {
        self.warnings.clone()
    }

    /// An answer from this snapshot, with a missed lookup resolved against the
    /// working tree. Every other outcome passes through untouched.
    pub fn require<T>(&self, root: &Path, config: &Config, answer: Result<T>) -> Result<T> {
        match answer {
            Err(Error::MissingNode(asked)) => Err(self.absence_of(root, config, asked)),
            other => other,
        }
    }

    /// What it means that this snapshot does not hold `id`, measured rather
    /// than assumed, and asked only once a lookup has already missed.
    ///
    /// The probe a read pays for is membership-only, which is enough to
    /// *report* drift but never enough to *deny* a document: an in-place edit
    /// that gives a document a new id leaves the path set and the config hash
    /// untouched, so that probe agrees while the id the caller asked for sits
    /// on disk. So a miss — and only a miss, which ends the command — pays for
    /// the content probe, whose verdict decides between the three answers a
    /// consumer must be able to tell apart, each with a remedy that can
    /// actually succeed:
    ///
    /// - the snapshot matches the working tree, so the id is genuinely not in
    ///   the project (`NOT_FOUND` — correct the id);
    /// - the snapshot has drifted, so it never read what holds that id
    ///   (`GRAPH_OUTDATED` — rebuild);
    /// - the working tree could not be read, so nothing about it has been
    ///   established at all. That is neither absence nor staleness, and a
    ///   rebuild cannot fix it — it fails the same way. The probe's own error
    ///   is the answer, naming the condition whose repair is the remedy.
    fn absence_of(&self, root: &Path, config: &Config, asked: crate::error::Lookup) -> Error {
        match compute_divergence(&self.graph, config, root, DivergenceProbe::Content) {
            Ok(divergence) if divergence.is_divergent() => Error::StaleGraph {
                asked,
                divergence: divergence_cause(&divergence),
            },
            Ok(_) => Error::MissingNode(asked),
            Err(cause) => cause,
        }
    }
}

/// The divergence as a cause phrase, naming every divergent dimension and
/// nothing else. Two callers state a remedy around it — the read-path
/// advisory and [`Error::StaleGraph`] — and a remedy baked in here would be
/// stated twice by whichever caller adds its own.
fn divergence_cause(divergence: &SnapshotDivergence) -> String {
    let mut causes = Vec::new();
    if divergence.config_changed {
        causes.push("config changed".to_string());
    }
    if !divergence.added_paths.is_empty() {
        causes.push(format!("{} file(s) added", divergence.added_paths.len()));
    }
    if !divergence.removed_paths.is_empty() {
        causes.push(format!(
            "{} file(s) removed",
            divergence.removed_paths.len()
        ));
    }
    if let Some(changed) = &divergence.changed_paths
        && !changed.is_empty()
    {
        causes.push(format!("{} file(s) changed", changed.len()));
    }
    format!(
        "graph.json is outdated ({} since the last build)",
        causes.join("; ")
    )
}

/// The read-path advisory: the cause, plus what it means for the answer the
/// caller is about to receive and how to clear it.
fn divergence_advisory(divergence: &SnapshotDivergence) -> String {
    format!(
        "{} — results may not reflect the working tree; run `nodex build`",
        divergence_cause(divergence)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use tempfile::TempDir;

    fn project_with(docs: &[(&str, &str)]) -> (TempDir, Config) {
        let dir = TempDir::new().unwrap();
        let mut config = Config::default();
        config.scope.include = vec!["docs/**/*.md".to_string()];
        for (rel, content) in docs {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        (dir, config)
    }

    fn write_snapshot(root: &Path, config: &Config, graph: &Graph) {
        let out_dir = root.join(&config.output.dir);
        std::fs::create_dir_all(&out_dir).unwrap();
        std::fs::write(
            out_dir.join("graph.json"),
            crate::output::json::render_graph_json(graph),
        )
        .unwrap();
    }

    fn build_and_snapshot(root: &Path, config: &Config) -> Graph {
        let outcome = crate::builder::build(root, config, true).expect("build");
        write_snapshot(root, config, &outcome.graph);
        outcome.graph
    }

    const DOC_A: (&str, &str) = ("docs/a.md", "---\nid: doc-a\ntitle: A\n---\n# A\n");

    #[test]
    fn compute_status_reports_absent_without_snapshot() {
        let (dir, config) = project_with(&[DOC_A]);
        let report = compute_status(dir.path(), &config).unwrap();
        assert_eq!(report.state, GraphState::Absent);
        assert_eq!(report.graph_path, "_index/graph.json");
        assert!(report.snapshot_schema_version.is_none());
        assert!(report.divergence.is_none());
    }

    #[test]
    fn compute_status_reports_unreadable_on_corrupt_json() {
        let (dir, config) = project_with(&[DOC_A]);
        std::fs::create_dir_all(dir.path().join("_index")).unwrap();
        std::fs::write(dir.path().join("_index/graph.json"), "not json").unwrap();
        let report = compute_status(dir.path(), &config).unwrap();
        assert_eq!(report.state, GraphState::Unreadable);
        assert!(report.unreadable_reason.is_some());
    }

    #[test]
    fn compute_status_reports_schema_mismatch_with_recorded_version() {
        // The version pre-probe classifies an old snapshot as its own
        // state — never blurred into an unreadable-shape error.
        let (dir, config) = project_with(&[DOC_A]);
        std::fs::create_dir_all(dir.path().join("_index")).unwrap();
        std::fs::write(
            dir.path().join("_index/graph.json"),
            r#"{"schema_version": 1, "nodes": {}, "edges": []}"#,
        )
        .unwrap();
        let report = compute_status(dir.path(), &config).unwrap();
        assert_eq!(report.state, GraphState::SchemaMismatch);
        assert_eq!(report.snapshot_schema_version, Some(1));
        assert_eq!(report.supported_schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn version_probe_tolerates_foreign_meta_shapes() {
        // `schema_version` alone discriminates; a snapshot whose `meta`
        // shape this binary does not recognise still classifies as
        // `schema_mismatch` — the probe must not be failed into
        // `unreadable` by the very field a different version reshaped.
        let (dir, config) = project_with(&[DOC_A]);
        std::fs::create_dir_all(dir.path().join("_index")).unwrap();
        std::fs::write(
            dir.path().join("_index/graph.json"),
            r#"{"schema_version": 99, "meta": {"nodex_version": 42, "extra": [1]}, "nodes": {}}"#,
        )
        .unwrap();
        let report = compute_status(dir.path(), &config).unwrap();
        assert_eq!(report.state, GraphState::SchemaMismatch);
        assert_eq!(report.snapshot_schema_version, Some(99));
        assert_eq!(
            report.snapshot_nodex_version, None,
            "a non-string nodex_version is simply absent, never a failure"
        );
    }

    #[test]
    fn compute_status_reports_current_on_pristine_snapshot() {
        let (dir, config) = project_with(&[DOC_A]);
        build_and_snapshot(dir.path(), &config);
        let report = compute_status(dir.path(), &config).unwrap();
        assert_eq!(report.state, GraphState::Current);
        assert_eq!(report.snapshot_schema_version, Some(SCHEMA_VERSION));
        assert_eq!(
            report.snapshot_nodex_version.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert!(report.divergence.is_none());
    }

    #[test]
    fn compute_status_reports_outdated_with_changed_paths_on_edit() {
        let (dir, config) = project_with(&[DOC_A]);
        build_and_snapshot(dir.path(), &config);
        std::fs::write(
            dir.path().join("docs/a.md"),
            "---\nid: doc-a\ntitle: A\n---\n# A\n\nedited\n",
        )
        .unwrap();
        let report = compute_status(dir.path(), &config).unwrap();
        assert_eq!(report.state, GraphState::Outdated);
        let divergence = report.divergence.expect("outdated carries the delta");
        assert_eq!(
            divergence.changed_paths,
            Some(vec!["docs/a.md".to_string()])
        );
        assert!(divergence.added_paths.is_empty());
        assert!(!divergence.config_changed);
    }

    #[test]
    fn compute_status_flags_graph_shaping_config_change() {
        let (dir, mut config) = project_with(&[DOC_A]);
        build_and_snapshot(dir.path(), &config);
        config.identity.kind_rules = vec![crate::config::KindRule {
            glob: "docs/**".into(),
            kind: "generic".into(),
        }];
        let report = compute_status(dir.path(), &config).unwrap();
        assert_eq!(report.state, GraphState::Outdated);
        assert!(report.divergence.unwrap().config_changed);
    }

    #[test]
    fn membership_probe_never_measures_content() {
        // The cheap probe reads no document content: an edited file is
        // invisible to it (`changed_paths: None` — unmeasured, never
        // claimed fresh), while membership stays exact.
        let (dir, config) = project_with(&[DOC_A]);
        let graph = build_and_snapshot(dir.path(), &config);
        std::fs::write(
            dir.path().join("docs/a.md"),
            "---\nid: doc-a\n---\nedited\n",
        )
        .unwrap();
        let divergence =
            compute_divergence(&graph, &config, dir.path(), DivergenceProbe::Membership).unwrap();
        assert_eq!(divergence.changed_paths, None);
        assert!(!divergence.is_divergent());
    }

    #[test]
    fn divergence_reports_added_and_removed_paths_exactly() {
        let (dir, config) = project_with(&[DOC_A]);
        let graph = build_and_snapshot(dir.path(), &config);
        std::fs::write(dir.path().join("docs/new.md"), "# New\n").unwrap();
        std::fs::remove_file(dir.path().join("docs/a.md")).unwrap();
        let divergence =
            compute_divergence(&graph, &config, dir.path(), DivergenceProbe::Membership).unwrap();
        assert_eq!(divergence.added_paths, vec!["docs/new.md".to_string()]);
        assert_eq!(divergence.removed_paths, vec!["docs/a.md".to_string()]);
        assert!(divergence.is_divergent());
    }

    #[test]
    fn byte_level_rewrite_counts_as_changed_under_the_content_probe() {
        // The probe hashes raw bytes — the same digest the build
        // records — so a covered file rewritten with bytes that are not
        // even valid UTF-8 still compares against its recorded hash and
        // reports changed.
        let (dir, config) = project_with(&[DOC_A]);
        let graph = build_and_snapshot(dir.path(), &config);
        std::fs::write(dir.path().join("docs/a.md"), [0xFF, 0xFE, 0x01]).unwrap();
        let divergence =
            compute_divergence(&graph, &config, dir.path(), DivergenceProbe::Content).unwrap();
        assert_eq!(
            divergence.changed_paths,
            Some(vec!["docs/a.md".to_string()])
        );
    }

    #[test]
    fn recorded_non_utf8_failure_with_unchanged_bytes_is_current() {
        // A non-UTF-8 in-scope file is covered-but-unbuildable: the
        // snapshot records its byte digest, so unchanged bytes read
        // `current` (a rebuild could change nothing) and the path
        // surfaces as unbuildable, never as membership divergence.
        let (dir, config) = project_with(&[DOC_A]);
        let raw_bytes: &[u8] = &[0xFF, 0xFE, 0x01, 0x02];
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::write(dir.path().join("docs/raw.md"), raw_bytes).unwrap();
        let graph = build_and_snapshot(dir.path(), &config);
        assert_eq!(graph.parse_failures().len(), 1, "fixture failed to fail");

        let report = compute_status(dir.path(), &config).unwrap();
        assert_eq!(report.state, GraphState::Current);
        assert_eq!(report.unbuildable_paths, vec!["docs/raw.md".to_string()]);

        // Different broken bytes: the recorded digest distinguishes
        // them, and a rebuild genuinely refreshes the record.
        std::fs::write(dir.path().join("docs/raw.md"), [0xFF, 0xFE, 0x09]).unwrap();
        let report = compute_status(dir.path(), &config).unwrap();
        assert_eq!(report.state, GraphState::Outdated);
        assert_eq!(
            report.divergence.unwrap().changed_paths,
            Some(vec!["docs/raw.md".to_string()])
        );
    }

    #[test]
    fn recorded_parse_failure_is_covered_not_divergent() {
        // Coverage = nodes ∪ parse_failures: a faithfully-built snapshot
        // with an unbuildable doc is CURRENT — `check`'s parse_failure
        // rule owns the breakage signal, and status surfaces the path
        // distinctly instead of claiming a staleness a rebuild could
        // never clear.
        let (dir, config) =
            project_with(&[DOC_A, ("docs/bad.md", "---\nid: [unclosed\n---\n# Bad\n")]);
        let graph = build_and_snapshot(dir.path(), &config);
        assert_eq!(graph.parse_failures().len(), 1, "fixture failed to fail");

        let report = compute_status(dir.path(), &config).unwrap();
        assert_eq!(report.state, GraphState::Current);
        assert_eq!(report.unbuildable_paths, vec!["docs/bad.md".to_string()]);

        let warnings = load_graph(dir.path(), &config).unwrap().warnings();
        assert!(
            warnings.is_empty(),
            "a recorded failure must not ride a staleness warning: {warnings:?}"
        );
    }

    #[test]
    fn changed_parse_failure_bytes_flag_outdated() {
        // Same broken doc, new bytes: the recorded failure's
        // content_hash distinguishes "same broken bytes" (current) from
        // "changed since build" (outdated) — a rebuild genuinely
        // refreshes the record, so the remedy holds.
        let (dir, config) =
            project_with(&[DOC_A, ("docs/bad.md", "---\nid: [unclosed\n---\n# Bad\n")]);
        build_and_snapshot(dir.path(), &config);
        std::fs::write(
            dir.path().join("docs/bad.md"),
            "---\nid: [still-unclosed\n---\n# Bad v2\n",
        )
        .unwrap();
        let report = compute_status(dir.path(), &config).unwrap();
        assert_eq!(report.state, GraphState::Outdated);
        assert_eq!(
            report.divergence.unwrap().changed_paths,
            Some(vec!["docs/bad.md".to_string()])
        );
    }

    #[test]
    fn load_graph_missing_snapshot_is_typed() {
        let (dir, config) = project_with(&[DOC_A]);
        let err = load_graph(dir.path(), &config).unwrap_err();
        assert!(matches!(err, Error::MissingGraph { .. }));
        assert_eq!(err.code(), "GRAPH_MISSING");
        assert!(
            err.to_string().contains("nodex build"),
            "the remedy rides the message: {err}"
        );
    }

    #[test]
    fn load_graph_attaches_one_divergence_warning() {
        let (dir, config) = project_with(&[DOC_A]);
        build_and_snapshot(dir.path(), &config);

        let fresh = load_graph(dir.path(), &config).unwrap();
        assert!(
            fresh.warnings().is_empty(),
            "fresh snapshot is warning-free"
        );
        assert!(
            matches!(
                fresh.require(
                    dir.path(),
                    &config,
                    Err::<(), _>(Error::MissingNode(crate::error::Lookup::Id(
                        "absent".into()
                    )))
                ),
                Err(Error::MissingNode(_))
            ),
            "a current snapshot answers absence as absence"
        );

        std::fs::write(dir.path().join("docs/new.md"), "# New\n").unwrap();
        let snapshot = load_graph(dir.path(), &config).unwrap();
        let (graph, warnings) = (snapshot.graph(), snapshot.warnings());
        assert_eq!(graph.node_count(), 1, "the read itself still succeeds");
        let attributed = snapshot
            .require(
                dir.path(),
                &config,
                Err::<(), _>(Error::MissingNode(crate::error::Lookup::Id(
                    "docs-new".into(),
                ))),
            )
            .unwrap_err();
        assert_eq!(
            attributed.code(),
            "GRAPH_OUTDATED",
            "a miss against a snapshot known to disagree is not absence: {attributed}"
        );
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, crate::WarningCode::SnapshotDivergence);
        assert!(
            warnings[0].message.contains("outdated")
                && warnings[0].message.contains("1 file(s) added")
                && warnings[0].message.contains("nodex build"),
            "warning names the divergence and the remedy: {warnings:?}"
        );
    }
}
