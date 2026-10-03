//! Committed explicit scripts must not replay after an owned process kill.

mod common;

use common::scratch::ScratchDb;
use siderite_migrations::{Migration, MigrationError, MigrationGraph};
use siderite_migrations::{Migrator, Operation};
use siderite_orm::{BackendKind, Db};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

type Result = std::result::Result<(), Box<dyn std::error::Error>>;
type GraphResult = std::result::Result<MigrationGraph, MigrationError>;

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn graph(kind: BackendKind) -> GraphResult {
    // PostgreSQL simple-query batches implicitly transact unless COMMIT
    // explicitly separates the durable effect from the unfinished command.
    let sql = match kind {
        BackendKind::Postgres => {
            "BEGIN; INSERT INTO effects VALUES (1); COMMIT; \
             SELECT pg_sleep(3)"
        }
        _ => "INSERT INTO effects VALUES (1); SELECT SLEEP(3)",
    };
    let migration = Migration::new(
        "0001_script",
        vec![],
        vec![Operation::RunSQL {
            sql: sql.into(),
            reverse_sql: None,
        }],
        false,
        vec![],
    )?;
    MigrationGraph::build(vec![migration])
}

#[tokio::test]
#[ignore = "child helper: invoked only by owned SQL crash tests"]
async fn sql_recovery_child() -> Result {
    let url = std::env::var("SIDERITE_SQL_RECOVERY_URL")?;
    let db = if url.starts_with("postgres") {
        Db::new(siderite_backends::postgres::PgBackend::connect(&url).await?)
    } else {
        Db::new(siderite_backends::mysql::MySqlBackend::connect(&url).await?)
    };
    let graph = graph(db.capabilities().kind)?;
    Migrator::new(&db, &graph).migrate(None, false).await?;
    Err("child unexpectedly completed before termination".into())
}

async fn crash_contract(scratch: ScratchDb) -> Result {
    let result = async {
        let db = &scratch.db;
        db.execute_script("CREATE TABLE effects (id INTEGER)")
            .await?;
        let mut child = OwnedChild(
            Command::new(std::env::current_exe()?)
                .args(["--ignored", "--exact", "sql_recovery_child"])
                .env("SIDERITE_SQL_RECOVERY_URL", scratch.connection_url())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?,
        );
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if child.0.try_wait()?.is_some() {
                    return Err("child exited before committed row".into());
                }
                let rows = db.raw_sql("SELECT id FROM effects", vec![]).await?;
                if rows.rows.len() == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok::<_, Box<dyn std::error::Error>>(())
        })
        .await??;
        child.0.kill()?;
        child.0.wait()?;
        let graph = graph(db.capabilities().kind)?;
        let migrator = Migrator::new(db, &graph);
        let report = migrator.inspect_recovery().await?;
        if report.uncertain_steps.len() != 1
            || report.uncertain_steps[0].callback.is_some()
            || !report.applied.is_empty()
        {
            return Err("missing SQL uncertainty after committed data".into());
        }
        // The server may finish its short sleep before observing disconnect.
        // Bound waiting for its advisory lock; never kill a shared session.
        let retry = migrator
            .with_lock_timeout(Duration::from_secs(5))?
            .migrate(None, false)
            .await;
        if !matches!(retry, Err(MigrationError::UncertainSqlStep { .. })) {
            return Err("uncertain SQL retry was not rejected".into());
        }
        let rows = db.raw_sql("SELECT id FROM effects", vec![]).await?;
        if rows.rows.len() != 1 {
            return Err("uncertain SQL replay duplicated data".into());
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    let cleanup = scratch.cleanup().await;
    result?;
    cleanup?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires PostgreSQL with private database creation rights"]
async fn postgres_committed_script_crash_blocks_replay() -> Result {
    let scratch = ScratchDb::postgres().await?.ok_or("missing scratch")?;
    crash_contract(scratch).await
}

#[tokio::test]
#[ignore = "requires MySQL with private database creation rights"]
async fn mysql_committed_script_crash_blocks_replay() -> Result {
    let scratch = ScratchDb::mysql().await?.ok_or("missing scratch")?;
    crash_contract(scratch).await
}
