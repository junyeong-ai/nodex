---
paths:
  - "nodex-cli/src/**"
---

# Adding a CLI command

1. Create `commands/new_cmd.rs` owning every clap type the command
   needs (`#[derive(Subcommand)]` / `#[derive(ValueEnum)]` /
   `#[derive(Args)]` — use `Args` to group four or more flat flags so
   dispatch stays a single-argument forward) and the
   `pub fn run(root: &Path, …typed args…, pretty: bool) -> Result<()>`
   handler.
2. Register the module in `commands/mod.rs`.
3. Add the variant to the top-level `Command` enum in `main.rs` and a
   one-line dispatch arm forwarding to `commands::new_cmd::run` —
   `main.rs` never contains a command's CLI shape.
4. Emit output with `print_json(&Envelope::success(data), pretty)` —
   or `format::emit_read*` for read commands, which merge the
   binary-compat advisory — never `println!`
   (`.claude/rules/json-output.md`).
5. Register the command's data-payload schema in
   `nodex_core::export::per_command_schemas` under its dotted
   invocation path (e.g. `query.dependents`) — the
   `every_cli_leaf_has_a_per_command_schema` test in `main.rs` fails
   any CLI leaf without a schema and any schema key without a leaf,
   so the typed-codegen contract cannot drift from the command
   surface.
6. A second flag-selected response shape (one leaf, two payload
   schemas — e.g. `query trust --bottom/--top` → `query.trust-list`)
   must additionally be declared in `commands/export.rs::FLAG_MODES`:
   the table feeds both the published commands manifest and the
   bijection test, which verifies the declared flags exist on the
   leaf.
