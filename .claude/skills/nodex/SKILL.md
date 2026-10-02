---
name: nodex
description: >-
  JSON-first CLI for markdown document graphs governed by a root `nodex.toml`. Use to check or
  lint docs: frontmatter schema, body immutability and its `append_section` corrections, `check
  --since <ref>`, the write-time gate `check --content <path>=-`, the pre-commit gate `check
  --staged`, typed violation `details` for
  auto-fix, `rule_coverage` ("did my rules actually check anything"). Use to query backlinks,
  supersession chains, orphans, stale docs, dependents, annotations, nodes by kind / status /
  tag, a path's node, trust scores and similar docs; to scaffold, rename, migrate, retarget or
  supersede documents through one guarded write path; to diff graphs between git refs or ask
  "what breaks if I merge this"; to export schema / enums / rules / envelope-schema / config /
  commands for typed codegen and API drift. Also for `nodex status` and a stale graph.json,
  the `[statuses.flow]` lifecycle with its `status_transition` / `status_entry` rules,
  body-line vocabulary, `schema.require_explicit` / `forbidden` and per-rule `kinds` filters.
allowed-tools: Bash(nodex *)
metadata:
  version: 0.51.0
---

# nodex — markdown document graph CLI

**The binary is the contract.** Where anything here disagrees with it, the generated manifests decide: `nodex export diagnostics` (error / warning / exit-code vocabularies), `nodex export commands` (every leaf and its positionals; flags are in `--help`), `nodex export envelope-schema` (payload shapes).

## Envelope

Every command (bar clap's `--help` / `help` / `--version`) emits one of:

```json
{"ok": true,  "data": {...}, "warnings": [{"code": "...", "message": "..."}]}
{"ok": false, "error": {"code": "CODE", "message": "..."}}
```

Branch on `error.code` and `warnings[].code`, never on message text. `warnings` is always at envelope level, never inside `data`, and is omitted when empty. **An error envelope carries no `warnings`** — a failing command loses every advisory it had, so anything it must still tell you is in the `error.message`.

Exit codes: `0` ok · `1` `check` found Error-severity violations · `2` every error envelope, and output stdout would not take (a pipe closed early).

List results use `{items, total}`. Listings count all matches and add `returned` when capped by `--limit`; selection queries (`trust --top/--bottom`, `similar`, `recent`) count the selected items.

Global flags: `--jobs N` sets a positive per-process worker count (default: `RAYON_NUM_THREADS` or available CPUs); `--pretty` indents JSON; `-C <dir>` selects the project; `--check-version <semver-req>` rejects incompatible binaries; `--today YYYY-MM-DD` fixes date-relative rules, queries and scaffold dates. `[meta] nodex_version` pins the project: reads warn; writes refuse with `VERSION_MISMATCH`.

## Commands

```
nodex init                      nodex build                     nodex status
nodex check                     nodex report                    nodex migrate
nodex diff <before> <after>     nodex impact <before> <after>
nodex scaffold                  nodex rename <old> <new>        nodex retarget <old_id> <new_id>
nodex lifecycle review <id>     nodex lifecycle set <id>        nodex lifecycle supersede <id>
nodex query search <keyword>    nodex query nodes               nodex query node <id>
nodex query backlinks <id>      nodex query chain <id>          nodex query covered-by <path>
nodex query orphans             nodex query stale               nodex query issues
nodex query trust <id>          nodex query similar             nodex query recent
nodex query components          nodex query neighborhood <id>   nodex query dependents <id>
nodex query annotations
nodex export schema             nodex export enums              nodex export rules
nodex export envelope-schema    nodex export config             nodex export commands
nodex export diagnostics
```

Flags, payload fields and per-leaf semantics: **`reference/commands.md`**. Authoring `nodex.toml`: **`reference/config.md`** and the worked `reference/minimal-config.toml`.

## Build first

**Run `nodex build` before any `query`** — queries read the indexed `_index/graph.json`; without one they fail `GRAPH_MISSING` (exit 2). Build is incremental and cheap to re-run. No other command needs a prior build.

Queries read snapshots; rebuild after edits. Envelope `snapshot` separates verification
(`membership` checks paths/config, `content` also checks bytes) from parsing coverage;
`unbuildable > 0` means documents have no parsed node. `query --require-current` rejects
drift; body reads/search verify revisions. Body-search `--max-matches` / `--max-line-chars`
bound output, not revision checks. Details: `reference/commands.md`.

Lookup failures: `NOT_FOUND` follows a content-verified miss (correct the id; the message
also names empty/unbuildable corpora); `GRAPH_OUTDATED` means rebuild, except an unreadable
in-scope file must first be made readable; `IO_ERROR` means fix the named path because a
rebuild encounters it too. `nodex status` hashes content and returns `state` ∈
`absent | unreadable | schema_mismatch | outdated | current`; CI gates on that state,
and schema mismatch needs `nodex build --full`.

Every corpus reader discloses scope gaps. Inspect `scope_coverage` before treating an
empty answer as complete.

## The write-time gate

```bash
nodex check --content docs/a.md=-                            # proposed bytes from stdin
nodex check --content docs/a.md=- --content docs/b.md=b.md   # batch: one build, cross-proposal refs resolve
nodex check --staged     # pre-commit: the index git commits, not the working tree
```

Validate proposed bytes **before** writing them. `SOURCE` is `-` (stdin) or a file path resolved against the invoking directory, never `-C <dir>`. At most one `SOURCE` may be `-`; a target `PATH` may appear once. Excludes `--since` and `--staged`.

Every proposal is overlaid into ONE graph build, so a reference one proposal authors resolves against another in the same batch — a supersede that rewrites N referrers is judged as one combined proposal. The reported set is the **introduced delta**: a violation already present without the proposal never blocks it; one the overlay adds reds the gate at exit 1.

Caveats:

- `required_field` never fires for engine-derived fields. A proposal missing `id` / `status` (or a stem-derived `title`) still passes because the build infers them. A clean verdict does not certify those keys are spelled out unless `schema.require_explicit` is configured.
- Both builds are read-only, so a write-time check never touches `cache.json`. A path need not exist yet — that is the point. A path inside the project root but outside the scope globs is vacuously clean and the run warns it validated nothing; a path escaping the root is refused with `PATH_ESCAPES_ROOT`.
- The gate reports what a proposal *introduces*; a write seam's immutability verdict is **absolute**. A document that already drifted from its frozen baseline passes the gate and is still refused by the write. Revert the drift or supersede the record.

`--severity` narrows the list, never the verdict: `has_errors` and the exit code answer for every violation checked, so `--severity warning` over a project holding errors still exits 1. A `gate_suppression` warning counts what the **envelope** stops carrying, which is not the same as what the list stops showing — in `--content` mode a filtered-out warning on a proposal path is still in `standing`, so no suppression is announced for it.

## Write seams

`scaffold` · `rename` · `retarget` · `lifecycle` · `migrate --apply` all route through one guarded path.

Write gates compare full before/after rule results and refuse introduced Error findings
with `CONTENT_VIOLATIONS`. Unrelated standing violations do not block writes, but a
baseline lock also holds pre-existing drift. Default-only scaffold advises on findings
owned by its placeholder node; supplied body/fields and findings owned elsewhere are
strict. `migrate` injects config defaults preserving the bare document's inferred values.

Preparation and content-gate refusal happen before document commits. Rename moves the
untouched source, commits prepared destination bytes, then inbound references. A failed
move leaves source content unchanged; a failed destination rewrite after moving reports
`completion: partial` / `failures` and stops inbound rewrites. Batch commits can fail
individually. Batch `completion` is `planned` (dry run), `complete` or `partial`;
inspect warnings/failures even with exit 0 and recompute after a partial result.
Scaffold candidates are comparisons, not duplication or supersession verdicts.

Read these write advisories before continuing (details: `reference/commands.md`):

- `reference_kept`: retarget leaves the successor's references to the predecessor;
  its `supersedes` succession record is exempt.
- `document_evicted`: a terminal conditional-exclusion parent drops sub-artifacts
  in its directory subtree, including parse failures. The files remain untouched;
  the warning discloses scope loss that can otherwise turn check green.
- `file_skipped`: a lock/symlink/read failure held an edit, or rename left a reference
  now naming another document. The latter can leave a valid graph, so check cannot
  substitute for this warning.
- `baseline_inert`: configured locks could not engage; inspect the condition.

Retarget rewrites id relations and id-syntax body references only. Path links still
name the superseded file; `superseded_reference` reports live citations to repoint.
Rename's `id_stability.type` is `already_anchored | unchanged | anchored |
bare_no_frontmatter | anchor_failed`. Bare Markdown gets no invented frontmatter:
a changed inferred id stops pairing identity-scoped locks with its baseline. Add id
or migrate first. `anchor_failed` names the old id the post-move rewrite could not
preserve; inspect the destination and failures before rebuilding or retrying.

Every path a write command accepts (`scaffold --path`, `rename`'s two paths, `check --content`) is refused when spelled differently from the filesystem's own — a case- or normalization-insensitive volume resolves both to one file while every comparison nodex makes is exact. The error names the spelling to use.

## Reading a check

`CheckResult`: `{violations, skipped_rules, rule_coverage, total, has_errors, proposals?, standing?}`.

Branch on violation `details.type` and structured fields, never message text.
`skipped_rules` and `rule_coverage` partition the registry. Each coverage record is
`{rule_id, unit, subjects, unjudged}`: subjects are all guarded records before report
narrowing, not just violations or changed records. Zero subjects means no measured
population; inspect scope and kind filters before trusting a green result.

`unjudged` means selected but not judged. Locks commonly count documents added since
the baseline: gate on standing gaps among untouched documents, not any non-zero count.
Git drift instead counts a node whose every offered edge is unmeasurable, with a
`git_drift_unmeasurable` warning naming targets to fix. Step rules count unknown history
or records no commit can carry. Coverage/history details: `reference/config.md`.

`--content` adds `proposals: [{path, in_scope, has_path_errors}]` and `standing`
(the proposed nodes' warning findings). Top-level `has_errors` gates the whole run;
`violations` is the introduced delta, so standing warnings may cancel from it.

`query issues` shares coverage and includes `unresolved_edges` with typed `cause`
(`missing | target_unparsed | excluded_from_scope | id_not_found | escapes_source |
absolute`), severity and matching policy_name. Read those rather than re-deriving the
project's policy. Payload details: `reference/commands.md`.

## Error codes

Stable across releases; matched via `error.code`, never by message string.

<!-- published:error-codes -->
`IO_ERROR`, `PARSE_ERROR`, `CONFIG_ERROR`, `CYCLE_DETECTED`, `DUPLICATE_ID`, `INVALID_TRANSITION`, `NOT_FOUND`, `GRAPH_MISSING`, `GRAPH_OUTDATED`, `WRITE_CONFLICT`, `ALREADY_EXISTS`, `PATH_ESCAPES_ROOT`, `SYMLINK_TARGET`, `CONTENT_VIOLATIONS`, `VERSION_MISMATCH`, `GIT_ERROR`, `INVALID_ARGUMENT`, `INTERNAL_ERROR`.
<!-- /published:error-codes -->

## Warning codes

Envelope-level, same discipline. The full published set:

<!-- published:warning-codes -->
`scope_coverage` (scope/identity declarations or walk boundaries left coverage gaps) ·
`cache` (cache IO failed; the next run repeats work) ·
`snapshot_divergence` (snapshot drift, or a failed probe: rebuild for drift, fix the
named path for IO) · `build_recommended` (message names a required follow-up) ·
`binary_compat` (outside the project's version pin) ·
`gate_suppression` (displayed findings differ from the judged set; has_errors/exit
still answer for all judgments) · `baseline_inert` (a baseline/project/document
was absent where the ref was asked; inspect what went ungated) ·
`ranking_unscored` (no usable score; trust detail names undeclared inputs) ·
`file_skipped` (held/failed edit or reference now naming another document; see Write seams) ·
`reference_kept` (retarget left a successor's reference standing) ·
`document_evicted` (a write removed other documents from scope, not disk) ·
`history_unread` (step history could not be read; counted unjudged) ·
`threshold_undeclared` (the listing's detection threshold is absent, so its empty
answer measured nothing).
<!-- /published:warning-codes -->

## Workflows

**Before authoring**

```bash
nodex build
nodex query similar --title "<draft>"    # avoid duplicates
nodex scaffold --kind <k> --title "<t>"
nodex build                              # reindex
```

**Before a PR**

```bash
nodex check --severity error             # exit 1 on any error
nodex query issues                       # everything actionable in one call
```

**PR diff gate**

```bash
nodex check --since origin/main          # arms the locks; reports what the diff answers for (reference/commands.md)
nodex diff origin/main HEAD              # structural delta for the review summary
```

**Replacing a doc** — `lifecycle supersede` sets the state, `retarget` moves everyone's forward references:

```bash
nodex lifecycle supersede <old-id> --to <new-id>
nodex retarget <old-id> <new-id>
nodex check          # superseded_reference: the path links left to repoint
```

**Status flow** — where `[statuses.flow]` governs a kind, `status_transition` reds a move it does not name and `status_entry` a record entering past its entry status, judged per commit; `lifecycle` and `scaffold --force` refuse both before writing. Move one with `nodex rename`, never `git mv`.

**Correcting a frozen record** — where its `body_immutable` block declares `append_section` (see `export rules`), append the correction under that heading at the end of the body and gate it with `check --content`; `details.refusal` names what to undo (`reference/config.md`). A decision that changed still takes `lifecycle supersede`.

**Cleanup triage** — no single verb; compose: `query issues` (what's broken) → `check --severity error` (what blocks) → `query trust --bottom N --status active` (what to distrust; terminal docs score near zero by design and would drown the signal) → act with `lifecycle set --status archived`, `retarget`, or `rename`.

**Impact before a refactor**

```bash
nodex query dependents <id> --depth 3 --relations implements,supersedes
nodex impact origin/main HEAD            # what breaks if this merges
```

**External tooling sync** — every export is wrapped in the `{ok,data}` envelope:

```bash
nodex export enums           | jq .data       > tools/lint/enums.json
nodex export schema          | jq .data       > tools/lint/frontmatter.schema.json
nodex export rules           | jq .data.rules > tools/lint/rules.json
nodex export envelope-schema | jq '.data.per_command["query.issues"]' > tools/codegen/query-issues.schema.json
```
