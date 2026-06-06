use anyhow::Result;
use std::path::Path;

use nodex_core::query::similar::{SimilarityOptions, SimilarityTarget};
use nodex_core::query::trust::{TrustExtreme, TrustListOptions};

use crate::format::{ItemsEnvelope, emit_read};

use super::{
    SimilarityArgs, load_graph, reject_non_finite_or_out_of_unit_range, reject_unknown_vocabulary,
    reject_zero_usize,
};

/// Dispatch `query trust`. The clap layer already enforces exactly one
/// of `id` / `bottom` / `top`; this function maps that choice to the
/// single-node lookup or the listing primitive.
///
/// Every input-shape check (zero cap, non-finite cutoff, out-of-range
/// cutoff, unknown kind) runs before `load_graph` so a missing
/// `graph.json` cannot mask a flag bug behind an `IO_ERROR`.
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
        emit_read(report, &config, pretty);
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

    // Input validation runs BEFORE `load_graph` so an invalid flag
    // surfaces as `CONFIG_ERROR` even when `graph.json` is missing —
    // otherwise the user gets `IO_ERROR` from the build-prereq check
    // and never sees the actual flag bug.
    let limit_flag = match extreme {
        TrustExtreme::Bottom => "--bottom",
        TrustExtreme::Top => "--top",
    };
    reject_zero_usize(limit, limit_flag)?;
    if let Some(cutoff) = below {
        reject_non_finite_or_out_of_unit_range(cutoff, "--below")?;
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
    let items = nodex_core::query::trust::compute_trust_ranking(&graph, &config, root, &opts);
    emit_read(ItemsEnvelope::new(items), &config, pretty);
    Ok(())
}

/// Dispatch `query similar`. Same hoisting discipline as `run_trust` —
/// every input-shape check (unknown kind, zero limit, non-finite or
/// out-of-range cutoff) runs before `load_graph` so a missing graph
/// cannot mask a flag bug.
pub(crate) fn run_similar(root: &Path, args: SimilarityArgs, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;

    if let Some(kind) = args.kind.as_deref()
        && !config.kinds.allowed.iter().any(|k| k == kind)
    {
        return Err(nodex_core::error::Error::Config(format!(
            "--kind {kind:?} is not in kinds.allowed ({:?}); pick a known kind or omit the flag",
            config.kinds.allowed
        ))
        .into());
    }
    if let Some(limit) = args.limit {
        reject_zero_usize(limit, "--limit")?;
    }
    if let Some(cutoff) = args.min_score {
        reject_non_finite_or_out_of_unit_range(cutoff, "--min-score")?;
    }

    let graph = load_graph(root, &config)?;

    let opts = SimilarityOptions {
        limit: args.limit.unwrap_or(config.similarity.default_limit),
    };

    // clap's `ArgGroup(required=true)` on `similar_target` guarantees
    // exactly one of `--id` / `--title` was supplied; the third arm
    // is unreachable.
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
            unreachable!("clap group enforces exactly one of --id / --title")
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

    emit_read(ItemsEnvelope::new(items), &config, pretty);
    Ok(())
}
