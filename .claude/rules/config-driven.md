# Config-Driven Design

All project-specific behavior must come from `nodex.toml` — never hardcode domain logic.

## Semantic config items

Every semantic behavior is declared once in `nodex.toml` and read many times. What each key means, its default and what load refuses is the user-facing reference `.claude/skills/nodex/reference/config.md`; `Config::validate` is the code. A key added, changed or newly refused at load changes that reference in the same change.

A declaration read in more than one place is read through one seam:

- governing flow and entry status: `Config::status_flow_for`, `Config::initial_status_for` — parsing, scaffolding, migration and entry checks
- lock arming: `Config::lock_arms`, for both lock families and flow validation; whether a block holds a record: `rules::lock_holds`; which frontmatter fields a lock covers (`fields = ["*"]` / `except`): `FrontmatterImmutableRuleConfig::locks_field`, flow validation and citation exemptions included
- declared ID relations: `Node::frontmatter_relations`, keyed by authored field, so field-based schema and locks and relation-based graph filters read one declaration
- reference lock protection: `rules::reference_protection`
- forbidden fields: `Config::validate_forbidden`, `validate_merged_forbidden`
- lifecycle steps at write seams: `BaselineProbe::refusals`, plus `BaselineProbe::undeclared_move` for the lifecycle and status-authoring seams; history semantics: [Rule population, findings and history](../../nodex-core/CLAUDE.md#rule-population-findings-and-history)
- rule severity: `Rule::configured_severity` / `Rule::evaluate`, shared by checks, manifests and mutation gates
- the output directory: folded into nodex's path language once as `output.dir` is read, so the traversal guard, the scan's self-exclusion glob, every writer's join and the path `status` reports are one value on every platform

Invariants behind what the reference states:

- `may_be_empty` is read by the coverage disclosure alone, never by the walk: a scope that behaved one way and disclosed another is the defect it exists to prevent. Where a message carries a cause (a `prune_dirs` entry the pattern lies under), the cause rides the message it silences rather than becoming a carve-out, because the walk can miss a pattern in more ways than there are detectors for.
- Directory pruning preserves every admitted document: [Parsing, scope and cache](../../nodex-core/CLAUDE.md#parsing-scope-and-cache).
- Only a frontmatter relation's `field` and `relation` affect parsing; its `retarget` policy does not invalidate the parse cache.
- A version pin is a value that ages: the init template states none as a literal (`nodex init` writes the running binary's own), and every pin the docs teach is held to the binary by `every_documented_version_pin_admits_the_binary_beside_it`.

**Decision:** "Does this vary by project?" → Yes = config, No = code.

## Self-consistency invariant

Tool-written documents (scaffold, migrate, lifecycle) must pass the same config's check. Enforce by one of:
- Rejecting incompatible config shapes at load time (`Config::validate`), or
- Deriving tool output from config (cannot produce out-of-vocabulary values), or
- Validating a user-supplied value at the command's write seam, when validity depends on the document being acted on (`lifecycle set --status` refuses a status the kind's vocabulary rejects, or a terminal state forbids leaving)

The backstop under all three is `mutate::introduced`: every write seam but `migrate` asks it before writing and refuses on the Error-severity violations the proposal *introduces* (`migrate` writes only fields the document already inferred, so its violation set is unchanged by construction). The guards above are preconditions with remedies of their own; the gate is what makes the invariant complete, because the rules a mutation can break are the whole registry rather than the family a seam was written against. It is also the limit: a seam refuses **exactly** what the project's own config makes an error — the same mutation under a config that only warns must succeed, and a rule that cannot fire on a document (out of scope, no node) cannot refuse a write touching it. *Exactly* holds for the unit as well: a lock costs only the part of a document it names, and `mutate::narrow` writes the rest. The one thing the delta cannot see is a document the write drops from the project, so a write that evicts sub-artifacts through `scope.conditional_exclude` names them (`document_evicted`, from `mutate::evicted`) rather than refusing them. How each is decided — which findings cost a document whole, which writes evict and why none is refused: [Mutation verdicts and baselines](../../nodex-core/CLAUDE.md#mutation-verdicts-and-baselines).

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

## No silent vacuous passes

Load-time validation proves a config value is *usable*. It cannot prove the
project has anything for that value to act on — a `kinds` filter naming a
kind no document carries, an `acyclic_relations` entry no document links on,
a `stale_days` threshold in a corpus where nothing declares `reviewed:`. Each
loads clean, runs, and reports green over an empty population, which is
indistinguishable from enforcement.

So a rule reports its reach as well as its findings. `Rule::check` returns
`RuleRun { subjects, unjudged, violations }`, and every check / issues response carries
`rule_coverage` alongside `skipped_rules` — together a total census of the
registry. `subjects` is the population the rule **guards**, never the
offending subset and never the slice that changed on this run, so zero has
one meaning everywhere: this rule is in effect over nothing.

The count leaves the same pass as the violations, never a second traversal —
a reach that could disagree with the verdict would be worse than no reach at
all. A rule that judges a record against a prior state guards only what it
can be judged against: a diff carries its per-node channels over the ids both
snapshots hold, so a record the baseline has no node for is outside the
population however it looks now. What the scope selected and the rule could
not judge is reported beside the reach (`unjudged`), so one response carries
both without anything having to attribute a missing prior state to a cause.
It is a difference, not a defect, and what a non-zero points at is the
rule's own question: a lock reading against a baseline counts a record
authored since it, which costs nothing, alongside one whose baseline record
went missing, which does — and cannot tell them apart. Each rule's cause is
what names the remedy.

A rule judging steps counts the same way per step: `status_entry` guards every
record a step holds under the flow — one a parent already held has its answer
— and `status_transition` the records some step held on a parent, `unjudged`
counting the ones that only ever entered. Both count as `unjudged` a record
the flow governs that the step the run ends at does not carry — a document
git ignores, which no commit can record — however an earlier step judged it,
since what a past step judged is no answer for where the record stands now. A document a commit could not parse
is read back from the commit before the change that broke it rather than
counted apart, because a count is all an unjudged record would leave behind,
and a later step holding the same id would hide it. Where that reading is cut
off — a shallow clone whose earlier state lies beyond its cut, or a commit
whose tree the build refuses, which the envelope names (`history_unread`) —
the records arriving at that step are counted rather than judged, and that
count is the one thing no later step takes back.

## Symmetric guards

Security/safety checks must apply uniformly across all mutation points. When guarding one command (e.g., migrate skipping symlinks), apply the same guard to every other command that touches the same resource. Pattern: Core library functions enforce the guard so no handler can forget it.
