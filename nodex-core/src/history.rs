//! What the project's history holds of its documents: the documents a commit
//! `HEAD` reaches held and `HEAD` no longer holds ([`find_history`]), and
//! every binding a recorded revision offered ([`ProjectHistory`]), which tells
//! a reference to a document since removed from a name no document carried.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::builder::resolver::Bindings;
use crate::builder::scanner::ScopeSpelling;
use crate::config::Config;
use crate::error::{Error, Result};
use crate::git::{CommitStamp, FileEvent, FileEventKind, Repository};
use crate::model::Node;
use crate::parser::{ParseConfig, parse_document};
use crate::query::NodeRef;
use crate::rules::git_drift::DriftHistory;
use crate::warning::{Warning, WarningCode};

/// What [`find_history`] is asked about.
#[derive(Debug, Clone, Copy)]
pub enum HistorySubject<'a> {
    Id(&'a str),
    /// A project path, normalized for lookup.
    Path(&'a str),
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct HistoryReport {
    /// Whether the working tree holds a document under the id, or at the
    /// path.
    pub present: bool,
    /// Each document a commit `HEAD` reaches held under the id, or at the
    /// path, whose path `HEAD` no longer holds, latest removal first.
    pub removed: Vec<RemovedDocument>,
    /// Removed paths whose last revision does not parse, so the id each held
    /// is unknown; for a path lookup, that path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unparsed: Vec<String>,
}

/// A document as its last revision reads under the current configuration,
/// with the commits that added and removed its path.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RemovedDocument {
    #[serde(flatten)]
    pub node: NodeRef,
    /// The last revision's blob; `git cat-file -p <blob>` prints it.
    pub blob: String,
    /// The earliest commit adding the path; absent where only a merge's
    /// resolution added it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_added: Option<CommitStamp>,
    /// The latest commit removing the path.
    pub last_removed: CommitStamp,
}

/// What [`find_history`] found, with the working-tree build's disclosures.
pub struct HistoryOutcome {
    pub report: HistoryReport,
    pub warnings: Vec<Warning>,
}

/// Every document the history `HEAD` reaches held under `subject` and no
/// longer holds, read under the current configuration: a removed path counts
/// where the scope's spelling admits it, and its last revision is parsed as a
/// build would parse it now. `present` reads the working tree.
///
/// A clone whose history git reshapes — shallow, grafted or replaced — is
/// refused: a document it does not hold is one it cannot say was never there.
pub fn find_history(
    root: &Path,
    config: &Config,
    repository: &Repository,
    subject: HistorySubject<'_>,
) -> Result<HistoryOutcome> {
    require_whole_history(repository, "nodex history")?;
    let built = crate::builder::build_with_overlay(root, config, &[])?;
    let present = match subject {
        HistorySubject::Id(id) => built.graph.node(id).is_some(),
        HistorySubject::Path(path) => built
            .graph
            .nodes()
            .values()
            .any(|node| crate::path_guard::forward_string(&node.path) == path),
    };

    let spelling = ScopeSpelling::new(config)?;
    let roots = spelling.include_roots();
    let removals = match repository
        .head()
        .map_err(|e| git_error("resolving HEAD", e))?
    {
        Some(head) => {
            let events = repository
                .file_events(&head, &roots)
                .map_err(|e| git_error("reading the documents history added and removed", e))?;
            let held: BTreeSet<PathBuf> = repository
                .held_files(&head, &roots)
                .map_err(|e| git_error("listing the documents HEAD holds", e))?
                .into_iter()
                .map(|(path, _)| path)
                .collect();
            removals(
                events.into_iter().filter(|event| {
                    spelling.admits(&event.path)
                        && match subject {
                            HistorySubject::Path(path) => {
                                crate::path_guard::forward_string(&event.path) == path
                            }
                            HistorySubject::Id(_) => true,
                        }
                }),
                &held,
            )
        }
        None => Vec::new(),
    };
    let contents = repository
        .blobs(
            &removals
                .iter()
                .map(|removal| removal.blob.clone())
                .collect::<Vec<_>>(),
        )
        .map_err(|e| git_error("reading the removed documents' last revisions", e))?;

    let parse_config = ParseConfig::new(config);
    let mut removed = Vec::new();
    let mut unparsed = Vec::new();
    for (removal, content) in removals.into_iter().zip(contents) {
        let Some(node) = parse_revision(&removal.path, content, &parse_config) else {
            unparsed.push(crate::path_guard::forward_string(&removal.path));
            continue;
        };
        if let HistorySubject::Id(id) = subject
            && node.id != id
        {
            continue;
        }
        removed.push(RemovedDocument {
            node: NodeRef::from_node(&node),
            blob: removal.blob,
            first_added: removal.first_added,
            last_removed: removal.last_removed,
        });
    }
    Ok(HistoryOutcome {
        report: HistoryReport {
            present,
            removed,
            unparsed,
        },
        warnings: built.warnings,
    })
}

/// A document history removed and a tree no longer holds.
struct Removal {
    path: PathBuf,
    /// The revision its latest removal took away.
    blob: String,
    first_added: Option<CommitStamp>,
    last_removed: CommitStamp,
}

/// The files `events` (newest commit first) saw removed whose path `held`
/// does not hold, each with its latest removal and its path's earliest
/// addition, latest removal first.
fn removals(events: impl Iterator<Item = FileEvent>, held: &BTreeSet<PathBuf>) -> Vec<Removal> {
    let mut removals: BTreeMap<PathBuf, Removal> = BTreeMap::new();
    let mut added: BTreeMap<PathBuf, CommitStamp> = BTreeMap::new();
    for event in events.filter(|event| !held.contains(&event.path)) {
        match event.kind {
            FileEventKind::Removed => {
                removals.entry(event.path.clone()).or_insert(Removal {
                    path: event.path,
                    blob: event.blob,
                    first_added: None,
                    last_removed: event.commit,
                });
            }
            FileEventKind::Added => {
                added.insert(event.path, event.commit);
            }
        }
    }
    let mut removals: Vec<Removal> = removals
        .into_values()
        .map(|mut removal| {
            removal.first_added = added.remove(&removal.path);
            removal
        })
        .collect();
    removals.sort_by(|a, b| {
        b.last_removed
            .committed
            .cmp(&a.last_removed.committed)
            .then_with(|| a.path.cmp(&b.path))
    });
    removals
}

/// What git says about the project for one command, read once however many
/// passes ask. A command running more than one pass over one project — a
/// `--content` gate judges the working tree and the proposal it would become
/// — holds one of these across them, because a repository's history does not
/// move while a command reads it.
///
/// The drift reading is taken on demand ([`DriftHistory`]). The bindings
/// history recorded are read here, where a failure still refuses the command:
/// a reference no row could classify by its history would take whichever row
/// comes next, and a gate whose verdict a damaged repository can change is
/// one a damaged clone passes.
pub struct ProjectHistory {
    drift: DriftHistory,
    recorded: Option<Bindings>,
    /// What keeping the recorded bindings ran into, for the command that
    /// stores them.
    warnings: Vec<Warning>,
}

impl ProjectHistory {
    /// The reading of a project that reads no git: no drift threshold and no
    /// policy row asking history.
    pub const fn unread() -> Self {
        Self {
            drift: DriftHistory::unmeasured(),
            recorded: None,
            warnings: Vec::new(),
        }
    }

    /// The reading a project's own config asks for. It consults the kept
    /// readings and stores none.
    pub fn of(config: &Config, root: &Path) -> Result<Self> {
        Self::reading(config, root, false)
    }

    /// [`Self::of`] for the command that owns the kept readings, a `check` of
    /// the working tree or the index: it stores what it reads wherever the
    /// kept reading did not already answer, and reports on [`Self::warnings`]
    /// what kept it from doing so.
    pub fn refreshing(config: &Config, root: &Path) -> Result<Self> {
        Self::reading(config, root, true)
    }

    fn reading(config: &Config, root: &Path, refresh: bool) -> Result<Self> {
        let drift = if refresh {
            DriftHistory::refreshing(config, root)
        } else {
            DriftHistory::of(config, root)
        };
        let (recorded, warnings) = match config.history_policy_row() {
            Some(row) => {
                let repository = row_repository(root, row)?;
                let kept = KeptFile::new(root, config, RECORDED_FILE, refresh);
                (
                    Some(recorded_bindings(&repository, config, &kept)?),
                    kept.warnings(),
                )
            }
            None => (None, Vec::new()),
        };
        Ok(Self {
            drift,
            recorded,
            warnings,
        })
    }

    pub fn drift(&self) -> &DriftHistory {
        &self.drift
    }

    /// Every binding a revision of a document `HEAD` reaches offered, `None`
    /// where the project reads no history.
    pub(crate) fn recorded(&self) -> Option<&Bindings> {
        self.recorded.as_ref()
    }

    /// What a refreshing reading could not do with its kept files; always
    /// empty for one that only consults them.
    pub fn warnings(&self) -> Vec<Warning> {
        let mut warnings = self.drift.warnings();
        warnings.extend(self.warnings.iter().cloned());
        warnings
    }
}

const RECORDED_FILE: &str = "recorded.json";

/// On-disk shape version of `recorded.json`. Bump on any change to
/// [`KeptRecord`].
const RECORDED_SCHEMA_VERSION: u32 = 1;

/// `recorded.json` as it is written. The key is everything a reading of
/// `head` depends on besides the commit: what admits a path and infers an id,
/// with the binary that read them (`graph_config_hash`), and the project's
/// prefix, which bounds the walk.
#[derive(Serialize, Deserialize)]
struct KeptRecord {
    schema_version: u32,
    config: String,
    prefix: String,
    head: String,
    /// Each path a commit added or removed a document at, with the id that
    /// revision carried.
    events: BTreeSet<(String, String)>,
    /// Each document `head` holds, by path.
    held: BTreeMap<String, HeldRevision>,
}

#[derive(Serialize, Deserialize)]
struct HeldRevision {
    blob: String,
    /// `None` where the revision does not parse.
    id: Option<String>,
}

impl KeptRecord {
    fn bindings(&self) -> Bindings {
        let held = self
            .held
            .iter()
            .filter_map(|(path, held)| held.id.as_deref().map(|id| (path, id)));
        Bindings::of(
            self.events
                .iter()
                .map(|(path, id)| (path, id.as_str()))
                .chain(held)
                .map(|(path, id)| (Path::new(path.as_str()), id)),
        )
    }
}

/// Every binding a revision of a document `HEAD` reaches offered, read as a
/// build would read each revision now: at each commit adding or removing the
/// document, and at `HEAD`. An id a document carried only between those
/// revisions, changed in place, is not among them.
///
/// A kept reading the current `HEAD` reaches is completed by the commits
/// since; `HEAD`'s documents are listed again, and only a revision the kept
/// reading did not hold is parsed.
fn recorded_bindings(
    repository: &Repository,
    config: &Config,
    kept: &KeptFile,
) -> Result<Bindings> {
    let Some(head) = repository
        .head()
        .map_err(|e| git_error("resolving HEAD", e))?
    else {
        return Ok(Bindings::default());
    };
    let key = crate::builder::graph_config_hash(config);
    let prefix = repository.prefix().to_str();
    let prior = prefix.and_then(|prefix| {
        kept.load::<KeptRecord>(RECORDED_SCHEMA_VERSION)
            .filter(|record| record.config == key && record.prefix == prefix)
    });
    let (mut events, prior_held, range) = match prior {
        Some(record) if record.head == head => return Ok(record.bindings()),
        Some(record) if matches!(repository.is_ancestor(&record.head, &head), Ok(true)) => {
            let range = format!("{}..{head}", record.head);
            (record.events, record.held, range)
        }
        _ => (BTreeSet::new(), BTreeMap::new(), head.clone()),
    };

    let spelling = ScopeSpelling::new(config)?;
    let roots = spelling.include_roots();
    let walked: Vec<FileEvent> = repository
        .file_events(&range, &roots)
        .map_err(|e| git_error("reading the documents history added and removed", e))?
        .into_iter()
        .filter(|event| spelling.admits(&event.path))
        .collect();
    let listed: Vec<(String, String)> = repository
        .held_files(&head, &roots)
        .map_err(|e| git_error("listing the documents HEAD holds", e))?
        .into_iter()
        .filter(|(path, _)| spelling.admits(path))
        .map(|(path, blob)| (crate::path_guard::forward_string(&path), blob))
        .collect();

    let mut revisions: Vec<(String, String)> = walked
        .iter()
        .map(|event| {
            (
                crate::path_guard::forward_string(&event.path),
                event.blob.clone(),
            )
        })
        .chain(
            listed
                .iter()
                .filter(|(path, blob)| prior_held.get(path).is_none_or(|held| held.blob != *blob))
                .cloned(),
        )
        .collect();
    revisions.sort();
    revisions.dedup();
    let contents = repository
        .blobs(
            &revisions
                .iter()
                .map(|(_, blob)| blob.clone())
                .collect::<Vec<_>>(),
        )
        .map_err(|e| git_error("reading the recorded revisions", e))?;
    let parse_config = ParseConfig::new(config);
    let mut parsed: BTreeMap<(String, String), Option<String>> = revisions
        .into_iter()
        .zip(contents)
        .map(|((path, blob), content)| {
            let id = parse_revision(Path::new(&path), content, &parse_config).map(|node| node.id);
            ((path, blob), id)
        })
        .collect();

    for event in &walked {
        let revision = (
            crate::path_guard::forward_string(&event.path),
            event.blob.clone(),
        );
        if let Some(Some(id)) = parsed.get(&revision) {
            events.insert((revision.0, id.clone()));
        }
    }
    let mut prior_held = prior_held;
    let held = listed
        .into_iter()
        .map(|(path, blob)| {
            let id = match prior_held.remove(&path) {
                Some(held) if held.blob == blob => held.id,
                _ => parsed.remove(&(path.clone(), blob.clone())).flatten(),
            };
            (path, HeldRevision { blob, id })
        })
        .collect();
    let record = KeptRecord {
        schema_version: RECORDED_SCHEMA_VERSION,
        config: key,
        prefix: prefix.unwrap_or_default().to_owned(),
        head,
        events,
        held,
    };
    kept.store(|| {
        prefix
            .map(|_| &record)
            .ok_or("the project's path inside the repository is not UTF-8")
    });
    Ok(record.bindings())
}

/// The repository `row`'s history is read from: a work tree holding the
/// whole history. Anything else is the environment the row requires and does
/// not have, refused as configuration is, naming the row and the remedy; a
/// clone git reshapes cannot tell a document it does not hold from one that
/// was never there.
fn row_repository(
    root: &Path,
    row: &crate::config::UnresolvedPolicyRuleConfig,
) -> Result<Repository> {
    let reason = match Repository::discover(root) {
        Ok(Some(repository)) => match repository.reshapes_history() {
            Ok(false) => return Ok(repository),
            Ok(true) => "this clone's history is shallow, grafted or replaced".to_string(),
            Err(e) => {
                format!("whether this clone holds its whole history could not be read ({e})")
            }
        },
        Ok(None) => format!("no git work tree was found for {}", root.display()),
        Err(e) => format!("its repository could not be resolved ({e})"),
    };
    Err(Error::Config(format!(
        "detection.unresolved_policy row {name:?} reads history but {reason}; run inside a git \
         work tree holding the whole history (`git fetch --unshallow`), or remove the row's \
         history",
        name = row.name,
    )))
}

/// A revision as a build would parse it now; `None` where it does not parse.
fn parse_revision(path: &Path, content: Vec<u8>, parse_config: &ParseConfig<'_>) -> Option<Node> {
    let content = String::from_utf8(content).ok()?;
    parse_document(path, &content, parse_config)
        .ok()
        .map(|parsed| parsed.node)
}

/// Refuse a clone whose history git reshapes — shallow, grafted or replaced:
/// a document it does not hold is one it cannot say was never there.
fn require_whole_history(repository: &Repository, reader: &str) -> Result<()> {
    if repository
        .reshapes_history()
        .map_err(|e| git_error("whether the clone holds its whole history", e))?
    {
        return Err(Error::Git {
            context: format!("{reader} requires the whole history; fetch it"),
            stderr: "this clone's history is shallow, grafted or replaced".to_string(),
        });
    }
    Ok(())
}

fn git_error(context: &str, error: std::io::Error) -> Error {
    Error::Git {
        context: context.to_string(),
        stderr: error.to_string(),
    }
}

/// A reading of git kept in the output directory between commands, keyed by
/// the commit it was taken at, and whether this command owns it.
///
/// A reading of a commit is a function of that commit, so a kept one whose
/// commit the current `HEAD` reaches is completed by reading only the commits
/// between them. Only the owner, a `check` of the working tree or the index,
/// stores a reading and reports what kept it from reading or storing one;
/// every other command consults the file and writes nothing, so
/// `check --content` stays the read-only gate it is. A file of another shape
/// version is passed over without a word, as a foreign `cache.json` is: that
/// is the expected invalidation after an upgrade, and the walk it forces
/// answers the same.
pub(crate) struct KeptFile {
    root: PathBuf,
    path: PathBuf,
    refresh: bool,
    warnings: Mutex<Vec<Warning>>,
}

impl KeptFile {
    pub(crate) fn new(root: &Path, config: &Config, name: &str, refresh: bool) -> Self {
        Self {
            root: root.to_path_buf(),
            path: root.join(&config.output.dir).join(name),
            refresh,
            warnings: Mutex::new(Vec::new()),
        }
    }

    /// The kept reading, where the file holds one of `schema_version`.
    pub(crate) fn load<T: DeserializeOwned>(&self, schema_version: u32) -> Option<T> {
        let raw = match std::fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => return self.unreadable(e),
        };
        let value: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(value) => value,
            Err(e) => return self.unreadable(e),
        };
        // Every shape these files have had is an object, so anything else is
        // damage rather than a version to read past.
        if !value.is_object() {
            return self.unreadable("not a JSON object");
        }
        if value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            != Some(u64::from(schema_version))
        {
            return None;
        }
        serde_json::from_value(value).map_or_else(|e| self.unreadable(e), Some)
    }

    /// Pass over a kept file that cannot be read as a reading, saying so to
    /// the owner.
    pub(crate) fn unreadable<T>(&self, reason: impl std::fmt::Display) -> Option<T> {
        self.report(format!(
            "kept history unreadable at {}: {reason}; walking the whole history",
            self.path.display()
        ));
        None
    }

    /// Keep the reading `reading` builds, where this command owns the file.
    /// The write goes through the guarded primitive, so a crash leaves the
    /// previous file whole and a path escaping the project is refused rather
    /// than followed.
    pub(crate) fn store<T: Serialize>(
        &self,
        reading: impl FnOnce() -> std::result::Result<T, &'static str>,
    ) {
        if !self.refresh {
            return;
        }
        let stored = reading().map_err(str::to_owned).and_then(|reading| {
            let json =
                serde_json::to_string(&reading).expect("a kept reading is JSON-serialisable");
            crate::path_guard::write_atomic_in_root(&self.root, &self.path, &json)
                .map_err(|e| e.to_string())
        });
        if let Err(reason) = stored {
            self.report(format!(
                "history not kept at {}: {reason}; the next command walks the whole history again",
                self.path.display()
            ));
        }
    }

    /// What reading and storing ran into. Always empty for a command that
    /// only consults the file: it neither owns nor repairs it, and whatever
    /// it met is met again by the next refresh, which says so.
    pub(crate) fn warnings(&self) -> Vec<Warning> {
        self.warnings
            .lock()
            .expect("no reader panics while holding this")
            .clone()
    }

    fn report(&self, message: String) {
        if self.refresh {
            self.warnings
                .lock()
                .expect("no reader panics while holding this")
                .push(Warning::new(WarningCode::Cache, message));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::fixture::{commit_on, history_repo, run_git};
    use chrono::NaiveDate;

    fn day(n: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 1, n).expect("a valid day")
    }

    fn reading() -> Config {
        let mut config = Config::default();
        config.scope.include = vec!["docs/**/*.md".into()];
        config.detection.unresolved_policy = vec![crate::config::UnresolvedPolicyRuleConfig {
            name: "removed".into(),
            cause: crate::model::UnresolvedCause::IdNotFound,
            relations: Vec::new(),
            glob: None,
            source_protection: None,
            source_kinds: Vec::new(),
            history: Some(crate::model::TargetHistory::Recorded),
            severity: crate::config::UnresolvedSeverity::Info,
        }];
        config
    }

    fn doc(id: &str) -> String {
        format!("---\nid: {id}\n---\n# {id}\n")
    }

    fn binds(history: &ProjectHistory, id: &str) -> bool {
        matches!(
            crate::builder::resolver::resolve_target(
                crate::model::ReferenceSyntax::Frontmatter,
                id,
                "supersedes",
                Path::new(""),
                history.recorded().expect("the project reads history"),
                &[],
            ),
            crate::model::ResolvedTarget::Resolved { .. }
        )
    }

    /// A kept record `HEAD` reaches is completed by the commits since — the
    /// id planted in it alone is still bound — and otherwise binds what a
    /// whole walk binds: an id a removed document carried, the one a
    /// document carried when added, and the one `HEAD` holds it under. A
    /// record taken under another scope is passed over.
    #[test]
    fn a_kept_record_the_head_reaches_is_completed_by_the_commits_since() {
        let dir = history_repo();
        let root = dir.path();
        let config = reading();
        let kept = root.join(&config.output.dir).join(RECORDED_FILE);
        commit_on(root, day(1), 9, &[("docs/a.md", &doc("first"))]);
        ProjectHistory::refreshing(&config, root).unwrap();
        let mut record: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&kept).unwrap()).unwrap();
        record["events"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!(["docs/ghost.md", "ghost"]));
        std::fs::write(&kept, record.to_string()).unwrap();
        commit_on(root, day(2), 9, &[("docs/b.md", &doc("passing"))]);
        run_git(root, &["rm", "-q", "docs/b.md"]);
        commit_on(root, day(3), 9, &[]);
        commit_on(root, day(4), 9, &[("docs/a.md", &doc("renamed"))]);

        let completed = ProjectHistory::of(&config, root).unwrap();
        assert!(binds(&completed, "ghost"), "the kept record was consulted");
        for id in ["first", "passing", "renamed"] {
            assert!(binds(&completed, id), "{id}");
        }
        assert!(!binds(&completed, "never"));

        let mut rescoped = reading();
        rescoped.scope.include.push("notes/**/*.md".into());
        assert!(!binds(
            &ProjectHistory::of(&rescoped, root).unwrap(),
            "ghost"
        ));
        std::fs::remove_file(&kept).unwrap();
        let walked = ProjectHistory::of(&config, root).unwrap();
        for id in ["first", "passing", "renamed"] {
            assert!(binds(&walked, id), "{id}");
        }
    }
}
