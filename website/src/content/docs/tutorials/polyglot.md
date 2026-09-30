---
title: Two databases
description: Route users and events to different databases with DatabaseRouter.
---

Package: `examples/polyglot`. Two databases behind one API: `users` on the
`default` alias, `events` on the `analytics` alias, chosen by a
`DatabaseRouter` that routes by table name.

## Run

```bash
ADDR=127.0.0.1:18080 cargo run -p polyglot
```

Both aliases default to SQLite files in the current directory. Point
analytics at PostgreSQL if you want:

```bash
export ANALYTICS_DATABASE_URL=postgres://siderite:siderite@127.0.0.1:55432/siderite
export USERS_DATABASE_URL='sqlite://users.db?mode=rwc'
```

Tables are created on start, each only on the alias the router’s
`allow_migrate` permits (existing tables are left alone).

```bash
B=http://127.0.0.1:18080
curl -XPOST $B/users -H 'content-type: application/json' -d '{"name":"ann"}'
curl -XPOST $B/events -H 'content-type: application/json' -d '{"user_id":1,"kind":"login"}'
curl $B/users/1/events
curl "$B/events/count?database=analytics"
curl $B/cross-database-union
```

The last call is 400: querysets cannot span databases.

`cargo test -p polyglot` runs everything on two in-memory SQLite databases.

## What it shows

- `Databases::with_router`, `for_read` / `for_write` / `objects` follow the
  router; handlers never name an alias.
- `using(alias)` picks a database explicitly and bypasses the router; an
  unregistered alias is `OrmError::UnknownDatabase`.
- **No cross-database joins.** A queryset is bound to one database, so
  `Event.user_id` is a plain integer (no foreign key). The reference is
  checked and joined in application code. Combining querysets of different
  databases (`union`, `intersection`, …) fails with `InvalidPlan` before
  any query runs.

## See also

- [Database routing](/siderite/guides/data/database-routing/)
- [QuerySets](/siderite/guides/data/querysets/)
- [Backends](/siderite/guides/data/backends/)
