use anyhow::Result;
use clap::Args;
use std::path::Path;

use crate::format::{Envelope, print_json};

use super::git_checkout::{Checkout, ensure_repository, required_tree};

/// Args for `nodex impact`.
#[derive(Args)]
pub struct ImpactArgs {
    /// The "before" git ref (commit, branch, tag).
    pub before: String,
    /// The "after" git ref.
    pub after: String,
    /// Bound the transitive dependency walk to N hops (default: unbounded).
    #[arg(long)]
    pub depth: Option<u32>,
    /// Restrict the dependency walk to specific edge relations
    /// (comma-separated; default: every relation).
    #[arg(long, value_delimiter = ',')]
    pub relations: Vec<String>,
}

pub fn run(root: &Path, args: ImpactArgs, pretty: bool) -> Result<()> {
    let repository = ensure_repository(root, "nodex impact")?;

    if args.depth == Some(0) {
        return Err(nodex_core::error::Error::Config(
            "--depth 0 expands nothing; omit it for an unbounded walk or pass a value >= 1".into(),
        )
        .into());
    }

    // Both sides are required here — a ref that does not carry the project
    // has nothing to compare — and both are established before either is
    // checked out.
    let before_tree = required_tree(&repository, &args.before)?;
    let after_tree = required_tree(&repository, &args.after)?;
    let checkout = Checkout::acquire(&repository)?;
    let after_root = checkout.hold(&after_tree)?;

    // Single-lens semantics (same as `diff`): the *after* ref's config
    // is the one lens — both snapshots are graphed under it and the
    // before ref supplies content only, so the PR that migrates the
    // config format itself can still be impact-analysed. Its extensions
    // also recognise extension-less references to a removed file when
    // classifying danglers. Loaded as a lens, like `diff`'s.
    let after_config = nodex_core::Config::load(&after_root)?;
    // Both graphs carry the lens's relations and no others, so the lens is
    // the vocabulary `--relations` names: a relation it does not declare
    // matches no edge on either side.
    let known = after_config.known_relations();
    let unknown: Vec<&str> = args
        .relations
        .iter()
        .filter(|r| !known.contains(r.as_str()))
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        let known_sorted: Vec<&str> = known.iter().map(String::as_str).collect();
        return Err(nodex_core::error::Error::Config(format!(
            "--relations contains unknown value(s) {unknown:?}; known to {}'s config: {known_sorted:?}",
            args.after
        ))
        .into());
    }
    let after_build = checkout.graph_held(&after_config)?;
    let before_build = checkout.graph(&before_tree, &after_config)?;
    // A ref build drops what the ref did not record — a link out of the
    // checkout, a link with no target — and stops at a boundary it does not
    // cross. Dropping the accounting with it would let an impact report read
    // complete while documents were omitted from one side of the comparison,
    // so each omission is named against its own ref.
    let mut omissions: Vec<nodex_core::Warning> =
        super::git_checkout::ref_omissions(&args.before, &before_build);
    omissions.extend(super::git_checkout::ref_omissions(
        &args.after,
        &after_build,
    ));
    let before_graph = before_build.graph;
    let after_graph = after_build.graph;
    let after_extensions = after_config.parser.extensions;

    let report = nodex_core::compute_impact(
        &before_graph,
        &after_graph,
        &args.relations,
        args.depth,
        &after_extensions,
    );

    let mut warnings: Vec<nodex_core::Warning> = nodex_core::Config::load(root)
        .ok()
        .and_then(|config| nodex_core::binary_compat_warning(&config))
        .into_iter()
        .collect();
    warnings.extend(omissions);
    print_json(&Envelope::with_warnings(report, warnings), pretty);

    Ok(())
}
