---
title: Database routing
description: Several databases, DatabaseRouter, for_read / for_write, and why querysets never span databases.
---

An application can talk to several databases: a primary and a read replica,
or a separate store for analytics. `Databases` is the registry, and a
`DatabaseRouter` picks the alias per model.

## Registering databases

```rust
use siderite::orm::Databases;
use siderite::prelude::*;

let app = App::new()
    .database("default", primary)
    .database("replica", replica)
    .route("/books", get(list_books));

async fn list_books(State(dbs): State<Databases>) -> Result<Json<Vec<Book>>, ApiError> {
    let books = dbs.objects::<Book>()?.all().await?;
    Ok(Json(books))
}
```

`App::database(alias, db)` adds one database. `App::databases(Databases)`
replaces the whole registry, including a router; a router set that way
survives later `App::database` calls. `App::database_registry()` returns
the registry. Handlers receive it as `State<Databases>`.

`Databases::new().with(alias, db).with_router(router)` builds a registry by
hand. `"default"` is the alias used whenever no router or caller says
otherwise (`Databases::DEFAULT`).

## Writing a router

```rust
use siderite::orm::ModelMeta;
use siderite::orm::router::DatabaseRouter;

struct AppRouter;

impl DatabaseRouter for AppRouter {
    fn db_for_read(&self, model: &ModelMeta) -> Option<&str> {
        match model.table {
            "books" => Some("replica"),
            "events" => Some("analytics"),
            _ => None,
        }
    }

    fn db_for_write(&self, model: &ModelMeta) -> Option<&str> {
        (model.table == "events").then_some("analytics")
    }

    fn allow_migrate(&self, alias: &str, model: &ModelMeta) -> bool {
        (model.table == "events") == (alias == "analytics")
    }
}
```

Every hook has a neutral default: `None` means “use `default`”, and
`allow_migrate` defaults to `true`.

## Choosing a database

| Call | Chooses |
|---|---|
| `dbs.objects::<M>()` | a `QuerySet<M>` on the read database (router, then `default`) |
| `dbs.for_read::<M>()` | the read `&Db` |
| `dbs.for_write::<M>()` | the write `&Db`; pass it to `save`, `delete`, or transactions |
| `dbs.using::<M>(alias)` | a `QuerySet<M>` on `alias`, **bypassing** the router |
| `dbs.get(alias)`, `dbs.default_db()` | a registered `&Db` directly |
| `dbs.aliases()` | the registered aliases, sorted |
| `dbs.allow_migrate(alias, &M::META)` | the router’s `allow_migrate` (`true` without a router) |

Writes are not routed implicitly. `user.save(&db)` uses whichever `Db` you
pass, so fetch it with `dbs.for_write::<User>()?` when routing matters.

A router (or a caller of `using`) that names an unregistered alias yields
`OrmError::UnknownDatabase(alias)`. Treat it as a configuration error: it
maps to an internal server error.

`QuerySet::using(&Db)` still exists and rebinds a queryset to a handle you
already hold.

`allow_migrate` is a query on the registry. The migration runner and the
CLI do not consult it yet; they apply migrations to the database you give
them (`--database ALIAS`). `AppCli::database_router` installs the router
when `run` builds the registry.

## Querysets never span databases

A `QuerySet` holds exactly one `Db`. Combining querysets bound to different
databases with `union`, `union_all`, `intersection`, or `difference` fails
with `QueryError::InvalidPlan` before any SQL is sent. A transaction handle
counts as the database it was opened on, so combining a transactional
queryset with a pool queryset of the same database is fine.

A queryset turned into a subquery (`QuerySet::subquery(..)` or
`exists_expr()`) remembers its database. Running a query or bulk
update/delete that contains a subquery from another database fails the
same way. A transaction handle counts as its pool’s database.

The [Two databases](/siderite/tutorials/polyglot/) tutorial walks the `polyglot`
example.

## See also

- [CLI](/siderite/guides/production/cli/) — `AppCli::database_router`
- [Transactions](/siderite/guides/data/transactions/)
- [QuerySets](/siderite/guides/data/querysets/)
