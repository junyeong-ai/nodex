use anyhow::{Context, Result};
use clap::Args;
use std::path::Path;

use nodex_core::command_result::RetargetResult;
use nodex_core::error::Error as CoreError;

use crate::format::{Envelope, print_json};

/// Args for `nodex retarget`.
#[derive(Args)]
pub struct RetargetArgs {
    /// The id being replaced — references to it are repointed.
    pub old_id: String,
    /// The successor id those references should point to.
    pub new_id: String,
}

pub fn run(root: &Path, args: RetargetArgs, pretty: bool) -> Result<()> {
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

    // Every in-scope file path, so id retargeting can honour the
    // resolver's path-first precedence: a `[[old]]` that binds a file
    // by path is not an id reference and must be left alone.
    let in_scope: std::collections::BTreeSet<String> = graph
        .nodes()
        .values()
        .map(|n| nodex_core::path_guard::forward_string(&n.path))
        .collect();

    // Immutability lock probe: the baseline snapshot a `check` against
    // `immutable_baseline` would diff against. Outside a git work tree
    // (or with no baseline) those rules are inert for `check`, so the
    // probe is inert too — the mutation seam consults it per file, with
    // relation-field locks engaged (`frontmatter_relations`): a repoint
    // rewrites id-valued frontmatter relations, exactly the aspect a
    // `frontmatter_immutable` lock can freeze.
    let probe = nodex_core::BaselineProbe::resolve(root, &config);

    let mut updated = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for node in graph.nodes().values() {
        let retarget = |content: &str| {
            nodex_core::retarget::retarget_document(
                content,
                node,
                &args.old_id,
                &args.new_id,
                &in_scope,
                &config.parser,
            )
        };
        // The reader-follows / writer-skips symlink discipline, the
        // immutability writer-skip (a repoint nodex's own `check` would
        // flag — a body lock when the body changes, or a frontmatter
        // lock on a relation field that changes — is not performed;
        // frozen history keeps its original reference and surfaces on
        // the next build as an unresolved edge), and the atomic write
        // all live in the one core seam.
        match nodex_core::mutate::apply_to_file(
            root,
            &node.path,
            &config,
            &probe,
            nodex_core::RewriteLock {
                baseline_path: &node.path,
                frontmatter_relations: true,
            },
            retarget,
            |reason| match reason {
                nodex_core::SkipReason::Symlink => format!(
                    "{} references {} but is or resolves through a symlink; it was not \
                     repointed (writing through a symlink could escape the project root) — \
                     update it manually",
                    nodex_core::path_guard::forward_string(&node.path),
                    args.old_id
                ),
                nodex_core::SkipReason::Locked(lock) => format!(
                    "{} references {} but is locked ({lock}); it was not repointed — the \
                     reference keeps its original target",
                    nodex_core::path_guard::forward_string(&node.path),
                    args.old_id
                ),
            },
        )? {
            nodex_core::mutate::FileOutcome::Rewritten => {
                updated.push(nodex_core::path_guard::forward_string(&node.path));
            }
            nodex_core::mutate::FileOutcome::Skipped(warning) => skipped.push(warning),
            nodex_core::mutate::FileOutcome::Unchanged => {}
        }
    }

    let data = RetargetResult {
        old_id: args.old_id,
        new_id: args.new_id,
        total_updated: updated.len(),
        references_updated: updated,
    };
    if skipped.is_empty() {
        print_json(&Envelope::success(data), pretty);
    } else {
        print_json(&Envelope::with_warnings(data, skipped), pretty);
    }

    Ok(())
}
