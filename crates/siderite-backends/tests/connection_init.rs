//! Explicit hooks and mandatory settings on fresh/replaced live sessions.
#![cfg(any(feature = "postgres", feature = "mysql"))]

use siderite_backends::connection_init::ConnectionInit;
use siderite_orm::{Executor, QueryResult, Value};
use std::io::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn rejected_init<DB: sqlx::Database>() -> ConnectionInit<DB> {
    Arc::new(|_, _| Box::pin(async { Err(sqlx::Error::Protocol("init fault".into())) }))
}

fn check(rows: &QueryResult, timezone: &str) -> Result<i64, Error> {
    let row = rows
        .rows
        .first()
        .ok_or_else(|| Error::other("missing row"))?;
    assert_eq!(row.get("tz"), Some(&Value::Text(timezone.into())));
    assert_eq!(
        row.get("marker"),
        Some(&Value::Text("siderite-init".into()))
    );
    match row.get("id") {
        Some(Value::Int(id)) => Ok(*id),
        _ => Err(Error::other("missing connection identity")),
    }
}

#[cfg(feature = "postgres")]
#[tokio::test]
#[ignore = "requires DATABASE_URL with own-session termination permission"]
async fn postgres_initialization_reconnect_and_failure() -> TestResult {
    use siderite_backends::postgres::PgBackend;
    use sqlx::postgres::PgPoolOptions;
    let url = std::env::var("DATABASE_URL")?;
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let hook: ConnectionInit<sqlx::Postgres> = Arc::new(move |conn, _| {
        let calls = observed.clone();
        Box::pin(async move {
            let name = "SET application_name = 'siderite-init'";
            sqlx::Executor::execute(&mut *conn, name).await?;
            let zone = "SET TIME ZONE 'Pacific/Honolulu'";
            sqlx::Executor::execute(conn, zone).await?;
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    });
    let backend = PgBackend::connect_with_init(
        &url,
        PgPoolOptions::new().max_connections(2).min_connections(2),
        hook,
    )
    .await?;
    const QUERY: &str = "WITH wait AS (SELECT pg_sleep(0.02)) \
        SELECT current_setting('TimeZone') AS tz, \
        current_setting('application_name') AS marker, \
        pg_backend_pid()::bigint AS id FROM wait";
    let (first, second) = tokio::join!(
        backend.fetch_raw(QUERY, vec![]),
        backend.fetch_raw(QUERY, vec![])
    );
    let first = check(&first?, "UTC")?;
    let second = check(&second?, "UTC")?;
    assert_ne!(first, second);
    assert!(calls.load(Ordering::SeqCst) >= 2);
    let admin = sqlx::PgPool::connect(&url).await?;
    sqlx::query("SELECT pg_terminate_backend($1)")
        .bind(first as i32)
        .execute(&admin)
        .await?;
    let (first, second) = tokio::join!(
        backend.fetch_raw(QUERY, vec![]),
        backend.fetch_raw(QUERY, vec![])
    );
    check(&first?, "UTC")?;
    check(&second?, "UTC")?;
    assert!(calls.load(Ordering::SeqCst) >= 3);
    drop(backend);
    admin.close().await;
    let hook = rejected_init::<sqlx::Postgres>();
    let result = PgBackend::connect_with_init(
        &url,
        PgPoolOptions::new().acquire_timeout(Duration::from_millis(150)),
        hook,
    )
    .await;
    assert!(result.is_err());
    Ok(())
}

#[cfg(feature = "mysql")]
#[tokio::test]
#[ignore = "requires MYSQL_URL with own-session termination permission"]
async fn mysql_initialization_reconnect_and_failure() -> TestResult {
    use siderite_backends::mysql::MySqlBackend;
    use sqlx::mysql::MySqlPoolOptions;
    let url = std::env::var("MYSQL_URL")?;
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let hook: ConnectionInit<sqlx::MySql> = Arc::new(move |conn, _| {
        let calls = observed.clone();
        Box::pin(async move {
            for statement in [
                "SET @siderite_init_marker = 'siderite-init'",
                "SET time_zone = '-10:00'",
                "SET SESSION group_concat_max_len = 7",
                "SET SESSION sql_mode = 'NO_BACKSLASH_ESCAPES'",
            ] {
                sqlx::Executor::execute(&mut *conn, statement).await?;
            }
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    });
    let backend = MySqlBackend::connect_with_init(
        &url,
        MySqlPoolOptions::new()
            .max_connections(2)
            .min_connections(2),
        hook,
    )
    .await?;
    const QUERY: &str = "SELECT @@time_zone AS tz, \
        @siderite_init_marker AS marker, CONNECTION_ID() AS id, \
        @@group_concat_max_len AS concat_limit, @@sql_mode AS mode, \
        SLEEP(0.02) AS waited";
    let validate = |result: QueryResult| -> Result<i64, Error> {
        let id = check(&result, "+00:00")?;
        let row = result.rows.first().ok_or_else(|| Error::other("no row"))?;
        assert_eq!(row.get("concat_limit"), Some(&Value::Int(4_294_967_295)));
        let Some(Value::Text(mode)) = row.get("mode") else {
            return Err(Error::other("missing sql mode"));
        };
        assert!(!mode.contains("NO_BACKSLASH_ESCAPES"));
        Ok(id)
    };
    let (first, second) = tokio::join!(
        backend.fetch_raw(QUERY, vec![]),
        backend.fetch_raw(QUERY, vec![])
    );
    let first = validate(first?)?;
    let second = validate(second?)?;
    assert_ne!(first, second);
    assert!(calls.load(Ordering::SeqCst) >= 2);
    let admin = sqlx::MySqlPool::connect(&url).await?;
    sqlx::query(&format!("KILL CONNECTION {first}"))
        .execute(&admin)
        .await?;
    let (first, second) = tokio::join!(
        backend.fetch_raw(QUERY, vec![]),
        backend.fetch_raw(QUERY, vec![])
    );
    validate(first?)?;
    validate(second?)?;
    assert!(calls.load(Ordering::SeqCst) >= 3);
    drop(backend);
    admin.close().await;
    let hook = rejected_init::<sqlx::MySql>();
    let result = MySqlBackend::connect_with_init(
        &url,
        MySqlPoolOptions::new().acquire_timeout(Duration::from_millis(150)),
        hook,
    )
    .await;
    assert!(result.is_err());
    Ok(())
}
