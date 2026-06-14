//! Project configuration (`nodex.toml`): the single source of truth for
//! all project-varying behaviour. Split by concern — data [`types`],
//! load-time [`validate`]ation, runtime [`views`], and the cross_field
//! [`predicate`] language — re-exported here so `crate::config::*` is the
//! one import path.

mod predicate;
mod types;
mod validate;
mod views;

pub use predicate::{
    BUILTIN_COLLECTION_FIELDS, BUILTIN_SCALAR_FIELDS, INFERRED_FRONTMATTER_FIELDS, WhenPredicate,
    is_builtin_node_field, is_collection_builtin, parse_when,
};
pub use types::*;
pub(crate) use views::resolve_initial_status;

#[cfg(test)]
mod tests;
