# nodex — `nodex.toml` reference

Read this when authoring or debugging a config. `nodex init` writes an annotated starter, which documents `[trust]`, `[similarity]` and `[search]` key by key and lists `[report]`'s defaults; `reference/minimal-config.toml` beside this file is a worked minimal example. `nodex export config` and `nodex export rules` show what the project actually resolved to.

## Load-time rejections

Each of these is a real `CONFIG_ERROR` at load, not a silent no-op:

- `schema.types` values are `string | integer | bool | date` only, and only project keys take one — every built-in field (`reviewed`, `owner`, `tags`, `related`, …) is typed by the parser and refused.
- `identity.id_rules` templates take `{kind}`, `{stem}`, `{parent}` and `{path_slug}`; any other placeholder is refused.
- `schema.required` takes authored fields only — `id` / `title` / `kind` / `status` / `orphan_ok` are parser-resolved and refused.
- `schema.enums` values are string arrays; a bare TOML integer is rejected. Quote numeric vocabulary: `["1", "2"]`.
- `default_limit` sits under `[similarity]`, not `[similarity.weights]`.
- `parser.extensions` entries carry the leading dot.
- `[[annotations]]` patterns need a named capture matching `key`.
- Narrowing `statuses.allowed` means declaring `statuses.terminal` too — every terminal status must stay allowed.
- `[statuses.flow]`, when declared, answers for the statuses it **names** — its entry point, its keys, its targets: no way out of a terminal one, a way out of every other, all reachable from the entry point, and each admitted by every kind the flow governs. Separately, a status no flow names and no ungoverned kind may hold is refused as vocabulary nothing could carry.
- A `kinds` entry on any per-block rule must be in `kinds.allowed`, so a typo can never become a silent never-fire.

With `parser.wikilink_enabled = true`, a `[[...]]`-shaped annotation marker is **also** parsed as a wikilink and surfaces as an unresolved edge in `query issues`. Use a non-bracket marker syntax if you want annotations only.

An annotation pattern reads every line outside a code block, inline code included, so a corpus may write its markers as code (`` `[PROMOTES: x]` ``). To keep an example of its own syntax from declaring a marker, anchor the pattern where examples do not stand: the `superseded-ok` block in the `nodex init` template reads a marker only on a line starting with `<!--`, so the syntax shown in inline code declares nothing.

## Scope

`scope.include` / `scope.exclude` are glob lists. An `include` entry is written bare or as a table:

```toml
[scope]
include = ["docs/**/*.md", { glob = "specs/**/*.md", may_be_empty = true }]
```

`may_be_empty` — also accepted on `[[identity.kind_rules]]`, `[[identity.id_rules]]` and `[[scope.conditional_exclude]]` — says that selecting nothing is expected, so a declared area that is idle between milestones stops emitting a `scope_coverage` warning on every command. It is read by the disclosure only, never by the walk. It silences that declaration's own "matched no files" and nothing else — not a sibling's, not the unclassified-document report, not an undescended symlink, and not a scan that read *nothing at all*, which is a fact about the project rather than about a pattern.

`scope.prune_dirs` names directory basenames pruned at any depth (default `["node_modules", "__pycache__", "target", ".git", ".venv"]`; an empty list prunes nothing).

Dot-prefixed paths (`.draft.md`, `.archive/`, `.claude/`) are skipped unless an include pattern **requires** the dot at that position. `.claude/**/*.md` opts `.claude` in, as do the spellings globset treats as the same literal (`\.claude`, `[.]claude`) and a pattern that can only match a hidden entry (`.*/**/*.md`). A wildcard that merely *matches* one (`**/*.md`, `?claude/**`) does not. Dot-prefixed trees stay caught by the hidden-path guard regardless of `prune_dirs`.

A directory reached through a **symlink** is not descended unless `scope.follow_symlinks = true`. The default matches `git` / `ripgrep` / `fd` / `find` and keeps the path space a tree, so every path-keyed rule (`include`, `exclude`, a `conditional_exclude` `parent_glob`, an `identity` glob) has exactly one path to key on. Each undescended link is named in the build's `unfollowed_paths`.

Turn it on for a project whose documents live behind a link — a vendored tree linked into `docs/`. The scan then admits every name a document is reachable under, keeps one document per directory entry, and reports it under the smallest admitted name, at a traversal cost that grows with nested links. Each unused name appears in `aliased_paths` paired with the name in use, and a write seam naming an unused one is refused with the path to use instead. A symlink to a *file* is read wherever it points either way.

`[[scope.conditional_exclude]]` drops the sub-artifacts a terminal parent governs: `parent_glob` selects the parent, `child_glob` selects which paths are derivative, `condition = "status_terminal"`. Only `child_glob` matches are excluded. The dropped paths are reported on the build result, and both the write that makes the parent terminal and the `check --content` gate that previews it name them as `document_evicted`.

The unit is the parent's **directory subtree**, not the individual record. Nothing pairs a child with one parent by name, so in a flat directory of records — `parent_glob = "docs/adr/*.md"`, `child_glob = "docs/adr/*.notes.md"` — superseding one ADR drops the other ADRs' notes too, live ones included. Give each record its own directory when you need per-record eviction; there the subtree is the record and the rule is exact.

Note what that `parent_glob` also says: `docs/adr/*.md` matches `0001.notes.md` as readily as `0001.md`, so every notes file is a record this rule may read a status from. A notes file that is *itself* terminal is therefore one of the parents, and parents are kept — superseding an ADR together with its notes leaves those notes graphed while the live ADRs' notes leave. The build names both halves: the departures under `conditionally_excluded`, and the spared ones under `conditionally_kept`. The rule has no other way to tell a record from a derivative, and a `child_glob = "**/*"` project depends on the parent being kept.

To exclude the derivatives from `parent_glob` you need a form their names cannot satisfy, because a wildcard reaches straight through a dot: `docs/adr/[0-9]*.md` still matches `0001.notes.md` (the class takes `0`, the star takes `001.notes`). Several forms do work — a fixed width `docs/adr/[0-9][0-9][0-9][0-9].md`, an alternation over the widths in use `docs/adr/{[0-9][0-9][0-9][0-9],[0-9][0-9][0-9]}.md`, a negated class where the two names differ `docs/adr/*[!s].md`. What globset does not have is *pattern-level* negation: a leading `!` is a literal, so `!docs/adr/*.notes.md` compiles, matches nothing, and leaves the rule inert (the build reports the glob as having selected nothing).

Every one of the working forms separates on an accident of how the two are named — a width, the set of widths, a character one has and the other lacks — so each is exact for the corpus it was written against and silently wrong for the next name added to it: `*[!s].md` also drops `0001-metrics.md`, and the fixed width stops seeing a record the day someone numbers one `002`. Giving each record its own directory is the answer that does not depend on a naming accident; there the subtree is the record and the question does not arise.

Several rules compose as a union, and the order they are declared in does not change the result: each contributes the paths it drops, and each keeps only the parents *it* read a status from. A document some other rule calls a parent is an ordinary candidate here.

`document_evicted` reports a **delta** — what this write removes from the project — so in that flat directory the *first* archive names every ADR's notes at once, and every later archive says nothing, because there is nothing left to remove. That is the same warning going quiet, not a missing one: the standing answer to "what is on disk but outside the project" is the build's `conditionally_excluded`.

## Body links

Standard markdown (`[text](path.md)`) by default. Wikilinks (`[[id]]`) opt in via `parser.wikilink_enabled = true`.

`parser.link_patterns` declares arbitrary syntaxes. Each block needs a `pattern` with **exactly one** capture group and a `relation`. Any relation name is legal except the built-ins with code-fixed resolution, which are rejected at load: `covers` (path-only) and `supersedes` / `implements` / `related` (id-resolved) are declared through their frontmatter fields only. `references` stays legal.

A block may set `code_spans = true`: an inline code span whose **entire content** the pattern matches is then a citation on both the extraction and the rewriting side, so a corpus writing ids as `` `adr-001` `` is reachable as edges and `retarget` repoints them. A span is matched as its own text, so `^` / `$` mean the span and the backticks are never part of what the pattern sees; what the match leaves over is what keeps a span code (`` `just adr-tool` `` cites nothing). Code *blocks* stay opaque unconditionally. The field defaults off.

Path resolution: a link opening `./` is read from the directory its document is in, never from the project root — the marker names the frame. Segments that name nothing are dropped before any rung is tried, so `docs//x.md` and `docs/./x.md` are both `docs/x.md`, while `.//x.md` still says its frame. `..` is kept and resolved by the frame that read it. A leading `/` is refused with `cause: absolute`.

## Schema rules

`[schema].mode = "strict"` rejects any frontmatter key that is neither built-in nor declared in `types` / `enums` / `required` / `cross_field` or a per-kind `forbidden` — this is what catches `relatd:`. Default is `lenient`.

`[[schema.cross_field]]` predicates take four forms:

| `when` | meaning |
|---|---|
| `"field=value"` | equality |
| `"field in {v1,v2,v3}"` | membership |
| `"field exists"` | presence |
| `"field not_exists"` | absence |

Scalar predicates (`=`, `in`) are rejected on collection fields (`tags`, `covers`, …) at load; use `exists` / `not_exists` for collection presence.

`schema.require_explicit` names inferrable built-ins (`id` / `title` / `kind` / `status`) a document must author rather than inherit from a fallback. An inferred or empty named field reds `check` via `explicit_field`. This is what makes a `check --content` verdict certify those keys are spelled out.

`schema.overrides` applies per-kind required fields, type changes, enum changes and cross-field checks, and `forbidden` names fields documents of those kinds must not carry (`forbidden_field`). A field counts as carried when `required` would call it present, so `covers: []` reads as absent and passes. Load refuses a `forbidden` entry the same kind requires, one the parser resolves for every document (`id` / `title` / `kind` / `status` / `orphan_ok`), and one a `cross_field` of that kind requires — unless its predicate cannot hold on a document of the kind: it excludes the kind (`when = "kind=runbook"` for `learning`), asks for a field the kind forbids to be present (`exists`, `=`, `in`), or asks for a parser-resolved field to be absent. In the same override, a `types` / `enums` entry or a `cross_field` predicate that needs a forbidden field present is refused too: it could only apply to a document already red.

## Built-in rule ids

`parse_failure` (node-less, one per dropped in-scope document) · `field_parse` (one per wrong-typed built-in field on a present node) · `required_field` · `forbidden_field` (registered only when an override declares `forbidden`) · `field_type` · `field_enum` · `cross_field` · `unknown_field` (registered in strict mode only) · `explicit_field` (registered only when `schema.require_explicit` is set) · `stale_review` (registered only when `detection.stale_days` is set) · `orphan` (always registered; its exemptions narrow the population, and a corpus they cover entirely reads `subjects: 0`) · `superseded_reference` (always registered; its population is the citations live documents make, less `supersedes` itself, those of `detection.superseded_reference_ok_kinds`, body citations of a target the citing document names in a `detection.superseded_reference_ok_annotation` marker, and any read from a part an immutability lock holds in the run; a run with no baseline cannot tell, and counts the citations a lock could hold as `unjudged` until `--since` or `rules.immutable_baseline` supplies one) · `git_drift` (registered only when `git_drift_threshold` is set) · `status_transition` / `status_entry` (registered only when `[statuses.flow]` is declared) · `filename_pattern` (registered only with a `[[rules.naming]]` block) · `sequential_numbering` / `unique_numbering` (only when a naming block sets `sequential` / `unique`) · `acyclic_relation` (always on; the relation set is config-driven via `rules.acyclic_relations`, default `["implements"]`, and its reach counts those relations' edges — a `supersedes` cycle never reaches it, because `build` refuses one with `CYCLE_DETECTED`).

Config-driven ids: `body_line/<name>` · `body_immutable/<name>` · `frontmatter_immutable/<name>` · `unresolved_reference/<name>`.

## Vocabulary rules

`[[rules.body_line]]` — per-line vocabulary conformance. Each block declares a regex with named captures; every match outside a code block must carry capture values from declared enums. One violation per failed (line, capture). Lines that do not match the pattern are ignored.

`[[rules.naming]]` — filename patterns, path-scoped: it carries `glob`, not `kinds`.

The content-scoped per-block families — `body_immutable`, `frontmatter_immutable`, `body_line` — and `[[annotations]]` accept an optional `kinds` list. Empty means no restriction; otherwise the rule fires only on nodes whose `kind` appears in it.

## Diff-aware rules

`frontmatter_immutable` and `body_immutable` need a before-state. They get it from `--since <ref>`, from `rules.immutable_baseline`, or, under `check --content`, from the working tree the proposal would replace. Without any of these they self-report in `skipped_rules` with a reason — silent non-fires are forbidden.

```toml
[rules]
immutable_baseline = "origin/main"
```

That is the default ref `check` diffs against when `--since` is omitted, so the locks are enforced on a plain `nodex check`. Unlike `--since` it never narrows the report to what the diff answers for — it only supplies the before-state.

When the baseline cannot engage — the project is not in a git work tree, or the ref carries nothing for the project — the run proceeds with a `baseline_inert` warning and the rules land in `skipped_rules`. The same advisory rides every mutating command, so a write whose locks were never enforced never reads as clean. A ref git cannot resolve at all is refused outright with `CONFIG_ERROR` by reads and writes alike, `check --content` included, so an unreadable baseline cannot let the pre-write gate clear an edit the write would refuse. A repository with no commits yet is inert instead, so a project can be scaffolded before its first commit.

A CI checkout that lacks the ref — a shallow clone without `origin/main` — fails every baseline-resolving command this way; fetch the ref first.

```toml
[[rules.frontmatter_immutable]]
name = "identity"
fields = ["kind"]
trigger = "creation"     # settled by the commit that creates the record

[[rules.frontmatter_immutable]]
name = "supersession"
fields = ["superseded_by"]
# kinds = ["adr"]        # optional; empty = every kind
```

Freezes declared fields once the block's `trigger` engages — gated on the diff's *before* status, so the write that first arms the lock is allowed and only later edits lock. `id` is rejected at load because every armed lock already protects identity; `status` is accepted and enforced via the status-transition stream. Names must be unique across blocks.

Reach for a trigger other than `terminal` when the field decides how the rest of the config reads the record. `kind` is the one every kind-scoped rule reads first — `statuses.flow`, the schema overrides, the locks' own `kinds` filters — and at `terminal` it is settled only once the record is finished with, which is after all of them have been asked. Locking it from `creation`, or at the statuses the project accepts a record at, settles it while the answer still matters. A write that takes a record out of a block's kinds is judged by the block that held it, because the `kinds` filter reads the before frame too.

```toml
[[rules.body_immutable]]
name = "adr-decisions"
mode = "frozen"          # any body edit → violation
trigger = "creation"     # locked from the first committed snapshot, status notwithstanding
kinds = ["adr"]

[[rules.body_immutable]]
name = "runbook-history"
mode = "append_only"     # the locked body must remain a prefix of the new body
kinds = ["runbook"]      # trigger omitted = "terminal"
```

`append_section = "## Corrections"` (with `mode = "append_only"` only) confines growth to the section that heading opens: every non-blank appended line must fall inside it, nothing may follow it at its heading level or above, and no appended line may belong to a link reference definition a committed reference resolves to — a record takes corrections while everything committed above them reads as it did. The correction's content is not judged; that stays a review decision. Headings match by level and text as the markdown parser reads them; one inside code, a quote or a list opens no section. A violation's `details.append_section` names the section and `details.refusal` what to undo: `rewritten` (a committed line changed — restore it), `outside_section` (something landed before the section, or a heading at its level closed it — move it inside), or `redefines_reference` (an appended `[label]: …` definition resolves a reference on a committed line — rename the label). Read the heading from `nodex export rules` (`params.append_section`) and gate the appended entry with `nodex check --content <path>=-` before writing it.

`body_immutable` is driven by per-node body fingerprints computed at build time, so no file is re-read at check time.

### Triggers

Both lock families read the same `trigger`, in the diff's before frame, so the single write that first arms a lock may set what that lock covers in the same edit.

| `trigger` | Arms at | `statuses` |
| --- | --- | --- |
| `terminal` (default) | every status in `statuses.terminal` | refused |
| `status` | the statuses the block names | required |
| `creation` | every status, from the first committed snapshot | refused |

`creation` is the immutable-from-day-one contract: on a body it freezes the record while frontmatter including `status` stays editable for supersession.

`status` is for a record editable while it is a draft and fixed once the project adopts it:

```toml
[[rules.body_immutable]]
name = "adr-body"
mode = "frozen"
trigger = "status"
statuses = ["active", "superseded", "archived"]
kinds = ["adr"]
```

Reach for it rather than moving a status into `statuses.terminal`: that word is also read by `statuses.flow` validation, `conditional_exclude`, trust scoring, the `terminal` trigger, the lifecycle write seam, `git_drift`, orphan and stale detection, `superseded_reference`, and the `GRAPH.md` report, so arming a lock through it declares the record finished to every one of them. The first is not a behaviour change but a refusal — a flow declaring a move out of that status stops loading.

Declare `[statuses.flow]` over the same kinds and load proves two things about the arming, both `CONFIG_ERROR`s naming the pair. No declared transition may leave the arming, or a status edit would disarm the lock. And where the block locks `status` itself, no declared transition may move a document while it is armed, or the lock would refuse a move the flow calls legal and no document could satisfy both — which is why locking `status` fits `terminal`, where a terminal status declares no move, and not `creation`, which arms everywhere. For a kind no flow governs there is nothing to prove against: a status edit can disarm the lock, and the project has declared no lifecycle that would say otherwise.

## Status flow

`[statuses.flow]` declares a lifecycle — which statuses follow which, over the kinds that have that lifecycle. Omit it and nothing is judged; declare it and two rules register:

```toml
[kinds]
allowed = ["generic", "adr"]

[statuses]
allowed = ["proposed", "active", "superseded", "archived"]
terminal = ["superseded", "archived"]
initial = "proposed"

[statuses.flow]
kinds = ["adr"]          # empty = every kind
initial = "proposed"     # where a governed kind starts; omitted = statuses.initial
transitions = { proposed = ["active", "archived"], active = ["superseded", "archived"] }
```

`status_transition` refuses a move the flow does not name, including any move out of a terminal status — the refusal the `lifecycle` write seam already gives, now reaching an edit that did not go through it. `status_entry` refuses a document authored into anything but the flow's entry status, which is the one way around a transition check: a record born accepted never transitioned.

`kinds` is what keeps a lifecycle from being invented for a kind that has none. An ADR is proposed and then accepted; a runbook is written and is live from that moment, and judging it against the ADR flow would demand a promotion step it has no event for. A kind outside the filter is judged by neither rule and keeps whatever status it is authored at. The guards follow the same scoping: a status only an ungoverned kind can hold needs no way out and need not be reachable, and a `trigger = "status"` lock is only held against a flow that governs a kind it locks.

`initial` is the flow's entry point, and it is what `scaffold`, `migrate` and the parse of a document declaring no status write **for the kinds it governs**. Every other kind keeps `statuses.initial`. That is what lets a project adopt a lifecycle for one kind without moving the status every other kind is created at. Omit it and the flow falls back to the global, which load then holds to the same reachability proof — a flow whose own statuses cannot reach the global initial is refused.

Both rules judge history **a step at a time**, against git rather than `rules.immutable_baseline`. Every `check`, `query issues` and write seam judges the uncommitted change — the staged one under `check --staged` — against `HEAD` — and every `MERGE_HEAD` while a merge is under way; `check --since <ref>` also judges each commit the heads reach and `<ref>` does not, against its parents. A range reports what a gate on each of its commits would under today's config, so authoring at `proposed` and accepting a commit later passes, and a detour through an undeclared status is reported at the commit that took it, even when the range ends where a declared move would. A merge is judged like any other step — a resolution in favour of one line leaves a tree that line's parent already had, and is still where the other line's record moved — and stands on what each line moved since the parents last agreed, every place they agreed where they agreed in more than one; those places are read only where the lines disagree about some record. A line that did not touch the record makes no claim about it, so a branch forked before a move cannot carry the old status back. Where a step cannot be read unambiguously — lines sharing no commit (an `--allow-unrelated-histories` merge), places they agreed that disagree about the record, a record read more than one way or governed only in part, a commit the build refuses, or history a shallow clone cut off — the record is counted in `unjudged` rather than judged. The last two also ride a `history_unread` warning naming what could not be read: a refused commit with the build's refusal (two documents whose ids resolve the same way while a merge holds both, say), which nothing short of rewriting that commit makes readable; history a shallow clone cut off with the fetch that restores it. The first three leave only the count. `details.commit` names the step; it is absent for the uncommitted change. A step finding is about a commit, so a plain `check` does not re-judge history — a past finding belongs to the `--since` range that holds it. `lifecycle` and `scaffold --force` refuse a move the write would commit from `HEAD` as `INVALID_TRANSITION`, whatever the document carries uncommitted — including a write that changes no byte, since a status hand-written into the working tree and then confirmed through the seam commits the move `HEAD` records. Where the step's priors are among what the run counts rather than judges, the seam refuses nothing either; the document's own status is the prior only where no commit can hold the record — outside a work tree, or at a path git ignores. `check --content` has no commit to step from and reports both rules skipped, as does any run outside a git work tree.

In each step a governed record the flow also governed on a parent is `status_transition`'s — its status must equal a parent's or be a declared move from one — and one it governed on no parent is `status_entry`'s. That second set is what **entering the flow** means, and a node is its id, so every way in reads the same: written here, moved under an id that follows its path, re-keyed, given a governed kind, or restored after a commit that deleted it. A document a commit could not parse holds the record it held before the change that broke it, read at the path it stands on, so one broken and repaired is judged as the record it was and one never readable before its repair enters there. A commit that both moves a document and leaves it unparseable takes the path's history with it, and the repair reads as an arrival — move it in a commit of its own. Where a shallow clone cuts that reading off, or a commit's tree is one the build refuses under today's config (`history_unread` names either), the records that may have stood there are counted `unjudged` rather than judged; a clone fetched with `fetch-depth: 0` judges the first. A document git ignores takes no step, since no commit can hold it, so both rules count it `unjudged`. A branch with no commit yet enters every record its first commit carries. The finding names the remedies — author at the entry status and move in a later commit, or give a document that already existed its record back by anchoring `id` in frontmatter or moving it with `nodex rename`, which anchors it. A record leaving the governed kinds is judged by neither rule.

The history judged is the one the repository keeps: a squash merge enters a record authored and accepted on the branch at its accepted status, so a squash-merging repository accepts in a PR of its own; a `--since` range a shallow clone cuts is refused (`GIT_ERROR`) rather than read as every record arriving — fetch it (`fetch-depth: 0`); and an amended commit records its step from `HEAD^`, while a `check` run before `git commit --amend` judged it from `HEAD`. Each run builds `HEAD`'s snapshot, every `MERGE_HEAD` and every place the lines agreed while a merge is under way, and each commit in a `--since` range one more; a document a commit could not parse costs one per commit the read-back crosses, answered once each however many commits stand on it. A run with no applicable step rules reads no such history. **One flow per project**: a second lifecycle would be a config-key change, deferred because `kinds` already expresses the requirement that arrived (kinds with *no* lifecycle) and the guards are already scoped per kind.

### Locks are identity-scoped

Locks judge every commit in an explicit `--since` range and its uncommitted step. Reverting an illegal edit does not erase its finding; `details.commit` names the offending commit. Merges compare protected state: unrelated edits and moves do not select a different locked revision. A frozen merge step permits either independently armed revision, while the result must also satisfy any armed baseline used for the endpoint comparison. Append-only locks preserve every applicable armed parent as a prefix. An unarmed parent cannot release an armed lock.

The baseline is paired with the working tree **by node id**, so a lock guards a body for as long as the document keeps its id, and `check` and the write seams agree because they pair the same way. Replacing an armed record at the same path with a new id is refused on both planes when the old id has no counterpart. A new record elsewhere has no baseline to compare against. What preserves a lock across a move is preserving the id:

- an explicit `id:` in frontmatter survives any move (`id_stability: already_anchored`);
- an id derived from `identity.id_rules` survives `nodex rename`, which writes the derived id in explicitly before moving the file (`anchored`) — but not `mv` / `git mv`, which change the path and therefore the id;
- a **bare-markdown** document cannot be anchored — `rename` will not invent a frontmatter block for a path operation — so its id does change and `id_stability: bare_no_frontmatter` says so.

So: move a locked document with `nodex rename`, and give it an id that is not derived from its path.

## Unresolved-reference policy

`[[detection.unresolved_policy]]` is an ordered, first-match-wins table of `(cause, relations?, glob?) → severity` rows classifying unresolved references.

- `error` registers a check rule `unresolved_reference/<name>`; matching edges fail `nodex check` and count as `violation_unresolved_reference/<name>`.
- `warning` edges count in `summary.total` under `unresolved_edge`.
- `info` edges are reported out of `total` under their row's name.

Three axes narrow a row, and only `cause` is required:

- `relations` — which relation the edge carries; omitted matches every one. This is the axis that separates a structural edge from a prose citation when both name the same dead id for the same reason, and it is where `superseded_by` is nameable: a resolved successor reference is materialised as a `supersedes` edge, so `superseded_by` exists only unresolved and is rejected everywhere a resolved edge is read (`git_drift_relations`, `acyclic_relations`, `--relations` on `query dependents` / `impact`).
- `glob` — the names the resolution *sought*: the node id for an id relation, the normalized root-relative resolution candidates for a document reference. Never the raw authored target, so `../docs/x.md` written from `designs/a.md` matches `docs/**`. Rejected at load on `escapes_source` / `absolute`, which are refused before anything is looked up.

A row an earlier one already covers is **rejected at load**: first match wins, so it could never classify an edge, and an error row that never fires reports a clean gate over exactly what it was declared to catch. Declare the narrow rows before the broad ones.

```toml
[[detection.unresolved_policy]]
name = "dead-successor"
cause = "id_not_found"
relations = ["superseded_by"]
severity = "error"

[[detection.unresolved_policy]]
name = "point-in-time"
cause = "missing"
severity = "info"
```

Declaring the table **replaces** the built-in default row `{name = "excluded_target", cause = "excluded_from_scope", severity = "info"}` — re-declare it to keep it.

## Detection thresholds

`detection.stale_days` and `detection.git_drift_threshold` are omitted to disable; `0` is rejected at load as ambiguous. Omitting `stale_days` leaves `stale_review` unregistered, and `query stale` then answers empty with a `threshold_undeclared` warning while `GRAPH.md`'s Stale section says it is not tracked; `query issues` carries no warning, since its `rule_coverage` already lacks the rule. It also drops the trust composite's `freshness` component — freshness is measured against that horizon, and a project declaring no horizon has no scale to place a date on.

Setting `git_drift_threshold` requires `git` on PATH and a git work tree holding the project: without them every command that loads the config refuses with `CONFIG_ERROR`. With it set, a `check` of the working tree or the index keeps the git reading it took in `<output.dir>/history.json` — a cache like `cache.json`, which every other command only reads.

`detection.git_drift_relations` selects which relations carry the measurement (default `["references", "implements", "covers"]`) and is validated at load — non-empty, no duplicates, every entry a known relation — whether or not the threshold is set. `detection.orphan_grace_days` is a plain duration, so `0` is valid and means "check immediately"; it exempts a document only when that document declares `created`, since an undated one has no age to place. `detection.orphan_ok_kinds` names kinds that are leaf-by-design. With the per-node `orphan_ok` flag and terminal status, those are the four exemptions that decide the `orphan` rule's population — the reach reports how much of the corpus is left. `detection.superseded_reference_ok_kinds` names kinds that cite superseded documents by design — an ADR index, a decision log — which `superseded_reference` does not ask to repoint. `detection.superseded_reference_ok_annotation` names an `[[annotations]]` block (refused at load if none declares it) whose markers make the same statement one document and one target at a time: a marker keyed by a target's id, anywhere in the citing document's body, takes that document's body citations of that target out of the question, while a frontmatter relation to it (`implements`, `related`) is a structural claim the marker does not speak for and stays asked. It is keyed by target rather than by line because a document's citations of one target through one relation are one edge wherever they sit; a marker inside a code block is not read.

The detection plane reads terminal status one way throughout: `stale_review` does not review what the project retired, `git_drift` does not measure it against source, `orphan` does not ask it for references, `superseded_reference` does not ask it to repoint what it cites, and the trust composite places neither review-anchored component on it. Every remedy those surfaces name is a maintenance action.

## Git measurement

Every git-backed feature — `rules.immutable_baseline`, `git_drift`, the `statuses.flow` rules, `diff`, `impact`, `check --staged` — measures the project **at its own location** inside the repository that tracks it. A `nodex.toml` in a subdirectory of a larger repository is measured as itself, not as the repository around it, and no inherited git environment variable (`GIT_DIR`, `GIT_WORK_TREE`, a server-side hook's quarantine object directory, pathspec magic) can redirect it; the one read is `GIT_INDEX_FILE`, by `check --staged`, where git names the index a commit under way is made from. A ref is read by checking it out into `.git/nodex/checkout/`, which nodex keeps and switches from tree to tree; a file git writes through a filter, `ident` or `working-tree-encoding` (a decrypted document, for one) is removed when the run that read it ends, and deleting the directory while no nodex runs is safe.

Document mutations serialise cooperating nodex writers through `<output.dir>/write.lock`. Edited files are checked against the exact bytes read during planning and again before replacement. `WRITE_CONFLICT` requires reading the current revision and replanning; nodex does not merge concurrent edits. External editors do not take this lock, so this is revision checking rather than a filesystem transaction over arbitrary writers.
