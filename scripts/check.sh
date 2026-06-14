#!/usr/bin/env bash
# Local gate — runs the same checks as .github/workflows/lint.yml (fmt,
# clippy, doc) then ci.yml (check, nextest, build, audit). Not a complete CI
# proxy: CI's MSRV job uses the pinned toolchain from Cargo.toml's
# rust-version (this script uses your local toolchain) and CI's test job
# runs a multi-OS matrix. The divergence this prevents: `cargo test` runs
# every test in one process, but CI uses `cargo nextest`, which runs each
# in its own process and so surfaces test-isolation bugs (shared CWD /
# `/tmp` roots / global state) that `cargo test` hides. Run before every
# push.
set -euo pipefail
cd "$(dirname "$0")/.."

step() { printf '\n\033[1;36m▶ %s\033[0m\n' "$1"; }

# Steps that ran in a weaker form (or not at all) because a tool is
# missing locally. A degraded run never earns the green banner — CI
# runs the full set, so a weaker local pass is not the same signal.
degraded=0

step "fmt   (cargo fmt --all -- --check)"
cargo fmt --all -- --check

step "clippy (--all-targets --all-features -- -D warnings)"
cargo clippy --all-targets --all-features -- -D warnings

step "doc    (cargo doc --no-deps --workspace, RUSTDOCFLAGS=-D warnings)"
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --all-features

step "check  (--workspace --all-features --locked)"
cargo check --workspace --all-features --locked

step "test   (cargo nextest run --all-features --workspace)"
if command -v cargo-nextest >/dev/null 2>&1; then
  cargo nextest run --all-features --workspace
else
  degraded=$((degraded + 1))
  echo "  cargo-nextest not found — CI runs it (per-process isolation catches"
  echo "  bugs cargo test hides). Install: cargo install cargo-nextest"
  echo "  Falling back to cargo test (weaker):"
  cargo test --all-features --workspace
fi

step "build  (cargo build --release --locked)"
cargo build --release --locked

step "audit  (cargo audit)"
if command -v cargo-audit >/dev/null 2>&1; then
  cargo audit
else
  degraded=$((degraded + 1))
  echo "  cargo-audit not found — CI runs it. Install: cargo install cargo-audit"
fi

if [ "$degraded" -gt 0 ]; then
  printf '\n\033[1;33m⚠ passed with %d degraded step(s) — install cargo-nextest / cargo-audit for the full gate\033[0m\n' "$degraded"
else
  printf '\n\033[1;32m✓ all CI checks passed locally\033[0m\n'
fi
