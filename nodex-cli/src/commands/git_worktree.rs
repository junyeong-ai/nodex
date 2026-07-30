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

use anyhow::Result;
use nodex_core::{BaselineProbe, RefState, Repository, Warning, WarningCode};
use std::path::{Path, PathBuf};

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
) -> Result<BaselineResolution> {
    match baseline_graph(root, repository, git_ref, config, scratch_name)? {
        BaselineSnapshot::Absent { warning } => Ok(BaselineResolution::Inert { warning }),
        BaselineSnapshot::Graphed(baseline) => {
            Ok(BaselineResolution::Resolved(Box::new(BaselineDiff {
                diff: nodex_core::diff::compute_diff(&baseline.graph, current),
                warnings: baseline.warnings,
            })))
        }
    }
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

/// A baseline graph and everything about building it a caller must surface.
pub struct GraphedBaseline {
    pub graph: nodex_core::Graph,
    pub warnings: Vec<Warning>,
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
) -> Result<BaselineSnapshot> {
    let scratch = scratch_dir(root, scratch_name)?;
    let before_target = scratch.join("before");
    let before = Worktree::add(repository, git_ref, &before_target, Some(scratch.clone()))?;
    let Some(before_root) = before.project_root() else {
        return Ok(BaselineSnapshot::Absent {
            warning: before.absent_project_warning(),
        });
    };
    let before_result = nodex_core::builder::build(before_root, config, true)?;
    // Surface the baseline build's own advisories, ref-tagged. A
    // document that fails to parse AT the baseline vanishes from the
    // before graph, so it looks "added" and the diff-aware immutability
    // rules silently do not fire for it — `check --since`/default check
    // would pass on a lock it never actually enforced. The rule pass
    // only sees the CURRENT graph's parse failures, so the baseline's
    // recorded drops reach the envelope here, as warnings about the
    // baseline (not violations of the working tree).
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
        .collect();
    Ok(BaselineSnapshot::Graphed(Box::new(GraphedBaseline {
        graph: before_result.graph,
        warnings,
    })))
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
/// Costs a materialisation only where a baseline is bound: a project with no
/// baseline, or none of the rules a baseline feeds, resolves to a binding
/// that spawns nothing and snapshots nothing.
pub fn write_baseline(root: &Path, config: &nodex_core::Config) -> Result<BaselineProbe> {
    let binding = nodex_core::BaselineBinding::resolve(root, config)?;
    Ok(binding.snapshot(|repository, git_ref| {
        match baseline_graph(root, repository, git_ref, config, ".nodex-baseline") {
            Ok(BaselineSnapshot::Graphed(baseline)) => Ok((baseline.graph, baseline.warnings)),
            // The binding is only bound for a ref that carries the project,
            // so materialising it cannot find otherwise. Say so rather than
            // assume it: a lock that cannot be evaluated refuses the write.
            Ok(BaselineSnapshot::Absent { warning }) => Err(CoreError::Git {
                context: format!("{git_ref:?} carries the project but did not materialise it"),
                stderr: warning.message,
            }),
            Err(e) => Err(CoreError::Git {
                context: format!("the baseline at {git_ref:?} could not be graphed"),
                stderr: e.to_string(),
            }),
        }
    })?)
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
) -> Result<BaselineResolution> {
    // A baseline whose ref cannot be read refuses the run outright, the
    // same way every write seam does: a `check` that went green here would
    // be reporting on rules that can never fire.
    let probe = nodex_core::BaselineBinding::resolve(root, config)?;
    match probe.bound() {
        Some((repository, git_ref)) => {
            diff_against_ref(root, repository, git_ref, config, current, scratch_name)
        }
        None => Ok(match probe.advisory() {
            Some(warning) => BaselineResolution::Inert { warning },
            None => BaselineResolution::NotApplicable,
        }),
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

/// Create a scratch directory under `root` used as the parent for one
/// or more worktrees. The directory is destroyed by the owning
/// [`Worktree`]'s `Drop` impl when `Some(scratch_root)` is passed to
/// [`Worktree::add`].
///
/// The chosen name embeds the current process id so concurrent
/// invocations in the same project (`nodex diff … &; nodex check … &`)
/// land in disjoint scratch trees and cannot race on cleanup.
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
