//! What the project's history holds of its documents, read once into a ledger
//! kept between commands: the documents a revision `HEAD` reaches held that
//! the project no longer holds ([`find_history`]), and every binding a recorded
//! revision offered ([`ProjectHistory`]), which tells a reference to a
//! document since removed from a name no document carried.

use std::cmp::Reverse;
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
    /// Each document — an id at a path — a revision `HEAD` reaches held,
    /// `HEAD` included, that the project no longer holds there: under the id
    /// or at the path asked about, or, asked about neither, every one whose
    /// id the working tree no longer holds. Uncommitted removals first, then
    /// committed ones latest first, then those no commit removed.
    pub items: Vec<RemovedDocument>,
    pub total: usize,
    /// Whether the working tree holds a document under the id, or at the
    /// path, asked about; absent when asked about neither.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub present: Option<bool>,
    /// Paths a removal took a revision from that does not parse, so the id it
    /// held is unknown; for a path lookup, that path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unparsed: Vec<String>,
}

/// A document as a revision that held it reads under the current
/// configuration, with how it left the project.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RemovedDocument {
    #[serde(flatten)]
    pub node: NodeRef,
    /// The revision read; `git cat-file -p <blob>` prints it. The one its
    /// removal took away, `HEAD`'s for an uncommitted removal, and the one
    /// that first added it where no commit removed it.
    pub blob: String,
    /// The earliest commit adding the path with the id; absent where none
    /// did — only a merge's resolution added it, or the id was given in place.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_added: Option<CommitStamp>,
    pub removal: Removal,
}

/// How a document left the project.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Removal {
    /// The latest commit removing the path while it held the id.
    Committed { commit: CommitStamp },
    /// `HEAD` holds it, and the work tree has removed it, staged or not, or
    /// given the path another id.
    Uncommitted,
    /// No commit the walk reads removed it: the id was changed in place, or
    /// only a merge's resolution dropped the path.
    Unattributed,
}

/// What [`find_history`] found, with the working-tree build's disclosures and
/// what keeping the ledger ran into.
pub struct HistoryOutcome {
    pub report: HistoryReport,
    pub warnings: Vec<Warning>,
}

/// The documents — an id at a path — the history `HEAD` reaches held, `HEAD`
/// included, that the project no longer holds there, read under the current
/// configuration: a path counts where the scope's spelling admits it, and each
/// revision is parsed as a build would parse it now. `present` reads the
/// working tree.
///
/// A path whose revision at `HEAD`, or in the work tree, does not parse is not
/// judged to have dropped an id it may still carry. A clone whose history git
/// reshapes — shallow, grafted or replaced — is refused: a document it does
/// not hold is one it cannot say was never there.
pub fn find_history(
    root: &Path,
    config: &Config,
    repository: &Repository,
    subject: Option<HistorySubject<'_>>,
) -> Result<HistoryOutcome> {
    require_whole_history(repository, "nodex history")?;
    let built = crate::builder::build_with_overlay(root, config, &[])?;
    let kept = KeptFile::new(root, config, LEDGER_FILE, true);
    let ledger = ledger(repository, config, &kept)?;
    let deleted: BTreeSet<String> = if ledger.head.is_empty() {
        BTreeSet::new()
    } else {
        let roots = ScopeSpelling::new(config)?.include_roots();
        repository
            .removed_since(&ledger.head, &roots)
            .map_err(|e| git_error("listing the documents the work tree removed", e))?
            .iter()
            .map(|path| crate::path_guard::forward_string(path))
            .collect()
    };
    let worked: BTreeMap<String, &str> = built
        .graph
        .nodes()
        .values()
        .map(|node| {
            (
                crate::path_guard::forward_string(&node.path),
                node.id.as_str(),
            )
        })
        .collect();

    let asked = |path: &str, id: Option<&str>| match (subject, id) {
        (Some(HistorySubject::Path(asked)), _) => path == asked,
        (_, None) => true,
        (Some(HistorySubject::Id(asked)), Some(id)) => id == asked,
        (None, Some(id)) => built.graph.node(id).is_none(),
    };
    let mut items = Vec::new();
    let mut unparsed = Vec::new();
    let mut unread: Vec<(&str, &str, Option<&CommitStamp>, Removal)> = Vec::new();
    for (path, record) in &ledger.paths {
        if record.unparsed && asked(path, None) {
            unparsed.push(path.clone());
        }
        let holding = ledger.held.get(path).map(|held| held.id.as_deref());
        if holding == Some(None) {
            continue;
        }
        for (id, carried) in &record.ids {
            if holding == Some(Some(id.as_str())) || !asked(path, Some(id)) {
                continue;
            }
            let first_added = carried.first_added.as_ref().map(|added| &added.commit);
            match (&carried.last_removed, &carried.first_added) {
                (Some(removed), _) => items.push(RemovedDocument {
                    node: removed.node.clone(),
                    blob: removed.blob.clone(),
                    first_added: first_added.cloned(),
                    removal: Removal::Committed {
                        commit: removed.commit.clone(),
                    },
                }),
                (None, Some(added)) => {
                    unread.push((path, &added.blob, first_added, Removal::Unattributed));
                }
                (None, None) => {}
            }
        }
    }
    for (path, held) in &ledger.held {
        let left = deleted.contains(path)
            || held
                .id
                .as_deref()
                .is_some_and(|id| worked.get(path).is_some_and(|working| *working != id));
        if !left || !asked(path, held.id.as_deref()) {
            continue;
        }
        let first_added = held.id.as_ref().and_then(|id| {
            ledger
                .paths
                .get(path)?
                .ids
                .get(id)?
                .first_added
                .as_ref()
                .map(|added| &added.commit)
        });
        unread.push((path, &held.blob, first_added, Removal::Uncommitted));
    }

    let contents = repository
        .blobs(
            &unread
                .iter()
                .map(|(_, blob, _, _)| (*blob).to_owned())
                .collect::<Vec<_>>(),
        )
        .map_err(|e| git_error("reading the removed documents' revisions", e))?;
    let parse_config = ParseConfig::new(config);
    for ((path, blob, first_added, removal), content) in unread.into_iter().zip(contents) {
        match parse_revision(Path::new(path), content, &parse_config) {
            Some(node) => items.push(RemovedDocument {
                node,
                blob: blob.to_owned(),
                first_added: first_added.cloned(),
                removal,
            }),
            None => unparsed.push(path.to_owned()),
        }
    }
    items.sort_by_cached_key(|item| {
        let (rank, when) = match &item.removal {
            Removal::Uncommitted => (0, None),
            Removal::Committed { commit } => (1, Some(Reverse(commit.committed))),
            Removal::Unattributed => (2, None),
        };
        (rank, when, item.node.path.clone(), item.node.id.clone())
    });
    unparsed.sort();
    unparsed.dedup();

    let present = subject.map(|subject| match subject {
        HistorySubject::Id(id) => built.graph.node(id).is_some(),
        HistorySubject::Path(path) => worked.contains_key(path),
    });
    let mut warnings = built.warnings;
    warnings.extend(kept.warnings());
    Ok(HistoryOutcome {
        report: HistoryReport {
            total: items.len(),
            items,
            present,
            unparsed,
        },
        warnings,
    })
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
    /// What keeping the ledger ran into, for the command that stores it.
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

    /// [`Self::of`] for a command that owns the kept readings, a `check` of
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
                let kept = KeptFile::new(root, config, LEDGER_FILE, refresh);
                (
                    Some(ledger(&repository, config, &kept)?.bindings()),
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

const LEDGER_FILE: &str = "recorded.json";

/// On-disk shape version of `recorded.json`. Bump on any change to [`Ledger`].
const LEDGER_SCHEMA_VERSION: u32 = 2;

/// What history recorded of the project's documents up to `head`, as
/// `recorded.json` keeps it. The key is everything a reading of `head` depends
/// on besides the commit: what admits a path and infers an id, with the binary
/// that read them (`graph_config_hash`), and the project's prefix, which
/// bounds the walk. An empty `head` is a repository with no commit yet.
#[derive(Default, Serialize, Deserialize)]
struct Ledger {
    schema_version: u32,
    config: String,
    prefix: String,
    head: String,
    /// Each path a commit added or removed a document at.
    paths: BTreeMap<String, PathRecord>,
    /// Each document `head` holds, by path.
    held: BTreeMap<String, HeldRevision>,
}

#[derive(Default, Serialize, Deserialize)]
struct PathRecord {
    /// Each id a revision those commits added or removed there carried.
    ids: BTreeMap<String, Carried>,
    /// Whether a commit removed a revision there that does not parse, so the
    /// id it held is unknown.
    unparsed: bool,
}

/// What history recorded of one id at one path.
#[derive(Default, Serialize, Deserialize)]
struct Carried {
    /// The earliest commit adding the path with the id, and what it added.
    first_added: Option<AddedRevision>,
    /// The latest commit removing the path with the id, and what it took.
    last_removed: Option<RemovedRevision>,
}

#[derive(Serialize, Deserialize)]
struct AddedRevision {
    commit: CommitStamp,
    blob: String,
}

#[derive(Serialize, Deserialize)]
struct RemovedRevision {
    commit: CommitStamp,
    blob: String,
    node: NodeRef,
}

#[derive(Serialize, Deserialize)]
struct HeldRevision {
    blob: String,
    /// `None` where the revision does not parse.
    id: Option<String>,
}

impl Carried {
    /// The latest commit recording the id at the path.
    fn latest(&self) -> Option<&CommitStamp> {
        self.last_removed
            .as_ref()
            .map(|removed| &removed.commit)
            .or_else(|| self.first_added.as_ref().map(|added| &added.commit))
    }
}

impl Ledger {
    /// A path binds to the document recorded there last: [`Bindings::of`]
    /// keeps the id offered last, so each path's ids go in the order history
    /// recorded them, and the one `HEAD` holds after all of them.
    fn bindings(&self) -> Bindings {
        let recorded = self.paths.iter().flat_map(|(path, record)| {
            let mut ids: Vec<(&String, &Carried)> = record.ids.iter().collect();
            ids.sort_by_key(|&(_, carried)| carried.latest().map(chronological));
            ids.into_iter().map(move |(id, _)| (path, id))
        });
        let held = self
            .held
            .iter()
            .filter_map(|(path, held)| held.id.as_ref().map(|id| (path, id)));
        Bindings::of(
            recorded
                .chain(held)
                .map(|(path, id)| (Path::new(path.as_str()), id.as_str())),
        )
    }
}

/// Order commits by when git records them, the id settling a tie, so a ledger
/// completed from a kept one picks the commits a whole walk picks.
fn chronological(commit: &CommitStamp) -> (chrono::DateTime<chrono::Utc>, &str) {
    (commit.committed, commit.id.as_str())
}

/// The ledger of `HEAD`, read as a build would read each revision now: at each
/// commit adding or removing a document, and at `HEAD`. An id a document
/// carried only between those revisions, changed in place, is not in it.
///
/// A kept ledger the current `HEAD` reaches is completed by the commits since;
/// `HEAD`'s documents are listed again, and only a revision the kept ledger
/// did not hold is parsed.
fn ledger(repository: &Repository, config: &Config, kept: &KeptFile) -> Result<Ledger> {
    let Some(head) = repository
        .head()
        .map_err(|e| git_error("resolving HEAD", e))?
    else {
        return Ok(Ledger::default());
    };
    let key = crate::builder::graph_config_hash(config);
    let prefix = repository.prefix().to_str();
    let prior = prefix.and_then(|prefix| {
        kept.load::<Ledger>(LEDGER_SCHEMA_VERSION)
            .filter(|ledger| ledger.config == key && ledger.prefix == prefix)
    });
    let mut ledger = Ledger {
        schema_version: LEDGER_SCHEMA_VERSION,
        config: key,
        prefix: prefix.unwrap_or_default().to_owned(),
        head: head.clone(),
        ..Ledger::default()
    };
    let range = match prior {
        Some(prior) if prior.head == head => return Ok(prior),
        Some(prior) if matches!(repository.is_ancestor(&prior.head, &head), Ok(true)) => {
            ledger.paths = prior.paths;
            ledger.held = prior.held;
            format!("{}..{head}", prior.head)
        }
        _ => head.clone(),
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
                .filter(|(path, blob)| ledger.held.get(path).is_none_or(|held| held.blob != *blob))
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
    let parsed: BTreeMap<(String, String), Option<NodeRef>> = revisions
        .into_iter()
        .zip(contents)
        .map(|((path, blob), content)| {
            let node = parse_revision(Path::new(&path), content, &parse_config);
            ((path, blob), node)
        })
        .collect();

    for event in walked {
        let path = crate::path_guard::forward_string(&event.path);
        let node = parsed
            .get(&(path.clone(), event.blob.clone()))
            .cloned()
            .flatten();
        let Some(node) = node else {
            if event.kind == FileEventKind::Removed {
                ledger.paths.entry(path).or_default().unparsed = true;
            }
            continue;
        };
        let carried = ledger
            .paths
            .entry(path)
            .or_default()
            .ids
            .entry(node.id.clone())
            .or_default();
        match event.kind {
            FileEventKind::Added => {
                if carried
                    .first_added
                    .as_ref()
                    .is_none_or(|first| chronological(&event.commit) < chronological(&first.commit))
                {
                    carried.first_added = Some(AddedRevision {
                        commit: event.commit,
                        blob: event.blob,
                    });
                }
            }
            FileEventKind::Removed => {
                if carried
                    .last_removed
                    .as_ref()
                    .is_none_or(|last| chronological(&event.commit) > chronological(&last.commit))
                {
                    carried.last_removed = Some(RemovedRevision {
                        commit: event.commit,
                        blob: event.blob,
                        node,
                    });
                }
            }
        }
    }
    let mut prior_held = std::mem::take(&mut ledger.held);
    ledger.held = listed
        .into_iter()
        .map(|(path, blob)| {
            let id = match prior_held.remove(&path) {
                Some(held) if held.blob == blob => held.id,
                _ => parsed
                    .get(&(path.clone(), blob.clone()))
                    .cloned()
                    .flatten()
                    .map(|node| node.id),
            };
            (path, HeldRevision { blob, id })
        })
        .collect();
    kept.store(|| {
        prefix
            .map(|_| &ledger)
            .ok_or("the project's path inside the repository is not UTF-8")
    });
    Ok(ledger)
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
fn parse_revision(
    path: &Path,
    content: Vec<u8>,
    parse_config: &ParseConfig<'_>,
) -> Option<NodeRef> {
    let content = String::from_utf8(content).ok()?;
    parse_document(path, &content, parse_config)
        .ok()
        .map(|parsed| NodeRef::from_node(&parsed.node))
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
/// between them. Only an owner — a `check` of the working tree or the index,
/// and `nodex history` for the ledger it lists from — stores a reading and
/// reports what kept it from reading or storing one; every other command
/// consults the file and writes nothing, so `check --content` stays the
/// read-only gate it is. A file of another shape
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
            source_statuses: Vec::new(),
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

    /// The days an id was first added and last removed at a path, and the id
    /// the path binds to, as the ledger the kept one completes records them.
    fn recorded_at(
        root: &Path,
        config: &Config,
        path: &str,
        id: &str,
    ) -> (Option<NaiveDate>, Option<NaiveDate>, String) {
        let repository = Repository::discover(root).unwrap().unwrap();
        let kept = KeptFile::new(root, config, LEDGER_FILE, false);
        let ledger = ledger(&repository, config, &kept).unwrap();
        let day =
            |commit: &CommitStamp| commit.committed.with_timezone(&chrono::Local).date_naive();
        let carried = &ledger.paths[path].ids[id];
        (
            carried.first_added.as_ref().map(|added| day(&added.commit)),
            carried
                .last_removed
                .as_ref()
                .map(|removed| day(&removed.commit)),
            ledger.bindings().id_at(path).unwrap().to_owned(),
        )
    }

    /// A kept ledger `HEAD` reaches is completed by the commits since — the
    /// id planted in it alone is still bound — and otherwise records what a
    /// whole walk records: an id a removed document carried, the one a
    /// document carried when added, and the one `HEAD` holds it under. An id
    /// added and removed at a path on both sides of the kept head keeps its
    /// earliest addition and latest removal, and a path two documents held in
    /// turn binds to the later one, either way. A ledger taken under another
    /// scope is passed over.
    #[test]
    fn a_kept_record_the_head_reaches_is_completed_by_the_commits_since() {
        let dir = history_repo();
        let root = dir.path();
        let config = reading();
        let kept = root.join(&config.output.dir).join(LEDGER_FILE);
        commit_on(
            root,
            day(1),
            9,
            &[
                ("docs/a.md", &doc("first")),
                ("docs/c.md", &doc("cycled")),
                ("docs/d.md", &doc("zeta")),
            ],
        );
        run_git(root, &["rm", "-q", "docs/c.md", "docs/d.md"]);
        commit_on(root, day(2), 9, &[]);
        ProjectHistory::refreshing(&config, root).unwrap();
        let mut record: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&kept).unwrap()).unwrap();
        record["paths"]["docs/ghost.md"] =
            serde_json::json!({"ids": {"ghost": {}}, "unparsed": false});
        std::fs::write(&kept, record.to_string()).unwrap();
        commit_on(
            root,
            day(3),
            9,
            &[
                ("docs/b.md", &doc("passing")),
                ("docs/c.md", &doc("cycled")),
                ("docs/d.md", &doc("alpha")),
            ],
        );
        run_git(root, &["rm", "-q", "docs/b.md", "docs/c.md", "docs/d.md"]);
        commit_on(root, day(4), 9, &[]);
        commit_on(root, day(5), 9, &[("docs/a.md", &doc("renamed"))]);
        let recorded = |root: &Path| {
            [
                recorded_at(root, &config, "docs/c.md", "cycled"),
                recorded_at(root, &config, "docs/d.md", "zeta"),
                recorded_at(root, &config, "docs/d.md", "alpha"),
            ]
        };
        let expected = [
            (Some(day(1)), Some(day(4)), "cycled".to_owned()),
            (Some(day(1)), Some(day(2)), "alpha".to_owned()),
            (Some(day(3)), Some(day(4)), "alpha".to_owned()),
        ];

        let completed = ProjectHistory::of(&config, root).unwrap();
        assert!(binds(&completed, "ghost"), "the kept record was consulted");
        for id in ["first", "passing", "renamed", "zeta", "alpha"] {
            assert!(binds(&completed, id), "{id}");
        }
        assert!(!binds(&completed, "never"));
        assert_eq!(recorded(root), expected);

        let mut rescoped = reading();
        rescoped.scope.include.push("notes/**/*.md".into());
        assert!(!binds(
            &ProjectHistory::of(&rescoped, root).unwrap(),
            "ghost"
        ));
        std::fs::remove_file(&kept).unwrap();
        let walked = ProjectHistory::of(&config, root).unwrap();
        for id in ["first", "passing", "renamed", "zeta", "alpha"] {
            assert!(binds(&walked, id), "{id}");
        }
        assert_eq!(recorded(root), expected);
    }
}
