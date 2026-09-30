//! Dependency injection: resolution, caching, scoping, overrides, global
//! dependencies, cycles, teardown and OpenAPI documentation.
#![allow(clippy::unwrap_used)]

use ::http::request::Parts;
use serde_json::Value;
use siderite_core::di::{Dependency, Depends, Provided, ResolveContext};
use siderite_core::http::StatusCode;
use siderite_core::*;
use siderite_openapi::{Operation, Parameter, ParameterLocation, SchemaRegistry};
use siderite_testkit::TestClient;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

fn header(parts: &Parts, name: &str) -> Option<String> {
    parts.headers.get(name)?.to_str().ok().map(str::to_owned)
}

async fn get_with(
    client: &TestClient,
    path: &str,
    headers: &[(&str, &str)],
) -> siderite_testkit::TestResponse {
    let mut req = ::http::Request::builder().uri(path);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    client.send(req.body(Body::empty()).unwrap()).await.unwrap()
}

// ---- nested dependencies + caching -------------------------------------

#[derive(Clone)]
struct Config(String);

impl Dependency for Config {
    async fn resolve(ctx: &mut ResolveContext<'_>) -> Result<Self, ApiError> {
        Ok(Config(
            header(ctx.parts(), "x-tenant").unwrap_or_else(|| "default".into()),
        ))
    }

    fn describe(op: &mut Operation, r: &mut SchemaRegistry) {
        op.add_parameter(Parameter::new(
            "x-tenant",
            ParameterLocation::Header,
            false,
            r.subschema::<String>(),
        ));
    }
}

/// Resolve counts per tenant, so parallel tests do not interfere.
static DB_RESOLVES: Mutex<BTreeMap<String, usize>> = Mutex::new(BTreeMap::new());

fn resolves(tenant: &str) -> usize {
    DB_RESOLVES
        .lock()
        .unwrap()
        .get(tenant)
        .copied()
        .unwrap_or(0)
}

struct Db {
    tenant: String,
}

impl Dependency for Db {
    async fn resolve(ctx: &mut ResolveContext<'_>) -> Result<Self, ApiError> {
        tokio::task::yield_now().await;
        let config = ctx.resolve::<Config>().await?;
        *DB_RESOLVES
            .lock()
            .unwrap()
            .entry(config.0.clone())
            .or_default() += 1;
        Ok(Db {
            tenant: config.0.clone(),
        })
    }

    fn describe(op: &mut Operation, r: &mut SchemaRegistry) {
        <Depends<Config> as FromRequestParts>::describe(op, r);
    }
}

struct Repo {
    db: Arc<Db>,
}

impl Dependency for Repo {
    async fn resolve(ctx: &mut ResolveContext<'_>) -> Result<Self, ApiError> {
        Ok(Repo {
            db: ctx.resolve::<Db>().await?,
        })
    }

    fn describe(op: &mut Operation, r: &mut SchemaRegistry) {
        <Depends<Db> as FromRequestParts>::describe(op, r);
    }
}

async fn who(db: Depends<Db>, repo: Depends<Repo>, cfg: Depends<Config>) -> String {
    assert!(Arc::ptr_eq(&db.0, &repo.db));
    format!("{}/{}/{}", db.tenant, repo.db.tenant, cfg.0.0)
}

#[tokio::test]
async fn nested_and_request_scoped_cached() {
    let client = TestClient::new(App::new().route("/who", get(who)));
    let res = get_with(&client, "/who", &[("x-tenant", "cache")]).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.text(), "cache/cache/cache");
    assert_eq!(resolves("cache"), 1, "one resolve for two consumers");
    get_with(&client, "/who", &[("x-tenant", "cache")]).await;
    assert_eq!(resolves("cache"), 2, "cache is per request");
}

// ---- provide -------------------------------------------------------------

struct Pool(u32);

async fn pool_size(p: Provided<Pool>) -> String {
    p.0.0.to_string()
}

#[tokio::test]
async fn provide_shares_app_scoped_values() {
    let client = TestClient::new(App::new().provide(Pool(7)).route("/p", get(pool_size)));
    assert_eq!(client.get("/p").await.unwrap().text(), "7");
}

#[tokio::test]
async fn missing_provided_value_is_a_500_problem() {
    let client = TestClient::new(App::new().route("/p", get(pool_size)));
    let res = client.get("/p").await.unwrap();
    assert_eq!(res.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(res.content_type(), Some("application/problem+json"));
}

#[tokio::test]
async fn provide_takes_precedence_over_resolve() {
    let app = App::new()
        .provide(Config("provided".into()))
        .route("/who", get(who));
    let res = get_with(&TestClient::new(app), "/who", &[("x-tenant", "ignored")]).await;
    assert_eq!(res.text(), "provided/provided/provided");
}

// ---- overrides -----------------------------------------------------------

#[tokio::test]
async fn overrides_apply_to_nested_resolution() {
    let app = App::new()
        .override_dependency(|head| async move {
            Ok(Config(format!(
                "test-{}",
                head.uri.path().trim_start_matches('/')
            )))
        })
        .route("/who", get(who));
    let res = TestClient::new(app).get("/who").await.unwrap();
    assert_eq!(res.text(), "test-who/test-who/test-who");

    let app = App::new()
        .override_value(Config("fixed".into()))
        .route("/who", get(who));
    assert_eq!(
        TestClient::new(app)
            .get("/who")
            .await
            .unwrap()
            .text()
            .matches("fixed")
            .count(),
        3
    );
}

#[tokio::test]
async fn override_errors_propagate() {
    let app = App::new()
        .override_dependency::<Config, _, _>(|_| async {
            Err(ApiError::new(StatusCode::FORBIDDEN, "nope"))
        })
        .route("/who", get(who));
    assert_eq!(
        TestClient::new(app).get("/who").await.unwrap().status,
        StatusCode::FORBIDDEN
    );
}

// ---- global dependencies -------------------------------------------------

struct Auth;

impl Dependency for Auth {
    async fn resolve(ctx: &mut ResolveContext<'_>) -> Result<Self, ApiError> {
        match header(ctx.parts(), "x-token").as_deref() {
            Some("secret") => Ok(Auth),
            _ => Err(ApiError::new(StatusCode::UNAUTHORIZED, "bad token")),
        }
    }
}

#[tokio::test]
async fn global_dependency_short_circuits_including_mounts() {
    let child = App::new().route("/c", get(|| async { "child" }));
    let app = App::new()
        .route("/a", get(|| async { "a" }))
        .mount("/m", child)
        .dependency::<Auth>();
    let client = TestClient::new(app);
    for path in ["/a", "/m/c"] {
        let res = client.get(path).await.unwrap();
        assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(res.content_type(), Some("application/problem+json"));
        let ok = get_with(&client, path, &[("x-token", "secret")]).await;
        assert_eq!(ok.status, StatusCode::OK, "{path}");
    }
    assert_eq!(
        client.get("/missing").await.unwrap().status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        client.get("/openapi.json").await.unwrap().status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn parent_registrations_reach_mounted_children() {
    let child = App::new().route("/who", get(who));
    let app = App::new()
        .override_value(Config("parent".into()))
        .mount("/m", child);
    let res = TestClient::new(app).get("/m/who").await.unwrap();
    assert_eq!(res.text(), "parent/parent/parent");
}

// ---- cycles ----------------------------------------------------------------

struct CycleA;
struct CycleB;

impl Dependency for CycleA {
    async fn resolve(ctx: &mut ResolveContext<'_>) -> Result<Self, ApiError> {
        ctx.resolve::<CycleB>().await?;
        Ok(CycleA)
    }

    fn describe(op: &mut Operation, r: &mut SchemaRegistry) {
        <Depends<CycleB> as FromRequestParts>::describe(op, r);
    }
}

impl Dependency for CycleB {
    async fn resolve(ctx: &mut ResolveContext<'_>) -> Result<Self, ApiError> {
        ctx.resolve::<CycleA>().await?;
        Ok(CycleB)
    }

    fn describe(op: &mut Operation, r: &mut SchemaRegistry) {
        <Depends<CycleA> as FromRequestParts>::describe(op, r);
    }
}

#[tokio::test]
async fn cycles_yield_500_and_openapi_terminates() {
    let app = App::new().route("/c", get(|_a: Depends<CycleA>| async { "unreachable" }));
    let doc = app.openapi().unwrap();
    assert!(serde_json::to_value(&doc).unwrap()["paths"]["/c"].is_object());
    let res = TestClient::new(app).get("/c").await.unwrap();
    assert_eq!(res.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(res.content_type(), Some("application/problem+json"));
    assert!(
        !res.text().contains("Cycle"),
        "internals stay out of the response"
    );
}

#[test]
fn cycle_error_names_the_chain() {
    let err = siderite_core::di::DependencyError::Cycle {
        chain: vec!["A", "B", "A"],
    };
    assert_eq!(err.to_string(), "dependency cycle detected: A -> B -> A");
}

// ---- teardown ----------------------------------------------------------------

#[derive(Default)]
struct Probe {
    log: Mutex<Vec<&'static str>>,
    done: Notify,
}

impl Probe {
    fn push(&self, entry: &'static str) {
        self.log.lock().unwrap().push(entry);
    }
}

struct First;
struct Second;
struct Exploding;

impl Dependency for First {
    async fn resolve(ctx: &mut ResolveContext<'_>) -> Result<Self, ApiError> {
        let probe = Provided::<Probe>::from_request_parts(ctx.parts()).await?.0;
        ctx.on_teardown(async move {
            probe.push("first");
            probe.done.notify_one();
        });
        Ok(First)
    }
}

impl Dependency for Second {
    async fn resolve(ctx: &mut ResolveContext<'_>) -> Result<Self, ApiError> {
        ctx.resolve::<First>().await?;
        let probe = Provided::<Probe>::from_request_parts(ctx.parts()).await?.0;
        ctx.on_teardown(async move {
            probe.push("second");
            Err::<(), _>("cleanup failed")
        });
        Ok(Second)
    }
}

impl Dependency for Exploding {
    async fn resolve(ctx: &mut ResolveContext<'_>) -> Result<Self, ApiError> {
        ctx.resolve::<Second>().await?;
        ctx.on_teardown(async { std::hint::black_box(None::<()>).unwrap() });
        Ok(Exploding)
    }
}

async fn uses_deps(_d: Depends<Exploding>, probe: Provided<Probe>) -> &'static str {
    probe.push("handler");
    "ok"
}

#[tokio::test]
async fn teardown_runs_after_response_in_lifo_order_and_survives_failures() {
    let probe = Arc::new(Probe::default());
    let shared = Arc::clone(&probe);
    let app = App::new().provide_arc(shared).route("/t", get(uses_deps));
    let client = TestClient::new(app);
    let res = client.get("/t").await.unwrap();
    assert_eq!(res.text(), "ok");
    tokio::time::timeout(Duration::from_secs(5), probe.done.notified())
        .await
        .unwrap();
    assert_eq!(*probe.log.lock().unwrap(), ["handler", "second", "first"]);
}

// ---- OpenAPI ----------------------------------------------------------------

#[tokio::test]
async fn nested_dependency_docs_reach_openapi() {
    let app = App::new().route("/who", get(who));
    let doc = serde_json::to_value(app.openapi().unwrap()).unwrap();
    let params = doc["paths"]["/who"]["get"]["parameters"]
        .as_array()
        .unwrap();
    let tenant: Vec<&Value> = params.iter().filter(|p| p["name"] == "x-tenant").collect();
    assert_eq!(tenant.len(), 1, "documented once despite three consumers");
    assert_eq!(tenant[0]["in"], "header");
}
