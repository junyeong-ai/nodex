# nodex-core

Library crate; graph, config and rule logic live here. Use the `nodex_core::*`
facade in `src/lib.rs` for tests and embeds, reaching into modules only for items
it does not export. This file records architectural boundaries; read the named
source/rustdoc for implementation details. Project-wide rules remain authoritative
in `.claude/rules/` rather than being copied here.

## Paths and guarded writes

- `path_guard::forward_str` normalizes authored paths: `\` separates components on
  every host. `forward_string` renders filesystem paths using host separators
  only; a Unix filename containing a literal backslash must remain reversible.
  Never combine these helpers.
- `normalize_doc_path` is the single write-path normalization for `scaffold
  --path`, both `rename` paths and `check --content`: fold separators, collapse
  `.`, refuse traversal/absolute paths and filesystem spelling aliases. Use its
  result for scope, identity inference, planning and writing. Case/Unicode aliases
  on insensitive volumes must not bypass exact graph comparisons.
  `filesystem_spelling` checks existing components individually, permits new
  components and does not resolve correctly spelled symlinks. Read lookup uses
  `normalize_for_lookup`, which also permits absolute paths under the root.
- Every content write uses `path_guard::stage_in_root` or its stage/commit wrapper
  `write_atomic_in_root`: documents and graph.json, GRAPH.md, cache.json,
  history.json, init's nodex.toml. The seam refuses final-component symlinks and
  checks root containment through ancestors; direct `std::fs::write` in a mutation
  path is a defect. Reading follows symlinks; document rewriting skips them.
- Batch rewrites use `mutate::plan_file` → `narrow` → `stage_plan` / `write_plan`.
  Planning canonicalizes both input and transformed output at `transform`, so CRLF
  cannot change how parts are split or compared. Verdicts precede writes.
  Staging prepares temporary content beside each target without replacing it;
  each commit checks the expected source revision and replaces one file atomically.
  Staging every plan prevents preparation failures from leaving earlier document
  edits, but commits can still fail: batches are not filesystem transactions.
  Preserve successful writes and report per-file failures with partial completion.
  `migrate --apply` commits each independent migration and continues after skips.
- CLI `rename` stages the final destination content (identity anchor and rebased
  references) before moving the untouched source, verifies the moved source's
  revision, then commits that content before inbound-reference edits. A failed
  move leaves source content untouched. If the destination rewrite fails after
  the move, report the surviving move as partial, stop inbound commits, and report
  `anchor_failed` when the previous id could not be fixed. No automatic rollback
  or multi-file atomicity is promised. Sequencing: `nodex-cli/src/commands/rename.rs`;
  primitive guarantees: `src/path_guard.rs` and `src/mutate.rs` rustdoc.

## Git binding

`git::Repository::discover` binds repository, work tree and project prefix once
per measuring consumer, then passes that binding explicitly. Preflight may
resolve independently because it has no result channel; never rediscover per
node. `tracked_path` maps project paths to git and `locate` maps checkouts back to
the project, including projects nested inside larger repositories.

`git::bare` alone names the executable; every invocation goes through `scoped`
(`git::command`, `Repository::command`, `checkout_command`). These clear environment
variables that redirect/reinterpret git while retaining discovery bounds
(GIT_CEILING_DIRECTORIES / GIT_DISCOVERY_ACROSS_FILESYSTEM); clippy disallows other
process spawns except individually justified ones. Location answers use separate
single-question invocations and OS paths, never a multi-answer String split.
Bindings are validated against the project. `ref_state` requires a tree at its
prefix, not a resolving file/gitlink; document baselines come from checking out
and graphing that tree, not per-document ref reads. Variable groups and invocation
safety: `src/git.rs` rustdoc.

## Mutation verdicts and baselines

- `mutate::BaselineBinding::resolve` binds `rules.immutable_baseline` once per
  command; `snapshot` pairs it with the baseline graph and is the only way to
  obtain `BaselineProbe`. The CLI's `baseline_graph` builds that graph under the
  judged project's config; both read-plane diffs and write-plane locks use it.
  An unreadable baseline is an error, not an inert lock. No work tree or no
  project at the ref is inert and disclosed on both planes, including the
  baseline build's own warnings. Activation/wiring: `src/mutate.rs` rustdoc and
  `nodex-cli/src/commands/git_checkout.rs`.
- `BaselineProbe::refusals` overlays plans and asks applicable prior-state rules:
  diff-aware locks against the baseline, step rules about the step a write would
  commit. Locks (`Rule::is_lock`) refuse absolute drift, even if it predates the
  write; other rules refuse only introduced Error-severity findings. Do not
  re-derive lock rules in a command. `ViolationDetails::part` identifies a
  `DocumentPart`; `narrow` holds that part and re-gates the surviving bytes.
  Document-wide findings and composed plans (`Planned::composed`, a move's
  destination) cannot be narrowed and are held whole. Run only the rules that
  feed this verdict, not a registry whose other answers are discarded.
- `frozen_at` / `frozen_record_lost` protect recreation of a frozen record that
  disappears from the project. Identity travels by id: a record moved elsewhere
  no longer occupies the old path, so a free old path is not itself frozen.
- `mutate::introduced` runs the whole registry before and after a proposal and
  takes a count-aware multiset delta. Unrelated standing violations do not block
  a write; duplicated findings still introduce another violation. Authored bytes
  (`ProposalDiff`, scaffold and `check --content`) activate diff-aware rules
  against the working tree; transformations use the configured baseline probe.
  All document writes except config-derived `migrate` take this gate.
  Default-only scaffold advises on findings owned by its new node; supplying
  body/fields is strict. Findings owned by other nodes or no node always refuse
  (`Introduced::owned_by_others`), irrespective of a finding's display path.
- `mutate::evicted` discloses documents a content change drops through
  `scope.conditional_exclude`. Membership is measured from the scan, over nodes
  ∪ parse failures, not inferred from missing nodes. Explicitly proposed paths
  are excluded from the advisory. Eviction itself advises rather than refuses;
  an error-classified dangling reference it introduces still refuses normally.
  Every gated write and `check --content` surfaces `Introduced::advisories`.
- `migrate` needs no introduced gate only because injected defaults preserve the
  fields the bare document already inferred. The conditional-exclude parent
  probe must use the same frontmatter parser and `parser::Completion` as builds;
  a separate YAML/default interpretation could change scope during migration.
- Seam-specific guards remain only when they are a strict subset of the rule
  gate and provide a remedy it cannot express. Do not duplicate naming/schema
  rules. `model::validate_explicit_id` protects scaffold/retarget ids against
  trim instability and reference metacharacters; custom reference replacements
  additionally require a parser round trip.

## Rule population, findings and history

- Rules use `RuleContext { graph, config, files, history, since, steps, today }`.
  Filesystem probes go through `ProjectFiles`, which includes overlay proposals;
  joining the working-tree root directly judges different bytes. Environment
  preflight and measurements are distinct: git/stat-backed rules are not graph-
  only computations. Diff-aware rules declare non-applicability via
  `is_applicable`; the runner records `skipped_rules`.
- `RuleRun::subjects` and `unjudged` state reach for every evaluated rule;
  `rule_coverage` and `skipped_rules` partition the registry. Reach is measured
  before any `--since` report narrowing. Counting conventions and new-rule
  procedure: `.claude/rules/config-driven.md` (No silent vacuous passes) and
  `.claude/rules/adding-a-validation-rule.md`.
  Diff-aware rules explicitly subtract `GraphDiff::added_ids`: `before_status`
  and `before_kind` fall back to the supplied current value for absent records.
- `rules::Since::Baseline` arms locks without narrowing. `Since::Narrowed` asks
  each rule's `touched_by` per finding. The default is `GraphDiff::touched_ids`
  (record, authored edge or annotation); node-less findings remain. Orphan widens
  through `Touched::relinked`; superseded-reference includes the target's
  successor lineage. Git drift asks whether `since..HEAD` added a measured
  commit (`DriftHistory::commits_added`), including covered code outside the
  graph, rather than pretending an uncommitted graph edit changed history.
- Step rules (`Rule::judges_steps`) read `ancestry::Step`, never endpoint diffs.
  Every plain check, issues query and write gate judges the uncommitted change
  against HEAD and all MERGE_HEADs; staged check substitutes the committed index.
  An explicit `--since` also judges each added commit against its parents; all
  step findings survive report narrowing. A baseline neither bounds nor arms
  non-diff-aware step rules; proposals against the working tree have no steps.
- Merge priors come from changes since all merge bases, in the asking rule's
  sight (`Step::priors`). An untouched line claims no move, and a line moving a
  record outside the rule's scope must not displace its visible prior. Missing
  common ancestry, disagreeing bases, ambiguous records or unreadable required
  snapshots make `Priors::known` false: count unjudged, never fabricate arrival.
  A readable parent alone does not establish all priors.
- History graphs each commit under one judged config. Unparseable documents
  recover the previous record at their path (`Repository::before_change`);
  shallow cuts leave it unknown. Git-ignored documents take no committed step.
  Write guards use the document's current status as prior only outside a work
  tree or at ignored paths, and `BaselineProbe::undeclared_move` judges the step
  the write would commit. Unknown priors refuse nothing, matching the rules.
  History semantics: `src/ancestry.rs`, `src/git.rs` and CLI checkout rustdoc.
- `rules::finding_identity` compares rule, severity, node id and typed details.
  `rules::detail::Evidence<T>` compares equal regardless of value while serde and
  schema expose the underlying value. Use it only for location/rendering data,
  not subjects. Node-less subjects belong in `details`, not the display `path`.
  Parse failures keep path + digest as identity (rendered reason is evidence);
  a move of an unparseable document thus introduces a finding and is gate-refused.
  Duplicate-number conflicts retain member ids while paths are evidence.
  Field-by-field decisions: `src/rules/detail.rs`.

## Parsing, scope and cache

- `Config` owns vocabulary. Tool writers consume merged views (`required_for`,
  `types_for`, `enums_for`, `cross_field_for`, `forbidden_for`,
  `declared_fields_for`, `trust_weights_for`), not raw overrides.
  Read identity/status with `parser::parse_document`, never the frontmatter line
  editor. Default rendering reparses the real node after each cross-field change
  to a fixpoint; a synthetic node is not an equivalent schema reading.
- `parser::Completion::resolve_identity` completes kind before id/status.
  Fallback kind is `generic` (required in allowed vocabulary), id is
  `{kind}-{stem}`, status is `Config::initial_status_for(kind)` (governing flow's
  initial, global initial, then first allowed status). The parser, scope's bare-
  parent probe and lifecycle share this chain. Inferred built-ins cannot be
  meaningfully required by presence; load rejects them in required/cross-field
  require and offers `schema.require_explicit` for authored declarations.
- Built-in field type errors become `FieldParseIssue`, read as absent and never
  enter attrs. Invalid YAML, non-mapping blocks, non-string keys and unclosed
  fences drop the node into `Graph::parse_failures`. Always-registered
  `field_parse` and `parse_failure` make both states Error findings. Readers
  degrade; writers refuse or per-file skip malformed input. Lifecycle can
  overwrite a field-level issue but cannot edit a dropped document or a document
  without frontmatter; `migrate` supplies a missing block. Exact states:
  `src/parser/frontmatter.rs` rustdoc.
- Parser entries canonicalize BOM and CRLF/CR to LF; body scans share the fence-
  aware `parser::body::iter_body_lines`. Body-derived data is extracted once at
  build time; rules do not re-read body bytes. Config kind filters with empty =
  unrestricted use `model::kind_allowed` / `Node::matches_kinds`; overrides use
  membership and cannot have empty kinds. Lock membership uses `rules::lock_holds`
  plus `Config::lock_arms`. Link patterns require exactly one capture group.
- `builder::build`, `build_with_overlay`, `build_of_ref` couple content source to
  persistence via private `BuildMode`. Only WorkingTree persists cache.json;
  Overlay is read-only and substitutes proposed bytes; Ref reads/writes no disk
  parse cache and confines its scan to the checkout. All modes share the private
  scanner authority, so proposals and real post-write builds agree on membership.
- `BuildSession` reuses parsing and prepared patterns within one invocation.
  Overlay loads the disk parse cache once, checks actual-byte hashes and never
  persists proposals. Complete-outcome reuse is Ref-only, conditional on actual
  scoped bytes, paths, config/version and scan disclosures matching; git content
  conversions precede those reads. See `src/builder/mod.rs`.
- `parser::ParseConfig::cache_key` is SHA-256 of parsing inputs + binary version:
  identity, initial-status inputs, parser, annotations and body-line inputs.
  Order-critical reordering invalidates; whitespace/check-only tuning does not.
  Cache-shape mismatch (`CACHE_SCHEMA_VERSION`, `builder/cache.rs`) discards it
  for a cold rebuild, never an error. `graph_config_hash` adds `ScanConfig`
  (scope, output dir, and conditional-exclusion status/completion inputs) and
  becomes `GraphMeta::config_hash`. Parse cache and snapshot provenance are
  different projections. `ScopeMembership` / `IdentityParse` destructure fields
  exhaustively so additions require an explicit projection decision.
  Source: `src/parser/mod.rs`, `src/builder/scanner.rs`, `src/builder/mod.rs`.
- Hidden admission (`hidden_admitted`) asks each matching include glob whether
  replacing a segment's leading dot loses the match. Walk hints
  (`IncludeLead::may_hold_hidden`, `could_reach`) may walk wider, never narrower:
  every ancestor of an admitted path must be reachable. Read compiled glob
  behavior, not textual wildcard guesses; `include_leads` is shared by pruning
  and diagnostics. See scanner rustdoc and
  `the_walk_never_stops_short_of_what_admission_would_take`.
- Every corpus reader surfaces its scan's coverage/boundary warnings. Graph
  readers forward `BuildOutcome::warnings`; scan-only readers use
  `scanner::coverage_warning` / `boundary_warning`; snapshot readers forward
  `DivergenceOutcome` reach. Do not reconstruct subsets. An error has no warnings
  channel: `Error::MissingNode` carries `error::Corpus` into the NOT_FOUND message.

## Detection and rankings

- `stale_days` / `git_drift_threshold`: omitted = disabled; zero rejected. Init's
  visible config sets startup choices, not serde defaults that erase "disabled".
  `orphan_grace_days` is a duration and permits zero. Orphan also exempts terminal
  records, configured orphan-ok kinds and per-node orphan_ok.
- Terminal status narrows maintenance subjects, never evidence edges. Stale,
  drift, orphan and superseded-reference exempt retired subjects; backlinks,
  unresolved-reference and cycles still read their edges. A live record cited
  only by a terminal one remains referenced.
- Shared detectors (`find_stale_past`, `find_orphans`, `find_unresolved_edges`)
  supply both listings and rule reach. `find_stale` returns None for an undeclared
  horizon: stale query warns `threshold_undeclared`, report says untracked, and
  stale rule is absent. `IssueReport` counts each gated finding through its
  violation once; only unresolved warning fallthrough counts independently, and
  info rows stay outside total. Coverage assertion:
  `every_listed_finding_is_counted_once_through_its_rule`.
- Trust renormalizes over components the run can measure. Omitted stale horizon
  drops freshness. A positively weighted measurable axis missing document input
  makes score undefined (`TrustEntry::undeclared`, `RankingOutcome::unscored`);
  zero weight asks for no declaration. Terminal subjects have no review-anchored
  components. Similarity renormalizes over the target's available signals only;
  a candidate lacking a target signal scores zero, and a no-signal target ranks
  nothing. Source: `src/query/trust.rs`, `src/query/similar.rs`.
- `git_drift::drift_targets` supplies one per-edge resolution to rule, narrowing
  and trust. The rule requires reviewed before resolving; trust resolves first
  so no offered target is inapplicable rather than an undeclared date. Unresolvable
  targets are explicit (`GitDriftUnmeasurable`); `commits_since: None` is not zero.
  All offered edges unmeasurable makes the rule's node unjudged, with a warning
  naming targets; offering no edge is a judged answer. Trust drops unmeasurable
  drift rather than fabricating clean evidence.
- `DriftHistory` lazily shares one indexed revision walk across passes. It counts
  commits introducing path changes (`--full-history`, combined merge diffs), and
  applies review-date bounds to indexed dates, independent of time of day.
  history.json keys HEAD, binary and project prefix, extending reachable cached
  history only by new commits (`History::joined`). Shallow/graft/replace history
  bypasses persistence. Only working-tree/index check refreshes and reports cache
  IO; proposal gates and other readers consult without writing. Source:
  `src/rules/git_drift.rs` and `src/git.rs` rustdoc.

## References and cycles

- Body extraction (`parser::body`, pulldown-cmark destinations, wikilinks/custom
  patterns, optional whole-code-span citations) is shared with rewriting.
  `reference_path_candidates` is the one ladder: literal/relative path, configured
  extension candidates, bare id. `./` forces the document-relative frame.
  `resolver::frame_and_path` refuses root anchoring before collapsing empty/`.`
  segments; `..` remains an operation in its frame. `covers` is path-only;
  supersedes/superseded_by/implements/related are id-only, and custom patterns
  cannot manufacture those code-fixed relations. `resolver::sought_names` / typed
  `Sought` distinguish lookup names from raw authored targets and disk-probe paths.
- `body::Destination` pairs decoded path with source span: entities, escaping and
  fragments follow the parser's grammar, including nested links and escaped
  delimiters. Never resolve raw spelling or edit using decoded offsets.
- `reference_rewrite::apply_proposals` round-trips each proposal with its own
  `ReferenceForm` in the evolving document, preserving the frontmatter boundary.
  It then verifies all original references, including untouched ones. Dependent
  captures and overlapping edits cannot silently erase another edge: a reference
  lost by a trial names the culprit and rejects that rewrite; iterations are
  bounded by newly named references. A replacement may take only a reference it
  entirely encloses and removes in its own output. Partial overlap is severed
  (`Landing::Severed`) and refused, even if similar text appears elsewhere.
- `rewrite_for_move` handles changed target path and changed referring frame in
  one pass over authored content; two sequential repoint/rebase passes would
  read generated output as original input. Bind against graph nodes, not merely
  scanned filenames. `Rewritten::rebound` examines standing references in the
  finished document and names those now bound elsewhere; a valid graph alone
  cannot reveal this change. CLI warns once per reference.
- Destinations try original, escaped, then pointy encodings; forms try their
  original frame then an explicit frame (`./` for destinations, root naming for
  captures). An un-round-trippable proposal is left visible with an advisory;
  the write gate refuses when its resulting unresolved edge is configured Error.
  Exact grammar and retention rules: `src/reference_rewrite.rs` and
  `src/parser/body.rs` rustdoc.
- `CycleDetectionRule` reads strongly connected components for configured
  acyclic relations (non-empty known vocabulary; default implements).
  Supersedes has a separate hard build error (`validate_supersedes_dag`); covers
  cannot cycle. One node-less violation per document newly caught in a cycle,
  identity = `details.member`; `region` and `via` are grouping/cut evidence, not
  a route. Ring/region identity is traversal-dependent or turns repairs into
  introduced cycles. Source/property tests: `src/rules/graph_invariants.rs` and
  `ViolationDetails::Cycle` in `src/rules/detail.rs`.

## Snapshot and output contracts

`Graph` serialization reconstructs derived adjacency via `Graph::new`.
Provenance (`meta.nodex_version`, `config_hash`) and parse failures default during
old-file deserialization so compatibility fails via the schema guard. Bump
`SCHEMA_VERSION` in `src/model/graph.rs` on an on-disk shape change.

`src/status.rs` owns graph.json reads. `load_graph` advises on membership/config
drift; `load_current_graph` requires config, membership and exact content.
Missing = GRAPH_MISSING; drift = GRAPH_OUTDATED; failed read = its own IO_ERROR.
Neither rebuilds. Coverage is nodes ∪ parse failures; covered-but-unbuildable is
not stale. `compute_divergence` Membership does not read document bytes; Content
hashes them. `Snapshot::read_info` carries probe scope and coverage separately
from successful parsing. `Snapshot::require` escalates only a missed lookup to
Content before claiming NOT_FOUND; drift/read failure keeps its distinct remedy.
`Snapshot::body` owns revision-checked canonical reads; body-search limits bound
retained output, never the files whose revision must be checked.

Queries never mutate but may measure git/stat (`find_unresolved_edges`, trust).
Naming: `find_*` traversal/filter, `compute_*` values, `search` text scoring;
`*Filter` pure narrowing, `*Options` ranking/selection/output-retention controls.
Metadata listings return full core results for CLI presentation caps; body search
retains capped output in core (`BodySearchOptions`, `BodySearchResult`).
`*Outcome` carries in-process result + metadata, not a wire type; `*Result`,
`*Report`, `*Manifest`, `*Ref`, `*Entry` / `*Group` describe serialized roles.
Keep one concept stem across function/options/components/CLI args. Rule types end
in `Rule`; configured ids are `<family>/<name>`, built-ins keep the family name.
Payload authority is emitted types and `export::per_command_schemas`; output rules
live in `.claude/rules/json-output.md`. New rule procedure:
`.claude/rules/adding-a-validation-rule.md`.
