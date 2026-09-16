# Engineering Principles

## Evidence-Based Decisions

Every technical choice must be backed by concrete evidence: file paths, error messages, documentation references, or measurement values. Decisions based on "feels right" or "usually" without verifiable evidence are forbidden. Naming follows existing codebase patterns verified by grep, not assumption.

A passing check is evidence only about the axis it varies. A suite that exercises a three-valued parameter at one value is silent about the other two rather than affirmative about them, and a differential that compares two failed runs reports agreement about nothing. Before a green result is taken as evidence for a behaviour, name the single change to the source that would turn it red; where there is none, the measurement has not been made yet. This is `rule_coverage`'s question asked of the suite rather than of the corpus.

## Root-Cause First

Temporary patches, symptomatic fixes, and workarounds are forbidden. Analyze the root cause, then solve it in a way that is long-term flexible, extensible, and maintainable. Backward-compatibility shims, deprecated code retention, and TODO/FIXME/HACK comments are forbidden.

## Config Over Code

If a behavior could vary between projects, it belongs in `nodex.toml`, not in source code. No hardcoded domain logic.
