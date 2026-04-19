use anyhow::{Context, Result};
use clap::Args;
use std::path::{Path, PathBuf};

use nodex_core::config::Config;
use nodex_core::model::Kind;
use nodex_core::scaffold::{self, ScaffoldSpec};

use crate::format::{Envelope, print_json};

use super::query::load_graph;

/// Flags accepted by `nodex scaffold`. Grouped into one `Args` struct
/// so clap generates the same `--kind` / `--title` / … flags while
/// the handler stays a two-parameter call, matching the shape of the
/// other command handlers.
#[derive(Args)]
pub struct ScaffoldArgs {
    /// Document kind (must be in config.kinds.allowed)
    #[arg(long)]
    pub kind: String,
    /// Document title (free-form; also used to slugify the filename)
    #[arg(long)]
    pub title: String,
    /// Override the auto-inferred node id
    #[arg(long)]
    pub id: Option<String>,
    /// Override the auto-inferred path (relative to root)
    #[arg(long)]
    pub path: Option<PathBuf>,
    /// Print the plan as JSON without writing the file
    #[arg(long)]
    pub dry_run: bool,
    /// Overwrite existing file at the target path
    #[arg(long)]
    pub force: bool,
}

pub fn run(root: &Path, args: ScaffoldArgs, pretty: bool) -> Result<()> {
    let config = Config::load(root)?;
    let graph = load_graph(root, &config).context(
        "graph.json not found. Run `nodex build` first so scaffold can \
         detect id collisions and next sequence numbers.",
    )?;

    let spec = ScaffoldSpec {
        kind: Kind::new(&args.kind),
        title: args.title,
        id: args.id,
        path: args.path,
    };

    let result = scaffold::scaffold(root, spec, &graph, &config, !args.dry_run, args.force)?;
    print_json(&Envelope::success(result), pretty);
    Ok(())
}
