use anyhow::Result;
use std::path::Path;

use nodex_core::error::Error as CoreError;

use crate::format::{Envelope, print_json};

const DEFAULT_CONFIG: &str = r#"[scope]
include = ["**/*.md"]
exclude = []

[kinds]
allowed = ["generic", "guide", "readme"]

[statuses]
allowed = ["active", "superseded", "archived", "deprecated", "abandoned"]
terminal = ["superseded", "archived", "deprecated", "abandoned"]

# Kind inference rules (first match wins)
# [[identity.kind_rules]]
# glob = "docs/decisions/**"
# kind = "adr"

# ID template rules
[[identity.id_rules]]
kind = "*"
template = "{kind}-{stem}"

[schema]
required = ["id", "title", "kind", "status"]
# Global cross-field constraint: every superseded document must declare
# its successor. Integrity rules live in config now, so projects can
# see — and override — exactly what is enforced.
cross_field = [
  { when = "status=superseded", require = "superseded_by" },
]

# Per-kind schema enforcement. Overrides merge on top of the globals
# above (required / types / enums / cross_field). Each sub-block is
# opt-in; omit what you don't need.
#
# Override enum values must be a subset of the global allowed lists
# (`kinds.allowed` / `statuses.allowed`). Any `enums.status`
# declaration — global or override — must also cover the four
# lifecycle targets (`superseded`, `archived`, `deprecated`,
# `abandoned`) so `nodex lifecycle` never writes an invalid value.
# `Config::load` rejects both mismatches at startup.
#
# [[schema.overrides]]
# kinds = ["adr"]
# required = ["id", "title", "kind", "status", "decision_date"]
# types = { decision_date = "date" }
# enums = { priority = ["low", "medium", "high"] }

[detection]
stale_days = 180
orphan_grace_days = 14
# Kinds whose nodes are leaf-by-design and never expected to have inbound
# edges (entry-point skills, package READMEs, runbooks). Listed kinds are
# skipped by orphan detection wholesale; the per-node `orphan_ok: true`
# escape hatch remains for one-off exceptions inside tracked kinds. Every
# entry must also appear in `kinds.allowed` — `Config::load` rejects typos.
# orphan_ok_kinds = ["readme"]

[output]
dir = "_index"

[report]
title = "Document Graph"
god_node_display_limit = 10
orphan_display_limit = 20
stale_display_limit = 20

# ─── AI Memory Layer (opt-in) ────────────────────────────────────────
# Uncomment the blocks below to enable session-log, trust scoring, and
# similarity detection. `nodex log`, `nodex continue`, `nodex trust`,
# `nodex similar`, and the matching MCP tools become functional.

# [session]
# # Append-only session log; documents grow by one event per call.
# # `log_kind` must be in `kinds.allowed`.
# log_kind = "session"
# session_dir = "_sessions"
# max_events_per_session = 200
# default_continue_days = 1

# [trust]
# # Composite reliability score weights. Each component is in [0, 1];
# # the composite is a weighted average normalised by the sum of
# # *active* weights — drift is dropped automatically when
# # `detection.git_drift_threshold` is unset.
# weights = { status = 0.4, freshness = 0.3, drift = 0.2, backlinks = 0.1 }

# [similarity]
# # Vector-free similarity scoring (token Jaccard + tag overlap +
# # kind/directory match + graph-neighbour overlap). Tune `threshold`
# # to your project's tolerance for false positives.
# threshold = 0.3
# weights = { title = 0.4, tags = 0.2, kind = 0.1, directory = 0.1, linked = 0.2 }
# # Drop these tokens when comparing titles. Tune for non-English projects.
# title_stop_words = ["the","a","an","and","or","of","to","for","in","on","with","is","are","be","by","as","at","from"]
"#;

pub fn run(root: &Path, pretty: bool) -> Result<()> {
    let config_path = root.join("nodex.toml");
    if config_path.exists() {
        return Err(CoreError::Exists(config_path).into());
    }

    nodex_core::path_guard::write_atomic(&config_path, DEFAULT_CONFIG)?;

    #[derive(serde::Serialize)]
    struct InitResult {
        path: String,
    }

    print_json(
        &Envelope::success(InitResult {
            path: nodex_core::path_guard::forward_string(&config_path),
        }),
        pretty,
    );

    Ok(())
}
