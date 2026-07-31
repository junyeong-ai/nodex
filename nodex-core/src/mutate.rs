//! The single guarded write seam for in-scope document mutations.
//!
//! Every batch command that rewrites existing files (`rename`,
//! `retarget`, `migrate --apply`) plans each file through [`plan_file`],
//! gates the whole batch through [`BaselineProbe::refusals`], and writes
//! the survivors through [`write_plan`] — so the "writer-skips /
//! reader-follows" symlink discipline, the immutability verdict, and the
//! atomic root-contained write each live in exactly one place. A future
//! rewrite command cannot forget the guards: it has nowhere else to write
//! through. Planning is separate from writing because the verdict is about
//! the whole batch, and a write that landed before it was answered could
//! not be taken back.
//!
//! [`introduced`] is what the whole batch answers for: the check
//! violations the project would carry after the proposal that it does not
//! carry now. It is asked by every write seam, including the ones that
//! write a single document, because the rules a mutation can break are the
//! whole registry rather than the family a seam happened to think of — a
//! reference this write leaves dangling, a cycle a repoint closes, a field
//! a status change leaves unsatisfied. [`BaselineProbe`] is the module's
//! third seam: every
//! mutation entry point ([`BaselineProbe::refusals`],
//! [`crate::lifecycle::transition`], [`crate::scaffold::scaffold`]) requires
//! one, so the `rules.immutable_baseline` activation logic is resolved once
//! per command instead of re-derived per handler.
//!
//! One mutation is routed around [`plan_file`] / [`write_plan`]: `rename`'s id
//! anchor writes the document's inferred id into its frontmatter *before*
//! `fs::rename`, because after the move the id it needs to preserve is already
//! gone. It is still gated — the bytes it produces are what the seam proposes
//! at the destination, so [`BaselineProbe::refusals`] judges them — and it
//! writes through [`crate::path_guard::write_atomic_in_root`], which refuses a
//! symlink target like every other write. What it rewrites is the whole
//! frontmatter block plus the file's line endings, since the anchor is decided
//! on the canonicalised text; the one field it introduces is `id`, which
//! `frontmatter_immutable` refuses to govern at load, so no lock is bypassed.

use std::path::{Path, PathBuf};

use crate::builder::scanner::{ProjectFiles, Proposed};
use crate::config::Config;
use crate::error::Result;
use crate::git::RefState;
use crate::path_guard;
use crate::warning::{Warning, WarningCode};

/// What `rules.immutable_baseline` resolved to for this run — the single
/// resolution behind both planes it governs: the diff a `check` runs
/// under, and the locks a write seam consults. A baseline that *cannot*
/// engage is a different fact from one that has nothing to govern, and
/// the operator must hear the first.
enum Binding {
    /// No baseline configured, or no immutability rules for it to feed.
    /// Nothing is locked and there is nothing to report.
    NotApplicable,
    /// A baseline is configured and immutability rules exist, but there
    /// is no snapshot to compare against — the project is not in a git
    /// work tree, or the ref does not carry the project. Nothing *can* be
    /// locked, and `reason` is why, so the advisory names the condition
    /// the operator can act on rather than the likeliest one.
    Inert { baseline: String, reason: String },
    /// The ref is a commit that carries the project, so a document the
    /// baseline graph has no node for is genuinely new.
    Bound {
        repository: crate::git::Repository,
        baseline: String,
    },
}

/// What `rules.immutable_baseline` names, before any snapshot of it is
/// taken. Cheap by construction: the config gate decides first, so a
/// project with no baseline — or none of the rules a baseline feeds —
/// never spawns a process, and a bound one costs only the repository
/// binding.
///
/// The activation is established up front rather than inferred from the
/// first document: a ref that carries nothing for this project looks
/// exactly like a document that is new, and a lock reading the first as
/// the second permits every write it exists to refuse. So the ref is
/// resolved once, and its three answers are kept apart: bound, nothing
/// to compare against, or — refused at resolution — unreadable.
///
/// [`snapshot`](Self::snapshot) turns a binding into the
/// [`BaselineProbe`] a write seam consults. A command that needs only the
/// refusal resolves a binding and drops it: `check --content` gates a
/// proposal against the working tree, never against a ref, so it pays for
/// resolving the ref — discovery and `ref_state` — and never for
/// materialising it. A project with no baseline bound at all pays nothing.
pub struct BaselineBinding {
    binding: Binding,
}

impl BaselineBinding {
    /// Bind `rules.immutable_baseline` for the project at `root`. Checks
    /// config before shelling git, so a project with no baseline (or no
    /// immutability rules to feed) never spawns a process.
    ///
    /// `Err` when the configured ref cannot be read at all — a name git
    /// does not resolve. Nothing about such a run is trustworthy: the
    /// rules can neither fire at check time nor be enforced at a write
    /// seam. Refusing at resolution rather than carrying the state means
    /// every consumer is refused identically, whether or not it would
    /// have reached a lock — a `retarget` with nothing to repoint cannot
    /// report success on a baseline a `check` rejects.
    pub fn resolve(root: &Path, config: &Config) -> Result<Self> {
        let Some(baseline) = config
            .rules
            .immutable_baseline
            .as_deref()
            .filter(|_| config.has_immutable_rules())
        else {
            return Ok(Self {
                binding: Binding::NotApplicable,
            });
        };
        let baseline = baseline.to_string();
        let repository = match crate::git::Repository::discover(root) {
            Ok(Some(repository)) => repository,
            Ok(None) => {
                return Ok(Self::inert(
                    baseline,
                    format!("no git work tree was found for {}", root.display()),
                ));
            }
            Err(e) => {
                return Ok(Self::inert(
                    baseline,
                    format!(
                        "the repository holding {} could not be resolved ({e})",
                        root.display()
                    ),
                ));
            }
        };
        let unreadable = |reason: String| {
            crate::error::Error::Config(format!(
                "rules.immutable_baseline {baseline:?} cannot be read ({reason}), so the \
                 immutability rules can neither fire nor be enforced; fix the ref or remove the \
                 baseline"
            ))
        };
        match repository.ref_state(&baseline) {
            Ok(RefState::CarriesProject) => Ok(Self {
                binding: Binding::Bound {
                    repository,
                    baseline,
                },
            }),
            Ok(RefState::WithoutProject) => {
                let reason = format!(
                    "it does not carry {:?}",
                    crate::path_guard::forward_string(repository.prefix())
                );
                Ok(Self::inert(baseline, reason))
            }
            Ok(RefState::Unborn) => Ok(Self::inert(
                baseline,
                "no ref in the repository names a commit, so there is nothing to compare against"
                    .to_string(),
            )),
            Ok(RefState::Unresolvable) => Err(unreadable("git resolves no such ref".to_string())),
            Err(e) => Err(unreadable(format!("it could not be resolved ({e})"))),
        }
    }

    fn inert(baseline: String, reason: String) -> Self {
        Self {
            binding: Binding::Inert { baseline, reason },
        }
    }

    /// Pair the binding with the baseline graph a `check` against it would
    /// diff — `build` is handed the bound repository and ref and returns
    /// that graph.
    ///
    /// The only way to obtain a [`BaselineProbe`], so a write seam cannot
    /// hold a bound baseline it has no snapshot of. Both planes then judge
    /// from one graph and pair documents the same way, by node id: a
    /// document that moved, or that the filesystem spells differently than
    /// the tree does, is the same document to both.
    pub fn snapshot(
        self,
        build: impl FnOnce(&crate::git::Repository, &str) -> Result<(crate::model::Graph, Vec<Warning>)>,
    ) -> Result<BaselineProbe> {
        let mut advisories: Vec<Warning> = self.advisory().into_iter().collect();
        let baseline = match &self.binding {
            Binding::Bound {
                repository,
                baseline,
            } => {
                let (graph, warnings) = build(repository, baseline)?;
                advisories.extend(warnings);
                Some(graph)
            }
            Binding::NotApplicable | Binding::Inert { .. } => None,
        };
        Ok(BaselineProbe {
            baseline,
            advisories,
        })
    }

    /// The resolved binding and the ref it names, for the read side's
    /// baseline materialisation. `None` when nothing is bound.
    pub fn bound(&self) -> Option<(&crate::git::Repository, &str)> {
        match &self.binding {
            Binding::Bound {
                repository,
                baseline,
            } => Some((repository, baseline)),
            Binding::NotApplicable | Binding::Inert { .. } => None,
        }
    }

    /// The advisory for a baseline that is configured but could not
    /// engage — the wording is constructed here and nowhere else, so a
    /// `check` and a mutation describe an unenforced lock identically.
    pub fn advisory(&self) -> Option<Warning> {
        let Binding::Inert { baseline, reason } = &self.binding else {
            return None;
        };
        Some(Warning::new(
            WarningCode::BaselineInert,
            format!(
                "rules.immutable_baseline {baseline:?} is set but {reason}; immutability rules \
                 are inert this run"
            ),
        ))
    }
}

/// The baseline a write seam judges against: the graph a `check` against
/// `rules.immutable_baseline` diffs, or nothing when no baseline governs
/// this run.
///
/// Having nothing to judge against and having nothing locked are one state
/// here, so a seam that finds no baseline node knows the document is new at
/// the baseline — and the read plane's diff reaches that same conclusion for
/// that same document, because it is the same graph.
/// [`advisories`](Self::advisories) carries the one wording for "the configured
/// locks did not engage", which a run must surface whether it read or wrote.
pub struct BaselineProbe {
    baseline: Option<crate::model::Graph>,
    advisories: Vec<Warning>,
}

impl BaselineProbe {
    /// Everything about this run's baseline that a caller must surface: the
    /// wording for configured locks that could not engage, and the baseline
    /// build's own warnings. A document that failed to parse at the baseline
    /// has no node there, so no lock guards it — the same silence the read
    /// plane reports, and a write must report it too.
    pub fn advisories(&self) -> &[Warning] {
        &self.advisories
    }

    /// The lock that freezes whatever record the baseline holds at `rel_path`,
    /// when one does.
    ///
    /// The one question the rules cannot be asked. Replacing a record with a
    /// *different* one — a `--force` overwrite, or re-creating a deleted
    /// document under a new id — is a removal plus an addition to `check`, and
    /// no rule consumes either, so there is nothing to run. What the baseline
    /// can still be asked is whether what stands there is frozen at all, which
    /// is a property of the baseline node alone: an armed lock of either
    /// family covering its kind — a `body_immutable` block, or, on a record
    /// already terminal there, a `frontmatter_immutable` block. Destroying
    /// such a record is the write to refuse regardless of what replaces it,
    /// and a project that froze only frontmatter has frozen records too.
    ///
    /// Addressed by path, deliberately: an overwrite shares no id to pair on.
    /// Whether a creation that *keeps* the record's id may proceed is a
    /// different question, and [`refusals`](Self::refusals) answers that one
    /// by asking the rules.
    pub fn frozen_at(&self, rel_path: &Path, config: &Config) -> Option<String> {
        let before = self.baseline.as_ref()?.node_by_path(rel_path)?;
        Self::frozen(before, config)
    }

    /// [`frozen_at`](Self::frozen_at) for a baseline node already in hand.
    fn frozen(before: &crate::model::Node, config: &Config) -> Option<String> {
        let body = config.rules.body_immutable.iter().find_map(|rule| {
            let armed = before.matches_kinds(&rule.kinds)
                && match rule.trigger {
                    // The baseline holds the record, so a creation lock is
                    // armed by its mere existence there.
                    crate::config::ImmutableTrigger::Creation => true,
                    crate::config::ImmutableTrigger::Terminal => {
                        config.is_terminal(before.status.as_str())
                    }
                };
            armed.then(|| format!("body_immutable/{}", rule.name))
        });
        body.or_else(|| {
            // `frontmatter_immutable` arms only on an already-terminal
            // record, exactly as the rule does.
            if !config.is_terminal(before.status.as_str()) {
                return None;
            }
            config
                .rules
                .frontmatter_immutable
                .iter()
                .find(|rule| before.matches_kinds(&rule.kinds))
                .map(|rule| format!("frontmatter_immutable/{}", rule.name))
        })
    }

    /// Which of `plans` this baseline's own rules refuse, and by which rule.
    ///
    /// The verdict is *computed by the rules themselves*, not re-derived from
    /// them. The whole project is built once with every plan overlaid, the
    /// rules run against this baseline, and a plan whose own path carries a
    /// violation in that proposed state is refused. Two properties follow
    /// that a per-document re-derivation cannot have:
    ///
    /// - Every rule a baseline feeds is enforced, whole. Not a chosen subset
    ///   of fields, and not one of a rule's two channels: a locked `status`
    ///   travels through `status_transitions` rather than `field_changes`,
    ///   and asking the rule instead of the diff reaches both.
    /// - A proposal is judged at the path it will occupy, so the fields
    ///   config derives from a path (`title` from the stem, `kind` from
    ///   `identity.kind_rules`) are the ones the next build will assign.
    ///
    /// The question is **absolute, not incremental**: does the baseline hold
    /// this record frozen in the state this write would leave it in. It is
    /// deliberately not the introduced-delta the `check --content` gate uses.
    /// A document that already drifted from a frozen baseline is still frozen
    /// history, and piling another edit onto it is the write the seam exists
    /// to refuse — `frozen history keeps its original reference` is the
    /// promise, not `this particular edit added nothing new`. Clearing the
    /// refusal means fixing the drift or superseding the record.
    ///
    /// Scope is the rules a baseline exists to feed — those whose
    /// `Rule::diff_aware` is true,
    /// which is the immutability families and nothing else. That is the scope
    /// a write seam promises; the wider "everything `check` reports" gate is
    /// `scaffold --body`'s, and it is a different promise.
    ///
    /// A rewrite of a document the proposed project does not contain — a
    /// `conditional_exclude` can evict one a batch still has to repoint — is
    /// not refused. `check` cannot flag a document outside the project, so
    /// there is nothing for the seam to be consistent with, and refusing on
    /// evidence no rule produced would be a refusal the operator cannot clear:
    /// `check` would report nothing to fix. Leaving the reference unrewritten
    /// is the concrete harm the batch exists to prevent.
    ///
    /// Costs one build, so it is asked once per command over every plan, and
    /// never per document. With no baseline bound it refuses nothing, because
    /// the rules it consults cannot fire at check time either.
    pub fn refusals(
        &self,
        root: &Path,
        config: &Config,
        proposal: &[(PathBuf, Proposed)],
        today: chrono::NaiveDate,
    ) -> Result<Refusals> {
        let Some(baseline) = &self.baseline else {
            return Ok(Refusals::default());
        };
        if proposal.is_empty() {
            return Ok(Refusals::default());
        }

        let gated: Vec<Box<dyn crate::rules::Rule>> = crate::rules::registered_rules(config)
            .into_iter()
            .filter(|rule| rule.diff_aware())
            .collect();
        if gated.is_empty() {
            return Ok(Refusals::default());
        }

        let proposed = crate::builder::build_with_overlay(root, config, proposal)?;
        let diff = crate::diff::compute_diff(baseline, &proposed.graph);
        let violations = crate::rules::run_rules(
            gated,
            &proposed.graph,
            config,
            ProjectFiles::proposed(root, proposal),
            Some(&diff),
            today,
        )
        .violations
        .into_iter()
        .filter(|v| v.severity == crate::rules::Severity::Error);

        let mut refusals = Refusals::default();
        for violation in violations {
            // Only a document this batch writes can be refused by it. A
            // violation elsewhere is one the project already carried and this
            // write neither causes nor compounds; `check` reports it, which is
            // where it belongs.
            let Some(rel_path) = violation.path.as_deref().and_then(|p| {
                proposal
                    .iter()
                    .map(|(rel_path, _)| rel_path)
                    .find(|rel_path| crate::path_guard::forward_string(rel_path) == p)
            }) else {
                continue;
            };
            refusals
                .by_path
                .entry(rel_path.clone())
                .or_insert(violation.rule_id);
        }

        // A path the proposal *empties* asks a question no rule answers: `check`
        // sees a removal, and nothing consumes one. Destroying a frozen record
        // is the write to refuse, so the baseline is asked directly.
        //
        // Destruction is the bytes ceasing to exist, which is what an `Absent`
        // entry says and the only thing that says it. A record that leaves the
        // *graph* has not necessarily left the project: `conditional_exclude`
        // drops a terminal parent's sub-artifacts from scope by design, with
        // their files intact, unmodified and committed. `check` reports nothing
        // for that — on either plane — so refusing it would be a refusal with
        // no reading to be consistent with, and, once the parent is terminal,
        // no command sequence could clear it.
        //
        // Emptying a path is not by itself destruction either. A move takes the
        // same record, under the same id, to another path in the same proposal,
        // and a record that still stands has not been destroyed — treating the
        // two alike would refuse every move of a frozen document, which is the
        // operation that exists to relocate one.
        for (rel_path, proposed_state) in proposal {
            if !matches!(proposed_state, Proposed::Absent) {
                continue;
            }
            let Some(before) = baseline.node_by_path(rel_path) else {
                continue;
            };
            if proposed.graph.node(&before.id).is_some() {
                continue;
            }
            if let Some(lock) = Self::frozen(before, config) {
                refusals.destroyed.entry(rel_path.clone()).or_insert(lock);
            }
        }

        Ok(refusals)
    }
}

/// A rewrite of one document, planned but not written. Planning is separate
/// from writing because the lock cannot be evaluated one document at a time:
/// the question is what the project looks like *after* the whole batch, which
/// only the whole batch answers.
#[derive(Debug, Clone)]
pub struct Planned {
    pub rel_path: PathBuf,
    pub content: String,
}

impl Planned {
    /// This plan as the proposal entry the gate judges.
    pub fn proposed(&self) -> (PathBuf, Proposed) {
        (
            self.rel_path.clone(),
            Proposed::Content(self.content.clone()),
        )
    }
}

/// What [`BaselineProbe::refusals`] found.
///
/// Two kinds, because they are answered differently. A per-path refusal is a
/// rule the proposed project carries at a path the batch writes: skipping that
/// one write clears it. A destruction is a frozen baseline record whose bytes
/// the proposal removes and which reappears nowhere under its id — the write
/// that would empty it cannot be skipped, because emptying that path *is* the
/// operation, so it refuses the command rather than one of its files.
#[derive(Debug, Default)]
pub struct Refusals {
    by_path: std::collections::BTreeMap<PathBuf, String>,
    destroyed: std::collections::BTreeMap<PathBuf, String>,
}

impl Refusals {
    /// The rule refusing this path, when one does.
    pub fn refusing(&self, rel_path: &Path) -> Option<&str> {
        self.by_path.get(rel_path).map(String::as_str)
    }

    /// A frozen baseline record this write would leave without a counterpart,
    /// and the lock that freezes it. Its path need not appear in the proposal:
    /// a record can be evicted by the project the write produces.
    pub fn destroyed(&self) -> Option<(&Path, &str)> {
        self.destroyed
            .iter()
            .next()
            .map(|(path, lock)| (path.as_path(), lock.as_str()))
    }
}

/// What planning a rewrite of one document produced.
pub enum PlanOutcome {
    /// New content, ready to be gated and then written.
    Planned(Planned),
    /// The transform produced no change; nothing will be written.
    Unchanged,
    /// Nothing will be written, with a warning explaining why — the path is
    /// or resolves through a symlink and the transform *would* have changed
    /// it (read through, never written through), or it is a real file that
    /// could not be read.
    Skipped(String),
}

/// Plan a rewrite of the file at `rel_path`: the write discipline that has
/// to hold before anything is even proposed, then the caller's transform.
///
/// A path that is — or resolves through — a symlink (the scanner
/// legitimately follows symlinked directories on read) is read through to
/// detect a pending change but never written through, since the target could
/// escape the project root: a pending change yields
/// [`PlanOutcome::Skipped`] carrying `symlink_message()`, never a batch
/// abort, so one refused file cannot strand a half-applied batch. An
/// *unreadable* such path is [`PlanOutcome::Unchanged`]: it cannot
/// demonstrate a pending change, would never receive a write either way, and
/// the build records an unreadable in-scope file as a typed parse failure
/// that reds `check`.
///
/// The immutability lock is deliberately *not* consulted here.
/// [`BaselineProbe::refusals`] answers it for a whole batch at once, because
/// the question is what the project looks like once every plan lands, and no
/// single document can answer that. So the order is: plan every file, gate
/// once, then [`write_plan`] the survivors.
pub fn plan_file(
    root: &Path,
    rel_path: &Path,
    transform: impl FnOnce(&str) -> Result<Option<String>>,
    symlink_message: impl FnOnce() -> String,
) -> Result<PlanOutcome> {
    let abs = root.join(rel_path);

    if path_guard::is_symlink(&abs) || path_guard::reject_outside_root(root, &abs).is_err() {
        if let Ok(content) = std::fs::read_to_string(&abs)
            && transform(&content)?.is_some()
        {
            return Ok(PlanOutcome::Skipped(symlink_message()));
        }
        return Ok(PlanOutcome::Unchanged);
    }

    let content = match std::fs::read_to_string(&abs) {
        Ok(c) => c,
        Err(e) => {
            return Ok(PlanOutcome::Skipped(format!(
                "could not read in-scope file {}: {e}",
                path_guard::forward_string(rel_path)
            )));
        }
    };

    Ok(match transform(&content)? {
        Some(content) => PlanOutcome::Planned(Planned {
            rel_path: rel_path.to_path_buf(),
            content,
        }),
        None => PlanOutcome::Unchanged,
    })
}

/// Write a plan the gate did not refuse — atomically, and inside the root.
pub fn write_plan(root: &Path, plan: &Planned) -> Result<()> {
    path_guard::write_atomic_in_root(root, &root.join(&plan.rel_path), &plan.content)
}

/// Which delta a proposal's rule pass runs under — the one input that
/// legitimately differs between write seams, named so the choice is
/// visible at every call site rather than inlined as an `Option`.
pub enum ProposalDiff {
    /// The diff-aware rules stay inert, exactly as they are in a `check`
    /// this project runs with no baseline. The seams that *transform*
    /// documents already on disk (`rename`, `retarget`, `lifecycle`) take
    /// this: their immutability verdict is the baseline-relative one
    /// [`BaselineProbe::refusals`] gives against the ref
    /// `rules.immutable_baseline` names, and a second, working-tree-relative
    /// opinion would refuse a write on a rule the same project's `check`
    /// reports as skipped.
    Inert,
    /// The proposal's own delta over the working tree, which activates the
    /// diff-aware rules against it. The seams that gate *authored* content
    /// (`scaffold`, mirrored on the read plane by `check --content`) take
    /// this: already-on-disk is the launder-safe boundary, so a frozen field
    /// cannot be edited by proposing over a working tree edited first.
    OverWorkingTree,
}

/// What a proposal introduces: the check violations the project would
/// carry after it that it does not carry now.
///
/// Both of a write seam's answers come from here — [`refusal`] for the
/// Error-severity findings that must stop the write, [`advisories`] for
/// the rest — so the severity line and the finding's rendering are drawn
/// once for the whole write plane.
///
/// [`refusal`]: Self::refusal
/// [`advisories`]: Self::advisories
pub struct Introduced {
    violations: Vec<crate::rules::Violation>,
}

impl Introduced {
    /// The refusal this proposal earns, or `None` when it introduces no
    /// Error-severity violation. Error severity is the line `check`'s exit
    /// code draws, so a seam that refuses exactly here cannot report
    /// success onto a project the next `check` fails.
    ///
    /// `subject` is what the seam was asked to do — "proposed content",
    /// "moving X to Y" — so one typed code reads as the command the
    /// operator ran.
    pub fn refusal(&self, subject: impl Into<String>) -> Option<crate::error::Error> {
        let findings: Vec<String> = self
            .violations
            .iter()
            .filter(|v| v.severity == crate::rules::Severity::Error)
            .map(Self::finding)
            .collect();
        (!findings.is_empty()).then(|| crate::error::Error::ContentViolations {
            subject: subject.into(),
            findings,
        })
    }

    /// Every finding as an envelope advisory — the same set
    /// [`refusal`](Self::refusal) reports, for a seam that chooses to advise
    /// rather than refuse (`scaffold`'s config-default placeholders, which
    /// are meant to be filled in).
    pub fn advisories(&self) -> Vec<Warning> {
        self.violations
            .iter()
            .map(|v| Warning::new(WarningCode::BuildRecommended, Self::finding(v)))
            .collect()
    }

    /// The one rendering of a finding: which document, the rule that fired,
    /// and what it said — so the seam's prose and `check`'s are the same
    /// words, and a refusal names the file to go and fix. A batch refusal is
    /// where that matters: three referrers the seam could not repoint yield
    /// three findings whose rule and message are identical, and only the
    /// document tells them apart. A finding no document owns (a cycle) says
    /// so by carrying neither.
    fn finding(violation: &crate::rules::Violation) -> String {
        let rule = &violation.rule_id;
        let message = &violation.message;
        match violation.path.as_deref().or(violation.node_id.as_deref()) {
            Some(subject) => format!("{subject}: {rule}: {message}"),
            None => format!("{rule}: {message}"),
        }
    }
}

/// The check violations `proposal` introduces over the project as it
/// stands — the write plane's one answer to "would `check` say something
/// after this mutation that it does not say now?".
///
/// Every seam that writes documents asks before it writes and refuses on
/// [`Introduced::refusal`], so a command cannot report success onto a
/// project its own `check` then fails, and cannot refuse a mutation that
/// `check` would pass. Attribution is
/// [`crate::rules::introduced_violations`]' count-aware multiset delta
/// against the pre-proposal report: a violation the project already
/// carried never refuses a mutation, one the proposal adds always does.
///
/// `before` is the graph the seam reasoned across, so the delta answers
/// for the project the operator is actually looking at. `diff` selects
/// which delta the diff-aware rules see ([`ProposalDiff`]).
pub fn introduced(
    root: &Path,
    config: &Config,
    before: &crate::model::Graph,
    proposal: &[(PathBuf, Proposed)],
    diff: ProposalDiff,
    today: chrono::NaiveDate,
) -> Result<Introduced> {
    let after = crate::builder::build_with_overlay(root, config, proposal)?;
    let since = match diff {
        ProposalDiff::Inert => None,
        ProposalDiff::OverWorkingTree => Some(crate::diff::compute_diff(before, &after.graph)),
    };
    Ok(Introduced {
        violations: crate::rules::introduced_violations(
            crate::rules::run_rules(
                gate_rules(config),
                &after.graph,
                config,
                ProjectFiles::proposed(root, proposal),
                since.as_ref(),
                today,
            )
            .violations,
            &crate::rules::run_rules(
                gate_rules(config),
                before,
                config,
                ProjectFiles::working_tree(root),
                None,
                today,
            )
            .violations,
        ),
    })
}

/// The rules a proposal gate runs: every Error-severity rule, and only
/// those.
///
/// Error is the line `check`'s exit code draws, so a Warning-severity rule
/// can never refuse a write — and `git_drift` shells git once per measured
/// edge, which is a price no write should pay for an answer it discards.
/// `BaselineProbe::refusals` narrows its registry for the same reason; the
/// difference is only which family each needs.
fn gate_rules(config: &Config) -> Vec<Box<dyn crate::rules::Rule>> {
    crate::rules::registered_rules(config)
        .into_iter()
        .filter(|rule| rule.severity() == crate::rules::Severity::Error)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Warning-severity rule can never refuse a write, and `git_drift`
    /// shells git once per measured edge — so a proposal gate running the
    /// whole registry would pay, twice per write, for answers it discards.
    /// (Measured: a rename over 600 covered documents with
    /// `git_drift_threshold` set took tens of seconds against 0.2s.)
    #[test]
    fn the_gate_runs_only_the_rules_that_can_refuse() {
        let mut config = Config::default();
        config.detection.git_drift_threshold = Some(1);
        config.rules.naming = vec![crate::config::NamingRuleConfig {
            glob: "docs/**".into(),
            pattern: "^[a-z]+\\.md$".into(),
            sequential: true,
            unique: false,
        }];
        let warning_rules: Vec<String> = crate::rules::registered_rules(&config)
            .iter()
            .filter(|rule| rule.severity() == crate::rules::Severity::Warning)
            .map(|rule| rule.id().to_string())
            .collect();
        assert!(
            warning_rules.iter().any(|id| id == "git_drift"),
            "the fixture must arm the rule the gate exists to drop: {warning_rules:?}"
        );
        assert!(
            gate_rules(&config)
                .iter()
                .all(|rule| rule.severity() == crate::rules::Severity::Error),
            "the gate runs a rule that cannot refuse"
        );
    }

    use crate::config::{BodyImmutableMode, BodyImmutableRuleConfig, ImmutableTrigger};
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    fn upcase_if_lower(content: &str) -> Result<Option<String>> {
        let up = content.to_uppercase();
        Ok((up != content).then_some(up))
    }

    /// An inert probe plus a default config — the no-lock harness for
    /// the symlink/read-discipline tests.
    fn no_lock() -> (Config, BaselineProbe) {
        let config = Config::default();
        let probe = BaselineProbe {
            baseline: None,
            advisories: Vec::new(),
        };
        (config, probe)
    }

    #[test]
    fn a_planned_rewrite_is_written_atomically() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.md"), "body").unwrap();

        let plan = match plan_file(dir.path(), Path::new("a.md"), upcase_if_lower, || {
            unreachable!("no symlink involved")
        })
        .unwrap()
        {
            PlanOutcome::Planned(plan) => plan,
            _ => panic!("a changed file must plan"),
        };
        assert_eq!(plan.content, "BODY");
        assert_eq!(
            fs::read_to_string(dir.path().join("a.md")).unwrap(),
            "body",
            "planning writes nothing"
        );

        write_plan(dir.path(), &plan).unwrap();
        assert_eq!(fs::read_to_string(dir.path().join("a.md")).unwrap(), "BODY");
    }

    #[test]
    fn a_no_op_transform_plans_nothing() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.md"), "ALREADY").unwrap();

        let outcome = plan_file(dir.path(), Path::new("a.md"), upcase_if_lower, || {
            unreachable!("no symlink involved")
        })
        .unwrap();

        assert!(matches!(outcome, PlanOutcome::Unchanged));
        assert_eq!(
            fs::read_to_string(dir.path().join("a.md")).unwrap(),
            "ALREADY"
        );
    }

    #[test]
    fn an_unreadable_file_is_skipped_with_a_warning_not_an_error() {
        let dir = TempDir::new().unwrap();

        let outcome = plan_file(dir.path(), Path::new("missing.md"), upcase_if_lower, || {
            unreachable!("not a symlink")
        })
        .unwrap();

        match outcome {
            PlanOutcome::Skipped(warning) => {
                assert!(warning.contains("missing.md"), "{warning}");
                assert!(warning.contains("could not read"), "{warning}");
            }
            _ => panic!("an unreadable in-scope file must be Skipped, never an error"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_with_a_pending_change_is_skipped_and_never_written() {
        use std::os::unix::fs as unix_fs;
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let external = outside.path().join("ext.md");
        fs::write(&external, "body").unwrap();
        unix_fs::symlink(&external, dir.path().join("link.md")).unwrap();

        let outcome = plan_file(dir.path(), Path::new("link.md"), upcase_if_lower, || {
            "symlink skipped".to_string()
        })
        .unwrap();

        match outcome {
            PlanOutcome::Skipped(warning) => assert_eq!(warning, "symlink skipped"),
            _ => panic!("a symlinked pending change must be Skipped"),
        }
        assert_eq!(
            fs::read_to_string(&external).unwrap(),
            "body",
            "the external target is never written through"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_file_under_a_symlinked_directory_is_skipped_not_aborted() {
        use std::os::unix::fs as unix_fs;
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        fs::create_dir_all(outside.path().join("docs")).unwrap();
        let external = outside.path().join("docs/ext.md");
        fs::write(&external, "body").unwrap();
        unix_fs::symlink(outside.path().join("docs"), dir.path().join("linked")).unwrap();

        let outcome = plan_file(
            dir.path(),
            Path::new("linked/ext.md"),
            upcase_if_lower,
            || "under a symlink".to_string(),
        )
        .unwrap();

        match outcome {
            PlanOutcome::Skipped(warning) => assert_eq!(warning, "under a symlink"),
            _ => panic!("a file reached through a symlinked directory must be Skipped"),
        }
        assert_eq!(fs::read_to_string(&external).unwrap(), "body");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_without_a_pending_change_is_silently_unchanged() {
        use std::os::unix::fs as unix_fs;
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let external = outside.path().join("ext.md");
        fs::write(&external, "ALREADY").unwrap();
        unix_fs::symlink(&external, dir.path().join("link.md")).unwrap();

        let outcome = plan_file(dir.path(), Path::new("link.md"), upcase_if_lower, || {
            unreachable!("no pending change, so nothing to warn about")
        })
        .unwrap();

        assert!(matches!(outcome, PlanOutcome::Unchanged));
    }

    /// A project on disk whose baseline is stated rather than committed, so
    /// the gate is exercised without a git repository: the baseline is a
    /// graph either way, and one built here cannot disagree with itself.
    fn gated_project(
        docs: &[(&str, &str)],
        configure: impl FnOnce(&mut Config),
    ) -> (TempDir, Config, BaselineProbe) {
        let dir = TempDir::new().unwrap();
        let mut config = Config::default();
        config.statuses.allowed = vec!["active".into(), "superseded".into()];
        config.statuses.terminal = vec!["superseded".into()];
        config.rules.immutable_baseline = Some("HEAD".into());
        configure(&mut config);

        let mut nodes = indexmap::IndexMap::new();
        for (rel, content) in docs {
            fs::write(dir.path().join(rel), content).unwrap();
            let node = crate::parser::parse_document(
                Path::new(rel),
                content,
                &crate::parser::ParseConfig::new(&config),
            )
            .expect("fixture parses")
            .node;
            nodes.insert(node.id.clone(), node);
        }
        let probe = BaselineProbe {
            baseline: Some(crate::model::Graph::new(
                nodes,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                crate::model::GraphMeta::default(),
            )),
            advisories: Vec::new(),
        };
        (dir, config, probe)
    }

    fn plan(rel: &str, content: &str) -> (PathBuf, Proposed) {
        (PathBuf::from(rel), Proposed::Content(content.to_string()))
    }

    fn today() -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()
    }

    /// A rewrite of a body the baseline froze is refused, and the refusal
    /// names the rule `check` would name.
    #[test]
    fn refusals_names_the_rule_a_check_would_report() {
        let frozen =
            "---\nid: a\ntitle: A\nkind: generic\nstatus: superseded\n---\n# A\n\nfrozen\n";
        let (dir, config, probe) = gated_project(&[("a.md", frozen)], |config| {
            config.rules.body_immutable = vec![BodyImmutableRuleConfig {
                name: "frozen".into(),
                mode: BodyImmutableMode::Frozen,
                trigger: ImmutableTrigger::Terminal,
                kinds: vec![],
            }];
        });

        let rewritten =
            "---\nid: a\ntitle: A\nkind: generic\nstatus: superseded\n---\n# A\n\nrewritten\n";
        let refusals = probe
            .refusals(dir.path(), &config, &[plan("a.md", rewritten)], today())
            .unwrap();

        assert_eq!(
            refusals.refusing(Path::new("a.md")),
            Some("body_immutable/frozen")
        );
    }

    /// A record that already drifted from a frozen baseline is still frozen
    /// history: the question is whether the baseline holds it frozen in the
    /// state the write would leave it in, not whether this particular edit
    /// added something new. So a document already violating its lock is
    /// refused rather than written to again — clearing it means fixing the
    /// drift or superseding the record.
    #[test]
    fn refusals_refuses_writing_to_a_record_already_drifted_from_its_lock() {
        let baseline = "---\nid: a\ntitle: A\nkind: generic\nstatus: superseded\nowner: alice\n---\n# A\n\nbody\n";
        let drifted = "---\nid: a\ntitle: A\nkind: generic\nstatus: superseded\nowner: bob\n---\n# A\n\nbody\n";
        let (dir, config, probe) = gated_project(&[("a.md", baseline)], |config| {
            config.rules.frontmatter_immutable =
                vec![crate::config::FrontmatterImmutableRuleConfig {
                    name: "owner-locked".into(),
                    fields: vec!["owner".into()],
                    kinds: vec![],
                }];
        });
        // The working tree already carries the drift the lock forbids.
        fs::write(dir.path().join("a.md"), drifted).unwrap();

        // A rewrite that leaves `owner` exactly as it already is.
        let repointed = drifted.replace("body", "body, repointed");
        let refusals = probe
            .refusals(dir.path(), &config, &[plan("a.md", &repointed)], today())
            .unwrap();

        assert_eq!(
            refusals.refusing(Path::new("a.md")),
            Some("frontmatter_immutable/owner-locked"),
            "a frozen record is not written to again just because it already drifted"
        );
    }

    /// A locked `status` reaches the rule through `status_transitions`, not
    /// `field_changes`. Asking the rule reaches both channels; asking one
    /// channel's diff reaches one.
    #[test]
    fn refusals_covers_a_locked_status() {
        let frozen = "---\nid: a\ntitle: A\nkind: generic\nstatus: superseded\n---\n# A\n\nbody\n";
        let (dir, config, probe) = gated_project(&[("a.md", frozen)], |config| {
            config.rules.frontmatter_immutable =
                vec![crate::config::FrontmatterImmutableRuleConfig {
                    name: "status-locked".into(),
                    fields: vec!["status".into()],
                    kinds: vec![],
                }];
        });

        let un_terminalised =
            "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n\nbody\n";
        let refusals = probe
            .refusals(
                dir.path(),
                &config,
                &[plan("a.md", un_terminalised)],
                today(),
            )
            .unwrap();

        assert_eq!(
            refusals.refusing(Path::new("a.md")),
            Some("frontmatter_immutable/status-locked")
        );
    }

    /// A project that froze only frontmatter has frozen records too. The path
    /// address is the only thing that can guard them against destruction — an
    /// overwrite landing a different id shares no join key, so no rule fires —
    /// and it used to consult both families. Asking only about bodies left a
    /// frontmatter-only project with no path protection at all.
    #[test]
    fn frozen_at_sees_a_record_frozen_only_by_its_frontmatter() {
        let terminal = "---\nid: a\ntitle: A\nkind: generic\nstatus: superseded\n---\n# A\n";
        let (dir, config, probe) = gated_project(&[("a.md", terminal)], |config| {
            config.rules.frontmatter_immutable =
                vec![crate::config::FrontmatterImmutableRuleConfig {
                    name: "owner-locked".into(),
                    fields: vec!["owner".into()],
                    kinds: vec![],
                }];
        });
        let _ = dir;
        assert_eq!(
            probe.frozen_at(Path::new("a.md"), &config).as_deref(),
            Some("frontmatter_immutable/owner-locked"),
            "a terminal record under a frontmatter lock is frozen history"
        );

        // The same record before it is terminal is not frozen: the rule arms on
        // the terminal boundary, and so does this.
        let active = "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n";
        let (dir2, config2, probe2) = gated_project(&[("a.md", active)], |config| {
            config.rules.frontmatter_immutable =
                vec![crate::config::FrontmatterImmutableRuleConfig {
                    name: "owner-locked".into(),
                    fields: vec!["owner".into()],
                    kinds: vec![],
                }];
        });
        let _ = dir2;
        assert_eq!(probe2.frozen_at(Path::new("a.md"), &config2), None);
    }

    /// With no baseline bound, the rules the gate consults cannot fire at
    /// check time either, so nothing is refused and no build is paid for.
    #[test]
    fn refusals_is_empty_without_a_baseline() {
        let (config, probe) = no_lock();
        let dir = TempDir::new().unwrap();
        let refusals = probe
            .refusals(dir.path(), &config, &[plan("a.md", "anything")], today())
            .unwrap();
        assert!(refusals.refusing(Path::new("a.md")).is_none());
    }
}
