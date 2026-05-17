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
        let output = Command::new("git")
            .args([
                "worktree",
                "add",
                "--detach",
                target.to_str().expect("utf-8 path"),
                git_ref,
            ])
            .current_dir(repo_root)
            .output()
            .map_err(|e| CoreError::Git {
                context: format!("could not invoke `git worktree add` for {git_ref:?}"),
                stderr: e.to_string(),
            })?;
        if !output.status.success() {
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
