use anyhow::{Context, Result};
use clap::Subcommand;
use std::path::Path;

use nodex_core::command_result::LifecycleResult;
use nodex_core::lifecycle::{self, Action};

use crate::format::{Envelope, print_json};

/// Detect the `Action::Supersede` payload so the CLI runs
/// [`lifecycle::check_supersede_safe`] before any mutation. Other
/// actions don't need a graph-aware pre-check; this keeps the seam
/// where lifecycle guarantees "no broken graph written" in one place.
fn supersede_target(action: &Action) -> Option<&str> {
    match action {
        Action::Supersede { successor } => Some(successor.as_str()),
        _ => None,
    }
}

/// Lifecycle subcommands. Each variant carries exactly the arguments
/// its action needs, so clap enforces at parse time — `supersede`
/// cannot be invoked without `--to`, and `set` cannot be invoked
/// without `--status`. Every status other than `superseded` is reached
/// through `set`, whose value is validated against the project's
/// vocabulary at the write seam, so the lifecycle CLI carries no
/// status literals of its own.
#[derive(Subcommand)]
pub enum LifecycleCommand {
    /// Mark a node superseded by another
    Supersede {
        id: String,
        /// Successor node ID
        #[arg(long)]
        to: String,
    },
    /// Set a node's status to any value the project allows
    Set {
        id: String,
        /// Target status — must be in `statuses.allowed` for the node's kind
        #[arg(long)]
        status: String,
    },
    /// Refresh the reviewed date on a node
    Review { id: String },
}

impl LifecycleCommand {
    fn node_id(&self) -> &str {
        match self {
            Self::Supersede { id, .. } | Self::Set { id, .. } | Self::Review { id } => id,
        }
    }

    fn action(&self) -> Action {
        match self {
            Self::Supersede { to, .. } => Action::Supersede {
                successor: to.clone(),
            },
            Self::Set { status, .. } => Action::SetStatus {
                status: status.clone(),
            },
            Self::Review { .. } => Action::Review,
        }
    }
}

pub fn run(root: &Path, cmd: LifecycleCommand, pretty: bool) -> Result<()> {
    let node_id = cmd.node_id().to_string();
    let action = cmd.action();
    let action_name = action.name();

    let config = nodex_core::load_project_for_mutation(root)?;
    let result = nodex_core::builder::build(root, &config, false).context("graph build failed")?;

    let rel_path = result.graph.require_node(&node_id)?.path.clone();

    // Supersede is the only action that needs a graph-aware pre-check:
    // the successor must exist *and* the resulting edge must not close
    // a cycle. Run the check before any frontmatter mutation so a
    // rejected transition leaves the project byte-for-byte unchanged.
    if let Some(successor) = supersede_target(&action) {
        lifecycle::check_supersede_safe(&result.graph, &node_id, successor)?;
    }

    // Baseline snapshot the `frontmatter_immutable` guard inside
    // `transition` diffs against — the same one `rename`/`retarget`
    // consult. Outside a git work tree (or with no `immutable_baseline`)
    // the diff-aware rule is inert, so the probe returns `None` for every
    // path and the transition is unconstrained by immutability.
    let baseline_in_git =
        config.rules.immutable_baseline.is_some() && super::git_worktree::is_work_tree(root);
    let baseline_content = |p: &std::path::Path| -> Option<String> {
        if !baseline_in_git {
            return None;
        }
        super::git_worktree::ref_file_content(
            root,
            config
                .rules
                .immutable_baseline
                .as_deref()
                .expect("guarded by baseline_in_git"),
            p,
        )
    };

    lifecycle::transition(root, &rel_path, action, &config, &baseline_content)
        .context("lifecycle transition failed")?;

    print_json(
        &Envelope::success(LifecycleResult {
            node_id,
            action: action_name.to_string(),
            path: nodex_core::path_guard::forward_string(&rel_path),
        }),
        pretty,
    );

    Ok(())
}
