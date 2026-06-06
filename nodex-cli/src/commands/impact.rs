use anyhow::Result;
use clap::Args;
use std::path::Path;

use crate::format::{Envelope, print_json};

use super::git_worktree::{Worktree, ensure_work_tree, scratch_dir};

/// Args for `nodex impact`.
#[derive(Args)]
pub struct ImpactArgs {
    /// The "before" git ref (commit, branch, tag).
    pub before: String,
    /// The "after" git ref.
    pub after: String,
    /// Bound the transitive dependency walk to N hops (default: unbounded).
    #[arg(long)]
    pub depth: Option<u32>,
    /// Restrict the dependency walk to specific edge relations
    /// (comma-separated; default: every relation).
    #[arg(long, value_delimiter = ',')]
    pub relations: Vec<String>,
}

pub fn run(root: &Path, args: ImpactArgs, pretty: bool) -> Result<()> {
    ensure_work_tree(root, "nodex impact")?;

    if args.depth == Some(0) {
        return Err(nodex_core::error::Error::Config(
            "--depth 0 expands nothing; omit it for an unbounded walk or pass a value >= 1".into(),
        )
        .into());
    }

    // A ref-to-ref impact doesn't depend on the working-tree config — but
    // `--relations` is validated against the project vocabulary, so when it
    // is given a config load failure must surface rather than silently
    // skip validation and accept a typo'd relation.
    let current_config = nodex_core::Config::load(root);
    if !args.relations.is_empty() {
        let config = current_config.as_ref().map_err(|e| {
            nodex_core::error::Error::Config(format!(
                "cannot validate --relations against the project vocabulary: {e}"
            ))
        })?;
        let known = config.known_relations();
        let unknown: Vec<&str> = args
            .relations
            .iter()
            .filter(|r| !known.contains(r.as_str()))
            .map(String::as_str)
            .collect();
        if !unknown.is_empty() {
            let known_sorted: Vec<&str> = known.iter().map(String::as_str).collect();
            return Err(nodex_core::error::Error::Config(format!(
                "--relations contains unknown value(s) {unknown:?}; known: {known_sorted:?}"
            ))
            .into());
        }
    }

    let scratch = scratch_dir(root, ".nodex-impact")?;
    let before = Worktree::add(
        root,
        &args.before,
        &scratch.join("before"),
        Some(scratch.clone()),
    )?;
    let after = Worktree::add(root, &args.after, &scratch.join("after"), None)?;

    let (before_graph, _) = build_at(before.path())?;
    // The after config's extensions recognise extension-less references to a
    // removed file when classifying danglers.
    let (after_graph, after_extensions) = build_at(after.path())?;

    let report = nodex_core::compute_impact(
        &before_graph,
        &after_graph,
        &args.relations,
        args.depth,
        &after_extensions,
    );

    let warnings = current_config
        .ok()
        .and_then(|config| nodex_core::binary_compat_warning(&config))
        .into_iter()
        .collect();
    print_json(&Envelope::with_warnings(report, warnings), pretty);

    Ok(())
}

fn build_at(root: &Path) -> Result<(nodex_core::Graph, Vec<String>)> {
    let config = nodex_core::load_project(root)?;
    let result = nodex_core::builder::build(root, &config, true)?;
    Ok((result.graph, config.parser.extensions))
}
