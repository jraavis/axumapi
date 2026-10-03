//! One-slot live ownership gates; missing service configuration is an error.

use super::*;
use futures_util::FutureExt;
use sqlx::mysql::MySqlPoolOptions;
use std::io::Error;
use std::panic::AssertUnwindSafe;

type Check<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[tokio::test]
#[ignore = "requires MYSQL_URL with CREATE DATABASE permission"]
async fn native_session_ownership_contract() -> Check {
    let url = std::env::var("MYSQL_URL")?;
    let admin = MySqlPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await?;
    let name = format!("siderite_native_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&admin)
        .await?;
    let (server, _) = url
        .rsplit_once('/')
        .ok_or_else(|| Error::other("database URL needs a database"))?;
    let url = format!("{server}/{name}");
    let options = NativeMySqlOptions {
        max_connections: 1,
        max_waiters: 1,
        acquire_timeout: Duration::from_millis(500),
    };
    let backend = NativeMySqlBackend::connect_with(&url, options).await?;
    let exercise = AssertUnwindSafe(exercise(&backend, &admin)).catch_unwind();
    let result = tokio::time::timeout(Duration::from_secs(20), exercise).await;
    let closed = backend.close().await;
    let removed = sqlx::query(&format!("DROP DATABASE {name}"))
        .execute(&admin)
        .await;
    admin.close().await;
    match result? {
        Ok(result) => result?,
        Err(panic) => std::panic::resume_unwind(panic),
    }
    closed?;
    removed?;
    Ok(())
}

type Admin = sqlx::MySqlPool;

async fn exercise(backend: &NativeMySqlBackend, admin: &Admin) -> Check {
    let schema = "CREATE TABLE writes (id BIGINT PRIMARY KEY, value INT)";
    backend.execute_script(schema).await?;

    let first = backend.checkout().await?;
    let original = first.id()?;
    drop(first);
    let mut second = backend.checkout().await?;
    assert_eq!(second.id()?, original, "clean lease must be retained");
    second.fetch("SELECT 7 AS value", vec![]).await?;
    drop(second);
    let third = backend.checkout().await?;
    assert_eq!(third.id()?, original);
    drop(third);

    backend
        .execute_script(
            "SET @siderite_session = 42; SET time_zone = '+02:00';
         SET autocommit = 0; SET sql_mode = 'NO_BACKSLASH_ESCAPES'",
        )
        .await?;
    let mut clean = backend.checkout().await?;
    assert_ne!(clean.id()?, original, "raw session must be retired");
    let rows = clean
        .fetch(
            "SELECT @siderite_session AS marker, @@time_zone AS zone,
         @@autocommit AS auto, @@sql_mode AS mode",
            vec![],
        )
        .await?
        .rows;
    assert_eq!(rows[0].get("marker"), Some(&Value::Null));
    assert_eq!(rows[0].get("zone"), Some(&Value::Text("+00:00".into())));
    assert_eq!(rows[0].get("auto"), Some(&Value::Int(1)));
    let mode = rows[0].get_as::<String>("mode")?;
    assert!(!mode.contains("NO_BACKSLASH_ESCAPES"));
    drop(clean);

    // A cancelled wait releases admission capacity while the one slot is
    // held. A second waiter is rejected instead of growing an unbounded queue.
    let held = backend.checkout().await?;
    let mut wait = Box::pin(backend.checkout());
    tokio::select! {
        value = &mut wait => {
            value?;
            return Err(Error::other("checkout bypassed the held slot").into());
        }
        _ = tokio::time::sleep(Duration::from_millis(10)) => {}
    }
    assert!(
        backend.checkout().await.is_err(),
        "admission must be bounded"
    );
    drop(wait);
    drop(held);
    drop(backend.checkout().await?);

    let mut checked = backend.checkout().await?;
    let id = checked.id()?;
    assert!(checked.fetch("SELEC invalid", vec![]).await.is_err());
    assert!(
        checked
            .fetch("SELECT CAST('-00:00:01' AS TIME) AS invalid", vec![],)
            .await
            .is_err()
    );
    checked.fetch("SELECT 9 AS value", vec![]).await?;
    assert_eq!(checked.id()?, id, "drained errors must allow rollback");
    drop(checked);

    backend
        .execute_script(
            "CREATE PROCEDURE late_error() BEGIN SELECT 1 AS value;
         SIGNAL SQLSTATE '45000' SET MESSAGE_TEXT = 'native late error'; END",
        )
        .await?;
    let mut checked = backend.checkout().await?;
    let id = checked.id()?;
    assert!(checked.fetch("CALL late_error()", vec![]).await.is_err());
    checked.fetch("SELECT 10 AS value", vec![]).await?;
    assert_eq!(checked.id()?, id, "late errors must drain the protocol");
    drop(checked);

    let tx = backend.begin(None).await?;
    tx.execute_raw("INSERT INTO writes VALUES (1, 10)", vec![])
        .await?;
    drop(tx);
    assert_empty(backend).await?;

    let tx = backend.begin(None).await?;
    tx.execute_raw("INSERT INTO writes VALUES (2, 20)", vec![])
        .await?;
    let cancelled = tokio::time::timeout(
        Duration::from_millis(10),
        tx.fetch_raw("SELECT SLEEP(0.2)", vec![]),
    )
    .await;
    assert!(cancelled.is_err());
    // The escaped transaction remains alive, but its cancelled exchange
    // already retired the connection and released the admission permit.
    assert_empty(backend).await?;
    assert!(matches!(
        tx.commit().await,
        Err(OrmError::Query(QueryError::TransactionAborted))
    ));
    drop(tx);

    let mut killed = backend.checkout().await?;
    let id = killed.id()?;
    sqlx::query(&format!("KILL CONNECTION {id}"))
        .execute(admin)
        .await?;
    assert!(killed.fetch("SELECT 1 AS value", vec![]).await.is_err());
    assert!(killed.fetch("SELECT 2 AS value", vec![]).await.is_err());
    drop(killed);
    let next = backend.checkout().await?;
    assert_ne!(next.id()?, id);
    drop(next);

    let lock = format!("siderite_native_lock_{}", uuid::Uuid::new_v4());
    let schema = backend.begin_schema(false).await?;
    let rows = schema
        .fetch_raw(
            "SELECT GET_LOCK(?, 0) AS taken",
            vec![Value::Text(lock.clone())],
        )
        .await?;
    assert_eq!(rows.rows[0].get("taken"), Some(&Value::Int(1)));
    schema.commit().await?;
    drop(schema);
    let mut free = 0_i64;
    for _ in 0..100 {
        free = sqlx::query_scalar("SELECT IS_FREE_LOCK(?)")
            .bind(&lock)
            .fetch_one(admin)
            .await?;
        if free == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(free, 1, "schema completion must release session locks");
    Ok(())
}

async fn assert_empty(backend: &NativeMySqlBackend) -> Check {
    let rows = backend
        .fetch_raw("SELECT COUNT(*) AS n FROM writes", vec![])
        .await?;
    assert_eq!(rows.rows[0].get("n"), Some(&Value::Int(0)));
    Ok(())
}
