use anyhow::Result;
use chrono::NaiveDate;
use std::path::Path;

use nodex_core::query::similar::{SimilarityOptions, SimilarityTarget};
use nodex_core::query::trust::{TrustExtreme, TrustListOptions};

use crate::format::{ItemsEnvelope, emit_read_with};

use super::{
    SimilarityArgs, TrustArgs, reject_non_finite_or_out_of_unit_range, reject_unknown_vocabulary,
    reject_zero_usize,
};

/// Dispatch `query trust`. The clap layer already enforces exactly one
/// of `id` / `bottom` / `top`; this function maps that choice to the
/// single-node lookup or the listing primitive.
///
/// Every input-shape check (zero cap, non-finite cutoff, out-of-range
/// cutoff, unknown kind / status) runs before `load_graph` so a missing
/// `graph.json` cannot mask a flag bug behind `GRAPH_MISSING`.
pub(crate) fn run_trust(
    root: &Path,
    args: TrustArgs,
    pretty: bool,
    today: NaiveDate,
) -> Result<()> {
    let TrustArgs {
        id,
        bottom,
        top,
        kind,
        status,
        below,
    } = args;
    let config = nodex_core::load_project(root)?;
    if let Some(id) = id {
        let snapshot = nodex_core::load_graph(root, &config)?;
        let (graph, warnings) = (snapshot.graph(), snapshot.warnings());
        let report = snapshot.require(
            root,
            &config,
            nodex_core::query::trust::compute_trust(graph, &config, root, &id, today),
        )?;
        emit_read_with(report, warnings, &config, pretty);
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
    // otherwise the user gets `GRAPH_MISSING` from the snapshot read
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
    if let Some(s) = status.as_deref() {
        reject_unknown_vocabulary(
            "--status",
            std::slice::from_ref(&s.to_string()),
            &config.statuses.allowed,
        )?;
    }

    let snapshot = nodex_core::load_graph(root, &config)?;
    let (graph, mut warnings) = (snapshot.graph(), snapshot.warnings());
    let opts = TrustListOptions {
        extreme,
        limit,
        kind,
        status,
        below,
    };
    let outcome =
        nodex_core::query::trust::compute_trust_ranking(graph, &config, root, &opts, today);
    // An unrankable node (no positively-weighted trust signal) is not
    // in the ranking's domain — excluded from items and total — and
    // the exclusion is never silent: it rides the envelope warnings.
    if outcome.unscored > 0 {
        warnings.push(nodex_core::Warning::new(
            nodex_core::WarningCode::RankingUnscored,
            format!(
                "{} node(s) excluded from the ranking: no positively-weighted trust signal under \
             the active weights — inspect with `query trust <id>`",
                outcome.unscored
            ),
        ));
    }
    emit_read_with(
        ItemsEnvelope::new(outcome.entries),
        warnings,
        &config,
        pretty,
    );
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

    let snapshot = nodex_core::load_graph(root, &config)?;
    let (graph, mut warnings) = (snapshot.graph(), snapshot.warnings());

    let opts = SimilarityOptions {
        limit: args.limit.unwrap_or(config.similarity.default_limit),
    };

    // clap's `ArgGroup(required=true)` on `similar_target` guarantees
    // exactly one of `--id` / `--title` was supplied; the third arm
    // is unreachable.
    let outcome = match (args.id.as_deref(), args.title.as_deref()) {
        (Some(id), _) => snapshot.require(
            root,
            &config,
            nodex_core::query::similar::compute_similarity(
                graph,
                &config,
                &SimilarityTarget::Node(id),
                &opts,
            ),
        )?,
        (None, Some(title)) => {
            let target = SimilarityTarget::Spec {
                title,
                kind: args.kind.as_deref(),
                tags: &args.tags,
                parent_dir: args.parent_dir.as_deref(),
            };
            nodex_core::query::similar::compute_similarity(graph, &config, &target, &opts)?
        }
        (None, None) => {
            unreachable!("clap group enforces exactly one of --id / --title")
        }
    };
    let mut items = outcome.entries;

    // Opt-in score cutoff. Applied after ranking + truncation so the
    // semantic is unambiguous: "of the top-`limit` candidates, keep
    // only those scoring at least `min_score`". The ranking primitive
    // never carries the cutoff so a corpus drift doesn't silently
    // change top-K results.
    if let Some(min_score) = args.min_score {
        items.retain(|e| e.score >= min_score);
    }

    // A candidate with no comparable signal has no composite — it is
    // excluded from the ranking's domain (so `--min-score` can never
    // be satisfied by a fabricated 0.0) and announced, never silent.
    if outcome.unscored > 0 {
        warnings.push(nodex_core::Warning::new(
            nodex_core::WarningCode::RankingUnscored,
            format!(
                "{} candidate(s) excluded from the ranking: no comparable signal with the target",
                outcome.unscored
            ),
        ));
    }
    emit_read_with(ItemsEnvelope::new(items), warnings, &config, pretty);
    Ok(())
}
