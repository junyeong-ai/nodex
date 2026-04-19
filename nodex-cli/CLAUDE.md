# nodex-cli

Thin CLI binary wrapping `nodex-core`. All logic is in core — CLI handles argument parsing and JSON formatting.

## Structure

- `main.rs` — top-level `Command` enum, clap parsing, dispatch only
- `format.rs` — `Envelope<T>` / `ErrorEnvelope` JSON wrappers, `print_json()`, error classification via `downcast_ref`
- `commands/<name>.rs` — one file per subcommand. Each file owns every clap type its command needs (`Subcommand`, `ValueEnum`, or `Args`) **and** the `pub fn run(...)` handler. `main.rs` never contains a command's CLI shape.

## Adding a Command

1. Create `commands/new_cmd.rs` with:
   - Any `#[derive(Subcommand)]` / `#[derive(ValueEnum)]` / `#[derive(Args)]` types the command needs (use `Args` to group four or more flat flags so the dispatch stays a single-argument forward)
   - `pub fn run(root: &Path, …typed args…, pretty: bool) -> Result<()>`
2. Register the module in `commands/mod.rs`
3. Import the types in `main.rs` and add the variant to the top-level `Command` enum
4. Add a one-line dispatch arm in `main()` that forwards to `commands::new_cmd::run`
5. Emit output with `print_json(&Envelope::success(data), pretty)` — never `println!`

## Error Handling

- Use `anyhow::Result` in all command functions
- `main()` catches errors and emits `ErrorEnvelope` with classified error code
- Error codes are derived from `nodex_core::error::Error` variants via `downcast_ref`, not string matching
