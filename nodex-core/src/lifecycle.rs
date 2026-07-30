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
pub fn transition(
    root: &Path,
    rel_path: &Path,
    action: Action,
    config: &Config,
    probe: &crate::mutate::BaselineProbe,
) -> Result<String> {
    let abs_path = root.join(rel_path);

    if path_guard::is_symlink(&abs_path) {
        return Err(Error::OutsideRoot(rel_path.to_path_buf()));
    }

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

    // Refuse to write through a node carrying field-level parse issues:
    // the broken field reads as absent, so a transition would launder a
    // value `check` flags into a document the tool just touched. The
    // first issue (sorted by field) names the field to fix. Scaffold
    // (new files) and migrate (bare files) structurally cannot meet
    // this state; rename/retarget refuse only on an unsplittable fence
    // because they edit identity/relations, not typed field state.
    // Every refusal in this function attributes to the absolute path,
    // including the whole-document failure this parse can raise.
    let (parsed, _) =
        crate::parser::frontmatter::parse_frontmatter(rel_path, &content).map_err(|e| match e {
            Error::Parse { source, .. } => Error::Parse {
                path: abs_path.clone(),
                source,
            },
            other => other,
        })?;
    if let Some(issue) = parsed.parse_issues.first() {
        return Err(Error::Parse {
            path: abs_path,
            source: ParseError::InvalidField {
                field: issue.field.clone(),
                expected: issue.expected.clone(),
            },
        });
    }

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
                    expected: "scalar string".into(),
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
    }

    // Capture this action's name and the fields it writes before the
    // action is consumed by the match below. The post-write
    // self-consistency guard considers only cross_field predicates keyed
    // on these fields — the ones the transition actually changes — so it
    // never false-rejects on a pre-existing problem the action did not
    // cause.
    let action_name = action.name();
    let written_fields: &[&str] = match &action {
        Action::Supersede { .. } => &["status", "superseded_by", "updated"],
        Action::SetStatus { .. } => &["status", "updated"],
        Action::Review => &["reviewed"],
    };

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

    // Self-consistency invariant: NO transition may produce a document
    // its own `check` would reject. Evaluate the cross_field requirement
    // against the exact post-write node `check` will build from these
    // bytes — inferred id/kind/status, the just-written fields, typed
    // attrs, and collections alike — so the guard and the rule agree by
    // construction, for EVERY action. `supersede` writing `superseded_by`
    // satisfies a rule keyed on it; a rule requiring any OTHER field once
    // superseded must still refuse, exactly as `set` does for the same
    // target status (the gap this closes).
    let parse_config = crate::parser::ParseConfig::new(config);
    let parsed = crate::parser::parse_document(rel_path, &new_content, &parse_config)?;
    if let Some(required) = unsatisfied_cross_field(
        config,
        parsed.node.kind.as_str(),
        &parsed.node,
        written_fields,
    ) {
        return Err(Error::Config(format!(
            "lifecycle {action_name} cannot complete: a cross_field rule requires \
             \"{required}\" for this transition, but the document does not declare it — \
             set \"{required}\" first"
        )));
    }

    // Symmetric immutability guard (the same `frontmatter_immutable` lock
    // `rename` / `retarget` writer-skip via `rewrite_lock_reason`): the
    // terminal guard above already blocks `set`/`supersede` on a terminal
    // doc, but `review` is exempt from it and writes `reviewed` — which a
    // rule may freeze once a doc is terminal. Refuse so lifecycle never
    // writes a field its own `frontmatter_immutable` check then flags.
    // With an inert probe (not git / no `immutable_baseline`) the rule is
    // inert and so is this guard.
    if let Some(lock) = crate::rules::body_immutable::frontmatter_write_lock(
        &new_content,
        rel_path,
        config,
        probe,
        written_fields,
    )? {
        // The lock reads as a trailing clause rather than mid-sentence: it
        // is usually a rule id, but it can also name a lock that could not
        // be evaluated at all, and only a trailing position reads correctly
        // for both without implying a rule by that name exists.
        return Err(Error::Config(format!(
            "lifecycle {action_name} cannot complete: it would rewrite a field locked on this \
             document once its status is terminal — {lock}"
        )));
    }

    path_guard::write_atomic_in_root(root, &abs_path, &new_content)?;

    Ok(new_content)
}

/// Fields a `set` transition writes — the only fields whose value the
/// action can change, and therefore the only ones a `cross_field`
/// predicate it must answer for can be keyed on.
///
/// The field a `cross_field` rule requires but that the post-write
/// `node` is missing, or `None` when the transition is check-clean.
/// `node` is the fully-parsed post-write document, so it carries exactly
/// what `check` will see — inferred id/kind/status plus the fields the
/// action just wrote (`written`).
///
/// Only predicates keyed on a field the action wrote are considered: a
/// requirement gated on any other field is unaffected by the action and
/// stays the operator's concern, so surfacing it would false-reject on a
/// pre-existing problem the action did not cause. For those in-scope
/// predicates the guard reuses the rule's own
/// [`predicate_matches_node`] and [`is_field_missing`], so the guard and
/// `check` agree by construction for every field kind.
///
/// [`predicate_matches_node`]: crate::rules::schema::predicate_matches_node
/// [`is_field_missing`]: crate::rules::schema::is_field_missing
fn unsatisfied_cross_field(
    config: &Config,
    kind: &str,
    node: &crate::model::Node,
    written: &[&str],
) -> Option<String> {
    config.cross_field_for(kind).into_iter().find_map(|cf| {
        // `Config::load` parses every `cross_field.when`
        // (`validate_cross_field_syntax`), so the predicate always
        // parses here.
        let predicate = crate::config::parse_when(&cf.when).expect("validated by Config::load");
        (written.contains(&predicate.field())
            && crate::rules::schema::predicate_matches_node(&predicate, node)
            && crate::rules::schema::is_field_missing(node, &cf.require))
        .then_some(cf.require)
    })
}
