---
paths:
  - "**/*.rs"
---

# Rust Conventions

- Edition 2024, minimum rust-version 1.94
- `thiserror` for library errors, `anyhow` for CLI — never mix
- No `unwrap()` on user-supplied data. Use `?` or `expect("reason")` for hardcoded values only
- No `async` — use `rayon::par_iter()` for parallelism
- `BTreeMap` over `HashMap` where deterministic ordering matters (serialization, output)
- `IndexMap` for insertion-order-preserving node storage
- Custom `Serialize`/`Deserialize` only when derived behavior is wrong (e.g., `Graph` skips indices)
- Test with `#[cfg(test)] mod tests` in same file — no separate test files for unit tests
