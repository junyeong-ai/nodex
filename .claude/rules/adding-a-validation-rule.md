---
paths:
  - "nodex-core/src/rules/**"
---

# Adding a validation rule

1. Implement `Rule` (`id`, `severity`, `check`, `description`,
   `subject_unit`). `check` returns a `RuleRun` — the violations *and*
   `subjects`, the population this rule guards. A rule that guards nothing
   passes for the same reason a rule that guards everything passes, and the
   reach is the only place that difference shows. `subject_unit` names what
   is counted (`Nodes` / `Edges` / `Files`) so `subjects: 0` reads without
   the manifest.

   The population is what the rule's declared scope selects — its `kinds`
   filter, its relation, the precondition its threshold needs — never the
   offending subset and never the slice that moved on this run. Where the
   violation loop already iterates exactly that population, count in it
   (`required_field` counts the nodes it has a required set for). Where the
   guarded population is wider than what the loop touches, count the
   population: a `body_line` block guards documents of its kinds whether or
   not any line matched, `parse_failure` guards every document the build
   attempted, and a diff-aware lock guards the records it is armed over —
   which on a clean tree is the whole point, since the diff is empty and the
   lock is not idle. Derive that wider count from the same predicates the
   rule judges with (`Node::matches_kinds`, `Config::is_terminal`), never
   from a restatement of them: a reach that can disagree with the verdict is
   worse than no reach at all. A diff-aware rule subtracts
   `GraphDiff::added_ids` first — a diff carries its per-node channels over
   the ids both snapshots hold, so a record the baseline has no node for is
   one the rule provably cannot fire for, and counting it claims a reach the
   verdict can never match. Report those as `RuleRun::unjudged` rather than
   dropping them — and any other unit the scope selects that the rule has
   nothing to judge against, whichever way it judges: a reach alone says what
   the rule stood over, never what it could reach.
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
   Every rule also answers which of its findings a diff is responsible
   for — `Rule::touched_by`, what `check --since` keeps. The default is
   the finding's own document being a record the diff touched (a
   node-less finding is kept: it is about the project). Override it when
   the findings are decided by *other* documents' records — `orphan`
   adds the documents an added or removed edge points at, `git_drift`
   the documents its reading counts commits on (through `ctx.graph`) —
   because a default that reads only the subject drops the finding
   exactly when a neighbour's edit created it.
5. Per-block kind filter: carry `kinds: Vec<String>`, gate with
   `node.matches_kinds(...)`; `Config::validate_kinds` rejects typos at
   load, immutability families also route `validate_immutable_blocks`.
6. Nothing to add in `export.rs`: `export_rules` derives entirely from
   `registered_rules` (diff-aware rules always appear in the manifest,
   flagged `diff_aware`) — activity gating happens at registration,
   nowhere else.
