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
