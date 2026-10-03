//! Locks helpers for the migration executor.

use super::{BackendKind, Db, MigrationError as Error, Value};
use std::time::Duration;

type LockResult = Result<(), Error>;

/// PostgreSQL `pg_advisory_lock` key (`SIDE` in ASCII).
pub(super) const PG_MIGRATE_LOCK: i64 = 0x5349_4445;
pub(super) const MYSQL_LOCK_NAME: &str = "siderite_migrate";
/// Try without a server-side wait; cancellation cannot leave a pending
/// blocking lock request behind on a reusable connection.
pub(super) async fn acquire(db: &Db, timeout: Duration) -> LockResult {
    let kind = db.capabilities().kind;
    if kind == BackendKind::Sqlite {
        return Ok(());
    }
    tokio::time::timeout(timeout, async {
        loop {
            let result = match kind {
                BackendKind::Postgres => {
                    db.raw_sql(
                        "SELECT pg_try_advisory_lock($1) AS acquired",
                        vec![Value::Int(PG_MIGRATE_LOCK)],
                    )
                    .await?
                }
                BackendKind::MySql => {
                    db.raw_sql(
                        "SELECT GET_LOCK(?, 0) AS acquired",
                        vec![Value::Text(MYSQL_LOCK_NAME.into())],
                    )
                    .await?
                }
                other => return Err(Error::UnsupportedBackend(other)),
            };
            match result.rows.first().and_then(|row| row.get("acquired")) {
                Some(Value::Bool(true)) | Some(Value::Int(1)) => return Ok(()),
                Some(Value::Bool(false)) | Some(Value::Int(0)) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                other => {
                    return Err(Error::state(format!(
                        "migration lock attempt returned {other:?}"
                    )));
                }
            }
        }
    })
    .await
    .map_err(|_| Error::LockTimeout)?
}

pub(super) async fn release_migrate_lock(db: &Db) -> LockResult {
    match db.capabilities().kind {
        BackendKind::Sqlite => Ok(()),
        BackendKind::Postgres => {
            // `pg_advisory_unlock` returns `bool`, which decodes fine.
            let result = db
                .raw_sql(
                    "SELECT pg_advisory_unlock($1) AS released",
                    vec![Value::Int(PG_MIGRATE_LOCK)],
                )
                .await?;
            match result.rows.first().and_then(|row| row.get("released")) {
                Some(Value::Bool(true)) | Some(Value::Int(1)) => Ok(()),
                // False means the lock was not held by this session, which
                // only happens if the connection was replaced mid-run.
                other => Err(Error::state(format!(
                    "the migration lock was not held on release: {other:?}"
                ))),
            }
        }
        BackendKind::MySql => {
            let result = db
                .raw_sql(
                    "SELECT RELEASE_LOCK(?) AS released",
                    vec![Value::Text(MYSQL_LOCK_NAME.into())],
                )
                .await?;
            match result.rows.first().and_then(|row| row.get("released")) {
                Some(Value::Int(1)) => Ok(()),
                other => Err(Error::state(format!(
                    "migration lock was not held on release: {other:?}"
                ))),
            }
        }
        other => Err(Error::UnsupportedBackend(other)),
    }
}
