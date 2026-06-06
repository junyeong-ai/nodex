use anyhow::{Context, Result};
use clap::Args;
use std::path::{Path, PathBuf};

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
    let config = nodex_core::load_project(root)?;
    // The version pin gates *writing*, not previewing — a dry-run only
    // reads, so it carries the advisory like any read; a real write is
    // refused on an incompatible binary.
    let mut warnings = Vec::new();
    if args.dry_run {
        warnings.extend(nodex_core::binary_compat_warning(&config));
    } else {
        nodex_core::ensure_binary_compatible(&config)?;
    }
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

    let (result, scaffold_warnings) =
        scaffold::scaffold(root, spec, &graph, &config, !args.dry_run, args.force)?;
    warnings.extend(scaffold_warnings);
    print_json(&Envelope::with_warnings(result, warnings), pretty);
    Ok(())
}
