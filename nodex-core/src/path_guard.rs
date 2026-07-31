//! Path-safety helpers shared between CLI commands that accept user-
//! controlled paths (scaffold, rename, migrate).
//!
//! The contract: every file nodex writes or moves must live under the
//! project root. An AI agent (or a hand-typed CLI invocation) must not
//! be able to make `nodex` scaffold/rename/migrate write into
//! `/etc/passwd` or a sibling project by crafting `../../...` paths.

use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};

/// Project-wide canonical path string for a path the filesystem gave
/// us: the platform's separators rendered as `/`. Used for JSON
/// output, glob keys, and any cross-platform comparison.
///
/// Which characters divide components is the platform's to say, and
/// [`std::path::is_separator`] is where it says it — `\` on Windows,
/// nowhere else. A path from the walk is a name that exists, so
/// rendering it has to be reversible: folding a character the platform
/// allows in a filename would put a path in the graph that no reader
/// can open, and every seam that reads a document by its recorded path
/// would skip it.
pub fn forward_string(path: &Path) -> String {
    path.to_string_lossy().replace(std::path::is_separator, "/")
}

/// Forward-slash normalisation for an authored path — a CLI argument, a
/// link destination — where the text is a spelling rather than a name
/// that exists.
///
/// Here `\` divides components whatever the host is: nodex's path
/// language is one language, so a document reads and writes the same
/// from either platform, and `\etc\passwd.md` is the drive-relative
/// shape on both rather than a filename on one of them. That is the
/// opposite of [`forward_string`]'s job, which renders a name the
/// filesystem already holds, and the two must not be shared.
///
/// Both endpoints cannot fold, so a document whose name holds a literal
/// `\` is graphed where it lives and no authored surface can name it: a
/// body link, `query node --path`, `rename`, `check --content` and
/// `scaffold --path` all read that spelling as a separator. Such a
/// document is reachable by id, and a link to it reports as unresolved.
pub fn forward_str(s: &str) -> String {
    s.replace('\\', "/")
}

/// Lexically resolve `.` / `..` / empty segments in a relative path
/// without touching the filesystem, returning the forward-slashed
/// remainder — or `None` when a `..` walks above the start (an escape).
/// The single normalisation primitive shared by link resolution and the
/// unresolved-edge disk probe, so both reject the same escaping paths
/// (symmetric guards) rather than each re-deriving containment.
pub fn normalize_relative(path: &Path) -> Option<String> {
    let forward = forward_string(path);
    let mut parts: Vec<&str> = Vec::new();
    for component in forward.split('/') {
        match component {
            "." | "" => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

/// Canonicalize a user-supplied document path for a mutation or
/// write-gate surface, returning the scanner's root-relative
/// forward-slashed form. One seam, four steps: fold `\` to `/` —
/// nodex's path language is forward-slashed on every platform; the
/// scanner's globs, the JSON serializer, and the lookup surface all
/// fold already, and a write seam that doesn't would materialize a
/// file the graph addresses under a different key — then refuse
/// traversal / absolute forms, then collapse `.` segments, then
/// refuse a spelling the filesystem does not use. Every downstream consumer (id
/// inference, scope probes, reference rewriting, the write itself)
/// keys on the result, so the probe verdict, the written artifact,
/// and the next scan can never disagree about which document was
/// named.
///
/// The spelling test lives here rather than at each seam because
/// every path a user names reaches a write through this one call, and
/// a guard a handler can forget is one a handler will forget: the
/// four surfaces that accept a document path (`scaffold --path`,
/// `rename`'s source and destination, `check --content`) are exactly
/// this function's callers.
pub fn normalize_doc_path(root: &Path, input: &str) -> Result<String> {
    let folded = forward_str(input);
    let p = Path::new(&folded);
    reject_traversal(p)?;
    let collapsed: PathBuf = p
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect();
    let normalized = forward_string(&collapsed);
    if let Some(spelling) = filesystem_spelling(root, &normalized)? {
        return Err(Error::Config(format!(
            "path {normalized:?} is spelled differently from the filesystem's own \
             {spelling:?} — this filesystem folds letter case or unicode normalization, and \
             nodex addresses a document by the spelling the scan reads from disk; use \
             {spelling:?}"
        )));
    }
    Ok(normalized)
}

/// The filesystem's own spelling of `rel`, when it differs from how
/// `rel` spells it — otherwise `None`.
///
/// A case-insensitive (APFS, NTFS) or normalization-insensitive
/// (HFS+, NFC/NFD) volume resolves two distinct spellings to one
/// directory entry, so a path can name an existing document while
/// sharing no byte string with it. Every comparison nodex makes is
/// exact — the scan's path index, the id-collision probe, the
/// immutability lock's baseline lookup, the resolution of a rewritten
/// link — so a folded spelling addresses a document that no lookup
/// finds while the write lands on the real file: how a frozen record
/// is overwritten by a "new" document, and how a rename rewrites
/// references onto a name the next scan never produces.
///
/// The test is the filesystem's own answer, component by component:
/// at each level the directory must list an entry named exactly as
/// `rel` spells it. A component that exists under no spelling ends
/// the walk — everything below it is new, and a path that does not
/// exist cannot alias one that does, so a genuinely new document is
/// never refused. A correctly spelled component is taken as read
/// without being resolved, so a symlink is judged by its spelling
/// like any other entry and a path through one stays legal. Only at
/// a component the volume folded is a canonical path consulted, to
/// name the entry it folded onto: two distinct entries never share
/// one, so the answer is the entry the write would hit.
fn filesystem_spelling(root: &Path, rel: &str) -> Result<Option<String>> {
    if rel.is_empty() {
        return Ok(None);
    }
    let segments: Vec<&str> = rel.split('/').collect();
    let mut at = root.to_path_buf();
    let mut spelled: Vec<String> = Vec::with_capacity(segments.len());
    let mut folded = false;

    for (depth, segment) in segments.iter().enumerate() {
        let candidate = at.join(segment);
        if std::fs::symlink_metadata(&candidate).is_err() {
            spelled.extend(segments[depth..].iter().map(|s| (*s).to_string()));
            break;
        }
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(&at).map_err(|source| Error::Io {
            path: at.clone(),
            source,
        })? {
            entries.push(
                entry
                    .map_err(|source| Error::Io {
                        path: at.clone(),
                        source,
                    })?
                    .file_name(),
            );
        }
        if entries.iter().any(|name| name.as_os_str() == *segment) {
            spelled.push((*segment).to_string());
            at = candidate;
            continue;
        }
        // Something answers to this name that the directory does not list
        // under it. Which entry the volume folded it onto is the one thing
        // the message must get right, so it is established rather than
        // guessed — and a location that resolves to nothing (a broken
        // symlink named in another case) is refused for the same reason
        // rather than let through unspelled.
        let target = std::fs::canonicalize(&candidate).map_err(|source| Error::Io {
            path: candidate.clone(),
            source,
        })?;
        let existing = entries
            .into_iter()
            .find(|name| std::fs::canonicalize(at.join(name)).is_ok_and(|c| c == target))
            .ok_or_else(|| {
                Error::Config(format!(
                    "path {rel:?} resolves at {:?} to a location the directory does not list \
                     under that name; use the spelling the directory lists",
                    forward_string(&candidate)
                ))
            })?;
        folded = true;
        spelled.push(existing.to_string_lossy().into_owned());
        at = at.join(existing);
    }

    Ok(folded.then(|| spelled.join("/")))
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
    let rel = if p.has_root() || input.starts_with('\\') {
        // Root-anchored paths must live under the project root or
        // the lookup is about a file outside the scanned project.
        // Root-anchored paths must live under the project root or
        // the lookup is about a file outside the scanned project.
        // `has_root` covers Unix absolute (`/etc/passwd`) and Windows
        // both fully-absolute (`C:\...`) and drive-relative; the explicit
        // leading-`\` check keeps that classification identical on Unix,
        // where `\` names no separator and the drive-relative shape would
        // otherwise read as a relative path and resolve project-relative —
        // exactly the re-interpretation this contract forbids.
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
    // What remains after the root prefix is the authored part, and only
    // that is folded: the prefix is the operating system's own spelling of
    // where the project sits, so folding it would rewrite a root that holds
    // a literal `\` into one that does not exist and refuse every absolute
    // lookup as escaping. Folding the remainder is what makes a
    // backslash-separated authoring form (`docs\sub\..\index.md`) collapse
    // identically on every platform: `Path::components()` would treat the
    // whole string as one segment on Unix and leave the `..` intact. The
    // intent is *path
    // equivalence* (`src/../src/lib.rs` ≡ `src/lib.rs`): any `..` that
    // would pop above the project root surfaces as `OutsideRoot`,
    // matching the symmetric-guard discipline mutation surfaces enforce
    // via `reject_traversal`, while a `..` absorbed by earlier segments
    // is honoured so a reverse lookup accepts any equivalent form an
    // editor / IDE produces.
    normalize_relative(Path::new(&forward_str(&rel.to_string_lossy())))
        .ok_or_else(|| Error::OutsideRoot(p.to_path_buf()))
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

/// Reject `target` unless — after resolving any symlinked ancestors
/// through the filesystem — it stays under the project `root`.
///
/// [`reject_traversal`] is the lexical half of this guard: it refuses
/// `..` and absolute forms in the user-supplied *relative* path. This
/// is the filesystem half: a lexically clean path can still escape
/// when an ancestor directory is a symlink pointing outside the
/// project (`docs/external -> /etc`). Both sides canonicalise their
/// deepest *existing* ancestor (the target may not exist yet — neither
/// may the root, during project bootstrap) and re-append the
/// not-yet-existing remainder, so a fresh nested target under a fresh
/// root is accepted while a symlinked-ancestor escape is not.
///
/// The check is check-time only (TOCTOU): a concurrent filesystem
/// mutation between this check and the subsequent write is not
/// defended against — the same honest boundary
/// [`write_atomic_in_root`] documents for its crash semantics.
pub fn reject_outside_root(root: &Path, target: &Path) -> Result<()> {
    let canonical_root = canonicalize_deepest_existing(root)
        .ok_or_else(|| Error::OutsideRoot(target.to_path_buf()))?;
    let canonical_target = canonicalize_deepest_existing(target)
        .ok_or_else(|| Error::OutsideRoot(target.to_path_buf()))?;
    if canonical_target.starts_with(&canonical_root) {
        Ok(())
    } else {
        Err(Error::OutsideRoot(target.to_path_buf()))
    }
}

/// Canonicalise the deepest existing ancestor of `path` and re-append
/// the not-yet-existing remainder. `None` when no ancestor
/// canonicalises (unreadable filesystem) or when the remainder
/// contains a `..` component — the filesystem cannot resolve a parent
/// hop through a directory that does not exist yet, and no legitimate
/// project write target contains `..`, so a lexical re-escape of the
/// canonical base is refused rather than guessed at.
fn canonicalize_deepest_existing(path: &Path) -> Option<PathBuf> {
    for ancestor in path.ancestors() {
        if let Ok(canonical) = std::fs::canonicalize(ancestor) {
            let remainder = path
                .strip_prefix(ancestor)
                .expect("Path::ancestors yields prefixes of the path");
            if remainder
                .components()
                .any(|c| matches!(c, Component::ParentDir))
            {
                return None;
            }
            return Some(canonical.join(remainder));
        }
    }
    None
}

/// Atomically write `content` to `target` by staging it at a unique
/// sibling temp path and renaming — the private staging half of
/// [`write_atomic_in_root`], which is the only public write primitive.
/// A crash mid-write leaves either the previous file intact or no file
/// at all — never a half-written one.
///
/// The temp name carries the process id and a per-call counter
/// (`<target>.<pid>.<n>.tmp`) so concurrent writers of the *same*
/// target — two `build`s, an editor racing a pre-commit hook — never
/// share a staging path. A fixed `.tmp` would let one writer's rename
/// consume the temp and the other's rename race to `ENOENT`; the
/// content is deterministic, so whichever rename lands last is correct.
/// The temp is removed if the write or rename fails, so a failed write
/// never litters the output directory.
///
/// Appending the suffix via [`std::ffi::OsString::push`] is mandatory:
/// `Path::with_extension` would *replace* everything after the last
/// `.` in the filename, clobbering paths whose basename already
/// contains a dot (`0001-v1.2.md` → `0001-v1.tmp`).
/// Content written to its staging file and waiting to be renamed into place.
///
/// The two halves of an atomic write, held apart so a *batch* of them can be
/// all-or-nothing. Everything that can fail — the directory does not exist or
/// is not writable, the disk is full — fails while staging, where nothing has
/// been replaced yet and dropping the staged writes leaves the tree as it was.
/// What remains is a same-directory rename per file, which is the atomic
/// primitive itself.
///
/// A staged write that is never committed removes its temp file on drop, so a
/// batch abandoned halfway litters nothing.
#[must_use = "a staged write does nothing until it is committed"]
pub struct Staged {
    tmp: std::path::PathBuf,
    target: std::path::PathBuf,
    committed: bool,
}

impl Staged {
    /// Rename the staged content into place.
    pub fn commit(mut self) -> Result<()> {
        match std::fs::rename(&self.tmp, &self.target) {
            Ok(()) => {
                self.committed = true;
                Ok(())
            }
            Err(e) => Err(Error::Io {
                path: self.target.clone(),
                source: e,
            }),
        }
    }

    /// Where the content will land.
    pub fn target(&self) -> &Path {
        &self.target
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.tmp);
        }
    }
}

/// Write `content` to a staging file beside `target`, ready to be renamed
/// into place by [`Staged::commit`].
fn stage_atomic(target: &Path, content: &str) -> Result<Staged> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    let nonce = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut tmp_os: std::ffi::OsString = target.as_os_str().to_os_string();
    tmp_os.push(format!(".{}.{nonce}.tmp", std::process::id()));
    let tmp = std::path::PathBuf::from(tmp_os);

    if let Err(e) = std::fs::write(&tmp, content) {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::Io {
            path: tmp,
            source: e,
        });
    }
    Ok(Staged {
        tmp,
        target: target.to_path_buf(),
        committed: false,
    })
}

/// The single public write primitive: an atomic staged write preceded
/// by the full write guard. The target must not itself be a symlink
/// (the staged rename would replace the link — never what writing a
/// file means; a symlinked artifact is loudly refused instead of
/// silently swapped for a regular file) and must stay under `root`
/// after resolving symlinked ancestors ([`reject_outside_root`], which
/// still accepts in-root symlinked directories). Every write routes
/// through here — document mutations (scaffold, lifecycle, migrate,
/// rename, retarget), infra artifacts (graph.json, GRAPH.md,
/// cache.json), and init's nodex.toml — so containment is a property
/// of the primitive rather than a per-handler obligation: no writer
/// can forget the guard (symmetric guards). `Config::validate_output`
/// is the lexical early-feedback half for `output.dir`; this is the
/// filesystem half, where a symlinked-ancestor escape is actually
/// detectable. Command seams still pre-classify symlinked paths for
/// their reader-follows / writer-skips warnings; this refusal is the
/// backstop for any caller that doesn't. The contract is exactly root
/// containment plus final-component symlink refusal — immutability-lock
/// consultation is owned by the mutation seams (`mutate::apply_to_file`,
/// `lifecycle::transition`, `scaffold`), never by this primitive.
pub fn write_atomic_in_root(root: &Path, target: &Path, content: &str) -> Result<()> {
    stage_in_root(root, target, content)?.commit()
}

/// [`write_atomic_in_root`] stopped one step short: the guard is applied and
/// the content is on disk beside its target, waiting for the rename that puts
/// it there.
///
/// This is what makes a multi-file write all-or-nothing. A batch stages every
/// file first, so the failures that actually happen — an unwritable directory,
/// a full disk — happen while the tree is still untouched and every staged
/// write is dropped; only then does it commit, and a commit is a
/// same-directory rename. Without it a batch could pass its gate, write half
/// of itself, and leave the project in a state nothing had judged.
pub fn stage_in_root(root: &Path, target: &Path, content: &str) -> Result<Staged> {
    if is_symlink(target) {
        return Err(Error::OutsideRoot(target.to_path_buf()));
    }
    reject_outside_root(root, target)?;
    stage_atomic(target, content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[cfg(unix)]
    #[test]
    fn write_atomic_in_root_refuses_symlink_target() {
        // The primitive itself refuses a symlink target — replacing the
        // link is never what a document mutation means, regardless of
        // where it points — so a caller that forgets the seam-level
        // pre-classification still cannot write onto one.
        use std::os::unix::fs as unix_fs;
        let root = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();

        let external = outside.path().join("external.md");
        std::fs::write(&external, "original").unwrap();
        let out_link = root.path().join("out.md");
        unix_fs::symlink(&external, &out_link).unwrap();
        let err = write_atomic_in_root(root.path(), &out_link, "new").unwrap_err();
        assert!(matches!(err, Error::OutsideRoot(_)));
        assert_eq!(std::fs::read_to_string(&external).unwrap(), "original");
        assert!(is_symlink(&out_link), "the link itself survives");

        // An in-root target is refused just the same — the guard is
        // about the link, not only about escape.
        let internal = root.path().join("internal.md");
        std::fs::write(&internal, "original").unwrap();
        let in_link = root.path().join("in.md");
        unix_fs::symlink(&internal, &in_link).unwrap();
        let err = write_atomic_in_root(root.path(), &in_link, "new").unwrap_err();
        assert!(matches!(err, Error::OutsideRoot(_)));
        assert_eq!(std::fs::read_to_string(&internal).unwrap(), "original");
    }

    #[test]
    fn write_atomic_survives_concurrent_writers_of_one_target() {
        // Many writers staging the same target must all succeed — with a
        // fixed `.tmp` one writer's rename consumed the temp and the rest
        // raced to ENOENT. The content is deterministic, so last-rename-
        // wins is correct, and no `.tmp` is left behind.
        use std::sync::Arc;
        let dir = tempfile::TempDir::new().unwrap();
        let target = Arc::new(dir.path().join("graph.json"));
        let handles: Vec<_> = (0..16)
            .map(|_| {
                let t = Arc::clone(&target);
                std::thread::spawn(move || {
                    write_atomic_in_root(t.parent().unwrap(), &t, "deterministic")
                })
            })
            .collect();
        for h in handles {
            h.join()
                .unwrap()
                .expect("concurrent write_atomic must not race to an error");
        }
        assert_eq!(std::fs::read_to_string(&*target).unwrap(), "deterministic");
        let strays: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(
            strays.is_empty(),
            "no temp file left after successful writes: {strays:?}"
        );
    }

    #[test]
    fn normalize_relative_resolves_dotdot() {
        assert_eq!(
            normalize_relative(Path::new("docs/decisions/../guides/setup.md")).as_deref(),
            Some("docs/guides/setup.md")
        );
        assert_eq!(
            normalize_relative(Path::new("a/b/c/../../d.md")).as_deref(),
            Some("a/d.md")
        );
        assert_eq!(
            normalize_relative(Path::new("./a/./b.md")).as_deref(),
            Some("a/b.md")
        );
    }

    #[test]
    fn normalize_relative_rejects_escape() {
        assert_eq!(normalize_relative(Path::new("../escape.md")), None);
        assert_eq!(normalize_relative(Path::new("a/../../escape.md")), None);
    }

    #[test]
    fn write_atomic_preserves_dotted_basename() {
        let tmpdir =
            std::env::temp_dir().join(format!("nodex-path-guard-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmpdir);
        std::fs::create_dir_all(&tmpdir).unwrap();
        let target = tmpdir.join("0001-v1.2.md");
        write_atomic_in_root(target.parent().unwrap(), &target, "hello").unwrap();
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
        write_atomic_in_root(target.parent().unwrap(), &target, "hi").unwrap();
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
    fn reject_outside_root_accepts_nonexistent_nested_target() {
        // scaffold's normal case: the target (and possibly intermediate
        // directories) don't exist yet under an existing root.
        let root = std::env::temp_dir().join(format!("nodex-guard-nested-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("docs").join("new").join("doc.md");
        assert!(reject_outside_root(&root, &target).is_ok());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn reject_outside_root_accepts_nonexistent_root() {
        // Project-bootstrap regression guard: the root itself may not
        // exist yet (init into a fresh directory). The guard must not
        // require an existing root to accept a target under it.
        let root =
            std::env::temp_dir().join(format!("nodex-guard-freshroot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let target = root.join("docs").join("doc.md");
        assert!(reject_outside_root(&root, &target).is_ok());
    }

    #[test]
    fn reject_outside_root_rejects_symlinked_ancestor_escape() {
        // A lexically clean relative path whose ancestor directory is a
        // symlink pointing outside the project — the case
        // `reject_traversal` cannot see.
        #[cfg(unix)]
        {
            let base =
                std::env::temp_dir().join(format!("nodex-guard-symlink-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            let root = base.join("project");
            let outside = base.join("outside");
            std::fs::create_dir_all(&root).unwrap();
            std::fs::create_dir_all(&outside).unwrap();
            std::os::unix::fs::symlink(&outside, root.join("docs")).unwrap();

            let target = root.join("docs").join("doc.md");
            let err = reject_outside_root(&root, &target).unwrap_err();
            assert!(matches!(err, Error::OutsideRoot(_)));
            std::fs::remove_dir_all(&base).ok();
        }
    }

    #[test]
    fn reject_outside_root_accepts_symlinked_prefix_root() {
        // macOS `/tmp` → `/private/tmp`: the *root itself* reached
        // through a symlinked prefix must still accept its own
        // children, because both sides canonicalise.
        if !std::path::Path::new("/private/tmp").is_dir() {
            return; // not a macOS-style layout — skip rather than fake
        }
        let real = std::path::Path::new("/private/tmp")
            .join(format!("nodex-guard-prefix-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&real);
        std::fs::create_dir_all(&real).unwrap();
        let via_symlink =
            std::path::PathBuf::from("/tmp").join(real.strip_prefix("/private/tmp").unwrap());
        let target = via_symlink.join("docs").join("doc.md");
        assert!(reject_outside_root(&via_symlink, &target).is_ok());
        // Mixed forms agree too: symlinked root, canonical target.
        assert!(reject_outside_root(&via_symlink, &real.join("doc.md")).is_ok());
        std::fs::remove_dir_all(&real).ok();
    }

    #[test]
    fn reject_outside_root_rejects_parent_hop_in_nonexistent_remainder() {
        // A `..` inside a not-yet-existing suffix cannot be resolved by
        // the filesystem; refusing it keeps the guard sound even when a
        // caller skipped the lexical `reject_traversal` pre-check.
        let root = std::env::temp_dir().join(format!("nodex-guard-dotdot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("ghost").join("..").join("..").join("escape.md");
        let err = reject_outside_root(&root, &target).unwrap_err();
        assert!(matches!(err, Error::OutsideRoot(_)));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    #[cfg(unix)]
    fn normalize_for_lookup_folds_the_authored_part_not_the_root_prefix() {
        // The prefix is the operating system's own spelling of where the
        // project sits, and only a platform that allows `\` in a name can
        // hold one literally. It still strips, and the remainder — the
        // authored part — still folds.
        let root = std::path::Path::new("/tmp/rev\\root");
        assert_eq!(
            normalize_for_lookup("/tmp/rev\\root/docs/a.md", root).unwrap(),
            "docs/a.md"
        );
        assert_eq!(
            normalize_for_lookup("/tmp/rev\\root/docs\\a.md", root).unwrap(),
            "docs/a.md"
        );
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
        // ...and `.` / `..` collapse through backslash separators too —
        // the fold happens before the lexical collapse, so a Windows-form
        // path normalises identically on every platform (on Unix the old
        // `Path::components()` left `..` intact because `\` isn't a
        // separator there).
        assert_eq!(
            normalize_for_lookup("docs\\sub\\..\\a.md", root).unwrap(),
            "docs/a.md"
        );
        assert_eq!(
            normalize_for_lookup(".\\docs\\a.md", root).unwrap(),
            "docs/a.md"
        );
        // A backslash `..` that escapes the root is still refused.
        assert!(normalize_for_lookup("..\\outside.md", root).is_err());
        // A backslash-ROOTED form is the Windows drive-relative shape —
        // anchored outside the project, never re-interpreted as
        // project-relative (identical classification on Unix, where `\`
        // is not a separator and the input would otherwise fold into a
        // resolving relative path).
        assert!(normalize_for_lookup("\\etc\\passwd.md", root).is_err());
        assert!(normalize_for_lookup("\\docs\\a.md", root).is_err());
    }
}
