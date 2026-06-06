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

    let mut updated = Vec::new();
    for node in graph.nodes().values() {
        let abs = root.join(&node.path);
        // Writer-skips-symlinks: rewriting through a symlink could mutate a
        // file outside the project root.
        if nodex_core::path_guard::is_symlink(&abs) {
            continue;
        }
        let content = match std::fs::read_to_string(&abs) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if let Some(rewritten) = nodex_core::retarget::retarget_document(
            &content,
            node,
            &args.old_id,
            &args.new_id,
            &config.parser,
        )? {
            nodex_core::path_guard::write_atomic(&abs, &rewritten)?;
            updated.push(nodex_core::path_guard::forward_string(&node.path));
        }
    }

    let data = RetargetResult {
        old_id: args.old_id,
        new_id: args.new_id,
        total_updated: updated.len(),
        references_updated: updated,
    };
    print_json(&Envelope::success(data), pretty);

    Ok(())
}
