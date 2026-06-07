//! The single guarded write seam for in-scope document mutations.
//!
//! Every batch command that rewrites existing files (`rename`,
//! `retarget`) routes each file through [`apply_to_file`], so the
//! "writer-skips / reader-follows" symlink discipline and the atomic,
//! root-contained write live in exactly one place. A future fourth
//! rewrite command cannot forget the guard: it has nowhere else to
//! write through.

use std::path::Path;

use crate::error::Result;
use crate::path_guard;

/// Outcome of applying a transform to one in-scope file.
pub enum FileOutcome {
    /// The transform produced new content and it was written atomically.
    Rewritten,
    /// The transform produced no change; the file was left untouched.
    Unchanged,
    /// The file was not written, with a warning explaining why — its
    /// path is or resolves through a symlink and the transform *would*
    /// have changed it (read through, never written through), or it is
    /// a real file that could not be read.
    Skipped(String),
}

/// Apply `transform` to the file at `rel_path` under the project's write
/// guard, and report what happened.
///
/// A path that is — or resolves through — a symlink (the scanner
/// legitimately follows symlinked directories on read) is read through
/// to detect a pending change but never written through, since the
/// target could escape the project root: a pending change yields
/// [`FileOutcome::Skipped`] carrying `skip_message`, never a batch
/// abort, so one refused file cannot strand a half-applied rename or
/// retarget. An *unreadable* such path is [`FileOutcome::Unchanged`]:
/// it cannot demonstrate a pending change, would never receive a write
/// either way, and the build's read stage already surfaces unreadable
/// in-scope files. A real file is rewritten through
/// [`path_guard::write_atomic_in_root`] when the transform returns
/// `Some`, and left untouched on `None`; its read error is surfaced as
/// `Skipped`, never a hard failure.
pub fn apply_to_file(
    root: &Path,
    rel_path: &Path,
    transform: impl FnOnce(&str) -> Result<Option<String>>,
    skip_message: impl FnOnce() -> String,
) -> Result<FileOutcome> {
    let abs = root.join(rel_path);

    if path_guard::is_symlink(&abs) || path_guard::reject_outside_root(root, &abs).is_err() {
        if let Ok(content) = std::fs::read_to_string(&abs)
            && transform(&content)?.is_some()
        {
            return Ok(FileOutcome::Skipped(skip_message()));
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
            path_guard::write_atomic_in_root(root, &abs, &rewritten)?;
            Ok(FileOutcome::Rewritten)
        }
        None => Ok(FileOutcome::Unchanged),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    fn upcase_if_lower(content: &str) -> Result<Option<String>> {
        let up = content.to_uppercase();
        Ok((up != content).then_some(up))
    }

    #[test]
    fn rewrites_a_real_file_atomically() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.md"), "body").unwrap();

        let outcome = apply_to_file(dir.path(), Path::new("a.md"), upcase_if_lower, || {
            unreachable!("no symlink involved")
        })
        .unwrap();

        assert!(matches!(outcome, FileOutcome::Rewritten));
        assert_eq!(fs::read_to_string(dir.path().join("a.md")).unwrap(), "BODY");
    }

    #[test]
    fn leaves_file_untouched_when_transform_is_a_no_op() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.md"), "BODY").unwrap();

        let outcome = apply_to_file(dir.path(), Path::new("a.md"), upcase_if_lower, || {
            unreachable!("no symlink involved")
        })
        .unwrap();

        assert!(matches!(outcome, FileOutcome::Unchanged));
        assert_eq!(fs::read_to_string(dir.path().join("a.md")).unwrap(), "BODY");
    }

    #[test]
    fn unreadable_file_is_skipped_with_warning_not_error() {
        let dir = TempDir::new().unwrap();
        let outcome = apply_to_file(dir.path(), Path::new("missing.md"), upcase_if_lower, || {
            unreachable!("no symlink involved")
        })
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

        let outcome = apply_to_file(dir.path(), Path::new("link.md"), upcase_if_lower, || {
            "link.md skipped".to_string()
        })
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

        let outcome = apply_to_file(
            dir.path(),
            Path::new("linked/external.md"),
            upcase_if_lower,
            || "linked dir skipped".to_string(),
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

        let outcome = apply_to_file(dir.path(), Path::new("link.md"), upcase_if_lower, || {
            unreachable!("a no-op transform never warns")
        })
        .unwrap();

        assert!(matches!(outcome, FileOutcome::Unchanged));
    }
}
