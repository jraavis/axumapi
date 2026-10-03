//! Verified transport against disposable TLS-only fixture services.
#![cfg(all(
    feature = "tls",
    feature = "postgres",
    feature = "mysql-native",
    feature = "redis",
))]

use siderite_backends::mysql::MySqlBackend;
use siderite_backends::mysql::native::{NativeMySqlBackend, NativeMySqlOptions};
use siderite_backends::postgres::PgBackend;
use siderite_backends::redis::RedisStore;
use siderite_orm::{Executor, Value};
use std::time::Duration;

type NativeResult = Result<mysql_async::Opts, mysql_async::UrlError>;
type RedisClient = redis::RedisResult<redis::Client>;
type TestResult = Result<(), Box<dyn std::error::Error>>;

#[cfg(feature = "mongodb")]
#[tokio::test]
#[ignore = "run with scripts/test_tls.py disposable verified services"]
async fn tls_certificate_contracts_mongodb() -> TestResult {
    use siderite_backends::mongodb::MongoBackend;

    let url = std::env::var("SIDERITE_TLS_MONGO")?;
    let ca = std::env::var("SIDERITE_TLS_CA")?;
    let wrong = std::env::var("SIDERITE_TLS_WRONG_CA")?;
    let backend = MongoBackend::connect(&url, "tls_contract").await?;
    // The owned server requires TLS, so a successful ping proves encryption.
    backend
        .raw_command(mongodb::bson::doc! { "ping": 1 })
        .await?;
    for invalid in [
        url.replace("localhost", "127.0.0.1"),
        url.replace(&ca, &wrong),
    ] {
        assert!(
            MongoBackend::connect(&invalid, "tls_contract")
                .await
                .is_err()
        );
    }
    Ok(())
}

#[tokio::test]
#[ignore = "run with scripts/test_tls.py disposable verified services"]
async fn tls_certificate_contracts() -> TestResult {
    let ca_path = std::env::var("SIDERITE_TLS_CA")?;
    let wrong_path = std::env::var("SIDERITE_TLS_WRONG_CA")?;
    let ca = std::fs::read(&ca_path)?;
    let wrong = std::fs::read(&wrong_path)?;
    let pg_url = std::env::var("SIDERITE_TLS_PG")?;
    let mysql_url = std::env::var("SIDERITE_TLS_MYSQL")?;
    let native_url = std::env::var("SIDERITE_TLS_NATIVE")?;
    let redis_url = std::env::var("SIDERITE_TLS_REDIS")?;
    let pool = || {
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(2))
    };
    let pg = PgBackend::connect_with(&pg_url, pool()).await?;
    let rows = pg
        .fetch_raw(
            "SELECT ssl FROM pg_stat_ssl WHERE pid=pg_backend_pid()",
            vec![],
        )
        .await?;
    assert_eq!(
        rows.rows.first().and_then(|r| r.get("ssl")),
        Some(&Value::Bool(true))
    );
    for url in [
        pg_url.replace("localhost", "127.0.0.1"),
        pg_url.replace(&ca_path, &wrong_path),
    ] {
        assert!(PgBackend::connect_with(&url, pool()).await.is_err());
    }
    drop(pg);
    let pool = || {
        sqlx::mysql::MySqlPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(2))
    };
    let mysql = MySqlBackend::connect_with(&mysql_url, pool()).await?;
    assert_cipher(&mysql).await?;
    for url in [
        mysql_url.replace("localhost", "127.0.0.1"),
        mysql_url.replace(&ca_path, &wrong_path),
    ] {
        assert!(MySqlBackend::connect_with(&url, pool()).await.is_err());
    }
    drop(mysql);
    let limits = || NativeMySqlOptions {
        max_connections: 1,
        max_waiters: 0,
        acquire_timeout: Duration::from_secs(2),
    };
    let options = native_options(&native_url, ca.clone())?;
    let native = NativeMySqlBackend::connect_options(options, limits()).await?;
    assert_cipher(&native).await?;
    native.close().await?;
    for (url, root) in [
        (native_url.replace("localhost", "127.0.0.1"), ca.clone()),
        (native_url, wrong.clone()),
    ] {
        let options = native_options(&url, root)?;
        let future = NativeMySqlBackend::connect_options(options, limits());
        let result = future.await;
        assert!(result.is_err());
    }
    let client = redis_client(&redis_url, ca.clone())?;
    let store = RedisStore::connect_with(client, redis_limits()).await?;
    let store = store.with_prefix("tls-contract:");
    assert!(store.set_nx("verified", "yes").await?);
    assert_eq!(store.get("verified").await?.as_deref(), Some("yes"));
    store.del("verified").await?;
    drop(store);
    for (url, root) in [
        (redis_url.replace("localhost", "127.0.0.1"), ca),
        (redis_url, wrong),
    ] {
        let client = redis_client(&url, root)?;
        assert!(
            RedisStore::connect_with(client, redis_limits())
                .await
                .is_err()
        );
    }
    Ok(())
}

fn native_options(url: &str, root: Vec<u8>) -> NativeResult {
    let tls = mysql_async::SslOpts::default()
        .with_root_certs(vec![root.into()])
        .with_disable_built_in_roots(true);
    let options = mysql_async::Opts::from_url(url)?;
    Ok(mysql_async::OptsBuilder::from_opts(options)
        .ssl_opts(tls)
        .into())
}

fn redis_client(url: &str, root: Vec<u8>) -> RedisClient {
    redis::Client::build_with_tls(
        url,
        redis::TlsCertificates {
            client_tls: None,
            root_cert: Some(root),
        },
    )
}

fn redis_limits() -> redis::aio::ConnectionManagerConfig {
    redis::aio::ConnectionManagerConfig::new()
        .set_number_of_retries(0)
        .set_connection_timeout(Some(Duration::from_secs(2)))
        .set_response_timeout(Some(Duration::from_secs(2)))
}

async fn assert_cipher(backend: &impl Executor) -> TestResult {
    let rows = backend
        .fetch_raw("SHOW STATUS LIKE 'Ssl_cipher'", vec![])
        .await?;
    let cipher = rows.rows.first().and_then(|r| r.get("Value"));
    assert!(matches!(cipher, Some(Value::Text(value)) if !value.is_empty()));
    Ok(())
}
