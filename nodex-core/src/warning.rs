//! Typed advisory warnings — the non-fatal counterpart to
//! [`crate::error::Error`].
//!
//! Every warning the build or a CLI command surfaces carries a stable
//! [`WarningCode`] an agent branches on, plus the rendered human detail
//! in `message`. This mirrors the `error.code` discipline on the failure
//! plane: a consumer matches the closed `code` vocabulary instead of
//! regex-ing prose, and the message stays free to carry dynamic detail
//! (paths, counts) without a per-field schema. The published
//! `code` set is exported by `nodex export diagnostics`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The closed category of an advisory [`Warning`]. Adding a warning means
/// adding a variant here, so the vocabulary a consumer codegens stays
/// exhaustive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WarningCode {
    /// A `scope.include` / `identity` glob matched no files — a possible
    /// mis-scope (the corpus, or part of it, went unscanned).
    ScopeCoverage,
    /// The build cache could not be loaded (corrupt / foreign) or
    /// persisted; the next build re-parses from scratch (correct, just
    /// slower).
    Cache,
    /// The `graph.json` snapshot diverges from the working tree, or could
    /// not be read — a `nodex build` would refresh it.
    SnapshotDivergence,
    /// A scaffold target closely resembles an existing document; consider
    /// `lifecycle supersede` instead of creating a duplicate.
    SimilarDocument,
    /// A written document needs a follow-up before it is complete or
    /// visible: it is not yet in the graph (run `nodex build`), or a
    /// config-default scaffold left a rule unsatisfied (a placeholder to
    /// fill). The message names the specific action.
    BuildRecommended,
    /// The running binary falls outside the project's pinned
    /// `meta.nodex_version` range.
    BinaryCompat,
    /// `--severity` hid lower-severity violations from the gate output;
    /// the gate verdict still reflects them.
    GateSuppression,
    /// A configured immutability baseline could not engage this run (e.g.
    /// the root is not a git work tree), so the locks were not enforced.
    BaselineInert,
    /// Candidates were excluded from a ranking because they shared no
    /// comparable signal with the target — counted, never silently
    /// dropped.
    RankingUnscored,
    /// A mutation skipped a file (a symlink, an immutability lock, an
    /// unclosed fence, an unreadable path, or a mid-flight change).
    FileSkipped,
}

impl WarningCode {
    /// Every code, in declaration order — the published vocabulary
    /// `export diagnostics` emits. The exhaustive `match` in
    /// `all_is_exhaustive` forces a new variant to be added here too.
    pub const ALL: &'static [WarningCode] = &[
        Self::ScopeCoverage,
        Self::Cache,
        Self::SnapshotDivergence,
        Self::SimilarDocument,
        Self::BuildRecommended,
        Self::BinaryCompat,
        Self::GateSuppression,
        Self::BaselineInert,
        Self::RankingUnscored,
        Self::FileSkipped,
    ];
}

/// A non-fatal advisory: a stable [`WarningCode`] plus its rendered
/// detail. Lives at the envelope level (`{ ok, data, warnings }`), never
/// inside `data`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Warning {
    pub code: WarningCode,
    pub message: String,
}

impl Warning {
    /// Construct a warning from its category and rendered detail.
    pub fn new(code: WarningCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_is_exhaustive() {
        // The exhaustive match breaks compilation when a variant is added,
        // forcing it into `ALL` too; the count guards against a stale entry.
        for code in WarningCode::ALL {
            match code {
                WarningCode::ScopeCoverage
                | WarningCode::Cache
                | WarningCode::SnapshotDivergence
                | WarningCode::SimilarDocument
                | WarningCode::BuildRecommended
                | WarningCode::BinaryCompat
                | WarningCode::GateSuppression
                | WarningCode::BaselineInert
                | WarningCode::RankingUnscored
                | WarningCode::FileSkipped => {}
            }
        }
        assert_eq!(WarningCode::ALL.len(), 10);
    }

    #[test]
    fn code_serializes_snake_case() {
        let w = Warning::new(WarningCode::GateSuppression, "hid 1");
        let v = serde_json::to_value(&w).unwrap();
        assert_eq!(v["code"], "gate_suppression");
        assert_eq!(v["message"], "hid 1");
    }
}
