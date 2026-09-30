---
title: Security
description: HTTP Bearer, Basic, API keys, OAuth2 password flow, Authenticate, and OpenAPI securitySchemes.
---

Security schemes are extractors that document themselves. Reading credentials
and describing them in OpenAPI happen in one place, so the generated
document mirrors the handler signature. Items live in
`siderite_core::security` and are re-exported as `siderite::security`.

The extractors **read** credentials. They never decide whether the
credentials are valid. Implement `Authenticate` and use `Security<T, S>`
for that.

## Schemes

| Extractor | Reads | OpenAPI scheme name | Missing or malformed | `WWW-Authenticate` |
|---|---|---|---|---|
| `HttpBearer` | `Authorization: Bearer <token>` | `HTTPBearer` | 401 | `Bearer` |
| `HttpBasic` | `Authorization: Basic <base64(user:pass)>` | `HTTPBasic` | 401 | `Basic realm="api"` |
| `ApiKey<S>` | header, query parameter, or cookie named by `S` | `S::SCHEME` | 401 | none |
| `OAuth2PasswordBearer<S>` | `Authorization: Bearer <token>` | `S::SCHEME` | 401 | `Bearer` |
| `OAuth2PasswordRequestForm` | urlencoded token-endpoint body | not a scheme | 422 / 415 | none |

- The scheme name in `Authorization` is matched case-insensitively. A
  bearer token that is empty or contains whitespace is rejected.
- `HttpBasic` splits the decoded value at the **first** colon, so a
  password may contain colons. Bad base64, invalid UTF-8, or a missing
  colon is a 401.
- `ApiKey` takes the first non-empty value. A missing or empty key is a
  401 without a challenge header.
- `OAuth2PasswordRequestForm` requires `username` and `password` (422 when
  absent or empty). `scope` is split on whitespace into `scopes`;
  `client_id` and `client_secret` are optional. `grant_type` is accepted
  and not checked. A content type other than
  `application/x-www-form-urlencoded` is a 415.
- `Debug` on every credential type prints `[REDACTED]`.

```rust
use siderite::security::{ApiKey, ApiKeyLocation, ApiKeySpec};

struct PartnerKey;

impl ApiKeySpec for PartnerKey {
    const NAME: &'static str = "X-API-Key";
    const LOCATION: ApiKeyLocation = ApiKeyLocation::Header; // or Query, Cookie
    const SCHEME: &'static str = "PartnerKey";
}

async fn feed(key: ApiKey<PartnerKey>) -> String {
    format!("key of {} bytes", key.key.len())
}
```

OAuth2 password flow pairs a token endpoint with a bearer extractor:

```rust
use siderite::security::{OAuth2PasswordBearer, OAuth2PasswordRequestForm, OAuth2Spec};

struct Oauth;

impl OAuth2Spec for Oauth {
    const TOKEN_URL: &'static str = "/token";
    const SCOPES: &'static [(&'static str, &'static str)] =
        &[("read", "Read items"), ("admin", "Administer")];
    const SCHEME: &'static str = "OAuth2";
}

async fn token(form: OAuth2PasswordRequestForm) -> Result<Json<TokenResponse>, ApiError> {
    // Verify form.username / form.password and issue a token.
    unimplemented!()
}

async fn items(token: OAuth2PasswordBearer<Oauth>) -> String {
    token.token.len().to_string()
}
```

## `Authenticate` and `Security`

```rust
use siderite::security::{Authenticate, HttpBearer, Security, check_scopes};
use siderite::{ApiError, scopes};
use http::request::Parts;

struct CurrentUser(String);

impl Authenticate for CurrentUser {
    type Credentials = HttpBearer;

    async fn authenticate(
        credentials: HttpBearer,
        required: &[&'static str],
        parts: &Parts,
    ) -> Result<Self, ApiError> {
        // Look the token up. State set with App::with_state is in parts.extensions.
        let granted = ["read"];
        check_scopes(required, granted)?;
        Ok(CurrentUser(credentials.token))
    }
}

scopes!(pub ReadScopes = ["read"]);

async fn me(Security(user, _): Security<CurrentUser, ReadScopes>) -> String {
    user.0
}
```

`Security<T, S>` extracts `T::Credentials`, calls
`T::authenticate(credentials, S::SCOPES, parts)`, and documents the scheme
together with the scopes. Scopes default to `NoScopes`, so
`Security<CurrentUser>` requires none. The second tuple field is a type
marker: destructure it as `_`.

| Outcome | Status |
|---|---|
| Credentials missing or malformed | 401 with the scheme’s challenge |
| `authenticate` rejects the credentials | whatever `ApiError` you return, normally 401 |
| `check_scopes` finds scopes missing | 403, `Missing required scopes: a, b.` |

`scopes!` declares a marker type implementing `Scopes` because const `&str`
generics are not stable. `check_scopes(required, granted)` is a plain
function you can call from any `Authenticate` implementation.

## OpenAPI output

Each scheme registers itself under `components.securitySchemes` and pushes
a requirement onto the operation’s `security`.

| Extractor | `securitySchemes` entry |
|---|---|
| `HttpBearer` | `{"type": "http", "scheme": "bearer"}` |
| `HttpBasic` | `{"type": "http", "scheme": "basic"}` |
| `ApiKey<S>` | `{"type": "apiKey", "in": "header" \| "query" \| "cookie", "name": S::NAME}` |
| `OAuth2PasswordBearer<S>` | `{"type": "oauth2", "flows": {"password": {"tokenUrl": .., "scopes": {..}}}}` |

Security requirement objects are alternatives; the schemes inside one
object are all required:

| Handler arguments | `security` |
|---|---|
| `HttpBearer` | `[{"HTTPBearer": []}]` |
| `HttpBearer, ApiKey<K>` | `[{"HTTPBearer": [], "K": []}]` (both required) |
| `Option<HttpBearer>` | `[{}, {"HTTPBearer": []}]` (anonymous or bearer) |
| `ApiKey<K>, Option<HttpBearer>` | `[{"K": []}, {"K": [], "HTTPBearer": []}]` |

`Security<T, S>` re-emits the scheme its credentials add, listing
`S::SCOPES` on that same requirement object. Duplicate scheme names on one
operation collapse to a single entry.

## Constant-time comparison

The extractors return raw values and never compare them. Compare API keys,
tokens, and passwords in constant time (for example with the `subtle`
crate), not with `==`. Store password hashes, not passwords, and compare
hashes with a purpose-built verifier.

If you cache GET responses with `RouteCache`, credential-like headers
(`Authorization`, `Cookie`, `X-API-Key`, and names containing `auth`,
`token`, `session`, `jwt`, `secret`, `api-key` or `access-key`) skip the
cache automatically. Register any other header that decides who may see a
response, such as `X-Tenant` or `X-Signature`, with `bypass_header`. See
[Cache](/siderite/guides/production/cache/).

## See also

- [OpenAPI 3.1](/siderite/guides/http/openapi/)
- [Errors](/siderite/guides/http/errors/)
- [Blog on PostgreSQL](/siderite/tutorials/blog-postgres/) — OAuth2 password flow in an example
