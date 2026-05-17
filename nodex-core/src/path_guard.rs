//! Path-safety helpers shared between CLI commands that accept user-
//! controlled paths (scaffold, rename, migrate).
//!
//! The contract: every file nodex writes or moves must live under the
//! project root. An AI agent (or a hand-typed CLI invocation) must not
//! be able to make `nodex` scaffold/rename/migrate write into
//! `/etc/passwd` or a sibling project by crafting `../../...` paths.

use std::path::{Component, Path};

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
}
