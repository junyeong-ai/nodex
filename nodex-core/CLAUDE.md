# nodex-core

Library crate. All logic lives here — CLI is a thin wrapper.

## Module Map

- `model/` — data types: `Node`, `Edge`, `Graph`, `Kind`, `Status`, `Confidence`, `ResolvedTarget`
- `parser/` — `frontmatter.rs` (YAML), `body.rs` (pulldown-cmark links + custom patterns), `identity.rs` (config-based kind/id inference)
- `builder/` — `scanner.rs` (scope glob walk + conditional_exclude), `resolver.rs` (path→node_id), `validator.rs` (DAG cycle detection), `cache.rs` (SHA256 incremental), `mod.rs` (build orchestration)
- `query/` — `search.rs` (keyword/tag), `traverse.rs` (backlinks/chain/node detail), `detect.rs` (orphans/stale)
- `rules/` — `Rule` trait + built-in: `schema.rs` (required fields), `integrity.rs` (superseded_by), `freshness.rs` (stale review), `naming.rs` (filename patterns, sequential/unique numbering)
- `output/` — `json.rs` (graph.json + backlinks.json), `markdown.rs` (deterministic GRAPH.md)
- `lifecycle.rs` — state transitions (supersede/archive/deprecate/abandon/review), modifies frontmatter YAML in-place
- `config.rs` — `nodex.toml` deserialization, all config structs with serde defaults
- `error.rs` — `Error` enum with thiserror, `Result<T>` type alias

## Data Flow

`scan_scope()` → `parse_document()` [rayon parallel] → `resolve_edges()` → `validate_supersedes_dag()` → `Graph::new()` (immutable)

## Graph Serialization

`Graph` has custom `Serialize`/`Deserialize`. Adjacency indices (`incoming`/`outgoing`) are `#[serde(skip)]` — rebuilt automatically in `Deserialize` impl via `Graph::new()`. No manual `rebuild_indices()` needed.

## Adding a Validation Rule

1. Create struct in `rules/` implementing `Rule` trait (`id()`, `severity()`, `check()`)
2. Register in `rules::check_all()` vec
3. Rule reads from `Graph` + `Config` — no file I/O
