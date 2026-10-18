use anyhow::Result;
use nodex_core::warning::{RankingExclusion, WarningDetails};

use nodex_core::query::similar::{SimilarityOptions, SimilarityTarget};

use crate::format::ItemsEnvelope;

use super::{
    SimilarityArgs, reject_non_finite_or_out_of_unit_range, reject_unknown_vocabulary,
    reject_zero_usize,
};

pub(crate) fn run_similar(
    context: &super::QueryContext<'_>,
    args: SimilarityArgs,
    pretty: bool,
) -> Result<()> {
    let config = nodex_core::load_project(context.root)?;

    if let Some(status) = args.status.as_deref() {
        reject_unknown_vocabulary("--status", &[status.to_string()], &config.statuses.allowed)?;
    }
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

    let opts = SimilarityOptions {
        limit: args.limit.unwrap_or(config.similarity.default_limit),
        kind: args.kind,
        status: args.status,
        parent_dir: args
            .parent_dir
            .as_deref()
            .map(|path| {
                nodex_core::path_guard::normalize_for_lookup(&path.to_string_lossy(), context.root)
                    .map(Into::into)
            })
            .transpose()?,
    };

    let snapshot = context.load_graph(&config)?;
    let (graph, mut warnings) = (snapshot.graph(), snapshot.warnings());

    // clap's `ArgGroup(required=true)` on `similar_target` guarantees
    // exactly one of `--id` / `--title` was supplied; the third arm
    // is unreachable.
    let outcome = match (args.id.as_deref(), args.title.as_deref()) {
        (Some(id), _) => snapshot.require(
            context.root,
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
                tags: &args.tags,
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

    // Presence is the target's, so a query carrying no positively-
    // weighted signal gives no candidate a composite — every one is
    // outside the ranking's domain (so `--min-score` can never be
    // satisfied by a fabricated 0.0) and the count is announced, never
    // silent.
    if outcome.unscored > 0 {
        warnings.push(
            nodex_core::Warning::new(
                nodex_core::WarningCode::RankingUnscored,
                format!(
                    "{} candidate(s) excluded from the ranking: the target carries no \
                 positively-weighted signal to rank by",
                    outcome.unscored
                ),
            )
            .with_details(WarningDetails::RankingUnscored {
                count: outcome.unscored,
                reason: RankingExclusion::TargetWithoutSignals,
            }),
        );
    }
    context.emit_read_with(ItemsEnvelope::new(items), warnings, &config, pretty);
    Ok(())
}
