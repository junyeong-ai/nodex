use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs::FileType;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

/// Directory names and terminal types observed during one evaluation.
#[derive(Default)]
pub(crate) struct FileProbe {
    observed: Mutex<Observed>,
}

#[derive(Default)]
struct Observed {
    directories: HashMap<PathBuf, HashSet<OsString>>,
    types: HashMap<PathBuf, Option<FileType>>,
}

impl FileProbe {
    /// Absent or disallowed types need no directory enumeration. Existing targets
    /// must match every component's exact entry name on any filesystem.
    /// Terminal metadata follows symlinks; only path-only references admit directories.
    pub(crate) fn holds(&self, root: &Path, rel: &Path, admit_dirs: bool) -> bool {
        if rel.as_os_str().is_empty()
            || !rel
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
        {
            return false;
        }
        let mut observed = self.observed.lock().expect("file probe lock poisoned");
        let path = root.join(rel);
        let kind = match observed.types.get(&path) {
            Some(kind) => *kind,
            None => {
                let Some(kind) = std::fs::metadata(&path)
                    .ok()
                    .map(|metadata| metadata.file_type())
                else {
                    observed.types.insert(path, None);
                    return false;
                };
                if !kind.is_file() && !(admit_dirs && kind.is_dir()) {
                    return false;
                }
                let mut at = root.to_path_buf();
                for name in rel.iter() {
                    let names = observed.directories.entry(at.clone()).or_insert_with(|| {
                        std::fs::read_dir(&at)
                            .into_iter()
                            .flatten()
                            .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
                            .collect()
                    });
                    if !names.contains(name) {
                        observed.types.insert(path, None);
                        return false;
                    }
                    at.push(name);
                }
                observed.types.insert(path, Some(kind));
                Some(kind)
            }
        };
        kind.is_some_and(|kind| kind.is_file() || (admit_dirs && kind.is_dir()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::scanner::{ProjectFiles, Proposed};

    #[test]
    fn directory_observations_cover_distinct_names_and_end_with_the_evaluation() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("docs")).unwrap();
        std::fs::write(root.path().join("docs/present.md"), "content").unwrap();
        let probe = FileProbe::default();
        assert!(probe.holds(root.path(), Path::new("docs/present.md"), false));
        std::fs::write(root.path().join("docs/added.md"), "content").unwrap();
        assert!(!probe.holds(root.path(), Path::new("docs/added.md"), false));
        assert!(probe.holds(root.path(), Path::new("docs/present.md"), false));
        assert!(FileProbe::default().holds(root.path(), Path::new("docs/added.md"), false));
    }

    #[test]
    fn terminal_types_preserve_file_and_directory_admission() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("docs")).unwrap();
        let file = root.path().join("docs/present.md");
        std::fs::write(&file, "content").unwrap();
        let probe = FileProbe::default();
        assert!(!probe.holds(root.path(), Path::new("docs"), false));
        assert!(probe.holds(root.path(), Path::new("docs"), true));
        assert!(probe.holds(root.path(), Path::new("docs/present.md"), false));
        assert!(probe.holds(root.path(), Path::new("docs/present.md"), true));
        std::fs::remove_file(file).unwrap();
        assert!(probe.holds(root.path(), Path::new("docs/present.md"), false));
        assert!(!FileProbe::default().holds(root.path(), Path::new("docs/present.md"), false));
    }

    #[test]
    fn spelling_and_path_boundaries_are_exact() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("docs/Guides")).unwrap();
        std::fs::write(root.path().join("docs/Guides/Intro.md"), "content").unwrap();
        let probe = FileProbe::default();
        assert!(probe.holds(root.path(), Path::new("docs/Guides/Intro.md"), false));
        for path in [
            "docs/guides/Intro.md",
            "docs/Guides/intro.md",
            "Docs/Guides/Intro.md",
            "./docs/Guides/Intro.md",
            "../docs/Guides/Intro.md",
            "",
        ] {
            assert!(!probe.holds(root.path(), Path::new(path), false), "{path}");
        }
        assert!(!probe.holds(
            root.path(),
            &root.path().join("docs/Guides/Intro.md"),
            false
        ));
    }

    #[test]
    fn proposals_override_cached_presence_absence_and_directory_types() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("removed.md"), "content").unwrap();
        let probe = FileProbe::default();
        assert!(probe.holds(root.path(), Path::new("removed.md"), false));
        assert!(!probe.holds(root.path(), Path::new("new/nested/added.md"), false));
        let overlay = [
            (PathBuf::from("removed.md"), Proposed::Absent),
            (
                PathBuf::from("new/nested/added.md"),
                Proposed::Content(String::new()),
            ),
        ];
        let proposed = ProjectFiles::proposed(root.path(), &overlay).with_probe(&probe);
        assert!(!proposed.holds(Path::new("removed.md"), true));
        assert!(proposed.holds(Path::new("new/nested/added.md"), false));
        assert!(proposed.holds(Path::new("new"), true));
        assert!(proposed.holds(Path::new("new/nested"), true));
        assert!(!proposed.holds(Path::new("new"), false));
        assert!(!proposed.holds(Path::new("new/nested/missing.md"), false));
        let standing = ProjectFiles::working_tree(root.path()).with_probe(&probe);
        assert!(standing.holds(Path::new("removed.md"), false));
        assert!(!standing.holds(Path::new("new"), true));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_keep_the_read_planes_terminal_semantics() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("present.md"), "content").unwrap();
        symlink(outside.path(), root.path().join("linked")).unwrap();
        symlink(outside.path().join("present.md"), root.path().join("file")).unwrap();
        symlink(
            outside.path().join("absent.md"),
            root.path().join("dangling"),
        )
        .unwrap();
        let socket = std::os::unix::net::UnixListener::bind(root.path().join("socket")).unwrap();
        let probe = FileProbe::default();
        assert!(!probe.holds(root.path(), Path::new("linked"), false));
        assert!(probe.holds(root.path(), Path::new("linked"), true));
        assert!(probe.holds(root.path(), Path::new("linked/present.md"), false));
        assert!(!probe.holds(root.path(), Path::new("Linked/present.md"), false));
        assert!(probe.holds(root.path(), Path::new("file"), false));
        assert!(!probe.holds(root.path(), Path::new("file/child.md"), false));
        assert!(!probe.holds(root.path(), Path::new("dangling"), true));
        assert!(!probe.holds(root.path(), Path::new("socket"), true));
        drop(socket);
    }
}
