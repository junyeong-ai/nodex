# Nodex

Markdown frontmatter SSOT with a queryable document graph. Extracts
frontmatter, body links, and config-declared body markers from every
in-scope markdown file into an immutable graph, validates against a
config-driven schema, and routes every mutation through one safe
path. Pure CLI, JSON-first envelope.

## Build & Test

```bash
cargo build --release      # produces target/release/nodex
./scripts/check.sh         # the local gate — run before every push
```

`scripts/check.sh` runs the same checks as the CI workflows
(`.github/workflows/`); read the script for the exact steps. It is not a
complete CI proxy: CI's MSRV job checks under the pinned toolchain from
`rust-version` in the root Cargo.toml (check.sh uses your local toolchain,
so a post-MSRV feature passes locally and fails CI), and CI's test job
runs a multi-OS matrix. With `cargo-nextest` or `cargo-audit` missing,
check.sh degrades (falls back to `cargo test` / skips the audit) yet still
prints the success banner — install both. Use `cargo nextest run`, not
`cargo test`: CI runs nextest, whose per-process isolation catches
shared-state test bugs `cargo test` hides.

## Architecture

- `nodex-core/` — library; all graph, config, and rule logic. See `nodex-core/CLAUDE.md`.
- `nodex-cli/` — thin wrapper; routes JSON through core. See `nodex-cli/CLAUDE.md`.

## Project-wide rules

The `.claude/rules/` directory holds the authoritative rules:

- `principles.md` — evidence-based, root-cause-first, config-over-code
- `config-driven.md` — self-consistency invariants between config validation, runtime, and tool-written documents
- `rust.md` — Rust conventions (path-scoped: loads with `**/*.rs`)
- `json-output.md` — CLI envelope contract (path-scoped: `nodex-cli/**/*.rs`)
- `adding-a-validation-rule.md` / `adding-a-cli-command.md` — procedures (path-scoped: `nodex-core/src/rules/**` / `nodex-cli/src/**`)

When in doubt, read the rule file. Don't restate it here.
