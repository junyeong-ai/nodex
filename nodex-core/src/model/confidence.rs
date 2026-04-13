use serde::{Deserialize, Serialize};
use std::fmt;

/// How an edge was discovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Directly found in frontmatter or markdown link.
    Extracted,
    /// Derived by rule-based reasoning (e.g., same-directory relatedness).
    Inferred,
}

impl fmt::Display for Confidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Extracted => f.write_str("extracted"),
            Self::Inferred => f.write_str("inferred"),
        }
    }
}
