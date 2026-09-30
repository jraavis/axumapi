---
title: Todo on SQLite
description: Models, ForeignKey, CRUD handlers, and TestClient against in-memory SQLite.
---

Package: `examples/todo_sqlite`. Two models, six routes, and an in-process
test that covers the whole CRUD loop.

## Run

```bash
cargo run -p todo_sqlite
```

`DATABASE_URL` defaults to `sqlite://todo.db?mode=rwc`. `open_db` connects
with `SqliteBackend` and runs a small `CREATE TABLE IF NOT EXISTS` script
(this example does not use JSON migrations).

## Models

```rust
#[derive(Debug, Clone, Model, Serialize, Deserialize, Validate, Schema)]
#[model(table = "users")]
pub struct User {
    #[field(primary_key, auto)]
    #[serde(default)]
    pub id: i64,
    #[field(min_length = 1, max_length = 100)]
    pub name: String,
}

#[derive(Debug, Clone, Model, Serialize, Deserialize, Schema, Validate)]
#[model(table = "todos", ordering = ["id"])]
pub struct Todo {
    #[field(primary_key, auto)]
    #[serde(default)]
    pub id: i64,
    #[field(min_length = 1, max_length = 280)]
    pub title: String,
    pub done: bool,
    #[field(related_name = "todos")]
    pub owner: ForeignKey<User>,
}
```

`ForeignKey<User>` serializes as the owner’s primary key. `related_name`
adds `user.todos(&db)`.

## App factory

```rust
pub fn app(db: Db) -> App {
    App::new()
        .title("Todo")
        .version("1.0.0")
        .provide(db)
        .routes(routes![
            create_user, list_todos, create_todo, get_todo, patch_todo, delete_todo
        ])
}
```

Handlers take `Provided<Db>` — the value from `App::provide`. Create a row
with `Todo::objects(&db).create(..)`; fetch with `.get(Todo::id.eq(id))`;
patch by loading, mutating, and `todo.save(&db)`; delete with
`.filter(..).delete()`.

An empty title is 422 because of `min_length = 1`. A missing todo is 404
from `QueryError::DoesNotExist`.

## Tests

`cargo test -p todo_sqlite` opens `sqlite::memory:`, builds `TestClient`,
and asserts the round trip plus the 422. No sockets.

## Try it

```bash
curl -s -X POST http://127.0.0.1:8000/users \
  -H 'content-type: application/json' -d '{"name":"Ann"}'
curl -s -X POST http://127.0.0.1:8000/todos \
  -H 'content-type: application/json' -d '{"title":"write docs","owner":1}'
curl -s http://127.0.0.1:8000/todos
```

## See also

- [Models](/axumapi/guides/data/models/)
- [Testing](/axumapi/guides/production/testing/)
- [Migrations](/axumapi/guides/data/migrations/) — the blog example uses JSON
  migrations instead of `execute_script`
