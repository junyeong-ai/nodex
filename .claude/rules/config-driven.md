# Config-Driven Design

All project-specific behavior must come from `nodex.toml` — never hardcode domain logic.

## Semantic config items

Every semantic behavior is declared once, read many times:

**Vocabulary & Status:**
- `kinds.allowed` — document type vocabulary; must include "generic" (fallback)
- `statuses.allowed` — document lifecycle states (active, archived, etc.)
- `statuses.terminal` — states that block further transitions (gates lifecycle)
- `statuses.initial` — status for tool-written documents and frontmatter-less parses (optional; must be in `allowed`; absent → first `allowed` value)
- `statuses.flow` — one declared lifecycle: `transitions` (the targets each status may move to) and `initial` (where a governed kind starts, falling back to `statuses.initial`) over `kinds` (which kinds have that lifecycle at all; empty = every kind). `initial` is read by `scaffold`, `migrate` and the frontmatter-less parse for those kinds, through the one seam `Config::initial_status_for`, so a tool-written document satisfies `status_entry` by construction rather than by two agreeing derivations. Optional; omitted, nothing is judged. Declared, it is the one place the flow is written and `Config::validate` holds the declarations to each other, scoped to the statuses the flow **names**: a terminal one names no transition, every other names at least one, each is reachable from the entry point, and each is admitted by every kind the flow governs (asked per kind, never over their union, since a declared move writes the status onto any of them). A flow answers for nothing outside that set, which is what lets a kind-scoped flow ignore the statuses other kinds use; what catches dead vocabulary is a separate guard — a status no flow names and no ungoverned kind may hold could never be carried. It registers `status_transition` (a move the flow does not name, a move out of terminal included) and `status_entry` (a record entering the flow at anything but its entry status — written here, moved under a path-derived id, re-keyed, given a governed kind, or restored after a commit that deleted it, which read the same because a node is its id; a document a commit could not parse holds the record it held before the change that broke it, read at the path it stands on; a document git ignores takes no step, since no commit can hold it). Both judge git's history a step at a time (`Rule::judges_steps`, `RuleContext::steps`), never `rules.immutable_baseline`'s: every run judges the uncommitted change against `HEAD` and any `MERGE_HEAD`, and `check --since` also each commit the heads reach and the ref does not, against its parents, a merge on what each line moved since they last agreed rather than on where its own tree differs, counting rather than judging a record whose lines share no commit, whose readings disagree or are governed only in part, whose places of agreement disagree about it, or whose answer sits in a commit the build refuses — so a range answers what a gate on each of its commits would under today's config, a past step finding is not re-judged by a later plain run, and a write seam judges the step its write would commit (`BaselineProbe::undeclared_move`, whatever the document carries uncommitted). `kinds` is load-bearing rather than convenience — a kind written live has no acceptance event, and a flow without the filter would demand a promotion step it never has

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
- `rules.frontmatter_immutable[] / body_immutable[]` — diff-aware locks. Each block's `trigger` = `terminal` | `creation` | `status`, read by both families through the one seam `Config::lock_arms`, the last naming its own arming set in `statuses` — required there, refused on the other two — because `statuses.terminal` is read by `statuses.flow` validation, `conditional_exclude`, trust, the `terminal` trigger, the lifecycle seam, `git_drift`, orphan and stale detection and the `GRAPH.md` report, so a lock cannot borrow that word to arm at a status the project has not finished with — and where a flow declares a move out of the status borrowed, the config stops loading rather than behaving differently. A trigger other than `terminal` is what settles a field the rest of the config reads: `kind` decides which lifecycle a record answers to and which kind-scoped rules see it at all, and at `terminal` it is settled only after every one of them has been asked. Where a `statuses.flow` governing a kind the block locks is declared, load proves two things about the arming, asked through that same seam so a proof is a proof about what the rules do: no transition leaves it, so no status edit can disarm the lock — `terminal` and `creation` hold that already, one because a terminal status declares no way out and the other because it arms everywhere — and where the block locks `status` itself, no transition moves a document while it is armed, so the lock cannot refuse a move the flow declares, which is what leaves `status` lockable at `terminal` and not from `creation`. `append_section` confines an `append_only` block's growth to the section a markdown heading opens, which must end the body
- `rules.immutable_baseline` — default git ref `check` diffs against when `--since` is omitted (enables the immutability locks by default; never narrows the violation set)
- `rules.acyclic_relations` — relations whose edge graph must stay a DAG (default `["implements"]`; every entry must be a known relation; empty list rejected)

**Scoring & Queries:**
- `trust.weights` — composite score components (status, freshness, drift, backlinks); renormalised over what the run could measure, and *undefined* where the run can measure a positively-weighted component the document declares no input for — a zero weight is how a project says it does not track that axis
- `trust.overrides[]` — per-kind weight tuning (first-match lookup; replaces global entirely)
- `similarity.weights` — `query similar` ranking (title, tags, kind, directory, linked); composite renormalised over the signals the *target* carries, never over what a candidate lacks
- `similarity.default_limit` — results per query (must be ≥1)
- `similarity.title_stop_words` — tokens dropped from a title before the `title` component compares two of them; declaring the list replaces the built-in English one entirely, because a corpus whose titles are domain terms has its own idea of which words carry no signal
- `search.weights` — `query search` keyword ranking (`id`/`title` each with an exact + partial tier, `tag`); additive (a node's score is the sum of its matched fields), not renormalised — finite, non-negative, positive-sum
- `report.*_display_limit` — per-section entry caps in `GRAPH.md` (`god_node` / `orphan` / `stale`); each must be ≥1 (0 renders an empty section — rejected at load, like `similarity.default_limit`)

**Detection & Orphan Handling:**
- `detection.stale_days` — threshold for stale doc detection (omit the field to disable; 0 rejected at load). Omitting also drops the trust composite's `freshness` component — freshness is measured against this horizon
- `detection.git_drift_threshold` / `git_drift_relations` — commits-since-review drift gate (None = disabled; 0 rejected at load) and which relations it measures (`git_drift_relations` is validated at load — non-empty, no duplicates, every entry a known relation — regardless of whether the threshold is set, mirroring `acyclic_relations`)
- `detection.orphan_grace_days` — exempt new docs for N days (0 = immediate check)
- `detection.orphan_ok_kinds[]` — kinds that are leaf-by-design (never orphan). With `orphan_grace_days`, the per-node `orphan_ok` flag and terminal status, these are the four exemptions that decide the `orphan` rule's population — the rest of the corpus is guarded, and the reach says how much that is
- `detection.unresolved_policy[]` — ordered first-match-wins (cause, relations?, glob?) → severity rows classifying unresolved references (`error` = check rule `unresolved_reference/<name>`, `warning` = counted fallthrough, `info` = reported out of total; declaring replaces the default `excluded_target` info row). The three axes are what a project has to separate two references that fail identically: `relations` is the only one that tells a structural edge from a prose citation naming the same dead id, and it is where `superseded_by` is nameable — the relation only an unresolved edge carries, kept out of `known_relations` so no traversal, cycle check or drift measurement can filter on a relation no resolved edge has. `glob` matches the names the resolution *sought* — the node id for an id relation, the normalized resolution candidates for a document reference — never the raw target, and is refused at load on the causes that were refused before anything was looked up (`escapes_source` / `absolute`). Rows are first-match-wins, so a row an earlier one already covers is refused at load: it could never classify an edge, and an error row that never fires is the vacuous pass in its worst form — `rule_coverage` reports the whole offer as its reach and no violations, which is exactly what a thorough pass looks like

**Extraction & Safety:**
- `parser.link_patterns[]` — custom link extraction (exactly 1 capture group; duplicate (pattern, relation) pairs rejected; the relation must not be a code-fixed-resolution built-in — path-only `covers` and id-only `supersedes`/`superseded_by`/`implements`/`related` are rejected at load, `references` is legal)
- `parser.wikilink_enabled` — enable [[wikilink]] syntax
- `parser.extensions[]` — link target validation extensions
- `scope.include/exclude` — file scope inclusion/exclusion patterns
- `scope.follow_symlinks` — whether the walk descends a directory reached through a symlink (default `false`, matching git/ripgrep/fd/find). Off keeps the path space a tree, so every path-keyed rule has exactly one path per document; each undescended link is reported on the build result. On admits every name a document is reachable under and keeps one document per directory entry
- `scope.include` entries are a glob, written bare (`"docs/**/*.md"`) or as a table (`{ glob = "specs/**/*.md", may_be_empty = true }`). `may_be_empty` — also on `identity.kind_rules` / `identity.id_rules` entries — says selecting nothing is a state the project expects, and is read by the coverage disclosure alone: the walk never sees it, because a scope that behaved one way and disclosed another is the defect the attribute exists to avoid. It silences only that declaration's own "matched no files" — never a sibling declaration's, never the unclassified-document report, never the boundary a walk did not cross, and never a scan that read nothing at all, which is a statement about the project rather than about any pattern. Where the message carries a cause (a `prune_dirs` entry the pattern's path lies under), the cause rides the message it silences, not a carve-out: the walk can miss a pattern in more ways than there are detectors for it, and reporting the one with a detector while staying silent on the rest would be an asymmetry that grows with each new way
- `scope.prune_dirs` — directory basenames pruned from the walk at any depth (default `["node_modules","__pycache__","target",".git",".venv"]`; plain segments, no globs/separators, empty list prunes nothing; dot-prefixed trees stay caught by the hidden-path guard regardless)
- `scope.conditional_exclude[]` — drop the sub-artifacts governed by a terminal parent (`parent_glob` selects the parent, `child_glob` selects which paths are derivative; only `child_glob` matches are excluded; the dropped paths are reported on the build result, and the write that drops them names them as `document_evicted`; a `child_glob` match the same rule reads as a terminal parent is spared, and reported beside them as `conditionally_kept`, because what a rule kept back is not derivable from what it removed). The **directory subtree** is the unit: one terminal parent governs the `child_glob` matches beside it, including the sub-artifacts of parents that are still live, because nothing pairs a child with one parent by name. A project needing per-record eviction gives each record its own directory. A `child_glob` match the same rule also reads as a terminal parent is the exception, and is kept — a `parent_glob` broad enough to match the derivatives declares each of them a record, and a record whose own terminality is causing drops cannot be among them without taking the reason for them out of the graph. A project that means its derivatives never to be records says so in `parent_glob`; nothing else distinguishes the two. Saying it takes a form the derivative names cannot satisfy — a wildcard crosses a dot, so `adr/[0-9]*.md` still matches `0001.notes.md`, while a fixed width, an alternation over the widths in use, or a negated class where the names differ (`adr/*[!s].md`) does not. Pattern-level negation is not among them: a leading `!` is a literal to globset, so that spelling selects nothing and the rule goes inert. Every form that does work separates on a naming accident and is silently wrong for the next name added, so the per-record directory is the answer that does not depend on one. Rules compose as a union and their declaration order is not significant — each keeps only the parents *it* read a status from, so a document another rule calls a parent is an ordinary candidate here
- `annotations[]` — body-text marker extraction (name-keyed: each block's unique `name` is the stable lookup id in JSON output and CLI filters)

**Artifacts & Tooling:**
- `output.dir` — project-relative directory nodex writes its own artefacts into (`graph.json`, `cache.json`, `GRAPH.md`; default `_index`). Folded into nodex's path language once as it is read, so the traversal guard, the scan's self-exclusion glob, every writer's join and the path `status` reports are one value and the same `nodex.toml` resolves the same way on every platform
- `meta.nodex_version` — SemVer requirement the running binary must satisfy to **write** the project's documents (validated as a requirement at load): reads run and attach `binary_compat`, document-writing commands refuse with `VERSION_MISMATCH`. A pin is a value that ages, so nothing states one as a literal — `nodex init` writes the running binary's own

**Decision:** "Does this vary by project?" → Yes = config, No = code.

## Self-consistency invariant

Tool-written documents (scaffold, migrate, lifecycle) must pass the same config's check. Enforce by one of:
- Rejecting incompatible config shapes at load time (`Config::validate`), or
- Deriving tool output from config (cannot produce out-of-vocabulary values), or
- Validating a user-supplied value at the command's write seam, when validity depends on the document being acted on (`lifecycle set --status` refuses a status the kind's vocabulary rejects, or a terminal state forbids leaving)

The backstop under all three is `mutate::introduced`: every write seam asks it before writing and refuses on the Error-severity violations the proposal *introduces*. The guards above are preconditions with remedies of their own; the gate is what makes the invariant complete, because the rules a mutation can break are the whole registry rather than the family a seam was written against. It is also the limit: a seam refuses **exactly** what the project's own config makes an error — the same mutation under a config that only warns must succeed, and a rule that cannot fire on a document (out of scope, no node) cannot refuse a write touching it.

*Exactly* is a statement about the unit as much as the set. A lock names a part of a document — `frontmatter_immutable` a field, `body_immutable` the body — so that is what a refusal may cost, and `mutate::narrow` holds back the parts and writes the rest: a frozen ADR body keeps the citation that records what was true when it was written, while the same file's `superseded_by` is repointed, because those are two claims and only one of them is frozen. Held back whole is reserved for what narrowing cannot reach — a finding about the document rather than a part of it, and a record already drifted from its frozen baseline, where the verdict is absolute and the remedy is to fix the drift. The verdict is taken over the bytes that will land rather than inferred from the verdict on the bytes that will not, because reverting a part to what the file carries is not the same as putting it back the way the baseline has it.

That limit is also the gate's blind spot, and it is answered rather than accepted. A refusal is a delta over the population `check` runs on, so a write that *removes* a document from that population produces a smaller report and nothing else — the findings leave with the document. `mutate::evicted` carries what the delta cannot: `scope.conditional_exclude` is the one membership rule a document's content moves, so a write that puts a terminal document in the parent slot — changing its status, or moving one already terminal there — is the write that drops its sub-artifacts, and it names them on the envelope (`document_evicted`) rather than refusing them. Refusing would be the wrong answer twice over — the eviction is what the rule was declared to do, and `check` reports nothing about a document outside the project, so the operator would have no reading to clear it by.

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
`RuleRun { subjects, violations }`, and every check / issues response carries
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
