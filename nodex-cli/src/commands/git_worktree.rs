//! Shared git-worktree primitive used by `diff` and `check --since`.
//!
//! Both commands need to materialise a past ref on disk so the regular
//! `builder::build` pipeline can run against it. The detached
//! `git worktree add` approach keeps the user's working tree untouched
//! and survives the temporary checkout via RAII cleanup. This module
//! owns worktree materialisation only; the repository-scoped invocation
//! constructor and byte-level git access (the work-tree probe, a
//! document's bytes at a ref) live in `nodex_core::git`, and every
//! invocation here is built through `nodex_core::git::command` so it
//! binds to the project rather than to an inherited environment.

use anyhow::Result;
use nodex_core::{Warning, WarningCode};
use std::path::{Path, PathBuf};

use nodex_core::error::Error as CoreError;

/// Verify the path is inside a git work tree. Surfaces `GIT_ERROR`
/// when git is unavailable or the directory isn't a work tree — the
/// JSON envelope therefore distinguishes "operator is in the wrong
/// place" from a generic runtime failure (and from a `nodex.toml`
/// validation problem, which uses `CONFIG_ERROR`).
pub fn ensure_work_tree(root: &Path, who: &str) -> Result<()> {
    let output = nodex_core::git::command(root)
        .map_err(|e| CoreError::Git {
            context: format!("{who} could not invoke `git`"),
            stderr: e.to_string(),
        })?
        .args(["rev-parse", "--is-inside-work-tree"])
        .output();
    match output {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(CoreError::Git {
            context: format!("{who} requires a git work tree at the project root"),
            stderr: String::from_utf8_lossy(&o.stderr).trim().to_string(),
        }
        .into()),
        Err(e) => Err(CoreError::Git {
            context: format!("{who} could not invoke `git`"),
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
pub fn diff_against_ref(
    root: &Path,
    git_ref: &str,
    config: &nodex_core::Config,
    current: &nodex_core::Graph,
    scratch_name: &str,
) -> Result<BaselineDiff> {
    let scratch = scratch_dir(root, scratch_name)?;
    let before_target = scratch.join("before");
    let before = Worktree::add(root, git_ref, &before_target, Some(scratch.clone()))?;
    let before_result = nodex_core::builder::build(before.path(), config, true)?;
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
    Ok(BaselineDiff {
        diff: nodex_core::diff::compute_diff(&before_result.graph, current),
        warnings,
    })
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
    /// A baseline is configured and immutability rules exist, but the
    /// project root is not a git work tree — the diff-aware rules are
    /// inert this run. Carries the advisory.
    Inert { warning: Warning },
    /// The baseline diff plus the ref build's own warnings. Boxed so
    /// the enum's footprint is not dominated by the `GraphDiff`-sized
    /// variant the other two states never carry.
    Resolved(Box<BaselineDiff>),
}

/// Resolve the configured `rules.immutable_baseline` into the diff a
/// default `check` runs under. The single resolution seam for `check`
/// (without `--since`) and `query issues`, so the two commands can
/// never disagree about the immutability violations — nor about the
/// advisory when the baseline is inert: the warning wording is
/// constructed exactly once, here.
pub fn baseline_diff(
    root: &Path,
    config: &nodex_core::Config,
    current: &nodex_core::Graph,
    scratch_name: &str,
) -> Result<BaselineResolution> {
    let Some(baseline) = config.rules.immutable_baseline.as_deref() else {
        return Ok(BaselineResolution::NotApplicable);
    };
    if !config.has_immutable_rules() {
        return Ok(BaselineResolution::NotApplicable);
    }
    if !nodex_core::git::is_work_tree(root) {
        return Ok(BaselineResolution::Inert {
            warning: Warning::new(
                WarningCode::BaselineInert,
                format!(
                    "rules.immutable_baseline {baseline:?} is set but the project is not a git \
                     work tree; immutability rules are inert this run"
                ),
            ),
        });
    }
    Ok(BaselineResolution::Resolved(Box::new(diff_against_ref(
        root,
        baseline,
        config,
        current,
        scratch_name,
    )?)))
}

/// RAII guard around a `git worktree add --detach`. Removes the
/// worktree (and its enclosing scratch directory if supplied) on drop,
/// including on panic, so the operator's repo never accumulates
/// `.nodex-*` directories.
pub struct Worktree {
    repo_root: PathBuf,
    path: PathBuf,
    scratch_root: Option<PathBuf>,
}

impl Worktree {
    /// Add a detached worktree of `git_ref` at `target` rooted in
    /// `repo_root`. The optional `scratch_root` is removed alongside
    /// the worktree on drop — useful when `target` lives under a
    /// disposable parent like `.nodex-diff/`.
    pub fn add(
        repo_root: &Path,
        git_ref: &str,
        target: &Path,
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
        // The target derives from the user's project root, so a
        // non-UTF-8 spelling is reachable input — refused as the same
        // typed Git error every other failure here surfaces as, never
        // a panic.
        let Some(target_str) = target.to_str() else {
            cleanup(&scratch_root);
            return Err(CoreError::Git {
                context: format!("git worktree add {git_ref:?} requires a UTF-8 worktree path"),
                stderr: format!("target path {} is not valid UTF-8", target.display()),
            }
            .into());
        };
        let uninvokable = |e: std::io::Error| {
            cleanup(&scratch_root);
            CoreError::Git {
                context: format!("could not invoke `git worktree add` for {git_ref:?}"),
                stderr: e.to_string(),
            }
        };
        let mut git = nodex_core::git::command(repo_root).map_err(uninvokable)?;
        let output = match git
            .args(["worktree", "add", "--detach", target_str, git_ref])
            .output()
        {
            Ok(output) => output,
            Err(e) => return Err(uninvokable(e).into()),
        };
        if !output.status.success() {
            cleanup(&scratch_root);
            return Err(CoreError::Git {
                context: format!("git worktree add {git_ref:?} failed"),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            }
            .into());
        }
        Ok(Self {
            repo_root: repo_root.to_path_buf(),
            path: target.to_path_buf(),
            scratch_root,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        if let Ok(mut git) = nodex_core::git::command(&self.repo_root) {
            let _ = git
                .args([
                    "worktree",
                    "remove",
                    "--force",
                    self.path.to_str().unwrap_or_default(),
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
