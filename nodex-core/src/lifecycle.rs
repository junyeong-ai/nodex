//! Lifecycle state transitions for documents.
//!
//! Each transition rewrites a small number of frontmatter scalar fields
//! through [`crate::parser::editor::FrontmatterEditor`] — never a full
//! YAML round-trip — so the user's key order, comments, blank lines,
//! and quoting style survive intact. A status change produces a
//! one-line diff.

use chrono::NaiveDate;
use std::path::Path;

use crate::config::Config;
use crate::error::{Error, ParseError, Result};
use crate::model::{Edge, Graph, ResolvedTarget};
use crate::parser::editor::{FrontmatterEditor, Scalar};
use crate::parser::frontmatter::split_frontmatter;
use crate::path_guard;
use crate::warning::Warning;

/// The status `supersede` writes. Superseding carries a structural
/// payload — a successor id plus a supersession-DAG safety check — so it
/// has a dedicated action rather than going through the generic setter.
pub const SUPERSEDED: &str = "superseded";

/// A lifecycle action. Variants carry the data their action needs
/// in-line so callers cannot supply the wrong combination of fields:
/// `supersede` structurally requires a successor; `set` carries the
/// target status; `review` carries nothing. Every reachable status
/// other than `superseded` is written through `set`, whose target is
/// validated against the project's vocabulary at the write seam — so the
/// status vocabulary lives in `nodex.toml`, never in this enum.
#[derive(Debug, Clone)]
pub enum Action {
    Supersede { successor: String },
    SetStatus { status: String },
    Review,
}

impl Action {
    /// Target status written to the document, or `None` for review.
    pub fn target_status(&self) -> Option<&str> {
        match self {
            Self::Supersede { .. } => Some(SUPERSEDED),
            Self::SetStatus { status } => Some(status),
            Self::Review => None,
        }
    }

    /// Short name for logging / JSON output.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Supersede { .. } => "supersede",
            Self::SetStatus { .. } => "set",
            Self::Review => "review",
        }
    }
}

/// Pre-flight safety check for [`Action::Supersede`].
///
/// Refuses the transition when:
/// 1. `new_id` does not exist in the graph — surfaces as
///    [`Error::MissingNode`] (`NOT_FOUND`).
/// 2. Materialising the resulting `new_id SUPERSEDES old_id` edge
///    would close a cycle in the existing supersession chain —
///    surfaces as [`Error::Cycle`] (`CYCLE_DETECTED`).
///
/// This is the seam where lifecycle guarantees that a write never
/// produces a graph the next `build` would reject — the same
/// "no silent broken graph" discipline that anchors `rename`'s
/// id-stability fix. Run this before calling [`transition`] with an
/// `Action::Supersede` payload; callers handling other actions can
/// skip it. The check is pure (no mutation, no I/O) so it costs only
/// a single DAG scan over the existing supersedes edges.
pub fn check_supersede_safe(graph: &Graph, old_id: &str, new_id: &str) -> Result<()> {
    graph.require_node(new_id)?;
    let mut edges: Vec<Edge> = graph.edges().to_vec();
    edges.push(Edge {
        source: new_id.to_string(),
        target: ResolvedTarget::resolved(old_id),
        relation: "supersedes".to_string(),
        location: "lifecycle:supersede".to_string(),
    });
    crate::builder::validator::validate_supersedes_dag(&edges)
}

/// Apply a lifecycle transition to a document file. Returns the new
/// file content. Symlinks are refused (writing through one could
/// escape the project root); the scanner still follows them on read.
///
/// For [`Action::Supersede`] callers must run
/// [`check_supersede_safe`] beforehand — this function is the pure
/// frontmatter mutator and does not re-derive the graph to validate.
pub fn transition(
    root: &Path,
    rel_path: &Path,
    action: Action,
    config: &Config,
    before: &Graph,
    probe: &crate::mutate::BaselineProbe,
    today: NaiveDate,
) -> Result<(String, Vec<Warning>)> {
    let abs_path = root.join(rel_path);

    if path_guard::is_symlink(&abs_path) {
        return Err(Error::OutsideRoot(rel_path.to_path_buf()));
    }

    // The action's name, captured before the action is consumed by the match
    // below.
    let action_name = action.name();

    let content = std::fs::read_to_string(&abs_path).map_err(|source| Error::Io {
        path: abs_path.clone(),
        source,
    })?;
    let content = crate::parser::frontmatter::canonicalize(&content);

    let (yaml_opt, body) = split_frontmatter(&content).map_err(|source| Error::Parse {
        path: abs_path.clone(),
        source,
    })?;
    let Some(yaml_str) = yaml_opt else {
        return Err(Error::Parse {
            path: abs_path,
            source: ParseError::FrontmatterDelimiter,
        });
    };

    // Whole-document parse failures still refuse — there is no frontmatter to
    // edit — but a *field*-level parse issue does not. The guard that used to
    // refuse those was protecting against a laundering that cannot happen:
    // the editor rewrites the fields the action names and leaves every other
    // line exactly as it found it, so a malformed `created:` is still
    // malformed afterwards and `check` still flags it. What it did instead was
    // refuse a transition over a violation the document already carried,
    // which the general gate below is written to allow.
    crate::parser::frontmatter::parse_frontmatter(rel_path, &content).map_err(|e| match e {
        Error::Parse { source, .. } => Error::Parse {
            path: abs_path.clone(),
            source,
        },
        other => other,
    })?;

    let mut editor = FrontmatterEditor::parse(yaml_str, &abs_path)?;

    // What this document *is*, read the way the graph reads it.
    //
    // The editor is a line reader: it answers with the text on the line, so
    // `status: ~` reads as `"~"` where the parser reads YAML null and the
    // graph fills `statuses.initial`. Every guard below asks about the
    // document the project holds, not about its spelling, so every guard
    // reads it from here — one parse, the same one `check` performs — while
    // the editor stays what it is good at, which is writing.
    let parse_config = crate::parser::ParseConfig::new(config);
    let node = crate::parser::parse_document(rel_path, &content, &parse_config)?.node;
    // The id anchors error messages on the *node* the user operated on
    // rather than its on-disk path.
    let node_id = node.id.clone();
    let current_status = node.status.as_str().to_string();

    if config.is_terminal(&current_status) && !matches!(action, Action::Review) {
        let to = action
            .target_status()
            .expect("non-Review action always has a target status");
        return Err(Error::Transition {
            node_id,
            from: current_status,
            to: to.to_string(),
        });
    }

    // Refuse — before writing — any action whose target status this
    // document's kind does not allow, so the tool never produces a doc
    // its own `check` would reject (the self-consistency invariant),
    // while a project only needs to allow the statuses for the actions
    // it actually uses. The kind comes from the document itself
    // (frontmatter, else path inference), matching how the builder
    // classifies it.
    if let Some(target) = action.target_status() {
        let allowed = config.allowed_statuses_for(node.kind.as_str());
        if !allowed.iter().any(|s| s == target) {
            return Err(Error::Config(format!(
                "lifecycle {} writes status \"{target}\", but kind \"{}\" does not allow it; \
                 add \"{target}\" to statuses.allowed (or the kind's status enum) to enable \
                 this action",
                action.name(),
                node.kind.as_str(),
            )));
        }
    }

    let today_field = today.to_string();

    match action {
        Action::Supersede { successor } => {
            editor.set("status", SUPERSEDED);
            editor.set("superseded_by", &successor);
            editor.set("updated", &today_field);
        }
        Action::SetStatus { status } => {
            editor.set("status", &status);
            editor.set("updated", &today_field);
        }
        Action::Review => {
            // Monotonicity guard: refuse a review that would push the
            // `reviewed` date backward. A future-dated value already
            // on disk (`reviewed: 2030-01-01` from a clock-skew commit
            // or an intentional approved-through marker) carries real
            // information; silently replacing it with today would
            // erase that. Surface as `Error::Transition` with a
            // descriptive `from → to` so the operator sees exactly
            // what the existing value was.
            if let Scalar::Value(existing) = editor.scalar("reviewed")
                && let Ok(existing_date) = chrono::NaiveDate::parse_from_str(&existing, "%Y-%m-%d")
                && existing_date > today
            {
                return Err(Error::Transition {
                    node_id,
                    from: existing.to_string(),
                    to: today_field,
                });
            }
            editor.set("reviewed", &today_field);
        }
    }

    let new_content = format!("---\n{}---\n{body}", editor.render());

    // Symmetric immutability guard, asked of the rules rather than of a
    // hand-picked field list: the terminal guard above already blocks
    // `set`/`supersede` on a terminal doc, but `review` is exempt from it and
    // writes `reviewed` — which a rule may freeze once a doc is terminal.
    // Asking the rules over the proposed document means lifecycle never writes
    // a field its own `check` then flags, and never has to declare in advance
    // which fields its action touches. With an inert probe the rules are inert
    // and so is this guard.
    let plan = crate::mutate::Planned {
        rel_path: rel_path.to_path_buf(),
        content: new_content.clone(),
    };
    if let Some(lock) = probe
        .refusals(root, config, &[plan.proposed()], today)?
        .refusing(rel_path)
    {
        // The lock reads as a trailing clause rather than mid-sentence: it
        // is usually a rule id, but it can also name a lock that could not
        // be evaluated at all, and only a trailing position reads correctly
        // for both without implying a rule by that name exists.
        return Err(Error::Config(format!(
            "lifecycle {action_name} cannot complete: this document does not satisfy a lock \
             its baseline arms, so writing to it at all is refused — {lock}. The lock is \
             absolute, not a judgement on this action: a field that already differs from the \
             baseline is enough, whether or not {action_name} would touch it. `nodex check` \
             names the field; revert it, or supersede the record"
        )));
    }

    // The whole registry, over the project this transition produces. The
    // guards above each answer for one family the action was built around;
    // a status change reaches further than that — a `conditional_exclude`
    // parent going terminal drops its sub-artifacts, and every reference
    // into them is a check violation this write introduced.
    let introduced = crate::mutate::introduced(
        root,
        config,
        before,
        &[plan.proposed()],
        crate::mutate::ProposalDiff::Inert,
        today,
    )?;
    if let Some(refusal) = introduced.refusal(format!("lifecycle {action_name}")) {
        return Err(refusal);
    }

    path_guard::write_atomic_in_root(root, &abs_path, &new_content)?;

    Ok((new_content, introduced.advisories()))
}
