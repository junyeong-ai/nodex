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
- Per-field defaults (`scaffold::default_for_field`) consume `types_for(kind)` / `enums_for(kind)` — the merged views — so a top-level `[schema]` declaration without a per-kind override is still honoured.
- Scaffold and migrate both call `scaffold::render_default_frontmatter`; neither rolls its own field list or yaml_quote. New tool actions that write frontmatter should do the same.

## No silent runtime skips

The load-time validator's only purpose is to reject configs whose rules the runtime can honour. The mirror failure is also forbidden: the validator accepts a rule, the runtime silently never fires it. `read_field_as_string` previously missed `created` / `updated` / `reviewed`, so `cross_field.when = "reviewed=YYYY-MM-DD"` loaded cleanly but never matched. When adding a new built-in field, a new `WhenPredicate` shape, or a new rule hook: extend every reader that rule depends on, and add a test that asserts the rule *fires* on the expected input (not only that it loads).

When you add a new tool action that writes to disk, list the fields it writes and for each one either (a) show that `Config::validate` rejects any config where the written value would fail the same config's rules, or (b) route the written value through a `Config` method that cannot return an out-of-vocabulary result.
