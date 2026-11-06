use anyhow::Result;
use clap::Args;
use std::path::Path;

use crate::format::emit_read_with;

use super::git_checkout::ensure_repository;

#[derive(Args)]
#[command(group(
    clap::ArgGroup::new("history_subject")
        .required(true)
        .multiple(false)
        .args(["id", "path"])
))]
pub struct HistoryArgs {
    /// Node identifier (mutually exclusive with `--path`)
    #[arg(value_name = "ID")]
    pub id: Option<String>,
    /// Project-relative file path (mutually exclusive with `<id>`)
    #[arg(long, value_name = "FILE")]
    pub path: Option<String>,
}

/// `nodex history` — the documents a commit `HEAD` reaches held under an id,
/// or at a path, that `HEAD` no longer holds.
pub fn run(root: &Path, args: HistoryArgs, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let repository = ensure_repository(root, "nodex history")?;
    let path = args
        .path
        .map(|path| nodex_core::path_guard::normalize_for_lookup(&path, root))
        .transpose()?;
    let subject = match (args.id.as_deref(), path.as_deref()) {
        (Some(id), None) => nodex_core::HistorySubject::Id(id),
        (None, Some(path)) => nodex_core::HistorySubject::Path(path),
        _ => unreachable!("clap ArgGroup enforces exactly one of <id> or --path"),
    };
    let outcome = nodex_core::find_history(root, &config, &repository, subject)?;
    emit_read_with(outcome.report, outcome.warnings, &config, pretty);
    Ok(())
}
