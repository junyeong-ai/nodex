---
paths:
  - "nodex-cli/**/*.rs"
---

# JSON Output Contract

All CLI commands output JSON to stdout. No human-readable text unless `--pretty` is used.

## Envelope

```
Success: {"ok": true, "data": T, "warnings": [...]}
Error:   {"ok": false, "error": {"code": "CODE", "message": "..."}}
```

- `warnings` array is omitted when empty (`skip_serializing_if`)
- Error codes come from the typed `nodex_core::error::Error` variants via `downcast_ref` — never string matching. Two codes are owned by the CLI classifier, not the core enum: `INVALID_ARGUMENT` (clap parse failure) and `INTERNAL_ERROR` (the fallback for an unclassified cause — a bug)
- Listing query leaves return `{"items": [...], "total": N}` in data — always both fields. For plain listings (`nodes`, `search`, `backlinks`, `orphans`, `stale`, `components`), `total` counts every match and a `--limit` cap announces itself via `returned` (omitted otherwise) — their single truncation seam is `ItemsEnvelope::capped` and their core query functions return complete results. Selection-semantics commands (`trust --top/--bottom`, `similar`, `recent` — the `*Options` tier) deliberately select in core: their `total` is the size of the selection itself. Five leaves return object-shaped reports instead: `query node`, `query issues`, `query trust <id>`, `query neighborhood`, `query dependents`. `nodex export envelope-schema` is the authoritative per-command payload shape — consumers validate against it, never against this prose
- A `query` leaf that answers from `graph.json` distinguishes *absent from the project* (`NOT_FOUND`) from *absent from a snapshot that no longer matches the working tree* (`GRAPH_OUTDATED`) — the second is a rebuild, not a corrected id, and a consumer dispatching on the code must be able to tell them apart. `status::Snapshot::require` is the single seam that makes the distinction. `NOT_FOUND` is a claim about the project, so it is only ever made against a snapshot proven to match the working tree content-for-content — a miss escalates to the content probe to establish that, and a typo against a snapshot that passes stays `NOT_FOUND`. Everything short of proof, including a probe that could not run, is `GRAPH_OUTDATED`: the tool never denies a document on evidence that cannot see it
- Exit code 0 = success, 1 = `check` found Error-severity violations, 2 = every error envelope (config, parse, IO, version, CLI-arg, runtime)

## Adding Output

Use `Envelope::success(data)` or `Envelope::with_warnings(data, warnings)`. Never `println!` raw text from commands.
