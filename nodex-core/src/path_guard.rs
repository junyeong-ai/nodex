//! Path-safety helpers shared between CLI commands that accept user-
//! controlled paths (scaffold, rename, migrate).
//!
//! The contract: every file nodex writes or moves must live under the
//! project root. An AI agent (or a hand-typed CLI invocation) must not
//! be able to make `nodex` scaffold/rename/migrate write into
//! `/etc/passwd` or a sibling project by crafting `../../...` paths.

use std::path::{Component, Path};

use crate::error::{Error, Result};

/// Reject a relative path if it contains any parent (`..`) or root (`/`)
/// component, or if it is absolute. A valid nodex path stays inside
/// the project root by construction — even partial traversal that
/// would later be cancelled by descent is forbidden, because there is
/// no legitimate reason for a document path to contain `..`.
pub fn reject_traversal(rel_path: &Path) -> Result<()> {
    if rel_path.is_absolute() {
        return Err(Error::PathEscapesRoot {
            path: rel_path.to_path_buf(),
        });
    }
    for component in rel_path.components() {
        match component {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(Error::PathEscapesRoot {
                    path: rel_path.to_path_buf(),
                });
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
