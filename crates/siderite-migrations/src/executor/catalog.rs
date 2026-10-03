//! Parameterized catalog checks shared by inspection and journaling.

use super::{BackendKind, Db, MigrationError as Error, Value};

pub(super) async fn table_exists(db: &Db, table: &str) -> Result<bool, Error> {
    let sql = match db.capabilities().kind {
        BackendKind::Sqlite => {
            "SELECT 1 FROM sqlite_master WHERE type = 'table' \
             AND name = ?"
        }
        BackendKind::Postgres => {
            "SELECT 1 FROM information_schema.tables \
            WHERE table_schema = current_schema() AND table_name = $1"
        }
        BackendKind::MySql => {
            "SELECT 1 FROM information_schema.tables \
            WHERE table_schema = DATABASE() AND table_name = ?"
        }
        other => return Err(Error::UnsupportedBackend(other)),
    };
    Ok(!db
        .raw_sql(sql, vec![Value::Text(table.to_owned())])
        .await?
        .rows
        .is_empty())
}
