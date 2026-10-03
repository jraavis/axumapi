//! Fail closed when explicit non-transactional data work is unconfirmed.

use super::catalog::table_exists;
use super::recovery::{read_index, text};
use super::{Db, Migration, MigrationError as Error, MigrationRegistry};
use super::{Value, placeholder, quote_star};
use serde::Serialize;

const TABLE: &str = "siderite_migration_intents";
type Result<T> = std::result::Result<T, Error>;
type Intents = Vec<MigrationIntent>;

/// Explicit SQL/callback work entered before completion was confirmed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationIntent {
    /// Migration containing the explicit script or callback.
    pub migration_id: String,
    /// File checksum when execution started.
    pub checksum: String,
    /// `apply` or `unapply`.
    pub direction: String,
    /// Zero-based operation index in this direction.
    pub operation: usize,
    /// Callback name; None denotes an explicit RunSQL script.
    pub callback: Option<String>,
}

impl MigrationIntent {
    fn uncertain(&self) -> Error {
        match &self.callback {
            Some(callback) => Error::UncertainRustStep {
                id: self.migration_id.clone(),
                callback: callback.clone(),
                operation: self.operation,
            },
            None => Error::UncertainSqlStep {
                id: self.migration_id.clone(),
                operation: self.operation,
            },
        }
    }
}

/// Read recorded intents without creating bookkeeping tables.
///
/// Args:
///     db: Database handle used for catalog and bookkeeping reads.
///     id: Optional migration filter.
///
/// Returns:
///     Validated pending steps, ordered by migration id.
pub(super) async fn read(db: &Db, id: Option<&str>) -> Result<Intents> {
    if !table_exists(db, TABLE).await? {
        return Ok(Vec::new());
    }
    let kind = db.capabilities().kind;
    let mut sql = format!("SELECT * FROM {}", quote_star(kind, TABLE));
    let mut params = Vec::new();
    if let Some(id) = id {
        let bind = placeholder(kind, 1);
        sql.push_str(&format!(" WHERE migration_id = {bind}"));
        params.push(Value::Text(id.to_owned()));
    }
    sql.push_str(" ORDER BY migration_id");
    let mut intents = Vec::new();
    for row in db.raw_sql(&sql, params).await?.rows {
        let direction = text(&row, "direction")?;
        if !matches!(direction.as_str(), "apply" | "unapply") {
            return Err(Error::state("invalid intent direction"));
        }
        intents.push(MigrationIntent {
            migration_id: text(&row, "migration_id")?,
            checksum: text(&row, "checksum")?,
            direction,
            operation: read_index(&row, "operation_index")?,
            callback: match row.get("callback") {
                Some(Value::Null) => None,
                _ => Some(text(&row, "callback")?),
            },
        });
    }
    Ok(intents)
}

/// Reject uncertain work unless its callback is declared safe to replay.
///
/// Args:
///     db: Migration session.
///     migration: Original checksummed migration.
///     registry: Callback implementations and replay declarations.
///     direction: Requested apply or unapply direction.
///
/// Returns:
///     Success when the requested execution may proceed.
pub(super) async fn guard(
    db: &Db,
    migration: &Migration,
    registry: &MigrationRegistry,
    direction: &str,
) -> Result<()> {
    for intent in read(db, Some(&migration.id)).await? {
        if intent.checksum != migration.checksum {
            return Err(Error::state(format!(
                "migration `{}` changed since a partial execution; \
                 restore the original file before reconciliation",
                migration.id,
            )));
        }
        if intent.direction != direction
            || !intent
                .callback
                .as_deref()
                .is_some_and(|name| registry.is_replay_safe(name))
        {
            return Err(intent.uncertain());
        }
    }
    Ok(())
}

/// Persist uncertainty before entering non-transactional explicit work.
///
/// Args:
///     db: Migration session.
///     migration: Original checksummed migration.
///     direction: Apply or unapply direction.
///     operation: Zero-based operation index.
///     callback: Callback name, or None for an explicit SQL script.
///
/// Returns:
///     Success after the intent is recorded or its identity is verified.
pub(super) async fn begin(
    db: &Db,
    migration: &Migration,
    direction: &str,
    operation: usize,
    callback: Option<&str>,
) -> Result<()> {
    let kind = db.capabilities().kind;
    let table = quote_star(kind, TABLE);
    db.execute_script(&format!(
        "CREATE TABLE IF NOT EXISTS {table} (\
         migration_id VARCHAR(255) PRIMARY KEY, \
         checksum VARCHAR(255) NOT NULL, \
         direction VARCHAR(16) NOT NULL, \
         operation_index INTEGER NOT NULL, callback TEXT)"
    ))
    .await?;
    if let Some(intent) = read(db, Some(&migration.id)).await?.first() {
        let same_callback = intent.callback.as_deref() == callback;
        if intent.operation != operation || !same_callback {
            return Err(intent.uncertain());
        }
        return Ok(());
    }
    let values = (1..=5)
        .map(|index| placeholder(kind, index))
        .collect::<Vec<_>>()
        .join(", ");
    let operation = i64::try_from(operation).map_err(|_| index_overflow())?;
    db.raw_execute(
        &format!(
            "INSERT INTO {table} (migration_id, checksum, direction, \
         operation_index, callback) VALUES ({values})"
        ),
        vec![
            Value::Text(migration.id.clone()),
            Value::Text(migration.checksum.clone()),
            Value::Text(direction.to_owned()),
            Value::Int(operation),
            callback.map_or(Value::Null, |name| Value::Text(name.to_owned())),
        ],
    )
    .await?;
    Ok(())
}

/// Clear an intent after completion and progress are recorded.
///
/// Args:
///     db: Migration session.
///     migration: Original checksummed migration.
///
/// Returns:
///     Success only when exactly one matching intent was removed.
pub(super) async fn finish(db: &Db, migration: &Migration) -> Result<()> {
    let kind = db.capabilities().kind;
    let affected = db
        .raw_execute(
            &format!(
                "DELETE FROM {} WHERE migration_id = {} AND checksum = {}",
                quote_star(kind, TABLE),
                placeholder(kind, 1),
                placeholder(kind, 2),
            ),
            vec![
                Value::Text(migration.id.clone()),
                Value::Text(migration.checksum.clone()),
            ],
        )
        .await?;
    if affected != 1 {
        return Err(Error::state("migration intent lost before completion"));
    }
    Ok(())
}

fn index_overflow() -> Error {
    Error::state("migration operation index overflow")
}

/// Clear a guarded callback intent when durable progress records completion.
///
/// Args:
///     db: Migration session after the replay-safety guard succeeded.
///     migration: Original checksummed migration.
///     completed: Validated number of completed operations.
///
/// Returns:
///     Success after clearing an already-recorded callback completion.
pub(super) async fn finish_recorded_callback(
    db: &Db,
    migration: &Migration,
    completed: usize,
) -> Result<()> {
    if completed == 0 {
        return Ok(());
    }
    for intent in read(db, Some(&migration.id)).await? {
        if intent.callback.is_some() && intent.operation < completed {
            finish(db, migration).await?;
        }
    }
    Ok(())
}
