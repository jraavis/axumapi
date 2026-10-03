//! Owned process termination and advisory-lock deadlines on real databases.

mod common;

use common::scratch::ScratchDb;
use siderite_migrations::MigrationGraph as Graph;
use siderite_migrations::MigrationRegistry as Registry;
use siderite_migrations::{Migration, MigrationError, Migrator, Operation};
use siderite_orm::{BackendKind, Db, Value};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::sync::oneshot;
use uuid::Uuid;

type Result = std::result::Result<(), Box<dyn std::error::Error>>;

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn graph(atomic: bool) -> std::result::Result<Graph, MigrationError> {
    let before = "CREATE TABLE crash_before (id INTEGER PRIMARY KEY)";
    let after = "CREATE TABLE crash_after (id INTEGER PRIMARY KEY)";
    let migration = Migration::new(
        "0001_crash",
        vec![],
        vec![
            Operation::RunSQL {
                sql: before.into(),
                reverse_sql: Some("DROP TABLE crash_before".into()),
            },
            Operation::RunRust {
                name: "checkpoint".into(),
                backwards: None,
            },
            Operation::RunSQL {
                sql: after.into(),
                reverse_sql: Some("DROP TABLE crash_after".into()),
            },
        ],
        atomic,
        vec![],
    )?;
    Graph::build(vec![migration])
}

#[tokio::test]
#[ignore = "child helper: invoked only by owned crash tests"]
async fn recovery_child() -> Result {
    let url = std::env::var("SIDERITE_RECOVERY_CHILD_URL")?;
    let marker = std::env::var("SIDERITE_RECOVERY_CHILD_MARKER")?;
    let atomic = std::env::var("SIDERITE_RECOVERY_CHILD_ATOMIC")? == "true";
    let db = if url.starts_with("postgres") {
        Db::new(siderite_backends::postgres::PgBackend::connect(&url).await?)
    } else {
        Db::new(siderite_backends::mysql::MySqlBackend::connect(&url).await?)
    };
    let graph = graph(atomic)?;
    let mut registry = Registry::new();
    registry.register("checkpoint", move |db| {
        let marker = marker.clone();
        Box::pin(async move {
            db.raw_execute("INSERT INTO crash_before VALUES (1)", vec![])
                .await?;
            std::fs::write(marker, "ready")?;
            std::future::pending::<()>().await;
            Ok(())
        })
    });
    Migrator::new(&db, &graph)
        .with_registry(registry)
        .migrate(None, false)
        .await?;
    Err("child migration unexpectedly completed".into())
}

fn checkpoint(db: Db) -> siderite_migrations::registry::RustFuture {
    Box::pin(async move {
        let sql = match db.capabilities().kind {
            BackendKind::MySql => "INSERT IGNORE INTO crash_before VALUES (1)",
            _ => "INSERT INTO crash_before VALUES (1) ON CONFLICT DO NOTHING",
        };
        db.raw_execute(sql, vec![]).await?;
        Ok(())
    })
}

async fn kill_at_checkpoint(url: &str, atomic: bool, marker: &Path) -> Result {
    let mut child = OwnedChild(
        Command::new(std::env::current_exe()?)
            .args(["--ignored", "--exact", "recovery_child", "--nocapture"])
            .env("SIDERITE_RECOVERY_CHILD_URL", url)
            .env("SIDERITE_RECOVERY_CHILD_MARKER", marker)
            .env("SIDERITE_RECOVERY_CHILD_ATOMIC", atomic.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    tokio::time::timeout(Duration::from_secs(20), async {
        while !marker.exists() {
            if child.0.try_wait()?.is_some() {
                return Err("child exited before checkpoint".into());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    })
    .await??;
    child.0.kill()?;
    child.0.wait()?;
    Ok(())
}

async fn crash_contract(scratch: ScratchDb, atomic: bool) -> Result {
    let token = Uuid::new_v4().simple();
    let name = format!("siderite-recovery-{token}");
    let marker = std::env::temp_dir().join(name);
    let result = async {
        let graph = graph(atomic)?;
        kill_at_checkpoint(scratch.connection_url(), atomic, &marker).await?;
        let db = &scratch.db;
        let migrator = Migrator::new(db, &graph);
        let report = migrator.inspect_recovery().await?;
        assert!(report.applied.is_empty());
        if atomic && db.capabilities().kind == BackendKind::Postgres {
            assert!(report.partial.is_empty());
            assert!(report.uncertain_steps.is_empty());
            assert!(
                db.raw_sql("SELECT * FROM crash_before", vec![])
                    .await
                    .is_err()
            );
        } else {
            assert_eq!(report.partial.len(), 1);
            assert_eq!(report.uncertain_steps.len(), 1);
            let entry = &report.partial[0];
            assert_eq!(entry.file_matches, Some(true));
            assert_eq!(entry.completed_operations, 1);
            assert_eq!(entry.completed_statements, 0);
            let data = db.raw_sql("SELECT * FROM crash_before", vec![]).await?;
            assert_eq!(data.rows.len(), 1);
        }
        let mut registry = Registry::new();
        registry.register("checkpoint", checkpoint);
        if !report.uncertain_steps.is_empty() {
            let blocked = Migrator::new(db, &graph)
                .with_registry(registry.clone())
                .migrate(None, false)
                .await;
            assert!(matches!(
                blocked,
                Err(MigrationError::UncertainRustStep { .. })
            ));
            // This replacement is idempotent for the inspected fixture row.
            registry.register_replay_safe("checkpoint", checkpoint);
        }
        let report = Migrator::new(db, &graph)
            .with_registry(registry)
            .migrate(None, false)
            .await?;
        assert_eq!(report.applied, vec!["0001_crash"]);
        db.raw_sql("SELECT * FROM crash_after", vec![]).await?;
        let data = db.raw_sql("SELECT * FROM crash_before", vec![]).await?;
        assert_eq!(data.rows.len(), 1);
        assert!(
            Migrator::new(db, &graph)
                .inspect_recovery()
                .await?
                .partial
                .is_empty()
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = std::fs::remove_file(marker);
    let cleanup = scratch.cleanup().await;
    result?;
    cleanup?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires PostgreSQL with private database creation rights"]
async fn postgres_crash_atomic_rollback() -> Result {
    let scratch = ScratchDb::postgres().await?.ok_or("missing scratch")?;
    crash_contract(scratch, true).await
}

#[tokio::test]
#[ignore = "requires PostgreSQL with private database creation rights"]
async fn postgres_crash_nonatomic_resume() -> Result {
    let scratch = ScratchDb::postgres().await?.ok_or("missing scratch")?;
    crash_contract(scratch, false).await
}

#[tokio::test]
#[ignore = "requires MySQL with private database creation rights"]
async fn mysql_crash_resume() -> Result {
    let scratch = ScratchDb::mysql().await?.ok_or("missing scratch")?;
    crash_contract(scratch, true).await
}

async fn deadline_contract(scratch: ScratchDb) -> Result {
    let db = scratch.db.clone();
    let kind = db.capabilities().kind;
    let (started, ready) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let holder = tokio::spawn(async move {
        db.schema_change(false, |db| async move {
            let (lock, unlock, params) = match kind {
                BackendKind::Postgres => (
                    "SELECT pg_advisory_lock($1)",
                    "SELECT pg_advisory_unlock($1)",
                    vec![Value::Int(0x5349_4445)],
                ),
                _ => (
                    "SELECT GET_LOCK(?, 0)",
                    "SELECT RELEASE_LOCK(?)",
                    vec![Value::Text("siderite_migrate".into())],
                ),
            };
            // Execute avoids decoding PostgreSQL's void return type.
            db.raw_execute(lock, params.clone()).await?;
            let _ = started.send(());
            let _ = released.await;
            db.raw_execute(unlock, params).await?;
            Ok::<_, MigrationError>(())
        })
        .await
    });
    let result = async {
        tokio::time::timeout(Duration::from_secs(5), ready).await??;
        let graph = Graph::build(vec![])?;
        let deadline = Duration::from_millis(100);
        let db = &scratch.db;
        let migrator = Migrator::new(db, &graph).with_lock_timeout(deadline)?;
        let attempt = migrator.migrate(None, false);
        let budget = Duration::from_secs(2);
        let attempted = tokio::time::timeout(budget, attempt).await?;
        assert!(matches!(attempted, Err(MigrationError::LockTimeout)));
        assert!(migrator.inspect_recovery().await?.applied.is_empty());
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = release.send(());
    let held = holder.await?;
    let graph = Graph::build(vec![])?;
    let retry = Migrator::new(&scratch.db, &graph)
        .migrate(None, false)
        .await;
    let cleanup = scratch.cleanup().await;
    result?;
    held?;
    retry?;
    cleanup?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires PostgreSQL with private database creation rights"]
async fn postgres_lock_deadline_releases_session() -> Result {
    let scratch = ScratchDb::postgres().await?.ok_or("missing scratch")?;
    deadline_contract(scratch).await
}

#[tokio::test]
#[ignore = "requires MySQL with private database creation rights"]
async fn mysql_lock_deadline_releases_session() -> Result {
    let scratch = ScratchDb::mysql().await?.ok_or("missing scratch")?;
    deadline_contract(scratch).await
}

async fn caller_transaction_contract(scratch: ScratchDb) -> Result {
    let result = async {
        let db = &scratch.db;
        db.execute_script("CREATE TABLE caller_data (id INTEGER)")
            .await?;
        let graph = Graph::build(vec![])?;
        let attempted = db
            .transaction::<_, _, (), MigrationError>(|tx| async move {
                tx.raw_execute("INSERT INTO caller_data VALUES (1)", vec![])
                    .await?;
                let migration = Migrator::new(&tx, &graph);
                assert!(migration.migrate(None, false).await.is_err());
                assert!(migration.rollback(None, Some(1), false).await.is_err());
                assert!(migration.inspect_recovery().await?.applied.is_empty());
                Err(MigrationError::state("caller requests rollback"))
            })
            .await;
        assert!(attempted.is_err());
        let rows = db.raw_sql("SELECT id FROM caller_data", vec![]).await?;
        assert!(rows.rows.is_empty());
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    let cleanup = scratch.cleanup().await;
    result?;
    cleanup?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires MySQL with private database creation rights"]
async fn mysql_migrator_cannot_commit_caller_transaction() -> Result {
    let scratch = ScratchDb::mysql().await?.ok_or("missing scratch")?;
    caller_transaction_contract(scratch).await
}

#[tokio::test]
#[ignore = "requires PostgreSQL with private database creation rights"]
async fn postgres_migrator_cannot_consume_caller_transaction() -> Result {
    let scratch = ScratchDb::postgres().await?.ok_or("missing scratch")?;
    caller_transaction_contract(scratch).await
}
