# nodex-core

Library crate. All graph / config / rule logic lives here; the CLI is
a thin wrapper. Each section is an invariant — violating one breaks the
design. Full rationale lives in the cited rustdoc.

## Layering invariants

- The `nodex_core::*` facade re-exports (`lib.rs`) are the canonical
  names — use them in tests and embeds; reach into module paths only for
  items the facade doesn't surface.
- `path_guard::normalize_doc_path` is the single normalization for every
  user-supplied document path (fold `\`→`/`, refuse traversal/absolute,
  collapse `.`). `scaffold`, `rename`, and `check --content` key id
  inference, scope probes, rewrites, and the write on its result through
  it, so a probe verdict and the written artifact never disagree about
  which document was named.
- `path_guard::write_atomic_in_root` is the single public write
  primitive — every document mutation (scaffold, lifecycle, migrate,
  rename's id anchor, retarget) and infra artifact (graph.json, GRAPH.md,
  cache.json, init's nodex.toml) routes through it; it refuses a
  final-component symlink and enforces root containment. `std::fs::write`
  in a mutation path is a defect. Batch file rewrites (rename, retarget,
  migrate --apply) route through `mutate::apply_to_file` — the one seam
  owning the reader-follows / writer-skips symlink discipline and the
  immutability-lock consult.
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
  seam (`mutate::apply_to_file`, `lifecycle::transition`, scaffold's
  recreate / `--force` path) locks against the same one. The two planes
  cannot disagree about a baseline they share.
  A lock finds its before-snapshot **by node id**
  (`rules::body_immutable::rewrite_lock_reason`), which is how `check` pairs
  snapshots, so a document that moved — or that the filesystem spells
  differently than the tree does — is the same document to both. A
  *creation* reaches the baseline two ways, and `recreate_lock_reason` asks
  both, id first: the id it claims — how `check` pairs, so a record
  re-created elsewhere under its own id is still that record — and the path
  it lands on, because an overwrite replaces one record with a *different*
  one, which `check` reports as a removal plus an addition, so no id pairs
  them and only the path can ask. Asking one address leaves the other
  unguarded.
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
- `path_guard::find_scope_alias` is the one filesystem-alias test (case /
  NFC-NFD spellings whose canonicalized location equals a tracked
  document's but whose spelling differs); the rename source gate and the
  `check --content` admission gate both consult it.
  `model::ID_RELATION_FIELDS` is the single id-valued relation-field
  vocabulary the frontmatter-lock probe reads.
- Rules read from `RuleContext { graph, config, root, since }`.
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
  `scaffold` / `migrate` render defaults via the reparse-the-real-node
  discipline (shared with the lifecycle write seam): each `cross_field`
  predicate is evaluated against a node parsed from the frontmatter
  written so far, iterated to a fixpoint — never a synthetic stand-in —
  so the written document passes the same config's `check` by
  construction.

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
