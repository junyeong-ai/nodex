[![Rust](https://img.shields.io/badge/rust-1.95.0-orange?logo=rust)](https://www.rust-lang.org)
[![Edition](https://img.shields.io/badge/edition-2024-blue)](https://doc.rust-lang.org/edition-guide/rust-2024/)
[![License](https://img.shields.io/badge/license-MIT-green)](LICENSE)

# nodex

> **English** | **[한국어](README.ko.md)**

**Turn markdown files into a queryable, validated document graph.**

nodex scans your project's markdown files, extracts YAML frontmatter and link relationships, and builds an immutable document graph you can search, validate, diff, and report on — all through a JSON-first CLI. No agents, no servers, no AI dependencies. Just a Rust binary with a stable JSON contract.

---

## Table of Contents

1. [The Problem](#the-problem)
2. [Quick Start](#quick-start)
3. [Core Concepts](#core-concepts) — files become a graph, edge types, frontmatter schema
4. [How It Works](#how-it-works) — build pipeline, incremental cache, query algorithms
5. [JSON-First CLI](#json-first-cli) — envelope, error codes, exit codes, command reference
6. [Validation & Lifecycle](#validation--lifecycle) — built-in rules, strict mode, diff-aware rules
7. [Diff & Export](#diff--export) — structural delta, JSON Schema, enum manifests
8. [Configuration](#configuration) — every `nodex.toml` section explained
9. [Architecture](#architecture) — workspace, modules, design invariants
10. [Install](#install)
11. [License](#license)

---

## The Problem

A project's documentation is rarely a flat pile of files — it's a graph. ADR-0002 supersedes ADR-0001. A runbook depends on a guide. A spec is implemented by three rules. But that graph lives implicitly in `[link text](paths.md)` and frontmatter fields, invisible to `grep` and `find`.

This makes routine questions hard to answer:

| Question | What `grep` does | What you actually need |
|---|---|---|
| "What replaced this ADR?" | nothing — supersession isn't text | Walk `superseded_by` forward |
| "What depends on this doc?" | finds files mentioning its name, misses `related:` frontmatter | All incoming edges, regardless of source |
| "Which docs are isolated?" | nothing — absence isn't searchable | Nodes with zero incoming edges |
| "Which docs are stale?" | nothing — dates aren't compared | Active docs past review threshold |
| "What changed between these refs?" | line diff at best | Added / removed nodes, status transitions, field changes |
| "Find auth docs" | every file containing "auth" | Score by id/title/tag, with relationship context |

nodex makes the implicit graph explicit. It parses your markdown once, builds a typed in-memory graph with adjacency indices, and answers structural questions in sub-millisecond time. Routine workflows — pre-commit validation, PR diff gating, deduplication before authoring, vocabulary sync with external tooling — collapse into single JSON-emitting commands.

**Core properties:**

- **Graph, not folders** — supersession chains, backlinks, cross-references are first-class
- **Config, not code** — all project-specific rules live in `nodex.toml`; zero hardcoded domain logic
- **Incremental & parallel** — Rust + rayon parallel reads, SHA256 per-file cache invalidates only what changed
- **JSON-first contract** — every command emits a stable envelope (`{ok, data, warnings}` / `{ok, error: {code, message}}`) with classified error codes
- **Pure CLI** — no daemons, no servers, no AI / network dependencies; everything is a synchronous local process

---

## Quick Start

```bash
# Install (macOS / Linux)
curl -fsSL https://raw.githubusercontent.com/junyeong-ai/nodex/main/scripts/install.sh | bash

# Initialize config in your project
nodex init

# Build the document graph
nodex build

# Search for documents
nodex query search "auth"

# Explore relationships
nodex query backlinks <node-id>
nodex query chain <node-id>

# Validate against the schema
nodex check

# Diff between git refs
nodex diff origin/main HEAD
```

All commands output JSON. Add `--pretty` for human-readable formatting.

---

## Core Concepts

### Files Become a Graph

nodex transforms a flat collection of markdown files into a navigable graph. Each document becomes a **node**, and every link between documents becomes a directed **edge**.

### Edge Types

Edges come from two sources: YAML frontmatter fields, and the markdown body itself.

| Source | Default Relation | Example |
|---|---|---|
| Frontmatter `supersedes` | `supersedes` | ADR 2 supersedes ADR 1 |
| Frontmatter `implements` | `implements` | Rule implements ADR |
| Frontmatter `related` | `related` | Guide is related to ADR |
| Markdown body link `[text](path.md)` | `references` | Body link to another doc |
| Custom pattern (configurable) | **any string you choose** | e.g. `@path.md` → `imports` |

The five frontmatter / body relations above are built in. Beyond them, `[[parser.link_patterns]]` in `nodex.toml` lets you define **arbitrary relation names** — pair a regex with a relation string, and every match becomes an edge with that relation.

Markdown links are extracted via [pulldown-cmark](https://github.com/pulldown-cmark/pulldown-cmark) — an AST-based parser, not regex — so links inside fenced code blocks are correctly ignored.

### Frontmatter Schema

| Field | Type | Required | Meaning |
|---|---|---|---|
| `id` | string | yes (or auto-inferred from path) | Unique node identifier |
| `title` | string | yes | Human-readable name |
| `kind` | string | yes (or auto-inferred) | Document type — must be in `[kinds].allowed` |
| `status` | string | yes | Lifecycle state — must be in `[statuses].allowed` |
| `created` | date (ISO) | optional | Creation date |
| `updated` | date (ISO) | optional | Last edit date |
| `reviewed` | date (ISO) | optional | Last review date — drives stale detection |
| `owner` | string | optional | Owner identifier |
| `supersedes` | string \| array | optional | IDs of replaced docs |
| `superseded_by` | string | optional | ID of replacement doc |
| `implements` | string \| array | optional | IDs of implemented specs |
| `related` | string \| array | optional | IDs of related docs |
| `tags` | array | optional | Arbitrary tags |
| `covers` | string \| array | optional | Source-code paths this doc claims authority over |
| `orphan_ok` | bool | optional (default `false`) | Suppress orphan warning |
| (anything else) | any | optional | Stored under `attrs`; rejected under `[schema].mode = "strict"` |

`supersedes`, `implements`, `related`, `tags`, and `covers` accept both a single string and an array.

---

## How It Works

### Build Pipeline

| Stage | What it does | Module |
|---|---|---|
| **Scan** | Walks the filesystem using `[scope].include` / `exclude` globs. Applies `conditional_exclude` to skip child files of terminal-status parents. | `builder/scanner.rs` |
| **Cache** | Loads `_index/cache.json`. Cache invalidates wholesale if the config-serialization SHA256 or the `nodex` binary version changed. | `builder/cache.rs` |
| **Read** | Reads file contents in parallel via `rayon::par_iter`. IO errors become warnings, not fatal failures. | `builder/mod.rs` |
| **Parse** | Per-file SHA256 hash check. On hit, replay the cached `Node` + `RawEdge` set. On miss, parse YAML frontmatter, extract markdown links via pulldown-cmark, run any configured custom-pattern regexes — also in parallel. | `parser/` |
| **Dedupe IDs** | Reject the build with `Error::DuplicateId { id, first, second }` if two documents resolved to the same node id. | `builder/mod.rs` |
| **Resolve** | Convert each `RawEdge.target_path` into a node id. Strict matching only. Unmatched targets become `ResolvedTarget::Unresolved { raw, reason }` (preserved as warnings, not silently dropped). Mirror every `superseded_by: Y` scalar into a canonical `supersedes` edge. | `builder/resolver.rs` |
| **Validate** | Iterative 3-color DFS over `supersedes` edges to detect cycles. | `builder/validator.rs` |
| **Graph** | Sort edges and nodes for deterministic output, then construct the immutable `Graph`: nodes in an `IndexMap`, edges in a `Vec`, plus pre-built `incoming` / `outgoing` adjacency indices. | `model/graph.rs` |

After the graph is built, `_index/graph.json` is written. Backlinks are derived state — every consumer recomputes them from edges in O(degree) via `Graph::incoming_indices`.

### Index Once, Query Forever

- **Build artifact**: `graph.json` — single source of truth
- **Queries** read only `graph.json` — original markdown files are never re-touched, response is sub-millisecond
- **Incremental**: SHA256 per file means only changed files re-parse on the next build. Add `--full` to force a fresh build

### Query Algorithms

| Query | Result | Algorithm | Complexity |
|---|---|---|---|
| `search <kw>` | id/title/tag matches with score | Substring match, scored | O(n·m) |
| `nodes [--kind --status --tag]` | Every node matching every named predicate | Linear filter, no ranking | O(n·k) |
| `backlinks <id>` | Nodes linking to target | `incoming_indices(id)` lookup | O(degree_in) |
| `chain <id>` | Supersession chain | Walk `superseded_by` forward | O(chain_length) |
| `node <id> \| --path` | Full node + incoming/outgoing | Lookup (id direct, path linear) + both adjacency indices | O(degree), O(n) by path |
| `orphans` | Nodes with zero incoming edges | Linear scan + `orphan_grace_days` | O(n) |
| `stale` | Active docs past `stale_days` | Linear scan, filter by status + `reviewed` | O(n) |
| `recent` | Docs with date in window | Linear scan + date filter | O(n) |
| `similar` | Score-ranked candidates | Token Jaccard + tag / kind / dir / neighbour overlap | O(n·m) |
| `trust <id>` | Composite reliability + components | Weighted average over *present* component scores (absent signals dropped, denominator renormalised) | O(degree) |
| `components` | Connected component partition | Undirected BFS, deterministic ordering | O(n + e) |
| `neighborhood <id>` | Nodes within N hops | Bounded BFS (undirected) | O(visited) |
| `covered-by <path>` | Docs declaring this code path | Linear scan over `covers:` frontmatter | O(n) |
| `issues` | Orphans + stale + unresolved + rule violations + skipped rules | Composes the above + `check` | O(n + e) |

**Note on adjacency**: only resolved edges are indexed. `Unresolved { raw, reason }` edges still exist on the graph (so you can list them via `query issues`) but don't appear in `incoming_indices`.

---

## JSON-First CLI

Every command emits JSON to stdout. Human-readable text appears only for `--help` / `--version`.

### Envelope Schema

**Success:**
```json
{
  "ok": true,
  "data": { /* command-specific shape */ },
  "warnings": ["..."]
}
```
- `warnings` is omitted when empty.
- All list queries return `data: { items: [...], total: N }`.

**Error:**
```json
{
  "ok": false,
  "error": { "code": "ERROR_CODE", "message": "..." }
}
```

### Error Codes

Error codes are derived from the typed `nodex_core::error::Error` enum via `downcast_ref` — they are **never** string-matched on messages.

| Code | Cause |
|---|---|
| `CYCLE_DETECTED` | A cycle exists in `supersedes` edges |
| `DUPLICATE_ID` | Two documents resolved to the same node id |
| `PARSE_ERROR` | Malformed YAML frontmatter, or corrupt `graph.json` |
| `INVALID_TRANSITION` | `lifecycle` action attempted from a status that doesn't allow it |
| `NOT_FOUND` | Referenced node id doesn't exist in the graph |
| `ALREADY_EXISTS` | `scaffold` / `rename` target path already occupies a real file |
| `PATH_ESCAPES_ROOT` | A path traversal (`..`) or symlink would escape the project root |
| `CONFIG_ERROR` | `nodex.toml` failed validation at load time |
| `IO_ERROR` | Filesystem read/write failure |
| `VERSION_MISMATCH` | `--check-version <req>` did not match the running binary's version |
| `GIT_ERROR` | `git` failed (e.g., not a work tree, missing ref) — surfaced by `diff` and `check --since` |
| `INVALID_ARGUMENT` | clap parse failure |
| `INTERNAL_ERROR` | Anything unclassified (bug) |

### Exit Codes

| Code | Meaning |
|---|---|
| `0` | Success |
| `1` | `nodex check` found `severity = error` violations |
| `2` | Runtime failure — anything that produced an error envelope |

### Global Flags

| Flag | Effect |
|---|---|
| `-C DIR` | Run as if started in `DIR` (like `git -C`) |
| `--pretty` | Pretty-print JSON output |
| `--check-version <REQ>` | Refuse to run unless the binary version satisfies the SemVer requirement (CI pin) |

### Command Reference

| Command | Description |
|---|---|
| `nodex init` | Generate `nodex.toml` with annotated defaults |
| `nodex build [--full]` | Build graph; `--full` ignores cache |
| `nodex check [--severity error\|warning] [--since <ref>]` | Run validation rules; `--since` restricts violations to changed nodes and activates diff-aware rules; exit 1 on errors |
| `nodex diff <ref-a> <ref-b>` | Structural delta between two git refs |
| `nodex report [--format md\|json\|all]` | Generate `GRAPH.md` + `graph.json` (default: `all`) |
| `nodex migrate [--apply]` | Inject frontmatter into legacy docs (dry-run by default) |
| `nodex rename <old> <new>` | Move file and rewrite all references in body links |
| `nodex scaffold --kind X --title "..." [--id ...] [--path ...] [--dry-run] [--force]` | Create new document with valid frontmatter |
| `nodex query search <keyword> [--status x,y]` | Keyword search across id, title, tags |
| `nodex query backlinks <id>` | All nodes linking to target |
| `nodex query chain <id>` | Walk supersession chain |
| `nodex query orphans` | Nodes with zero incoming edges |
| `nodex query stale` | Active docs past `stale_days` review threshold |
| `nodex query nodes [--kind K1,K2] [--status S1,S2] [--tag T1,T2 --all-tags] [--limit N]` | Generic listing primitive — every node matching every predicate (AND across categories, OR within). Empty filter returns every node in id order. Tag matching is case-insensitive (same fold every tag-consuming surface uses). |
| `nodex query node <id> \| --path <file>` | Full node detail with incoming + outgoing edges. `--path` is the reverse lookup for editor / IDE integrations holding the file path. |
| `nodex query covered-by <path>` | Docs whose `covers:` frontmatter declares this code path |
| `nodex query issues` | Unified orphans + stale + unresolved + rule violations + skipped rules |
| `nodex query low-trust [--threshold N --kind K]` | Docs scoring below `trust.low_trust_threshold` (with per-component breakdown). Terminal-status docs always score 0 on `status` and therefore surface here too — pair with `--kind` to focus the list. |
| `nodex query trust <id>` | Composite reliability + per-component breakdown. `status` is always present; `freshness`, `drift`, `backlinks` are omitted from the JSON when their source signal is absent (no `reviewed:` date / `git_drift_threshold` unset / no external incoming edges anywhere). The composite renormalises over the present components rather than substituting a neutral value. |
| `nodex query similar [--id <id> \| --title "<t>" --kind K] [--tags a,b --threshold N --limit N]` | Vector-free similarity (token Jaccard + tag/kind/dir/neighbour overlap). Every per-component field is conditional — each is omitted when no signal exists (empty token / tag sets, pre-creation spec without `--kind` or `--parent-dir`, no graph id for `linked`). |
| `nodex query recent [--days N --field F --kind K --since YYYY-MM-DD --limit N]` | Docs whose configured date field falls in a recent window |
| `nodex query components` | Partition the graph into connected components (undirected projection, no policy) |
| `nodex query neighborhood <id> [--depth N]` | Nodes within `N` hops of `<id>` (undirected, no token counting) |
| `nodex query dependents <id> [--depth N --relations a,b]` | Transitive reverse traversal — every node that depends on `<id>` |
| `nodex query annotations [--name <pattern>] [--with-frontmatter f1,f2,...]` | Group body-text markers declared by `[[annotations]]` by capture key; `--with-frontmatter` enriches each source with selected frontmatter fields (built-in or project-declared) so consumers avoid file re-reads |
| `nodex lifecycle <action> <id> [--to id]` | Transition: `supersede --to <new>`, `archive`, `deprecate`, `abandon`, `review` |
| `nodex export schema` | JSON Schema (draft 2020-12) for the project's frontmatter |
| `nodex export enums` | Closed-vocabulary manifest (kinds, statuses, per-field enums) |
| `nodex export rules` | Active-rules manifest (which rules will fire under the current config, with per-rule `params` payload) |
| `nodex export envelope-schema` | JSON Schema (draft 2020-12) of every CLI envelope shape — drives codegen for typed downstream consumers |

---

## Validation & Lifecycle

### Built-in Rules

`nodex check` runs every registered rule against the graph and emits a flat list of `Violation` records. Each violation carries `rule_id`, `severity`, optional `node_id` / `path`, and a human message. The response also lists `skipped_rules: [{rule_id, reason}]` for rules that declined to fire — silent skips are forbidden.

| `rule_id` | Severity | What it checks |
|---|---|---|
| `required_field` | error | Every required field (per `[schema].required` + per-kind override) is present |
| `field_type` | error | `attrs` values match declared `types` (string / integer / bool / date) |
| `field_enum` | error | `attrs` + `kind` + `status` are in the declared `enums` allow-list |
| `cross_field` | error | Conditional requirements like `when status=superseded require superseded_by` |
| `unknown_field` | error | Undeclared frontmatter keys (active only under `[schema].mode = "strict"`) |
| `filename_pattern` | error | Filenames match `[[rules.naming]].pattern` regex |
| `sequential_numbering` | warning | No gaps in leading-digit sequences |
| `unique_numbering` | warning | No two files share the same leading digit prefix |
| `stale_review` | warning | Active (non-terminal) nodes not reviewed within `[detection].stale_days` |
| `git_drift` | warning | Active nodes whose referenced source files have changed since `reviewed` (opt-in via `git_drift_threshold`) |
| `frontmatter_immutable/<name>` | error | One per `[[rules.frontmatter_immutable]]` block — locked fields on terminal-status nodes have changed since the reference point (diff-aware: requires `check --since`) |
| `body_immutable/<name>` | error | One per `[[rules.body_immutable]]` block — body edit on terminal-status nodes; `mode = "frozen"` rejects any change, `mode = "append_only"` requires the pre-terminal body to remain a prefix of the new body (diff-aware) |
| `body_line/<name>` | error | One per `[[rules.body_line]]` block — lines matching `pattern` outside code blocks must carry capture values from declared enums |

Adding a custom rule means implementing the `Rule` trait in `nodex-core/src/rules/` and registering it in `registered_rules()`.

### Schema Mode

`[schema].mode` controls how undeclared frontmatter keys are treated:

- `lenient` (default): undeclared keys land in `Node::attrs` untouched
- `strict`: any frontmatter key not built-in and not declared in `types` / `enums` / `required` / `cross_field` (global + per-kind override) fires a `unknown_field` violation — catches typos like `relatd:` or `Implementss:`

### Lifecycle Actions

`nodex lifecycle <action> <node-id>` is the only safe way to mutate a document's status — it goes through `lifecycle::transition()`, which validates the source status, edits the YAML frontmatter in place, and refuses to write through symlinks.

| Action | Resulting `status` | Other fields written |
|---|---|---|
| `supersede --to <new-id>` | `superseded` | `superseded_by: <new-id>` |
| `archive` | `archived` | (none) |
| `deprecate` | `deprecated` | (none) |
| `abandon` | `abandoned` | (none) |
| `review` | (unchanged) | `reviewed: <today>` |

The four target statuses are **terminal** — once a doc is in a terminal status, no further `lifecycle` action will move it. `review` is the only non-status-changing action.

### Diff-Aware Validation

`nodex check --since <ref>` builds the graph at the named ref via `git worktree add --detach`, computes a structural diff, restricts violations to changed nodes (pure set-membership filter, no neighbour expansion), and activates rules whose semantics require two snapshots:

- `frontmatter_immutable/<name>` — lock declared frontmatter fields once a node reaches terminal status. Multiple blocks supported; each block carries a unique `name`, a `fields` list, and an optional `kinds` filter.
- `body_immutable/<name>` — lock document bodies once a node reaches terminal status. `mode = "frozen"` rejects any body edit; `mode = "append_only"` requires the pre-terminal body to remain a prefix of the new body. Driven by per-node body fingerprints (whole-body SHA-256 + per-line hash vector) computed at build time — no file re-reads at check time. The abstraction is the *simple* whole-body lock; documents with nuanced edit policies (e.g. "the `## Status` section may mirror frontmatter") should keep that logic in their own tooling.

Without `--since` both families report themselves non-applicable in `skipped_rules` rather than passing silently.

### Kind Filter

Every per-block rule family — `[[rules.body_line]]`, `[[rules.body_immutable]]`, `[[rules.frontmatter_immutable]]` — plus `[[annotations]]` accepts an optional `kinds: ["..."]` list. Empty = no restriction; otherwise the rule fires only on nodes whose `kind` appears in the list. Every entry must be in `kinds.allowed`; `Config::load` rejects typos so a silent never-fire is impossible.

### Binary-Version Pin

`[meta] nodex_version = ">=0.12, <0.13"` in `nodex.toml` makes `Config::load` refuse to return unless the running binary satisfies the SemVer requirement (error code `VERSION_MISMATCH`). The project pins its tooling instead of every CI / contributor re-implementing the check. Combines with the global `--check-version` CLI flag, which is enforced earlier (before config loads).

---

## Diff & Export

### Structural diff

```bash
nodex diff <ref-a> <ref-b>
```

Builds the graph at each git ref via `git worktree add --detach` and emits a deterministic delta:

```json
{
  "added_nodes":   [...],
  "removed_nodes": [...],
  "added_edges":   [...],
  "removed_edges": [...],
  "status_transitions": [{"id": "...", "from": "...", "to": "..."}],
  "field_changes":      [{"id": "...", "field": "...", "before": ..., "after": ...}]
}
```

Pure structural primitive — no policy, no heuristics. Drives `check --since` and the `frontmatter_immutable` rule; consumers can build CI summaries on it.

Both refs are parsed using the **current** `nodex.toml` (not the `nodex.toml` at each ref). This is deliberate: a vocabulary change — for example, removing a value from `kinds.allowed` — surfaces as concrete field changes on the affected nodes, instead of producing apples-to-oranges diffs across incompatible schemas.

### Authoritative manifests

```bash
nodex export schema           # JSON Schema (draft 2020-12) for the project's frontmatter
nodex export enums            # kinds + statuses + per-field enums
nodex export rules            # active rules (built-in + config-driven) with `params`
nodex export envelope-schema  # JSON Schema for every CLI envelope shape (typed-codegen contract)
```

The dependency direction is enforced: nodex emits, external tools (TypeScript linters, IDE plugins, CI sync gates) consume. There is no inverse — nodex never parses an external file to derive its own vocabulary.

`export envelope-schema` is the codegen contract: each per-command entry is a self-contained draft-2020-12 schema (with inlined `$defs` for the data payload), so downstream consumers can generate types directly from nodex's emitted shape instead of hand-mirroring it. The schema's `version` field is the source-of-truth nodex version, so a CI gate can detect envelope drift the same way an API schema drift would be detected.

---

## Configuration

All behavior is driven by `nodex.toml`. `Config::load` runs `validate()` at startup and rejects inconsistent configs (e.g., `lifecycle` would write a status that the same config rejects), so misconfigurations fail fast.

```toml
[scope]
include = ["docs/**/*.md", "specs/**/*.md", "README.md"]
exclude = ["docs/_index/**"]
# Skip child files of terminal-status parents:
# [[scope.conditional_exclude]]
# parent_glob = "specs/**/*.md"
# condition = "status_terminal"

[kinds]
allowed = ["generic", "guide", "readme", "adr"]

[statuses]
allowed = ["draft", "active", "superseded", "archived", "deprecated", "abandoned"]
terminal = ["superseded", "archived", "deprecated", "abandoned"]

[[identity.kind_rules]]
glob = "docs/decisions/**"
kind = "adr"

[[identity.id_rules]]
kind = "adr"
template = "adr-{stem}"

[[parser.link_patterns]]
pattern = "@([A-Za-z0-9_./-]+\\.md)"
relation = "imports"

[[rules.naming]]
glob = "docs/decisions/**"
pattern = "^\\d{4}-[a-z0-9-]+\\.md$"
sequential = true
unique = true

# Locked fields on terminal-status nodes; diff-aware (requires `check --since`).
# Multiple blocks supported — each carries a unique `name` and an optional `kinds` filter.
[[rules.frontmatter_immutable]]
name = "identity"
fields = ["id", "kind", "superseded_by"]
# kinds = ["adr"]

# Body lock — locks document body on terminal-status nodes.
# `frozen` rejects any body edit; `append_only` requires the pre-terminal
# body to remain a prefix of the new body.
# [[rules.body_immutable]]
# name = "adr-decisions"
# mode = "frozen"
# kinds = ["adr"]

# Per-line body-text vocabulary conformance — one block per pattern.
# Captures named in `enums` must hold a value from the allowed set;
# non-matching lines are silently ignored (this is a conformance rule,
# not a presence rule).
# [[rules.body_line]]
# name = "spec-decision-log"
# pattern = '''^- \*\*(?P<gate>[a-z-]+)\*\*'''
# kinds = ["spec"]
# enums.gate = ["scope", "design", "rollout", "ship"]

# Body-text marker extraction — surfaced by `nodex query annotations`.
# Pre-graph identifiers that intentionally do not resolve to a node
# (TODO topics, promotion candidates, open research questions).
# [[annotations]]
# name = "promotes"
# pattern = '''\[PROMOTES:\s*(?P<id>[\w-]+)\]'''
# key = "id"
# kinds = ["learning"]

[schema]
required = ["id", "title", "kind", "status"]
mode = "lenient"   # "strict" rejects undeclared frontmatter keys
cross_field = [
  { when = "status=superseded", require = "superseded_by" },
]

[[schema.overrides]]
kinds = ["adr"]
required = ["id", "title", "kind", "status", "decision_date"]
types = { decision_date = "date" }
enums = { priority = ["low", "medium", "high"] }

[detection]
stale_days = 180
orphan_grace_days = 14
# orphan_ok_kinds = ["readme"]
# git_drift_threshold = 5
# git_drift_relations = ["references"]

[output]
dir = "_index"

[report]
title = "Document Graph"
god_node_display_limit = 10
orphan_display_limit = 20
stale_display_limit = 20

[trust]
# Composite renormalises over *present* components only — each per-component
# field is omitted from the JSON when its source signal is absent:
#   - `freshness` absent ⇔ node has no `reviewed:` date
#   - `drift`     absent ⇔ `detection.git_drift_threshold` unset (or node has no `reviewed:`)
#   - `backlinks` absent ⇔ no external incoming edges anywhere in the graph
# Absent signals are dropped from the denominator, not replaced with a
# neutral fallback — tune weights on the components your corpus actually carries.
weights = { status = 0.4, freshness = 0.3, drift = 0.2, backlinks = 0.1 }
low_trust_threshold = 0.5

[similarity]
# Every component (`title`, `tags`, `kind`, `directory`, `linked`) is
# conditional — omitted from the JSON when no signal exists (empty token /
# tag sets, pre-creation spec without `--kind` or `--parent-dir`, no graph
# id for `linked`). Composite renormalises over the present components.
threshold = 0.3
default_limit = 10
weights = { title = 0.4, tags = 0.2, kind = 0.1, directory = 0.1, linked = 0.2 }
title_stop_words = ["the","a","an","and","or","of","to","for","in","on","with","is","are","be","by","as","at","from"]
```

| Section | Controls |
|---|---|
| `[scope]` | Which files are scanned (`include` / `exclude` globs, `conditional_exclude`, `include_hidden` — dotfiles are skipped by default) |
| `[kinds]` | Allowed `kind` values (must include `"generic"`) |
| `[statuses]` | Allowed `status` values + which are terminal |
| `[identity]` | `kind_rules` + `id_rules` (template with `{stem}`, `{parent}`, `{kind}`, `{path_slug}`) |
| `[parser]` | Custom `link_patterns`, extensions, wikilink toggle |
| `[rules]` | `naming` patterns + `frontmatter_immutable` (terminal-field lock) + `body_immutable` (terminal-body lock, `frozen` / `append_only`) + `body_line` (per-line vocabulary check) |
| `[[annotations]]` | Body-text marker patterns (regex + named-capture key); surfaced by `query annotations` |
| `[schema]` | `required` / `types` / `enums` / `cross_field` + per-kind `overrides` + `mode` |
| `[detection]` | `stale_days` / `orphan_grace_days` / `orphan_ok_kinds` / optional `git_drift_threshold` |
| `[output]` | Where build artifacts land |
| `[report]` | `GRAPH.md` formatting limits |
| `[trust]` | Composite-score weights + low-trust threshold |
| `[similarity]` | Similarity threshold, default limit, weights, stop words |

---

## Architecture

### Workspace Layout

```
nodex/
├── nodex-core/    Library — all logic: parser, builder, query, diff, export, rules, output, lifecycle, scaffold
└── nodex-cli/     Binary  — clap CLI; thin wrapper that adds JSON envelope + error classification
```

The split keeps `nodex-core` reusable — embedding it in another Rust tool doesn't pull a CLI dependency stack.

### nodex-core Modules

| Module | Responsibility |
|---|---|
| `model/` | Data types — `Node`, `Edge`, `Graph`, `Kind`, `Status`, `ResolvedTarget`, `RawEdge`, `Annotation`, `RawAnnotation`, `BodyLineMatch`, `RawBodyLineMatch` |
| `parser/` | Markdown → `(Node, Vec<RawEdge>, Vec<RawAnnotation>, Vec<RawBodyLineMatch>)`; YAML frontmatter, body links (pulldown-cmark AST), `iter_body_lines` fence-aware iterator, identity inference, minimal-diff `FrontmatterEditor` |
| `builder/` | Scan → cache → read → parse → resolve → validate → graph |
| `query/` | Read-only traversals: `search`, `traverse`, `detect`, `structure`, `issues`, `recent`, `similar` (`compute_similarity`), `trust` (`compute_trust`), `annotations` (`find_annotations`), `dependents` (`find_dependents`) |
| `diff.rs` | `compute_diff(before, after)` — pure structural delta primitive |
| `export.rs` | `export_schema(&Config)` + `export_enums(&Config)` + `export_rules(&Config)` + `export_envelope_schema()` — authoritative manifests |
| `rules/` | `Rule` trait + built-ins; `is_applicable` / `skip_reason` surface diff-aware rules; `check` returns `{violations, skipped}` |
| `command_result.rs` | Typed `data` payload of every command (`LifecycleResult`, `MigrateResult`, `RenameResult`, `InitResult`, `ReportResult`, `BuildResult`, `CheckResult`) — single source of truth for both the CLI emitter and the `export envelope-schema` derive |
| `output/` | `graph.json` (single source of truth) + deterministic `GRAPH.md` |
| `lifecycle.rs` | Status transitions that mutate frontmatter |
| `scaffold.rs` | Create new docs with valid frontmatter; deduplication via similarity |
| `path_guard.rs` | Reject `..` / symlinks; canonical `write_atomic` primitive |
| `config.rs` | `nodex.toml` load + validate; `Config::declared_fields_for(kind)` powers strict mode |
| `error.rs` | Typed `Error` enum + stable `code()` strings |

### Design Principles

1. **Immutable graph.** `Graph` is built once via `Graph::new()` and never mutated. Adjacency indices are derived state. Query results are always consistent.

2. **Config over code.** Anything project-specific lives in `nodex.toml`. Kind names, status vocabularies, edge relation names, ID templates, naming rules, schema constraints, custom link patterns, frontmatter lock lists, trust weights, similarity weights — all configurable. The core has zero hardcoded domain knowledge.

3. **Type-safe edge resolution.** `ResolvedTarget` is `Resolved { id }` or `Unresolved { raw, reason }`. Unresolved edges are surfaced via `query issues`; they are skipped by adjacency indices.

4. **SHA256 incremental + version invalidation.** Per-file content hashes mean only changed files re-parse. The cache key mixes in the config-serialization hash *and* the `nodex` binary version.

5. **Symmetric mutation guards.** Every command that writes to disk (`scaffold`, `migrate`, `rename`, `lifecycle`) routes through `path_guard` to reject `..` / absolute paths and refuse to write through symlinks. Guards live in core, not in each CLI handler.

6. **No silent rule skips.** Rules that decline to fire (`frontmatter_immutable` without `--since`, opt-in rules without their environment) appear in the `skipped_rules` array of every check / issues response — never as silent passes.

7. **One-way export.** External tools consume nodex's `export schema` / `export enums` manifests. nodex never parses an external file to derive its own vocabulary; the dependency direction is fixed.

A meta-invariant ties them together: **anything nodex itself writes must pass nodex's own `check`.** If `scaffold`, `migrate`, or `lifecycle` could produce a document the same config rejects, that's considered a bug and `Config::validate` is extended to reject the offending config shape at load time. See [`.claude/rules/config-driven.md`](.claude/rules/config-driven.md).

---

## Install

### Quick install

**macOS / Linux**
```bash
curl -fsSL https://raw.githubusercontent.com/junyeong-ai/nodex/main/scripts/install.sh | bash
```

**Windows (PowerShell)**
```powershell
iwr -useb https://raw.githubusercontent.com/junyeong-ai/nodex/main/scripts/install.ps1 | iex
```

The installer detects your platform, downloads a verified prebuilt binary, installs it to `~/.local/bin` (or `%USERPROFILE%\.local\bin` on Windows), and optionally installs the Claude Code skill.

### Supported platforms

| OS | Architecture | Target |
|---|---|---|
| Linux | x86_64 | `x86_64-unknown-linux-musl` (static) |
| Linux | arm64 | `aarch64-unknown-linux-musl` (static) |
| macOS | Intel + Apple Silicon | `universal-apple-darwin` (fat binary) |
| Windows | x86_64 | `x86_64-pc-windows-msvc` |

### Build from source

```bash
git clone https://github.com/junyeong-ai/nodex
cd nodex
./scripts/install.sh --from-source
# or: cargo install --path nodex-cli
```

### Pinning in CI

Every command accepts `--check-version <semver-req>` as a global flag — refuse to run unless the installed binary satisfies the requirement.

```bash
nodex --check-version ">=0.12,<0.13" build
```

---

## License

MIT

---

> **English** | **[한국어](README.ko.md)**
