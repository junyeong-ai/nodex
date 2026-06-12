//! Byte-level git access for the mutation seams.
//!
//! Core already shells to git for the drift probes
//! (`rules::git_drift::commits_since` / `probe_environment`); this
//! module is the one seam for the two byte-level questions the
//! immutability guards ask — "is this project a git work tree?" and
//! "what were this document's bytes at a ref?". Richer materialisation
//! (disposable worktrees for whole-graph diffs) stays a CLI concern in
//! `commands/git_worktree.rs`.

use std::path::Path;
use std::process::Command;

/// True if `root` is inside a git work tree. A non-erroring probe for
/// callers that treat absence as "skip" rather than "fail" — e.g. the
/// immutability baseline, which simply leaves the diff-aware rules to
/// self-report as skipped when there is no git history to diff against.
pub fn is_work_tree(root: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(root)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The bytes of `rel_path` as committed at `git_ref`, or `None` when the
/// path does not exist there (or git is unavailable). The baseline view
/// the rewrite-lock probes diff against: it lets a write seam compute
/// exactly what a `check` against `rules.immutable_baseline` would —
/// the before-snapshot status and body fingerprint — so the seam skips
/// or refuses a mutation iff `check` would flag it. Callers gate on
/// [`is_work_tree`] first ([`crate::mutate::BaselineProbe`] does);
/// outside a work tree the diff-aware immutability rules are inert and
/// nothing is locked.
pub fn ref_file_content(root: &Path, git_ref: &str, rel_path: &Path) -> Option<String> {
    let output = Command::new("git")
        .args([
            "show",
            &format!("{git_ref}:{}", crate::path_guard::forward_string(rel_path)),
        ])
        .current_dir(root)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}
