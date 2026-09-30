---
title: Examples
description: What each workspace example demonstrates and how to run it.
---

The repository `examples/` directory is the executable counterpart of these
docs. Each package is a workspace member.

| Package | Shows | Needs |
|---|---|---|
| `hello_world` | Routes, path/query extractors, JSON validation, OpenAPI | nothing |
| `todo_sqlite` | `#[derive(Model)]`, CRUD, `ForeignKey`, `TestClient` | SQLite (default) |
| `blog_postgres` | `AppCli`, migrations, OAuth2 password flow, signals, pagination | `DATABASE_URL` (or in-memory SQLite for tests) |
| `polyglot` | Two databases, `DatabaseRouter`, cross-db rejection | optional `USERS_DATABASE_URL` / `ANALYTICS_DATABASE_URL` |
| `todo_mongo` | Same handlers as the SQLite todo on MongoDB | `MONGODB_URL` |

```bash
cargo run -p hello_world
cargo run -p todo_sqlite
```

Walkthroughs:

- [Hello World](/siderite/tutorials/hello-world/)
- [Todo on SQLite](/siderite/tutorials/todo-sqlite/)
- [Blog on PostgreSQL](/siderite/tutorials/blog-postgres/)
- [Two databases](/siderite/tutorials/polyglot/)
- [Todo on MongoDB](/siderite/tutorials/todo-mongo/)

Live databases for the PostgreSQL, MySQL, MongoDB, and Redis suites start
from the root `docker-compose.yml`. See [Testing](/siderite/guides/production/testing/).
