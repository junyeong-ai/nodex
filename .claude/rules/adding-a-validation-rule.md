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
   filter, its relation, the precondition its threshold needs; what it never
   counts: [Counting the population](#counting-the-population). Where
   the violation loop already iterates exactly that population, count in it
   (`required_field` counts the nodes it has a required set for). Where the
   guarded population is wider than what the loop touches, count the
   population: a `body_line` block guards documents of its kinds whether or
   not any line matched, `parse_failure` guards every document the build
   attempted, and a diff-aware lock guards the records it is armed over, even
   on a clean tree where the diff is empty. Derive that count from the
   predicates the rule judges with (`Node::matches_kinds`,
   `Config::is_terminal`, `rules::lock_holds`), never from a restatement of
   them. A diff-aware rule subtracts `GraphDiff::added_ids` (its rustdoc says
   why) and reports them, with any other selected unit it has nothing to
   judge against, as `RuleRun::unjudged`.
   `Rule::severity` declares the default; `Rule::evaluate` applies configured warning
   promotion consistently. Consumers must not bypass it with direct `check` calls.
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
   no timestamps) — `details` is part of `rules::finding_identity` (rule,
   severity, node id, details; never `path` or `message`), on which the
   `introduced_violations` multiset diff behind every proposal gate pairs. Wrap a
   field that only locates or measures the finding (a line, a path, a
   count) in `rules::detail::Evidence`, which equals every other, and say
   why at the field: unwrapped, an edit that moves it reads to the gates
   as a new finding.
2. Register in the list `rules::registered_rules(config)` returns (the pushes
   live in `rules_with_classification`, which `check_with_unresolved`, behind
   `query issues`, calls with a shared classification) — the single registry
   `rules::check`, `query issues`, the write gates, `export::export_rules` and
   `Config::reads_a_baseline` / `Config::judges_steps` all read. Registry
   discipline: a rule whose driving config block is absent is omitted
   from the registry entirely (conditional registration, e.g. `git_drift` only when
   `git_drift_threshold` is set) — never registered-and-skipped. A rule
   registered on one config value takes it at registration
   (`GitDriftRule::new(threshold)`), so it has no branch for the value's
   absence — unless a seam reads the same declaration: the status-flow pair
   reads `statuses.flow` through `Config::status_flows` / `Config::status_flow_for`, because
   `status_entry` asks the entry status of `Config::initial_status_for`,
   and a copy would be a second path to one declaration. `skipped_rules`
   is reserved for rules whose
   config is present but whose runtime prerequisite (e.g. a diff) is
   not.
3. Read only from `RuleContext`. An environment-backed rule verifies
   its prerequisites in `rules::preflight` (fail fast as
   `CONFIG_ERROR`) and measures inside `check`; stay inside `root`.
   Consume merged config views (`required_for`, `types_for`, …) —
   never raw `schema_override_for`.
4. Diff-aware rule: override `diff_aware` to `true`, and have
   `is_applicable` return `false` when `ctx.since.is_none()`, with a
   `skip_reason` — silent non-fires are forbidden (see
   `.claude/rules/config-driven.md`). `diff_aware` is what
   `Config::reads_a_baseline` asks before `rules.immutable_baseline` is
   resolved at all, and, with `judges_steps`, what `BaselineProbe::refusals`
   selects on: a rule that needs a prior state but is left at the default
   gets a baseline only when another rule asks for one, and the probe never
   asks it. A rule that freezes
   a part of a document against the baseline also overrides `is_lock`
   to `true`: `BaselineProbe::refusals` refuses a lock's finding on the
   state alone, and every other rule's only as the delta the write
   introduces. A rule that judges
   how records move across history reads `ctx.steps` and declares
   `judges_steps(config)` — an endpoint diff folds a range into one move.
   Report this requirement from the configured policy so snapshot-only locks
   do not trigger unused history reads. A rule may also declare `diff_aware`
   when it requires an applicable baseline; step locks use both, while
   snapshot locks read only the baseline delta.
   What it counts per step, and why a step it could not answer stays
   `unjudged`: [Counting the population](#counting-the-population).
   Every rule also answers which of its findings a diff is responsible
   for — `Rule::touched_by`, what `check --since` keeps. The default is
   the finding's own document being a record the diff touched (a
   node-less finding is kept: it is about the project). Override it when
   the findings are decided by *other* documents' records — `orphan`
   adds the documents a pointer at which moved, an edge or a
   predecessor's `superseded_by`; `superseded_reference` the cited document
   and its lineage, read through `ctx` since the finding carries only the
   target; `git_drift`, whose reading is git's, asks git whether
   `since..HEAD` added a commit the reading counts on any measured path — because a default that
   reads only the subject drops the finding exactly when a neighbour's
   edit created it. A lock (`is_lock`) keeps every finding too, as
   `body_immutable` and `frontmatter_immutable` do: its finding is drift
   from the baseline, and under `check --since <ref>` with a configured
   baseline the diff that narrows the report (the range) is not the one
   the lock judged against, so the default would drop drift the range did
   not touch. A rule judging steps keeps every finding: each is
   about a step inside the range, a move the range went on to undo
   included.
5. Per-block kind filter: carry `kinds: Vec<String>`, gate with
   `node.matches_kinds(...)`; `Config::validate_kinds` rejects typos at
   load, immutability families also route `validate_immutable_blocks`.
   Whether a lock block holds a record — for a lock family, or a rule that
   honours locks as `superseded_reference` does — is
   `rules::lock_holds(config, block.arming(), kind, status)`, given the
   kind and status of the frame the rule judges in; never
   `matches_kinds` and `Config::lock_arms` composed by hand.
6. Nothing to add in `export.rs`: `export_rules` derives entirely from
   `registered_rules` (a rule needing two snapshots always appears in the
   manifest, flagged `diff_aware` or `judges_steps`) — activity gating
   happens at registration, nowhere else. A rule driven by config values
   overrides `Rule::params` to surface them on its manifest entry. Add the
   rule, with its registration condition, to the built-in rule list in
   `.claude/skills/nodex/reference/config.md`; no test ties that list to the
   registry.

## Counting the population

The thesis — a rule reports what it guards beside what it found — is
`.claude/rules/config-driven.md` (No silent vacuous passes). How a rule counts:

- The count leaves the same pass as the violations, never a second traversal:
  a reach that could disagree with the verdict would be worse than no reach at
  all.
- A rule that judges a record against a prior state guards only what it can be
  judged against. A diff carries its per-node channels over the ids both
  snapshots hold, so a record the baseline has no node for is outside the
  population however it looks now.
- What the scope selected and the rule could not judge is reported beside the
  reach as `unjudged`, so one response carries both without anything having to
  attribute a missing prior state to a cause. A non-zero is a difference, not a
  defect, and what it points at is the rule's own question: a lock reading
  against a baseline counts a record authored since it, which costs nothing,
  alongside one whose baseline record went missing, which does, and cannot tell
  them apart. Each rule's cause is what names the remedy.
- A rule judging steps counts the same way per step. `status_entry` guards every
  record a step holds under the flow; one a parent already held has its answer.
  `status_transition` guards the records some step held on a parent and counts
  the ones that only ever entered as `unjudged`.
- Both count as `unjudged` a record the flow governs that the run's final step
  does not carry — a document git ignores, which no commit can record — however
  an earlier step judged it: what a past step judged is no answer for where the
  record stands now.
- A document a commit could not parse is read back from the commit before the
  change that broke it rather than counted apart, because a count is all an
  unjudged record would leave behind, and a later step holding the same id
  would hide it.
- Where that reading is cut off — a shallow clone whose earlier state lies
  beyond its cut, or a commit whose tree the build refuses, which the envelope
  names (`history_unread`) — the records arriving at that step are counted
  rather than judged, and that count is the one thing no later step takes back.
