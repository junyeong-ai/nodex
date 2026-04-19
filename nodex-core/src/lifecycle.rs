use chrono::Local;
use std::path::Path;

use crate::config::Config;
use crate::error::{Error, Result};

/// Canonical status values produced by each non-review lifecycle action.
///
/// These are part of the tool's operational contract — `lifecycle
/// archive` means exactly "set status to archived". Projects may add
/// extra statuses to `statuses.allowed`, but must keep these four so
/// lifecycle-written documents pass `status` enum validation.
/// `Config::validate` enforces the coverage at load time.
pub const SUPERSEDED: &str = "superseded";
pub const ARCHIVED: &str = "archived";
pub const DEPRECATED: &str = "deprecated";
pub const ABANDONED: &str = "abandoned";

/// All status values the lifecycle command can write.
/// Used by `Config::validate` to enforce vocabulary coverage.
pub const LIFECYCLE_TARGET_STATUSES: &[&str] = &[SUPERSEDED, ARCHIVED, DEPRECATED, ABANDONED];

/// Lifecycle action.
///
/// Variants that need additional data (a successor id for `Supersede`)
/// carry it in-line so callers cannot invoke `transition()` with the
/// wrong combination of fields. The CLI layer and any library consumer
/// are structurally forced to supply the successor when — and only
/// when — they intend to supersede.
#[derive(Debug, Clone)]
pub enum Action<'a> {
    Supersede { successor: &'a str },
    Archive,
    Deprecate,
    Abandon,
    Review,
}

impl Action<'_> {
    /// Target status written to the document, or `None` for review
    /// (which only touches the `reviewed` date).
    pub fn target_status(&self) -> Option<&'static str> {
        match self {
            Self::Supersede { .. } => Some(SUPERSEDED),
            Self::Archive => Some(ARCHIVED),
            Self::Deprecate => Some(DEPRECATED),
            Self::Abandon => Some(ABANDONED),
            Self::Review => None,
        }
    }

    /// String rendering of an action, exposed for logging / JSON output.
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

/// Apply a lifecycle transition to a document file.
/// Returns the updated file content.
pub fn transition(
    root: &Path,
    rel_path: &Path,
    action: Action<'_>,
    config: &Config,
) -> Result<String> {
    let abs_path = root.join(rel_path);
    let content = std::fs::read_to_string(&abs_path).map_err(|e| Error::Io {
        path: abs_path.clone(),
        source: e,
    })?;

    let (yaml_opt, body) = crate::parser::frontmatter::split_frontmatter(&content);
    let Some(yaml_str) = yaml_opt else {
        return Err(Error::Frontmatter {
            path: abs_path,
            message: "no frontmatter found".to_string(),
        });
    };

    let mut fm: yaml_serde::Value = yaml_serde::from_str(yaml_str).map_err(|e| Error::Yaml {
        path: abs_path.clone(),
        source: e,
    })?;

    let mapping = fm.as_mapping_mut().ok_or_else(|| Error::Frontmatter {
        path: abs_path.clone(),
        message: "frontmatter is not a YAML mapping".to_string(),
    })?;

    // Validate current status
    let current_status = mapping
        .get(yaml_serde::Value::String("status".to_string()))
        .and_then(|v| v.as_str())
        .unwrap_or("active")
        .to_string();

    if config.is_terminal(&current_status) && !matches!(action, Action::Review) {
        return Err(Error::InvalidTransition {
            node_id: rel_path.to_string_lossy().to_string(),
            from: current_status,
            to: action.target_status().unwrap_or("review").to_string(),
        });
    }

    let today = Local::now().date_naive().to_string();

    match action {
        Action::Supersede { successor } => {
            set_field(mapping, "status", SUPERSEDED);
            set_field(mapping, "superseded_by", successor);
            set_field(mapping, "updated", &today);
        }
        Action::Archive => {
            set_field(mapping, "status", ARCHIVED);
            set_field(mapping, "updated", &today);
        }
        Action::Deprecate => {
            set_field(mapping, "status", DEPRECATED);
            set_field(mapping, "updated", &today);
        }
        Action::Abandon => {
            set_field(mapping, "status", ABANDONED);
            set_field(mapping, "updated", &today);
        }
        Action::Review => {
            set_field(mapping, "reviewed", &today);
        }
    }

    // Reconstruct file
    let new_yaml = yaml_serde::to_string(&fm)
        .map_err(|e| Error::Other(format!("YAML serialization error: {e}")))?;

    let new_content = format!("---\n{new_yaml}---\n{body}");

    std::fs::write(&abs_path, &new_content).map_err(|e| Error::Io {
        path: abs_path,
        source: e,
    })?;

    Ok(new_content)
}

fn set_field(mapping: &mut yaml_serde::Mapping, key: &str, value: &str) {
    mapping.insert(
        yaml_serde::Value::String(key.to_string()),
        yaml_serde::Value::String(value.to_string()),
    );
}
