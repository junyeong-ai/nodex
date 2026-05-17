use anyhow::Result;
use clap::Args;
use std::path::Path;

use crate::format::{Envelope, print_json};

use super::git_worktree::{Worktree, ensure_work_tree, scratch_dir};

/// Args for `nodex diff`.
#[derive(Args)]
pub struct DiffArgs {
    /// The "before" git ref (commit, branch, tag).
    pub before: String,
    /// The "after" git ref.
    pub after: String,
}

pub fn run(root: &Path, args: DiffArgs, pretty: bool) -> Result<()> {
    ensure_work_tree(root, "nodex diff")?;

    let scratch = scratch_dir(root, ".nodex-diff")?;
    let before_target = scratch.join("before");
    let after_target = scratch.join("after");

    // The first Worktree owns the scratch root; the second piggy-backs
    // on it. Both worktrees are removed on drop; the scratch directory
    // is removed by the first guard.
    let before = Worktree::add(root, &args.before, &before_target, Some(scratch.clone()))?;
    let after = Worktree::add(root, &args.after, &after_target, None)?;

    let before_graph = build_at(before.path())?;
    let after_graph = build_at(after.path())?;

    let diff = nodex_core::diff::compute_diff(&before_graph, &after_graph);
    print_json(&Envelope::success(diff), pretty);

    Ok(())
}

fn build_at(root: &Path) -> Result<nodex_core::Graph> {
    let config = nodex_core::load_project(root)?;
    let result = nodex_core::builder::build(root, &config, true)?;
    Ok(result.graph)
}
