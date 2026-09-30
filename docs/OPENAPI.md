# OpenAPI 3.1

siderite generates an **OpenAPI 3.1.0** document from your routes. You do not need to write any annotations; the document is built from the types in your handler signatures.

| Endpoint | Default | Serves |
|---|---|---|
| OpenAPI JSON | `/openapi.json` | The generated document |
| Swagger UI | `/docs` | Interactive explorer |
| ReDoc | `/redoc` | Reference docs |

To change these paths, or to turn an endpoint off by setting it to `None`, use `App::docs(DocsConfig { .. })`. The UI pages load their assets from jsDelivr.

## How operations are derived

Each handler is registered through the `Handler` trait, which is implemented for async functions with up to 12 arguments. Registration records a `describe` function built from the handler's signature:

| Signature element | Contributes |
|---|---|
| `Path<T>` | Path parameters. A struct gives one parameter per field, a tuple gives positional parameters, and a scalar gives one parameter named from the template. |
| `Query<T>` | One query parameter per field of `T`. A field is required when the schema lists it as required. |
| `Json<T>` (argument) | `requestBody` of type `application/json` |
| `Json<T>` (return) | `200` response of type `application/json` |
| `ApiError` / `ApiResult<_>` | A `default` response of type `application/problem+json` using the shared `Problem` component |
| `NoContent` | A `204` response |
| `HttpBearer`, `HttpBasic`, `ApiKey<S>`, `OAuth2PasswordBearer<S>`, `Security<T, S>` | A `components.securitySchemes` entry and an operation `security` requirement. Several schemes on one handler form a single requirement object (all required). `Option<Scheme>` adds an anonymous alternative. See [SECURITY.md](SECURITY.md). |
| Custom extractor or response | Whatever its `describe` hook adds. The hook does nothing unless you implement it. |

Metadata set on a route overrides or extends what the signature produced. The available methods are `.summary()`, `.description()`, `.tag()`, `.operation_id()`, `.deprecated()` and `.hidden()`.

* `.status(StatusCode::CREATED)` affects both the response and the document. At runtime it replaces a `200` returned by the handler. In the document it moves the success response to the new status code.
* `.response_model::<T>()` documents the success body as `T`, whatever the handler actually returns. It is the equivalent of FastAPI's `response_model`.

Route macros such as `#[post("/users", status = 201)]` expand to these same builder calls. See [ROUTING.md](ROUTING.md).

## Schemas and components

A type appears in the document through the `Schema` trait in `siderite-validation`, which is also re-exported from the facade:

* If `schema_name()` returns a name, the type is emitted **once**, under `components.schemas`, and every use becomes a `$ref` to it. Recursive types are supported, because a placeholder is registered before the definition is generated.
* Two different Rust types that claim the same name are reported as a `SchemaConflict`, not silently overwritten.
* Unnamed types, such as primitives and generic containers, are written inline. `Option<T>` is written as `anyOf [T, null]`, following JSON Schema 2020-12.

## Mounted applications

`app.mount("/api/v1", child)` **merges** the child's operations into the root document and prepends the prefix to each path. The child's own docs settings are ignored. Only the root app serves `/openapi.json`, `/docs` and `/redoc`.

## Validation of the output

These problems are detected when the app is built. `run()` and `into_router_service()` then return `ServerError::Configuration` instead of panicking:

* Duplicate `(path, method)` pairs
* Duplicate `operationId`s
* Schema-name conflicts
* Paths that do not start with `/`

Tests validate generated documents against the official OpenAPI 3.1 JSON Schema, stored in `crates/siderite-openapi/tests/fixtures`.

## Differences from FastAPI

* **Documentation hooks live on traits.** FastAPI inspects Python type hints at runtime. In siderite, extractors and responses describe themselves through `describe` hooks on siderite's own traits.
* **No reflection.** Schemas come from `Schema` implementations, usually through `#[derive(Schema)]`.
* **Security schemes** are extractors. Each one registers a `securitySchemes` component and a requirement on the operation, so the document matches the handler signature. See [SECURITY.md](SECURITY.md).
