//! Model signals end to end on SQLite: ordering, aborting `pre_*` receivers,
//! `post_*` receivers inside transactions, the `created` flag, `m2m_changed`
//! and the `#[receiver]` macro.
#![allow(clippy::unwrap_used, dead_code)]

use siderite::orm::signals::{
    M2mAction, Receiver, SignalError, SignalEvent, SignalKind, SignalName, Signals,
};
use siderite::orm::{Db, ModelOps, OrmError, QueryError, Value};
use siderite::prelude::*;
use siderite::receiver;
use siderite_backends::sqlite::SqliteBackend;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Model)]
#[model(table = "books", many_to_many(tags(Tag, through_table = "book_tags")))]
struct Book {
    #[field(primary_key, auto)]
    id: i64,
    title: String,
}

#[derive(Debug, Clone, PartialEq, Model)]
struct Tag {
    #[field(primary_key, auto)]
    id: i64,
    #[field(unique)]
    label: String,
}

#[derive(Debug, Clone, PartialEq, Model)]
#[model(table = "audit")]
struct Audit {
    #[field(primary_key, auto)]
    id: i64,
    note: String,
}

type Log = Arc<Mutex<Vec<String>>>;

async fn database(signals: &Signals) -> Db {
    let db = Db::new(SqliteBackend::connect("sqlite::memory:").await.unwrap())
        .with_signals(signals.clone());
    db.execute_script(
        "CREATE TABLE books (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL);
         CREATE TABLE tag (id INTEGER PRIMARY KEY AUTOINCREMENT, label TEXT NOT NULL UNIQUE);
         CREATE TABLE audit (id INTEGER PRIMARY KEY AUTOINCREMENT, note TEXT NOT NULL);
         CREATE TABLE book_tags (book_id INTEGER NOT NULL, tag_id INTEGER NOT NULL);",
    )
    .await
    .unwrap();
    db
}

fn book(title: &str) -> Book {
    Book {
        id: 0,
        title: title.into(),
    }
}

fn tag(label: &str) -> Tag {
    Tag {
        id: 0,
        label: label.into(),
    }
}

/// Receiver that appends `tag:kind` to `log` and succeeds.
fn recorder(log: &Log, name: SignalName, tag: &'static str) -> Receiver {
    let log = Arc::clone(log);
    Receiver::new::<Book, _>(name, move |book, event| {
        let log = Arc::clone(&log);
        Box::pin(async move {
            log.lock()
                .unwrap()
                .push(format!("{tag}:{:?}:{}", event.kind, book.title));
            Ok(())
        })
    })
}

fn failing(name: SignalName, message: &'static str) -> Receiver {
    Receiver::new::<Book, _>(name, move |_, _| {
        Box::pin(async move { Err(SignalError::new(message)) })
    })
}

fn entries(log: &Log) -> Vec<String> {
    log.lock().unwrap().clone()
}

async fn book_count(db: &Db) -> u64 {
    Book::objects(db).count().await.unwrap()
}

#[tokio::test]
async fn receivers_run_in_connection_order_around_the_statement() {
    let signals = Signals::new();
    let log = Log::default();
    signals.connect(recorder(&log, SignalName::PreSave, "pre-a"));
    signals.connect(recorder(&log, SignalName::PostSave, "post-a"));
    signals.connect(recorder(&log, SignalName::PreSave, "pre-b"));
    signals.connect(recorder(&log, SignalName::PostSave, "post-b"));
    let db = database(&signals).await;

    let mut b = book("Dune");
    b.save(&db).await.unwrap();

    assert_eq!(
        entries(&log),
        [
            "pre-a:PreSave:Dune",
            "pre-b:PreSave:Dune",
            "post-a:PostSave { created: true }:Dune",
            "post-b:PostSave { created: true }:Dune",
        ]
    );
}

#[tokio::test]
async fn post_save_sees_the_stored_row_and_the_created_flag() {
    let signals = Signals::new();
    let seen: Arc<Mutex<Vec<(i64, bool)>>> = Arc::default();
    let sink = Arc::clone(&seen);
    signals.connect(Receiver::new::<Book, _>(
        SignalName::PostSave,
        move |b, e| {
            let sink = Arc::clone(&sink);
            Box::pin(async move {
                let SignalKind::PostSave { created } = e.kind else {
                    return Err(SignalError::new("unexpected kind"));
                };
                sink.lock().unwrap().push((b.id, created));
                Ok(())
            })
        },
    ));
    let db = database(&signals).await;

    let mut b = book("Dune");
    b.save(&db).await.unwrap();
    b.title = "Dune Messiah".into();
    b.save(&db).await.unwrap();

    assert_eq!(*seen.lock().unwrap(), [(1, true), (1, false)]);
}

#[tokio::test]
async fn a_failing_pre_save_aborts_the_save_and_skips_later_receivers() {
    let signals = Signals::new();
    let log = Log::default();
    signals.connect(failing(SignalName::PreSave, "title is banned"));
    signals.connect(recorder(&log, SignalName::PreSave, "later"));
    signals.connect(recorder(&log, SignalName::PostSave, "post"));
    let db = database(&signals).await;

    let err = book("Nope").save(&db).await.unwrap_err();

    assert!(matches!(err, OrmError::Signal(ref e) if e.message() == "title is banned"));
    assert_eq!(book_count(&db).await, 0);
    assert!(entries(&log).is_empty());
}

#[tokio::test]
async fn a_failing_post_save_reports_after_the_write_and_rolls_back_in_a_transaction() {
    let signals = Signals::new();
    signals.connect(failing(SignalName::PostSave, "audit unavailable"));
    let db = database(&signals).await;

    // Outside a transaction the statement already ran.
    let err = book("Outside").save(&db).await.unwrap_err();
    assert!(matches!(err, OrmError::Signal(_)));
    assert_eq!(book_count(&db).await, 1);

    // Inside one, the caller's rollback undoes it.
    let result = db
        .transaction(|tx| async move {
            book("Inside").save(&tx).await?;
            Ok::<_, OrmError>(())
        })
        .await;
    assert!(matches!(result, Err(OrmError::Signal(_))));
    assert_eq!(book_count(&db).await, 1);
}

#[tokio::test]
async fn receivers_run_on_the_operations_transaction() {
    let signals = Signals::new();
    signals.connect(Receiver::new::<Book, _>(SignalName::PostSave, |b, e| {
        Box::pin(async move {
            assert!(e.db.in_transaction());
            let mut note = Audit {
                id: 0,
                note: format!("saved {}", b.title),
            };
            note.save(e.db).await?;
            Ok(())
        })
    }));
    let db = database(&signals).await;

    // Committed together...
    db.transaction(|tx| async move {
        book("Kept").save(&tx).await?;
        Ok::<_, OrmError>(())
    })
    .await
    .unwrap();
    assert_eq!(Audit::objects(&db).count().await.unwrap(), 1);

    // ...and rolled back together.
    let rolled_back: Result<(), OrmError> = db
        .transaction(|tx| async move {
            book("Dropped").save(&tx).await?;
            Err(QueryError::Model("abort".into()).into())
        })
        .await;
    assert!(rolled_back.is_err());
    assert_eq!(Audit::objects(&db).count().await.unwrap(), 1);
    assert_eq!(book_count(&db).await, 1);
}

#[tokio::test]
async fn receivers_only_see_their_own_model() {
    let signals = Signals::new();
    let log = Log::default();
    signals.connect(recorder(&log, SignalName::PostSave, "book"));
    let db = database(&signals).await;

    tag("rust").save(&db).await.unwrap();
    assert!(entries(&log).is_empty());
    book("Dune").save(&db).await.unwrap();
    assert_eq!(entries(&log).len(), 1);
}

#[tokio::test]
async fn delete_signals_wrap_the_delete_and_skip_missing_rows() {
    let signals = Signals::new();
    let log = Log::default();
    signals.connect(recorder(&log, SignalName::PreDelete, "pre"));
    signals.connect(recorder(&log, SignalName::PostDelete, "post"));
    let db = database(&signals).await;
    let mut b = book("Dune");
    b.save(&db).await.unwrap();

    assert_eq!(b.delete(&db).await.unwrap(), 1);
    assert_eq!(
        entries(&log),
        ["pre:PreDelete:Dune", "post:PostDelete:Dune"]
    );

    // The row is gone: pre_delete still fires, post_delete does not.
    log.lock().unwrap().clear();
    assert_eq!(b.delete(&db).await.unwrap(), 0);
    assert_eq!(entries(&log), ["pre:PreDelete:Dune"]);
}

#[tokio::test]
async fn a_failing_pre_delete_keeps_the_row() {
    let signals = Signals::new();
    signals.connect(failing(SignalName::PreDelete, "protected"));
    let db = database(&signals).await;
    let mut b = book("Dune");
    b.save(&db).await.unwrap();

    assert!(matches!(b.delete(&db).await, Err(OrmError::Signal(_))));
    assert_eq!(book_count(&db).await, 1);
}

#[tokio::test]
async fn bulk_queryset_writes_send_no_signals() {
    let signals = Signals::new();
    let log = Log::default();
    for name in [
        SignalName::PreSave,
        SignalName::PostSave,
        SignalName::PreDelete,
        SignalName::PostDelete,
    ] {
        signals.connect(recorder(&log, name, "x"));
    }
    let db = database(&signals).await;
    book("Dune").save(&db).await.unwrap();
    log.lock().unwrap().clear();

    Book::objects(&db).delete().await.unwrap();
    assert!(entries(&log).is_empty());
}

/// `m2m_changed` receiver logging `action relation [pks]`.
fn m2m_recorder(log: &Log) -> Receiver {
    let log = Arc::clone(log);
    Receiver::new::<Book, _>(SignalName::M2mChanged, move |book, event| {
        let log = Arc::clone(&log);
        Box::pin(async move {
            let SignalKind::M2mChanged { action } = event.kind else {
                return Err(SignalError::new("unexpected kind"));
            };
            let pks: Vec<String> = event.pk_set.iter().map(|v| format!("{v:?}")).collect();
            log.lock().unwrap().push(format!(
                "{action:?} {} {} [{}]",
                event.relation.unwrap_or("?"),
                book.title,
                pks.join(",")
            ));
            Ok(())
        })
    })
}

#[tokio::test]
async fn m2m_changed_reports_add_remove_clear_and_set() {
    let signals = Signals::new();
    let log = Log::default();
    signals.connect(m2m_recorder(&log));
    let db = database(&signals).await;
    let mut b = book("Dune");
    b.save(&db).await.unwrap();
    let (mut t1, mut t2, mut t3) = (tag("a"), tag("b"), tag("c"));
    for t in [&mut t1, &mut t2, &mut t3] {
        t.save(&db).await.unwrap();
    }
    let one = |t: &Tag| format!("{:?}", Value::Int(t.id));
    let tags = b.tags(&db);

    tags.add([&t1, &t2]).await.unwrap();
    assert_eq!(
        entries(&log),
        [
            format!("PreAdd tags Dune [{},{}]", one(&t1), one(&t2)),
            format!("PostAdd tags Dune [{},{}]", one(&t1), one(&t2)),
        ]
    );

    // Already linked: nothing to add, nothing sent.
    log.lock().unwrap().clear();
    tags.add([&t1]).await.unwrap();
    assert!(entries(&log).is_empty());

    tags.remove([&t1]).await.unwrap();
    assert_eq!(
        entries(&log),
        [
            format!("PreRemove tags Dune [{}]", one(&t1)),
            format!("PostRemove tags Dune [{}]", one(&t1)),
        ]
    );

    // set: t2 stays, t3 is added, nothing is stale -> only an add.
    log.lock().unwrap().clear();
    tags.set([&t2, &t3]).await.unwrap();
    assert_eq!(
        entries(&log),
        [
            format!("PreAdd tags Dune [{}]", one(&t3)),
            format!("PostAdd tags Dune [{}]", one(&t3)),
        ]
    );

    // set to {t1}: remove t2 and t3, add t1.
    log.lock().unwrap().clear();
    tags.set([&t1]).await.unwrap();
    let logged = entries(&log);
    assert_eq!(logged.len(), 4);
    assert!(logged[0].starts_with("PreRemove") && logged[1].starts_with("PostRemove"));
    assert_eq!(logged[2], format!("PreAdd tags Dune [{}]", one(&t1)));
    assert_eq!(logged[3], format!("PostAdd tags Dune [{}]", one(&t1)));

    log.lock().unwrap().clear();
    assert_eq!(tags.clear().await.unwrap(), 1);
    assert_eq!(
        entries(&log),
        ["PreClear tags Dune []", "PostClear tags Dune []"]
    );
    assert_eq!(tags.count().await.unwrap(), 0);
}

#[tokio::test]
async fn a_failing_pre_add_leaves_the_join_table_untouched() {
    let signals = Signals::new();
    signals.connect(failing(SignalName::M2mChanged, "no tags today"));
    let db = database(&signals).await;
    let mut b = book("Dune");
    b.save(&db).await.unwrap();
    let mut t = tag("a");
    t.save(&db).await.unwrap();

    let err = b.tags(&db).add([&t]).await.unwrap_err();

    assert!(matches!(err, OrmError::Signal(_)));
    assert_eq!(b.tags(&db).count().await.unwrap(), 0);
}

#[tokio::test]
async fn a_failing_post_add_rolls_the_links_back() {
    let signals = Signals::new();
    let seen = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&seen);
    // Fail on the post step only.
    signals.connect(Receiver::new::<Book, _>(
        SignalName::M2mChanged,
        move |_, e| {
            let counter = Arc::clone(&counter);
            Box::pin(async move {
                counter.fetch_add(1, Ordering::SeqCst);
                match e.kind {
                    SignalKind::M2mChanged {
                        action: M2mAction::PostAdd,
                    } => Err(SignalError::new("late failure")),
                    _ => Ok(()),
                }
            })
        },
    ));
    let db = database(&signals).await;
    let mut b = book("Dune");
    b.save(&db).await.unwrap();
    let mut t = tag("a");
    t.save(&db).await.unwrap();

    assert!(b.tags(&db).add([&t]).await.is_err());

    assert_eq!(seen.load(Ordering::SeqCst), 2);
    // `add` runs in its own transaction, so the failed post step undid the link.
    assert_eq!(b.tags(&db).count().await.unwrap(), 0);
}

#[tokio::test]
async fn unobserved_relations_do_not_load_the_source_object() {
    // No receiver for Book: mutations must work even if the source row is
    // missing, proving the lookup is skipped.
    let signals = Signals::new();
    let db = database(&signals).await;
    let mut t = tag("a");
    t.save(&db).await.unwrap();
    let ghost = Book {
        id: 99,
        title: "ghost".into(),
    };
    ghost.tags(&db).add([&t]).await.unwrap();
    assert_eq!(ghost.tags(&db).count().await.unwrap(), 1);
}

#[tokio::test]
async fn receivers_connected_later_reach_existing_handles() {
    let signals = Signals::new();
    let db = database(&signals).await;
    let clone = db.clone();
    let log = Log::default();
    signals.connect(recorder(&log, SignalName::PostSave, "late"));
    assert_eq!(db.signals().len(), 1);

    book("Dune").save(&clone).await.unwrap();
    assert_eq!(entries(&log).len(), 1);
}

// ---------------------------------------------------------------------
// `#[receiver]`
// ---------------------------------------------------------------------

static MACRO_HITS: AtomicUsize = AtomicUsize::new(0);
static MACRO_CREATED: AtomicUsize = AtomicUsize::new(0);

#[receiver(post_save, model = Book)]
async fn count_books(instance: &Book, event: &SignalEvent<'_>) -> Result<(), SignalError> {
    assert_eq!(event.model.table, "books");
    assert!(!instance.title.is_empty());
    MACRO_HITS.fetch_add(1, Ordering::SeqCst);
    if matches!(event.kind, SignalKind::PostSave { created: true }) {
        MACRO_CREATED.fetch_add(1, Ordering::SeqCst);
    }
    Ok(())
}

#[receiver(pre_delete, model = Book)]
async fn forbid_deleting_dune(
    instance: &Book,
    _event: &SignalEvent<'_>,
) -> Result<(), SignalError> {
    if instance.title == "Dune" {
        return Err(SignalError::new("Dune is permanent"));
    }
    Ok(())
}

#[tokio::test]
async fn the_receiver_macro_generates_connectable_receivers() {
    let signals = Signals::new();
    signals.connect(count_books_receiver());
    signals.connect(forbid_deleting_dune_receiver());
    assert_eq!(count_books_receiver().name(), SignalName::PostSave);
    assert_eq!(count_books_receiver().model().name, "Book");
    let db = database(&signals).await;

    let mut dune = book("Dune");
    dune.save(&db).await.unwrap();
    dune.save(&db).await.unwrap();
    let mut other = book("Emma");
    other.save(&db).await.unwrap();

    assert_eq!(MACRO_HITS.load(Ordering::SeqCst), 3);
    assert_eq!(MACRO_CREATED.load(Ordering::SeqCst), 2);
    assert!(matches!(dune.delete(&db).await, Err(OrmError::Signal(_))));
    assert_eq!(other.delete(&db).await.unwrap(), 1);
}

#[tokio::test]
async fn signal_failures_map_to_an_internal_server_error() {
    let signals = Signals::new();
    signals.connect(failing(SignalName::PreSave, "secret detail"));
    let db = database(&signals).await;
    let err = book("x").save(&db).await.unwrap_err();

    let api = siderite::ApiError::from(err);
    assert_eq!(api.status().as_u16(), 500);
}
