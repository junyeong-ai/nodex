//! Shared git-worktree primitive used by `diff`, `impact` and
//! `check --since`.
//!
//! Both commands need to materialise a past ref on disk so the regular
//! `builder::build` pipeline can run against it. The detached
//! `git worktree add` approach keeps the user's working tree untouched
//! and survives the temporary checkout via RAII cleanup. A checkout
//! carries the whole repository, so what is graphed is
//! [`Worktree::project_root`] — the project's own location inside it,
//! which is the checkout root only when the project *is* the repository
//! top level. This module owns materialisation only; the repository
//! binding it materialises from lives in `nodex_core::git`, and the
//! `rules.immutable_baseline` resolution behind [`baseline_diff`] lives
//! in `nodex_core::BaselineProbe`, shared with the write seams it locks.

use anyhow::{Context, Result};
use nodex_core::{
    Ancestry, BaselineProbe, Before, GraphedBaseline, Position, Positions, RefState, Repository,
    Step, Warning, WarningCode,
};
use std::collections::{BTreeSet, HashMap, HashSet};
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

/// Build the graph at `git_ref` (content only — the working tree's
/// `config` stays the single lens) in a disposable worktree and diff it
/// against the already-built `current` graph. The shared substrate for
/// `check --since`, the `rules.immutable_baseline` default, and `query
/// issues` — one implementation, so their violation sets can never
/// diverge.
///
/// `root` is the filesystem authority (the scratch checkout lands under
/// it); `repository` decides what git measures and where the project
/// sits inside a checkout. Returns [`BaselineResolution::Inert`] — never
/// `NotApplicable` — when the ref does not carry the project at all,
/// which is an ordinary state for a subdirectory project introduced
/// after the ref.
pub fn diff_against_ref(
    root: &Path,
    repository: &Repository,
    git_ref: &str,
    config: &nodex_core::Config,
    current: &nodex_core::Graph,
    scratch_name: &str,
) -> Result<Prior> {
    prior(
        root,
        repository,
        git_ref,
        config,
        current,
        scratch_name,
        Steps::Range,
    )
}

/// The baseline at `git_ref` and the history `steps` reaches, read from one
/// materialisation where the ref carries the project.
fn prior(
    root: &Path,
    repository: &Repository,
    git_ref: &str,
    config: &nodex_core::Config,
    current: &nodex_core::Graph,
    scratch_name: &str,
    steps: Steps,
) -> Result<Prior> {
    let since = match steps {
        Steps::Uncommitted => None,
        Steps::Range => Some(git_ref),
    };
    let (baseline, ancestry) =
        match baseline_graph(root, repository, git_ref, config, scratch_name, steps)? {
            BaselineSnapshot::Absent { warning } => (
                BaselineResolution::Inert { warning },
                history(root, repository, config, scratch_name, since)?,
            ),
            BaselineSnapshot::Graphed(baseline) => (
                BaselineResolution::Resolved(Box::new(BaselineDiff {
                    diff: nodex_core::diff::compute_diff(&baseline.graph, current),
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
        steps: ancestry.map(|ancestry| ancestry.through(current)),
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

/// What a ref turned out to hold for the project, once materialised.
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
/// The working tree's `config` stays the single lens — a diff is a question
/// asked from the newer contract, and the ref supplies content only.
pub fn baseline_graph(
    root: &Path,
    repository: &Repository,
    git_ref: &str,
    config: &nodex_core::Config,
    scratch_name: &str,
    steps: Steps,
) -> Result<BaselineSnapshot> {
    let scratch = scratch_dir(root, scratch_name)?;
    let before_target = scratch.join("before");
    let before = Worktree::add(repository, git_ref, &before_target, Some(scratch.clone()))?;
    let Some(before_root) = before.project_root() else {
        return Ok(BaselineSnapshot::Absent {
            warning: before.absent_project_warning(),
        });
    };
    let before_result = nodex_core::builder::build_of_ref(before_root, before.checkout(), config)?;
    // An entry the walk classified by type and could place in neither class —
    // a socket, a symlink resolving to nothing, a boundary it declined to
    // cross — may never have been a document at all, and an advisory about it
    // would assert one existed. The working tree is asked, at or under the
    // path because such a channel can name a directory the walk reaches
    // documents through, and a document standing there is what establishes
    // there was one to guard.
    let current: Vec<String> = nodex_core::builder::scanner::scan_scope(root, config)?
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
            let tree = repository.tree(git_ref).map_err(|e| CoreError::Git {
                context: format!("the tree {git_ref:?} records could not be read"),
                stderr: e.to_string(),
            })?;
            let mut snapshots = Snapshots {
                root,
                repository,
                config,
                scratch_name,
                worktree: Some(before),
                trees: HashMap::new(),
                graphed: HashMap::from([(tree, Arc::new(Positions::of(&before_result.graph)))]),
                recovered: HashMap::new(),
                unread: Vec::new(),
            };
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
/// in turn in one worktree and graphing it under the working tree's config.
/// `None` where no registered rule judges steps.
pub fn history(
    root: &Path,
    repository: &Repository,
    config: &nodex_core::Config,
    scratch_name: &str,
    since: Option<&str>,
) -> Result<Option<Ancestry>> {
    if !config.judges_steps() {
        return Ok(None);
    }
    let mut snapshots = Snapshots {
        root,
        repository,
        config,
        scratch_name,
        worktree: None,
        trees: HashMap::new(),
        graphed: HashMap::new(),
        recovered: HashMap::new(),
        unread: Vec::new(),
    };
    Ok(Some(snapshots.ancestry(since)?))
}

/// [`history`] for the uncommitted change alone, for a project whose
/// repository nothing has bound yet. `None` outside a git work tree, where
/// there is no commit to step from — the rules say so as they skip.
pub fn uncommitted_history(
    root: &Path,
    config: &nodex_core::Config,
    scratch_name: &str,
) -> Result<Option<Ancestry>> {
    if !config.judges_steps() {
        return Ok(None);
    }
    match Repository::discover(root) {
        Ok(Some(repository)) => history(root, &repository, config, scratch_name, None),
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
    root: &'a Path,
    repository: &'a Repository,
    config: &'a nodex_core::Config,
    scratch_name: &'a str,
    /// Materialised at the first commit that carries the project, and moved
    /// from commit to commit after that.
    worktree: Option<Worktree>,
    trees: HashMap<String, String>,
    graphed: HashMap<String, Arc<Positions>>,
    recovered: HashMap<String, Arc<Positions>>,
    /// The commits whose trees this walk could not graph.
    unread: Vec<Warning>,
}

impl Snapshots<'_> {
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
                base: self.agreed(&parents, &commit.parents)?,
                parents,
                child: self.at(&commit.id)?,
            });
        }
        let carried: Vec<Arc<Positions>> = heads
            .iter()
            .map(|head| self.at(head))
            .collect::<Result<_>>()?;
        let head_base = self.agreed(&carried, &heads)?;
        let ignored = self.repository.ignored().map_err(unreadable)?;
        Ok(Ancestry::new(
            committed,
            carried,
            head_base,
            ignored,
            std::mem::take(&mut self.unread),
        ))
    }

    /// Where the lines behind a step last agreed, for a step made on more
    /// than one of them and only where they disagree about a record: what
    /// every line still carries alike, the base can only confirm, and reading
    /// it would cost a snapshot to learn nothing.
    fn agreed(
        &mut self,
        carried: &[Arc<Positions>],
        commits: &[String],
    ) -> Result<Option<Arc<Positions>>> {
        if carried.len() < 2 || !disagree(carried) {
            return Ok(None);
        }
        let base = self
            .repository
            .merge_base(commits)
            .map_err(|e| CoreError::Git {
                context: format!(
                    "where the lines behind {commits:?} last agreed could not be read"
                ),
                stderr: e.to_string(),
            })?;
        base.map(|base| self.at(&base)).transpose()
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
                    if !self.held_before(commit, path, &mut records)? {
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

    /// What stood at `path` before the change that left `commit` unable to
    /// read it, collected into `records`: the nearest commits behind it whose
    /// own snapshot could read the path, one line of history at a time. Every
    /// line is followed, because a merge's parents may each hold a record
    /// there.
    ///
    /// `false` where a line ends at a shallow clone's cut instead of at a
    /// commit that could read the path: the record that stood there is beyond
    /// what this clone holds, and the records the other lines gave are still
    /// theirs.
    fn held_before(
        &mut self,
        commit: &str,
        path: &str,
        records: &mut Vec<(String, Position)>,
    ) -> Result<bool> {
        let mut known = true;
        let mut walked = HashSet::new();
        let mut frontier = self.before_change(commit, path, &mut known)?;
        while let Some(earlier) = frontier.pop() {
            if !walked.insert(earlier.clone()) {
                continue;
            }
            let held = self.graphed_at(&earlier)?;
            match held.unreadable().any(|unread| unread == path) {
                true => frontier.extend(self.before_change(&earlier, path, &mut known)?),
                false => records.extend(
                    held.at_path(path)
                        .map(|(id, position)| (id.to_string(), position.clone())),
                ),
            }
        }
        Ok(known)
    }

    /// The commits to read `path` from, one change back from `commit`. A cut
    /// clears `known` rather than ending the walk: the other lines still have
    /// answers to give.
    fn before_change(&self, commit: &str, path: &str, known: &mut bool) -> Result<Vec<String>> {
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
        let outcome = match &self.worktree {
            Some(worktree) => self.readable(commit, worktree.graph_at(commit, self.config))?,
            None => {
                let scratch = scratch_dir(self.root, self.scratch_name)?;
                let worktree = Worktree::add(
                    self.repository,
                    commit,
                    &scratch.join("steps"),
                    Some(scratch),
                )?;
                let built = worktree
                    .project_root()
                    .map(|project| {
                        nodex_core::builder::build_of_ref(project, worktree.checkout(), self.config)
                            .with_context(|| format!("graphing the project at commit {commit}"))
                    })
                    .transpose();
                let outcome = self.readable(commit, built)?;
                if matches!(outcome, Read::Graphed(Some(_))) {
                    self.worktree = Some(worktree);
                }
                outcome
            }
        };
        let positions = Arc::new(match outcome {
            Read::Graphed(Some(outcome)) => Positions::of(&outcome.graph),
            Read::Graphed(None) => Positions::empty(),
            Read::Refused => Positions::unread(),
        });
        self.graphed.insert(tree, Arc::clone(&positions));
        Ok(positions)
    }

    /// What a commit's build says about its tree. A build that refuses the
    /// tree is this walk's to carry rather than the run's to die of: nothing
    /// short of rewriting that commit could make it readable, so the records
    /// around it are counted rather than judged and the envelope says which
    /// commit went unread. A failure that is not the build's verdict on the
    /// tree — git itself, the filesystem — still ends the run.
    fn readable(
        &mut self,
        commit: &str,
        built: Result<Option<nodex_core::builder::BuildOutcome>>,
    ) -> Result<Read> {
        match built {
            Ok(outcome) => Ok(Read::Graphed(outcome.map(Box::new))),
            Err(e) => match e.downcast_ref::<CoreError>().is_some_and(refuses_the_tree) {
                true => {
                    self.unread.push(Warning {
                        code: WarningCode::HistoryUnread,
                        message: format!(
                            "commit {short} could not be graphed under this project's config, so \
                             the records its step carried are counted rather than judged: {e}",
                            short = commit.get(..12).unwrap_or(commit)
                        ),
                    });
                    Ok(Read::Refused)
                }
                false => Err(e),
            },
        }
    }
}

/// What one commit's build produced. Boxed so the enum's footprint is not
/// dominated by the graph-sized variant, the same reason
/// [`BaselineSnapshot`] boxes its own.
enum Read {
    /// The project as that commit holds it, or `None` where it holds none.
    Graphed(Option<Box<nodex_core::builder::BuildOutcome>>),
    /// The build refused the tree.
    Refused,
}

/// Whether an error is the build's verdict on a tree rather than a failure of
/// the machinery that read it — the first is a fact about that commit and
/// stays true however often it is read.
fn refuses_the_tree(error: &CoreError) -> bool {
    matches!(
        error,
        CoreError::DuplicateId { .. } | CoreError::Parse { .. } | CoreError::Config(_)
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
/// Costs a materialisation where a baseline is bound, or where a registered
/// rule judges steps and so reads `HEAD` whatever the baseline; a project with
/// neither spawns nothing and snapshots nothing.
pub fn write_baseline(root: &Path, config: &nodex_core::Config) -> Result<BaselineProbe> {
    let binding = nodex_core::BaselineBinding::resolve(root, config)?;
    Ok(binding.snapshot(
        |repository, git_ref| {
            match baseline_graph(
                root,
                repository,
                git_ref,
                config,
                ".nodex-baseline",
                Steps::Uncommitted,
            ) {
                Ok(BaselineSnapshot::Graphed(baseline)) => Ok(*baseline),
                // The binding is only bound for a ref that carries the project,
                // so materialising it cannot find otherwise. Say so rather than
                // assume it: a lock that cannot be evaluated refuses the write.
                Ok(BaselineSnapshot::Absent { warning }) => Err(CoreError::Git {
                    context: format!("{git_ref:?} carries the project but did not materialise it"),
                    stderr: warning.message,
                }),
                Err(e) => Err(typed(e, || {
                    format!("the baseline at {git_ref:?} could not be graphed")
                })),
            }
        },
        || {
            uncommitted_history(root, config, ".nodex-baseline").map_err(|e| {
                typed(e, || {
                    "the history statuses.flow judges could not be graphed".to_string()
                })
            })
        },
    )?)
}

/// The core error behind a failed materialisation. Graphing a ref runs the
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
/// against.
pub fn baseline_diff(
    root: &Path,
    config: &nodex_core::Config,
    current: &nodex_core::Graph,
    scratch_name: &str,
) -> Result<Prior> {
    // A baseline whose ref cannot be read refuses the run outright, the
    // same way every write seam does: a `check` that went green here would
    // be reporting on rules that can never fire.
    let binding = nodex_core::BaselineBinding::resolve(root, config)?;
    match binding.bound() {
        Some((repository, git_ref)) => prior(
            root,
            repository,
            git_ref,
            config,
            current,
            scratch_name,
            Steps::Uncommitted,
        ),
        None => {
            let ancestry = uncommitted_history(root, config, scratch_name)?;
            Ok(Prior {
                baseline: match binding.advisory() {
                    Some(warning) => BaselineResolution::Inert { warning },
                    None => BaselineResolution::NotApplicable,
                },
                unread: ancestry
                    .as_ref()
                    .map(|ancestry| ancestry.warnings().to_vec())
                    .unwrap_or_default(),
                steps: ancestry.map(|ancestry| ancestry.through(current)),
            })
        }
    }
}

/// RAII guard around a `git worktree add --detach`. Removes the
/// worktree (and its enclosing scratch directory if supplied) on drop,
/// including on panic, so the operator's repo never accumulates
/// `.nodex-*` directories.
///
/// A checkout exists exactly when the ref carries the project — the one
/// condition anything would read it for — so `state` decides both what
/// [`Worktree::project_root`] answers and whether there is a worktree to
/// remove. It is kept whole rather than reduced to that one bit: "this ref
/// names nothing" and "this ref does not hold the project" are different
/// facts with different verdicts, and a diagnostic that names the wrong
/// one sends the operator to fix the wrong thing.
pub struct Worktree {
    repository: Repository,
    git_ref: String,
    checkout: PathBuf,
    project_root: PathBuf,
    state: RefState,
    scratch_root: Option<PathBuf>,
}

impl Worktree {
    /// Check `git_ref` out at `checkout` as a detached worktree of
    /// `repository`. The optional `scratch_root` is removed alongside
    /// the worktree on drop — useful when `checkout` lives under a
    /// disposable parent like `.nodex-diff/`.
    ///
    /// `checkout` must be absolute: the invocation runs in the
    /// repository's work tree, so a relative path would name a location
    /// relative to *that* rather than to the project, and the checkout
    /// would silently land outside the project.
    pub fn add(
        repository: &Repository,
        git_ref: &str,
        checkout: &Path,
        scratch_root: Option<PathBuf>,
    ) -> Result<Self> {
        // The scratch directory was created before this call; until the
        // RAII guard owns it, any early error here would leak it (the
        // guard's Drop never runs because the guard is never built). So
        // every failure path removes it first.
        let cleanup = |scratch: &Option<PathBuf>| {
            if let Some(dir) = scratch {
                let _ = std::fs::remove_dir_all(dir);
            }
        };
        if !checkout.is_absolute() {
            cleanup(&scratch_root);
            return Err(CoreError::Git {
                context: format!("git worktree add {git_ref:?} requires an absolute worktree path"),
                stderr: format!(
                    "target path {} is relative, and git runs in the repository's work tree",
                    checkout.display()
                ),
            }
            .into());
        }
        // The checkout derives from the user's project root, so a
        // non-UTF-8 spelling is reachable input — refused as the same
        // typed Git error every other failure here surfaces as, never
        // a panic.
        let Some(checkout_str) = checkout.to_str() else {
            cleanup(&scratch_root);
            return Err(CoreError::Git {
                context: format!("git worktree add {git_ref:?} requires a UTF-8 worktree path"),
                stderr: format!("target path {} is not valid UTF-8", checkout.display()),
            }
            .into());
        };
        // Asked before anything is materialised, and of git rather than
        // of the checkout: `git worktree add` creates an ordinary empty
        // directory for a submodule path it does not populate, so a ref
        // that records the project's prefix as a gitlink leaves a
        // directory on disk that no document was ever checked out into.
        // A stat cannot tell that apart from the project itself, and
        // reading it as the project graphs an empty baseline — every
        // current document reported as newly added. Resolving first also
        // keeps a failure here from leaking a checkout no RAII guard owns
        // yet.
        let state = match repository.ref_state(git_ref) {
            Ok(state) => state,
            Err(e) => {
                cleanup(&scratch_root);
                return Err(CoreError::Git {
                    context: format!("could not establish what {git_ref:?} carries"),
                    stderr: e.to_string(),
                }
                .into());
            }
        };
        // A ref that names nothing is refused here rather than carried as
        // an absent project, on both planes: `check --since` would
        // otherwise report every node as in scope and exit 0 on a typo,
        // while the same name in `rules.immutable_baseline` refuses — and
        // `diff` would blame the project's location for a ref that does not
        // exist. `BaselineProbe` draws the line in the same place.
        if state == RefState::Unresolvable {
            cleanup(&scratch_root);
            return Err(CoreError::Git {
                context: format!("{git_ref:?} cannot be read"),
                stderr: "git resolves no such ref".to_string(),
            }
            .into());
        }
        // A ref without the project has nothing this checkout could be
        // read for, and the answer is already in hand: materialising it
        // would copy out a whole repository — every file of a monorepo,
        // twice for a two-ref comparison — to then be refused. The
        // baseline path reaches the same conclusion without an invocation;
        // the explicit refs `diff` / `impact` / `check --since` name reach
        // it here.
        if state == RefState::CarriesProject {
            let output = repository
                .command()
                .args(["worktree", "add", "--detach", checkout_str, git_ref])
                .output();
            let output = match output {
                Ok(output) => output,
                Err(e) => {
                    cleanup(&scratch_root);
                    return Err(CoreError::Git {
                        context: format!("could not invoke `git worktree add` for {git_ref:?}"),
                        stderr: e.to_string(),
                    }
                    .into());
                }
            };
            if !output.status.success() {
                cleanup(&scratch_root);
                return Err(CoreError::Git {
                    context: format!("git worktree add {git_ref:?} failed"),
                    stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
                }
                .into());
            }
        }
        Ok(Self {
            repository: repository.clone(),
            git_ref: git_ref.to_string(),
            project_root: repository.locate(checkout),
            checkout: checkout.to_path_buf(),
            state,
            scratch_root,
        })
    }

    /// The project's root inside the materialised checkout — the only
    /// directory a consumer may graph, so a project that is not the
    /// repository top level is never read as the repository around it.
    /// `None` when the ref does not carry the project at all.
    pub fn project_root(&self) -> Option<&Path> {
        (self.state == RefState::CarriesProject).then_some(&*self.project_root)
    }

    /// The checkout's own root — what the ref recorded, whole. The
    /// confinement boundary for graphing it: the project inside may hold an
    /// in-scope link to a tracked sibling outside itself, and the ref records
    /// that sibling too.
    pub fn checkout(&self) -> &Path {
        &self.checkout
    }

    /// Check `commit` out in place of what this worktree holds and graph the
    /// project there under `config`; `None` when that commit does not carry
    /// the project. Whatever was read from the checkout before is gone from
    /// disk afterwards.
    pub fn graph_at(
        &self,
        commit: &str,
        config: &nodex_core::Config,
    ) -> Result<Option<nodex_core::builder::BuildOutcome>> {
        let unreadable = |stderr: String| CoreError::Git {
            context: format!("commit {commit} could not be checked out"),
            stderr,
        };
        if self.project_root().is_none() {
            return Err(
                unreadable("no checkout was materialised for this worktree".to_string()).into(),
            );
        }
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
        let output = nodex_core::git::command(&self.checkout)
            .and_then(|mut git| {
                git.args(["checkout", "--quiet", "--force", "--detach", commit])
                    .output()
            })
            .map_err(|e| unreadable(e.to_string()))?;
        if !output.status.success() {
            return Err(
                unreadable(String::from_utf8_lossy(&output.stderr).trim().to_string()).into(),
            );
        }
        let outcome = nodex_core::builder::build_of_ref(&self.project_root, &self.checkout, config)
            .with_context(|| format!("graphing the project at commit {commit}"))?;
        Ok(Some(outcome))
    }

    /// [`project_root`](Self::project_root) for a consumer that cannot
    /// proceed without it (`nodex diff`, `nodex impact` need both sides
    /// of the comparison), as a typed `GIT_ERROR` naming the ref that
    /// does not carry the project.
    pub fn require_project_root(&self) -> Result<&Path> {
        self.project_root().ok_or_else(|| {
            CoreError::Git {
                context: format!("{:?} does not carry this project", self.git_ref),
                stderr: self.absent_project_detail(),
            }
            .into()
        })
    }

    /// The same condition as an advisory, for the baseline substrate:
    /// a ref without the project has no snapshot to lock against, so the
    /// diff-aware rules are inert rather than the run being refused.
    fn absent_project_warning(&self) -> Warning {
        Warning::new(
            WarningCode::BaselineInert,
            format!(
                "baseline {}: {} — diff-aware rules are inert this run",
                self.git_ref,
                self.absent_project_detail()
            ),
        )
    }

    /// Only reachable for a project with a prefix: a ref always records
    /// a tree at a repository's own top level, so a top-level project is
    /// never the absent one. Named as what git records rather than as
    /// what is on disk, because a ref may carry the name and not the
    /// project — a submodule gitlink at the prefix, say.
    fn absent_project_detail(&self) -> String {
        match self.state {
            RefState::Unborn => {
                "no ref in the repository names a commit, so there is nothing to compare against"
                    .to_string()
            }
            // Refused in `add`, so a `Worktree` never holds it.
            RefState::Unresolvable | RefState::CarriesProject => unreachable!(
                "a worktree exists only for a resolvable ref, and only an absent project is \
                 described here"
            ),
            RefState::WithoutProject => format!(
                "that ref records no project directory at {:?}",
                nodex_core::path_guard::forward_string(self.repository.prefix())
            ),
        }
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        if self.state == RefState::CarriesProject {
            let _ = self
                .repository
                .command()
                .args([
                    "worktree",
                    "remove",
                    "--force",
                    self.checkout.to_str().unwrap_or_default(),
                ])
                .output();
        }
        if let Some(scratch) = &self.scratch_root {
            let _ = std::fs::remove_dir_all(scratch);
        }
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
pub fn ref_omissions(git_ref: &str, build: &nodex_core::builder::BuildOutcome) -> Vec<Warning> {
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

/// Create a scratch directory under `root` used as the parent for one or
/// more worktrees. The directory is destroyed by the owning [`Worktree`]'s
/// `Drop` impl when `Some(scratch_root)` is passed to [`Worktree::add`].
///
/// The chosen name embeds the current process id so concurrent invocations
/// in the same project (`nodex diff … &; nodex check … &`) land in disjoint
/// scratch trees and cannot race on cleanup.
pub fn scratch_dir(root: &Path, name: &str) -> Result<PathBuf> {
    let scratch_root = root.join(format!("{name}-{}", std::process::id()));
    if scratch_root.exists() {
        std::fs::remove_dir_all(&scratch_root).map_err(|source| CoreError::Io {
            path: scratch_root.clone(),
            source,
        })?;
    }
    std::fs::create_dir_all(&scratch_root).map_err(|source| CoreError::Io {
        path: scratch_root.clone(),
        source,
    })?;
    Ok(scratch_root)
}
