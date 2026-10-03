//! Read-only inspection of recorded partial migration work.

use super::progress::PROGRESS_TABLE;
use super::{MigrationError as Error, MigrationIntent, Migrator, Value, quote_star};
use serde::Serialize;
use siderite_orm::Row;

type Result<T> = std::result::Result<T, Error>;

/// Recorded recovery state; catalogs must be reconciled before any repair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecoveryEntry {
    /// Migration whose statements were recorded.
    pub migration_id: String,
    /// Original migration file checksum.
    pub checksum: String,
    /// `apply` or `unapply`.
    pub direction: String,
    /// Number of completed operations recorded by the executor.
    pub completed_operations: usize,
    /// Completed statements within the next operation.
    pub completed_statements: usize,
    /// Whether the supplied graph has the same file; None means unknown id.
    pub file_matches: Option<bool>,
    /// Whether this migration also has an applied history row.
    pub history_applied: bool,
}

/// Snapshot of applied ids and recorded partial work, without modifying SQL.
///
/// A crash can occur after DDL commits but before its progress write. Entries
/// are bookkeeping, never proof that the live schema matches those indices.
/// Reads take no migration lock and may race with an active migrator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecoveryReport {
    /// Applied history ids, sorted lexically.
    pub applied: Vec<String>,
    /// Recorded partial work, sorted by migration id.
    pub partial: Vec<RecoveryEntry>,
    /// Explicit scripts or callbacks with unconfirmed completion.
    pub uncertain_steps: Vec<MigrationIntent>,
}

impl Migrator<'_> {
    /// Inspect recovery bookkeeping using SELECT statements only.
    ///
    /// Args:
    ///     self: Migrator with the database and current migration graph.
    ///
    /// Returns:
    ///     Recorded history/progress and file-checksum match information.
    ///     Missing tables produce empty lists and are never created here.
    ///
    /// # Errors
    /// Unsupported backend, unreadable catalogs/tables or malformed rows.
    pub async fn inspect_recovery(&self) -> Result<RecoveryReport> {
        self.require_backend()?;
        let history = self.load_history().await?;
        let mut applied: Vec<_> = history.ids.iter().cloned().collect();
        applied.sort();
        let mut partial = Vec::new();
        if self.table_exists_on(self.db, PROGRESS_TABLE).await? {
            let result = self
                .db
                .raw_sql(
                    &format!(
                        "SELECT * FROM {} ORDER BY {}",
                        quote_star(self.kind(), PROGRESS_TABLE),
                        quote_star(self.kind(), "migration_id")
                    ),
                    Vec::new(),
                )
                .await?;
            for row in result.rows {
                let migration_id = text(&row, "migration_id")?;
                let checksum = text(&row, "checksum")?;
                let direction = text(&row, "direction")?;
                if !matches!(direction.as_str(), "apply" | "unapply") {
                    return Err(Error::state("invalid progress direction"));
                }
                let file_matches = self
                    .graph
                    .get(&migration_id)
                    .map(|migration| migration.checksum == checksum);
                partial.push(RecoveryEntry {
                    history_applied: history.ids.contains(&migration_id),
                    migration_id,
                    checksum,
                    direction,
                    completed_operations: read_index(&row, "op_index")?,
                    completed_statements: read_index(&row, "stmt_index")?,
                    file_matches,
                });
            }
        }
        let uncertain_steps = super::intents::read(self.db, None).await?;
        Ok(RecoveryReport {
            applied,
            partial,
            uncertain_steps,
        })
    }
}

pub(super) fn text(row: &Row, key: &str) -> Result<String> {
    match row.get(key) {
        Some(Value::Text(value)) if !value.is_empty() => Ok(value.clone()),
        _ => Err(Error::state(format!(
            "migration recovery has an invalid `{key}`"
        ))),
    }
}

pub(super) fn read_index(row: &Row, key: &str) -> Result<usize> {
    match row.get(key) {
        Some(Value::Int(n)) => positive_index(*n),
        _ => Err(Error::state(format!(
            "migration recovery has an invalid `{key}`"
        ))),
    }
}

fn invalid_index() -> Error {
    Error::state("invalid progress index")
}

fn positive_index(value: i64) -> Result<usize> {
    usize::try_from(value).map_err(|_| invalid_index())
}
