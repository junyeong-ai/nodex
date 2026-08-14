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
///
/// Each variant's doc states the rule the code stands for, never one
/// emitter's instance, because a code outlives the situation it was minted
/// for: several of these acquired a second and third emitter whose remedy
/// differs from the first, and a description written from the original read
/// as a claim the code no longer makes. Adding an emission site therefore
/// means reading the variant's doc against *every* site (`grep` for the
/// variant) and widening it to the rule they share — or, where they share
/// too little, saying so, as [`Self::BaselineInert`] does. The published
/// glosses in the skill are copies of these, so a description that drifts
/// here drifts everywhere a consumer reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WarningCode {
    /// What was read and what the project governs did not line up: a
    /// declaration that selected nothing (a `scope.include` or `identity`
    /// glob matching no file), a document no `identity` rule names, a part
    /// of the tree the walk never read (an undescended directory symlink, an
    /// empty scan), or a path handed to `check --content` that the scope does
    /// not admit — where a clean verdict certifies nothing at all.
    ///
    /// Stated as the relation rather than as a list of causes, because each
    /// is the same gap seen from one side or the other. Which side — and so
    /// whether the config or the argument is what to correct — is the
    /// message's to say.
    ScopeCoverage,
    /// The build cache could not be read as a cache, or could not be
    /// persisted; the next build re-parses from scratch (correct, just
    /// slower).
    ///
    /// A cache discarded because it does not describe this project says
    /// nothing — a foreign schema version, or a parse surface the config has
    /// since changed. Both are the expected invalidation after an edit or an
    /// upgrade rather than a fault, and cost a cold rebuild rather than an
    /// answer.
    Cache,
    /// The `graph.json` snapshot does not answer for the working tree, and
    /// the two ways it can fail to want opposite things. It diverges from
    /// what is on disk — `nodex build` refreshes it. Or the staleness probe
    /// that would have compared them failed, and a rebuild does not help:
    /// the probe's one fallible step is the scope walk a build begins with,
    /// so whatever stopped the comparison stops the build too, and the
    /// message names the path to make readable first.
    ///
    /// The result is served either way — staleness advises, never gates.
    ///
    /// A snapshot that cannot be *read* is not this: there is no snapshot to
    /// be stale, so it is a typed error (`GRAPH_MISSING`, `IO_ERROR`,
    /// `PARSE_ERROR`) that ends the command.
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
    /// The violations on screen are not the set the invocation describes,
    /// and they come apart in either direction: `--severity` shows a single
    /// severity of everything that was judged, while a `--since` that could
    /// not resolve judged the whole project instead of the slice asked for.
    ///
    /// The verdict is never what moved. `has_errors` and the exit code are
    /// drawn from every violation the rules judged, so a display filter can
    /// narrow the list without narrowing the answer — which is the whole
    /// reason a filter is safe to offer on a gate.
    GateSuppression,
    /// A git ref was leaned on and had nothing at the point it was asked
    /// about, in one of three shapes: a configured immutability baseline that
    /// could not engage at all (the root is not a git work tree, the ref does
    /// not carry the project), so no lock was enforced; a single document the
    /// baseline holds no node for, so the locks are inert for that document
    /// while engaged over the rest; or, on the comparison plane (`diff` /
    /// `impact`, where no immutability baseline is in play), a path the ref
    /// does not record, so one side of the comparison is missing it.
    ///
    /// The name is the sharpest of the three rather than the whole. What they
    /// share is that a report is narrower than it reads, and only the message
    /// can say where the narrowing happened.
    BaselineInert,
    /// Candidates were excluded from a ranking because they carry no score to
    /// rank by: no comparable signal with the target (`query similar`), or no
    /// positively-weighted component under the active weights (`query trust
    /// --top` / `--bottom`). Counted, never silently dropped — an absent
    /// score must never read as a zero one.
    RankingUnscored,
    /// An edit did not land the way the command meant it to, in one of two
    /// shapes that read very differently.
    ///
    /// Either something stood between the command and the write — a symlink,
    /// an immutability lock, an unclosed fence, an unreadable path, a
    /// mid-flight change — and the file was left alone. Or the write landed
    /// and took a reference somewhere the command could not follow it:
    /// `rename` moving a document out from under a relative reference it
    /// could not repoint.
    ///
    /// The second splits by what the reference names afterwards. A *different
    /// valid document* is the one to read closely: the command succeeded and
    /// the graph it produced is valid, so nothing downstream has anything to
    /// say and this warning is the only place it is said. *Nothing* surfaces
    /// on the next build as an unresolved edge, which `query issues` counts
    /// and `check` reds at whatever severity
    /// `[[detection.unresolved_policy]]` gives the cause — there the warning
    /// is what says the rename gave up rather than never having tried, and
    /// says it now rather than a build later.
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
