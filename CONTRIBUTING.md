# Contributing

Before you open a PR, run:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Rules:

- No `unwrap`/`expect` in library code.
- No `unsafe`.
- Document every public item.
- Backend limitations must fail explicitly with a capability error.
