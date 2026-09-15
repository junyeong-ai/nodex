//! Where each record stood at each step of the history a run judges.
//!
//! A step is one snapshot and the snapshots it was made on top of: a commit
//! and its parents, or the uncommitted change and the commits it will be
//! committed onto. A rule that judges how records *move* reads steps rather
//! than the endpoint diff, because an endpoint diff folds a sequence into one
//! move: a record authored at its entry status and accepted in the next commit
//! reads, across both, as a record that arrived accepted. Asked one step at a
//! time, a range answers exactly what each of its commits answers alone, so
//! the verdict does not depend on where the range starts, and a gate run on
//! every commit agrees with one run over all of them.
//!
//! Only positions are kept — kind, status and path per record — so a walk
//! over many commits holds one small map per distinct snapshot rather than a
//! graph per commit, and a snapshot two steps share is shared.
//!
//! A document a commit could not parse still stands for a record: it holds
//! the one it last held, read from the commit before the change that broke
//! it ([`Positions::recovering`]). Without that, a record broken in one commit
//! and repaired in the next reads as arriving at the repair, and one authored
//! straight into acceptance through a broken commit reads as a record nothing
//! can judge. A shallow clone can hold neither answer — what the path held
//! before may lie beyond its cut — and a step whose parents carry such a path
//! says so ([`Step::priors_known`]) rather than reading "created here".

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::model::Graph;

/// Where one record stands in one snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Position {
    pub kind: String,
    pub status: String,
    pub path: String,
}

/// Every record's position in one snapshot, by id, and the paths whose record
/// it could not read.
#[derive(Debug, Clone, Default)]
pub struct Positions {
    records: BTreeMap<String, Position>,
    unreadable: BTreeSet<String>,
}

impl Positions {
    pub fn of(graph: &Graph) -> Self {
        Self {
            records: graph
                .nodes()
                .values()
                .map(|node| {
                    (
                        node.id.clone(),
                        Position {
                            kind: node.kind.to_string(),
                            status: node.status.to_string(),
                            path: crate::path_guard::forward_string(&node.path),
                        },
                    )
                })
                .collect(),
            unreadable: graph
                .parse_failures()
                .iter()
                .map(|failure| failure.path.clone())
                .collect(),
        }
    }

    pub fn get(&self, id: &str) -> Option<&Position> {
        self.records.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Position)> {
        self.records
            .iter()
            .map(|(id, position)| (id.as_str(), position))
    }

    /// The paths this snapshot holds a document at whose record is unknown:
    /// one it could not parse, less what has been read back from before it
    /// broke.
    pub fn unreadable(&self) -> impl Iterator<Item = &str> {
        self.unreadable.iter().map(String::as_str)
    }

    /// The records this snapshot holds at `path`.
    pub fn at_path<'a>(&'a self, path: &'a str) -> impl Iterator<Item = (&'a str, &'a Position)> {
        self.iter()
            .filter(move |(_, position)| position.path == path)
    }

    /// This snapshot with `records` read into it: what the documents it could
    /// not read held before they broke. A record it already holds by id keeps
    /// its own position. `unknown` is what reading back could not answer —
    /// the paths whose earlier state lies beyond a shallow clone's cut.
    pub fn recovering(
        &self,
        records: impl IntoIterator<Item = (String, Position)>,
        unknown: BTreeSet<String>,
    ) -> Self {
        let mut recovered = self.clone();
        for (id, position) in records {
            recovered.records.entry(id).or_insert(position);
        }
        recovered.unreadable = unknown;
        recovered
    }
}

/// One snapshot and the snapshots it was made on top of.
#[derive(Debug, Clone)]
pub struct Step {
    /// The commit that took this step; `None` for the uncommitted change.
    pub commit: Option<String>,
    pub parents: Vec<Arc<Positions>>,
    pub child: Arc<Positions>,
}

impl Step {
    /// Whether every record this step's parents held is known. Where a parent
    /// could not parse a document and what it held lies beyond a shallow
    /// clone's cut, a record with no prior may be the one that stood there,
    /// so how it arrived cannot be told.
    pub fn priors_known(&self) -> bool {
        self.parents
            .iter()
            .all(|parent| parent.unreadable().next().is_none())
    }

    /// The position `id` held on each parent that holds it. A merge has one
    /// per line of history that carried the record, and what the merge
    /// introduced is only what differs from every one of them — a side taken
    /// whole was that side's own commits' doing.
    pub fn priors<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a Position> {
        self.parents.iter().filter_map(move |parent| parent.get(id))
    }
}

/// The committed steps a run judges, and the commits the uncommitted change
/// descends from — `HEAD`, and every `MERGE_HEAD` while a merge is under way,
/// because those are the parents the next commit will record.
#[derive(Debug, Clone)]
pub struct Ancestry {
    committed: Vec<Step>,
    heads: Vec<Arc<Positions>>,
    /// What git ignores under the project ([`crate::git::Repository::ignored`]):
    /// a document there is never part of the change a commit records, so it
    /// takes no step at all.
    ignored: Vec<String>,
}

impl Ancestry {
    pub fn new(committed: Vec<Step>, heads: Vec<Arc<Positions>>, ignored: Vec<String>) -> Self {
        Self {
            committed,
            heads,
            ignored,
        }
    }

    /// The position `id` holds on each head that holds it — the priors of
    /// the step a write to it would commit.
    pub fn head_priors<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a Position> {
        self.heads.iter().filter_map(move |head| head.get(id))
    }

    /// Every step that ends at `graph`: the committed ones, then the
    /// uncommitted change that brings the heads to it — which holds no
    /// document git ignores.
    pub fn through(&self, graph: &Graph) -> Vec<Step> {
        let mut uncommitted = Positions::of(graph);
        uncommitted
            .records
            .retain(|_, position| !self.ignores(&position.path));
        self.committed
            .iter()
            .cloned()
            .chain(std::iter::once(Step {
                commit: None,
                parents: self.heads.clone(),
                child: Arc::new(uncommitted),
            }))
            .collect()
    }

    /// Whether git ignores the document at `path`, so no commit can hold it.
    pub fn ignores(&self, path: &str) -> bool {
        self.ignored
            .iter()
            .any(|entry| match entry.strip_suffix('/') {
                Some(directory) => {
                    directory.is_empty()
                        || path
                            .strip_prefix(directory)
                            .is_some_and(|rest| rest.starts_with('/'))
                }
                None => entry == path,
            })
    }
}
