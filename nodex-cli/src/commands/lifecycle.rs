use anyhow::{Context, Result};
use clap::Subcommand;
use std::path::Path;

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

    fn action(&self) -> Action {
        match self {
            Self::Supersede { to, .. } => Action::Supersede {
                successor: to.clone(),
            },
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

    let config = nodex_core::load_project(root)?;
    let result = nodex_core::builder::build(root, &config, false).context("graph build failed")?;

    let rel_path = result.graph.require_node(&node_id)?.path.clone();

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
            path: nodex_core::path_guard::forward_string(&rel_path),
        }),
        pretty,
    );

    Ok(())
}
