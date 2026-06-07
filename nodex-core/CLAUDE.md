# nodex-core

Library crate. All graph / config / rule logic lives here; the CLI is
a thin wrapper.

## Layering invariants

- The `nodex_core::*` facade re-exports (`lib.rs`) are the canonical
  names — use them in tests and embeds; reach into module paths only
  for items the facade doesn't surface
- `path_guard::normalize_doc_path` is the single normalization every
  user-supplied document path passes through (fold `\`→`/`, refuse
  traversal/absolute, collapse `.`) — `scaffold`, `rename`, and `check
  --content` key id inference, scope probes, rewrites, and the write on
  its result, so a probe verdict and the written artifact can never
  disagree about which document was named
- `path_guard::write_atomic_in_root` is the only write primitive for
  user-addressed mutation targets (scaffold, lifecycle, migrate,
  rename's id anchor, retarget): it refuses a final-component symlink
  and enforces root containment (`reject_outside_root`, symlinked-
  ancestor aware) before the atomic write. Batch *reference rewrites*
  (rename, retarget) route through `mutate::apply_to_file` — the one
  seam owning the reader-follows / writer-skips symlink discipline.
  Infra writers off load-validated config (`output.dir`, cache, init)
  use `path_guard::write_atomic`. No `std::fs::write` in mutation paths
- `rules::body_immutable::rewrite_lock_reason` is the writer-skip lock
  probe shared by rename / retarget; it computes exactly what a `check`
  against `rules.immutable_baseline` would. Given the document's committed
  baseline bytes (the caller fetches them) it diffs baseline-vs-after and
  engages a lock only when the rewrite changes the *locked aspect*: a body
  lock on a body-fingerprint change (gated on baseline status for
  `terminal`, baseline presence for `creation`), a `frontmatter_immutable`
  lock when a locked id-relation field changes on a baseline-terminal doc.
  No baseline → the diff-aware rules are inert and so is the probe
- `model::validate_explicit_id` gates a reference-unsafe id
  (trim-unstable, wikilink metacharacters) at every write seam that
  accepts one: `scaffold --id` and `retarget <new-id>`. For configured
  `link_patterns`, `reference_rewrite` additionally verifies each
  rewritten span re-captures the successor id (round-trip guard) and
  leaves un-round-trippable spans untouched
- `path_guard::find_scope_alias` is the one filesystem-alias test
  (case / NFC-NFD spellings whose canonicalized location equals a
  tracked document's but whose spelling differs); the rename source
  gate and the `check --content` admission gate both consult it, so
  neither moves a real document out from under its references through a
  phantom second node. `model::ID_RELATION_FIELDS` is the single
  id-valued relation-field vocabulary the frontmatter-lock probe reads
  (`retarget` keeps a private list-valued subset `LIST_RELATION_FIELDS`
  — `superseded_by` is a scalar it handles separately)
- Rules read only from `RuleContext { graph, config, root, since }`;
  external-tool probes (git, …) wire into `rules::preflight`, never
  inside `Rule::check`. Diff-aware rules (need `ctx.since`, supplied
  by `--since`, `rules.immutable_baseline`, or `check <path>
  --content`) self-report as non-applicable via `is_applicable`; the
  runner records the refusal in `skipped_rules` — silent non-fires are
  forbidden
- `Config` is the single source of truth for vocabulary. Tool actions
  that write frontmatter consume merged views (`required_for`,
  `types_for`, `enums_for`, `cross_field_for`, `declared_fields_for`,
  `trust_weights_for`) — never raw `schema.overrides` or
  `trust.overrides`. `scaffold`/`migrate` render defaults via the
  reparse-the-real-node discipline (shared with the lifecycle write
  seam): each `cross_field` predicate is evaluated against a node
  parsed from the frontmatter written so far, iterated to a fixpoint —
  never a synthetic stand-in — so a `when` keyed on a field scaffold
  itself defaulted still fires and the written document passes the same
  config's `check` by construction

## Build modes

`builder::BuildMode` couples content source to cache persistence: only
`WorkingTree` persists the cache; `Overlay` is read-only — proposed
bytes substitute the disk read (the substrate of `check <path>
--content`), so unwritten content never leaks into `cache.json`.
`scanner::scan_scope_with_overlay` is the single scope authority
(overlay paths join under the same static policy; `conditional_exclude`
reads a parent's status overlay-first), so an overlay graph and the
real post-write build can never disagree about membership.

## Fallback mechanisms (intentional, not optional)

Core invariants — every document always gets a valid kind, id, status,
so an incomplete config can never break the graph (declare exhaustive
rules to override):

- **kind**: `FALLBACK_KIND = "generic"` when no `identity.kind_rules`
  matches — so `kinds.allowed` MUST include "generic" (enforced at load)
- **id**: "{kind}-{stem}" when no `identity.id_rules` matches
- **status**: `statuses.initial` when declared, else the first
  `statuses.allowed` value (kind-independent — a `status` enum is a
  *set*, not an ordering). Used by `scaffold` / `migrate` /
  frontmatter-less parses; `Config::validate` rejects a config whose
  implicit default a declared enum excludes
- **orphan grace**: docs created < `orphan_grace_days` ago skip orphan
  detection (`u32` not `Option` — `0` = no grace); also exempt:
  `orphan_ok_kinds` membership, per-node `orphan_ok: true`

## Naming conventions

`query/` functions are pure (read graph + config; `lib.rs` re-exports
the stable set). Prefixes disclose ranking: `find_*` = traversal/filter
(no ranking), `compute_*` = value computation (similarity, trust,
diff); text-scored matching is `query/search.rs::search`.

Input specs: `*Filter` = pure predicate (every field narrows, no tuning
knobs; the CLI caps presentation — core returns complete results, see
`.claude/rules/json-output.md`). `*Options` = ranking / threshold /
selection knobs; a spec whose `limit` is *selection semantics* (top-K /
window — `RecentOptions`, `SimilarityOptions`, `TrustListOptions`) is
`*Options` even when most fields narrow.

Rule types end with `Rule` (per-block config rules: `RuleSource::Config`
+ a `<family>/<name>` id; built-ins keep the family name —
`required_field`, `stale_review`, `filename_pattern`). Output types:
`*Manifest` (exports), `*Report` (aggregates), `*Result` (mutation
outcomes — in `command_result.rs` or the command's module so
`export::per_command_schemas` derives JSON Schema from the same emitted
type), `*Ref` (flat projections), `*Entry` / `*Group` (items-list
elements). One stem per concept across item / components / options /
function (`TrustEntry`, `TrustComponents`, `TrustListOptions`,
`compute_trust`); the CLI `*Args` stem matches its core stem.

## Detection thresholds (explicit semantics)

`stale_days` / `git_drift_threshold` are `Option<u32>` — `None`
disables, `Some(0)` rejected at load (ambiguous: "off" vs "flag
immediately"). `orphan_grace_days` is plain `u32` (a duration), so `0`
is valid — the differing type is deliberate. `git_drift::commits_since`
returns `Option<u32>`: `None` = unmeasurable, distinct from `Some(0)` =
no drift. Neither fabricates max trust from absence (the `backlinks`
discipline): the check rule (guarded by `rules::preflight` up front)
skips an unmeasurable edge; the trust composite drops the whole drift
component — so a per-path git anomaly or a direct library caller can
never read absence as "no drift".

## Cache invalidation

`parser::ParseConfig` is the exact slice of `Config` parsing reads
(identity, the resolved initial status, parser, annotations,
body_line); parsing takes `&ParseConfig`, so a new parse-affecting
option cannot be added without surfacing there — the compiler enforces
it. It stores the *resolved* initial status `&str`, not the whole
`statuses`, so `statuses.terminal` (pure check-time) cannot force a
reparse by type. `cache_key()` is the build cache key — SHA-256 over
that surface plus `CARGO_PKG_VERSION`. Consequences: rule / annotation
/ id_rules *reordering* invalidates (order is semantic); whitespace /
comment edits never do (the hash is over parsed structs); parse-
irrelevant config (`schema`, `trust`, `similarity`, `detection`,
`scope`, `kinds`, `statuses.terminal`, naming rules) never does; a
binary upgrade invalidates once, guarding `Node` / `RawEdge` shape
drift.

## Cycle detection

`rules/graph_invariants.rs::CycleDetectionRule` checks every
`rules.acyclic_relations` relation (default `["implements"]`; validated
at load, empty list rejected) over resolved edges. `supersedes` is
validated separately and harder — a build-time `Error` from
`builder::validate_supersedes_dag`; `covers` names out-of-graph code
paths and cannot cycle. A cycle violation is Error severity and
node-less (`node_id: None`, `path` = a ring member, message carries the
full ring) — a project-wide finding, so `--since` / `--content`
narrowing never drops it.

## Data flow invariants

- Every parser entry routes content through
  `parser::frontmatter::canonicalize` (BOM strip + `\r\n`/`\r` → `\n`)
  so fingerprints, regex matches, and line iteration agree across
  line-ending styles. Body scanners share `parser::body::iter_body_lines`
  (one fence-aware iterator).
- Per-block `kinds: Vec<String>` filters go through
  `Node::matches_kinds` (empty = no restriction); `validate_kinds`
  rejects typos at load. Link patterns need exactly one capture group
  (rejected otherwise at load).
- Check-time rules are pure functions of `(graph, config)`; the parser
  extracts body-derived data once at build time, no rule re-reads files.
- Reference handling has one extraction and one resolution, so build
  and mutation can never disagree. Extraction: `parser::body` finds
  references once (pulldown-cmark markdown destinations incl.
  reference/collapsed/shortcut, plus `[[wikilink]]` / `link_patterns`,
  code-span + frontmatter aware); `extract_links` and `reference_rewrite`
  consume the same helpers. Resolution: `reference_path_candidates` is
  the single ladder (literal/relative path → path + each
  `parser.extensions` suffix → bare id; `covers` stays path-only),
  shared by the build resolver, the unresolved-edge classifier, and the
  rewriter. `reference_rewrite` touches a reference only when it
  resolves to the moved/retargeted target under that ladder against the
  pre-move scope — exactly the edges the build bound.

## Graph serialization

`Graph` has hand-written `Serialize` / `Deserialize`. Adjacency
indices are derived state — rebuilt inside `Deserialize` via
`Graph::new`. Bump `SCHEMA_VERSION` in `model/graph.rs` on any
on-disk shape change.

## Adding a validation rule

1. Implement `Rule` (`id`, `severity`, `check`, `description`); register
   in `rules::registered_rules(config)` — the single registry both
   `rules::check` and `export::export_rules` read from.
2. Read only from `RuleContext`; never touch the filesystem outside
   `root` (git-class rules only). Consume merged config views
   (`required_for`, `types_for`, …) — never raw `schema_override_for`.
3. Diff-aware rule: `is_applicable` returns `false` when
   `ctx.since.is_none()`, with a `skip_reason` — silent non-fires are
   forbidden (see `.claude/rules/config-driven.md`).
4. Per-block kind filter: carry `kinds: Vec<String>`, gate with
   `node.matches_kinds(...)`; `Config::validate_kinds` rejects typos at
   load, immutability families also route `validate_immutable_blocks`.
5. Surface in `export::export_rules` only when active under the current
   config, mirroring `is_applicable` (`Builtin` for code-shipped rules,
   `Config` for per-block instances).
