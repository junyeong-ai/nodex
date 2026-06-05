# Config-Driven Design

All project-specific behavior must come from `nodex.toml` — never hardcode domain logic.

## Semantic config items

Every semantic behavior is declared once, read many times:

**Vocabulary & Status:**
- `kinds.allowed` — document type vocabulary; must include "generic" (fallback)
- `statuses.allowed` — document lifecycle states (active, archived, etc.)
- `statuses.terminal` — states that block further transitions (gates lifecycle)
- `statuses.initial` — initial status for newly scaffolded documents (explicit; must be in allowed)

**Classification Rules:**
- `identity.kind_rules[]` — glob → kind (order-critical: first match wins)
- `identity.id_rules[]` — (glob, kind) → id template (order-critical; fallback: "{kind}-{stem}")

**Schema & Validation:**
- `schema.required`, `schema.types`, `schema.enums` — global frontmatter rules
- `schema.overrides[]` — per-kind overrides (required fields, type/enum changes, cross-field checks)
- `rules.naming[]` — filename validation patterns

**Scoring & Queries:**
- `trust.weights` — composite score components (status, freshness, drift, backlinks)
- `trust.overrides[]` — per-kind weight tuning (first-match lookup; replaces global entirely)
- `similarity.weights` — query ranking (title, tags, kind, directory, linked)
- `similarity.default_limit` — results per query (must be ≥1)

**Detection & Orphan Handling:**
- `detection.stale_days` — threshold for stale doc detection (None = disabled)
- `detection.orphan_grace_days` — exempt new docs for N days (0 = immediate check)
- `detection.orphan_ok_kinds[]` — kinds that are leaf-by-design (never orphan)

**Extraction & Safety:**
- `parser.link_patterns[]` — custom link extraction (order-critical; must have 1 capture group)
- `parser.wikilink_enabled` — enable [[wikilink]] syntax
- `parser.extensions[]` — link target validation extensions
- `scope.include/exclude` — file scope inclusion/exclusion patterns
- `scope.conditional_exclude[]` — status-based filtering rules
- `annotations[]` — body-text marker extraction (order-critical: index-based lookup)

**Decision:** "Does this vary by project?" → Yes = config, No = code.

## Self-consistency invariant

Tool-written documents (scaffold, migrate, lifecycle) must pass the same config's check. Enforce by either:
- Rejecting incompatible config shapes at load time (`Config::validate`), or  
- Deriving tool output from config (cannot produce out-of-vocabulary values)

Examples: lifecycle statuses must be in allowed list, initial status derives from config, scaffold defaults consume merged config views.

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
