# Engineering Principles

## Evidence-Based Decisions

Every technical choice must be backed by concrete evidence: file paths, error messages, documentation references, or measurement values. Decisions based on "feels right" or "usually" without verifiable evidence are forbidden. Naming follows existing codebase patterns verified by grep, not assumption.

## Root-Cause First

Temporary patches, symptomatic fixes, and workarounds are forbidden. Analyze the root cause, then solve it in a way that is long-term flexible, extensible, and maintainable. Backward-compatibility shims, deprecated code retention, and TODO/FIXME/HACK comments are forbidden.

## Config Over Code

If a behavior could vary between projects, it belongs in `nodex.toml`, not in source code. No hardcoded domain logic.
