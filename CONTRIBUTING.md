# Contributing

Before you open a PR, run:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Rules:

- No `unwrap`/`expect` in library code.
- No `unsafe`.
- Document every public item.
- Backend limitations must fail explicitly with a capability error.
- Never add `Co-Authored-By` or any AI attribution to commits, PRs, or signatures.

The full contributor guide is on GitHub Pages:
[Development](https://jraavis.github.io/siderite/contributing/development/).

Documentation site sources are in `website/`. From that directory: `bun install && bun run dev`.

