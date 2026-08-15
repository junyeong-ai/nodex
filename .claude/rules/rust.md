---
paths:
  - "**/*.rs"
---

# Rust Conventions

- `thiserror` for library errors, `anyhow` for CLI — never mix
- No `async` — use `rayon::par_iter()` for parallelism
- `IndexMap` for node storage (insertion order is graph data); `BTreeMap` for anything serialized
- Custom `Serialize`/`Deserialize` only when derived behavior is wrong (e.g., `Graph` skips indices)
- Unit tests live in `#[cfg(test)] mod tests` inside the file they exercise; integration tests live in `nodex-cli/tests/` and drive the compiled binary through its JSON contract
