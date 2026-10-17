use fluent_uri::{
    component::Scheme,
    pct_enc::{EString, Encoder, Table, encoder::IPath},
};

/// A local Markdown destination, after Markdown escapes and entities have
/// been read. URI separators are split before percent-decoding, so `%23`
/// and `%3F` name path characters rather than a fragment or query.
pub(crate) struct LocalDestination<'a> {
    pub path: String,
    pub suffix: &'a str,
}

impl<'a> LocalDestination<'a> {
    pub fn parse(destination: &'a str) -> Option<Self> {
        let destination = destination.trim();
        let end = destination.find(['?', '#']).unwrap_or(destination.len());
        let path = &destination[..end];
        if path.is_empty()
            || path.starts_with("//")
            || path
                .split_once(':')
                .is_some_and(|(scheme, _)| Scheme::new(scheme).is_some())
        {
            return None;
        }
        let path = percent_encoding::percent_decode_str(path)
            .decode_utf8()
            .ok()?
            .into_owned();
        Some(Self {
            path,
            suffix: &destination[end..],
        })
    }
}

struct LocalPath;

impl Encoder for LocalPath {
    const TABLE: &'static Table = &IPath::TABLE.sub(&Table::new(b":"));
}

pub(crate) fn encode_path(path: &str) -> String {
    let mut encoded = EString::<LocalPath>::new();
    encoded.encode_str::<LocalPath>(path);
    encoded.into_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_destinations_separate_uri_syntax_from_path_bytes() {
        for (source, path, suffix) in [
            (
                "space%20name.md?plain=1#part",
                "space name.md",
                "?plain=1#part",
            ),
            ("한글.md#제목", "한글.md", "#제목"),
            ("<file>.md", "<file>.md", ""),
            ("a%23b%3Fc%25.md", "a#b?c%.md", ""),
            ("./name:part.md", "./name:part.md", ""),
            ("%252e%252e/a.md", "%2e%2e/a.md", ""),
        ] {
            let local = LocalDestination::parse(source).unwrap();
            assert_eq!((local.path.as_str(), local.suffix), (path, suffix));
        }
        for external in [
            "HTTPS://host/a.md",
            "s3://bucket/a.md",
            "file:///a.md",
            "custom:a.md",
            "//host/a.md",
            "#part",
            "?plain=1",
            "",
        ] {
            assert!(LocalDestination::parse(external).is_none(), "{external}");
        }
    }

    #[test]
    fn encoded_paths_round_trip_without_decoding_twice() {
        for path in [
            "a b.md",
            "a#b?c%.md",
            "한글.md",
            "[a].md",
            "%20.md",
            "name:colon.md",
            "name:directory/a.md",
        ] {
            let encoded = encode_path(path);
            assert_eq!(LocalDestination::parse(&encoded).unwrap().path, path);
        }
    }
}
