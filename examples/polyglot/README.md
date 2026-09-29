# polyglot

Two databases behind one API: `users` on the `default` alias, `events` on the
`analytics` alias, chosen by a `DatabaseRouter` that routes by table name.

## Run

```bash
# both default to SQLite files in the current directory
ADDR=127.0.0.1:18080 cargo run -p polyglot

# or analytics on PostgreSQL
export ANALYTICS_DATABASE_URL=postgres://axumapi:axumapi@127.0.0.1:55432/axumapi
export USERS_DATABASE_URL='sqlite://users.db?mode=rwc'
```

Tables are created on start, each only on the alias the router's
`allow_migrate` permits (existing tables are left alone).

```bash
B=http://127.0.0.1:18080
curl -XPOST $B/users -H 'content-type: application/json' -d '{"name":"ann"}'
curl -XPOST $B/events -H 'content-type: application/json' -d '{"user_id":1,"kind":"login"}'
curl $B/users/1/events                 # joined in the application
curl "$B/events/count?database=analytics"   # using(alias), bypasses the router
curl $B/cross-database-union           # 400: querysets cannot span databases
```

## What it shows

- `Databases::with_router`, `for_read` / `for_write` / `objects` follow the
  router; handlers never name an alias.
- `using(alias)` picks a database explicitly and bypasses the router; an
  unregistered alias is `OrmError::UnknownDatabase` (mapped to 404 here).
- **No cross-database joins.** A queryset is bound to one database, so
  `Event.user_id` is a plain integer (no foreign key), the reference is checked
  and joined in application code, and combining querysets of different
  databases (`union`, `intersection`, ...) fails with
  `InvalidPlan("querysets bound to different databases cannot be combined")`
  before any query runs.

`cargo test -p polyglot` runs everything on two in-memory SQLite databases.
