# nodex — `nodex.toml` reference

Read this when authoring or debugging a config. `nodex init` writes an annotated starter; `reference/minimal-config.toml` beside this file is a worked minimal example. `nodex export config` and `nodex export rules` show what the project actually resolved to.

## Load-time rejections

Each of these is a real `CONFIG_ERROR` at load, not a silent no-op:

- `schema.types` values are `string | integer | bool | date` only, and collection fields (`tags`, `related`, …) take **no** type entry.
- `schema.required` takes authored fields only — `id` / `title` / `kind` / `status` / `orphan_ok` are parser-resolved and refused.
- `schema.enums` values are string arrays; a bare TOML integer is rejected. Quote numeric vocabulary: `["1", "2"]`.
- `default_limit` sits under `[similarity]`, not `[similarity.weights]`.
- `parser.extensions` entries carry the leading dot.
- `[[annotations]]` patterns need a named capture matching `key`.
- Narrowing `statuses.allowed` means declaring `statuses.terminal` too — every terminal status must stay allowed.
- A `kinds` entry on any per-block rule must be in `kinds.allowed`, so a typo can never become a silent never-fire.

With `parser.wikilink_enabled = true`, a `[[...]]`-shaped annotation marker is **also** parsed as a wikilink and surfaces as an unresolved edge in `query issues`. Use a non-bracket marker syntax if you want annotations only.

## Scope

`scope.include` / `scope.exclude` are glob lists. `scope.prune_dirs` names directory basenames pruned at any depth (default `["node_modules", "__pycache__", "target", ".git", ".venv"]`; an empty list prunes nothing).

Dot-prefixed paths (`.draft.md`, `.archive/`, `.claude/`) are skipped unless an include pattern **requires** the dot at that position. `.claude/**/*.md` opts `.claude` in, as do the spellings globset treats as the same literal (`\.claude`, `[.]claude`) and a pattern that can only match a hidden entry (`.*/**/*.md`). A wildcard that merely *matches* one (`**/*.md`, `?claude/**`) does not. Dot-prefixed trees stay caught by the hidden-path guard regardless of `prune_dirs`.

A directory reached through a **symlink** is not descended unless `scope.follow_symlinks = true`. The default matches `git` / `ripgrep` / `fd` / `find` and keeps the path space a tree, so every path-keyed rule (`include`, `exclude`, a `conditional_exclude` `parent_glob`, an `identity` glob) has exactly one path to key on. Each undescended link is named in the build's `unfollowed_paths`.

Turn it on for a project whose documents live behind a link — a vendored tree linked into `docs/`. The scan then admits every name a document is reachable under, keeps one document per directory entry, and reports it under the smallest admitted name, at a traversal cost that grows with nested links. Each unused name appears in `aliased_paths` paired with the name in use, and a write seam naming an unused one is refused with the path to use instead. A symlink to a *file* is read wherever it points either way.

`[[scope.conditional_exclude]]` drops a terminal parent's sub-artifacts: `parent_glob` selects the parent, `child_glob` selects which siblings are derivative, `condition = "status_terminal"`. Only `child_glob` matches are excluded. The dropped paths are reported on the build result, and both the write that makes the parent terminal and the `check --content` gate that previews it name them as `document_evicted`.

## Body links

Standard markdown (`[text](path.md)`) by default. Wikilinks (`[[id]]`) opt in via `parser.wikilink_enabled = true`.

`parser.link_patterns` declares arbitrary syntaxes. Each block needs a `pattern` with **exactly one** capture group and a `relation`. Any relation name is legal except the built-ins with code-fixed resolution, which are rejected at load: `covers` (path-only) and `supersedes` / `implements` / `related` (id-resolved) are declared through their frontmatter fields only. `references` stays legal.

A block may set `code_spans = true`: an inline code span whose **entire content** the pattern matches is then a citation on both the extraction and the rewriting side, so a corpus writing ids as `` `adr-001` `` is reachable as edges and `retarget` repoints them. A span is matched as its own text, so `^` / `$` mean the span and the backticks are never part of what the pattern sees; what the match leaves over is what keeps a span code (`` `just adr-tool` `` cites nothing). Code *blocks* stay opaque unconditionally. The field defaults off.

Path resolution: a link opening `./` is read from the directory its document is in, never from the project root — the marker names the frame. Segments that name nothing are dropped before any rung is tried, so `docs//x.md` and `docs/./x.md` are both `docs/x.md`, while `.//x.md` still says its frame. `..` is kept and resolved by the frame that read it. A leading `/` is refused with `cause: absolute`.

## Schema rules

`[schema].mode = "strict"` rejects any frontmatter key that is neither built-in nor declared in `types` / `enums` / `required` / `cross_field` — this is what catches `relatd:`. Default is `lenient`.

`[[schema.cross_field]]` predicates take four forms:

| `when` | meaning |
|---|---|
| `"field=value"` | equality |
| `"field in {v1,v2,v3}"` | membership |
| `"field exists"` | presence |
| `"field not_exists"` | absence |

Scalar predicates (`=`, `in`) are rejected on collection fields (`tags`, `covers`, …) at load; use `exists` / `not_exists` for collection presence.

`schema.require_explicit` names inferrable built-ins (`id` / `title` / `kind` / `status`) a document must author rather than inherit from a fallback. An inferred or empty named field reds `check` via `explicit_field`. This is what makes a `check --content` verdict certify those keys are spelled out.

`schema.overrides` applies per-kind required fields, type changes, enum changes and cross-field checks.

## Built-in rule ids

`parse_failure` (node-less, one per dropped in-scope document) · `field_parse` (one per wrong-typed built-in field on a present node) · `required_field` · `field_type` · `field_enum` · `cross_field` · `unknown_field` (strict mode only) · `explicit_field` (only when `schema.require_explicit` is set) · `stale_review` · `git_drift` · `filename_pattern` · `sequential_numbering` · `unique_numbering` · `acyclic_relation` (always on; the relation set is config-driven via `rules.acyclic_relations`, default `["implements"]`).

Config-driven ids: `body_line/<name>` · `body_immutable/<name>` · `frontmatter_immutable/<name>` · `unresolved_reference/<name>`.

## Vocabulary rules

`[[rules.body_line]]` — per-line vocabulary conformance. Each block declares a regex with named captures; every match outside a code block must carry capture values from declared enums. One violation per failed (line, capture). Lines that do not match the pattern are ignored.

`[[rules.naming]]` — filename patterns, path-scoped: it carries `glob`, not `kinds`.

The content-scoped per-block families — `body_immutable`, `frontmatter_immutable`, `body_line` — and `[[annotations]]` accept an optional `kinds` list. Empty means no restriction; otherwise the rule fires only on nodes whose `kind` appears in it.

## Diff-aware rules

`frontmatter_immutable` and `body_immutable` need a before-state. They get it from `--since <ref>` or from `rules.immutable_baseline`. Without either they self-report in `skipped_rules` with a reason — silent non-fires are forbidden.

```toml
[rules]
immutable_baseline = "origin/main"
```

That is the default ref `check` diffs against when `--since` is omitted, so the locks are enforced on a plain `nodex check`. Unlike `--since` it never narrows the reported violations to changed nodes — it only supplies the before-state.

When the baseline cannot engage — the project is not in a git work tree, or the ref carries nothing for the project — the run proceeds with a `baseline_inert` warning and the rules land in `skipped_rules`. The same advisory rides every mutating command, so a write whose locks were never enforced never reads as clean. A ref git cannot resolve at all is refused outright with `CONFIG_ERROR` by reads and writes alike, `check --content` included, so an unreadable baseline cannot let the pre-write gate clear an edit the write would refuse. A repository with no commits yet is inert instead, so a project can be scaffolded before its first commit.

Watch for this after upgrading: `"origin/main"` in a shallow checkout that lacks the ref now fails every baseline-resolving command.

```toml
[[rules.frontmatter_immutable]]
name = "identity"
fields = ["kind", "superseded_by"]
# kinds = ["adr"]        # optional; empty = every kind
```

Freezes declared fields once a doc is **already** terminal — gated on the diff's *before* status, so the write that first makes a doc terminal is allowed and only later edits lock. `id` is rejected at load (a changed id is a different node); `status` is accepted and enforced via the status-transition stream. Names must be unique across blocks.

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

`trigger = "terminal"` (default) uses the same already-terminal boundary as `frontmatter_immutable`. `trigger = "creation"` freezes the body as soon as a prior committed snapshot exists — the creating commit is structurally exempt, and frontmatter including `status` stays editable for supersession. Driven by per-node body fingerprints computed at build time, so no file is re-read at check time.

### Locks are identity-scoped

The baseline is paired with the working tree **by node id**, so a lock guards a body for as long as the document keeps its id, and `check` and the write seams agree because they pair the same way. A document with a new id has no baseline to compare against on either plane. What preserves a lock across a move is preserving the id:

- an explicit `id:` in frontmatter survives any move (`id_stability: already_anchored`);
- an id derived from `identity.id_rules` survives `nodex rename`, which writes the derived id in explicitly before moving the file (`anchored`) — but not `mv` / `git mv`, which change the path and therefore the id;
- a **bare-markdown** document cannot be anchored — `rename` will not invent a frontmatter block for a path operation — so its id does change and `id_stability: bare_no_frontmatter` says so.

So: move a locked document with `nodex rename`, and give it an id that is not derived from its path.

## Unresolved-reference policy

`[[detection.unresolved_policy]]` is an ordered, first-match-wins table of `(cause, glob?) → severity` rows classifying unresolved references.

- `error` registers a check rule `unresolved_reference/<name>`; matching edges fail `nodex check` and count as `violation_unresolved_reference/<name>`.
- `warning` edges count in `summary.total` under `unresolved_edge`.
- `info` edges are reported out of `total` under their row's name.

Row globs match the link's normalized root-relative resolution candidates, never the raw authored target. Declaring the table **replaces** the built-in default row `{name = "excluded_target", cause = "excluded_from_scope", severity = "info"}` — re-declare it to keep it.

## Detection thresholds

`detection.stale_days` and `detection.git_drift_threshold` are omitted to disable; `0` is rejected at load as ambiguous. Omitting `stale_days` also drops the trust composite's `freshness` component — freshness is measured against that horizon, and a project declaring no horizon has no scale to place a date on.

`detection.git_drift_relations` is validated at load — non-empty, no duplicates, every entry a known relation — whether or not the threshold is set. `detection.orphan_grace_days` is a plain duration, so `0` is valid and means "check immediately". `detection.orphan_ok_kinds` names kinds that are leaf-by-design.

## Git measurement

Every git-backed feature — `rules.immutable_baseline`, `git_drift`, `diff`, `impact` — measures the project **at its own location** inside the repository that tracks it. A `nodex.toml` in a subdirectory of a larger repository is measured as itself, not as the repository around it, and no inherited git environment variable (`GIT_DIR`, `GIT_WORK_TREE`, a server-side hook's quarantine object directory, pathspec magic) can redirect it.
