//! Whether a first-match entry can ever apply. `identity.kind_rules`,
//! `identity.id_rules` and `detection.unresolved_policy` take the first entry
//! that selects a path, so an entry whose every path an earlier one selects
//! never decides anything; the comparison is of the paths the globs select,
//! never of how they are spelled.
//!
//! Each glob compiles once to the minimal automaton of the paths globset
//! matches it against, and covering is searched over the states those
//! automata reach together on one input. An automaton of the earlier globs'
//! union is never built: a glob such as `**/plans/*.md` remembers whether its
//! directory has been passed, so determinizing a union of them doubles with
//! every glob it joins.

use std::collections::{HashMap, HashSet, VecDeque};

use regex_automata::dfa::{Automaton, StartKind, dense};
use regex_automata::nfa::thompson;
use regex_automata::util::{start, syntax};
use regex_automata::{Anchored, MatchKind};

/// Bytes one glob's automaton may take while it is built. A glob compiles to
/// a handful of states; a pattern past this is one the comparison declines.
const SIZE_LIMIT: usize = 1 << 20;

/// State combinations one search keeps before it declines, far past the few
/// dozen a table of path globs keeps.
const SEARCH_LIMIT: usize = 1 << 16;

/// What a declaration without a glob selects: every path.
const EVERY_PATH: &str = "**";

/// The globs of one first-match table, each compiled the first time it is
/// compared.
#[derive(Default)]
pub(super) struct Coverage<'a> {
    selections: HashMap<&'a str, Option<Selection>>,
}

impl<'a> Coverage<'a> {
    /// The earlier entries that select every path `later` does, by position:
    /// one that does so alone where there is one, otherwise entries none of
    /// which the others could do without, and `None` while some path is left
    /// for `later`. `None` as a glob stands for a declaration that selects
    /// every path.
    ///
    /// Decided over the languages globset compiles each glob to, so `docs/**`
    /// covers `docs/adr/*.md` while `docs/adr/0*.md` does not cover
    /// `docs/adr/**`, however the documents happen to be named today. Covering
    /// is claimed only where it is proven: a glob past [`SIZE_LIMIT`] or a
    /// search past [`SEARCH_LIMIT`] answers `None`, which leaves a declaration
    /// standing rather than refusing one that may apply.
    pub(super) fn covered_by(
        &mut self,
        earlier: &[(usize, Option<&'a str>)],
        later: Option<&'a str>,
    ) -> Option<Vec<usize>> {
        let globs: Vec<Option<&'a str>> = earlier.iter().map(|(_, glob)| *glob).collect();
        if !self.covers(&globs, later) {
            return None;
        }
        if let Some((index, _)) = earlier
            .iter()
            .find(|(_, glob)| self.covers(&[*glob], later))
        {
            return Some(vec![*index]);
        }
        let mut covering = earlier.to_vec();
        let mut at = 0;
        while at < covering.len() {
            let rest: Vec<Option<&'a str>> = covering
                .iter()
                .enumerate()
                .filter(|(position, _)| *position != at)
                .map(|(_, (_, glob))| *glob)
                .collect();
            if self.covers(&rest, later) {
                covering.remove(at);
            } else {
                at += 1;
            }
        }
        Some(covering.into_iter().map(|(index, _)| index).collect())
    }

    fn covers(&mut self, earlier: &[Option<&'a str>], later: Option<&'a str>) -> bool {
        if earlier.iter().any(|glob| glob.is_none() || *glob == later) {
            return true;
        }
        let earlier: Vec<&'a str> = earlier.iter().flatten().copied().collect();
        if earlier.is_empty() {
            return false;
        }
        let later = later.unwrap_or(EVERY_PATH);
        for glob in earlier.iter().copied().chain([later]) {
            self.selections
                .entry(glob)
                .or_insert_with(|| Selection::of(glob));
        }
        let (Some(later), Some(earlier)) = (
            self.selections[later].as_ref(),
            earlier
                .iter()
                .map(|glob| self.selections[glob].as_ref())
                .collect::<Option<Vec<_>>>(),
        ) else {
            return false;
        };
        includes(&earlier, later)
    }
}

/// The paths one glob selects, as a minimal automaton tabulated over the byte
/// classes it distinguishes. State 0 is where every path starts.
struct Selection {
    /// The class of each byte; bytes of one class always step alike.
    classes: [u8; 256],
    alphabet: usize,
    /// `next[state * alphabet + class]`.
    next: Vec<u32>,
    /// Whether the glob selects a path ending in the state.
    accepts: Vec<bool>,
    /// Whether some continuation from the state is selected.
    live: Vec<bool>,
    /// Whether every continuation from the state is selected.
    universal: Vec<bool>,
    /// `subsumes[a * states + b]`: every continuation `b` selects, `a`
    /// selects too.
    subsumes: Vec<bool>,
}

impl Selection {
    /// Compiled under the syntax globset matches with, or `None` for a glob
    /// that does not compile or whose automaton passes [`SIZE_LIMIT`].
    fn of(glob: &str) -> Option<Self> {
        let regex = globset::Glob::new(glob).ok()?.regex().to_string();
        let dfa = dense::Builder::new()
            .configure(
                dense::Config::new()
                    .start_kind(StartKind::Anchored)
                    .match_kind(MatchKind::All)
                    .minimize(true)
                    .dfa_size_limit(Some(SIZE_LIMIT))
                    .determinize_size_limit(Some(SIZE_LIMIT)),
            )
            .syntax(syntax::Config::new().utf8(false).dot_matches_new_line(true))
            .thompson(thompson::Config::new().utf8(false))
            .build(&regex)
            .ok()?;
        let first = dfa
            .start_state(&start::Config::new().anchored(Anchored::Yes))
            .ok()?;

        let mut classes = [0u8; 256];
        for byte in 0..=u8::MAX {
            classes[usize::from(byte)] = dfa.byte_classes().get(byte);
        }
        let alphabet = usize::from(classes.into_iter().max().unwrap_or(0)) + 1;
        let mut representatives = vec![0u8; alphabet];
        for byte in 0..=u8::MAX {
            representatives[usize::from(classes[usize::from(byte)])] = byte;
        }

        let mut index = HashMap::from([(first, 0u32)]);
        let mut order = vec![first];
        let mut next = Vec::new();
        let mut at = 0;
        while let Some(&state) = order.get(at) {
            for &byte in &representatives {
                let to = dfa.next_state(state, byte);
                let id = *index.entry(to).or_insert_with(|| {
                    order.push(to);
                    u32::try_from(order.len() - 1).expect("a glob automaton fits in u32 states")
                });
                next.push(id);
            }
            at += 1;
        }
        let states = order.len();
        let accepts: Vec<bool> = order
            .iter()
            .map(|&state| dfa.is_match_state(dfa.next_eoi_state(state)))
            .collect();
        let step = |state: usize, class: usize| next[state * alphabet + class] as usize;

        let live = fixpoint(accepts.clone(), |live, state| {
            live[state] || (0..alphabet).any(|class| live[step(state, class)])
        });
        let universal = fixpoint(accepts.clone(), |universal, state| {
            universal[state] && (0..alphabet).all(|class| universal[step(state, class)])
        });
        let subsumes = fixpoint(
            (0..states * states)
                .map(|pair| accepts[pair % states] <= accepts[pair / states])
                .collect(),
            |subsumes, pair| {
                let (a, b) = (pair / states, pair % states);
                subsumes[pair]
                    && (0..alphabet).all(|class| subsumes[step(a, class) * states + step(b, class)])
            },
        );

        Some(Self {
            classes,
            alphabet,
            next,
            accepts,
            live,
            universal,
            subsumes,
        })
    }

    fn step(&self, state: u32, byte: u8) -> u32 {
        self.next[state as usize * self.alphabet + usize::from(self.classes[usize::from(byte)])]
    }

    fn subsumes(&self, a: u32, b: u32) -> bool {
        self.subsumes[a as usize * self.accepts.len() + b as usize]
    }
}

/// Recomputes every entry from the others until none changes. Each rule moves
/// entries one way only, `live` from false to true and the others from true to
/// false, so the rounds end.
fn fixpoint(mut values: Vec<bool>, rule: impl Fn(&[bool], usize) -> bool) -> Vec<bool> {
    loop {
        let next: Vec<bool> = (0..values.len()).map(|at| rule(&values, at)).collect();
        if next == values {
            return values;
        }
        values = next;
    }
}

/// Whether every path `later` selects, one of `earlier` selects too: a search
/// of the states the automata reach on one input for a path `later` selects
/// and none of `earlier` does.
///
/// Two prunings keep the search to what can still decide. A combination in
/// which one earlier glob selects every continuation is covered whatever
/// follows. And a combination that subsumes, glob by glob, one already searched
/// from the same `later` state leaves unselected only continuations that one
/// leaves unselected too, so it finds no path that one misses (the antichain
/// of De Wulf, Doyen, Henzinger and Raskin's inclusion check).
fn includes(earlier: &[&Selection], later: &Selection) -> bool {
    let mut seen_classes = HashSet::new();
    let steps: Vec<u8> = (0..=u8::MAX)
        .filter(|&byte| {
            seen_classes.insert(
                std::iter::once(later)
                    .chain(earlier.iter().copied())
                    .map(|selection| selection.classes[usize::from(byte)])
                    .collect::<Vec<u8>>(),
            )
        })
        .collect();

    let mut searched: HashMap<u32, Vec<Vec<u32>>> = HashMap::new();
    let mut kept = 0;
    let mut pending = VecDeque::from([(0, vec![0; earlier.len()])]);
    while let Some((at, states)) = pending.pop_front() {
        if earlier
            .iter()
            .zip(&states)
            .any(|(selection, &state)| selection.universal[state as usize])
        {
            continue;
        }
        let frontier = searched.entry(at).or_default();
        if frontier.iter().any(|weaker| {
            earlier
                .iter()
                .zip(weaker.iter().zip(&states))
                .all(|(selection, (&weaker, &state))| selection.subsumes(state, weaker))
        }) {
            continue;
        }
        if later.accepts[at as usize]
            && !earlier
                .iter()
                .zip(&states)
                .any(|(selection, &state)| selection.accepts[state as usize])
        {
            return false;
        }
        kept += 1;
        if kept > SEARCH_LIMIT {
            return false;
        }
        frontier.push(states.clone());
        for &byte in &steps {
            let next = later.step(at, byte);
            if later.live[next as usize] {
                pending.push_back((
                    next,
                    earlier
                        .iter()
                        .zip(&states)
                        .map(|(selection, &state)| selection.step(state, byte))
                        .collect(),
                ));
            }
        }
    }
    true
}
