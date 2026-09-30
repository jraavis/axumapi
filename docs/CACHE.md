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

`RouteCache` caches whole `GET` and `HEAD` responses. Caching is
**opt-in**: a route is cached only when its response or its placement says
so.

**Per response.** Put the layer on the app. It stores only responses marked
`Cache-Control: public`; `Cached::public(ttl, response)` sets
`public, max-age=<ttl>`:

```rust
use siderite::cache::{MemoryCache, RouteCache};
use siderite::Cached;
use std::time::Duration;

#[get("/items")]
async fn list_items() -> Cached<Json<Vec<Item>>> {
    Cached::public(Duration::from_secs(60), Json(load_items().await))
}

let app = App::new()
    .routes(routes![list_items, me])   // `me` is never stored
    .layer(RouteCache::new(MemoryCache::new(1024)).max_body_bytes(256 * 1024));
```

**Per route.** Put the layer on one route with `MethodRouter::layer` and set
`default_ttl`; that route is cached without a `public` marker:

```rust
let app = App::new().route(
    "/stats",
    get(stats).layer(RouteCache::new(cache.clone()).default_ttl(Duration::from_secs(30))),
);
```

Do not set `default_ttl` on an app-wide layer unless every `GET` route in
the app is safe to share between users.

- **Key:** method, scheme, `Host`, path and query, and the request’s `Accept`
  and `Accept-Encoding` values. Each part is length-prefixed, so virtual
  hosts sharing one cache never see each other's responses and different
  header values cannot collide.
- **Stored only when** the status is `200`, there is no `Set-Cookie`, and
  `Cache-Control` has none of `no-store`, `no-cache` or `private`. `Vary`
  may name only `Accept` or `Accept-Encoding`. The body length must be known
  and at most `max_body_bytes` (1 MiB by default). The response must also be
  `public`, unless `default_ttl` is set.
- **Lifetime:** `s-maxage`, else `max-age`, else `default_ttl`. A `public`
  response with none of these, or a lifetime of zero, is not stored.
- **Bypass:** requests carrying credentials are neither answered from nor
  stored in the cache: `Authorization`, `Proxy-Authorization`, `Cookie`,
  `X-API-Key`, any header whose name contains `auth`, `token`, `session`,
  `jwt`, `secret`, `api-key`, `apikey` or `access-key`, and any header added
  with `bypass_header`. This is a backstop; the opt-in above is what keeps
  private responses out of the cache.
- **Streaming bodies** (server-sent events, `StreamingResponse`, file
  streams) are never buffered; they pass through untouched.
- **Marking:** every `GET`/`HEAD` response through the layer gets
  `x-cache: hit` or `miss`. A cached `HEAD` keeps the `Content-Length` of the
  original response.
- **Backend errors fail open:** the request is served as a miss.
