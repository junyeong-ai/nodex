---
name: nodex
description: Query, validate, and author markdown documents under nodex.toml. JSON-first CLI. Use when the user asks about doc relationships (backlinks, supersession, orphans, stale, neighbours, components, dependents), runs project-wide validation, scaffolds / renames / migrates markdown files, computes trust or similarity, diffs the graph between git refs, extracts body annotations (optionally filtered by `--min-count` and enriched with frontmatter fields), or exports schema / enums / rules / envelope-schema for external tooling and typed codegen.
when_to_use: Trigger on backlinks, supersedes, orphan, stale, frontmatter / body immutability, schema check / validate / lint docs, list nodes by kind/status/tag, reverse path-to-node lookup, scaffold / migrate / rename markdown, trust score, low trust, doc similarity, graph diff, export schema / enums / rules / envelope-schema, codegen / typed client / API drift, query dependents, query annotations (with `--with-frontmatter` and `--min-count`), body-line vocabulary check, `check --since <ref>`, per-rule `kinds` filter. Operates only on markdown projects governed by a root `nodex.toml`.
argument-hint: <subcommand> [args]
allowed-tools: Bash(nodex *)
---

# nodex — markdown document graph CLI

JSON-first. Every command emits one of:

```json
{"ok": true,  "data": {...}, "warnings": [...]}    // warnings omitted when empty; always at envelope level, never inside `data`
{"ok": false, "error": {"code": "CODE", "message": "..."}}
```

List queries put items in `data` as `{"items": [...], "total": N}`. Exit codes: `0` ok, `1` validation errors, `2` runtime error. Global flags: `--pretty` (indented JSON), `-C <dir>` (run against another project root), `--check-version <semver-req>` (refuse to run unless the binary version satisfies the requirement). Projects can also pin the binary via `[meta] nodex_version = "..."` in `nodex.toml` — `Config::load` enforces both gates; `VERSION_MISMATCH` is the error code.

**Always run `nodex build` first** for any `query` / `scaffold` / `check` — they read the indexed `_index/graph.json`. Build is incremental and cheap to re-run.

Body links: standard markdown (`[text](path.md)`) by default. Wikilinks (`[[id]]`) opt-in via `parser.wikilink_enabled = true`; arbitrary syntaxes via `parser.link_patterns` regexes. Dot-prefixed paths (`.draft.md`, `.archive/`, `.claude/`) skipped unless `[scope].include_hidden = true`; `node_modules` / `__pycache__` / `target` / `.git` / `.venv` always excluded.

## Build

```bash
nodex build                                       # incremental (default)
nodex build --full                                # bypass cache, fresh parse
```

`BuildResult` envelope: `{nodes, edges, annotations, body_line_matches, cached, parsed, duration_ms}`. A single malformed YAML file is surfaced as an envelope warning, not a build-halting error — the rest of the project still indexes.

## Query

All read operations live under `query`.

```bash
nodex query search <kw> [--status x,y]            # id / title / tags
nodex query nodes [--kind K1,K2] [--status S1,S2] [--tag T1,T2 --all-tags] [--limit N]  # generic listing: AND across categories, OR within. Empty filter = all nodes in id order. Tag matching is case-insensitive.
nodex query backlinks <id>                        # nodes that link to <id> — self-edges excluded
nodex query chain <id>                            # supersession chain, oldest → newest
nodex query node <id>                             # full detail + incoming + outgoing (honest; self-edges visible)
nodex query node --path <file>                    # reverse lookup: same envelope as <id>, addressed by on-disk path
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
nodex query annotations [--name <pattern>] [--with-frontmatter f1,f2,...] [--min-count N]
                                                  # group `[[annotations]]` markers by capture key
                                                  # --with-frontmatter enriches each source with selected node frontmatter (built-in or project-declared)
                                                  # --min-count N drops entries with count < N; empty groups removed (promotion-candidate / repeated-topic queries)
```

`query issues` always carries `skipped_rules: [{rule_id, reason}]` — silent skips are forbidden. `unresolved_edges` entries carry a typed `kind: missing | excluded_from_scope | id_not_found | escapes_source | absolute` so consumers can dispatch on cause.

## Diff

```bash
nodex diff <ref-a> <ref-b>                        # structural delta via `git worktree add --detach`
```

Output: `added_nodes`, `removed_nodes`, `added_edges`, `removed_edges`, `status_transitions: [{id, from, to}]`, `field_changes: [{id, field, before, after}]`, `added_annotations`, `removed_annotations`. Both refs parsed with the **current** `nodex.toml` — a vocabulary change surfaces as concrete field changes rather than apples-to-oranges diffs.

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

`CheckResult` envelope: `{violations: [...], skipped_rules: [...], total, has_errors}`. Built-in rule_ids: `required_field`, `field_type`, `field_enum`, `cross_field`, `unknown_field` (strict mode only), `stale_review`, `git_drift`, `filename_pattern`, `sequential_numbering`, `unique_numbering`. Config-driven rule_ids: `body_line/<name>`, `body_immutable/<name>`, `frontmatter_immutable/<name>`.

`[schema].mode = "strict"` rejects any frontmatter key that is neither built-in nor declared in `types` / `enums` / `required` / `cross_field`. Catches typos (`relatd:` → fail). Default `lenient`.

### Diff-aware rule families (require `--since`)

`[[rules.frontmatter_immutable]]` — locks declared frontmatter fields on terminal-status nodes. Per-block config:

```toml
[[rules.frontmatter_immutable]]
name = "identity"
fields = ["id", "kind", "superseded_by"]
# Optional kind filter — empty = every kind:
# kinds = ["adr"]
```

Violations carry `rule_id = "frontmatter_immutable/<name>"`. Names must be unique across blocks.

`[[rules.body_immutable]]` — locks document bodies on terminal-status nodes. Two modes:

```toml
[[rules.body_immutable]]
name = "adr-decisions"
mode = "frozen"                          # any body edit → violation
kinds = ["adr"]

[[rules.body_immutable]]
name = "runbook-history"
mode = "append_only"                     # pre-terminal body must remain a prefix of the new body
kinds = ["runbook"]
```

Violations carry `rule_id = "body_immutable/<name>"`. Driven by per-node body fingerprints (SHA-256 of body + per-line vector) computed at build time — no file re-reads at check time. Applies to *simple* whole-body locking; documents with nuanced edit policies (e.g. "only the `## Status` section may mirror frontmatter") should keep that logic in their own tooling.

Both families self-report as `skipped_rules` (with reason) when invoked without `--since`. Silent non-fires are forbidden.

### Vocabulary rule families (always active)

`[[rules.body_line]]` — per-line vocabulary conformance. Each block declares a regex with named captures; every match outside a code block must carry capture values from declared enums. One violation per failed (line, capture). Lines that don't match the pattern are silently ignored. Rule_id `body_line/<name>`.

### Kind filter (every per-block rule family + `[[annotations]]`)

Every per-block rule family and `[[annotations]]` accepts an optional `kinds: ["..."]` list. Empty = no restriction; otherwise the rule fires only on nodes whose `kind` appears in the list. Every entry must be in `kinds.allowed`; `Config::load` rejects typos so a silent never-fire is impossible.

## Export

```bash
nodex export schema                               # JSON Schema (draft 2020-12) for project frontmatter
nodex export enums                                # kinds + statuses + per-field enums
nodex export rules                                # active rules (built-in + config-driven) with params payload
nodex export envelope-schema                      # JSON Schema for every CLI envelope shape — typed-codegen contract
```

External lints consume these instead of re-parsing `nodex.toml`. Dependency direction is one-way: nodex emits, downstream reads. `envelope-schema` runs without `nodex.toml` (project-independent) so it can be invoked anywhere; the `version` field in its output is the SoT for downstream codegen drift gates.

`export rules` `RuleManifestEntry`: `{id, source: builtin|config, severity, description, diff_aware, params}`. `params` carries the rule's configured values (regex, kinds, mode, enums, thresholds, …) — schema is per-rule, kept free-form so adding a new built-in doesn't reshape the manifest.

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
nodex check --since origin/main                   # only PR-touched nodes; activates frontmatter_immutable + body_immutable
nodex diff origin/main HEAD                       # structural delta for review summary
```

**Replacing a doc**

```bash
nodex lifecycle supersede <old-id> --to <new-id>
```

**External tooling sync**

```bash
nodex export enums           > tools/lint/enums.json
nodex export schema          > tools/lint/frontmatter.schema.json
nodex export rules           > tools/lint/rules.json
nodex export envelope-schema > tools/codegen/envelopes.schema.json   # generate typed clients from this
```

**Impact analysis before refactor**

```bash
nodex query dependents <id> --depth 3 --relations implements,supersedes
```

Returns every doc that transitively depends on `<id>` with shortest-path witness chains.

**Body-marker triage**

```bash
nodex query annotations --name promotes                                          # config-declared `[PROMOTES: <id>]` markers grouped by id
nodex query annotations --name promotes --min-count 3                            # only keys repeated ≥3 times (promotion candidates)
nodex query annotations --name promotes --with-frontmatter created,owner,tags    # add per-source frontmatter so consumers skip file re-reads
```

Pre-graph identifiers (TODO topics, promotion candidates, open research questions) — markers that intentionally do not resolve to a node. `--with-frontmatter` accepts any built-in or project-declared field; unknown names are rejected at load with `CONFIG_ERROR`. `--min-count` is the natural primitive for "show me only keys that appear N+ times" without downstream filtering.
