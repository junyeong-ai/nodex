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
- Query commands return `{"items": [...], "total": N}` in data — always both fields
- Exit code 0 = success, 1 = validation errors found, 2 = runtime error

## Adding Output

Use `Envelope::success(data)` or `Envelope::with_warnings(data, warnings)`. Never `println!` raw text from commands.
