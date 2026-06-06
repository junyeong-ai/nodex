# nodex-cli

Thin CLI binary wrapping `nodex-core`. All logic is in core — CLI handles argument parsing and JSON formatting.

## Structure

- `main.rs` — top-level `Command` enum, clap parsing, dispatch only
- `format.rs` — `Envelope<T>` / `ErrorEnvelope` JSON wrappers, `print_json()`, error classification via `downcast_ref`
- `commands/<name>.rs` or `commands/<name>/` — one file or submodule directory per subcommand. Each owns every clap type its command needs (`Subcommand`, `ValueEnum`, or `Args`) **and** the `pub fn run(...)` handler. Large commands (e.g. `query/`) split handlers into submodules by concern. `main.rs` never contains a command's CLI shape.

## Adding a Command

1. Create `commands/new_cmd.rs` with:
   - Any `#[derive(Subcommand)]` / `#[derive(ValueEnum)]` / `#[derive(Args)]` types the command needs (use `Args` to group four or more flat flags so the dispatch stays a single-argument forward)
   - `pub fn run(root: &Path, …typed args…, pretty: bool) -> Result<()>`
2. Register the module in `commands/mod.rs`
3. Import the types in `main.rs` and add the variant to the top-level `Command` enum
4. Add a one-line dispatch arm in `main()` that forwards to `commands::new_cmd::run`
5. Emit output with `print_json(&Envelope::success(data), pretty)` — never `println!`

## Config & Boundaries

- `Config::load()` is called early and validates ALL semantic fields at load time (in `nodex-core`)
- CLI never re-validates or re-loads config — it passes the validated `Config` directly to core commands
- Every command receives the same validated config; errors at load time prevent the CLI from even starting

## Error Handling

`main()` catches errors and emits `ErrorEnvelope` via `format::ErrorEnvelope::from_error`, which classifies the typed cause through `downcast_ref::<nodex_core::error::Error>`. Command functions return `anyhow::Result`; the typed `Error` chain must be preserved through any `with_context` wrapping so the classifier can still find it. Exit codes: 0 (success), 1 (`check` found Error-severity violations), 2 (every error envelope — config, parse, IO, version, CLI-arg, runtime). See `.claude/rules/json-output.md` for the envelope contract.
