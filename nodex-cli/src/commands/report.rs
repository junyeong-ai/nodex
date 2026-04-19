use anyhow::{Context, Result};
use std::path::Path;

use nodex_core::config::Config;

use crate::format::{Envelope, print_json};

pub fn run(root: &Path, format: Option<String>, pretty: bool) -> Result<()> {
    let config = Config::load(root)?;

    let result = nodex_core::builder::build(root, &config, false).context("graph build failed")?;

    let output_dir = root.join(&config.output.dir);
    std::fs::create_dir_all(&output_dir).map_err(|source| nodex_core::error::Error::Io {
        path: output_dir.clone(),
        source,
    })?;

    let format = format.unwrap_or_else(|| "all".to_string());
    let mut generated = Vec::new();

    if format == "json" || format == "all" {
        nodex_core::output::json::write_json_outputs(&result.graph, &output_dir)
            .context("failed to write JSON outputs")?;
        generated.push("graph.json");
        generated.push("backlinks.json");
    }

    if format == "md" || format == "all" {
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
