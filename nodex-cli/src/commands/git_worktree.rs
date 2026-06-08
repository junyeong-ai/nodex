//! Shared git-worktree primitive used by `diff` and `check --since`.
//!
//! Both commands need to materialise a past ref on disk so the regular
//! `builder::build` pipeline can run against it. The detached
//! `git worktree add` approach keeps the user's working tree untouched
//! and survives the temporary checkout via RAII cleanup.

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::Command;

use nodex_core::error::Error as CoreError;

/// Verify the path is inside a git work tree. Surfaces `GIT_ERROR`
/// when git is unavailable or the directory isn't a work tree — the
/// JSON envelope therefore distinguishes "operator is in the wrong
/// place" from a generic runtime failure (and from a `nodex.toml`
/// validation problem, which uses `CONFIG_ERROR`).
pub fn ensure_work_tree(root: &Path, who: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(root)
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

/// True if `root` is inside a git work tree. The non-erroring sibling
/// of [`ensure_work_tree`], for callers that treat absence as "skip"
/// rather than "fail" — e.g. the default immutability baseline, which
/// simply leaves the diff-aware rules to self-report as skipped when
/// there is no git history to diff against.
pub fn is_work_tree(root: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(root)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
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
    // Surface the baseline build's own warnings, ref-tagged. A document
    // that fails to parse AT the baseline vanishes from the before graph,
    // so it looks "added" and the diff-aware immutability rules silently
    // do not fire for it — `check --since`/default check would pass on a
    // lock it never actually enforced. Carrying the warning to the
    // envelope keeps that from being invisible.
    let warnings = before_result
        .warnings
        .into_iter()
        .map(|w| format!("baseline {git_ref}: {w}"))
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
    pub warnings: Vec<String>,
}

/// Resolve the configured `rules.immutable_baseline` into the diff a
/// default `check` runs under, or `None` when the baseline cannot apply
/// (not configured, no immutability rules to feed, or not a git work
/// tree — where those rules are inert).
pub fn baseline_diff(
    root: &Path,
    config: &nodex_core::Config,
    current: &nodex_core::Graph,
    scratch_name: &str,
) -> Result<Option<BaselineDiff>> {
    let Some(baseline) = config.rules.immutable_baseline.as_deref() else {
        return Ok(None);
    };
    if !config.has_immutable_rules() || !is_work_tree(root) {
        return Ok(None);
    }
    Ok(Some(diff_against_ref(
        root,
        baseline,
        config,
        current,
        scratch_name,
    )?))
}

/// The bytes of `rel_path` as committed at `git_ref`, or `None` when the
/// path does not exist there (or git is unavailable). The baseline view
/// the rewrite-lock probe diffs against: it lets rename/retarget compute
/// exactly what a `check` against `immutable_baseline` would — the
/// before-snapshot status and body fingerprint — so the probe skips a
/// rewrite iff `check` would flag it. Callers gate on [`is_work_tree`]
/// first; outside a work tree the diff-aware immutability rules are
/// inert and nothing is locked.
pub fn ref_file_content(root: &Path, git_ref: &str, rel_path: &Path) -> Option<String> {
    let output = Command::new("git")
        .args([
            "show",
            &format!(
                "{git_ref}:{}",
                nodex_core::path_guard::forward_string(rel_path)
            ),
        ])
        .current_dir(root)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
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
        let output = match Command::new("git")
            .args([
                "worktree",
                "add",
                "--detach",
                target.to_str().expect("utf-8 path"),
                git_ref,
            ])
            .current_dir(repo_root)
            .output()
        {
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
        let _ = Command::new("git")
            .args([
                "worktree",
                "remove",
                "--force",
                self.path.to_str().unwrap_or_default(),
            ])
            .current_dir(&self.repo_root)
            .output();
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
