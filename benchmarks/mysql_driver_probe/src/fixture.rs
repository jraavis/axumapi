//! Unique, disposable databases and independent stored-row verification.

use crate::driver::TITLE;
use crate::{ProbeResult, invalid};
use sqlx::mysql::MySqlConnectOptions;
use sqlx::{Connection, MySqlConnection, Row};
use std::collections::{BTreeMap, HashSet};
use std::str::FromStr;

pub(crate) struct Fixture {
    admin: MySqlConnection,
    pub(crate) name: String,
}

impl Fixture {
    pub(crate) async fn create(url: &str) -> ProbeResult<Self> {
        let opts = MySqlConnectOptions::from_str(url)?;
        let mut admin = MySqlConnection::connect_with(&opts).await?;
        let name = format!("siderite_probe_{}", uuid::Uuid::new_v4().simple());
        let create = format!("CREATE DATABASE `{name}`");
        sqlx::raw_sql(&create).execute(&mut admin).await?;
        let mut fixture = Self { admin, name };
        if let Err(error) = fixture.initialize().await {
            let _ = fixture.destroy().await;
            return Err(error);
        }
        Ok(fixture)
    }

    async fn initialize(&mut self) -> ProbeResult<()> {
        let select = format!("USE `{}`", self.name);
        sqlx::raw_sql(&select).execute(&mut self.admin).await?;
        let schema = "CREATE TABLE todos (
            id BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
            title VARCHAR(200) NOT NULL,
            done BOOLEAN NOT NULL DEFAULT FALSE
        ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4";
        sqlx::raw_sql(schema).execute(&mut self.admin).await?;
        Ok(())
    }

    pub(crate) async fn settings(
        &mut self,
    ) -> ProbeResult<BTreeMap<String, String>> {
        let row = sqlx::query(
            "SELECT VERSION() AS version,
             CAST(@@innodb_flush_log_at_trx_commit AS CHAR) AS flush,
             CAST(@@sync_binlog AS CHAR) AS sync_binlog,
             CAST(@@log_bin AS CHAR) AS log_bin,
             CAST(@@autocommit AS CHAR) AS autocommit,
             @@sql_mode AS sql_mode",
        )
        .fetch_one(&mut self.admin)
        .await?;
        let mut settings = BTreeMap::new();
        settings.insert("version".into(), row.try_get("version")?);
        settings.insert("sql_mode".into(), row.try_get("sql_mode")?);
        for key in ["flush", "sync_binlog", "log_bin", "autocommit"] {
            let value: String = row.try_get(key)?;
            settings.insert(key.into(), value);
        }
        Ok(settings)
    }

    pub(crate) async fn counters(
        &mut self,
    ) -> ProbeResult<BTreeMap<String, u64>> {
        let rows = sqlx::raw_sql(
            "SHOW GLOBAL STATUS WHERE Variable_name IN (
                'Com_insert', 'Com_stmt_execute', 'Com_stmt_prepare',
                'Com_reset_connection', 'Com_admin_commands')",
        )
        .fetch_all(&mut self.admin)
        .await?;
        rows.into_iter()
            .map(|row| {
                let key = row.try_get::<String, _>(0)?;
                let value = row.try_get::<String, _>(1)?.parse()?;
                Ok((key, value))
            })
            .collect()
    }

    pub(crate) async fn reset(&mut self) -> ProbeResult<()> {
        // Preserve schema identity and warmed statements; do not reset keys.
        sqlx::query("DELETE FROM todos")
            .execute(&mut self.admin)
            .await?;
        Ok(())
    }

    pub(crate) async fn verify(&mut self, ids: &[u64]) -> ProbeResult<()> {
        let expected: HashSet<u64> = ids.iter().copied().collect();
        if expected.len() != ids.len() || expected.contains(&0) {
            return Err(invalid("duplicate or missing returned insert IDs"));
        }
        let rows = sqlx::query("SELECT id, title, done FROM todos")
            .fetch_all(&mut self.admin)
            .await?;
        let mut actual = HashSet::with_capacity(rows.len());
        for row in rows {
            let id: i64 = row.try_get("id")?;
            actual.insert(u64::try_from(id)?);
            let title: String = row.try_get("title")?;
            let done: bool = row.try_get("done")?;
            if title != TITLE || done {
                return Err(invalid(
                    "stored row differs from acknowledged insert",
                ));
            }
        }
        if actual != expected {
            return Err(invalid(
                "stored rows do not match returned insert IDs",
            ));
        }
        Ok(())
    }

    pub(crate) async fn destroy(mut self) -> ProbeResult<()> {
        let drop = format!("DROP DATABASE `{}`", self.name);
        sqlx::raw_sql(&drop).execute(&mut self.admin).await?;
        self.admin.close().await?;
        Ok(())
    }
}
