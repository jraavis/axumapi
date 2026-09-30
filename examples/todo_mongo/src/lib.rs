//! MongoDB-backed todo API.
//!
//! The ORM compiles queries to MongoDB filters and aggregation pipelines, so
//! the models and handlers look like the SQL examples. What MongoDB cannot
//! express fails explicitly with a capability error (`501`); see the README
//! for the list. This example sticks to what works: single-collection CRUD,
//! filters, ordering, `limit`/`offset` and counts.

use siderite::prelude::*;
use siderite_backends::mongodb::MongoBackend;

/// Default maximum page size of `GET /todos`.
pub const MAX_LIMIT: u64 = 100;

/// A todo item, stored in the `todos` collection.
///
/// The `id` column becomes the document's `_id`; the backend generates it
/// from a counter when it is omitted.
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
}

/// Body of `POST /todos`.
#[derive(Debug, Deserialize, Validate, Schema)]
pub struct NewTodo {
    /// What to do.
    #[field(min_length = 1, max_length = 280)]
    pub title: String,
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

/// Query parameters of `GET /todos`.
#[derive(Debug, Deserialize, Validate, Schema)]
pub struct TodoFilter {
    /// Only items with this completion flag.
    pub done: Option<bool>,
    /// Page size (default 20, at most 100).
    #[field(ge = 1, le = 100)]
    pub limit: Option<u64>,
    /// Items to skip (default 0).
    pub offset: Option<u64>,
}

/// Response of `GET /todos/stats`.
#[derive(Debug, Serialize, Schema)]
pub struct Stats {
    /// All items.
    pub total: u64,
    /// Completed items.
    pub done: u64,
}

/// Connect to MongoDB and return a [`Db`] on `database`.
///
/// # Errors
/// Connection failures, mapped to an internal API error.
pub async fn open_db(url: &str, database: &str) -> Result<Db, ApiError> {
    let backend = MongoBackend::connect(url, database)
        .await
        .map_err(ApiError::internal)?;
    Ok(Db::new(backend))
}

/// Application serving the todo API against `db`.
pub fn app(db: Db) -> App {
    App::new()
        .title("Todo (MongoDB)")
        .version("1.0.0")
        .provide(db)
        .routes(routes![
            list_todos,
            create_todo,
            todo_stats,
            get_todo,
            patch_todo,
            delete_todo
        ])
}

/// List todos, optionally by completion, one page at a time.
#[get("/todos")]
async fn list_todos(
    db: Provided<Db>,
    Query(filter): Query<TodoFilter>,
) -> Result<Json<Vec<Todo>>, ApiError> {
    let mut queryset = Todo::objects(&db);
    if let Some(done) = filter.done {
        queryset = queryset.filter(Todo::done.eq(done));
    }
    let limit = filter.limit.unwrap_or(20).min(MAX_LIMIT);
    let items = queryset
        .offset(filter.offset.unwrap_or(0))
        .limit(limit)
        .all()
        .await?;
    Ok(Json(items))
}

/// Create a todo.
#[post("/todos", status = 201)]
async fn create_todo(db: Provided<Db>, Json(body): Json<NewTodo>) -> Result<Json<Todo>, ApiError> {
    let todo = Todo::objects(&db)
        .create(Todo {
            id: 0,
            title: body.title,
            done: false,
        })
        .await?;
    Ok(Json(todo))
}

/// Count all and completed todos.
#[get("/todos/stats")]
async fn todo_stats(db: Provided<Db>) -> Result<Json<Stats>, ApiError> {
    let total = Todo::objects(&db).count().await?;
    let done = Todo::objects(&db)
        .filter(Todo::done.eq(true))
        .count()
        .await?;
    Ok(Json(Stats { total, done }))
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
