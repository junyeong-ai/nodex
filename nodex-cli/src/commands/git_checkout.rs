//! Checking the project's git history out to read it. `diff` and `impact`
//! read both refs; `check` (under `--since` or `rules.immutable_baseline`),
//! `query issues` and every document-writing command (`write_baseline`) read
//! the baseline; and the commits the step rules judge are read here too.
//!
//! Every tree is written into a [`Checkout`] and graphed through
//! `nodex_core::builder::build_of_ref`, which reads and writes no cache and
//! keeps its scan to the checkout. The operator's work tree is never
//! touched. A checkout carries the whole repository, so what is graphed is
//! the project's own location inside it, which is the checkout root only
//! when the project *is* the repository top level. This module owns the
//! checkouts only; the repository binding they are written from lives in
//! `nodex_core::git`, and the `rules.immutable_baseline` resolution behind
//! [`baseline_diff`] lives in `nodex_core::BaselineProbe`, shared with the
//! write seams it locks.

use anyhow::{Context, Result};
use nodex_core::builder::BuildOutcome;
use nodex_core::{
    Ancestry, BaselineProbe, Before, GraphedBaseline, Lines, Position, Positions, RefState,
    Repository, Step, Warning, WarningCode,
};
use std::collections::{BTreeSet, HashMap};
use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nodex_core::error::Error as CoreError;

/// The repository the project at `root` is tracked in. Surfaces
/// `GIT_ERROR` when git is unavailable or the project is not in a work
/// tree — the JSON envelope therefore distinguishes "operator is in the
/// wrong place" from a generic runtime failure (and from a `nodex.toml`
/// validation problem, which uses `CONFIG_ERROR`).
pub fn ensure_repository(root: &Path, who: &str) -> Result<Repository> {
    match Repository::discover(root) {
        Ok(Some(repository)) => Ok(repository),
        Ok(None) => Err(CoreError::Git {
            context: format!("{who} requires a git work tree at the project root"),
            stderr: format!("no git work tree was found for {}", root.display()),
        }
        .into()),
        Err(e) => Err(CoreError::Git {
            context: format!("{who} could not resolve the repository for the project"),
            stderr: e.to_string(),
        }
        .into()),
    }
}

/// The project a read judges against its history: the graph built of it,
/// and where on disk the files that graph was built from are — the working
/// tree, or the checkout of the index `check --staged` holds.
#[derive(Clone, Copy)]
pub struct Current<'a> {
    pub graph: &'a nodex_core::Graph,
    pub files: &'a Path,
}

/// Build the graph at `git_ref` (content only — the config of the project
/// being judged stays the single lens) in a [`Checkout`] and diff it
/// against the already-built `current` graph: the seam behind `check
/// --since`. [`baseline_diff`] resolves `rules.immutable_baseline` for a
/// plain `check` and `query issues`, and both read the ref through one
/// implementation, so their violation sets can never diverge.
///
/// `repository` decides what git measures and where the project sits
/// inside a checkout. Returns [`BaselineResolution::Inert`] — never
/// `NotApplicable` — when the ref does not carry the project at all, which
/// is an ordinary state for a subdirectory project introduced after the
/// ref.
pub fn diff_against_ref(
    repository: &Repository,
    git_ref: &str,
    config: &nodex_core::Config,
    current: Current<'_>,
) -> Result<Prior> {
    prior(repository, git_ref, config, current, Steps::Range)
}

/// The baseline at `git_ref` and the history `steps` reaches, read through
/// one checkout where the ref carries the project.
fn prior(
    repository: &Repository,
    git_ref: &str,
    config: &nodex_core::Config,
    current: Current<'_>,
    steps: Steps,
) -> Result<Prior> {
    let since = match steps {
        Steps::Uncommitted => None,
        Steps::Range => Some(git_ref),
    };
    let (baseline, ancestry) =
        match baseline_graph(current.files, repository, git_ref, config, steps)? {
            BaselineSnapshot::Absent { warning } => (
                BaselineResolution::Inert { warning },
                history(repository, config, since)?,
            ),
            BaselineSnapshot::Graphed(baseline) => (
                BaselineResolution::Resolved(Box::new(BaselineDiff {
                    diff: nodex_core::diff::compute_diff(&baseline.graph, current.graph),
                    warnings: baseline.warnings,
                })),
                baseline.ancestry,
            ),
        };
    Ok(Prior {
        baseline,
        unread: ancestry
            .as_ref()
            .map(|ancestry| ancestry.warnings().to_vec())
            .unwrap_or_default(),
        steps: ancestry.map(|ancestry| ancestry.through(current.graph)),
    })
}

/// What a read judges against: the baseline the locks read, and the history
/// the rules that judge steps read, which is git's and not the baseline's.
pub struct Prior {
    pub baseline: BaselineResolution,
    /// Every step ending at the graph judged, where a registered rule judges
    /// steps and the project is in a git work tree.
    pub steps: Option<Vec<Step>>,
    /// What reading that history could not read — a commit whose tree the
    /// build refuses. The records its step carried are counted rather than
    /// judged, and a count alone does not say why.
    pub unread: Vec<Warning>,
}

/// What a ref turned out to hold for the project.
pub enum BaselineSnapshot {
    /// The ref does not carry the project. Carries the advisory naming which
    /// condition it was, constructed where the ref state is known.
    Absent { warning: Warning },
    /// The project as that ref holds it, plus the build's own warnings.
    /// Boxed so the enum's footprint is not dominated by the graph-sized
    /// variant the absent state never carries — the same reason
    /// [`BaselineResolution`] boxes its diff.
    Graphed(Box<GraphedBaseline>),
}

/// How much history a read takes along, for the rules that judge it a step
/// at a time. Read only where a registered rule does
/// (`Config::judges_steps`).
#[derive(Debug, Clone, Copy)]
pub enum Steps {
    /// What the uncommitted change is made on, and nothing earlier: what a
    /// plain `check`, `query issues` and every write seam judge. A step
    /// finding is about a commit, so re-judging every commit since a
    /// configured baseline on each run would keep a finding nothing short of
    /// rewriting history clears.
    Uncommitted,
    /// Every commit the heads reach and the ref does not, as well — what
    /// `check --since <ref>` asks about.
    Range,
}

/// The project graphed at `git_ref`, plus everything about that build a
/// caller must surface. `None` when the ref does not carry the project at
/// all — an ordinary state for a subdirectory project introduced after the
/// ref, which the caller reports as an advisory or a refusal depending on
/// whether the ref was configured or named.
///
/// The one definition of "the baseline" for both planes: the read side diffs
/// this graph, and a write seam's [`nodex_core::BaselineProbe`] judges its
/// locks against it. Neither can hold a different baseline than the other,
/// because there is only this one to hold.
///
/// The config of the project being judged stays the single lens — a diff is
/// a question asked from the newer contract, and the ref supplies content
/// only. `files` is where that project is on disk.
pub fn baseline_graph(
    files: &Path,
    repository: &Repository,
    git_ref: &str,
    config: &nodex_core::Config,
    steps: Steps,
) -> Result<BaselineSnapshot> {
    let tree = match recorded(repository, git_ref)? {
        Recorded::Tree(tree) => tree,
        Recorded::Absent(detail) => {
            return Ok(BaselineSnapshot::Absent {
                warning: Warning::new(
                    WarningCode::BaselineInert,
                    format!("baseline {git_ref}: {detail} — diff-aware rules are inert this run"),
                ),
            });
        }
    };
    let checkout = Checkout::acquire(repository)?;
    let before_result = checkout.graph(&tree, config)?;
    // An entry the walk classified by type and could place in neither class —
    // a socket, a symlink resolving to nothing, a boundary it declined to
    // cross — may never have been a document at all, and an advisory about it
    // would assert one existed. The project being judged is asked, at or
    // under the path because such a channel can name a directory the walk
    // reaches documents through, and a document standing there is what
    // establishes there was one to guard.
    let current: Vec<String> = nodex_core::builder::scanner::scan_scope(files, config)?
        .paths
        .iter()
        .map(|p| nodex_core::path_guard::forward_string(p))
        .collect();
    let holds_a_document = |path: &str| {
        current
            .iter()
            .any(|doc| doc == path || doc.starts_with(&format!("{path}/")))
    };
    // Surface the baseline build's own advisories, ref-tagged, and beside them
    // every path the ref held that this build could not read. Those are what
    // the baseline is missing through no decision of the project's: the
    // document vanishes from the before graph, so it reads as added and the
    // diff-aware rules have nothing to judge it against.
    //
    // Scope is not one of them. `scope.conditional_exclude` is evaluated from
    // the same config on both sides, so a path it drops is one the project
    // says is not its own — at the ref exactly as here, and permanently, since
    // the config outlives any run. Where the two sides disagree it is because
    // their content does, and what that produces is a record the current graph
    // carries and the baseline does not: an absence the rules answer for
    // themselves, in the population they report guarding, rather than one the
    // ref build could guess at from a path.
    let warnings: Vec<Warning> = before_result
        .warnings
        .into_iter()
        .map(|w| Warning::new(w.code, format!("baseline {git_ref}: {}", w.message)))
        .chain(before_result.graph.parse_failures().iter().map(|f| {
            Warning::new(
                WarningCode::BaselineInert,
                format!(
                    "baseline {git_ref}: {} — the document has no baseline node, so diff-aware \
                     rules are inert for it",
                    f.message
                ),
            )
        }))
        .chain(
            before_result
                .dangling_paths
                .iter()
                .filter(|path| holds_a_document(path))
                .map(|path| {
                    Warning::new(
                        WarningCode::BaselineInert,
                        format!(
                            "baseline {git_ref}: {path} holds no readable document there (a \
                             symlink whose target the ref does not carry, or an entry that is \
                             not a file) — the document has no baseline node, so diff-aware \
                             rules are inert for it"
                        ),
                    )
                }),
        )
        .chain(before_result.escaping_paths.iter().filter(|path| holds_a_document(path)).map(|path| {
            Warning::new(
                WarningCode::BaselineInert,
                format!(
                    "baseline {git_ref}: {path} resolves outside the checkout, so the ref does \
                     not record what it holds — it was not read, and diff-aware rules are \
                     inert for it"
                ),
            )
        }))
        .chain(before_result.unfollowed_paths.iter().filter(|path| holds_a_document(path)).map(|path| {
            Warning::new(
                WarningCode::BaselineInert,
                format!(
                    "baseline {git_ref}: {path} is a directory symlink there and \
                     scope.follow_symlinks is off, so nothing below it was read — the document \
                     has no baseline node, so diff-aware rules are inert for it"
                ),
            )
        }))
        .collect();
    let ancestry = match config.judges_steps() {
        true => {
            let mut snapshots = Snapshots::new(repository, config, Some(checkout));
            snapshots
                .graphed
                .insert(tree, Arc::new(Positions::of(&before_result.graph)));
            Some(snapshots.ancestry(match steps {
                Steps::Uncommitted => None,
                Steps::Range => Some(git_ref),
            })?)
        }
        false => None,
    };
    Ok(BaselineSnapshot::Graphed(Box::new(GraphedBaseline {
        graph: before_result.graph,
        ancestry,
        warnings,
    })))
}

/// Where each record stood at every step from the heads back to `since` —
/// only the heads when `since` is `None` — read by checking each commit out
/// in turn in one [`Checkout`] and graphing it under the working tree's
/// config. `None` where no registered rule judges steps.
pub fn history(
    repository: &Repository,
    config: &nodex_core::Config,
    since: Option<&str>,
) -> Result<Option<Ancestry>> {
    if !config.judges_steps() {
        return Ok(None);
    }
    Ok(Some(
        Snapshots::new(repository, config, None).ancestry(since)?,
    ))
}

/// [`history`] for the uncommitted change alone, for a project whose
/// repository nothing has bound yet. `None` outside a git work tree, where
/// there is no commit to step from — the rules say so as they skip.
pub fn uncommitted_history(root: &Path, config: &nodex_core::Config) -> Result<Option<Ancestry>> {
    if !config.judges_steps() {
        return Ok(None);
    }
    match Repository::discover(root) {
        Ok(Some(repository)) => history(&repository, config, None),
        Ok(None) => Ok(None),
        Err(e) => Err(CoreError::Git {
            context: "the repository whose history statuses.flow judges could not be resolved"
                .to_string(),
            stderr: e.to_string(),
        }
        .into()),
    }
}

/// Whether any record stands differently on one of these snapshots than on
/// another.
fn disagree(carried: &[Arc<Positions>]) -> bool {
    let ids: BTreeSet<&str> = carried
        .iter()
        .flat_map(|line| line.iter())
        .map(|(id, _)| id)
        .collect();
    ids.into_iter().any(|id| {
        carried
            .windows(2)
            .any(|pair| pair[0].at(id) != pair[1].at(id))
    })
}

/// The positions graphed so far on one walk: by the tree each commit records,
/// so a commit whose tree another already graphed — the baseline itself when
/// it is `HEAD`, a merge that took one side whole — costs nothing more; and by
/// commit once what its unreadable documents held is read back in, which
/// depends on the history behind it and not only on its tree.
struct Snapshots<'a> {
    repository: &'a Repository,
    config: &'a nodex_core::Config,
    /// Taken at the first commit that carries the project, and moved from
    /// commit to commit after that.
    checkout: Option<Checkout>,
    trees: HashMap<String, String>,
    graphed: HashMap<String, Arc<Positions>>,
    recovered: HashMap<String, Arc<Positions>>,
    /// What each path stands for at each commit the recovery walk reached.
    /// A document broken once and left alone is unreadable at every commit
    /// after it, and each of them stands on the same line behind the break —
    /// so answered per asking the walk costs the square of the commits it
    /// crosses, and answered per commit it costs the commits.
    stands: HashMap<(String, String), Held>,
    /// The paths whose earlier state this clone cut off, reported once each:
    /// the walk meets the same cut again from every commit standing on it.
    cut: BTreeSet<String>,
    /// The commits whose trees this walk could not graph.
    unread: Vec<Warning>,
}

/// What a path stands for at one commit, once the walk has answered it.
#[derive(Clone)]
struct Held {
    records: Vec<(String, Position)>,
    /// Whether that is the whole of what stood there, or a line ended at a
    /// shallow clone's cut before reaching a commit that could read the path.
    known: bool,
}

/// One step of the recovery walk: a commit to answer for, and the join that
/// answers for the commit its lines were reached from.
enum Visit {
    Ask(String),
    Join(String, Vec<String>, bool),
}

impl<'a> Snapshots<'a> {
    fn new(
        repository: &'a Repository,
        config: &'a nodex_core::Config,
        checkout: Option<Checkout>,
    ) -> Self {
        Self {
            repository,
            config,
            checkout,
            trees: HashMap::new(),
            graphed: HashMap::new(),
            recovered: HashMap::new(),
            stands: HashMap::new(),
            cut: BTreeSet::new(),
            unread: Vec::new(),
        }
    }

    fn ancestry(&mut self, since: Option<&str>) -> Result<Ancestry> {
        let unreadable = |e: std::io::Error| CoreError::Git {
            context: "the history statuses.flow judges could not be read".to_string(),
            stderr: e.to_string(),
        };
        let heads = self.repository.heads().map_err(unreadable)?;
        let range = match since {
            Some(since) => self.repository.range(since, &heads).map_err(unreadable)?,
            None => nodex_core::git::Range::default(),
        };
        self.trees.extend(
            range
                .added
                .iter()
                .chain(&range.boundary)
                .map(|commit| (commit.id.clone(), commit.tree.clone())),
        );
        let mut committed = Vec::with_capacity(range.added.len());
        for commit in &range.added {
            let parents: Vec<Arc<Positions>> = commit
                .parents
                .iter()
                .map(|parent| self.at(parent))
                .collect::<Result<_>>()?;
            committed.push(Step {
                commit: Some(commit.id.clone()),
                lines: self.agreed(&parents, &commit.parents)?,
                parents,
                child: self.at(&commit.id)?,
            });
        }
        let carried: Vec<Arc<Positions>> = heads
            .iter()
            .map(|head| self.at(head))
            .collect::<Result<_>>()?;
        let head_lines = self.agreed(&carried, &heads)?;
        let ignored = self.repository.ignored().map_err(unreadable)?;
        Ok(Ancestry::new(
            committed,
            carried,
            head_lines,
            ignored,
            std::mem::take(&mut self.unread),
        ))
    }

    /// What the lines behind a step last agreed on, read only where they were
    /// several and disagree about a record: what every line carries alike a
    /// base can only confirm, and reading it would cost a snapshot to learn
    /// nothing.
    fn agreed(&mut self, carried: &[Arc<Positions>], commits: &[String]) -> Result<Lines> {
        if carried.len() < 2 || !disagree(carried) {
            return Ok(Lines::Agreeing);
        }
        let agreed = self
            .repository
            .merge_base(commits)
            .map_err(|e| CoreError::Git {
                context: format!(
                    "where the lines behind {commits:?} last agreed could not be read"
                ),
                stderr: e.to_string(),
            })?;
        let bases = match agreed {
            Before::Cut => {
                let lines: Vec<&str> = commits
                    .iter()
                    .map(|commit| commit.get(..12).unwrap_or(commit))
                    .collect();
                self.unread.push(Warning {
                    code: WarningCode::HistoryUnread,
                    message: format!(
                        "this shallow clone holds no commit behind both {}, so the records \
                         they disagree about are counted rather than judged: fetch the \
                         history behind them to find out whether they share one, since a \
                         clone this shallow reads a place they agreed beyond its cut exactly \
                         as it reads lines that never met",
                        lines.join(" and ")
                    ),
                });
                return Ok(Lines::Cut);
            }
            Before::Commits(bases) => bases,
        };
        if bases.is_empty() {
            return Ok(Lines::Unrelated);
        }
        Ok(Lines::Agreed(
            bases
                .iter()
                .map(|base| self.at(base))
                .collect::<Result<_>>()?,
        ))
    }

    /// Where each record stood at `commit`, a document it could not parse
    /// holding the record it held before the change that broke it — and where
    /// a shallow clone cuts that off, standing for a record this walk cannot
    /// name.
    fn at(&mut self, commit: &str) -> Result<Arc<Positions>> {
        if let Some(positions) = self.recovered.get(commit) {
            return Ok(Arc::clone(positions));
        }
        let graphed = self.graphed_at(commit)?;
        let unreadable: Vec<String> = graphed.unreadable().map(str::to_string).collect();
        let positions = match unreadable.is_empty() {
            true => graphed,
            false => {
                let mut records = Vec::new();
                let mut unknown = BTreeSet::new();
                for path in &unreadable {
                    let stood = self.stands_for(commit, path)?;
                    records.extend(stood.records);
                    if !stood.known {
                        unknown.insert(path.clone());
                    }
                }
                Arc::new(graphed.recovering(records, unknown))
            }
        };
        self.recovered
            .insert(commit.to_string(), Arc::clone(&positions));
        Ok(positions)
    }

    /// What `path` stands for at `commit`: what its own snapshot reads there,
    /// and where it cannot read it, what it stood for on every line behind the
    /// change that broke it. Every line is followed, because a merge's parents
    /// may each hold a record there.
    ///
    /// [`Held::known`] is false where a line ends at a shallow clone's cut
    /// instead of at a commit that could read the path: the record that stood
    /// there is beyond what this clone holds, and the records the other lines
    /// gave are still theirs.
    ///
    /// Walked with its own stack rather than by recursion — a line of history
    /// is as deep as the project is old — and every commit it reaches is
    /// answered once, its answer standing for every later commit that stands
    /// on it.
    fn stands_for(&mut self, commit: &str, path: &str) -> Result<Held> {
        let key = |at: &str| (path.to_string(), at.to_string());
        let mut walk = vec![Visit::Ask(commit.to_string())];
        while let Some(visit) = walk.pop() {
            match visit {
                Visit::Ask(at) => {
                    if self.stands.contains_key(&key(&at)) {
                        continue;
                    }
                    let graphed = self.graphed_at(&at)?;
                    if !graphed.unreadable().any(|unread| unread == path) {
                        let held = Held {
                            records: graphed
                                .at_path(path)
                                .map(|(id, position)| (id.to_string(), position.clone()))
                                .collect(),
                            // A tree the build refused holds no document at
                            // this path the way a commit that never had one
                            // does, and the two are not the same answer: it
                            // holds records this walk cannot name.
                            known: graphed.readable(),
                        };
                        self.stands.insert(key(&at), held);
                        continue;
                    }
                    let mut known = true;
                    let earlier = self.before_change(&at, path, &mut known)?;
                    walk.push(Visit::Join(at, earlier.clone(), known));
                    walk.extend(earlier.into_iter().map(Visit::Ask));
                }
                Visit::Join(at, earlier, known) => {
                    let mut held = Held {
                        records: Vec::new(),
                        known,
                    };
                    for line in earlier {
                        let stood = &self.stands[&key(&line)];
                        held.known &= stood.known;
                        held.records.extend(stood.records.iter().cloned());
                    }
                    held.records.sort();
                    held.records.dedup();
                    self.stands.insert(key(&at), held);
                }
            }
        }
        Ok(self.stands[&key(commit)].clone())
    }

    /// The commits to read `path` from, one change back from `commit`. A cut
    /// clears `known` rather than ending the walk: the other lines still have
    /// answers to give, and the run says which path it could not read back,
    /// because the count it leaves behind does not say why.
    fn before_change(&mut self, commit: &str, path: &str, known: &mut bool) -> Result<Vec<String>> {
        let before = self
            .repository
            .before_change(commit, Path::new(path))
            .map_err(|e| CoreError::Git {
                context: format!(
                    "what {path} held before commit {commit} broke it could not be read"
                ),
                stderr: e.to_string(),
            })?;
        Ok(match before {
            Before::Commits(earlier) => earlier,
            Before::Cut => {
                *known = false;
                if self.cut.insert(path.to_string()) {
                    self.unread.push(Warning {
                        code: WarningCode::HistoryUnread,
                        message: format!(
                            "what {path} held before the change that broke it lies beyond this \
                             shallow clone's cut, so the records it may have stood for are \
                             counted rather than judged: fetch the history behind it to judge \
                             them"
                        ),
                    });
                }
                Vec::new()
            }
        })
    }

    /// The project as `commit`'s tree holds it, graphed once per tree.
    fn graphed_at(&mut self, commit: &str) -> Result<Arc<Positions>> {
        let tree = match self.trees.get(commit) {
            Some(tree) => tree.clone(),
            None => self.repository.tree(commit).map_err(|e| CoreError::Git {
                context: format!("the tree commit {commit} records could not be read"),
                stderr: e.to_string(),
            })?,
        };
        if let Some(positions) = self.graphed.get(&tree) {
            return Ok(Arc::clone(positions));
        }
        let built = self.graph_commit(commit, &tree);
        let outcome = self.readable(commit, built)?;
        let positions = Arc::new(match outcome {
            Read::Graphed(Some(outcome)) => Positions::of(&outcome.graph),
            Read::Graphed(None) => Positions::empty(),
            Read::Refused => Positions::unread(),
        });
        self.graphed.insert(tree, Arc::clone(&positions));
        Ok(positions)
    }

    /// The project as `commit` records it at `tree`, or `None` where the
    /// commit carries no project.
    fn graph_commit(&mut self, commit: &str, tree: &str) -> Result<Option<BuildOutcome>> {
        let unreadable = |stderr: String| CoreError::Git {
            context: format!("commit {commit} could not be checked out"),
            stderr,
        };
        match self
            .repository
            .ref_state(commit)
            .map_err(|e| unreadable(e.to_string()))?
        {
            RefState::CarriesProject => {}
            RefState::WithoutProject => return Ok(None),
            RefState::Unborn | RefState::Unresolvable => {
                return Err(unreadable("git resolves no such commit".to_string()).into());
            }
        }
        let checkout = match self.checkout.take() {
            Some(checkout) => checkout,
            None => Checkout::acquire(self.repository)?,
        };
        self.checkout
            .insert(checkout)
            .graph(tree, self.config)
            .with_context(|| format!("graphing the project at commit {commit}"))
            .map(Some)
    }

    /// What a commit's build says about its tree. A build that refuses the
    /// tree is this walk's to carry rather than the run's to die of: nothing
    /// short of rewriting that commit could make it readable, so the records
    /// around it are counted rather than judged and the envelope says which
    /// commit went unread. A failure that is not the build's verdict on the
    /// tree — git itself, the filesystem — still ends the run.
    fn readable(&mut self, commit: &str, built: Result<Option<BuildOutcome>>) -> Result<Read> {
        match built {
            Ok(outcome) => Ok(Read::Graphed(outcome.map(Box::new))),
            Err(e) => match e
                .downcast_ref::<CoreError>()
                .filter(|e| refuses_the_tree(e))
            {
                // The verdict itself, not the context the build wrapped it
                // in: that context names the commit this message already
                // names, and the operator's question is what the tree does
                // that this config refuses.
                Some(refusal) => {
                    self.unread.push(Warning {
                        code: WarningCode::HistoryUnread,
                        message: format!(
                            "commit {short} could not be graphed under this project's config, so \
                             the records its step carried are counted rather than judged: \
                             {refusal}",
                            short = commit.get(..12).unwrap_or(commit)
                        ),
                    });
                    Ok(Read::Refused)
                }
                None => Err(e),
            },
        }
    }
}

/// What one commit's build produced. Boxed so the enum's footprint is not
/// dominated by the graph-sized variant, the same reason
/// [`BaselineSnapshot`] boxes its own.
enum Read {
    /// The project as that commit holds it, or `None` where it holds none.
    Graphed(Option<Box<BuildOutcome>>),
    /// The build refused the tree.
    Refused,
}

/// Whether an error is the build's verdict on a tree rather than a failure of
/// the machinery that read it — the first is a fact about that commit and
/// stays true however often it is read.
fn refuses_the_tree(error: &CoreError) -> bool {
    matches!(
        error,
        CoreError::DuplicateId { .. }
            | CoreError::Parse { .. }
            | CoreError::Config(_)
            | CoreError::Cycle { .. }
    )
}

/// A diff against a git ref plus the ref build's own warnings — a parse
/// failure at the baseline silently disables the diff-aware rules for
/// that document, so the warning must reach the envelope, not be dropped.
pub struct BaselineDiff {
    pub diff: nodex_core::diff::GraphDiff,
    pub warnings: Vec<Warning>,
}

/// A resolved `rules.immutable_baseline` — what a default `check` and
/// `query issues` run under. The three states are typed so neither
/// consumer can drop the inert advisory: a configured baseline that
/// cannot engage is a warning the operator must see, never a silent
/// `None`.
pub enum BaselineResolution {
    /// No baseline applies: none configured, or no immutability rules
    /// for it to feed. Nothing to surface.
    NotApplicable,
    /// A baseline is configured and immutability rules exist, but it
    /// cannot be read — the project is not a git work tree, or the ref
    /// does not carry the project. The diff-aware rules are inert this
    /// run; carries the advisory.
    Inert { warning: Warning },
    /// The baseline diff plus the ref build's own warnings. Boxed so
    /// the enum's footprint is not dominated by the `GraphDiff`-sized
    /// variant the other two states never carry.
    Resolved(Box<BaselineDiff>),
}

/// Resolve `rules.immutable_baseline` and take the snapshot the write seams
/// judge against — the one place a mutating command obtains a probe, so
/// every one of them locks against the same baseline `check` reports on.
///
/// Checks a tree out where a baseline is bound, or where a registered rule
/// judges steps and so reads `HEAD` whatever the baseline; a project with
/// neither spawns nothing and snapshots nothing.
pub fn write_baseline(root: &Path, config: &nodex_core::Config) -> Result<BaselineProbe> {
    let binding = nodex_core::BaselineBinding::resolve(root, config)?;
    Ok(binding.snapshot(
        |repository, git_ref| {
            match baseline_graph(root, repository, git_ref, config, Steps::Uncommitted) {
                Ok(BaselineSnapshot::Graphed(baseline)) => Ok(*baseline),
                // The binding is only bound for a ref that carries the project,
                // so reading it cannot find otherwise. Say so rather than
                // assume it: a lock that cannot be evaluated refuses the write.
                Ok(BaselineSnapshot::Absent { warning }) => Err(CoreError::Git {
                    context: format!("{git_ref:?} carries the project but was read without it"),
                    stderr: warning.message,
                }),
                Err(e) => Err(typed(e, || {
                    format!("the baseline at {git_ref:?} could not be graphed")
                })),
            }
        },
        || {
            uncommitted_history(root, config).map_err(|e| {
                typed(e, || {
                    "the history statuses.flow judges could not be graphed".to_string()
                })
            })
        },
    )?)
}

/// The core error behind a failed read of a ref. Graphing a ref runs the
/// same build `check` runs, so it fails the same typed ways; keep that cause,
/// because the two planes must name one condition with one code, and only a
/// failure with no typed cause is genuinely a git failure.
fn typed(error: anyhow::Error, context: impl FnOnce() -> String) -> CoreError {
    error
        .downcast::<CoreError>()
        .unwrap_or_else(|untyped| CoreError::Git {
            context: context(),
            stderr: untyped.to_string(),
        })
}

/// Resolve the configured `rules.immutable_baseline` into the diff a
/// default `check` runs under. The single resolution seam for `check`
/// (without `--since`) and `query issues`, so the two commands can
/// never disagree about the immutability violations — nor about the
/// advisory when the baseline is inert: activation and wording come from
/// `nodex_core::BaselineProbe`, the same resolution the write seams lock
/// against. `root` is the project's working tree, which binds the
/// repository; `current` is what is judged.
pub fn baseline_diff(
    root: &Path,
    config: &nodex_core::Config,
    current: Current<'_>,
) -> Result<Prior> {
    // A baseline whose ref cannot be read refuses the run outright, the
    // same way every write seam does: a `check` that went green here would
    // be reporting on rules that can never fire.
    let binding = nodex_core::BaselineBinding::resolve(root, config)?;
    match binding.bound() {
        Some((repository, git_ref)) => {
            prior(repository, git_ref, config, current, Steps::Uncommitted)
        }
        None => {
            let ancestry = uncommitted_history(root, config)?;
            Ok(Prior {
                baseline: match binding.advisory() {
                    Some(warning) => BaselineResolution::Inert { warning },
                    None => BaselineResolution::NotApplicable,
                },
                unread: ancestry
                    .as_ref()
                    .map(|ancestry| ancestry.warnings().to_vec())
                    .unwrap_or_default(),
                steps: ancestry.map(|ancestry| ancestry.through(current.graph)),
            })
        }
    }
}

/// A directory the repository's trees are checked out into, one at a time,
/// held by this process until it is dropped.
///
/// The directories live under the repository's common git directory and
/// persist between runs, so checking a tree out writes only what differs
/// from the tree the directory last held: a read costs what changed, not the
/// repository. Each is held through an exclusive lock on a file beside it,
/// and a process finding one held takes the next, so concurrent runs never
/// share a directory and none waits on another.
///
/// A tree is written with `read-tree --reset -u` against the directory's own
/// index, which rewrites every file that differs from the tree — edited,
/// removed, or left half-written by a run that stopped. A file no index lists
/// is outside what that sees, so taking a directory cleans it out.
pub struct Checkout {
    repository: Repository,
    dir: PathBuf,
    index: PathBuf,
    _held: File,
}

impl Checkout {
    /// Take the first directory no other process holds, creating one when
    /// every existing directory is held.
    pub fn acquire(repository: &Repository) -> Result<Self> {
        let pool = repository
            .common_dir()
            .map_err(|e| CoreError::Git {
                context: "the repository's common git directory could not be resolved".to_string(),
                stderr: e.to_string(),
            })?
            .join("nodex")
            .join("checkout");
        std::fs::create_dir_all(&pool).map_err(|source| CoreError::Io {
            path: pool.clone(),
            source,
        })?;
        let mut slot = 0usize;
        loop {
            let lock = pool.join(format!("{slot}.lock"));
            let held = File::options()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&lock)
                .map_err(|source| CoreError::Io {
                    path: lock.clone(),
                    source,
                })?;
            match held.try_lock() {
                Ok(()) => {
                    return Self::take(
                        repository,
                        pool.join(slot.to_string()),
                        pool.join(format!("{slot}.index")),
                        held,
                    );
                }
                Err(TryLockError::WouldBlock) => slot += 1,
                Err(TryLockError::Error(source)) => {
                    return Err(CoreError::Io { path: lock, source }.into());
                }
            }
        }
    }

    /// Leave a directory this process now holds with nothing its index does
    /// not list.
    fn take(repository: &Repository, dir: PathBuf, index: PathBuf, held: File) -> Result<Self> {
        // Git locks an index by creating a file beside it, and only a holder
        // of this directory writes its index, so a lock found now was left by
        // a run that stopped mid-write.
        let mut stale = index.clone().into_os_string();
        stale.push(".lock");
        let stale = PathBuf::from(stale);
        match std::fs::remove_file(&stale) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(CoreError::Io {
                    path: stale,
                    source,
                }
                .into());
            }
        }
        std::fs::create_dir_all(&dir).map_err(|source| CoreError::Io {
            path: dir.clone(),
            source,
        })?;
        let checkout = Self {
            repository: repository.clone(),
            dir,
            index,
            _held: held,
        };
        checkout.git(&["clean", "-ffdxq"])?;
        Ok(checkout)
    }

    /// Check `tree` out, leaving exactly what it records, and return where
    /// the project sits in it.
    pub fn hold(&self, tree: &str) -> Result<PathBuf> {
        self.git(&["read-tree", "--reset", "-u", tree])?;
        Ok(self.repository.locate(&self.dir))
    }

    /// The project as `tree` records it, graphed under `config`. The tree
    /// carries the project — [`recorded`] is what establishes that.
    pub fn graph(&self, tree: &str, config: &nodex_core::Config) -> Result<BuildOutcome> {
        let project = self.hold(tree)?;
        Ok(nodex_core::builder::build_of_ref(
            &project, &self.dir, config,
        )?)
    }

    /// The directory's own root: what the tree recorded, whole. The
    /// confinement boundary for graphing it, because the project inside may
    /// hold an in-scope link to a tracked sibling outside itself, and the
    /// tree records that sibling too.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn git(&self, args: &[&str]) -> Result<()> {
        let output = self
            .repository
            .checkout_command(&self.dir, &self.index)
            .args(args)
            .output()
            .map_err(|e| CoreError::Git {
                context: format!("could not invoke `git {}`", args.join(" ")),
                stderr: e.to_string(),
            })?;
        if !output.status.success() {
            return Err(CoreError::Git {
                context: format!(
                    "`git {}` failed in {}, a checkout only nodex reads and which may be \
                     deleted while no nodex runs",
                    args.join(" "),
                    self.dir.display()
                ),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            }
            .into());
        }
        Ok(())
    }
}

/// What a ref records for the project, established of git before anything
/// is checked out: checking out a ref that turns out not to carry the
/// project would write a whole repository to then refuse it.
pub enum Recorded {
    /// The tree the ref's commit records, which carries the project's
    /// directory.
    Tree(String),
    /// Why the ref holds no project to read. Named as what git records rather
    /// than as what is on disk, because a ref may carry the name and not the
    /// project — a submodule gitlink at the prefix, say.
    Absent(String),
}

/// What `git_ref` records for the project. A ref that names nothing is
/// refused rather than read as an absent project, on both planes: `check
/// --since` would otherwise report every node as in scope and exit 0 on a
/// typo, while the same name in `rules.immutable_baseline` refuses — and
/// `diff` would blame the project's location for a ref that does not exist.
/// `BaselineProbe` draws the line in the same place.
pub fn recorded(repository: &Repository, git_ref: &str) -> Result<Recorded> {
    let state = repository.ref_state(git_ref).map_err(|e| CoreError::Git {
        context: format!("could not establish what {git_ref:?} carries"),
        stderr: e.to_string(),
    })?;
    match state {
        RefState::Unresolvable => Err(CoreError::Git {
            context: format!("{git_ref:?} cannot be read"),
            stderr: "git resolves no such ref".to_string(),
        }
        .into()),
        // Only reachable for a project with a prefix: a ref always records a
        // tree at a repository's own top level.
        RefState::WithoutProject => Ok(Recorded::Absent(format!(
            "that ref records no project directory at {:?}",
            nodex_core::path_guard::forward_string(repository.prefix())
        ))),
        RefState::Unborn => Ok(Recorded::Absent(
            "no ref in the repository names a commit, so there is nothing to compare against"
                .to_string(),
        )),
        RefState::CarriesProject => repository.tree(git_ref).map(Recorded::Tree).map_err(|e| {
            CoreError::Git {
                context: format!("the tree {git_ref:?} records could not be read"),
                stderr: e.to_string(),
            }
            .into()
        }),
    }
}

/// The tree the next commit records, established to carry the project.
///
/// The index read is the one git is committing: a commit under way names
/// its own in `GIT_INDEX_FILE` for the hooks it runs (`git commit -a`, `git
/// commit <path>`), and anywhere else the work tree's index is the one. The
/// variable is read here and nowhere else; every other invocation clears it,
/// because there it would redirect a read of the repository.
pub fn staged_tree(repository: &Repository) -> Result<String> {
    let index = match std::env::var_os("GIT_INDEX_FILE").filter(|named| !named.is_empty()) {
        None => None,
        Some(named) => {
            let index = repository.index_file(Path::new(&named));
            if !index.is_file() {
                return Err(CoreError::Git {
                    context: "GIT_INDEX_FILE names no index".to_string(),
                    stderr: format!("{} is not a file", index.display()),
                }
                .into());
            }
            Some(index)
        }
    };
    let tree = repository
        .index_tree(index.as_deref())
        .map_err(|e| CoreError::Git {
            context: "the index could not be written as a tree".to_string(),
            stderr: e.to_string(),
        })?;
    let carries = repository
        .carries_project(&tree)
        .map_err(|e| CoreError::Git {
            context: "could not establish what the index carries".to_string(),
            stderr: e.to_string(),
        })?;
    if !carries {
        return Err(CoreError::Git {
            context: "the index does not carry this project".to_string(),
            stderr: format!(
                "it records no project directory at {:?}; stage the project's files first",
                nodex_core::path_guard::forward_string(repository.prefix())
            ),
        }
        .into());
    }
    Ok(tree)
}

/// The tree `git_ref` records, for a comparison that cannot proceed without
/// the project on both sides (`nodex diff`, `nodex impact`).
pub fn required_tree(repository: &Repository, git_ref: &str) -> Result<String> {
    match recorded(repository, git_ref)? {
        Recorded::Tree(tree) => Ok(tree),
        Recorded::Absent(detail) => Err(CoreError::Git {
            context: format!("{git_ref:?} does not carry this project"),
            stderr: detail,
        }
        .into()),
    }
}

/// Everything a ref build could not read, named against the ref it came
/// from — the completeness accounting a ref-to-ref report carries.
///
/// A ref build is confined to what the ref records, so what it drops is
/// invisible in the report itself: a document absent from one side reads as
/// added or removed, and one absent from both reads as no change at all. The
/// build's own advisories (a boundary the walk did not cross, a coverage gap)
/// travel with the ref name prefixed; the paths the ref does not record are
/// named here, because nothing else reports them.
///
/// The baseline plane ([`baseline_graph`]) enumerates the same causes in its
/// own words: there an omission means a lock did not engage, here it means a
/// comparison lost one side.
pub fn ref_omissions(git_ref: &str, build: &BuildOutcome) -> Vec<Warning> {
    build
        .warnings
        .iter()
        .map(|w| Warning::new(w.code, format!("{git_ref}: {}", w.message)))
        .chain(
            build
                .dangling_paths
                .iter()
                .chain(build.escaping_paths.iter())
                .map(|path| {
                    Warning::new(
                        WarningCode::BaselineInert,
                        format!(
                            "{git_ref}: {path} is not something the ref records (it resolves to \
                             nothing, or outside the checkout), so it is absent from that side of \
                             the comparison"
                        ),
                    )
                }),
        )
        .collect()
}
