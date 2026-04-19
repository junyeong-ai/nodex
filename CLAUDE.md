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

- **Immutable graph**: built from scratch each run via `Graph::new()`, no mutation after construction
- **pulldown-cmark for links**: AST-based markdown parsing, not regex — avoids code block false positives
- **SHA256 incremental cache**: `_index/cache.json` auto-invalidates on config change or binary upgrade
- **ResolvedTarget enum**: edges use `Resolved{id}` / `Unresolved{raw, reason}` — type-safe, no string-prefix hacking
- **Kind/Status newtypes**: `String` wrappers validated by config, not hardcoded enums

## Naming Conventions

- Module names: nouns (`model`, `parser`, `builder`, `query`, `rules`, `output`, `scaffold`)
- Functions: verbs (`parse_document()`, `find_orphans()`, `resolve_edges()`, `collect_issues()`, `scaffold()`)
- Types: PascalCase nouns (`Node`, `Edge`, `Graph`, `Kind`, `Status`, `IssueReport`, `ScaffoldSpec`)

## Schema Enforcement

Per-kind schema constraints live in `nodex.toml` under `[[schema.overrides]]`:

- `required` — field names that must be present
- `types` — `{ field = "string|integer|bool|date" }`
- `enums` — `{ field = ["allowed", "values"] }`
- `cross_field` — `[{ when = "field=value", require = "other_field" }]`

Each block is opt-in: omit it and the corresponding rule short-circuits. `Config::load` calls `validate()` so inconsistent enum/cross_field definitions fail fast at load time, not at check time.
