---
paths:
  - "nodex-cli/**/*.rs"
---

# JSON Output Contract

All CLI commands write one JSON envelope to stdout; `--pretty` only indents it. Only clap's `--help` / `help` / `--version` print text.

## Envelope

```
Success: {"ok": true, "data": T, "warnings": [...]}
Error:   {"ok": false, "error": {"code": "CODE", "message": "..."}}
```

- Snapshot queries verify content by default; `--allow-stale` explicitly selects membership-only reads. Schema compatibility is checked before decoding graph data in either mode; mismatch yields `GRAPH_OUTDATED` with a rebuild remedy. Envelope `snapshot` carries probe scope and coverage: membership does not measure content, and content fidelity does not certify parsing completeness.
- Batch mutation `completion` distinguishes `planned`, `complete` and `partial`; non-empty typed `failures` retain the result envelope and exit 2 through `emit_batch_write`. Policy holds alone exit 0. Scope and retention advisories still matter.
- `warnings` array is omitted when empty (`skip_serializing_if`)
- Warnings with machine-readable evidence use optional typed `WarningDetails`.
  Branch on its discriminant and fields, never message text. Keep the generic
  envelope schema in `export.rs` aligned with the serialized warning shape;
  per-command payload schemas do not validate envelope metadata.
- Error codes come from the typed `nodex_core::error::Error` variants via `downcast_ref` — never string matching. Two codes are owned by the CLI classifier, not the core enum: `INVALID_ARGUMENT` (clap parse failure) and `INTERNAL_ERROR` (the fallback for an unclassified cause — a bug)
- Listing query leaves return `{"items": [...], "total": N}` in data — always both fields. For plain listings (`nodes`, `search`, `backlinks`, `orphans`, `stale`, `components`), `total` counts every match and a `--limit` cap announces itself via `returned` (omitted otherwise) — their single truncation seam is `ItemsEnvelope::capped` and their core query functions return complete results. `chain`, `covered-by` and `annotations` take no `--limit` and never carry `returned`. Selection-semantics commands (`similar`, `recent` — the `*Options` tier) deliberately select in core: their `total` is the size of the selection itself. Four leaves return object-shaped reports instead: `query node`, `query issues`, `query neighborhood`, `query dependents`. `nodex export envelope-schema` is the authoritative per-command payload shape — consumers validate against it, never against this prose
- Node detail uses `NodeMetadata`, excluding internal fingerprints; every line number reported or accepted, `--body-lines` included, is a one-based file line. Detection listings expose `subjects` and `unjudged` alongside the ordinary list fields.
- A `query` leaf that answers from `graph.json` distinguishes *absent from the project* (`NOT_FOUND`) from *absent from a snapshot that no longer matches the working tree* (`GRAPH_OUTDATED`) — the second is a rebuild, not a corrected id, and a consumer dispatching on the code must be able to tell them apart. `status::Snapshot::require` is the single seam that makes the distinction. `NOT_FOUND` is a claim about the project, so it is only ever made against a snapshot proven to match the working tree content-for-content — a miss escalates to the content probe to establish that, and a typo against a snapshot that passes stays `NOT_FOUND`. A snapshot the probe finds drifted yields `GRAPH_OUTDATED`. A probe that could not run yields its own error (e.g. `IO_ERROR`) rather than either: nothing about the working tree was established, and rebuilding cannot restore file access, so neither code's remedy would succeed
- Exit code 0 = success, 1 = `check` found Error-severity violations, 2 = every error envelope (config, parse, IO, version, CLI-arg, runtime), batch result envelopes with actual write failures, and every envelope stdout would not take (a pipe closed early, a full disk) — a verdict that was not delivered never exits as one that was. The encoder (`envelope::print_json`) owns that exit, so no command can outlive a failed write
- A verdict answers for what was judged, never for what is displayed. `check --severity` narrows the listed violations; `has_errors` and the exit code stay drawn from the whole judged set, and a finding the response stops reporting at all — `CheckResult::reported_beside_the_list` is the one reading of that — is disclosed as `gate_suppression`. A presentation knob that moved the verdict would be a gate certifying something other than what it checked — the same defect as a rule reporting green over a population it never ran on

## Adding Output

Snapshot query leaves emit through `QueryContext::emit_read_with` (`commands/query/mod.rs`), which attaches `snapshot` metadata before delegating to the read emitter. Other commands emit through `nodex-cli/src/format.rs`: `emit_read` / `emit_read_with` for a command that reads the project (merges the binary-compat advisory), `emit_write` for one that writes documents (merges the unenforced-baseline advisory). Call `print_json(&Envelope::…)` directly only where there is no loaded working-tree `Config` to hand `emit_read` — `init`, the config-free exports, and `diff` / `impact`, whose working-tree config is best-effort. Never `println!` raw text from commands; clippy's `disallowed-macros` (`clippy.toml`) refuses the print macros workspace-wide.
