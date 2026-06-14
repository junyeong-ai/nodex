# Config-Driven Design

All project-specific behavior must come from `nodex.toml` — never hardcode domain logic.

## Semantic config items

Every semantic behavior is declared once, read many times:

**Vocabulary & Status:**
- `kinds.allowed` — document type vocabulary; must include "generic" (fallback)
- `statuses.allowed` — document lifecycle states (active, archived, etc.)
- `statuses.terminal` — states that block further transitions (gates lifecycle)
- `statuses.initial` — status for tool-written documents and frontmatter-less parses (optional; must be in `allowed`; absent → first `allowed` value)

**Classification Rules:**
- `identity.kind_rules[]` — glob → kind (order-critical: first match wins)
- `identity.id_rules[]` — (glob, kind) → id template (order-critical; fallback: "{kind}-{stem}")

**Schema & Validation:**
- `schema.required`, `schema.types`, `schema.enums`, `schema.cross_field[]` — global frontmatter rules
- `schema.overrides[]` — per-kind overrides (required fields, type/enum changes, cross-field checks)
- `schema.mode` — `lenient` (default) | `strict` (undeclared frontmatter keys rejected)
- `schema.require_explicit[]` — inferrable built-ins (`id`/`title`/`kind`/`status`) a document must author rather than inherit from a fallback; an inferred (or empty) named field reds `check` via `explicit_field`. `orphan_ok` rejected (a bool is structurally always present)
- `rules.naming[]` — filename validation patterns
- `rules.body_line[]` — per-line body vocabulary (regex with named captures; capture values must come from the block's declared enums)
- `rules.frontmatter_immutable[] / body_immutable[]` — diff-aware locks (each `body_immutable` block's `trigger` = `terminal` | `creation`)
- `rules.immutable_baseline` — default git ref `check` diffs against when `--since` is omitted (enables the immutability locks by default; never narrows the violation set)
- `rules.acyclic_relations` — relations whose edge graph must stay a DAG (default `["implements"]`; every entry must be a known relation; empty list rejected)

**Scoring & Queries:**
- `trust.weights` — composite score components (status, freshness, drift, backlinks)
- `trust.overrides[]` — per-kind weight tuning (first-match lookup; replaces global entirely)
- `similarity.weights` — `query similar` ranking (title, tags, kind, directory, linked); composite renormalised over present components
- `similarity.default_limit` — results per query (must be ≥1)
- `search.weights` — `query search` keyword ranking (`id`/`title` each with an exact + partial tier, `tag`); additive (a node's score is the sum of its matched fields), not renormalised — finite, non-negative, positive-sum

**Detection & Orphan Handling:**
- `detection.stale_days` — threshold for stale doc detection (None = disabled; 0 rejected at load)
- `detection.git_drift_threshold` / `git_drift_relations` — commits-since-review drift gate (None = disabled; 0 rejected at load) and which relations it measures
- `detection.orphan_grace_days` — exempt new docs for N days (0 = immediate check)
- `detection.orphan_ok_kinds[]` — kinds that are leaf-by-design (never orphan)
- `detection.unresolved_policy[]` — ordered first-match-wins (cause, glob?) → severity rows classifying unresolved references (`error` = check rule `unresolved_reference/<name>`, `warning` = counted fallthrough, `info` = reported out of total; globs match normalized resolution candidates, never the raw target; declaring replaces the default `excluded_target` info row)

**Extraction & Safety:**
- `parser.link_patterns[]` — custom link extraction (exactly 1 capture group; duplicate (pattern, relation) pairs rejected; the relation must not be a code-fixed-resolution built-in — path-only `covers` and id-only `supersedes`/`implements`/`related` are rejected at load, `references` is legal)
- `parser.wikilink_enabled` — enable [[wikilink]] syntax
- `parser.extensions[]` — link target validation extensions
- `scope.include/exclude` — file scope inclusion/exclusion patterns
- `scope.prune_dirs` — directory basenames pruned from the walk at any depth (default `["node_modules","__pycache__","target",".git",".venv"]`; plain segments, no globs/separators, empty list prunes nothing; dot-prefixed trees stay caught by the hidden-path guard regardless)
- `scope.conditional_exclude[]` — drop a terminal parent's sub-artifacts (`parent_glob` selects the parent, `child_glob` selects which siblings are derivative; only `child_glob` matches are excluded and the dropped paths are reported on the build result)
- `annotations[]` — body-text marker extraction (name-keyed: each block's unique `name` is the stable lookup id in JSON output and CLI filters)

**Decision:** "Does this vary by project?" → Yes = config, No = code.

## Self-consistency invariant

Tool-written documents (scaffold, migrate, lifecycle) must pass the same config's check. Enforce by one of:
- Rejecting incompatible config shapes at load time (`Config::validate`), or
- Deriving tool output from config (cannot produce out-of-vocabulary values), or
- Validating a user-supplied value at the command's write seam, when validity depends on the document being acted on (`lifecycle set --status` refuses a status the kind's vocabulary rejects, a terminal state forbids leaving, or a status-keyed `cross_field` rule would leave unsatisfied)

Examples: initial status derives from config, scaffold defaults consume merged config views, `supersede` writes `superseded_by` in the same transaction so its own cross-field rule holds.

## No silent runtime skips

Config must be validated comprehensively at load time. When a config value is accepted, the runtime must use it — never silently ignore or bypass it.

This applies to:
- Value ranges (thresholds must be valid for their domain)
- Predicate correctness (when/require must reference declared fields)
- Pattern compilation (globs and regexes must compile)
- Vocabulary alignment (field values must be in allowed sets)
- Cardinal rules (every filter/override must be non-empty, duplicates rejected)

See `Config::validate()` for comprehensive guards.

## Symmetric guards

Security/safety checks must apply uniformly across all mutation points. When guarding one command (e.g., migrate skipping symlinks), apply the same guard to every other command that touches the same resource. Pattern: Core library functions enforce the guard so no handler can forget it.
