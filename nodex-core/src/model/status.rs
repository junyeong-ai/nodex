use serde::{Deserialize, Serialize};
use std::fmt;

/// Lifecycle status. Config-driven — no hardcoded variants.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
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

impl Default for Status {
    fn default() -> Self {
        Self("active".to_string())
    }
}

impl From<&str> for Status {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}
