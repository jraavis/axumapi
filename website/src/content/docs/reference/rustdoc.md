---
title: API rustdoc
description: Hosted rustdoc for every workspace crate.
---

Every public item is documented. CI builds rustdoc with
`RUSTDOCFLAGS='-D warnings'` and publishes it next to this site.

**Crate rustdoc:** [axumapi API](/axumapi/api/axumapi/) (generated on deploy).

Until crates.io publishes the first release, this hosted rustdoc is the
API reference. After publish, docs.rs will carry the same docs per
version.

| Crate | rustdoc |
|---|---|
| `axumapi` | [/api/axumapi/](/axumapi/api/axumapi/) |
| `axumapi-core` | [/api/axumapi_core/](/axumapi/api/axumapi_core/) |
| `axumapi-validation` | [/api/axumapi_validation/](/axumapi/api/axumapi_validation/) |
| `axumapi-orm` | [/api/axumapi_orm/](/axumapi/api/axumapi_orm/) |
| `axumapi-backends` | [/api/axumapi_backends/](/axumapi/api/axumapi_backends/) |
| `axumapi-macros` | [/api/axumapi_macros/](/axumapi/api/axumapi_macros/) |
| `axumapi-openapi` | [/api/axumapi_openapi/](/axumapi/api/axumapi_openapi/) |
| `axumapi-migrations` | [/api/axumapi_migrations/](/axumapi/api/axumapi_migrations/) |
| `axumapi-config` | [/api/axumapi_config/](/axumapi/api/axumapi_config/) |
| `axumapi-cache` | [/api/axumapi_cache/](/axumapi/api/axumapi_cache/) |
| `axumapi-cli` | [/api/axumapi_cli/](/axumapi/api/axumapi_cli/) |
| `axumapi-testkit` | [/api/axumapi_testkit/](/axumapi/api/axumapi_testkit/) |

Build it locally:

```bash
cargo doc --workspace --no-deps --all-features --open
```

On this GitHub Pages site the same output is copied to `/api/` after the
Starlight build. The rustdoc pages are not in the Starlight sidebar; they
use rustdoc’s own navigation.

## See also

- [Crate map](/axumapi/reference/crates/)
- [Prelude](/axumapi/reference/prelude/)
