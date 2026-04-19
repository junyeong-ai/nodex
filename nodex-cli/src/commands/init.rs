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
# [[schema.overrides]]
# kinds = ["adr"]
# required = ["id", "title", "kind", "status", "decision_date"]
# types = { decision_date = "date" }
# enums = { status = ["draft", "active", "superseded", "deprecated"] }

[detection]
stale_days = 180
orphan_grace_days = 14

[output]
dir = "_index"

[report]
title = "Document Graph"
god_node_display_limit = 10
orphan_display_limit = 20
stale_display_limit = 20
"#;

pub fn run(root: &Path, pretty: bool) -> Result<()> {
    let config_path = root.join("nodex.toml");
    if config_path.exists() {
        return Err(CoreError::AlreadyExists { path: config_path }.into());
    }

    std::fs::write(&config_path, DEFAULT_CONFIG).map_err(|source| CoreError::Io {
        path: config_path.clone(),
        source,
    })?;

    #[derive(serde::Serialize)]
    struct InitResult {
        path: String,
    }

    print_json(
        &Envelope::success(InitResult {
            path: config_path.to_string_lossy().to_string(),
        }),
        pretty,
    );

    Ok(())
}
