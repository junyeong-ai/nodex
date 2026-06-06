use anyhow::{Context, Result};
use clap::Args;
use std::path::{Component, Path, PathBuf};

use nodex_core::Config;
use nodex_core::command_result::{IdStability, RenameResult};
use nodex_core::error::Error as CoreError;
use nodex_core::parser::editor::{FrontmatterEditor, Scalar};
use nodex_core::parser::frontmatter::split_frontmatter;
use nodex_core::parser::identity::{infer_id, infer_kind};

use crate::format::{Envelope, print_json};

/// Args for `nodex rename`.
#[derive(Args)]
pub struct RenameArgs {
    /// Source path (relative to root).
    pub old: String,
    /// Target path (relative to root).
    pub new: String,
}

pub fn run(root: &Path, args: RenameArgs, pretty: bool) -> Result<()> {
    let old_path = args.old.as_str();
    let new_path = args.new.as_str();
    let config = nodex_core::load_project_for_mutation(root)?;

    // Refuse `..` / absolute forms in either argument so an AI agent
    // or a typoed invocation cannot move a project file outside root.
    nodex_core::path_guard::reject_traversal(Path::new(old_path))?;
    nodex_core::path_guard::reject_traversal(Path::new(new_path))?;

    let old_abs = root.join(old_path);
    let new_abs = root.join(new_path);

    if !old_abs.exists() {
        return Err(CoreError::Io {
            path: old_abs,
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        }
        .into());
    }
    if new_abs.exists() {
        return Err(CoreError::Exists(new_abs).into());
    }

    // ─── id-stability anchoring ────────────────────────────────────
    //
    // Before the move, check whether the *inferred* id would change
    // (path-derived ids depend on the file's stem / parent / glob).
    // If yes and the doc doesn't already pin an explicit `id:`, write
    // the current effective id into the doc's frontmatter so the move
    // doesn't silently break every cross-document `related:` /
    // `supersedes:` / `implements:` reference in the rest of the
    // graph. This is the single seam where rename guarantees that the
    // file-system primitive (`fs::rename`) doesn't produce a broken
    // semantic graph.
    let stability =
        anchor_id_before_move(&old_abs, Path::new(old_path), Path::new(new_path), &config)?;

    if let Some(parent) = new_abs.parent() {
        std::fs::create_dir_all(parent).map_err(|source| CoreError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    // The file move itself stays a `rename` — that *is* the atomic
    // primitive. The link rewriter below has to be guarded separately.
    std::fs::rename(&old_abs, &new_abs).map_err(|source| CoreError::Io {
        path: old_abs.clone(),
        source,
    })?;

    // Update body-link references by walking every in-scope document,
    // parsing its markdown links, and rewriting each whose target
    // resolves to the renamed file. We resolve each link against the
    // linking file's own directory and compare to the normalised
    // renamed path, so both `[x](docs/decisions/first.md)` and
    // `[x](first.md)` (written from `docs/decisions/second.md`) update
    // correctly.
    let paths =
        nodex_core::builder::scanner::scan_scope(root, &config).context("scope scan failed")?;

    let old_norm = normalize(&PathBuf::from(old_path));
    let new_norm = normalize(&PathBuf::from(new_path));

    let link_re = regex::Regex::new(r"\]\(([^)#\s]+)(#[^)]*)?\)").expect("static regex compiles");

    let mut updated_files = Vec::new();

    for rel_path in &paths {
        let abs_path = root.join(rel_path);
        // Symlinks follow the project-wide writer-skips / reader-follows
        // pattern: scanning indexes the linked file, but rewriting
        // would mutate whatever the symlink points at — possibly
        // outside the project root.
        if nodex_core::path_guard::is_symlink(&abs_path) {
            continue;
        }
        let content = match std::fs::read_to_string(&abs_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let parent_dir = rel_path.parent().unwrap_or_else(|| Path::new(""));
        let mut changed = false;

        let rewritten = link_re.replace_all(&content, |caps: &regex::Captures<'_>| {
            let url = &caps[1];
            let anchor = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            let url_as_is = normalize(&PathBuf::from(url));
            let url_relative = normalize(&parent_dir.join(url));

            // Match both authoring styles: a link written root-relative
            // (`docs/b.md`) and one written file-relative (`b.md` from
            // inside `docs/`). Preserve the author's style in the
            // rewritten URL so their intent survives the rename.
            if url_as_is == old_norm {
                changed = true;
                format!("]({}{anchor})", new_path)
            } else if url_relative == old_norm {
                changed = true;
                let new_rel = relative_from(parent_dir, &new_norm);
                format!(
                    "]({}{anchor})",
                    nodex_core::path_guard::forward_string(&new_rel)
                )
            } else {
                caps[0].to_string()
            }
        });

        if changed {
            nodex_core::path_guard::write_atomic(&abs_path, rewritten.as_ref())?;
            updated_files.push(nodex_core::path_guard::forward_string(rel_path));
        }
    }

    let warnings: Vec<String> = match &stability {
        IdStability::BareNoFrontmatter { warning } => vec![warning.clone()],
        _ => Vec::new(),
    };

    let data = RenameResult {
        old_path: old_path.to_string(),
        new_path: new_path.to_string(),
        total_updated: updated_files.len(),
        references_updated: updated_files,
        id_stability: stability,
    };

    if warnings.is_empty() {
        print_json(&Envelope::success(data), pretty);
    } else {
        print_json(&Envelope::with_warnings(data, warnings), pretty);
    }

    Ok(())
}

/// Read the doc at `old_abs`, compare its effective id against the id
/// it *would* infer at `new_rel`, and — if a path-derived id would
/// change — anchor the previous id into the doc's frontmatter before
/// the move. Returns the [`IdStability`] outcome for the envelope.
///
/// This is the only mutation point for stability anchoring; it runs
/// before `fs::rename` so a write failure aborts cleanly without
/// leaving the move half-done.
fn anchor_id_before_move(
    old_abs: &Path,
    old_rel: &Path,
    new_rel: &Path,
    config: &Config,
) -> Result<IdStability> {
    let content = std::fs::read_to_string(old_abs).map_err(|source| CoreError::Io {
        path: old_abs.to_path_buf(),
        source,
    })?;
    let (yaml_opt, body) = split_frontmatter(&content);

    // Kind inference uses the *current* (old) path so the doc's
    // existing identity is what we anchor — never a kind the renamed
    // location would happen to land in.
    let old_kind = infer_kind(old_rel, &config.identity);
    let new_kind = infer_kind(new_rel, &config.identity);
    let inferred_old_id = infer_id(old_rel, &old_kind, &config.identity);
    let inferred_new_id = infer_id(new_rel, &new_kind, &config.identity);

    let Some(yaml) = yaml_opt else {
        // Bare markdown: nodex still infers an id from the path and
        // other docs can reference it. Path change → id change. We
        // refuse to silently invent a frontmatter block (too invasive
        // for a path operation), but surface a warning so the caller
        // can fix up references manually instead of discovering broken
        // edges on the next `build`.
        if inferred_old_id != inferred_new_id {
            return Ok(IdStability::BareNoFrontmatter {
                warning: format!(
                    "renamed file has no frontmatter; its inferred id changed from \
                     {inferred_old_id:?} to {inferred_new_id:?}. Other documents \
                     referencing {inferred_old_id:?} via `related` / `supersedes` / \
                     `implements` / `superseded_by` will become stale. Add an explicit \
                     `id:` frontmatter to the file (or run `nodex migrate --apply` to \
                     generate one) and re-run rename to re-anchor."
                ),
            });
        }
        return Ok(IdStability::Unchanged);
    };

    let mut editor = FrontmatterEditor::parse(yaml, old_abs)?;
    let effective_old_id = match editor.scalar("id") {
        Scalar::Value(v) if !v.is_empty() => {
            // Explicit id already pinned; move is path-only by
            // construction, no anchoring needed.
            return Ok(IdStability::AlreadyAnchored);
        }
        Scalar::Value(_) | Scalar::Absent => inferred_old_id.clone(),
        Scalar::NonScalar => {
            // An `id:` field that isn't a scalar (e.g., a list) is a
            // pre-existing authoring error; surface it instead of
            // attempting to silently overwrite the structure.
            return Err(CoreError::Config(format!(
                "{} has an `id:` field that is not a scalar; cannot anchor it during rename",
                old_abs.display()
            ))
            .into());
        }
    };

    if inferred_old_id == inferred_new_id {
        return Ok(IdStability::Unchanged);
    }

    editor.set("id", &effective_old_id);
    let new_frontmatter = editor.render();
    // Rewrite the source file in place so the post-move file already
    // carries the anchored id. Using the project-wide atomic-write
    // primitive keeps the failure mode binary (old content intact or
    // new content written — never half-written).
    let rewritten = format!("---\n{new_frontmatter}---\n{body}");
    nodex_core::path_guard::write_atomic(old_abs, &rewritten)?;

    Ok(IdStability::Anchored {
        id: effective_old_id,
    })
}

/// Resolve `.` and `..` segments without filesystem access.
/// `docs/./a/../b.md` → `docs/b.md`.
fn normalize(p: &Path) -> PathBuf {
    let mut parts: Vec<Component<'_>> = Vec::new();
    for component in p.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.iter().collect()
}

/// Compute `target` as a path relative to `from_dir` (both
/// project-root-relative). Emits `..` segments where needed and
/// returns just the filename when both paths share a parent.
fn relative_from(from_dir: &Path, target: &Path) -> PathBuf {
    let from_components: Vec<_> = from_dir.components().collect();
    let target_components: Vec<_> = target.components().collect();
    let common = from_components
        .iter()
        .zip(target_components.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let ups = from_components.len() - common;
    let mut result = PathBuf::new();
    for _ in 0..ups {
        result.push("..");
    }
    for c in &target_components[common..] {
        result.push(c);
    }
    if result.as_os_str().is_empty() {
        result.push(".");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_resolves_dot_dot() {
        assert_eq!(
            normalize(&PathBuf::from("docs/./a/../b.md")),
            PathBuf::from("docs/b.md")
        );
    }

    #[test]
    fn relative_same_dir() {
        assert_eq!(
            relative_from(
                Path::new("docs/decisions"),
                Path::new("docs/decisions/x.md")
            ),
            PathBuf::from("x.md")
        );
    }

    #[test]
    fn relative_walks_up() {
        assert_eq!(
            relative_from(Path::new("docs/a"), Path::new("docs/b/x.md")),
            PathBuf::from("../b/x.md")
        );
    }
}
