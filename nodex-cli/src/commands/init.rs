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
# nodex_version = "__VERSION_PIN__"

[scope]
include = ["**/*.md"]
exclude = []
# Dot-prefixed files and directories (`.draft.md`, `.archive/`,
# `.claude/`, …) are skipped by default — same convention as
# `ripgrep` / `fd` / `git`. An include pattern that literally names a
# dotted segment opts that hidden path in (e.g. `.claude/**/*.md`).
# An entry matching nothing is reported, since a typo'd path would
# otherwise validate green over a corpus nobody read. Where an area is
# empty on purpose — a specs directory between milestones — say so at
# the declaration and the report goes quiet for that one only:
# include = ["docs/**/*.md", { glob = "specs/**/*.md", may_be_empty = true }]
# The same field is accepted on [[identity.kind_rules]] and
# [[identity.id_rules]] entries.
# Directory basenames pruned from the walk at any depth (default shown).
# Tune for your stack; an empty list prunes nothing.
# prune_dirs = ["node_modules", "__pycache__", "target", ".git", ".venv"]
# A directory reached through a symlink is not descended (default),
# which keeps the path space a tree so every path-keyed rule has one
# path per document; each undescended link is named in the build's
# `unfollowed_paths`. Turn it on for documents that live behind a link
# — every extra name a document is then reachable under is reported in
# `aliased_paths` beside the one in use.
# follow_symlinks = false
# Drop the sub-artifacts a terminal parent governs: `parent_glob`
# selects the parent, `child_glob` which paths beside it are
# derivative. The unit is the parent's directory subtree, so give each
# record its own directory when you need per-record eviction. Dropped
# paths are reported on the build result, and the write that makes the
# parent terminal names them as `document_evicted`.
# [[scope.conditional_exclude]]
# parent_glob = "specs/**/SPEC.md"
# child_glob = "specs/**/tasks/**"
# condition = "status_terminal"

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
# Fields every document must author. id / title / kind / status / orphan_ok
# are resolved by the parser for every document (identity rules, H1 fallback,
# statuses.initial) — listing them here is rejected at load.
# required = ["created", "owner"]
#
# `mode = "lenient"` (default) lets undeclared frontmatter keys land in
# `attrs`. Switch to `"strict"` to surface typos like `relatd:` as
# `unknown_field` violations — every key must be built-in or declared
# in `types` / `enums` / `required` / `cross_field` (global + override).
# mode = "strict"
#
# The inferrable built-ins a document must author rather than inherit
# from a fallback. `required` cannot ask for these — the parser always
# resolves them — so this is how a project says the resolved value is
# not good enough; an inferred one reds `check` via `explicit_field`.
# `orphan_ok` is refused: a bool is structurally always present.
# require_explicit = ["id", "kind"]
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
# above: `required` is unioned with the global list (an override adds
# per-kind fields, never drops a global one), and `types` / `enums` /
# `cross_field` accumulate the same way. Each sub-block is opt-in; omit
# what you don't need.
#
# Override enum values must be a subset of the global allowed lists
# (`kinds.allowed` / `statuses.allowed`); `Config::load` rejects
# mismatches at startup. Lifecycle targets are validated at the write
# seam instead — `lifecycle set` / `supersede` refuse a status the
# document's kind doesn't allow, so a project only declares the
# statuses it actually uses.
#
# [[schema.overrides]]
# kinds = ["adr"]
# required = ["decision_date"]   # added on top of the global required set
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
# fields = ["kind", "superseded_by"]
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

# [parser]
# # Which link targets count as documents. Entries carry the leading dot.
# extensions = [".md"]
# # `[[wikilink]]` body syntax, off by default. With it on, a
# # `[[...]]`-shaped annotation marker is parsed as a wikilink too and
# # surfaces as an unresolved edge — use a non-bracket marker syntax if
# # you want annotations only.
# wikilink_enabled = false
#
# # Extraction for a corpus that cites in its own syntax. Exactly one
# # capture group, plus the relation the match becomes. The built-ins
# # whose resolution is fixed in code are refused here — path-only
# # `covers` and id-only `supersedes` / `implements` / `related`;
# # `references` is legal. `code_spans` additionally reads an inline
# # code span whose *entire* content the pattern matches, so a corpus
# # writing ids as `adr-001` is reachable as edges and `retarget`
# # repoints them.
# [[parser.link_patterns]]
# pattern = '''@ref:([\w-]+)'''
# relation = "references"
# code_spans = false

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
# Which relations carry the measurement (default shown).
# git_drift_relations = ["references", "implements", "covers"]

# Unresolved-reference policy — ordered, first match wins. Each row
# maps a typed cause (id_not_found | missing | target_unparsed |
# excluded_from_scope | escapes_source | absolute) to a severity:
# "error" registers the check rule `unresolved_reference/<name>`
# (matching edges fail `nodex check`), "warning" counts under
# `unresolved_edge` in `query issues` (also the fallthrough for
# unmatched edges), "info" reports the edge out of `total` under the
# row's name. `glob` is legal on the causes that carry a path
# (missing | target_unparsed | excluded_from_scope) and refused at
# load on the rest; it matches the link's normalized root-relative
# resolution candidates — `../docs/x.md` written from `designs/a.md`
# matches `docs/**`. Declaring the table replaces the default row
# below; re-declare it to keep it.
#
# [[detection.unresolved_policy]]
# name = "excluded_target"
# cause = "excluded_from_scope"
# severity = "info"

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
# # Composite reliability score weights. Each component is in [0, 1].
# # A component the run could not measure comes in two kinds, and they
# # are scored differently. *Inapplicable* — nothing the document could
# # write would produce it: `drift` when `detection.git_drift_threshold`
# # is unset or the tree is not a repository, `freshness` when
# # `detection.stale_days` is unset, either one on a terminal document,
# # `backlinks` when no node in the graph is referenced. It is dropped
# # and the composite renormalises over the rest. *Undeclared* — the run
# # can measure the component and this document supplied no input for it
# # (a positively-weighted `freshness` with no `reviewed` date). There is
# # no composite: `score` is omitted and the component is named in
# # `undeclared`, because renormalising would hand it the score its
# # other components earned and pay it for declaring nothing. Weight a
# # component zero to say the project does not track that axis.
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
# # kind/directory match + graph-neighbour overlap). A component is
# # present when the *target* carries the signal to rank by, and absent
# # for every candidate alike when it does not — an untagged target
# # asks nothing about tags, while a tagged one scores an untagged
# # candidate 0.0 rather than excusing it. The composite renormalises
# # over what the target carries, never over what a candidate lacks.
# # Default item count for `query similar` when `--limit` is omitted.
# default_limit = 10
# weights = { title = 0.4, tags = 0.2, kind = 0.1, directory = 0.1, linked = 0.2 }
# # Drop these tokens when comparing titles. Tune for non-English projects.
# title_stop_words = ["the","a","an","and","or","of","to","for","in","on","with","is","are","be","by","as","at","from"]

# [search]
# # Keyword ranking for `nodex query search`. A node's score is the sum
# # of the fields it matched — additive rather than renormalised, so a
# # node matching on both id and title outranks one matching on either.
# # `id` and `title` each carry an exact and a partial (substring) tier,
# # which puts the exact-over-partial preference in config instead of a
# # constant. Every entry reports the per-field breakdown that produced
# # its score.
# weights = { id_exact = 3.0, id_partial = 1.5, title_exact = 2.5, title_partial = 1.0, tag = 0.5 }
"#;

/// The SemVer pin example for the running binary: same-minor for 0.x
/// (where minor bumps are breaking) and same-major from 1.0 on. Derived
/// from the binary version so the generated template can never point at
/// a stale release.
fn compatible_version_pin(version: &str) -> String {
    let mut parts = version.split('.');
    let major: u64 = parts
        .next()
        .and_then(|p| p.parse().ok())
        .expect("CARGO_PKG_VERSION has a numeric major");
    let minor: u64 = parts
        .next()
        .and_then(|p| p.parse().ok())
        .expect("CARGO_PKG_VERSION has a numeric minor");
    if major == 0 {
        format!(">={major}.{minor}, <{major}.{next}", next = minor + 1)
    } else {
        format!(">={major}.{minor}, <{next}", next = major + 1)
    }
}

pub fn run(root: &Path, pretty: bool) -> Result<()> {
    let config_path = root.join("nodex.toml");
    if config_path.exists() {
        return Err(CoreError::Exists(config_path).into());
    }

    let config = DEFAULT_CONFIG.replace(
        "__VERSION_PIN__",
        &compatible_version_pin(env!("CARGO_PKG_VERSION")),
    );
    nodex_core::path_guard::write_atomic_in_root(root, &config_path, &config)?;

    print_json(
        &Envelope::success(InitResult {
            path: nodex_core::path_guard::forward_string(&config_path),
        }),
        pretty,
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_pin_tracks_the_running_binary() {
        assert_eq!(compatible_version_pin("0.15.0"), ">=0.15, <0.16");
        assert_eq!(compatible_version_pin("1.2.3"), ">=1.2, <2");
        // The shipped template always carries the live binary's pin —
        // never a hardcoded release that can go stale.
        assert!(!DEFAULT_CONFIG.contains(">=0."));
        assert!(DEFAULT_CONFIG.contains("__VERSION_PIN__"));
    }
}
