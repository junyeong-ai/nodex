use anyhow::{Context, Result};
use chrono::NaiveDate;
use clap::{Args, ValueEnum};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use nodex_core::check;
use nodex_core::rules::Severity;

use crate::format::emit_read_with;

use super::content_source::read_content_source;
use super::git_worktree::{BaselineResolution, ensure_repository};

/// Severity filter accepted by `nodex check --severity`.
#[derive(Clone, Copy, ValueEnum)]
pub enum CheckSeverity {
    Error,
    Warning,
}

impl From<CheckSeverity> for Severity {
    fn from(s: CheckSeverity) -> Self {
        match s {
            CheckSeverity::Error => Self::Error,
            CheckSeverity::Warning => Self::Warning,
        }
    }
}

/// Args for `nodex check`.
#[derive(Args)]
pub struct CheckArgs {
    /// Validate proposed content before it is written, as `PATH=SOURCE`
    /// pairs. Repeatable: every pair is overlaid into ONE graph build so
    /// cross-proposal references resolve (a `supersede` that rewrites N
    /// referrers gates as one atomic edit). `PATH` is the in-project
    /// document the bytes would become; `SOURCE` is `-` for stdin or a
    /// file path resolved against the invoking directory (not `-C DIR`;
    /// the proposed bytes may legitimately live outside the project). The
    /// same immutability / schema / cross-field rules gate the edit at
    /// its source instead of every agent reimplementing them. At most one
    /// `SOURCE` may be `-` (stdin is one stream); a target path may appear
    /// once. Mutually exclusive with `--since`.
    #[arg(long, value_name = "PATH=SOURCE", conflicts_with = "since")]
    pub content: Vec<String>,
    /// Narrow the `violations` list to one severity. Presentation only:
    /// `has_errors`, the per-proposal verdicts and the exit code answer for
    /// every violation checked, and anything the envelope stops carrying
    /// rides a `gate_suppression` warning.
    #[arg(long, value_enum)]
    pub severity: Option<CheckSeverity>,
    /// Restrict violations to nodes that changed since the given git
    /// ref. Activates diff-aware rules (e.g. `frontmatter_immutable`).
    #[arg(long, value_name = "REF")]
    pub since: Option<String>,
}

pub fn run(root: &Path, args: CheckArgs, pretty: bool, today: NaiveDate) -> Result<()> {
    let severity_filter = args.severity.map(Severity::from);
    let config = nodex_core::load_project(root)?;

    let target = resolve_target(root, &args, &config, today)?;

    // The graph under evaluation may be an overlay build's (`--content`),
    // and a rule that stat-probes the project has to measure the project
    // that graph describes rather than the tree on disk. A working-tree
    // target carries an empty overlay, so one construction serves both.
    let check_report = check(
        &target.graph,
        &config,
        nodex_core::builder::scanner::ProjectFiles::proposed(root, &target.overlay),
        target.diff.as_ref(),
        today,
    );

    // The proposed nodes' absolute warning view, captured from the
    // overlay report before the introduced-delta filter consumes it.
    // Absolute means superset: a warning the proposal itself introduces
    // appears here AND in `violations` — `standing` answers "what does
    // this doc carry in the proposed state", the delta answers "what
    // did this write add", and the two questions overlap by design.
    let standing = target.proposals.as_ref().map(|proposals| {
        check_report
            .violations
            .iter()
            .filter(|v| {
                v.severity == Severity::Warning
                    && v.path.as_deref().is_some_and(|p| {
                        proposals
                            .iter()
                            .any(|(path, in_scope)| *in_scope && p == path)
                    })
            })
            .cloned()
            .collect::<Vec<_>>()
    });

    // Scoping is per-mode. `--content` uses the before/after delta
    // (`rules::introduced_violations` — the count-aware multiset
    // difference shared with scaffold's gate): a violation also present
    // in the pre-overlay report is pre-existing and never refuses the
    // proposal; one the overlay introduces — whatever node it lands on,
    // including a node-less parse_failure for a proposal that destroys
    // its own node — does. `--since` keeps the pure set-membership
    // filter, where node-less violations (project-wide problems, e.g.
    // cycle detection) are *kept* so a narrowed scope never silently
    // drops a finding that can't be attributed to a specific id; the
    // "no silent skips" doctrine applies to violations as well as rules.
    let violations_filtered: Vec<_> = if let Some(before) = &target.baseline_violations {
        nodex_core::rules::introduced_violations(check_report.violations, before)
    } else {
        match &target.changed_ids {
            Some(ids) => check_report
                .violations
                .into_iter()
                .filter(|v| match &v.node_id {
                    Some(id) => ids.contains(id),
                    None => true,
                })
                .collect(),
            None => check_report.violations,
        }
    };

    // Every verdict this command publishes is drawn from the set the rules
    // judged, before `--severity` narrows what is displayed. A filter is
    // presentation, and presentation that moves a verdict is a gate that
    // answers for something other than what it checked: `--severity warning`
    // would otherwise report a project with eight errors as green.
    let has_errors = violations_filtered
        .iter()
        .any(|v| v.severity == Severity::Error);

    // Per-proposal verdicts (`--content` only). The introduced violations
    // live once in `violations`, each carrying its `path`; here we only
    // enumerate the proposals and whether each introduced an error, so a
    // clean or out-of-scope proposal is still reported as checked. Node-less
    // and cross-file findings stay in the flat list — a per-proposal reader
    // never silently loses them.
    let proposals = target.proposals.as_ref().map(|proposals| {
        proposals
            .iter()
            .map(|(path, in_scope)| nodex_core::ProposalEntry {
                path: path.clone(),
                in_scope: *in_scope,
                has_path_errors: violations_filtered.iter().any(|v| {
                    v.severity == Severity::Error && v.path.as_deref() == Some(path.as_str())
                }),
            })
            .collect()
    });

    let violations_final: Vec<_> = match severity_filter {
        Some(target) => violations_filtered
            .iter()
            .filter(|v| v.severity == target)
            .cloned()
            .collect(),
        None => violations_filtered.clone(),
    };

    let result = nodex_core::CheckResult {
        total: violations_final.len(),
        violations: violations_final,
        skipped_rules: check_report.skipped_rules,
        rule_coverage: check_report.rule_coverage,
        has_errors,
        proposals,
        standing,
    };

    // The ones the filter took out of the list, less any the response reports
    // elsewhere: a warning `standing` still carries was never hidden, and
    // announcing it would spend the one code meaning "there is a finding you
    // cannot see" on a finding in the same envelope.
    let hidden_by_filter = match severity_filter {
        None => 0,
        Some(target) => violations_filtered
            .iter()
            .filter(|v| v.severity != target)
            .filter(|v| !result.reported_beside_the_list(v))
            .count(),
    };

    let mut warnings = target.warnings;
    if hidden_by_filter > 0 {
        // The spelling clap parsed the flag with, so the advisory quotes the
        // operator's own word rather than a second rendering of the vocabulary.
        let shown = args
            .severity
            .and_then(|s| s.to_possible_value())
            .expect("a filter is what hid them");
        warnings.push(nodex_core::Warning::new(
            nodex_core::WarningCode::GateSuppression,
            format!(
                "--severity {} hid {hidden_by_filter} violation(s) from the list; \
                 `has_errors` and the exit code answer for every violation checked, not for the \
                 shown set — drop --severity to see them all",
                shown.get_name()
            ),
        ));
    }

    emit_read_with(result, warnings, &config, pretty);

    if has_errors {
        std::process::exit(1);
    }

    Ok(())
}

/// The graph a check run evaluates, plus how its violations are scoped.
struct CheckTarget {
    /// Graph the rules run against — the working tree, or the working
    /// tree with a proposed-content overlay (`--content`).
    graph: nodex_core::Graph,
    /// Node ids to narrow violations to (set-membership) for `--since`,
    /// or `None` for an unscoped project-wide check.
    changed_ids: Option<BTreeSet<String>>,
    /// Violations of the pre-overlay working tree (`--content` only).
    /// The reported set is the count-aware multiset difference
    /// (`rules::introduced_violations`): each occurrence here cancels
    /// at most one identical occurrence in the overlay report, so
    /// every violation the proposal introduces gates it — including a
    /// duplicate of a pre-existing one.
    baseline_violations: Option<Vec<nodex_core::Violation>>,
    /// Diff that activates diff-aware rules, when one is available.
    diff: Option<nodex_core::diff::GraphDiff>,
    /// One `(normalized forward-slash path, in_scope)` per `--content`
    /// proposal, in invocation order. `Some` only in `--content` mode —
    /// drives the per-proposal verdicts so a clean or out-of-scope
    /// proposal is reported as checked, never a silent green.
    proposals: Option<Vec<(String, bool)>>,
    /// Non-fatal advisories to surface on the envelope.
    warnings: Vec<nodex_core::Warning>,
    /// The proposal the target graph was built with, empty for a
    /// working-tree target. The rule pass probes the filesystem through it,
    /// so a `--content` verdict measures the project the proposal produces.
    overlay: Vec<(PathBuf, nodex_core::builder::scanner::Proposed)>,
}

/// Resolve what to check and how to scope it.
///
/// `--content` validates one or more unwritten proposals: the *before*
/// graph is the working tree and the *after* graph overlays every
/// proposed `PATH=SOURCE` pair at once, so the diff names exactly what
/// the edit (or batch) changes and the diff-aware immutability rules see
/// "already on disk" as the baseline (the launder-safe boundary — never
/// an older committed ref).
/// Both graphs are built read-only, so a write-time check never touches
/// `cache.json`. Otherwise the working tree is the target, scoped by
/// `--since` / `rules.immutable_baseline` via [`resolve_diff`].
fn resolve_target(
    root: &Path,
    args: &CheckArgs,
    config: &nodex_core::Config,
    today: NaiveDate,
) -> Result<CheckTarget> {
    if !args.content.is_empty() {
        return resolve_content_target(root, &args.content, config, today);
    }

    let outcome = nodex_core::builder::build(root, config, false).context("graph build failed")?;
    let current = outcome.graph;
    let (changed_ids, diff, baseline_warnings) = resolve_diff(root, args, config, &current)?;
    // Surface the build's non-fatal advisories (scope coverage gaps,
    // cache problems); the diff-baseline advisory follows. Dropped
    // documents — unreadable, non-UTF-8, or unparseable — are not
    // advisories: they are `parse_failure` violations the rule pass
    // reports from the graph itself.
    let mut warnings = outcome.warnings;
    warnings.extend(baseline_warnings);
    Ok(CheckTarget {
        graph: current,
        changed_ids,
        baseline_violations: None,
        diff,
        proposals: None,
        warnings,
        overlay: Vec::new(),
    })
}

/// Resolve a `--content` batch into a check target. Every proposal is
/// overlaid into ONE graph build, so a reference one proposal authors
/// resolves against another proposal in the same batch — the cross-file
/// case a one-at-a-time gate gets wrong (it would report a still-dangling
/// reference). The before/after delta then refuses exactly the violations
/// the whole batch introduces (`rules::introduced_violations`), and both
/// graphs are built read-only so `cache.json` is never touched.
fn resolve_content_target(
    root: &Path,
    pairs: &[String],
    config: &nodex_core::Config,
    today: NaiveDate,
) -> Result<CheckTarget> {
    // `--content` gates a write, so it must refuse whatever the write
    // would refuse. Its own diff comes from the overlay — the working
    // tree is the before-state, and the configured baseline plays no
    // part in the verdict — but a baseline whose ref cannot be read
    // refuses every mutating command, and a gate that stayed green while
    // the write it clears cannot run would be the misleading answer.
    nodex_core::BaselineBinding::resolve(root, config)?;

    let overlay = parse_proposals(root, pairs)?;

    // The gate applies to exactly the bytes the scan would admit: an
    // out-of-scope path is vacuously clean whatever it contains (nodex
    // governs no node there). An unparseable admitted proposal needs no
    // special case — it drops from the overlay graph as a typed
    // `Graph::parse_failures` record, and the delta refuses on the new
    // `parse_failure` violation.
    let scan = nodex_core::builder::scanner::scan_scope_with_overlay(root, config, &overlay)
        .context("scope scan failed")?;

    let mut proposals = Vec::with_capacity(overlay.len());
    let mut out_of_scope = Vec::new();
    for (path, _bytes) in &overlay {
        // A proposal naming a document the graph carries under another name is
        // not out of scope — it is the same document, and blessing bytes there
        // while reporting that nodex governs nothing is the misleading green.
        if let Some((_, named)) = scan.aliases.iter().find(|(unused, _)| unused == path) {
            return Err(nodex_core::error::Error::Config(format!(
                "path {:?} names the document the graph carries as {:?}; use that path so the \
                 gate checks the right node",
                nodex_core::path_guard::forward_string(path),
                nodex_core::path_guard::forward_string(named)
            ))
            .into());
        }
        let admitted = scan.paths.iter().any(|p| p == path);
        let fwd = nodex_core::path_guard::forward_string(path);
        // A path the scan does not admit is vacuously clean whatever it
        // contains — a write gate would pass on a misaimed/out-of-scope
        // path having validated nothing. Surface it so the green is never
        // silent; the per-proposal `in_scope` flag carries the same fact.
        if !admitted {
            out_of_scope.push(nodex_core::Warning::new(
                nodex_core::WarningCode::ScopeCoverage,
                format!(
                    "path {fwd:?} is out of scope — the proposed content was validated against no \
                     rule (nodex governs no document there); verify the path or scope.include"
                ),
            ));
        }
        proposals.push((fwd, admitted));
    }

    let before =
        nodex_core::builder::build_with_overlay(root, config, &[]).context("graph build failed")?;
    let after = nodex_core::builder::build_with_overlay(root, config, &overlay)
        .context("proposed-content graph build failed")?;
    // A proposal that turns a `conditional_exclude` parent terminal drops that
    // parent's sub-artifacts from the project, and the delta below can only
    // lose the findings that leave with them. The write seams answer for it
    // through `Introduced::advisories`; a gate that stayed silent would clear
    // an edit whose effect on the project it never mentioned.
    let evicted = nodex_core::evicted(&before.graph, &after, &overlay);
    // The overlay build's advisories are the ones that ride this gate: its
    // verdict is about the project the proposal produces. One thing that build
    // cannot say is what the proposal was measured *against* — a proposal is
    // itself a scanned file, so the overlay scan is never empty and a clean
    // verdict over a corpus of nothing reads exactly like a clean verdict over
    // a whole project.
    //
    // Asked of the standing scan as its own question rather than by carrying
    // that build's advisories across. The rest of what it says is about a
    // project this proposal changes — a `scope.include` pattern that matched
    // no files there may be the very pattern the proposal's path matches — and
    // an envelope calling one proposal in scope while calling its pattern dead
    // contradicts itself.
    let standing =
        nodex_core::builder::scanner::scan_scope(root, config).context("scope scan failed")?;
    let mut warnings = after.warnings;
    warnings.extend(nodex_core::builder::scanner::coverage_warning(
        standing.paths.len(),
        "gate this proposal against",
    ));
    warnings.extend(out_of_scope);
    warnings.extend(evicted);
    let before = before.graph;
    let after = after.graph;
    let diff = nodex_core::diff::compute_diff(&before, &after);
    // The before-report anchors the delta: it runs without a diff
    // (diff-aware rules need "what changed", and nothing has), so any
    // diff-aware violation in the after-report is new by construction and
    // gates the batch.
    let baseline = check(
        &before,
        config,
        nodex_core::builder::scanner::ProjectFiles::working_tree(root),
        None,
        today,
    )
    .violations;
    Ok(CheckTarget {
        graph: after,
        changed_ids: None,
        baseline_violations: Some(baseline),
        diff: Some(diff),
        proposals: Some(proposals),
        warnings,
        overlay,
    })
}

/// Parse `--content PATH=SOURCE` pairs into a normalized overlay. Splits
/// on the first `=`; normalizes each PATH through the one canonical
/// document-path normalization (symmetric with scaffold / rename — fold
/// `\` to `/`, refuse traversal / absolute forms, collapse `.`); reads
/// each SOURCE (`-` = stdin, else a file). Two invocation-level guards
/// live here at the write seam, where validity depends on the invocation
/// not the project config: a target path may appear once (ambiguous
/// bytes otherwise) and at most one SOURCE may be stdin (one stream).
fn parse_proposals(
    root: &Path,
    pairs: &[String],
) -> Result<Vec<(PathBuf, nodex_core::builder::scanner::Proposed)>> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut stdin_used = false;
    let mut overlay = Vec::with_capacity(pairs.len());
    for pair in pairs {
        let Some((raw_path, source)) = pair.split_once('=') else {
            return Err(nodex_core::error::Error::Config(format!(
                "--content expects PATH=SOURCE, got {pair:?} (no '='); e.g. \
                 --content docs/a.md=- or --content docs/a.md=draft.md"
            ))
            .into());
        };
        if raw_path.is_empty() {
            return Err(nodex_core::error::Error::Config(format!(
                "--content {pair:?} has an empty PATH"
            ))
            .into());
        }
        let path = nodex_core::path_guard::normalize_doc_path(root, raw_path)?;
        if !seen.insert(path.clone()) {
            return Err(nodex_core::error::Error::Config(format!(
                "--content names {path:?} more than once; each target path may appear once"
            ))
            .into());
        }
        if source == "-" {
            if stdin_used {
                return Err(nodex_core::error::Error::Config(
                    "--content reads stdin (`-`) for at most one proposal; stdin is a single \
                     stream — write the other proposals to files"
                        .to_string(),
                )
                .into());
            }
            stdin_used = true;
        }
        let bytes = read_content_source(source)?;
        overlay.push((
            PathBuf::from(path),
            nodex_core::builder::scanner::Proposed::Content(bytes),
        ));
    }
    Ok(overlay)
}

/// `(changed_ids, diff, warnings)` from [`resolve_diff`]: which node ids
/// to narrow violations to (only for explicit `--since`), the diff that
/// activates diff-aware rules, and any non-fatal advisories.
type DiffResolution = (
    Option<BTreeSet<String>>,
    Option<nodex_core::diff::GraphDiff>,
    Vec<nodex_core::Warning>,
);

/// Resolve the diff baseline for a check run, returning
/// `(changed_ids, diff, warnings)`.
///
/// An explicit `--since` does double duty: it supplies the diff that
/// activates diff-aware rules AND narrows the reported violations to
/// the nodes it names (`GraphDiff::touched_ids`, so the narrowing and
/// the activation can never disagree). When `--since` is omitted, the
/// configured `rules.immutable_baseline` supplies a diff so the
/// immutability rules run by default. The baseline deliberately does NOT
/// narrow the violation set, because the operator never asked to scope
/// the report.
///
/// Both go through the one shared substrate (`git_worktree`, also
/// consumed by `query issues`), so every consumer surfaces the same
/// violations and the same inert advisory when the baseline cannot
/// engage — not a silent skip, and not the misleading "needs --since"
/// skip reason the rules would emit. The diff is computed against the
/// already-built `current` graph, never a rebuild.
///
/// Single-lens semantics: the working tree's `config` is the one lens
/// and the ref supplies *content only* — the before tree's own
/// `nodex.toml` is never loaded. The diff reports content changes under
/// today's contract (mirroring `--content`, where one config views two
/// content states), and a PR that migrates the config format itself can
/// still pass the gate — under per-ref configs such a PR deadlocks,
/// because the base ref's config no longer parses under the new binary.
fn resolve_diff(
    root: &Path,
    args: &CheckArgs,
    config: &nodex_core::Config,
    current: &nodex_core::Graph,
) -> Result<DiffResolution> {
    let (resolution, narrowing) = match args.since.as_deref() {
        Some(git_ref) => {
            let repository = ensure_repository(root, "nodex check --since")?;
            let resolution = super::git_worktree::diff_against_ref(
                root,
                &repository,
                git_ref,
                config,
                current,
                ".nodex-check",
            )?;
            (resolution, true)
        }
        None => (
            super::git_worktree::baseline_diff(root, config, current, ".nodex-check")?,
            false,
        ),
    };
    Ok(match resolution {
        BaselineResolution::Resolved(baseline) => (
            narrowing.then(|| baseline.diff.touched_ids()),
            Some(baseline.diff),
            baseline.warnings,
        ),
        // An inert resolution leaves nothing to narrow *to*, so an
        // explicit `--since` widens back to the whole project. The
        // operator asked for a scope and is getting another one, which
        // they must hear — the advisory alone reads as being about the
        // rules, not about the report they are holding.
        BaselineResolution::Inert { warning } => {
            let mut warnings = vec![warning];
            if narrowing {
                warnings.push(nodex_core::Warning::new(
                    nodex_core::WarningCode::GateSuppression,
                    "--since could not be resolved into a set of changed nodes, so the \
                     report covers every node rather than the scope requested"
                        .to_string(),
                ));
            }
            (None, None, warnings)
        }
        BaselineResolution::NotApplicable => (None, None, vec![]),
    })
}
