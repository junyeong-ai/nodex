use anyhow::{Context, Result};
use std::path::Path;

use nodex_core::config::Config;
use nodex_core::rules::{self, Severity};

use crate::format::{Envelope, print_json};

pub fn run(root: &Path, severity_filter: Option<Severity>, pretty: bool) -> Result<()> {
    let config = Config::load(root)?;

    // Build graph first
    let result = nodex_core::builder::build(root, &config, false).context("graph build failed")?;

    let violations = rules::check_all(&result.graph, &config);

    let filtered: Vec<_> = match severity_filter {
        Some(target) => violations
            .into_iter()
            .filter(|v| v.severity == target)
            .collect(),
        None => violations,
    };

    let has_errors = filtered.iter().any(|v| v.severity == Severity::Error);

    print_json(
        &Envelope::success(serde_json::json!({
            "violations": filtered,
            "total": filtered.len(),
            "has_errors": has_errors,
        })),
        pretty,
    );

    if has_errors {
        std::process::exit(1);
    }

    Ok(())
}
