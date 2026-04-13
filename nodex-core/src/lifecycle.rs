use chrono::Local;
use std::path::Path;

use crate::config::Config;
use crate::error::{Error, Result};

/// Lifecycle action.
#[derive(Debug, Clone, Copy)]
pub enum Action {
    Supersede,
    Archive,
    Deprecate,
    Abandon,
    Review,
}

impl Action {
    pub fn target_status(&self) -> Option<&str> {
        match self {
            Self::Supersede => Some("superseded"),
            Self::Archive => Some("archived"),
            Self::Deprecate => Some("deprecated"),
            Self::Abandon => Some("abandoned"),
            Self::Review => None, // Review doesn't change status
        }
    }
}

/// Apply a lifecycle transition to a document file.
/// Returns the updated file content.
pub fn transition(
    root: &Path,
    rel_path: &Path,
    action: Action,
    successor_id: Option<&str>,
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
        Action::Supersede => {
            let successor = successor_id
                .ok_or_else(|| Error::Other("supersede requires a successor_id".to_string()))?;
            set_field(mapping, "status", "superseded");
            set_field(mapping, "superseded_by", successor);
            set_field(mapping, "updated", &today);
        }
        Action::Archive => {
            set_field(mapping, "status", "archived");
            set_field(mapping, "updated", &today);
        }
        Action::Deprecate => {
            set_field(mapping, "status", "deprecated");
            set_field(mapping, "updated", &today);
        }
        Action::Abandon => {
            set_field(mapping, "status", "abandoned");
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
