//! Lifecycle state transitions for documents.
//!
//! Each transition rewrites a small number of frontmatter scalar fields
//! through [`crate::parser::editor::FrontmatterEditor`] — never a full
//! YAML round-trip — so the user's key order, comments, blank lines,
//! and quoting style survive intact. A status change produces a
//! one-line diff.

use chrono::Local;
use std::path::Path;

use crate::config::Config;
use crate::error::{Error, ParseError, Result};
use crate::model::{Edge, Graph, ResolvedTarget};
use crate::parser::editor::{FrontmatterEditor, Scalar};
use crate::parser::frontmatter::split_frontmatter;
use crate::path_guard;

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
pub fn transition(root: &Path, rel_path: &Path, action: Action, config: &Config) -> Result<String> {
    let abs_path = root.join(rel_path);

    if path_guard::is_symlink(&abs_path) {
        return Err(Error::OutsideRoot(rel_path.to_path_buf()));
    }

    let content = std::fs::read_to_string(&abs_path).map_err(|source| Error::Io {
        path: abs_path.clone(),
        source,
    })?;
    let content = crate::parser::frontmatter::canonicalize(&content);

    let (yaml_opt, body) = split_frontmatter(&content);
    let Some(yaml_str) = yaml_opt else {
        return Err(Error::Parse {
            path: abs_path,
            source: ParseError::FrontmatterDelimiter,
        });
    };

    let mut editor = FrontmatterEditor::parse(yaml_str, &abs_path)?;

    // The id anchors error messages on the *node* the user operated
    // on rather than its on-disk path.
    let node_id = match editor.scalar("id") {
        Scalar::Value(s) => s.to_string(),
        _ => rel_path.to_string_lossy().into_owned(),
    };

    // Missing status is treated as non-terminal so a fresh document
    // can still receive its first lifecycle action; a non-scalar
    // status is an authoring error the editor cannot reason about.
    let current_status = match editor.scalar("status") {
        Scalar::Value(s) => s.to_string(),
        Scalar::Absent => String::new(),
        Scalar::NonScalar => {
            return Err(Error::Parse {
                path: abs_path,
                source: ParseError::InvalidField {
                    field: "status".into(),
                    expected: "scalar string",
                },
            });
        }
    };

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
        let kind = match editor.scalar("kind") {
            Scalar::Value(k) => crate::model::Kind::new(k.as_ref()),
            _ => crate::parser::identity::infer_kind(rel_path, &config.identity),
        };
        let allowed = config.allowed_statuses_for(kind.as_str());
        if !allowed.iter().any(|s| s == target) {
            return Err(Error::Config(format!(
                "lifecycle {} writes status \"{target}\", but kind \"{}\" does not allow it; \
                 add \"{target}\" to statuses.allowed (or the kind's status enum) to enable \
                 this action",
                action.name(),
                kind.as_str(),
            )));
        }

        // Self-consistency invariant: `set` writes only `status` (and
        // `updated`), so it must refuse a target a `cross_field` rule
        // governs while the required field is missing — otherwise the
        // generic setter could write a document its own `check` rejects.
        // The structural payload comes from a dedicated action
        // (`supersede` supplies `superseded_by`) or must already be on
        // the document. Config-driven: a project that places no
        // requirement on the status sets it freely. `supersede`/`review`
        // supply their own fields and are exempt. "Missing" is decided
        // by the cross_field rule's own `is_field_missing` over the same
        // parsed node the rule would see, so the guard and the rule can
        // never disagree (built-in scalars, typed attrs, collections).
        if matches!(action, Action::SetStatus { .. }) {
            let (node, _) = crate::parser::frontmatter::parse_frontmatter(&abs_path, &content)?;
            if let Some(required) = unsatisfied_cross_field(config, kind.as_str(), target, &node) {
                return Err(Error::Config(format!(
                    "lifecycle set cannot write status \"{target}\": cross_field rule requires \
                     \"{required}\" for it, but the document does not declare it; use the \
                     dedicated action that supplies it (e.g. `supersede` for superseded) or set \
                     \"{required}\" first"
                )));
            }
        }
    }

    let today = Local::now().date_naive().to_string();

    match action {
        Action::Supersede { successor } => {
            editor.set("status", SUPERSEDED);
            editor.set("superseded_by", &successor);
            editor.set("updated", &today);
        }
        Action::SetStatus { status } => {
            editor.set("status", &status);
            editor.set("updated", &today);
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
                && existing_date > chrono::Local::now().date_naive()
            {
                return Err(Error::Transition {
                    node_id,
                    from: existing.to_string(),
                    to: today,
                });
            }
            editor.set("reviewed", &today);
        }
    }

    let new_content = format!("---\n{}---\n{body}", editor.render());

    path_guard::write_atomic_in_root(root, &abs_path, &new_content)?;

    Ok(new_content)
}

/// The field a `cross_field` rule requires for `status` but that the
/// document is missing, or `None` when setting `status` keeps the
/// document check-clean. Only status-keyed predicates are considered: a
/// `set` writes nothing but `status`, so a requirement gated on any
/// other field is unaffected by the action and stays the operator's
/// concern. "Missing" is the rule's own [`is_field_missing`] over the
/// parsed node, so the guard agrees with the check by construction —
/// for built-in scalars, typed attrs (where an explicitly empty value
/// counts as missing), and collections alike.
///
/// [`is_field_missing`]: crate::rules::schema::is_field_missing
fn unsatisfied_cross_field(
    config: &Config,
    kind: &str,
    status: &str,
    node: &crate::model::Node,
) -> Option<String> {
    config.cross_field_for(kind).into_iter().find_map(|cf| {
        let predicate = crate::config::parse_when(&cf.when).ok()?;
        (status_predicate_activates(&predicate, status)
            && crate::rules::schema::is_field_missing(node, &cf.require))
        .then_some(cf.require)
    })
}

/// Whether writing `status` makes `predicate` hold, for predicates keyed
/// on the `status` field. Non-status predicates return `false` — the
/// write doesn't touch them. The exhaustive match means a new
/// [`WhenPredicate`] variant forces this guard to be reconsidered.
fn status_predicate_activates(predicate: &crate::config::WhenPredicate, status: &str) -> bool {
    use crate::config::WhenPredicate;
    match predicate {
        WhenPredicate::Equals { field, value } => field == "status" && value == status,
        WhenPredicate::In { field, values } => {
            field == "status" && values.iter().any(|v| v == status)
        }
        WhenPredicate::Exists { field } => field == "status",
        WhenPredicate::NotExists { .. } => false,
    }
}
