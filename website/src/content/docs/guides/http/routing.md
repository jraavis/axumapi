---
title: Routing
description: Function API, route macros, routes![], mounting, and OpenAPI merge.
---

Routes are values. A path plus a `MethodRouter` makes a `Route`. An `App`
collects routes, mounts other apps, and generates the OpenAPI 3.1 document
from those same values, so the docs cannot drift from the code.

## Function API

```rust
use siderite::prelude::*;

let app = App::new()
    .title("Users")
    .version("1.0.0")
    .route(
        "/users",
        post(create_user).status(StatusCode::CREATED).tag("users")
            .get(list_users).summary("List users"),
    );
```

- `get`, `post`, `put`, `patch`, `delete`, `head`, `options` create a
  `MethodRouter` and can be chained. `.on(Method, h)` takes any method.
- Metadata builders `.summary()`, `.description()`, `.tag()`,
  `.operation_id()`, `.deprecated()`, `.hidden()`, `.status(StatusCode)`,
  `.response_model::<T>()` apply to the **most recently added** method, so
  order matters.
- `.layer(layer)` wraps every method added so far on that path in a
  middleware layer (for example `RouteCache`). It runs after routing and
  only for that path; methods chained after it are not wrapped.
- `App::routes(iter)` accepts `Route` values (`Route::new(path, router)`),
  which is what the macros below produce.
- Path templates use `{name}` parameters (`{*rest}` for a catch-all).
  Duplicate `(path, method)` pairs and paths without a leading `/` are
  rejected when the app is built.

## Route macros

`#[get]`, `#[post]`, `#[put]`, `#[patch]`, `#[delete]`, `#[head]`,
`#[options]`, and `#[ws]` (a `GET`) attach a route to an `async fn`.

```rust
/// Create a user.
///
/// Stores the user and returns it.
#[post("/users", status = 201, response_model = UserOut, tag = "users")]
async fn create_user(Json(input): Json<UserIn>) -> Json<UserOut> { /* .. */ }
```

The first argument is the path (a string literal). All other arguments are
optional and may appear in any order; each may appear once (`tag` and `tags`
are separate keys and are combined).

### Metadata

| Argument | Default | Effect |
|---|---|---|
| `operation_id` | function name | OpenAPI `operationId` |
| `summary` | first line of the doc comment | OpenAPI summary |
| `description` | remaining doc-comment lines | OpenAPI description |
| `status` | 200 | Replaces a `200 OK` at runtime and moves the documented success response. Handlers returning another status (`WithStatus`) are left alone. Range: 100..=599. |
| `response_model = T` | handler return type | Documents the success body as `T` (`application/json`). Does not convert the response. |
| `tag` / `tags(...)` | none | OpenAPI tags |
| `deprecated` | off | Marks the operation deprecated |
| `hidden` | off | Serves the route, omits it from OpenAPI |

Explicit arguments replace the doc-derived summary and description.

### Compile-time diagnostics

Reported with spans on the offending token: path not a string literal or not
starting with `/`, unbalanced braces, invalid or duplicate `{param}` names,
unknown or duplicate arguments, status outside 100..=599, non-`async` or
generic handlers, and route attributes on items other than functions.

<details>
<summary>What the macro emits</summary>

The function is left untouched (it stays callable). Next to it, with the same
visibility, the macro emits:

```rust
#[doc(hidden)]
#[allow(non_snake_case, dead_code)]
fn __siderite_route_create_user() -> ::siderite::Route {
    ::siderite::Route::new(
        "/users",
        ::siderite::post(create_user)
            .operation_id("create_user")
            .summary("Create a user.")
            .description("Stores the user and returns it.")
            .tag("users")
            .status(::siderite::http::StatusCode::from_u16(201)
                .unwrap_or(::siderite::http::StatusCode::OK))
            .response_model::<UserOut>(),
    )
}
```

`deprecated` and `hidden` add `.deprecated()` / `.hidden()`. `#[ws]` uses
`::siderite::get`. The `unwrap_or` fallback is unreachable: the status is
validated at compile time.

</details>

## `routes![]`

```rust
App::new().routes(routes![create_user, users::list])
```

expands to a `Vec` of the generated `__siderite_route_*` functions. Only the
last path segment is rewritten (a leading `::` is kept). Generic arguments
are rejected. `routes![]` is an empty `Vec`; a trailing comma is fine.

Because the generated function is separate from the handler, refer to
handlers by a path where they are defined (`users::list`), not through a
`use users::list;` re-import of the handler alone.

## Mounting

`App::mount(prefix, app)` (alias `nest`) serves `app` below `prefix`.

- Child operations are merged into the parent OpenAPI document under the
  prefixed path (`/` maps to the prefix itself).
- Component schemas from all apps share one registry, so a type used in
  several places is emitted once.
- Two different Rust types claiming the same schema name, duplicate
  operation ids, and duplicate `(path, method)` pairs are configuration
  errors when the app is built.
- Startup/shutdown hooks of mounted apps are lifted to the root.
- Title, version, and docs settings come from the root app only.
- State registered with `with_state` applies to the routes of the app it
  was called on.
- Root-app middleware also wraps 404s and the docs endpoints; a mounted
  child’s middleware wraps only its own routes.

## FastAPI differences

- Routes are values. Decorators only generate a sibling function, so
  nothing is registered at import time and handlers stay plain functions.
- Request validation is driven by types (`Json<T>`, `Query<T>`), not by
  default-argument markers.
- Dependency injection is explicit: `Depends<T>`, `Provided<T>`, `State<T>`.
- `status` is per operation and replaces only a `200`.
- Routers are `App`s (`mount`) rather than a separate `APIRouter` type.

## See also

- [Extractors and responses](/siderite/guides/http/extractors/)
- [OpenAPI 3.1](/siderite/guides/http/openapi/)
- [Dependency injection](/siderite/guides/http/di/)
