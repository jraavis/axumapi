//! SQLite-backed todo API: models, routes and an [`App`] factory.

use siderite::cache::{MemoryCache, RouteCache};
use siderite::prelude::*;
use siderite_backends::sqlite::SqliteBackend;
use std::time::Duration;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS todos (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    title TEXT NOT NULL,
    done INTEGER NOT NULL,
    owner_id INTEGER NOT NULL REFERENCES users(id)
);
";

/// A person who owns todos.
#[derive(Debug, Clone, Model, Serialize, Deserialize, Validate, Schema)]
#[model(table = "users")]
pub struct User {
    /// Database key.
    #[field(primary_key, auto)]
    #[serde(default)]
    pub id: i64,
    /// Display name.
    #[field(min_length = 1, max_length = 100)]
    pub name: String,
}

/// A todo item owned by a [`User`].
#[derive(Debug, Clone, Model, Serialize, Deserialize, Validate, Schema)]
#[model(table = "todos", ordering = ["id"])]
pub struct Todo {
    /// Database key.
    #[field(primary_key, auto)]
    #[serde(default)]
    pub id: i64,
    /// What to do.
    #[field(min_length = 1, max_length = 280)]
    pub title: String,
    /// Whether the item is complete.
    pub done: bool,
    /// Owner of the item.
    #[field(related_name = "todos")]
    pub owner: ForeignKey<User>,
}

/// Body of `POST /users`.
#[derive(Debug, Deserialize, Validate, Schema)]
pub struct NewUser {
    /// Display name.
    #[field(min_length = 1, max_length = 100)]
    pub name: String,
}

/// Body of `POST /todos`.
#[derive(Debug, Deserialize, Validate, Schema)]
pub struct NewTodo {
    /// What to do.
    #[field(min_length = 1, max_length = 280)]
    pub title: String,
    /// Owner primary key.
    pub owner: ForeignKey<User>,
}

/// Body of `PATCH /todos/{id}`.
#[derive(Debug, Deserialize, Validate, Schema)]
pub struct PatchTodo {
    /// Replacement title.
    #[field(min_length = 1, max_length = 280)]
    pub title: Option<String>,
    /// Replacement completion flag.
    pub done: Option<bool>,
}

/// Open a SQLite database and create the todo tables.
///
/// # Errors
/// Connection or DDL failures, mapped to an internal API error.
pub async fn open_db(url: &str) -> Result<Db, ApiError> {
    let backend = SqliteBackend::connect(url)
        .await
        .map_err(ApiError::internal)?;
    let db = Db::new(backend);
    db.execute_script(SCHEMA)
        .await
        .map_err(ApiError::internal)?;
    Ok(db)
}

/// Application serving the todo API against `db`.
pub fn app(db: Db) -> App {
    App::new()
        .title("Todo")
        .version("1.0.0")
        .provide(db)
        .routes(routes![
            create_user,
            list_todos,
            create_todo,
            get_todo,
            patch_todo,
            delete_todo
        ])
        // Stores only responses that opt in with `Cache-Control: public`.
        .layer(RouteCache::new(MemoryCache::new(256)))
}

/// Create a user.
#[post("/users", status = 201)]
async fn create_user(db: Provided<Db>, Json(body): Json<NewUser>) -> Result<Json<User>, ApiError> {
    let user = User::objects(&db)
        .create(User {
            id: 0,
            name: body.name,
        })
        .await?;
    Ok(Json(user))
}

/// List every todo, oldest first.
///
/// Cached for a few seconds to show `RouteCache`: the list may lag behind
/// writes by up to that long, since nothing invalidates it.
#[get("/todos")]
async fn list_todos(db: Provided<Db>) -> Result<Cached<Json<Vec<Todo>>>, ApiError> {
    let todos = Todo::objects(&db).all().await?;
    Ok(Cached::public(Duration::from_secs(5), Json(todos)))
}

/// Create a todo.
#[post("/todos", status = 201)]
async fn create_todo(db: Provided<Db>, Json(body): Json<NewTodo>) -> Result<Json<Todo>, ApiError> {
    let todo = Todo::objects(&db)
        .create(Todo {
            id: 0,
            title: body.title,
            done: false,
            owner: body.owner,
        })
        .await?;
    Ok(Json(todo))
}

/// Fetch one todo.
#[get("/todos/{id}")]
async fn get_todo(db: Provided<Db>, Path(id): Path<i64>) -> Result<Json<Todo>, ApiError> {
    Ok(Json(Todo::objects(&db).get(Todo::id.eq(id)).await?))
}

/// Patch title and/or completion of a todo.
#[patch("/todos/{id}")]
async fn patch_todo(
    db: Provided<Db>,
    Path(id): Path<i64>,
    Json(body): Json<PatchTodo>,
) -> Result<Json<Todo>, ApiError> {
    let mut todo = Todo::objects(&db).get(Todo::id.eq(id)).await?;
    if let Some(title) = body.title {
        todo.title = title;
    }
    if let Some(done) = body.done {
        todo.done = done;
    }
    todo.save(&db).await?;
    Ok(Json(todo))
}

/// Delete a todo.
#[delete("/todos/{id}")]
async fn delete_todo(db: Provided<Db>, Path(id): Path<i64>) -> Result<NoContent, ApiError> {
    let n = Todo::objects(&db).filter(Todo::id.eq(id)).delete().await?;
    if n == 0 {
        return Err(ApiError::not_found("todo not found"));
    }
    Ok(NoContent)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use http::{Method, Request, header};
    use serde_json::{Value, json};
    use siderite::Body;
    use siderite::http::StatusCode;
    use siderite_testkit::TestClient;

    async fn client() -> TestClient {
        let db = open_db("sqlite::memory:").await.unwrap();
        TestClient::new(app(db))
    }

    async fn send_json(
        client: &TestClient,
        method: Method,
        path: &str,
        body: &Value,
    ) -> siderite_testkit::TestResponse {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(body).unwrap()))
            .unwrap();
        client.send(req).await.unwrap()
    }

    #[tokio::test]
    async fn crud_round_trip() {
        let client = client().await;

        let created_user = client
            .post_json("/users", &json!({"name": "Ann"}))
            .await
            .unwrap();
        assert_eq!(created_user.status, StatusCode::CREATED);
        let user: Value = created_user.json().unwrap();
        let owner = user["id"].clone();

        let created = client
            .post_json("/todos", &json!({"title": "write docs", "owner": owner}))
            .await
            .unwrap();
        assert_eq!(created.status, StatusCode::CREATED);
        let todo: Value = created.json().unwrap();
        assert_eq!(todo["title"], "write docs");
        assert_eq!(todo["done"], false);
        let id = todo["id"].as_i64().unwrap();

        let listed = client.get("/todos").await.unwrap();
        assert_eq!(listed.status, StatusCode::OK);
        let items: Vec<Value> = listed.json().unwrap();
        assert_eq!(items.len(), 1);

        let fetched = client.get(&format!("/todos/{id}")).await.unwrap();
        assert_eq!(fetched.status, StatusCode::OK);
        assert_eq!(fetched.json::<Value>().unwrap()["title"], "write docs");

        let patched = send_json(
            &client,
            Method::PATCH,
            &format!("/todos/{id}"),
            &json!({"done": true}),
        )
        .await;
        assert_eq!(patched.status, StatusCode::OK);
        assert_eq!(patched.json::<Value>().unwrap()["done"], true);

        let deleted = client
            .send(
                Request::builder()
                    .method(Method::DELETE)
                    .uri(format!("/todos/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(deleted.status, StatusCode::NO_CONTENT);

        let missing = client.get(&format!("/todos/{id}")).await.unwrap();
        assert_eq!(missing.status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn todo_list_is_cached_and_other_routes_are_not() {
        let client = client().await;
        let first = client.get("/todos").await.unwrap();
        assert_eq!(first.headers["x-cache"], "miss");
        assert_eq!(first.headers["cache-control"], "public, max-age=5");
        let second = client.get("/todos").await.unwrap();
        assert_eq!(second.headers["x-cache"], "hit");

        let missing = client.get("/todos/999").await.unwrap();
        assert_eq!(missing.status, StatusCode::NOT_FOUND);
        let again = client.get("/todos/999").await.unwrap();
        assert_eq!(again.headers["x-cache"], "miss");
    }

    #[tokio::test]
    async fn empty_title_is_422() {
        let client = client().await;
        let user = client
            .post_json("/users", &json!({"name": "Ann"}))
            .await
            .unwrap()
            .json::<Value>()
            .unwrap();
        let res = client
            .post_json("/todos", &json!({"title": "", "owner": user["id"]}))
            .await
            .unwrap();
        assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    }
}
