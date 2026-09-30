---
title: Middleware and lifespan
description: Tower layers, built-in middleware, ordering, startup and shutdown hooks.
---

## Layers

`App::layer(layer)` takes any `tower::Layer` over `middleware::BoxService`
(`BoxCloneSyncService<http::Request<Body>, http::Response<Body>, Infallible>`).
`middleware::from_fn(|req, next| async { .. })` builds one from a function.

**The first layer registered is the outermost.** It sees the request first
and the response last. `state`, `provide`, and lifespan resources are
always inside all middleware, regardless of call order. Root-app middleware
also wraps 404s and the docs endpoints; a mounted child’s middleware wraps
only its own routes. Rejections from built-ins are RFC 7807 problems.

## Built-ins

| Type | App method | Behaviour |
|---|---|---|
| `Cors` | `cors` | origins / methods / headers / credentials / max-age; `permissive()`. Credentials plus “any” mirrors the request instead of `*` |
| `Compression` | `compression` | gzip + brotli, toggleable |
| `TrustedHosts` | `trusted_hosts` | exact / `*.sub` / `*`; 400 problem otherwise |
| `HttpsRedirect` | `https_redirect` | 308 to `https://host/path?query`. Honours `X-Forwarded-Proto` only if `trust_forwarded_proto(true)` (off by default) |
| `RequestIdLayer` | `request_id` | Propagates a valid `x-request-id` or generates UUID v4; `RequestId` extractor |
| `RequestLogging` | `request_logging` | `http.request` span with request_id, method, matched route, status, latency_ms |
| `Timeout` | `timeout` | 504 problem by default (`.status(..)` to change); covers response production, not body streaming |
| `ConcurrencyLimit` | `concurrency_limit` | Excess requests wait for a slot |
| `BodyLimit` | `body_limit` | 413 problem via `Content-Length`; streamed bodies are capped while read |
| `RateLimit` | `rate_limit` | Token bucket per client IP, 429 + `Retry-After` |

Logging never records headers (`Authorization`, `Cookie`, API keys), query
strings, or bodies. Register `request_id` before `request_logging`. Request
spans are documented in [Observability](/axumapi/guides/production/observability/).
HTTP GET/HEAD response caching is `RouteCache`; see
[Cache](/axumapi/guides/production/cache/).

`RateLimit` is **process-local**: with N replicas the effective limit is N
times higher, and a restart resets it. The client key is the socket peer
(available under `App::run`). Enable `trust_forwarded_for` only behind a
trusted proxy. Without any address (in-process tests) all requests share a
bucket.

## Lifespan

- `on_startup` / `on_shutdown` hooks: startup in registration order,
  shutdown reversed. A mounted child’s hooks start after the parent’s and
  stop before it. A failing startup hook yields `ServerError::Lifespan` (no
  shutdown hooks run); every shutdown hook runs and the first error is
  returned.
- `lifespan_resource(|| async { Ok((value, teardown_future)) })` creates a
  value at startup, exposes it as `Resource<T>`, and awaits the teardown
  future at shutdown (reverse order). Requests get a 503 problem before
  startup and after shutdown.
- Tests: `TestClient::start(app).await` runs startup;
  `client.shutdown().await` runs shutdown; `TestClient::new` runs no hooks.

## See also

- [Observability](/axumapi/guides/production/observability/)
- [Cache](/axumapi/guides/production/cache/)
- [Testing](/axumapi/guides/production/testing/)
