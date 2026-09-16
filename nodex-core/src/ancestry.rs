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
//! says so ([`Priors::known`]) rather than reading "created here".

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::model::Graph;

/// Where one record stands in one snapshot. Ordered, so a record read back
/// through lines that disagree holds its positions in an order its content
/// decides rather than the order git recorded a merge's parents in.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    pub kind: String,
    pub status: String,
    pub path: String,
}

/// Every record's position in one snapshot, by id, and the paths whose record
/// it could not read.
///
/// A snapshot reads one position per record. A read-back one can hold more
/// than one where the lines behind it disagree about what stood at a document
/// it could not parse: each is a position the record may have held there, and
/// each is a prior of the step that follows, the way a merge's parents each
/// carry one.
#[derive(Debug, Clone, Default)]
pub struct Positions {
    records: BTreeMap<String, Vec<Position>>,
    unreadable: BTreeSet<String>,
    /// Whether anything at all could be read here. False for a commit whose
    /// tree the build refuses under today's config, which holds records this
    /// walk cannot name — as against a commit that carries no project, which
    /// holds none.
    read: bool,
}

impl Positions {
    /// A commit whose tree could not be graphed: it stands for records this
    /// walk cannot name, so a step made on it knows none of its priors.
    pub fn unread() -> Self {
        Self::default()
    }

    /// A commit that carries no project holds no record — which is knowable,
    /// unlike [`Positions::unread`].
    pub fn empty() -> Self {
        Self {
            read: true,
            ..Self::default()
        }
    }

    pub fn of(graph: &Graph) -> Self {
        Self {
            read: true,
            records: graph
                .nodes()
                .values()
                .map(|node| {
                    (
                        node.id.clone(),
                        vec![Position {
                            kind: node.kind.to_string(),
                            status: node.status.to_string(),
                            path: crate::path_guard::forward_string(&node.path),
                        }],
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

    /// Every position `id` may have held here — one, but for a record read
    /// back through lines that disagree.
    pub fn at(&self, id: &str) -> &[Position] {
        self.records.get(id).map_or(&[][..], Vec::as_slice)
    }

    /// Each record and where it stood, taking the first of the positions a
    /// read-back record may have held. What a step judges of its own child is
    /// judged against parents that carry every one of them, so a disagreement
    /// the walk could not settle cannot decide a verdict here.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Position)> {
        self.records
            .iter()
            .filter_map(|(id, positions)| Some((id.as_str(), positions.first()?)))
    }

    /// Whether anything could be read here at all.
    pub fn readable(&self) -> bool {
        self.read
    }

    /// The records this snapshot holds more than one position for: read back
    /// through lines that disagreed, so which of them it stood at is unknown.
    pub fn ambiguous(&self) -> impl Iterator<Item = (&str, &[Position])> {
        self.records
            .iter()
            .filter(|(_, positions)| positions.len() > 1)
            .map(|(id, positions)| (id.as_str(), positions.as_slice()))
    }

    /// The paths this snapshot holds a document at whose record is unknown:
    /// one it could not parse, less what has been read back from before it
    /// broke.
    pub fn unreadable(&self) -> impl Iterator<Item = &str> {
        self.unreadable.iter().map(String::as_str)
    }

    /// Every record this snapshot holds at `path`, each with every position
    /// it may have held.
    pub fn at_path<'a>(&'a self, path: &'a str) -> impl Iterator<Item = (&'a str, &'a Position)> {
        self.records
            .iter()
            .flat_map(|(id, positions)| positions.iter().map(move |p| (id.as_str(), p)))
            .filter(move |(_, position)| position.path == path)
    }

    /// This snapshot with `records` read into it: what the documents it could
    /// not read held before they broke. A record it already holds by id keeps
    /// its own position, and one read back through lines that disagree keeps
    /// every distinct position they gave — nothing here picks between them,
    /// because the order they arrived in is the order git recorded a merge's
    /// parents in. `unknown` is what reading back could not answer: the paths
    /// whose earlier state lies beyond a shallow clone's cut.
    pub fn recovering(
        &self,
        records: impl IntoIterator<Item = (String, Position)>,
        unknown: BTreeSet<String>,
    ) -> Self {
        let mut recovered = self.clone();
        for (id, position) in records {
            if self.records.contains_key(&id) {
                continue;
            }
            let held = recovered.records.entry(id).or_default();
            if !held.contains(&position) {
                held.push(position);
                held.sort();
            }
        }
        recovered.unreadable = unknown;
        recovered
    }
}

/// What the lines behind a step last agreed on, which is what says which of
/// them moved a record and which only carried it.
#[derive(Debug, Clone, Default)]
pub enum Lines {
    /// One line, or lines holding every record alike: each carries what the
    /// step was made on, and there is nothing a base could settle.
    #[default]
    Agreeing,
    /// Where they last agreed. Several where the lines have merged each other
    /// before and none of those stands above the rest.
    Agreed(Vec<Arc<Positions>>),
    /// The lines share no commit — an `--allow-unrelated-histories` merge —
    /// so which of them moved a record they disagree about cannot be told,
    /// and neither can what the step was made on.
    Unrelated,
}

/// What a step was made on, for one record.
#[derive(Debug, Clone)]
pub struct Priors<'a> {
    /// The positions the step may have moved from.
    positions: Vec<&'a Position>,
    /// Whether that is the whole answer. False where the walk could not read
    /// what stood behind the step: a document a shallow clone cannot read
    /// back, a commit whose tree would not graph, or lines that share no
    /// commit and disagree about this record.
    known: bool,
}

impl<'a> Priors<'a> {
    /// A narrowing of what a step was made on — the positions a rule's own
    /// scope keeps — carrying whether the walk could read the rest.
    pub fn of(positions: Vec<&'a Position>, known: bool) -> Self {
        Self { positions, known }
    }

    pub fn positions(&self) -> impl Iterator<Item = &'a Position> {
        self.positions.clone().into_iter()
    }

    pub fn known(&self) -> bool {
        self.known
    }
}

/// What a step made on `carriers` was made on, for one record: the position
/// each line that moved it since they last agreed left it at, and where no
/// line moved it, what the lines they came from still carry.
fn claimed<'a>(carriers: &'a [Arc<Positions>], lines: &'a Lines, id: &str) -> Priors<'a> {
    let carried: Vec<&'a Position> = carriers.iter().flat_map(|line| line.at(id)).collect();
    let unread = carried.is_empty()
        && carriers
            .iter()
            .any(|line| !line.readable() || line.unreadable().next().is_some());
    match lines {
        Lines::Agreeing => Priors {
            positions: carried,
            known: !unread,
        },
        Lines::Unrelated => {
            let agreeing = carriers
                .windows(2)
                .all(|pair| pair[0].at(id) == pair[1].at(id));
            Priors {
                known: agreeing && !unread,
                positions: carried,
            }
        }
        Lines::Agreed(bases) => {
            // Where no line moved it, the lines still carry it and that is
            // what the step stands on — never the places they agreed, which
            // can hold a reading no line kept: an older status the lines both
            // walked away from, or a record every line since deleted.
            let held: Vec<&'a Position> = bases.iter().flat_map(|base| base.at(id)).collect();
            let moved: Vec<&'a Position> = carried
                .iter()
                .copied()
                .filter(|position| !held.contains(position))
                .collect();
            Priors {
                positions: match moved.is_empty() {
                    true => carried,
                    false => moved,
                },
                known: !unread,
            }
        }
    }
}

/// One snapshot and the snapshots it was made on top of.
#[derive(Debug, Clone)]
pub struct Step {
    /// The commit that took this step; `None` for the uncommitted change.
    pub commit: Option<String>,
    pub parents: Vec<Arc<Positions>>,
    pub child: Arc<Positions>,
    /// What the lines this step was made on last agreed on.
    pub lines: Lines,
}

impl Step {
    /// The positions this step was made on: what each line that moved the
    /// record since the parents last agreed left it at, and where none moved
    /// it, what they all still carry.
    ///
    /// A line that did not touch the record makes no claim about it, exactly
    /// as it makes no claim about a file it did not edit. Without that, a
    /// branch forked before a record was superseded carries the old status
    /// back as a position the merge may move from, and a terminal record is
    /// resurrected by merging any line old enough to predate it.
    pub fn priors<'a>(&'a self, id: &'a str) -> Priors<'a> {
        claimed(&self.parents, &self.lines, id)
    }
}

/// The committed steps a run judges, and the commits the uncommitted change
/// descends from — `HEAD`, and every `MERGE_HEAD` while a merge is under way,
/// because those are the parents the next commit will record.
#[derive(Debug, Clone)]
pub struct Ancestry {
    committed: Vec<Step>,
    heads: Vec<Arc<Positions>>,
    /// What the heads last agreed on, while a merge is under way.
    head_lines: Lines,
    /// What this walk could not read, for the envelope: a commit whose tree
    /// the build refuses names itself here, because the records around it are
    /// counted rather than judged and a count alone does not say why.
    warnings: Vec<crate::Warning>,
    /// What git ignores under the project ([`crate::git::Repository::ignored`]):
    /// a document there is never part of the change a commit records, so it
    /// takes no step at all.
    ignored: Vec<String>,
}

impl Ancestry {
    pub fn new(
        committed: Vec<Step>,
        heads: Vec<Arc<Positions>>,
        head_lines: Lines,
        ignored: Vec<String>,
        warnings: Vec<crate::Warning>,
    ) -> Self {
        Self {
            committed,
            heads,
            head_lines,
            ignored,
            warnings,
        }
    }

    /// What this walk could not read.
    pub fn warnings(&self) -> &[crate::Warning] {
        &self.warnings
    }

    /// The position `id` holds on each head that holds it — the priors of
    /// the step a write to it would commit.
    pub fn head_priors<'a>(&'a self, id: &'a str) -> Priors<'a> {
        claimed(&self.heads, &self.head_lines, id)
    }

    /// Every step that ends at `graph`: the committed ones, then the
    /// uncommitted change that brings the heads to it — which holds no
    /// document git ignores.
    pub fn through(&self, graph: &Graph) -> Vec<Step> {
        let mut uncommitted = Positions::of(graph);
        uncommitted
            .records
            .retain(|_, positions| positions.iter().all(|p| !self.ignores(&p.path)));
        self.committed
            .iter()
            .cloned()
            .chain(std::iter::once(self.uncommitted(Arc::new(uncommitted))))
            .collect()
    }

    /// The step the heads would commit, ending at `child`.
    fn uncommitted(&self, child: Arc<Positions>) -> Step {
        Step {
            commit: None,
            parents: self.heads.clone(),
            child,
            lines: self.head_lines.clone(),
        }
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
