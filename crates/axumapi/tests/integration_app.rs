//! End-to-end: model + database + routes driven through `TestClient` on an
//! isolated database, DI overrides, security and signals through handlers.
#![allow(clippy::unwrap_used, dead_code)]

use axumapi::http::{StatusCode, header};
use axumapi::orm::signals::{Receiver, SignalKind, SignalName, Signals};
use axumapi::orm::{Databases, Db};
use axumapi::prelude::*;
use axumapi::security::HttpBearer;
use axumapi::{Dependency, ResolveContext};
use axumapi_testkit::http::Request;
use axumapi_testkit::{TestClient, TestDatabase};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Model, Serialize, Deserialize, Validate, Schema)]
#[model(table = "notes", ordering = ["id"])]
struct Note {
    #[field(primary_key, auto)]
    #[serde(default)]
    id: i64,
    #[field(min_length = 1, max_length = 100)]
    title: String,
}

#[derive(Debug, Deserialize, Validate, Schema)]
struct NewNote {
    #[field(min_length = 1, max_length = 100)]
    title: String,
}

/// Data access the handlers depend on: the database, or a canned list.
#[derive(Debug, Clone)]
enum NoteRepo {
    Database(Db),
    Fixed(Vec<String>),
}

impl Dependency for NoteRepo {
    async fn resolve(ctx: &mut ResolveContext<'_>) -> Result<Self, ApiError> {
        let State(databases) = State::<Databases>::from_request_parts(ctx.parts()).await?;
        databases
            .default_db()
            .cloned()
            .map(NoteRepo::Database)
            .ok_or_else(|| ApiError::internal("no default database"))
    }
}

impl NoteRepo {
    async fn titles(&self) -> Result<Vec<String>, ApiError> {
        match self {
            Self::Database(db) => Ok(Note::objects(db)
                .all()
                .await?
                .into_iter()
                .map(|n| n.title)
                .collect()),
            Self::Fixed(titles) => Ok(titles.clone()),
        }
    }
}

/// The authenticated caller; the token `secret` maps to `alice`.
#[derive(Debug, Clone, PartialEq)]
struct Principal(String);

impl Dependency for Principal {
    async fn resolve(ctx: &mut ResolveContext<'_>) -> Result<Self, ApiError> {
        let bearer = HttpBearer::from_request_parts(ctx.parts()).await?;
        if bearer.token == "secret" {
            Ok(Principal("alice".into()))
        } else {
            Err(ApiError::new(StatusCode::UNAUTHORIZED, "bad token"))
        }
    }

    fn describe(
        op: &mut axumapi::openapi::Operation,
        registry: &mut axumapi::openapi::SchemaRegistry,
    ) {
        <HttpBearer as FromRequestParts>::describe(op, registry);
    }
}

#[post("/notes", status = 201)]
async fn create_note(
    repo: Depends<NoteRepo>,
    Json(body): Json<NewNote>,
) -> Result<Json<Note>, ApiError> {
    let NoteRepo::Database(db) = &*repo.0 else {
        return Err(ApiError::internal("read-only repository"));
    };
    // `save` (unlike `QuerySet::create`) is the path that fires model signals.
    let mut note = Note {
        id: 0,
        title: body.title,
    };
    note.save(db).await?;
    Ok(Json(note))
}

#[get("/notes")]
async fn list_notes(repo: Depends<NoteRepo>) -> Result<Json<Vec<String>>, ApiError> {
    Ok(Json(repo.0.titles().await?))
}

#[get("/notes/{id}")]
async fn get_note(repo: Depends<NoteRepo>, Path(id): Path<i64>) -> Result<Json<Note>, ApiError> {
    let NoteRepo::Database(db) = &*repo.0 else {
        return Err(ApiError::internal("read-only repository"));
    };
    Ok(Json(Note::objects(db).get(Note::id.eq(id)).await?))
}

#[delete("/notes/{id}")]
async fn delete_note(repo: Depends<NoteRepo>, Path(id): Path<i64>) -> Result<NoContent, ApiError> {
    let NoteRepo::Database(db) = &*repo.0 else {
        return Err(ApiError::internal("read-only repository"));
    };
    match Note::objects(db).filter(Note::id.eq(id)).delete().await? {
        0 => Err(ApiError::not_found("note not found")),
        _ => Ok(NoContent),
    }
}

#[get("/me")]
async fn me(who: Depends<Principal>) -> String {
    who.0.0.clone()
}

fn app() -> App {
    App::new().title("Notes").version("1.0.0").routes(routes![
        create_note,
        list_notes,
        get_note,
        delete_note,
        me
    ])
}

async fn database() -> TestDatabase {
    TestDatabase::sqlite_memory()
        .await
        .unwrap()
        .with_models(&[Note::META])
        .await
        .unwrap()
}

#[tokio::test]
async fn crud_round_trip_on_an_isolated_database() {
    let test = database().await;
    test.isolated(|db| async move {
        let client = TestClient::builder(app())
            .with_database("default", db)
            .build();

        let created = client
            .post_json("/notes", &json!({"title": "first"}))
            .await
            .unwrap();
        assert_eq!(created.status, StatusCode::CREATED);
        let id = created.json::<Value>().unwrap()["id"].as_i64().unwrap();

        let fetched = client.get(&format!("/notes/{id}")).await.unwrap();
        assert_eq!(fetched.status, StatusCode::OK);
        assert_eq!(fetched.json::<Value>().unwrap()["title"], "first");
        assert_eq!(
            client
                .get("/notes")
                .await
                .unwrap()
                .json::<Vec<String>>()
                .unwrap(),
            ["first"]
        );

        let invalid = client
            .post_json("/notes", &json!({"title": ""}))
            .await
            .unwrap();
        assert_eq!(invalid.status, StatusCode::UNPROCESSABLE_ENTITY);

        assert_eq!(
            client.delete(&format!("/notes/{id}")).await.unwrap().status,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            client.delete(&format!("/notes/{id}")).await.unwrap().status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            client.get(&format!("/notes/{id}")).await.unwrap().status,
            StatusCode::NOT_FOUND
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn isolated_tests_do_not_see_each_others_writes() {
    let test = database().await;
    for _ in 0..2 {
        test.isolated(|db| async move {
            let client = TestClient::builder(app())
                .with_database("default", db)
                .build();
            assert!(
                client
                    .get("/notes")
                    .await
                    .unwrap()
                    .json::<Vec<String>>()
                    .unwrap()
                    .is_empty()
            );
            client
                .post_json("/notes", &json!({"title": "temp"}))
                .await
                .unwrap();
        })
        .await
        .unwrap();
    }
    assert_eq!(Note::objects(test.db()).count().await.unwrap(), 0);
}

#[tokio::test]
async fn a_db_backed_dependency_can_be_overridden() {
    let test = database().await;
    let client = TestClient::builder(app())
        .with_database("default", test.db().clone())
        .override_value(NoteRepo::Fixed(vec!["canned".into()]))
        .build();

    let listed = client.get("/notes").await.unwrap();
    assert_eq!(listed.json::<Vec<String>>().unwrap(), ["canned"]);

    // The override replaces resolution entirely: the write path sees a
    // read-only repository and the database stays untouched.
    let created = client
        .post_json("/notes", &json!({"title": "x"}))
        .await
        .unwrap();
    assert_eq!(created.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(Note::objects(test.db()).count().await.unwrap(), 0);
}

#[tokio::test]
async fn override_dependency_receives_the_request_head() {
    let client = TestClient::builder(app())
        .override_dependency::<Principal, _, _>(|head| async move {
            let name = head
                .headers
                .get("x-user")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("anonymous")
                .to_owned();
            Ok(Principal(name))
        })
        .build();
    let req = Request::builder()
        .uri("/me")
        .header("x-user", "bob")
        .body(axumapi::Body::empty())
        .unwrap();
    assert_eq!(client.send(req).await.unwrap().text(), "bob");
    assert_eq!(client.get("/me").await.unwrap().text(), "anonymous");
}

#[tokio::test]
async fn bearer_auth_is_enforced_and_can_be_overridden() {
    let real = TestClient::new(app());
    let missing = real.get("/me").await.unwrap();
    assert_eq!(missing.status, StatusCode::UNAUTHORIZED);
    assert!(missing.headers.contains_key(header::WWW_AUTHENTICATE));

    let bad = Request::builder()
        .uri("/me")
        .header(header::AUTHORIZATION, "Bearer nope")
        .body(axumapi::Body::empty())
        .unwrap();
    assert_eq!(
        real.send(bad).await.unwrap().status,
        StatusCode::UNAUTHORIZED
    );

    let good = Request::builder()
        .uri("/me")
        .header(header::AUTHORIZATION, "Bearer secret")
        .body(axumapi::Body::empty())
        .unwrap();
    assert_eq!(real.send(good).await.unwrap().text(), "alice");

    let overridden = TestClient::builder(app())
        .override_value(Principal("tester".into()))
        .build();
    let response = overridden.get("/me").await.unwrap();
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.text(), "tester");
}

#[tokio::test]
async fn signals_fire_through_an_http_handler() {
    let signals = Signals::new();
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    for name in [SignalName::PreSave, SignalName::PostSave] {
        let log = Arc::clone(&log);
        signals.connect(Receiver::new::<Note, _>(name, move |note, event| {
            let log = Arc::clone(&log);
            Box::pin(async move {
                let created = matches!(event.kind, SignalKind::PostSave { created: true });
                log.lock().unwrap().push(format!(
                    "{:?}:{}:{created}",
                    event.kind.name(),
                    note.title
                ));
                Ok(())
            })
        }));
    }
    let test = TestDatabase::sqlite_memory()
        .await
        .unwrap()
        .with_signals(signals)
        .with_models(&[Note::META])
        .await
        .unwrap();
    let client = TestClient::builder(app())
        .with_database("default", test.db().clone())
        .build();

    let created = client
        .post_json("/notes", &json!({"title": "signalled"}))
        .await
        .unwrap();
    assert_eq!(created.status, StatusCode::CREATED);
    assert_eq!(
        *log.lock().unwrap(),
        ["PreSave:signalled:false", "PostSave:signalled:true"]
    );
}
