//! The repository a project is tracked in — resolved from the project's
//! own location, bound explicitly, and the one seam every `git`
//! invocation is built through.
//!
//! Two levels, and neither lets anything but the project's location
//! decide what git measures. [`command`] builds an invocation the
//! ambient environment cannot redirect, for a directory that need not
//! hold a repository yet — a fixture's `git init`, and the private
//! probes this module resolves a binding from.
//! [`Repository`] is that binding: which git directory, which
//! work tree, and *where inside that work tree the project sits*. The
//! prefix is what makes a project that is not the repository's top level
//! measure itself rather than the repository: every path handed to git
//! goes through [`Repository::tracked_path`], and every checkout of a
//! past ref is graphed from [`Repository::locate`].
//!
//! On top of the binding, the immutability guards read a document's
//! baseline as a graph (`commands/git_worktree.rs`), [`History`] indexes
//! a revision range by the paths its commits changed so the drift
//! measurement (`rules::git_drift`) can ask about every document without
//! asking git about every document, and the CLI materialises a past ref
//! in a disposable worktree (`commands/git_worktree.rs`).

use chrono::NaiveDate;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Variables that reinterpret the path arguments this seam passes. An
/// inherited `GIT_ICASE_PATHSPECS` makes the drift probe count commits
/// on a case-variant of the path it was asked about, and each of the
/// four is rejected outright by the `--literal-pathspecs` every
/// invocation here pins ("global 'literal' pathspec setting is
/// incompatible with all other global pathspec settings"), which would
/// turn a measurement into a failure.
const PATHSPEC_SEMANTICS: [&str; 4] = [
    "GIT_LITERAL_PATHSPECS",
    "GIT_GLOB_PATHSPECS",
    "GIT_NOGLOB_PATHSPECS",
    "GIT_ICASE_PATHSPECS",
];

/// Every variable cleared before a `git` process starts: the repository
/// git would otherwise select for itself, plus the pathspec group above.
///
/// The repository-identity group is `git rev-parse --local-env-vars` —
/// git's own answer, and the same set git clears before running a
/// command in a different repository. It is read from the installed
/// binary rather than frozen here, so the seam tracks the git it is
/// driving instead of the git that was current when this was written.
///
/// The variables that *bound discovery* (`GIT_CEILING_DIRECTORIES`,
/// `GIT_DISCOVERY_ACROSS_FILESYSTEM`) are deliberately left alone. They
/// cannot point an invocation at a different repository — they only
/// decide whether the one above the project is found at all, an outcome
/// this crate reports rather than papers over. Clearing them would
/// override a deliberate operator setting and lose a work tree that
/// legitimately straddles a mount boundary.
///
/// Probed once per process, caching only success — a failed probe leaves
/// the cell empty so a later call retries instead of treating one
/// transient spawn failure as "no git for the rest of this process".
fn overriding_variables() -> io::Result<&'static [String]> {
    static VARS: OnceLock<Vec<String>> = OnceLock::new();
    if let Some(vars) = VARS.get() {
        return Ok(vars);
    }
    let output = bare().args(["rev-parse", "--local-env-vars"]).output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "git rev-parse --local-env-vars failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let local: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    if local.is_empty() {
        return Err(io::Error::other(
            "git rev-parse --local-env-vars listed nothing, so no repository scope can be \
             established",
        ));
    }
    Ok(VARS.get_or_init(|| {
        local
            .into_iter()
            .chain(PATHSPEC_SEMANTICS.iter().map(|v| (*v).to_owned()))
            .collect()
    }))
}

/// A bare `git`, carrying whatever environment this process inherited.
/// The only legitimate caller is [`overriding_variables`], whose
/// question — "which variables does this git treat as repository-local?"
/// — needs no repository and must be answerable before any scope exists.
#[expect(
    clippy::disallowed_methods,
    reason = "the one place the git binary is named; every other invocation is built from here"
)]
fn bare() -> Command {
    Command::new("git")
}

/// A `git` invocation with the environment neutralised, pathspec
/// interpretation pinned, and `cwd` as its working directory.
fn scoped(cleared: &'static [String], cwd: &Path) -> Command {
    let mut git = bare();
    for var in cleared {
        git.env_remove(var);
    }
    // Paths this seam passes are filesystem paths, never patterns: the
    // graph and the resolver produce them, and a document or code path
    // may legitimately contain `*`, `?` or `[`.
    git.arg("--literal-pathspecs");
    git.current_dir(cwd);
    git
}

/// A `git` invocation started in `dir` that the ambient environment
/// cannot redirect.
///
/// A working directory only decides where git *starts* looking for a
/// repository: git skips that search outright when `GIT_DIR` is set, and
/// consults `GIT_INDEX_FILE` / `GIT_OBJECT_DIRECTORY` / `GIT_COMMON_DIR`
/// whether or not it is. Server-side hooks export `GIT_DIR` plus an
/// absolute `GIT_OBJECT_DIRECTORY`, `git submodule foreach` exports
/// `GIT_DIR`, and every shell-based git subcommand sourcing
/// `git-sh-setup` exports it too — so an inherited environment silently
/// points an invocation at a repository other than the one being
/// analysed, and each consequence is a wrong answer wearing the shape of
/// a right one. Every variable that can redirect an invocation — the set
/// git itself reports as repository-local, plus the ones that reinterpret
/// a path argument — is cleared, so `dir` is the only thing that decides
/// which repository answers.
///
/// This is the level *below* [`Repository`]: an invocation for a
/// directory that need not hold a repository yet, which outside this
/// module means a fixture's `git init`. Anything that reads or writes
/// repository state goes through the resolved binding instead, which
/// additionally pins the git directory, the work tree, and the project's
/// prefix within it.
///
/// `Err` when git cannot be invoked at all: no scope can be
/// established, so callers take the git-unavailable path they already
/// have rather than run an unscoped command.
pub fn command(dir: &Path) -> io::Result<Command> {
    Ok(scoped(overriding_variables()?, dir))
}

/// One `rev-parse` answer, or `None` when the question does not apply to
/// `dir` (it holds no repository, or holds a bare one).
///
/// The whole of stdout, minus the single newline that terminates it, is
/// the answer. That is the only unambiguous reading: git does not quote
/// or escape the paths `rev-parse` reports and offers no NUL-delimited
/// mode, so an invocation carrying several answers cannot be split back
/// into them — a path component may itself contain a newline, which is
/// legal on POSIX filesystems. One question per invocation is what makes
/// the answer exact.
///
/// The bytes become a path without passing through a `String`: the
/// repository's own location is the operating system's to spell, and a
/// lossy decode would hand git a path that does not exist while looking
/// like one that does.
fn answer(cleared: &'static [String], dir: &Path, question: &str) -> io::Result<Option<PathBuf>> {
    let output = scoped(cleared, dir)
        .args(["rev-parse", question])
        .output()?;
    if !output.status.success() {
        return Ok(None);
    }
    let mut bytes = output.stdout;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    // Windows forbids control characters in a path component, so a
    // carriage return there is a line terminator and never data; on a
    // POSIX filesystem it can be part of the name and must survive.
    #[cfg(windows)]
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    Ok(Some(os_path(bytes)?))
}

/// A path from git's stdout, byte-exact.
#[cfg(unix)]
fn os_path(bytes: Vec<u8>) -> io::Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

/// Windows paths are Unicode and git spells them UTF-8, so a decode
/// failure is a real anomaly rather than a legal name this seam should
/// carry.
#[cfg(not(unix))]
fn os_path(bytes: Vec<u8>) -> io::Result<PathBuf> {
    String::from_utf8(bytes)
        .map(PathBuf::from)
        .map_err(|e| io::Error::other(format!("git reported a path that is not UTF-8: {e}")))
}

/// A project-relative path as git spells one — forward-slashed, and the
/// document's own bytes.
///
/// The graph keeps a document's path exactly as the filesystem gave it, and
/// the crate's display form folds `\` to `/` for a stable JSON contract. On
/// unix a `\` is an ordinary byte in a name rather than a separator, so
/// folding it here would measure a path no ref records.
#[cfg(unix)]
fn git_path(rel_path: &Path) -> std::ffi::OsString {
    rel_path.as_os_str().to_owned()
}

/// Windows spells separators `\`, so they do have to fold — and every path
/// reaching this seam is UTF-8 there ([`os_path`] refuses anything else),
/// which makes the folded string exact.
#[cfg(not(unix))]
fn git_path(rel_path: &Path) -> std::ffi::OsString {
    std::ffi::OsString::from(crate::path_guard::forward_string(rel_path))
}

/// What a git ref holds for one project — the states a baseline can be
/// in before any document is looked up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefState {
    /// No ref in the repository names a commit, so no baseline could
    /// name a snapshot either — an ordinary state for a project set up
    /// before its first commit, and not the operator's mistake. An object
    /// no ref reaches is deliberately not counted: a baseline names refs,
    /// so a commit that has lost its last ref is as unnameable as one that
    /// never existed, and counting it would make the verdict depend on
    /// reflog expiry.
    Unborn,
    /// The ref names no commit in a repository that has some: a typo, or
    /// a ref that was never fetched. Nothing can be read from it.
    Unresolvable,
    /// The ref is a commit, but the project's own directory is not a
    /// directory in it — absent, or recorded there as a file or a
    /// submodule. There is no snapshot to compare against.
    WithoutProject,
    /// The ref is a commit whose tree carries the project's directory, so
    /// a per-document lookup that finds nothing means that document is
    /// new.
    CarriesProject,
}

/// The repository a project is tracked in, together with where the
/// project sits inside it.
///
/// Resolved from the project root ([`discover`]) by the commands and
/// rule passes that measure git, and then the binding is explicit: every
/// invocation names the git directory and the work tree, so nothing
/// about the environment or the state of the filesystem between two
/// invocations can move the target. The `prefix` is the project's own
/// path inside the work tree — empty when the project *is* the
/// repository's top level, `docs-site/` when it is a subdirectory of a
/// larger repository. Both are ordinary: a `nodex.toml` in a monorepo
/// subdirectory is as valid as one at the root, and the two must measure
/// the same way.
///
/// Git-facing paths go through [`tracked_path`](Self::tracked_path) and
/// a materialised checkout through [`locate`](Self::locate). The
/// caller's own `root` stays the filesystem authority — writes are
/// contained against it by `path_guard`, never against the work tree
/// discovered here.
///
/// [`discover`]: Self::discover
#[derive(Debug, Clone)]
pub struct Repository {
    git_dir: PathBuf,
    work_tree: PathBuf,
    prefix: PathBuf,
    cleared: &'static [String],
}

impl Repository {
    /// Resolve the repository containing `root`, or `Ok(None)` when
    /// `root` is not inside a git work tree (including a bare
    /// repository, which has none). `Err` when git cannot be invoked, or
    /// when what it reports does not describe `root` — so callers can
    /// distinguish "this project has no git history to measure" from
    /// "this environment cannot answer for it", and a binding that exists
    /// is a binding that was checked.
    ///
    /// Three questions, three invocations, because git reports paths
    /// unquoted and offers no NUL-delimited mode: only a single-answer
    /// invocation can be read back exactly, since a path component may
    /// itself contain a newline. Consistency
    /// across the three is then established rather than assumed: the git
    /// directory and work tree must exist, and the work tree's own
    /// `prefix` must lead back to `root`. A layout that shifts under a
    /// concurrent `git init` mid-resolution therefore fails loudly
    /// instead of binding to a repository that is not the project's.
    pub fn discover(root: &Path) -> io::Result<Option<Self>> {
        let cleared = overriding_variables()?;
        let Some(git_dir) = answer(cleared, root, "--absolute-git-dir")? else {
            return Ok(None);
        };
        let Some(work_tree) = answer(cleared, root, "--show-toplevel")? else {
            return Ok(None);
        };
        let Some(prefix) = answer(cleared, root, "--show-prefix")? else {
            return Ok(None);
        };
        let repository = Self {
            git_dir,
            work_tree,
            // Empty at the top level of the work tree, `docs-site/` below
            // it. Reassembled from its components so the trailing
            // separator git includes is gone without the names' own bytes
            // being touched, leaving `tracked_path` the only place a
            // separator is introduced.
            prefix: prefix.components().collect(),
            cleared,
        };
        repository.verify(root)?;
        Ok(Some(repository))
    }

    /// Establish that this binding describes the project at `root`.
    ///
    /// The paths git reported must exist, and the work tree plus the
    /// project's prefix must be the same directory as `root` — compared
    /// through the filesystem, so a symlinked or differently-cased route
    /// to the same directory agrees while a genuinely different one does
    /// not. Every downstream measurement is written against this binding,
    /// so a binding that cannot be shown to describe the project is an
    /// error here rather than a wrong answer later.
    fn verify(&self, root: &Path) -> io::Result<()> {
        let mismatch = |detail: String| {
            io::Error::other(format!(
                "git reported a repository that does not describe {}: {detail}",
                root.display()
            ))
        };
        if !self.git_dir.exists() {
            return Err(mismatch(format!(
                "git directory {} does not exist",
                self.git_dir.display()
            )));
        }
        let located = self.locate(&self.work_tree);
        let (Ok(located), Ok(root)) = (located.canonicalize(), root.canonicalize()) else {
            return Err(mismatch(format!(
                "{} could not be resolved on disk",
                located.display()
            )));
        };
        if located != root {
            return Err(mismatch(format!(
                "work tree plus prefix is {}",
                located.display()
            )));
        }
        Ok(())
    }

    /// A `git` invocation bound to this repository: the git directory
    /// and work tree are named outright, so neither discovery nor the
    /// environment participates. Paths passed to it are work-tree
    /// relative — [`tracked_path`](Self::tracked_path) writes them.
    pub fn command(&self) -> Command {
        let mut git = scoped(self.cleared, &self.work_tree);
        git.arg("--git-dir")
            .arg(&self.git_dir)
            .arg("--work-tree")
            .arg(&self.work_tree);
        git
    }

    /// A project-relative path as git tracks it: the project's prefix
    /// followed by `rel_path`, forward-slashed. The one translation from
    /// "where the project keeps this file" to "what this repository
    /// calls it", so a pathspec and a `<ref>:<path>` lookup can never
    /// disagree about which file is meant.
    ///
    /// The two halves are spelled differently on purpose. The prefix is
    /// the repository's own location, which only the operating system
    /// names, so it is carried byte-exact; `rel_path` comes from the
    /// graph, whose paths round-trip through JSON and are therefore text
    /// by construction, so its separators are normalised as everywhere
    /// else in the project.
    pub fn tracked_path(&self, rel_path: &Path) -> std::ffi::OsString {
        let mut tracked = self.prefix.clone().into_os_string();
        if !tracked.is_empty() {
            tracked.push("/");
        }
        tracked.push(git_path(rel_path));
        tracked
    }

    /// The project's own path inside the repository — empty when the
    /// project is the repository's top level. Diagnostics name it when a
    /// ref does not carry the project at all.
    pub fn prefix(&self) -> &Path {
        &self.prefix
    }

    /// Where the project sits inside `tree_root` — a checkout of this
    /// repository at some ref, or the live work tree. `tree_root` itself
    /// for a project at the repository's top level, the project's own
    /// subdirectory otherwise. A baseline build reads from here, so a
    /// subdirectory project is graphed as itself and not as the
    /// repository around it.
    pub fn locate(&self, tree_root: &Path) -> PathBuf {
        if self.prefix.as_os_str().is_empty() {
            tree_root.to_path_buf()
        } else {
            tree_root.join(&self.prefix)
        }
    }

    /// What a ref offers this project, established once so that a
    /// per-document answer can be trusted afterwards.
    ///
    /// Without it, "no bytes for this document at the baseline" covers
    /// two unrelated facts — the document is new, or the baseline holds
    /// nothing for this project at all — and a lock reading the second as
    /// the first permits every write it exists to refuse.
    pub fn ref_state(&self, git_ref: &str) -> io::Result<RefState> {
        if !self.resolves(&format!("{git_ref}^{{commit}}"))? {
            // A repository that has recorded nothing is a different fact
            // from one pointed at a ref it does not know, and only the
            // first is an ordinary state to go inert on. `HEAD` cannot
            // discriminate them: it also names nothing when it is a
            // dangling symref over real history, which would report a
            // repository with commits as having none and downgrade a
            // refusal to an advisory.
            return Ok(if self.records_a_commit()? {
                RefState::Unresolvable
            } else {
                RefState::Unborn
            });
        }
        // The project's directory has to be a *directory* there. A
        // `rev-parse` of the path resolves just as happily for a file or
        // a submodule gitlink recorded at that name, and binding to one
        // would leave every document lookup empty — a baseline that reads
        // as "nothing is frozen" for the whole project. Git's own type
        // answer is the question that discriminates; the peel syntax
        // cannot, because `<ref>:<path>^{tree}` reads the suffix as part
        // of the path.
        //
        // A symlink recorded at the prefix answers `blob`, so a ref that
        // reaches the project only through one reads as carrying nothing.
        // That is the deliberate direction: following it would mean
        // resolving a link inside a tree, whose target may be another
        // link or outside the repository entirely, to decide what a lock
        // compares against. Under-enforcing while saying so beats binding
        // to a location git was not asked about.
        let object = format!(
            "{git_ref}:{}",
            crate::path_guard::forward_string(&self.prefix)
        );
        let output = self.command().args(["cat-file", "-t", &object]).output()?;
        let is_tree =
            output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "tree";
        Ok(if is_tree {
            RefState::CarriesProject
        } else {
            RefState::WithoutProject
        })
    }

    /// Whether any ref in this repository names a commit — the question
    /// "has anything been recorded here yet", asked of the refs rather
    /// than of `HEAD`, which speaks only for itself.
    fn records_a_commit(&self) -> io::Result<bool> {
        let output = self
            .command()
            .args(["rev-list", "--all", "--max-count=1"])
            .output()?;
        Ok(output.status.success() && !output.stdout.is_empty())
    }

    /// Whether git resolves `object` in this repository.
    fn resolves(&self, object: &str) -> io::Result<bool> {
        let output = self
            .command()
            .args(["rev-parse", "--verify", "--quiet", object])
            .output()?;
        Ok(output.status.success())
    }
}

/// Every commit a revision range records for the project's paths,
/// indexed by what each one changed.
///
/// The question behind it is per document — "how many commits landed on
/// this path since it was reviewed" — but one revision walk holds every
/// answer, and putting the question to git per document spends a process
/// on each: the cost of a pass then tracks the size of the repository it
/// reads rather than of the corpus. The walk is bounded by the project's
/// prefix, so a project inside a larger repository indexes itself and
/// nothing around it.
///
/// That trade is worth taking wherever documents outnumber the history
/// they measure against, and it inverts where they do not: a handful of
/// documents in a long-lived repository pays for every commit under the
/// prefix, where a walk per document would have paid for a handful. The
/// exchange rate is a walked commit against a spawned process, and a
/// process is worth thousands of them — 20k commits index in a quarter
/// of a second, which one `rev-list` per document reaches at about
/// twenty documents.
///
/// What it counts is every commit that *introduced* a change to a path.
/// `--full-history` is what makes that true of a bounded walk: with a
/// pathspec, git's default is to report the simplest history explaining
/// the final state, so churn a later merge resolved away disappears, and
/// a document covering that file has drifted from it either way. A merge
/// introduces a change only where it differs from *every* parent — a
/// conflict resolved into something no side had — because one that took
/// a side's version whole introduced nothing that side's own commit did
/// not, and that is exactly what `--diff-merges=combined` reports.
/// `--no-renames` keeps a rename counting against both names, the way a
/// tree diff does, and `-z` makes the reported names the bytes git holds
/// rather than a quoted rendering of them — a name may contain a newline,
/// and every other rendering quotes it into something no lookup matches.
///
/// `--diff-merges=<how>` is git 2.31, so a project measuring drift on an
/// older git finds the walk refused. Nothing is mismeasured: the reading
/// fails, every target reports unmeasurable, and the drift component
/// leaves the trust composite — the same states an unreadable repository
/// already produces.
pub struct History {
    /// The local calendar date of each commit, by walk position.
    dates: Vec<NaiveDate>,
    /// `(project-relative path, walk position)` sorted by path, so
    /// everything under a directory is one contiguous range.
    touches: Vec<(OsString, u32)>,
}

impl History {
    /// Walk `revisions` once. `Err` when git could not answer — an
    /// unborn `HEAD`, a range naming a ref the repository does not hold
    /// — which a caller must not read as "nothing changed".
    pub fn read(repository: &Repository, revisions: &str) -> io::Result<Self> {
        let prefix = repository.prefix();
        let mut git = repository.command();
        git.args([
            "log",
            "--full-history",
            "--diff-merges=combined",
            "--no-renames",
            "--name-only",
            "-z",
            "--format=%x00%x00%ct",
        ])
        .arg(revisions);
        if !prefix.as_os_str().is_empty() {
            git.arg("--").arg(prefix);
        }
        let output = git.output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "git could not walk {revisions}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Self::index(&output.stdout, prefix)
    }

    /// Read the walk's stream. Every record opens `%x00%x00` — two empty
    /// fields, a shape no diff section can produce: git reports no empty
    /// name, and separates a commit from its names with at most one field
    /// of its own. That separator differs by commit — a newline for an
    /// ordinary diff, an empty field for a merge's combined one — so it
    /// is read rather than assumed, which is also what keeps a name
    /// legitimately beginning with a newline intact.
    fn index(stream: &[u8], prefix: &Path) -> io::Result<Self> {
        let malformed =
            |detail: &str| io::Error::other(format!("git reported {detail} for a commit"));
        let fields: Vec<&[u8]> = stream.split(|byte| *byte == 0).collect();
        let mut dates: Vec<NaiveDate> = Vec::new();
        let mut touches: Vec<(OsString, u32)> = Vec::new();
        let mut at = 0usize;
        while at < fields.len() {
            let opening = fields[at..].iter().take_while(|f| f.is_empty()).count();
            at += opening;
            // The NUL terminating the last name closes the stream, so a
            // final run of empty fields is the end of the walk.
            if at == fields.len() {
                break;
            }
            if opening < 2 {
                return Err(malformed("a name where a record opens"));
            }
            let date = std::str::from_utf8(fields[at])
                .ok()
                .and_then(|seconds| seconds.parse().ok())
                .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
                .ok_or_else(|| malformed("an unreadable timestamp"))?
                .with_timezone(&chrono::Local)
                .date_naive();
            let commit = u32::try_from(dates.len())
                .map_err(|_| malformed("more history than an index can hold"))?;
            dates.push(date);
            // A merge's names sit behind an empty field; an ordinary
            // commit's behind the newline glued to the first of them.
            let combined = fields.get(at + 1).is_some_and(|f| f.is_empty())
                && fields.get(at + 2).is_some_and(|f| !f.is_empty());
            at += if combined { 2 } else { 1 };
            let mut opens_the_diff = !combined;
            while let Some(name) = fields.get(at).filter(|name| !name.is_empty()) {
                let name = if opens_the_diff {
                    opens_the_diff = false;
                    name.strip_prefix(b"\n")
                        .ok_or_else(|| malformed("a diff that does not open where git opens one"))?
                } else {
                    name
                };
                if let Some(path) = project_relative(&os_path(name.to_vec())?, prefix) {
                    touches.push((path, commit));
                }
                at += 1;
            }
        }
        touches.sort_unstable();
        Ok(Self { dates, touches })
    }

    /// How many commits changed `path` — or anything under it, when
    /// `path` names a directory — on a day after `reviewed`.
    ///
    /// The day is the operator's, the frame every other date in a pass is
    /// read in. Leaving the cutoff to git would make the count depend on
    /// the hour it was asked for: `--since <date>` fills the time of day
    /// it was not given from the clock, so one repository read twice in a
    /// day answers twice.
    pub fn commits_since(&self, path: &Path, reviewed: NaiveDate) -> u32 {
        let mut counted: Vec<u32> = self
            .touching(&git_path(path))
            .filter(|commit| self.dates[*commit as usize] > reviewed)
            .collect();
        // A directory names one commit once per file it changed there.
        counted.sort_unstable();
        counted.dedup();
        counted.len() as u32
    }

    /// The walk positions recorded for `key` and for everything under it
    /// — a pathspec matches a directory as readily as a file, and a
    /// `covers` target is as often one as the other. `/` sorts below
    /// `0`, so the names under `key/` are exactly those between them.
    fn touching(&self, key: &OsStr) -> impl Iterator<Item = u32> {
        let mut under = key.to_os_string();
        under.push("/");
        let mut past = key.to_os_string();
        past.push("0");
        let named = self.between(|path| path < key, |path| path <= key);
        let nested = self.between(|path| path < under, |path| path < past);
        named.iter().chain(nested).map(|(_, commit)| *commit)
    }

    /// The entries between the two bounds, each given as the predicate
    /// that holds below it.
    fn between(
        &self,
        below_start: impl Fn(&OsStr) -> bool,
        below_end: impl Fn(&OsStr) -> bool,
    ) -> &[(OsString, u32)] {
        let start = self.touches.partition_point(|(path, _)| below_start(path));
        let end = self.touches.partition_point(|(path, _)| below_end(path));
        &self.touches[start..end]
    }
}

/// A path the walk reported, spelled as the project spells it — `None`
/// when it lies outside the project's own directory.
fn project_relative(path: &Path, prefix: &Path) -> Option<OsString> {
    if prefix.as_os_str().is_empty() {
        return Some(path.as_os_str().to_owned());
    }
    Some(path.strip_prefix(prefix).ok()?.as_os_str().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// Run `git` in `root` under a fixed identity, so nothing a machine
    /// configures for its owner can move a fixture's history.
    fn run_git(root: &Path, args: &[&str]) {
        let out = command(root)
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
    }

    /// Initialise a repository at `root` holding one committed document,
    /// in a project directory at `prefix` (the project *is* the repository
    /// when `prefix` is empty).
    fn init_repo_with_project(root: &Path, prefix: &str) {
        run_git(root, &["init", "-q"]);
        // Signing off, as every other git fixture in the workspace does:
        // a machine with `commit.gpgsign = true` would otherwise fail here
        // and nowhere else.
        run_git(root, &["config", "commit.gpgsign", "false"]);
        let project = root.join(prefix);
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("d.md"), "committed\n").unwrap();
        run_git(root, &["add", "-A"]);
        run_git(root, &["commit", "-q", "-m", "base"]);
    }

    fn repo_with_project(prefix: &str) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        init_repo_with_project(dir.path(), prefix);
        dir
    }

    /// The scoping contract: every variable that could redirect the
    /// invocation is cleared, pathspec interpretation is pinned, and the
    /// working directory is the one asked for. The repository-identity
    /// group is asserted against git's live answer, so a variable the
    /// installed git knows about and this seam does not is a failure here
    /// rather than a silently mismeasured repository in the field.
    #[test]
    fn command_clears_every_overriding_variable_and_pins_pathspecs() {
        let root = tempfile::TempDir::new().unwrap();
        let git = command(root.path()).expect("git on PATH");
        let cleared: Vec<&str> = git
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key.to_str().expect("git variable names are UTF-8"))
            .collect();
        for var in overriding_variables().expect("git on PATH") {
            assert!(
                cleared.contains(&var.as_str()),
                "{var} can redirect an invocation but is not cleared: {cleared:?}"
            );
        }
        let live = bare()
            .args(["rev-parse", "--local-env-vars"])
            .output()
            .expect("git ran");
        for var in String::from_utf8_lossy(&live.stdout).lines() {
            assert!(
                cleared.contains(&var.trim()),
                "{var} is repository-local to the installed git but not cleared"
            );
        }
        assert!(
            git.get_args().any(|a| a == "--literal-pathspecs"),
            "pathspec interpretation must not depend on the environment"
        );
        assert_eq!(git.get_current_dir(), Some(root.path()));
    }

    #[test]
    fn discover_reports_absence_outside_a_work_tree() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(
            Repository::discover(dir.path())
                .expect("git on PATH")
                .is_none(),
            "a directory holding no repository has none to bind"
        );
    }

    /// A project at the repository's top level has no prefix, so tracked
    /// paths and checkouts are the project's own.
    #[test]
    fn discover_binds_a_top_level_project_without_a_prefix() {
        let dir = repo_with_project("");
        let repo = Repository::discover(dir.path())
            .expect("git on PATH")
            .expect("the project is a work tree");
        assert_eq!(repo.tracked_path(Path::new("docs/d.md")), "docs/d.md");
        assert_eq!(repo.locate(Path::new("/checkout")), Path::new("/checkout"));
    }

    /// A project in a subdirectory is bound to its own location: tracked
    /// paths carry the prefix and a checkout resolves to the project
    /// inside it. Without this, git answers about the repository root's
    /// same-named path — a different file, or none.
    #[test]
    fn discover_binds_a_subdirectory_project_to_its_own_prefix() {
        let dir = repo_with_project("docs-site");
        let repo = Repository::discover(&dir.path().join("docs-site"))
            .expect("git on PATH")
            .expect("a subdirectory of a work tree is a work tree");
        assert_eq!(repo.tracked_path(Path::new("d.md")), "docs-site/d.md");
        assert_eq!(
            repo.locate(Path::new("/checkout")),
            Path::new("/checkout/docs-site")
        );
    }

    /// A newline in a path component is legal on a POSIX filesystem, and
    /// `rev-parse` reports paths unquoted with no NUL-delimited mode —
    /// so answers from one invocation cannot be told apart, and a
    /// misread binding still *looks* bound while naming a directory that
    /// does not exist. One question per invocation keeps every answer
    /// exact, and the verdict identical to any other path's.
    #[cfg(unix)]
    #[test]
    fn discover_binds_a_repository_whose_path_is_not_one_line() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("we\nird");
        std::fs::create_dir(&root).unwrap();
        init_repo_with_project(&root, "docs-site");

        let repo = Repository::discover(&root.join("docs-site"))
            .expect("git on PATH")
            .expect("a subdirectory of a work tree is a work tree");
        assert_eq!(
            repo.tracked_path(Path::new("d.md")),
            std::ffi::OsStr::new("docs-site/d.md")
        );
    }

    /// "Nothing has been recorded yet" is the one state a baseline may go
    /// inert on, so it must not be inferred from `HEAD` alone: a dangling
    /// symref over real history names nothing either, and reading that as
    /// an empty repository turns a ref the operator must fix into an
    /// advisory the run continues past.
    #[test]
    fn ref_state_separates_an_empty_repository_from_a_head_that_names_nothing() {
        let dir = tempfile::TempDir::new().unwrap();
        let run = |args: &[&str]| {
            command(dir.path())
                .expect("git on PATH")
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .expect("git ran")
        };
        run(&["init", "-q"]);
        let repo = Repository::discover(dir.path())
            .expect("git on PATH")
            .expect("an initialised repository is a work tree");
        assert_eq!(
            repo.ref_state("HEAD").expect("git ran"),
            RefState::Unborn,
            "a repository with no commit at all"
        );

        std::fs::write(dir.path().join("d.md"), "committed\n").unwrap();
        run(&["config", "commit.gpgsign", "false"]);
        run(&["add", "-A"]);
        run(&[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-q",
            "-m",
            "base",
        ]);
        run(&["symbolic-ref", "HEAD", "refs/heads/does-not-exist"]);
        assert_eq!(
            repo.ref_state("HEAD").expect("git ran"),
            RefState::Unresolvable,
            "history exists; HEAD is the thing that names nothing"
        );

        // Deliberate: once the last ref to a commit is gone, the object
        // survives but no baseline could name it, and only the reflog
        // still reaches it — for as long as it is kept. Counting it would
        // make the verdict expire with the reflog, so a repository whose
        // refs name nothing is Unborn whatever objects it still holds.
        let refs = run(&["for-each-ref", "--format=%(refname)"]);
        for name in String::from_utf8_lossy(&refs.stdout).lines() {
            run(&["update-ref", "-d", name]);
        }
        assert_eq!(
            repo.ref_state("HEAD").expect("git ran"),
            RefState::Unborn,
            "no ref names a commit, so there is no snapshot to compare against"
        );
    }

    /// A repository whose history the walk can be pointed at, with one
    /// commit per call so a fixture reads as the history it describes.
    fn history_repo() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        run_git(dir.path(), &["init", "-q"]);
        run_git(dir.path(), &["config", "commit.gpgsign", "false"]);
        dir
    }

    /// Commit `files` as one commit dated `when` in the operator's own
    /// zone, so a fixture's calendar days are the days the count reads.
    fn commit_on(root: &Path, when: NaiveDate, hour: u32, files: &[(&str, &str)]) {
        let stamp = chrono::Local
            .from_local_datetime(&when.and_hms_opt(hour, 0, 0).expect("a valid hour"))
            .earliest()
            .expect("a local time on this day")
            .to_rfc3339();
        for (path, body) in files {
            let file = root.join(path);
            std::fs::create_dir_all(file.parent().expect("a file has a parent")).unwrap();
            std::fs::write(file, body).unwrap();
        }
        run_git(root, &["add", "-A"]);
        let out = command(root)
            .expect("git on PATH")
            .args(["commit", "-q", "-m", "x"])
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_DATE", &stamp)
            .env("GIT_COMMITTER_DATE", &stamp)
            .output()
            .expect("git ran");
        assert!(out.status.success(), "commit failed");
    }

    fn history_of(root: &Path) -> History {
        let repository = Repository::discover(root)
            .expect("git answered")
            .expect("a work tree");
        History::read(&repository, "HEAD").expect("a readable history")
    }

    /// The review day is the boundary, and the day is the whole day.
    /// Delegating the cutoff to `--since <date>` would instead fill the
    /// unstated time of day from the clock, so the same repository read
    /// at 09:00 and at 17:00 would report two different drifts.
    #[test]
    fn the_review_day_bounds_the_count_whatever_hour_it_is_asked_at() {
        let dir = history_repo();
        let reviewed = NaiveDate::from_ymd_opt(2024, 3, 10).unwrap();
        commit_on(dir.path(), reviewed, 23, &[("a.txt", "reviewed day\n")]);
        commit_on(
            dir.path(),
            reviewed.succ_opt().unwrap(),
            0,
            &[("a.txt", "day after\n")],
        );
        assert_eq!(
            history_of(dir.path()).commits_since(Path::new("a.txt"), reviewed),
            1,
            "the commit on the review day is what the review saw; the one after it is drift"
        );
    }

    /// A `covers` target is as often a directory as a file, and git
    /// measures a directory's history as one commit per commit — not one
    /// per file the commit changed under it.
    #[test]
    fn a_directory_counts_a_commit_once_however_many_files_it_changed() {
        let dir = history_repo();
        let reviewed = NaiveDate::from_ymd_opt(2024, 3, 10).unwrap();
        let day = reviewed.succ_opt().unwrap();
        commit_on(
            dir.path(),
            day,
            9,
            &[("src/one.rs", "1\n"), ("src/two.rs", "2\n")],
        );
        commit_on(dir.path(), day, 10, &[("src/nested/three.rs", "3\n")]);
        commit_on(dir.path(), day, 11, &[("srcs.rs", "beside, not under\n")]);
        let history = history_of(dir.path());
        assert_eq!(
            history.commits_since(Path::new("src"), reviewed),
            2,
            "a directory counts commits, and only the ones under it"
        );
        assert_eq!(history.commits_since(Path::new("src/one.rs"), reviewed), 1);
    }

    /// The prefix is the walk's boundary and its path language: a project
    /// below the repository root measures its own file, never the
    /// repository root's same-named one.
    #[test]
    fn a_project_below_the_repository_root_measures_only_itself() {
        let dir = history_repo();
        let reviewed = NaiveDate::from_ymd_opt(2024, 3, 10).unwrap();
        let day = reviewed.succ_opt().unwrap();
        commit_on(dir.path(), day, 9, &[("site/a.md", "project\n")]);
        commit_on(dir.path(), day, 10, &[("a.md", "repository root\n")]);
        commit_on(dir.path(), day, 11, &[("a.md", "root again\n")]);
        assert_eq!(
            history_of(&dir.path().join("site")).commits_since(Path::new("a.md"), reviewed),
            1,
            "the project's own a.md moved once; the repository root's is not its file"
        );
    }

    /// A range git cannot resolve leaves no reading, and a caller must be
    /// able to tell that from a range that added nothing — one is an
    /// unmeasurable environment, the other is a measured zero.
    #[test]
    fn a_range_the_repository_does_not_hold_is_an_error_not_an_empty_reading() {
        let dir = history_repo();
        commit_on(
            dir.path(),
            NaiveDate::from_ymd_opt(2024, 3, 11).unwrap(),
            9,
            &[("a.txt", "one\n")],
        );
        let repository = Repository::discover(dir.path())
            .expect("git answered")
            .expect("a work tree");
        assert!(History::read(&repository, "no-such-ref..HEAD").is_err());
        assert!(
            History::read(&repository, "HEAD..HEAD")
                .expect("an empty range is still a reading")
                .commits_since(
                    Path::new("a.txt"),
                    NaiveDate::from_ymd_opt(2024, 3, 10).unwrap()
                )
                == 0
        );
    }

    /// `-z` is what makes the walk's names the bytes git holds: a name
    /// carrying a newline reads back whole, where the quoted rendering
    /// git falls back to would have to be unescaped to be compared at
    /// all — and the newline the format writes before the first name is
    /// not part of it.
    #[cfg(unix)]
    #[test]
    fn a_name_carrying_a_newline_reads_back_whole() {
        let dir = history_repo();
        let reviewed = NaiveDate::from_ymd_opt(2024, 3, 10).unwrap();
        commit_on(
            dir.path(),
            reviewed.succ_opt().unwrap(),
            9,
            &[("\nodd.md", "odd\n"), ("plain.md", "plain\n")],
        );
        let history = history_of(dir.path());
        assert_eq!(history.commits_since(Path::new("\nodd.md"), reviewed), 1);
        assert_eq!(history.commits_since(Path::new("plain.md"), reviewed), 1);
    }

    /// Every commit that changed the path counts, including the ones a
    /// merge later resolved away. Git's default for a pathspec-limited
    /// walk is the opposite — it reports the simplest history explaining
    /// the final state, so a side branch whose change the merge did not
    /// keep disappears — and a document covering that file has drifted
    /// from it either way. The merge itself introduced nothing: it took
    /// one side's version whole.
    #[test]
    fn a_side_branch_the_merge_did_not_keep_still_counts() {
        let dir = history_repo();
        let reviewed = NaiveDate::from_ymd_opt(2024, 3, 10).unwrap();
        let day = reviewed.succ_opt().unwrap();
        commit_on(dir.path(), day, 8, &[("a.txt", "base\n")]);
        run_git(dir.path(), &["checkout", "-q", "-b", "feat"]);
        commit_on(dir.path(), day, 9, &[("a.txt", "feat\n")]);
        run_git(dir.path(), &["checkout", "-q", "-"]);
        commit_on(dir.path(), day, 10, &[("a.txt", "main\n")]);
        // Resolved in main's favour, which is what makes the branch's
        // commit invisible to the default walk.
        run_git(
            dir.path(),
            &[
                "merge",
                "-q",
                "--no-ff",
                "--no-commit",
                "-s",
                "ours",
                "feat",
            ],
        );
        run_git(dir.path(), &["commit", "-q", "-m", "merge"]);
        assert_eq!(
            history_of(dir.path()).commits_since(Path::new("a.txt"), reviewed),
            3,
            "base, the branch's commit and main's commit all changed the file"
        );
    }

    /// A merge that resolved a conflict into something no side had did
    /// change the path, and counts. One that took a side's version whole
    /// introduced nothing that side's own commit did not, and does not —
    /// counting it would charge the same change twice.
    #[test]
    fn a_merge_counts_only_where_it_changed_the_path_against_every_parent() {
        let dir = history_repo();
        let reviewed = NaiveDate::from_ymd_opt(2024, 3, 10).unwrap();
        let day = reviewed.succ_opt().unwrap();
        commit_on(dir.path(), day, 8, &[("p.txt", "base\n")]);
        run_git(dir.path(), &["checkout", "-q", "-b", "feat"]);
        commit_on(dir.path(), day, 9, &[("p.txt", "feat\n")]);
        run_git(dir.path(), &["checkout", "-q", "-"]);
        commit_on(dir.path(), day, 10, &[("p.txt", "main\n")]);
        run_git(
            dir.path(),
            &["merge", "--no-ff", "--no-commit", "-s", "ours", "feat"],
        );
        run_git(dir.path(), &["commit", "-q", "-m", "merge taking ours"]);
        assert_eq!(
            history_of(dir.path()).commits_since(Path::new("p.txt"), reviewed),
            3,
            "a merge that took one side whole adds nothing to that side's own commit"
        );

        commit_on(dir.path(), day, 11, &[("q.txt", "base\n")]);
        run_git(dir.path(), &["checkout", "-q", "-b", "other"]);
        commit_on(dir.path(), day, 12, &[("q.txt", "theirs\n")]);
        run_git(dir.path(), &["checkout", "-q", "-"]);
        commit_on(dir.path(), day, 13, &[("q.txt", "ours\n")]);
        run_git(
            dir.path(),
            &["merge", "--no-ff", "--no-commit", "-s", "ours", "other"],
        );
        commit_on(dir.path(), day, 14, &[("q.txt", "neither side had this\n")]);
        assert_eq!(
            history_of(dir.path()).commits_since(Path::new("q.txt"), reviewed),
            4,
            "a merge resolving into something no parent had changed the file itself"
        );
    }

    /// A commit that changed nothing writes no diff section at all, so
    /// what separates it from the next record is the record delimiter
    /// alone — the case that decides whether the stream can be read back
    /// into commits without guessing.
    #[test]
    fn a_commit_that_changed_nothing_does_not_desynchronise_the_walk() {
        let dir = history_repo();
        let reviewed = NaiveDate::from_ymd_opt(2024, 3, 10).unwrap();
        let day = reviewed.succ_opt().unwrap();
        commit_on(dir.path(), day, 8, &[("a.txt", "one\n")]);
        run_git(
            dir.path(),
            &["commit", "-q", "--allow-empty", "-m", "nothing"],
        );
        commit_on(dir.path(), day, 10, &[("a.txt", "two\n")]);
        assert_eq!(
            history_of(dir.path()).commits_since(Path::new("a.txt"), reviewed),
            2
        );
    }

    /// A merge that introduced nothing writes a diff section that is one
    /// empty field — indistinguishable, read alone, from the field that
    /// opens the next record. Two of them in a row is where a reader that
    /// guessed would lose the commit after them, so the framing is pinned
    /// here rather than left to the shapes an ordinary history happens to
    /// produce.
    #[test]
    fn merges_that_introduced_nothing_do_not_desynchronise_the_walk() {
        let dir = history_repo();
        let reviewed = NaiveDate::from_ymd_opt(2024, 3, 10).unwrap();
        let day = reviewed.succ_opt().unwrap();
        commit_on(dir.path(), day, 8, &[("a.txt", "base\n")]);
        run_git(dir.path(), &["tag", "base"]);
        // Named rather than inherited: which branch `git init` starts on is
        // the machine's `init.defaultBranch`, not this fixture's to assume.
        run_git(dir.path(), &["branch", "-M", "trunk"]);
        for (branch, hour) in [("feat1", 9), ("feat2", 10)] {
            run_git(dir.path(), &["checkout", "-q", "-b", branch, "base"]);
            commit_on(dir.path(), day, hour, &[("a.txt", &format!("{branch}\n"))]);
        }
        run_git(dir.path(), &["checkout", "-q", "trunk"]);
        commit_on(dir.path(), day, 11, &[("a.txt", "main\n")]);
        for branch in ["feat1", "feat2"] {
            run_git(
                dir.path(),
                &["merge", "--no-ff", "--no-commit", "-s", "ours", branch],
            );
            run_git(dir.path(), &["commit", "-q", "-m", "merge taking ours"]);
        }
        // Every commit that changed the file, and neither merge: each took
        // one side's version whole.
        assert_eq!(
            history_of(dir.path()).commits_since(Path::new("a.txt"), reviewed),
            4,
            "a record after two merges that introduced nothing is still read"
        );
    }
}
