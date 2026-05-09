# nodex-core

Library crate. All logic lives here — CLI is a thin wrapper.

## Module Map

- `model/` — data types: `Node`, `Edge`, `Graph`, `Kind`, `Status`, `ResolvedTarget`. `Graph::require_node` is the canonical "exists or `MissingNode`" boundary
- `parser/` — `frontmatter.rs` (YAML + pub `extract_h1`), `body.rs` (pulldown-cmark links + wikilinks + custom patterns), `identity.rs` (config-based kind/id inference), `editor.rs` (minimal-diff `FrontmatterEditor` with `Scalar` typed lookups + `set_list` for YAML lists)
- `builder/` — `scanner.rs` (scope glob walk + conditional_exclude), `resolver.rs` (path→node_id with `..` underflow detection), `validator.rs` (DAG cycle detection), `cache.rs` (SHA256 incremental), `mod.rs` (build orchestration + edge dedup via typed `EdgeIdentity`)
- `query/` — `search.rs` (keyword/tag), `traverse.rs` (backlinks/chain/node detail/covered_by), `detect.rs` (orphans/stale), `issues.rs` (unified issue report), `pack.rs` (token-budgeted context bundle, BFS-priority via BinaryHeap), `recent.rs` (recency-based listing), `similar.rs` (vector-free similarity), `trust.rs` (composite reliability score). `NodeRef` (in `mod.rs`) is the common `{id,title,kind,status,path}` view flattened into every entry struct
- `rules/` — `Rule` trait + `RuleContext { graph, config, root }` + built-in: `schema.rs`, `freshness.rs`, `naming.rs`, `git_drift.rs`. `preflight()` validates env prerequisites of opt-in rules at command boundary
- `output/` — `json.rs` (graph.json — single source of truth), `markdown.rs` (deterministic GRAPH.md)
- `lifecycle.rs` — state transitions; `Action` is owned (no lifetime parameter) to keep MCP dispatch clean
- `scaffold.rs` — create new documents; `render_default_frontmatter` is the shared entry point for every tool action that writes frontmatter; warns about similar existing docs via `query::similar`
- `session.rs` — append-only event log (`log_event`) and continuity bootstrap (`continue_from_last_session`). Rollover via the existing supersession chain when `max_events_per_session` is reached
- `path_guard.rs` — reject `..`/absolute paths, detect symlinks, atomic `write_atomic` (the canonical write primitive every mutation surface routes through), plus `forward_string`/`forward_str` for cross-platform JSON output
- `yaml_text.rs` (pub(crate)) — line-level YAML scalar helpers shared between scaffold + lifecycle + session writers
- `hash.rs` (pub(crate)) — `sha256_hex` content fingerprint shared by the build cache (full hex, content-change detection) and the `GRAPH.md` generation stamp (truncated). Centralised so swapping algorithms is a single-file change
- `config.rs` — `nodex.toml` deserialization. `Config::load()` validates pure-data invariants; runtime env probes live in `rules::preflight`. The facade entry point is `nodex_core::load_project(root)` which combines both
- `error.rs` — `Error` enum + nested `ParseError`; `Error::code()` returns the stable string surface used by the JSON envelope

## Data Flow

`scan_scope()` → `parse_document()` [rayon parallel] → `resolve_edges()` → `validate_supersedes_dag()` → `Graph::new()` (immutable)

## Graph Serialization

`Graph` has hand-written `Serialize`/`Deserialize` impls (no serde derive). Only `schema_version`, `nodes`, and `edges` cross the wire; adjacency indices are derived state and rebuilt from edges inside the `Deserialize` impl via `Graph::new()`. Bump `SCHEMA_VERSION` on any on-disk shape change.

## Adding a Validation Rule

1. Create struct named `XxxRule` in `rules/` implementing `Rule` trait (`id()`, `severity()`, `check(&RuleContext)`). The `Rule` suffix is the project-wide convention — it disambiguates from same-named config types (e.g. `FieldType` enum vs `FieldTypeRule` struct) and makes every implementor greppable.
2. Register in `rules::check_all()` vec.
3. Rule reads from `RuleContext { graph, config, root }` — no file I/O outside the explicit `root` (used only by `git_drift`-class rules).
4. Consume merged views (`Config::required_for` / `types_for` / `enums_for` / `cross_field_for`) — never reach into `schema_override_for(kind).enums` directly, or the rule will silently skip global `[schema]` declarations.
5. If the rule depends on an external tool (git, …), implement the env probe in the rule module and call it from `rules::preflight`. Never check env at `Rule::check` time — the rule should assume preflight has already passed.
