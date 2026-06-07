# nodex-core

Library crate. All graph / config / rule logic lives here; the CLI is
a thin wrapper.

## Layering invariants

- The `nodex_core::*` facade re-exports (`lib.rs`) are the canonical
  names. Use them in tests and external embeds; reach into module
  paths only for items the facade intentionally does not surface
- `path_guard::normalize_doc_path` is the single normalization every
  user-supplied document path passes through (fold `\` to `/`, refuse
  traversal/absolute, collapse `.` segments) — `scaffold`, `rename`,
  and `check --content` key id inference, scope probes, rewrites, and
  the write itself on its result, so a probe verdict and the written
  artifact can never disagree about which document was named
- `path_guard::write_atomic_in_root` is the only legitimate write
  primitive for user-addressed mutation targets (scaffold, lifecycle,
  migrate, rename's id anchor, retarget) — it refuses a
  final-component symlink target and enforces root containment
  (`reject_outside_root`, symlinked-ancestor aware) before the atomic
  write, so no handler can forget either guard. Batch *reference
  rewrites* (rename, retarget) route through `mutate::apply_to_file` —
  the one seam owning the reader-follows / writer-skips symlink
  discipline and its skip warnings. Infra writers whose target derives
  from load-validated config (`output.dir`, build cache, init) use
  `path_guard::write_atomic`. No `std::fs::write` in mutation paths
- `rules::body_immutable::rewrite_lock_reason` is the writer-skip lock
  probe shared by rename / retarget: a rewrite `check` would flag — a
  body lock, or (retarget) a `frontmatter_immutable` block covering a
  relation field — is skipped with a warning, never performed.
  Conservative on purpose: terminal-trigger keys on the *current*
  status, creation-trigger on the baseline snapshot of the *before*
  path; identity (kind) is judged at the before-path
- `model::validate_explicit_id` gates a reference-unsafe id
  (trim-unstable, wikilink metacharacters) at every write seam that
  accepts one: `scaffold --id` and `retarget <new-id>`. For configured
  `link_patterns`, `reference_rewrite` additionally verifies each
  rewritten span re-captures the successor id (round-trip guard) and
  leaves un-round-trippable spans untouched
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
  `trust.overrides`

## Build modes

`builder::BuildMode` couples content source to cache persistence:
`WorkingTree` is the only mode that persists the cache; `Overlay` is
read-only — proposed bytes substitute the disk read (the substrate of
`check <path> --content`), so unwritten content never leaks into
`cache.json`. `scanner::scan_scope_with_overlay` is the single scope
authority (overlay paths join under the same static policy;
`conditional_exclude` reads a parent's status overlay-first), so an
overlay graph and the real post-write build can never disagree about
membership.

## Fallback mechanisms (intentional, not optional)

Core invariants: every document always gets a valid kind, id, and
status, so an incomplete config can never break the graph. Declare
exhaustive rules to override them.

- **kind**: `FALLBACK_KIND = "generic"` when no `identity.kind_rules`
  glob matches — so `kinds.allowed` MUST include "generic" (enforced
  at load)
- **id**: "{kind}-{stem}" when no `identity.id_rules` rule matches
- **status**: `statuses.initial` when declared, else the first
  `statuses.allowed` value (kind-independent — a `status` enum is a
  *set*, not an ordering). Used by `scaffold`, `migrate`,
  frontmatter-less parses; `Config::validate` rejects a config whose
  implicit default a declared `status` enum excludes
- **orphan grace**: documents created < `orphan_grace_days` ago are
  exempt from orphan detection (`u32`, not `Option` — `0` = no grace).
  Also exempt: `orphan_ok_kinds` membership, per-node `orphan_ok: true`

## Naming conventions

`query/` functions are pure (read graph + config, no side effects; the
`lib.rs` facade re-exports the stable set). Prefixes disclose ranking:
`find_*` = traversal/filter (structural, no ranking); `compute_*` =
value computation (similarity, trust, diff). Text-scored matching is
`query/search.rs::search` — the module name is the verb.

Input specs: `*Filter` is a pure predicate (every field narrows, no
tuning knobs; presentation capping is the CLI's job — core returns
complete results, see `.claude/rules/json-output.md`). `*Options`
carries ranking / threshold / selection knobs; a spec whose `limit` is
*selection semantics* (top-K / window — `RecentOptions`,
`SimilarityOptions`, `TrustListOptions`) is `*Options` even when most
fields narrow.

Rule types end with `Rule`. Per-block config-driven rules carry
`RuleSource::Config` and a `<family>/<name>` qualified id; built-ins
keep the family name verbatim (`required_field`, `stale_review`,
`filename_pattern`). Output types: `*Manifest` (exports), `*Report`
(aggregates), `*Result` (mutation outcomes — kept in
`command_result.rs` or the command's own module so
`export::per_command_schemas` derives JSON Schema from the same Rust
type the CLI emits), `*Ref` (flat projections), `*Entry` / `*Group`
(items-list sub-elements). One stem per domain concept across item /
components / options / function (`TrustEntry`, `TrustComponents`,
`TrustListOptions`, `compute_trust`); the CLI `*Args` stem matches its
core stem.

## Detection thresholds (explicit semantics)

`stale_days` / `git_drift_threshold` are `Option<u32>` — `None`
disables, `Some(0)` rejected at load (ambiguous between "off" and
"flag immediately"). `orphan_grace_days` is plain `u32` — a duration,
so `0` is valid. The differing type is deliberate: a threshold
toggles, a duration is always active.

## Cache invalidation

`parser::ParseConfig` is the exact slice of `Config` that parsing
reads (identity, statuses, parser, annotations, body_line); parsing
takes `&ParseConfig`, so a new parse-affecting option cannot be added
without surfacing there — the compiler enforces it. `schema` is
deliberately excluded: it steers check-time validation, never a cached
parse. `ParseConfig::cache_key()` is the build cache key — SHA-256
over that serialised surface plus `CARGO_PKG_VERSION`. Consequences:
rule / annotation / id_rules *reordering* invalidates (order is
semantic — first match wins); whitespace / comment edits never do (the
hash is over parsed structs, not TOML text); parse-irrelevant config
(`schema`, `trust`, `similarity`, `detection`, `scope`, `kinds`,
naming rules) never does; a binary upgrade invalidates once, guarding
serialised `Node` / `RawEdge` struct-shape drift.

## Cycle detection

`rules/graph_invariants.rs::CycleDetectionRule` checks every
`rules.acyclic_relations` relation (default `["implements"]`; entries
validated at load, empty list rejected) over resolved edges.
`supersedes` is validated separately and harder — a build-time `Error`
from `builder::validate_supersedes_dag`; `covers` names out-of-graph
code paths and cannot cycle. A cycle violation is Error severity and
node-less (`node_id: None`, `path` = a representative ring member, the
message carries the full ring) — a project-wide finding, so `--since`
/ `--content` violation narrowing never drops it.

## Data flow invariants

- Every parser entry routes file content through
  `parser::frontmatter::canonicalize` (BOM strip + `\r\n`/`\r` → `\n`)
  so fingerprints, regex matches, and line iteration agree across
  mixed-line-ending sources.
- Body-text scanners share `parser::body::iter_body_lines` so
  fence-aware iteration has one implementation.
- Per-block `kinds: Vec<String>` filters are evaluated via
  `Node::matches_kinds(&kinds)` (empty list = no restriction);
  `Config::validate_kinds` rejects typos at load.
- All check-time rules are pure functions of `(graph, config)`. The
  parser extracts body-derived data once at build time; no rule
  re-reads files at check time.
- Link patterns must have exactly one capture group (the link target);
  zero or multiple groups are rejected at config load.
- Reference handling has exactly one extraction and one resolution,
  shared by every consumer, so build and mutation can never disagree.
  Extraction: `parser::body` finds references once — markdown link
  destinations (incl. reference-style/collapsed/shortcut definitions)
  via the pulldown-cmark token stream, plus `[[wikilink]]` / custom
  `parser.link_patterns` captures, code-span and frontmatter aware;
  the builder (`extract_links`) and the rewriter (`reference_rewrite`)
  consume the same helpers — never a private regex re-scan.
  Resolution: `builder::resolver::reference_path_candidates` is the
  single ladder — literal/relative path, then path + each
  `parser.extensions` suffix, then bare node id (`covers` stays
  path-only); the build resolver, the unresolved-edge classifier, and
  the rewriter all consume it.
- `reference_rewrite` rewrites a reference only when it resolves to
  the renamed/retargeted target under that shared ladder (against the
  pre-move scope) — exactly the references the builder bound as edges.

## Graph serialization

`Graph` has hand-written `Serialize` / `Deserialize`. Adjacency
indices are derived state — rebuilt inside `Deserialize` via
`Graph::new`. Bump `SCHEMA_VERSION` in `model/graph.rs` on any
on-disk shape change.

## Adding a validation rule

1. Implement `Rule` (`id`, `severity`, `check`, `description`).
2. Register in `rules::registered_rules(config)` — the single registry
   both `rules::check` and `export::export_rules` read from.
3. Read only from `RuleContext`; never touch the filesystem outside
   `root` (git-class rules only). Consume merged config views
   (`required_for`, `types_for`, …) — never raw `schema_override_for`.
4. Diff-aware rule: `is_applicable` returns `false` when
   `ctx.since.is_none()`, with a `skip_reason` — silent non-fires are
   forbidden (see `.claude/rules/config-driven.md`).
5. Per-block kind filter: carry `kinds: Vec<String>`, gate with
   `node.matches_kinds(...)`; `Config::validate_kinds` rejects typos at
   load, immutability families also route `validate_immutable_blocks`.
6. Surface in `export::export_rules` only when active under the
   current config, mirroring `is_applicable` — `Builtin` for
   code-shipped rules, `Config` for per-block instances.
