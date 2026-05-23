use anyhow::Result;
use std::path::Path;

use crate::format::{Envelope, ItemsEnvelope, print_json};

use super::load_graph;

pub(crate) fn run_annotations(
    root: &Path,
    name: Option<&str>,
    with_frontmatter: Vec<String>,
    min_count: usize,
    pretty: bool,
) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    if let Some(filter) = name
        && !config.annotations.iter().any(|a| a.name == filter)
    {
        let known: Vec<&str> = config.annotations.iter().map(|a| a.name.as_str()).collect();
        return Err(nodex_core::error::Error::Config(format!(
            "--name {filter:?} is not a declared annotation pattern; known: {known:?}"
        ))
        .into());
    }
    if min_count == 0 {
        return Err(nodex_core::error::Error::Config(
            "--min-count must be >= 1; omit the flag to keep every entry".into(),
        )
        .into());
    }
    if !with_frontmatter.is_empty() {
        let universe = config.declared_fields_universe();
        let unknown: Vec<&str> = with_frontmatter
            .iter()
            .filter(|f| !universe.contains(f.as_str()))
            .map(String::as_str)
            .collect();
        if !unknown.is_empty() {
            let mut known_sorted: Vec<&str> = universe.iter().map(String::as_str).collect();
            known_sorted.sort_unstable();
            return Err(nodex_core::error::Error::Config(format!(
                "--with-frontmatter contains unknown field(s) {unknown:?}; declared: {known_sorted:?}"
            ))
            .into());
        }
    }
    let graph = load_graph(root, &config)?;
    let items = nodex_core::query::annotations::find_annotations(
        &graph,
        &nodex_core::AnnotationOptions {
            pattern: name,
            with_frontmatter: &with_frontmatter,
            min_count,
        },
    );
    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}
