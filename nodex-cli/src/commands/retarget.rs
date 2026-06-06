use anyhow::{Context, Result};
use clap::Args;
use std::path::Path;

use nodex_core::command_result::RetargetResult;
use nodex_core::error::Error as CoreError;

use crate::format::{Envelope, print_json};

/// Args for `nodex retarget`.
#[derive(Args)]
pub struct RetargetArgs {
    /// The id being replaced — references to it are repointed.
    pub old_id: String,
    /// The successor id those references should point to.
    pub new_id: String,
}

pub fn run(root: &Path, args: RetargetArgs, pretty: bool) -> Result<()> {
    if args.old_id == args.new_id {
        return Err(CoreError::Config(
            "old-id and new-id are the same; nothing to retarget".into(),
        )
        .into());
    }

    let config = nodex_core::load_project_for_mutation(root)?;
    // Build to read each document's parsed relation fields. Both endpoints
    // must exist: repointing to or from an unknown id is a typo, not an
    // operation.
    let outcome = nodex_core::builder::build(root, &config, true).context("build failed")?;
    let graph = outcome.graph;
    graph.require_node(&args.old_id)?;
    graph.require_node(&args.new_id)?;

    // Every in-scope file path, so id retargeting can honour the
    // resolver's path-first precedence: a `[[old]]` that binds a file
    // by path is not an id reference and must be left alone.
    let in_scope: std::collections::BTreeSet<String> = graph
        .nodes()
        .values()
        .map(|n| nodex_core::path_guard::forward_string(&n.path))
        .collect();

    let mut updated = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for node in graph.nodes().values() {
        let abs = root.join(&node.path);
        let retarget = |content: &str| {
            nodex_core::retarget::retarget_document(
                content,
                node,
                &args.old_id,
                &args.new_id,
                &in_scope,
                &config.parser,
            )
        };

        // Writer-skips / reader-follows: read through a symlink to detect an
        // un-repointed reference, but never write through it (the target may
        // escape the project root). Surface the skip only when the file
        // actually references the old id, so the operator can fix it manually.
        if nodex_core::path_guard::is_symlink(&abs) {
            if let Ok(content) = std::fs::read_to_string(&abs)
                && retarget(&content)?.is_some()
            {
                skipped.push(format!(
                    "{} references {} but is a symlink; it was not repointed \
                     (writing through a symlink could escape the project root) — update it manually",
                    nodex_core::path_guard::forward_string(&node.path),
                    args.old_id
                ));
            }
            continue;
        }
        let content = match std::fs::read_to_string(&abs) {
            Ok(c) => c,
            Err(e) => {
                skipped.push(format!(
                    "could not read in-scope file {}: {e}",
                    nodex_core::path_guard::forward_string(&node.path)
                ));
                continue;
            }
        };
        if let Some(rewritten) = retarget(&content)? {
            nodex_core::path_guard::write_atomic_in_root(root, &abs, &rewritten)?;
            updated.push(nodex_core::path_guard::forward_string(&node.path));
        }
    }

    let data = RetargetResult {
        old_id: args.old_id,
        new_id: args.new_id,
        total_updated: updated.len(),
        references_updated: updated,
    };
    if skipped.is_empty() {
        print_json(&Envelope::success(data), pretty);
    } else {
        print_json(&Envelope::with_warnings(data, skipped), pretty);
    }

    Ok(())
}
