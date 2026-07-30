//! The single guarded write seam for in-scope document mutations.
//!
//! Every batch command that rewrites existing files (`rename`,
//! `retarget`, `migrate --apply`) routes each file through
//! [`apply_to_file`], so the "writer-skips / reader-follows" symlink
//! discipline, the immutability lock probe, and the atomic,
//! root-contained write live in exactly one place. A future rewrite
//! command cannot forget the guards: it has nowhere else to write
//! through. [`BaselineProbe`] is the module's second seam: every
//! mutation entry point (`apply_to_file`, [`crate::lifecycle::transition`],
//! [`crate::scaffold::scaffold`]) requires one, so the
//! `rules.immutable_baseline` activation logic is resolved once per
//! command instead of re-derived per handler.

use std::path::{Path, PathBuf};

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
    /// The document this id names at the baseline, or `None` when the
    /// baseline has no such document — it is new — or when no baseline
    /// governs this run.
    pub fn baseline_node(&self, id: &str) -> Option<&crate::model::Node> {
        self.baseline.as_ref()?.node(id)
    }

    /// The document standing at this path at the baseline, or `None` when
    /// the baseline holds none there. Addressed by path for the one question
    /// that is about a location rather than a record: whether overwriting
    /// this file would destroy a frozen one.
    pub fn baseline_node_at(&self, rel_path: &Path) -> Option<&crate::model::Node> {
        self.baseline.as_ref()?.node_by_path(rel_path)
    }

    /// Everything about this run's baseline that a caller must surface: the
    /// wording for configured locks that could not engage, and the baseline
    /// build's own warnings. A document that failed to parse at the baseline
    /// has no node there, so no lock guards it — the same silence the read
    /// plane reports, and a write must report it too.
    pub fn advisories(&self) -> &[Warning] {
        &self.advisories
    }

    /// Which of `plans` this baseline's own rules refuse, and by which rule.
    ///
    /// The verdict is *computed the way `check` computes it*, not re-derived
    /// from it. The whole project is built once with every plan overlaid, the
    /// rules run against this baseline, and the answer is the **introduced**
    /// delta — the same count-aware multiset difference `check --content` and
    /// `scaffold` already gate on. Three properties follow that a
    /// per-document re-derivation cannot have:
    ///
    /// - Every rule a baseline feeds is enforced, whole. Not a chosen subset
    ///   of fields, and not one of a rule's two channels: a locked `status`
    ///   travels through `status_transitions` rather than `field_changes`,
    ///   and asking the rule instead of the diff reaches both.
    /// - A violation the project already carries never refuses a write that
    ///   did not cause it. One hand-edited locked field would otherwise
    ///   block every later rewrite of that document, and blame the rewrite.
    /// - A proposal is judged at the path it will occupy, so the fields
    ///   config derives from a path (`title` from the stem, `kind` from
    ///   `identity.kind_rules`) are the ones the next build will assign.
    ///
    /// Scope is the rules a baseline exists to feed — those whose
    /// `Rule::diff_aware` is true,
    /// which is the immutability families and nothing else. That is the scope
    /// a write seam promises; the wider "everything `check` reports" gate is
    /// `scaffold --body`'s, and it is a different promise.
    ///
    /// Costs two builds, so it is asked once per command over every plan, and
    /// never per document. With no baseline bound it refuses nothing, because
    /// the rules it consults cannot fire at check time either.
    pub fn refusals(
        &self,
        root: &Path,
        config: &Config,
        plans: &[Planned],
        today: chrono::NaiveDate,
    ) -> Result<Refusals> {
        let Some(baseline) = &self.baseline else {
            return Ok(Refusals::default());
        };
        if plans.is_empty() {
            return Ok(Refusals::default());
        }

        let gated: std::collections::BTreeSet<String> = crate::rules::registered_rules(config)
            .iter()
            .filter(|rule| rule.diff_aware())
            .map(|rule| rule.id().to_string())
            .collect();
        if gated.is_empty() {
            return Ok(Refusals::default());
        }

        let overlay: Vec<(PathBuf, String)> = plans
            .iter()
            .map(|p| (p.rel_path.clone(), p.content.clone()))
            .collect();
        let judge = |graph: &crate::model::Graph| {
            let diff = crate::diff::compute_diff(baseline, graph);
            crate::rules::check(graph, config, root, Some(&diff), today)
                .violations
                .into_iter()
                .filter(|v| gated.contains(&v.rule_id))
                .collect::<Vec<_>>()
        };

        let current = crate::builder::build_with_overlay(root, config, &[])?;
        let proposed = crate::builder::build_with_overlay(root, config, &overlay)?;
        let introduced =
            crate::rules::introduced_violations(judge(&proposed.graph), &judge(&current.graph));

        let mut refusals = Refusals::default();
        for violation in introduced {
            match violation.path.as_deref().and_then(|p| {
                plans
                    .iter()
                    .find(|plan| crate::path_guard::forward_string(&plan.rel_path) == p)
            }) {
                Some(plan) => {
                    refusals
                        .by_path
                        .entry(plan.rel_path.clone())
                        .or_insert(violation.rule_id);
                }
                // A refusal the batch caused but no plan owns. The
                // immutability families always attribute to the node whose
                // record changed, so this is unreachable through them —
                // carried rather than dropped, because a refusal nobody
                // surfaces is a write permitted by silence.
                None => refusals.unattributed.push(violation),
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

/// What [`BaselineProbe::refusals`] found: the rule refusing each path, and
/// any refusal the batch caused that no single plan owns.
#[derive(Debug, Default)]
pub struct Refusals {
    by_path: std::collections::BTreeMap<PathBuf, String>,
    unattributed: Vec<crate::rules::Violation>,
}

impl Refusals {
    /// The rule refusing this path, when one does.
    pub fn refusing(&self, rel_path: &Path) -> Option<&str> {
        self.by_path.get(rel_path).map(String::as_str)
    }

    /// Refusals the batch caused that no plan owns. A caller must surface
    /// these; ignoring one turns a refusal into a silent write.
    pub fn unattributed(&self) -> &[crate::rules::Violation] {
        &self.unattributed
    }
}

/// The two per-call immutability-lock parameters of [`apply_to_file`]:
/// where the document's baseline snapshot lives (`rename`'s moved file
/// reads its baseline at the old path) and whether locked id-relation
/// frontmatter fields engage the lock (`retarget` rewrites them).
pub struct RewriteLock<'a> {
    pub baseline_path: &'a Path,
    pub frontmatter_relations: bool,
}

/// Why [`apply_to_file`] declined to write a pending change. The
/// caller's `skip_message` closure renders each reason into its own
/// warning text, so one seam serves every command's distinct wording.
pub enum SkipReason {
    /// The path is — or resolves through — a symlink; writing through
    /// it could escape the project root.
    Symlink,
    /// The rewrite would introduce the named immutability violation
    /// (`rewrite_lock_reason`'s qualified rule id).
    Locked(String),
}

/// Outcome of applying a transform to one in-scope file.
pub enum FileOutcome {
    /// The transform produced new content and it was written atomically.
    Rewritten,
    /// The transform produced no change; the file was left untouched.
    Unchanged,
    /// The file was not written, with a warning explaining why — its
    /// path is or resolves through a symlink and the transform *would*
    /// have changed it (read through, never written through), the
    /// pending rewrite is frozen by an immutability lock, or it is a
    /// real file that could not be read.
    Skipped(String),
}

/// Apply `transform` to the file at `rel_path` under the project's
/// write guards, and report what happened. One read, one transform,
/// then the guards in order: write discipline first, lock second.
///
/// A path that is — or resolves through — a symlink (the scanner
/// legitimately follows symlinked directories on read) is read through
/// to detect a pending change but never written through, since the
/// target could escape the project root: a pending change yields
/// [`FileOutcome::Skipped`] carrying `skip_message(SkipReason::Symlink)`,
/// never a batch abort, so one refused file cannot strand a half-applied
/// batch. An *unreadable* such path is [`FileOutcome::Unchanged`]: it
/// cannot demonstrate a pending change, would never receive a write
/// either way, and the build records an unreadable in-scope file as a
/// typed parse failure that reds `check`.
///
/// A pending rewrite that [`rewrite_lock_reason`] would lock — judged
/// against the document's bytes at `probe`'s resolved baseline, read
/// from `lock.baseline_path` — yields `Skipped` with
/// `skip_message(SkipReason::Locked(rule_id))`: a rewrite the project's
/// own `check` would flag is never performed, exactly as a symlink is
/// never written through. With an inert probe the lock cannot engage.
///
/// A real, unlocked file is rewritten through
/// [`path_guard::write_atomic_in_root`] when the transform returns
/// `Some`, and left untouched on `None`; its read error is surfaced as
/// `Skipped`, never a hard failure.
///
/// [`rewrite_lock_reason`]: crate::rules::body_immutable::rewrite_lock_reason
pub fn apply_to_file(
    root: &Path,
    rel_path: &Path,
    config: &Config,
    probe: &BaselineProbe,
    lock: RewriteLock<'_>,
    transform: impl FnOnce(&str) -> Result<Option<String>>,
    skip_message: impl FnOnce(SkipReason) -> String,
) -> Result<FileOutcome> {
    let abs = root.join(rel_path);

    if path_guard::is_symlink(&abs) || path_guard::reject_outside_root(root, &abs).is_err() {
        if let Ok(content) = std::fs::read_to_string(&abs)
            && transform(&content)?.is_some()
        {
            return Ok(FileOutcome::Skipped(skip_message(SkipReason::Symlink)));
        }
        return Ok(FileOutcome::Unchanged);
    }

    let content = match std::fs::read_to_string(&abs) {
        Ok(c) => c,
        Err(e) => {
            return Ok(FileOutcome::Skipped(format!(
                "could not read in-scope file {}: {e}",
                path_guard::forward_string(rel_path)
            )));
        }
    };

    match transform(&content)? {
        Some(rewritten) => {
            if let Some(rule_id) = crate::rules::body_immutable::rewrite_lock_reason(
                &rewritten,
                lock.baseline_path,
                config,
                probe,
                lock.frontmatter_relations,
            )? {
                return Ok(FileOutcome::Skipped(skip_message(SkipReason::Locked(
                    rule_id,
                ))));
            }
            path_guard::write_atomic_in_root(root, &abs, &rewritten)?;
            Ok(FileOutcome::Rewritten)
        }
        None => Ok(FileOutcome::Unchanged),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn self_lock(rel: &Path) -> RewriteLock<'_> {
        RewriteLock {
            baseline_path: rel,
            frontmatter_relations: false,
        }
    }

    /// A throwaway git repo with one committed document and a config
    /// whose `immutable_baseline` + frozen body lock engage on it.
    /// A project whose baseline holds `doc`, and a probe that judges against
    /// it. The baseline is stated rather than committed and read back: it is
    /// a graph either way, and one built here cannot disagree with itself.
    fn locked_fixture(doc: &str) -> (TempDir, Config, BaselineProbe) {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.md"), doc).unwrap();

        let mut config = Config::default();
        config.statuses.terminal = vec!["superseded".into()];
        config.rules.immutable_baseline = Some("HEAD".into());
        config.rules.body_immutable = vec![BodyImmutableRuleConfig {
            name: "frozen".into(),
            mode: BodyImmutableMode::Frozen,
            trigger: ImmutableTrigger::Terminal,
            kinds: vec![],
        }];
        let probe = probe_against(doc, Path::new("a.md"), &config);
        (dir, config, probe)
    }

    /// A probe whose baseline is the single document `doc` sits at.
    fn probe_against(doc: &str, rel: &Path, config: &Config) -> BaselineProbe {
        let node = crate::rules::body_immutable::parse_for_probe(doc, rel, config)
            .expect("the fixture document parses");
        let mut nodes = indexmap::IndexMap::new();
        nodes.insert(node.id.clone(), node);
        BaselineProbe {
            baseline: Some(crate::model::Graph::new(
                nodes,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                crate::model::GraphMeta::default(),
            )),
            advisories: Vec::new(),
        }
    }

    #[test]
    fn rewrites_a_real_file_atomically() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.md"), "body").unwrap();
        let (config, probe) = no_lock();

        let outcome = apply_to_file(
            dir.path(),
            Path::new("a.md"),
            &config,
            &probe,
            self_lock(Path::new("a.md")),
            upcase_if_lower,
            |_| unreachable!("no symlink or lock involved"),
        )
        .unwrap();

        assert!(matches!(outcome, FileOutcome::Rewritten));
        assert_eq!(fs::read_to_string(dir.path().join("a.md")).unwrap(), "BODY");
    }

    #[test]
    fn leaves_file_untouched_when_transform_is_a_no_op() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.md"), "BODY").unwrap();
        let (config, probe) = no_lock();

        let outcome = apply_to_file(
            dir.path(),
            Path::new("a.md"),
            &config,
            &probe,
            self_lock(Path::new("a.md")),
            upcase_if_lower,
            |_| unreachable!("no symlink or lock involved"),
        )
        .unwrap();

        assert!(matches!(outcome, FileOutcome::Unchanged));
        assert_eq!(fs::read_to_string(dir.path().join("a.md")).unwrap(), "BODY");
    }

    #[test]
    fn unreadable_file_is_skipped_with_warning_not_error() {
        let dir = TempDir::new().unwrap();
        let (config, probe) = no_lock();
        let outcome = apply_to_file(
            dir.path(),
            Path::new("missing.md"),
            &config,
            &probe,
            self_lock(Path::new("missing.md")),
            upcase_if_lower,
            |_| unreachable!("no symlink or lock involved"),
        )
        .unwrap();

        match outcome {
            FileOutcome::Skipped(warning) => {
                assert!(warning.contains("could not read in-scope file missing.md"));
            }
            _ => panic!("a read failure must surface as Skipped"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_with_pending_change_is_skipped_with_warning_and_never_written() {
        use std::os::unix::fs as unix_fs;
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let target = outside.path().join("external.md");
        fs::write(&target, "body").unwrap();
        unix_fs::symlink(&target, dir.path().join("link.md")).unwrap();
        let (config, probe) = no_lock();

        let outcome = apply_to_file(
            dir.path(),
            Path::new("link.md"),
            &config,
            &probe,
            self_lock(Path::new("link.md")),
            upcase_if_lower,
            |reason| match reason {
                SkipReason::Symlink => "link.md skipped".to_string(),
                SkipReason::Locked(_) => unreachable!("no lock configured"),
            },
        )
        .unwrap();

        match outcome {
            FileOutcome::Skipped(warning) => assert_eq!(warning, "link.md skipped"),
            _ => panic!("a symlink the transform would rewrite must be Skipped"),
        }
        // Reader-follows, writer-skips: the external target is untouched.
        assert_eq!(fs::read_to_string(&target).unwrap(), "body");
    }

    #[cfg(unix)]
    #[test]
    fn file_under_symlinked_directory_is_skipped_not_aborted() {
        // The scanner follows symlinked directories on read, so an
        // in-scope file can resolve outside the root through a
        // symlinked ancestor. The seam must give it the same
        // reader-follows / writer-skips treatment as a file-level
        // symlink — a warning, never a batch-aborting error that could
        // strand a half-applied rename.
        use std::os::unix::fs as unix_fs;
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        fs::write(outside.path().join("external.md"), "body").unwrap();
        unix_fs::symlink(outside.path(), dir.path().join("linked")).unwrap();
        let (config, probe) = no_lock();

        let outcome = apply_to_file(
            dir.path(),
            Path::new("linked/external.md"),
            &config,
            &probe,
            self_lock(Path::new("linked/external.md")),
            upcase_if_lower,
            |reason| match reason {
                SkipReason::Symlink => "linked dir skipped".to_string(),
                SkipReason::Locked(_) => unreachable!("no lock configured"),
            },
        )
        .unwrap();

        match outcome {
            FileOutcome::Skipped(warning) => assert_eq!(warning, "linked dir skipped"),
            _ => panic!("a pending rewrite through a symlinked ancestor must be Skipped"),
        }
        assert_eq!(
            fs::read_to_string(outside.path().join("external.md")).unwrap(),
            "body",
            "the external target must never be written"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_without_pending_change_is_silently_unchanged() {
        use std::os::unix::fs as unix_fs;
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let target = outside.path().join("external.md");
        fs::write(&target, "BODY").unwrap();
        unix_fs::symlink(&target, dir.path().join("link.md")).unwrap();
        let (config, probe) = no_lock();

        let outcome = apply_to_file(
            dir.path(),
            Path::new("link.md"),
            &config,
            &probe,
            self_lock(Path::new("link.md")),
            upcase_if_lower,
            |_| unreachable!("a no-op transform never warns"),
        )
        .unwrap();

        assert!(matches!(outcome, FileOutcome::Unchanged));
    }

    // ─── BaselineProbe activation ──────────────────────────────────────

    #[test]
    fn baseline_probe_is_inert_without_config_baseline() {
        let dir = TempDir::new().unwrap();
        let mut config = Config::default();
        config.rules.body_immutable = vec![BodyImmutableRuleConfig {
            name: "frozen".into(),
            mode: BodyImmutableMode::Frozen,
            trigger: ImmutableTrigger::Terminal,
            kinds: vec![],
        }];
        let binding = BaselineBinding::resolve(dir.path(), &config).expect("a readable baseline");
        assert!(binding.bound().is_none(), "no baseline configured → inert");
        let probe = binding
            .snapshot(|_, _| unreachable!("nothing is bound, so nothing is built"))
            .expect("a binding with nothing bound needs no snapshot");
        assert!(probe.baseline_node("generic-a").is_none());
        assert!(
            probe.advisories().is_empty(),
            "no baseline was asked for, so nothing went unenforced"
        );
    }

    #[test]
    fn baseline_probe_is_inert_without_immutable_rules() {
        let dir = TempDir::new().unwrap();
        let mut config = Config::default();
        config.rules.immutable_baseline = Some("HEAD".into());
        let binding = BaselineBinding::resolve(dir.path(), &config).expect("a readable baseline");
        assert!(
            binding.bound().is_none(),
            "a baseline with no immutability rules to feed is inert"
        );
        let probe = binding
            .snapshot(|_, _| unreachable!("nothing is bound, so nothing is built"))
            .expect("a binding with nothing bound needs no snapshot");
        assert!(probe.baseline_node("generic-a").is_none());
        assert!(
            probe.advisories().is_empty(),
            "a baseline with no rules to feed leaves nothing unenforced"
        );
    }

    #[test]
    fn baseline_probe_is_inert_outside_work_tree() {
        // tempdir under /tmp is not a git work tree (and the repo's own
        // tree never contains it).
        let dir = TempDir::new().unwrap();
        let mut config = Config::default();
        config.rules.immutable_baseline = Some("HEAD".into());
        config.rules.body_immutable = vec![BodyImmutableRuleConfig {
            name: "frozen".into(),
            mode: BodyImmutableMode::Frozen,
            trigger: ImmutableTrigger::Terminal,
            kinds: vec![],
        }];
        let binding = BaselineBinding::resolve(dir.path(), &config).expect("a readable baseline");
        assert!(
            binding.bound().is_none(),
            "outside a git work tree the diff-aware rules are inert and so is the probe"
        );
        let probe = binding
            .snapshot(|_, _| unreachable!("nothing is bound, so nothing is built"))
            .expect("a binding with nothing bound needs no snapshot");
        assert!(probe.baseline_node("generic-a").is_none());
        // The locks the project asked for did not engage. A mutation that
        // proceeds without saying so is the silent-skip failure mode, so
        // the probe carries the advisory its consumers must surface.
        let advisory = probe
            .advisories()
            .first()
            .expect("a configured baseline went unenforced");
        assert_eq!(advisory.code, WarningCode::BaselineInert);
        assert!(
            advisory.message.contains("immutability rules are inert"),
            "{}",
            advisory.message
        );
    }

    // ─── immutability lock at the seam ─────────────────────────────────

    #[test]
    fn apply_to_file_skips_locked_pending_change_with_lock_reason() {
        let doc = "---\nid: a\ntitle: A\nstatus: superseded\n---\nfrozen body\n";
        let (dir, config, probe) = locked_fixture(doc);

        let rel = Path::new("a.md");
        let outcome = apply_to_file(
            dir.path(),
            rel,
            &config,
            &probe,
            self_lock(rel),
            |content| Ok(Some(content.replace("frozen body", "rewritten body"))),
            |reason| match reason {
                SkipReason::Locked(rule_id) => format!("a.md locked ({rule_id})"),
                SkipReason::Symlink => unreachable!("a.md is a real file"),
            },
        )
        .unwrap();

        match outcome {
            FileOutcome::Skipped(warning) => {
                assert_eq!(warning, "a.md locked (body_immutable/frozen)");
            }
            _ => panic!("a lock-frozen pending rewrite must be Skipped"),
        }
        assert_eq!(
            fs::read_to_string(dir.path().join("a.md")).unwrap(),
            doc,
            "nothing was written"
        );
    }

    #[cfg(unix)]
    #[test]
    fn apply_to_file_reports_symlink_reason_when_both_symlinked_and_locked() {
        // The seam checks the write discipline first: a path that is
        // both a symlink and lock-frozen reports the symlink skip — the
        // write would be refused for that reason before any lock probe
        // could run.
        use std::os::unix::fs as unix_fs;
        let doc = "---\nid: a\ntitle: A\nstatus: superseded\n---\nfrozen body\n";
        let (dir, config, probe) = locked_fixture(doc);

        let outside = TempDir::new().unwrap();
        fs::write(outside.path().join("ext.md"), doc).unwrap();
        unix_fs::symlink(outside.path().join("ext.md"), dir.path().join("link.md")).unwrap();

        let rel = Path::new("link.md");
        let outcome = apply_to_file(
            dir.path(),
            rel,
            &config,
            &probe,
            // The lock would engage if consulted — the baseline at a.md
            // is terminal and the body changes.
            RewriteLock {
                baseline_path: Path::new("a.md"),
                frontmatter_relations: false,
            },
            |content| Ok(Some(content.replace("frozen body", "rewritten body"))),
            |reason| match reason {
                SkipReason::Symlink => "symlink wins".to_string(),
                SkipReason::Locked(_) => panic!("write discipline is checked before the lock"),
            },
        )
        .unwrap();

        match outcome {
            FileOutcome::Skipped(warning) => assert_eq!(warning, "symlink wins"),
            _ => panic!("a symlinked pending rewrite must be Skipped"),
        }
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
            let node =
                crate::rules::body_immutable::parse_for_probe(content, Path::new(rel), &config)
                    .expect("fixture parses");
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

    fn plan(rel: &str, content: &str) -> Planned {
        Planned {
            rel_path: PathBuf::from(rel),
            content: content.to_string(),
        }
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
        assert!(refusals.unattributed().is_empty());
    }

    /// A locked field the document *already* differs from the baseline in is
    /// not this write's doing. Asking the rules for the introduced delta is
    /// what keeps one hand-edit from blocking every later rewrite — and from
    /// blaming the rewrite for a violation `check` reported before it.
    #[test]
    fn refusals_ignores_a_violation_the_project_already_carries() {
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
            None,
            "the pre-existing violation is not what this write introduced"
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
        assert!(refusals.unattributed().is_empty());
    }
}
