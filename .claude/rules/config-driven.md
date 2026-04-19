# Config-Driven Design

All project-specific behavior must come from `nodex.toml` — never hardcode domain logic.

- Kind / status vocabulary: every doc's `kind` must be in `config.kinds.allowed` and `status` in `config.statuses.allowed`; `FieldEnumRule` enforces this even when no explicit per-kind enum override is declared
- Terminality: `config.statuses.terminal` decides which statuses block further lifecycle transitions
- Kind inference: `config.identity.kind_rules` glob patterns
- ID inference: `config.identity.id_rules` templates with `{stem}`, `{parent}`, `{kind}`, `{path_slug}`
- Custom link patterns: `config.parser.link_patterns` regex
- Validation rules: `config.rules.naming` for filename patterns
- Schema overrides: `config.schema.overrides` for per-kind required / types / enums / cross_field
- Detection thresholds: `config.detection.stale_days`, `config.detection.orphan_grace_days`

When adding new features, ask: "Does this belong in config or in code?" If it could vary between projects, it belongs in config.
