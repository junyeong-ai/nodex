pub mod builder;
pub mod config;
pub mod diff;
pub mod error;
pub mod export;
pub mod lifecycle;
pub mod model;
pub mod output;
pub mod parser;
pub mod path_guard;
pub mod query;
pub mod rules;
pub mod scaffold;

pub(crate) mod hash;
pub(crate) mod yaml_text;

// ─── Facade ─────────────────────────────────────────────────────────
//
// Consumers should address the symbols below rather than the internal
// module paths, so the canonical surface stays stable when modules are
// reorganised. Less-common items remain reachable via their module
// path (e.g. `nodex_core::query::trust::compute_trust`).

pub use config::{BUILTIN_FRONTMATTER_FIELDS, Config, FrontmatterImmutableConfig, SchemaMode};
pub use diff::{EdgeRef, FieldChange, GraphDiff, StatusTransition, compute_diff};
pub use error::{Error, ParseError, Result};
pub use export::{EnumsManifest, SchemaManifest, StatusesManifest, export_enums, export_schema};
pub use lifecycle::{Action, check_supersede_safe, transition};
pub use model::{Edge, Graph, Kind, Node, RawEdge, ResolvedTarget, Status};
pub use query::issues::{
    IssueReport, IssueSummary, UnresolvedEdge, UnresolvedKind, collect_issues,
};
pub use query::structure::{
    Component, Neighborhood, NeighborhoodNode, find_components, find_neighborhood,
};
pub use rules::{
    CheckReport, Rule, RuleContext, Severity, SkippedRule, Violation, check_all, check_with_diff,
    preflight,
};
pub use scaffold::{ScaffoldResult, ScaffoldSpec, scaffold};

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

/// Load the project's configuration *and* verify any opt-in rule
/// prerequisites. The single entry point every command should use —
/// `Config::load` alone leaves environment-dependent rules unchecked.
pub fn load_project(root: &Path) -> Result<Config> {
    let config = Config::load(root)?;
    preflight(&config, root)?;
    Ok(config)
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
