# nodex-core

Library crate. All graph / config / rule logic lives here; the CLI is
a thin wrapper.

## Layering invariants

- The `nodex_core::*` facade re-exports (`lib.rs`) are the canonical
  names. Use them in tests and external embeds; reach into module
  paths only for items the facade intentionally does not surface
- `path_guard::write_atomic_in_root` is the only legitimate write
  primitive for user-addressed mutation targets (scaffold, lifecycle,
  migrate, rename, retarget) — it enforces root containment
  (`reject_outside_root`, symlinked-ancestor aware) before the atomic
  write, so no handler can forget the guard. Infra writers whose
  target derives from load-validated config (`output.dir`, build
  cache, init) use `path_guard::write_atomic` directly. No
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
- Initial status is `statuses.initial` when declared, else the first
  `statuses.allowed` value. Kind-independent — a `status` enum is a *set*,
  not a lifecycle ordering, so its order never decides the default.
- Used by: `scaffold`, `migrate`, and frontmatter-less parses (when no
  status is present)
- Consequence: Tool-written docs are always valid (self-consistency
  invariant). `Config::validate` rejects a config whose implicit default
  (`statuses.allowed.first()`) is excluded by any declared `status` enum —
  it demands an explicit `statuses.initial` instead of silently reading a
  default out of enum order.
- Override: Declare `statuses.initial` explicitly

### orphan grace period (time-based exemption)
- New documents (created < `orphan_grace_days` ago) are exempt from orphan
  detection — new docs often aren't linked yet, so the window allows a
  creation → linking workflow. `orphan_grace_days` is a `u32` (not Option):
  `0` is valid and means "no grace, check immediately".
- A document is orphan-exempt if ANY of three independent mechanisms apply:
  `orphan_ok_kinds` (kind is leaf-by-design), per-node `orphan_ok: true` (this
  doc is intentionally orphaned), or the grace period above.

## Naming conventions

`query/` functions carry one of two intent-disclosing prefixes:

- `find_*` — graph traversal or filter (structural results, no
  ranking)
- `compute_*` — value computation (similarity, trust, diff)

Text-scored matching lives at `query/search.rs::search` — the module
name is the verb, so the function isn't redundantly re-prefixed.

Input specs for `find_*` / `compute_*`:

- `*Filter` — pure predicate (every field narrows), no tuning knobs
  (e.g. `NodeFilter`). Presentation capping (`--limit` on plain
  listings) is not a query knob: core returns complete deterministic
  results and the CLI's `ItemsEnvelope::capped` truncates, reporting
  `total` (matching) vs `returned` (shipped).
- `*Options` — ranking / threshold / selection knobs, optionally mixed
  with predicates. A spec whose `limit` is *selection semantics*
  (top-K / window — e.g. `RecentOptions`, `SimilarityOptions`,
  `TrustListOptions`) is `*Options`, not `*Filter`, even when most of
  its fields narrow.

Rule types end with `Rule`. Per-block config-driven rules carry
`RuleSource::Config` and a `<family>/<name>` qualified id; built-ins
keep the family name verbatim (`required_field`, `stale_review`,
`filename_pattern`). Output types: `*Manifest` (exports), `*Report`
(aggregates), `*Result` (mutation outcomes — every `*Result` lives in
`command_result.rs` or its command's own module so
`export::per_command_schemas` derives JSON Schema from the same Rust
type the CLI emits), `*Ref` (flat projections), `*Entry` / `*Group`
(items-list sub-elements).

One stem per domain concept: a concept uses a single noun across its
item / components / options / function (`Trust*` everywhere →
`TrustEntry`, `TrustComponents`, `TrustListOptions`, `compute_trust`;
`Similarity*` likewise → `SimilarityEntry`, `SimilarityComponents`,
`SimilarityOptions`). The CLI `*Args` stem matches its core stem
(`SimilarityArgs` ↔ `SimilarityOptions`).


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
  (identity, statuses, parser, annotations, body_line). Parsing takes
  `&ParseConfig`, so a new parse-affecting option cannot be added without
  surfacing there — the compiler enforces it. `schema` is deliberately
  excluded: it steers check-time validation only, never a cached parse.
- `ParseConfig::cache_key()` is the build cache key: a SHA-256 over that
  serialised surface plus `CARGO_PKG_VERSION`. Consequences:
  - rule / annotation / id_rules reordering invalidates the cache (order
    is semantic — first match wins, index-based lookup)
  - whitespace / comment edits never invalidate (the hash is over parsed
    structs, not TOML text)
  - parse-irrelevant config (`schema`, `trust`, `similarity`, `detection`,
    `scope`, `kinds`, naming rules) never invalidates — tuning a weight or
    a validation enum must not force a full reparse
  - a binary upgrade invalidates once, guarding serialised `Node` /
    `RawEdge` struct-shape drift

## Cycle detection

- `rules/graph_invariants.rs::CycleDetectionRule` detects cycles in the
  resolved edge graph for every `rules.acyclic_relations` relation
  (default `["implements"]`; entries validated against
  `known_relations()` at load, empty list rejected). `supersedes` is
  validated separately (and harder — a build-time `Error`) by
  `builder::validate_supersedes_dag`; `covers` names out-of-graph code
  paths and cannot cycle through documents.
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
- Reference handling has exactly one extraction and one resolution,
  shared by every consumer, so build and mutation can never disagree:
  - Extraction: `parser::body` finds references once — markdown link
    destinations (incl. reference-style/collapsed/shortcut definitions)
    via the pulldown-cmark token stream, plus `[[wikilink]]` / custom
    `parser.link_patterns` captures, code-span and frontmatter aware.
    The builder (`extract_links`) and the rename/retarget rewriter
    (`reference_rewrite`) consume the same helpers — never re-scan with
    a private regex.
  - Resolution: `builder::resolver::reference_path_candidates` is the
    single ladder — literal/relative path, then path + each
    `parser.extensions` suffix, then a bare node id for document
    references (`covers` stays path-only). The build resolver, the
    query-time unresolved-edge classifier
    (`query::issues::target_exists_on_disk`), and the rewriter all
    consume it, resolving an identical binding.
- `reference_rewrite` rewrites a reference only when it resolves to the
  renamed/retargeted target under that shared ladder (against the
  pre-move scope), so the rewriter touches exactly the references the
  builder bound as edges — no more, no less.

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

All in `query/`, pure (read graph + config, no side effects). The
facade (`lib.rs`) re-exports the stable set; signatures live in
rustdoc. Names follow the `find_*` / `compute_*` / `search` split under
Naming conventions, so the prefix discloses whether a query ranks.
