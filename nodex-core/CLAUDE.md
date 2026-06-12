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
- `path_guard::write_atomic_in_root` is the single public write
  primitive — document mutations (scaffold, lifecycle, migrate,
  rename's id anchor, retarget), infra artifacts (graph.json, GRAPH.md,
  cache.json), and init's nodex.toml alike: it refuses a final-
  component symlink (a user's symlinked artifact is loudly refused,
  never silently replaced by the staged rename) and enforces root
  containment (`reject_outside_root`, symlinked-ancestor aware) before
  the atomic write, so no writer can opt out. `Config::validate_output`
  is the lexical early-feedback half for `output.dir`; the filesystem
  half is a property of the primitive. Batch file rewrites (rename,
  retarget, migrate --apply) route through `mutate::apply_to_file` —
  the one seam owning the reader-follows / writer-skips symlink
  discipline and the immutability lock consult. No `std::fs::write` in
  mutation paths
- `mutate::BaselineProbe::resolve(root, config)` binds
  `rules.immutable_baseline` once per mutating command — inert (every
  `content()` answers `None`) unless a baseline is configured, the
  project declares immutability rules, and root is a git work tree
  (byte-level git access lives in core `git::{is_work_tree,
  ref_file_content}`). Every mutation seam requires it:
  `mutate::apply_to_file` consults
  `rules::body_immutable::rewrite_lock_reason` with its per-call
  `RewriteLock { baseline_path, frontmatter_relations }` (rename's
  moved file reads its baseline at the old path; retarget engages
  relation-field locks), `lifecycle::transition` consults
  `frontmatter_write_lock`, and scaffold's recreate / `--force` path
  consults `rewrite_lock_reason` directly. The probes compute exactly
  what a `check` against the baseline would: a lock engages only when
  the write changes the *locked aspect* — a body lock on a body-
  fingerprint change (gated on baseline status for `terminal`, baseline
  presence for `creation`), a `frontmatter_immutable` lock when a
  locked id-relation field changes on a baseline-terminal doc. Inert
  probe → the diff-aware rules cannot fire at check time, so nothing is
  locked
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
--content` and of scaffold's before/after validation, which builds its
graphs live instead of reading a snapshot), so unwritten content never
leaks into `cache.json`. Both proposal gates share one attribution
policy: a proposal is refused on exactly the Error-severity violations
the overlay *introduces* (`rules::introduced_violations` — a
count-aware multiset difference by exact `Violation` equality against
the pre-overlay report, so a duplicate of a pre-existing violation
still refuses) — a pre-existing project violation never blocks an
unrelated write.
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

Because the parser resolves id / title / kind / status / orphan_ok for
every document (`INFERRED_FRONTMATTER_FIELDS`), a `schema.required` or
`cross_field.require` entry naming one could never fire —
`Config::validate` rejects both at load (`orphan_ok` stays legal as a
`require` target: its boolean is structurally present, the documented
predicate contract in `rules/schema.rs::is_field_missing`).

Built-in frontmatter fields parse leniently, field by field: a value
that fails its type (bad date, bad bool, non-string scalar, malformed
list) records a `FieldParseIssue` on the node and the field reads as
absent under exactly the fallbacks above — nothing is fabricated, and
the failed value never reaches `attrs`. Only unparseable YAML, a
non-mapping block, or an opened-but-unclosed fence
(`ParseError::FrontmatterDelimiter` — the close fence is the first
whole line `^---[ \t]*$`, newline- or EOF-terminated) drop the
document, and the drop is canonical graph data
(`Graph::parse_failures`). Two always-registered built-ins make both
states Error-severity check findings: `field_parse` (node-attributed)
and `parse_failure` (node-less). Write seams split reader-degrades /
writer-refuses: `lifecycle` refuses a document carrying parse issues
or an unsplittable fence, `rename` / `retarget` / `migrate` refuse or
per-file-skip the unsplittable fence, `scaffold` with supplied content
refuses on the introduced `field_parse` / `parse_failure` violation
through its overlay delta, while the scanner terminality probe (a
read-only probe) degrades conservatively — the same file is guaranteed
to red `check`.

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
drift. `cache.json` additionally carries its own shape guard
(`CACHE_SCHEMA_VERSION` in `builder/cache.rs`, checked on load like
graph.json's `SCHEMA_VERSION`): a mismatching or absent version
discards the cache — cold rebuild, never an error — so old-shape
entries can never deserialize leniently into defaulted fields.

`scanner::ScanConfig` is the membership twin: the exact slice of
`Config` that decides scope (`scope`, `output.dir`, and
`statuses.terminal` only when a `conditional_exclude` can consult it).
Public scan functions project into it immediately and every private
helper takes `&ScanConfig`, so a new membership-affecting option
cannot be read without surfacing there. `builder::graph_config_hash`
— SHA-256 over `CARGO_PKG_VERSION` + `ParseConfig` + `ScanConfig` —
is recorded as `GraphMeta::config_hash` on every built snapshot, so
graph.json self-describes which config shaped it; check-only tuning
never perturbs it. `Config::validate_scope` compiles the include and
effective-exclude globs from the same projection at load, so
load-accept implies scan-success.

## Cycle detection

`rules/graph_invariants.rs::CycleDetectionRule` checks every
`rules.acyclic_relations` relation (default `["implements"]`; validated
at load, empty list rejected) over resolved edges. `supersedes` is
validated separately and harder — a build-time `Error` from
`builder::validate_supersedes_dag`; `covers` names out-of-graph code
paths and cannot cycle. A cycle violation is Error severity and
node-less (`node_id: None`, `path` = a ring member, message carries the
full ring) — a project-wide finding, so `--since` narrowing never drops
it, and the `--content` gate's before/after delta cancels it only when
the identical violation pre-existed the proposal.

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
  `parser.extensions` suffix → bare id; `covers` stays path-only —
  `model::edge::is_document_ref_relation` is the one predicate, and the
  relation is producible only by the frontmatter `covers:` field
  because `Config::validate` rejects a link pattern naming it), shared
  by the build resolver, the unresolved-edge classifier, and the
  rewriter. `reference_rewrite` touches a reference only when it
  resolves to the moved/retargeted target under that ladder against the
  pre-move scope — exactly the edges the build bound.
  `resolver::normalized_resolution_candidates` projects the ladder to
  normalized root-relative form (root- and source-relative
  interpretations, escaping candidates dropped) — the one definition of
  "what could this link mean" consumed by the unresolved-cause probes
  (`Graph::parse_failures` → `target_unparsed`; in-root stat →
  `excluded_from_scope` vs `missing`) and by
  `[[detection.unresolved_policy]]` row globs, which match these
  candidates, never the raw authored target. The policy is the one
  judgment seat for unresolved references: first matching (cause,
  glob?) row assigns `error` (per-row check rule
  `unresolved_reference/<name>`), `info` (reported out of
  `summary.total` under the row's name), or the counted `warning`
  fallthrough — every edge stays visible with its per-edge `severity` +
  `policy_name` attribution, and the default table is the single
  `excluded_target` info row (declaring the table replaces it).

## Graph serialization

`Graph` has hand-written `Serialize` / `Deserialize`. Adjacency
indices are derived state — rebuilt inside `Deserialize` via
`Graph::new`. The snapshot carries its own provenance (`meta:
{nodex_version, config_hash}`) and its recorded drops
(`parse_failures`, each with the content digest the cache keys on);
both default during deserialisation so an older file fails through the
schema-version message, never a missing-field error. Bump
`SCHEMA_VERSION` in `model/graph.rs` on any on-disk shape change.

## Snapshot introspection

`status.rs` owns every read of `graph.json`. `load_graph(root,
config)` is the single snapshot-read seam: a missing file is the typed
`Error::MissingGraph` (`GRAPH_MISSING`), and every read attaches an
exact membership+config divergence warning — advisory only, never a
gate; a probe failure degrades to a warning. Snapshot coverage is
nodes ∪ `parse_failures`: a recorded parse failure is
covered-but-unbuildable (`nodex status` surfaces it as
`unbuildable_paths`; `check`'s `parse_failure` rule reds it), so a
faithfully-built snapshot never reads stale for a document a rebuild
cannot fix. `compute_divergence(graph, config, root, probe)` is the
shared primitive — `Membership` (every `query *` read) never reads
document content (`changed_paths: None` = unmeasured, never "fresh");
`Content` (`nodex status` only) par-hashes the corpus against each
recorded `content_hash`.

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
