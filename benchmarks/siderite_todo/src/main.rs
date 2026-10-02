//! Siderite Todo API benchmark binary matching FastAPI todo_app.

use siderite::prelude::*;
use siderite_backends::mongodb::MongoBackend;
use siderite_backends::mysql::MySqlBackend;
use siderite_backends::postgres::PgBackend;
use siderite_backends::sqlite::SqliteBackend;

const SQLITE_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS todos (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    title TEXT NOT NULL,
    done INTEGER NOT NULL DEFAULT 0
);
";

const PG_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS todos (
    id BIGSERIAL PRIMARY KEY,
    title VARCHAR(280) NOT NULL,
    done BOOLEAN NOT NULL DEFAULT FALSE
);
";

const MYSQL_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS todos (
    id BIGINT PRIMARY KEY AUTO_INCREMENT,
    title VARCHAR(280) NOT NULL,
    done BOOLEAN NOT NULL DEFAULT FALSE
);
";

#[derive(Debug, Clone, Model, Serialize, Deserialize, Validate, Schema)]
#[model(table = "todos", ordering = ["-id"])]
pub struct Todo {
    #[field(primary_key, auto)]
    #[serde(default)]
    pub id: i64,
    #[field(min_length = 1, max_length = 280)]
    pub title: String,
    pub done: bool,
}

#[derive(Debug, Deserialize, Validate, Schema)]
pub struct NewTodo {
    #[field(min_length = 1, max_length = 280)]
    pub title: String,
}

pub async fn open_db(url: &str) -> Result<Db, ApiError> {
    if url.starts_with("postgres://") || url.starts_with("postgresql://") {
        let backend = PgBackend::connect(url).await.map_err(ApiError::internal)?;
        let db = Db::new(backend);
        db.execute_script(PG_SCHEMA)
            .await
            .map_err(ApiError::internal)?;
        Ok(db)
    } else if url.starts_with("mysql://") {
        let backend = MySqlBackend::connect(url)
            .await
            .map_err(ApiError::internal)?;
        let db = Db::new(backend);
        db.execute_script(MYSQL_SCHEMA)
            .await
            .map_err(ApiError::internal)?;
        Ok(db)
    } else if url.starts_with("mongodb://") {
        let backend = MongoBackend::connect(url, "siderite")
            .await
            .map_err(ApiError::internal)?;
        Ok(Db::new(backend))
    } else {
        let backend = SqliteBackend::connect(url)
            .await
            .map_err(ApiError::internal)?;
        let db = Db::new(backend);
        db.execute_script(SQLITE_SCHEMA)
            .await
            .map_err(ApiError::internal)?;
        Ok(db)
    }
}

#[get("/health")]
async fn health() -> PlainText<&'static str> {
    PlainText("ok")
}

#[get("/todos")]
async fn list_todos(db: Provided<Db>) -> Result<Json<Vec<Todo>>, ApiError> {
    let items = Todo::objects(&db).limit(20).all().await?;
    Ok(Json(items))
}

#[get("/todos/{id}")]
async fn get_todo(db: Provided<Db>, Path(id): Path<i64>) -> Result<Json<Todo>, ApiError> {
    let item = Todo::objects(&db).get(Todo::id.eq(id)).await?;
    Ok(Json(item))
}

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

pub fn app(db: Db) -> App {
    App::new()
        .title("Todo Benchmark")
        .version("1.0.0")
        .provide(db)
        .routes(routes![health, list_todos, get_todo, create_todo])
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let mut addr = "127.0.0.1:8081".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "run" {
            continue;
        }
        if arg == "--addr"
            && let Some(val) = args.next()
        {
            addr = val;
        }
    }

    let url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://todo_bench.db?mode=rwc".into());
    let db = match open_db(&url).await {
        Ok(db) => db,
        Err(err) => {
            eprintln!("Database error: {err}");
            return std::process::ExitCode::from(1);
        }
    };

    if let Err(err) = app(db).run(&addr).await {
        eprintln!("Server error: {err}");
        return std::process::ExitCode::from(1);
    }
    std::process::ExitCode::SUCCESS
}
