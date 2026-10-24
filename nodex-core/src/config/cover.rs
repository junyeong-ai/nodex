//! Whether a first-match entry can ever apply. `identity.kind_rules`,
//! `identity.id_rules` and `detection.unresolved_policy` take the first entry
//! that selects a path, so an entry whose every path an earlier one selects
//! never decides anything; the comparison is of the paths the globs select,
//! never of how they are spelled.

use regex_automata::dfa::{Automaton, StartKind, dense};
use regex_automata::nfa::thompson;
use regex_automata::util::{start, syntax};
use regex_automata::{Anchored, MatchKind};

/// Bytes either automaton may take. A glob compiles to a handful of states;
/// a pattern past this is one the comparison declines to decide.
const SIZE_LIMIT: usize = 1 << 20;

/// Whether every path `later` selects is selected by one of `earlier`,
/// `None` standing for a declaration that selects every path.
///
/// Decided over the languages globset compiles each glob to, so `docs/**`
/// covers `docs/adr/*.md` while `docs/adr/0*.md` does not cover `docs/adr/**`,
/// however the documents happen to be named today. Covering is claimed only
/// where it is proven: an automaton past [`SIZE_LIMIT`] answers `false`, which
/// leaves a declaration standing rather than refusing one that may apply.
pub(super) fn covers(earlier: &[Option<&str>], later: Option<&str>) -> bool {
    if earlier.iter().any(|glob| glob.is_none() || *glob == later) {
        return true;
    }
    let Some(earlier) = earlier
        .iter()
        .flatten()
        .map(|glob| regex_of(glob))
        .collect::<Option<Vec<_>>>()
        .filter(|patterns| !patterns.is_empty())
    else {
        return false;
    };
    let later = match later {
        Some(glob) => regex_of(glob),
        None => Some("(?-u)^.*$".to_string()),
    };
    let (Some(later), Some(earlier)) = (later.and_then(|p| automaton(&[p])), automaton(&earlier))
    else {
        return false;
    };
    includes(&earlier, &later)
}

/// The earlier entries that select every path `later` does, by position: one
/// that does so alone where there is one, every candidate otherwise, and
/// `None` while some path is left for `later`.
pub(super) fn covered_by(
    earlier: &[(usize, Option<&str>)],
    later: Option<&str>,
) -> Option<Vec<usize>> {
    let globs: Vec<Option<&str>> = earlier.iter().map(|(_, glob)| *glob).collect();
    if !covers(&globs, later) {
        return None;
    }
    let alone = earlier
        .iter()
        .find(|(_, glob)| covers(&[*glob], later))
        .map(|(index, _)| vec![*index]);
    Some(alone.unwrap_or_else(|| earlier.iter().map(|(index, _)| *index).collect()))
}

fn regex_of(glob: &str) -> Option<String> {
    globset::Glob::new(glob)
        .ok()
        .map(|glob| glob.regex().to_string())
}

/// One automaton accepting exactly the paths any of `patterns` matches, built
/// under the syntax globset matches with.
fn automaton(patterns: &[String]) -> Option<dense::DFA<Vec<u32>>> {
    let alternation = patterns
        .iter()
        .map(|pattern| format!("(?:{pattern})"))
        .collect::<Vec<_>>()
        .join("|");
    dense::Builder::new()
        .configure(
            dense::Config::new()
                .start_kind(StartKind::Anchored)
                .match_kind(MatchKind::All)
                .dfa_size_limit(Some(SIZE_LIMIT))
                .determinize_size_limit(Some(SIZE_LIMIT)),
        )
        .syntax(syntax::Config::new().utf8(false).dot_matches_new_line(true))
        .thompson(thompson::Config::new().utf8(false))
        .build(&alternation)
        .ok()
}

/// Whether `inner` accepts no input `outer` rejects: a search of the state
/// pairs both reach on one input for a pair where `inner` accepts at the end
/// and `outer` does not.
fn includes(outer: &dense::DFA<Vec<u32>>, inner: &dense::DFA<Vec<u32>>) -> bool {
    let anchored = start::Config::new().anchored(Anchored::Yes);
    let (Ok(inner_start), Ok(outer_start)) =
        (inner.start_state(&anchored), outer.start_state(&anchored))
    else {
        return false;
    };
    let mut seen = std::collections::HashSet::from([(inner_start, outer_start)]);
    let mut pending = vec![(inner_start, outer_start)];
    while let Some((at_inner, at_outer)) = pending.pop() {
        if inner.is_match_state(inner.next_eoi_state(at_inner))
            && !outer.is_match_state(outer.next_eoi_state(at_outer))
        {
            return false;
        }
        for byte in 0..=u8::MAX {
            let next_inner = inner.next_state(at_inner, byte);
            if inner.is_dead_state(next_inner) {
                continue;
            }
            let pair = (next_inner, outer.next_state(at_outer, byte));
            if seen.insert(pair) {
                pending.push(pair);
            }
        }
    }
    true
}
