# Nodex

Universal graph-based document tool. Parses markdown files with YAML frontmatter, builds an immutable document graph, and exposes queries via a JSON-first CLI.

## Build & Test

```bash
cargo build --release      # produces target/release/nodex
cargo test                 # workspace tests (unit + nodex-cli integration)
```

## Workspace

- `nodex-core/` — library; all logic lives here. See `nodex-core/CLAUDE.md`.
- `nodex-cli/` — thin clap binary, JSON envelope wrapper. See `nodex-cli/CLAUDE.md`.

All project-specific behavior is driven by `nodex.toml`. No domain logic is hardcoded in core.

## Project-wide rules

The `.claude/rules/` directory holds the authoritative rules:

- `principles.md` — evidence-based, root-cause-first, config-over-code
- `config-driven.md` — self-consistency invariants between config validation, runtime, and tool-written documents
- `rust.md` — Rust conventions
- `json-output.md` — CLI envelope contract

When in doubt, read the rule file. Don't restate it here.
