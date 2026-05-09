use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use std::path::Path;

use nodex_core::rules::{self, Severity};

use crate::format::{Envelope, print_json};

/// Severity filter accepted by `nodex check --severity`.
/// Maps 1:1 to [`nodex_core::rules::Severity`] at the command boundary
/// so the CLI layer owns its clap-specific vocabulary and core stays
/// free of clap as a dependency.
#[derive(Clone, Copy, ValueEnum)]
pub enum CheckSeverity {
    Error,
    Warning,
}

impl From<CheckSeverity> for Severity {
    fn from(s: CheckSeverity) -> Self {
        match s {
            CheckSeverity::Error => Self::Error,
            CheckSeverity::Warning => Self::Warning,
        }
    }
}

/// Args for `nodex check`.
#[derive(Args)]
pub struct CheckArgs {
    /// Filter by severity.
    #[arg(long, value_enum)]
    pub severity: Option<CheckSeverity>,
}

pub fn run(root: &Path, args: CheckArgs, pretty: bool) -> Result<()> {
    let severity_filter = args.severity.map(Severity::from);
    let config = nodex_core::load_project(root)?;

    // Build graph first
    let result = nodex_core::builder::build(root, &config, false).context("graph build failed")?;

    let violations = rules::check_all(&result.graph, &config, root);

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
