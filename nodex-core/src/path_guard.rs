//! Path-safety helpers shared between CLI commands that accept user-
//! controlled paths (scaffold, rename, migrate).
//!
//! The contract: every file nodex writes or moves must live under the
//! project root. An AI agent (or a hand-typed CLI invocation) must not
//! be able to make `nodex` scaffold/rename/migrate write into
//! `/etc/passwd` or a sibling project by crafting `../../...` paths.

use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};

/// Project-wide canonical path string: always forward slashes,
/// regardless of platform. Used for JSON output, glob keys, and any
/// cross-platform comparison.
pub fn forward_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Forward-slash variant for raw user-authored path strings where
/// conversion through [`Path`] would be redundant.
pub fn forward_str(s: &str) -> String {
    s.replace('\\', "/")
}

/// Reject a relative path if it contains any parent (`..`) or root (`/`)
/// component, or if it is absolute. A valid nodex path stays inside
/// the project root by construction — even partial traversal that
/// would later be cancelled by descent is forbidden, because there is
/// no legitimate reason for a document path to contain `..`.
pub fn reject_traversal(rel_path: &Path) -> Result<()> {
    if rel_path.is_absolute() {
        return Err(Error::OutsideRoot(rel_path.to_path_buf()));
    }
    for component in rel_path.components() {
        match component {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(Error::OutsideRoot(rel_path.to_path_buf()));
            }
            Component::Normal(_) | Component::CurDir => {}
        }
    }
    Ok(())
}

/// Normalise a user-supplied path into the project-relative,
/// forward-slashed form nodex stores on nodes. Strips a single
/// leading `./`, converts a root-anchored path under `root` to its
/// relative remainder, and forward-slashes the result. Returns
/// [`Error::OutsideRoot`] if a root-anchored path falls outside the
/// project root.
///
/// Uses [`Path::has_root`] rather than [`Path::is_absolute`] so the
/// Windows "drive-relative" form (`/etc/passwd`, `\etc\passwd`) is
/// classified the same as a Unix absolute path — both are anchored
/// outside the project root and must not be re-interpreted as
/// project-relative just because Windows lacks a drive letter.
///
/// Designed for read-only lookups (`query node --path`) where the
/// caller may have a `cwd`-relative, absolute, or root-relative path
/// in hand — editors and IDE integrations supply each form
/// interchangeably. Mutation surfaces should use
/// [`reject_traversal`] instead and refuse anything that isn't
/// already project-relative.
pub fn normalize_for_lookup(input: &str, root: &Path) -> Result<String> {
    let p = Path::new(input);
    let rel = if p.has_root() {
        // Root-anchored paths must live under the project root or
        // the lookup is about a file outside the scanned project.
        // `has_root` covers Unix absolute (`/etc/passwd`) and Windows
        // both fully-absolute (`C:\...`) and drive-relative
        // (`/etc/passwd`, `\etc\passwd`) — none of those are legal
        // project-relative inputs.
        //
        // Literal `strip_prefix` first (fast path, no syscall). If
        // that fails the input may still be inside root reached
        // through a symlinked prefix — `/tmp/proj/...` vs canonical
        // `/private/tmp/proj/...` on macOS, or `/var/...` vs `/private/var/...`,
        // or any junction on Windows. Fall back to canonicalising
        // both sides; if either canonicalise fails (file missing,
        // unreadable) the lookup can't be inside the project →
        // `OutsideRoot` is the honest diagnostic.
        strip_within_root(p, root).ok_or_else(|| Error::OutsideRoot(p.to_path_buf()))?
    } else if input == "." {
        Path::new("").to_path_buf()
    } else {
        // `./prefix` and other relative forms fall through to lexical
        // normalisation below — no need to special-case the prefix.
        p.to_path_buf()
    };
    // Lexical normalisation: collapse `.` and `..` components without
    // touching the filesystem. The intent here is *path equivalence*
    // (`src/../src/lib.rs` ≡ `src/lib.rs`), not traversal sloppiness:
    // any `..` that would pop above the project root surfaces as
    // `OutsideRoot`, matching the symmetric-guard discipline
    // mutation surfaces enforce via `reject_traversal`. Lookups using
    // `..` that stay inside the project (because earlier `Normal`
    // components absorb the pop) are honoured — a graph reverse
    // lookup is useful only when callers can phrase the same path in
    // any equivalent form their editor / IDE happens to produce.
    let mut out: Vec<Component<'_>> = Vec::new();
    for component in rel.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if out.pop().is_none() {
                    return Err(Error::OutsideRoot(p.to_path_buf()));
                }
            }
            // After the `has_root` strip + `..` checks, any root /
            // prefix component is a bug — surface it loudly instead
            // of letting a misshapen lookup silently succeed.
            Component::RootDir | Component::Prefix(_) => {
                return Err(Error::OutsideRoot(p.to_path_buf()));
            }
            Component::Normal(_) => out.push(component),
        }
    }
    let collapsed: PathBuf = out.iter().collect();
    Ok(forward_string(&collapsed))
}

/// Strip the project root from a root-anchored input path, handling
/// symlink-equivalent prefixes (macOS `/tmp` → `/private/tmp`, Linux
/// `/var` → `/private/var`, Windows junctions). Returns `None` when
/// the input — under any canonicalisation reachable from the
/// filesystem — does not live under `root`.
fn strip_within_root(input: &Path, root: &Path) -> Option<PathBuf> {
    if let Ok(rel) = input.strip_prefix(root) {
        return Some(rel.to_path_buf());
    }
    // Symlink-equivalent fallback. Canonicalise both sides via the
    // filesystem; if either side can't be canonicalised (missing
    // file, unreadable directory) the input is not addressable as a
    // path under the project, so no relative form exists.
    let canonical_root = std::fs::canonicalize(root).ok()?;
    let canonical_input = std::fs::canonicalize(input).ok()?;
    canonical_input
        .strip_prefix(&canonical_root)
        .ok()
        .map(PathBuf::from)
}

/// Return `true` when the given absolute path is a symlink.
pub fn is_symlink(abs_path: &Path) -> bool {
    std::fs::symlink_metadata(abs_path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Atomically write `content` to `target` by staging it at `<target>.tmp`
/// and renaming. A crash mid-write leaves either the previous file
/// intact or no file at all — never a half-written one.
///
/// Co-located with the other filesystem safety helpers so every
/// frontmatter-mutating command (scaffold, lifecycle, migrate-style
/// appliers) routes through the same primitive and cannot accidentally
/// fall back to plain `fs::write`.
///
/// Appending `.tmp` via [`std::ffi::OsString::push`] is mandatory:
/// `Path::with_extension` would *replace* everything after the last
/// `.` in the filename, clobbering paths whose basename already
/// contains a dot (`0001-v1.2.md` → `0001-v1.tmp`).
pub fn write_atomic(target: &Path, content: &str) -> Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    let mut tmp_os: std::ffi::OsString = target.as_os_str().to_os_string();
    tmp_os.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp_os);
    std::fs::write(&tmp, content).map_err(|e| Error::Io {
        path: tmp.clone(),
        source: e,
    })?;
    std::fs::rename(&tmp, target).map_err(|e| Error::Io {
        path: target.to_path_buf(),
        source: e,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn write_atomic_preserves_dotted_basename() {
        let tmpdir =
            std::env::temp_dir().join(format!("nodex-path-guard-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmpdir);
        std::fs::create_dir_all(&tmpdir).unwrap();
        let target = tmpdir.join("0001-v1.2.md");
        write_atomic(&target, "hello").unwrap();
        assert!(target.exists());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
        // `Path::with_extension` would have produced "0001-v1.tmp"; verify
        // none of those cousin files remained.
        let leftovers: Vec<_> = std::fs::read_dir(&tmpdir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
        std::fs::remove_dir_all(&tmpdir).ok();
    }

    #[test]
    fn write_atomic_creates_parent_directories() {
        let tmpdir =
            std::env::temp_dir().join(format!("nodex-path-guard-mkdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmpdir);
        let target = tmpdir.join("nested").join("dirs").join("doc.md");
        write_atomic(&target, "hi").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hi");
        std::fs::remove_dir_all(&tmpdir).ok();
    }

    #[test]
    fn rejects_parent_dir() {
        assert!(reject_traversal(&PathBuf::from("../evil.md")).is_err());
        assert!(reject_traversal(&PathBuf::from("docs/../../evil.md")).is_err());
    }

    #[test]
    fn rejects_absolute() {
        assert!(reject_traversal(&PathBuf::from("/etc/passwd")).is_err());
    }

    #[test]
    fn accepts_legitimate() {
        assert!(reject_traversal(&PathBuf::from("docs/a.md")).is_ok());
        assert!(reject_traversal(&PathBuf::from("./docs/a.md")).is_ok());
        assert!(reject_traversal(&PathBuf::from("a.md")).is_ok());
    }

    #[test]
    fn normalize_for_lookup_strips_dot_slash_prefix() {
        let root = std::path::Path::new("/project");
        assert_eq!(
            normalize_for_lookup("./docs/a.md", root).unwrap(),
            "docs/a.md"
        );
    }

    #[test]
    fn normalize_for_lookup_strips_root_prefix_from_absolute() {
        let root = std::path::Path::new("/project");
        assert_eq!(
            normalize_for_lookup("/project/docs/a.md", root).unwrap(),
            "docs/a.md"
        );
    }

    #[test]
    fn normalize_for_lookup_rejects_absolute_outside_root() {
        let root = std::path::Path::new("/project");
        let err = normalize_for_lookup("/etc/passwd", root).unwrap_err();
        assert!(matches!(err, Error::OutsideRoot(_)));
    }

    #[test]
    fn normalize_for_lookup_passes_project_relative_unchanged() {
        let root = std::path::Path::new("/project");
        assert_eq!(
            normalize_for_lookup("docs/a.md", root).unwrap(),
            "docs/a.md"
        );
    }

    #[test]
    fn normalize_for_lookup_rejects_parent_dir_traversal() {
        // Symmetric guard with `reject_traversal`: a read-only lookup
        // with `..` indicates the caller is addressing outside the
        // indexed set, which is the same failure mode mutation
        // surfaces reject. Surfaces as `OutsideRoot` so the consumer
        // sees `PATH_ESCAPES_ROOT`, not `NOT_FOUND`.
        let root = std::path::Path::new("/project");
        let err = normalize_for_lookup("../foo.md", root).unwrap_err();
        assert!(matches!(err, Error::OutsideRoot(_)));
    }

    #[test]
    fn normalize_for_lookup_collapses_inproject_parent_dir() {
        // Lexical normalisation: `..` that stays inside the project is
        // *path equivalence*, not escape. `src/../src/lib.rs` is the
        // same file as `src/lib.rs`. The editor / IDE producing either
        // form must hit the same node on reverse lookup.
        let root = std::path::Path::new("/project");
        assert_eq!(
            normalize_for_lookup("src/../src/lib.rs", root).unwrap(),
            "src/lib.rs"
        );
        assert_eq!(
            normalize_for_lookup("docs/./a.md", root).unwrap(),
            "docs/a.md"
        );
    }

    #[test]
    fn normalize_for_lookup_rejects_escape_via_repeated_parent_dir() {
        // `..` that pops above the project root surfaces as OutsideRoot
        // — same diagnostic as a literal `/etc/passwd` would produce.
        let root = std::path::Path::new("/project");
        let err = normalize_for_lookup("docs/../../etc/passwd", root).unwrap_err();
        assert!(matches!(err, Error::OutsideRoot(_)));
    }

    #[test]
    fn strip_within_root_handles_symlinked_prefix() {
        // macOS `/tmp` is a symlink to `/private/tmp`; the canonical
        // form differs from the user-typed form. `strip_within_root`
        // must accept either: a real consumer (editor, IDE) supplies
        // whichever the OS gives them. Linux has the equivalent `/var`
        // → `/private/var`; Windows has junctions. This test runs only
        // on macOS where the symlink is reliably present.
        if !std::path::Path::new("/private/tmp").is_dir() {
            return; // not a macOS-style layout — skip rather than fake
        }
        let tmp_real = std::path::Path::new("/private/tmp");
        let project = tmp_real.join(format!("nodex-strip-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&project);
        std::fs::create_dir_all(project.join("docs")).unwrap();
        std::fs::write(project.join("docs/a.md"), "x").unwrap();

        // User passes the symlink-prefixed form (`/tmp/...`);
        // canonical root passed in is `/private/tmp/...`.
        let unsealed_input = std::path::PathBuf::from("/tmp")
            .join(project.strip_prefix("/private/tmp").unwrap())
            .join("docs/a.md");
        let rel = super::strip_within_root(&unsealed_input, &project)
            .expect("symlink-equivalent input must strip");
        assert_eq!(rel, std::path::PathBuf::from("docs/a.md"));
        std::fs::remove_dir_all(&project).ok();
    }

    #[test]
    fn normalize_for_lookup_forward_slashes_backslashes() {
        let root = std::path::Path::new("/project");
        // Windows-style separators in user input fold to forward slashes
        // so cross-platform consumers can pass either form.
        assert_eq!(
            normalize_for_lookup("docs\\a.md", root).unwrap(),
            "docs/a.md"
        );
    }
}
