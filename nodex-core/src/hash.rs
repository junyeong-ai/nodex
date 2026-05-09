//! SHA256 hex hashing — single source of truth for content
//! fingerprints. Used by the build cache (full hex, for content-change
//! detection) and the GRAPH.md report (truncated to 16 chars for the
//! generation stamp). Centralised so swapping algorithms is a
//! single-file change.

use sha2::{Digest, Sha256};
use std::fmt::Write;

/// Lowercase hex SHA256 digest of `content`.
pub fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hasher.finalize().iter().fold(String::new(), |mut acc, b| {
        Write::write_fmt(&mut acc, format_args!("{b:02x}")).unwrap();
        acc
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_produces_known_digest() {
        // SHA256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn output_is_lowercase_64_hex_chars() {
        let hex = sha256_hex("hello");
        assert_eq!(hex.len(), 64);
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }
}
