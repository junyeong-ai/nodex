use anyhow::{Context, Result};
use chrono::NaiveDate;
use std::path::Path;

use nodex_core::command_result::ReportResult;

use crate::format::emit_read_with;

pub fn run(root: &Path, pretty: bool, today: NaiveDate) -> Result<()> {
    let config = nodex_core::load_project(root)?;

    let result = nodex_core::builder::build(root, &config, false).context("graph build failed")?;

    let output_dir = root.join(&config.output.dir);

    let md = nodex_core::output::markdown::render_markdown(&result.graph, &config, today);
    nodex_core::path_guard::write_atomic_in_root(root, &output_dir.join("GRAPH.md"), &md)
        .context("failed to write Markdown report")?;

    // The graph these artefacts render carries what the walk could not read,
    // so a report of a partial corpus says which part it is missing.
    emit_read_with(
        ReportResult {
            generated: vec!["GRAPH.md".into()],
            // Forward-slashed like every other path nodex emits: the JSON
            // contract's path language is the same on every platform, and a
            // consumer joining this against a document path must not have to
            // reconcile two separators.
            output_dir: nodex_core::path_guard::forward_string(&output_dir),
        },
        result.warnings.clone(),
        &config,
        pretty,
    );

    Ok(())
}
