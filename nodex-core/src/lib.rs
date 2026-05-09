pub mod builder;
pub mod config;
pub mod error;
pub mod lifecycle;
pub mod model;
pub mod output;
pub mod parser;
pub mod path_guard;
pub mod query;
pub mod rules;
pub mod scaffold;
pub mod session;

/// SHA256 hex hashing shared between the build cache and the GRAPH.md
/// generation stamp. Internal — never exposed at the facade level.
pub(crate) mod hash;

/// Internal helpers — line-level YAML editing shared between the
/// scaffold and lifecycle writers. Not part of the public surface.
pub(crate) mod yaml_text;

// ─── Facade ─────────────────────────────────────────────────────────
//
// Consumers should address these top-level paths rather than the
// internal module layout, so the canonical surface stays stable when
// internal modules are reorganised. Module paths remain available for
// less-common items.

pub use config::Config;
pub use error::{Error, ParseError, Result};
pub use lifecycle::{Action, transition};
pub use model::{Edge, Graph, Kind, Node, RawEdge, ResolvedTarget, Status};
pub use rules::{Rule, RuleContext, Severity, Violation, check_all, preflight};
pub use scaffold::{ScaffoldResult, ScaffoldSpec, scaffold};
pub use session::{
    Continuation, ContinueOptions, LogEventOutcome, LogEventResult, LogEventSpec,
    continue_from_last_session, log_event,
};

use std::path::Path;

/// Load the project's configuration *and* verify any opt-in rule
/// prerequisites. The single entry point every command should use —
/// `Config::load` alone leaves environment-dependent rules unchecked.
pub fn load_project(root: &Path) -> Result<Config> {
    let config = Config::load(root)?;
    preflight(&config, root)?;
    Ok(config)
}
