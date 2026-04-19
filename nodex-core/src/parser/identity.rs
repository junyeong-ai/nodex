use globset::Glob;
use std::path::Path;

use crate::config::Config;
use crate::model::Kind;

/// Built-in fallback kind used when no `identity.kind_rules` glob
/// matches a document's path. `Config::validate` requires this value
/// to stay in `kinds.allowed` so migrate / parse can never produce a
/// document with an out-of-vocabulary kind.
pub const FALLBACK_KIND: &str = "generic";

/// Infer document kind from path using config rules. First match wins.
pub fn infer_kind(path: &Path, config: &Config) -> Kind {
    let path_str = normalize_path(path);

    for rule in &config.identity.kind_rules {
        if let Ok(glob) = Glob::new(&rule.glob) {
            let matcher = glob.compile_matcher();
            if matcher.is_match(&path_str) {
                return Kind::new(&rule.kind);
            }
        }
    }

    Kind::new(FALLBACK_KIND)
}

/// Infer document id from path and kind using config template rules.
pub fn infer_id(path: &Path, kind: &Kind, config: &Config) -> String {
    let path_str = normalize_path(path);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    let parent = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let path_slug = slugify_path(path);

    for rule in &config.identity.id_rules {
        // Check kind match (* = wildcard)
        if rule.kind != "*" && rule.kind != kind.as_str() {
            continue;
        }

        // Check optional glob match
        if let Some(ref glob_str) = rule.glob {
            if let Ok(glob) = Glob::new(glob_str) {
                let matcher = glob.compile_matcher();
                if !matcher.is_match(&path_str) {
                    continue;
                }
            } else {
                continue;
            }
        }

        return expand_template(&rule.template, kind.as_str(), stem, parent, &path_slug);
    }

    // Default fallback
    format!("{}-{}", kind, slugify(stem))
}

fn expand_template(
    template: &str,
    kind: &str,
    stem: &str,
    parent: &str,
    path_slug: &str,
) -> String {
    template
        .replace("{kind}", kind)
        .replace("{stem}", &slugify(stem))
        .replace("{parent}", &slugify(parent))
        .replace("{path_slug}", path_slug)
}

/// Normalize path separators to forward slash.
fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Convert a string to a slug (lowercase, non-alphanum → hyphen, collapse).
fn slugify(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut prev_hyphen = false;

    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            result.push(c.to_ascii_lowercase());
            prev_hyphen = false;
        } else if (c == '-' || c == '_' || c == '.' || c == ' ')
            && !prev_hyphen
            && !result.is_empty()
        {
            result.push('-');
            prev_hyphen = true;
        }
    }

    // Trim trailing hyphen
    if result.ends_with('-') {
        result.pop();
    }

    result
}

/// Slugify the full relative path (without extension).
fn slugify_path(path: &Path) -> String {
    let without_ext = path.with_extension("");
    slugify(&without_ext.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{IdRule, IdentityConfig, KindRule};

    fn make_config(kind_rules: Vec<KindRule>, id_rules: Vec<IdRule>) -> Config {
        Config {
            identity: IdentityConfig {
                kind_rules,
                id_rules,
            },
            ..Config::default()
        }
    }

    #[test]
    fn infer_kind_by_glob() {
        let config = make_config(
            vec![KindRule {
                glob: "docs/decisions/**".to_string(),
                kind: "adr".to_string(),
            }],
            vec![],
        );
        let kind = infer_kind(Path::new("docs/decisions/0001-auth.md"), &config);
        assert_eq!(kind.as_str(), "adr");
    }

    #[test]
    fn infer_kind_fallback_generic() {
        let config = make_config(vec![], vec![]);
        let kind = infer_kind(Path::new("random/file.md"), &config);
        assert_eq!(kind.as_str(), "generic");
    }

    #[test]
    fn infer_id_template() {
        let config = make_config(
            vec![],
            vec![
                IdRule {
                    kind: "adr".to_string(),
                    glob: None,
                    template: "adr-{stem}".to_string(),
                },
                IdRule {
                    kind: "*".to_string(),
                    glob: None,
                    template: "{kind}-{stem}".to_string(),
                },
            ],
        );
        let id = infer_id(
            Path::new("docs/decisions/0001-auth-protocol.md"),
            &Kind::new("adr"),
            &config,
        );
        assert_eq!(id, "adr-0001-auth-protocol");
    }

    #[test]
    fn infer_id_with_glob() {
        let config = make_config(
            vec![],
            vec![
                IdRule {
                    kind: "readme".to_string(),
                    glob: Some("README.md".to_string()),
                    template: "readme-root".to_string(),
                },
                IdRule {
                    kind: "readme".to_string(),
                    glob: None,
                    template: "readme-{parent}".to_string(),
                },
            ],
        );

        let id1 = infer_id(Path::new("README.md"), &Kind::new("readme"), &config);
        assert_eq!(id1, "readme-root");

        let id2 = infer_id(
            Path::new("packages/core/README.md"),
            &Kind::new("readme"),
            &config,
        );
        assert_eq!(id2, "readme-core");
    }

    #[test]
    fn infer_id_default_fallback() {
        let config = make_config(vec![], vec![]);
        let id = infer_id(Path::new("docs/guide.md"), &Kind::new("guide"), &config);
        assert_eq!(id, "guide-guide");
    }

    #[test]
    fn slugify_preserves_numbers() {
        assert_eq!(slugify("0001-auth-protocol"), "0001-auth-protocol");
    }

    #[test]
    fn slugify_strips_special_chars() {
        assert_eq!(slugify("Hello World!@#"), "hello-world");
    }
}
