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
use crate::parser::editor::{FrontmatterEditor, Scalar};
use crate::parser::frontmatter::split_frontmatter;
use crate::path_guard;

/// Canonical statuses written by each non-review lifecycle action.
///
/// Projects may extend `statuses.allowed` freely but must keep these
/// four — `Config::validate` enforces the coverage at load so a
/// transition never writes a value the same config rejects.
pub const SUPERSEDED: &str = "superseded";
pub const ARCHIVED: &str = "archived";
pub const DEPRECATED: &str = "deprecated";
pub const ABANDONED: &str = "abandoned";

/// All statuses the lifecycle command can write. Read by
/// `Config::validate` to enforce vocabulary coverage.
pub const LIFECYCLE_TARGET_STATUSES: &[&str] = &[SUPERSEDED, ARCHIVED, DEPRECATED, ABANDONED];

/// A lifecycle action. Variants carry the data their action needs
/// in-line so callers cannot supply the wrong combination of fields —
/// `supersede` structurally requires a successor, the others reject one.
#[derive(Debug, Clone)]
pub enum Action {
    Supersede { successor: String },
    Archive,
    Deprecate,
    Abandon,
    Review,
}

impl Action {
    /// Target status written to the document, or `None` for review.
    pub fn target_status(&self) -> Option<&'static str> {
        match self {
            Self::Supersede { .. } => Some(SUPERSEDED),
            Self::Archive => Some(ARCHIVED),
            Self::Deprecate => Some(DEPRECATED),
            Self::Abandon => Some(ABANDONED),
            Self::Review => None,
        }
    }

    /// Short name for logging / JSON output.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Supersede { .. } => "supersede",
            Self::Archive => "archive",
            Self::Deprecate => "deprecate",
            Self::Abandon => "abandon",
            Self::Review => "review",
        }
    }
}

/// Apply a lifecycle transition to a document file. Returns the new
/// file content. Symlinks are refused (writing through one could
/// escape the project root); the scanner still follows them on read.
pub fn transition(root: &Path, rel_path: &Path, action: Action, config: &Config) -> Result<String> {
    let abs_path = root.join(rel_path);

    if path_guard::is_symlink(&abs_path) {
        return Err(Error::OutsideRoot(rel_path.to_path_buf()));
    }

    let content = std::fs::read_to_string(&abs_path).map_err(|source| Error::Io {
        path: abs_path.clone(),
        source,
    })?;

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

    let today = Local::now().date_naive().to_string();

    match action {
        Action::Supersede { successor } => {
            editor.set("status", SUPERSEDED);
            editor.set("superseded_by", &successor);
            editor.set("updated", &today);
        }
        Action::Archive => {
            editor.set("status", ARCHIVED);
            editor.set("updated", &today);
        }
        Action::Deprecate => {
            editor.set("status", DEPRECATED);
            editor.set("updated", &today);
        }
        Action::Abandon => {
            editor.set("status", ABANDONED);
            editor.set("updated", &today);
        }
        Action::Review => {
            editor.set("reviewed", &today);
        }
    }

    let new_content = format!("---\n{}---\n{body}", editor.render());

    path_guard::write_atomic(&abs_path, &new_content)?;

    Ok(new_content)
}
