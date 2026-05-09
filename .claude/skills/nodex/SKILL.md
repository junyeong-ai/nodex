---
name: nodex
description: JSON-first CLI for the project's document graph. Build the graph from markdown frontmatter + body links, query it (search, backlinks, chain, orphans, stale, issues, recent, similar, trust, pack), validate against rules, transition lifecycle, scaffold new docs that pass the project's schema, migrate legacy files, rename docs and rewrite cross-references atomically, and append session events for AI long-term memory.
when_to_use: User asks about doc relationships, validation, or authoring under nodex.toml. Specifically — searching docs by keyword/tag, listing what links to / supersedes a doc, finding stale or orphan docs, checking schema before a PR, creating a doc with valid frontmatter, deduplicating before scaffolding, migrating bare files, renaming a doc safely, transitioning lifecycle, computing trust score, building a token-budgeted context pack, or recording session events.
argument-hint: <subcommand> [args]
allowed-tools: Bash(nodex:*)
---

# nodex — document graph CLI

JSON-first. Every command emits one of:

```json
{"ok": true,  "data": {...}, "warnings": [...]}    // warnings omitted when empty; ALWAYS at envelope level — never inside `data`
{"ok": false, "error": {"code": "CODE", "message": "..."}}
```

List queries put items inside `data` as `{"items": [...], "total": N}`. Exit codes: `0` ok, `1` validation errors found, `2` runtime error. Append `--pretty` for indented JSON. Use `-C <dir>` to operate against another project root.

Body-link extraction follows standard markdown only by default — `[text](path.md)`. Wikilinks (`[[id]]`) are off until you set `parser.wikilink_enabled = true`; arbitrary syntaxes go through `parser.link_patterns` regexes.

**Always `nodex build` first** for query / scaffold / similar / trust / recent / pack / continue — they read the indexed graph. `build` is incremental; re-running is cheap (cached files reused).

## Build

```bash
nodex build              # incremental (default)
nodex build --full       # force full rebuild — bypass cache
```

## Query

```bash
nodex query search <kw>                  # match id / title / tags
nodex query search <kw> --status active  # comma-separated status filter
nodex query tags <t1> <t2>               # any tag matches
nodex query tags <t1> <t2> --all         # all tags required
nodex query backlinks <id>               # what links to <id>
nodex query chain <id>                   # supersession chain, oldest → newest
nodex query node <id>                    # full detail + incoming + outgoing edges
nodex query covered-by <path>            # docs whose `covers:` declares this code path
nodex query orphans                      # zero-incoming-edge nodes (respects orphan_grace_days)
nodex query stale                        # active docs past detection.stale_days
nodex query issues                       # orphans + stale + unresolved + rule violations in one call
```

`query node` returns `incoming: [{source, relation}]` and `outgoing: [{target, relation}]` — each end is named honestly.

## Recent

```bash
nodex recent                                     # last 7 days, any of created/updated/reviewed
nodex recent --days 30 --field updated --limit 5
nodex recent --since 2026-01-01 --kind adr
```

## Similarity (vector-free)

```bash
nodex similar --id <existing-id>                                # neighbours of existing doc
nodex similar --title "<draft title>" --kind <k>                # probe before scaffolding
nodex similar --title "<t>" --kind <k> --tags a,b --threshold 0.4 --limit 5
```

Threshold default = `config.similarity.threshold` (0.3). Components: title-token Jaccard, tag overlap, kind match, parent-dir match, graph-neighbour overlap.

## Trust score

```bash
nodex trust <id>                         # composite [0,1] + per-component breakdown
```

Components: `status` (0 if terminal, else 1), `freshness` (linear decay against `detection.stale_days`), `drift` (git commits to referenced docs since reviewed; only when `detection.git_drift_threshold` is set), `backlinks` (log-normalised against the graph's max). `drift` weight is excluded from the denominator when unavailable.

## Pack

```bash
nodex pack <seed-id>                                  # token-budgeted context bundle
nodex pack <seed-id> --depth 2 --token-budget 4000
```

BFS from the seed via supersession + backlinks + outgoing references. Healthy nodes processed before terminal-status ones at each depth. Returns `{seed, total_tokens, included: [{id, depth, reason, tokens, body_excerpt}], excluded}`.

## Authoring

```bash
nodex scaffold --kind <k> --title "<t>"                       # path + id inferred from config
nodex scaffold --kind <k> --title "<t>" --id <explicit-id>    # override the inferred id
nodex scaffold --kind <k> --title "<t>" --path docs/foo.md    # explicit path
nodex scaffold --kind <k> --title "<t>" --dry-run             # preview, no write
nodex scaffold --kind <k> --title "<t>" --force               # overwrite existing file

nodex migrate                                                 # dry run
nodex migrate --apply                                         # inject frontmatter into bare md

nodex rename <old-path> <new-path>                            # move file + rewrite every link
```

`scaffold` warns (envelope-level `warnings`) when a similar doc already exists — supersede instead of forking.

## Lifecycle

```bash
nodex lifecycle review    <id>               # refresh `reviewed` date
nodex lifecycle archive   <id>               # → archived
nodex lifecycle deprecate <id>               # → deprecated
nodex lifecycle abandon   <id>               # → abandoned
nodex lifecycle supersede <id> --to <new-id> # → superseded, successor recorded
```

Terminal statuses (`archived` / `superseded` / `deprecated` / `abandoned`) block further transitions except `review`.

## Validation

```bash
nodex check                                  # all rules, exit 1 if any error
nodex check --severity error                 # errors only
nodex check --severity warning               # warnings only
```

Built-in rules: `required_field`, `field_type`, `field_enum`, `cross_field`, `stale_review`, `git_drift` (opt-in), `filename_pattern`, `sequential_numbering`, `unique_numbering`.

## Report

```bash
nodex report                # writes graph.json + GRAPH.md (default)
nodex report --format md    # only GRAPH.md
nodex report --format json  # only graph.json
```

## Session log (AI memory, opt-in)

Requires `[session] log_kind = "session"` and `session` in `kinds.allowed`. The session directory (`_sessions/` by default) must also be in `scope.include` so `continue` can index sessions back into the graph.

```bash
nodex log "<one-line summary>"                                  # creates a brand-new session
nodex log "<summary>" --session <id> --related a,b --tags x,y   # APPENDS to <id> (must reuse a previous session_id)
nodex continue                                                  # most recent session + pack
nodex continue --since-days 3 --token-budget 4000 --depth 2
```

`log` without `--session` always **creates** a new session (`outcome.kind == "created"`); reusing a session id via `--session` is the only way to **append** (`outcome.kind == "appended"`). On rollover the outcome is `{kind: "rolled_over", from_session_id: "<old>"}`. `continue` returns `null` (in `data`) when no session exists inside the window — bootstrap from scratch.

## Init

```bash
nodex init                  # writes annotated nodex.toml in current dir
```

## Typical workflows

**Before authoring:**
```bash
nodex build
nodex similar --title "<draft>" --kind <k>      # avoid duplicates
nodex scaffold --kind <k> --title "<t>"
nodex build                                     # reindex
```

**Before a PR:**
```bash
nodex build
nodex check --severity error                    # exit 1 on any error
nodex query issues                              # everything actionable in one call
```

**Replacing a doc:**
```bash
nodex lifecycle supersede <old-id> --to <new-id>
```

**Resume work:**
```bash
nodex continue                                  # last session + auto-built context pack
```
