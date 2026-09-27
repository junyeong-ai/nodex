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
   handler; a command whose answer is date-relative also takes
   `today: NaiveDate` from `main`, the one place the clock is read.
2. Register the module in `commands/mod.rs`.
3. Add the variant to the top-level `Command` enum in `main.rs` and a
   one-line dispatch arm forwarding to `commands::new_cmd::run` —
   `main.rs` never contains a command's CLI shape.
4. Emit output through `format::emit_read*` for a read command, which
   merges the binary-compat advisory, or `format::emit_write` for one
   that writes documents, which merges the advisories of the
   `BaselineProbe` its writes locked against (obtained from
   `git_worktree::write_baseline`) — never `println!`. Where
   `print_json` is called directly instead:
   `.claude/rules/json-output.md`.
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
7. Close the leaf into the tests that enumerate the command surface,
   each of which fails naming what is missing: `SKILL.md` names it, and
   any new error or warning code in its `published:` lists
   (`the_skill_names_every_published_vocabulary`); the behaviour sweep
   runs or exempts it (`every_command_is_swept_or_exempt`, then
   `behaviour_sweep`'s snapshots of the command surface); and a real
   output of it is validated against its schema
   (`every_command_real_output_conforms_to_its_per_command_schema`).
