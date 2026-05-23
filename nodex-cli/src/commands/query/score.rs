use anyhow::Result;
use std::path::Path;

use nodex_core::query::similar::{SimilarityOptions, SimilarityTarget};

use crate::format::{Envelope, ItemsEnvelope, print_json};

use super::{SimilarArgs, load_graph, reject_unknown_vocabulary};

pub(crate) fn run_trust(root: &Path, id: &str, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;
    let report = nodex_core::query::trust::compute_trust(&graph, &config, root, id)?;
    print_json(&Envelope::success(report), pretty);
    Ok(())
}

pub(crate) fn run_low_trust(
    root: &Path,
    threshold: Option<f64>,
    kind: Option<&str>,
    pretty: bool,
) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    if let Some(k) = kind {
        reject_unknown_vocabulary(
            "--kind",
            std::slice::from_ref(&k.to_string()),
            &config.kinds.allowed,
        )?;
    }
    let graph = load_graph(root, &config)?;
    let cutoff = threshold.unwrap_or(config.trust.low_trust_threshold);
    let items = nodex_core::query::trust::find_low_trust(&graph, &config, root, cutoff, kind);
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
        threshold: args.threshold.unwrap_or(config.similarity.threshold),
        limit: args.limit.unwrap_or(config.similarity.default_limit),
    };

    let items = match (args.id.as_deref(), args.title.as_deref()) {
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

    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}
