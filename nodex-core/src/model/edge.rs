use serde::{Deserialize, Serialize};

use super::confidence::Confidence;

/// A resolved edge in the graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub source: String,
    pub target: ResolvedTarget,
    pub relation: String,
    pub confidence: Confidence,
    /// Source location, e.g. "L42" or "frontmatter:supersedes".
    pub location: String,
}

/// Type-safe representation of an edge target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResolvedTarget {
    /// Successfully resolved to a node id.
    Resolved { id: String },
    /// Could not be resolved — external or missing reference.
    Unresolved { raw: String, reason: String },
}

impl ResolvedTarget {
    pub fn resolved(id: impl Into<String>) -> Self {
        Self::Resolved { id: id.into() }
    }

    pub fn unresolved(raw: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Unresolved {
            raw: raw.into(),
            reason: reason.into(),
        }
    }

    /// Returns the resolved node id, or `None` if unresolved.
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Resolved { id } => Some(id),
            Self::Unresolved { .. } => None,
        }
    }
}

/// An edge before target resolution (produced by the parser).
#[derive(Debug, Clone)]
pub struct RawEdge {
    /// Raw target path or id from the document.
    pub target_path: String,
    pub relation: String,
    pub confidence: Confidence,
    /// Source location, e.g. "L42" or "frontmatter:supersedes".
    pub location: String,
}
