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
- Unit tests live in `#[cfg(test)] mod tests` inside the file they exercise; integration tests live in `nodex-cli/tests/` and drive the compiled binary through its JSON contract
