# nodex-cli

Thin CLI binary wrapping `nodex-core`. Domain logic is in core — CLI handles argument parsing and JSON formatting — with two named exceptions: `rename` and `migrate` are CLI-orchestrated compositions of core primitives, whose multi-step sequencing (per-file planning, reference rewrites, result aggregation) lives in their command modules while every guard and write they perform routes through core seams (`apply_to_file`, `write_atomic_in_root`, `BaselineProbe`, `reference_rewrite`).

## Structure

- `main.rs` — top-level `Command` enum, clap parsing, dispatch only
- `envelope.rs` — the bin-shared envelope encoder (`ErrorEnvelope` + `print_json()`); both bin targets emit through it — `nodex` via `format`'s re-export, `contract-gate` via `#[path]` inclusion — so the envelope contract has exactly one encoder
- `format.rs` — `Envelope<T>` / `ItemsEnvelope` wrappers, error classification via `downcast_ref`, re-exports the shared encoder; `emit_read` / `emit_read_with` are the single seam merging the binary-compat advisory into read-command envelopes, and `emit_write` is its write-side twin, merging the unenforced-baseline advisory into every mutating command's envelope — so no handler on either plane has to remember its cross-cutting advisory
- `commands/<name>.rs` or `commands/<name>/` — one file or submodule directory per subcommand. Each owns every clap type its command needs (`Subcommand`, `ValueEnum`, or `Args`) **and** the `pub fn run(...)` handler. Large commands (e.g. `query/`) split handlers into submodules by concern. `main.rs` never contains a command's CLI shape.

## Adding a Command

See `.claude/rules/adding-a-cli-command.md` — it loads when a file under `nodex-cli/src/` is being read or edited.

## Config & Boundaries

- `Config::load()` is called early and validates ALL semantic fields at load time (in `nodex-core`)
- CLI never re-validates or re-loads config — it passes the validated `Config` directly to core commands
- Every command receives the same validated config; errors at load time prevent the CLI from even starting

## Shared substrates

`commands/git_worktree.rs` owns worktree materialisation:
`diff_against_ref` (check a ref out in a disposable RAII worktree, graph
the project inside it, diff against the current graph) is the one
substrate behind `check --since`, default `check`'s
`rules.immutable_baseline`, `query issues`, `diff` and `impact`. Every
invocation is built from a `nodex_core::Repository` — obtained via
`ensure_repository` (typed `GIT_ERROR`) or from `BaselineProbe::bound`
— and a checkout is only ever graphed through `Worktree::project_root`
(`require_project_root` for `diff` / `impact`, which need both sides),
so a project that is not the repository's top level is never read as
the repository around it.

`diff_against_ref` and `baseline_diff` both return the typed
`BaselineResolution` — `NotApplicable` (no baseline configured, or no
immutability rules to feed), `Inert { warning }` (no work tree, or the
ref does not carry the project), or `Resolved(BaselineDiff)` = the diff
plus the baseline build's own ref-tagged warnings — so every consumer
maps the same three states and none can silently drop the inert
advisory. Activation and its wording come from
`nodex_core::BaselineProbe` (resolved once per command, also passed to
`scaffold` / `transition` / `apply_to_file`), so the read and write
planes cannot disagree about whether the locks engaged. `commands/content_source.rs`
owns the byte-source grammar (`-` = stdin, else a file path) shared by
`check --content` and `scaffold --body`.

## Error Handling

`main()` catches errors and emits `ErrorEnvelope` via `format::ErrorEnvelope::from_error`, which classifies the typed cause through `downcast_ref::<nodex_core::error::Error>`. Command functions return `anyhow::Result`; the typed `Error` chain must be preserved through any `with_context` wrapping so the classifier can still find it. Envelope contract and exit codes: `.claude/rules/json-output.md`.
