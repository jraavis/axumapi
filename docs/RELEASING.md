# Releasing

## Checklist

1. Run the gates: `cargo fmt --check`,
   `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
   `cargo test --workspace --all-features`,
   `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features`.
2. Run the live-database suites against `docker-compose.yml` (see
   [TESTING.md](TESTING.md)).
3. Check the MSRV: `cargo +1.92.0 check --workspace --all-features --all-targets`.
4. Run `cargo deny check`.
5. Move the `[Unreleased]` section of `CHANGELOG.md` under the new version
   and bump `workspace.package.version` plus the `version` of every internal
   dependency in the root `Cargo.toml`.
6. Tag `vX.Y.Z` and publish in the order below.

## Publish order

Each crate depends only on crates listed before it (normal dependencies):

1. `axumapi-config`
2. `axumapi-macros`
3. `axumapi-validation`
4. `axumapi-openapi`
5. `axumapi-orm`
6. `axumapi-backends`
7. `axumapi-core`
8. `axumapi-migrations`
9. `axumapi-cache`
10. `axumapi-cli`
11. `axumapi-testkit`
12. `axumapi`

`axumapi-bench` and the examples are `publish = false`.

Several crates use `axumapi-testkit` as a dev-dependency while the testkit
depends on them. Cargo does not build dev-dependencies when it verifies a
package, but the first release has not been published yet, so this order has
not been checked against crates.io.
