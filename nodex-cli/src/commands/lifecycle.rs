use anyhow::{Context, Result};
use std::path::Path;

use nodex_core::config::Config;
use nodex_core::error::Error as CoreError;
use nodex_core::lifecycle::{self, Action};

use crate::format::{Envelope, print_json};

pub fn run(
    root: &Path,
    action_str: &str,
    node_id: &str,
    successor: Option<&str>,
    pretty: bool,
) -> Result<()> {
    let action = match action_str {
        "supersede" => Action::Supersede,
        "archive" => Action::Archive,
        "deprecate" => Action::Deprecate,
        "abandon" => Action::Abandon,
        "review" => Action::Review,
        _ => anyhow::bail!("unknown action: {action_str}"),
    };

    // Validate --to is provided for supersede
    if matches!(action, Action::Supersede) && successor.is_none() {
        anyhow::bail!("--to <successor_id> is required for supersede action");
    }

    let config = Config::load(root)?;

    let result = nodex_core::builder::build(root, &config, false).context("graph build failed")?;

    let node = result
        .graph
        .node(node_id)
        .ok_or_else(|| CoreError::NodeNotFound(node_id.to_string()))?;

    let rel_path = node.path.clone();

    lifecycle::transition(root, &rel_path, action, successor, &config)
        .context("lifecycle transition failed")?;

    #[derive(serde::Serialize)]
    struct LifecycleOutput {
        node_id: String,
        action: String,
        path: String,
    }

    print_json(
        &Envelope::success(LifecycleOutput {
            node_id: node_id.to_string(),
            action: action_str.to_string(),
            path: rel_path.to_string_lossy().to_string(),
        }),
        pretty,
    );

    Ok(())
}
