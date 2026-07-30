---
name: nodex
version: 0.25.0
description: JSON-first CLI for markdown document graphs governed by a root `nodex.toml`. Query supersession / backlinks / orphans / stale, validate frontmatter and body immutability (project-wide or diff-aware via `--since`), scaffold / rename / migrate documents, compute trust and similarity, diff graphs between git refs, analyse merge impact (what breaks if I merge this), repoint id references with retarget, extract `[[annotations]]` body markers with `--min-count` and `--with-frontmatter` enrichment, report graph snapshot freshness with `status`, and export schema / enums / rules / envelope-schema (`--inline-refs` for self-contained schemas) / config / commands for typed codegen.
when_to_use: backlinks, supersedes, orphan, stale, frontmatter / body immutability, schema check / lint docs, list nodes by kind / status / tag, reverse path-to-node lookup, scaffold / migrate / rename markdown, impact analysis / what breaks if I merge, retarget / repoint references after supersession, trust score, low trust, doc similarity, graph diff, graph snapshot freshness / `nodex status` / stale graph.json, export schema / enums / rules / envelope-schema / config / commands, codegen / typed client / API drift, query dependents, query annotations, body-line vocabulary check, `check --since <ref>`, write-time validation / `check --content <path>=-` / gate a proposed edit before writing / batch multi-file gate, typed violation `details` for auto-fix, `schema.require_explicit` strict presence, `[search.weights]` ranking, per-rule `kinds` filter
argument-hint: <subcommand> [args]
allowed-tools: Bash(nodex *)
---

# nodex — markdown document graph CLI

**Check this document against the binary before trusting it.** The `version:`
in the frontmatter names the release it describes; `nodex --version` names the
one installed. When they differ, this file is not the contract — it may list
fewer error codes or an older command grammar than the binary actually has,
and nothing detects that for you. The binary is always its own source of
truth: `nodex export diagnostics` (error / warning / exit-code vocabularies),
`nodex export commands` (leaf paths and flags), `nodex export envelope-schema`
(payload shapes) each carry their own `version` and are generated, not
written. Read those on a mismatch, and install the matching skill from the
release (`nodex-skill-v<ver>.tar.gz`) rather than working from a stale copy.

JSON-first. Every command (bar clap's `--help` / `help` / `--version`) emits one of:

```json
{"ok": true,  "data": {...}, "warnings": [{"code": "...", "message": "..."}]}  // warnings: typed {code,message}; omitted when empty; always at envelope level, never inside `data`
{"ok": false, "error": {"code": "CODE", "message": "..."}}
```

List queries put items in `data` as `{"items": [...], "total": N}`. On plain listings (`nodes`, `search`, `backlinks`, `orphans`, `stale`, `components`) `total` counts every match and a `--limit` cap announces itself via `returned` (omitted otherwise), so a capped response never reads as complete; selection queries (`trust --top/--bottom`, `similar`, `recent`) select in core, so their `total` is the selection size itself.

Exit codes: `0` ok, `1` `check` found Error-severity violations, `2` every error envelope (config, parse, IO, version, CLI-arg, runtime).

Global flags: `--pretty` (indented JSON), `-C <dir>` (run against another project root), `--check-version <semver-req>` (refuse to run unless the binary version satisfies the requirement).

Version pinning: projects can also pin the binary via `[meta] nodex_version = "..."` in `nodex.toml` — reads warn, document-writing commands refuse with `VERSION_MISMATCH`; only the `--check-version` flag hard-gates every command.

**Always run `nodex build` first** for any `query` — queries read the indexed `_index/graph.json`; without one they fail with `GRAPH_MISSING` (exit 2). A snapshot that no longer matches the working tree (files added/removed, graph-shaping config edited) still serves the query but rides one envelope warning naming the divergence. A leaf asked for a specific id goes further: before reporting an id absent it verifies the snapshot against the working tree's content, so an id that is only missing because a document was edited in place — a new `id:`, no path moved — fails with `GRAPH_OUTDATED` (exit 2) rather than `NOT_FOUND`. Absence from a stale snapshot is not absence from the project; run `nodex build`. `NOT_FOUND` means the snapshot was checked and the id really is not in the project. If that verification cannot run at all — an in-scope path the process cannot read — you get that failure's own code (`IO_ERROR`) instead of either, because a rebuild would fail on the same path. `nodex status` reports the same content probe on demand. Build is incremental and cheap to re-run. (`check` and `scaffold` build their view live from the working tree and need no prior build.)

Body links: standard markdown (`[text](path.md)`) by default. Wikilinks (`[[id]]`) opt-in via `parser.wikilink_enabled = true`; arbitrary syntaxes via `parser.link_patterns` (each block needs a `pattern` with exactly one capture group **and** a `relation` — any name except the built-ins with code-fixed resolution, rejected at load: `covers` (path-only) and `supersedes` / `implements` / `related` (id-resolved) are declared via their frontmatter fields only; `references` stays legal). Dot-prefixed paths (`.draft.md`, `.archive/`, `.claude/`) skipped unless an include pattern literally names the dotted segment (e.g. `.claude/**/*.md`); `node_modules` / `__pycache__` / `target` / `.git` / `.venv` are pruned by default (`scope.prune_dirs`, configurable — empty list prunes nothing), and dot-prefixed trees (`.git` / `.venv`) stay caught by the hidden-path guard regardless.

## Build

```bash
nodex build                                       # incremental (default)
nodex build --full                                # bypass cache, fresh parse
```

`BuildResult` envelope: `{nodes, edges, annotations, body_line_matches, cached, parsed, duration_ms}`, plus `conditionally_excluded` (paths a `[[scope.conditional_exclude]]` rule dropped), `dangling_paths` (in-scope paths resolving to nothing — a symlink with no target) and `parse_failures` (`{path, message, content_hash}` per in-scope document that failed to parse and has no node) when non-empty. A whole-document failure (unparseable YAML, non-mapping frontmatter, an opened-but-unclosed `---` fence) never halts the build — the rest of the project still indexes — but the drop is structural data the next `check` reds via the `parse_failure` rule. A single wrong-typed built-in field (bad date, bad bool, non-string scalar) does NOT drop the document: the node stays, the field reads as absent, and `check` flags it via `field_parse`.

## Status

```bash
nodex status                                      # graph snapshot state — probe, not gate (exit 0 whenever the probe runs)
```

`data.state` ∈ `absent | unreadable | schema_mismatch | outdated | current`. `outdated` carries the exact `divergence` `{config_changed, added_paths, removed_paths, changed_paths}` (content probed against each node's recorded `content_hash`; `config_changed` is keyed on the parse+scan surface — scope, output, parser, identity, `[[annotations]]`, `rules.body_line`, `statuses.initial` — never trust/similarity/detection tuning). `unbuildable_paths` lists the snapshot's recorded parse failures — covered, never staleness; fix the document, `check` reds it. `snapshot_nodex_version` names the producing binary: a binary upgrade flags existing snapshots `outdated` until one rebuild. CI gates on `data.state` (e.g. `jq -e '.data.state == "current"'`); `schema_mismatch` means `nodex build --full`.

## Query

All read operations live under `query`.

```bash
nodex query search <kw> [--status x,y] [--limit N]  # id / title / tags, score-then-id ranked
nodex query nodes [--kind K1,K2] [--status S1,S2] [--tag T1,T2 --all-tags] [--where F=V ...] [--limit N] [--fields id,title,...]
                                                  # generic listing: AND across categories, OR within. Empty filter = all nodes in id order.
                                                  # `--fields` projects: spine fields (id,title,kind,status,path) in place; any project-declared field (other built-ins / attrs keys) under a nested `attrs` object. Undeclared field = CONFIG_ERROR.
                                                  # `--where field=value` (repeatable) narrows by exact equality over the same vocabulary's scalar fields (incl. `path`; a collection built-in like `tags` is rejected — use `--tag`), matched like a cross_field `when` predicate.
                                                  # Tag matching is case-insensitive.
nodex query backlinks <id> [--limit N]            # nodes that link to <id> — self-edges excluded
nodex query chain <id>                            # full supersession lineage (the whole connected component), oldest → newest topological order. Anchor on ANY member, even the current doc — every branch is returned, never collapsed. supersedes is a DAG: a linear lineage has one tip (the live doc) as the last entry; a fork/consolidation can have several tips — read "what's current" from the non-terminal `status`, not position alone
nodex query node <id> [--with-body]               # full detail + incoming + outgoing (honest; self-edges visible).
                                                  # `--with-body` attaches the body text (canonical line endings) — saves a separate file read;
                                                  # body-less docs get `""`, the key is absent when not asked.
nodex query node --path <file>                    # reverse lookup: same envelope as <id>, addressed by on-disk path
nodex query covered-by <path>                     # docs whose `covers:` declares this code path (a file or a whole directory — git drift measures either)
nodex query orphans [--limit N]                   # zero external incoming, after orphan_grace_days
nodex query stale [--limit N]                     # active docs past detection.stale_days
nodex query issues                                # orphans + stale + unresolved + violations + skipped_rules (resolves rules.immutable_baseline like a default check)
nodex query trust <id>                            # single-node composite [0,1] + per-component breakdown; uses per-kind weights when `[[trust.overrides]]` matches.
                                                  # `freshness` / `drift` / `backlinks` are omitted from the JSON when their source signal is absent
                                                  # (no `reviewed:` date / `git_drift_threshold` unset / no external incoming edges anywhere). Composite
                                                  # renormalises over the present components — absent signals are dropped, not replaced with a neutral value.
                                                  # When NO positively-weighted component is present, `score` itself is omitted (same convention) —
                                                  # a composite exists only where a signal exists; the read still succeeds and `components` stays inspectable.
nodex query trust --bottom N [--kind K] [--status S] [--below S]   # ranked listing: N lowest-trust nodes (asc); each item carries `score` + `components`. `--kind` / `--status`
                                                      # narrow the corpus — `--status active` is the review-queue read: terminal nodes legitimately score near
                                                      # zero and would drown the signal. `--below S` is an opt-in cutoff (keep entries strictly below S).
                                                      # Mutually exclusive with `--top` and the single-node `<id>` form.
                                                      # A node with no composite is NOT in the ranking's domain: excluded from `items`/`total`, never a
                                                      # bottom-N slot, never satisfies `--below` — the exclusion rides the envelope warnings with the count.
nodex query trust --top N    [--kind K] [--status S] [--below S]   # ranked listing: N highest-trust nodes (desc). Same opt-in filters as `--bottom`.
nodex query similar --id <id> [--limit N] [--min-score S]      # neighbours of existing doc; `--limit` caps (default `similarity.default_limit`),
                                                                # `--min-score S` is an opt-in cutoff (keep candidates scoring ≥ S).
nodex query similar --title "<t>" [--kind <k>] [--tags a,b] [--parent-dir <dir>] [--limit N] [--min-score S]
                                                  # probe before scaffolding (--kind optional; validated against kinds.allowed when given);
                                                  # --tags / --parent-dir supply the tag / directory signals for the prospective doc.
                                                  # Components `title` / `tags` / `kind` / `directory` / `linked` are all conditional — each is omitted when
                                                  # no signal is available (pre-creation spec without kind / parent_dir, no graph id for `linked`).
                                                  # Set-valued signals (title tokens, tags) are absent only when BOTH sides are empty — one side empty
                                                  # against a present set is an honest 0.0 ("the non-empty side disagrees"), so an empty --title still
                                                  # scores 0.0 against titled candidates. Composite renormalises over the present components. A candidate
                                                  # sharing NO comparable signal with the target is excluded from the ranking (never listed at a
                                                  # fabricated 0.00, so `--min-score` can't be gamed by absence) and announced via an envelope warning.
nodex query recent [--days N --field F --kind K --since YYYY-MM-DD --limit N]
nodex query components [--limit N]                # connected components, undirected (no policy), size-desc
nodex query neighborhood <id> [--depth N]         # N-hop (default 1), undirected; --depth 0 rejected
nodex query dependents <id> [--depth N --relations a,b]   # transitive reverse — every doc that depends on <id>;
                                                  # entries carry inline {id,title,kind,status,path} + hops + via witness chain (no follow-up `query node` needed)
nodex query annotations [--name <block-name>] [--with-frontmatter f1,f2,...] [--min-count N]
                                                  # `--name` exact-matches a declared `[[annotations]]` block name (not a glob); an unknown name is a CONFIG_ERROR
                                                  # result groups by annotation `name`, then by capture `key`: items[{name, entries[{key, count, sources}]}]
                                                  # --with-frontmatter enriches each source with selected node frontmatter (built-in or project-declared)
                                                  # --min-count N drops entries with count < N; empty groups removed (promotion-candidate / repeated-topic queries)
```

`query issues` always carries `skipped_rules: [{rule_id, reason}]` — silent skips are forbidden. `unresolved_edges` entries carry a typed `cause: missing | target_unparsed | excluded_from_scope | id_not_found | escapes_source | absolute` plus the `severity` (`error | warning | info`) and `policy_name` the ordered `[[detection.unresolved_policy]]` table assigned (`policy_name` absent = the built-in `warning` fallthrough), so consumers branch on typed fields instead of re-deriving the project's policy. Severity planes: `error` rows register check rules `unresolved_reference/<name>` (matching edges fail `nodex check`, counted as `violation_unresolved_reference/<name>`); `warning` edges count in `summary.total` under `unresolved_edge`; `info` edges are reported out of `total` under their row's name. Row globs match the link's normalized root-relative resolution candidates, never the raw authored target. Declaring the table replaces the default single row `{name = "excluded_target", cause = "excluded_from_scope", severity = "info"}` — re-declare it to keep it.

## Diff

```bash
nodex diff <ref-a> <ref-b>                        # structural delta; single lens = the after ref's config (refs supply content only)
```

Output: `added_nodes`, `removed_nodes`, `added_edges`, `removed_edges`, `status_transitions: [{id, from, to}]`, `field_changes: [{id, field, before, after}]`, `added_annotations`, `removed_annotations`. Both snapshots are graphed under a single lens — the **after ref's** `nodex.toml` (`check --since`: the working tree's); the before ref supplies content only.

## Impact

```bash
nodex impact <ref-a> <ref-b>                      # "what breaks if I merge this?" — diff + dependents (modified: transitive / removed: direct dangling referrers)
nodex impact <ref-a> <ref-b> --depth N --relations implements,supersedes
```

Output: `{diff, impacted, likely_breaking}`. `diff` is the full `nodex diff` envelope; `impacted: [{id, change: removed|modified, dependents: [{id, title, kind, status, path, hops, via}]}]` pairs each changed node with its dependents — a **modified** node's *transitive* dependents in the after graph, a **removed** node's *direct* referrers that still point at it and now dangle (references the same change repointed elsewhere are correctly absent). Each dependent carries inline node metadata plus the witness chain in `via` — same shape as `query dependents`. `likely_breaking: [id, …]` lists removed nodes whose referrers now dangle — the sharpest "this will break" signal. Added nodes and changes that affect nobody are omitted from `impacted` (the full delta stays in `diff`). `--depth` bounds the dependency walk, `--relations` restricts which edges it follows (validated against the project vocabulary).

## Authoring

```bash
nodex scaffold --kind <k> --title "<t>"           # id inferred; path inferred only when an identity.kind_rule maps the kind to a dir, else pass --path
nodex scaffold --kind <k> --title "<t>" --id <explicit-id>
nodex scaffold --kind <k> --title "<t>" --path docs/foo.md
nodex scaffold --kind <k> --title "<t>" --dry-run # preview, no write
nodex scaffold --kind <k> --title "<t>" --force   # overwrite existing file at same path (id collisions still refused; a doc frozen at rules.immutable_baseline refuses with the lock id)
nodex scaffold --kind <k> --title "<t>" --path docs/foo.md \
  --field 'supersedes=[old-id]' --field created=2026-06-12 --body -
                                                  # real content through the guarded seam: `--body` reads the same SOURCE grammar as `check --content`
                                                  # (`-` = stdin, else file path); `--field KEY=VALUE` (value is YAML; repeatable) renders after the
                                                  # identity lines and feeds the cross_field fixpoint. A key with a canonical source (a dedicated flag, config derivation, or the structural `path`) is refused as a --field key; the error names the exact set.
                                                  # Supplying --body/--field engages the strict gate: an Error-severity check violation the new document
                                                  # *introduces* refuses with CONTENT_VIOLATIONS (pre-existing project violations never block; every
                                                  # finding is satisfiable via --field). Default-only scaffolds keep write-and-advise: placeholder
                                                  # findings ride the warnings array.

nodex migrate                                     # plan-only (default)
nodex migrate --apply                             # inject frontmatter into bare md; atomic refuse on id collision; per-file skips (symlink/unreadable/raced frontmatter) ride warnings

nodex rename <old-path> <new-path>                # move + rewrite refs — one document only (a directory arg is refused; iterate over its files). in-scope source only; out-of-scope source = plain guarded move; alias spellings refused; locked referencing docs skipped w/ warning
nodex retarget <old-id> <new-id>                  # repoint references from one id to another (e.g. after supersession)
```

`scaffold` emits an envelope-level warning when a near-duplicate doc exists. `rename` envelope includes `id_stability: {type: already_anchored | unchanged | anchored | bare_no_frontmatter}` — when the path change would shift a path-derived id, the previous id is auto-anchored into the moved file's frontmatter so other docs' cross-references stay valid.

`retarget` rewrites every reference to `<old-id>` so it names `<new-id>`: the id-valued frontmatter relation fields (`supersedes` / `implements` / `related` / `superseded_by` — the first three accept string or array; `superseded_by` is a single-id scalar, so `superseded_by: [id]` is a `field_parse` error) and body id references (`[[wikilinks]]`, custom `link_patterns`). Matching is by **exact id** — an id that merely appears in prose is never touched — and the successor document (`<new-id>`) is skipped so its own `supersedes: [<old-id>]` never becomes a self-edge. Both ids must exist; a reference-unsafe successor id (trim-unstable / wikilink metacharacters) is refused, and a doc locked by `body_immutable` (or a `frontmatter_immutable` block covering a relation field) is skipped with a warning. Envelope: `RetargetResult {old_id, new_id, references_updated, total_updated}`. Pairs with `lifecycle supersede`: supersede sets the lifecycle state, retarget moves everyone's forward references onto the successor. Standard markdown **path** links (`[text](old.md)`) are path-bound, not id references — they keep resolving to the now-superseded file and are not rewritten; repoint them by hand (or `rename` the file when the path itself should change).

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
nodex check --content docs/a.md=-                 # validate PROPOSED bytes (stdin) before writing docs/a.md
nodex check --content docs/a.md=FILE              # …or from a file
nodex check --content docs/a.md=- --content docs/b.md=b.md   # BATCH: N proposals, one build, cross-proposal refs resolve
```

`--content` takes `PATH=SOURCE` pairs and is repeatable. `SOURCE` is `-` (stdin) or a file path resolved against the invoking directory, never `-C <dir>` — with `-C`, pass an absolute file path or use stdin. At most one `SOURCE` may be `-`; a target `PATH` may appear once.

`--severity` is an exact-match display filter: `--severity warning` shows only warnings, hides every error, **and the exit code follows the shown set** (exit 0 despite errors) — the suppression is announced as an envelope warning. Gate on errors with `--severity error` or no filter. It composes with `--content`.

`check --content PATH=SOURCE...` is the write-time gate: every proposal is overlaid into ONE graph build, so a reference one proposal authors resolves against another proposal in the same batch (a `supersede` that rewrites N referrers gates as one atomic edit — one-at-a-time would report a still-dangling link). It diffs against the current on-disk state and runs every rule (schema, cross-field, immutability). The reported set is the exact before/after delta — a violation also present in the pre-overlay report is pre-existing and never refuses the proposal; one the overlay introduces (on any node, or the node-less `parse_failure` of a proposal that destroys its own node) reds the gate at exit 1. So an agent validates an edit through nodex's own engine before the write lands, is never blocked by someone else's broken document, and an unparseable proposal fails through the same uniform rule path as every other finding. A path need not exist yet; a path inside the project root but outside the scope globs is vacuously clean (and the run warns it validated nothing), while a path that escapes the root is refused with `PATH_ESCAPES_ROOT`; both builds are read-only (no `cache.json` write). Mutually exclusive with `--since`. Caveat: `required_field` never fires for engine-derived fields — a proposal missing `id` / `status` (or a stem-derived `title`) still passes because the build infers them (and listing those fields in `schema.required` is rejected at load); a clean gate verdict does not certify those keys are spelled out unless `schema.require_explicit` is configured (see below).

`CheckResult` envelope: `{violations: [...], skipped_rules: [...], total, has_errors, proposals?, standing?}`. In `--content` mode, `proposals` carries one `{path, in_scope, has_path_errors}` verdict per pair (in invocation order) — so a clean or out-of-scope proposal is reported as checked, never a silent green; `has_path_errors` is scoped to violations attributed to that proposal's own `path` (the run-wide gate verdict is the top-level `has_errors`), and the introduced violations live once in `violations`, each keyed by its `path`. `standing` (also `--content` only) is the proposed nodes' warning-severity violations in the proposed state — the absolute view: `violations` is the introduced delta, so a node's pre-existing housekeeping warnings (`stale_review`, `git_drift`) cancel out of it; read them from `standing` instead of running a second project-wide check. Every violation also carries a typed `details: {type, ...}` — a stable machine category (the `type` discriminator) plus structured params (offending `field`, `expected` set, failing value) — so an agent branches on `details.type` and auto-proposes a fix instead of parsing the human `message` (which is a single-source rendering of the same `details`). Built-in rule_ids: `parse_failure` (node-less, one per dropped in-scope document), `field_parse` (one per wrong-typed built-in field on a present node), `required_field`, `field_type`, `field_enum`, `cross_field`, `unknown_field` (strict mode only), `explicit_field` (only when `schema.require_explicit` is set), `stale_review`, `git_drift`, `filename_pattern`, `sequential_numbering`, `unique_numbering`, `acyclic_relation` (always on; relation set is config-driven via `rules.acyclic_relations`, default `["implements"]`). Config-driven rule_ids: `body_line/<name>`, `body_immutable/<name>`, `frontmatter_immutable/<name>`.

`[schema].mode = "strict"` rejects any frontmatter key that is neither built-in nor declared in `types` / `enums` / `required` / `cross_field`. Catches typos (`relatd:` → fail). Default `lenient`. `schema.enums` values are string arrays — a non-string member (e.g. a bare TOML integer) is a load-time CONFIG_ERROR; quote numeric vocabulary (`["1","2"]`).

`[[schema.cross_field]]` predicates support four forms: `when = "field=value"` (equality), `when = "field in {v1,v2,v3}"` (membership), `when = "field exists"` (presence), `when = "field not_exists"` (absence). Scalar predicates (`=`, `in`) are rejected on collection fields (`tags`, `covers`, …) at load; use `exists`/`not_exists` for collection presence.

### Diff-aware rule families (require `--since` or `rules.immutable_baseline`)

`rules.immutable_baseline = "<git-ref>"` (e.g. `"origin/main"`) — the default ref `check` diffs against when `--since` is omitted, so `frontmatter_immutable` / `body_immutable` are enforced on plain `nodex check`. Unlike `--since` it never narrows the reported violations to changed nodes — it only supplies the before-state the immutability rules need. When the baseline cannot engage — the project is not in a git work tree, or the ref carries nothing for the project — the run proceeds with a `baseline_inert` warning naming the condition and the rules land in `skipped_rules`; the same advisory rides every mutating command (`scaffold` / `lifecycle` / `rename` / `retarget` / `migrate --apply`), so a write whose locks were never enforced never reads as clean. A ref git cannot resolve is refused outright (`CONFIG_ERROR`) by reads and writes alike — including `check --content`, so the pre-write gate never clears an edit the write itself would refuse. Watch for this after upgrading: `"origin/main"` in a shallow checkout that lacks the ref now fails every baseline-resolving command. A repository with no commits yet is inert instead, so a project can be scaffolded before its first commit. An inherited `GIT_DIR` / `GIT_WORK_TREE` is deliberately ignored — the project's own location decides which repository is measured.

Every git-backed feature (this baseline, `git_drift`, `diff`, `impact`) measures the project at its own location inside the repository that tracks it, so a `nodex.toml` in a subdirectory of a larger repository is measured as itself, not as the repository around it — and no inherited git environment variable (`GIT_DIR`, a server-side hook's quarantine object directory, pathspec magic) can redirect it.

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

Both families self-report as `skipped_rules` (with reason) when no diff is available (`--since` omitted and no resolvable `rules.immutable_baseline`). Silent non-fires are forbidden.

Both are **identity-scoped**: the baseline is paired with the working tree by node id, so a lock guards a body for as long as the document keeps its id, and `check` and the write seams agree about that because they pair the same way. A document with a new id has no baseline to compare against on either plane, so what preserves a lock across a move is preserving the id:

- an explicit `id:` in frontmatter survives any move (`id_stability: {"type": "already_anchored"}`);
- an id derived from `identity.id_rules` survives `nodex rename`, which writes the derived id in explicitly before moving the file (`{"type": "anchored"}`) — but not `mv` / `git mv`, which change the path and therefore the id;
- a **bare-markdown** document (no frontmatter at all) cannot be anchored — `rename` will not invent a frontmatter block for a path operation — so its id does change, and the `id_stability: {"type": "bare_no_frontmatter"}` warning says so. Give it an explicit `id:` (or `nodex migrate --apply`) before renaming if a lock must follow it.

So: move a locked document with `nodex rename`, and make sure it has an id that is not derived from its path.

### Vocabulary rule families (always active)

`[[rules.body_line]]` — per-line vocabulary conformance. Each block declares a regex with named captures; every match outside a code block must carry capture values from declared enums. One violation per failed (line, capture). Lines that don't match the pattern are silently ignored. Rule_id `body_line/<name>`.

### Kind filter (`body_immutable` / `frontmatter_immutable` / `body_line` / `[[annotations]]`)

The content-scoped per-block families — `body_immutable`, `frontmatter_immutable`, `body_line` — and `[[annotations]]` accept an optional `kinds: ["..."]` list. Empty = no restriction; otherwise the rule fires only on nodes whose `kind` appears in the list. Every entry must be in `kinds.allowed`; `Config::load` rejects typos so a silent never-fire is impossible. (`[[rules.naming]]` is path-scoped instead — it carries `glob`, not `kinds`.)

## Export

```bash
nodex export schema                               # JSON Schema (draft 2020-12) for project frontmatter
nodex export enums                                # kinds + statuses + per-field enums
nodex export rules                                # active rules (built-in + config-driven) with params payload
nodex export envelope-schema                      # JSON Schema for every CLI envelope shape — typed-codegen contract
nodex export envelope-schema --inline-refs        # same model, every $ref resolved in place (for $ref-naive generators like json-schema-to-zod)
nodex export config                               # resolved document-locating surface: scope, output, parser, identity rules + fallbacks, initial_status
nodex export commands                             # authoritative CLI grammar: leaf paths, positional arity, flag-selected payload modes
nodex export diagnostics                          # error-code (each core/cli origin) + warning-code + exit-code (0/1/2) vocabularies — closed sets for codegen
```

External lints consume these instead of re-parsing `nodex.toml`. `envelope-schema`, `commands`, and `diagnostics` run without `nodex.toml` (project-independent) so they can be invoked anywhere; the `version` field in their output is the SoT for downstream codegen drift gates. `export config` shows post-default resolved values (an omitted `scope.include` reads `["**/*.md"]`) plus the code-level fallbacks `identity.fallback_kind` / `identity.fallback_id_template` — derive artifact paths from `data.output.dir` instead of hardcoding `_index`. `export commands` entries carry `{path, schema}` plus `modes` / `positionals` only when applicable (omitted otherwise): `schema` is the `per_command` envelope-schema key, `modes` names flag-selected alternate shapes (`query.trust-list` behind `--bottom`/`--top`). Every release publishes `nodex-envelope-schema-v<ver>.json` and `nodex-commands-v<ver>.json` as pinnable assets, and release CI fails any envelope shape change that lacks the promised minor-or-major bump.

`export rules` `RuleManifestEntry`: `{id, source: builtin|config, severity, description, diff_aware, params}`. `params` carries the rule's configured values (regex, kinds, mode, enums, thresholds, …) — schema is per-rule, kept free-form so adding a new built-in doesn't reshape the manifest.

## Report / Init

```bash
nodex report                                      # writes graph.json + GRAPH.md (default = all)
nodex report --format md|json                     # only one
nodex init                                        # writes annotated nodex.toml
```

When authoring `nodex.toml` inline instead of via `init`, the gotchas —
each is a real load-time rejection: `schema.types` values are `string |
integer | bool | date` only and collection fields (`tags`, `related`, …)
take NO type entry; `schema.required` takes authored fields only (id /
title / kind / status / orphan_ok are parser-resolved and refused);
`default_limit` sits under `[similarity]`, not `[similarity.weights]`;
`parser.extensions` entries carry the leading dot; `annotations` patterns
need a named capture matching `key`; narrowing `statuses.allowed` means
setting `statuses.terminal` too (every default terminal must stay
allowed). A worked example lives in `minimal-config.toml` next to this
file — read it before writing a config by hand.

With `wikilink_enabled = true`, a `[[...]]`-shaped annotation marker is ALSO
parsed as a wikilink and surfaces as an unresolved edge in `query issues` —
use a non-bracket marker syntax if you want annotations only.

## Error codes

Stable across releases; matched via `error.code` in the envelope, never by message string.

`IO_ERROR`, `PARSE_ERROR`, `CONFIG_ERROR`, `CYCLE_DETECTED`, `DUPLICATE_ID`, `INVALID_TRANSITION`, `NOT_FOUND`, `GRAPH_MISSING`, `ALREADY_EXISTS`, `PATH_ESCAPES_ROOT`, `CONTENT_VIOLATIONS`, `VERSION_MISMATCH`, `GIT_ERROR`, `INVALID_ARGUMENT`, `INTERNAL_ERROR`.

`GRAPH_MISSING` = a `query` ran with no `graph.json` snapshot — run `nodex build`.

## Workflows

**Cleanup triage** — no single "cleanup" verb; compose the primitives:
`query issues` (what's broken) → `check --severity error` (what blocks) →
`query trust --bottom N --status active` (what to distrust — the review
queue; terminal docs score near zero by design and would drown it), then
act with `lifecycle set --status archived`, `retarget`, or `rename`.

**Before authoring**

```bash
nodex build
nodex query similar --title "<draft>" [--kind <k>]  # avoid duplicates (--kind optional)
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
# every export is wrapped in the {ok,data} envelope — unwrap .data for raw-schema consumers
nodex export enums           | jq .data > tools/lint/enums.json
nodex export schema          | jq .data > tools/lint/frontmatter.schema.json
nodex export rules           | jq .data.rules > tools/lint/rules.json   # .data wraps {version, rules}; enums/schema are direct
nodex export envelope-schema | jq '.data.per_command["query.issues"]' > tools/codegen/query-issues.schema.json   # one entry per CLI leaf (docs/CODEGEN.md)
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
