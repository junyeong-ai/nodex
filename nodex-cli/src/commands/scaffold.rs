use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use nodex_core::config::Config;
use nodex_core::model::Kind;
use nodex_core::scaffold::{self, ScaffoldSpec};

use crate::format::{Envelope, print_json};

use super::query::load_graph;

#[allow(clippy::too_many_arguments)]
pub fn run(
    root: &Path,
    kind: &str,
    title: &str,
    id: Option<String>,
    path: Option<PathBuf>,
    dry_run: bool,
    force: bool,
    pretty: bool,
) -> Result<()> {
    let config = Config::load(root)?;
    let graph = load_graph(root, &config).context(
        "graph.json not found. Run `nodex build` first so scaffold can \
         detect id collisions and next sequence numbers.",
    )?;

    let spec = ScaffoldSpec {
        kind: Kind::new(kind),
        title: title.to_string(),
        id,
        path,
    };

    let result = scaffold::scaffold(root, spec, &graph, &config, !dry_run, force)?;
    print_json(&Envelope::success(result), pretty);
    Ok(())
}
