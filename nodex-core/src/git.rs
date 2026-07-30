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
//! committed bytes ([`Repository::file_at`]), the drift probe counts
//! commits (`rules::git_drift`), and the CLI materialises a past ref in
//! a disposable worktree (`commands/git_worktree.rs`).

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
/// Tree entry modes for a regular file, the only shape a document can be
/// committed as. Git records a symlink as a blob too (`120000`, holding
/// the target path) and a submodule as a commit (`160000`), so an object's
/// type does not answer "is this a file" — its mode does.
const REGULAR_FILE_MODES: [&str; 2] = ["100644", "100755"];

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
        tracked.push(crate::path_guard::forward_string(rel_path));
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

    /// The bytes of `rel_path` as committed at `git_ref`, or `Ok(None)`
    /// when the ref does not carry that path. The baseline view the
    /// rewrite-lock probes diff against: it lets a write seam compute
    /// exactly what a `check` against `rules.immutable_baseline` would —
    /// the before-snapshot status and body fingerprint — so the seam
    /// skips or refuses a mutation iff `check` would flag it.
    ///
    /// `Err` when the invocation could not run at all. That is a
    /// different fact from "the ref does not carry this path", and
    /// collapsing the two would let a lock read an unanswerable question
    /// as "nothing to lock" and permit the write it exists to refuse.
    ///
    /// A ref carries the path only when it records a *regular file*
    /// there, which is what the tree entry's mode says and neither the
    /// object's type nor its readability does. Anything else recorded at a
    /// document's name — a directory, a submodule gitlink, a symlink whose
    /// blob holds the target path — otherwise reads back as content with
    /// no frontmatter, whose status falls back to a non-terminal value:
    /// a fabricated before-snapshot where the document is in truth new,
    /// which disengages a terminal lock and engages a creation one.
    pub fn file_at(&self, git_ref: &str, rel_path: &Path) -> io::Result<Option<String>> {
        let Some(object) = self.regular_file_at(git_ref, rel_path)? else {
            return Ok(None);
        };
        let output = self
            .command()
            .args(["cat-file", "blob", &object])
            .output()?;
        if !output.status.success() {
            // Git named this object a regular file a moment ago, so
            // failing to read it is repository damage, not absence — and
            // absence is what a lock reads as "nothing to freeze".
            return Err(io::Error::other(format!(
                "git cat-file blob {object} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
    }

    /// The object id of the regular file `git_ref` records at `rel_path`,
    /// or `None` when it records anything else there — including nothing.
    fn regular_file_at(&self, git_ref: &str, rel_path: &Path) -> io::Result<Option<String>> {
        let output = self
            .command()
            .args(["ls-tree", git_ref, "--"])
            .arg(self.tracked_path(rel_path))
            .output()?;
        if !output.status.success() {
            return Ok(None);
        }
        // `<mode> SP <type> SP <object> TAB <path>`. Only the fields
        // before the tab are read, so nothing here depends on how git
        // spells the path back: it quotes some, leaves others as the
        // bytes they are, and a path may legally span lines.
        let fields = output
            .stdout
            .split(|byte| *byte == b'\t')
            .next()
            .unwrap_or_default();
        let fields = std::str::from_utf8(fields).map_err(|e| {
            io::Error::other(format!("git ls-tree reported no readable entry: {e}"))
        })?;
        let mut parts = fields.split_whitespace();
        let (Some(mode), Some(_), Some(object)) = (parts.next(), parts.next(), parts.next()) else {
            return Ok(None);
        };
        Ok(REGULAR_FILE_MODES
            .contains(&mode)
            .then(|| object.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Initialise a repository at `root` holding one committed document,
    /// in a project directory at `prefix` (the project *is* the repository
    /// when `prefix` is empty).
    fn init_repo_with_project(root: &Path, prefix: &str) {
        let run = |args: &[&str]| {
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
        };
        run(&["init", "-q"]);
        // Signing off, as every other git fixture in the workspace does:
        // a machine with `commit.gpgsign = true` would otherwise fail here
        // and nowhere else.
        run(&["config", "commit.gpgsign", "false"]);
        let project = root.join(prefix);
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("d.md"), "committed\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "base"]);
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
        assert_eq!(
            repo.file_at("HEAD", Path::new("d.md"))
                .expect("git ran")
                .as_deref(),
            Some("committed\n")
        );
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
        assert_eq!(
            repo.file_at("HEAD", Path::new("d.md"))
                .expect("git ran")
                .as_deref(),
            Some("committed\n"),
            "the project's own committed bytes, not the repository root's"
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
        assert_eq!(
            repo.file_at("HEAD", Path::new("d.md"))
                .expect("git ran")
                .as_deref(),
            Some("committed\n"),
            "the project's own committed bytes, whatever its path spells"
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

    /// A document's baseline is the regular file a ref records at its
    /// path, and only the tree entry's mode says which entries those are:
    /// git stores a symlink as a blob holding the target path, so both its
    /// type and its readability answer yes. Anything else read as content
    /// parses as a document with no frontmatter, whose status falls back
    /// to a non-terminal value — a fabricated before-snapshot for a
    /// document that is in truth new, disengaging a terminal lock and
    /// engaging a creation one.
    #[test]
    fn file_at_reads_only_what_a_ref_records_as_a_regular_file() {
        let dir = tempfile::TempDir::new().unwrap();
        init_repo_with_project(dir.path(), "docs-site");
        let project = dir.path().join("docs-site");
        let run = |args: &[&str]| {
            command(dir.path())
                .expect("git on PATH")
                .args(args)
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .expect("git ran")
        };
        std::fs::write(project.join("plain.md"), "plain\n").unwrap();
        std::fs::write(project.join("empty.md"), "").unwrap();
        std::fs::write(project.join("runnable.md"), "runnable\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                project.join("runnable.md"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
            std::os::unix::fs::symlink("plain.md", project.join("linked.md")).unwrap();
        }
        std::fs::create_dir(project.join("foldered.md")).unwrap();
        std::fs::write(project.join("foldered.md").join("note.md"), "note\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "one of every shape"]);
        let vendored = run(&["rev-parse", "HEAD"]);
        let vendored = String::from_utf8_lossy(&vendored.stdout).trim().to_string();
        run(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{vendored},docs-site/vendored.md"),
        ]);
        run(&["commit", "-q", "-m", "a gitlink at a document's name"]);

        let repo = Repository::discover(&project)
            .expect("git on PATH")
            .expect("a subdirectory of a work tree is a work tree");
        let at = |name: &str| repo.file_at("HEAD", Path::new(name)).expect("git ran");
        assert_eq!(at("plain.md").as_deref(), Some("plain\n"));
        assert_eq!(
            at("empty.md").as_deref(),
            Some(""),
            "an empty document has a baseline; it is not an absent one"
        );
        assert_eq!(
            at("runnable.md").as_deref(),
            Some("runnable\n"),
            "an executable bit does not stop a file being one"
        );
        #[cfg(unix)]
        assert_eq!(
            at("linked.md"),
            None,
            "a symlink's blob holds a path, not the document"
        );
        assert_eq!(at("foldered.md"), None, "a directory carries no document");
        assert_eq!(at("vendored.md"), None, "a gitlink carries no document");
        assert_eq!(at("absent.md"), None);
    }

    /// A path the ref does not carry is absence, not an error: the
    /// rewrite-lock probes read "this document had no baseline" from it.
    #[test]
    fn file_at_reports_absence_for_a_path_the_ref_does_not_carry() {
        let dir = repo_with_project("docs-site");
        let repo = Repository::discover(&dir.path().join("docs-site"))
            .expect("git on PATH")
            .expect("a subdirectory of a work tree is a work tree");
        assert!(
            repo.file_at("HEAD", Path::new("absent.md"))
                .expect("git ran")
                .is_none()
        );
    }
}
