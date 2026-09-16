# Engineering Principles

## Evidence-Based Decisions

Every technical choice must be backed by concrete evidence: file paths, error messages, documentation references, or measurement values. Decisions based on "feels right" or "usually" without verifiable evidence are forbidden. Naming follows existing codebase patterns verified by grep, not assumption.

A passing check is evidence only about the axis it varies. A suite that exercises a three-valued parameter at one value is silent about the other two rather than affirmative about them, and a differential that compares two failed runs reports agreement about nothing. Before a green result is taken as evidence for a behaviour, name the single change to the source that would turn it red; where there is none, the measurement has not been made yet. This is `rule_coverage`'s question asked of the suite rather than of the corpus.

A script that compares command output asserts each command succeeded before comparing — exit status and `"ok": true`, since an error envelope is JSON with an `ok` key too — and reads a graph each binary built itself.

## Root-Cause First

Temporary patches, symptomatic fixes, and workarounds are forbidden. Analyze the root cause, then solve it in a way that is long-term flexible, extensible, and maintainable. Backward-compatibility shims, deprecated code retention, and TODO/FIXME/HACK comments are forbidden.

A decision read in more than one place is read through one function — whether a lock is armed (`Config::lock_arms`), where a governed kind starts (`Config::initial_status_for`). A new reader calls the seam that exists; a fix that finds a second derivation routes every reader through one seam in the same change, because two derivations that agree today are one edit away from disagreeing silently.

## Triage Before Fixing

A change here reaches several interacting surfaces — history judged a step at a time across merges and shallow clones; locks, flows, write seams, load-time validation order and the published output contract — so a review always finds a further interaction, and fixing every finding never converges. Classify each finding first:

- **Fix in the change**: what a realistic history or a CI default reproduces as a bypass (a lock, rule or gate that silently does not enforce what it declares), data loss, a crash (panic, out-of-memory, cost that grows with history), or a false positive. `actions/checkout`'s default `fetch-depth: 1` is a CI default, not an edge case.
- **Defer**: wording, a configuration combination no project declares, a feature no one has asked for, and documentation that was already out of date. Report it with its reproduction instead of widening the change.

Documentation a change makes untrue is part of that change, never a deferred finding. A finding that disagrees with a documented contract is a proposal to change the contract, not a defect — unless the contract itself permits one of the fix-now outcomes. Triage decides whether a finding is fixed now; Root-Cause First decides how.

A change that warrants an independent review gets one round, with the reviewer writing the whole report to a file so nothing is lost in relay. Verify the fixes once; run another round only when that verification finds a new fix-now issue.

## Config Over Code

If a behavior could vary between projects, it belongs in `nodex.toml`, not in source code. No hardcoded domain logic.
