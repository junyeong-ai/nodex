use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use std::path::Path;

use nodex_core::command_result::ReportResult;

use crate::format::{Envelope, print_json};

/// Output format selector for `nodex report --format`.
#[derive(Clone, Copy, ValueEnum)]
pub enum ReportFormat {
    /// Only GRAPH.md
    Md,
    /// Only graph.json
    Json,
    /// Both GRAPH.md and graph.json (default)
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

/// Args for `nodex report`.
#[derive(Args)]
pub struct ReportArgs {
    /// Output format.
    #[arg(long, value_enum, default_value_t = ReportFormat::All)]
    pub format: ReportFormat,
}

pub fn run(root: &Path, args: ReportArgs, pretty: bool) -> Result<()> {
    let format = args.format;
    let config = nodex_core::load_project(root)?;

    let result = nodex_core::builder::build(root, &config, false).context("graph build failed")?;

    let output_dir = root.join(&config.output.dir);

    let mut generated = Vec::new();

    if format.writes_json() {
        // write_json_outputs creates the parent directory through the
        // shared atomic-write primitive.
        nodex_core::output::json::write_json_outputs(&result.graph, &output_dir)
            .context("failed to write JSON outputs")?;
        generated.push("graph.json");
    }

    if format.writes_md() {
        let md = nodex_core::output::markdown::render_markdown(&result.graph, &config);
        let md_path = output_dir.join("GRAPH.md");
        nodex_core::path_guard::write_atomic(&md_path, &md)?;
        generated.push("GRAPH.md");
    }

    print_json(
        &Envelope::success(ReportResult {
            generated: generated.into_iter().map(String::from).collect(),
            output_dir: output_dir.to_string_lossy().into_owned(),
        }),
        pretty,
    );

    Ok(())
}
