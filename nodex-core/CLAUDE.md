# nodex-core

Library crate. All graph / config / rule logic lives here; the CLI is
a thin wrapper.

## Layering invariants

- The `nodex_core::*` facade re-exports (`lib.rs`) are the canonical
  names. Use them in tests and external embeds; reach into module
  paths only for items the facade intentionally does not surface
- `path_guard::write_atomic` is the only legitimate write primitive
  for project files. Every mutation routes through it — no
  `std::fs::write` in mutation paths
- Rules read only from `RuleContext { graph, config, root, since }`.
  External-tool probes (git, …) live in the rule module and wire
  into `rules::preflight` — never inside `Rule::check`. Diff-aware
  rules whose semantic requires a diff context (`ctx.since`, supplied
  by `--since` or `rules.immutable_baseline`) self-report as
  non-applicable via `is_applicable`; the runner records the refusal
  in `skipped_rules` — silent non-fires are forbidden
- `Config` is the single source of truth for vocabulary. Tool actions
  that write frontmatter consume merged views (`required_for`,
  `types_for`, `enums_for`, `cross_field_for`, `declared_fields_for`,
  `trust_weights_for`) — never raw `schema.overrides` or
  `trust.overrides`

## Fallback mechanisms (intentional, not optional)

These are NOT hacks or convenience features — they are core invariants that ensure
every document always has valid kind, id, and status. Projects must declare
exhaustive rules to override them, but they exist to prevent incomplete configs
from breaking the graph.

### kind inference fallback
- `FALLBACK_KIND = "generic"` when no `identity.kind_rules` glob matches
- Consequence: `kinds.allowed` MUST include "generic" (enforced at load)
- Override: Declare `identity.kind_rules` covering 100% of paths

### id inference fallback
- Default ID = "{kind}-{stem}" when no `identity.id_rules` rule matches
- Consequence: Every document gets an ID
- Override: Declare `identity.id_rules` for all kinds

### status inference fallback
- Initial status determined by priority:
  1. `statuses.initial` if explicitly declared (explicit > implicit)
  2. First value in `schema.enums.status` (per-kind or global)
  3. First value in `statuses.allowed` (ultimate fallback)
- Used by: `scaffold`, `migrate` (when no --status provided)
- Consequence: Tool-written docs are always valid (self-consistency invariant)
- Override: Declare `statuses.initial` explicitly, or declare exhaustive `schema.enums.status`

### orphan grace period (time-based exemption)
- New documents (created < `orphan_grace_days` ago) are exempt from orphan detection
- Rationale: New docs often haven't been linked yet; grace period allows creation → linking workflow
- `orphan_grace_days` is a `u32` (not Option); zero is valid (no grace = immediate orphan check)
- Override: Set `orphan_grace_days = 0` for immediate orphan detection, OR use `orphan_ok_kinds` for kinds that are leaf-by-design (never orphan), OR use per-node `orphan_ok: true` for specific documents
- Three independent mechanisms work together:
  1. `orphan_ok_kinds` — kind is always orphan-ok (architecture, readme, etc.)
  2. Per-node `orphan_ok: true` — this document is intentionally orphaned
  3. Grace period — new documents get N days before orphan check
- A document is orphan-exempt if ANY of the above apply

## Naming conventions

`query/` functions carry one of two intent-disclosing prefixes:

- `find_*` — graph traversal or filter (structural results, no
  ranking)
- `compute_*` — value computation (similarity, trust, diff)

Text-scored matching lives at `query/search.rs::search` — the module
name is the verb, so the function isn't redundantly re-prefixed.

Input specs for `find_*` / `compute_*`:

- `*Filter` — pure predicate (every field narrows), no tuning knobs.
- `*Options` — ranking / threshold / limit knobs, optionally mixed with
  predicates. A spec that carries a `limit` (e.g. `NodeListOptions`,
  `RecentOptions`, `SimilarityOptions`, `TrustListOptions`) is `*Options`,
  not `*Filter`, even when most of its fields narrow.

Rule types end with `Rule`. Per-block config-driven rules carry
`RuleSource::Config` and a `<family>/<name>` qualified id; built-ins
keep the family name verbatim (`required_field`, `stale_review`,
`filename_pattern`). Output types: `*Manifest` (exports), `*Report`
(aggregates), `*Result` (mutation outcomes — every `*Result` lives in
`command_result.rs` or its command's own module so
`export::per_command_schemas` derives JSON Schema from the same Rust
type the CLI emits), `*Ref` (flat projections), `*Entry` / `*Group`
(items-list sub-elements).


## Detection thresholds (explicit semantics)

- `stale_days` / `git_drift_threshold`: `Option<u32>` — `None` disables;
  `Some(n)` flags at the threshold (reviewed n+ days ago / drift > n
  commits). `Some(0)` is rejected at load — ambiguous between "off" and
  "flag immediately".
- `orphan_grace_days`: plain `u32` (not `Option`) — a duration, so `0` is
  valid and meaningful (no grace = immediate orphan check). The differing
  type is deliberate: a threshold toggles, a duration is always active.

## Cache invalidation

- `parser::ParseConfig` is the exact slice of `Config` that parsing reads
  (identity, statuses, schema, parser, annotations, body_line). Parsing
  takes `&ParseConfig`, so a new parse-affecting option cannot be added
  without surfacing there — the compiler enforces it.
- `ParseConfig::cache_key()` is the build cache key: a SHA-256 over that
  serialised surface plus `CARGO_PKG_VERSION`. Consequences:
  - rule / annotation / id_rules reordering invalidates the cache (order
    is semantic — first match wins, index-based lookup)
  - whitespace / comment edits never invalidate (the hash is over parsed
    structs, not TOML text)
  - parse-irrelevant config (`trust`, `similarity`, `detection`, `scope`,
    `kinds`, naming rules) never invalidates — tuning a weight must not
    force a full reparse
  - a binary upgrade invalidates once, guarding serialised `Node` /
    `RawEdge` struct-shape drift

## Cycle detection

- `rules/graph_invariants.rs::CycleDetectionRule` detects cycles in frontmatter relations (`implements`, `covers`).
- DAG invariant failure → Error severity (must resolve).
- Reports exact cycle path for debugging.

## Data flow invariants

- Every parser entry routes file content through
  `parser::frontmatter::canonicalize` (BOM strip + `\r\n`/`\r` → `\n`)
  so fingerprints, regex matches, and line iteration agree across
  mixed-line-ending sources.
- Body-text scanners share `parser::body::iter_body_lines` so
  fence-aware iteration has one implementation.
- Per-block `kinds: Vec<String>` filters are evaluated via
  `Node::matches_kinds(&kinds)` (empty list = no restriction). Load-
  time enforced by `Config::validate_kinds`.
- All check-time rules are pure functions of `(graph, config)`. The
  parser extracts body-derived data once at build time; rules read
  from the graph. No rule re-reads files at check time.
- Link patterns must have exactly one capture group (the link target).
  Zero groups = no edges extracted. Multiple groups = only first used,
  causing silent misbehavior. Rejected at config load.
- Body-link resolution is a single ladder owned by
  `builder::resolver::reference_path_candidates` — literal/relative path,
  then path + each `parser.extensions` suffix, then a bare node id for
  document references (`covers` stays path-only). The build-time resolver
  and the query-time unresolved-edge `cause` classifier
  (`query::issues::target_exists_on_disk`) both consume it, so they can
  never disagree on what a reference points to.

## Graph serialization

`Graph` has hand-written `Serialize` / `Deserialize`. Adjacency
indices are derived state — rebuilt inside `Deserialize` via
`Graph::new`. Bump `SCHEMA_VERSION` in `model/graph.rs` on any
on-disk shape change.

## Adding a validation rule

1. Implement `Rule` (`id`, `severity`, `check`, `description`).
2. Register in `rules::registered_rules(config)` — single registry
   that both `rules::check` and `export::export_rules` read from.
3. Read only from `RuleContext`. Never touch the filesystem outside
   `root` (git-class rules only).
4. Consume merged config views (`required_for`, `types_for`, …) —
   never `schema_override_for(kind).*` directly.
5. Diff-aware rule: override `is_applicable` to return `false` when
   `ctx.since.is_none()` and supply a `skip_reason`. Silent
   non-fires are forbidden (see `.claude/rules/config-driven.md`).
6. Per-block kind filter: carry `kinds: Vec<String>` on the config
   struct, gate with `node.matches_kinds(&self.config.kinds)`, let
   `Config::validate_kinds` reject typos at load. Immutability
   families additionally route through `validate_immutable_blocks`
   for unique-name + field-universe gates.
7. Surface in `export::export_rules` — only when the rule is active
   under the current config, mirroring `is_applicable`. `Builtin`
   for code-shipped rules; `Config` for per-block instances so
   external consumers can tell which entries disappear when a
   config block is removed.

## Query API

All in `query/`, pure (read graph + config, no side effects). Signatures
live in rustdoc; the facade (`lib.rs`) re-exports the stable set. The
vocabulary:

- `find_*` — structural traversal / filter: `backlinks`, `node_entry`,
  `nodes`, `stale`, `orphans`, `dependents`, `covered_by`, `recent`,
  `issues` (aggregated orphans + stale + unresolved edges + check).
- `compute_*` — scored: `similarity` (text + metadata), `trust` and
  `trust_ranking` (status + freshness + drift + backlinks composite).
- `search::search` — text matching (module name is the verb).
