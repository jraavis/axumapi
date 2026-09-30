---
title: OpenAPI 3.1
description: Generate an OpenAPI 3.1 document from handler signatures, schemas, and security extractors.
---

siderite generates an **OpenAPI 3.1.0** document from your routes. You do not
write a separate spec; the document is built from the types in handler
signatures.

| Endpoint | Default | Serves |
|---|---|---|
| OpenAPI JSON | `/openapi.json` | The generated document |
| Swagger UI | `/docs` | Interactive explorer |
| ReDoc | `/redoc` | Reference docs |

Change these paths, or turn an endpoint off with `None`, via
`App::docs(DocsConfig { .. })`. The UI pages load their assets from jsDelivr.

## How operations are derived

Each handler is registered through the `Handler` trait (async functions with
up to 12 arguments). Registration records a `describe` function built from
the signature:

| Signature element | Contributes |
|---|---|
| `Path<T>` | Path parameters |
| `Query<T>` | One query parameter per field of `T`. Required when the schema lists it as required |
| `Json<T>` (argument) | `requestBody` of type `application/json` |
| `Json<T>` (return) | `200` response of type `application/json` |
| `ApiError` / `ApiResult<_>` | A `default` response of type `application/problem+json` using the shared `Problem` component |
| `NoContent` | A `204` response |
| `HttpBearer`, `HttpBasic`, `ApiKey<S>`, `OAuth2PasswordBearer<S>`, `Security<T, S>` | A `components.securitySchemes` entry and an operation `security` requirement. Several schemes on one handler form a single requirement object (all required). `Option<Scheme>` adds an anonymous alternative |
| Custom extractor or response | Whatever its `describe` hook adds (default: nothing) |

Route metadata overrides or extends what the signature produced:
`.summary()`, `.description()`, `.tag()`, `.operation_id()`,
`.deprecated()`, `.hidden()`.

- `.status(StatusCode::CREATED)` affects both the response and the document:
  at runtime it replaces a `200`; in the document it moves the success
  response to the new status code.
- `.response_model::<T>()` documents the success body as `T`, whatever the
  handler actually returns (FastAPI’s `response_model`).

Route macros such as `#[post("/users", status = 201)]` expand to these same
builder calls. See [Routing](/siderite/guides/http/routing/).

## Schemas and components

A type appears through the `Schema` trait in `siderite-validation` (also
re-exported from the facade):

- If `schema_name()` returns a name, the type is emitted **once** under
  `components.schemas`, and every use becomes a `$ref`. Recursive types
  work because a placeholder is registered before the definition is
  generated.
- Two different Rust types that claim the same name are a
  `SchemaConflict`, not silently overwritten.
- Unnamed types (primitives, generic containers) are written inline.
  `Option<T>` is `anyOf [T, null]`, following JSON Schema 2020-12.

## Mounted applications

`app.mount("/api/v1", child)` **merges** the child’s operations into the
root document and prepends the prefix to each path. The child’s own docs
settings are ignored. Only the root app serves `/openapi.json`, `/docs`,
and `/redoc`.

## Build-time checks

These problems are detected when the app is built. `run()` and
`into_router_service()` then return `ServerError::Configuration`:

- Duplicate `(path, method)` pairs
- Duplicate `operationId`s
- Schema-name conflicts
- Paths that do not start with `/`

Tests validate generated documents against the official OpenAPI 3.1 JSON
Schema in `crates/siderite-openapi/tests/fixtures`.

## FastAPI differences

- Documentation hooks live on traits. FastAPI inspects type hints at
  runtime; siderite extractors and responses describe themselves.
- No reflection. Schemas come from `Schema` implementations, usually
  `#[derive(Schema)]`.
- Security schemes are extractors, so the document matches the handler
  signature. See [Security](/siderite/guides/http/security/).

## See also

- [Extractors and responses](/siderite/guides/http/extractors/)
- [Validation](/siderite/guides/http/validation/)
- [API rustdoc](/siderite/reference/rustdoc/)
