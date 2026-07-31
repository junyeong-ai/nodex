use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::config::{Config, ScopeConfig};
use crate::error::{Error, Result};

/// The exact slice of [`Config`] that decides scope membership: the
/// `[scope]` block, the output directory (whose self-exclusion glob is
/// derived below), and — only when a `conditional_exclude` rule can
/// consult it — the terminal status vocabulary. Public scan functions
/// project into this view immediately and every private helper takes
/// `&ScanConfig`, so a new membership-affecting option cannot be read
/// without surfacing in the hashed projection
/// (`builder::graph_config_hash`) — the same compiler-enforcement
/// story as `parser::ParseConfig`. `terminal` and `initial_status` are
/// `None` when `scope.conditional_exclude` is empty, so retuning the
/// status vocabulary can never flag a graph outdated when no exclusion
/// rule reads it.
#[derive(Serialize)]
pub struct ScanConfig<'a> {
    scope: &'a ScopeConfig,
    output_dir: &'a str,
    terminal: Option<&'a [String]>,
    /// The status a document that declares none is *built* with — the same
    /// resolution `parser::ParseConfig` applies. The scan reads it because
    /// a document's status is what the graph gives it, not only what it
    /// spells: reading "declares none" as "not terminal" makes the scan and
    /// the graph describe different documents, and under a config whose
    /// initial status is terminal they disagree about every bare one.
    initial_status: Option<&'a str>,
}

impl<'a> ScanConfig<'a> {
    /// Project the membership-affecting surface out of the full config.
    pub fn new(config: &'a Config) -> Self {
        Self {
            scope: &config.scope,
            output_dir: &config.output.dir,
            terminal: (!config.scope.conditional_exclude.is_empty())
                .then_some(config.statuses.terminal.as_slice()),
            initial_status: (!config.scope.conditional_exclude.is_empty())
                .then(|| crate::config::resolve_initial_status(&config.statuses)),
        }
    }

    /// True when `status` is terminal under the projected vocabulary.
    /// Always false when no `conditional_exclude` rule exists to ask.
    fn is_terminal(&self, status: &str) -> bool {
        self.terminal
            .is_some_and(|terminal| terminal.iter().any(|s| s == status))
    }

    /// The exclude patterns a scan actually enforces: the user's
    /// `scope.exclude` plus nodex's own output directory. Users would
    /// otherwise have to copy-paste `"_index/**"` into every project,
    /// and forgetting it silently causes `migrate`, `rename`, and
    /// `build` to treat GRAPH.md as a user document. The self-exclusion
    /// is unconditional — `Config::validate` refuses an empty
    /// `output.dir`. `Config::validate_scope` compiles exactly this
    /// list at load, so load-accept implies scan-success.
    pub(crate) fn effective_exclude_patterns(&self) -> Vec<String> {
        let mut patterns = self.scope.exclude.clone();
        patterns.push(format!("{}/**", self.output_dir.trim_end_matches('/')));
        patterns
    }
}

/// What a proposal says about one path: the bytes it would hold, or that it
/// would hold nothing at all.
///
/// Absence is a distinct answer from empty bytes, and conflating them is a
/// real bug: an empty document still parses into a node, while a path a
/// proposal removes leaves the graph. A move needs both in one proposal — the
/// destination gains content and the source loses it — and without absence a
/// move cannot be expressed at all, so nothing can judge it.
#[derive(Debug, Clone)]
pub enum Proposed {
    Content(String),
    Absent,
}

/// Where the project's bytes are for one evaluation.
///
/// A working-tree evaluation reads the tree under `root`. A *proposal*
/// evaluation — the write plane's gates, which judge the project a mutation
/// produces before the disk holds it — reads that tree with the proposal
/// applied. Every probe that asks the filesystem a question *about the
/// project* asks it through here, or it answers about a project nobody
/// asked about: a path the proposal removes is still on disk, and one it
/// creates is not there yet. The unresolved-cause classifier is where that
/// matters most — it distinguishes "nothing is there" from "something is
/// there the graph excludes", and a stale answer moves an edge from one
/// `[[detection.unresolved_policy]]` row to another.
#[derive(Clone, Copy)]
pub struct ProjectFiles<'a> {
    root: &'a Path,
    overlay: &'a [(PathBuf, Proposed)],
}

impl<'a> ProjectFiles<'a> {
    /// The tree as it stands. Every read-plane evaluation, and the
    /// before-half of every proposal gate.
    pub fn working_tree(root: &'a Path) -> Self {
        Self { root, overlay: &[] }
    }

    /// The tree with `overlay` applied — the same slice
    /// [`build_with_overlay`](crate::builder::build_with_overlay) graphed, so
    /// the probes and the graph describe one project.
    pub fn proposed(root: &'a Path, overlay: &'a [(PathBuf, Proposed)]) -> Self {
        Self { root, overlay }
    }

    /// The project root — for the probes that walk it, and for the rules
    /// that measure git rather than files.
    pub fn root(&self) -> &'a Path {
        self.root
    }

    /// Whether the project holds readable content at `rel`, a normalised
    /// root-relative path. `admit_dirs` widens the answer to directories for
    /// the path-only relation, whose targets name code rather than
    /// documents. The proposal answers first where it speaks at all.
    pub(crate) fn holds(&self, rel: &Path, admit_dirs: bool) -> bool {
        match self.overlay.iter().find(|(path, _)| path == rel) {
            Some((_, Proposed::Content(_))) => true,
            Some((_, Proposed::Absent)) => false,
            None => crate::builder::resolver::exists_case_sensitive(self.root, rel, admit_dirs),
        }
    }
}

/// In-scope document paths, plus every way this walk declined to yield one.
/// Each decline is reported on the build result so it is auditable rather
/// than silent — a document the build never saw is a document no rule
/// judged, and the loss has to be visible from the outside.
pub struct ScopeScan {
    pub paths: Vec<PathBuf>,
    /// Paths a `conditional_exclude` rule dropped.
    pub conditionally_excluded: Vec<PathBuf>,
    /// In-scope paths whose resolved location lies outside the scanned root,
    /// dropped because they are not part of it. Only a confined scan
    /// ([`scan_ref`]) produces these: the working tree follows a symlink out
    /// of the project by design, and the writer-skip discipline is what keeps
    /// a write from following it back.
    pub escaping: Vec<PathBuf>,
    /// Entries the walk reached that are neither a file to read nor a
    /// directory to descend: a symlink whose target is absent, or a socket /
    /// FIFO / device node. The walk classifies by `is_dir` / `is_file`, both
    /// of which answer false here, so without this the entry would fall out of
    /// the classification with no record anywhere. Gated by what the walk
    /// decides type-blind (`scope.prune_dirs`, the hidden guard) and nothing
    /// narrower — the document globs cannot judge an entry whose type is
    /// unknowable.
    pub dangling: Vec<PathBuf>,
    /// Directory symlinks the walk did not descend, because
    /// `scope.follow_symlinks` is off. Documents below them are not graphed,
    /// which is a decline to yield and so is reported like every other.
    pub unfollowed: Vec<PathBuf>,
    /// The subset of [`Self::unfollowed`] a `scope.include` pattern could
    /// reach below — the part that bounds the document corpus rather than the
    /// walk. A validation run says this out loud; the whole accounting stays
    /// on the build result.
    pub unfollowed_in_scope: Vec<PathBuf>,
    /// Names the scan holds a document under but does not use, each paired
    /// with the one it does, as `(not used, in use)`. Only a followed link
    /// produces these. Nothing is lost — the document is graphed under the
    /// name in use — so this is not a decline; it is reported because a path
    /// the operator can read that the graph does not carry needs an
    /// explanation, and because a write seam naming an unused one has to say
    /// which name to use instead.
    pub aliases: Vec<(PathBuf, PathBuf)>,
}

/// The boundary a scan could not read, as the one warning every command built
/// on it emits — or `None` when the walk reached everything an include pattern
/// could have admitted.
///
/// One phrasing, in one place: a command that reasons across a graph has to
/// say what the graph is missing, and three of them saying it three ways is
/// three chances for one of them to drift.
pub fn boundary_warning(unfollowed_in_scope: &[PathBuf], action: &str) -> Option<crate::Warning> {
    if unfollowed_in_scope.is_empty() {
        return None;
    }
    let names: Vec<String> = unfollowed_in_scope
        .iter()
        .map(|p| crate::path_guard::forward_string(p))
        .collect();
    Some(crate::Warning::new(
        crate::WarningCode::ScopeCoverage,
        format!(
            "{} directory symlink(s) were not descended, so nothing below them was read ({}); \
             set scope.follow_symlinks to {action} documents that live behind a link",
            names.len(),
            names.join(", ")
        ),
    ))
}

/// Scan the filesystem for in-scope document paths.
/// Applies include/exclude globs, then conditional_exclude rules.
pub fn scan_scope(root: &Path, config: &Config) -> Result<ScopeScan> {
    scan(root, config, &[], Confinement::Follow)
}

/// [`scan_scope`] for a materialised git ref: a path whose resolved location
/// lies outside `root` is not part of that ref.
///
/// The working tree legitimately follows a symlink out of the project — the
/// bytes there are the bytes the operator sees, and the writer-skip
/// discipline keeps a write from following it back. A checkout is different:
/// git recorded the link, not its target, so following one out of the
/// checkout reads the *present* and presents it as the ref's past. A lock
/// comparing that against the working tree finds them identical and never
/// fires, which is a silence no advisory can make honest — the document has
/// no faithful content at the ref, so it has none here either.
pub fn scan_ref(root: &Path, checkout: &Path, config: &Config) -> Result<ScopeScan> {
    scan(root, config, &[], Confinement::Confine(checkout))
}

/// [`scan_scope`] with proposed content overlaid: each overlay
/// `(rel_path, content)` participates in scope exactly as if it were
/// that file's on-disk bytes. A not-yet-written path joins the
/// candidate set under the same static policy the walk applies, and
/// `conditional_exclude` reads a parent's status from the overlay
/// before the disk. The scan is the single scope authority, so an
/// overlay graph and the real post-write build can never disagree
/// about a path's membership — including when the proposal itself
/// flips a conditional-exclude parent into or out of a terminal
/// status.
pub fn scan_scope_with_overlay(
    root: &Path,
    config: &Config,
    overlay: &[(PathBuf, Proposed)],
) -> Result<ScopeScan> {
    scan(root, config, overlay, Confinement::Follow)
}

/// Whether a scan may leave the root it was asked about.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Confinement<'a> {
    /// Resolve symlinks wherever they lead — the working tree's reader-follows
    /// discipline.
    Follow,
    /// Keep to the *checkout*, which is the boundary of what the ref recorded.
    /// Not the project root: a project inside a larger repository may hold an
    /// in-scope link to a tracked sibling outside itself, and the ref records
    /// that sibling — treating it as an escape would drop real history.
    Confine(&'a Path),
}

fn scan(
    root: &Path,
    config: &Config,
    overlay: &[(PathBuf, Proposed)],
    confinement: Confinement<'_>,
) -> Result<ScopeScan> {
    let scan = ScanConfig::new(config);
    let include = build_globset(&scan.scope.include, "scope.include")?;
    let exclude = build_globset(&scan.effective_exclude_patterns(), "scope.exclude")?;

    // Hidden paths (`.draft.md`, `.archive/`, `.claude/`, …) are skipped
    // by default — the ripgrep / fd / git convention, since dot-prefixed
    // entries are overwhelmingly editor state or tooling config. An
    // include pattern overrides that default for exactly the segments it
    // names literally: `.claude/routines/*.md` opts `.claude` in, while a
    // greedy `**/*.md` does not. The include pattern is the opt-in; there
    // is no separate flag to keep in sync.
    let prefixes = literal_prefixes(&scan.scope.include);

    // A confined scan compares against the checkout's *real* location, so a
    // checkout reached through a symlink does not read as an escape from
    // itself.
    let confine = match confinement {
        Confinement::Follow => None,
        Confinement::Confine(checkout) => {
            Some(std::fs::canonicalize(checkout).unwrap_or_else(|_| checkout.to_path_buf()))
        }
    };
    // Where the output directory really is, so reaching it under any name is
    // reaching it. Absent until nodex has written there, which is also when
    // there is nothing inside to admit.
    // Kept only where it is strictly inside the project: an output directory
    // that resolves to the root — or above it, through a link — would decline
    // every directory in the project and leave nothing scanned. `Config`
    // refuses the lexical spellings of that; a link is only knowable here.
    let output = std::fs::canonicalize(root)
        .ok()
        .zip(std::fs::canonicalize(root.join(scan.output_dir.trim_end_matches('/'))).ok())
        .filter(|(real_root, output)| output != real_root && output.starts_with(real_root))
        .map(|(_, output)| output);
    let policy = WalkPolicy {
        include: &include,
        exclude: &exclude,
        prefixes: &prefixes,
        prune_dirs: &scan.scope.prune_dirs,
        follow_symlinks: scan.scope.follow_symlinks,
        confine: confine.as_deref(),
        output: output.as_deref(),
    };
    let mut found = WalkFindings {
        paths: Vec::new(),
        dangling: Vec::new(),
        unfollowed: Vec::new(),
        unfollowed_in_scope: Vec::new(),
        undescended: Vec::new(),
        escaping: Vec::new(),
        dir_names: BTreeMap::new(),
        aliased: false,
    };
    walk_dir(root, root, &policy, &mut found)?;
    let WalkFindings {
        mut paths,
        mut dangling,
        mut unfollowed,
        mut unfollowed_in_scope,
        undescended,
        mut escaping,
        dir_names,
        aliased,
    } = found;

    // Overlay paths join or leave the candidate set under the same static
    // policy the walk just applied entry by entry. A path the proposal removes
    // leaves it whether or not the walk found it on disk — that is what makes a
    // move expressible: the destination joins and the source goes.
    //
    // A proposal names a document, not a spelling of one: where the walk
    // reached a directory under several names, the file the proposal names is
    // admitted under each, and removing only the string the caller typed would
    // leave the document behind under another name — a move whose source is
    // both gone and still present. So the match is on the entry a path
    // resolves to wherever the walk found more than one name for anything.
    for (rel_path, proposed) in overlay {
        match proposed {
            Proposed::Content(_) => {
                for candidate in proposal_names(root, rel_path, &dir_names) {
                    if !paths.contains(&candidate)
                        && policy.admits(&candidate)
                        && !beneath(&undescended, &candidate)
                    {
                        paths.push(candidate);
                    }
                }
            }
            Proposed::Absent => {
                if aliased {
                    let leaving = entry_of(root, rel_path);
                    paths.retain(|p| entry_of(root, p) != leaving);
                } else {
                    paths.retain(|p| p != rel_path);
                }
            }
        }
    }

    // Every policy that keys on a path is applied while all of a document's
    // admitted spellings are still present, because each names the document as
    // truthfully as the others: a `parent_glob` that matches only one of them
    // still describes the terminal parent it found, and its sub-artifacts are
    // derivative whichever name they are read under.
    let mut conditionally_excluded = if scan.scope.conditional_exclude.is_empty() {
        Vec::new()
    } else {
        apply_conditional_excludes(root, &mut paths, &scan, overlay)
    };

    // Only now is there one document per entry. An entry any spelling excluded
    // is excluded — the property the rule tests belongs to the document, not to
    // the name it was reached by. Where the walk found one name for every
    // directory this is the identity map, so it is not computed: resolving each
    // document's entry is a `canonicalize` per document, and a project with no
    // aliased directory would pay it only to be told what the walk established.
    let mut aliases: Vec<(PathBuf, PathBuf)> = Vec::new();
    if aliased {
        let excluded_entries: BTreeSet<PathBuf> = conditionally_excluded
            .iter()
            .map(|p| entry_of(root, p))
            .collect();
        let kept = documents_by_file(root, paths, &mut aliases);
        let (evicted, surviving): (Vec<PathBuf>, Vec<PathBuf>) = kept
            .into_iter()
            .partition(|rel| excluded_entries.contains(&entry_of(root, rel)));
        paths = surviving;
        conditionally_excluded.extend(evicted);
        aliases.sort();
    }

    // Sort for deterministic processing order
    paths.sort();
    conditionally_excluded.sort();
    dangling.sort();
    unfollowed.sort();
    unfollowed_in_scope.sort();
    escaping.sort();
    Ok(ScopeScan {
        paths,
        conditionally_excluded,
        dangling,
        unfollowed,
        unfollowed_in_scope,
        escaping,
        aliases,
    })
}

/// The compiled static scope policy: non-content pruning, hidden-segment
/// opt-in, include / exclude globs. The walk applies it entry by entry and
/// [`WalkPolicy::admits`] applies it to a single candidate path, so an
/// overlay path not yet on disk is judged by exactly what the walk judges.
struct WalkPolicy<'a> {
    include: &'a GlobSet,
    exclude: &'a GlobSet,
    prefixes: &'a [Vec<String>],
    prune_dirs: &'a [String],
    follow_symlinks: bool,
    /// The real location of the scanned root, when this scan must keep to it.
    confine: Option<&'a Path>,
    /// The real location of `output.dir`, when it exists.
    ///
    /// `effective_exclude_patterns` excludes it by glob, which names one
    /// spelling; the exclusion it states is unconditional, and a directory is a
    /// location rather than a spelling. Without this, a link pointing at the
    /// output directory admits nodex's own `GRAPH.md` as a project document,
    /// and `migrate` writes frontmatter into it.
    output: Option<&'a Path>,
}

impl WalkPolicy<'_> {
    /// Whether `path` resolves inside the output directory.
    fn leads_to_output(&self, path: &Path) -> bool {
        self.output.is_some_and(|output| {
            std::fs::canonicalize(path).is_ok_and(|real| real.starts_with(output))
        })
    }

    /// Whether any `scope.include` pattern could match a path below `rel`.
    ///
    /// A pattern's literal prefix is the part every path it matches must
    /// spell exactly, so a directory outside every prefix cannot hold a
    /// document the scan would admit — which is what makes this a proof
    /// rather than a guess. `scope.exclude` supports no such answer and is
    /// deliberately not consulted: its patterns match files, so excluding a
    /// directory's own path says nothing about what is under it.
    fn could_admit_below(&self, rel: &Path) -> bool {
        let rel_str = crate::path_guard::forward_string(rel);
        let segments: Vec<&str> = rel_str.split('/').collect();
        self.prefixes.iter().any(|prefix| {
            let shared = prefix.len().min(segments.len());
            prefix[..shared] == segments[..shared]
        })
    }
}

impl WalkPolicy<'_> {
    /// Whether `path` resolves outside the root this scan is confined to.
    /// Always false for an unconfined scan.
    fn escapes(&self, path: &Path) -> bool {
        let Some(confine) = self.confine else {
            return false;
        };
        match std::fs::canonicalize(path) {
            Ok(real) => !real.starts_with(confine),
            // Unresolvable is not an escape — a dangling entry is classified
            // as dangling, which is a fact of its own.
            Err(_) => false,
        }
    }
}

impl WalkPolicy<'_> {
    /// Whether the walk goes near `segments` at all.
    ///
    /// These are the tests that answer the same way whatever the entry turns
    /// out to be: `scope.prune_dirs` names path segments and the hidden guard
    /// reads them, so both a directory the walk would descend and a file it
    /// would admit are settled by the same verdict. `include` / `exclude` are
    /// globs over a *document* path, so they only ever answer the file
    /// reading — a directory is descended without consulting them, and the
    /// documents beneath it are matched one by one.
    ///
    /// That split is what an entry resolving to neither can be judged by: its
    /// target is gone, so which reading applied is unknowable, and only the
    /// type-independent half of the policy still holds.
    fn walks(&self, segments: &[&str]) -> bool {
        !segments
            .iter()
            .any(|s| self.prune_dirs.iter().any(|d| d == s))
            && !hidden_path_skipped(segments, self.prefixes)
    }

    /// Whether this policy admits `rel_path` as an in-scope document.
    fn admits(&self, rel_path: &Path) -> bool {
        let rel_str = crate::path_guard::forward_string(rel_path);
        let segments: Vec<&str> = rel_str.split('/').collect();
        self.walks(&segments) && self.include.is_match(&rel_str) && !self.exclude.is_match(&rel_str)
    }
}

/// What one walk yielded: the documents, and every path it declined.
struct WalkFindings {
    paths: Vec<PathBuf>,
    dangling: Vec<PathBuf>,
    escaping: Vec<PathBuf>,
    unfollowed: Vec<PathBuf>,
    /// The subset of `unfollowed` an `scope.include` pattern could reach below.
    ///
    /// `unfollowed` is the whole accounting — every link the walk declined,
    /// reported so the boundary is auditable. This is the part that bounds the
    /// *document corpus*, which is what a validation run has to say out loud:
    /// a link no include pattern can reach below holds nothing the scan would
    /// have admitted, and announcing it on every run would be a warning no
    /// configuration can clear.
    unfollowed_in_scope: Vec<PathBuf>,
    /// Every directory the walk declined to enter because of where it leads —
    /// an undescended symlink, or the output directory reached under any name.
    ///
    /// A proposed path is in scope only where the walk would have reached it,
    /// and the globs cannot answer that: they judge a path's spelling, while
    /// these declines are about its location. Without them a write seam
    /// approves a document below a link it does not descend, writes it, and the
    /// next build cannot see it. Superset of `unfollowed`, which is the part
    /// the operator is told about.
    undescended: Vec<PathBuf>,
    /// Every name the walk reached each directory under, keyed by identity.
    ///
    /// Only a followed link puts a directory under a second name, and only
    /// then is this populated. A proposal names a file inside one of these
    /// directories, and the scan has to judge it under every name the walk
    /// would produce for it — otherwise the one name the caller happened to
    /// type is the only one considered, and the post-write build, which sees
    /// them all, keeps a different one.
    dir_names: BTreeMap<PathBuf, Vec<PathBuf>>,
    /// Whether the walk reached one directory under more than one name.
    ///
    /// A document's name is ambiguous only when the directory holding it is,
    /// so this is the precondition for every step that resolves one name out
    /// of several — the overlay's entry-space merge and the collapse to one
    /// document per entry. Off, each is provably the identity: distinct paths
    /// with the same file name have distinct parents, distinct parents that
    /// were each reached once have distinct identities, so no two paths share
    /// an entry. Establishing it costs the walk nothing it was not already
    /// paying — a directory's identity is what bounds the descent — while
    /// deciding it per document costs a `canonicalize` per document, on every
    /// project, to answer "no".
    aliased: bool,
}

/// The overlay bytes for `rel_path`, when the path is overlaid.
pub(crate) fn proposed_for<'a>(
    overlay: &'a [(PathBuf, Proposed)],
    rel_path: &Path,
) -> Option<&'a Proposed> {
    overlay
        .iter()
        .find(|(p, _)| p == rel_path)
        .map(|(_, proposed)| proposed)
}

/// The bytes a proposal puts at `rel_path`, if it puts any there.
pub(crate) fn overlay_content<'a>(
    overlay: &'a [(PathBuf, Proposed)],
    rel_path: &Path,
) -> Option<&'a str> {
    match proposed_for(overlay, rel_path)? {
        Proposed::Content(content) => Some(content.as_str()),
        Proposed::Absent => None,
    }
}

/// For each conditional_exclude rule:
/// 1. Find "parent" files matching `parent_glob` whose frontmatter
///    status is terminal — read from the overlay when the parent is
///    overlaid, so a proposal's own bytes decide its terminality.
/// 2. Within that parent's directory subtree, drop every file that
///    matches the rule's `child_glob` — except the parent itself. A
///    sibling the `child_glob` does not name is left in scope, so an
///    independently-owned document is never erased just for sharing a
///    directory with a terminal parent.
///
/// Mutates `paths` in place to the surviving set and returns the
/// excluded paths (for reporting on the build result — the exclusion is
/// auditable, never silent).
fn apply_conditional_excludes(
    root: &Path,
    paths: &mut Vec<PathBuf>,
    scan: &ScanConfig<'_>,
    overlay: &[(PathBuf, Proposed)],
) -> Vec<PathBuf> {
    // A sub-artifact is dropped iff it sits under a terminal parent's
    // directory AND matches that rule's `child_glob`. The parent file
    // itself is always kept so it still parses into the graph.
    let mut drop: BTreeSet<PathBuf> = BTreeSet::new();
    let mut parents_to_keep: BTreeSet<PathBuf> = BTreeSet::new();

    for rule in &scan.scope.conditional_exclude {
        if rule.condition != "status_terminal" {
            continue;
        }

        let parent_glob = Glob::new(&rule.parent_glob)
            .expect("validated by Config::load")
            .compile_matcher();
        let child_glob = Glob::new(&rule.child_glob)
            .expect("validated by Config::load")
            .compile_matcher();

        // Directories whose terminal parent this rule governs.
        let mut terminal_dirs: BTreeSet<PathBuf> = BTreeSet::new();
        for rel_path in paths.iter() {
            let rel_str = crate::path_guard::forward_string(rel_path);
            if !parent_glob.is_match(&rel_str) {
                continue;
            }

            // The overlay is the authoritative source for an overlaid
            // parent — its proposed bytes, not the stale on-disk ones,
            // decide whether the parent is terminal.
            let content = if let Some(proposed) = overlay_content(overlay, rel_path) {
                proposed.to_string()
            } else {
                // A parent the probe cannot read (vanished, permissions,
                // bad UTF-8, …) cannot be *confirmed* terminal, and
                // exclusion is a positive decision — so the probe
                // degrades conservatively: the sub-artifacts stay in
                // scope, and the unreadable parent — itself a scanned
                // path — is recorded as a typed `ParseFailure` by the
                // build's read phase, so `check` reds the same file
                // (reader-degrades / loud-elsewhere, exactly like an
                // unparseable or fence-broken parent below). Never a
                // silent exclusion, never a halted scan.
                match std::fs::read_to_string(root.join(rel_path)) {
                    Ok(c) => c,
                    Err(_) => continue,
                }
            };
            let content = crate::parser::frontmatter::canonicalize(&content);

            if is_terminal_status(&content, scan) {
                parents_to_keep.insert(rel_path.clone());
                terminal_dirs.insert(rel_path.parent().map(Path::to_path_buf).unwrap_or_default());
            }
        }

        if terminal_dirs.is_empty() {
            continue;
        }
        for rel_path in paths.iter() {
            if parents_to_keep.contains(rel_path) {
                continue;
            }
            let under_terminal = terminal_dirs.iter().any(|dir| rel_path.starts_with(dir));
            let rel_str = crate::path_guard::forward_string(rel_path);
            if under_terminal && child_glob.is_match(&rel_str) {
                drop.insert(rel_path.clone());
            }
        }
    }

    if drop.is_empty() {
        return Vec::new();
    }
    paths.retain(|p| !drop.contains(p));
    drop.into_iter().collect()
}

/// Whether the document at `content` holds a terminal status — the
/// question a `conditional_exclude` rule asks of a parent.
///
/// Uses a lightweight YAML parse (not the full frontmatter parser) on the
/// hot scan path, but answers the same way the build does. A document that
/// declares no status is built with the project's initial one, so that is
/// the status it has; reading "declares none" as "not terminal" would make
/// the scan and the graph describe different documents from the same bytes.
///
/// Unparseable YAML or an unclosed fence is different: the build produces
/// no node at all (`check` reds it as `parse_failure`), so there is no
/// document there to be a terminal parent.
fn is_terminal_status(content: &str, scan: &ScanConfig<'_>) -> bool {
    let Ok((yaml, _)) = crate::parser::frontmatter::split_frontmatter(content) else {
        return false;
    };
    let declared = match yaml {
        Some(yaml) => {
            let Ok(value) = yaml_serde::from_str::<yaml_serde::Value>(yaml) else {
                return false;
            };
            value
                .as_mapping()
                .and_then(|m| m.get(yaml_serde::Value::String("status".to_string())))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        }
        None => None,
    };
    match declared.as_deref().or(scan.initial_status) {
        Some(status) => scan.is_terminal(status),
        // No `conditional_exclude` rule exists to ask, so the vocabulary was
        // never projected and nothing consults this answer.
        None => false,
    }
}

/// The leading segments each include pattern spells literally — the part
/// every path it matches must have exactly. `.claude/**/*.md` → `[.claude]`;
/// `docs/.drafts/**` → `[docs, .drafts]`.
///
/// A segment stops the prefix when it carries a glob metacharacter or a
/// backslash: globset reads `\` as an escape, so `lit\\ref` is the literal
/// segment `lit\ref` and the pattern text is not what it names. Stopping
/// there says the spelling is unknown from that point on, which is the side
/// to be wrong on — both consumers then treat the pattern as reaching
/// further, not less far.
///
/// A greedy pattern contributes an empty prefix, which is the honest answer
/// and each consumer reads correctly: the hidden-path opt-in needs a segment
/// named literally and an empty prefix names none, while
/// [`WalkPolicy::could_admit_below`] asks whether a pattern could reach below
/// a directory, and one that spells nothing literally could reach anywhere.
fn literal_prefixes(include: &[String]) -> Vec<Vec<String>> {
    include
        .iter()
        .map(|pattern| {
            pattern
                .split('/')
                .take_while(|seg| !seg.contains(['*', '?', '[', '{', '\\']))
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// True if a path with a hidden (`.`-prefixed) segment is *not* opted in
/// by any include pattern, and so should be skipped — the ripgrep / fd /
/// git convention. A hidden segment counts as opted in only when an
/// include pattern names it literally *at its position*: a prefix `P`
/// admits path `R` when one is a path-prefix of the other and every
/// hidden segment of `R` falls within `P`'s literal coverage. So
/// `.claude/**/*.md` (`P = [.claude]`) admits root `.claude/...` but not
/// a nested `foo/.claude/...`, and a greedy `**/*.md` (empty prefix)
/// admits no hidden path at all.
fn hidden_path_skipped(rel: &[&str], prefixes: &[Vec<String>]) -> bool {
    let last_hidden = rel.iter().rposition(|seg| seg.starts_with('.'));
    let Some(last_hidden) = last_hidden else {
        return false; // no hidden segment — never skipped here
    };
    let admitted = prefixes.iter().any(|p| {
        let l = rel.len().min(p.len());
        last_hidden < l && rel[..l] == p[..l]
    });
    !admitted
}

fn walk_dir(
    base: &Path,
    root: &Path,
    policy: &WalkPolicy<'_>,
    found: &mut WalkFindings,
) -> Result<()> {
    // Iterative DFS over an explicit stack: the scanner follows symlinks on
    // read (`is_dir` / `is_file` resolve them), so a symlinked directory that
    // points back into the tree — or a pathologically deep one — must not
    // recurse the call stack into an overflow, which aborted `nodex build`
    // with SIGABRT, outside the JSON envelope.
    //
    // A cycle is a directory that is its own ancestor, so that is the test:
    // each entry carries the canonical identities on the path down to it.
    // Two spellings of one directory that are merely siblings are not a
    // cycle, and both are walked — which spelling represents a document is
    // decided by [`documents_by_file`], where the scope globs have been
    // applied and the question can be answered instead of guessed.
    let mut stack: Vec<(PathBuf, Vec<PathBuf>)> = vec![(root.to_path_buf(), Vec::new())];
    // Which directories the walk has reached, by identity rather than by name.
    // Arriving twice is what makes a document's name ambiguous, and the walk is
    // the only place that knows: it already resolves each directory's identity
    // to bound the descent, so recording it answers for the whole scan what no
    // later step could ask without re-resolving every path.
    let mut reached: BTreeSet<PathBuf> = BTreeSet::new();

    while let Some((dir, ancestors)) = stack.pop() {
        let identity = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if ancestors.contains(&identity) {
            // The directory is one the descent already passed through, so it
            // holds nothing new. Recorded like every other decline: a proposal
            // below it names documents the graph carries under the ancestor's
            // name, and a seam told only that the path is "outside
            // scope.include" would send the operator looking at globs that are
            // not the cause.
            found
                .undescended
                .push(dir.strip_prefix(base).unwrap_or(&dir).to_path_buf());
            continue;
        }
        if !reached.insert(identity.clone()) {
            found.aliased = true;
        }
        if policy.follow_symlinks {
            found
                .dir_names
                .entry(identity.clone())
                .or_default()
                .push(dir.strip_prefix(base).unwrap_or(&dir).to_path_buf());
        }
        if dir != root
            && policy
                .output
                .is_some_and(|output| identity.starts_with(output))
        {
            // The glob in `effective_exclude_patterns` names one spelling of
            // the output directory; the exclusion it states is unconditional,
            // and a directory is a location. Reached under another name,
            // nodex's own `GRAPH.md` would be a project document that
            // `migrate` writes frontmatter into.
            found
                .undescended
                .push(dir.strip_prefix(base).unwrap_or(&dir).to_path_buf());
            continue;
        }
        let descended: Vec<PathBuf> = ancestors
            .iter()
            .cloned()
            .chain(std::iter::once(identity))
            .collect();

        let entries = std::fs::read_dir(&dir).map_err(|e| Error::Io {
            path: dir.clone(),
            source: e,
        })?;

        for entry in entries {
            let entry = entry.map_err(|e| Error::Io {
                path: dir.clone(),
                source: e,
            })?;
            let path = entry.path();

            let rel = path.strip_prefix(base).unwrap_or(&path);
            let rel_str = crate::path_guard::forward_string(rel);
            let segments: Vec<&str> = rel_str.split('/').collect();

            if path.is_dir() {
                // `scope.prune_dirs` basenames (node_modules / target / …)
                // are pruned at any depth regardless of include patterns;
                // dot-prefixed trees are also caught by the hidden guard.
                if !policy.walks(&segments) {
                    continue;
                }
                if policy.escapes(&path) {
                    // A whole subtree the ref does not carry. Recorded rather
                    // than merely skipped: nothing below it will ever surface,
                    // so this entry is the only chance to say it was not read.
                    found.escaping.push(rel.to_path_buf());
                    continue;
                }
                if !policy.follow_symlinks && crate::path_guard::is_symlink(&path) {
                    // The project's path space stays a tree: one name per
                    // directory, so every rule that keys on a path has one
                    // path to key on. Recorded because documents below it are
                    // not graphed, and a drop the operator cannot see is the
                    // one thing the scan must never do.
                    found.unfollowed.push(rel.to_path_buf());
                    // What bounds the *documents* is narrower: a link leading
                    // to nodex's own output directory holds none under any
                    // configuration, and one no include pattern can reach
                    // below holds none the scan would have admitted.
                    if policy.could_admit_below(rel) && !policy.leads_to_output(&path) {
                        found.unfollowed_in_scope.push(rel.to_path_buf());
                    }
                    found.undescended.push(rel.to_path_buf());
                    continue;
                }
                stack.push((path, descended.clone()));
            } else if path.is_file() {
                if policy.admits(rel) {
                    if policy.escapes(&path) {
                        found.escaping.push(rel.to_path_buf());
                    } else {
                        found.paths.push(rel.to_path_buf());
                    }
                }
            } else if policy.walks(&segments) {
                // Neither a directory nor a file: an entry whose target is
                // absent. There is nothing to read, so the walk cannot yield
                // it — but a build that omits an in-scope document without
                // saying so is how a baseline loses one whose lock then never
                // fires, with an empty warnings array.
                found.dangling.push(rel.to_path_buf());
            }
        }
    }

    Ok(())
}

/// Every name the scan would produce for a proposed file: one per name the
/// walk reached its directory under, or the authored path alone where the walk
/// did not reach that directory (a document authored into a new one).
///
/// A proposal names a document, and where its directory has several names the
/// document does too. Judging only the name the caller typed lets the proposal
/// be admitted under a name the post-write build then discards for a smaller
/// one, so a write seam blesses a path the graph will not carry.
fn proposal_names(
    root: &Path,
    rel_path: &Path,
    dir_names: &BTreeMap<PathBuf, Vec<PathBuf>>,
) -> Vec<PathBuf> {
    let Some((parent, name)) = rel_path.parent().zip(rel_path.file_name()) else {
        return vec![rel_path.to_path_buf()];
    };
    let Some(identity) = std::fs::canonicalize(root.join(parent)).ok() else {
        return vec![rel_path.to_path_buf()];
    };
    match dir_names.get(&identity) {
        Some(names) => names.iter().map(|dir| dir.join(name)).collect(),
        None => vec![rel_path.to_path_buf()],
    }
}

/// Whether `rel` lies below any directory the walk declined to enter.
fn beneath(undescended: &[PathBuf], rel: &Path) -> bool {
    undescended.iter().any(|dir| rel.starts_with(dir))
}

/// One document per directory entry, at the smallest path the scope admits it
/// under.
///
/// A directory reachable by several spellings puts the entries inside it at
/// several paths, and the globs judge each on its own. Where more than one is
/// admitted the project would hold one file as two documents, so they are
/// collapsed here rather than while descending: only paths the policy has
/// already accepted take part, and the choice among them is by name, which is
/// the same everywhere `read_dir` is not.
///
/// The surviving name is the document's, and an `identity.id_rules` template
/// reading the path (`{parent}`) infers the id from it — so under an aliased
/// directory the ids the other spellings would have produced are ids the
/// project does not have. A reference written to one of them does not resolve,
/// and says so as an unresolved edge. Each name it drops is returned in
/// `discarded`, paired with the one kept: a path the operator can read that
/// the graph does not carry is the one decline that would otherwise have no
/// record, and a write seam naming a dropped one has to say which to use.
///
/// Keyed on the entry, which is the canonical directory holding it plus the
/// name it is filed under. Deliberately not on what the entry resolves to: a
/// document that is a symlink to another is a second entry, not a second
/// spelling of the first, and both are documents with ids of their own.
fn documents_by_file(
    base: &Path,
    admitted: Vec<PathBuf>,
    discarded: &mut Vec<(PathBuf, PathBuf)>,
) -> Vec<PathBuf> {
    let mut by_entry: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for rel in admitted {
        by_entry.entry(entry_of(base, &rel)).or_default().push(rel);
    }
    by_entry
        .into_values()
        .map(|mut names| {
            names.sort();
            let kept = names.remove(0);
            discarded.extend(names.into_iter().map(|name| (name, kept.clone())));
            kept
        })
        .collect()
}

/// The directory entry `rel` names: the canonical directory holding it plus the
/// name it is filed under. Two spellings of one directory give their entries the
/// same answer; a document that links to another does not, because it is filed
/// under a name of its own.
fn entry_of(base: &Path, rel: &Path) -> PathBuf {
    let full = base.join(rel);
    full.parent()
        .and_then(|dir| std::fs::canonicalize(dir).ok())
        .zip(full.file_name())
        .map_or_else(|| full.clone(), |(dir, name)| dir.join(name))
}

/// Compile a glob list into one matcher set, labelling errors with the
/// config surface (`key`) the patterns came from. The single compile
/// path shared by the scanner and `Config::validate_scope` — load-time
/// acceptance and scan-time compilation can never drift, and the
/// load-time error names exactly the key the operator must fix.
pub(crate) fn build_globset(patterns: &[String], key: &str) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = Glob::new(pattern).map_err(|e| {
            Error::Config(format!(
                "{key} pattern {pattern:?} is not a valid glob: {e}"
            ))
        })?;
        builder.add(glob);
    }
    builder
        .build()
        .map_err(|e| Error::Config(format!("{key} globset build error: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConditionalExclude;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn scan_includes_matching_files() {
        let dir = TempDir::new().unwrap();
        let docs = dir.path().join("docs");
        fs::create_dir_all(&docs).unwrap();
        fs::write(docs.join("guide.md"), "# Guide").unwrap();
        fs::write(docs.join("notes.txt"), "notes").unwrap();
        fs::write(dir.path().join("README.md"), "# Root").unwrap();

        let mut config = Config::default();
        config.scope.include = vec!["**/*.md".to_string()];

        let paths = scan_scope(dir.path(), &config).unwrap().paths;
        assert_eq!(paths.len(), 2);
        assert!(paths.iter().any(|p| p.ends_with("guide.md")));
        assert!(paths.iter().any(|p| p.ends_with("README.md")));
    }

    /// An entry that resolves to neither a file nor a directory is recorded
    /// unless the walk would have kept away from it whatever it was. `include`
    /// / `exclude` cannot make that call: they are globs over a document path,
    /// and a real directory is descended without consulting them — so an
    /// `exclude` that names a directory spelling still lets its children in,
    /// and suppressing the record on that basis would lose exactly the
    /// documents the record exists to announce.
    #[test]
    #[cfg(unix)]
    fn a_dangling_entry_is_recorded_wherever_the_walk_would_have_gone() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        for sub in ["docs", "docs/shared", "node_modules/pkg"] {
            fs::create_dir_all(root.join(sub)).unwrap();
        }
        for at in [
            "docs/gone.md",
            "docs/shared/gone.md",
            "node_modules/pkg/gone.md",
        ] {
            std::os::unix::fs::symlink("nowhere", root.join(at)).unwrap();
        }
        // A sibling under the excluded directory spelling proves the walk
        // still admits documents from there.
        fs::write(root.join("docs/shared/real.md"), "# real").unwrap();

        let mut config = Config::default();
        config.scope.include = vec!["docs/**/*.md".to_string()];
        config.scope.exclude = vec!["docs/shared".to_string()];

        let scan = scan_scope(root, &config).unwrap();
        assert!(
            scan.paths.iter().any(|p| p.ends_with("shared/real.md")),
            "the exclude names the directory, not its children: {:?}",
            scan.paths
        );
        assert_eq!(
            scan.dangling,
            vec![
                PathBuf::from("docs/gone.md"),
                PathBuf::from("docs/shared/gone.md")
            ],
            "a pruned tree has lost nothing; everywhere else the walk reaches has"
        );
    }

    /// Which spelling of an aliased directory represents its documents is a
    /// scope question, so it is answered after the globs and not while
    /// descending: an alias the project excludes cannot take the documents its
    /// real directory admits.
    #[test]
    #[cfg(unix)]
    fn an_excluded_alias_does_not_take_the_documents_it_shadows() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("docs/real")).unwrap();
        fs::write(root.join("docs/real/a.md"), "# A").unwrap();
        std::os::unix::fs::symlink("real", root.join("docs/alias")).unwrap();

        let mut config = Config::default();
        config.scope.follow_symlinks = true;
        config.scope.include = vec!["docs/**/*.md".to_string()];
        config.scope.exclude = vec!["docs/alias/**".to_string()];
        assert_eq!(
            scan_scope(root, &config).unwrap().paths,
            vec![PathBuf::from("docs/real/a.md")]
        );

        // With both spellings admitted the file is still one document, and
        // which name it wears is decided by the name rather than by whatever
        // order the filesystem returned its entries in.
        config.scope.exclude.clear();
        let mut seen = Vec::new();
        for _ in 0..3 {
            seen.push(scan_scope(root, &config).unwrap().paths);
        }
        assert_eq!(seen[0], seen[1]);
        assert_eq!(seen[1], seen[2]);
        assert_eq!(seen[0], vec![PathBuf::from("docs/alias/a.md")]);
    }

    /// A directory reached through a symlink is not descended unless the
    /// project asks, which is what keeps the path space a tree: one name per
    /// directory, so every rule that keys on a path has one path to key on.
    #[test]
    #[cfg(unix)]
    fn only_a_link_an_include_could_reach_below_bounds_the_corpus() {
        // The accounting names every link the walk declined. What a validation
        // run says out loud is the part that bounds the *documents*: a link no
        // include pattern can reach below holds nothing the scan would have
        // admitted, and nodex's own output directory holds none under any
        // configuration — announcing either is a warning no configuration can
        // clear.
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::create_dir_all(root.join("real")).unwrap();
        fs::create_dir_all(root.join("_index")).unwrap();
        fs::write(root.join("docs/own.md"), "# Own").unwrap();
        std::os::unix::fs::symlink("../real", root.join("docs/reachable")).unwrap();
        std::os::unix::fs::symlink("real", root.join("elsewhere")).unwrap();
        std::os::unix::fs::symlink("_index", root.join("mirror")).unwrap();

        let mut config = Config::default();
        config.scope.include = vec!["docs/**/*.md".to_string()];
        let scan = scan_scope(root, &config).unwrap();

        assert_eq!(
            scan.unfollowed,
            vec![
                PathBuf::from("docs/reachable"),
                PathBuf::from("elsewhere"),
                PathBuf::from("mirror")
            ],
            "the accounting names every link the walk declined"
        );
        assert_eq!(
            scan.unfollowed_in_scope,
            vec![PathBuf::from("docs/reachable")],
            "only the one an include pattern could reach below"
        );

        // A pattern that spells nothing literally could reach anywhere, so
        // reachability stops narrowing — the output directory still does.
        config.scope.include = vec!["**/*.md".to_string()];
        let greedy = scan_scope(root, &config).unwrap();
        assert_eq!(
            greedy.unfollowed_in_scope,
            vec![PathBuf::from("docs/reachable"), PathBuf::from("elsewhere")]
        );
    }

    /// The boundary is reported, because documents below it are not graphed.
    #[test]
    #[cfg(unix)]
    fn a_directory_link_is_not_descended_unless_the_project_asks() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("vendor/docs")).unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("docs/own.md"), "# Own").unwrap();
        fs::write(root.join("vendor/docs/v.md"), "# V").unwrap();
        std::os::unix::fs::symlink("../vendor/docs", root.join("docs/vendored")).unwrap();

        let mut config = Config::default();
        config.scope.include = vec!["docs/**/*.md".to_string()];

        let scan = scan_scope(root, &config).unwrap();
        assert_eq!(scan.paths, vec![PathBuf::from("docs/own.md")]);
        assert_eq!(
            scan.unfollowed,
            vec![PathBuf::from("docs/vendored")],
            "the boundary is named, not passed over in silence"
        );

        config.scope.follow_symlinks = true;
        let followed = scan_scope(root, &config).unwrap();
        assert_eq!(
            followed.paths,
            vec![
                PathBuf::from("docs/own.md"),
                PathBuf::from("docs/vendored/v.md")
            ]
        );
        assert!(followed.unfollowed.is_empty());
    }

    /// `conditional_exclude` keys on paths, so it is applied while every
    /// admitted spelling is still present: a `parent_glob` naming one of them
    /// still found the terminal parent, and its sub-artifacts are derivative
    /// under any name. Otherwise an unrelated symlink turns the rule off.
    #[test]
    #[cfg(unix)]
    fn an_alias_does_not_switch_off_a_conditional_exclude() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("docs/real")).unwrap();
        fs::write(
            root.join("docs/real/SPEC.md"),
            "---\nstatus: archived\n---\n# S\n",
        )
        .unwrap();
        fs::write(
            root.join("docs/real/note-a.md"),
            "---\nstatus: active\n---\n# N\n",
        )
        .unwrap();
        std::os::unix::fs::symlink("real", root.join("docs/alias")).unwrap();

        let mut config = Config::default();
        config.scope.follow_symlinks = true;
        config.scope.include = vec!["docs/**/*.md".to_string()];
        config.scope.conditional_exclude = vec![ConditionalExclude {
            condition: "status_terminal".to_string(),
            parent_glob: "docs/real/SPEC.md".to_string(),
            child_glob: "docs/**/note-*.md".to_string(),
        }];
        config.statuses.terminal = vec!["archived".to_string()];

        let scan = scan_scope(root, &config).unwrap();
        assert_eq!(
            scan.paths,
            vec![PathBuf::from("docs/alias/SPEC.md")],
            "the sub-artifact is dropped whichever name it was reached by"
        );
        assert!(
            scan.conditionally_excluded
                .contains(&PathBuf::from("docs/real/note-a.md")),
            "and the exclusion is reported: {:?}",
            scan.conditionally_excluded
        );
    }

    /// A document that is a symlink to another is a second entry, not a second
    /// spelling of the first: both are filed under names of their own, infer
    /// ids of their own, and are two documents.
    #[test]
    #[cfg(unix)]
    fn a_document_linking_to_another_is_not_the_same_entry() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("a")).unwrap();
        fs::create_dir_all(root.join("z")).unwrap();
        fs::write(root.join("z/y.md"), "# Y").unwrap();
        std::os::unix::fs::symlink("../z/y.md", root.join("a/x.md")).unwrap();

        let mut config = Config::default();
        config.scope.include = vec!["**/*.md".to_string()];
        assert_eq!(
            scan_scope(root, &config).unwrap().paths,
            vec![PathBuf::from("a/x.md"), PathBuf::from("z/y.md")]
        );
    }

    /// A directory that is its own ancestor is a cycle and is not descended.
    /// Two spellings that are merely siblings are not, and both are walked.
    #[test]
    #[cfg(unix)]
    fn a_directory_link_pointing_upwards_terminates() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("docs/d.md"), "# D").unwrap();
        std::os::unix::fs::symlink("..", root.join("docs/up")).unwrap();

        let mut config = Config::default();
        config.scope.follow_symlinks = true;
        config.scope.include = vec!["**/*.md".to_string()];
        assert_eq!(
            scan_scope(root, &config).unwrap().paths,
            vec![PathBuf::from("docs/d.md")]
        );
    }

    #[test]
    fn default_prune_dirs_skips_dependency_trees() {
        let dir = TempDir::new().unwrap();
        let nm = dir.path().join("node_modules/pkg");
        fs::create_dir_all(&nm).unwrap();
        fs::write(nm.join("readme.md"), "# dep").unwrap();
        fs::write(dir.path().join("real.md"), "# real").unwrap();

        // Default config prunes node_modules.
        let paths = scan_scope(dir.path(), &Config::default()).unwrap().paths;
        assert!(paths.iter().any(|p| p.ends_with("real.md")));
        assert!(
            !paths
                .iter()
                .any(|p| p.to_string_lossy().contains("node_modules")),
            "node_modules pruned by default: {paths:?}"
        );
    }

    #[test]
    fn prune_dirs_override_admits_a_formerly_pruned_dir() {
        // A project that vaults docs under a directory named like a build
        // tree opts it back in by dropping it from scope.prune_dirs — the
        // prune list is config, not a hardcoded fact.
        let dir = TempDir::new().unwrap();
        let t = dir.path().join("target/docs");
        fs::create_dir_all(&t).unwrap();
        fs::write(t.join("spec.md"), "# spec").unwrap();

        let mut config = Config::default();
        config.scope.prune_dirs = vec!["node_modules".to_string(), ".git".to_string()];
        let paths = scan_scope(dir.path(), &config).unwrap().paths;
        assert!(
            paths.iter().any(|p| p.ends_with("spec.md")),
            "target/ is scannable when not in prune_dirs: {paths:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn scan_does_not_overflow_on_a_symlink_cycle() {
        // A symlinked directory pointing back into the tree must not loop
        // the scanner into a stack overflow (the recursive walk aborted
        // `nodex build` with SIGABRT). A descent stops at a directory whose
        // identity it already passed through, so the re-entry ends the branch:
        // the real file is found and the cycle adds no unbounded phantom paths.
        let dir = TempDir::new().unwrap();
        let docs = dir.path().join("docs");
        fs::create_dir_all(&docs).unwrap();
        fs::write(docs.join("a.md"), "# A").unwrap();
        // docs/loop -> project root, i.e. docs/loop/docs/loop/… forever.
        std::os::unix::fs::symlink(dir.path(), docs.join("loop")).unwrap();

        let mut config = Config::default();
        config.scope.include = vec!["**/*.md".to_string()];

        // Completes (no SIGABRT / hang); the real doc is found and the
        // cycle yields no unbounded multiplication of paths.
        let paths = scan_scope(dir.path(), &config).unwrap().paths;
        assert!(
            paths.iter().any(|p| p.ends_with("a.md")),
            "real file must be found: {paths:?}"
        );
        assert!(paths.len() < 5, "cycle must not multiply paths: {paths:?}");
    }

    #[test]
    fn conditional_exclude_drops_only_child_glob_matches() {
        // A terminal `SPEC.md` drops its `child_glob`-matching
        // sub-artefacts (here `tasks/*`) while keeping the parent — and
        // crucially leaves an independently-owned sibling (an active
        // decision log the rule never names) in scope, instead of
        // erasing the whole directory.
        let dir = TempDir::new().unwrap();
        let auth = dir.path().join("specs/auth");
        fs::create_dir_all(auth.join("tasks")).unwrap();
        fs::write(
            auth.join("SPEC.md"),
            "---\nid: spec-auth\ntitle: Auth\nkind: spec\nstatus: superseded\n---\n",
        )
        .unwrap();
        fs::write(
            auth.join("tasks/t1.md"),
            "---\nid: spec-auth-t1\ntitle: T1\nkind: spec\nstatus: draft\n---\n",
        )
        .unwrap();
        fs::write(
            auth.join("decisions.md"),
            "---\nid: dec-auth\ntitle: Decisions\nkind: generic\nstatus: active\n---\n",
        )
        .unwrap();

        let mut config = Config::default();
        config.scope.include = vec!["specs/**/*.md".to_string()];
        config.scope.conditional_exclude = vec![ConditionalExclude {
            parent_glob: "specs/**/SPEC.md".to_string(),
            child_glob: "specs/**/tasks/**".to_string(),
            condition: "status_terminal".to_string(),
        }];

        let scan = scan_scope(dir.path(), &config).unwrap();
        let kept: Vec<String> = scan
            .paths
            .iter()
            .map(|p| crate::path_guard::forward_string(p))
            .collect();
        assert!(
            kept.iter().any(|p| p.ends_with("SPEC.md")),
            "parent kept: {kept:?}"
        );
        assert!(
            kept.iter().any(|p| p.ends_with("decisions.md")),
            "independent active sibling must survive: {kept:?}"
        );
        assert!(
            !kept.iter().any(|p| p.contains("tasks/")),
            "child_glob sub-artefact dropped: {kept:?}"
        );
        // The drop is reported, not silent.
        assert_eq!(
            scan.conditionally_excluded
                .iter()
                .map(|p| crate::path_guard::forward_string(p))
                .collect::<Vec<_>>(),
            vec!["specs/auth/tasks/t1.md".to_string()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn conditional_exclude_degrades_on_unreadable_parent_and_build_reds_it() {
        // A parent the terminality probe cannot read is never *confirmed*
        // terminal, so the scan completes with its sub-artifacts in scope
        // (exclusion is a positive decision) — and the unreadable parent,
        // itself a scanned path, becomes a typed ParseFailure in the
        // build, so `check` reds the same file. Loud through the typed
        // channel, never a halted scan, never a silent exclusion.
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let auth = dir.path().join("specs/auth");
        fs::create_dir_all(auth.join("tasks")).unwrap();
        let spec = auth.join("SPEC.md");
        fs::write(
            &spec,
            "---\nid: spec-auth\ntitle: Auth\nkind: spec\nstatus: superseded\n---\n",
        )
        .unwrap();
        fs::write(
            auth.join("tasks/t1.md"),
            "---\nid: spec-auth-t1\ntitle: T1\nkind: spec\nstatus: draft\n---\n",
        )
        .unwrap();
        fs::set_permissions(&spec, fs::Permissions::from_mode(0o000)).unwrap();

        let mut config = Config::default();
        config.scope.include = vec!["specs/**/*.md".to_string()];
        config.scope.conditional_exclude = vec![ConditionalExclude {
            parent_glob: "specs/**/SPEC.md".to_string(),
            child_glob: "specs/**/tasks/**".to_string(),
            condition: "status_terminal".to_string(),
        }];

        let scan = scan_scope(dir.path(), &config).expect("scan completes");
        let kept: Vec<String> = scan
            .paths
            .iter()
            .map(|p| crate::path_guard::forward_string(p))
            .collect();
        assert!(
            kept.contains(&"specs/auth/tasks/t1.md".to_string()),
            "unconfirmed terminality keeps the children in scope: {kept:?}"
        );
        assert!(
            scan.conditionally_excluded.is_empty(),
            "nothing was excluded: {:?}",
            scan.conditionally_excluded
        );

        let outcome = crate::builder::build(dir.path(), &config, true).expect("build never halts");
        assert!(
            outcome
                .graph
                .parse_failures()
                .iter()
                .any(|f| f.path == "specs/auth/SPEC.md"),
            "the unreadable parent is a typed parse failure: {:?}",
            outcome.graph.parse_failures()
        );

        fs::set_permissions(&spec, fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[test]
    fn conditional_exclude_can_drop_whole_directory_explicitly() {
        // `child_glob = "**/*"` is the explicit opt-in to clearing the
        // whole subtree under a terminal parent.
        let dir = TempDir::new().unwrap();
        let auth = dir.path().join("specs/auth");
        fs::create_dir_all(&auth).unwrap();
        fs::write(
            auth.join("SPEC.md"),
            "---\nid: spec-auth\ntitle: Auth\nkind: spec\nstatus: superseded\n---\n",
        )
        .unwrap();
        fs::write(
            auth.join("tasks.md"),
            "---\nid: spec-auth-tasks\ntitle: Tasks\nkind: spec\nstatus: draft\n---\n",
        )
        .unwrap();

        let mut config = Config::default();
        config.scope.include = vec!["specs/**/*.md".to_string()];
        config.scope.conditional_exclude = vec![ConditionalExclude {
            parent_glob: "specs/**/SPEC.md".to_string(),
            child_glob: "**/*".to_string(),
            condition: "status_terminal".to_string(),
        }];

        let paths = scan_scope(dir.path(), &config).unwrap().paths;
        assert_eq!(paths.len(), 1, "only the parent survives: {paths:?}");
        assert!(paths[0].ends_with("SPEC.md"));
    }

    #[test]
    fn overlay_parent_status_drives_conditional_exclude_both_ways() {
        // The proposal's bytes — not the stale on-disk ones — decide a
        // conditional-exclude parent's terminality, so an overlay scan
        // matches the post-write world in both directions: a proposal
        // that terminalizes the parent drops its children, and one that
        // re-activates a terminal parent re-admits them.
        let dir = TempDir::new().unwrap();
        let auth = dir.path().join("specs/auth");
        fs::create_dir_all(auth.join("tasks")).unwrap();
        fs::write(
            auth.join("SPEC.md"),
            "---\nid: spec-auth\ntitle: Auth\nkind: spec\nstatus: active\n---\n",
        )
        .unwrap();
        fs::write(
            auth.join("tasks/t1.md"),
            "---\nid: spec-auth-t1\ntitle: T1\nkind: spec\nstatus: draft\n---\n",
        )
        .unwrap();

        let mut config = Config::default();
        config.scope.include = vec!["specs/**/*.md".to_string()];
        config.scope.conditional_exclude = vec![ConditionalExclude {
            parent_glob: "specs/**/SPEC.md".to_string(),
            child_glob: "specs/**/tasks/**".to_string(),
            condition: "status_terminal".to_string(),
        }];

        let terminal_parent = (
            PathBuf::from("specs/auth/SPEC.md"),
            Proposed::Content(
                "---\nid: spec-auth\ntitle: Auth\nkind: spec\nstatus: superseded\n---\n"
                    .to_string(),
            ),
        );
        let scan =
            scan_scope_with_overlay(dir.path(), &config, std::slice::from_ref(&terminal_parent))
                .unwrap();
        assert!(
            !scan
                .paths
                .iter()
                .any(|p| p.to_string_lossy().contains("tasks")),
            "a proposal terminalizing the parent must drop its children: {:?}",
            scan.paths
        );

        // Reverse: terminal on disk, proposal re-activates → children return.
        fs::write(
            auth.join("SPEC.md"),
            "---\nid: spec-auth\ntitle: Auth\nkind: spec\nstatus: superseded\n---\n",
        )
        .unwrap();
        let active_parent = (
            PathBuf::from("specs/auth/SPEC.md"),
            Proposed::Content(
                "---\nid: spec-auth\ntitle: Auth\nkind: spec\nstatus: active\n---\n".to_string(),
            ),
        );
        let scan =
            scan_scope_with_overlay(dir.path(), &config, std::slice::from_ref(&active_parent))
                .unwrap();
        assert!(
            scan.paths
                .iter()
                .any(|p| p.to_string_lossy().contains("tasks")),
            "a proposal re-activating a terminal parent must re-admit its children: {:?}",
            scan.paths
        );
    }

    #[test]
    fn greedy_include_skips_hidden_paths() {
        // A wildcard include that does not name a dotted segment leaves
        // hidden entries skipped — the ripgrep / fd / git convention.
        let dir = TempDir::new().unwrap();
        let docs = dir.path().join("docs");
        fs::create_dir_all(&docs).unwrap();
        fs::create_dir_all(dir.path().join(".archive")).unwrap();
        fs::write(docs.join("guide.md"), "# Guide").unwrap();
        fs::write(docs.join(".draft.md"), "# Draft").unwrap();
        fs::write(dir.path().join(".archive/old.md"), "# Old").unwrap();

        let mut config = Config::default();
        config.scope.include = vec!["**/*.md".to_string()];

        let paths = scan_scope(dir.path(), &config).unwrap().paths;
        let names: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        assert!(
            names.iter().any(|n| n.ends_with("guide.md")),
            "regular doc must be scanned: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains(".draft")),
            "hidden file must be skipped under a greedy include: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains(".archive")),
            "hidden directory must be skipped under a greedy include: {names:?}"
        );
    }

    #[test]
    fn include_naming_dotted_segment_admits_hidden_path() {
        // An include pattern that literally names a dotted segment opts
        // exactly that hidden path in — no separate flag. Non-content
        // directories (`.git`, `.venv`, `node_modules`) stay pruned even
        // if a sibling pattern would otherwise reach them.
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join(".claude/skills")).unwrap();
        fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
        fs::create_dir_all(dir.path().join(".git/refs")).unwrap();
        fs::create_dir_all(dir.path().join(".venv/lib")).unwrap();
        fs::write(dir.path().join(".claude/skills/x.md"), "# x").unwrap();
        fs::write(dir.path().join("node_modules/pkg/readme.md"), "# r").unwrap();
        fs::write(dir.path().join(".git/readme.md"), "# git").unwrap();
        fs::write(dir.path().join(".venv/lib/notes.md"), "# venv").unwrap();
        fs::write(dir.path().join("doc.md"), "# doc").unwrap();

        let mut config = Config::default();
        config.scope.include = vec![".claude/**/*.md".to_string(), "**/*.md".to_string()];
        let paths = scan_scope(dir.path(), &config).unwrap().paths;
        let names: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        assert!(names.iter().any(|n| n.contains(".claude")), "{names:?}");
        assert!(names.iter().any(|n| n.ends_with("doc.md")), "{names:?}");
        for noise in ["node_modules", ".git", ".venv"] {
            assert!(
                !names.iter().any(|n| n.contains(noise)),
                "non-content dir {noise:?} stays excluded: {names:?}"
            );
        }
    }

    #[test]
    fn dotted_include_is_position_anchored_not_basename() {
        // `.claude/**/*.md` opts in the ROOT `.claude`, not a `.claude`
        // nested anywhere. A vendored `sub/.claude/` must stay hidden
        // even though a sibling greedy `**/*.md` would otherwise match
        // its files once the directory is descended.
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join(".claude")).unwrap();
        fs::create_dir_all(dir.path().join("sub/.claude")).unwrap();
        fs::write(dir.path().join(".claude/root.md"), "# root").unwrap();
        fs::write(dir.path().join("sub/.claude/nested.md"), "# nested").unwrap();
        fs::write(dir.path().join("sub/plain.md"), "# plain").unwrap();

        let mut config = Config::default();
        config.scope.include = vec![".claude/**/*.md".to_string(), "**/*.md".to_string()];
        let paths = scan_scope(dir.path(), &config).unwrap().paths;
        let names: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        assert!(
            names
                .iter()
                .any(|n| n.contains(".claude") && n.contains("root")),
            "root .claude must be admitted: {names:?}"
        );
        assert!(
            names.iter().any(|n| n.ends_with("plain.md")),
            "non-hidden sibling must be admitted: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains("nested")),
            "nested sub/.claude must NOT be admitted by the root-anchored pattern: {names:?}"
        );
    }

    #[test]
    fn scan_excludes_patterns() {
        let dir = TempDir::new().unwrap();
        let docs = dir.path().join("docs");
        let index = docs.join("_index");
        fs::create_dir_all(&index).unwrap();
        fs::write(docs.join("guide.md"), "# Guide").unwrap();
        fs::write(index.join("generated.md"), "gen").unwrap();

        let mut config = Config::default();
        config.scope.include = vec!["**/*.md".to_string()];
        config.scope.exclude = vec!["docs/_index/**".to_string()];

        let paths = scan_scope(dir.path(), &config).unwrap().paths;
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("guide.md"));
    }
}
