# nodex — command reference

Per-leaf flags and payload semantics. The authoritative grammar is `nodex export commands`; the authoritative payload shapes are `nodex export envelope-schema`. This file carries what neither of those states: what a flag *means* and where a result is easy to misread.

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

A whole-document failure (unparseable YAML, non-mapping frontmatter, an opened-but-unclosed `---` fence) never halts the build — the rest still indexes — but the drop is structural data the next `check` reds via `parse_failure`. A single wrong-typed built-in field (bad date, bad bool, non-string scalar) does **not** drop the document: the node stays, the field reads as absent, and `check` flags it via `field_parse`.

## status

`data.state` ∈ `absent | unreadable | schema_mismatch | outdated | current`.

`outdated` carries `divergence: {config_changed, added_paths, removed_paths, changed_paths}` — content probed against each node's recorded `content_hash`. `config_changed` is keyed on the parse+scan surface (scope, output, parser, identity, `[[annotations]]`, `rules.body_line`, `statuses.initial`) and never on trust / similarity / detection tuning.

`unbuildable_paths` lists the snapshot's recorded parse failures. They are covered by the digest the failed parse consumed, so unchanged broken bytes are never staleness — fix the document, `check` reds it. One exception: a path the build could not read at all had no bytes to digest, so the probe can never confirm it and the state stays `outdated` (naming that path in `changed_paths`) until the file is readable; a rebuild will not clear it.

`snapshot_nodex_version` names the producing binary — a binary upgrade flags existing snapshots `outdated` until one rebuild.

## query

```bash
nodex query search <kw> [--status x,y] [--limit N]
```
id / title / tags, score-then-id ranked.

```bash
nodex query nodes [--kind K1,K2] [--status S1,S2] [--tag T1,T2 --all-tags]
                  [--where F=V ...] [--limit N] [--fields id,title,...]
```
Generic listing: AND across categories, OR within. Empty filter = all nodes in id order. Tag matching is case-insensitive.
`--fields` projects: spine fields (`id`, `title`, `kind`, `status`, `path`) in place; any project-declared field (other built-ins, `attrs` keys) under a nested `attrs` object. An undeclared field is `CONFIG_ERROR`.
`--where field=value` (repeatable) narrows by exact equality over the same vocabulary's scalar fields, `path` included. A collection built-in like `tags` is rejected — use `--tag`.

```bash
nodex query node <id> [--with-body]
nodex query node --path <file>
```
Full detail plus incoming and outgoing edges, honest — self-edges are visible. `--with-body` attaches the body text with canonical line endings, saving a separate file read; body-less docs get `""` and the key is absent when not asked. `--path` is the reverse lookup with the same envelope.

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
Documents whose `covers:` declares this code path — a file or a whole directory; git drift measures either. The declaring document's value is read on the build's own ladder, so `covers: ["./src/a.rs"]` in `docs/x.md` names `docs/src/a.rs`. The `<path>` you pass is a needle with no frame of its own, so `./`, `..` and `\` in it normalise away.

```bash
nodex query orphans [--limit N]        # zero external incoming, after orphan_grace_days
nodex query stale   [--limit N]        # active docs past detection.stale_days
nodex query issues                     # orphans + stale + unresolved + violations + coverage
```
`query issues` resolves `rules.immutable_baseline` exactly as a default `check` does.

```bash
nodex query trust <id>
```
Single-node composite in `[0,1]` plus a per-component breakdown, using per-kind weights when a `[[trust.overrides]]` block matches. `freshness` / `drift` / `backlinks` are **omitted** from the JSON whenever the run did not measure them, and two different absences hide behind that omission.

A component **nothing the document could write would produce** — `stale_days` or `git_drift_threshold` unset, no repository, a terminal document, no covered source, no external incoming edges anywhere — is dropped and the composite renormalises over the rest, never substituting a neutral value.

A component the run **can** measure that the document supplies no input for is different: `freshness` and `drift` both read `reviewed:`, so a live document without one is named in `undeclared` and carries **no composite at all** (`score` omitted). Renormalising there would impute, for the missing component, exactly the score the present ones produced — so withholding `reviewed:` could only raise a rank. A project that does not track an axis sets its weight to `0` (globally or per kind); a zero-weighted component carries no evidence either way, so it neither suppresses a composite nor asks for a declaration. A project that wants every document to declare `reviewed:` lists it in `schema.required` (or a per-kind override) — `check` then names each document that does not, which is the listing the ranking's count does not carry.

`score` is also omitted when no positively-weighted component is present at all. Either way `components` and `undeclared` stay inspectable, and the node is excluded from `--top` / `--bottom` and counted in the `ranking_unscored` warning.

```bash
nodex query trust --bottom N [--kind K] [--status S] [--below S]
nodex query trust --top    N [--kind K] [--status S] [--below S]
```
Ranked listings; each item carries `score` + `components`. `--status active` is the review-queue read — terminal nodes legitimately score near zero and would drown the signal. `--below S` is an opt-in cutoff (strictly below). Mutually exclusive with each other and with the single-node form. A node with no composite is **not in the ranking's domain**: excluded from `items` and `total`, never a bottom-N slot, never satisfying `--below`. The exclusion rides the envelope warnings with a count.

```bash
nodex query similar --id <id> [--limit N] [--min-score S]
nodex query similar --title "<t>" [--kind <k>] [--tags a,b] [--parent-dir <dir>] [--limit N] [--min-score S]
```
`--limit` defaults to `similarity.default_limit`; `--min-score` is an opt-in cutoff (≥ S). The `--title` form probes before scaffolding: `--kind` is optional and validated against `kinds.allowed` when given, `--tags` / `--parent-dir` supply the tag and directory signals for the prospective doc.
Components `title` / `tags` / `kind` / `directory` / `linked` are all conditional and omitted when no signal is available. Set-valued signals (title tokens, tags) are absent only when **both** sides are empty — one side empty against a present set is an honest `0.0`, so an empty `--title` still scores 0.0 against titled candidates. A candidate sharing no comparable signal with the target is excluded from the ranking rather than listed at a fabricated `0.00` (so `--min-score` cannot be gamed by absence) and announced via `ranking_unscored`.

```bash
nodex query recent [--days N --field F --kind K --since YYYY-MM-DD --limit N]
nodex query components [--limit N]              # connected components, undirected, size-desc
nodex query neighborhood <id> [--depth N]       # N-hop (default 1), undirected; --depth 0 rejected
nodex query dependents <id> [--depth N --relations a,b]
```
`dependents` is the transitive reverse — every doc that depends on `<id>`. Entries carry inline `{id, title, kind, status, path}` plus `hops` and a `via` witness chain, so no follow-up `query node` is needed.

```bash
nodex query annotations [--name <block>] [--with-frontmatter f1,f2] [--min-count N]
```
`--name` exact-matches a declared `[[annotations]]` block name (not a glob); an unknown name is `CONFIG_ERROR`. Results group by annotation `name`, then by capture `key`: `items[{name, entries[{key, count, sources}]}]`. `--with-frontmatter` enriches each source with selected node frontmatter (built-in or project-declared; unknown names rejected). `--min-count N` drops entries below the count and removes emptied groups — the natural primitive for promotion candidates and repeated topics.

Annotations are for pre-graph identifiers — TODO topics, promotion candidates, open research questions — markers that intentionally do not resolve to a node. A block declaring `[PROMOTES: <id>]` is queried as `nodex query annotations --name promotes`.

## diff / impact

```bash
nodex diff <ref-a> <ref-b>
```
`{added_nodes, removed_nodes, added_edges, removed_edges, status_transitions: [{id, from, to}], field_changes: [{id, field, before, after}], added_annotations, removed_annotations}`.

Both snapshots are graphed under a **single lens** — the after ref's `nodex.toml` (for `check --since`, the working tree's). The before ref supplies content only.

```bash
nodex impact <ref-a> <ref-b> [--depth N] [--relations implements,supersedes]
```
`{diff, impacted, likely_breaking}`. `diff` is the full `nodex diff` envelope. `impacted: [{id, change: removed|modified, dependents: [...]}]` pairs each changed node with its dependents — a **modified** node's *transitive* dependents in the after graph, a **removed** node's *direct* referrers that still point at it and now dangle (references the same change repointed elsewhere are correctly absent). Each dependent carries inline metadata plus the `via` witness chain, the same shape as `query dependents`.

`likely_breaking` lists removed nodes whose referrers now dangle — the sharpest "this will break" signal. Added nodes and changes that affect nobody are omitted from `impacted`; the full delta stays in `diff`.

## scaffold

```bash
nodex scaffold --kind <k> --title "<t>"                    # id inferred; path inferred only when an
                                                           # identity.kind_rule maps the kind to a dir
nodex scaffold --kind <k> --title "<t>" --id <explicit-id>
nodex scaffold --kind <k> --title "<t>" --path docs/foo.md
nodex scaffold --kind <k> --title "<t>" --dry-run          # preview, no write
nodex scaffold --kind <k> --title "<t>" --force            # overwrite the file at that path
nodex scaffold --kind <k> --title "<t>" --path docs/foo.md \
  --field 'supersedes=[old-id]' --field created=2026-06-12 --body -
```

`--body` reads the same `SOURCE` grammar as `check --content` (`-` = stdin, else a file path). `--field KEY=VALUE` (value is YAML, repeatable) renders after the identity lines and feeds the `cross_field` fixpoint. A key with a canonical source — a dedicated flag, a config derivation, or the structural `path` — is refused as a `--field` key and the error names the exact set.

`--force` still refuses an id collision, and a document frozen at `rules.immutable_baseline` refuses with the lock id. A target the scan would never admit is refused too: a written-then-ignored file is a document the graph can never see.

`scaffold` emits `similar_document` when a near-duplicate exists.

## rename / retarget

```bash
nodex rename <old-path> <new-path>
```
Move plus reference rewrite, one document only — a directory argument is refused; iterate over its files. References are rewritten for an in-scope source only; locked referencing docs are skipped with a warning.

A markdown link destination *spells* a path rather than being one, so `rename` repoints what the destination **names**: `[x](old&#x2e;md)` and `[x](a\(1\).md)` are edges like any other and move with the file. It writes back the spelling the parser reads as the new path — plain where that works (byte-for-byte the author's style), pointy `<…>` or backslash-escaped where the name needs it, so a move to `docs/new one.md` repoints rather than leaving the link behind. A reference no spelling survives (a wikilink whose new target carries `]`) is left untouched and visible, surfacing as an unresolved edge rather than being mangled.

The envelope carries `id_stability: {type: already_anchored | unchanged | anchored | bare_no_frontmatter}`. When the path change would shift a path-derived id, the previous id is auto-anchored into the moved file's frontmatter so cross-references stay valid.

`rename` also names every reference it leaves something to say about, once each. Two kinds: one it had a replacement for and **declined to write** (no spelling the move could accept — it goes on naming what it named, and the next build reports the unresolved edge, which `query issues` counts and `check` never reds — a project whose `[[detection.unresolved_policy]]` calls that cause an error is one whose write gate refuses the move outright, leaving no warning to read), and one it left standing that **now names somebody else** (the move took the rung out from under it, or carried the referring document to where the same spelling means something different). The second is the one nothing downstream would mention: the graph it produces is valid, so only the command that made it can say so.

```bash
nodex retarget <old-id> <new-id>
```
Rewrites every reference to `<old-id>` so it names `<new-id>`: the id-valued frontmatter relation fields (`supersedes` / `implements` / `related` / `superseded_by`) and body id references (`[[wikilinks]]`, custom `link_patterns`). The first three accept a string or an array; `superseded_by` is a single-id scalar, so `superseded_by: [id]` is a `field_parse` error.

Matching is by **exact id** — an id that merely appears in prose is never touched — and the successor document is skipped so nothing in it comes to name itself. Both ids must exist. A reference-unsafe successor id is refused: trim-unstable, or carrying a metacharacter of a syntax nodex writes (`[`, `]`, `|`, `` ` ``, line breaks). A doc locked by `body_immutable`, or by a `frontmatter_immutable` block covering a relation field, is skipped with a warning.

Envelope: `RetargetResult {old_id, new_id, references_updated, total_updated}`. Standard markdown **path** links (`[text](old.md)`) are path-bound, not id references — they keep resolving to the now-superseded file and are not rewritten. Repoint them by hand, or `rename` the file when the path itself should change.

## lifecycle

```bash
nodex lifecycle review    <id>                    # bump `reviewed: <today>`; refuses a future date
nodex lifecycle set       <id> --status <status>  # any value in statuses.allowed for the kind
nodex lifecycle supersede <id> --to <new-id>      # → superseded; pre-checks successor + DAG
```

`supersede` is its own action because it carries a structural payload: a successor plus a supersession-DAG check. Every other transition goes through `set`, whose target is validated against the project's vocabulary at the write seam. `set` refuses a transition that would introduce a check violation — a `cross_field` rule the target status governs while the required field is absent (`superseded` needs `superseded_by`; use `supersede`), and any other rule the project's own `check` would red, including effects on other documents. A violation the document already carried never refuses it.

Terminal statuses block further transitions except `review`; `set` can never un-terminalize a doc. `set` and `supersede` write `updated: <today>`.

## migrate / report / init

```bash
nodex migrate                # plan-only (default)
nodex migrate --apply        # inject frontmatter into bare markdown
nodex report                 # writes graph.json + GRAPH.md (default = all)
nodex report --format md|json
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
nodex export commands            # authoritative CLI grammar
nodex export diagnostics         # error / warning / exit-code vocabularies
```

External lints consume these instead of re-parsing `nodex.toml`. `envelope-schema`, `commands` and `diagnostics` run without a `nodex.toml` so they can be invoked anywhere; the `version` field in their output is the source of truth for downstream drift gates.

`export config` shows post-default resolved values (an omitted `scope.include` reads `["**/*.md"]`) plus the code-level fallbacks `identity.fallback_kind` / `identity.fallback_id_template` and the resolved `initial_status`. Derive artifact paths from `data.output.dir` instead of hardcoding `_index`.

`export commands` entries carry `{path, schema}` plus `modes` / `positionals` only when applicable: `schema` is the `per_command` envelope-schema key, `modes` names flag-selected alternate shapes (`query.trust-list` behind `--bottom` / `--top`).

`export rules` `RuleManifestEntry`: `{id, source: builtin|config, severity, description, diff_aware, params}`. `params` carries the rule's configured values and is deliberately free-form, so adding a built-in does not reshape the manifest.

Every release publishes `nodex-envelope-schema-v<ver>.json` and `nodex-commands-v<ver>.json` as pinnable assets, and release CI fails any envelope shape change lacking the promised version bump.
