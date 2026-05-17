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

`query/` functions use one of three intent-disclosing prefixes:

- `find_*` — graph traversal returning structural results
- `compute_*` — value computation (similarity, trust, diff)
- `search_*` — text-scored matching

Rule types end with `Rule` (`UnknownFieldRule`, `FrontmatterImmutableRule`, …).
Result-shaped outputs follow `*Manifest` (exports), `*Report`
(aggregates), `*Result` (mutation outcomes), `*Ref` (flat node/edge
projections).

## Data flow

`scan_scope` → `parse_document` (rayon parallel) → `resolve_edges` →
`validate_supersedes_dag` → `Graph::new` (immutable).

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
