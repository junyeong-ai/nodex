use schemars::JsonSchema;
use serde_json::{Map, Value};

pub mod body_immutable;
pub mod body_line;
pub mod detail;
pub mod freshness;
pub mod frontmatter_immutable;
pub mod git_drift;
pub mod graph_invariants;
pub mod naming;
pub mod parse;
pub mod schema;
pub mod unresolved_reference;

use chrono::NaiveDate;
use std::collections::HashMap;
use std::path::Path;

use crate::builder::scanner::ProjectFiles;
use crate::config::Config;
use crate::diff::GraphDiff;
use crate::error::{Error, Result};
use crate::model::Graph;

pub use detail::{DriftHotspot, Evidence, ValueKind, ViolationDetails};

/// Provenance of a [`Rule`] — distinguishes nodex-shipped built-ins
/// from rules instantiated per `[[rules.body_line]]` (or future
/// per-block) config block. Consumers of `nodex export rules` use
/// this to render UIs that say "this rule disappears if the config
/// block is removed".
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuleSource {
    /// Rule code is part of nodex (e.g. `required_field`,
    /// `frontmatter_immutable`). May still be inert when its driving
    /// config is absent — in which case `registered_rules` omits it
    /// from the registry entirely.
    Builtin,
    /// One rule per config block (`body_line/<name>`). Removing the
    /// block removes the rule from the registry.
    Config,
}

/// Verify the runtime prerequisites of every opt-in rule. Today only
/// `git_drift_threshold` has any (git on PATH + a git work tree
/// containing `root`). Call once after [`Config::load`] and before any
/// command that could exercise the rules — failures surface as
/// [`Error::Config`] so the operator sees `CONFIG_ERROR` and exit 2,
/// not a buried check violation.
///
/// Resolving the binding *is* the probe: [`crate::git::Repository`] has
/// no representation for an unusable environment, so the two states this
/// reports — no git, or no work tree — are exactly the two states in
/// which the rule cannot measure.
pub fn preflight(config: &Config, root: &Path) -> Result<()> {
    if config.detection.git_drift_threshold.is_some() {
        let reason = match crate::git::Repository::discover(root) {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => format!("no git work tree was found for {}", root.display()),
            Err(e) => format!("its repository could not be resolved ({e})"),
        };
        return Err(Error::Config(format!(
            "detection.git_drift_threshold is set but {reason}; \
             install git and run inside a git work tree, or remove the threshold"
        )));
    }
    Ok(())
}

/// Severity of a rule violation.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

/// A single rule violation. Every field is built only from deterministic
/// graph and config data, so two passes over one project produce the same
/// findings — which is what lets the proposal gates diff them
/// ([`introduced_violations`], pairing on a `finding_identity` rather than
/// on the whole struct: a document that moved is the same document).
///
/// `message` is a rendered projection of `details`
/// ([`ViolationDetails::render_message`]); `details` is the typed,
/// machine-actionable cause an agent branches on. They are kept in lock
/// step by the single constructor [`Violation::new`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, JsonSchema)]
pub struct Violation {
    pub rule_id: String,
    pub severity: Severity,
    pub node_id: Option<String>,
    pub path: Option<String>,
    pub message: String,
    pub details: ViolationDetails,
}

impl Violation {
    /// Build a violation from its typed cause, rendering the human
    /// `message` from `details` so the prose and the structured payload
    /// are one source. Every rule constructs violations through here.
    pub fn new(
        rule_id: impl Into<String>,
        severity: Severity,
        node_id: Option<String>,
        path: Option<String>,
        details: ViolationDetails,
    ) -> Self {
        let message = details.render_message();
        Self {
            rule_id: rule_id.into(),
            severity,
            node_id,
            path,
            message,
            details,
        }
    }
}

/// Everything a [`Rule`] is allowed to read while evaluating. Bundling
/// these into a single context lets the trait grow new inputs (file
/// mtime cache, git history reader, …) without churning every
/// implementor's signature.
pub struct RuleContext<'a> {
    pub graph: &'a Graph,
    pub config: &'a Config,
    /// Where the project's bytes are for this pass. A rule that probes the
    /// filesystem asks through this rather than joining `root` itself, so a
    /// pass over an overlay build's graph measures the project that graph
    /// describes instead of the tree on disk. `files.root()` is the root for
    /// everything else.
    pub files: ProjectFiles<'a>,
    /// The repository the project is tracked in, resolved once for this
    /// pass by the runner — owned because the runner is its only
    /// producer. `None` when no registered rule measures git, so a
    /// git-backed rule reads a binding instead of rediscovering one per
    /// document, and a project without git-backed rules never spawns a
    /// process.
    pub repository: Option<crate::git::Repository>,
    /// Structural delta from a past ref to the current graph. `None`
    /// when no diff context is available; `Some(_)` when `check` has one
    /// from `--since <ref>` or a configured `rules.immutable_baseline`.
    /// Rules whose semantic requires "this is what changed" (e.g.
    /// `frontmatter_immutable`) declare themselves non-applicable via
    /// [`Rule::is_applicable`] when this is `None`.
    pub since: Option<&'a GraphDiff>,
    /// The date every date-relative rule measures against, resolved once
    /// per pass by the caller. A rule reads this rather than the system
    /// clock so a pass is a pure function of its inputs: the same graph
    /// checked against the same date yields the same report, on any
    /// machine and on any day.
    pub today: NaiveDate,
}

/// Whether a per-block `kinds` filter admits `kind` — an empty filter
/// admits every kind. The string-keyed counterpart to
/// [`crate::model::Node::matches_kinds`], for the diff-aware rules that
/// gate on a node's *before* kind (a bare `&str` carried by the diff)
/// rather than a live [`crate::model::Node`].
pub(crate) fn kind_allowed(kinds: &[String], kind: &str) -> bool {
    kinds.is_empty() || kinds.iter().any(|k| k == kind)
}

/// One rule that the runner declined to evaluate, with a one-line reason.
/// Symmetric to [`Violation`] — silent skipping would let a strict-mode
/// rule appear to "pass" when it never actually ran.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
pub struct SkippedRule {
    pub rule_id: String,
    pub reason: String,
}

/// The unit a rule iterates. A closed vocabulary, so a coverage record
/// reads on its own — `subjects: 0` is only actionable when "0 what?"
/// has an answer that does not require the rules manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SubjectUnit {
    Nodes,
    Edges,
    Files,
}

/// What one applied rule was given to judge, and what it found.
///
/// `subjects` is the population the rule guarded — the documents, edges or
/// files its declared scope selected, whether or not any of them turned out
/// to offend. It is deliberately not the offending subset: a rule that
/// guarded four hundred documents and a rule that guarded none both return
/// no violations, and telling those apart is the whole point. Read this way
/// zero has one meaning everywhere — the rule was handed nothing, so what it
/// enforces is not in effect here.
pub struct RuleRun {
    pub subjects: usize,
    pub unjudged: usize,
    pub violations: Vec<Violation>,
}

impl RuleRun {
    /// A run over `subjects` units that found nothing.
    pub fn clean(subjects: usize) -> Self {
        Self {
            subjects,
            unjudged: 0,
            violations: Vec::new(),
        }
    }

    pub fn new(subjects: usize, violations: Vec<Violation>) -> Self {
        Self {
            subjects,
            unjudged: 0,
            violations,
        }
    }

    /// Units this rule's declared scope selected and it could not judge,
    /// because what it judges them *against* is missing.
    ///
    /// Not a loss count, and what a non-zero asks of a reader is the rule's
    /// own question. A lock reading a record against a baseline counts one
    /// with no baseline record — most often a document authored since, which
    /// costs nothing and is the ordinary state of any run that adds one;
    /// `git_drift` counts a node whose every drift-relation edge went
    /// unmeasured, which is a reference to fix. Neither rule can tell its
    /// harmless case from the one that does cost something, and a count that
    /// guessed would be worth less than one that does not. What it is is the
    /// difference between what a rule stands over and what it could reach,
    /// said in the response that reports the reach rather than left to be
    /// inferred by comparing against a run the reader does not have.
    ///
    /// Only a rule that judges a unit against something outside the unit can
    /// have any: for the rest the scope selects exactly what gets judged, and
    /// zero is the honest answer rather than an unfilled field.
    pub fn unjudged(mut self, unjudged: usize) -> Self {
        self.unjudged = unjudged;
        self
    }
}

/// One applied rule's reach: the rule ran, and this is how much of the
/// project it had to run over.
///
/// A rule that examines nothing passes for the same reason a rule that
/// examines everything passes, and `violations` alone cannot tell the two
/// apart. A declared rule whose subject set is empty is inert config —
/// governance in the file, none in effect — and the only place that shows
/// is here.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
pub struct RuleCoverage {
    pub rule_id: String,
    pub unit: SubjectUnit,
    pub subjects: usize,
    /// How many units the rule's scope selected and it could not judge — see
    /// [`RuleRun::unjudged`]. Reported beside the reach so one response
    /// carries both, and read as a difference rather than a defect: what a
    /// non-zero points at is the rule's own question. For an immutability
    /// lock it rises with the documents a run adds, and those are recoverable
    /// as the ids the baseline holds no node for. For `git_drift` it is the
    /// documents whose every drift edge was unmeasurable, recoverable from
    /// `query issues` wherever the cause is a `covers:` target that resolves
    /// to nothing — but not where the target resolves and git cannot measure
    /// it, which is a count with no members published anywhere.
    pub unjudged: usize,
}

/// Self-describing validation rule. The single source of truth for
/// everything that [`check`] runs *and* everything that
/// `export::export_rules` surfaces in the manifest — there is no
/// parallel hand-written description / params / source / diff-aware
/// list in `export.rs` to keep in sync.
///
/// Adding a new built-in rule is a single-file change: implement this
/// trait, then add an entry to [`registered_rules`].
pub trait Rule: Send + Sync {
    fn id(&self) -> &str;
    fn severity(&self) -> Severity;
    fn check(&self, ctx: &RuleContext<'_>) -> RuleRun;

    /// The unit [`Rule::check`] counts into [`RuleRun::subjects`].
    /// Static per rule — a rule iterates one kind of thing.
    fn subject_unit(&self) -> SubjectUnit;

    /// One-line human-readable description of what this rule enforces.
    /// Surfaced in `nodex export rules` so downstream consumers don't
    /// hardcode rule semantics. Static for built-ins; instances can
    /// override when the description varies per construction (none
    /// today).
    fn description(&self) -> &str;

    /// True when this rule's prerequisites are satisfied for the given
    /// context. Default: always applicable. Override for rules whose
    /// semantics requires a diff context, opt-in environment, etc.
    fn is_applicable(&self, _ctx: &RuleContext<'_>) -> bool {
        true
    }
    /// One-line reason returned when [`Rule::is_applicable`] returns
    /// false. Empty by default — required only for rules that override
    /// `is_applicable`.
    fn skip_reason(&self, _ctx: &RuleContext<'_>) -> String {
        String::new()
    }
    /// Self-report whether this rule semantically requires a diff
    /// context (`check --since <ref>`) to fire. Surfaced on the
    /// rules manifest so downstream tooling (CI gates, PR-only
    /// validators) can dispatch on this without hardcoding the list
    /// of diff-aware rules. Default `false` for rules that operate on
    /// a single graph snapshot.
    fn diff_aware(&self) -> bool {
        false
    }
    /// Where the rule comes from — built-in code or per-config-block
    /// instance. Default [`RuleSource::Builtin`]; per-block rules
    /// (e.g. `BodyLineRule`) override to [`RuleSource::Config`].
    fn source(&self) -> RuleSource {
        RuleSource::Builtin
    }
    /// Rule-specific parameters surfaced on the manifest entry — the
    /// configured values that distinguish this rule instance from
    /// another in the same family (regex pattern, kinds,
    /// mode, enums, thresholds, …). Default empty; rules whose
    /// behaviour depends on declarative config (e.g. `stale_review`
    /// reads `detection.stale_days`) override to surface the live
    /// values. The schema is per-rule (described in
    /// [`Self::description`]) — kept as a free-form object so adding
    /// a new built-in rule doesn't reshape the manifest.
    fn params(&self, _config: &Config) -> Map<String, Value> {
        Map::new()
    }
}

/// Build the registered rule set for the project. Single source of
/// truth for both [`check`] (runs them) and
/// `nodex_core::export::export_rules` (emits the manifest). Adding a
/// new rule = adding it here.
///
/// Rules whose driving config block is absent are omitted from the
/// registry entirely — they are not "skipped" because there was
/// nothing to skip. The skipped-rule surface remains for rules whose
/// config IS present but whose runtime prerequisites aren't met
/// (e.g. `frontmatter_immutable` configured but `check` invoked
/// without `--since`).
///
/// A registry is valid for one `(graph, root)` evaluation: the per-row
/// `unresolved_reference` instances share a classification cell filled
/// once on first check, so a registry reused against a different graph
/// or root serves the first pass's classification. Build a fresh
/// registry per pass — as [`check`] does.
pub fn registered_rules(config: &Config) -> Vec<Box<dyn Rule>> {
    rules_with_classification(
        config,
        unresolved_reference::SharedClassification::default(),
    )
}

/// [`registered_rules`] with the unresolved-edge classification cell
/// supplied by the caller — the seam [`check_with_unresolved`] uses to
/// seed an already-computed classification into the per-row
/// `unresolved_reference` instances. The cell type stays private to the
/// rules layer; external callers hand over a plain `Vec`.
fn rules_with_classification(
    config: &Config,
    classification: unresolved_reference::SharedClassification,
) -> Vec<Box<dyn Rule>> {
    let mut rules: Vec<Box<dyn Rule>> = vec![
        Box::new(parse::ParseFailureRule),
        Box::new(parse::FieldParseRule),
        Box::new(schema::RequiredFieldRule),
        Box::new(schema::FieldTypeRule),
        Box::new(schema::FieldEnumRule),
        Box::new(schema::CrossFieldRule),
    ];
    if matches!(config.schema.mode, crate::config::SchemaMode::Strict) {
        rules.push(Box::new(schema::UnknownFieldRule));
    }
    if !config.schema.require_explicit.is_empty() {
        rules.push(Box::new(schema::ExplicitFieldRule));
    }
    rules.push(Box::new(freshness::StaleReviewRule));
    if config.detection.git_drift_threshold.is_some() {
        rules.push(Box::new(git_drift::GitDriftRule));
    }
    if !config.rules.naming.is_empty() {
        rules.push(Box::new(naming::FilenamePatternRule));
        if config.rules.naming.iter().any(|n| n.sequential) {
            rules.push(Box::new(naming::SequentialNumberingRule));
        }
        if config.rules.naming.iter().any(|n| n.unique) {
            rules.push(Box::new(naming::UniqueNumberingRule));
        }
    }
    for block in &config.rules.frontmatter_immutable {
        rules.push(Box::new(
            frontmatter_immutable::FrontmatterImmutableRule::new(block.clone()),
        ));
    }
    for block in &config.rules.body_immutable {
        rules.push(Box::new(body_immutable::BodyImmutableRule::new(
            block.clone(),
        )));
    }
    for block in &config.rules.body_line {
        rules.push(Box::new(body_line::BodyLineRule::new(block.clone())));
    }
    // One rule per error-severity `[[detection.unresolved_policy]]`
    // row. The instances share one classification cell, so the cause
    // probes (stat-only, in-root) run once per check pass however many
    // error rows the project declares.
    for row in &config.detection.unresolved_policy {
        if row.severity == crate::config::UnresolvedSeverity::Error {
            rules.push(Box::new(
                unresolved_reference::UnresolvedReferenceRule::new(
                    row.clone(),
                    classification.clone(),
                ),
            ));
        }
    }
    // DAG cycle detection over the resolved edge graph. The relation
    // set is config-sourced (`rules.acyclic_relations`), validated
    // non-empty at load.
    rules.push(Box::new(graph_invariants::CycleDetectionRule::new(
        config.rules.acyclic_relations.clone(),
    )));
    rules
}

/// Test-only helper: build a [`RuleContext`] with a placeholder root.
/// Lives here so each rule's unit tests can construct a context
/// without redefining the same boilerplate.
#[cfg(test)]
pub(crate) fn test_ctx<'a>(graph: &'a Graph, config: &'a Config) -> RuleContext<'a> {
    RuleContext {
        graph,
        config,
        files: ProjectFiles::working_tree(Path::new(".")),
        repository: None,
        since: None,
        today: chrono::Local::now().date_naive(),
    }
}

/// Result of [`check`] — the fires (`violations`), the declined fires
/// (`skipped_rules`), and the reach of the fires that happened
/// (`rule_coverage`).
///
/// `skipped_rules` and `rule_coverage` partition the registry: a rule
/// either declined to run or ran, never both and never neither. Together
/// they answer "was this gate complete?", which `violations` alone cannot
/// — an empty violation list is what a thorough pass and a vacuous one
/// both look like. Surfacing all three is what `.claude/rules/config-driven.md`
/// asks for; a rule that passes over nothing is the silent-skip failure
/// mode wearing a green result.
#[derive(Debug, Clone, serde::Serialize, Default, JsonSchema)]
pub struct CheckReport {
    pub violations: Vec<Violation>,
    pub skipped_rules: Vec<SkippedRule>,
    pub rule_coverage: Vec<RuleCoverage>,
}

/// One pass of every registered rule against the supplied context.
/// Rules that report themselves non-applicable (e.g. diff-aware rules
/// invoked without `--since`) are surfaced in [`CheckReport::skipped_rules`]
/// with their reason — silent non-fires are forbidden under
/// `.claude/rules/config-driven.md`.
pub fn check(
    graph: &Graph,
    config: &Config,
    files: ProjectFiles<'_>,
    since: Option<&GraphDiff>,
    today: NaiveDate,
) -> CheckReport {
    run_rules(registered_rules(config), graph, config, files, since, today)
}

/// [`check`] with the unresolved-edge classification already computed.
/// `query issues` classifies unresolved edges for its own report; the
/// per-row `unresolved_reference` rules read the seeded cell instead of
/// re-running the same stat probes, so the probes run once per report
/// and the violations derive from exactly the edges the report lists.
/// The seeded vector must be the same-context classification
/// (`find_unresolved_edges(graph, config, files)`) the rules would
/// compute themselves.
pub(crate) fn check_with_unresolved(
    graph: &Graph,
    config: &Config,
    files: ProjectFiles<'_>,
    since: Option<&GraphDiff>,
    unresolved: Vec<crate::query::issues::UnresolvedEdge>,
    today: NaiveDate,
) -> CheckReport {
    let classification = unresolved_reference::SharedClassification::default();
    classification
        .set(unresolved)
        .expect("freshly constructed cell is empty");
    run_rules(
        rules_with_classification(config, classification),
        graph,
        config,
        files,
        since,
        today,
    )
}

/// One pass of the supplied rule set — the shared body of [`check`]
/// and [`check_with_unresolved`], so the two can never diverge in
/// applicability handling or report ordering.
/// Run exactly `rules` — the seam for a caller that needs a subset rather
/// than the registry. [`check`] is the whole-registry form.
pub(crate) fn run_rules(
    rules: Vec<Box<dyn Rule>>,
    graph: &Graph,
    config: &Config,
    files: ProjectFiles<'_>,
    since: Option<&GraphDiff>,
    today: NaiveDate,
) -> CheckReport {
    let ctx = RuleContext {
        graph,
        config,
        files,
        // `git_drift` is the one rule that measures git, and `preflight`
        // has already refused the run if its threshold is set without a
        // usable repository.
        repository: git_drift::drift_binding(config, files.root()),
        since,
        today,
    };

    let mut violations: Vec<Violation> = Vec::new();
    let mut skipped: Vec<SkippedRule> = Vec::new();
    let mut coverage: Vec<RuleCoverage> = Vec::new();
    for rule in &rules {
        if rule.is_applicable(&ctx) {
            let run = rule.check(&ctx);
            // A rule that found something looked at something. The reach and
            // the verdict are two readings of one pass, so a rule reporting
            // findings over an empty population has read the project in two
            // frames — the failure the census exists to expose, arriving in
            // the census itself. Cheap, and it holds for every rule: a
            // finding is always *about* a member of the guarded population,
            // however many findings one member draws.
            debug_assert!(
                run.violations.is_empty() || run.subjects > 0,
                "{} reported {} violation(s) over an empty subject set",
                rule.id(),
                run.violations.len()
            );
            coverage.push(RuleCoverage {
                rule_id: rule.id().to_string(),
                unit: rule.subject_unit(),
                subjects: run.subjects,
                unjudged: run.unjudged,
            });
            violations.extend(run.violations);
        } else {
            skipped.push(SkippedRule {
                rule_id: rule.id().to_string(),
                reason: rule.skip_reason(&ctx),
            });
        }
    }
    coverage.sort_by(|a, b| a.rule_id.cmp(&b.rule_id));

    violations.sort_by(|a, b| {
        a.rule_id
            .cmp(&b.rule_id)
            .then_with(|| a.node_id.cmp(&b.node_id))
    });
    skipped.sort_by(|a, b| a.rule_id.cmp(&b.rule_id));

    CheckReport {
        violations,
        skipped_rules: skipped,
        rule_coverage: coverage,
    }
}

/// What makes two findings *the same finding* rather than the same text —
/// the identity [`introduced_violations`] pairs on.
///
/// A finding is about a document, a rule, and a cause. A document's
/// identity is its node id, which travels with it: `rename` anchors a
/// path-derived id precisely so a move produces the same document
/// somewhere else rather than a new one. Its path is a *location*, so a
/// mutation that only moves it has introduced nothing — `check` says the
/// same sentence before and after — and pairing on the path would make
/// every move of an already-flagged document look like a fresh offence.
///
/// The path is left out entirely, including for the findings no node owns.
/// A project-wide finding carries a path only to point somewhere — the
/// cycle rule names its smallest member — and that pointer moves with a
/// rename the cycle itself is indifferent to. What identifies such a
/// finding is in `details`: for a cycle, the document caught in it; for a
/// parse failure, the document and the bytes it failed in.
///
/// `message` is left out because it is a rendered projection of `details`
/// ([`Violation::new`] builds it from nothing else), so weighing it would
/// only count the same fact twice — and it renders the evidence, which is
/// the part that moves.
///
/// `details` is compared as it stands. Nothing normalises it first: a field
/// that locates a finding rather than identifying it is declared
/// [`ViolationDetails`]-side as [`crate::rules::detail::Evidence`], which
/// equals every other, so the derived comparison is already this question.
fn finding_identity(v: &Violation) -> FindingIdentity {
    FindingIdentity {
        rule_id: v.rule_id.clone(),
        severity: v.severity,
        node_id: v.node_id.clone(),
        details: v.details.clone(),
    }
}

/// What [`introduced_violations`] pairs on, owned so the pairing can index
/// by it. Hashing agrees with equality by construction: every field derives
/// both, and `Evidence` — the one payload equality ignores — hashes to the
/// same constant for the same reason.
#[derive(PartialEq, Eq, Hash)]
struct FindingIdentity {
    rule_id: String,
    severity: Severity,
    node_id: Option<String>,
    details: ViolationDetails,
}

/// The violations `after` introduces over `before` — a **count-aware
/// multiset difference** by `finding_identity` (below). Each `before`
/// occurrence cancels at most one matching `after` occurrence, so a
/// proposal that adds a second instance of a pre-existing violation still
/// answers for the instance it introduced; plain set membership would let
/// the pre-existing copy absorb both. `after`'s order is preserved. This
/// is the single attribution substrate of every proposal gate
/// (`check --content` and the write plane's `mutate::introduced`): a
/// violation present in the before report never refuses a proposal, one
/// the overlay introduces always does.
pub fn introduced_violations(after: Vec<Violation>, before: &[Violation]) -> Vec<Violation> {
    let mut unmatched: HashMap<FindingIdentity, usize> = HashMap::new();
    for violation in before {
        *unmatched.entry(finding_identity(violation)).or_default() += 1;
    }
    after
        .into_iter()
        .filter(
            |violation| match unmatched.get_mut(&finding_identity(violation)) {
                Some(remaining) if *remaining > 0 => {
                    *remaining -= 1;
                    false
                }
                _ => true,
            },
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two identical reports cancel completely, at the size a real tangle
    /// reaches.
    ///
    /// The pairing scanned the unmatched list once per finding, so a write
    /// gate over a project whose cyclic region holds tens of thousands of
    /// documents paid O(before × after) per pass — a minute, twice, for a
    /// verdict of "nothing introduced". Indexing the identity makes it one
    /// pass over each side; if the scan ever comes back, this test takes
    /// that minute and says so.
    #[test]
    fn a_large_report_pairs_against_itself_in_one_pass() {
        let report = |n: usize| -> Vec<Violation> {
            (0..n)
                .map(|i| {
                    Violation::new(
                        "acyclic_relation",
                        Severity::Error,
                        None,
                        Some(format!("docs/n{i}.md")),
                        ViolationDetails::Cycle {
                            relation: "implements".into(),
                            member: format!("n{i}"),
                            region: detail::Evidence("n0".into()),
                            via: detail::Evidence(format!("n{}", i + 1)),
                        },
                    )
                })
                .collect()
        };
        let before = report(50_000);
        assert!(introduced_violations(report(50_000), &before).is_empty());
    }

    #[test]
    fn introduced_violations_is_a_count_aware_multiset_difference() {
        let v = Violation::new(
            "parse_failure",
            Severity::Error,
            None,
            Some("docs/a.md".to_string()),
            ViolationDetails::ParseFailure {
                path: "docs/a.md".to_string(),
                reason: Evidence("yaml".to_string()),
                content_digest: "abc123".to_string(),
            },
        );
        // One baseline occurrence cancels exactly one of two identical
        // after occurrences — the duplicate the overlay introduced
        // survives the difference.
        let delta = introduced_violations(vec![v.clone(), v.clone()], std::slice::from_ref(&v));
        assert_eq!(delta, vec![v.clone()]);

        // Identical sets cancel completely; an empty baseline cancels
        // nothing.
        assert!(introduced_violations(vec![v.clone()], std::slice::from_ref(&v)).is_empty());
        assert_eq!(introduced_violations(vec![v.clone()], &[]), vec![v]);
    }

    /// Every registered rule lands on exactly one of the two lists. A rule
    /// on neither is a rule that ran with nothing recorded about it, which
    /// is the state the census exists to make impossible.
    #[test]
    fn skips_and_coverage_partition_the_registry() {
        let config = Config::default();
        let graph = Graph::new(
            indexmap::IndexMap::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            crate::model::GraphMeta::default(),
        );
        let registry: Vec<String> = registered_rules(&config)
            .iter()
            .map(|r| r.id().to_string())
            .collect();

        let report = check(
            &graph,
            &config,
            ProjectFiles::working_tree(Path::new(".")),
            None,
            chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date"),
        );

        let mut accounted: Vec<String> = report
            .rule_coverage
            .iter()
            .map(|c| c.rule_id.clone())
            .chain(report.skipped_rules.iter().map(|s| s.rule_id.clone()))
            .collect();
        accounted.sort();
        let mut expected = registry;
        expected.sort();
        assert_eq!(accounted, expected);
    }

    /// The failure this exists for: a relation carrying no edges is a DAG
    /// vacuously, and the violation list of a run that examined nothing is
    /// the violation list of a run that examined everything. Only the reach
    /// tells them apart.
    #[test]
    fn a_rule_over_an_empty_subject_set_reports_zero_reach() {
        let config = Config::default();
        let graph = Graph::new(
            indexmap::IndexMap::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            crate::model::GraphMeta::default(),
        );
        let report = check(
            &graph,
            &config,
            ProjectFiles::working_tree(Path::new(".")),
            None,
            chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date"),
        );

        let cycle = report
            .rule_coverage
            .iter()
            .find(|c| c.rule_id == "acyclic_relation")
            .expect("acyclic_relation is always registered");
        assert_eq!(cycle.subjects, 0);
        assert_eq!(cycle.unit, SubjectUnit::Edges);
        assert!(
            report
                .violations
                .iter()
                .all(|v| v.rule_id != "acyclic_relation"),
            "the empty relation passes — which is exactly why the reach has to be readable"
        );
    }
}
