//! Errors raised by the migration loader, autodetector, executor and CLI.

use siderite_orm::{BackendCapabilityError, BackendKind, OrmError};
use thiserror::Error;

/// Failure of a migration command, graph check, or DDL step.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MigrationError {
    /// A migration lists a dependency that is not on disk.
    #[error("unknown dependency `{dependency}` of migration `{id}`")]
    MissingDependency {
        /// Migration that named the missing dependency.
        id: String,
        /// Missing migration id.
        dependency: String,
    },
    /// The dependency graph contains a cycle.
    #[error("cycle in migration graph: {}", .nodes.join(" -> "))]
    Cycle {
        /// Nodes of one cycle, starting and ending at the same id.
        nodes: Vec<String>,
    },
    /// More than one leaf (unmerged heads).
    #[error(
        "multiple migration heads: {}. Create a merge migration or run squashmigrations",
        .heads.join(", ")
    )]
    MultipleHeads {
        /// Leaf migration ids, sorted.
        heads: Vec<String>,
    },
    /// An applied migration's file no longer matches the checksum recorded in
    /// the history table.
    #[error("checksum mismatch for applied migration `{id}` (history {history}, file {file})")]
    ChecksumMismatch {
        /// Migration id.
        id: String,
        /// Checksum stored in `siderite_migrations`.
        history: String,
        /// Checksum of the file currently on disk.
        file: String,
    },
    /// A rollback was requested for a migration that cannot be reversed.
    #[error("cannot reverse migration `{id}`: {reason}")]
    Irreversible {
        /// Migration id.
        id: String,
        /// Why reversal is impossible.
        reason: String,
    },
    /// Filesystem failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Migration JSON could not be encoded or decoded.
    #[error("invalid migration JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// The ORM backend rejected a statement or connection.
    #[error(transparent)]
    Orm(#[from] OrmError),
    /// A requested SQL feature is not supported by this backend.
    #[error(transparent)]
    Capability(#[from] BackendCapabilityError),
    /// Schema migrations are only implemented for PostgreSQL, SQLite and MySQL.
    #[error("schema migrations are not supported by {0:?}")]
    UnsupportedBackend(BackendKind),
    /// The operation does not apply to the current [`crate::state::ProjectState`].
    #[error("{0}")]
    State(String),
    /// A named `RunRust` operation is not in the [`crate::registry::MigrationRegistry`].
    #[error("unregistered RunRust operation `{0}`")]
    UnregisteredRust(String),
    /// Command-line usage error.
    #[error("{0}")]
    Usage(String),
    /// MySQL DDL committed earlier statements, then a later one failed.
    #[error(
        "MySQL migration `{id}` failed on statement {index} of {total} after earlier DDL committed (repair the schema by hand, then re-run): {source}"
    )]
    MysqlPartial {
        /// Migration id.
        id: String,
        /// 1-based statement that failed.
        index: usize,
        /// Number of SQL statements in the migration.
        total: usize,
        /// Driver error from the failing statement.
        #[source]
        source: OrmError,
    },
}

impl MigrationError {
    /// [`State`](Self::State) helper.
    pub fn state(msg: impl Into<String>) -> Self {
        Self::State(msg.into())
    }

    /// [`Usage`](Self::Usage) helper.
    pub fn usage(msg: impl Into<String>) -> Self {
        Self::Usage(msg.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use siderite_orm::BackendError;

    #[test]
    fn mysql_partial_names_the_failed_statement() {
        let err = MigrationError::MysqlPartial {
            id: "0002_add".into(),
            index: 2,
            total: 3,
            source: BackendError::Database("table exists".into()).into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("statement 2 of 3"), "{msg}");
        assert!(msg.contains("0002_add"), "{msg}");
        assert!(msg.contains("repair the schema by hand"), "{msg}");
    }
}
