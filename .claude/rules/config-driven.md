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

## Self-consistency invariant

Any document the tool itself produces — `scaffold` creating a new file, `migrate` injecting frontmatter, `lifecycle` mutating a status — must pass the project's own `check`. If there is a config shape that lets a tool action write a document the same config then rejects, either reject that config shape at load (`Config::validate`) or derive the written value from config so the two can never drift.

Concrete applications of this rule already in the code:

- `statuses.allowed` must cover the four lifecycle target statuses (`superseded`, `archived`, `deprecated`, `abandoned`).
- Any `schema.enums.status` declaration — global or per-kind override — must also cover those four.
- Every value listed in an `enums.<field>` must parse as the field's `types.<field>` if both are declared.
- `kinds.allowed` must include `FALLBACK_KIND` (`"generic"`) — the kind `infer_kind` assigns when no `identity.kind_rules` glob matches.
- Initial-status defaults for tool-written documents come from `Config::initial_status_for(kind)`, never from hardcoded strings.

When you add a new tool action that writes to disk, list the fields it writes and for each one either (a) show that `Config::validate` rejects any config where the written value would fail the same config's rules, or (b) route the written value through a `Config` method that cannot return an out-of-vocabulary result.
