# nodex-cli

Thin CLI binary wrapping `nodex-core`. Domain logic is in core — CLI handles argument parsing and JSON formatting — with three named exceptions: `rename`, `migrate` and `retarget` are CLI-orchestrated compositions of core primitives, whose multi-step sequencing (per-file planning, reference rewrites, result aggregation) lives in their command modules while every guard and content write they perform routes through core seams (`plan_file` / `narrow` / `stage_plan` / `write_plan`, `path_guard::stage_in_root`, `reference_rewrite`); `rename` moves the file itself with `std::fs::rename`, between paths `path_guard::reject_outside_root` has checked.

## Structure

- `main.rs` — top-level `Command` enum, clap parsing, dispatch only
- `envelope.rs` — the bin-shared envelope encoder (`ErrorEnvelope` + `print_json()`); both bin targets emit through it — `nodex` via `format`'s re-export, `contract-gate` via `#[path]` inclusion — so the envelope contract has exactly one encoder
- `format.rs` — `Envelope<T>` / `ItemsEnvelope` wrappers, error classification via `downcast_ref`, re-exports the shared encoder; `emit_read` / `emit_read_with` are the single seam merging the binary-compat advisory into read-command envelopes, and `emit_write` is its write-side twin, merging the unenforced-baseline advisory into every mutating command's envelope — so no handler on either plane has to remember its cross-cutting advisory
- `commands/<name>.rs` or `commands/<name>/` — one file or submodule directory per subcommand. Each owns every clap type its command needs (`Subcommand`, `ValueEnum`, or `Args`) **and** the `pub fn run(...)` handler. Large commands (e.g. `query/`) split handlers into submodules by concern. `main.rs` never contains a command's CLI shape.

## Adding a Command

See `.claude/rules/adding-a-cli-command.md` — it loads when a file under `nodex-cli/src/` is being read or edited.

## Config & Boundaries

- Each handler that reads the project loads its config through `nodex_core::load_project` (`Config::load`, which validates every semantic field, plus `rules::preflight`); a write of documents is also gated by `ensure_binary_compatible` (`load_project_for_mutation`, or called before the write where a dry run stays readable). `init` and `export envelope-schema|commands|diagnostics` load none; `diff` and `impact` graph both refs under the after ref's config and `check --staged` the index under the index's, each read with `Config::load` from the checkout (the lens) — a checkout is no project location for `load_project`'s preflight to measure, so `check --staged` runs `preflight` against the working tree
- CLI never re-validates or re-loads config — it passes the validated `Config` directly to core commands

## Shared substrates

`commands/git_checkout.rs` owns reading the project's git history. Every
tree is written into a `Checkout`: a directory under the repository's common
git directory (`nodex/checkout/<n>`, with its own index beside it) that
persists between runs and is switched from tree to tree with `read-tree
--reset -u`, so a read writes what differs from the tree it last held rather
than the whole repository. A process holds one through an exclusive file
lock for as long as it keeps the `Checkout`, and takes the next when one is
held, so concurrent runs never share a directory. It is no work tree of the
repository — nothing registers it and no hook runs — and its invocations keep
the operator's content conversion while pinning off every setting that
reaches past the directory and its index: sparse checkout, a filesystem
monitor, a split index, submodule recursion (`Repository::checkout_command`).
What persists is a function of each blob alone. A file git writes through a
filter, `ident` or `working-tree-encoding` (`Repository::converted_files`)
depends on the configuration when it is written, so dropping a `Checkout`
removes those — `check` drops its staged one before a verdict can end the
process — and a marker beside the index (`<n>.converted`) tells the next
process to take the directory that a run stopped before it could.
`baseline_graph` is the one definition of "the baseline": it checks a ref
out, graphs the project inside it under the config of the project being
judged (the single lens), and returns that graph with the build's own
warnings. `diff_against_ref` (behind `check --since`) and
`baseline_diff` (behind a plain `check` and `query issues`, under
`rules.immutable_baseline`) diff it against the current graph, and
`write_baseline` hands the same graph to
`nodex_core::BaselineBinding::snapshot`, so a mutating command locks against
the baseline `check` reports on rather than a second reading of it. `diff`
and `impact` take no baseline: they check the after ref out, load its config
as the lens, then graph both refs through
`nodex_core::builder::build_of_ref`. Every invocation is built from a
`nodex_core::Repository` — obtained via `ensure_repository` (typed
`GIT_ERROR`) or from the binding — and a checkout is only ever graphed at
`Repository::locate` of its directory, so a project that is not the
repository's top level is never read as the repository around it. Whether a
ref carries the project is established by `recorded` from
`Repository::ref_state` before anything is checked out, never from the
checkout on disk: a checkout leaves an empty directory for a submodule it
does not populate, so a stat reads a gitlink at the prefix as the project and
graphs an empty baseline. Graphing the baseline runs the build `check` runs,
so it fails the same typed ways: `write_baseline` keeps the core error a
failed baseline build carries and synthesises `GIT_ERROR` only for a cause
that has none — one condition cannot answer to two codes depending on which
plane reached it. Where a registered rule judges steps
(`Config::judges_steps`), history is read beside the baseline and
independently of it: `baseline_graph` walks it in the baseline's checkout,
and `history` / `uncommitted_history` take their own at the first commit that
carries the project. Only an explicit `--since` walks a range
(`Steps::Range`); a plain `check`, `query issues` and `write_baseline` read
only `HEAD` and any `MERGE_HEAD` (`Steps::Uncommitted`). Read commands receive
both as `Prior`, judged against a `Current`: the graph being judged and where
its files are. That is the working tree, or under `check --staged` the
checkout of the tree `staged_tree` writes of the index git is committing —
the one place `GIT_INDEX_FILE` is read — which the command holds through the
rule pass while the baseline and history take a second checkout. The tree is
written from a copy the checkout holds (`Checkout::tree_of_index`), because
`write-tree` locks and writes the index it reads, and that index is the
operator's or the committing `git commit`'s.

`diff_against_ref` and `baseline_diff` both return a `Prior` whose baseline
is the typed `BaselineResolution` — `NotApplicable` (no baseline configured, or no
immutability rules to feed), `Inert { warning }` (no work tree, or the ref
does not carry the project), or `Resolved(BaselineDiff)` = the diff plus the
baseline build's own ref-tagged warnings — so every consumer maps the same
three states and none can silently drop the inert advisory. Activation and
its wording come from `nodex_core::BaselineBinding`, whose snapshot the write
seams (`scaffold` / `transition` / the batch gate) receive, so the read and
write planes cannot disagree about whether the locks engaged. `commands/content_source.rs`
owns the byte-source grammar (`-` = stdin, else a file path) shared by
`check --content` and `scaffold --body`.

## Error Handling

`main()` catches errors and emits `ErrorEnvelope` via `format::ErrorEnvelope::from_error`, which classifies the typed cause through `downcast_ref::<nodex_core::error::Error>`. Command functions return `anyhow::Result`; the typed `Error` chain must be preserved through any `with_context` wrapping so the classifier can still find it. Envelope contract and exit codes: `.claude/rules/json-output.md`.
