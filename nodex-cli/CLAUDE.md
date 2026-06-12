# nodex-cli

Thin CLI binary wrapping `nodex-core`. Domain logic is in core — CLI handles argument parsing and JSON formatting — with two named exceptions: `rename` and `migrate` are CLI-orchestrated compositions of core primitives, whose multi-step sequencing (per-file planning, reference rewrites, result aggregation) lives in their command modules while every guard and write they perform routes through core seams (`apply_to_file`, `write_atomic_in_root`, `BaselineProbe`, `reference_rewrite`).

## Structure

- `main.rs` — top-level `Command` enum, clap parsing, dispatch only
- `envelope.rs` — the bin-shared envelope encoder (`ErrorEnvelope` + `print_json()`); both bin targets emit through it — `nodex` via `format`'s re-export, `contract-gate` via `#[path]` inclusion — so the envelope contract has exactly one encoder
- `format.rs` — `Envelope<T>` / `ItemsEnvelope` wrappers, error classification via `downcast_ref`, re-exports the shared encoder; `emit_read` / `emit_read_with` are the single seam merging the binary-compat advisory into read-command envelopes, so no query handler has to remember it
- `commands/<name>.rs` or `commands/<name>/` — one file or submodule directory per subcommand. Each owns every clap type its command needs (`Subcommand`, `ValueEnum`, or `Args`) **and** the `pub fn run(...)` handler. Large commands (e.g. `query/`) split handlers into submodules by concern. `main.rs` never contains a command's CLI shape.

## Adding a Command

See `.claude/rules/adding-a-cli-command.md` — it loads when a file under `nodex-cli/src/` is being read or edited.

## Config & Boundaries

- `Config::load()` is called early and validates ALL semantic fields at load time (in `nodex-core`)
- CLI never re-validates or re-loads config — it passes the validated `Config` directly to core commands
- Every command receives the same validated config; errors at load time prevent the CLI from even starting

## Shared substrates

`commands/git_worktree.rs` owns worktree materialisation:
`diff_against_ref` (build a ref in a disposable RAII worktree, diff
against the current graph, returning `BaselineDiff` = the diff plus the
baseline build's own ref-tagged warnings) is the one substrate behind
`check --since`, default `check`'s `rules.immutable_baseline`, and
`query issues`. The latter two consume it through `baseline_diff`,
which resolves the configured baseline into the typed
`BaselineResolution` — `NotApplicable` (no baseline configured, or no
immutability rules to feed), `Inert { warning }` (baseline set but the
root is not a git work tree; the advisory wording is constructed
exactly once, here), or `Resolved(BaselineDiff)` — so the consumers
can never disagree about immutability violations nor silently drop
the inert advisory. Byte-level git access (`nodex_core::git::{is_work_tree,
ref_file_content}`) and the immutability lock probe
(`nodex_core::BaselineProbe`, constructed once per mutating command and
passed to `scaffold` / `transition` / `apply_to_file`) live in core,
where the mutation seams consume them. `commands/content_source.rs`
owns the byte-source grammar (`-` = stdin, else a file path) shared by
`check --content` and `scaffold --body`.

## Error Handling

`main()` catches errors and emits `ErrorEnvelope` via `format::ErrorEnvelope::from_error`, which classifies the typed cause through `downcast_ref::<nodex_core::error::Error>`. Command functions return `anyhow::Result`; the typed `Error` chain must be preserved through any `with_context` wrapping so the classifier can still find it. Envelope contract and exit codes: `.claude/rules/json-output.md`.
