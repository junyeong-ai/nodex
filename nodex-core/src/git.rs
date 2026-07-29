//! The one seam every `git` invocation is built through, plus the
//! byte-level access the mutation guards ask for.
//!
//! [`command`] binds an invocation to a single repository — the project
//! at `root` and no other — which is what makes every git-backed rule
//! and guard measure the tree it was pointed at. On top of it this
//! module answers the two byte-level questions the immutability guards
//! ask: "is this project a git work tree?" and "what were this
//! document's bytes at a ref?". The drift probes
//! (`rules::git_drift::commits_since` / `probe_environment`) and the
//! CLI's disposable-worktree materialisation
//! (`commands/git_worktree.rs`) are built through the same constructor;
//! no other module names the binary.

use std::io;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

/// A bare `git`, carrying whatever environment this process inherited.
/// The only legitimate caller is [`local_env_vars`], whose question —
/// "which variables does this git treat as repository-local?" — needs no
/// repository and must be answerable before any scope exists.
fn unscoped() -> Command {
    Command::new("git")
}

/// The variables that select a repository, as `git rev-parse
/// --local-env-vars` reports them: git's own answer, and the set git
/// itself clears before running a command in a different repository.
/// Read from the installed binary rather than frozen as a literal here,
/// so the seam tracks the git it is actually driving.
///
/// Probed once per process, caching only success — a failed probe leaves
/// the cell empty so a later call retries instead of treating one
/// transient spawn failure as "no git for the rest of this process".
fn local_env_vars() -> io::Result<&'static [String]> {
    static VARS: OnceLock<Vec<String>> = OnceLock::new();
    if let Some(vars) = VARS.get() {
        return Ok(vars);
    }
    let output = unscoped()
        .args(["rev-parse", "--local-env-vars"])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "git rev-parse --local-env-vars failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let vars: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    if vars.is_empty() {
        return Err(io::Error::other(
            "git rev-parse --local-env-vars listed nothing, so no repository scope can be \
             established",
        ));
    }
    Ok(VARS.get_or_init(|| vars))
}

/// A `git` invocation bound to the repository at `root`, and to no other.
///
/// A working directory only decides where git *starts* looking for a
/// repository. Git skips that search outright when `GIT_DIR` is set, and
/// consults `GIT_INDEX_FILE` / `GIT_OBJECT_DIRECTORY` / `GIT_COMMON_DIR`
/// whether or not it is — so an inherited environment silently redirects
/// an invocation at a repository other than the one being analysed.
/// Every git hook and every pre-commit runner exports exactly those
/// variables, which makes the inherited case the ordinary one rather
/// than the exotic one, and each consequence is a wrong answer wearing
/// the shape of a right one: drift counted against foreign history, a
/// baseline read from foreign bytes, a work-tree probe that answers
/// `true` for a directory holding no repository at all, and a `git
/// worktree add` that checks a foreign ref out into the project and
/// registers it in the foreign repository's metadata. Clearing the
/// variables is what makes `root` load-bearing.
///
/// `Err` when git cannot be invoked at all: the scope cannot be
/// established, so callers take the git-unavailable path they already
/// have rather than run an unscoped command.
pub fn command(root: &Path) -> io::Result<Command> {
    let mut command = unscoped();
    for var in local_env_vars()? {
        command.env_remove(var);
    }
    command.current_dir(root);
    Ok(command)
}

/// True if `root` is inside a git work tree. A non-erroring probe for
/// callers that treat absence as "skip" rather than "fail" — e.g. the
/// immutability baseline, which simply leaves the diff-aware rules to
/// self-report as skipped when there is no git history to diff against.
pub fn is_work_tree(root: &Path) -> bool {
    command(root)
        .and_then(|mut git| git.args(["rev-parse", "--is-inside-work-tree"]).output())
        .is_ok_and(|output| output.status.success())
}

/// The bytes of `rel_path` as committed at `git_ref`, or `None` when the
/// path does not exist there (or git is unavailable). The baseline view
/// the rewrite-lock probes diff against: it lets a write seam compute
/// exactly what a `check` against `rules.immutable_baseline` would —
/// the before-snapshot status and body fingerprint — so the seam skips
/// or refuses a mutation iff `check` would flag it. Callers gate on
/// [`is_work_tree`] first ([`crate::mutate::BaselineProbe`] does);
/// outside a work tree the diff-aware immutability rules are inert and
/// nothing is locked.
pub fn ref_file_content(root: &Path, git_ref: &str, rel_path: &Path) -> Option<String> {
    let output = command(root)
        .ok()?
        .args([
            "show",
            &format!("{git_ref}:{}", crate::path_guard::forward_string(rel_path)),
        ])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The constructor's whole contract: every variable this git calls
    /// repository-local is cleared, and `root` is the working directory.
    /// Asserted against git's live answer, so a variable the installed
    /// git knows about and the seam does not is a failure here rather
    /// than a silently mismeasured repository in the field.
    #[test]
    fn command_clears_every_repository_local_variable() {
        let root = tempfile::TempDir::new().unwrap();
        let git = command(root.path()).expect("git on PATH");
        let cleared: Vec<&str> = git
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key.to_str().expect("git variable names are UTF-8"))
            .collect();
        for var in local_env_vars().expect("git on PATH") {
            assert!(
                cleared.contains(&var.as_str()),
                "{var} is repository-local but not cleared: {cleared:?}"
            );
        }
        assert_eq!(git.get_current_dir(), Some(root.path()));
    }

    /// One module in the workspace names the `git` binary. A call site
    /// that spawns git directly reintroduces the cross-repository
    /// defect, and its symptom is a finding that quietly stops appearing
    /// rather than an error — invisible to review and to every lint the
    /// workspace runs — so the constraint is a gate, not a convention.
    #[test]
    fn nothing_outside_this_module_spawns_git() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the crate directory sits under the workspace root");
        // Composed at runtime so this assertion is not itself a match.
        let needle = format!("Command::new({:?})", "git");
        let mut modules = Vec::new();
        for member in ["nodex-core", "nodex-cli"] {
            collect_spawn_sites(
                &workspace.join(member).join("src"),
                &needle,
                workspace,
                &mut modules,
            );
        }
        modules.sort();
        assert_eq!(
            modules,
            ["nodex-core/src/git.rs"],
            "git must be spawned only through `git::command` — route these modules through it"
        );
    }

    fn collect_spawn_sites(dir: &Path, needle: &str, workspace: &Path, found: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("the source directory is readable") {
            let path = entry.expect("the directory entry is readable").path();
            if path.is_dir() {
                collect_spawn_sites(&path, needle, workspace, found);
            } else if path.extension().is_some_and(|ext| ext == "rs")
                && std::fs::read_to_string(&path)
                    .expect("the source file is readable")
                    .contains(needle)
            {
                found.push(
                    path.strip_prefix(workspace)
                        .expect("the source file lives under the workspace")
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
}
