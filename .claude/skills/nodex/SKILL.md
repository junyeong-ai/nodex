---
name: nodex
description: JSON-first CLI for markdown document graphs governed by a root `nodex.toml`. Query supersession / backlinks / orphans / stale, validate frontmatter and body immutability (project-wide or diff-aware via `--since`), scaffold / rename / migrate documents, compute trust and similarity, diff graphs between git refs, analyse merge impact (what breaks if I merge this), repoint id references with retarget, extract `[[annotations]]` body markers with `--min-count` and `--with-frontmatter` enrichment, and export schema / enums / rules / envelope-schema for typed codegen.
when_to_use: backlinks, supersedes, orphan, stale, frontmatter / body immutability, schema check / lint docs, list nodes by kind / status / tag, reverse path-to-node lookup, scaffold / migrate / rename markdown, impact analysis / what breaks if I merge, retarget / repoint references after supersession, trust score, low trust, doc similarity, graph diff, export schema / enums / rules / envelope-schema, codegen / typed client / API drift, query dependents, query annotations, body-line vocabulary check, `check --since <ref>`, write-time validation / `check <path> --content -` / gate a proposed edit before writing, per-rule `kinds` filter
argument-hint: <subcommand> [args]
allowed-tools: Bash(nodex *)
---

# nodex — markdown document graph CLI

JSON-first. Every command emits one of:

```json
{"ok": true,  "data": {...}, "warnings": [...]}    // warnings omitted when empty; always at envelope level, never inside `data`
{"ok": false, "error": {"code": "CODE", "message": "..."}}
```

List queries put items in `data` as `{"items": [...], "total": N}`. On plain listings (`nodes`, `search`, `backlinks`, `orphans`, `stale`, `components`) `total` counts every match and a `--limit` cap announces itself via `returned` (omitted otherwise), so a capped response never reads as complete; selection queries (`trust --top/--bottom`, `similar`, `recent`) select in core, so their `total` is the selection size itself. Exit codes: `0` ok, `1` `check` found Error-severity violations, `2` every error envelope (config, parse, IO, version, CLI-arg, runtime). Global flags: `--pretty` (indented JSON), `-C <dir>` (run against another project root), `--check-version <semver-req>` (refuse to run unless the binary version satisfies the requirement). Projects can also pin the binary via `[meta] nodex_version = "..."` in `nodex.toml` — reads warn, document-writing commands refuse with `VERSION_MISMATCH`; only the `--check-version` flag hard-gates every command.

**Always run `nodex build` first** for any `query` / `scaffold` / `check` — they read the indexed `_index/graph.json`. Build is incremental and cheap to re-run.

Body links: standard markdown (`[text](path.md)`) by default. Wikilinks (`[[id]]`) opt-in via `parser.wikilink_enabled = true`; arbitrary syntaxes via `parser.link_patterns` (each block needs a `pattern` with exactly one capture group **and** a `relation`). Dot-prefixed paths (`.draft.md`, `.archive/`, `.claude/`) skipped unless an include pattern literally names the dotted segment (e.g. `.claude/**/*.md`); `node_modules` / `__pycache__` / `target` / `.git` / `.venv` always excluded.

## Build

```bash
nodex build                                       # incremental (default)
nodex build --full                                # bypass cache, fresh parse
```

`BuildResult` envelope: `{nodes, edges, annotations, body_line_matches, cached, parsed, duration_ms}`. A single malformed YAML file is surfaced as an envelope warning, not a build-halting error — the rest of the project still indexes.

## Query

All read operations live under `query`.

```bash
nodex query search <kw> [--status x,y] [--limit N]  # id / title / tags, score-then-id ranked
nodex query nodes [--kind K1,K2] [--status S1,S2] [--tag T1,T2 --all-tags] [--limit N] [--fields id,title,...]
                                                  # generic listing: AND across categories, OR within. Empty filter = all nodes in id order.
                                                  # `--fields` keeps only the named item fields (token economy; vocabulary: id,title,kind,status,path).
                                                  # Tag matching is case-insensitive.
nodex query backlinks <id> [--limit N]            # nodes that link to <id> — self-edges excluded
nodex query chain <id>                            # supersession chain, oldest → newest
nodex query node <id> [--with-body]               # full detail + incoming + outgoing (honest; self-edges visible).
                                                  # `--with-body` attaches the body text (canonical line endings) — saves a separate file read;
                                                  # body-less docs get `""`, the key is absent when not asked.
nodex query node --path <file>                    # reverse lookup: same envelope as <id>, addressed by on-disk path
nodex query covered-by <path>                     # docs whose `covers:` declares this code path
nodex query orphans [--limit N]                   # zero external incoming, after orphan_grace_days
nodex query stale [--limit N]                     # active docs past detection.stale_days
nodex query issues                                # orphans + stale + unresolved + violations + skipped_rules
nodex query trust <id>                            # single-node composite [0,1] + per-component breakdown; uses per-kind weights when `[[trust.overrides]]` matches.
                                                  # `freshness` / `drift` / `backlinks` are omitted from the JSON when their source signal is absent
                                                  # (no `reviewed:` date / `git_drift_threshold` unset / no external incoming edges anywhere). Composite
                                                  # renormalises over the present components — absent signals are dropped, not replaced with a neutral value.
nodex query trust --bottom N [--kind K] [--below S]   # ranked listing: N lowest-trust nodes (asc). `--kind` narrows; `--below S` is an opt-in cutoff
                                                      # (keep entries strictly below S). Mutually exclusive with `--top` and the single-node `<id>` form.
nodex query trust --top N    [--kind K] [--below S]   # ranked listing: N highest-trust nodes (desc). Same opt-in filters as `--bottom`.
nodex query similar --id <id> [--limit N] [--min-score S]      # neighbours of existing doc; `--limit` caps (default `similarity.default_limit`),
                                                                # `--min-score S` is an opt-in cutoff (keep candidates scoring ≥ S).
nodex query similar --title "<t>" --kind <k> [--limit N] [--min-score S]
                                                  # probe before scaffolding (kind validated against kinds.allowed).
                                                  # Components `title` / `tags` / `kind` / `directory` / `linked` are all conditional — each is omitted when
                                                  # no signal is available (empty token / tag sets, pre-creation spec without kind / parent_dir, no graph id
                                                  # for `linked`). Composite renormalises over the present components.
nodex query recent [--days N --field F --kind K --since YYYY-MM-DD --limit N]
nodex query components [--limit N]                # connected components, undirected (no policy), size-desc
nodex query neighborhood <id> --depth N           # N-hop neighbours, undirected
nodex query dependents <id> [--depth N --relations a,b]   # transitive reverse — every doc that depends on <id>;
                                                  # entries carry inline {id,title,kind,status,path} + hops + via witness chain (no follow-up `query node` needed)
nodex query annotations [--name <pattern>] [--with-frontmatter f1,f2,...] [--min-count N]
                                                  # group `[[annotations]]` markers by capture key
                                                  # --with-frontmatter enriches each source with selected node frontmatter (built-in or project-declared)
                                                  # --min-count N drops entries with count < N; empty groups removed (promotion-candidate / repeated-topic queries)
```

`query issues` always carries `skipped_rules: [{rule_id, reason}]` — silent skips are forbidden. `unresolved_edges` entries carry a typed `cause: missing | excluded_from_scope | id_not_found | escapes_source | absolute` so consumers can dispatch on it.

## Diff

```bash
nodex diff <ref-a> <ref-b>                        # structural delta; single lens = the after ref's config (refs supply content only)
```

Output: `added_nodes`, `removed_nodes`, `added_edges`, `removed_edges`, `status_transitions: [{id, from, to}]`, `field_changes: [{id, field, before, after}]`, `added_annotations`, `removed_annotations`. Both refs parsed with the **current** `nodex.toml` — a vocabulary change surfaces as concrete field changes rather than apples-to-oranges diffs.

## Impact

```bash
nodex impact <ref-a> <ref-b>                      # "what breaks if I merge this?" — diff + dependents (modified: transitive / removed: direct dangling referrers)
nodex impact <ref-a> <ref-b> --depth N --relations implements,supersedes
```

Output: `{diff, impacted, likely_breaking}`. `diff` is the full `nodex diff` envelope; `impacted: [{id, change: removed|modified, dependents: [{id, title, kind, status, path, hops, via}]}]` pairs each changed node with its dependents — a **modified** node's *transitive* dependents in the after graph, a **removed** node's *direct* referrers that still point at it and now dangle (references the same change repointed elsewhere are correctly absent). Each dependent carries inline node metadata plus the witness chain in `via` — same shape as `query dependents`. `likely_breaking: [id, …]` lists removed nodes whose referrers now dangle — the sharpest "this will break" signal. Added nodes and changes that affect nobody are omitted from `impacted` (the full delta stays in `diff`). `--depth` bounds the dependency walk, `--relations` restricts which edges it follows (validated against the project vocabulary). One call answers "is this safe to merge?".

## Authoring

```bash
nodex scaffold --kind <k> --title "<t>"           # id inferred; path inferred only when an identity.kind_rule maps the kind to a dir, else pass --path
nodex scaffold --kind <k> --title "<t>" --id <explicit-id>
nodex scaffold --kind <k> --title "<t>" --path docs/foo.md
nodex scaffold --kind <k> --title "<t>" --dry-run # preview, no write
nodex scaffold --kind <k> --title "<t>" --force   # overwrite existing file at same path (id collisions still refused)

nodex migrate                                     # plan-only (default)
nodex migrate --apply                             # inject frontmatter into bare md; atomic refuse on id collision

nodex rename <old-path> <new-path>                # move file + rewrite body-link references
nodex retarget <old-id> <new-id>                  # repoint references from one id to another (e.g. after supersession)
```

`scaffold` emits an envelope-level warning when a near-duplicate doc exists. `rename` envelope includes `id_stability: {kind: already_anchored | unchanged | anchored | bare_no_frontmatter}` — when the path change would shift a path-derived id, the previous id is auto-anchored into the moved file's frontmatter so other docs' cross-references stay valid.

`retarget` rewrites every reference to `<old-id>` so it names `<new-id>`: the id-valued frontmatter relation fields (`supersedes` / `implements` / `related` / `superseded_by`) and body id references (`[[wikilinks]]`, custom `link_patterns`). Matching is by **exact id** — an id that merely appears in prose is never touched — and the successor document (`<new-id>`) is skipped so its own `supersedes: [<old-id>]` never becomes a self-edge. Both ids must exist. Envelope: `RetargetResult {old_id, new_id, references_updated, total_updated}`. Pairs with `lifecycle supersede`: supersede sets the lifecycle state, retarget moves everyone's forward references onto the successor.

## Lifecycle

```bash
nodex lifecycle review    <id>                    # bump `reviewed: <today>` — refuses if existing date is in the future
nodex lifecycle set       <id> --status <status>  # → <status> (any value in statuses.allowed for the kind); writes `updated: <today>`
nodex lifecycle supersede <id> --to <new-id>      # → superseded; pre-checks successor exists + no supersession cycle
```

`supersede` is its own action because it carries a structural payload (successor + supersession-DAG check); every other status transition goes through `set`, whose target is validated against the project's vocabulary at the write seam. `set` refuses a status a `cross_field` rule governs while the required field is absent (e.g. `superseded` needs `superseded_by` — use `supersede`), so it never writes a doc `check` would reject. Terminal statuses block further transitions except `review`; `set` can never un-terminalize a doc.

## Validation

```bash
nodex check                                       # all rules; exit 1 on any error
nodex check --severity error|warning              # filter by severity
nodex check --since <git-ref>                     # restrict to changed nodes; activates diff-aware rules
nodex check <path> --content -                    # validate PROPOSED bytes (stdin) before writing <path>
nodex check <path> --content FILE                 # …or from a file
```

`check <path> --content <source>` is the write-time gate: it overlays the proposed bytes onto the working tree, diffs against the current on-disk state, and runs every rule (schema, cross-field, immutability) scoped to the nodes the proposal changes (the same touched-node set `--since` narrows to) plus project-wide findings — so an agent validates an edit through nodex's own engine before the write lands, instead of reimplementing the rules. The path need not exist yet; an out-of-scope path is vacuously clean; both builds are read-only (no `cache.json` write). Mutually exclusive with `--since`.

`CheckResult` envelope: `{violations: [...], skipped_rules: [...], total, has_errors}`. Built-in rule_ids: `required_field`, `field_type`, `field_enum`, `cross_field`, `unknown_field` (strict mode only), `stale_review`, `git_drift`, `filename_pattern`, `sequential_numbering`, `unique_numbering`, `graph_invariants/cycle-detection` (always on; relation set is config-driven via `rules.acyclic_relations`, default `["implements"]`). Config-driven rule_ids: `body_line/<name>`, `body_immutable/<name>`, `frontmatter_immutable/<name>`.

`[schema].mode = "strict"` rejects any frontmatter key that is neither built-in nor declared in `types` / `enums` / `required` / `cross_field`. Catches typos (`relatd:` → fail). Default `lenient`.

`[[schema.cross_field]]` predicates support four forms: `when = "field=value"` (equality), `when = "field in {v1,v2,v3}"` (membership), `when = "field exists"` (presence), `when = "field not_exists"` (absence). Scalar predicates (`=`, `in`) are rejected on collection fields (`tags`, `covers`, …) at load; use `exists`/`not_exists` for collection presence.

### Diff-aware rule families (require `--since`)

`[[rules.frontmatter_immutable]]` — freezes declared fields once a doc is ALREADY terminal (gated on the diff's *before* status, so the write that first makes a doc terminal is allowed; only later edits lock). Per-block config:

```toml
[[rules.frontmatter_immutable]]
name = "identity"
fields = ["kind", "superseded_by"]
# Optional kind filter — empty = every kind:
# kinds = ["adr"]
```

`id` is rejected at load (structurally immutable — a changed id is a different node); `status` is accepted and enforced via the status-transition stream. Violations carry `rule_id = "frontmatter_immutable/<name>"`; names must be unique across blocks.

`[[rules.body_immutable]]` — body locks. Two modes × two triggers:

```toml
[[rules.body_immutable]]
name = "adr-decisions"
mode = "frozen"                          # any body edit → violation
trigger = "creation"                     # locked from the first committed snapshot, status notwithstanding
kinds = ["adr"]

[[rules.body_immutable]]
name = "runbook-history"
mode = "append_only"                     # locked body must remain a prefix of the new body
kinds = ["runbook"]                      # trigger omitted = "terminal" (locks once status is terminal)
```

`trigger = "terminal"` (default) uses the same already-terminal boundary as `frontmatter_immutable`; `trigger = "creation"` freezes the body as soon as a prior committed snapshot exists — the creating commit is structurally exempt, and frontmatter (including `status`) stays editable for supersession. Violations carry `rule_id = "body_immutable/<name>"`. Driven by per-node body fingerprints (SHA-256 of body + per-line vector) computed at build time — no file re-reads at check time.

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
