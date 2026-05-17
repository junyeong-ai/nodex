use anyhow::{Context, Result};
use clap::Args;
use std::path::Path;
use std::time::Instant;

use crate::format::{Envelope, print_json};

/// Args for `nodex build`.
#[derive(Args)]
pub struct BuildArgs {
    /// Force full rebuild (ignore cache).
    #[arg(long)]
    pub full: bool,
}

pub fn run(root: &Path, args: BuildArgs, pretty: bool) -> Result<()> {
    let full = args.full;
    let config = nodex_core::load_project(root).context("failed to load config")?;
    let start = Instant::now();

    let result = nodex_core::builder::build(root, &config, full).context("graph build failed")?;

    let duration_ms = start.elapsed().as_millis() as u64;

    // Write outputs
    let output_dir = root.join(&config.output.dir);
    nodex_core::output::json::write_json_outputs(&result.graph, &output_dir)
        .context("failed to write JSON outputs")?;

    let data = nodex_core::BuildResult {
        nodes: result.stats.nodes,
        edges: result.stats.edges,
        annotations: result.stats.annotations,
        body_line_matches: result.stats.body_line_matches,
        cached: result.stats.cached,
        parsed: result.stats.parsed,
        duration_ms,
    };
    // `with_warnings` collapses to the same JSON as `success` when the
    // vec is empty (`#[serde(skip_serializing_if = "Vec::is_empty")]`),
    // so a single branch covers both paths.
    print_json(&Envelope::with_warnings(data, result.warnings), pretty);
    Ok(())
}
