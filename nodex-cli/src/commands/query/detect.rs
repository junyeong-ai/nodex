use anyhow::Result;
use chrono::NaiveDate;

use crate::format::{ItemsEnvelope, emit_read_with};

use super::reject_zero_usize;

pub(crate) fn run_orphans(
    context: &super::QueryContext<'_>,
    limit: Option<usize>,
    pretty: bool,
    today: NaiveDate,
) -> Result<()> {
    let config = nodex_core::load_project(context.root)?;
    if let Some(n) = limit {
        reject_zero_usize(n, "--limit")?;
    }
    let snapshot = context.load_graph(&config)?;
    let (graph, warnings) = (snapshot.graph(), snapshot.warnings());
    let items = nodex_core::query::detect::find_orphans(graph, &config, today).entries;
    emit_read_with(
        ItemsEnvelope::capped(items, limit),
        warnings,
        &config,
        pretty,
    );
    Ok(())
}

pub(crate) fn run_stale(
    context: &super::QueryContext<'_>,
    limit: Option<usize>,
    pretty: bool,
    today: NaiveDate,
) -> Result<()> {
    let config = nodex_core::load_project(context.root)?;
    if let Some(n) = limit {
        reject_zero_usize(n, "--limit")?;
    }
    let snapshot = context.load_graph(&config)?;
    let (graph, mut warnings) = (snapshot.graph(), snapshot.warnings());
    let items = match nodex_core::query::detect::find_stale(graph, &config, today) {
        Some(outcome) => outcome.entries,
        None => {
            warnings.push(nodex_core::Warning::new(
                nodex_core::WarningCode::ThresholdUndeclared,
                "`[detection].stale_days` is not set, so staleness is not tracked and no document \
                 was measured; set it to list the documents whose `reviewed` date is that many \
                 days old",
            ));
            Vec::new()
        }
    };
    emit_read_with(
        ItemsEnvelope::capped(items, limit),
        warnings,
        &config,
        pretty,
    );
    Ok(())
}

pub(crate) fn run_issues(
    context: &super::QueryContext<'_>,
    pretty: bool,
    today: NaiveDate,
) -> Result<()> {
    let config = nodex_core::load_project(context.root)?;
    let snapshot = context.load_graph(&config)?;
    let (graph, mut warnings) = (snapshot.graph(), snapshot.warnings());

    // The same diff context a default `check` runs under — the
    // configured `rules.immutable_baseline`, resolved through the one
    // shared substrate — so "what's broken?" and `check` can never
    // disagree about the immutability violations, nor about the inert
    // advisory (baseline set, immutability rules declared, context.root not a
    // git work tree — one wording, constructed in the substrate). The
    // baseline build's own warnings (e.g. a document unparseable at
    // the baseline, which silently disables its diff-aware rules) ride
    // along to the envelope.
    use crate::commands::git_checkout::{BaselineResolution, Current, Prior};
    let Prior {
        baseline,
        steps,
        unread,
    } = crate::commands::git_checkout::baseline_diff(
        context.root,
        &config,
        Current {
            graph,
            files: context.root,
        },
    )?;
    warnings.extend(unread);
    let diff = match baseline {
        BaselineResolution::Resolved(baseline) => {
            warnings.extend(baseline.warnings);
            Some(baseline.diff)
        }
        BaselineResolution::Inert { warning } => {
            warnings.push(warning);
            None
        }
        BaselineResolution::NotApplicable => None,
    };
    let report = nodex_core::query::issues::find_issues(
        graph,
        &config,
        context.root,
        diff.as_ref(),
        steps.as_deref(),
        today,
    );
    emit_read_with(report, warnings, &config, pretty);
    Ok(())
}
