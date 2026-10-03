//! Comparable prepared inserts on SQLx and native MySQL connections.

use crate::{ProbeResult, invalid};
use mysql_async::prelude::Queryable;
use mysql_async::{Opts, OptsBuilder, Pool, PoolConstraints, PoolOpts};
use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions};
use sqlx::{Executor as _, MySql, MySqlPool, pool::PoolConnection};
use std::str::FromStr;
use std::sync::atomic::Ordering;
use std::time::Duration;

pub(crate) const TITLE: &str = "native driver comparison";
const INSERT: &str = "INSERT INTO todos (title, done) VALUES (?, ?)";
const SETUP: [&str; 4] = [
    "SET time_zone = '+00:00'",
    "SET SESSION sql_mode = REPLACE(@@sql_mode, 'NO_BACKSLASH_ESCAPES', '')",
    "SET SESSION group_concat_max_len = 4294967295",
    "SET autocommit = 1",
];

#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Sqlx,
    NativeReset,
    NativeRetain,
}

#[derive(Clone)]
pub(crate) enum Driver {
    Sqlx(MySqlPool),
    Native(Pool),
}

pub(crate) enum Connection {
    Sqlx(PoolConnection<MySql>),
    Native(mysql_async::Conn),
}

impl Driver {
    pub(crate) async fn open(
        kind: Kind,
        url: &str,
        database: &str,
        size: usize,
    ) -> ProbeResult<Self> {
        match kind {
            Kind::Sqlx => {
                let size = u32::try_from(size)?;
                let opts = MySqlConnectOptions::from_str(url)?
                    .database(database)
                    .statement_cache_capacity(128);
                let pool = MySqlPoolOptions::new()
                    .min_connections(size)
                    .max_connections(size)
                    .test_before_acquire(false)
                    .acquire_timeout(Duration::from_secs(10))
                    .after_connect(|conn, _| {
                        Box::pin(async move {
                            for sql in SETUP {
                                conn.execute(sql).await?;
                            }
                            Ok(())
                        })
                    })
                    .connect_with(opts)
                    .await?;
                Ok(Self::Sqlx(pool))
            }
            Kind::NativeReset | Kind::NativeRetain => {
                let bounds = PoolConstraints::new(size, size)
                    .ok_or_else(|| invalid("invalid native pool bounds"))?;
                let reset = matches!(kind, Kind::NativeReset);
                let pool = PoolOpts::default()
                    .with_constraints(bounds)
                    .with_reset_connection(reset);
                let opts = OptsBuilder::from_opts(Opts::from_url(url)?)
                    .db_name(Some(database))
                    .stmt_cache_size(128)
                    .setup(SETUP.to_vec())
                    .pool_opts(pool);
                Ok(Self::Native(Pool::new(opts)))
            }
        }
    }

    pub(crate) async fn acquire(&self) -> ProbeResult<Connection> {
        match self {
            Self::Sqlx(pool) => Ok(Connection::Sqlx(pool.acquire().await?)),
            Self::Native(pool) => {
                Ok(Connection::Native(pool.get_conn().await?))
            }
        }
    }

    pub(crate) async fn idle(&self, size: usize) {
        loop {
            let idle = match self {
                Self::Sqlx(pool) => pool.num_idle(),
                Self::Native(pool) => {
                    pool.metrics().connections_in_pool.load(Ordering::SeqCst)
                }
            };
            if idle == size {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    pub(crate) async fn warm(&self, size: usize) -> ProbeResult<()> {
        let mut leases = Vec::with_capacity(size);
        for _ in 0..size {
            let mut conn = self.acquire().await?;
            for _ in 0..10 {
                conn.insert().await?;
            }
            leases.push(conn);
        }
        drop(leases);
        self.idle(size).await;
        Ok(())
    }

    pub(crate) async fn close(self) -> ProbeResult<()> {
        match self {
            Self::Sqlx(pool) => pool.close().await,
            Self::Native(pool) => pool.disconnect().await?,
        }
        Ok(())
    }
}

impl Connection {
    pub(crate) async fn insert(&mut self) -> ProbeResult<u64> {
        let (affected, id) = match self {
            Self::Sqlx(conn) => {
                let done = sqlx::query(INSERT)
                    .bind(TITLE)
                    .bind(false)
                    .execute(&mut **conn)
                    .await?;
                (done.rows_affected(), done.last_insert_id())
            }
            Self::Native(conn) => {
                conn.exec_drop(INSERT, (TITLE, false)).await?;
                (conn.affected_rows(), conn.last_insert_id().unwrap_or(0))
            }
        };
        if affected != 1 || id == 0 {
            return Err(invalid(
                "insert did not acknowledge one generated row",
            ));
        }
        Ok(id)
    }
}
