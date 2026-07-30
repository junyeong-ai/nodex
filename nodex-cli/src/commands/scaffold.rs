use anyhow::Result;
use clap::Args;
use std::path::{Path, PathBuf};

use nodex_core::model::Kind;
use nodex_core::scaffold::{self, ScaffoldSpec};

use crate::format::emit_write;

use super::content_source::read_content_source;

/// Flags accepted by `nodex scaffold`. Grouped into one `Args` struct
/// so clap generates the same `--kind` / `--title` / … flags while
/// the handler stays a two-parameter call, matching the shape of the
/// other command handlers.
#[derive(Args)]
pub struct ScaffoldArgs {
    /// Document kind (must be in config.kinds.allowed)
    #[arg(long)]
    pub kind: String,
    /// Document title (free-form; also used to slugify the filename)
    #[arg(long)]
    pub title: String,
    /// Override the auto-inferred node id
    #[arg(long)]
    pub id: Option<String>,
    /// Override the auto-inferred path (relative to root)
    #[arg(long)]
    pub path: Option<PathBuf>,
    /// Markdown body for the new document — `-` reads stdin, otherwise
    /// a file path resolved against the invoking directory (the same
    /// SOURCE grammar as `check --content`). Supplying it engages the
    /// strict gate: a check violation the document introduces refuses
    /// the scaffold instead of riding as a warning.
    #[arg(long, value_name = "SOURCE")]
    pub body: Option<String>,
    /// Frontmatter field as KEY=VALUE (value is YAML; repeatable).
    /// Rendered after the identity lines. A key whose value has a
    /// canonical source — a dedicated flag, config derivation, or the
    /// structural filesystem path — is refused (the error names the set).
    #[arg(long = "field", value_name = "KEY=VALUE", value_parser = parse_field)]
    pub fields: Vec<(String, String)>,
    /// Print the plan as JSON without writing the file
    #[arg(long)]
    pub dry_run: bool,
    /// Overwrite existing file at the target path
    #[arg(long)]
    pub force: bool,
}

/// Split one `--field KEY=VALUE` pair at clap parse time, so a
/// malformed pair fails as `INVALID_ARGUMENT` — never a runtime error.
fn parse_field(s: &str) -> Result<(String, String), String> {
    let Some((key, value)) = s.split_once('=') else {
        return Err(format!("expected KEY=VALUE, got {s:?}"));
    };
    let key = key.trim();
    if key.is_empty() {
        return Err(format!("field key is empty in {s:?}"));
    }
    Ok((key.to_string(), value.to_string()))
}

pub fn run(root: &Path, args: ScaffoldArgs, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    // The version pin gates *writing*, not previewing — a dry-run only
    // reads, so it carries the advisory like any read; a real write is
    // refused on an incompatible binary.
    let mut warnings = Vec::new();
    if args.dry_run {
        warnings.extend(nodex_core::binary_compat_warning(&config));
    } else {
        nodex_core::ensure_binary_compatible(&config)?;
    }

    let body = args.body.as_deref().map(read_content_source).transpose()?;
    let spec = ScaffoldSpec {
        kind: Kind::new(&args.kind),
        title: args.title,
        id: args.id,
        path: args.path,
        body,
        fields: args.fields,
    };

    // The immutability lock probe, resolved once — inert unless
    // `rules.immutable_baseline` + immutability rules + a git work tree
    // line up. Core scaffold builds its own before-graph live, so no
    // prior `nodex build` (and no graph.json) is involved.
    let probe = super::git_worktree::write_baseline(root, &config)?;
    let (result, scaffold_warnings) =
        scaffold::scaffold(root, spec, &config, &probe, !args.dry_run, args.force)?;
    warnings.extend(scaffold_warnings);
    emit_write(result, warnings, &probe, pretty);
    Ok(())
}
