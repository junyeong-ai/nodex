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
    /// The corpus and the config do not cover each other: a declaration that
    /// selected nothing (a `scope.include` or `identity` glob matching no
    /// file), a document no `identity` rule names, or a part of the tree the
    /// walk never read (an undescended directory symlink, an empty scan).
    ///
    /// Stated as the relation rather than as a list of causes, because each
    /// is the same fact from one side or the other and every remedy is an
    /// edit to the config — which the message names.
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
    /// A mutation left a follow-up the operator should run before the
    /// graph is consistent again: a scaffolded document is not yet in the
    /// graph (run `nodex build`), a config-default scaffold left a rule
    /// unsatisfied (a placeholder to fill), or a rename changed a
    /// frontmatter-less document's inferred id so cross-references to it
    /// must be re-anchored (add an explicit `id:` to the moved file). The
    /// message names the specific action.
    BuildRecommended,
    /// The running binary falls outside the project's pinned
    /// `meta.nodex_version` range.
    BinaryCompat,
    /// The violations reported are not the set that was asked for:
    /// `--severity` shows one severity of what was judged, or a `--since`
    /// that could not resolve widened the report back to the whole project.
    ///
    /// The verdict is never what moved. `has_errors` and the exit code are
    /// drawn from every violation the rules judged, so a display filter can
    /// narrow the list without narrowing the answer — which is the whole
    /// reason a filter is safe to offer on a gate.
    GateSuppression,
    /// A configured immutability baseline could not engage this run (e.g.
    /// the root is not a git work tree), so the locks were not enforced.
    BaselineInert,
    /// Candidates were excluded from a ranking because they shared no
    /// comparable signal with the target — counted, never silently
    /// dropped.
    RankingUnscored,
    /// A mutation could not write a file it set out to: a symlink, an
    /// immutability lock, an unclosed fence, an unreadable path, a mid-flight
    /// change. Something is between the command and the edit, and the message
    /// names it.
    FileSkipped,
    /// A mutation left a reference standing that it moves everywhere else,
    /// because moving it there would turn it on the document holding it.
    ///
    /// Its own code rather than a [`Self::FileSkipped`] whose message happens
    /// to ask for nothing: a consumer branches on the vocabulary, so "I could
    /// not do this" and "I was never going to do this here" cannot share one.
    /// Nothing downstream reports it either — the reference goes on naming a
    /// document that still exists — so the command is the only place the
    /// difference between that and referencing nothing at all is known.
    ReferenceKept,
    /// A mutation dropped a document from the project that it never named: a
    /// `[[scope.conditional_exclude]]` rule evicts the `child_glob` matches in
    /// a terminal parent's directory subtree, and the write that made the
    /// parent terminal is what evicted them. The file is untouched and no rule
    /// guards it from here on.
    ///
    /// Its own code because nothing else can carry it. A gate reports the
    /// findings a proposal introduces, and a document leaving the project
    /// takes its findings with it — so the one write that ends a document's
    /// governance is the one place that fact exists to be reported.
    DocumentEvicted,
}

impl WarningCode {
    /// Every code, in declaration order — the published vocabulary
    /// `export diagnostics` emits. Keep it complete: the exhaustive
    /// `match` in `all_is_exhaustive` forces a new variant into the *match
    /// arms*, and the length assert beside it is the actual completeness
    /// guard for this slice — bump it with every variant.
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
        Self::ReferenceKept,
        Self::DocumentEvicted,
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
        // The exhaustive match forces a newly added variant into the match
        // arms (a compile error otherwise) — but it cannot force it into
        // `ALL`, which the loop only ever walks. The two asserts below are
        // the real `ALL`-completeness guard: the unique count rejects a
        // duplicate entry masking a missing one, and the length pins the
        // total — bump it with every variant.
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
                | WarningCode::FileSkipped
                | WarningCode::ReferenceKept
                | WarningCode::DocumentEvicted => {}
            }
        }
        for (i, a) in WarningCode::ALL.iter().enumerate() {
            for b in &WarningCode::ALL[i + 1..] {
                assert_ne!(a, b, "WarningCode::ALL has a duplicate entry");
            }
        }
        assert_eq!(WarningCode::ALL.len(), 12);
    }

    #[test]
    fn code_serializes_snake_case() {
        let w = Warning::new(WarningCode::GateSuppression, "hid 1");
        let v = serde_json::to_value(&w).unwrap();
        assert_eq!(v["code"], "gate_suppression");
        assert_eq!(v["message"], "hid 1");
    }
}
