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

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::builder::scanner::{ProjectFiles, Proposed};
use crate::config::Config;
use crate::error::Result;
use crate::git::RefState;
use crate::path_guard;
use crate::rules::DocumentPart;
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

    /// [`frozen_at`](Self::frozen_at) narrowed to a record that would actually
    /// be lost: `None` when the frozen record's id still stands somewhere in
    /// `present`.
    ///
    /// A record travels under its id, not under its path — that is why
    /// `rename` anchors one. So a frozen record whose id moved on has left its
    /// path free, and refusing a write there would refuse a mutation `check`
    /// reads as nothing at all: the id is present before and after, and no
    /// rule mentions where it sits. The path-only reading is what
    /// [`destroyed`](Refusals::destroyed) already avoids on the other side of
    /// the same question, by asking whether the id survives the proposal.
    pub fn frozen_record_lost(
        &self,
        rel_path: &Path,
        present: &crate::model::Graph,
        config: &Config,
    ) -> Option<String> {
        let before = self.baseline.as_ref()?.node_by_path(rel_path)?;
        let lock = Self::frozen(before, config)?;
        present.node(&before.id).is_none().then_some(lock)
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
            crate::rules::Since::Baseline(&diff),
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
                .or_default()
                .locks
                .entry(violation.details.part())
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
///
/// A write that edits a document carries the document as it stands, so which
/// parts of it the write touches is read off the two texts rather than
/// declared by the seam that built them: a declared set is a second statement
/// about the same bytes and can disagree with them.
#[derive(Debug, Clone)]
pub struct Planned {
    pub rel_path: PathBuf,
    pub content: String,
    source: Source,
}

/// What a plan's content was composed from, which is what decides whether a
/// lock can cost it a part or must cost it the write.
#[derive(Debug, Clone)]
enum Source {
    /// The document as it stands, canonicalised.
    Edit(String),
    /// A document composed whole rather than edited — `rename` carrying a
    /// record to its destination. Holding back a part would leave bytes
    /// nobody authored, so a refusal costs the write.
    Composed,
}

impl Planned {
    /// A write that composes its document rather than editing one.
    pub fn composed(rel_path: PathBuf, content: String) -> Self {
        Self {
            rel_path,
            content,
            source: Source::Composed,
        }
    }

    /// This plan as the proposal entry the gate judges.
    pub fn proposed(&self) -> (PathBuf, Proposed) {
        (
            self.rel_path.clone(),
            Proposed::Content(self.content.clone()),
        )
    }

    /// This write with `held` left as the document already has it, or `None`
    /// when nothing of it is left to write.
    ///
    /// The result is the document the write started from with the parts it is
    /// allowed to write laid over it, so everything no part accounts for — the
    /// fence spelling, the key order, a comment or blank line no key owns — is
    /// the author's bytes and not this write's to normalise. Holding back
    /// everything therefore reproduces the document exactly, which is what
    /// makes "nothing left to write" a byte comparison.
    ///
    /// A document that carries no block has no shape to keep, and there the
    /// write's own is the only one there is.
    ///
    /// What a narrowed write may contribute is therefore closed, and closed by
    /// one rule: it writes only what a hold could have withheld. That is a
    /// key's lines — its own interior trivia included, so holding a field
    /// holds the comments inside it — the body, and a block around fields it
    /// lands in a document that had none. Key order, trivia no key owns, fence
    /// spelling, and the removal of a block the author fenced are all outside
    /// it: no [`DocumentPart`] names them, so a hold could not refuse them and
    /// a narrowing does not perform them. A seam that comes to write one of
    /// those needs a part of its own, not a different base — and a write
    /// nothing holds back is not narrowed at all, so it still lands whole.
    pub fn without(&self, held: &BTreeSet<DocumentPart>) -> Result<Option<Self>> {
        if held.is_empty() {
            return Ok(Some(self.clone()));
        }
        let Source::Edit(before) = &self.source else {
            return Ok(None);
        };
        let standing = self.framed(before)?;
        let proposed = self.framed(&self.content)?;
        let standing_yaml = self.editor(standing.yaml)?;
        let proposed_yaml = self.editor(proposed.yaml)?;
        // The block is composed on top of the one the document already
        // carries, so everything a field does not account for — the fence
        // spelling, the order of the keys, a comment or blank line no key
        // owns — is the author's rather than the write's. A document with no
        // block has no shape to keep, and there the write's own is the only
        // one there is.
        let shape = match standing.fenced() {
            true => &standing,
            false => &proposed,
        };
        let mut yaml = self.editor(shape.yaml)?;
        // The write's keys first, in the order it declared them: a key the
        // base already carries is spliced where it stands, so only a key new
        // to this write has a position to get right, and that position is the
        // one the write composed.
        let mut seen = BTreeSet::new();
        let keys: Vec<String> = proposed_yaml
            .keys()
            .chain(standing_yaml.keys())
            .filter(|key| seen.insert(key.to_string()))
            .map(str::to_string)
            .collect();
        for key in keys {
            let source = match held.contains(&DocumentPart::Field(key.clone())) {
                true => &standing_yaml,
                false => &proposed_yaml,
            };
            yaml.set_block(&key, source.block(&key).unwrap_or_default());
        }
        let body = match held.contains(&DocumentPart::Body) {
            true => standing.body,
            false => proposed.body,
        };
        // A block stands in the result when a field lands in it, or when the
        // document already carried one — an emptied block is still the block
        // the author fenced.
        //
        // Trivia is not a field, so a comment the write happened to compose is
        // no reason to open a block the document never had. And the author's
        // fence is not taken away here even by a write that removes it
        // outright: no `DocumentPart` names a block's presence, so a hold
        // could not have refused that removal, and a narrowing writes only
        // what a hold could have withheld.
        let blocked = yaml.keys().next().is_some() || standing.fenced();
        let (open, close) = match (blocked, standing.fenced()) {
            (false, _) => ("", ""),
            (true, true) => (standing.open, standing.close),
            (true, false) => (proposed.open, proposed.close),
        };
        let rendered = match blocked {
            true => yaml.render(),
            false => String::new(),
        };
        let content = format!("{open}{rendered}{close}{body}");
        Ok((&content != before).then(|| Self {
            rel_path: self.rel_path.clone(),
            content,
            source: self.source.clone(),
        }))
    }

    /// `content` in the pieces a write reassembles it from.
    fn framed<'a>(&self, content: &'a str) -> Result<Framed<'a>> {
        let (yaml, body) =
            crate::parser::frontmatter::split_frontmatter(content).map_err(|source| {
                crate::error::Error::Parse {
                    path: self.rel_path.clone(),
                    source,
                }
            })?;
        let Some(yaml) = yaml else {
            return Ok(Framed {
                open: "",
                yaml: None,
                close: "",
                body,
            });
        };
        // The block sits between two fence lines, and `split_frontmatter`
        // yields the YAML without the newline that ends it — so the closing
        // fence starts one byte past the block, and at the block itself when
        // it is empty.
        //
        // A trailing run of blank lines rides the close rather than the YAML.
        // The editor leaves a trailing trivia run unowned anyway, and the
        // stripped terminator means a block ending in a newline ends in an
        // *empty line* — which `str::lines` cannot represent, so the editor
        // would read one line fewer than the document has and render the
        // blank away.
        let head = &content[..content.len() - body.len()];
        let opened = head
            .find('\n')
            .expect("a split block opens on a fence line")
            + 1;
        let yaml = yaml.trim_end_matches('\n');
        let closed = opened + yaml.len() + usize::from(!yaml.is_empty());
        Ok(Framed {
            open: &head[..opened],
            yaml: Some(yaml),
            close: &head[closed..],
            body,
        })
    }

    fn editor(&self, yaml: Option<&str>) -> Result<crate::parser::editor::FrontmatterEditor> {
        crate::parser::editor::FrontmatterEditor::parse(yaml.unwrap_or_default(), &self.rel_path)
    }
}

/// A document in the pieces a write reassembles it from: the frontmatter
/// block's own delimiters and YAML, and the body. `open` / `close` carry the
/// fence lines exactly as the document spells them, through their newlines,
/// and are empty for a document that carries no block.
struct Framed<'a> {
    open: &'a str,
    yaml: Option<&'a str>,
    close: &'a str,
    body: &'a str,
}

impl Framed<'_> {
    fn fenced(&self) -> bool {
        self.yaml.is_some()
    }
}

/// What [`BaselineProbe::refusals`] found.
///
/// Two kinds, because they are answered differently. A per-path refusal is a
/// rule the proposed project carries at a path the batch writes: not writing
/// what that rule locks clears it. A destruction is a frozen baseline record
/// whose bytes the proposal removes and which reappears nowhere under its id —
/// the write that would empty it cannot be narrowed, because emptying that
/// path *is* the operation, so it refuses the command rather than one of its
/// files.
#[derive(Debug, Default)]
pub struct Refusals {
    by_path: std::collections::BTreeMap<PathBuf, Refusal>,
    destroyed: std::collections::BTreeMap<PathBuf, String>,
}

/// The rules refusing one path, keyed by the part of the document each is
/// about — `None` where the finding is about the document as a whole and no
/// smaller unit would be truthful.
#[derive(Debug, Default)]
pub struct Refusal {
    locks: std::collections::BTreeMap<Option<DocumentPart>, String>,
}

impl Refusals {
    /// The refusal standing at this path, when one does.
    pub fn refusing(&self, rel_path: &Path) -> Option<&Refusal> {
        self.by_path.get(rel_path)
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

impl Refusal {
    /// One rule to name when a write is held back.
    pub fn lock(&self) -> &str {
        self.locks
            .values()
            .next()
            .map(String::as_str)
            .expect("a refusal is recorded with its rule")
    }

    /// The parts this refusal names, each with the rule naming it. A
    /// document-wide finding names none, so a write carrying one is never
    /// narrowed and is held back whole.
    fn parts(&self) -> BTreeMap<DocumentPart, String> {
        self.locks
            .iter()
            .filter_map(|(part, lock)| Some((part.clone()?, lock.clone())))
            .collect()
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

    // Both halves of the plan are canonicalised, the transform's output as
    // much as the document it read: a plan is compared against its document
    // and split into parts, and one side arriving with CRLF would be read as
    // carrying no frontmatter at all. `transform` is this seam's public
    // surface, so the discipline every parser entry follows is applied here
    // rather than asked of each caller.
    let canonical = |text: &str| crate::parser::frontmatter::canonicalize(text).into_owned();
    Ok(match transform(&content)? {
        Some(planned) => PlanOutcome::Planned(Planned {
            rel_path: rel_path.to_path_buf(),
            content: canonical(&planned),
            source: Source::Edit(canonical(&content)),
        }),
        None => PlanOutcome::Unchanged,
    })
}

/// What a baseline let through of a batch, and what it kept back.
pub struct Narrowing {
    pub writable: Vec<Planned>,
    pub held: Vec<HeldBack>,
}

/// What a baseline's locks cost one plan: the rule refusing its path, and the
/// parts each lock names — empty where the finding is about the document and
/// no part of the write can be held back on its own.
struct Cost {
    lock: String,
    parts: BTreeMap<DocumentPart, String>,
}

/// A write a baseline did not let through whole.
pub struct HeldBack {
    pub rel_path: PathBuf,
    pub kept: Kept,
}

/// What a baseline kept back of one write.
pub enum Kept {
    /// The whole write, by this rule.
    Whole(String),
    /// These parts, each by the rule that names it, while the rest landed.
    /// Two locks can hold two parts of one document, so the rule travels with
    /// the part rather than beside them — an operator sent to the wrong rule
    /// has nothing to read.
    Parts(Vec<(DocumentPart, String)>),
}

/// Apply a baseline's locks to a batch: each plan loses the parts its
/// baseline refuses and writes the rest.
///
/// A lock names a part of a document, so that is what a refusal may cost.
/// Held back whole is what remains for a plan the baseline still refuses once
/// narrowed — a document already drifted from its frozen state, where the
/// finding is not this write's to clear, and one carrying a finding about the
/// document rather than a part of it.
///
/// The verdict is taken over the bytes that will land, never inferred from the
/// verdict on the bytes that will not: reverting a part to what the file
/// carries does not put it back the way the baseline has it, and only the
/// rules can say whether it did. That second pass is load-bearing rather than
/// belt-and-braces — a held part can move a *neighbouring* one's value, as a
/// keep-chomped block scalar's trailing blanks do when a field lands after
/// them — so it is not an optimisation to drop.
///
/// `base` is what the batch proposes besides these plans — the move overlay a
/// rename's rewrites are judged inside. Writes come back in the order they
/// were planned. Costs one build, and a second only when something was
/// refused.
pub fn narrow(
    probe: &BaselineProbe,
    root: &Path,
    config: &Config,
    base: &[(PathBuf, Proposed)],
    plans: Vec<Planned>,
    today: chrono::NaiveDate,
) -> Result<Narrowing> {
    let proposal = |plans: &[Planned]| -> Vec<(PathBuf, Proposed)> {
        let mut proposal: Vec<(PathBuf, Proposed)> = base.to_vec();
        for plan in plans {
            match proposal.iter_mut().find(|(path, _)| *path == plan.rel_path) {
                Some(entry) => entry.1 = Proposed::Content(plan.content.clone()),
                None => proposal.push(plan.proposed()),
            }
        }
        proposal
    };

    let refused = probe.refusals(root, config, &proposal(&plans), today)?;
    let planned: Vec<(Planned, Option<Cost>)> = plans
        .into_iter()
        .map(|plan| {
            let cost = refused.refusing(&plan.rel_path).map(|refusal| Cost {
                lock: refusal.lock().to_string(),
                parts: refusal.parts(),
            });
            (plan, cost)
        })
        .collect();
    let held_parts = |cost: Cost| Kept::Parts(cost.parts.into_iter().collect());
    if planned.iter().all(|(_, cost)| cost.is_none()) {
        return Ok(Narrowing {
            writable: planned.into_iter().map(|(plan, _)| plan).collect(),
            held: Vec::new(),
        });
    }

    // Every plan as the locks leave it, in the order the batch planned them.
    let narrowed: Vec<Option<Planned>> = planned
        .iter()
        .map(|(plan, cost)| match cost {
            None => Ok(Some(plan.clone())),
            Some(cost) => plan.without(&cost.parts.keys().cloned().collect()),
        })
        .collect::<Result<_>>()?;
    let landing: Vec<Planned> = narrowed.iter().flatten().cloned().collect();
    let standing = probe.refusals(root, config, &proposal(&landing), today)?;

    let mut writable = Vec::with_capacity(planned.len());
    let mut held = Vec::new();
    for ((plan, cost), narrowed) in planned.into_iter().zip(narrowed) {
        let Some(cost) = cost else {
            writable.extend(narrowed);
            continue;
        };
        match narrowed.filter(|_| standing.refusing(&plan.rel_path).is_none()) {
            Some(narrowed) => {
                writable.push(narrowed);
                if !cost.parts.is_empty() {
                    held.push(HeldBack {
                        rel_path: plan.rel_path,
                        kept: held_parts(cost),
                    });
                }
            }
            None => held.push(HeldBack {
                rel_path: plan.rel_path,
                kept: Kept::Whole(cost.lock),
            }),
        }
    }
    Ok(Narrowing { writable, held })
}

/// Write a plan the gate did not refuse — atomically, and inside the root.
pub fn write_plan(root: &Path, plan: &Planned) -> Result<()> {
    stage_plan(root, plan)?.commit()
}

/// [`write_plan`] stopped one step short, for a batch that has to land whole.
///
/// A gate judges the project a batch produces, and that judgement is worth
/// only as much as the batch's all-or-nothing-ness: a write that fails after
/// its siblings landed leaves a project nothing judged. Staging every plan
/// first moves every failure that actually happens to a point where nothing
/// has been replaced and the staged writes are simply dropped.
pub fn stage_plan(root: &Path, plan: &Planned) -> Result<path_guard::Staged> {
    path_guard::stage_in_root(root, &root.join(&plan.rel_path), &plan.content)
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
    evicted: Vec<Warning>,
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

    /// The findings this document does not own.
    ///
    /// A seam that writes a placeholder may advise about its own document —
    /// the fields left to fill in are the point of it. It may not advise
    /// about anything else: a reference this write stranded, or a number it
    /// duplicated with somebody, is nothing the operator is being invited to
    /// complete, and no shape of input makes it one.
    ///
    /// Ownership is the node id, never the path. A finding no node owns is
    /// owned by no document at all — a duplicated number is a conflict
    /// *between* documents, and the path it carries is the member that
    /// happened to sort first, so filtering on it made the verdict depend on
    /// a filename's alphabetical luck.
    ///
    /// An eviction is never filtered: it is a document the proposal drops
    /// *without naming it*, so it is somebody else's by construction, and no
    /// licence to skip advising about one's own placeholder reaches it.
    pub fn owned_by_others(&self, id: &str) -> Self {
        Self {
            violations: self
                .violations
                .iter()
                .filter(|v| v.node_id.as_deref() != Some(id))
                .cloned()
                .collect(),
            evicted: self.evicted.clone(),
        }
    }

    /// Everything about this proposal a seam must surface that is not a
    /// refusal: the findings [`refusal`](Self::refusal) reports, for a seam
    /// that chooses to advise rather than refuse (`scaffold`'s config-default
    /// placeholders, which are meant to be filled in), and the documents the
    /// proposal evicts, which no seam may refuse and none may drop.
    ///
    /// Every write seam calls this, which is why the eviction channel lives
    /// here rather than in an accessor of its own — a report a handler has to
    /// remember is one a handler can forget.
    pub fn advisories(&self) -> Vec<Warning> {
        self.violations
            .iter()
            .map(|v| Warning::new(WarningCode::BuildRecommended, Self::finding(v)))
            .chain(self.evicted.iter().cloned())
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
        evicted: evicted(before, &after, proposal),
        violations: crate::rules::introduced_violations(
            crate::rules::run_rules(
                gate_rules(config),
                &after.graph,
                config,
                ProjectFiles::proposed(root, proposal),
                since
                    .as_ref()
                    .map_or(crate::rules::Since::None, crate::rules::Since::Baseline),
                today,
            )
            .violations,
            &crate::rules::run_rules(
                gate_rules(config),
                before,
                config,
                ProjectFiles::working_tree(root),
                crate::rules::Since::None,
                today,
            )
            .violations,
        ),
    })
}

/// The documents a proposal drops from the project without naming them —
/// records whose files it leaves byte for byte, and which no rule speaks for
/// once it lands.
///
/// [`introduced`] cannot report these, and not by oversight: it is a delta of
/// findings over the population `check` runs on, so a document that leaves
/// that population takes its findings with it and the delta can only shrink.
/// The read plane answers the same blind spot with the reach a rule reports
/// ([`crate::rules::RuleRun::subjects`]); this is the write plane's half of
/// it, so a mutation answers for the project it produces and not only for
/// what that project's `check` would go on to say.
///
/// `scope.conditional_exclude` is the whole of it, and the set is read from
/// the scan's own record rather than inferred from a node gone missing: it is
/// the one membership rule a document's *content* moves, so it is the one way
/// a write evicts a record it never names. Every other way a node can vanish
/// is already accounted for — the rest of scope keys on paths, which only a
/// proposal changes; a duplicate id fails the build outright; and an
/// untouched file parses to what it parsed to before.
///
/// What the project holds is nodes *and* the documents it could not parse
/// — `Graph::parse_failures`, the same union `status` reports coverage over.
/// A record with no node is the one this matters most for: its
/// `parse_failure` is an Error the project's `check` is reporting right now,
/// and a write that drops the document drops the finding with it, turning a
/// red `check` green with nothing said.
///
/// A path the proposal itself names is never here. A deletion and a move are
/// what the operator asked for, and a move takes the record with it.
///
/// Advisory, never a refusal. Evicting a terminal parent's sub-artifacts is
/// what the rule was declared to do, and the seam's contract is to refuse
/// what a proposal *introduces* — a removal inverts that, and inverting it
/// for one rule would leave a project unable to terminalize a parent whose
/// sub-artifact is exactly what it is archiving away. What the advisory owes
/// instead is the consequence, named.
pub fn evicted(
    before: &crate::model::Graph,
    after: &crate::builder::BuildOutcome,
    proposal: &[(PathBuf, Proposed)],
) -> Vec<Warning> {
    if after.conditionally_excluded.is_empty() {
        return Vec::new();
    }
    let named: BTreeSet<String> = proposal
        .iter()
        .map(|(rel_path, _)| crate::path_guard::forward_string(rel_path))
        .collect();
    let held: BTreeMap<String, Option<&str>> = before
        .nodes()
        .values()
        .map(|node| {
            (
                crate::path_guard::forward_string(&node.path),
                Some(node.id.as_str()),
            )
        })
        .chain(
            before
                .parse_failures()
                .iter()
                .map(|failure| (failure.path.clone(), None)),
        )
        .collect();
    let remedy = "a [[scope.conditional_exclude]] rule drops the child_glob matches in a terminal \
                  parent's directory subtree — this file's own parent need not be the terminal \
                  one — so the file stays exactly as it is; give the records their own \
                  directories, or change the rule, to keep it graphed";
    after
        .conditionally_excluded
        .iter()
        .filter(|path| !named.contains(*path))
        .filter_map(|path| held.get(path).map(|id| (path, id)))
        .map(|(path, id)| {
            Warning::new(
                WarningCode::DocumentEvicted,
                match id {
                    Some(id) => format!(
                        "{path} (`{id}`) leaves the project with this write, and no rule guards \
                         it from here on — {remedy}"
                    ),
                    None => format!(
                        "{path} leaves the project with this write, and the `parse_failure` error \
                         `check` reports for it leaves with it — {remedy}"
                    ),
                },
            )
        })
        .collect()
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

    /// Plan a rewrite of one document through the seam, so what the plan
    /// holds about the document as it stands is what the seam read.
    fn planned_edit(before: &str, after: &str) -> (tempfile::TempDir, Planned) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let rel = Path::new("docs/a.md");
        std::fs::create_dir_all(tmp.path().join("docs")).expect("dir");
        std::fs::write(tmp.path().join(rel), before).expect("write");
        let outcome = plan_file(
            tmp.path(),
            rel,
            |_| Ok(Some(after.to_string())),
            || unreachable!("not a symlink"),
        )
        .expect("planned");
        match outcome {
            PlanOutcome::Planned(plan) => (tmp, plan),
            _ => panic!("the transform changed the document"),
        }
    }

    fn field(name: &str) -> BTreeSet<DocumentPart> {
        BTreeSet::from([DocumentPart::Field(name.to_string())])
    }

    #[test]
    fn holding_back_a_field_writes_the_rest_of_the_document() {
        let (_tmp, plan) = planned_edit(
            "---\nid: a\nowner: alice\nrelated: [old]\n---\n# A\n\nsee old\n",
            "---\nid: a\nowner: bob\nrelated: [new]\n---\n# A\n\nsee new\n",
        );
        let narrowed = plan
            .without(&field("owner"))
            .expect("narrowed")
            .expect("something is left to write");
        assert!(
            narrowed.content.contains("owner: alice"),
            "{:?}",
            narrowed.content
        );
        assert!(narrowed.content.contains("new"), "{:?}", narrowed.content);
        assert!(
            narrowed.content.ends_with("# A\n\nsee new\n"),
            "{:?}",
            narrowed.content
        );
    }

    #[test]
    fn holding_back_the_body_writes_the_frontmatter() {
        let (_tmp, plan) = planned_edit(
            "---\nid: a\nrelated: [old]\n---\n# A\n\nsee old\n",
            "---\nid: a\nrelated: [new]\n---\n# A\n\nsee new\n",
        );
        let narrowed = plan
            .without(&BTreeSet::from([DocumentPart::Body]))
            .expect("narrowed")
            .expect("something is left to write");
        assert_eq!(
            narrowed.content,
            "---\nid: a\nrelated: [new]\n---\n# A\n\nsee old\n"
        );
    }

    #[test]
    fn holding_back_every_part_leaves_nothing_to_write() {
        // The reconstruction is exact, which is what lets "nothing left" be a
        // byte comparison rather than a second opinion about what changed.
        let (_tmp, plan) = planned_edit(
            "---\nid: a\nowner: alice\n---\n# A\n\nsee old\n",
            "---\nid: a\nowner: bob\n---\n# A\n\nsee new\n",
        );
        let every = BTreeSet::from([DocumentPart::Body, DocumentPart::Field("owner".into())]);
        assert!(plan.without(&every).expect("narrowed").is_none());
    }

    #[test]
    fn holding_back_an_injected_field_keeps_the_order_of_the_rest() {
        // A document with no frontmatter gains one, minus the held field. The
        // block keeps the order the write composed it in, not an ordering the
        // narrowing invented.
        let (_tmp, plan) = planned_edit("# A\n", "---\ntitle: A\nid: a\nowner: alice\n---\n# A\n");
        let narrowed = plan
            .without(&field("owner"))
            .expect("narrowed")
            .expect("something is left to write");
        assert_eq!(narrowed.content, "---\ntitle: A\nid: a\n---\n# A\n");
    }

    #[test]
    fn holding_back_every_injected_field_leaves_the_bare_document() {
        let (_tmp, plan) = planned_edit("# A\n", "---\nid: a\n---\n# A\n");
        assert!(plan.without(&field("id")).expect("narrowed").is_none());
    }

    mod properties {
        use super::*;
        use proptest::prelude::*;

        /// A fence line as an author may spell one: `---` with any run of
        /// trailing spaces and tabs, which `is_fence_line` tolerates.
        fn fence() -> impl Strategy<Value = String> {
            proptest::string::string_regex("---[ \t]{0,3}").expect("literal regex")
        }

        /// Every top-level key a generated block can declare, so a property
        /// can hold all of them without reading the document back.
        const KEYS: &[&str] = &["id", "k", "b"];

        /// Every part of a document, so a property can hold back the whole of
        /// a write. A part neither document carries is a no-op to restore.
        fn every_part() -> BTreeSet<DocumentPart> {
            std::iter::once(DocumentPart::Body)
                .chain(KEYS.iter().map(|key| DocumentPart::Field(key.to_string())))
                .collect()
        }

        /// A line inside the block. None of them is a fence, so where the
        /// block ends is the generator's choice rather than an accident.
        ///
        /// The chomping indicators are here because they make a trailing
        /// blank run semantically significant rather than trivia, and the
        /// indented lines because a plain scalar folds across them — both are
        /// shapes where "which lines does this entry own" is not obvious from
        /// the key line alone.
        fn yaml_line() -> impl Strategy<Value = &'static str> {
            prop::sample::select(vec![
                "id: a",
                "id: z",
                "id: 값 🌿",
                "k: v",
                "k:",
                "",
                "   ",
                "  - x",
                "# comment",
                "b: |",
                "b: |+",
                "b: |-",
                "  folded",
                "-- x",
            ])
        }

        /// A body sitting after a closing fence, including one whose own
        /// first line looks like a fence — the split ends at the *first*
        /// close, so everything past it is body however it is spelled.
        fn body() -> impl Strategy<Value = &'static str> {
            prop_oneof![
                fenceless(),
                prop::sample::select(vec!["---\n# B\n", "---\n"]),
            ]
        }

        /// A whole document that opens no block. It must not open a fence
        /// either: a fence that never closes is the error path, which the
        /// reassembly identity says nothing about.
        fn fenceless() -> impl Strategy<Value = &'static str> {
            prop::sample::select(vec!["", "# B\n", "x\n\ny\n", "\n", "🌿\n", "-- x\n"])
        }

        /// A whole document, across the spellings the splitter accepts:
        /// fenceless, fences carrying trailing whitespace, a block of any
        /// shape including empty and blank-terminated, and a close terminated
        /// by a newline or by EOF.
        fn document() -> impl Strategy<Value = String> {
            let fenced = (
                fence(),
                prop::collection::vec(yaml_line(), 0..5),
                fence(),
                // A close terminated by EOF ends the document, so it pairs
                // only with an empty body — anything after it would join the
                // fence line and stop it being one.
                prop_oneof![body().prop_map(|body| ("\n", body)), Just(("", "")),],
            )
                .prop_map(|(open, lines, close, (terminator, body))| {
                    let block: String = lines.iter().map(|line| format!("{line}\n")).collect();
                    format!("{open}\n{block}{close}{terminator}{body}")
                });
            prop_oneof![
                4 => fenced,
                1 => fenceless().prop_map(str::to_string),
            ]
        }

        proptest! {
            /// Framing a document and putting it back through the editor —
            /// the composition `without` performs, with nothing held and
            /// nothing edited — reproduces it byte for byte.
            ///
            /// This is the invariant the whole narrowing rests on: it is what
            /// makes "nothing left to write" a byte comparison, and what keeps
            /// a write that changes only a held part from touching a frozen
            /// document at all. Three separate defects reached that path
            /// through a shape no hand-written table held, so the shapes are
            /// generated rather than listed.
            #[test]
            fn framing_a_document_and_reassembling_it_is_the_identity(content in document()) {
                let plan = Planned::composed(PathBuf::from("docs/a.md"), String::new());
                let framed = plan.framed(&content).expect("a generated document splits");
                // A generated block may repeat a key, which the editor refuses
                // by design and `without` propagates as a typed parse error.
                let Ok(editor) = plan.editor(framed.yaml) else {
                    return Ok(());
                };
                prop_assert_eq!(
                    format!(
                        "{}{}{}{}",
                        framed.open,
                        editor.render(),
                        framed.close,
                        framed.body
                    ),
                    content
                );
            }

            /// Holding back the whole of a write leaves nothing to write, for
            /// any two documents.
            ///
            /// This is the identity above composed with the operation it
            /// leaves out: restoring a part splices a block torn from one
            /// document's extents into another at that key's position there,
            /// and the two documents are generated independently so the
            /// splice is quantified rather than sampled. Reproducing the
            /// document exactly is what `None` means, so the assertion is the
            /// guarantee.
            #[test]
            fn holding_back_a_whole_write_leaves_nothing_to_write(
                standing in document(),
                proposed in document(),
            ) {
                let (_tmp, plan) = planned_edit(&standing, &proposed);
                // A generated block may repeat a key, which the editor refuses
                // by design and `without` propagates as a typed parse error.
                let Ok(narrowed) = plan.without(&every_part()) else {
                    return Ok(());
                };
                prop_assert!(
                    narrowed.is_none(),
                    "left: {:?}",
                    narrowed.map(|plan| plan.content)
                );
            }
        }
    }

    /// The document's own delimiters survive a narrowing, whatever shape the
    /// author fenced it in. A write that changes nothing but a held part must
    /// come back as "nothing left to write" — and it only can if the
    /// reassembly is byte-exact.
    #[test]
    fn narrowing_reproduces_the_documents_own_frontmatter_shape() {
        for before in [
            "---\n---\n# A\n\nsee old\n",
            "--- \nid: a\n---\t\n# A\n\nsee old\n",
            "---\n\n\n---\n# A\n\nsee old\n",
            "---\nid: a\n\n---\n# A\n\nsee old\n",
            "---\nid: a\n   \n---\n# A\n\nsee old\n",
            "---\nid: a\n---\n# A\n\nsee old\n",
            "# A\n\nsee old\n",
        ] {
            let after = before.replace("see old", "see new");
            let (_tmp, plan) = planned_edit(before, &after);
            assert!(
                plan.without(&BTreeSet::from([DocumentPart::Body]))
                    .expect("narrowed")
                    .is_none(),
                "holding the only changed part must leave nothing to write: {before:?}"
            );
        }
    }

    #[test]
    fn narrowing_keeps_an_empty_block_the_author_fenced() {
        // The body is held and a field lands, so the write does go out — and
        // the block it lands in is the one the document already had, empty and
        // fenced exactly as written.
        let (_tmp, plan) = planned_edit(
            "--- \n---\n# A\n\nsee old\n",
            "--- \nowner: bob\n---\n# A\n\nsee new\n",
        );
        assert_eq!(
            plan.without(&BTreeSet::from([DocumentPart::Body]))
                .expect("narrowed")
                .expect("the field still lands")
                .content,
            "--- \nowner: bob\n---\n# A\n\nsee old\n"
        );
    }

    #[test]
    fn trivia_the_write_composed_does_not_open_a_block() {
        // The write's block carries a line no key owns. Nothing the parts
        // model names is in it, so a document that had no block does not gain
        // one — otherwise holding everything would still have left a block
        // behind on a document that never had one.
        let (_tmp, plan) = planned_edit("# A\n", "---\n   \n---\n# A\n");
        assert!(
            plan.without(&BTreeSet::from([DocumentPart::Body]))
                .expect("narrowed")
                .is_none()
        );
        // A field in it, and the write's block lands around the field.
        let (_tmp, plan) = planned_edit("# A\n", "---\n   \nid: a\n---\n# A\n");
        assert_eq!(
            plan.without(&BTreeSet::from([DocumentPart::Body]))
                .expect("narrowed")
                .expect("the field lands")
                .content,
            "---\n   \nid: a\n---\n# A\n"
        );
    }

    #[test]
    fn a_narrowed_write_does_not_take_away_the_block_the_author_fenced() {
        // The write removes the frontmatter outright and only the body is
        // held. Its per-field removals land — neither field is held — but the
        // fence stays: no part names a block's presence, so a hold could not
        // have refused that removal, and a narrowing writes only what a hold
        // could have withheld. Held back by nothing, the same write is not
        // narrowed and removes it.
        let (_tmp, plan) = planned_edit(
            "---\nid: a\nowner: alice\n---\n# A\n\nsee old\n",
            "# A\n\nsee new\n",
        );
        assert_eq!(
            plan.without(&BTreeSet::from([DocumentPart::Body]))
                .expect("narrowed")
                .expect("the field removals land")
                .content,
            "---\n---\n# A\n\nsee old\n"
        );
        assert_eq!(
            plan.without(&BTreeSet::new())
                .expect("narrowed")
                .expect("nothing held")
                .content,
            "# A\n\nsee new\n"
        );
    }

    #[test]
    fn fields_new_to_a_write_land_in_the_order_it_composed_them() {
        // A key the document already carries is spliced where it stands, so
        // only a new key has a position to get right — and it is the write's,
        // not whatever order the key names happen to sort in. `title` belongs
        // second, after `id`, which is where the write put it.
        let (_tmp, plan) = planned_edit(
            "---\nowner: alice\n---\n# A\n",
            "---\nowner: bob\nid: xyz\ntitle: Doc\nkind: generic\nstatus: active\n---\n# A\n",
        );
        assert_eq!(
            plan.without(&field("owner"))
                .expect("narrowed")
                .expect("the injected fields land")
                .content,
            "---\nowner: alice\nid: xyz\ntitle: Doc\nkind: generic\nstatus: active\n---\n# A\n"
        );
    }

    #[test]
    fn a_plan_is_canonical_on_both_sides() {
        // `transform` is this seam's public surface. Non-canonical output
        // would be read as carrying no frontmatter, and a body-only narrowing
        // would then reassemble the document without its fields.
        let (_tmp, plan) = planned_edit(
            "---\nid: a\nowner: alice\n---\n# A\n\nsee old\n",
            "---\r\nid: a\r\nowner: bob\r\n---\r\n# A\r\n\r\nsee new\r\n",
        );
        assert_eq!(
            plan.without(&BTreeSet::from([DocumentPart::Body]))
                .expect("narrowed")
                .expect("the field still lands")
                .content,
            "---\nid: a\nowner: bob\n---\n# A\n\nsee old\n"
        );
    }

    #[test]
    fn a_write_that_removes_a_held_field_keeps_it_where_it_stood() {
        // Composing on top of the document the write started from means a
        // field the write dropped is simply never dropped — it keeps the line
        // it stood on, and the rest of the write still lands.
        let (_tmp, plan) = planned_edit(
            "---\nid: a\nrelated:\n  - old\nowner: alice\n---\n# A\n",
            "---\nid: a\nowner: bob\n---\n# A\n",
        );
        assert_eq!(
            plan.without(&field("related"))
                .expect("narrowed")
                .expect("the rest of the write lands")
                .content,
            "---\nid: a\nrelated:\n  - old\nowner: bob\n---\n# A\n"
        );
        // A field the write only *edits* narrows in place the same way, and
        // the field it dropped stays dropped.
        assert_eq!(
            plan.without(&field("owner"))
                .expect("narrowed")
                .expect("something is left to write")
                .content,
            "---\nid: a\nowner: alice\n---\n# A\n"
        );
    }

    #[test]
    fn a_composed_write_has_no_part_to_hold_back() {
        // `rename` carries a record to its destination; a part held back there
        // would leave bytes nobody authored.
        let plan = Planned::composed(
            PathBuf::from("docs/a.md"),
            "---\nid: a\n---\n# A\n".to_string(),
        );
        assert!(plan.without(&field("id")).expect("narrowed").is_none());
        assert!(plan.without(&BTreeSet::new()).expect("narrowed").is_some());
    }

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

    /// A project whose terminal `specs/*/spec.md` drops its siblings, built
    /// from `files` — the one config shape an eviction is reachable under.
    fn sub_artifact_project(files: &[(&str, &str)]) -> (TempDir, Config) {
        let dir = TempDir::new().unwrap();
        for (rel_path, content) in files {
            let abs = dir.path().join(rel_path);
            fs::create_dir_all(abs.parent().unwrap()).unwrap();
            fs::write(abs, content).unwrap();
        }
        let mut config = Config::default();
        config.scope.conditional_exclude = vec![crate::config::ConditionalExclude {
            parent_glob: "specs/*/spec.md".into(),
            child_glob: "specs/*/*.md".into(),
            condition: "status_terminal".into(),
            may_be_empty: false,
        }];
        config.validate().unwrap();
        (dir, config)
    }

    fn spec(status: &str) -> String {
        format!("---\nid: spec-a\ntitle: A\nkind: generic\nstatus: {status}\n---\n# A\n")
    }

    const PLAN: &str = "---\nid: plan-a\ntitle: Plan\nkind: generic\nstatus: active\n---\n# Plan\n";

    fn evicted_for(
        dir: &TempDir,
        config: &Config,
        proposal: &[(PathBuf, Proposed)],
    ) -> Vec<Warning> {
        let before = crate::builder::build_with_overlay(dir.path(), config, &[])
            .unwrap()
            .graph;
        let after = crate::builder::build_with_overlay(dir.path(), config, proposal).unwrap();
        evicted(&before, &after, proposal)
    }

    /// The write that makes a parent terminal is the write that drops its
    /// sub-artifacts, and it is the only place that fact exists to be
    /// reported: the document leaves the population `check` runs on, taking
    /// its findings with it, so the introduced-violation delta is silent by
    /// construction.
    #[test]
    fn a_status_change_names_the_sub_artifacts_it_evicts() {
        let (dir, config) = sub_artifact_project(&[
            ("specs/a/spec.md", &spec("active")),
            ("specs/a/plan.md", PLAN),
        ]);

        let warnings = evicted_for(
            &dir,
            &config,
            &[(
                PathBuf::from("specs/a/spec.md"),
                Proposed::Content(spec("superseded")),
            )],
        );

        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(warnings[0].code, WarningCode::DocumentEvicted);
        assert!(
            warnings[0].message.contains("specs/a/plan.md")
                && warnings[0].message.contains("plan-a"),
            "the advisory must name the document that left: {}",
            warnings[0].message
        );
    }

    /// The record with no node is the one this matters most for. Its
    /// `parse_failure` is an Error the project's `check` reports right now, so
    /// a write that drops the document takes the finding with it and turns a
    /// red `check` green — the exact silence the channel exists to end, and
    /// invisible to a population read as nodes alone.
    #[test]
    fn a_record_the_project_holds_as_a_parse_failure_is_evicted_out_loud() {
        let (dir, config) = sub_artifact_project(&[
            ("specs/a/spec.md", &spec("active")),
            ("specs/a/plan.md", "---\n  title: [unclosed\n---\nBroken.\n"),
        ]);
        let before = crate::builder::build_with_overlay(dir.path(), &config, &[])
            .unwrap()
            .graph;
        assert!(
            before.node_by_path(Path::new("specs/a/plan.md")).is_none()
                && before
                    .parse_failures()
                    .iter()
                    .any(|f| f.path == "specs/a/plan.md"),
            "the fixture must hold the document with no node"
        );

        let warnings = evicted_for(
            &dir,
            &config,
            &[(
                PathBuf::from("specs/a/spec.md"),
                Proposed::Content(spec("superseded")),
            )],
        );

        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].message.contains("specs/a/plan.md")
                && warnings[0].message.contains("parse_failure"),
            "the advisory must name the finding that left with it: {}",
            warnings[0].message
        );
    }

    /// A document the project already dropped is not this write's doing, and
    /// re-reporting it on every unrelated mutation is the census that buries
    /// the signal. It has no node in the before graph, which is exactly what
    /// says the project was not holding it.
    #[test]
    fn a_document_the_project_already_dropped_is_not_reported_again() {
        let (dir, config) = sub_artifact_project(&[
            ("specs/a/spec.md", &spec("superseded")),
            ("specs/a/plan.md", PLAN),
            (
                "note.md",
                "---\nid: note\ntitle: N\nkind: generic\nstatus: active\n---\n# N\n",
            ),
        ]);

        let warnings = evicted_for(
            &dir,
            &config,
            &[(
                PathBuf::from("note.md"),
                Proposed::Content(
                    "---\nid: note\ntitle: N\nkind: generic\nstatus: superseded\n---\n# N\n".into(),
                ),
            )],
        );

        assert!(warnings.is_empty(), "{warnings:?}");
    }

    /// A path the proposal names is the operator's own instruction. A
    /// deletion is what was asked for, and a move takes the record with it —
    /// reported as an eviction, every `rename` of a sub-artifact would claim
    /// the project had lost a document it still holds.
    #[test]
    fn a_path_the_proposal_names_is_never_an_eviction() {
        let (dir, config) = sub_artifact_project(&[
            ("specs/a/spec.md", &spec("active")),
            ("specs/a/plan.md", PLAN),
        ]);

        let warnings = evicted_for(
            &dir,
            &config,
            &[
                (
                    PathBuf::from("specs/a/spec.md"),
                    Proposed::Content(spec("superseded")),
                ),
                (PathBuf::from("specs/a/plan.md"), Proposed::Absent),
            ],
        );

        assert!(warnings.is_empty(), "{warnings:?}");
    }

    /// Why reading one record accounts for every document a write drops: with
    /// no `conditional_exclude` rule, `ScanConfig` projects no status
    /// vocabulary at all, so membership is a function of paths — which only a
    /// proposal that names one can move.
    #[test]
    fn without_a_conditional_exclude_rule_content_cannot_move_membership() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("specs/a")).unwrap();
        fs::write(dir.path().join("specs/a/spec.md"), spec("active")).unwrap();
        fs::write(dir.path().join("specs/a/plan.md"), PLAN).unwrap();
        let config = Config::default();
        assert!(config.scope.conditional_exclude.is_empty());

        let scan = |status: &str| {
            crate::builder::scanner::scan_scope_with_overlay(
                dir.path(),
                &config,
                &[(
                    PathBuf::from("specs/a/spec.md"),
                    Proposed::Content(spec(status)),
                )],
            )
            .unwrap()
        };

        let (active, terminal) = (scan("active"), scan("superseded"));
        assert_eq!(active.paths, terminal.paths);
        assert!(active.conditionally_excluded.is_empty());
        assert!(terminal.conditionally_excluded.is_empty());
    }

    #[test]
    /// A rewrite of a body the baseline froze is refused, and the refusal
    /// names the rule `check` would name.
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
            refusals.refusing(Path::new("a.md")).map(Refusal::lock),
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
            refusals.refusing(Path::new("a.md")).map(Refusal::lock),
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
            refusals.refusing(Path::new("a.md")).map(Refusal::lock),
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
