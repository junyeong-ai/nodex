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
   handler (batch writes return `Result<ExitCode>` through `emit_batch_write`); a command whose answer is date-relative also takes
   `today: NaiveDate` from `main`, the one place the clock is read.
2. Register the module in `commands/mod.rs`.
3. Add the variant to the top-level `Command` enum in `main.rs` and a
   one-line dispatch arm forwarding to `commands::new_cmd::run` —
   `main.rs` never contains a command's CLI shape. A document-writing mode
   also joins `writes_documents` so `ProjectLock` covers the handler's first
   read through its final write; dry runs remain read-only.
4. Emit through the seam `.claude/rules/json-output.md` (Adding Output)
   names for the command's plane; a writing command passes the
   `BaselineProbe` its writes locked against (from
   `git_checkout::write_baseline`).
5. Register the command's data-payload schema in
   `nodex_core::export::per_command_schemas` under its dotted
   invocation path (e.g. `query.dependents`) — the
   `every_cli_leaf_has_a_per_command_schema` test in `main.rs` fails
   any CLI leaf without a schema and any schema key without a leaf,
   so the typed-codegen contract cannot drift from the command
   surface.
6. Close the leaf into the tests that enumerate the command surface,
   each of which fails naming what is missing: `SKILL.md` and both READMEs
   name it, and any new error or warning code in their `published:` lists
   (`every_page_lists_the_published_vocabularies_whole`); the behaviour sweep
   runs or exempts it (`every_command_is_swept_or_exempt`, then
   `behaviour_sweep`'s snapshots of the command surface, judged with
   `cargo insta review`); and a real
   output of it is validated against its schema
   (`every_command_real_output_conforms_to_its_per_command_schema`).
