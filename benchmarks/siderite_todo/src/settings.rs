//! Capture a sanitized sample of effective SQL settings before timed work.

use serde_json::{Map, Value};
use siderite::orm::{BackendKind, Db};
use siderite::prelude::ApiError;

/// Read effective settings using fixed queries that never expose credentials.
///
/// Args:
///     db: Open benchmark database pool.
///
/// Returns:
///     A sampled settings object; None for non-SQL backends.
pub async fn capture(db: &Db) -> Result<Option<Value>, ApiError> {
    let queries: &[(&str, &str)] = match db.capabilities().kind {
        BackendKind::Sqlite => &[
            ("version", "SELECT sqlite_version() AS value"),
            ("journal_mode", "PRAGMA journal_mode"),
            ("synchronous", "PRAGMA synchronous"),
            ("foreign_keys", "PRAGMA foreign_keys"),
            ("busy_timeout", "PRAGMA busy_timeout"),
        ],
        BackendKind::Postgres => &[
            ("version", "SELECT version() AS value"),
            ("synchronous_commit", "SHOW synchronous_commit"),
            ("fsync", "SHOW fsync"),
            ("full_page_writes", "SHOW full_page_writes"),
            ("timezone", "SHOW TimeZone"),
        ],
        BackendKind::MySql => &[
            ("version", "SELECT VERSION() AS value"),
            ("autocommit", "SELECT @@session.autocommit AS value"),
            ("timezone", "SELECT @@session.time_zone AS value"),
            ("sql_mode", "SELECT @@session.sql_mode AS value"),
            (
                "innodb_flush_log_at_trx_commit",
                "SELECT @@global.innodb_flush_log_at_trx_commit AS value",
            ),
            ("sync_binlog", "SELECT @@global.sync_binlog AS value"),
        ],
        _ => return Ok(None),
    };
    let mut settings = Map::new();
    for (key, query) in queries {
        let rows = db
            .raw_sql(query, vec![])
            .await
            .map_err(ApiError::internal)?;
        let value = rows
            .rows
            .first()
            .and_then(|row| row.iter().next())
            .map(|(_, value)| value)
            .ok_or_else(|| ApiError::internal("missing setting result"))?;
        let value = serde_json::to_value(value).map_err(ApiError::internal)?;
        settings.insert((*key).into(), value);
    }
    Ok(Some(Value::Object(settings)))
}

/// Emit the fixed benchmark settings record with the owning process id.
///
/// Args:
///     settings: Sanitized settings from a fixed capture routine.
///
/// Returns:
///     Unit after writing the startup record.
pub fn emit(settings: Value) {
    eprintln!(
        "BENCHMARK_DATABASE {}",
        serde_json::json!({
            "pid": std::process::id(), "settings": settings,
        })
    );
}
