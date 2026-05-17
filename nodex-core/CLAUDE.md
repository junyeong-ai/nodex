# nodex-core

Library crate. All graph / config / rule logic lives here; the CLI is
a thin wrapper.

## Layering invariants

- The `nodex_core::*` facade re-exports (in `lib.rs`) are the canonical
  names. Use them in tests, other modules, and external embeds; reach
  into module paths only for items the facade intentionally does not
  surface
- `path_guard::write_atomic` is the only legitimate write primitive
  for project files. Every new mutation surface routes through it —
  no `std::fs::write` in mutation code paths
- Rules read only from `RuleContext { graph, config, root, since }`.
  External-tool probes (git, …) live in the rule module and are wired
  into `rules::preflight` — never invoked inside `Rule::check`
- `Config` is the single source of truth for vocabulary. Tool actions
  that write frontmatter consume merged views (`required_for`,
  `types_for`, `enums_for`, `cross_field_for`, `declared_fields_for`)
  — never reach into raw `schema.overrides`

## Naming conventions

`query/` functions use one of two intent-disclosing prefixes:

- `find_*` — graph traversal or filter (structural results, no ranking)
- `compute_*` — value computation (similarity, trust, diff)

Text-scored matching lives under `query/search.rs::search` — the module
name itself is the verb, so the inner function isn't redundantly
re-prefixed (mirrors `std::env::set_var`, not `env::env_set_var`).

Input specs for `find_*` / `compute_*` functions taking complex
arguments fall into two categories:

- `*Filter` — *predicate* spec, every field narrows the result set
  (`NodeFilter`). New listing primitives extend `query/listing.rs`
  with a typed filter rather than growing the function signature.
- `*Options` — *algorithm-config* spec, fields tune ranking /
  thresholds / limits rather than restrict the candidate set
  (`SimilarityOptions`, `RecencyOptions`).

The distinction matters for naming because a "filter with a limit"
is still a filter (the limit caps the result, doesn't change which
nodes match), whereas options carry both filtering and tuning knobs
together. Use `*Filter` when every field is a pure predicate;
`*Options` when the spec mixes predicates with ranking / threshold
parameters.

Rule types end with `Rule` (`UnknownFieldRule`, `FrontmatterImmutableRule`,
`BodyLineRule`, …). Result-shaped outputs follow `*Manifest`
(exports — `SchemaManifest`, `EnumsManifest`, `RulesManifest`,
`EnvelopeSchemaManifest`), `*Report` (aggregates — `IssueReport`,
`CheckReport`, `DependentsReport`, `TrustReport`), `*Result`
(mutation / command outcomes — `ScaffoldResult`, `LifecycleResult`,
`MigrateResult`, `RenameResult`, `InitResult`, `ReportResult`,
`CheckResult`, `BuildResult`),
`*Ref` (flat node/edge projections), `*Entry` / `*Group` (sub-elements
inside an items list).

Every `*Result` mutation type lives in `command_result.rs` (or its
command's own module, the `scaffold.rs` precedent) so
`export::per_command_schemas` derives the JSON Schema for each via
`schema_for!<T>` against the same Rust type the CLI actually emits —
hand-written schemas drift, derived schemas can't.

Built-in vocabulary lives in a single declared constant per concept:
`BUILTIN_FRONTMATTER_FIELDS` (frontmatter fields), `BUILTIN_EDGE_RELATIONS`
(edge relations). Adding a new built-in extends one constant — every
consumer that filters on the vocabulary reads from it.

## Data flow

`scan_scope` → `parse_document` (rayon parallel; produces
`(Node, Vec<RawEdge>, Vec<RawAnnotation>, Vec<RawBodyLineMatch>)`) →
`resolve_edges` → `validate_supersedes_dag` → `materialise_*`
(applies per-pattern / per-block `applies_to_kind` filter using the
resolved node kind) → `Graph::new` (immutable; stores nodes +
resolved edges + materialised annotations + body-line matches, with
adjacency + annotation-by-source + body-line-by-source / by-rule
indices rebuilt from the canonical vectors).

Body-text scanners share `parser::body::iter_body_lines(body)` so
fence-aware line iteration has exactly one implementation — every
body-derived primitive (annotations, body-line matches, future
extraction surfaces) consumes the same iterator instead of writing
its own ``` `-`/`~`-sniff.

All check-time rules are pure functions of `(graph, config)`. The
parser extracts body-derived data once at build time; the rule
consumes from the graph. No rule re-reads files at check time
(git-class rules talk to `.git`, which is *not* file content).

## Graph serialization

`Graph` has hand-written `Serialize` / `Deserialize`. Adjacency
indices are derived state — rebuilt inside `Deserialize` via
`Graph::new`. Bump `SCHEMA_VERSION` in `model/graph.rs` on any on-disk
shape change.

## Adding a validation rule

1. Create `XxxRule` in `rules/` implementing `Rule` (`id`, `severity`,
   `check`).
2. Register the rule in `rules::check_with_diff()`.
3. Read only from `RuleContext`. Never touch the filesystem outside
   the explicit `root` (used only by git-class rules).
4. Consume merged config views (`required_for`, `types_for`, …) —
   never `schema_override_for(kind).*` directly, or the rule will
   silently skip global `[schema]` declarations.
5. If the rule depends on external state: probe in the rule module +
   call from `rules::preflight`. Never check env inside `check`.
6. If the rule needs `ctx.since` (the diff context): override
   `is_applicable` to return `false` when `ctx.since.is_none()` and
   supply a one-line `skip_reason`. Silent non-fires are forbidden —
   see `.claude/rules/config-driven.md`.
7. Surface the rule in `export::export_rules` so the manifest
   (`nodex export rules`) advertises it — only when the rule is
   *active* under the current config, mirroring `is_applicable`'s
   self-report. Built-in rules carry `RuleSource::Builtin`; rules
   dynamically generated per config block (one per
   `[[rules.body_line]]`, future per-block families) carry
   `RuleSource::Config` so external consumers can tell which entries
   disappear when their config block is removed.
