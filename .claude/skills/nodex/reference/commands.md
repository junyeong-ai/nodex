# nodex — command reference

Per-leaf flags and payload semantics. The authoritative list of leaves, their positionals and their flags is `nodex export commands` (`--help` explains each flag); the authoritative payload shapes are `nodex export envelope-schema`. This file carries what neither of those states: what a flag *means* and where a result is easy to misread.

## Contents

- [Build](#build) and [snapshot status](#status)
- [Queries](#query): search, node detail, relations, similarity and annotations
- [Check and proposed content](#check)
- [Range checks](#check---since) and [staged checks](#check---staged)
- [Diff and impact](#diff--impact)
- [Scaffold](#scaffold)
- [Rename and retarget](#rename--retarget)
- [Lifecycle](#lifecycle)
- [Migrate, report and init](#migrate--report--init)
- [Exports](#export)

## build

```bash
nodex build                  # incremental (default)
nodex build --full           # bypass cache, fresh parse
```

`BuildResult`: `{nodes, edges, annotations, body_line_matches, cached, parsed, duration_ms}`, plus these when non-empty:

- `conditionally_excluded` — paths a `[[scope.conditional_exclude]]` rule dropped.
- `conditionally_kept` — paths such a rule matched as a derivative and spared, because the same rule also read them as one of its terminal parents. The other half of the pair: what left, and what stayed against expectation.
- `dangling_paths` — paths holding no readable document (a symlink with no target, an entry that is not a file).
- `unfollowed_paths` — directory symlinks not descended because `scope.follow_symlinks` is off.
- `aliased_paths` — `{path, named}` per name the scan holds a document under but does not use. Only `scope.follow_symlinks` produces them and nothing is lost: the document is graphed under `named`.
- `escaping_paths` — ref builds only: paths resolving outside the checkout, which the ref does not record.
- `parse_failures` — `{path, message, content_hash}` per in-scope document that failed to parse and has no node.

Walk diagnostics describe reached entries; subtrees skipped by the [scope policy](config.md#scope) are not inventoried.

A whole-document failure (unparseable YAML, non-mapping frontmatter, a mapping under a non-string key, an opened-but-unclosed `---` fence) never halts the build — the rest still indexes — but the drop is structural data the next `check` reds via `parse_failure`. A single wrong-typed parser-owned field (bad date, bad bool, non-string scalar) does **not** drop the document: the node stays, the field reads as absent, and `check` flags it via `field_parse`.

## status

`data.state` ∈ `absent | unreadable | schema_mismatch | outdated | current`.

`outdated` carries `divergence: {config_changed, added_paths, removed_paths, changed_paths}` — content probed against each node's recorded `content_hash`. `config_changed` is keyed on the parse+scan surface (scope, output, parser, identity, `[[annotations]]`, `rules.body_line`, the status each kind starts at — the resolved global initial and, where a `[[statuses.flow]]` `initial` differs from it, that `initial` with the kinds the flow governs — and `statuses.terminal` where a `conditional_exclude` reads it) and never on similarity / detection tuning.

`unbuildable_paths` lists the snapshot's recorded parse failures. They are covered by the digest the failed parse consumed, so unchanged broken bytes are never staleness — fix the document, `check` reds it. One exception: a path the build could not read at all had no bytes to digest, so the probe can never confirm it and the state stays `outdated` (naming that path in `changed_paths`) until the file is readable; a rebuild will not clear it.

`snapshot_nodex_version` names the producing binary. Its version is part of what `config_changed` is keyed on, so an upgrade flags existing snapshots `outdated` with `config_changed: true` until one rebuild; an incompatible snapshot format reads `schema_mismatch` instead, and a query refuses that snapshot (`GRAPH_OUTDATED`) until `nodex build --full`. The error names the recorded and supported schema versions. `--allow-stale` does not bypass schema compatibility.

## query

Snapshot queries attach envelope `snapshot`: `verification` is `membership`,
`content`, or `unavailable`; `unbuildable` counts recorded parse failures;
`unfollowed` counts in-scope directory boundaries and is omitted if the probe failed.
Optional `divergence` counts measured drift (`config_changed`, `added`, `removed`,
`changed?`); omitted `changed` means content was not measured. Membership verification does not
verify content. Content fidelity does not imply every document parsed successfully.
Detailed unbuildable paths are available through `status` and `check`.

Every query leaf verifies configuration, membership and content by default. `query --allow-stale` opts into membership-only snapshot reads with drift advisories. In the default mode, configuration, membership or
content drift refuses with `GRAPH_OUTDATED`; probe I/O errors remain `IO_ERROR`.
No rebuild is performed. Files may change after the probe.

```bash
nodex query search <kw> [--status x,y] [--limit N]
```
id / title / tags are score-then-id ranked. Document text is a `query nodes`
filter, `--body-contains`.

```bash
nodex query nodes [--kind K1,K2] [--status S1,S2] [--tag T1,T2 --all-tags]
                  [--where F=V ...] [--missing F1,F2] [--body-contains TEXT]
                  [--limit N] [--fields id,title,...]
```
Generic listing: AND across categories, OR within. Empty filter = all nodes in id order. Tag matching is case-insensitive.
`--fields` projects: spine fields (`id`, `title`, `kind`, `status`, `path`) in place; any project-declared field (other built-ins, `attrs` keys) under a nested `attrs` object. An undeclared field is `CONFIG_ERROR`.
`--missing F1,F2` selects documents missing every named field, using schema presence semantics; nonempty collections, objects, `false` and `0` are present.
`--where field=value` (repeatable) narrows by exact equality over the same vocabulary's scalar fields, `path` included. A collection built-in like `tags` is rejected — use `--tag`.
`--body-contains TEXT` keeps the documents whose body (frontmatter excluded) contains TEXT, case-insensitively. It reads each document the other filters select at the revision the snapshot indexed, so a document changed since the build refuses with `GRAPH_OUTDATED`, `--allow-stale` included. It answers which documents; read or search those files for the lines.

```bash
nodex query node <id> [--with-body [--body-lines START:END]]
nodex query node --path <file>
```
`query node --with-body` compares the file's exact content revision with the indexed node before attaching text. `GRAPH_OUTDATED` requires rebuilding; metadata and body are never mixed across revisions.

Authored and resolved metadata plus incoming and outgoing edges; internal content and line fingerprints are excluded. Self-edges are visible. `--body-lines START:END` selects the body between those inclusive file lines — the numbering every reported location uses — and returns `body_range: {start, end, body_start, body_end}`; a range reaching outside the body is refused. `--with-body` attaches the body text with canonical line endings, saving a separate file read; body-less docs get `""` and the key is absent when not asked. `--path` is the reverse lookup with the same envelope.

```bash
nodex query backlinks <id> [--limit N]
```
Nodes that link to `<id>`; self-edges excluded.

```bash
nodex query chain <id>
```
The full supersession lineage — the whole connected component, oldest → newest topological order. Anchor on **any** member, even the current doc; every branch is returned, never collapsed. `supersedes` is a DAG: a linear lineage has one tip (the live doc) as the last entry, while a fork or consolidation can have several. Read "what's current" from the non-terminal `status`, not from position alone.

```bash
nodex query covered-by <path>
```
Documents whose `covers:` names this path or a directory holding it — the reach git drift measures for the same entry. The declaring document's value is read on the build's own ladder, so `covers: ["./src/a.rs"]` in `docs/x.md` names `docs/src/a.rs`. The `<path>` you pass is a needle with no frame of its own, so `./`, `..` and `\` in it normalise away.

```bash
nodex query orphans [--limit N]        # live docs no record names (no incoming edge, no predecessor's superseded_by), past the exemptions
nodex query stale   [--limit N]        # live docs past detection.stale_days (unset: none, with threshold_undeclared)
nodex query issues                     # orphans + stale + unresolved + violations + coverage
```
`orphans` and `stale` also return `subjects` and `unjudged`. Missing review dates are unjudged; use `nodes --missing reviewed` to list them.

`query issues` resolves `rules.immutable_baseline` exactly as a default `check` does.

Both listings are the typed half of a rule: `orphan` and `stale_review` read the same predicates, so `query orphans` and the `orphan` violations are one pass seen twice, and `rule_coverage` answers for the population either way. A finding with a gate record is counted once — through the violation — so `summary.total` counts problems rather than mentions and `by_category` keys it `violation_<rule_id>`. Unresolved warning and error findings follow the same counting. Info rows are reported under their own name outside `total`.

```bash
nodex query similar --id <id> [--kind K] [--parent-dir D] [--status S] [--limit N] [--min-score S]
nodex query similar --title "<t>" [--kind <k>] [--tags a,b] [--parent-dir <dir>] [--status S] [--limit N] [--min-score S]
```
`--kind`, `--status` and `--parent-dir` filter candidates before ranking, for both
forms. Parent directory is an exact project-relative match; `.` selects the project
root. No filter changes the target or removes historical graph evidence.

Title-token, tag and graph-neighbour Jaccard overlaps form the score. Only positive
weighted overlap is returned. `--limit` defaults to `similarity.default_limit`;
`--min-score` optionally narrows it further. `--tags` requires `--title`.
A component absent from the target is omitted uniformly; an absent candidate set
measures zero. A target without weighted signals returns no candidates and warns
with `details.reason: target_without_signals`. These candidates do not establish
semantic duplication.

```bash
nodex query recent [--days N --field F --kind K --since YYYY-MM-DD --limit N]
nodex query components [--max-size N --limit N] # connected components, undirected, size-desc
nodex query neighborhood <id> [--depth N]       # N-hop (default 1), undirected; --depth 0 rejected
nodex query dependents <id> [--depth N --relations a,b]   # default 1 hop
```
`components --max-size N` keeps the components of at most N members, such as the islands beside a corpus's main component, each under the `component_id` it ranks at in the whole partition. `dependents` follows incoming edges back from `<id>`: the documents that reference it directly, or with `--depth N` every document within N hops (a hub's transitive closure over citations can reach most of a corpus). Entries carry inline `{id, title, kind, status, path}` plus `hops` and a `via` witness chain, so no follow-up `query node` is needed.

```bash
nodex query annotations [--name <block>] [--with-frontmatter f1,f2] [--min-count N]
```
`--name` exact-matches a declared `[[annotations]]` block name (not a glob); an unknown name is `CONFIG_ERROR`. Results group by annotation `name`, then by capture `key`: `items[{name, entries[{key, count, sources}]}]`. `--with-frontmatter` enriches each source with selected node frontmatter (built-in or project-declared; unknown names rejected). `--min-count N` drops entries below the count and removes emptied groups — the natural primitive for promotion candidates and repeated topics.

Annotations are for pre-graph identifiers — TODO topics, promotion candidates, open research questions — markers that intentionally do not resolve to a node. The block `detection.superseded_reference_ok_annotation` names is the exception in what it holds: its key is the id of a target a document cites as history, which `superseded_reference` compares without resolving. A block named `promotes` (matching, say, `[PROMOTES: <id>]`) is queried as `nodex query annotations --name promotes`.

## check

```bash
nodex check
nodex check --severity error
nodex check --content docs/a.md=- --content docs/b.md=b.md
```

`CheckResult` contains `violations`, `skipped_rules`, `rule_coverage`, `total` and
`has_errors`. Branch on violation `details.type` and structured fields.
`--severity` filters the displayed list, never the verdict or exit code;
`gate_suppression` discloses findings the envelope stops carrying.

`skipped_rules` and `rule_coverage` partition the registry. Coverage entries are
`{rule_id, unit, subjects, unjudged}`, measured before report narrowing. Zero
subjects means nothing was guarded; inspect scope and kind filters. Unjudged
records lack the evidence that rule needs: a baseline lock commonly counts new
documents, while git drift can lack measurable targets and step rules can lack
history. Read the rule's evidence before deciding whether a gap needs repair.

`--content PATH=SOURCE` validates proposed bytes before writing. SOURCE is `-`
(stdin) or a path relative to the invoking directory, never `-C`. At most one
source is stdin; target paths must be distinct. Every proposal overlays one
combined graph, so cross-proposal references resolve. Excludes `--since` and
`--staged`; no cache is written.

Ordinary rules judge introduced errors; targeted locks judge absolute drift
against the configured baseline, or the working tree without one. Step rules
judge the move the proposal would commit. Unrelated standing errors do not block
a proposal. `proposals` reports `{path, in_scope, has_path_errors}` and `standing`
reports the proposed nodes' warning findings; top-level `has_errors` gates the
whole batch. A warning retained in `standing` is not suppressed by `--severity`.

An in-project path outside scope validates nothing and warns; a path escaping the
root is refused. Inferred id, title, kind and status do not prove those keys were
authored: configure `schema.require_explicit` when explicit declarations matter.
Restore lock drift before retrying a refused edit; replace a changed decision
through supersession. A configured `append_section` permits
corrections under its final body heading without rewriting the frozen prefix.

## check --since

```bash
nodex check --since <ref>
```
Builds the graph at `<ref>`, diffs it against the working tree (the index under `--staged`) under that tree's config, and narrows the report to findings that change answers for. A configured `rules.immutable_baseline` remains the lock baseline; otherwise `<ref>` supplies it. Which findings those are is each rule's to say: by default a finding on a document the diff touched — its own record added, removed or changed, or an edge / annotation it authored moved — with no neighbour expansion; `orphan` also the documents a pointer at which moved — an added or removed edge, or a predecessor's `superseded_by` — so a document stranded by a neighbour's edit is reported, and a standing orphan only when the diff touched its own record; `superseded_reference` also the cited document and every successor in its lineage, its own record or a pointer at it, since a terminal status or a succession declared there is the edit that makes a standing citation stale; `git_drift`, whose reading is git's, keeps a finding when the commits `<ref>..HEAD` added one it counts — on a measured document or a covered code path outside the graph alike; a node-less, project-wide finding (`acyclic_relation`, `parse_failure`, `unique_numbering`, `sequential_numbering`) always; and every `status_transition` / `status_entry` finding, each about a step inside the range, a move the range went on to undo included. A move that keeps the id is a change to the record (`path_changes` on `diff`), so a `filename_pattern` finding a move creates is kept. `rule_coverage` is never narrowed — a rule guards what it guards whatever slice is shown. A `<ref>` git cannot resolve is refused (`GIT_ERROR`); one that does not carry the project — a subdirectory project added after it — widens back to the whole project and says so (`baseline_inert`, `gate_suppression`).

## check --staged

```bash
nodex check --staged
```
Writes the index git is committing as a tree — from a copy, so the index itself is never locked or written — checks it out, and runs `check` over that project under the tree's own `nodex.toml` — the config the commit carries. The baseline and the history are a plain `check`'s; the step rules judge the move from `HEAD` (and every `MERGE_HEAD`) to the index, so a staged move the working tree has since undone is judged and an unstaged one is not. The index is `GIT_INDEX_FILE` when git names one — the hooks of `git commit -a` and `git commit <path>` get the index those commits make; a relative one is read against the work tree's top level, as git reads it — and the working tree's otherwise. `GIT_ERROR` for an index with unmerged entries, one that does not record the project's directory, or a `GIT_INDEX_FILE` naming no file. The index is graphed as a ref is: a staged link resolving outside the repository is not read, and a `scope_coverage` warning names it. Two commits make a step the hook cannot see whole, and both are judged from `HEAD`: `git commit --amend` makes its step from `HEAD^`, and `git merge` runs `pre-merge-commit` before it records the merged head; gate merges in `pre-commit` or with `check --since`. Mutually exclusive with `--content`; combines with `--since`.

## diff / impact

```bash
nodex diff <ref-a> <ref-b>
```
`{added_nodes, removed_nodes, added_edges, removed_edges, status_transitions: [{id, from, to}], field_changes: [{id, field, before, after}], path_changes: [{id, from, to}], added_annotations, removed_annotations}`. `path_changes` is a move that kept the id — nothing authored changed, every path-keyed reading of the document did.

Both snapshots are graphed under a **single lens** — the after ref's `nodex.toml` (for `check --since`, the checked project's: the working tree's, or the index's under `--staged`). The before ref supplies content only.

```bash
nodex impact <ref-a> <ref-b> [--depth N] [--relations implements,supersedes]
```
`{diff, impacted, likely_breaking}`. `diff` is the full `nodex diff` envelope. `impacted: [{id, change: removed|moved|modified, dependents: [...]}]` pairs each changed node with its dependents — a **modified** node's dependents in the after graph up to `--depth` hops (default 1, the documents that reference it directly; a hub's transitive closure over citations can reach most of a corpus), a **removed** node's *direct* referrers that still point at it and now dangle (references the same change repointed elsewhere are correctly absent), and a **moved** node's both: its dependents where it now is, then the direct referrers still pointing at where it was; a document that does both is listed once, under the edge that still binds it, and the stranded references themselves are in `diff.added_edges` as unresolved targets. Each dependent carries inline metadata plus the `via` witness chain, the same shape as `query dependents`.

`likely_breaking` lists the removed nodes whose referrers now dangle and the moved nodes still referenced at their old path — the sharpest "this will break" signal; a move whose references followed it (an id relation, a link `rename` rewrote) is reported and not breaking. Added nodes and changes that affect nobody are omitted from `impacted`; the full delta stays in `diff`.

## scaffold

```bash
nodex scaffold --kind <k> --title "<t>"                    # id inferred; path requires a
                                                           # scaffold.paths template for that kind
nodex scaffold --kind <k> --title "<t>" --id <explicit-id>
nodex scaffold --kind <k> --title "<t>" --path docs/foo.md
nodex scaffold --kind <k> --title "<t>" --dry-run          # preview, no write
nodex scaffold --kind <k> --title "<t>" --force            # overwrite the file at that path
nodex scaffold --kind <k> --title "<t>" --path docs/foo.md \
  --field 'supersedes=[old-id]' --field created=2026-06-12 --body -
```

`--body` reads the same `SOURCE` grammar as `check --content` (`-` = stdin, else a file path). `--field KEY=VALUE` (value is YAML, repeatable) renders after the identity lines and feeds the `cross_field` fixpoint. A key with a canonical source — a dedicated flag, a config derivation, or the structural `path` — is refused as a `--field` key and the error names the exact set.

`--force` preserves the existing identity unless `--id` explicitly replaces it. It still refuses an id collision, and a document frozen at `rules.immutable_baseline` refuses with the lock id. A target the scan would never admit is refused too: a written-then-ignored file is a document the graph can never see.

`scaffold` returns ranked comparison candidates in `data.candidates`, with scores and components. Candidates do not establish duplication or supersession. `written` says whether the file was written (`false` under `--dry-run`).

Fill the placeholders scaffold writes for required fields: an empty built-in string such as `owner: ""` counts as present, so `required` does not flag it.

## rename / retarget

```bash
nodex rename <old-path> <new-path> [--dry-run]
```
`--dry-run` runs every guard and gate the write would and reports the same result with `completion: planned`, writing nothing; `retarget --dry-run` does the same for a repoint.

Move plus reference rewrite, one document only — a directory argument is refused; iterate over its files. References are rewritten for an in-scope source only; locked referencing docs are skipped with a warning.

A markdown link destination *spells* a path rather than being one, so `rename` repoints what the destination **names**: `[x](old&#x2e;md)` and `[x](a\(1\).md)` are edges like any other and move with the file. It writes back the spelling the parser reads as the new path — plain where that works (byte-for-byte the author's style), pointy `<…>` or backslash-escaped where the name needs it, so a move to `docs/new one.md` repoints rather than leaving the link behind. A reference no spelling survives (a wikilink whose new target carries `]`) is left untouched and visible, surfacing as an unresolved edge rather than being mangled.

The envelope carries `id_stability: {type: already_anchored | unchanged | anchored | bare_no_frontmatter | anchor_failed}`. When the path change would shift a path-derived id, the previous id is auto-anchored into the moved file's frontmatter so cross-references stay valid. `unchanged` is the move the id template reads nothing of — under an explicit `{kind}-{stem}` template a document changes directory without changing id, and nothing needs anchoring — so what re-keys a record is the part of the path its own `identity.id_rules` template names, not the path.

The source is moved without editing its content first. Preparation or move failure leaves
its content unchanged. After the move, the prepared destination content is committed
before inbound references. If that commit fails, the moved file remains, inbound
references stay unchanged, and the result reports `completion: partial` and `failures`.
`id_stability: {type: anchor_failed, id}` means the previous id could not be anchored;
inspect the destination before rebuilding or retrying. Each file replacement is guarded;
the move and rewrites are not one filesystem transaction and no rollback is promised.

Bare Markdown receives no invented frontmatter. If its path-derived id changes,
`bare_no_frontmatter` means identity-scoped locks no longer pair it with the old
record. Anchor its id or run `migrate --apply` before moving it when identity must
survive. Write paths require the filesystem's exact case and normalization;
an alternate spelling is refused with the name to use.

`rename` also names every reference it leaves something to say about, once each. Two kinds: one it had a replacement for and **declined to write** (no spelling the move could accept — it goes on naming what it named, and the next build reports the unresolved edge, which `check` and `query issues` report according to configured severity — a project whose `[[detection.unresolved_policy]]` calls that cause an error is one whose write gate refuses the move outright, leaving no warning to read), and one it left standing that **now names somebody else** (the move took the rung out from under it, or carried the referring document to where the same spelling means something different). The second is the one nothing downstream would mention: the graph it produces is valid, so only the command that made it can say so.

```bash
nodex retarget <old-id> <new-id> [--supersede] [--dry-run]
```
Repoints references to `<old-id>` onto `<new-id>`: the id-valued frontmatter relation fields (`supersedes` / `implements` / `related` / `superseded_by`) and project-declared frontmatter relations with `retarget = "repoint"`, and body references (Markdown paths, `[[wikilinks]]`, custom `link_patterns`). Declared fields with `retarget = "keep"` retain their targets and emit `reference_kept`; this policy does not govern body links. The first three accept a string or an array; `superseded_by` is a single-id scalar, so `superseded_by: [id]` is a `field_parse` error.

Matching is by **resolved document identity** — an id that merely appears in prose is never touched — and the successor document is skipped so nothing in it comes to name itself. Both ids must exist. A reference-unsafe successor id is refused: trim-unstable, or carrying a metacharacter of a syntax nodex writes (`[`, `]`, `|`, `` ` ``, line breaks).

A lock names a *part* of a document, so that is what it costs: a `body_immutable` block keeps the body's citations naming the predecessor — a point-in-time record — while the same document's relation fields are repointed, and a `frontmatter_immutable` block covering `superseded_by` keeps that field while the rest of the write lands. The warning names the parts kept back and the rule that froze them. Two cases are held back whole: a document already drifted from its frozen baseline (the finding is not this write's to clear — `nodex check` names the field), and one carrying a finding about the document rather than a part of it.

Envelope: `RetargetResult {old_id, new_id, references_updated, total_updated, completion, failures?}`. Path references are rendered relative to the citing document and checked by resolution before writing. `--supersede` plans the predecessor lifecycle change and reference edits together, so strict citation rules judge the complete proposal. The predecessor transition is a required plan: if it is held or cannot be staged, the operation refuses before writing any document. File commits retain the documented partial-write reporting.

`migrate`, `rename` and `retarget` report `completion: planned | complete | partial`.
`planned` is a dry run: `migrate` without `--apply`, or `rename` / `retarget --dry-run`.
`partial` means a requested edit was held or failed; a no-op may be complete.
Completion answers for the configured, readable corpus; scope warnings still matter.
Actual write failures are listed in `failures` (`path`, `code`, `message`; omitted
when empty), keep the `ok: true` result and exit 2; policy holds alone remain
warnings and exit 0. Read stdout even on nonzero exits: already-written files
remain changed, so inspect the tree and recompute the plan before retrying.

## lifecycle

```bash
nodex lifecycle review    <id>                    # set `reviewed: <today>`; INVALID_TRANSITION if the existing date is later
nodex lifecycle set       <id> --status <status>  # any value in statuses.allowed for the kind
nodex lifecycle supersede <id> --to <new-id>      # → superseded; pre-checks successor + DAG
```

`supersede` is its own action because it carries a structural payload: a successor plus a supersession-DAG check. Every other transition goes through `set`, whose target is validated against the project's vocabulary at the write seam. `set` refuses a transition that would introduce a check violation — a `cross_field` rule the target status governs while the required field is absent (where the project requires `superseded_by` for `superseded`, as the `nodex init` template does, use `supersede`), and any other rule the project's own `check` would red, including effects on other documents, and that refusal is `CONTENT_VIOLATIONS`; a move the status flow does not declare, or one out of a terminal status, is refused before the gate with `INVALID_TRANSITION`. A violation the document already carried never refuses it.

Terminal statuses block further transitions except `review`; `set` can never un-terminalize a doc. `set` and `supersede` write `updated: <today>`, unless the document's kind forbids `updated` (`[[schema.overrides]] forbidden`).

## migrate / report / init

```bash
nodex migrate                # plan-only (default)
nodex migrate --apply        # inject frontmatter into bare markdown
nodex report                 # writes GRAPH.md; build writes the JSON snapshot
nodex init                   # writes an annotated nodex.toml
```

`migrate --apply` refuses atomically on an id collision; per-file skips (symlink, unreadable, frontmatter that appeared between plan and apply) ride the warnings array. `total: 0` is a finished migration **or** a scan that reached nothing — a `scope_coverage` warning rides the second, on both `migrate` and `--apply`.

## export

```bash
nodex export schema              # JSON Schema (draft 2020-12) for project frontmatter
nodex export enums               # kinds + statuses + per-field enums
nodex export rules               # active rules (built-in + config-driven) with params
nodex export envelope-schema     # JSON Schema for every CLI envelope shape
nodex export envelope-schema --inline-refs   # every $ref resolved in place, for $ref-naive generators
nodex export config              # resolved document-locating surface
nodex export commands            # every leaf, its positionals and its flags
nodex export diagnostics         # error / warning / exit-code vocabularies
```

External lints consume these instead of re-parsing `nodex.toml`. Generate typed clients from `export envelope-schema` and regenerate them on every upgrade. `envelope-schema`, `commands` and `diagnostics` read no `nodex.toml`, not even a broken one, so they can be invoked anywhere; the `version` field in their output is the source of truth for downstream drift gates.

`export config` shows post-default resolved values (an omitted `scope.include` reads `["**/*.md"]`) plus the code-level fallbacks `identity.fallback_kind` / `identity.fallback_id_template` and the resolved global `initial_status` (a kind a `[[statuses.flow]]` governs starts at the flow's `initial`). Derive artifact paths from `data.output.dir` instead of hardcoding `_index`.

`export commands` carries `global_flags`, accepted by every leaf, and one entry per leaf with `{path, schema}` plus `positionals` and `flags` when it has any: `schema` is the `per_command` envelope-schema key, and each flag states its `long` / `short` spelling, `takes_value`, `repeatable` and `required`. A group's flag (`query --allow-stale`) is listed on each of its leaves.

`export rules` `RuleManifestEntry`: `{id, source: builtin|config, severity, description, diff_aware, judges_steps, params}`. `diff_aware` needs a baseline diff to fire and declines into `skipped_rules` without one; `judges_steps` reads git's history a commit at a time. A lock can require both. `params` carries the rule's configured values and is deliberately free-form, so adding a built-in does not reshape the manifest.

Every release publishes `nodex-envelope-schema-v<ver>.json` and `nodex-commands-v<ver>.json` as pinnable assets, and release CI fails any envelope shape change lacking the promised version bump.
