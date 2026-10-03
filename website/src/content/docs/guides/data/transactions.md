---
title: Transactions
description: Scoped transaction closures, savepoints, isolation levels, and on_commit hooks.
---

Transactions are scoped closures. The closure receives a `Db` bound to the
open transaction — the same type as the pool handle.

```rust
db.transaction(|tx| async move {
    let mut user = User::objects(&tx).get(User::id.eq(1)).await?;
    user.name = "Ann".into();
    user.save(&tx).await?;
    tx.on_commit(|| tracing::info!("committed"));
    Ok::<_, OrmError>(())
}).await?;
```

- Returning `Ok` commits; `Err` rolls back.
- Cancelling a closure, child savepoint or statement makes the whole
  transaction unusable. The outer scope cannot commit its writes, even if
  application code catches the cancellation or panic and returns success.
- Overlapping statements and sibling scopes return `TransactionBusy`.
  While a child is active, use its handle; the parent is temporarily inactive.
  Recursive nesting through the child remains supported.
- Handles retained after a completed scope cannot execute or register
  commit hooks. Hooks of cancelled or rolled-back scopes are discarded.
- Cancellation during COMMIT can leave the database outcome unknown;
  resolve the operation outcome before retrying an insert.
- Calling `transaction` on a transaction handle creates a savepoint.
  `on_commit` hooks registered inside a savepoint that rolls back are
  discarded.
- `transaction_with(IsolationLevel::..)` accepts only the levels the
  backend lists. SQLite lists only `Serializable`. Any other level is a
  capability error raised before any I/O.
- Inside the closure, use `tx` and not the outer `db`. On a one-connection
  pool such as `sqlite::memory:`, the outer handle would wait forever for a
  second connection.

`on_commit` is the place for mail, webhooks, and cache invalidation. Outside
a transaction the hook runs immediately.

## Isolation and locking

| Backend | Isolation levels | Savepoints | `select_for_update` |
|---|---|---|---|
| PostgreSQL | read committed, repeatable read, serializable | yes | yes, plus `nowait`, `skip_locked` |
| SQLite | serializable only | yes | capability error |
| MySQL | all three | yes | yes, plus `nowait`, `skip_locked` |
| MongoDB | none requestable | no (flat transactions) | capability error |

`select_for_update()` keeps locks until the surrounding transaction ends.
Use it inside `Db::transaction`. Outside one, PostgreSQL releases the locks
immediately. PostgreSQL refuses `FOR UPDATE` on the nullable side of an
outer join: do not combine it with `select_related` or filters through
nullable foreign keys.

MongoDB transactions need a replica set. Nested `Db::transaction` and
`bulk_create` on a transactional MongoDB handle are capability errors (no
savepoints).

## See also

- [Signals](/siderite/guides/data/signals/) — receivers run on the operation’s `Db`
- [Backends](/siderite/guides/data/backends/)
- [Testing](/siderite/guides/production/testing/) — `TestDatabase::isolated`
