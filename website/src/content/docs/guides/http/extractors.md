---
title: Extractors and responses
description: Path, Query, Json, Form, headers, cookies, WebSockets, files, and custom describe hooks.
---

A handler is an `async fn` with zero or more arguments returning something
that implements `IntoResponse`. Every argument except the last implements
`FromRequestParts`. The **last** argument may implement `FromRequest` and
consume the request body.

All of these traits are siderite’s own. No axum types leak into the public
API.

## Built-in extractors

| Extractor | Source | Notes |
|---|---|---|
| `Path<T>` | Path parameters | Struct → one parameter per field; tuple → positional; scalar → one parameter named from the template |
| `Query<T>` | Query string | One parameter per field of `T`. Repeated keys fill a `Vec` field. Text input always allows string-to-number coercion |
| `Json<T>` | JSON body | Runs the [validation](/siderite/guides/http/validation/) pipeline |
| `Form<T>` | `application/x-www-form-urlencoded` | Same validation pipeline; text input |
| `Multipart` | `multipart/form-data` | Streaming fields |
| `State<T>` | `App::with_state` | `Arc<T>` |
| `Depends<T>` | DI | See [Dependency injection](/siderite/guides/http/di/) |
| `Provided<T>` | `App::provide` | Application-scoped value that is not a `Dependency` |
| `Header<H>` | Named header | `H: NamedHeader` |
| `UserAgent`, `Accept` | Common headers | |
| `Cookies` | Cookie header | |
| `HttpBearer` and other schemes | Credentials | See [Security](/siderite/guides/http/security/) |
| `WebSocketUpgrade` | WebSocket handshake | Pair with `#[ws]` |
| `RawRequest` | Whole request | Escape hatch |

## Built-in responses

| Type | Status | Body |
|---|---|---|
| `Json<T>` | 200 | JSON through `Dump` (not raw Serde) |
| `JsonDump<T>` | 200 | JSON with `DumpOptions` (`exclude_none`, field sets, …) |
| `PlainText<T>` | 200 | `text/plain` |
| `Html<T>` | 200 | `text/html` |
| `NoContent` | 204 | empty |
| `Redirect` | 3xx | Location |
| `WithStatus<R>` | chosen | wraps another response |
| `WithHeaders<R>` | inner | extra headers |
| `WithCookies<R>` | inner | `Set-Cookie` |
| `StreamingResponse` | 200 | streaming body |
| `FileResponse` | 200 | file / static |

`App::docs` serves Swagger UI and ReDoc; static files are
`App` helpers in `siderite-core` (`static_files`).

## Optional extractors

Wrap an extractor in `Option` to accept requests without that input.
`Option<T>` is `None` only when the input is **absent**; input that is
present but invalid still fails the request with `T`'s error:

| Extractor | `None` when | Still an error |
|---|---|---|
| `Header<H>` | the header is not sent | an undecodable value (`422`) |
| `Query<T>` | there is no query string and `T` rejects the empty input | any query string `T` rejects (`422`) |
| `HttpBearer`, `OAuth2PasswordBearer<S>` | no `Authorization` header, or another scheme | a malformed token (`401`) |
| `HttpBasic` | no `Authorization` header, or another scheme | undecodable credentials (`401`) |
| `ApiKey<S>` | the key is not sent | |
| `Security<T, S>` | its credentials are absent | `T::authenticate` fails (`401`) |

`State<T>`, `Provided<T>` and `Resource<T>` never become `None`: a missing
value is a server misconfiguration and stays an error.

## Custom extractors

Implement `FromRequestParts` (or `FromRequest`) and optionally `describe` so
the extractor documents itself:

```rust
struct ApiKey;

impl FromRequestParts for ApiKey {
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> { /* .. */ }

    fn describe(op: &mut Operation, r: &mut SchemaRegistry) {
        op.add_parameter(Parameter::new(
            "x-api-key", ParameterLocation::Header, true, r.subschema::<String>(),
        ));
    }
}
```

`IntoResponse::describe` works the same way (`op.add_response(..)`). The
default `describe` documents nothing, so an undocumented extractor still
runs.

For `Option<YourExtractor>` to be `None` when the input is missing, mark
that error with `ApiError::absent()`. Unmarked errors propagate through the
`Option`:

```rust
let key = parts.headers.get("x-api-key").ok_or_else(|| {
    ApiError::new(StatusCode::UNAUTHORIZED, "Not authenticated.").absent()
})?;
```

Types used in bodies, queries, and responses derive `Schema`. Nested types
go through `registry.subschema::<T>()`, so named types become
`components.schemas` entries referenced by `$ref`.

## `#[derive(Schema)]`

Generates `Schema` for structs and enums. It honours common serde
attributes (`rename`, `rename_all`, `skip*`, `default`,
`deny_unknown_fields`, `tag` / `content` / `untagged`) plus `#[field(..)]`
constraints (`min_length`, `max_length`, `pattern` / `regex`, `gt`, `ge`,
`lt`, `le`, `multiple_of`, `email`, `url`, `title`, `description`,
`examples(..)`, `alias`). Doc comments become descriptions.

- Non-generic types are named after the type; generic types are inline
  unless `#[schema(name = "..")]`. `#[schema(inline)]` always inlines.
- Constraints added to a `$ref` property wrap it as
  `{"allOf": [{"$ref": ..}], ...}`.
- `flatten`, `transparent`, `from` / `into` / `try_from`, `remote`,
  `with`, `serialize_with` / `deserialize_with`, split
  `rename(serialize = ..)` forms, `other`, variant-level `untagged`, and
  tuple variants of internally tagged enums are compile errors rather than
  described wrongly.

## See also

- [Routing](/siderite/guides/http/routing/)
- [Validation](/siderite/guides/http/validation/)
- [OpenAPI 3.1](/siderite/guides/http/openapi/)
