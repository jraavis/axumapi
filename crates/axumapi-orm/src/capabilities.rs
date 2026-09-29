//! Backend capability model.
//!
//! Backends declare what they support; the query layer checks a plan against
//! these declarations **before** execution so unsupported features fail with a
//! [`BackendCapabilityError`] instead of being silently ignored.

use crate::error::BackendCapabilityError;

/// Family of a backend. Used for diagnostics and dialect selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BackendKind {
    /// PostgreSQL.
    Postgres,
    /// SQLite.
    Sqlite,
    /// MySQL / MariaDB.
    MySql,
    /// MongoDB.
    MongoDb,
    /// Redis (restricted key/value model; not a relational ORM backend).
    Redis,
}

/// Transaction support level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TransactionSupport {
    /// No multi-statement transactions.
    None,
    /// Transactions without nested savepoints.
    Flat,
    /// Transactions with savepoints (nested transactions).
    Savepoints,
}

/// Row-locking support level (`SELECT ... FOR UPDATE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RowLocking {
    /// No row locking.
    None,
    /// Plain `FOR UPDATE`.
    Basic,
    /// `FOR UPDATE` with `NOWAIT` and `SKIP LOCKED`.
    Full,
}

/// A single optional query feature that a plan may require.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Feature {
    /// JOINs between sources.
    Joins,
    /// `RETURNING` clauses on writes.
    Returning,
    /// Row locking.
    RowLocking,
    /// `NOWAIT` / `SKIP LOCKED` lock modifiers.
    LockModifiers,
    /// Window functions.
    WindowFunctions,
    /// Regular expression matching.
    Regex,
    /// Native array columns.
    Arrays,
    /// `DISTINCT ON (...)`.
    DistinctOn,
    /// `ILIKE` style case-insensitive matching natively.
    CaseInsensitiveLike,
}

/// Declared capabilities of a backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendCapabilities {
    /// Backend family.
    pub kind: BackendKind,
    /// Transaction support level.
    pub transactions: TransactionSupport,
    /// Row-locking support level.
    pub row_locking: RowLocking,
    /// JOIN support.
    pub joins: bool,
    /// `RETURNING` support.
    pub returning: bool,
    /// Window function support.
    pub window_functions: bool,
    /// Regex lookups.
    pub regex: bool,
    /// Native arrays.
    pub arrays: bool,
    /// `DISTINCT ON`.
    pub distinct_on: bool,
    /// Native `ILIKE`.
    pub ilike: bool,
}

impl BackendCapabilities {
    /// Capabilities of PostgreSQL (12+).
    pub const fn postgres() -> Self {
        Self {
            kind: BackendKind::Postgres,
            transactions: TransactionSupport::Savepoints,
            row_locking: RowLocking::Full,
            joins: true,
            returning: true,
            window_functions: true,
            regex: true,
            arrays: true,
            distinct_on: true,
            ilike: true,
        }
    }

    /// Capabilities of SQLite (3.35+ for `RETURNING`, 3.25+ for windows).
    ///
    /// SQLite has no row locking and no built-in `REGEXP` function.
    pub const fn sqlite() -> Self {
        Self {
            kind: BackendKind::Sqlite,
            transactions: TransactionSupport::Savepoints,
            row_locking: RowLocking::None,
            joins: true,
            returning: true,
            window_functions: true,
            regex: false,
            arrays: false,
            distinct_on: false,
            ilike: false,
        }
    }

    /// Whether `feature` is supported.
    pub fn supports(&self, feature: Feature) -> bool {
        match feature {
            Feature::Joins => self.joins,
            Feature::Returning => self.returning,
            Feature::RowLocking => self.row_locking >= RowLocking::Basic,
            Feature::LockModifiers => self.row_locking >= RowLocking::Full,
            Feature::WindowFunctions => self.window_functions,
            Feature::Regex => self.regex,
            Feature::Arrays => self.arrays,
            Feature::DistinctOn => self.distinct_on,
            // Emulated with LOWER(..) LIKE LOWER(..) where not native.
            Feature::CaseInsensitiveLike => true,
        }
    }

    /// Return `Ok(())` if `feature` is supported, otherwise a typed error.
    pub fn require(&self, feature: Feature) -> Result<(), BackendCapabilityError> {
        if self.supports(feature) {
            Ok(())
        } else {
            Err(BackendCapabilityError::from_feature(self.kind, feature))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_rejects_row_locking() {
        let err = BackendCapabilities::sqlite().require(Feature::RowLocking);
        assert!(matches!(
            err,
            Err(BackendCapabilityError::RowLockingUnsupported {
                backend: BackendKind::Sqlite
            })
        ));
    }

    #[test]
    fn postgres_supports_everything_modelled() {
        let caps = BackendCapabilities::postgres();
        for f in [
            Feature::Joins,
            Feature::Regex,
            Feature::LockModifiers,
            Feature::DistinctOn,
        ] {
            assert!(caps.supports(f), "{f:?}");
        }
    }
}
