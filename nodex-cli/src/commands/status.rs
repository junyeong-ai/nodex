use anyhow::Result;
use std::path::Path;

use crate::format::emit_read;

/// `nodex status` — report the graph snapshot's machine-coded state
/// (`absent` / `unreadable` / `schema_mismatch` / `outdated` /
/// `current`) with the full content probe. A probe, not a gate: exit 0
/// whenever the probe itself runs (the `query issues` precedent) — CI
/// gates dispatch on `data.state`.
pub fn run(root: &Path, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let report = nodex_core::compute_status(root, &config)?;
    emit_read(report, &config, pretty);
    Ok(())
}
