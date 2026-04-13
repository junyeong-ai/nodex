# Nodex

Universal graph-based document tool. Parses markdown files with YAML frontmatter, builds an immutable document graph, and exposes queries via a JSON-first CLI.

## Build & Test

```bash
cargo build --release
cargo test
```

Binary: `target/release/nodex`

## Architecture

Cargo workspace with two crates:

- **nodex-core** — library: parsing, graph building, queries, validation, output
- **nodex-cli** — binary (`nodex`): clap CLI wrapping nodex-core

All project-specific behavior is driven by `nodex.toml` config. No domain logic is hardcoded in core.

## Key Design Decisions

- **Config-driven**: kind/status/rules/patterns all come from `nodex.toml`, never hardcoded
- **Immutable graph**: built from scratch each run via `Graph::new()`, no mutation after construction
- **JSON-first output**: every CLI command returns `{"ok": bool, "data": {...}}` envelope
- **pulldown-cmark for links**: AST-based markdown parsing, not regex — avoids code block false positives
- **rayon parallel parsing**: file I/O and parsing parallelized for large repos
- **SHA256 incremental cache**: `_index/cache.json` with config hash auto-invalidation
- **ResolvedTarget enum**: edges use `Resolved{id}` / `Unresolved{raw, reason}` — type-safe, no string-prefix hacking
- **Kind/Status newtypes**: `String` wrappers validated by config, not hardcoded enums

## Naming Conventions

- Module names: nouns (`model`, `parser`, `builder`, `query`, `rules`, `output`)
- Functions: verbs (`parse_document()`, `find_orphans()`, `resolve_edges()`)
- Types: PascalCase nouns (`Node`, `Edge`, `Graph`, `Kind`, `Status`)
