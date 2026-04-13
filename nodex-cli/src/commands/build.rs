use anyhow::{Context, Result};
use std::path::Path;
use std::time::Instant;

use nodex_core::config::Config;

use crate::format::{Envelope, print_json};

pub fn run(root: &Path, full: bool, pretty: bool) -> Result<()> {
    let config = Config::load(root).context("failed to load config")?;
    let start = Instant::now();

    let result = nodex_core::builder::build(root, &config, full).context("graph build failed")?;

    let duration_ms = start.elapsed().as_millis() as u64;

    // Write outputs
    let output_dir = root.join(&config.output.dir);
    nodex_core::output::json::write_json_outputs(&result.graph, &output_dir)
        .context("failed to write JSON outputs")?;

    #[derive(serde::Serialize)]
    struct BuildOutput {
        nodes: usize,
        edges: usize,
        cached: usize,
        parsed: usize,
        duration_ms: u64,
    }

    let envelope = if result.stats.warnings.is_empty() {
        Envelope::success(BuildOutput {
            nodes: result.stats.nodes,
            edges: result.stats.edges,
            cached: result.stats.cached,
            parsed: result.stats.parsed,
            duration_ms,
        })
    } else {
        Envelope::with_warnings(
            BuildOutput {
                nodes: result.stats.nodes,
                edges: result.stats.edges,
                cached: result.stats.cached,
                parsed: result.stats.parsed,
                duration_ms,
            },
            result.stats.warnings,
        )
    };

    print_json(&envelope, pretty);
    Ok(())
}
