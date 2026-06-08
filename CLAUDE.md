# Nodex

Markdown frontmatter SSOT with a queryable document graph. Extracts
frontmatter, body links, and config-declared body markers from every
in-scope markdown file into an immutable graph, validates against a
config-driven schema, and routes every mutation through one safe
path. Pure CLI, JSON-first envelope.

## Build & Test

```bash
cargo build --release      # produces target/release/nodex
./scripts/check.sh         # the full gate — mirrors CI exactly; run before every push
```

`scripts/check.sh` runs the same checks as `.github/workflows/ci.yml`, in
order: `fmt --check`, `clippy --all-targets --all-features -D warnings`,
`check --all-features --locked`, **`cargo nextest run --all-features
--workspace`**, `build --release --locked`, `cargo audit`. Use `nextest`,
not `cargo test`: nextest runs each test in its own process, so it catches
test-isolation bugs (a test leaning on shared CWD / `/tmp` / global state)
that single-process `cargo test` silently passes — exactly the class that
turns a green local run into a red CI.

## Architecture

- `nodex-core/` — library; all graph, config, and rule logic. See `nodex-core/CLAUDE.md`.
- `nodex-cli/` — thin wrapper; routes JSON through core. See `nodex-cli/CLAUDE.md`.

## Project-wide rules

The `.claude/rules/` directory holds the authoritative rules:

- `principles.md` — evidence-based, root-cause-first, config-over-code
- `config-driven.md` — self-consistency invariants between config validation, runtime, and tool-written documents
- `rust.md` — Rust conventions
- `json-output.md` — CLI envelope contract

When in doubt, read the rule file. Don't restate it here.
