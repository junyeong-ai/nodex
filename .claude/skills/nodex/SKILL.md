---
name: nodex
description: >-
  JSON-first CLI for markdown document graphs governed by a root `nodex.toml`. Validates
  frontmatter and body immutability, gates a proposed edit before it is written, queries
  supersession / backlinks / orphans / stale / dependents / annotations, scaffolds / renames /
  migrates / retargets documents through one guarded write path, computes trust and similarity,
  diffs graphs between git refs, analyses merge impact, and exports schema / enums / rules /
  envelope-schema / config / commands for typed codegen. Use for: check or lint docs, schema
  and frontmatter validation, body immutability, `check --since <ref>`, the write-time gate
  `check --content <path>=-`, typed violation `details` for auto-fix; backlinks, supersedes,
  orphans, stale, dependents, annotations, list nodes by kind / status / tag, reverse
  path-to-node lookup, trust score, low trust, doc similarity, graph diff, merge impact, "what
  breaks if I merge this"; scaffold / rename / migrate markdown, retarget references after
  supersession, lifecycle supersede; `nodex status` / stale graph.json; export for codegen,
  typed clients, API drift; `rule_coverage` / "did my rules actually check anything" / inert
  config detection; body-line vocabulary, `schema.require_explicit`, `[search.weights]`
  ranking, per-rule `kinds` filter.
allowed-tools: Bash(nodex *)
metadata:
  version: 0.38.0
---

# nodex — markdown document graph CLI

**The binary is the contract.** Where anything here disagrees with it, the generated manifests decide: `nodex export diagnostics` (error / warning / exit-code vocabularies), `nodex export commands` (every leaf, its positionals and flags), `nodex export envelope-schema` (payload shapes).

## Envelope

Every command (bar clap's `--help` / `help` / `--version`) emits one of:

```json
{"ok": true,  "data": {...}, "warnings": [{"code": "...", "message": "..."}]}
{"ok": false, "error": {"code": "CODE", "message": "..."}}
```

Branch on `error.code` and `warnings[].code`, never on message text. `warnings` is always at envelope level, never inside `data`, and is omitted when empty. **An error envelope carries no `warnings`** — a failing command loses every advisory it had, so anything it must still tell you is in the `error.message`.

Exit codes: `0` ok · `1` `check` found Error-severity violations · `2` every error envelope.

List queries put items in `data` as `{items, total}`. On plain listings (`nodes`, `search`, `backlinks`, `orphans`, `stale`, `components`) `total` counts every match and a `--limit` cap announces itself via `returned`, so a capped response never reads as complete. Selection queries (`trust --top/--bottom`, `similar`, `recent`) select in core, so their `total` is the selection size.

Global flags: `--pretty` (indented JSON) · `-C <dir>` (run against another project root) · `--check-version <semver-req>` (refuse to run unless the binary satisfies it) · `--today YYYY-MM-DD` (evaluate every date-relative rule and query as if today were that date, instead of reading the clock — staleness, orphan grace, recency, trust freshness and the dates written into scaffolded documents all measure from it, so pinning it makes a run reproducible). A project can also pin the binary with `[meta] nodex_version` in `nodex.toml`: reads warn, document-writing commands refuse with `VERSION_MISMATCH`.

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

**Run `nodex build` before any `query`** — queries read the indexed `_index/graph.json`; without one they fail `GRAPH_MISSING` (exit 2). Build is incremental and cheap to re-run. `check` and `scaffold` build their view live and need no prior build.

A snapshot that no longer matches the working tree still serves the query but rides a `snapshot_divergence` warning. Three answers a missed id must be told apart:

- `NOT_FOUND` — the snapshot was verified against the working tree and the id really is not in the project. Correct the id. The message names what the project held: a corpus governing nothing, or one whose every document failed to parse, is not answered by correcting anything.
- `GRAPH_OUTDATED` — the id is absent from a snapshot the tree no longer matches. Run `nodex build` — unless the cause is an in-scope file the walk can list but not *read*: there were no bytes to digest, so the probe can never confirm it and a rebuild will not clear it. Make the file readable.
- `IO_ERROR` — a directory the walk could not enter. A rebuild fails the same way; fix the path.

`nodex status` reports the same probe on demand: `data.state` ∈ `absent | unreadable | schema_mismatch | outdated | current`, with `divergence` when outdated. CI gates on `data.state`; `schema_mismatch` means `nodex build --full`.

Every command that reads the corpus says what it read. A `scope_coverage` warning means part of it went unscanned — a glob matched nothing, or the walk did not cross a boundary — so an empty result is never mistaken for a complete one.

## The write-time gate

```bash
nodex check --content docs/a.md=-                            # proposed bytes from stdin
nodex check --content docs/a.md=- --content docs/b.md=b.md   # batch: one build, cross-proposal refs resolve
```

Validate proposed bytes **before** writing them. `SOURCE` is `-` (stdin) or a file path resolved against the invoking directory, never `-C <dir>`. At most one `SOURCE` may be `-`; a target `PATH` may appear once. Mutually exclusive with `--since`.

Every proposal is overlaid into ONE graph build, so a reference one proposal authors resolves against another in the same batch — a supersede that rewrites N referrers gates as a single atomic edit. The reported set is the **introduced delta**: a violation already present without the proposal never blocks it; one the overlay adds reds the gate at exit 1. So someone else's broken document never blocks your edit.

Caveats:

- `required_field` never fires for engine-derived fields. A proposal missing `id` / `status` (or a stem-derived `title`) still passes because the build infers them. A clean verdict does not certify those keys are spelled out unless `schema.require_explicit` is configured.
- Both builds are read-only, so a write-time check never touches `cache.json`. A path need not exist yet — that is the point. A path inside the project root but outside the scope globs is vacuously clean and the run warns it validated nothing; a path escaping the root is refused with `PATH_ESCAPES_ROOT`.
- The gate reports what a proposal *introduces*; a write seam's immutability verdict is **absolute**. A document that already drifted from its frozen baseline passes the gate and is still refused by the write. Revert the drift or supersede the record.

`--severity` narrows the list, never the verdict: `has_errors` and the exit code answer for every violation checked, so `--severity warning` over a project holding errors still exits 1. Safe in a gate under any filter. A `gate_suppression` warning counts what the **envelope** stops carrying, which is not the same as what the list stops showing — in `--content` mode a filtered-out warning on a proposal path is still in `standing`, so no suppression is announced for it.

## Write seams

`scaffold` · `rename` · `retarget` · `lifecycle` · `migrate --apply` all route through one guarded path.

A seam refuses a mutation that would leave the project failing its own `check` — and refuses **only** that. It builds the project the mutation produces, runs the full rule set, and compares: an Error-severity violation the mutation *introduces* refuses with `CONTENT_VIOLATIONS` naming the rule. Pre-existing violations never block an unrelated write; a finding the project's config makes a warning rides the envelope instead; a rule that cannot fire on a document cannot refuse a write touching it. `rename` decides before `fs::rename`, so a refused move leaves the tree byte-for-byte unchanged.

Default-only `scaffold` is the one exception, and only for its own document: config-derived placeholders are meant to be filled in, so findings about the document being written ride the envelope. Supplying `--body` / `--field` engages the strict gate. Findings about any *other* document always refuse.

What a seam reports that nothing downstream would:

- `reference_kept` — `retarget` skips the successor document, so its own references to `<old-id>` stay: id relation fields and body references alike. The `supersedes` **field** is exempt — on the successor it *is* the succession record, present in every supersede-then-retarget there is — so a flow with nothing else naming `<old-id>` reports `total_updated: 0` and no warning.
- `document_evicted` — reported by the pre-write gate (`check --content`) as well as by the write, so an agent learns the eviction before it commits to the edit. A write put a terminal document in a `[[scope.conditional_exclude]]` parent slot, dropping every `child_glob` match in that parent's **directory subtree** — a live record's sub-artifacts go too, so read the list rather than predicting it. Never refuses; the file is untouched. Watch the parse-failure case: there a write turns a red `check` green, and this warning is the only thing that says so.
- `file_skipped` — two things, and they read differently. Either something stood between the command and an edit it intended (a symlink, a lock, an unreadable path), or `rename` left a reference standing that **now names somebody else**: the move took the rung out from under it, or carried the referring document to where the same spelling means something different. The second is the sharpest warning this tool emits — the write succeeded, nothing was skipped, and the graph it produced is valid, so `check` has nothing to say — unless it ends up naming **nothing**, where the next build reports an unresolved edge instead. Never treat this code as peripheral.
- `baseline_inert` — a configured immutability baseline could not engage, so those locks were never enforced this run.

Two things a write does **not** do, where the result looks like success:

- `retarget` moves id references only — the id-valued relation fields and id-syntax body references. A plain markdown path link (`[text](old.md)`) is path-bound, so it keeps resolving to the now-superseded file and is left alone. There is no unresolved edge to find: the link points at a real document that is simply the wrong one. Repoint those by hand, or `rename` the file when the path itself should change.
- `rename` anchors a path-derived id into the moved file's frontmatter so references stay valid — but a **bare-markdown** document has no frontmatter to anchor into, and `rename` will not invent one for a path operation. Its id therefore changes, and an identity-scoped lock (`body_immutable` / `frontmatter_immutable`) silently stops pairing with its baseline from then on. The envelope says which happened in `id_stability: {type: already_anchored | unchanged | anchored | bare_no_frontmatter}` — read it. Give a bare document an explicit `id:` (or run `nodex migrate --apply`) before renaming it.

The write-plane codes are deliberately separate: `file_skipped` means an edit did not land the way it was meant to, `reference_kept` that no edit was ever going to happen there (nothing to fix), and `document_evicted` that a document the command never mentioned left the project because of it. Conflating them either chases a phantom fix or walks past a real one.

Every path a write command accepts (`scaffold --path`, `rename`'s two paths, `check --content`) is refused when spelled differently from the filesystem's own — on a case-insensitive (APFS, NTFS) or normalization-insensitive (HFS+) volume `docs/REAL/a.md` and `docs/real/a.md` are one file, while every comparison nodex makes is exact, so the folded spelling addresses a document no lookup finds while the write lands on the real one. The error names the spelling to use.

## Reading a check

`CheckResult`: `{violations, skipped_rules, rule_coverage, total, has_errors, proposals?, standing?}`.

Every violation carries a typed `details: {type, ...}` — a stable machine category plus structured params (offending `field`, `expected` set, failing value) — so branch on `details.type` and auto-propose a fix instead of parsing `message`.

`skipped_rules` and `rule_coverage` partition the registry: a rule either declined or ran. **Silent skips and silent vacuous passes are both forbidden.**

`rule_coverage` is `{rule_id, unit, subjects, unjudged}` per rule that ran. `subjects` is the population the rule *guards* — never the offending subset, never the slice that changed. `subjects: 0` says the rule is in effect over nothing whatever the config declares: a `kinds` filter naming a kind no document has, an `acyclic_relations` entry no document uses, a `stale_days` threshold with no `reviewed:` dates anywhere.

`unjudged` is what the scope selected and the rule could not judge. For a diff-aware lock the commonest cause is *added since the baseline*, which costs nothing and moves on every routine PR — so gate on a non-zero standing over documents the run did not touch, never on non-zero alone. `git_drift` reads the other way round: a node lands there when every one of its drift-relation edges went unmeasured (a dangling reference, an absent path, one outside the root), which is a reference to fix rather than a baseline to refresh — and that one is also a Warning violation (`details.type: git_drift_unmeasurable`) naming the node and the targets, because a count is not something you can repair. A rule that judges a unit on the unit alone always reports `0`.

`--content` mode adds `proposals` (one `{path, in_scope, has_path_errors}` per pair, so a clean or out-of-scope proposal is reported as checked) and `standing` (the proposed nodes' warning-severity violations in the proposed state — `violations` is the introduced delta, so pre-existing housekeeping warnings cancel out of it).

`query issues` carries the same `skipped_rules` / `rule_coverage` plus `unresolved_edges`, each with a typed `cause` (`missing | target_unparsed | excluded_from_scope | id_not_found | escapes_source | absolute`), a `severity`, and the `policy_name` the ordered `[[detection.unresolved_policy]]` table assigned — branch on those instead of re-deriving the project's policy.

## Error codes

Stable across releases; matched via `error.code`, never by message string.

<!-- published:error-codes -->
`IO_ERROR`, `PARSE_ERROR`, `CONFIG_ERROR`, `CYCLE_DETECTED`, `DUPLICATE_ID`, `INVALID_TRANSITION`, `NOT_FOUND`, `GRAPH_MISSING`, `GRAPH_OUTDATED`, `ALREADY_EXISTS`, `PATH_ESCAPES_ROOT`, `SYMLINK_TARGET`, `CONTENT_VIOLATIONS`, `VERSION_MISMATCH`, `GIT_ERROR`, `INVALID_ARGUMENT`, `INTERNAL_ERROR`.
<!-- /published:error-codes -->

## Warning codes

Envelope-level, same discipline. The full published set:

<!-- published:warning-codes -->
`scope_coverage` (what was read and what the project governs do not line up — a glob that selected nothing, a document no `identity` rule names, a part of the tree the walk never read, or a `--content` path the scope does not admit) · `cache` (build cache unreadable or unpersistable; the next build re-parses) · `snapshot_divergence` (`graph.json` does not answer for the working tree — it no longer matches, so `nodex build`; or the staleness probe itself failed, where a rebuild fails the same way and the message names the path to fix) · `similar_document` (a scaffold target resembles an existing doc — consider `lifecycle supersede`) · `build_recommended` (a follow-up is needed before the graph is consistent; the message names it) · `binary_compat` (the binary falls outside `[meta] nodex_version`) · `gate_suppression` (the violations on screen are not the set the invocation describes — `--severity` shows one severity of everything judged, an unresolvable `--since` judged the whole project instead of the slice asked for; `has_errors` and the exit code always answer for everything judged, never for what is shown) · `baseline_inert` (a ref had nothing where it was asked: a baseline that could not engage at all, one document it holds no node for, or — in `diff` / `impact` — a path the ref does not record) · `ranking_unscored` (candidates left out of a ranking for carrying no score: no comparable signal with the target, or no positively-weighted trust component) · `file_skipped` (an edit did not land the way it was meant to — either something stood between the command and it, or `rename` left a reference naming a different valid document, which nothing downstream reports, or nothing, which the next build does) · `reference_kept` (a repoint left a reference standing rather than turn it on the document holding it) · `document_evicted` (a write dropped a document from the project without naming it).
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
nodex check --since origin/main          # activates frontmatter_immutable + body_immutable
nodex diff origin/main HEAD              # structural delta for the review summary
```

**Replacing a doc** — `lifecycle supersede` sets the state, `retarget` moves everyone's forward references:

```bash
nodex lifecycle supersede <old-id> --to <new-id>
nodex retarget <old-id> <new-id>
```

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
