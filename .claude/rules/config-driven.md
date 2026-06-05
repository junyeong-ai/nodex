# Config-Driven Design

All project-specific behavior must come from `nodex.toml` — never hardcode domain logic.

## Semantic config items

Every semantic behavior is declared once, read many times:

- **Vocabulary**: kinds (allowed list), statuses (allowed list + terminal markers)
- **Classification**: kind_rules (path → kind), id_rules (path+kind → id)
- **Patterns**: link_patterns (custom link extraction), naming rules (filename validation)
- **Validation schema**: required fields, types, enums, cross-field predicates (per-kind overrides available)
- **Scoring**: trust weights (status/freshness/drift/backlinks), similarity weights
- **Detection**: stale threshold, orphan detection (grace period + ok_kinds), git drift
- **Mutation guards**: path traversal protection, symlink handling

When adding a feature: "Does this vary by project?" → Yes = config, No = code.

## Self-consistency invariant

Tool-written documents (scaffold, migrate, lifecycle) must pass the same config's check. Enforce by either:
- Rejecting incompatible config shapes at load time (`Config::validate`), or  
- Deriving tool output from config (cannot produce out-of-vocabulary values)

Examples: lifecycle statuses must be in allowed list, initial status derives from schema, scaffold defaults consume merged config views.

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
