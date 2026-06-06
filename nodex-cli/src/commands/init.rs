use anyhow::Result;
use std::path::Path;

use nodex_core::command_result::InitResult;
use nodex_core::error::Error as CoreError;

use crate::format::{Envelope, print_json};

const DEFAULT_CONFIG: &str = r#"# [meta]
# # Binary-compatibility pin: when the running binary is outside the
# # SemVer requirement, read commands still run (attaching a warning)
# # while document-writing commands refuse with VERSION_MISMATCH.
# # Recommended once the project's tooling is on a stable minor —
# # contributors and CI catch a mismatched binary before it writes
# # frontmatter the project can't read back.
# nodex_version = ">=0.13, <0.14"

[scope]
include = ["**/*.md"]
exclude = []
# Dot-prefixed files and directories (`.draft.md`, `.archive/`,
# `.claude/`, …) are skipped by default — same convention as
# `ripgrep` / `fd` / `git`. An include pattern that literally names a
# dotted segment opts that hidden path in (e.g. `.claude/**/*.md`).

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
# Ref that `nodex check` diffs against when `--since` is omitted, so the
# diff-aware immutability rules below are enforced by default (against
# the last commit) instead of only when a ref is passed. Set to a branch
# (e.g. "origin/main") in CI to check a whole branch; clear it to make
# immutability opt-in per command.
immutable_baseline = "HEAD"

# Relations whose edge graph must stay acyclic (a cycle is reported at
# Error severity). Every entry must be a known relation — built-in or
# declared via [[parser.link_patterns]]; an empty list is rejected.
# acyclic_relations = ["implements"]

# Filename / numbering checks (opt-in per glob).
# [[rules.naming]]
# glob = "docs/decisions/**"
# pattern = "^\\d{4}-[a-z0-9-]+\\.md$"
# sequential = true
# unique = true

# Diff-aware frontmatter lock — one block per locking policy so a
# project can keep identity fields universally frozen while locking
# additional decision metadata only for ADR-kind docs at `archived`.
# Activates only at terminal status; enforced against `immutable_baseline`
# by default (or an explicit `--since`). Violations carry
# `rule_id = "frontmatter_immutable/<name>"`; `Config::load` rejects
# duplicate names so violation ids stay distinguishable.
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
# on others. Enforced against `immutable_baseline` by default (or an
# explicit `--since`). `mode = "frozen"` rejects any body edit;
# `mode = "append_only"` requires the locked body to remain a prefix
# of the new body (suits log-shaped documents). `trigger` picks when
# the lock engages: "terminal" (default) locks once status is
# terminal; "creation" locks as soon as a prior committed snapshot
# exists, regardless of status — the immutable-from-day-one contract
# for ADR-style records (frontmatter, including `status`, stays
# editable for supersession). Violations carry
# `rule_id = "body_immutable/<name>"`; `Config::load` rejects
# duplicate names so violation ids stay distinguishable.
#
# [[rules.body_immutable]]
# name = "adr-decisions"
# mode = "frozen"
# trigger = "creation"
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
# services.
#
# Note: score cutoffs are not part of these config blocks. The
# project-wide tuning knobs here are weights (composite-shape) and
# the default operator-capacity cap (`similarity.default_limit`).
# Threshold-style filters are opt-in at the call site:
#   `nodex query trust --bottom N --below S`
#   `nodex query similar --id <id> --min-score S`
# Corpus-dependent cutoffs are not stable across projects, so they
# stay at the CLI layer rather than baked into config defaults.

# [trust]
# # Composite reliability score weights. Each component is in [0, 1];
# # the composite is a weighted average normalised by the sum of
# # *active* weights — `freshness` is omitted when the node has no
# # `reviewed` date, `drift` is omitted when `detection.git_drift_threshold`
# # is unset, `backlinks` is omitted when the graph has no external
# # incoming edges on any node. Absent components are dropped from the
# # denominator rather than substituted with a neutral fallback — tune
# # weights on the signals your corpus actually carries.
# weights = { status = 0.4, freshness = 0.3, drift = 0.2, backlinks = 0.1 }
#
# # Per-kind weight overrides — replace global weights entirely for
# # the listed kinds. Useful when e.g. ADRs care more about backlinks
# # than freshness.
# [[trust.overrides]]
# kinds = ["adr"]
# weights = { status = 0.2, freshness = 0.2, drift = 0.2, backlinks = 0.4 }

# [similarity]
# # Vector-free similarity scoring (token Jaccard + tag overlap +
# # kind/directory match + graph-neighbour overlap). Every component
# # is conditional — each is omitted from the JSON when no signal
# # exists (empty title-token / tag sets, pre-creation spec without
# # explicit kind / parent_dir, no graph id for `linked`). The
# # composite renormalises over the present components.
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
