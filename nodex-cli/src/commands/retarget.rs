use anyhow::{Context, Result};
use chrono::NaiveDate;
use clap::Args;
use std::path::Path;

use nodex_core::command_result::RetargetResult;
use nodex_core::error::Error as CoreError;

use crate::format::emit_batch_write;

/// Args for `nodex retarget`.
#[derive(Args)]
pub struct RetargetArgs {
    /// The id being replaced — references to it are repointed.
    pub old_id: String,
    /// The successor id those references should point to.
    pub new_id: String,
    /// Supersede the predecessor and repoint its citations in one guarded batch.
    #[arg(long)]
    pub supersede: bool,
    /// Print the plan as JSON without rewriting any file
    #[arg(long)]
    pub dry_run: bool,
}

/// The successor's own references to the predecessor, named as the sites
/// that hold them: a frontmatter field by its name, body references by
/// their count — an id reference spells the id verbatim, so quoting one
/// would repeat the id the message already carries twice.
fn self_edge_sites(held: &nodex_core::retarget::SelfEdges) -> String {
    let mut sites: Vec<String> = held
        .fields
        .iter()
        .map(|field| format!("frontmatter `{field}`"))
        .collect();
    match held.body_references {
        0 => {}
        1 => sites.push("1 body reference".to_string()),
        n => sites.push(format!("{n} body references")),
    }
    sites.join("; ")
}

pub fn run(
    root: &Path,
    args: RetargetArgs,
    pretty: bool,
    today: NaiveDate,
) -> Result<std::process::ExitCode> {
    if args.old_id == args.new_id {
        return Err(CoreError::InvalidArgument(
            "old-id and new-id are the same; nothing to retarget".into(),
        )
        .into());
    }

    let config = nodex_core::load_project(root)?;
    // The version pin gates writing, not previewing: a dry run carries the
    // advisory like any read.
    let mut warnings = Vec::new();
    if args.dry_run {
        warnings.extend(nodex_core::binary_compat_warning(&config));
    } else {
        nodex_core::ensure_binary_compatible(&config)?;
    }
    // Build to read each document's parsed relation fields. Both endpoints
    // must exist: repointing to or from an unknown id is a typo, not an
    // operation.
    let outcome = nodex_core::builder::build(root, &config, true).context("build failed")?;
    let graph = outcome.graph;
    graph.require_node(&args.old_id)?;
    graph.require_node(&args.new_id)?;
    // The successor id gets written into other documents' body
    // references; an id that is not reference-safe (trim-unstable or
    // carrying wikilink metacharacters) would be repointed into a form
    // the next build cannot resolve back to the node — refuse before any
    // rewrite and point at the actual fix (the target document's id).
    nodex_core::model::validate_explicit_id(&args.new_id).map_err(|e| match e {
        nodex_core::error::Error::InvalidArgument(base) => {
            nodex_core::error::Error::InvalidArgument(format!(
                "{base}; fix the target document's `id:` frontmatter before retargeting"
            ))
        }
        other => other,
    })?;

    // Immutability lock probe: the baseline snapshot a `check` against
    // `immutable_baseline` would diff against. Outside a git work tree
    // (or with no baseline) those rules are inert for `check`, so the
    // probe is inert too. A repoint writes both parts a lock can name —
    // frontmatter relations and body references — which is
    // why it is the seam where holding back a part rather than a file
    // decides the outcome.
    let probe = super::git_checkout::write_baseline(root, &config)?;
    let succession = if args.supersede {
        nodex_core::lifecycle::check_supersede_safe(&graph, &args.old_id, &args.new_id)?;
        let predecessor = graph.require_node(&args.old_id)?;
        Some(nodex_core::lifecycle::prepare_transition(
            root,
            &predecessor.path,
            nodex_core::Action::Supersede {
                successor: args.new_id.clone(),
            },
            &config,
            &graph,
            &probe,
            today,
        )?)
    } else {
        None
    };

    // Plan every repoint first, gate the batch once, then write. The lock
    // asks what the project looks like after the whole repoint lands, which
    // no single file can answer — and a write must not land before the
    // answer, or a refused file would already be on disk.
    let mut plans: Vec<_> = succession.iter().cloned().collect();
    let mut skipped: Vec<String> = Vec::new();
    // Kept apart from `skipped` because the codes are: one says a write was
    // attempted and something stood in the way, the other that no write was
    // ever going to happen there.
    let mut kept: Vec<String> = Vec::new();
    // What the references are read against, so reference retargeting honours
    // the resolver's path-first precedence out of the resolver itself: a
    // `[[old]]` that binds a different document by path is left alone.
    let bound = nodex_core::builder::resolver::Bindings::of_graph(&graph);
    for node in graph.nodes().values() {
        if succession
            .as_ref()
            .is_some_and(|plan| plan.rel_path == node.path)
        {
            continue;
        }
        let shown = nodex_core::path_guard::forward_string(&node.path);
        // A reference the rewrite had a replacement for and could not
        // write. A retarget moves no file, so the reference goes on
        // naming exactly what it named and the project it leaves is in
        // order — nothing downstream has a reason to mention it, which
        // is why the command does.
        let mut refused: Vec<String> = Vec::new();
        // The successor's own references to the predecessor, which a repoint
        // declines to move. Unlike every other skip here they leave nothing
        // wrong behind — so nothing later has a reason to mention them, and
        // an empty `references_updated` would read the same whether they
        // existed or the project referenced the predecessor nowhere.
        let mut self_edges = nodex_core::retarget::SelfEdges::default();
        let mut kept_fields = Vec::new();
        let retarget = |content: &str| {
            if !node.matches_content(content) {
                return Err(CoreError::WriteConflict(node.path.clone()));
            }
            let retargeted = nodex_core::retarget::retarget_document(
                content,
                node,
                &args.old_id,
                &args.new_id,
                &bound,
                &config.parser,
            )?;
            refused = retargeted.refused;
            self_edges = retargeted.self_edges;
            kept_fields = retargeted.kept_fields;
            Ok(retargeted.content)
        };
        // The reader-follows / writer-skips symlink discipline and the read
        // live in the one core seam.
        match nodex_core::mutate::plan_file(root, &node.path, retarget, || {
            format!(
                "{} references {} but is or resolves through a symlink; it was not repointed \
                 (writing through a symlink could escape the project root) — update it manually",
                nodex_core::path_guard::forward_string(&node.path),
                args.old_id
            )
        })? {
            nodex_core::mutate::PlanOutcome::Planned(plan) => plans.push(plan),
            nodex_core::mutate::PlanOutcome::Skipped(warning) => skipped.push(warning),
            nodex_core::mutate::PlanOutcome::Unchanged => {}
        }
        for field in kept_fields {
            kept.push(format!(
                "{shown} frontmatter `{field}` keeps naming `{}` by its declared retarget policy",
                args.old_id
            ));
        }
        if !self_edges.is_empty() {
            let new = &args.new_id;
            let old = &args.old_id;
            let sites = self_edge_sites(&self_edges);
            kept.push(if self_edges.len() == 1 {
                format!(
                    "{shown} is `{new}` itself, and a repoint does not rewrite the document it \
                     points at, so its own reference to `{old}` ({sites}) was left standing and \
                     goes on naming `{old}`"
                )
            } else {
                format!(
                    "{shown} is `{new}` itself, and a repoint does not rewrite the document it \
                     points at, so its own references to `{old}` ({sites}) were left standing \
                     and go on naming `{old}`"
                )
            });
        }
        for reference in refused {
            skipped.push(format!(
                "{shown} reference \"{reference}\" still names `{}`: no rewrite of it to `{}` \
                 was accepted, so it was left as it is — respell it by hand",
                args.old_id, args.new_id
            ));
        }
    }

    // A repoint nodex's own `check` would flag is not performed; frozen
    // history keeps its original reference. A lock names a part of a document, so what it costs
    // is that part: a frozen body keeps its citation while the same
    // document's relation fields are repointed.
    let narrowing = nodex_core::narrow(&probe, root, &config, &[], plans, today)?;
    for held in narrowing.held {
        let shown = nodex_core::path_guard::forward_string(&held.rel_path);
        if succession
            .as_ref()
            .is_some_and(|plan| plan.rel_path == held.rel_path)
        {
            return Err(held
                .refusal
                .error(format!(
                    "retarget of {:?} to {:?}",
                    args.old_id, args.new_id
                ))
                .into());
        }
        skipped.push(match held.kept {
            nodex_core::Kept::Whole(lock) => format!(
                "{shown} references {} but is locked ({lock}); it was not repointed — the \
                 reference keeps its original target",
                args.old_id
            ),
            nodex_core::Kept::Parts(parts) => format!(
                "{shown} was repointed to `{}`, except in {}, which is locked — there the \
                 reference keeps naming `{}`",
                args.new_id,
                nodex_core::named_parts(&parts),
                args.old_id
            ),
        });
    }
    let writable = narrowing.writable;

    // The project this repoint really produces — exactly the rewrites that
    // will land, the locked ones keeping their original reference. A repoint
    // moves edges, and edges are what several rules are about: an
    // `implements` chain the new target closes into a cycle is the project's
    // own `check` failing on a command that reported success.
    let landing: Vec<_> = writable.iter().map(nodex_core::Planned::proposed).collect();
    let introduced = nodex_core::introduced(
        root,
        &config,
        &graph,
        &landing,
        nodex_core::ProposalDiff::Configured,
        &probe,
        today,
    )
    .context("the project this repoint would produce could not be checked")?;
    if let Some(refusal) = introduced.refusal(format!(
        "retarget of {:?} to {:?}",
        args.old_id, args.new_id
    )) {
        return Err(refusal.into());
    }

    let mut updated = Vec::new();
    let mut failures = Vec::new();
    if args.dry_run {
        updated.extend(
            writable
                .iter()
                .map(|plan| nodex_core::path_guard::forward_string(&plan.rel_path)),
        );
    } else {
        // Stage every write before committing any file. A staging failure leaves
        // the tree untouched; a later commit failure preserves the partial result.
        let mut staged = Vec::new();
        for plan in &writable {
            staged.push((
                plan,
                nodex_core::mutate::stage_plan(root, plan).with_context(|| {
                    format!(
                        "the repoint in {} could not be staged, so nothing was written",
                        nodex_core::path_guard::forward_string(&plan.rel_path)
                    )
                })?,
            ));
        }
        for (plan, staged) in staged {
            let shown = nodex_core::path_guard::forward_string(&plan.rel_path);
            match staged.commit() {
                Ok(()) => updated.push(shown),
                Err(e) => {
                    failures.push(nodex_core::FileWriteFailure::of(&plan.rel_path, &e));
                    skipped.push(format!(
                        "{shown} references {} but could not be rewritten ({}); the reference keeps \
                         its original target",
                        args.old_id,
                        nodex_core::error::chain(&e)
                    ));
                }
            }
        }
    }

    let data = RetargetResult {
        completion: nodex_core::WriteCompletion::of(!args.dry_run, !skipped.is_empty()),
        failures,
        old_id: args.old_id,
        new_id: args.new_id,
        total_updated: updated.len(),
        references_updated: updated,
    };
    // The graph the rewrite was planned against carries what the walk could
    // not read: a reference behind a boundary is one this run did not repoint.
    warnings.extend(outcome.warnings.iter().cloned());
    warnings.extend(introduced.advisories());
    warnings.extend(
        skipped
            .into_iter()
            .map(|w| nodex_core::Warning::new(nodex_core::WarningCode::FileSkipped, w)),
    );
    warnings.extend(
        kept.into_iter()
            .map(|w| nodex_core::Warning::new(nodex_core::WarningCode::ReferenceKept, w)),
    );
    Ok(emit_batch_write(
        &data,
        &data.failures,
        warnings,
        &probe,
        pretty,
    ))
}
