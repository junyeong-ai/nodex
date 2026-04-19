use anyhow::{Context, Result};
use clap::ValueEnum;
use std::path::Path;

use nodex_core::config::Config;

use crate::format::{Envelope, print_json};

/// Output format selector for `nodex report --format`.
#[derive(Clone, Copy, ValueEnum)]
pub enum ReportFormat {
    /// Only GRAPH.md
    Md,
    /// Only graph.json + backlinks.json
    Json,
    /// All of the above (default)
    All,
}

impl ReportFormat {
    fn writes_json(self) -> bool {
        matches!(self, Self::Json | Self::All)
    }
    fn writes_md(self) -> bool {
        matches!(self, Self::Md | Self::All)
    }
}

pub fn run(root: &Path, format: ReportFormat, pretty: bool) -> Result<()> {
    let config = Config::load(root)?;

    let result = nodex_core::builder::build(root, &config, false).context("graph build failed")?;

    let output_dir = root.join(&config.output.dir);
    std::fs::create_dir_all(&output_dir).map_err(|source| nodex_core::error::Error::Io {
        path: output_dir.clone(),
        source,
    })?;

    let mut generated = Vec::new();

    if format.writes_json() {
        nodex_core::output::json::write_json_outputs(&result.graph, &output_dir)
            .context("failed to write JSON outputs")?;
        generated.push("graph.json");
        generated.push("backlinks.json");
    }

    if format.writes_md() {
        let md = nodex_core::output::markdown::render_markdown(&result.graph, &config);
        let md_path = output_dir.join("GRAPH.md");
        std::fs::write(&md_path, &md).map_err(|source| nodex_core::error::Error::Io {
            path: md_path.clone(),
            source,
        })?;
        generated.push("GRAPH.md");
    }

    print_json(
        &Envelope::success(serde_json::json!({
            "generated": generated,
            "output_dir": output_dir.to_string_lossy(),
        })),
        pretty,
    );

    Ok(())
}
