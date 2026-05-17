---
name: nodex
description: Query, validate, and author markdown documents under nodex.toml. JSON-first CLI. Use when the user asks about doc relationships (backlinks, supersession, orphans, stale, neighbours, components, dependents), runs validation, scaffolds / renames / migrates markdown files, computes trust or similarity, diffs the graph between git refs, extracts body annotations, or exports schema / enums / rules for external tooling.
when_to_use: Trigger on backlinks, supersedes, orphan, stale, frontmatter, schema check / validate / lint docs, scaffold / migrate / rename markdown, trust score, low trust, doc similarity, graph diff, export schema / enums / rules, query dependents, query annotations, body-line vocabulary check. Operates only on markdown projects governed by a root `nodex.toml`.
argument-hint: <subcommand> [args]
allowed-tools: Bash(nodex:*)
---

# nodex — markdown document graph CLI

JSON-first. Every command emits one of:

```json
{"ok": true,  "data": {...}, "warnings": [...]}    // warnings omitted when empty; always at envelope level, never inside `data`
{"ok": false, "error": {"code": "CODE", "message": "..."}}
```

List queries put items in `data` as `{"items": [...], "total": N}`. Exit codes: `0` ok, `1` validation errors, `2` runtime error. Global flags: `--pretty` (indented JSON), `-C <dir>` (run against another project root), `--check-version <semver-req>` (refuse to run unless the binary version satisfies the requirement).

**Always run `nodex build` first** for any `query` / `scaffold` / `check` — they read the indexed `_index/graph.json`. Build is incremental and cheap to re-run.

Body links: standard markdown (`[text](path.md)`) by default. Wikilinks (`[[id]]`) opt-in via `parser.wikilink_enabled = true`; arbitrary syntaxes via `parser.link_patterns` regexes. Dot-prefixed paths (`.draft.md`, `.archive/`, `.claude/`) skipped unless `[scope].include_hidden = true`; `node_modules` / `__pycache__` / `target` / `.git` / `.venv` always excluded.

## Build

```bash
nodex build                                       # incremental (default)
nodex build --full                                # bypass cache, fresh parse
```

A single malformed YAML file is surfaced as an envelope warning, not a build-halting error — the rest of the project still indexes.

## Query

All read operations live under `query`.

```bash
nodex query search <kw> [--status x,y]            # id / title / tags
nodex query tags <t...> [--all]                   # any (default) or all
nodex query backlinks <id>                        # nodes that link to <id> — self-edges excluded
nodex query chain <id>                            # supersession chain, oldest → newest
nodex query node <id>                             # full detail + incoming + outgoing (honest; self-edges visible)
nodex query covered-by <path>                     # docs whose `covers:` declares this code path
nodex query orphans                               # zero external incoming, after orphan_grace_days
nodex query stale                                 # active docs past detection.stale_days
nodex query issues                                # orphans + stale + unresolved + violations + skipped_rules
nodex query trust <id>                            # composite [0,1] + always-included per-component breakdown
nodex query low-trust [--threshold N --kind K]    # docs below trust.low_trust_threshold (with components)
nodex query similar --id <id>                     # neighbours of existing doc
nodex query similar --title "<t>" --kind <k>      # probe before scaffolding (kind validated against kinds.allowed)
nodex query recent [--days N --field F --kind K --since YYYY-MM-DD --limit N]
nodex query components                            # connected components, undirected (no policy)
nodex query neighborhood <id> --depth N           # N-hop neighbours, undirected
nodex query dependents <id> [--depth N --relations a,b]   # transitive reverse — every doc that depends on <id>
nodex query annotations [--name <pattern>]        # group `[[annotations]]` markers by capture key
```

`query issues` always carries `skipped_rules: [{rule_id, reason}]` — silent skips are forbidden. `unresolved_edges` entries carry a typed `kind: missing | excluded_from_scope | id_not_found | escapes_source | absolute` so consumers can dispatch on cause.

## Diff

```bash
nodex diff <ref-a> <ref-b>                        # structural delta via `git worktree add --detach`
```

Output: `added_nodes`, `removed_nodes`, `added_edges`, `removed_edges`, `status_transitions: [{id, from, to}]`, `field_changes: [{id, field, before, after}]`. Both refs parsed with the **current** `nodex.toml` — a vocabulary change surfaces as concrete field changes rather than apples-to-oranges diffs.

## Authoring

```bash
nodex scaffold --kind <k> --title "<t>"           # path + id inferred from config
nodex scaffold --kind <k> --title "<t>" --id <explicit-id>
nodex scaffold --kind <k> --title "<t>" --path docs/foo.md
nodex scaffold --kind <k> --title "<t>" --dry-run # preview, no write
nodex scaffold --kind <k> --title "<t>" --force   # overwrite existing file at same path (id collisions still refused)

nodex migrate                                     # plan-only (default)
nodex migrate --apply                             # inject frontmatter into bare md; atomic refuse on id collision

nodex rename <old-path> <new-path>                # move file + rewrite body-link references
```

`scaffold` emits an envelope-level warning when a near-duplicate doc exists. `rename` envelope includes `id_stability: {kind: already_anchored | unchanged | anchored | bare_no_frontmatter}` — when the path change would shift a path-derived id, the previous id is auto-anchored into the moved file's frontmatter so other docs' cross-references stay valid.

## Lifecycle

```bash
nodex lifecycle review    <id>                    # bump `reviewed: <today>` — refuses if existing date is in the future
nodex lifecycle archive   <id>                    # → archived
nodex lifecycle deprecate <id>                    # → deprecated
nodex lifecycle abandon   <id>                    # → abandoned
nodex lifecycle supersede <id> --to <new-id>      # → superseded; pre-checks successor exists + no supersession cycle
```

Terminal statuses (`archived` / `superseded` / `deprecated` / `abandoned`) block further transitions except `review`. Every action that would produce a graph the next `build` would reject is refused before any write.

## Validation

```bash
nodex check                                       # all rules; exit 1 on any error
nodex check --severity error|warning              # filter by severity
nodex check --since <git-ref>                     # restrict to changed nodes; activates diff-aware rules
```

`[schema].mode = "strict"` rejects any frontmatter key that is neither built-in nor declared in `types` / `enums` / `required` / `cross_field`. Catches typos (`relatd:` → fail). Default `lenient`.

`[rules.frontmatter_immutable] fields = [...]` locks declared fields on terminal-status nodes — diff-aware, surfaces violations only under `check --since <ref>`. Without `--since` the rule self-reports as skipped (with reason); silent non-fires are forbidden.

`[[rules.body_line]]` enforces per-line vocabulary conformance — each block declares a regex with named captures, and every match outside a code block must carry capture values from declared enums. One violation per failed (line, capture). Lines that don't match the pattern are silently ignored.

## Export

```bash
nodex export schema                               # JSON Schema (draft 2020-12) for project frontmatter
nodex export enums                                # kinds + statuses + per-field enums
nodex export rules                                # active rules (built-in + config-driven) with scope
```

External lints consume these instead of re-parsing `nodex.toml`. Dependency direction is one-way: nodex emits, downstream reads.

## Report / Init

```bash
nodex report                                      # writes graph.json + GRAPH.md (default = all)
nodex report --format md|json                     # only one
nodex init                                        # writes annotated nodex.toml
```

## Error codes

Stable across releases; matched via `error.code` in the envelope, never by message string.

`IO_ERROR`, `PARSE_ERROR`, `CONFIG_ERROR`, `CYCLE_DETECTED`, `DUPLICATE_ID`, `INVALID_TRANSITION`, `NOT_FOUND`, `ALREADY_EXISTS`, `PATH_ESCAPES_ROOT`, `VERSION_MISMATCH`, `GIT_ERROR`, `INVALID_ARGUMENT`, `INTERNAL_ERROR`.

## Workflows

**Before authoring**

```bash
nodex build
nodex query similar --title "<draft>" --kind <k>  # avoid duplicates
nodex scaffold --kind <k> --title "<t>"
nodex build                                       # reindex
```

**Before a PR**

```bash
nodex build
nodex check --severity error                      # exit 1 on any error
nodex query issues                                # everything actionable in one call
```

**PR diff gate**

```bash
nodex check --since origin/main                   # only PR-touched nodes; activates frontmatter_immutable
nodex diff origin/main HEAD                       # structural delta for review summary
```

**Replacing a doc**

```bash
nodex lifecycle supersede <old-id> --to <new-id>
```

**External tooling sync**

```bash
nodex export enums  > tools/lint/enums.json
nodex export schema > tools/lint/frontmatter.schema.json
nodex export rules  > tools/lint/rules.json
```

**Impact analysis before refactor**

```bash
nodex query dependents <id> --depth 3 --relations implements,supersedes
```

Returns every doc that transitively depends on `<id>` with shortest-path witness chains.

**Body-marker triage**

```bash
nodex query annotations --name promotes    # config-declared `[PROMOTES: <id>]` markers grouped by id
```

Pre-graph identifiers (TODO topics, promotion candidates, open research questions) — markers that intentionally do not resolve to a node.
