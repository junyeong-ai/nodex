use anyhow::{Context, Result};
use clap::Subcommand;
use std::path::Path;

use nodex_core::config::Config;
use nodex_core::error::Error as CoreError;
use nodex_core::lifecycle::{self, Action};

use crate::format::{Envelope, print_json};

/// Lifecycle subcommands. Each variant carries exactly the arguments
/// its action needs, so clap enforces at parse time — `supersede`
/// cannot be invoked without `--to`, and the other actions cannot
/// receive a stray `--to`.
#[derive(Subcommand)]
pub enum LifecycleCommand {
    /// Mark a node superseded by another
    Supersede {
        id: String,
        /// Successor node ID
        #[arg(long)]
        to: String,
    },
    /// Archive a node
    Archive { id: String },
    /// Mark a node deprecated
    Deprecate { id: String },
    /// Mark a node abandoned
    Abandon { id: String },
    /// Refresh the reviewed date on a node
    Review { id: String },
}

impl LifecycleCommand {
    fn node_id(&self) -> &str {
        match self {
            Self::Supersede { id, .. }
            | Self::Archive { id }
            | Self::Deprecate { id }
            | Self::Abandon { id }
            | Self::Review { id } => id,
        }
    }

    fn action(&self) -> Action<'_> {
        match self {
            Self::Supersede { to, .. } => Action::Supersede { successor: to },
            Self::Archive { .. } => Action::Archive,
            Self::Deprecate { .. } => Action::Deprecate,
            Self::Abandon { .. } => Action::Abandon,
            Self::Review { .. } => Action::Review,
        }
    }
}

pub fn run(root: &Path, cmd: LifecycleCommand, pretty: bool) -> Result<()> {
    let node_id = cmd.node_id().to_string();
    let action = cmd.action();
    let action_name = action.name();

    let config = Config::load(root)?;
    let result = nodex_core::builder::build(root, &config, false).context("graph build failed")?;

    let node = result
        .graph
        .node(&node_id)
        .ok_or_else(|| CoreError::NodeNotFound(node_id.clone()))?;
    let rel_path = node.path.clone();

    lifecycle::transition(root, &rel_path, action, &config)
        .context("lifecycle transition failed")?;

    #[derive(serde::Serialize)]
    struct LifecycleOutput {
        node_id: String,
        action: String,
        path: String,
    }

    print_json(
        &Envelope::success(LifecycleOutput {
            node_id,
            action: action_name.to_string(),
            path: rel_path.to_string_lossy().to_string(),
        }),
        pretty,
    );

    Ok(())
}
