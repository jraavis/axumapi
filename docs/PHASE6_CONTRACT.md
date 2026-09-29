# Phase 6 contract

This document fixes the public API and file ownership for the Phase 6
workstreams. It is the source of truth for the parallel branches. Deviations
must be reported back to the lead, not silently introduced.

## Global rules

- Rustdoc on every public item. No `unwrap`/`expect` in library code
  (tests may use them). `#![forbid(unsafe_code)]`.
- Never log bind parameters, passwords, tokens, API keys or `Secret` values.
- Do not edit the root `Cargo.toml`, `Cargo.lock` policy aside: if a new
  workspace dependency is truly required, stop and report it.
- Edit only the files your branch owns (table below). New files inside owned
  directories are fine.
- No AI attribution in commits, code or docs. Do not push.
- Branch gates (scoped, to avoid thrashing parallel builds):
  ```bash
  cargo fmt --check
  cargo clippy -p <crate> --all-targets --all-features -- -D warnings
  cargo test -p <crate> --all-features
  cargo check --workspace --all-features
  ```

## Ownership

| Branch | Owns |
|---|---|
| `phase6/config-obs` | `crates/axumapi-config/**`, `crates/axumapi-core/src/middleware/observe.rs`, `crates/axumapi-core/src/middleware.rs` |
| `phase6/cache` | `crates/axumapi-cache/**` |
| `phase6/security` | `crates/axumapi-core/src/security.rs` (may become `security/`), `crates/axumapi-core/src/lib.rs` (re-export lines only), `crates/axumapi-core/tests/security*.rs` |
| `phase6/orm-ext` | `crates/axumapi-orm/**`, `crates/axumapi-macros/src/receiver.rs`, `crates/axumapi/tests/signals*.rs`, `crates/axumapi/tests/routing_db*.rs` |
| `phase6/cli` | `crates/axumapi-cli/**`, `crates/axumapi-migrations/**` |

Already done by the lead in the contract commit: workspace members and
dependencies, module declarations, `SchemaRegistry::add_security_scheme`
(wired into `components.securitySchemes`), `App::database` /
`App::databases` / `App::database_registry` (the registry reaches handlers as
`State<Databases>`), the `#[receiver]` entry point, and facade re-exports
(`axumapi::config`, `axumapi::cache`, `axumapi::receiver`,
`axumapi::security`).

## 1. Configuration (`axumapi-config`)

Built on `figment`. Precedence, lowest to highest: defaults, TOML file,
environment, programmatic overrides.

```rust
pub struct Secret<T>(/* private */);
impl<T> Secret<T> { pub fn new(value: T) -> Self; pub fn expose(&self) -> &T; }
// Debug and Display print `[REDACTED]`; Deserialize is transparent;
// no Serialize impl.

pub struct Settings {
    pub app: AppSettings,            // name: String, debug: bool
    pub server: ServerSettings,      // addr: String (default "127.0.0.1:8000")
    pub databases: BTreeMap<String, DatabaseSettings>,
    pub cache: CacheSettings,        // url: Option<Secret<String>>, max_entries: usize
    pub log: LogSettings,            // level: String ("info"), json: bool
    pub secret_key: Option<Secret<String>>,
}
pub struct DatabaseSettings { pub url: Secret<String>, pub max_connections: Option<u32> }

pub struct ConfigBuilder;
impl ConfigBuilder {
    pub fn new() -> Self;                                   // defaults only
    pub fn file(self, path: impl AsRef<Path>) -> Self;      // required file
    pub fn file_optional(self, path: impl AsRef<Path>) -> Self;
    pub fn env_prefix(self, prefix: &str) -> Self;          // `AXUMAPI_DATABASES__DEFAULT__URL`
    pub fn set(self, key: &str, value: impl Serialize) -> Self;
    pub fn extract<T: DeserializeOwned>(&self) -> Result<T, ConfigError>;
    pub fn build(&self) -> Result<Settings, ConfigError>;
}
/// `axumapi.toml` (optional) + `AXUMAPI_` env; `DATABASE_URL` and `ADDR`
/// map to `databases.default.url` and `server.addr`.
pub fn load() -> Result<Settings, ConfigError>;
/// Install a `tracing-subscriber` using `LogSettings` (`RUST_LOG` wins).
pub fn init_tracing(log: &LogSettings) -> Result<(), ConfigError>;
pub enum ConfigError { /* thiserror; messages never contain secret values */ }
```

## 2. Observability (`axumapi-core` middleware)

Extend the existing `RequestLogging` / `RequestId` middleware in
`middleware/observe.rs` (no parallel middleware). Each request gets an
`http.request` span with `request_id`, `method`, `route` (matched path
template, not the raw URI), `status` and `latency_ms`. Query strings, headers,
and bodies are not recorded.

ORM query spans belong to `phase6/orm-ext`: `Db` read/write/raw calls run
inside an `orm.query` span (`db.system`, `db.operation`, `db.table`) and
record the duration. SQL text is recorded only at `trace` level. Bind
parameters are never recorded.

## 3. Security (`axumapi_core::security`)

Every scheme is a `FromRequestParts` extractor whose `describe` calls
`registry.add_security_scheme(name, json)` and pushes an entry onto
`op.security`. Missing or invalid credentials yield 401 with a
`WWW-Authenticate` header (API key: 401 when missing).

```rust
pub struct HttpBearer { pub token: String }             // scheme "HTTPBearer"
pub struct HttpBasic { pub username: String, pub password: String } // "HTTPBasic"

pub enum ApiKeyLocation { Header, Query, Cookie }
pub trait ApiKeySpec: Send + Sync + 'static {
    const NAME: &'static str;              // header/query/cookie name
    const LOCATION: ApiKeyLocation;
    const SCHEME: &'static str;            // components name
}
pub struct ApiKey<S: ApiKeySpec> { pub key: String, /* PhantomData */ }

pub trait OAuth2Spec: Send + Sync + 'static {
    const TOKEN_URL: &'static str;
    const SCOPES: &'static [(&'static str, &'static str)]; // (scope, description)
    const SCHEME: &'static str;
}
pub struct OAuth2PasswordBearer<S: OAuth2Spec> { pub token: String, /* PhantomData */ }
pub struct OAuth2PasswordRequestForm {                  // FromRequest (urlencoded body)
    pub username: String, pub password: String,
    pub scopes: Vec<String>, pub client_id: Option<String>, pub client_secret: Option<String>,
}

/// Required scopes, as a marker type (const `&str` generics are unstable).
pub trait Scopes: Send + Sync + 'static { const SCOPES: &'static [&'static str]; }
pub struct NoScopes;
#[macro_export] macro_rules! scopes { ($name:ident = [$($s:literal),*]) => { ... } }

/// User-defined principal built from a scheme's credentials.
pub trait Authenticate: Sized + Send + 'static {
    type Credentials: FromRequestParts;
    fn authenticate(credentials: Self::Credentials, required: &[&'static str], parts: &Parts)
        -> impl Future<Output = Result<Self, ApiError>> + Send;
}
/// `Security<CurrentUser, AdminScopes>`: extracts credentials, runs
/// `authenticate` (403 on missing scopes), documents scheme and scopes.
pub struct Security<T: Authenticate, S: Scopes = NoScopes>(pub T, /* PhantomData */);
```

`Authenticate` may need application state: it receives `&Parts`, so state
set with `App::with_state` is reachable through `parts.extensions`.

## 4. CLI (`axumapi-cli`)

App-binary entry point (these commands need the user's `App`):

```rust
pub struct AppCli { /* App factory, models, settings, migrations dir */ }
impl AppCli {
    pub fn new(app: impl Fn() -> App + Send + Sync + 'static) -> Self;
    pub fn models(self, models: &[&'static ModelMeta]) -> Self;
    pub fn settings(self, settings: Settings) -> Self;
    pub fn migrations_dir(self, dir: impl Into<PathBuf>) -> Self;
    /// Parses `std::env::args`: runserver [--addr] (ADDR env, then
    /// settings.server.addr), routes, check, dbshell, makemigrations,
    /// migrate, rollback, showmigrations, squashmigrations.
    pub async fn run(self) -> ExitCode;
}
pub struct CheckIssue { pub level: CheckLevel, pub id: &'static str, pub message: String }
pub fn check(app: &App, models: &[&'static ModelMeta], settings: &Settings, ...) -> Vec<CheckIssue>;
```

`check` validates: configuration, model metadata, the migration graph,
duplicate routes, OpenAPI generation, and backend capability mismatches.
`shell` is replaced by `dbshell`, which starts the backend's native client
(`sqlite3`, `psql`, `mysql`). The standalone `axumapi` binary gains
`postgres` and `mysql` features. A MySQL schema editor is added to
`axumapi-migrations`.

## 5. Signals (`axumapi_orm::signals`)

- The `Signals` registry lives on `Db`: `Db::with_signals(Signals) -> Db`,
  `Db::signals() -> &Signals`. Clones and transaction handles share it.
- Kinds: `PreSave`, `PostSave { created }`, `PreDelete`, `PostDelete`,
  `M2mChanged { action }`.
- Registration is explicit: `signals.connect(receiver)`. `#[receiver(post_save,
  model = User)]` on `async fn f(instance: &User, event: &SignalEvent<'_>) ->
  Result<(), SignalError>` generates `fn f_receiver() -> Receiver`. We do not
  use static registration: `inventory`/`linkme` emit link-section items that
  conflict with `forbid(unsafe_code)` in user crates.
- Semantics:
  - Receivers are async and awaited sequentially, in connection order.
  - They run on the same `Db` handle as the operation, so they run inside the
    caller's transaction when there is one.
  - If a `pre_*` receiver fails, the operation is aborted with
    `OrmError::Signal`.
  - If a `post_*` receiver fails, the error is returned after the statement
    ran. Inside a transaction, the caller's rollback undoes the statement.
  - Use `Db::on_commit` for work that must run only after commit.
  - Bulk `QuerySet` update and delete do not send signals, as in Django.
- Update the `ops.rs` module docs, which currently say there are no signals.

## 6. Database routing (`axumapi_orm::router`)

```rust
pub trait DatabaseRouter: Send + Sync + 'static {
    fn db_for_read(&self, model: &ModelMeta) -> Option<&str> { None }
    fn db_for_write(&self, model: &ModelMeta) -> Option<&str> { None }
    fn allow_migrate(&self, alias: &str, model: &ModelMeta) -> bool { true }
}
impl Databases {
    pub fn with_router(self, router: impl DatabaseRouter) -> Self;
    pub fn for_read<M: Model>(&self) -> Result<&Db, OrmError>;   // router, then "default"
    pub fn for_write<M: Model>(&self) -> Result<&Db, OrmError>;
    pub fn objects<M: Model>(&self) -> Result<QuerySet<M>, OrmError>; // read db
    pub fn using<M: Model>(&self, alias: &str) -> Result<QuerySet<M>, OrmError>;
    pub fn aliases(&self) -> impl Iterator<Item = &str>;
}
```

`QuerySet::using(&Db)` keeps its signature. Alias-based selection goes
through `Databases`, because a `QuerySet` holds one `Db` and not the
registry. Querysets never span databases: combining querysets bound to
different databases (subquery, set operation) is an `OrmError`.

## 7. Cache (`axumapi-cache`)

```rust
#[async_trait]
pub trait Cache: Send + Sync + 'static {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, CacheError>;
    async fn set(&self, key: &str, value: Vec<u8>, ttl: Option<Duration>) -> Result<(), CacheError>;
    async fn delete(&self, key: &str) -> Result<bool, CacheError>;
    async fn increment(&self, key: &str, by: i64) -> Result<i64, CacheError>;
    async fn clear(&self) -> Result<(), CacheError>;
}
pub trait CacheExt: Cache {     // blanket impl
    async fn get_json<T: DeserializeOwned>(..); async fn set_json<T: Serialize>(..);
    async fn get_or_set<T, F, Fut>(&self, key, ttl, f: F) -> Result<T, CacheError>;
}
pub struct MemoryCache;   // MemoryCache::new(capacity): LRU + per-entry TTL
pub struct RedisCache;    // feature "redis", wraps axumapi_backends::redis::RedisStore
pub struct RouteCache;    // middleware: caches 200 GET/HEAD responses by method+URI
                          // for a TTL; bypasses requests with Authorization or Cookie
```
