//! End-to-end tests on in-memory SQLite, using the generated migrations.
#![allow(clippy::unwrap_used)]

use axumapi::Body;
use axumapi::http::StatusCode;
use axumapi::orm::Model;
use axumapi_testkit::{TestClient, TestDatabase, TestResponse};
use blog_postgres::models::{AuditEntry, Comment, all_models};
use blog_postgres::{MIGRATIONS_DIR, app};
use http::{Method, Request, header};
use serde_json::{Value, json};

struct Fixture {
    client: TestClient,
    db: axumapi::orm::Db,
}

async fn fixture() -> Fixture {
    let test = TestDatabase::sqlite_memory()
        .await
        .unwrap()
        .with_migrations(MIGRATIONS_DIR)
        .await
        .unwrap();
    let db = test.into_db();
    let client = TestClient::builder(app())
        .with_database("default", db.clone())
        .build();
    Fixture { client, db }
}

async fn send(
    client: &TestClient,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> TestResponse {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let body = match body {
        Some(value) => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&value).unwrap())
        }
        None => Body::empty(),
    };
    client.send(req.body(body).unwrap()).await.unwrap()
}

async fn login(client: &TestClient, name: &str, scope: &str) -> TestResponse {
    let form = format!("username={name}&password=secret-pass&scope={scope}");
    let req = Request::builder()
        .method(Method::POST)
        .uri("/token")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(form))
        .unwrap();
    client.send(req).await.unwrap()
}

/// Register `name` and return a token with all scopes.
async fn user_token(client: &TestClient, name: &str) -> String {
    let res = send(
        client,
        Method::POST,
        "/users",
        None,
        Some(json!({"username": name, "password": "secret-pass"})),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED);
    let token = login(client, name, "").await;
    assert_eq!(token.status, StatusCode::OK);
    token.json::<Value>().unwrap()["access_token"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn new_post(client: &TestClient, token: &str, title: &str, published: bool) -> Value {
    let res = send(
        client,
        Method::POST,
        "/posts",
        Some(token),
        Some(json!({"title": title, "body": "text", "published": published, "tags": ["Rust", "web"]})),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    res.json().unwrap()
}

#[tokio::test]
async fn registration_hides_the_password_and_rejects_duplicates() {
    let f = fixture().await;
    let res = send(
        &f.client,
        Method::POST,
        "/users",
        None,
        Some(json!({"username": "ann", "password": "secret-pass"})),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED);
    assert!(!res.text().contains("secret-pass") && !res.text().contains("hash"));
    let dup = send(
        &f.client,
        Method::POST,
        "/users",
        None,
        Some(json!({"username": "ann", "password": "secret-pass"})),
    )
    .await;
    assert_eq!(dup.status, StatusCode::CONFLICT);
    let short = send(
        &f.client,
        Method::POST,
        "/users",
        None,
        Some(json!({"username": "bob", "password": "short"})),
    )
    .await;
    assert_eq!(short.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn token_endpoint_checks_credentials_and_scopes() {
    let f = fixture().await;
    user_token(&f.client, "ann").await;
    let bad_user = login(&f.client, "nobody", "").await;
    assert_eq!(bad_user.status, StatusCode::UNAUTHORIZED);
    let bad_scope = login(&f.client, "ann", "admin").await;
    assert_eq!(bad_scope.status, StatusCode::BAD_REQUEST);
    let narrow = login(&f.client, "ann", "comments:write").await;
    assert_eq!(narrow.json::<Value>().unwrap()["scope"], "comments:write");
    let wrong_pw = Request::builder()
        .method(Method::POST)
        .uri("/token")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from("username=ann&password=nope-nope"))
        .unwrap();
    assert_eq!(
        f.client.send(wrong_pw).await.unwrap().status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn write_routes_need_a_valid_token_with_the_right_scope() {
    let f = fixture().await;
    let body = json!({"title": "t", "body": "b"});
    let anon = send(&f.client, Method::POST, "/posts", None, Some(body.clone())).await;
    assert_eq!(anon.status, StatusCode::UNAUTHORIZED);
    assert!(anon.headers.contains_key(header::WWW_AUTHENTICATE));
    let bogus = send(
        &f.client,
        Method::POST,
        "/posts",
        Some("bogus"),
        Some(body.clone()),
    )
    .await;
    assert_eq!(bogus.status, StatusCode::UNAUTHORIZED);

    user_token(&f.client, "ann").await;
    let narrow = login(&f.client, "ann", "comments:write").await;
    let narrow = narrow.json::<Value>().unwrap()["access_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let forbidden = send(&f.client, Method::POST, "/posts", Some(&narrow), Some(body)).await;
    assert_eq!(forbidden.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn expired_tokens_are_rejected() {
    let f = fixture().await;
    let token = user_token(&f.client, "ann").await;
    f.db.raw_execute(
        "UPDATE access_tokens SET expires_at = '2000-01-01T00:00:00Z'",
        vec![],
    )
    .await
    .unwrap();
    let res = send(&f.client, Method::GET, "/users/me", Some(&token), None).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn post_lifecycle_slugs_tags_visibility_and_ownership() {
    let f = fixture().await;
    let ann = user_token(&f.client, "ann").await;
    let bob = user_token(&f.client, "bob").await;

    let first = new_post(&f.client, &ann, "Hello World", true).await;
    assert_eq!(first["slug"], "hello-world");
    assert_eq!(first["tags"], json!(["rust", "web"]));
    let second = new_post(&f.client, &ann, "Hello World", false).await;
    assert_eq!(second["slug"], "hello-world-2");

    // The draft is hidden from the public and from other users.
    let public = send(&f.client, Method::GET, "/posts/hello-world-2", None, None).await;
    assert_eq!(public.status, StatusCode::NOT_FOUND);
    let other = send(
        &f.client,
        Method::GET,
        "/posts/hello-world-2",
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(other.status, StatusCode::NOT_FOUND);
    let own = send(
        &f.client,
        Method::GET,
        "/posts/hello-world-2",
        Some(&ann),
        None,
    )
    .await;
    assert_eq!(own.status, StatusCode::OK);

    // Only the author may change it.
    let patch = json!({"published": true, "tags": ["news"]});
    let denied = send(
        &f.client,
        Method::PATCH,
        "/posts/hello-world",
        Some(&bob),
        Some(patch.clone()),
    )
    .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let patched = send(
        &f.client,
        Method::PATCH,
        "/posts/hello-world-2",
        Some(&ann),
        Some(patch),
    )
    .await;
    assert_eq!(patched.status, StatusCode::OK);
    let patched: Value = patched.json().unwrap();
    assert_eq!(patched["tags"], json!(["news"]));
    assert_eq!(patched["published"], true);

    let denied = send(
        &f.client,
        Method::DELETE,
        "/posts/hello-world",
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let deleted = send(
        &f.client,
        Method::DELETE,
        "/posts/hello-world",
        Some(&ann),
        None,
    )
    .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    let gone = send(&f.client, Method::GET, "/posts/hello-world", None, None).await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn listing_paginates_and_filters_by_tag() {
    let f = fixture().await;
    let ann = user_token(&f.client, "ann").await;
    for n in 1..=5 {
        new_post(&f.client, &ann, &format!("Post {n}"), true).await;
    }
    new_post(&f.client, &ann, "Draft", false).await;
    let res = send(
        &f.client,
        Method::GET,
        "/posts?page=2&per_page=2",
        None,
        None,
    )
    .await;
    let page: Value = res.json().unwrap();
    assert_eq!(page["total"], 5);
    assert_eq!(page["total_pages"], 3);
    assert_eq!(page["page"], 2);
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    assert_eq!(page["items"][0]["title"], "Post 3");

    let tagged = send(&f.client, Method::GET, "/posts?tag=rust", None, None).await;
    assert_eq!(tagged.json::<Value>().unwrap()["total"], 5);
    let none = send(&f.client, Method::GET, "/posts?tag=missing", None, None).await;
    assert_eq!(none.json::<Value>().unwrap()["total"], 0);

    let bad = send(&f.client, Method::GET, "/posts?per_page=1000", None, None).await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);

    let tags = send(&f.client, Method::GET, "/tags", None, None).await;
    let names: Vec<String> = tags.json::<Value>().unwrap()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["rust", "web"]);
}

#[tokio::test]
async fn comments_are_scoped_and_cascade_with_the_post() {
    let f = fixture().await;
    let ann = user_token(&f.client, "ann").await;
    new_post(&f.client, &ann, "Talk", true).await;
    let anon = send(
        &f.client,
        Method::POST,
        "/posts/talk/comments",
        None,
        Some(json!({"body": "hi"})),
    )
    .await;
    assert_eq!(anon.status, StatusCode::UNAUTHORIZED);
    let ok = send(
        &f.client,
        Method::POST,
        "/posts/talk/comments",
        Some(&ann),
        Some(json!({"body": "hi"})),
    )
    .await;
    assert_eq!(ok.status, StatusCode::CREATED);
    let list = send(&f.client, Method::GET, "/posts/talk/comments", None, None).await;
    assert_eq!(list.json::<Value>().unwrap()["total"], 1);
    let empty = send(
        &f.client,
        Method::POST,
        "/posts/talk/comments",
        Some(&ann),
        Some(json!({"body": ""})),
    )
    .await;
    assert_eq!(empty.status, StatusCode::UNPROCESSABLE_ENTITY);

    send(&f.client, Method::DELETE, "/posts/talk", Some(&ann), None).await;
    assert_eq!(Comment::objects(&f.db).count().await.unwrap(), 0);
}

#[tokio::test]
async fn post_changes_are_audited_by_signal_receivers() {
    let f = fixture().await;
    let ann = user_token(&f.client, "ann").await;
    new_post(&f.client, &ann, "Audit me", true).await;
    send(
        &f.client,
        Method::PATCH,
        "/posts/audit-me",
        Some(&ann),
        Some(json!({"title": "New"})),
    )
    .await;
    send(
        &f.client,
        Method::DELETE,
        "/posts/audit-me",
        Some(&ann),
        None,
    )
    .await;
    let actions: Vec<String> = AuditEntry::objects(&f.db)
        .all()
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.action)
        .collect();
    assert_eq!(actions, ["post.created", "post.updated", "post.deleted"]);
}

#[tokio::test]
async fn openapi_documents_the_oauth2_scheme_and_tags() {
    let f = fixture().await;
    let doc = f.client.get("/openapi.json").await.unwrap();
    let doc: Value = doc.json().unwrap();
    let scheme = &doc["components"]["securitySchemes"]["OAuth2PasswordBearer"];
    assert_eq!(scheme["flows"]["password"]["tokenUrl"], "/token");
    let create = &doc["paths"]["/posts"]["post"];
    assert_eq!(create["tags"], json!(["posts"]));
    assert_eq!(
        create["security"][0]["OAuth2PasswordBearer"],
        json!(["posts:write"])
    );
}

#[test]
fn check_finds_no_errors_with_a_database_configured() {
    let settings = axumapi_cli::CliSettings::new().database("default", "sqlite::memory:");
    let issues = axumapi_cli::check(
        &app(),
        &all_models(),
        &settings,
        Some(std::path::Path::new(MIGRATIONS_DIR)),
    );
    assert!(
        !issues
            .iter()
            .any(|i| i.level == axumapi_cli::CheckLevel::Error),
        "{issues:?}"
    );
}
