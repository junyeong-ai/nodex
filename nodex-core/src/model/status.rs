use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Lifecycle status. Config-driven — no hardcoded variants.
///
/// Deliberately does not implement [`Default`]: the canonical
/// project-wide default lives in [`crate::config::Config::initial_status`]
/// and depends on the document's kind. A blanket `Default` here would
/// hardcode a status string the user's config might not even allow,
/// re-introducing exactly the kind of out-of-vocabulary write the
/// config validator is built to prevent.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct Status(String);

impl Status {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Status {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}
