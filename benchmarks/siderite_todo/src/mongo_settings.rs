//! Record MongoDB concern and pool policy without credentials or topology.

use mongodb::Database;
use mongodb::bson::doc;
use mongodb::options::{ClientOptions, ReadPreference, SelectionCriteria};
use serde::Serialize;
use serde_json::{Value, json};
use siderite::prelude::ApiError as Error;

type Result<T> = std::result::Result<T, Error>;

/// Capture client policy and effective server defaults before timed work.
///
/// Args:
///     db: Database used by the benchmark adapter.
///     options: Parsed URI options used to create that adapter.
///
/// Returns:
///     Sanitized comparable settings, or an error for unreadable policy.
pub async fn capture(db: &Database, options: &ClientOptions) -> Result<Value> {
    if !matches!(
        db.selection_criteria(),
        None | Some(SelectionCriteria::ReadPreference(ReadPreference::Primary))
    ) {
        return Err(Error::internal("benchmark requires primary reads"));
    }
    let admin = db.client().database("admin");
    let build = admin
        .run_command(doc! { "buildInfo": 1 })
        .await
        .map_err(Error::internal)?;
    let defaults = admin
        .run_command(doc! { "getDefaultRWConcern": 1 })
        .await
        .map_err(Error::internal)?;
    let replica = admin
        .run_command(doc! { "replSetGetConfig": 1 })
        .await
        .map_err(Error::internal)?;
    let config = replica.get_document("config").map_err(Error::internal)?;
    let journal = config
        .get_bool("writeConcernMajorityJournalDefault")
        .map_err(Error::internal)?;
    let pool = options
        .max_pool_size
        .ok_or_else(|| Error::internal("explicit Mongo pool size required"))?;
    Ok(json!({
        "version": build.get_str("version").map_err(Error::internal)?,
        "read_preference": "primary",
        "client_read_concern": canonical(db.read_concern())?,
        "client_write_concern": canonical(db.write_concern())?,
        "server_default_read_concern": canonical(
            defaults.get_document("defaultReadConcern").ok()
        )?,
        "server_default_write_concern": canonical(
            defaults.get_document("defaultWriteConcern").ok()
        )?,
        "majority_journal_default": u8::from(journal),
        "max_pool_size": pool,
        "min_pool_size": options.min_pool_size.unwrap_or(0),
        "max_connecting": options.max_connecting.unwrap_or(2),
        "retry_reads": u8::from(options.retry_reads.unwrap_or(true)),
        "retry_writes": u8::from(options.retry_writes.unwrap_or(true)),
    }))
}

fn canonical<T: Serialize>(value: Option<T>) -> Result<String> {
    let value = match value {
        Some(value) => serde_json::to_value(value).map_err(Error::internal)?,
        None => json!({}),
    };
    serde_json::to_string(&value).map_err(Error::internal)
}
