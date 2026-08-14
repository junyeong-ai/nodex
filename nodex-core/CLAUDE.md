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
  What that delta cannot reach is a document the proposal drops from the
  project, because the delta is taken over the population `check` runs on and
  a document leaving it takes its findings along — the write plane's version
  of the blind spot `RuleRun::subjects` answers on the read plane.
  `mutate::evicted` is the other half: `scope.conditional_exclude` is the one
  membership rule a document's *content* moves (`scanner::ScanConfig` reaches
  a document through nothing else), so a write that puts a terminal
  document in the parent slot — changing its status, or moving one already
  terminal there — is the write that drops the `child_glob` matches in that
  parent's directory subtree, and it names them from the scan's
  own record rather than inferring them from a node gone missing. Naming them
  is what the directory unit makes load-bearing: a live record's sub-artifacts
  go with the terminal record beside them, so what left the project is a fact
  about the write and not one a reader could derive from the document it named.
  The
  population it names them out of is what the project *holds* — nodes ∪
  `Graph::parse_failures`, the union `status` reports coverage over — because
  the record with no node is the one this matters most for: its `parse_failure`
  is an Error `check` is reporting right now, and a write that drops the
  document drops the finding, turning a red `check` green. A path the
  proposal itself names is never among them — a deletion is what was asked for
  and a move takes the record with it. The advisory itself never refuses:
  `check` says nothing about a document outside the project, so a refusal on
  the eviction would be one no reading backs. What the eviction *breaks*
  elsewhere still refuses through the ordinary gate — a reference into a
  dropped document the project's own `unresolved_policy` calls an error is a
  violation the proposal introduced like any other. It rides
  `Introduced::advisories`, which every write seam already calls, and
  `check --content` reports the same set for the same proposal.
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
- `scanner::coverage_warning` and `scanner::boundary_warning` are the scan's
  own disclosures — a scan that yielded no file at all, and one bounded by a
  link the walk declined to descend. They live in the scanner because what
  they state is a property of the scan, so a command that scans without
  building a graph owes them exactly as much: `migrate` reporting `total: 0`
  over a mis-scoped project is the same JSON as a finished migration, and
  only the scan tells them apart. The build supplies one verb for every
  command that graphs through it, because one run emits both disclosures and
  naming any one of those commands' jobs would be a foreign verb in the rest;
  `migrate`, which scans without building, supplies its own.
  A command that *does* build carries them by surfacing that build's
  `BuildOutcome::warnings` rather than by re-deriving any of them — the
  scan behind the build is the same `scan()` call, so a hand-rebuilt subset
  is a second reading that can only lose channels (`rename` reconstructed
  the boundary warning alone and dropped coverage and cache with it). There is no
  line: every command that reads the corpus says what it read. The snapshot
  plane says it from `compute_divergence`, whose scan was hidden inside the
  comparison it fed — a probe that reports fidelity and not reach cannot tell
  "the snapshot matches the tree" from "both are empty", which is why
  `DivergenceOutcome` carries the reach beside the verdict. The one place a
  disclosure cannot ride a warning is an error envelope, so `Error::Corpus`
  puts it in the `NOT_FOUND` message: over a project governing nothing, or one
  whose every document failed to parse, no corrected id resolves and the
  remedy the message states has to be one that can succeed.
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
- A rule that *ran* answers for its reach. `Rule::check` returns a
  `RuleRun` — violations plus the number of units it iterated after its
  own scope filters — and the runner records one `RuleCoverage` per
  evaluated rule. `skipped_rules` and `rule_coverage` partition the
  registry, so a report answers "was this gate complete?" and not only
  "did it find anything": an empty violation list is what a thorough pass
  and a vacuous one both look like, and a rule reporting zero subjects was
  in effect over nothing whatever its config declares.
  `subjects` is the population the rule *guards*, never the offending
  subset and never the slice that happened to move: a `body_line` block
  counts documents of its kinds (not lines that matched), `parse_failure`
  counts every document the build attempted (not the ones that dropped),
  and a diff-aware lock counts the records it is armed over (not the ones
  edited this run, which is empty on a clean tree). Read that way zero has
  one meaning everywhere — the rule was handed nothing — and a rule whose
  population is its own findings would report a healthy project and an
  empty one identically.
  Armed over is decided against the baseline, not the working tree.
  `compute_diff` builds every per-node channel over the ids both snapshots
  hold, so a record the baseline carries no node for reaches none of them
  and no diff-aware rule can fire for it; `GraphDiff::added_ids` is that
  set and both locks subtract it before counting. Without the subtraction
  the reach counts a document the lock provably cannot judge — which is
  what *every* way of losing a baseline node looks like from inside the
  rule, whether the ref could not parse the document, scope declined it
  there, it sits behind an undescended symlink, or the record has since
  moved. The population answers for all of them at once, so nothing
  upstream has to attribute a missing node to a cause. What the scope
  selected and the rule could not judge leaves the same pass as
  `RuleRun::unjudged`, because a reach read alone says how much a rule
  guards without saying how much it was meant to — one record short is
  legible only against a run the reader does not have. `before_status` /
  `before_kind` are why it must be explicit: both answer for a
  still-present node and fall back to what they are handed, so asked about
  an added id they describe the document as it stands and a record with no
  baseline reads as one the baseline governed.
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
  when a paragraph is inserted above it), `Cycle::{region, via}` (both move
  when the region does), `ParseFailure::reason`
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
`rules::finding_identity`: a duplicate of a pre-existing violation still
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
`Some(0)` rejected at load (ambiguous: "off" vs "flag immediately"). Both
reach `None` by being *omitted*, and neither carries a serde default: TOML
has no spelling for `None`, so a default in that position would make the
one state the field documents as "disabled" the one state a project could
not ask for, while the rejection message told the author to omit the field
to get it. The threshold a new project starts with belongs in the config
`init` writes, where it is visible and editable, not in an attribute that
closes the door behind it.

Omitting `stale_days` therefore drops the trust composite's `freshness`
component as well as the `stale_review` rule: freshness places a review
date on the staleness horizon, and a project that declares no horizon has
no scale to place it on. That is the `drift` discipline again — the
composite renormalises over what is present rather than substituting a
neutral value.
`orphan_grace_days` is plain `u32` (a duration), so `0` is valid — the
differing type is deliberate. `git_drift::commits_since` returns
`Option<u32>`: `None` = unmeasurable, distinct from `Some(0)` = no drift.
Neither fabricates max trust from absence (the `backlinks` discipline):
the check rule skips an unmeasurable edge; the trust composite drops the
whole drift component — absence never reads as "no drift". Skipping every
edge a node offered would put that absence back at the node, where zero
commits reads as a clean record, so the rule reports such a node as
`RuleRun::unjudged` instead of counting it among the records it guards, and
names it besides — a `GitDriftUnmeasurable` violation carrying the targets.
The reach says how many documents the rule does not gate and the finding
says which, because a count is not something an operator can repair: the
population is legible only against a run they do not have, and the remedy is
a specific target to repoint. A
node offering no drift edge at all is a subject like any other: nothing to
measure is an answer, where nothing measurable is not.

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

Both project *fields*, never whole config blocks: `ScopeMembership` and
`IdentityParse` destructure their block exhaustively, so a field added to
`[scope]` or `[identity]` is a compile error until somebody decides whether
a build reads it. A block borrowed whole covers every field it will ever
grow — including the ones no build can reach, which then declare every
existing graph outdated the moment a project writes one down. A projection
that names its fields is checked in the direction that matters: the compiler
asks about the new field, and
`the_config_hash_moves_with_what_a_build_reads_and_nothing_else` asks about
every field either way.

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

One violation per *document caught in a cycle*, and `details.member` is the
finding's subject. The region cannot be: a component that shrinks or splits
yields components the project never carried, so a count-aware multiset delta
reads a repair that frees a document — or one that cuts a tangle in two — as
minting cycles, and every write seam refuses the one edit a tangled graph
most needs. No per-region identity avoids that; the atom has to be something
a repair leaves alone. A document is in a cycle or it is not, whatever
happens to the region around it, so the delta is monotone: refuse exactly
when a document is newly caught.

`details.region` is the region's smallest member — the same value on every
finding from one region and different on findings from another, so grouping
by it recovers which documents are tangled together. `details.via` is one
outgoing edge of the member that stays inside the region: an edge on a cycle,
and a concrete thing to cut. Both are `Evidence`, because each moves when the
region does, which is exactly when the finding must not.

Neither composes into a route. Each member picks its own smallest in-region
successor independently, so chasing `via` from finding to finding can wander
into a sub-ring that excludes where it started. A ring through a particular
member would be the thing to follow, and it is not carried: it is not
constant-sized, and one finding per member holding one is quadratic — a
50k-document tangle would cost more to report than to have. A region label
and one edge are each constant-sized, which is what lets every finding carry
them.

Node-less (`node_id: None`) even though each finding names one document:
`--since` keeps a node-less violation whatever changed, and a document
dragged into a cycle by an edit to its neighbour is exactly the finding
narrowing would drop.

The invariant is pinned by property tests over the whole small-graph domain
(`rules::graph_invariants::tests::properties`) rather than by scenarios: the
rule must catch exactly the documents that reach themselves, and a gate must
answer for exactly the documents an edit newly catches. Four separate
scenario-found defects preceded them.

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
  touches exactly the edges the build bound). A reference opening `./`
  names the directory its document is in — to CommonMark, to every
  filesystem, to every editor that follows the link — so it skips the
  literal rung. `resolver::frame_and_path` is where a reference's path is
  read, and every reader of the ladder goes through it, because a marker
  one reader honours and another normalises away is two readings of one
  link. It refuses a root-anchored path first, on the spelling exactly as
  written — a leading empty segment is what says root and nothing else
  does — then reads the marker and drops the segments that name nothing,
  `//` and `.` alike, which every reader of a path collapses with no
  lookup involved. Asked in the other order it needed a special case to
  keep root recognisable, and `.//x.md` read as an absolute path.
  `..` is not noise and stays: it is an operation on what precedes it, and
  *where* it is resolved is what decides which frame a reference binds in.
  `covers` stays path-only
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
- A markdown destination *spells* a path rather than being one:
  `old&#x2e;md` and `a\(1\).md` name `old.md` and `a(1).md`, and which
  bytes open a `#fragment` is a fact about the decoded text, not the
  source. `body::Destination` therefore carries both halves — the span
  that does the spelling and the path it spells, read by the one helper
  `process_link_target` reads it with. Resolving the spelling instead
  found no such file in scope and left the link alone, so `rename`
  answered success with `total_updated: 0` over edges it had stranded.
  The span half is read under the same grammar: an escaped delimiter ends
  nothing (`<a\>b.md>`, `[a\]b]:`) — including in this crate's own pointy
  output — and a link inside a link label opens a link of its own, so the
  parser's open links are a stack rather than a slot. Each of those cut a
  span the decoder never would, which is the one defect this pairing
  exists to prevent, arriving from the other side.
- A rewrite is a proposal, and `reference_rewrite::apply_proposals` is
  where it is accepted: the reader that found a reference has to find the
  intended target in the bytes a write would leave. `ReferenceForm` carries
  that reader on the span — the parser for a destination, the pattern for a
  capture, the citation probe for a code span — so `rename`, a
  cross-directory rebase, and `retarget` ask one question instead of each
  carrying the half of the answer its own defect taught it.
  What the reader must accept is the document the write *will leave*, not
  the document as it stands, so proposals are applied in document order and
  each is read back in the document carrying the ones accepted before it,
  the frontmatter boundary checked there too. Judged alone, three captures
  on a line reading `xxx` each rewrite to `-` without moving that boundary;
  together they spell `---`, and the document took somebody else's id and
  lost every edge under an envelope reporting success. Two proposals
  claiming overlapping source need no separate rule for the same reason —
  the second is asked about the text the first left.
  The finished document must then read back every reference the original
  had, the ones left alone included. A pattern whose match reaches past its
  capture depends on text another reference occupies, so repointing both
  leaves the first matching nowhere: its edge does not come to dangle,
  which `check` reports — it stops existing, which nothing reports. Which
  rewrite cost it *is* answerable, and by the trial itself: a trial differs
  from what is accepted by one rewrite, so what the trial loses, that
  rewrite cost. A reference found lost is named, and from then on a rewrite
  the naming survives is refused — which gives up the culprit rather than
  everything after it. A pass names at least one reference no pass named
  before, so passes are bounded by references, and only what the document
  reads back to begin with can be named. A rewrite therefore never trades
  an edge for a repoint; the worst it does is leave a reference naming a
  file that has moved, which `check` says. A rewrite may take only a
  reference it replaced *entirely*: that one has no bytes of its own left,
  so it is read by what the rewrite wrote rather than by where it sat —
  `docs/a.md` repointed to `docs/b.md` no longer spells the `a.md` a
  basename pattern captured and was its to take, while repointed to
  `docs2/a.md` it still spells it, survived, and a later rewrite may not
  cost it. Both halves are load-bearing. Taking is decided by the
  rewrite's own output, because a later one changing the text again is a
  loss and not a taking; and it is decided by enclosure rather than
  overlap, because a reference a rewrite only reaches into still stands in
  the bytes outside it — read by overlap, a short rewrite would take the
  long reference around it, which silently moved an edge off a file that
  still existed. A reference a rewrite reached into without enclosing is
  read *nowhere* (`Landing::Severed`) and the rewrite refused: what is left
  of its text no longer joins up, and a range widened to cover the rewrite
  let a destination beside it answer in its place. Such a reference can
  survive, where what the rewrite wrote re-spells the bytes it took, and it
  is refused there too — finding it would mean reading it at an image the
  document does not say it has. That costs help, never safety: the
  reference stays, naming a file that has moved, and `check` says so.
  A move owes a reference one thing — that it go on naming the document it
  named — and `rewrite_for_move` is that one rule, because which of the two
  files moved is not a second question. A reference names a document; the
  move gives that document a new path or gives the referring file a new
  vantage point, and either way the spelling is recomputed from what it
  named, in the frame that read it. Which document that is comes from the
  ladder the graph binds edges with, not from a set of scanned paths: a
  candidate that is a file but carries no document is not a binding, and
  read as one it stranded the edge the build had bound lower down. Split in
  two — repoint what moved, then rebase the vantage point — the moved
  document was rewritten twice over one buffer, and the second pass read
  the first's output as the text its author had written, which made every
  claim it went on to publish about a self-reference a claim about a
  spelling that had never been in the document.
  What no re-rendering reaches is a reference that comes out spelled as it
  went in, and a relative one means whatever it means from where it now
  sits, so it can come to name a different document — a valid graph `check`
  has nothing to say about. `Rewritten::rebound` carries those and `rename`
  warns with what the reference named and what it names now, once per
  reference however many readers found it. Which references those are is
  `standing`, and it asks the finished document rather than the map of
  where each rewrite landed: a reference a rewrite wrote *over* can survive
  it word for word — `[t](sub/w.md)` rebased to `[t](c/sub/w.md)` still
  says `w.md` — and that reference is as much the author's as any other,
  while what it reaches may have changed under it. Read off the landing map
  instead, the one reference a move could rebind unreported was the one it
  rebound by carrying somewhere else.
  Only a destination has a choice of *encoding*, and it is offered them in
  order (as written, escaped, pointy) until one reads back. A path also
  has spellings by *frame*, and there every form has two: the frame that
  read it, then that frame said out loud — for the reference a plain
  spelling would lose to a document arriving beside it. What saying it
  amounts to is the vocabulary's answer rather than the renderer's: `./`
  for a destination, which spells a path and which every reader of a path
  follows; naming from the root for a capture, because no wikilink
  vocabulary has `./` and writing it would trade a link readers follow for
  one only this graph does. What no
  spelling reaches is a name a destination cannot *mean* — one carrying
  `#`, spelled with edge whitespace, or beginning with a URI scheme — and a
  move onto one is refused by the write gate exactly when the project's own
  `[[detection.unresolved_policy]]` calls the stranded reference an error.
  Every other proposal that cannot round-trip is dropped and the rest go
  on: it stays visible, and surfaces as an unresolved edge, rather than
  being mangled into a reference to nothing.

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
