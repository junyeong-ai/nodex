# nodex-core

Library crate. All logic lives here — CLI is a thin wrapper.

## Module Map

- `model/` — data types: `Node`, `Edge`, `Graph`, `Kind`, `Status`, `Confidence`, `ResolvedTarget`
- `parser/` — `frontmatter.rs` (YAML), `body.rs` (pulldown-cmark links + custom patterns), `identity.rs` (config-based kind/id inference)
- `builder/` — `scanner.rs` (scope glob walk + conditional_exclude), `resolver.rs` (path→node_id), `validator.rs` (DAG cycle detection), `cache.rs` (SHA256 incremental), `mod.rs` (build orchestration)
- `query/` — `search.rs` (keyword/tag), `traverse.rs` (backlinks/chain/node detail), `detect.rs` (orphans/stale), `issues.rs` (unified issue report)
- `rules/` — `Rule` trait + built-in: `schema.rs` (required + type + enum + cross-field), `freshness.rs` (stale review), `naming.rs` (filename patterns, sequential/unique numbering)
- `output/` — `json.rs` (graph.json + backlinks.json), `markdown.rs` (deterministic GRAPH.md)
- `lifecycle.rs` — state transitions (supersede/archive/deprecate/abandon/review), modifies frontmatter YAML in-place
- `scaffold.rs` — create new documents with valid frontmatter
- `path_guard.rs` — reject `..`/absolute paths and detect symlinks at CLI boundaries
- `config.rs` — `nodex.toml` deserialization, `Config::load()` validates at startup
- `error.rs` — `Error` enum with thiserror, `Result<T>` type alias

## Data Flow

`scan_scope()` → `parse_document()` [rayon parallel] → `resolve_edges()` → `validate_supersedes_dag()` → `Graph::new()` (immutable)

## Graph Serialization

`Graph` has hand-written `Serialize`/`Deserialize` impls (no serde derive). Only `schema_version`, `nodes`, and `edges` cross the wire; adjacency indices are derived state and rebuilt from edges inside the `Deserialize` impl via `Graph::new()`. Bump `SCHEMA_VERSION` on any on-disk shape change.

## Adding a Validation Rule

1. Create struct in `rules/` implementing `Rule` trait (`id()`, `severity()`, `check()`)
2. Register in `rules::check_all()` vec
3. Rule reads from `Graph` + `Config` — no file I/O
