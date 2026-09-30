# todo_mongo

The todo CRUD API from `todo_sqlite`, backed by MongoDB through the same ORM
models and handlers.

## Run

```bash
export MONGODB_URL='mongodb://127.0.0.1:57017/siderite?directConnection=true'
MONGODB_DATABASE=todos ADDR=127.0.0.1:18080 cargo run -p todo_mongo
curl -XPOST localhost:18080/todos -H 'content-type: application/json' -d '{"title":"try mongo"}'
curl 'localhost:18080/todos?done=false&limit=10&offset=0'
curl localhost:18080/todos/stats
```

`cargo test -p todo_mongo` uses `MONGODB_URL` and a throw-away database per
test; without the variable the tests print a notice and pass.

## What works

Single-collection CRUD, `filter`/`exclude`, ordering, `limit`/`offset`,
`count`, `save` (update by key). The `id` column is stored as `_id`; when it is
omitted the backend generates increasing `i64` keys from a counter collection
(`siderite_counters`).

## Limitations (kept out of this example)

The MongoDB backend rejects, before any I/O, with a capability error (HTTP
`501`): joins (`select_related`, foreign-key traversal), subqueries
(`in_subquery`, `exists`), set operations (`union`, ...), window functions,
row locking, `DISTINCT ON` and array aggregates. Consequently there is no
`ForeignKey` between models here, and no many-to-many (its queries use
subqueries). Other differences:

- No SQL migrations: collections appear on first write. Unique constraints
  exist only for `_id` unless created with `MongoBackend::create_unique_index`.
- Transactions need a replica set and are flat (no savepoints), so nested
  `Db::transaction` and `bulk_create` on a transactional handle are rejected.
- Timestamps have millisecond precision.
