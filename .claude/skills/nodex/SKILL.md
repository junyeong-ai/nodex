---
name: nodex
description: >-
  Inspects, validates and edits Markdown document graphs governed by nodex.toml.
  Use for document lookup, relation traversal, proposed-content checks,
  guarded lifecycle or reference changes, and machine-readable CLI exports.
  Use when the project holds a nodex.toml or the user mentions nodex.
allowed-tools: Bash(nodex *)
metadata:
  version: 0.61.0
---

# nodex

The binary is the contract. Discover commands, positionals and flags with `nodex
export commands` (`<command> --help` explains each), payloads with `nodex export
envelope-schema`, and codes with `nodex export diagnostics`. Use project vocabulary
from `nodex export config`, `enums` and `rules` rather than inventing kinds or statuses.

Read the relevant reference before its workflow:

- [Commands and result semantics](reference/commands.md): query filters, proposed
  content, history checks, lifecycle changes and partial writes.
- [Configuration](reference/config.md): scope, schema, relations, locks and status flows.
- [Minimal configuration](reference/minimal-config.toml): a complete starting example;
  `nodex init` generates the annotated starter for the running version.

## Read and validate

```bash
nodex build
nodex query similar --title "<draft title>"
nodex query node <id> --with-body
nodex query issues
nodex check --severity error
```

Build before querying and after edits. Queries read `graph.json` under
`data.output.dir` from `export config` (`_index` by default), verify content and
refuse drift. `query --allow-stale` explicitly permits membership-only snapshot
reads; it does not establish content freshness. Body reads still verify revisions.

A missed lookup yields `NOT_FOUND` only after content verification. Rebuild for
`GRAPH_MISSING` / `GRAPH_OUTDATED`; fix unreadable paths for `IO_ERROR`.
`CONFIG_ERROR` also refuses an argument the configuration rules out (an undeclared
kind, status, field, relation or annotation name, a `--body-lines` range outside
the body, a zero cap, a scaffold with no path); the refusal message lists the
accepted values, so correct the argument rather than `nodex.toml`.
`nodex status` distinguishes `absent`, `unreadable`, `schema_mismatch`, `outdated`
and `current`; rebuild with `--full` for a schema mismatch.
Incompatible snapshot schemas also yield `GRAPH_OUTDATED`, naming the recorded
and supported versions. `--allow-stale` does not bypass schema compatibility.

Before treating an empty answer or green check as complete, inspect
`scope_coverage`, `snapshot.unbuildable`, `skipped_rules` and `rule_coverage`.
Coverage is measured before report narrowing; `unjudged` means selected but not
judged, not necessarily a broken document. Details: [check](reference/commands.md#check).

To find documents by body text, use `nodex query nodes --body-contains "<text>"`;
add `--status` with the project's non-terminal statuses (`export enums`) to leave
retired records out. It lists documents, not lines.

Similarity selects positive title-token, tag and graph-neighbour overlap. Kind,
status and exact parent-directory filters narrow candidates before ranking.
No result proves semantic duplication or its absence. Read candidates before
choosing a lifecycle change; orphan status or an old review date is not a verdict.

## Write and verify

1. Hand edit: pipe the new bytes to `nodex check --content <path>=-`; write the
   file only when `data.has_errors` is false, else fix the proposal and re-run.
2. Tool write: run `scaffold`, `rename` or `retarget` with `--dry-run` first. It
   runs the same gates and writes nothing (`scaffold` reports `written: false`,
   `rename` and `retarget` `completion: planned`); read its warnings
   (`file_skipped`, `reference_kept`, `document_evicted`), then run it without.
3. After any write: `nodex check`, then `nodex build`.

```bash
nodex check --content docs/a.md=- < proposal.md
nodex check --content docs/a.md=- --content docs/b.md=b.md
nodex scaffold --kind <kind> --title "<title>" --path docs/new.md --dry-run
nodex retarget <old-id> <new-id> --supersede --dry-run
nodex check
nodex build
```

Read the [write gate](reference/commands.md#check) and the relevant command before
mutating. `check --content` validates proposed bytes in one combined graph;
`SOURCE` is stdin (`-`) or a file relative to the invoking directory. A clean
verdict does not certify explicit metadata unless the schema requires it.

For reference findings, inspect `details.sources` (`sources` on unresolved listings)
before proposing an edit; protection constrains modification and does not prove
historical target validity. See [source protection](reference/config.md#unresolved-reference-policy).

Use `scaffold`, `rename`, `retarget` and `lifecycle` for guarded graph changes.
Classification globs do not choose creation paths; scaffold needs `--path` or a
`scaffold.paths` template. Scaffold without `--body` / `--field` writes even when
the new document has findings and reports them in `build_recommended` warnings, so
run `nodex check` after it; with `--body` or `--field`, an introduced error refuses
the write (`CONTENT_VIOLATIONS`). `migrate --apply` injects inferred defaults into
bare Markdown.

Writes refuse introduced errors and targeted baseline-lock drift. A record under an
`append_only` lock takes corrections in the section its `append_section` names; a
`frozen` body takes none, and a changed decision takes supersession. Discover lock
and relation policy from `export rules` / `export config`. Declared relations with
`retarget = "keep"` remain historical citations; body links are repointed unless a
lock holds them (`file_skipped`).

Batch writes are not a filesystem transaction. Inspect `completion`, `failures`
and warnings even on nonzero exits; successful earlier writes remain. Policy
holds alone exit 0. After a partial write, inspect the tree and recompute the plan
before retrying. Rename can leave a surviving move with `id_stability: anchor_failed`.
[Write details](reference/commands.md#rename--retarget).

## History and integrations

```bash
nodex check --staged
nodex check --since origin/main
nodex diff origin/main HEAD
nodex query dependents <id> --depth 3 --relations implements,supersedes
nodex impact origin/main HEAD
```

`--staged` judges the index git commits. `--since` narrows findings and selects
history; a configured `rules.immutable_baseline` still supplies the lock baseline.
Read the [history-check semantics](reference/commands.md#check---since) before
using them as a gate. `--content` excludes both flags.

`[meta].nodex_version` warns on incompatible reads and refuses document writes;
dry runs stay reads. `--check-version <requirement>` refuses any command outside it.
`--today YYYY-MM-DD` fixes date-relative judgments and scaffold dates; `-C` selects
the project. Typed clients and drift gates: [export](reference/commands.md#export).

## Result contract

```json
{"ok": true, "data": {}, "warnings": [{"code": "...", "message": "..."}]}
{"ok": false, "error": {"code": "CODE", "message": "..."}}
```

Branch on typed codes and details, never message text. Empty warnings are omitted;
error envelopes have no warnings. Snapshot queries add `snapshot`, and a warning may
carry typed `details`. Help and version output are plain text.
Exit 0 means success, 1 means `check` found error-severity violations (warnings
alone exit 0; `query issues` exits 0 whatever it lists), and 2 means an error,
actual batch write failures, or undelivered output. Read stdout even on exit 1 or 2.

Listings use `{items,total}` and add `returned` when capped; `similar` and `recent`
report `total` as the number selected after `--limit`, never `returned`. `node`, `issues`, `neighborhood` and `dependents` return
objects. `check --severity` filters presentation, never `has_errors` or the exit
code. Snapshot metadata describes verification separately from parsing coverage.

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
nodex query similar             nodex query recent
nodex query components          nodex query neighborhood <id>   nodex query dependents <id>
nodex query annotations
nodex export schema             nodex export enums              nodex export rules
nodex export envelope-schema    nodex export config             nodex export commands
nodex export diagnostics
```

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
`ranking_unscored` (target has no weighted similarity signals) ·
`file_skipped` (held/failed edit or reference now naming another document; see
[rename / retarget](reference/commands.md#rename--retarget)) ·
`reference_kept` (retarget preserved a policy-held or successor reference) ·
`document_evicted` (a write removed other documents from scope, not disk) ·
`history_unread` (step history could not be read; counted unjudged) ·
`threshold_undeclared` (the listing's detection threshold is absent, so its empty
answer measured nothing).
<!-- /published:warning-codes -->
