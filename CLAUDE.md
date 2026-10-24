# Nodex

Markdown frontmatter SSOT with a queryable document graph. Extracts
frontmatter (including config-declared ID relations), body links, and declared
body markers into an immutable graph, validates against a
config-driven schema, and routes every mutation through one safe
path. Pure CLI, JSON-first envelope.

## Build & Test

```bash
cargo build --release      # produces target/release/nodex
./scripts/check.sh         # the local gate — run before every push
```

`scripts/check.sh` runs the checks of the `lint.yml` and `ci.yml` workflows
(`.github/workflows/`); read the script for the exact steps. It is not a
complete CI proxy: CI also checks the MSRV (`rust-version` in the root
Cargo.toml) and runs the tests on a multi-OS matrix, so a failure specific
to another platform — a Windows checkout's CRLF line endings, or an import
only `#[cfg(unix)]` tests use, which CI's `-D warnings` refuses on Windows —
shows up only there. `RUSTFLAGS="-D warnings" cargo check --workspace --tests
--all-features --target x86_64-pc-windows-msvc` (after `rustup target add
x86_64-pc-windows-msvc`) catches the second locally. zizmor's online audits, which resolve each pinned
action on GitHub, run locally only when `GH_TOKEN` is set. With
`cargo-nextest`, `cargo-audit` or `zizmor` missing, check.sh degrades
(falls back to `cargo test` / skips that audit) and says so: a degraded
run ends on the yellow banner naming the step count, and only a complete
run earns the green one. Install all three to get the green.
Use `cargo nextest run`, not `cargo test`: CI runs nextest, whose
per-process isolation catches shared-state test bugs `cargo test` hides.

## Architecture

- `nodex-core/` — library; all graph, config, and rule logic. See `nodex-core/CLAUDE.md`.
- `nodex-cli/` — thin wrapper; routes JSON through core. See `nodex-cli/CLAUDE.md`.

## Project-wide rules

The `.claude/rules/` directory holds the authoritative rules; each file's `paths:`
frontmatter says when it loads:

- `principles.md` — evidence-based, root-cause-first, triage-before-fixing, config-over-code
- `config-driven.md` — self-consistency invariants between config validation, runtime, and tool-written documents
- `rust.md` — Rust conventions
- `json-output.md` — CLI envelope contract
- `adding-a-validation-rule.md` / `adding-a-cli-command.md` — procedures
- `releasing.md` — choosing, bumping, publishing and verifying a version

When in doubt, read the rule file. Don't restate it here.

CLI usage workflows live in `.claude/skills/nodex/SKILL.md` and its `reference/`
files. Validate those instructions against the working-tree binary's help and
exports; an installed binary or skill may describe a different release.
