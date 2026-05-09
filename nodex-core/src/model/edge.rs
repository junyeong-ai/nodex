use serde::{Deserialize, Serialize};

/// A resolved edge in the graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub source: String,
    pub target: ResolvedTarget,
    pub relation: String,
    /// Source location, e.g. "L42" or "frontmatter:supersedes".
    pub location: String,
}

/// Type-safe representation of an edge target. `Hash + Ord` participate
/// in `Edge` deduplication, so two unresolved edges with the same `raw`
/// but different `reason` strings still collapse — the *target* is the
/// raw string the user wrote, not our diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

    /// Component used for edge deduplication. For unresolved targets we
    /// key on the raw user-written string and ignore the diagnostic
    /// `reason`, so two callers' different explanations don't yield a
    /// duplicate edge.
    fn dedup_target(&self) -> DedupTarget {
        match self {
            Self::Resolved { id } => DedupTarget::Resolved(id.clone()),
            Self::Unresolved { raw, .. } => DedupTarget::Unresolved(raw.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum DedupTarget {
    Resolved(String),
    Unresolved(String),
}

impl Edge {
    /// Identity for deduplication: source, target, and relation.
    /// `location` is not part of identity — two authoring sites for
    /// the same logical relation collapse to the first encountered.
    pub(crate) fn identity(&self) -> EdgeIdentity {
        EdgeIdentity {
            source: self.source.clone(),
            target: self.target.dedup_target(),
            relation: self.relation.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct EdgeIdentity {
    source: String,
    target: DedupTarget,
    relation: String,
}

/// An edge before target resolution (produced by the parser).
///
/// Carries `Serialize` / `Deserialize` so the build cache stores it
/// directly — there is no mirror struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawEdge {
    /// Raw target path or id from the document.
    pub target_path: String,
    pub relation: String,
    /// Source location, e.g. "L42" or "frontmatter:supersedes".
    pub location: String,
}
