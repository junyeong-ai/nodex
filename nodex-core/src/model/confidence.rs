use serde::{Deserialize, Serialize};
use std::fmt;

/// How an edge was discovered.
///
/// Today every edge nodex produces is `Extracted` — directly observed
/// in frontmatter or markdown. The enum stays as the extension point
/// for rule-based inference (same-directory relatedness, similarity
/// heuristics, etc.), but variants are added only when a producer
/// exists, not speculatively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Directly found in frontmatter or markdown link.
    Extracted,
}

impl fmt::Display for Confidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Extracted => f.write_str("extracted"),
        }
    }
}
