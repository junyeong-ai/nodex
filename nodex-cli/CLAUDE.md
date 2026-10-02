# nodex-cli

Thin wrapper around core: arguments, orchestration and JSON output. `rename`,
`migrate` and `retarget` compose core primitives in their command modules; guards,
planning and content writes remain in core (`plan_file` / `narrow` / `stage_plan` /
`write_plan`, `path_guard`, `reference_rewrite`). Rename's actual path move is
`std::fs::rename` between guarded paths. Its preparation never edits the source;
final destination content commits after the move and before inbound references.
A failed destination commit reports a partial move and stops reference commits.

## Command and output boundaries

- `main.rs` owns the top-level Command, clap parsing and one-line dispatch.
  `commands/<name>.rs` or its submodules own every command clap type and handler.
  Global `--jobs` configures the CLI pool; embeds choose their own Rayon policy.
- `envelope.rs` is the shared encoder for nodex and contract-gate (path inclusion).
  `format.rs` owns Envelope / ItemsEnvelope and typed error classification.
  `emit_read*` merges binary compatibility; `emit_write` merges baseline advisories.
  Snapshot queries use `commands/query/mod.rs::QueryContext::emit_read_with` to
  attach probe metadata before the read emitter. Do not bypass it in a query leaf.
- Project reads use `nodex_core::load_project` (config validation + preflight).
  Writes also require binary compatibility (`load_project_for_mutation`, or an
  explicit check where dry runs stay readable). Validate prerequisites and locate
  the write lock before acquiring it; `writes_documents` covers the first read
  through final write of mutating invocations.
- `init` and export envelope-schema/commands/diagnostics load no project. Diff/
  impact graph both refs under the after ref's config; staged check uses the index
  config. These load `Config` from checkouts, not `load_project`: preflight is
  about the real working tree, so staged check asks it there.
- Command errors are `anyhow::Result` preserving the typed core cause through
  context wrapping; main classifies by downcast, never message text. Contracts:
  `.claude/rules/json-output.md`; new-command procedure:
  `.claude/rules/adding-a-cli-command.md`.

## Git checkouts and judgment inputs

`commands/git_checkout.rs` owns `Checkout`, `Prior`, `Current`, baseline/history
reads and their rustdoc. Keep these invariants when extending that substrate:

- A Checkout persists at the common git dir's `nodex/checkout/<n>` with a separate
  index and exclusive lock. `read-tree --reset -u` writes only tree differences.
  A git writer holds the lock through stdin (`Checkout::writer`); on Unix, a child
  surviving a killed parent retains it. Concurrent readers never share a slot.
- Checkouts are not registered worktrees; hooks do not run. Preserve operator
  conversion while disabling settings reaching outside checkout/index (sparse,
  fsmonitor, split index, submodule recursion via `Repository::checkout_command`).
  Converted files (filters, ident, encoding) depend on current config, so dropping
  a Checkout removes them. The `<n>.converted` marker recovers cleanup after a
  stopped run; staged check drops its checkout before an encoder can exit.
- Every invocation uses one `Repository` (`ensure_repository` supplies typed
  GIT_ERROR). Graph at `Repository::locate(checkout)` so nested projects stay
  scoped. Ask `ref_state` / `recorded` before checkout; disk stats cannot
  distinguish an absent project from a gitlink's empty directory.
- `baseline_graph` builds the baseline under the judged config and retains its
  warnings. `diff_against_ref` / `baseline_diff` use that graph for reads;
  `write_baseline` passes it into `BaselineBinding::snapshot` for locks. Preserve
  typed baseline build errors; synthesize GIT_ERROR only for untyped causes.
  Diff/impact have no baseline: the after config lenses both `build_of_ref` calls.
- `BaselineResolution` is NotApplicable, Inert { warning }, or Resolved(diff plus
  ref-tagged build warnings). Every consumer handles all three. Activation and
  wording come from core `BaselineBinding`, not a second CLI derivation.
- With step rules, baseline_graph also reads history. Without an applicable
  baseline, `history` / `uncommitted_history` still feeds non-diff-aware step rules
  (status flows), while diff-aware locks skip. Explicit `--since` alone reads
  `Steps::Range`; plain check, issues and write gates read HEAD/MERGE_HEAD through
  `Steps::Uncommitted`. Read judgment receives Prior plus Current (graph + files).
- `check --staged` holds the index tree's checkout through judgment; baseline and
  history use a second checkout. `staged_tree` alone reads GIT_INDEX_FILE.
  `Checkout::tree_of_index` copies the operator's index before `write-tree`, which
  can lock/write its input; never mutate git commit's or the operator's index.

`commands/content_source.rs` is the single SOURCE grammar for check --content and
scaffold --body: `-` reads stdin, other paths resolve against invoking directory.
