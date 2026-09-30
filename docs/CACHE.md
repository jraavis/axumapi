# Cache

`siderite::cache` (crate `siderite-cache`) provides a small async cache API,
two backends and a response-caching middleware.

## The `Cache` trait

| Method | Behaviour |
|---|---|
| `get(key)` | `Option<Vec<u8>>`; expired entries count as missing |
| `set(key, value, ttl)` | store bytes; `None` TTL keeps the entry until evicted |
| `delete(key)` | `true` when an entry was removed |
| `increment(key, by)` | Redis `INCRBY` semantics: a missing key starts at `0`, a non-integer value is an error |
| `clear()` | remove every entry (Redis: only this cache's key prefix) |

`CacheExt` is implemented for every `Cache` and adds JSON helpers:

```rust
use siderite::cache::{CacheExt, MemoryCache};
use std::time::Duration;

let cache = MemoryCache::new(1024);
let stats: Stats = cache
    .get_or_set("stats", Some(Duration::from_secs(60)), || async {
        Ok(compute_stats().await)
    })
    .await?;
```

`get_or_set` returns a cached value when it decodes as `T`. Otherwise it runs
the closure and stores the result. An error from the closure is returned and
nothing is stored. Concurrent misses may each run the closure; there is no
stampede lock.

## Backends

- **`MemoryCache::new(capacity)`**: in-process LRU with per-entry TTL, bounded
  to `capacity` entries. The least recently used entry is evicted first.
- **`RedisCache`** (feature `redis`): `RedisCache::connect(url)` or
  `RedisCache::new(store)` on the Phase 5 `RedisStore`. It uses the store's key
  prefix and native TTLs. Live tests run when `REDIS_URL` points at database
  15 (see [TESTING.md](TESTING.md)).

## Route caching

`RouteCache` caches whole `GET` and `HEAD` responses:

```rust
use siderite::cache::{MemoryCache, RouteCache};
use std::time::Duration;

let app = App::new()
    .routes(routes![list_items])
    .layer(
        RouteCache::new(MemoryCache::new(1024), Duration::from_secs(30))
            .bypass_header(http::HeaderName::from_static("x-tenant"))
            .max_body_bytes(256 * 1024),
    );
```

- **Key:** method, scheme, `Host`, path and query, and the request's `Accept`
  and `Accept-Encoding` values. Each part is length-prefixed, so virtual
  hosts sharing one cache never see each other's responses and different
  header values cannot collide.
- **Bypass:** requests carrying credentials skip the cache:
  `Authorization`, `Proxy-Authorization`, `Cookie`, `X-API-Key`, any header
  whose name contains `auth`, `token`, `session`, `jwt`, `secret`,
  `api-key`, `apikey` or `access-key` (so `X-Auth-Token`, `X-Session-Id`
  and `X-Jwt` are covered), and any header added with `bypass_header`.
- **Register every other credential header** with `bypass_header`. Names
  outside the list above, such as `X-Tenant`, `X-Signature` or
  `X-Client-Id`, are not detected. If one of them decides who may see a
  response, an authenticated response would be stored and served to
  anonymous callers.
- **Stored only when** the status is `200`, there is no `Set-Cookie`, and
  `Cache-Control` has neither `no-store` nor `private`. `Vary` may name only
  `Accept` or `Accept-Encoding`. The body length must be known and at most
  `max_body_bytes` (1 MiB by default).
- **Streaming bodies** (server-sent events, `StreamingResponse`, file streams)
  are never buffered; they pass through untouched.
- **Marking:** every `GET`/`HEAD` response gets `x-cache: hit` or `miss`.
  A cached `HEAD` keeps the `Content-Length` of the original response.
- **Backend errors fail open:** the request is served as a miss.
