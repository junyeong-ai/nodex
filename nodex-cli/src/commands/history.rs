use anyhow::Result;
use clap::Args;
use std::path::Path;

use crate::format::emit_read_with;

use super::git_checkout::ensure_repository;

#[derive(Args)]
pub struct HistoryArgs {
    /// Node identifier; omitted with `--path` too, every document whose id the
    /// working tree no longer holds
    #[arg(value_name = "ID", conflicts_with = "path")]
    pub id: Option<String>,
    /// Project-relative file path (mutually exclusive with `<id>`)
    #[arg(long, value_name = "FILE")]
    pub path: Option<String>,
}

/// `nodex history` — the documents history recorded at a path the project no
/// longer holds: under an id, at a path, or every one whose id is gone.
pub fn run(root: &Path, args: HistoryArgs, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let repository = ensure_repository(root, "nodex history")?;
    let path = args
        .path
        .map(|path| nodex_core::path_guard::normalize_for_lookup(&path, root))
        .transpose()?;
    let subject = match (args.id.as_deref(), path.as_deref()) {
        (Some(id), None) => Some(nodex_core::HistorySubject::Id(id)),
        (None, Some(path)) => Some(nodex_core::HistorySubject::Path(path)),
        (None, None) => None,
        (Some(_), Some(_)) => unreachable!("clap refuses <id> beside --path"),
    };
    let outcome = nodex_core::find_history(root, &config, &repository, subject)?;
    emit_read_with(outcome.report, outcome.warnings, &config, pretty);
    Ok(())
}
