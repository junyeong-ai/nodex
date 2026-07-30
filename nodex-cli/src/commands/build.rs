use anyhow::{Context, Result};
use clap::Args;
use std::path::Path;
use std::time::Instant;

use crate::format::emit_read_with;

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
    nodex_core::output::json::write_json_outputs(root, &result.graph, &output_dir)
        .context("failed to write JSON outputs")?;

    let data = nodex_core::BuildResult {
        nodes: result.stats.nodes,
        edges: result.stats.edges,
        annotations: result.stats.annotations,
        body_line_matches: result.stats.body_line_matches,
        cached: result.stats.cached,
        parsed: result.stats.parsed,
        duration_ms,
        conditionally_excluded: result.conditionally_excluded,
        dangling_paths: result.dangling_paths,
        parse_failures: result.graph.parse_failures().to_vec(),
    };
    emit_read_with(data, result.warnings, &config, pretty);
    Ok(())
}
