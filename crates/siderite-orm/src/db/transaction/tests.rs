//! Deterministic cancellation and cleanup failure at scope SQL boundaries.

use super::*;
use crate::backend::{ExecResult, QueryResult as Rows};
use crate::capabilities::{BackendCapabilities, IsolationLevel as Isolation};
use crate::plan::QueryPlan;
use crate::value::Value;
use crate::write::WritePlan;
use async_trait::async_trait;
use std::future::pending;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::oneshot;

type Params = Vec<Value>;
type Handle = Box<dyn Transaction>;

fn original() -> OrmError {
    QueryError::InvalidPlan("original".into()).into()
}

struct Probe {
    commands: Mutex<Vec<String>>,
    paused_prefix: Option<&'static str>,
    failed_prefix: Option<&'static str>,
    reached: Mutex<Option<oneshot::Sender<()>>>,
    commits: AtomicUsize,
    rollbacks: AtomicUsize,
}

struct Adapter(Arc<Probe>);

#[async_trait]
impl Executor for Adapter {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::sqlite()
    }

    async fn fetch(&self, _: &QueryPlan) -> Result<Rows, OrmError> {
        Ok(Rows::default())
    }

    async fn execute(&self, _: &WritePlan) -> Result<ExecResult, OrmError> {
        Ok(ExecResult::default())
    }

    async fn fetch_raw(&self, _: &str, _: Params) -> Result<Rows, OrmError> {
        Ok(Rows::default())
    }

    async fn execute_raw(&self, s: &str, _: Params) -> Result<u64, OrmError> {
        self.execute_script(s).await?;
        Ok(1)
    }

    async fn execute_script(&self, sql: &str) -> Result<(), OrmError> {
        self.0
            .commands
            .lock()
            .map_err(|_| QueryError::TransactionAborted)?
            .push(sql.into());
        if self
            .0
            .paused_prefix
            .is_some_and(|prefix| sql.starts_with(prefix))
        {
            if let Some(notify) = self
                .0
                .reached
                .lock()
                .map_err(|_| QueryError::TransactionAborted)?
                .take()
            {
                let _ = notify.send(());
            }
            return pending().await;
        }
        if self
            .0
            .failed_prefix
            .is_some_and(|prefix| sql.starts_with(prefix))
        {
            let error = crate::BackendError::Database("cleanup failed".into());
            return Err(error.into());
        }
        Ok(())
    }
}

#[async_trait]
impl Backend for Adapter {
    async fn begin(&self, _: Option<Isolation>) -> Result<Handle, OrmError> {
        Ok(Box::new(Adapter(Arc::clone(&self.0))))
    }

    async fn begin_schema(&self, _: bool) -> Result<Handle, OrmError> {
        self.begin(None).await
    }
}

#[async_trait]
impl Transaction for Adapter {
    async fn commit(&self) -> Result<(), OrmError> {
        self.0.commits.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn rollback(&self) -> Result<(), OrmError> {
        self.0.rollbacks.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn fixture(
    paused: Option<&'static str>,
    failed: Option<&'static str>,
) -> (Db, Arc<Probe>, oneshot::Receiver<()>) {
    let (sender, receiver) = oneshot::channel();
    let probe = Arc::new(Probe {
        commands: Mutex::default(),
        paused_prefix: paused,
        failed_prefix: failed,
        reached: Mutex::new(Some(sender)),
        commits: AtomicUsize::new(0),
        rollbacks: AtomicUsize::new(0),
    });
    (Db::new(Adapter(Arc::clone(&probe))), probe, receiver)
}

#[tokio::test]
async fn cancellation_at_each_scope_command_prevents_commit() {
    for prefix in ["SAVEPOINT", "RELEASE", "ROLLBACK TO"] {
        let (db, probe, receiver) = fixture(Some(prefix), None);
        let result = db
            .transaction(|tx| async move {
                let mut child = Box::pin(tx.transaction(|_| async move {
                    if prefix == "ROLLBACK TO" {
                        Err(QueryError::InvalidPlan("original".into()).into())
                    } else {
                        Ok::<_, OrmError>(())
                    }
                }));
                tokio::select! {
                    reached = receiver => assert!(reached.is_ok()),
                    result = &mut child => panic!("child: {result:?}"),
                }
                drop(child);
                Ok::<_, OrmError>(())
            })
            .await;
        assert!(matches!(
            result,
            Err(OrmError::Query(QueryError::TransactionAborted))
        ));
        assert_eq!(probe.commits.load(Ordering::SeqCst), 0);
        assert_eq!(probe.rollbacks.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn cancelled_statement_marks_transaction_unusable() {
    let (db, probe, receiver) = fixture(Some("INSERT"), None);
    let result = db
        .transaction(|tx| async move {
            let mut query = Box::pin(tx.raw_execute("INSERT pending", vec![]));
            tokio::select! {
                reached = receiver => assert!(reached.is_ok()),
                result = &mut query => panic!("query completed: {result:?}"),
            }
            assert!(matches!(
                tx.raw_execute("another statement", vec![]).await,
                Err(OrmError::Query(QueryError::TransactionBusy))
            ));
            let nested = tx.transaction(|_| async { Ok::<_, OrmError>(()) });
            let sibling = nested.await;
            assert!(matches!(
                sibling,
                Err(OrmError::Query(QueryError::TransactionBusy))
            ));
            drop(query);
            Ok::<_, OrmError>(())
        })
        .await;
    assert!(matches!(
        result,
        Err(OrmError::Query(QueryError::TransactionAborted))
    ));
    assert_eq!(probe.commits.load(Ordering::SeqCst), 0);
    assert_eq!(probe.rollbacks.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cleanup_failure_preserves_original_error_and_aborts_parent() {
    let (db, probe, _) = fixture(None, Some("ROLLBACK TO"));
    let result = db
        .transaction(|tx| async move {
            let sp = tx.transaction(|_| async { Err::<(), _>(original()) });
            let child = sp.await;
            assert!(matches!(child,
            Err(OrmError::Query(QueryError::InvalidPlan(ref message)))
            if message == "original"));
            Ok::<_, OrmError>(())
        })
        .await;
    assert!(matches!(
        result,
        Err(OrmError::Query(QueryError::TransactionAborted))
    ));
    assert_eq!(probe.commits.load(Ordering::SeqCst), 0);
    assert_eq!(probe.rollbacks.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn failed_savepoints_release() -> Result<(), OrmError> {
    let (db, probe, _) = fixture(None, None);
    db.transaction(|tx| async move {
        let sp = tx.transaction(|_| async { Err::<(), _>(original()) });
        let child = sp.await;
        assert!(child.is_err());
        tx.transaction(|_| async { Ok::<_, OrmError>(()) }).await
    })
    .await?;
    let commands = probe
        .commands
        .lock()
        .map_err(|_| QueryError::TransactionAborted)?;
    assert_eq!(
        *commands,
        [
            "SAVEPOINT siderite_sp_1",
            "ROLLBACK TO SAVEPOINT siderite_sp_1",
            "RELEASE SAVEPOINT siderite_sp_1",
            "SAVEPOINT siderite_sp_2",
            "RELEASE SAVEPOINT siderite_sp_2",
        ]
    );
    Ok(())
}

#[tokio::test]
async fn dedicated_schema_connection_cannot_commit_cancelled_child() {
    let (db, probe, receiver) = fixture(Some("BEGIN"), None);
    let result = db
        .schema_change(false, |connection| async move {
            let begin = |_| async { Ok::<_, OrmError>(()) };
            let child_future = connection.transaction(begin);
            let mut child = Box::pin(child_future);
            tokio::select! {
                reached = receiver => assert!(reached.is_ok()),
                result = &mut child => panic!("child: {result:?}"),
            }
            drop(child);
            Ok::<_, OrmError>(())
        })
        .await;
    assert!(matches!(
        result,
        Err(OrmError::Query(QueryError::TransactionAborted))
    ));
    assert_eq!(probe.commits.load(Ordering::SeqCst), 0);
    assert_eq!(probe.rollbacks.load(Ordering::SeqCst), 1);
}
