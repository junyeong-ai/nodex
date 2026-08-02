use anyhow::{Context, Result};
use chrono::NaiveDate;
use clap::Args;
use std::path::Path;

use nodex_core::command_result::RetargetResult;
use nodex_core::error::Error as CoreError;

use crate::format::emit_write;

/// Args for `nodex retarget`.
#[derive(Args)]
pub struct RetargetArgs {
    /// The id being replaced — references to it are repointed.
    pub old_id: String,
    /// The successor id those references should point to.
    pub new_id: String,
}

pub fn run(root: &Path, args: RetargetArgs, pretty: bool, today: NaiveDate) -> Result<()> {
    if args.old_id == args.new_id {
        return Err(CoreError::Config(
            "old-id and new-id are the same; nothing to retarget".into(),
        )
        .into());
    }

    let config = nodex_core::load_project_for_mutation(root)?;
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
    nodex_core::model::validate_explicit_id(&args.new_id).map_err(|e| {
        // Extend the message inside the same typed variant — re-wrapping
        // the Display form would double the "config error:" prefix.
        let base = match e {
            nodex_core::error::Error::Config(m) => m,
            other => other.to_string(),
        };
        nodex_core::error::Error::Config(format!(
            "{base}; fix the target document's `id:` frontmatter before retargeting"
        ))
    })?;

    // Immutability lock probe: the baseline snapshot a `check` against
    // `immutable_baseline` would diff against. Outside a git work tree
    // (or with no baseline) those rules are inert for `check`, so the
    // probe is inert too — the mutation seam consults it per file, with
    // relation-field locks engaged (`frontmatter_relations`): a repoint
    // rewrites id-valued frontmatter relations, exactly the aspect a
    // `frontmatter_immutable` lock can freeze.
    let probe = super::git_worktree::write_baseline(root, &config)?;

    // Plan every repoint first, gate the batch once, then write. The lock
    // asks what the project looks like after the whole repoint lands, which
    // no single file can answer — and a write must not land before the
    // answer, or a refused file would already be on disk.
    let mut plans = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    // What the references are read against, so id retargeting honours
    // the resolver's path-first precedence out of the resolver itself: a
    // `[[old]]` that binds a file by path is not an id reference and must
    // be left alone.
    let bound = nodex_core::builder::resolver::Bindings::of_graph(&graph);
    for node in graph.nodes().values() {
        let retarget = |content: &str| {
            nodex_core::retarget::retarget_document(
                content,
                node,
                &args.old_id,
                &args.new_id,
                &bound,
                &config.parser,
            )
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
    }

    // A repoint nodex's own `check` would flag is not performed; frozen
    // history keeps its original reference and surfaces on the next build as
    // an unresolved edge.
    let proposal: Vec<_> = plans.iter().map(nodex_core::Planned::proposed).collect();
    let refusals = probe.refusals(root, &config, &proposal, today)?;
    let mut writable: Vec<&nodex_core::Planned> = Vec::new();
    for plan in &plans {
        match refusals.refusing(&plan.rel_path) {
            Some(lock) => skipped.push(format!(
                "{} references {} but is locked ({lock}); it was not repointed — the \
                 reference keeps its original target",
                nodex_core::path_guard::forward_string(&plan.rel_path),
                args.old_id
            )),
            None => writable.push(plan),
        }
    }

    // The project this repoint really produces — exactly the rewrites that
    // will land, the locked ones keeping their original reference. A repoint
    // moves edges, and edges are what several rules are about: an
    // `implements` chain the new target closes into a cycle is the project's
    // own `check` failing on a command that reported success.
    let landing: Vec<_> = writable
        .iter()
        .map(|plan| nodex_core::Planned::proposed(plan))
        .collect();
    let introduced = nodex_core::introduced(
        root,
        &config,
        &graph,
        &landing,
        nodex_core::ProposalDiff::Inert,
        today,
    )
    .context("the project this repoint would produce could not be checked")?;
    if let Some(refusal) =
        introduced.refusal(format!("repointing {:?} to {:?}", args.old_id, args.new_id))
    {
        return Err(refusal.into());
    }

    // A repoint is one edit across several files, and the gate judged it
    // whole, so it lands whole: every write is staged first — where an
    // unwritable directory or a full disk fails while the tree is still
    // untouched and every staged write is dropped — and only then committed.
    // Unlike `rename` there is no irreversible step to strand, so a staging
    // failure refuses the command outright.
    let mut staged = Vec::new();
    for plan in &writable {
        staged.push((
            *plan,
            nodex_core::mutate::stage_plan(root, plan).with_context(|| {
                format!(
                    "the repoint in {} could not be staged, so nothing was written",
                    nodex_core::path_guard::forward_string(&plan.rel_path)
                )
            })?,
        ));
    }
    let mut updated = Vec::new();
    for (plan, staged) in staged {
        let shown = nodex_core::path_guard::forward_string(&plan.rel_path);
        match staged.commit() {
            Ok(()) => updated.push(shown),
            Err(e) => skipped.push(format!(
                "{shown} references {} but could not be rewritten ({}); the reference keeps \
                 its original target",
                args.old_id,
                nodex_core::error::chain(&e)
            )),
        }
    }

    let data = RetargetResult {
        old_id: args.old_id,
        new_id: args.new_id,
        total_updated: updated.len(),
        references_updated: updated,
    };
    // The graph the rewrite was planned against carries what the walk could
    // not read: a reference behind a boundary is one this run did not repoint.
    let mut warnings = outcome.warnings.clone();
    warnings.extend(introduced.advisories());
    warnings.extend(
        skipped
            .into_iter()
            .map(|w| nodex_core::Warning::new(nodex_core::WarningCode::FileSkipped, w)),
    );
    emit_write(data, warnings, &probe, pretty);

    Ok(())
}
