use serde::{Deserialize, Serialize};
use std::fmt;

/// Document kind. Config-driven — no hardcoded variants.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Kind(String);

impl Kind {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Kind {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}
