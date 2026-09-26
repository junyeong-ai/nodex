use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Document kind. Config-driven — no hardcoded variants.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct Kind(String);

impl Kind {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Whether a config `kinds` filter admits `kind` — an empty filter admits
/// every kind. The one reading of such a filter, whichever block declares
/// it: a per-block rule or annotation, a lock block, `statuses.flow`. A node
/// asks it through [`crate::model::Node::matches_kinds`]. A schema or trust
/// override's `kinds` is not such a filter: it selects by membership and is
/// refused empty at load.
pub(crate) fn kind_allowed(kinds: &[String], kind: &str) -> bool {
    kinds.is_empty() || kinds.iter().any(|k| k == kind)
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Kind {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}
