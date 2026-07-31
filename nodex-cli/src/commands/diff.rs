use anyhow::Result;
use clap::Args;
use std::path::Path;

use crate::format::{Envelope, print_json};

use super::git_worktree::{Worktree, ensure_repository, ref_omissions, scratch_dir};

/// Args for `nodex diff`.
#[derive(Args)]
pub struct DiffArgs {
    /// The "before" git ref (commit, branch, tag).
    pub before: String,
    /// The "after" git ref.
    pub after: String,
}

pub fn run(root: &Path, args: DiffArgs, pretty: bool) -> Result<()> {
    let repository = ensure_repository(root, "nodex diff")?;

    let scratch = scratch_dir(root, ".nodex-diff")?;
    let before_target = scratch.join("before");
    let after_target = scratch.join("after");

    // The first Worktree owns the scratch root; the second piggy-backs
    // on it. Both worktrees are removed on drop; the scratch directory
    // is removed by the first guard.
    let before = Worktree::add(
        &repository,
        &args.before,
        &before_target,
        Some(scratch.clone()),
    )?;
    let after = Worktree::add(&repository, &args.after, &after_target, None)?;
    // A checkout carries the whole repository; the project is graphed at
    // its own location inside it. Both sides are required here — a ref
    // that does not carry the project has nothing to compare.
    let before_root = before.require_project_root()?;
    let after_root = after.require_project_root()?;

    // Single-lens semantics: the *after* ref's config is the one lens —
    // both snapshots are graphed under it and the before ref supplies
    // content only. A diff is a question asked from the newer contract,
    // and per-ref configs would deadlock the exact PR that migrates the
    // config format (the before ref's config no longer parses under the
    // new binary). The after side still validates its own config, so a
    // genuinely broken target ref surfaces as CONFIG_ERROR.
    let after_config = nodex_core::load_project(after_root)?;
    // Each side is graphed as what its ref *records* and nothing else. An
    // unconfined build follows a link out of the checkout into the live
    // filesystem, so content neither ref carries enters the comparison and is
    // reported as history: a symlink whose target changed between the refs
    // yields field changes that happened outside the repository entirely.
    let before_build =
        nodex_core::builder::build_of_ref(before_root, before.checkout(), &after_config)?;
    let after_build =
        nodex_core::builder::build_of_ref(after_root, after.checkout(), &after_config)?;

    let diff = nodex_core::diff::compute_diff(&before_build.graph, &after_build.graph);
    // A ref-to-ref diff doesn't depend on the current working-tree
    // config — but if it loads cleanly we still surface the binary-compat
    // advisory. Best-effort: a broken/absent current `nodex.toml` must
    // never fail a diff between two valid refs.
    let mut warnings: Vec<nodex_core::Warning> = nodex_core::Config::load(root)
        .ok()
        .and_then(|c| nodex_core::binary_compat_warning(&c))
        .into_iter()
        .collect();
    // What each ref build could not read. A delta reads as the whole story,
    // and a document omitted from one side reads as added or removed while a
    // document omitted from both reads as no change at all.
    warnings.extend(ref_omissions(&args.before, &before_build));
    warnings.extend(ref_omissions(&args.after, &after_build));
    print_json(&Envelope::with_warnings(diff, warnings), pretty);

    Ok(())
}
