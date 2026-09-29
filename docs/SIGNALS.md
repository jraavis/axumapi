# Signals

Model signals are the ORM's counterpart of Django's `pre_save`, `post_save`, `pre_delete`, `post_delete` and `m2m_changed`. Items live in `axumapi_orm::signals` (`axumapi::orm::signals`).

## Kinds

| `SignalName` | `SignalKind` delivered in `event.kind` | Fires |
|---|---|---|
| `PreSave` | `PreSave` | before `save()` writes |
| `PostSave` | `PostSave { created }` | after `save()`; `created` is `true` for an INSERT |
| `PreDelete` | `PreDelete` | before `delete()` |
| `PostDelete` | `PostDelete` | after `delete()` removed a row (skipped when no row matched) |
| `M2mChanged` | `M2mChanged { action }` | around `add`, `remove`, `clear` and `set` on a many-to-many manager |

`M2mAction` is `PreAdd`, `PostAdd`, `PreRemove`, `PostRemove`, `PreClear` or `PostClear`. `set` is a `remove` of the stale links followed by an `add` of the missing ones. `add` and `remove` with nothing to change send nothing.

## Writing a receiver

A receiver is an async function over one model. `#[receiver(signal, model = M)]` keeps the function and generates `fn <name>_receiver() -> Receiver`:

```rust
use axumapi::orm::signals::{SignalError, SignalEvent, Signals};
use axumapi::prelude::*;
use axumapi::receiver;

#[receiver(post_save, model = User)]
async fn audit(user: &User, event: &SignalEvent<'_>) -> Result<(), SignalError> {
    // `event.db` is the handle of the operation, so writes here run inside
    // the caller's transaction when there is one.
    let _ = (user, event);
    Ok(())
}

let signals = Signals::new();
signals.connect(audit_receiver());
let db = Db::new(backend).with_signals(signals);
```

The accepted names are `pre_save`, `post_save`, `pre_delete`, `post_delete` and `m2m_changed`. Without the macro, build one by hand with `Receiver::new::<User, _>(SignalName::PostSave, |user, event| Box::pin(async move { Ok(()) }))`.

`SignalEvent` carries:

| Field | Meaning |
|---|---|
| `kind` | which signal fired, with its payload |
| `db` | the handle the operation runs on (the open transaction, if any) |
| `model` | `&'static ModelMeta` of the instance's model |
| `relation` | `m2m_changed` only: the relation's field name on the declaring model |
| `pk_set` | `m2m_changed` only: primary keys of the targets involved (empty for `clear`) |

For `m2m_changed` the receiver is typed on the **declaring** model and gets the source object, loaded by primary key only when such a receiver is connected.

## Registration is explicit

The registry is a value: `Db::with_signals(Signals)` attaches it, and `Db::signals()` returns it. Clones of a `Db` and the transaction handles derived from it share one registry, so `signals.connect(..)` (which takes `&self`) after `with_signals` is seen everywhere. A receiver only sees its own model; the downcast is checked.

There is deliberately **no static registration**. Crates such as `inventory` and `linkme` collect items through link-section tricks that need `unsafe` in the user's crate, which conflicts with `#![forbid(unsafe_code)]`. The cost is one `connect` call per receiver at startup.

`TestDatabase::with_signals` attaches a registry to a test database (see [TESTING.md](TESTING.md)).

## Semantics

* **Order.** Receivers run one after the other, awaited, in the order they were connected.
* **Same handle.** Receivers get the operation's own `Db`. Inside `db.transaction(..)` they run in that transaction.
* **`pre_*` failure.** The operation is aborted, nothing is written, the error surfaces as `OrmError::Signal`, and receivers connected after the failing one do not run.
* **`post_*` failure.** The statement has already run and the error is returned afterwards. Inside a transaction the caller's rollback undoes the statement; outside one, the write stays. Wrap the call in `Db::transaction` if a failing `post_*` receiver must undo it.
* **After commit.** Use `Db::on_commit(|| ..)` for work that must run only once the outermost transaction commits (mail, webhooks, cache invalidation). Hooks of a rolled-back transaction or savepoint are discarded. Outside a transaction the hook runs immediately.
* **`SignalError`.** Build one from a message (`SignalError::new`), from any error (`SignalError::from_error`) or with `?` on an `OrmError`. The text is logged and never returned to HTTP clients.
* **Async.** Receivers are `async` and must be `Send`. Keep them short; they run on the request path.
* **`m2m_changed`.** The `Pre*` action fires before the join-table statements and the `Post*` action after them. Mutations run in a transaction (a savepoint inside an open one).

## Bulk operations send no signals

`QuerySet::update`, `QuerySet::delete` and bulk creation do not send signals, as in Django, because they never load the instances. If a side effect must happen for every row, load the rows and call `save()` / `delete()` on each, or do the work in the calling code.
