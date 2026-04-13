# nodex-cli

Thin CLI binary wrapping `nodex-core`. All logic is in core — CLI handles argument parsing and JSON formatting.

## Structure

- `main.rs` — clap derive CLI, dispatches to command handlers
- `format.rs` — `Envelope<T>` / `ErrorEnvelope` JSON wrappers, `print_json()`, error classification via `downcast_ref`
- `commands/` — one file per subcommand: `build.rs`, `query.rs`, `check.rs`, `lifecycle.rs`, `report.rs`, `migrate.rs`, `rename.rs`, `init.rs`

## Adding a Command

1. Add variant to `Command` enum in `main.rs`
2. Create `commands/new_cmd.rs`
3. Wire in match arm in `main()`
4. Output via `print_json(&Envelope::success(data), pretty)`

## Error Handling

- Use `anyhow::Result` in all command functions
- `main()` catches errors and emits `ErrorEnvelope` with classified error code
- Error codes are derived from `nodex_core::error::Error` variants via `downcast_ref`, not string matching
