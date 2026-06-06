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
- Error codes come from `nodex_core::error::Error` variants — never string matching
- Query commands return `{"items": [...], "total": N}` in data — always both fields. For plain listings (`nodes`, `search`, `backlinks`, `orphans`, `stale`, `components`), `total` counts every match and a `--limit` cap announces itself via `returned` (omitted otherwise) — their single truncation seam is `ItemsEnvelope::capped` and their core query functions return complete results. Selection-semantics commands (`trust --top/--bottom`, `similar`, `recent` — the `*Options` tier) deliberately select in core: their `total` is the size of the selection itself
- Exit code 0 = success, 1 = `check` found Error-severity violations, 2 = every error envelope (config, parse, IO, version, CLI-arg, runtime)

## Adding Output

Use `Envelope::success(data)` or `Envelope::with_warnings(data, warnings)`. Never `println!` raw text from commands.
