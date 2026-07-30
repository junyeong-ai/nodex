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

use std::path::Path;

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
    /// The ref is a commit that carries the project, so a document with
    /// no bytes there is genuinely new.
    Bound {
        repository: crate::git::Repository,
        baseline: String,
    },
}

/// The immutability-lock baseline: the snapshot a `check` against
/// `rules.immutable_baseline` would diff against, resolved once per
/// command. Inert (every [`content`](Self::content) answers `Ok(None)`)
/// unless a baseline is configured, the project declares immutability
/// rules, the project sits in a git work tree, and the ref carries the
/// project — outside that activation the diff-aware immutability rules
/// cannot fire at check time either, so no write can introduce a
/// violation and nothing is locked.
///
/// The activation is established up front rather than inferred from the
/// first document: a ref that carries nothing for this project looks
/// exactly like a document that is new, and a lock reading the first as
/// the second permits every write it exists to refuse. So the ref is
/// resolved once, and its three answers are kept apart: bound, nothing
/// to compare against, or — refused at resolution — unreadable.
///
/// Both planes read this one resolution: a write seam consults
/// [`content`](Self::content) per file, and the read side takes the
/// binding from [`bound`](Self::bound) to materialise the baseline diff.
/// [`advisory`](Self::advisory) is the same fact for both — a run whose
/// locks did not engage is the failure a caller cannot see, so every
/// command that resolves a probe surfaces it.
///
/// [`content`]: Self::content
pub struct BaselineProbe {
    binding: Binding,
}

impl BaselineProbe {
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

    /// The document's committed bytes at the resolved baseline, or
    /// `Ok(None)` when nothing is bound or the baseline does not carry
    /// this document. Path translation goes through the binding, so a
    /// project in a subdirectory of a larger repository reads its own
    /// file.
    ///
    /// `Err` when the baseline cannot be read — an unresolvable ref, or an
    /// invocation that could not run. A lock consults this to decide
    /// whether a write is frozen, so an unanswerable question must not
    /// arrive as "no baseline, nothing frozen": the write is refused
    /// instead of quietly performed.
    pub fn content(&self, rel_path: &Path) -> Result<Option<String>> {
        let Some((repository, baseline)) = self.bound() else {
            return Ok(None);
        };
        repository
            .file_at(baseline, rel_path)
            .map_err(|source| crate::error::Error::Io {
                path: rel_path.to_path_buf(),
                source,
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
            binding: Binding::NotApplicable,
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
    fn locked_fixture(doc: &str) -> (TempDir, Config) {
        let dir = TempDir::new().unwrap();
        let run = |args: &[&str]| {
            let out = crate::git::command(dir.path())
                .expect("git on PATH")
                .args(args)
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .expect("git ran");
            assert!(out.status.success(), "git {args:?} failed");
        };
        run(&["init"]);
        run(&["config", "commit.gpgsign", "false"]);
        fs::write(dir.path().join("a.md"), doc).unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "baseline"]);

        let mut config = Config::default();
        config.statuses.terminal = vec!["superseded".into()];
        config.rules.immutable_baseline = Some("HEAD".into());
        config.rules.body_immutable = vec![BodyImmutableRuleConfig {
            name: "frozen".into(),
            mode: BodyImmutableMode::Frozen,
            trigger: ImmutableTrigger::Terminal,
            kinds: vec![],
        }];
        (dir, config)
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
        let probe = BaselineProbe::resolve(dir.path(), &config).expect("a readable baseline");
        assert!(probe.bound().is_none(), "no baseline configured → inert");
        assert!(
            probe
                .content(Path::new("a.md"))
                .expect("inert probe")
                .is_none()
        );
        assert!(
            probe.advisory().is_none(),
            "no baseline was asked for, so nothing went unenforced"
        );
    }

    #[test]
    fn baseline_probe_is_inert_without_immutable_rules() {
        let dir = TempDir::new().unwrap();
        let mut config = Config::default();
        config.rules.immutable_baseline = Some("HEAD".into());
        let probe = BaselineProbe::resolve(dir.path(), &config).expect("a readable baseline");
        assert!(
            probe.bound().is_none(),
            "a baseline with no immutability rules to feed is inert"
        );
        assert!(
            probe
                .content(Path::new("a.md"))
                .expect("inert probe")
                .is_none()
        );
        assert!(
            probe.advisory().is_none(),
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
        let probe = BaselineProbe::resolve(dir.path(), &config).expect("a readable baseline");
        assert!(
            probe.bound().is_none(),
            "outside a git work tree the diff-aware rules are inert and so is the probe"
        );
        assert!(
            probe
                .content(Path::new("a.md"))
                .expect("inert probe")
                .is_none()
        );
        // The locks the project asked for did not engage. A mutation that
        // proceeds without saying so is the silent-skip failure mode, so
        // the probe carries the advisory its consumers must surface.
        let advisory = probe
            .advisory()
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
        let (dir, config) = locked_fixture(doc);
        let probe = BaselineProbe::resolve(dir.path(), &config).expect("a readable baseline");
        assert!(probe.bound().is_some(), "fixture activates the probe");

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
        let (dir, config) = locked_fixture(doc);
        let probe = BaselineProbe::resolve(dir.path(), &config).expect("a readable baseline");

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
}
