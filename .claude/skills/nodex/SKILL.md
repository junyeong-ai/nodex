---
name: nodex
description: Query, validate, and author the project's document graph via the nodex CLI. Every command emits JSON. Use for keyword search, backlink / supersession / orphan / stale traversal, schema validation, lifecycle transitions, scaffolding new docs with valid frontmatter, injecting frontmatter into legacy files, and renames that keep cross-references intact.
when_to_use: Searching docs by keyword or tag, exploring how documents relate, checking validation before a PR, creating a new doc that must pass the project's schema, updating a legacy doc's frontmatter, renaming a doc without breaking links, or transitioning lifecycle (supersede / archive / deprecate / abandon / review).
argument-hint: <subcommand> [args...]
allowed-tools: Bash(nodex *)
---

# nodex — document graph CLI

`nodex` is a JSON-first tool. Every invocation returns one of:

```json
{"ok": true, "data": {...}, "warnings": [...]}   // warnings omitted when empty
{"ok": false, "error": {"code": "CODE", "message": "..."}}
```

Query commands return `{items: [...], total: N}` inside `data`. Exit codes: `0` success, `1` validation errors found, `2` runtime error. Add `--pretty` for human-readable indent. Scope a command to a different project with `-C <dir>`.

Always run `nodex build` before any query-only command — queries read `_index/graph.json`, which `build` produces. `build` is incremental; re-running is cheap.

## Build

```bash
nodex build              # incremental: reparses only changed files
nodex build --full       # force full rebuild (bypass cache)
```

## Search and traversal

```bash
nodex query search <keyword>                 # id / title / tags
nodex query search <keyword> --status active # narrow by status (comma-separated)
nodex query tags <tag1> <tag2>               # any tag matches
nodex query tags <tag1> <tag2> --all         # all tags required
nodex query backlinks <id>                   # nodes that link to <id>
nodex query chain <id>                       # supersession chain, oldest → newest
nodex query node <id>                        # full detail + incoming/outgoing edges
```

## Detection

```bash
nodex query orphans                          # zero-incoming-edge nodes
nodex query stale                            # active docs past review threshold
nodex query issues                           # unified: orphans + stale + unresolved edges + rule violations
```

## Validation

```bash
nodex check                                  # all rules
nodex check --severity error                 # errors only (exit 1 if any)
nodex check --severity warning               # warnings only
```

## Lifecycle

```bash
nodex lifecycle review    <id>               # refresh reviewed date
nodex lifecycle archive   <id>               # → status: archived
nodex lifecycle deprecate <id>               # → status: deprecated
nodex lifecycle abandon   <id>               # → status: abandoned
nodex lifecycle supersede <id> --to <new-id> # → status: superseded, successor recorded
```

Terminal statuses (`archived`, `superseded`, `deprecated`, `abandoned`) block further transitions except `review`.

## Authoring

```bash
# Create a new doc with every required field populated from config.
nodex scaffold --kind <kind> --title "<title>"
nodex scaffold --kind <kind> --title "<title>" --path docs/foo.md   # override inferred path
nodex scaffold --kind <kind> --title "<title>" --dry-run            # preview frontmatter
nodex scaffold --kind <kind> --title "<title>" --force              # overwrite existing file

# Inject frontmatter into bare markdown files under `scope.include`.
nodex migrate                # dry run: report what would change
nodex migrate --apply        # write the files

# Move a file and rewrite every markdown link pointing at it.
nodex rename <old-path> <new-path>
```

`scaffold` reads `_index/graph.json` for id-collision detection, so run `build` first. `migrate` walks the scoped scan tree and skips any file that already has frontmatter.

## Report

```bash
nodex report                   # writes graph.json + backlinks.json + GRAPH.md (default)
nodex report --format md       # only GRAPH.md
nodex report --format json     # only graph.json + backlinks.json
```

## Typical workflows

**Before authoring a new doc:**
```bash
nodex query search <topic>     # does it already exist?
nodex build                    # ensure graph is fresh
nodex scaffold --kind <k> --title "<t>"
nodex build                    # reindex with the new doc
```

**Before a PR:**
```bash
nodex build
nodex check --severity error   # exit 1 on any error
nodex query issues             # surface every actionable problem in one call
```

**When replacing a doc:**
```bash
nodex lifecycle supersede <old-id> --to <new-id>
```

## Init

```bash
nodex init                     # writes an annotated nodex.toml
```
