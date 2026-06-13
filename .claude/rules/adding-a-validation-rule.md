---
paths:
  - "nodex-core/src/rules/**"
---

# Adding a validation rule

1. Implement `Rule` (`id`, `severity`, `check`, `description`).
   `severity` is the closed `Error | Warning` enum — there is no Info
   check severity (the per-edge `info` plane belongs to
   `detection.unresolved_policy`, a different type:
   `config::UnresolvedSeverity`). Build every violation through
   `Violation::new(rule_id, severity, node_id, path, details)` with a
   `rules::detail::ViolationDetails` variant — never a struct literal.
   Add a variant for a genuinely new violation class (its `#[serde(tag =
   "type")]` discriminator is the stable machine category an agent
   branches on); the exhaustive `match` in
   `ViolationDetails::render_message` then forces the human `message` at
   compile time, so prose and the typed payload are one source. Carry the
   structured params a consumer needs to act (offending field, expected
   set, failing value) and keep them deterministic (sorted / `BTreeMap`,
   no timestamps) — `details` participates in `Violation` equality, which
   the write-gate `introduced_violations` multiset diff relies on.
2. Register in `rules::registered_rules(config)` — the single registry
   both `rules::check` and `export::export_rules` read from. Registry
   discipline: a rule whose driving config block is absent is omitted
   from the registry entirely (conditional registration, e.g.
   `git_drift` only when `git_drift_threshold.is_some()`) — never
   registered-and-skipped. `skipped_rules` is reserved for rules whose
   config IS present but whose runtime prerequisite (e.g. a diff) is
   not.
3. Read only from `RuleContext`. An environment-backed rule verifies
   its prerequisites in `rules::preflight` (fail fast as
   `CONFIG_ERROR`) and measures inside `check`; stay inside `root`.
   Consume merged config views (`required_for`, `types_for`, …) —
   never raw `schema_override_for`.
4. Diff-aware rule: `is_applicable` returns `false` when
   `ctx.since.is_none()`, with a `skip_reason` — silent non-fires are
   forbidden (see `.claude/rules/config-driven.md`).
5. Per-block kind filter: carry `kinds: Vec<String>`, gate with
   `node.matches_kinds(...)`; `Config::validate_kinds` rejects typos at
   load, immutability families also route `validate_immutable_blocks`.
6. Nothing to add in `export.rs`: `export_rules` derives entirely from
   `registered_rules` (diff-aware rules always appear in the manifest,
   flagged `diff_aware`) — activity gating happens at registration,
   nowhere else.
