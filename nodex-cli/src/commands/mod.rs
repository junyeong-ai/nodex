pub mod build;
pub mod check;
pub mod content_source;
pub mod diff;
pub mod export;
pub mod git_worktree;
pub mod impact;
pub mod init;
pub mod lifecycle;
pub mod migrate;
pub mod query;
pub mod rename;
pub mod report;
pub mod retarget;
pub mod scaffold;
pub mod status;

/// The parts a write left as it found them, each named with the rule that
/// holds it — two locks can hold two parts of one document, and an operator
/// sent to the wrong rule has nothing to read.
pub fn held_back_parts(parts: &[(nodex_core::DocumentPart, String)]) -> String {
    let named: Vec<String> = parts
        .iter()
        .map(|(part, lock)| format!("{part} ({lock})"))
        .collect();
    match named.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}
