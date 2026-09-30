//! Phase 2 I/O: forms, headers, cookies, responses, websockets, background
//! tasks, and static files.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use siderite_core::background::BackgroundTasks;
use siderite_core::form::{DEFAULT_MULTIPART_LIMIT, Form, Multipart};
use siderite_core::header::{
    Accept, Cookies, Header, NamedHeader, SameSite, SetCookie, UserAgent, WithCookies,
};
use siderite_core::http::{Method, header};
use siderite_core::responses::{FileResponse, Redirect, StreamingResponse, WithHeaders};
use siderite_core::ws::{WebSocketResponse, WebSocketUpgrade};
use siderite_core::*;
use siderite_openapi::{Schema, SchemaObject, SchemaRegistry};
use siderite_testkit::TestClient;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct FormData {
    n: i32,
}

impl Schema for FormData {
    fn schema(r: &mut SchemaRegistry) -> SchemaObject {
        SchemaObject::of_type("object")
            .with("properties", json!({ "n": r.subschema::<i32>() }))
            .with("required", json!(["n"]))
    }
}

struct OnlyFoo;

impl NamedHeader for OnlyFoo {
    const NAME: &'static str = "x-foo";

    fn decode(value: &str) -> Result<Self, String> {
        if value == "foo" {
            Ok(Self)
        } else {
            Err("expected foo".into())
        }
    }
}

async fn form_ok(Form(data): Form<FormData>) -> Json<FormData> {
    Json(data)
}

async fn multipart_ok(mut multipart: Multipart) -> PlainText<String> {
    let mut parts = Vec::new();
    while let Some(field) = multipart.next_field().await.unwrap() {
        let name = field.name().unwrap_or("").to_owned();
        let file_name = field.file_name().map(str::to_owned);
        let content_type = field.content_type().map(str::to_owned);
        let text = field.text().await.unwrap();
        parts.push(format!(
            "{name}:{}:{}:{text}",
            file_name.unwrap_or_default(),
            content_type.unwrap_or_default()
        ));
    }
    PlainText(parts.join("|"))
}

async fn multipart_read(mut multipart: Multipart) -> ApiResult<&'static str> {
    while let Some(field) = multipart.next_field().await? {
        field.bytes().await?;
    }
    Ok("ok")
}

async fn need_ua(Header(UserAgent(ua)): Header<UserAgent>) -> PlainText<String> {
    PlainText(ua)
}

async fn optional_ua(ua: Option<Header<UserAgent>>) -> PlainText<String> {
    PlainText(
        ua.map(|Header(UserAgent(v))| v)
            .unwrap_or_else(|| "none".into()),
    )
}

async fn maybe_foo(h: Option<Header<OnlyFoo>>) -> &'static str {
    if h.is_some() { "foo" } else { "none" }
}

async fn need_foo(_h: Header<OnlyFoo>) -> &'static str {
    "ok"
}

async fn need_accept(Header(Accept(a)): Header<Accept>) -> PlainText<String> {
    PlainText(a)
}

async fn read_cookies(cookies: Cookies) -> PlainText<String> {
    PlainText(cookies.get("session").unwrap_or("missing").to_owned())
}

async fn set_cookie() -> WithCookies<&'static str> {
    WithCookies::new("ok").cookie(
        SetCookie::new("session", "abc")
            .http_only(true)
            .secure(true)
            .same_site(SameSite::Strict)
            .path("/"),
    )
}

async fn go_see_other() -> Redirect {
    Redirect::to("/next")
}

async fn go_temporary() -> Redirect {
    Redirect::temporary("/tmp")
}

async fn go_permanent() -> Redirect {
    Redirect::permanent("/perm")
}

async fn go_invalid() -> Redirect {
    Redirect::to("http://example.com/\n")
}

async fn stream_ok() -> StreamingResponse {
    StreamingResponse::new(futures_util::stream::iter([
        Ok::<_, std::io::Error>(Bytes::from_static(b"hello")),
        Ok(Bytes::from_static(b" ")),
        Ok(Bytes::from_static(b"world")),
    ]))
    .content_type("text/plain")
}

async fn with_extra_headers() -> WithHeaders<&'static str> {
    WithHeaders::new("ok").header("x-foo", "bar")
}

async fn with_bad_header() -> WithHeaders<&'static str> {
    WithHeaders::new("ok").header("not a header", "x")
}

async fn ws_handler(ws: WebSocketUpgrade) -> WebSocketResponse {
    ws.on_upgrade(|_socket| async {})
}

struct Flag(Arc<Notify>);

async fn spawn_task(State(flag): State<Flag>, mut tasks: BackgroundTasks) -> &'static str {
    let notify = Arc::clone(&flag.0);
    tasks.add(async move {
        notify.notify_one();
    });
    "ok"
}

async fn spawn_fn_and_panic(State(flag): State<Flag>, mut tasks: BackgroundTasks) -> &'static str {
    let notify = Arc::clone(&flag.0);
    tasks.add(async {
        panic!("background boom");
    });
    tasks.add_fn(move || async move {
        notify.notify_one();
    });
    "ok"
}

fn io_app() -> App {
    App::new()
        .route("/form", post(form_ok))
        .route("/multipart", post(multipart_ok))
        .route("/multipart-limit", post(multipart_read))
        .route("/ua", get(need_ua))
        .route("/ua-optional", get(optional_ua))
        .route("/foo", get(need_foo))
        .route("/foo-optional", get(maybe_foo))
        .route("/accept", get(need_accept))
        .route("/cookies", get(read_cookies))
        .route("/set-cookie", get(set_cookie))
        .route("/redirect", get(go_see_other))
        .route("/redirect-tmp", get(go_temporary))
        .route("/redirect-perm", get(go_permanent))
        .route("/redirect-bad", get(go_invalid))
        .route("/stream", get(stream_ok))
        .route("/headers", get(with_extra_headers))
        .route("/headers-bad", get(with_bad_header))
        .route("/ws", get(ws_handler))
        .route("/raw", post(raw_len))
}

/// A custom extractor reading the raw body, as user code would.
struct Raw(Vec<u8>);

impl FromRequest for Raw {
    async fn from_request(req: Request) -> Result<Self, ApiError> {
        Ok(Self(req.into_body().into_bytes().await?))
    }
}

async fn raw_len(Raw(bytes): Raw) -> String {
    bytes.len().to_string()
}

fn client() -> TestClient {
    TestClient::new(io_app())
}

fn problem(res: &siderite_testkit::TestResponse) -> Value {
    assert_eq!(res.content_type(), Some("application/problem+json"));
    res.json().unwrap()
}

#[tokio::test]
async fn form_success() {
    let res = client()
        .post_raw(
            "/form",
            "application/x-www-form-urlencoded",
            b"n=7".to_vec(),
        )
        .await
        .unwrap();
    assert_eq!(res.status, 200);
    assert_eq!(res.json::<FormData>().unwrap(), FormData { n: 7 });
}

#[tokio::test]
async fn form_invalid_is_422() {
    let res = client()
        .post_raw(
            "/form",
            "application/x-www-form-urlencoded",
            b"n=abc".to_vec(),
        )
        .await
        .unwrap();
    assert_eq!(res.status, 422);
    let body = problem(&res);
    assert_eq!(body["errors"][0]["location"], json!(["body", "n"]));
    assert_eq!(body["errors"][0]["code"], "int_parsing");
}

#[tokio::test]
async fn form_wrong_content_type_is_415() {
    let res = client()
        .post_raw("/form", "application/json", b"{\"n\":1}".to_vec())
        .await
        .unwrap();
    assert_eq!(res.status, 415);
    assert_eq!(res.content_type(), Some("application/problem+json"));
}

#[tokio::test]
async fn multipart_parses_fields() {
    let body = b"--xyz\r\nContent-Disposition: form-data; name=\"title\"\r\n\r\nhello\r\n--xyz\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.txt\"\r\nContent-Type: text/plain\r\n\r\nfile-contents\r\n--xyz--\r\n";
    let res = client()
        .post_raw(
            "/multipart",
            "multipart/form-data; boundary=xyz",
            body.to_vec(),
        )
        .await
        .unwrap();
    assert_eq!(res.status, 200);
    assert_eq!(
        res.text(),
        "title:::hello|file:a.txt:text/plain:file-contents"
    );
}

#[tokio::test]
async fn multipart_wrong_content_type_is_415() {
    let res = client()
        .post_raw("/multipart", "application/json", b"{}".to_vec())
        .await
        .unwrap();
    assert_eq!(res.status, 415);
    assert_eq!(res.content_type(), Some("application/problem+json"));
}

#[tokio::test]
async fn multipart_oversize_is_413() {
    let mut body = String::from("--xyz\r\nContent-Disposition: form-data; name=\"blob\"\r\n\r\n");
    body.push_str(&"x".repeat(DEFAULT_MULTIPART_LIMIT + 1));
    body.push_str("\r\n--xyz--\r\n");
    let res = client()
        .post_raw(
            "/multipart-limit",
            "multipart/form-data; boundary=xyz",
            body.into_bytes(),
        )
        .await
        .unwrap();
    assert_eq!(res.status, 413);
    assert_eq!(res.content_type(), Some("application/problem+json"));
}

#[tokio::test]
async fn multipart_above_axum_default_under_our_limit_succeeds() {
    let size = 2 * 1024 * 1024 + 64 * 1024;
    let mut body = String::from("--xyz\r\nContent-Disposition: form-data; name=\"blob\"\r\n\r\n");
    body.push_str(&"y".repeat(size));
    body.push_str("\r\n--xyz--\r\n");
    let res = client()
        .post_raw(
            "/multipart-limit",
            "multipart/form-data; boundary=xyz",
            body.into_bytes(),
        )
        .await
        .unwrap();
    assert_eq!(res.status, 200);
    assert_eq!(res.text(), "ok");
}

#[tokio::test]
async fn header_success() {
    let req = ::http::Request::builder()
        .method(Method::GET)
        .uri("/ua")
        .header(header::USER_AGENT, "siderite-test")
        .body(Body::empty())
        .unwrap();
    let res = client().send(req).await.unwrap();
    assert_eq!(res.status, 200);
    assert_eq!(res.text(), "siderite-test");
}

#[tokio::test]
async fn header_missing_is_422() {
    let res = client().get("/ua").await.unwrap();
    assert_eq!(res.status, 422);
    let body = problem(&res);
    assert_eq!(
        body["errors"][0]["location"],
        json!(["header", "User-Agent"])
    );
    assert_eq!(body["errors"][0]["code"], "missing");
}

#[tokio::test]
async fn header_invalid_is_422() {
    let req = ::http::Request::builder()
        .method(Method::GET)
        .uri("/foo")
        .header("x-foo", "bar")
        .body(Body::empty())
        .unwrap();
    let res = client().send(req).await.unwrap();
    assert_eq!(res.status, 422);
    let body = problem(&res);
    assert_eq!(body["errors"][0]["location"], json!(["header", "x-foo"]));
    assert_eq!(body["errors"][0]["code"], "header_invalid");
}

#[tokio::test]
async fn optional_header_is_none_when_missing() {
    let res = client().get("/ua-optional").await.unwrap();
    assert_eq!(res.status, 200);
    assert_eq!(res.text(), "none");
}

#[tokio::test]
async fn accept_header_is_extracted() {
    let req = ::http::Request::builder()
        .method(Method::GET)
        .uri("/accept")
        .header(header::ACCEPT, "application/json")
        .body(Body::empty())
        .unwrap();
    let res = client().send(req).await.unwrap();
    assert_eq!(res.status, 200);
    assert_eq!(res.text(), "application/json");
}

#[tokio::test]
async fn cookies_are_parsed() {
    let req = ::http::Request::builder()
        .method(Method::GET)
        .uri("/cookies")
        .header(header::COOKIE, "theme=dark; session=tok123")
        .body(Body::empty())
        .unwrap();
    let res = client().send(req).await.unwrap();
    assert_eq!(res.status, 200);
    assert_eq!(res.text(), "tok123");
}

#[tokio::test]
async fn set_cookie_headers_are_emitted() {
    let res = client().get("/set-cookie").await.unwrap();
    assert_eq!(res.status, 200);
    let cookie = res
        .headers
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(cookie.contains("session=abc"));
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("Secure"));
    assert!(cookie.contains("SameSite=Strict"));
}

#[tokio::test]
async fn redirect_sets_location() {
    let res = client().get("/redirect").await.unwrap();
    assert_eq!(res.status, 303);
    assert_eq!(res.headers.get(header::LOCATION).unwrap(), "/next");

    let tmp = client().get("/redirect-tmp").await.unwrap();
    assert_eq!(tmp.status, 307);
    assert_eq!(tmp.headers.get(header::LOCATION).unwrap(), "/tmp");

    let perm = client().get("/redirect-perm").await.unwrap();
    assert_eq!(perm.status, 308);
    assert_eq!(perm.headers.get(header::LOCATION).unwrap(), "/perm");
}

#[tokio::test]
async fn invalid_redirect_is_500_problem() {
    let res = client().get("/redirect-bad").await.unwrap();
    assert_eq!(res.status, 500);
    assert_eq!(res.content_type(), Some("application/problem+json"));
}

#[tokio::test]
async fn streaming_body_concatenates() {
    let res = client().get("/stream").await.unwrap();
    assert_eq!(res.status, 200);
    assert_eq!(res.content_type(), Some("text/plain"));
    assert_eq!(res.text(), "hello world");
}

#[tokio::test]
async fn with_headers_appends() {
    let res = client().get("/headers").await.unwrap();
    assert_eq!(res.status, 200);
    assert_eq!(res.headers.get("x-foo").unwrap(), "bar");
}

#[tokio::test]
async fn with_headers_invalid_is_500() {
    let res = client().get("/headers-bad").await.unwrap();
    assert_eq!(res.status, 500);
    assert_eq!(res.content_type(), Some("application/problem+json"));
}

#[tokio::test]
async fn file_response_content_type_and_404() {
    let dir = temp_dir();
    std::fs::write(dir.join("note.json"), "{\"ok\":true}").unwrap();
    std::fs::write(dir.join("note.txt"), "plain").unwrap();

    #[derive(Clone)]
    struct Paths {
        json: PathBuf,
        txt: PathBuf,
        missing: PathBuf,
    }

    async fn json_file(State(p): State<Paths>) -> ApiResult<FileResponse> {
        FileResponse::open(&p.json).await
    }
    async fn txt_file(State(p): State<Paths>) -> ApiResult<FileResponse> {
        Ok(FileResponse::open(&p.txt).await?.attachment("download.txt"))
    }
    async fn missing_file(State(p): State<Paths>) -> ApiResult<FileResponse> {
        FileResponse::open(&p.missing).await
    }

    let app = App::new()
        .route("/json", get(json_file))
        .route("/txt", get(txt_file))
        .route("/missing", get(missing_file))
        .with_state(Paths {
            json: dir.join("note.json"),
            txt: dir.join("note.txt"),
            missing: dir.join("nope.bin"),
        });
    let client = TestClient::new(app);

    let json = client.get("/json").await.unwrap();
    assert_eq!(json.status, 200);
    assert_eq!(json.content_type(), Some("application/json"));
    assert_eq!(json.text(), "{\"ok\":true}");

    let txt = client.get("/txt").await.unwrap();
    assert_eq!(txt.status, 200);
    assert_eq!(txt.content_type(), Some("text/plain; charset=utf-8"));
    assert_eq!(txt.text(), "plain");
    let disp = txt.headers.get(header::CONTENT_DISPOSITION).unwrap();
    assert_eq!(disp, "attachment; filename=\"download.txt\"");

    let missing = client.get("/missing").await.unwrap();
    assert_eq!(missing.status, 404);
    assert_eq!(missing.content_type(), Some("application/problem+json"));
}

#[tokio::test]
async fn static_files_serve_and_block_traversal() {
    let root = temp_dir();
    let public = root.join("public");
    std::fs::create_dir_all(&public).unwrap();
    std::fs::write(public.join("hello.txt"), "hello").unwrap();
    std::fs::write(root.join("secret.txt"), "SECRET").unwrap();

    let app = App::new().static_files("/static", public);
    let client = TestClient::new(app);

    let ok = client.get("/static/hello.txt").await.unwrap();
    assert_eq!(ok.status, 200);
    assert_eq!(ok.text(), "hello");

    for path in [
        "/static/../secret.txt",
        "/static/foo/../../secret.txt",
        "/static/%2e%2e/secret.txt",
    ] {
        let res = client.get(path).await.unwrap();
        assert_ne!(res.status, 200, "escaped via {path}: {}", res.text());
        assert!(
            !res.text().contains("SECRET"),
            "leaked secret via {path}: {}",
            res.text()
        );
    }
}

#[tokio::test]
async fn background_task_runs_after_handler() {
    let notify = Arc::new(Notify::new());
    let app = App::new()
        .route("/bg", get(spawn_task))
        .with_state(Flag(Arc::clone(&notify)));
    let client = TestClient::new(app);
    let res = client.get("/bg").await.unwrap();
    assert_eq!(res.status, 200);
    tokio::time::timeout(Duration::from_secs(1), notify.notified())
        .await
        .expect("background task did not run");
}

#[tokio::test]
async fn background_panic_does_not_stop_later_tasks() {
    let notify = Arc::new(Notify::new());
    let app = App::new()
        .route("/bg", get(spawn_fn_and_panic))
        .with_state(Flag(Arc::clone(&notify)));
    let client = TestClient::new(app);
    let res = client.get("/bg").await.unwrap();
    assert_eq!(res.status, 200);
    tokio::time::timeout(Duration::from_secs(1), notify.notified())
        .await
        .expect("later background task did not run after panic");
}

#[tokio::test]
async fn websocket_non_upgrade_is_problem() {
    let res = client().get("/ws").await.unwrap();
    assert!(
        res.status == 400 || res.status == 426,
        "status {}",
        res.status
    );
    assert_eq!(res.content_type(), Some("application/problem+json"));

    let req = ::http::Request::builder()
        .method(Method::GET)
        .uri("/ws")
        .header(header::CONNECTION, "Upgrade")
        .header(header::UPGRADE, "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
        .body(Body::empty())
        .unwrap();
    let res = client().send(req).await.unwrap();
    assert!(
        res.status == 400 || res.status == 426,
        "status {}",
        res.status
    );
    assert_eq!(res.content_type(), Some("application/problem+json"));
}

#[test]
fn openapi_documents_form_multipart_and_headers() {
    let doc = io_app().openapi().unwrap().to_value();
    let form = &doc["paths"]["/form"]["post"];
    assert!(
        form["requestBody"]["content"]
            .get("application/x-www-form-urlencoded")
            .is_some()
    );

    let multipart = &doc["paths"]["/multipart"]["post"];
    assert_eq!(
        multipart["requestBody"]["content"]["multipart/form-data"]["schema"]["type"],
        "object"
    );

    let ua = &doc["paths"]["/ua"]["get"];
    let params = ua["parameters"].as_array().unwrap();
    assert!(
        params
            .iter()
            .any(|p| p["name"] == "User-Agent" && p["in"] == "header" && p["required"] == true),
        "{params:?}"
    );

    let ws = &doc["paths"]["/ws"]["get"];
    assert_eq!(ws["responses"]["101"]["description"], "Switching Protocols");
}

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("siderite-io-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

mod phase2_fixes {
    use super::*;

    async fn ua(
        h: Option<siderite_core::header::Header<siderite_core::header::UserAgent>>,
    ) -> &'static str {
        if h.is_some() { "yes" } else { "no" }
    }

    #[test]
    fn optional_header_is_documented_as_optional() {
        let doc = App::new()
            .route("/ua", get(ua))
            .openapi()
            .unwrap()
            .to_value();
        let p = &doc["paths"]["/ua"]["get"]["parameters"][0];
        assert_eq!(p["in"], "header");
        assert_eq!(p["required"], false);
    }

    #[tokio::test]
    async fn root_static_files_serve_files_and_keep_problem_404() {
        let dir = std::env::temp_dir().join(format!("siderite-root-static-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("hello.txt"), "hi").unwrap();
        let client = TestClient::new(
            App::new()
                .route("/api", get(|| async { "api" }))
                .static_files("/", &dir),
        );
        assert_eq!(client.get("/hello.txt").await.unwrap().text(), "hi");
        assert_eq!(client.get("/api").await.unwrap().text(), "api");
        let missing = client.get("/nope.txt").await.unwrap();
        assert_eq!(missing.status, 404);
        assert_eq!(missing.content_type(), Some("application/problem+json"));
    }
}

// Opt-in impls; `#[derive(Validate)]` generates these in application code.
/// Hand-written equivalent of `#[derive(Validate)]` for `FormData`.
impl siderite_validation::Validate for FormData {
    fn prepare(input: &mut Value, ctx: &mut siderite_validation::ValidationContext) {
        use siderite_validation::model::{FieldSpec, prepare_object};
        const FIELDS: &[FieldSpec] = &[FieldSpec {
            key: "n",
            aliases: &[],
            required: true,
        }];
        prepare_object(
            input,
            ctx,
            siderite_validation::ModelConfig::DEFAULT,
            FIELDS,
            |_, slot, ctx| {
                <i32 as siderite_validation::Validate>::prepare(slot, ctx);
            },
        );
    }
}
impl siderite_validation::Dump for FormData {}

#[tokio::test]
async fn raw_body_over_default_limit_is_413_problem() {
    let ok = client()
        .post_raw(
            "/raw",
            "application/octet-stream",
            vec![b'x'; DEFAULT_BODY_LIMIT],
        )
        .await
        .unwrap();
    assert_eq!(ok.status, 200);
    assert_eq!(ok.text(), DEFAULT_BODY_LIMIT.to_string());

    let res = client()
        .post_raw(
            "/raw",
            "application/octet-stream",
            vec![b'x'; 3 * 1024 * 1024],
        )
        .await
        .unwrap();
    assert_eq!(res.status, 413);
    assert_eq!(problem(&res)["status"], 413);
}

#[tokio::test]
async fn optional_header_rejects_invalid_values() {
    let missing = client().get("/foo-optional").await.unwrap();
    assert_eq!(missing.text(), "none");
    let bad = ::http::Request::builder()
        .uri("/foo-optional")
        .header("x-foo", "bar")
        .body(Body::empty())
        .unwrap();
    let res = client().send(bad).await.unwrap();
    assert_eq!(res.status, 422);
    assert_eq!(problem(&res)["errors"][0]["code"], "header_invalid");
}
