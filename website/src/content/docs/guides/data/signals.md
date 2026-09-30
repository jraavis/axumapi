---
title: Signals
description: pre_save, post_save, pre_delete, post_delete, m2m_changed, and explicit receiver registration.
---

Model signals are the ORM’s counterpart of Django’s `pre_save`,
`post_save`, `pre_delete`, `post_delete`, and `m2m_changed`. Items live in
`siderite::orm::signals`.

## Kinds

| `SignalName` | `event.kind` | Fires |
|---|---|---|
| `PreSave` | `PreSave` | before `save()` writes |
| `PostSave` | `PostSave { created }` | after `save()`; `created` is `true` for an INSERT |
| `PreDelete` | `PreDelete` | before `delete()` |
| `PostDelete` | `PostDelete` | after `delete()` removed a row (skipped when no row matched) |
| `M2mChanged` | `M2mChanged { action }` | around `add`, `remove`, `clear`, and `set` on a many-to-many manager |

`M2mAction` is `PreAdd`, `PostAdd`, `PreRemove`, `PostRemove`, `PreClear`,
or `PostClear`. `set` is a `remove` of the stale links followed by an `add`
of the missing ones. `add` and `remove` with nothing to change send
nothing.

## Writing a receiver

```rust
use siderite::orm::signals::{SignalError, SignalEvent, Signals};
use siderite::prelude::*;
use siderite::receiver;

#[receiver(post_save, model = User)]
async fn audit(user: &User, event: &SignalEvent<'_>) -> Result<(), SignalError> {
    // event.db is the handle of the operation, so writes here run inside
    // the caller's transaction when there is one.
    let _ = (user, event);
    Ok(())
}

let signals = Signals::new();
signals.connect(audit_receiver());
let db = Db::new(backend).with_signals(signals);
```

Accepted names: `pre_save`, `post_save`, `pre_delete`, `post_delete`,
`m2m_changed`. Without the macro, build one with
`Receiver::new::<User, _>(SignalName::PostSave, |user, event| Box::pin(async move { Ok(()) }))`.

`SignalEvent` carries:

| Field | Meaning |
|---|---|
| `kind` | which signal fired, with its payload |
| `db` | the handle the operation runs on (the open transaction, if any) |
| `model` | `&'static ModelMeta` of the instance’s model |
| `relation` | `m2m_changed` only: the relation’s field name on the declaring model |
| `pk_set` | `m2m_changed` only: primary keys of the targets involved (empty for `clear`) |

For `m2m_changed` the receiver is typed on the **declaring** model and gets
the source object, loaded by primary key only when such a receiver is
connected.

## Registration is explicit

The registry is a value: `Db::with_signals(Signals)` attaches it, and
`Db::signals()` returns it. Clones of a `Db` and the transaction handles
derived from it share one registry. A receiver only sees its own model.

There is no static registration. Crates such as `inventory` collect items
through link-section tricks that need `unsafe` in the user’s crate, which
conflicts with `#![forbid(unsafe_code)]`. The cost is one `connect` call
per receiver at startup.

`TestDatabase::with_signals` attaches a registry to a test database. On
`runserver`, attach it once with
`AppCli::configure_db(|_alias, db| db.with_signals(signals()))`. The
`blog_postgres` example does this.

## Semantics

- **Order.** Receivers run one after the other, awaited, in connection
  order.
- **Same handle.** Receivers get the operation’s own `Db`. Inside
  `db.transaction(..)` they run in that transaction.
- **`pre_*` failure.** The operation is aborted, nothing is written, the
  error surfaces as `OrmError::Signal`, and later receivers do not run.
- **`post_*` failure.** The statement has already run. Inside a
  transaction the caller’s rollback undoes it; outside one, the write
  stays. Wrap the call in `Db::transaction` if a failing `post_*` receiver
  must undo it.
- **After commit.** Use `Db::on_commit(|| ..)` for work that must run only
  once the outermost transaction commits. Hooks of a rolled-back
  transaction or savepoint are discarded.
- **`SignalError`.** Build from a message (`SignalError::new`), from any
  error (`SignalError::from_error`), or with `?` on an `OrmError`. The text
  is logged and never returned to HTTP clients.
- **Async.** Receivers are `async` and must be `Send`. They run on the
  request path — keep them short.
- **`m2m_changed`.** `Pre*` fires before the join-table statements and
  `Post*` after them. Mutations run in a transaction (a savepoint inside an
  open one).

## Bulk operations send no signals

`QuerySet::update`, `QuerySet::delete`, and bulk creation do not send
signals, as in Django, because they never load the instances. If a side
effect must happen for every row, load the rows and call `save()` /
`delete()` on each.

## See also

- [Transactions](/siderite/guides/data/transactions/)
- [CLI](/siderite/guides/production/cli/) — `AppCli::configure_db`
- [Blog on PostgreSQL](/siderite/tutorials/blog-postgres/)
