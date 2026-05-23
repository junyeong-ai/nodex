use anyhow::Result;
use std::path::Path;

use nodex_core::command_result::InitResult;
use nodex_core::error::Error as CoreError;

use crate::format::{Envelope, print_json};

const DEFAULT_CONFIG: &str = r#"# [meta]
# # Binary-compatibility pin: nodex refuses to load this config unless
# # the running binary's version satisfies the SemVer requirement.
# # Recommended once the project's tooling is on a stable minor —
# # contributors and CI catch a mismatched binary at load time instead
# # of seeing a baffling rule-fired-without-config behaviour later.
# nodex_version = ">=0.11, <0.12"

[scope]
include = ["**/*.md"]
exclude = []
# Dot-prefixed files and directories (`.draft.md`, `.archive/`,
# `.claude/`, …) are skipped by default — same convention as
# `ripgrep` / `ag`. Flip to `true` if your project keeps real
# documentation under a hidden path.
# include_hidden = false

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
# `mode = "lenient"` (default) lets undeclared frontmatter keys land in
# `attrs`. Switch to `"strict"` to surface typos like `relatd:` as
# `unknown_field` violations — every key must be built-in or declared
# in `types` / `enums` / `required` / `cross_field` (global + override).
# mode = "strict"
#
# Global cross-field constraint: every superseded document must declare
# its successor. Integrity rules live in config now, so projects can
# see — and override — exactly what is enforced.
cross_field = [
  { when = "status=superseded", require = "superseded_by" },
  # { when = "status in {superseded,archived}", require = "superseded_by" },
  # { when = "owner exists", require = "reviewed" },
  # { when = "reviewed not_exists", require = "owner" },
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

[rules]
# Filename / numbering checks (opt-in per glob).
# [[rules.naming]]
# glob = "docs/decisions/**"
# pattern = "^\\d{4}-[a-z0-9-]+\\.md$"
# sequential = true
# unique = true

# Diff-aware frontmatter lock — one block per locking policy so a
# project can keep identity fields universally frozen while locking
# additional decision metadata only for ADR-kind docs at `archived`.
# Activates only at terminal status; requires `--since`. Violations
# carry `rule_id = "frontmatter_immutable/<name>"`; `Config::load`
# rejects duplicate names so violation ids stay distinguishable.
#
# [[rules.frontmatter_immutable]]
# name = "identity"
# fields = ["id", "kind", "superseded_by"]
#
# [[rules.frontmatter_immutable]]
# name = "adr-decision-date"
# fields = ["decision_date"]
# kinds = ["adr"]

# Diff-aware body lock — one block per locking policy so a project
# can freeze some kinds outright while permitting append-only growth
# on others. Activates only at terminal status; requires `--since`.
# `mode = "frozen"` rejects any body edit; `mode = "append_only"`
# requires the pre-terminal body to remain a prefix of the new body
# (suits log-shaped documents). Violations carry
# `rule_id = "body_immutable/<name>"`; `Config::load` rejects
# duplicate names so violation ids stay distinguishable.
#
# [[rules.body_immutable]]
# name = "adr-decisions"
# mode = "frozen"
# kinds = ["adr"]
#
# [[rules.body_immutable]]
# name = "runbook-history"
# mode = "append_only"
# kinds = ["runbook"]

# Per-line body-text vocabulary conformance — one block per pattern.
# Captures named in `enums` must hold a value from the declared
# allowed set; non-matching lines are silently ignored (this is a
# *conformance* rule, not a *presence* rule). Violations carry
# `rule_id = "body_line/<name>"`. Names must be unique; `Config::load`
# rejects duplicates so violation ids stay distinguishable.
#
# [[rules.body_line]]
# name = "decision-log"
# pattern = '''^- \*\*(?P<gate>[a-z-]+)\*\*'''
# enums.gate = ["scope", "design", "rollout", "ship"]
# # `kinds` narrows which docs the rule scans. Empty = every kind;
# # every listed kind must be in `kinds.allowed`.
# # kinds = ["guide"]

# Body-text marker extraction — surfaced by `nodex query annotations`.
# Pre-graph identifiers (TODO topics, promotion candidates, open
# research questions) that intentionally do not resolve to a node —
# use `[[parser.link_patterns]]` for markers that *should* resolve to
# graph edges. `Config::load` requires `key` to be one of the
# pattern's named captures and `kinds` entries to be in
# `kinds.allowed`.
#
# [[annotations]]
# name = "promotes"
# pattern = '''\[PROMOTES:\s*(?P<id>[\w-]+)\]'''
# key = "id"
# # kinds = ["guide"]

[detection]
stale_days = 180
orphan_grace_days = 14
# Kinds whose nodes are leaf-by-design and never expected to have inbound
# edges (entry-point skills, package READMEs, runbooks). Listed kinds are
# skipped by orphan detection wholesale; the per-node `orphan_ok: true`
# escape hatch remains for one-off exceptions inside tracked kinds. Every
# entry must also appear in `kinds.allowed` — `Config::load` rejects typos.
# orphan_ok_kinds = ["readme"]

# Git-aware drift signal (opt-in). When set, `query trust` and `check`
# look at how many commits touched a node's referenced files since its
# `reviewed` date and surface high counts as low trust / warnings.
# Requires `git` on PATH and a git work tree at the project root —
# `Config::load` rejects this block when both relations and threshold
# are misaligned.
# git_drift_threshold = 5
# git_drift_relations = ["references"]

[output]
dir = "_index"

[report]
title = "Document Graph"
god_node_display_limit = 10
orphan_display_limit = 20
stale_display_limit = 20

# Trust and similarity scoring (opt-in — defaults apply if these
# blocks are omitted). Both surfaces are pure-CLI; no external
# services. Tune the weights / thresholds to your corpus.

# [trust]
# # Composite reliability score weights. Each component is in [0, 1];
# # the composite is a weighted average normalised by the sum of
# # *active* weights — `drift` is dropped automatically when
# # `detection.git_drift_threshold` is unset.
# weights = { status = 0.4, freshness = 0.3, drift = 0.2, backlinks = 0.1 }
# # Cut-off used by `nodex query low-trust` when the caller does not
# # supply `--threshold`. Docs scoring at or below this surface as
# # candidates for review.
# low_trust_threshold = 0.5
#
# # Per-kind weight overrides — replace global weights entirely for
# # the listed kinds. Useful when e.g. ADRs care more about backlinks
# # than freshness.
# [[trust.overrides]]
# kinds = ["adr"]
# weights = { status = 0.2, freshness = 0.2, drift = 0.2, backlinks = 0.4 }

# [similarity]
# # Vector-free similarity scoring (token Jaccard + tag overlap +
# # kind/directory match + graph-neighbour overlap). Tune `threshold`
# # to your project's tolerance for false positives.
# threshold = 0.3
# # Default item count for `query similar` when `--limit` is omitted.
# default_limit = 10
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

    print_json(
        &Envelope::success(InitResult {
            path: nodex_core::path_guard::forward_string(&config_path),
        }),
        pretty,
    );

    Ok(())
}
