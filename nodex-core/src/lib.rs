pub mod builder;
pub mod command_result;
pub mod config;
pub mod diff;
pub mod error;
pub mod export;
pub mod git;
pub(crate) mod hash;
pub mod impact;
pub mod lifecycle;
pub mod model;
pub mod mutate;
pub mod output;
pub mod parser;
pub mod path_guard;
pub mod query;
pub mod reference_rewrite;
pub mod retarget;
pub mod rules;
pub mod scaffold;
pub mod status;
pub(crate) mod yaml_text;

// ─── Facade ─────────────────────────────────────────────────────────
//
// Consumers should address the symbols below rather than the internal
// module paths, so the canonical surface stays stable when modules are
// reorganised. Less-common items remain reachable via their module
// path (e.g. `nodex_core::query::trust::compute_trust`).

pub use command_result::{
    BuildResult, CheckResult, IdStability, InitResult, LifecycleResult, MigrateResult,
    MigrationChange, ProposalEntry, RenameResult, ReportResult, RetargetResult,
};
pub use config::{
    AnnotationConfig, BUILTIN_FRONTMATTER_FIELDS, BodyImmutableMode, BodyImmutableRuleConfig,
    BodyLineRuleConfig, Config, FrontmatterImmutableRuleConfig, MetaConfig, SchemaMode,
    UnresolvedPolicyRuleConfig, UnresolvedSeverity,
};
pub use diff::{BodyChange, EdgeRef, FieldChange, GraphDiff, StatusTransition, compute_diff};
pub use error::{Error, ParseError, Result};
pub use export::{
    CommandManifestEntry, CommandMode, CommandsManifest, ConfigManifest, ContractChange,
    EnumsManifest, EnvelopeSchemaDiff, EnvelopeSchemaManifest, IdentityManifest, PositionalEntry,
    RuleManifestEntry, RuleSource, RulesManifest, SchemaManifest, StatusesManifest,
    compute_envelope_schema_diff, export_config, export_enums, export_envelope_schema,
    export_rules, export_schema,
};
pub use impact::{ChangeKind, ImpactEntry, ImpactReport, compute_impact};
pub use lifecycle::{Action, check_supersede_safe, transition};
pub use model::{
    Annotation, BUILTIN_EDGE_RELATIONS, BodyLineMatch, Edge, FieldParseIssue, Graph, GraphMeta,
    Kind, Node, ParseFailure, RawAnnotation, RawBodyLineMatch, RawEdge, ResolvedTarget, Status,
    UnresolvedCause,
};
pub use mutate::{BaselineProbe, FileOutcome, RewriteLock, SkipReason, apply_to_file};
pub use query::annotations::{
    AnnotationEntry, AnnotationGroup, AnnotationOptions, AnnotationSourceRef, find_annotations,
};
pub use query::dependents::{DependentEntry, DependentsReport, find_dependents};
pub use query::detect::{OrphanEntry, StaleEntry, find_orphans, find_stale};
pub use query::issues::{IssueReport, IssueSummary, UnresolvedEdge, find_issues};
pub use query::listing::{NodeFilter, find_nodes};
pub use query::recent::{RecentEntry, RecentField, RecentOptions, RecentSince, find_recent};
pub use query::search::{SearchComponents, SearchEntry, search};
pub use query::similar::{
    SimilarityComponents, SimilarityEntry, SimilarityOptions, SimilarityTarget, compute_similarity,
};
pub use query::structure::{
    Component, Neighborhood, NeighborhoodEntry, find_components, find_neighborhood,
};
pub use query::traverse::{
    BacklinkEntry, ChainEntry, CoveredByEntry, IncomingEdgeRef, NodeEntry, OutgoingEdgeRef,
    find_backlinks, find_chain, find_covered_by, find_node_entry,
};
pub use query::trust::{
    TrustComponents, TrustEntry, TrustExtreme, TrustListOptions, compute_trust,
    compute_trust_ranking,
};
pub use query::{NodeRef, RankingOutcome};
pub use rules::{
    CheckReport, DriftHotspot, Rule, RuleContext, Severity, SkippedRule, ValueKind, Violation,
    ViolationDetails, check, preflight,
};
pub use scaffold::{ScaffoldResult, ScaffoldSpec, scaffold};
pub use status::{
    DivergenceProbe, GraphState, SnapshotDivergence, StatusReport, compute_divergence,
    compute_status, load_graph,
};

use std::path::Path;

/// The `nodex-core` crate version, sourced from `Cargo.toml`. Public so
/// library consumers can compare it directly when they need a value
/// outside [`verify_version`]'s SemVer-requirement model.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Refuse to proceed unless the running binary version satisfies the
/// SemVer requirement (e.g. `"0.5"`, `">=0.5,<0.6"`). Surfaces a
/// typed [`Error::Config`] on a malformed requirement and a typed
/// [`Error::VersionMismatch`] on a real mismatch, so both the CLI and
/// downstream library callers can gate on the same `error.code()`
/// without each re-implementing semver parsing.
pub fn verify_version(requirement: &str) -> Result<()> {
    let req = semver::VersionReq::parse(requirement).map_err(|e| {
        crate::error::Error::Config(format!(
            "version requirement {requirement:?} is not valid SemVer: {e}"
        ))
    })?;
    let actual = semver::Version::parse(VERSION).expect("CARGO_PKG_VERSION is always valid SemVer");
    if !req.matches(&actual) {
        return Err(crate::error::Error::VersionMismatch {
            actual: VERSION,
            requirement: requirement.to_string(),
        });
    }
    Ok(())
}

/// Load the project's configuration and verify environment-dependent
/// rule prerequisites. The entry point for read-only commands —
/// `Config::load` alone leaves opt-in rules (e.g. git-backed) unchecked.
///
/// Binary compatibility (`meta.nodex_version`) is deliberately *not*
/// enforced here: reading a graph can never corrupt it, so a version
/// pin must not block inspection. Callers that surface results to a
/// user attach [`binary_compat_warning`] to the envelope; mutating
/// commands use [`load_project_for_mutation`] instead.
pub fn load_project(root: &Path) -> Result<Config> {
    let config = Config::load(root)?;
    preflight(&config, root)?;
    Ok(config)
}

/// Load the project's configuration for a command that *writes*
/// documents. Identical to [`load_project`] but additionally enforces
/// the `meta.nodex_version` pin via [`ensure_binary_compatible`]: an
/// incompatible binary could write frontmatter the project can't read
/// back, so mutation is refused with [`Error::VersionMismatch`].
pub fn load_project_for_mutation(root: &Path) -> Result<Config> {
    let config = load_project(root)?;
    ensure_binary_compatible(&config)?;
    Ok(config)
}

/// Enforce the `meta.nodex_version` pin, returning [`Error::VersionMismatch`]
/// when the running binary is outside it. The boundary a command crosses
/// the instant before it writes — so a dry-run / preview that loads via
/// [`load_project`] stays readable, and only the actual write is gated.
/// The pin string is already validated as a SemVer requirement by
/// [`Config::validate`].
pub fn ensure_binary_compatible(config: &Config) -> Result<()> {
    if let Some(req) = config.meta.nodex_version.as_deref() {
        verify_version(req)?;
    }
    Ok(())
}

/// Non-fatal advisory for read-only commands when the running binary
/// falls outside the project's `meta.nodex_version` pin. Returns `None`
/// when no pin is set or the binary satisfies it. Mutating commands turn
/// the same condition into a hard error via [`load_project_for_mutation`].
pub fn binary_compat_warning(config: &Config) -> Option<String> {
    let req_str = config.meta.nodex_version.as_deref()?;
    let req = semver::VersionReq::parse(req_str)
        .expect("meta.nodex_version is validated as a SemVer requirement by Config::validate");
    let actual = semver::Version::parse(VERSION).expect("CARGO_PKG_VERSION is always valid SemVer");
    if req.matches(&actual) {
        return None;
    }
    Some(format!(
        "binary version {VERSION} is outside the project's meta.nodex_version pin {req_str:?}; \
         read-only results may be inaccurate — install a matching nodex to be certain"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_version_accepts_wildcard() {
        // Wildcard always matches; sanity check that the verifier
        // isn't accidentally inverted.
        verify_version("*").expect("wildcard requirement");
    }

    #[test]
    fn verify_version_accepts_exact_match() {
        // The crate's own version must always satisfy itself —
        // protects against a CARGO_PKG_VERSION drift accidentally
        // breaking every `--check-version` gate.
        verify_version(VERSION).expect("self-match");
    }

    #[test]
    fn verify_version_rejects_unreachable_upper_bound() {
        let err = verify_version("<0.0.1").unwrap_err();
        matches!(err, Error::VersionMismatch { .. });
        assert_eq!(err.code(), "VERSION_MISMATCH");
    }

    #[test]
    fn verify_version_rejects_malformed_requirement() {
        let err = verify_version("not a semver requirement!").unwrap_err();
        matches!(err, Error::Config(_));
        assert_eq!(err.code(), "CONFIG_ERROR");
    }
}
