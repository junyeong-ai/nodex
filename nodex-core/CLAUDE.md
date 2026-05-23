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
  rules whose semantic requires `--since` self-report as
  non-applicable via `is_applicable`; the runner records the refusal
  in `skipped_rules` — silent non-fires are forbidden
- `Config` is the single source of truth for vocabulary. Tool actions
  that write frontmatter consume merged views (`required_for`,
  `types_for`, `enums_for`, `cross_field_for`, `declared_fields_for`,
  `trust_weights_for`) — never raw `schema.overrides` or
  `trust.overrides`

## Naming conventions

`query/` functions carry one of two intent-disclosing prefixes:

- `find_*` — graph traversal or filter (structural results, no
  ranking)
- `compute_*` — value computation (similarity, trust, diff)

Text-scored matching lives at `query/search.rs::search` — the module
name is the verb, so the function isn't redundantly re-prefixed.

Input specs for `find_*` / `compute_*`:

- `*Filter` — pure predicate (every field narrows). Listing
  primitives extend `query/listing.rs` with a typed filter rather
  than growing the signature.
- `*Options` — ranking / threshold / limit knobs. Use when the spec
  mixes predicates with tuning.

Rule types end with `Rule`. Per-block config-driven rules carry
`RuleSource::Config` and a `<family>/<name>` qualified id; built-ins
keep the family name verbatim (`required_field`, `stale_review`,
`filename_pattern`). Output types: `*Manifest` (exports), `*Report`
(aggregates), `*Result` (mutation outcomes — every `*Result` lives in
`command_result.rs` or its command's own module so
`export::per_command_schemas` derives JSON Schema from the same Rust
type the CLI emits), `*Ref` (flat projections), `*Entry` / `*Group`
(items-list sub-elements).

Built-in vocabulary lives in a single declared constant per concept:
`BUILTIN_FRONTMATTER_FIELDS` (superset), `BUILTIN_SCALAR_FIELDS`,
`BUILTIN_COLLECTION_FIELDS`, `BUILTIN_EDGE_RELATIONS`. Adding a new
built-in extends the relevant constant — every consumer reads from it.

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
