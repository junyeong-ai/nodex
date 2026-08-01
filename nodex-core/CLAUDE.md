# nodex-core

Library crate. All graph / config / rule logic lives here; the CLI is
a thin wrapper. Each section is an invariant — violating one breaks the
design. Full rationale lives in the cited rustdoc.

## Layering invariants

- The `nodex_core::*` facade re-exports (`lib.rs`) are the canonical
  names — use them in tests and embeds; reach into module paths only for
  items the facade doesn't surface.
- `path_guard` renders paths in two directions and they are not the same
  operation. `forward_str` normalizes an *authored* path — a CLI argument, a
  link destination — where `\` divides components whatever the host is, so a
  document reads and writes the same from either platform and `\etc\passwd.md`
  is the drive-relative shape on both. `forward_string` renders a path the
  *filesystem* gave us, folding only what `std::path::is_separator` calls a
  separator, because a name the walk read has to render reversibly: a Unix
  document may legitimately be called `literal\ref.md`, and folding it would
  put a path in the graph that no reader can open and every seam reading a
  document by its recorded path would skip. Sharing one helper between the two
  is the bug.
- `path_guard::normalize_doc_path` is the single normalization for every
  user-supplied document path (fold `\`→`/`, refuse traversal/absolute,
  collapse `.`, refuse a spelling the filesystem does not use).
  `scaffold`, `rename`, and `check --content` key id
  inference, scope probes, rewrites, and the write on its result through
  it, so a probe verdict and the written artifact never disagree about
  which document was named. The spelling refusal lives inside this seam
  rather than at each gate because a case- or normalization-insensitive
  volume (APFS, NTFS, HFS+) resolves several spellings to one entry while
  every comparison nodex makes is exact, so a folded spelling addresses a
  document no lookup finds while the write lands on the real file — the
  path by which a frozen record is overwritten by a "new" document and a
  rename rewrites references onto a name the next scan never produces.
  The four surfaces that accept a document path (`scaffold --path`,
  `rename`'s source and destination, `check --content`) are exactly this
  function's callers, so enrolment is the call itself.
  `path_guard::filesystem_spelling` asks the filesystem component by
  component: each level must list an entry named exactly as authored, a
  component existing under no spelling ends the walk (a new document is
  never refused), and a correctly spelled component is never resolved —
  so a path through a symlink stays legal and only a folded component
  consults a canonical path, to name the entry the write would hit.
- `path_guard::write_atomic_in_root` is the single public write
  primitive — every document mutation (scaffold, lifecycle, migrate,
  rename's id anchor, retarget) and infra artifact (graph.json, GRAPH.md,
  cache.json, init's nodex.toml) routes through it; it refuses a
  final-component symlink and enforces root containment. `std::fs::write`
  in a mutation path is a defect. Batch file rewrites (rename, retarget,
  migrate --apply) plan through `mutate::plan_file`, take one verdict from
  `BaselineProbe::refusals`, and write survivors through
  `mutate::write_plan` — the reader-follows / writer-skips symlink
  discipline, the immutability verdict, and the atomic write each in one
  place. Planning is separate from writing because the verdict is about the
  whole batch, and a write that landed before it was answered could not be
  taken back. Writing is separate from committing for the same reason:
  `path_guard::stage_in_root` puts the content on disk beside its target and
  `Staged::commit` renames it there, so a batch stages everything before it
  commits anything. The failures that actually happen — an unwritable
  directory, a full disk — then happen while the tree is untouched and every
  staged write is dropped, and a gate's verdict about the project a batch
  produces is worth what it says: what remains after staging is
  same-directory renames, the atomic primitive itself.
- `git::Repository::discover(root)` is the single git binding: the
  repository tracking the project, its work tree, and the project's own
  prefix inside it. Each consumer that measures git resolves it once and
  passes it explicitly (`RuleContext::repository`, `BaselineProbe`, the
  CLI's worktree materialisation) — never rediscovered per document; the
  `git_drift` preflight and the rule pass each resolve independently
  because a fail-fast gate has no channel to hand the binding forward,
  and the answer is a pure function of the project's location. Paths reach git
  through `Repository::tracked_path` and checkouts through
  `Repository::locate`, so a project in a subdirectory of a larger
  repository measures itself and not the repository around it.
  `git::command` is the only place the binary is named and clears every
  variable that could redirect an invocation or reinterpret a path
  argument — but not the ones that merely bound discovery
  (`GIT_CEILING_DIRECTORIES`, `GIT_DISCOVERY_ACROSS_FILESYSTEM`), which
  cannot select a different repository and whose effect is reported rather
  than overridden; a `clippy.toml`
  `disallowed-methods` entry fails the build on any other
  `std::process::Command::new`, each legitimate spawn carrying its own
  `#[expect]`. Every answer git gives about the repository's own location
  comes from its own single-question invocation and never round-trips
  through a `String` — `rev-parse` output is unquoted, so a multi-answer
  read cannot be split back into answers, and a path is the operating
  system's to spell. A binding is returned only after it is checked
  against the project it claims to describe. What a ref records at the
  project's own prefix is asked of git rather than inferred from a command
  that merely resolves: `ref_state` requires it to answer as a tree,
  because a file or a submodule gitlink recorded at that name resolves just
  as happily and would bind a baseline that reads as "nothing is frozen".
  Nothing else reads a ref per path — a baseline is a graph
  (`BaselineBinding::snapshot`), so what a ref records at a *document's*
  path is whatever checking it out and walking it produces.
  Rationale and the measured variable groups: rustdoc in `git.rs`.
- `mutate::BaselineBinding::resolve(root, config)` binds
  `rules.immutable_baseline` once per command, and
  `BaselineBinding::snapshot` pairs that binding with the baseline *graph* —
  the only way to obtain a `mutate::BaselineProbe`, so a write seam cannot
  hold a bound baseline it has no snapshot of. There is one baseline in a
  run and one definition of it: the CLI's `baseline_graph` materialises the
  ref and builds it, the read plane diffs that graph, and every mutation
  seam (the batch gate, `lifecycle::transition`, scaffold's recreate /
  `--force` path) locks against the same one. The two planes
  cannot disagree about a baseline they share.
  A lock is never re-derived: `BaselineProbe::refusals` builds the project
  with the planned writes overlaid and runs the rules a baseline feeds
  (`Rule::diff_aware`, Error severity only — the line `check`'s exit code
  draws) against this baseline, so the write plane and the read plane cannot
  hold different opinions about the same document. Only those rules run, not
  the whole registry: `git_drift` shells out per node and a write must not
  pay for an answer it discards. The verdict is absolute rather than the
  introduced delta `check --content` uses — a record already drifted from a
  frozen baseline is still frozen history, so piling another edit onto it is
  the write to refuse. One question the rules cannot answer stays separate:
  `BaselineProbe::frozen_at` asks whether the baseline holds a frozen record
  at a path, because replacing a record with a *different* one is a removal
  plus an addition to `check` and nothing consumes either. `frozen_record_lost`
  narrows that to a record the project no longer holds anywhere: a record
  travels under its id (which is why `rename` anchors one), so one that merely
  moved has left its path free, and refusing there would refuse a mutation
  `check` reads as nothing at all.
  A binding that is bound costs one materialisation, so a write command with
  a baseline pays what `check` pays — O(repository), which in a monorepo whose
  project is one subdirectory is the whole repository, not the project. A
  project with no baseline, or none of the rules a baseline feeds, spawns
  nothing. `check --content` resolves a binding and drops it, so it pays for
  resolution (discovery + `ref_state`) and never for materialisation.
  A probe with nothing bound locks nothing and carries
  `BaselineProbe::advisories` — the wording for "the configured locks did not
  engage", plus the baseline build's own warnings, because a document that
  failed to parse there has no baseline node and so no lock guards it. Read
  *and* write commands surface them. A baseline that cannot be *read* is not
  an inert one: `BaselineBinding::resolve` returns `Err`, so a lock that
  cannot be evaluated refuses the run instead of permitting it.
  Activation matrix and per-seam wiring: rustdoc in `mutate.rs`.
- `model::validate_explicit_id` gates a reference-unsafe id
  (trim-unstable, wikilink metacharacters) at every write seam that
  accepts one: `scaffold --id` and `retarget <new-id>`. For configured
  `link_patterns`, `reference_rewrite` additionally verifies each
  rewritten span re-captures the successor id (round-trip guard) and
  leaves un-round-trippable spans untouched.
- `model::ID_RELATION_FIELDS` is the single id-valued relation-field
  vocabulary the frontmatter-lock probe reads.
- `mutate::introduced` is what a mutation answers for: the check
  violations the project would carry after the proposal that it does not
  carry now (the count-aware multiset delta against the pre-proposal
  report). Every seam that writes documents asks before it writes and
  refuses on the Error-severity findings, so a command cannot report
  success onto a project its own `check` then fails — nor refuse one
  `check` would pass, which is the same defect. The rules a mutation can
  break are the whole registry, not the family a seam was built around: a
  reference a move strands, a cycle a repoint closes, a sub-artifact a
  status change evicts. `ProposalDiff` names the one input that differs
  between seams — a seam gating *authored* content (`scaffold`, mirrored
  by `check --content`) activates the diff-aware rules against the working
  tree, the launder-safe boundary; a seam *transforming* documents already
  on disk leaves that family to `BaselineProbe`, which asks it against the
  ref `rules.immutable_baseline` names.
  `scaffold`'s config-default path advises instead of refusing, and that
  licence covers the findings its own document *owns* — by node id, never by
  path (`Introduced::owned_by_others`). A placeholder's findings are the
  fields to fill in; a reference the write stranded, or a number it duplicated
  with somebody, is not. A finding no node owns is owned by no document: the
  path a duplicate-number conflict carries is whichever member sorted first,
  so filtering on it made the verdict depend on a filename's alphabetical
  luck.
  A seam's own guards stay in front of the gate only where they are a
  strict subset of it *and* phrase a remedy the gate cannot — which status
  to add, which field to set first, which path is not graphed. A guard
  that merely restates a rule is deleted: `rename` does not pre-check
  `rules.naming`, because a rule cannot fire on a document the graph does
  not carry and neither may the seam.
  `migrate` has no gate and needs none: it writes only what
  `render_default_frontmatter` derives from config, the "cannot produce
  out-of-vocabulary values" strategy — the fields it injects are the ones
  the bare document already inferred, so its violation set is unchanged by
  construction. That holds only while every reader of a document agrees
  about what it inferred: the scan's `conditional_exclude` probe resolves a
  missing status through `resolve_initial_status`, the same seam
  `parser::ParseConfig` uses, because a scan reading "declares none" as
  "not terminal" would describe a different document from the one the graph
  holds — and writing the status it already had would then change the
  project.
- Rules read from `RuleContext { graph, config, files, since }`.
  `files` is `builder::scanner::ProjectFiles` — where the project's bytes
  are for this pass, the working tree or the working tree with a proposal
  applied. A rule that probes the filesystem asks through it rather than
  joining a root itself: a proposal gate judges a project the disk does
  not hold yet, and for every path the proposal speaks about the two
  disagree. The unresolved-cause classifier is where that decides an
  outcome — it tells "nothing is there" from "something is there the
  graph excludes", which selects the `[[detection.unresolved_policy]]`
  row, which selects whether a write is refused.
  `rules::preflight` verifies an opt-in rule's environment up front (git
  on PATH + work tree for `git_drift`); the measurement runs inside
  `Rule::check` (`git_drift` shells git, the unresolved-reference
  classifier stat-probes in-root paths), so check results depend on the
  work tree and git state, not the graph alone. Diff-aware rules (need
  `ctx.since` from `--since`, `rules.immutable_baseline`, or `check
  --content`) self-report non-applicable via `is_applicable`; the runner
  records the refusal in `skipped_rules` — silent non-fires are forbidden.
- Rule `Severity` is a closed `Error | Warning` enum (`rules/mod.rs`);
  the per-edge `info` plane of `detection.unresolved_policy` is a
  different type (`config::UnresolvedSeverity`) — there is no
  Info-severity check rule.
- `Config` is the single source of truth for vocabulary. Tool actions
  that write frontmatter consume merged views (`required_for`,
  `types_for`, `enums_for`, `cross_field_for`, `declared_fields_for`,
  `trust_weights_for`) — never raw `schema.overrides` / `trust.overrides`.
  A seam reads a document the way the graph does — `lifecycle` takes its id,
  status and kind from `parser::parse_document`, never from the frontmatter
  editor, which is a *line* reader (`status: ~` is the text `"~"` to it and
  YAML null to the parser) and belongs to the write half only.
  `scaffold` / `migrate` render defaults via the reparse-the-real-node
  discipline (shared with the lifecycle write seam): each `cross_field`
  predicate is evaluated against a node parsed from the frontmatter
  written so far, iterated to a fixpoint — never a synthetic stand-in —
  so the written document passes the same config's `check` by
  construction.

- The hidden-path opt-in is asked twice because it is asked of two different
  things, and only one of them can be answered exactly. `hidden_admitted`
  decides a *document*: per include pattern, the pattern matches the path and
  stops matching once a hidden segment's leading dot is replaced. A path needs
  no alignment, so a pattern naming a dotted segment behind a `**` answers
  like any other, and asking per pattern is what keeps a greedy sibling from
  answering for one that names the segment. Every admission — the walk's,
  `check --content`'s, every write seam's scope probe — goes through it.
  `IncludeLead::may_hold_hidden` decides a *directory*, which has no document
  to match, so it reads the pattern position by position — and a component
  that can consume any number of segments makes every later position
  unreadable, including, for a leading `**`, its own. Past the literal run the
  question is therefore not *which* component governs a segment but whether
  any of them could discriminate on its dot, asked of globset for the segment
  in hand. None can ⇒ prune, which keeps a greedy `**/*.md` out of every
  dotted tree; one can ⇒ descend, because refusing on a position it cannot
  read is what emptied a corpus `**/.obsidian/**/*.md` matched. Never asked of
  the pattern's text: `*.hidden` starts with a wildcard and still insists on
  the dot.
  The two halves may differ in exactly one direction — the walk wider than
  admission, never narrower — and that is asserted, not reasoned about
  (`the_walk_never_stops_short_of_what_admission_would_take`): every ancestor
  of every admitted path must be walkable, because one denial on the way down
  loses the document silently, with `check` green over what it never read.
- `scanner::include_leads` is the one reading of an include pattern's leading
  part, and the two questions it answers are kept apart because they need
  opposite error directions. `IncludeLead::could_reach` bounds the walk and
  may truncate — a shorter run bounds less, so it errs toward "could reach".
  `may_hold_hidden` cannot afford the same truncation at a position the lead
  *does* cover: answering "no" there skips the tree. So the lead carries
  the literal run *and* the component that ended it, and asks globset what
  that component does with a dot rather than decoding the text — `\.dotted`,
  `[.]dotted` and `.*` each turn on a dot the text does not spell, and
  `discriminates_on_leading_dot` is that question asked of the compiled
  matcher: it matches the segment as spelled and stops once the dot is
  replaced. Exact for the patterns an operator writes, and inexact in the
  safe direction otherwise — a grant costs a walk and admits nothing the
  include globset does not, while a denial is loud (the per-pattern "matched
  no files" warning). The only text rule left is which components are
  literal, and it is sound by exclusion (none of `* ? [ { \\`, the five
  constructs that can cross a separator under globset's defaults), pinned by
  a property test that compiles every accepted component, one fixture per
  excluded construct — under the config's own precondition, that an include
  pattern is a valid glob. The prune
  hint in `builder::scope_coverage_warnings` reads the same lead, so a
  diagnostic can never disagree with the walk about what a pattern spells.

- `mutate::introduced` pairs findings by `rules::finding_identity` — rule,
  severity, node id, and `ViolationDetails::cause`. A document's identity is
  its node id, which travels with a move; a finding's is its cause, which
  `cause` projects by normalising away any payload that merely *locates* it —
  the files sharing a duplicated number (the *documents* stay, by id, because
  a conflict between a different pair is a different conflict) and the parse
  error's rendered reason (the path and the content digest stay: a document
  that failed to parse has no id to be known by, so the path is the whole of
  what the finding is about, and the digest is the byte state it failed in).
  The match there is exhaustive, so a new variant decides at compile time
  whether it carries evidence — the discipline `render_message` already
  enforces for the prose.
  A node-less finding carries its subject in `details` rather than in the
  `path` every violation has, because that field means different things per
  rule: for `parse_failure` it is the subject, for `acyclic_relation` and
  `unique_numbering` it is whichever member sorted first, and keying on it
  would refuse a rename that moved a ring member's file while the ring stayed
  what it was. A document that does not parse therefore cannot be renamed —
  the failure lands at a path the project did not carry one at — and nothing
  guards that separately; the gate is what answers it.

- `rules::detail::Evidence<T>` is how a payload says it locates or renders a
  finding rather than identifying it. Every `Evidence` equals every other, so
  the derived `PartialEq` on `ViolationDetails` *is* the "same finding"
  question and `finding_identity` compares `details` as it stands — there is
  no normalising pass, and a variant cannot forget to join one. Serde and
  `JsonSchema` delegate whole, so nothing about the wrapper reaches a
  consumer: the JSON carries the value and the exported schema describes the
  value, never an `Evidence3` a generated client would name and renumber.
  What is wrapped today, and why each moves while its finding does not:
  `BodyLine::line` and `UnresolvedReference::location` (a line number shifts
  when a paragraph is inserted above it), `Cycle::ring` (a route lengthens
  when a chord is dropped without freeing anybody), `ParseFailure::reason`
  (the operating system words a failed read its own way per platform),
  `UniqueNumbering::paths` and `FilenamePattern::filename` (a document keeps
  its id wherever it sits and whatever it is called), `StaleReview::days` and
  `GitDrift::{total_commits, hottest}` (magnitudes that grow on their own),
  `BodyImmutable::{before_lines, after_lines}` (how much of a locked body
  moved, where the finding is that it moved at all).
  This replaced a hand-written `ViolationDetails::cause`, which had to be
  right once per variant and was wrong three times: the decision belongs at
  the field, where the field's meaning is being written down, not in a match
  arm a new variant joins by copying its neighbour.

## Build modes

`builder::build` / `builder::build_with_overlay` are the public build
surface; the private `BuildMode` behind them couples content source to
cache persistence — only the working-tree mode persists `cache.json`, an
overlay build is read-only (proposed bytes substitute the disk read), so
unwritten content never leaks into the cache. Both proposal gates (`check
--content`, scaffold's before/after validation) refuse a proposal on
exactly the Error-severity violations the overlay *introduces*
(`rules::introduced_violations` — a count-aware multiset difference by
exact `Violation` equality: a duplicate of a pre-existing violation still
refuses; a pre-existing violation elsewhere never blocks).
`scanner::scan_scope_with_overlay` is the single scope authority, so an
overlay graph and the real post-write build never disagree about
membership.

## Fallback mechanisms (intentional, not optional)

Every document always gets a valid kind, id, status, so an incomplete
config can never break the graph (declare exhaustive rules to override):

- **kind**: `FALLBACK_KIND = "generic"` when no `identity.kind_rules`
  matches — so `kinds.allowed` MUST include "generic" (enforced at load).
- **id**: `"{kind}-{stem}"` when no `identity.id_rules` matches.
- **status**: `statuses.initial` when declared, else the first
  `statuses.allowed` value (kind-independent). Used by `scaffold` /
  `migrate` / frontmatter-less parses; `Config::validate` rejects a
  config whose implicit default a declared enum excludes.
- **orphan grace**: docs created < `orphan_grace_days` ago skip orphan
  detection (`u32` not `Option` — `0` = no grace); also exempt:
  `orphan_ok_kinds` membership, per-node `orphan_ok: true`.

The parser resolves id / title / kind / status / orphan_ok for every
document (`INFERRED_FRONTMATTER_FIELDS`), so a `schema.required` or
`cross_field.require` naming one could never fire — `Config::validate`
rejects both at load. `ParseConfig::resolve_identity` is where the
config-supplied ones land, kind before id because `identity.id_rules` are
keyed by kind. Every reader that pairs one parsed document against another
completes both through it — the write seams' lock probe
(`rules::body_immutable::parse_for_probe`) as much as the build — because
a second completion chain lets two readings of the same bytes disagree
about a field the document never wrote, and the id is what a pairing is
keyed on.

Built-in frontmatter fields parse leniently, field by field: a value that
fails its type records a `FieldParseIssue` and reads as absent under the
fallbacks above — the failed value never reaches `attrs`. Only
unparseable YAML, a non-mapping block, or an unclosed fence drop the
document, and the drop is canonical graph data (`Graph::parse_failures`).
Two always-registered built-ins make both states Error-severity findings:
`field_parse` (node-attributed) and `parse_failure` (node-less). Write
seams split reader-degrades / writer-refuses (`lifecycle` refuses parse
issues / an unsplittable fence; `rename` / `retarget` / `migrate` refuse
or per-file-skip; `scaffold` with supplied content refuses through its
overlay delta) — the same file is guaranteed to red `check`. Details:
rustdoc in `parser/frontmatter.rs`.

## Naming conventions

`query/` functions read graph + config and never mutate (`lib.rs`
re-exports the stable set) — but are not all filesystem-free:
`find_unresolved_edges(graph, config, root)` stat-probes in-root paths,
`compute_trust(graph, config, root, id)` runs git for the drift
component. Prefixes disclose ranking: `find_*` = traversal/filter (no
ranking), `compute_*` = value computation (similarity, trust, diff);
text-scored matching is `query/search.rs::search`.

Input specs: `*Filter` = pure predicate (every field narrows, no tuning
knobs; the CLI caps presentation — core returns complete results, see
`.claude/rules/json-output.md`). `*Options` = ranking / threshold /
selection knobs; a spec whose `limit` is *selection semantics* (top-K /
window — `RecentOptions`, `SimilarityOptions`, `TrustListOptions`) is
`*Options` even when most fields narrow.

Rule types end with `Rule` (`RuleSource::Config` + a `<family>/<name>`
id; built-ins keep the family name — `required_field`, `stale_review`,
`filename_pattern`). Output types: `*Manifest` (exports), `*Report`
(aggregates), `*Result` (mutation outcomes — in `command_result.rs` or
the command's module so `export::per_command_schemas` derives JSON Schema
from the same emitted type), `*Ref` (flat projections), `*Entry` /
`*Group` (items-list elements), `*Outcome` (an in-process carrier that
bundles a result with its own metadata and is *not* a serialized wire
type — `BuildOutcome`, `FileOutcome`, `RankingOutcome`). One stem per
concept across item /
components / options / function (`TrustEntry`, `TrustComponents`,
`TrustListOptions`, `compute_trust`); the CLI `*Args` stem matches its
core stem.

## Detection thresholds (explicit semantics)

`stale_days` / `git_drift_threshold` are `Option<u32>` — `None` disables,
`Some(0)` rejected at load (ambiguous: "off" vs "flag immediately").
`orphan_grace_days` is plain `u32` (a duration), so `0` is valid — the
differing type is deliberate. `git_drift::commits_since` returns
`Option<u32>`: `None` = unmeasurable, distinct from `Some(0)` = no drift.
Neither fabricates max trust from absence (the `backlinks` discipline):
the check rule skips an unmeasurable edge; the trust composite drops the
whole drift component — absence never reads as "no drift".

## Cache invalidation

`parser::ParseConfig` is the exact slice of `Config` parsing reads
(identity, resolved initial status, parser, annotations, body_line);
parsing takes `&ParseConfig`, so a new parse-affecting option cannot be
added without surfacing there — the compiler enforces it. `cache_key()`
is the build cache key — SHA-256 over that surface plus
`CARGO_PKG_VERSION`: reordering order-critical blocks invalidates,
whitespace edits and check-only tuning never do, a binary upgrade
invalidates once. `cache.json` carries its own shape guard
(`CACHE_SCHEMA_VERSION` in `builder/cache.rs`); a mismatch discards the
cache — cold rebuild, never an error. Full consequence table: rustdoc in
`parser/mod.rs`.

`scanner::ScanConfig` is the membership twin: the exact slice of `Config`
that decides scope (`scope`, `output.dir`, and `statuses.terminal` only
when a `conditional_exclude` can consult it); every private scan helper
takes `&ScanConfig`. `builder::graph_config_hash` — SHA-256 over
`CARGO_PKG_VERSION` + `ParseConfig` + `ScanConfig` — is recorded as
`GraphMeta::config_hash` on every snapshot; check-only tuning never
perturbs it. `Config::validate_scope` compiles the globs from the same
projection at load, so load-accept implies scan-success.

## Cycle detection

`rules/graph_invariants.rs::CycleDetectionRule` checks every
`rules.acyclic_relations` relation (default `["implements"]`; validated
at load, empty list rejected) over resolved edges. `supersedes` is
validated separately and harder — a build-time `Error` from
`builder::validate_supersedes_dag`; `covers` names out-of-graph code
paths and cannot cycle. A cycle violation is Error severity and node-less
(`node_id: None`, `path` = a ring member, message carries the full ring)
— a project-wide finding, so `--since` narrowing never drops it.

One finding per *strongly connected component*, never per ring a walk
closed. A relation is a DAG exactly when no component of it is cyclic, and
a component decomposition is a partition of the nodes — the same answer
whatever order the graph is walked in. Rings are not: a walk retires a node
the first time any root reaches it, so a chord inside a tangle surfaces or
hides according to where the walk came in, and the entry point moves when
an edge nowhere near the tangle moves. `Violation` equality is what the
proposal gates diff, so that difference read as a cycle the mutation closed
and refused mutations that closed nothing.

`details.members` is the whole component, sorted, and it is the finding's
subject — a document is caught in the cycle or it is not, and rearranging
the edges inside changes no part of that. `details.ring` is the tightest
route through the smallest member, a witness that exists by construction
since every member of a cyclic component lies on a cycle, and it is
evidence only: the shortest route lengthens when a chord is removed without
freeing anybody, and it stays put when the region gains a document it does
not pass through. Pairing on it therefore failed both ways — refusing an
edit that dropped an edge, and passing one that dragged a document in.

## Data flow invariants

- Every parser entry routes content through
  `parser::frontmatter::canonicalize` (BOM strip + `\r\n`/`\r` → `\n`) so
  fingerprints, regex matches, and line iteration agree across
  line-ending styles. Body scanners share
  `parser::body::iter_body_lines` (one fence-aware iterator).
- Per-block `kinds: Vec<String>` filters go through `Node::matches_kinds`
  (empty = no restriction); `validate_kinds` rejects typos at load. Link
  patterns need exactly one capture group (rejected otherwise at load).
- The parser extracts body-derived data once at build time; no rule
  re-reads document content at check time (the git/stat probes above
  measure the environment, not document bytes).
- Reference handling has one extraction and one resolution, so build and
  mutation can never disagree. Extraction: `parser::body` finds
  references once (pulldown-cmark destinations, `[[wikilink]]` /
  `link_patterns`, code-span + frontmatter aware); `extract_links` and
  `reference_rewrite` share the helpers. Resolution:
  `reference_path_candidates` is the single ladder (literal/relative path
  → path + each `parser.extensions` suffix → bare id), shared by the
  build resolver, the unresolved-edge classifier, and the rewriter (which
  touches exactly the edges the build bound). `covers` stays path-only
  (`model::edge::is_document_ref_relation`),
  `supersedes`/`implements`/`related` id-only
  (`model::edge::ID_RESOLVED_RELATIONS`); a link pattern naming a
  code-fixed-resolution relation is rejected at load, so each is
  producible only by its frontmatter field.
  `resolver::normalized_resolution_candidates` projects the ladder to
  normalized root-relative form — the one definition of "what could this
  link mean", read by the unresolved-cause probes and
  `[[detection.unresolved_policy]]` row globs (semantics:
  `.claude/rules/config-driven.md`).

## Graph serialization

`Graph` has hand-written `Serialize` / `Deserialize`. Adjacency indices
are derived state — rebuilt inside `Deserialize` via `Graph::new`. The
snapshot carries its own provenance (`meta: {nodex_version,
config_hash}`) and recorded drops (`parse_failures`); both default during
deserialisation so an older file fails through the schema-version
message, never a missing-field error. Bump `SCHEMA_VERSION` in
`model/graph.rs` on any on-disk shape change.

## Snapshot introspection

`status.rs` owns every read of `graph.json`. `load_graph(root, config)`
is the single snapshot-read seam: a missing file is the typed
`Error::MissingGraph` (`GRAPH_MISSING`); every read attaches a
membership+config divergence warning — advisory only, never a gate.
Snapshot coverage is nodes ∪ `parse_failures`: a recorded parse failure
is covered-but-unbuildable (`nodex status` surfaces it as
`unbuildable_paths`; `check`'s `parse_failure` rule reds it), never
stale. `compute_divergence(graph, config, root, probe)` is the shared
primitive — `Membership` (every `query *` read) never reads document
content; `Content` par-hashes the corpus (`nodex status`, and one
escalation described next). Details: rustdoc in `status.rs`.

`Snapshot::require` is the seam that decides what a missed lookup means.
Membership fidelity is enough to *report* drift but never enough to *deny*
a document — an in-place edit that gives a document a new id moves no path
and changes no config — so a miss, and only a miss, escalates to `Content`,
and that verdict picks between three answers whose remedies differ:
snapshot matches → `NOT_FOUND` (correct the id); snapshot drifted →
`GRAPH_OUTDATED` (rebuild); the working tree could not be read → the
probe's own error, unchanged. The third is neither absence nor staleness
and a rebuild fails the same way, so reporting it as either would
prescribe a remedy that cannot succeed. The escalation is on the error
path that ends the command, so it is paid at most once per process.

## Adding a validation rule

See `.claude/rules/adding-a-validation-rule.md` — it loads when a file
under `nodex-core/src/rules/` is being read or edited.
