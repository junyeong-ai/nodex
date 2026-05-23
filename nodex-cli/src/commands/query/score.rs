use anyhow::Result;
use std::path::Path;

use nodex_core::query::similar::{SimilarityOptions, SimilarityTarget};
use nodex_core::query::trust::{TrustExtreme, TrustListOptions};

use crate::format::{Envelope, ItemsEnvelope, print_json};

use super::{SimilarArgs, load_graph, reject_unknown_vocabulary};

/// Dispatch `query trust`. The clap layer already enforces exactly one
/// of `id` / `bottom` / `top`; this function maps that choice to the
/// single-node lookup or the listing primitive.
pub(crate) fn run_trust(
    root: &Path,
    id: Option<String>,
    bottom: Option<usize>,
    top: Option<usize>,
    kind: Option<String>,
    below: Option<f64>,
    pretty: bool,
) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    if let Some(id) = id {
        let graph = load_graph(root, &config)?;
        let report = nodex_core::query::trust::compute_trust(&graph, &config, root, &id)?;
        print_json(&Envelope::success(report), pretty);
        return Ok(());
    }

    let (extreme, limit) = match (bottom, top) {
        (Some(n), None) => (TrustExtreme::Bottom, n),
        (None, Some(n)) => (TrustExtreme::Top, n),
        // clap's `ArgGroup(required=true)` guarantees one of the
        // three was supplied; reaching this arm means an upstream
        // wiring mistake and should fail loudly.
        _ => unreachable!("clap group enforces exactly one of <id> / --bottom / --top"),
    };

    // Reject `--bottom 0` / `--top 0` at the CLI for symmetry with
    // `query nodes --limit 0` (filter.rs:25). A silent empty result
    // from a zero-cap is a footgun: the operator typed "show me
    // candidates" and got nothing without explanation.
    if limit == 0 {
        let flag = match extreme {
            TrustExtreme::Bottom => "--bottom",
            TrustExtreme::Top => "--top",
        };
        return Err(nodex_core::error::Error::Config(format!(
            "{flag} must be > 0 (use a positive cap, or omit for the default behaviour)"
        ))
        .into());
    }
    // `--below` accepts `f64`, so `NaN` and `±Infinity` parse cleanly
    // — but `NaN` filters everything (every comparison is false) and
    // `Infinity` produces all-or-none cutoffs. Reject both with a
    // clear message instead of silently surfacing garbage.
    if let Some(cutoff) = below
        && !cutoff.is_finite()
    {
        return Err(nodex_core::error::Error::Config(format!(
            "--below {cutoff} is not a finite number; supply a real cutoff or omit the flag"
        ))
        .into());
    }

    if let Some(k) = kind.as_deref() {
        reject_unknown_vocabulary(
            "--kind",
            std::slice::from_ref(&k.to_string()),
            &config.kinds.allowed,
        )?;
    }
    let graph = load_graph(root, &config)?;
    let opts = TrustListOptions {
        extreme,
        limit,
        kind,
        below,
    };
    let items = nodex_core::query::trust::list_trust(&graph, &config, root, &opts);
    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}

pub(crate) fn run_similar(root: &Path, args: SimilarArgs, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;

    if let Some(kind) = args.kind.as_deref()
        && !config.kinds.allowed.iter().any(|k| k == kind)
    {
        return Err(nodex_core::error::Error::Config(format!(
            "--kind {kind:?} is not in kinds.allowed ({:?}); pick a known kind or omit the flag",
            config.kinds.allowed
        ))
        .into());
    }
    // Symmetric guard with `query trust --bottom/--top` and
    // `query nodes --limit`: a zero cap silently empties the result,
    // which the operator never asked for.
    if let Some(0) = args.limit {
        return Err(nodex_core::error::Error::Config(
            "--limit must be > 0 (use a positive cap, or omit for the configured default)".into(),
        )
        .into());
    }
    // `NaN` / `±Infinity` slip through the `f64` parser. `NaN` keeps
    // nothing (every comparison is false); `+Infinity` keeps nothing
    // either; `-Infinity` keeps everything. Reject all non-finite
    // values with a clear message instead of fabricating a cutoff.
    if let Some(cutoff) = args.min_score
        && !cutoff.is_finite()
    {
        return Err(nodex_core::error::Error::Config(format!(
            "--min-score {cutoff} is not a finite number; supply a real cutoff or omit the flag"
        ))
        .into());
    }

    let opts = SimilarityOptions {
        limit: args.limit.unwrap_or(config.similarity.default_limit),
    };

    let mut items = match (args.id.as_deref(), args.title.as_deref()) {
        (Some(id), _) => nodex_core::query::similar::compute_similarity(
            &graph,
            &config,
            &SimilarityTarget::Node(id),
            &opts,
        )?,
        (None, Some(title)) => {
            let target = SimilarityTarget::Spec {
                title,
                kind: args.kind.as_deref(),
                tags: &args.tags,
                parent_dir: args.parent_dir.as_deref(),
            };
            nodex_core::query::similar::compute_similarity(&graph, &config, &target, &opts)?
        }
        (None, None) => {
            anyhow::bail!("either --id or --title must be supplied");
        }
    };

    // Opt-in score cutoff. Applied after ranking + truncation so the
    // semantic is unambiguous: "of the top-`limit` candidates, keep
    // only those scoring at least `min_score`". The ranking primitive
    // never carries the cutoff so a corpus drift doesn't silently
    // change top-K results.
    if let Some(min_score) = args.min_score {
        items.retain(|e| e.similarity >= min_score);
    }

    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}
