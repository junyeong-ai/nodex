//! Scaffold new document nodes.
//!
//! Creates a valid frontmatter + body skeleton obeying the project's
//! config (kind inference, id rules, required fields, enum defaults,
//! cross-field constraints). AI agents use this to avoid frontmatter
//! typos and missing-field errors when creating new documents — and,
//! via [`ScaffoldSpec::body`] / [`ScaffoldSpec::fields`], to land real
//! content through the one guarded creation seam instead of hand-
//! writing files around it.
//!
//! Every decision prefers config over heuristic; heuristics only kick
//! in when config is silent. Callers can override any inferred value
//! by supplying it explicitly on [`ScaffoldSpec`].
//!
//! Validation rides the same full-graph overlay substrate as
//! `check --content <path>=<source>`: the before-graph is built live from the
//! working tree (no `graph.json` snapshot, no prior `nodex build`),
//! the composed document is overlaid, and the proposal answers for
//! exactly the Error-severity violations it *introduces* (exact
//! [`Violation`](crate::rules::Violation) equality against the before
//! report) — when the caller supplied content. Config-derived default
//! scaffolds keep their write-and-advise behaviour: the same findings
//! ride the envelope as warnings, so a fill-me-in placeholder stays
//! scaffoldable.

use chrono::NaiveDate;
use globset::Glob;
use regex::Regex;
use schemars::JsonSchema;
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::config::{Config, FieldType, parse_when};
use crate::error::{Error, Result};
use crate::model::{Graph, Kind};
use crate::parser::identity::infer_id;
use crate::warning::{Warning, WarningCode};

/// User-supplied scaffold parameters. All override fields are optional;
/// [`scaffold`] fills in the rest from config.
#[derive(Debug, Clone)]
pub struct ScaffoldSpec {
    pub kind: Kind,
    pub title: String,
    /// Overrides automatic id inference when `Some`.
    pub id: Option<String>,
    /// Overrides automatic path inference when `Some`. Relative to root.
    pub path: Option<PathBuf>,
    /// Caller-supplied markdown body. `None` renders the default `# title`
    /// skeleton.
    pub body: Option<String>,
    /// Caller-supplied frontmatter fields as `(key, YAML value)` pairs,
    /// rendered right after the four identity lines — they enter the
    /// cross_field reparse-fixpoint, so a `when` keyed on a supplied value
    /// fires by construction. A key whose value has a canonical source — a
    /// dedicated spec field, config derivation, or the structural
    /// filesystem path — is refused (`validate_field_keys` names the exact
    /// set), as are duplicate keys.
    pub fields: Vec<(String, String)>,
}

/// Outcome of a scaffold request. When `write = false` (dry-run),
/// `written` is `false` and the file is untouched.
///
/// Advisory notes the caller might want to surface live in the
/// separate `Vec<String>` returned alongside this struct, never on the
/// struct itself — the JSON envelope contract puts `warnings` at the
/// envelope level, not inside `data`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ScaffoldResult {
    pub id: String,
    #[serde(serialize_with = "crate::model::node::serialize_path_forward")]
    pub path: PathBuf,
    pub content: String,
    pub written: bool,
}

/// `--field` keys with a canonical source that `fields` must not shadow.
/// `status` derives from `statuses.initial` and changes via the lifecycle
/// seam with its terminal/vocabulary/cross_field guards; `id` / `title` /
/// `kind` have dedicated spec fields — accepting any of them through
/// `fields` would create a second, weaker path to the same values. `path`
/// is refused too, but as a [`crate::config::is_reserved_structural_field`]
/// (the structural filesystem path, set via the path spec field, never
/// frontmatter), so `validate_field_keys` composes that check rather than
/// listing `path` here.
const RESERVED_FIELD_KEYS: &[&str] = &["id", "title", "kind", "status"];

/// Scaffold a new node.
///
/// When `write` is `true`, the file is written atomically (temp file +
/// rename) and `ScaffoldResult::written` is set. Existing files are
/// rejected unless `force` is set; a `--force` overwrite (or the
/// re-creation of a document deleted since `rules.immutable_baseline`)
/// additionally consults the immutability lock through `probe` and is
/// refused when the project's own `check` would flag the rewrite.
///
/// Returns `(result, warnings)` so the caller can surface warnings at
/// the JSON-envelope level (`json-output.md`'s `{ ok, data, warnings }`
/// contract) without fishing them out of `data`.
pub fn scaffold(
    root: &Path,
    spec: ScaffoldSpec,
    config: &Config,
    probe: &crate::mutate::BaselineProbe,
    write: bool,
    force: bool,
    today: NaiveDate,
) -> Result<(ScaffoldResult, Vec<Warning>)> {
    // 1. Validate kind against config, and the supplied fields' keys.
    if !config
        .kinds
        .allowed
        .contains(&spec.kind.as_str().to_string())
    {
        return Err(Error::Config(format!(
            "unknown kind {:?}; allowed: {:?}",
            spec.kind.as_str(),
            config.kinds.allowed
        )));
    }
    validate_field_keys(&spec.fields)?;

    // 2. Build the before-graph live from the working tree (read-only,
    //    cache untouched) — the one graph that drives sequence
    //    numbering, collision detection, similarity, and the validation
    //    delta below. A project whose graph cannot build (e.g. a
    //    pre-existing duplicate id) blocks scaffold with the build's
    //    typed error: a new document cannot be validated against a
    //    graph that does not exist.
    let before = crate::builder::build_with_overlay(root, config, &[])?;

    // 3. Resolve path (explicit override or infer from kind_rules).
    let rel_path = match spec.path.clone() {
        Some(p) => p,
        None => infer_path(&spec.kind, &spec.title, &before.graph, config)?,
    };

    // Scaffold targets must wear one of the project's document
    // extensions; otherwise nothing downstream (parser, link
    // extraction, scanner globs) would treat the file as in-graph.
    let target_ext = rel_path
        .extension()
        .and_then(|s| s.to_str())
        .map(|e| format!(".{e}"));
    if !target_ext.as_deref().is_some_and(|ext| {
        config
            .parser
            .extensions
            .iter()
            .any(|allowed| allowed == ext)
    }) {
        return Err(Error::Config(format!(
            "scaffold target must end with one of {:?}; got {}",
            config.parser.extensions,
            rel_path.display()
        )));
    }

    // The one canonical normalization every user-supplied document path
    // gets (symmetric with `check --content` and `rename`): fold `\` to
    // `/`, refuse traversal / absolute forms, collapse `.` segments —
    // so `./docs/a.md`, `docs\a.md`, and `docs/a.md` all name the same
    // document, id inference and the admission probe key on the
    // scanner's root-relative form, and the write lands exactly where
    // the envelope says it did.
    let rel_path = PathBuf::from(crate::path_guard::normalize_doc_path(
        &rel_path.to_string_lossy(),
    )?);

    let abs_path = root.join(&rel_path);

    // 4. Resolve id (explicit override or infer via existing identity
    //    rules). An explicit id must be reference-safe — inferred ids
    //    are slugs by construction and need no check.
    if let Some(explicit) = &spec.id {
        crate::model::validate_explicit_id(explicit)?;
    }
    let id = spec
        .id
        .clone()
        .unwrap_or_else(|| infer_id(&rel_path, &spec.kind, &config.identity));
    detect_id_collision(&id, &rel_path, &before.graph)?;

    // 5. Reject existing file unless --force.
    if abs_path.exists() && !force {
        return Err(Error::Exists(abs_path));
    }

    // 5.1 Build frontmatter YAML and body.
    let content = render_document(&id, &spec, &rel_path, config, today);

    // 5.5 Refuse a target the scan would never admit — outside
    // scope.include, inside scope.exclude, or dropped by a
    // conditional_exclude: the file would be written and then silently
    // ignored by every subsequent build, a document the graph can never
    // see. The probe overlays the exact bytes the write would produce
    // through the same scope authority the build uses, so the verdict
    // here and the post-write scan's cannot disagree — even for a
    // target that is itself a conditional-exclude parent whose own
    // status participates in the evaluation.
    let scan = crate::builder::scanner::scan_scope_with_overlay(
        root,
        config,
        &[(
            rel_path.clone(),
            crate::builder::scanner::Proposed::Content(content.clone()),
        )],
    )?;
    if !scan.paths.contains(&rel_path) {
        let cause = if scan.conditionally_excluded.contains(&rel_path) {
            "a [[scope.conditional_exclude]] rule drops it there (a terminal parent's \
             sub-artifact); change the parent's status or the rule"
        } else {
            "it is outside scope.include / inside scope.exclude; adjust the path or the scope \
             config in nodex.toml"
        };
        return Err(Error::Config(format!(
            "scaffold target {:?} would never be graphed — {cause}",
            crate::path_guard::forward_string(&rel_path)
        )));
    }

    // 5.6 Refuse a filename the project's `rules.naming` reject. scaffold
    // derives the name from the title (a sequential rule auto-numbers it;
    // see `next_filename_stem`), but an arbitrary naming pattern the slug
    // can't satisfy would be written and then flagged by the project's
    // own `filename_pattern` check — the self-consistency invariant. The
    // same predicate the rule uses decides here, so they cannot disagree;
    // the caller supplies `--path` with a conforming name instead.
    if let Some(rule) = crate::rules::naming::first_filename_violation(config, &rel_path) {
        return Err(Error::Config(format!(
            "scaffold target {:?} violates rules.naming pattern {:?} (glob {:?}); pass --path \
             with a conforming filename, or set sequential = true / adjust the naming rule",
            crate::path_guard::forward_string(&rel_path),
            rule.pattern,
            rule.glob,
        )));
    }

    // 5.7 Immutability lock. A creation reaches the baseline two ways and
    // each is a different question, answered by whoever can answer it.
    //
    // The id it claims: whether writing this document leaves a record the
    // baseline froze in a state its own rules reject. That is what the rules
    // judge, so they are asked — over the composed document at the path it
    // will occupy.
    //
    // The path it lands on: whether a frozen record stands there at all. No
    // rule can answer it, because replacing a record with a *different* one is
    // a removal plus an addition to `check` and nothing consumes either. So
    // the baseline is asked directly. With nothing bound neither engages.
    let plan = crate::mutate::Planned {
        rel_path: rel_path.clone(),
        content: content.clone(),
    };
    let refusals = probe.refusals(root, config, &[plan.proposed()], today)?;
    if let Some((path, lock)) = refusals.destroyed() {
        return Err(Error::Config(format!(
            "scaffold cannot complete: the project this write produces no longer holds the \
             baseline record at {:?}, and it is frozen ({lock}). Restore that record before \
             writing here",
            crate::path_guard::forward_string(path)
        )));
    }
    let lock = refusals
        .refusing(&rel_path)
        .map(str::to_owned)
        .or_else(|| probe.frozen_at(&rel_path, config));
    if let Some(lock) = lock {
        // The lock reads as a trailing clause, as it does at the lifecycle
        // seam: it is usually a rule id, but it can also name a lock that
        // could not be evaluated, and mid-sentence that implies a rule by
        // that name exists.
        return Err(Error::Config(format!(
            "scaffold target {:?} cannot be rewritten at rules.immutable_baseline; \
             supersede the record instead — {lock}",
            crate::path_guard::forward_string(&rel_path)
        )));
    }

    // 6. Validate through the `check --content` substrate: the
    // after-graph overlays the composed document onto the working tree
    // and both reports run the full rule set, so the proposal answers
    // for exactly the violations it introduces — the shared count-aware
    // multiset delta (`rules::introduced_violations`), never message
    // sniffing — and a pre-existing project violation never blocks an
    // unrelated scaffold. Structural breakage (a duplicate id elsewhere
    // on disk, a supersedes cycle from a supplied relation) refuses
    // here too: the overlay build itself errors.
    let after = crate::builder::build_with_overlay(
        root,
        config,
        &[(
            rel_path.clone(),
            crate::builder::scanner::Proposed::Content(content.clone()),
        )],
    )?;
    let diff = crate::diff::compute_diff(&before.graph, &after.graph);
    let baseline_violations =
        crate::rules::check(&before.graph, config, root, None, today).violations;
    let introduced: Vec<crate::rules::Violation> = crate::rules::introduced_violations(
        crate::rules::check(&after.graph, config, root, Some(&diff), today).violations,
        &baseline_violations,
    );

    // Strategy 3 (the lifecycle write-seam precedent) when the caller
    // supplied real content: an introduced Error-severity violation
    // refuses the scaffold — every finding is satisfiable via `--field`
    // / a corrected body. Strategy 2 otherwise: config-derived defaults
    // stay scaffoldable and the same findings ride the envelope as
    // fill-me-in advisories.
    if spec.body.is_some() || !spec.fields.is_empty() {
        let findings: Vec<String> = introduced
            .iter()
            .filter(|v| v.severity == crate::rules::Severity::Error)
            .map(|v| format!("{}: {}", v.rule_id, v.message))
            .collect();
        if !findings.is_empty() {
            return Err(Error::ContentViolations { findings });
        }
    }

    // 6.1 Advisories: a near-duplicate existing doc, then whatever the
    // overlay check surfaced that did not refuse.
    let mut warnings = Vec::new();
    if let Some(similar) = similar_doc_warning(&spec, &rel_path, &before.graph, config) {
        warnings.push(Warning::new(WarningCode::SimilarDocument, similar));
    }
    warnings.extend(introduced.iter().map(|v| {
        Warning::new(
            WarningCode::BuildRecommended,
            format!("{}: {}", v.rule_id, v.message),
        )
    }));

    // 7. Write atomically (or skip in dry-run).
    let written = if write {
        crate::path_guard::write_atomic_in_root(root, &abs_path, &content)?;
        warnings.push(Warning::new(
            WarningCode::BuildRecommended,
            "run `nodex build` to include this document in the graph",
        ));
        true
    } else {
        false
    };

    Ok((
        ScaffoldResult {
            id,
            path: rel_path,
            content,
            written,
        },
        warnings,
    ))
}

/// Refuse reserved and duplicate supplied-field keys before anything is
/// built or rendered. The reserved set is computed from code
/// ([`RESERVED_FIELD_KEYS`] ∪ the structural fields) so the error names
/// the exact enforced set — no doc has to restate (and risk drifting
/// from) the list.
fn validate_field_keys(fields: &[(String, String)]) -> Result<()> {
    let reserved: Vec<&str> = RESERVED_FIELD_KEYS
        .iter()
        .copied()
        .chain(crate::config::RESERVED_STRUCTURAL_FIELDS.iter().copied())
        .collect();
    let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for (key, _) in fields {
        if reserved.contains(&key.as_str()) {
            return Err(Error::Config(format!(
                "field {key:?} is reserved (the reserved keys are {reserved:?}): each has a \
                 canonical source — a dedicated flag, config derivation (`status` via \
                 `statuses.initial`, change it through `lifecycle set`), or the structural \
                 filesystem path (`path`) — so it cannot be set via --field"
            )));
        }
        if !seen.insert(key.as_str()) {
            return Err(Error::Config(format!(
                "field {key:?} is supplied more than once; frontmatter keys are unique"
            )));
        }
    }
    Ok(())
}

// ─── path inference ─────────────────────────────────────────────────

fn infer_path(kind: &Kind, title: &str, graph: &Graph, config: &Config) -> Result<PathBuf> {
    // Find the first kind_rule that produces this kind.
    let Some(rule) = config
        .identity
        .kind_rules
        .iter()
        .find(|r| r.kind == kind.as_str())
    else {
        return Err(Error::Config(format!(
            "cannot infer path for kind {:?}: no identity.kind_rules match; \
             supply `--path` explicitly",
            kind.as_str()
        )));
    };

    let dir = directory_from_glob(&rule.glob).ok_or_else(|| {
        Error::Config(format!(
            "kind_rule glob {:?} does not yield a concrete directory; \
             supply `--path` explicitly",
            rule.glob
        ))
    })?;

    let stem = next_filename_stem(&dir, title, graph, config)?;
    Ok(dir.join(format!("{stem}.md")))
}

/// Reduce a glob to its leading literal directory. `docs/decisions/**`
/// → `docs/decisions`. Returns `None` when the glob lacks a literal prefix.
fn directory_from_glob(glob: &str) -> Option<PathBuf> {
    let mut parts = Vec::new();
    for segment in glob.split('/') {
        if segment.contains('*') || segment.contains('?') || segment.contains('[') {
            break;
        }
        parts.push(segment);
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.iter().collect())
}

/// Build the filename stem. When a naming rule has `sequential = true`
/// and matches the target directory via directory-prefix containment,
/// use `NNNN-<slug>` with the next available number; otherwise plain
/// `<slug>`.
fn next_filename_stem(dir: &Path, title: &str, graph: &Graph, config: &Config) -> Result<String> {
    let slug = crate::parser::identity::slugify(title);
    let dir_str = crate::path_guard::forward_string(dir);

    for rule in &config.rules.naming {
        if !rule.sequential {
            continue;
        }
        let matcher = Glob::new(&rule.glob)
            .expect("validated by Config::load")
            .compile_matcher();

        // The rule's literal prefix (segments before the first
        // wildcard) must equal the scaffolded directory.
        if !rule_targets_directory(&rule.glob, &dir_str) {
            continue;
        }
        let (next, width) = next_sequence(graph, &matcher, &rule.pattern)?;
        let padded = format!("{:0>width$}", next, width = width);
        return Ok(format!("{padded}-{slug}"));
    }

    Ok(slug)
}

/// Does `glob` apply to files under `dir`? The glob's literal prefix
/// (every segment before the first wildcard) must equal `dir`.
///
/// `directory_from_glob` already computes that prefix — delegating to
/// it keeps the "literal prefix equality" contract documented at one
/// place and dodges a class of broken glob-synthesis edge cases
/// (`*.md`, `?*`, `[0-9]*.md`, middle-path wildcards) that the earlier
/// synthesis approach silently mis-matched.
///
/// Examples (all verified in tests):
///   glob = "docs/decisions/**",        dir = "docs/decisions"       → true
///   glob = "docs/decisions/*.md",      dir = "docs/decisions"       → true
///   glob = "docs/decisions/[0-9]*.md", dir = "docs/decisions"       → true
///   glob = "docs/*/decisions/**",      dir = "docs"                 → true
///   glob = "docs/guides/**",           dir = "docs/decisions"       → false
fn rule_targets_directory(glob: &str, dir: &str) -> bool {
    let Some(prefix) = directory_from_glob(glob) else {
        return false;
    };
    crate::path_guard::forward_string(&prefix) == dir
}

/// Find the next sequence number for files matching `matcher`, preserving
/// the digit width of existing filenames. Errors when the `u64` sequence
/// space is exhausted (a matching file is already numbered `u64::MAX`):
/// saturating would re-emit that number, writing a document that reds the
/// project's own `UniqueNumberingRule` — a violation of the
/// tool-written-must-pass invariant — so the only correct outcome is to
/// refuse with a clear message.
fn next_sequence(
    graph: &Graph,
    matcher: &globset::GlobMatcher,
    pattern: &str,
) -> Result<(u64, usize)> {
    let digits_re = crate::rules::naming::leading_digits_re();
    let pattern_re =
        Regex::new(pattern).expect("naming pattern is validated as a regex by Config::load");
    let mut max_seen: u64 = 0;
    let mut width: usize = 4; // sensible default for ADR-style numbering

    for node in graph.nodes().values() {
        let path_str = crate::path_guard::forward_string(&node.path);
        if !matcher.is_match(&path_str) {
            continue;
        }
        // The numbering check rules read the sequence number through this
        // exact helper, so the number `scaffold` writes is the number `check`
        // validates — one definition of "what is the number".
        if let Some((n, w)) =
            crate::rules::naming::numbering_sequence(&node.path, &pattern_re, &digits_re)
        {
            max_seen = max_seen.max(n);
            width = width.max(w);
        }
    }

    let next = max_seen.checked_add(1).ok_or_else(|| {
        Error::Config(format!(
            "sequence numbering is exhausted — a matching document is already numbered \
             {max_seen} (u64::MAX), so no next number exists. Renumber that file, or \
             scaffold with an explicit --path"
        ))
    })?;
    Ok((next, width))
}

// ─── frontmatter rendering ──────────────────────────────────────────

/// Render a YAML frontmatter body (without `---` delimiters) that
/// satisfies every `required` + `cross_field` rule the project has
/// declared for `kind`. Shared between `scaffold` (creating a new
/// file, where `fields` carries the caller's supplied pairs) and
/// `migrate` (injecting frontmatter into a bare file, `fields = &[]`)
/// so both paths produce documents that pass `check` immediately — the
/// self-consistency invariant codified in
/// `.claude/rules/config-driven.md`.
///
/// Supplied `fields` render right after the four identity lines and
/// enter `seen` before the defaults and the cross_field fixpoint, so a
/// supplied value preempts its default and a `when` keyed on it fires
/// by reparse, never by stand-in.
pub fn render_default_frontmatter(
    id: &str,
    title: &str,
    kind: &str,
    fields: &[(String, String)],
    config: &Config,
    today: NaiveDate,
) -> String {
    let required: Vec<String> = config.required_for(kind).to_vec();
    let today_field = today.to_string();

    let mut lines: Vec<String> = Vec::new();

    lines.push(format!("id: {}", crate::yaml_text::quote(id)));
    lines.push(format!("title: {}", crate::yaml_text::quote(title)));
    lines.push(format!("kind: {}", crate::yaml_text::quote(kind)));
    lines.push(format!(
        "status: {}",
        crate::yaml_text::quote(config.initial_status())
    ));

    let mut seen: std::collections::BTreeSet<String> = ["id", "title", "kind", "status"]
        .into_iter()
        .map(String::from)
        .collect();
    for (key, value) in fields {
        lines.push(format!("{key}: {value}"));
        seen.insert(key.clone());
    }
    for field in &required {
        if seen.contains(field) {
            continue;
        }
        lines.push(format!(
            "{field}: {}",
            default_for_field(field, kind, config, &today_field)
        ));
        seen.insert(field.clone());
    }

    // Honour cross_field (global + per-kind) with the reparse-the-real-
    // node discipline the lifecycle write-seam uses: evaluate every
    // predicate against a node parsed from the frontmatter *as written
    // so far*, never a synthetic stand-in — so scaffold and `check`
    // agree by construction about which predicates fire (a `when` keyed
    // on a required/enum field scaffold itself just defaulted, or on a
    // caller-supplied value, must see that value). Iterate to a
    // fixpoint: one emitted `require` field can itself satisfy or
    // trigger another `when`. Bounded by the cross_field count — each
    // round emits only fields not yet `seen` and never removes one.
    loop {
        let snapshot = format!("---\n{}\n---\n", lines.join("\n"));
        let Ok((node, _)) =
            crate::parser::frontmatter::parse_frontmatter(Path::new("scaffold"), &snapshot)
        else {
            break;
        };
        let mut emitted = false;
        for cf in config.cross_field_for(kind) {
            if seen.contains(&cf.require) {
                continue;
            }
            // `Config::load` parses every `cross_field.when`
            // (`validate_cross_field_syntax`), so the predicate always
            // parses here.
            let predicate = parse_when(&cf.when).expect("validated by Config::load");
            if !crate::rules::schema::predicate_matches_node(&predicate, &node)
                || !crate::rules::schema::is_field_missing(&node, &cf.require)
            {
                continue;
            }
            lines.push(format!(
                "{}: {}",
                cf.require,
                default_for_field(&cf.require, kind, config, &today_field)
            ));
            seen.insert(cf.require.clone());
            emitted = true;
        }
        if !emitted {
            break;
        }
    }

    lines.join("\n")
}

fn render_document(
    id: &str,
    spec: &ScaffoldSpec,
    path: &Path,
    config: &Config,
    today: NaiveDate,
) -> String {
    let frontmatter = render_default_frontmatter(
        id,
        &spec.title,
        spec.kind.as_str(),
        &spec.fields,
        config,
        today,
    );

    if let Some(body) = &spec.body {
        // The supplied bytes ARE the body — composed verbatim after the
        // close fence, canonicalized (BOM strip, CRLF → LF) exactly as
        // every parser entry would, so the validated bytes and the
        // written bytes are identical.
        let composed = format!("---\n{frontmatter}\n---\n{body}");
        return crate::parser::frontmatter::canonicalize(&composed).into_owned();
    }

    let stem_title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Document");
    // The body heading is plain markdown — control characters would
    // break the H1 line (a newline splits the heading into unrelated
    // prose). Collapse every control character to a space so the
    // visible title in the rendered markdown matches the frontmatter.
    let body_heading = if spec.title.is_empty() {
        stem_title.to_string()
    } else {
        spec.title
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect::<String>()
    };

    format!("---\n{frontmatter}\n---\n\n# {body_heading}\n")
}

fn default_for_field(field: &str, kind: &str, config: &Config, today: &str) -> String {
    // Use the merged (global + override) views so a project declaring
    // `types` / `enums` at the top-level `[schema]` — with no per-kind
    // override — still gets a type-/enum-valid default here. Reading
    // only from `schema_override_for(kind)` missed that case and let
    // scaffold write `priority: ""` against a global
    // `types = { priority = "integer" }`, which immediately failed
    // `FieldTypeRule`. `enums_for` and `types_for` are the same views
    // the rules themselves consume, so scaffold's defaults and
    // check's expectations cannot drift.
    let enums = config.enums_for(kind);
    if let Some(allowed) = enums.get(field)
        && let Some(first) = allowed.first()
    {
        return first.clone();
    }

    let types = config.types_for(kind);
    if let Some(ty) = types.get(field) {
        return match ty {
            FieldType::Date => today.to_string(),
            FieldType::Integer => "0".to_string(),
            FieldType::Bool => "false".to_string(),
            FieldType::String => "\"\"".to_string(),
        };
    }

    // Built-in field conventions
    match field {
        "created" | "updated" | "reviewed" => today.to_string(),
        "owner" | "superseded_by" => "\"\"".to_string(),
        // Every collection-valued built-in defaults to an empty list —
        // `covers` included, so a (required) `covers` never falls through
        // to the `""` arm and emits a `covers` edge with an empty target.
        "supersedes" | "implements" | "related" | "tags" | "covers" => "[]".to_string(),
        _ => "\"\"".to_string(),
    }
}

// ─── collision detection ────────────────────────────────────────────

/// Reject the scaffold if `id` already belongs to another document in
/// the live before-graph. The graph is built from the working tree at
/// call time, so there is no stale-snapshot window; a collision the
/// graph cannot see (the other file fails to parse and has no node)
/// surfaces as `DUPLICATE_ID` on the build that follows fixing that
/// file.
fn detect_id_collision(id: &str, rel_path: &Path, graph: &Graph) -> Result<()> {
    if let Some(existing) = graph.nodes().get(id) {
        // If the graph already indexes this id at the scaffold target
        // itself, it is not a collision — the caller's `--force` flag
        // decides whether to overwrite. The later `abs_path.exists()`
        // check gates that.
        if existing.path != rel_path {
            return Err(Error::DuplicateId {
                id: id.to_string(),
                first: existing.path.clone(),
                second: rel_path.to_path_buf(),
            });
        }
    }
    Ok(())
}

// ─── advisories ─────────────────────────────────────────────────────

/// Duplicate detection — vector-free similarity against the live
/// graph. Surfaces the top match with its score so the agent can
/// decide whether `lifecycle supersede` is the right move. Reads the
/// *scored* entries only: a candidate sharing no comparable signal
/// with the spec is excluded from the ranking, so the warning can
/// never report a fabricated "similarity 0.00".
fn similar_doc_warning(
    spec: &ScaffoldSpec,
    rel_path: &Path,
    graph: &Graph,
    config: &Config,
) -> Option<String> {
    let target = crate::query::similar::SimilarityTarget::Spec {
        title: &spec.title,
        kind: Some(spec.kind.as_str()),
        tags: &[],
        parent_dir: rel_path.parent(),
    };
    let opts = crate::query::similar::SimilarityOptions::from_config(config);
    let candidates =
        crate::query::similar::compute_similarity(graph, config, &target, &opts).ok()?;
    let top = candidates.entries.first()?;
    Some(format!(
        "similar doc exists: {:?} (similarity {:.2}); consider `lifecycle supersede` instead of creating a duplicate",
        top.node.id, top.score
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        IdRule, IdentityConfig, KindRule, KindsConfig, NamingRuleConfig, RulesConfig,
    };
    use crate::model::Kind;
    use crate::mutate::{BaselineBinding, BaselineProbe};

    fn adr_config() -> Config {
        Config {
            kinds: KindsConfig {
                allowed: vec!["adr".into(), "guide".into()],
            },
            identity: IdentityConfig {
                kind_rules: vec![KindRule {
                    glob: "docs/decisions/**".into(),
                    kind: "adr".into(),
                }],
                id_rules: vec![IdRule {
                    kind: "adr".into(),
                    glob: None,
                    template: "adr-{stem}".into(),
                }],
            },
            rules: RulesConfig {
                naming: vec![NamingRuleConfig {
                    glob: "docs/decisions/**".into(),
                    pattern: r"^\d{4}-[a-z0-9-]+\.md$".into(),
                    sequential: true,
                    unique: true,
                }],
                ..Default::default()
            },
            ..Config::default()
        }
    }

    fn spec(kind: &str, title: &str, id: Option<&str>, path: Option<&str>) -> ScaffoldSpec {
        ScaffoldSpec {
            kind: Kind::new(kind),
            title: title.into(),
            id: id.map(String::from),
            path: path.map(PathBuf::from),
            body: None,
            fields: vec![],
        }
    }

    fn inert_probe(root: &Path, config: &Config) -> BaselineProbe {
        BaselineBinding::resolve(root, config)
            .expect("a readable baseline")
            .snapshot(|_, _| unreachable!("the fixture binds no baseline"))
            .expect("a binding with nothing bound needs no snapshot")
    }

    #[test]
    fn infers_sequential_filename_from_empty_project() {
        let scratch = tempfile::tempdir().expect("scratch root");
        let config = adr_config();
        let probe = inert_probe(scratch.path(), &config);
        let (result, _) = scaffold(
            scratch.path(),
            spec("adr", "Retry policy", None, None),
            &config,
            &probe,
            false,
            false,
            crate::test_today(),
        )
        .unwrap();
        assert_eq!(
            crate::path_guard::forward_string(&result.path),
            "docs/decisions/0001-retry-policy.md"
        );
        assert_eq!(result.id, "adr-0001-retry-policy");
        assert!(!result.written);
    }

    #[test]
    fn increments_sequence_from_existing_documents() {
        // The before-graph is built live from the working tree, so a
        // real on-disk ADR drives the next sequence number — no prior
        // `nodex build` involved.
        let scratch = tempfile::tempdir().expect("scratch root");
        let dir = scratch.path().join("docs/decisions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("0003-auth.md"),
            "---\nid: adr-0003-auth\ntitle: Auth\nkind: adr\nstatus: active\n---\n# Auth\n",
        )
        .unwrap();
        let config = adr_config();
        let probe = inert_probe(scratch.path(), &config);
        let (result, _) = scaffold(
            scratch.path(),
            spec("adr", "Cache eviction", None, None),
            &config,
            &probe,
            false,
            false,
            crate::test_today(),
        )
        .unwrap();
        assert_eq!(
            crate::path_guard::forward_string(&result.path),
            "docs/decisions/0004-cache-eviction.md"
        );
    }

    #[test]
    fn rejects_unknown_kind() {
        let scratch = tempfile::tempdir().expect("scratch root");
        let config = adr_config();
        let probe = inert_probe(scratch.path(), &config);
        let err = scaffold(
            scratch.path(),
            spec("wat", "x", None, None),
            &config,
            &probe,
            false,
            false,
            crate::test_today(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Config(_)));
    }

    #[test]
    fn explicit_path_bypasses_kind_rule() {
        let config = Config {
            kinds: KindsConfig {
                allowed: vec!["note".into()],
            },
            ..Config::default()
        };
        let scratch = tempfile::tempdir().expect("scratch root");
        let probe = inert_probe(scratch.path(), &config);
        let (result, _) = scaffold(
            scratch.path(),
            spec("note", "Hello", Some("note-hello"), Some("misc/hello.md")),
            &config,
            &probe,
            false,
            false,
            crate::test_today(),
        )
        .unwrap();
        assert_eq!(result.path.to_string_lossy(), "misc/hello.md");
        assert_eq!(result.id, "note-hello");
    }

    #[test]
    fn detects_id_collision_against_a_live_on_disk_document() {
        let scratch = tempfile::tempdir().expect("scratch root");
        std::fs::create_dir_all(scratch.path().join("docs")).unwrap();
        std::fs::write(
            scratch.path().join("docs/taken.md"),
            "---\nid: note-hello\ntitle: Taken\n---\n# Taken\n",
        )
        .unwrap();
        let config = Config {
            kinds: KindsConfig {
                allowed: vec!["note".into(), "generic".into()],
            },
            ..Config::default()
        };
        let probe = inert_probe(scratch.path(), &config);
        let err = scaffold(
            scratch.path(),
            spec("note", "Hello", Some("note-hello"), Some("misc/hello.md")),
            &config,
            &probe,
            false,
            false,
            crate::test_today(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::DuplicateId { .. }));
    }

    #[test]
    fn reserved_and_duplicate_field_keys_refused() {
        let scratch = tempfile::tempdir().expect("scratch root");
        let config = Config {
            kinds: KindsConfig {
                allowed: vec!["note".into()],
            },
            ..Config::default()
        };
        let probe = inert_probe(scratch.path(), &config);
        for reserved in ["id", "title", "kind", "status", "path"] {
            let mut s = spec("note", "Hello", Some("note-hello"), Some("misc/hello.md"));
            s.fields = vec![(reserved.to_string(), "x".to_string())];
            let err = scaffold(
                scratch.path(),
                s,
                &config,
                &probe,
                false,
                false,
                crate::test_today(),
            )
            .unwrap_err();
            match err {
                Error::Config(msg) => assert!(msg.contains("reserved"), "{msg}"),
                other => panic!("expected Config error, got {other:?}"),
            }
        }
        let mut s = spec("note", "Hello", Some("note-hello"), Some("misc/hello.md"));
        s.fields = vec![
            ("owner".to_string(), "\"a\"".to_string()),
            ("owner".to_string(), "\"b\"".to_string()),
        ];
        let err = scaffold(
            scratch.path(),
            s,
            &config,
            &probe,
            false,
            false,
            crate::test_today(),
        )
        .unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("more than once"), "{msg}"),
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    #[test]
    fn supplied_field_preempts_its_required_default() {
        // A supplied `owner` enters `seen` before the defaults pass, so
        // the rendered frontmatter carries the caller's value exactly
        // once — never a second placeholder line.
        let scratch = tempfile::tempdir().expect("scratch root");
        let mut config = Config {
            kinds: KindsConfig {
                allowed: vec!["note".into()],
            },
            ..Config::default()
        };
        config.schema.required = vec!["owner".into()];
        let probe = inert_probe(scratch.path(), &config);
        let mut s = spec("note", "Hello", Some("note-hello"), Some("misc/hello.md"));
        s.fields = vec![("owner".to_string(), "\"platform\"".to_string())];
        let (result, _) = scaffold(
            scratch.path(),
            s,
            &config,
            &probe,
            false,
            false,
            crate::test_today(),
        )
        .unwrap();
        assert_eq!(
            result.content.matches("owner:").count(),
            1,
            "exactly one owner line:\n{}",
            result.content
        );
        assert!(result.content.contains("owner: \"platform\""));
    }

    #[test]
    fn directory_from_glob_handles_literals() {
        assert_eq!(
            directory_from_glob("docs/decisions/**"),
            Some(PathBuf::from("docs/decisions"))
        );
        assert_eq!(directory_from_glob("**/SKILL.md"), None);
    }

    #[test]
    fn rule_targets_directory_common_shapes() {
        // Trailing ** is the canonical form.
        assert!(rule_targets_directory(
            "docs/decisions/**",
            "docs/decisions"
        ));
        // Wildcard leaf globs must still target the parent directory.
        assert!(rule_targets_directory(
            "docs/decisions/*.md",
            "docs/decisions"
        ));
        assert!(rule_targets_directory(
            "docs/decisions/[0-9]*.md",
            "docs/decisions"
        ));
        assert!(rule_targets_directory(
            "docs/decisions/?*",
            "docs/decisions"
        ));
        // Middle-path wildcard resolves its literal prefix only.
        assert!(rule_targets_directory("docs/*/decisions/**", "docs"));
        // Disjoint directories must not match.
        assert!(!rule_targets_directory("docs/guides/**", "docs/decisions"));
        // Leading wildcard has no literal prefix at all.
        assert!(!rule_targets_directory("**/SKILL.md", "docs/decisions"));
    }

    #[test]
    fn scaffold_rejects_non_md_extension() {
        let config = Config {
            kinds: KindsConfig {
                allowed: vec!["note".into()],
            },
            ..Config::default()
        };
        let scratch = tempfile::tempdir().expect("scratch root");
        let probe = inert_probe(scratch.path(), &config);
        let err = scaffold(
            scratch.path(),
            spec("note", "x", Some("note-x"), Some("misc/hello.txt")),
            &config,
            &probe,
            false,
            false,
            crate::test_today(),
        )
        .unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains(".md"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }
}
