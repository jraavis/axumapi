//! Live native-driver prerequisites for a future production adapter.

use crate::ProbeResult;
use crate::driver::{Driver, Kind, TITLE};
use crate::fixture::Fixture;
use mysql_async::TxOpts;
use mysql_async::prelude::Queryable;
use std::time::Duration;

#[tokio::test]
#[ignore = "requires MYSQL_PROBE_URL with CREATE DATABASE permission"]
async fn native_reuse_contracts() -> ProbeResult<()> {
    let url = crate::required_url()?;
    let mut fixture = Fixture::create(&url).await?;
    let result = check_reuse(&url, &mut fixture).await;
    let cleanup = fixture.destroy().await;
    result?;
    cleanup
}

async fn check_reuse(url: &str, fixture: &mut Fixture) -> ProbeResult<()> {
    for kind in [Kind::NativeReset, Kind::NativeRetain] {
        let driver = Driver::open(kind, url, &fixture.name, 1).await?;
        let Driver::Native(pool) = &driver else {
            unreachable!("native driver requested");
        };
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let mut conn = pool.get_conn().await?;
            conn.query_drop("SET @siderite_probe = 42").await?;
            drop(conn);
            let mut conn = pool.get_conn().await?;
            let state: Option<(Option<u64>, String, u64, u64)> = conn
                .query_first(
                    "SELECT @siderite_probe, @@time_zone,
                     @@group_concat_max_len, @@autocommit",
                )
                .await?;
            let state =
                state.ok_or_else(|| crate::invalid("missing session"))?;
            let expected = match kind {
                Kind::NativeReset => None,
                Kind::NativeRetain => Some(42),
                Kind::Sqlx => unreachable!("native driver requested"),
            };
            assert_eq!(state, (expected, "+00:00".into(), 4294967295, 1));

            // Typed transaction drop must finish rollback before reuse.
            let mut tx = conn.start_transaction(TxOpts::default()).await?;
            tx.exec_drop(
                "INSERT INTO todos (title, done) VALUES (?, ?)",
                (TITLE, false),
            )
            .await?;
            drop(tx);
            drop(conn);
            let mut conn = pool.get_conn().await?;
            let count: Option<u64> =
                conn.query_first("SELECT COUNT(*) FROM todos").await?;
            assert_eq!(count, Some(0));

            // Cancel a protocol operation, then verify clean one-slot reuse.
            let cancelled = tokio::time::timeout(
                Duration::from_millis(5),
                conn.query_drop("SELECT SLEEP(0.05)"),
            )
            .await;
            assert!(cancelled.is_err());
            drop(conn);
            let mut conn = pool.get_conn().await?;
            let value: Option<u64> = conn.query_first("SELECT 7").await?;
            assert_eq!(value, Some(7));

            // A cancelled checkout cannot consume pool capacity permanently.
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(5),
                    pool.get_conn(),
                )
                .await
                .is_err()
            );
            drop(conn);
            let mut conn = pool.get_conn().await?;
            let value: Option<u64> = conn.query_first("SELECT 8").await?;
            assert_eq!(value, Some(8));
            Ok::<_, crate::ProbeError>(())
        })
        .await;
        let closed = driver.close().await;
        result??;
        closed?;
        fixture.verify(&[]).await?;
    }
    // Exercise queueing, uneven worker counts, schema-preserving reset and
    // independent ID/row verification for all three comparison modes.
    let config = crate::Config {
        url: url.into(),
        requests: 33,
        concurrency: 5,
        pool: 3,
        pairs: 1,
        timeout: 5,
        held: false,
    };
    for kind in [Kind::Sqlx, Kind::NativeReset, Kind::NativeRetain] {
        crate::trial::run(kind, 0, &config, fixture).await?;
    }
    Ok(())
}
