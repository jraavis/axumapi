//! Apply and reverse migrations against a [`siderite_orm::Db`].

use crate::error::MigrationError;
use crate::loader::{MigrationGraph, is_applied};
use crate::migration::Migration;
use crate::operation::Operation;
use crate::registry::MigrationRegistry;
use crate::schema_editor;
use crate::state::ProjectState;
use chrono::Utc;
use siderite_orm::types::TIMESTAMP_FORMAT;
use siderite_orm::{BackendKind, Db, TransactionSupport, Value};
use std::collections::{HashMap, HashSet};
use std::future::Future;

/// History table written by the migrator.
pub const HISTORY_TABLE: &str = "siderite_migrations";

/// Outcome of [`Migrator::migrate`] or [`Migrator::rollback`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    /// Migration ids that would be (or were) applied / unapplied, in order.
    pub planned: Vec<String>,
    /// Generated SQL, including the history-table DDL when it would be created.
    pub sql: Vec<String>,
    /// Ids actually written to (or deleted from) the history table.
    pub applied: Vec<String>,
    /// When true, no statement was executed.
    pub dry_run: bool,
}

/// Applies JSON migrations through [`Db`].
pub struct Migrator<'a> {
    db: &'a Db,
    graph: &'a MigrationGraph,
    registry: MigrationRegistry,
}

impl<'a> Migrator<'a> {
    /// Plan and execute using `graph`.
    pub fn new(db: &'a Db, graph: &'a MigrationGraph) -> Self {
        Self {
            db,
            graph,
            registry: MigrationRegistry::new(),
        }
    }

    /// Register `RunRust` functions used by the planned migrations.
    #[must_use]
    pub fn with_registry(mut self, registry: MigrationRegistry) -> Self {
        self.registry = registry;
        self
    }

    /// Ids that would be applied to reach `target` (`None` = all remaining).
    ///
    /// # Errors
    /// Checksum mismatch, unknown target, graph/state errors, or backend errors
    /// while reading the history table.
    pub async fn plan_forward(&self, target: Option<&str>) -> Result<Vec<String>, MigrationError> {
        let history = self.load_history().await?;
        self.verify_checksums(&history)?;
        self.forward_plan(&history, target)
    }

    /// Apply unapplied migrations up to `target` (`None` = all).
    ///
    /// `dry_run` returns the SQL without executing it (history is still read).
    ///
    /// # Errors
    /// See [`Migrator::plan_forward`]; also statement execution errors.
    pub async fn migrate(
        &self,
        target: Option<&str>,
        dry_run: bool,
    ) -> Result<Report, MigrationError> {
        self.require_backend()?;
        if dry_run {
            let history = self.load_history().await?;
            self.verify_checksums(&history)?;
            let plan = self.forward_plan(&history, target)?;
            let (sql, _) = self.plan_sql(&history, &plan)?;
            return Ok(Report {
                planned: plan,
                sql,
                applied: Vec::new(),
                dry_run: true,
            });
        }
        self.with_lock(|session| {
            let this = self;
            async move {
                let created_history = if !this.history_table_exists_on(&session).await? {
                    session
                        .execute_script(&create_history_sql(this.kind()))
                        .await?;
                    true
                } else {
                    false
                };
                let history = this.load_history_from(&session).await?;
                this.verify_checksums(&history)?;
                let plan = this.forward_plan(&history, target)?;
                // Render under the lock, from the plan re-read here:
                // `Report.sql` must not disagree with `Report.planned`
                // about what actually ran.
                let (mut sql, mut state) = this.plan_sql(&history, &plan)?;
                if created_history {
                    sql.insert(0, create_history_sql(this.kind()));
                }
                let mut applied = Vec::new();
                for id in &plan {
                    let migration = this.require_migration(id)?.clone();
                    this.apply_one(&session, &migration, &mut state).await?;
                    applied.push(id.clone());
                }
                Ok(Report {
                    planned: plan,
                    sql,
                    applied,
                    dry_run: false,
                })
            }
        })
        .await
    }

    /// Unapply migrations. `target` is the id that should remain the head
    /// (everything after it is rolled back). `steps` unapplies that many of
    /// the most recently applied visible migrations. Exactly one of the two
    /// must be set unless `steps` is `Some(n)`.
    ///
    /// # Errors
    /// Irreversible migrations, checksum mismatch, unknown target, backend
    /// errors.
    pub async fn rollback(
        &self,
        target: Option<&str>,
        steps: Option<u32>,
        dry_run: bool,
    ) -> Result<Report, MigrationError> {
        self.require_backend()?;
        if dry_run {
            let history = self.load_history().await?;
            self.verify_checksums(&history)?;
            let plan = self.rollback_plan(&history, target, steps)?;
            let sql = self.rollback_sql(&history, &plan)?;
            return Ok(Report {
                planned: plan,
                sql,
                applied: Vec::new(),
                dry_run: true,
            });
        }
        self.with_lock(|session| {
            let this = self;
            async move {
                let created_history = if !this.history_table_exists_on(&session).await? {
                    session
                        .execute_script(&create_history_sql(this.kind()))
                        .await?;
                    true
                } else {
                    false
                };
                let history = this.load_history_from(&session).await?;
                this.verify_checksums(&history)?;
                let plan = this.rollback_plan(&history, target, steps)?;
                let mut sql = this.rollback_sql(&history, &plan)?;
                if created_history {
                    sql.insert(0, create_history_sql(this.kind()));
                }
                let mut unapplied = Vec::new();
                for id in &plan {
                    let migration = this.require_migration(id)?.clone();
                    let state_before = state_before_migration(this.graph, &history, id)?;
                    this.unapply_one(&session, &migration, &state_before)
                        .await?;
                    unapplied.push(id.clone());
                }
                Ok(Report {
                    planned: plan,
                    sql,
                    applied: unapplied,
                    dry_run: false,
                })
            }
        })
        .await
    }

    /// Every loaded migration with its applied flag (including replaced ids).
    ///
    /// # Errors
    /// History-table or checksum errors.
    pub async fn show(&self) -> Result<Vec<(String, bool)>, MigrationError> {
        let history = self.load_history().await?;
        self.verify_checksums(&history)?;
        let mut rows = Vec::new();
        let mut seen = HashSet::new();
        for id in &self.graph.order {
            if let Some(m) = self.graph.get(id) {
                rows.push((id.clone(), is_applied(m, &history.ids)));
                seen.insert(id.clone());
            }
        }
        let mut rest: Vec<&Migration> = self
            .graph
            .all
            .iter()
            .filter(|m| !seen.contains(&m.id))
            .collect();
        rest.sort_by(|a, b| a.id.cmp(&b.id));
        for m in rest {
            rows.push((m.id.clone(), is_applied(m, &history.ids)));
        }
        Ok(rows)
    }

    fn kind(&self) -> BackendKind {
        self.db.capabilities().kind
    }

    async fn with_lock<F, Fut>(&self, f: F) -> Result<Report, MigrationError>
    where
        F: FnOnce(Db) -> Fut,
        Fut: Future<Output = Result<Report, MigrationError>>,
    {
        // SQLite holds a single `BEGIN IMMEDIATE` transaction for the whole
        // run (the transaction *is* the lock): `migrate`/`rollback` there
        // are all-or-nothing, and the per-migration `atomic` flag has no
        // effect. PostgreSQL and MySQL hold a session lock on a
        // non-transactional connection instead, and each migration commits
        // on its own.
        let transactional = self.kind() == BackendKind::Sqlite;
        self.db
            .schema_change(transactional, |session| async move {
                acquire_migrate_lock(&session).await?;
                let result = f(session.clone()).await;
                let unlocked = release_migrate_lock(&session).await;
                match (result, unlocked) {
                    (Ok(v), Ok(())) => Ok(v),
                    (Err(e), _) => Err(e),
                    (Ok(_), Err(e)) => Err(e),
                }
            })
            .await
    }

    fn require_backend(&self) -> Result<(), MigrationError> {
        match self.kind() {
            BackendKind::Postgres | BackendKind::Sqlite | BackendKind::MySql => Ok(()),
            other => Err(MigrationError::UnsupportedBackend(other)),
        }
    }

    fn require_migration(&self, id: &str) -> Result<&Migration, MigrationError> {
        self.graph
            .get(id)
            .ok_or_else(|| MigrationError::usage(format!("unknown migration `{id}`")))
    }

    /// Render `plan` to SQL, replaying project state from `history`.
    ///
    /// Returns the statements and the pre-plan state (callers feed it to
    /// `apply_one`, which advances it migration by migration so rendering
    /// and execution agree).
    fn plan_sql(
        &self,
        history: &History,
        plan: &[String],
    ) -> Result<(Vec<String>, ProjectState), MigrationError> {
        let state = replay(self.graph, history)?;
        let mut render = state.clone();
        let mut sql = Vec::new();
        if !history.table_exists {
            sql.push(create_history_sql(self.kind()));
        }
        for id in plan {
            let migration = self.require_migration(id)?;
            sql.extend(schema_editor::statements(
                self.kind(),
                &render,
                &migration.operations,
            )?);
            for op in &migration.operations {
                op.apply_to_state(&mut render)?;
            }
        }
        Ok((sql, state))
    }

    /// Render reverse SQL for `plan` from `history`.
    fn rollback_sql(
        &self,
        history: &History,
        plan: &[String],
    ) -> Result<Vec<String>, MigrationError> {
        let mut sql = Vec::new();
        for id in plan {
            let migration = self.require_migration(id)?;
            let state_before = state_before_migration(self.graph, history, id)?;
            sql.extend(reverse_sql(self.kind(), migration, &state_before)?);
        }
        Ok(sql)
    }

    fn forward_plan(
        &self,
        history: &History,
        target: Option<&str>,
    ) -> Result<Vec<String>, MigrationError> {
        if let Some(id) = target
            && self.graph.get(id).is_none()
        {
            return Err(MigrationError::usage(format!("unknown migration `{id}`")));
        }
        let mut plan = Vec::new();
        for id in &self.graph.order {
            let migration = self.require_migration(id)?;
            if is_applied(migration, &history.ids) {
                if target == Some(id.as_str()) {
                    return Ok(plan);
                }
                continue;
            }
            plan.push(id.clone());
            if target == Some(id.as_str()) {
                break;
            }
        }
        if let Some(id) = target
            && !plan.iter().any(|p| p == id)
            && !history.ids.contains(id)
            && !self
                .graph
                .get(id)
                .is_some_and(|m| is_applied(m, &history.ids))
        {
            // Target is a replaced id: applying the squash (already in plan or applied)
            // is enough; if the squash is not in the visible graph, error.
            if self.graph.replaced_by.contains_key(id) {
                return Ok(plan);
            }
            return Err(MigrationError::usage(format!(
                "migration `{id}` is not reachable from the current graph"
            )));
        }
        Ok(plan)
    }

    fn rollback_plan(
        &self,
        history: &History,
        target: Option<&str>,
        steps: Option<u32>,
    ) -> Result<Vec<String>, MigrationError> {
        let applied_visible: Vec<String> = self
            .graph
            .order
            .iter()
            .filter(|id| {
                self.graph
                    .get(id)
                    .is_some_and(|m| is_applied(m, &history.ids))
            })
            .cloned()
            .collect();
        if let Some(n) = steps {
            let n = n as usize;
            let start = applied_visible.len().saturating_sub(n);
            let mut plan = applied_visible[start..].to_vec();
            plan.reverse();
            return Ok(plan);
        }
        let Some(target) = target else {
            return Err(MigrationError::usage(
                "rollback requires --steps N or a target id",
            ));
        };
        if !applied_visible.iter().any(|id| id == target) && !history.ids.contains(target) {
            return Err(MigrationError::usage(format!(
                "migration `{target}` is not applied"
            )));
        }
        // Unapply everything after `target` (target stays applied).
        let mut plan = Vec::new();
        let mut after = false;
        for id in &applied_visible {
            if after {
                plan.push(id.clone());
            }
            if id == target {
                after = true;
            }
        }
        plan.reverse();
        Ok(plan)
    }

    fn verify_checksums(&self, history: &History) -> Result<(), MigrationError> {
        for migration in &self.graph.all {
            if let Some(stored) = history.checksums.get(&migration.id)
                && stored != &migration.checksum
            {
                return Err(MigrationError::ChecksumMismatch {
                    id: migration.id.clone(),
                    history: stored.clone(),
                    file: migration.checksum.clone(),
                });
            }
        }
        Ok(())
    }

    async fn load_history(&self) -> Result<History, MigrationError> {
        self.load_history_from(self.db).await
    }

    async fn load_history_from(&self, db: &Db) -> Result<History, MigrationError> {
        if !self.history_table_exists_on(db).await? {
            return Ok(History {
                ids: HashSet::new(),
                checksums: HashMap::new(),
                table_exists: false,
            });
        }
        // Any failure past this point (permissions, a dropped connection, a
        // damaged table) must surface: treating it as an empty history would
        // replay every migration against a live database.
        let result = db
            .raw_sql(
                &format!(
                    "SELECT {} as id, {} as checksum FROM {}",
                    quote_star(self.kind(), "id"),
                    quote_star(self.kind(), "checksum"),
                    quote_star(self.kind(), HISTORY_TABLE)
                ),
                Vec::new(),
            )
            .await?;
        let mut ids = HashSet::new();
        let mut checksums = HashMap::new();
        for row in result.rows {
            let Some(Value::Text(id)) = row.get("id") else {
                return Err(MigrationError::state(format!(
                    "`{HISTORY_TABLE}` has a row without a text `id`"
                )));
            };
            let checksum = match row.get("checksum") {
                Some(Value::Text(s)) => s.clone(),
                _ => String::new(),
            };
            ids.insert(id.clone());
            checksums.insert(id.clone(), checksum);
        }
        Ok(History {
            ids,
            checksums,
            table_exists: true,
        })
    }

    /// Ask the catalog whether the history table exists, so a failing read
    /// of the table itself is never mistaken for a fresh database.
    async fn history_table_exists_on(&self, db: &Db) -> Result<bool, MigrationError> {
        let sql = match self.kind() {
            BackendKind::Sqlite => format!(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '{HISTORY_TABLE}'"
            ),
            BackendKind::Postgres => format!(
                "SELECT 1 FROM information_schema.tables \
                 WHERE table_schema = current_schema() AND table_name = '{HISTORY_TABLE}'"
            ),
            BackendKind::MySql => format!(
                "SELECT 1 FROM information_schema.tables \
                 WHERE table_schema = DATABASE() AND table_name = '{HISTORY_TABLE}'"
            ),
            other => return Err(MigrationError::UnsupportedBackend(other)),
        };
        Ok(!db.raw_sql(&sql, Vec::new()).await?.rows.is_empty())
    }

    async fn apply_one(
        &self,
        db: &Db,
        migration: &Migration,
        state: &mut ProjectState,
    ) -> Result<(), MigrationError> {
        let kind = self.kind();
        let migration = migration.clone();
        let start_state = state.clone();
        let run = |db: Db| {
            let migration = migration.clone();
            let registry = self.registry.clone();
            let start_state = start_state.clone();
            async move {
                apply_ops_in_order(&db, kind, &migration, &start_state, &registry).await?;
                Ok::<_, MigrationError>(())
            }
        };
        // Migrations always run under `with_lock`. On SQLite that holds one
        // transaction for the whole run (the lock), so this branch always
        // hits; on other backends each migration gets its own transaction
        // when `runs_in_transaction`.
        if db.in_transaction() {
            run(db.clone()).await?;
        } else if runs_in_transaction(db, &migration) {
            db.transaction(run).await?;
        } else {
            run(db.clone()).await?;
        }
        for op in &migration.operations {
            op.apply_to_state(state)?;
        }
        Ok(())
    }

    async fn unapply_one(
        &self,
        db: &Db,
        migration: &Migration,
        state_before: &ProjectState,
    ) -> Result<(), MigrationError> {
        let kind = self.kind();
        let operations = migration.operations.clone();
        let id = migration.id.clone();
        let checksum = migration.checksum.clone();
        let start_state = state_before.clone();
        let run = |db: Db| {
            let operations = operations.clone();
            let registry = self.registry.clone();
            let id = id.clone();
            let checksum = checksum.clone();
            let start_state = start_state.clone();
            async move {
                unapply_ops_in_order(
                    &db,
                    kind,
                    &id,
                    &checksum,
                    &operations,
                    &start_state,
                    &registry,
                )
                .await?;
                Ok::<_, MigrationError>(())
            }
        };
        // See `apply_one`: SQLite always arrives inside the run-wide
        // transaction held by `with_lock`.
        if db.in_transaction() {
            run(db.clone()).await?;
        } else if runs_in_transaction(db, migration) {
            db.transaction(run).await?;
        } else {
            run(db.clone()).await?;
        }
        Ok(())
    }
}

struct History {
    ids: HashSet<String>,
    checksums: HashMap<String, String>,
    table_exists: bool,
}

fn quote_star(kind: BackendKind, ident: &str) -> String {
    schema_editor::quote_identifier(kind, ident)
}

/// PostgreSQL `pg_advisory_lock` key (`SIDE` in ASCII).
const PG_MIGRATE_LOCK: i64 = 0x5349_4445;
const MYSQL_LOCK_NAME: &str = "siderite_migrate";
/// MySQL `GET_LOCK` timeout in seconds. Negative waits indefinitely, matching
/// `pg_advisory_lock`, so a slow migration never makes other replicas fail
/// with "timed out" and crash-loop. The lock is session-held and released on
/// disconnect, so an indefinite wait cannot wedge forever on a dead holder.
const MYSQL_LOCK_TIMEOUT_SECS: i64 = -1;

async fn acquire_migrate_lock(db: &Db) -> Result<(), MigrationError> {
    match db.capabilities().kind {
        BackendKind::Sqlite => Ok(()),
        BackendKind::Postgres => {
            // `pg_advisory_lock` returns `void`, which sqlx cannot decode:
            // wrap it in a subquery that yields a decodable value.
            db.raw_sql(
                "SELECT 1 AS locked FROM (SELECT pg_advisory_lock($1)) AS taken",
                vec![Value::Int(PG_MIGRATE_LOCK)],
            )
            .await?;
            Ok(())
        }
        BackendKind::MySql => {
            let result = db
                .raw_sql(
                    "SELECT GET_LOCK(?, ?) AS acquired",
                    vec![
                        Value::Text(MYSQL_LOCK_NAME.into()),
                        Value::Int(MYSQL_LOCK_TIMEOUT_SECS),
                    ],
                )
                .await?;
            match result.rows.first().and_then(|row| row.get("acquired")) {
                Some(Value::Int(1)) => Ok(()),
                Some(Value::Int(0)) => Err(MigrationError::state(
                    "timed out waiting for the migration lock",
                )),
                other => Err(MigrationError::state(format!(
                    "GET_LOCK returned {other:?}"
                ))),
            }
        }
        other => Err(MigrationError::UnsupportedBackend(other)),
    }
}

async fn release_migrate_lock(db: &Db) -> Result<(), MigrationError> {
    match db.capabilities().kind {
        BackendKind::Sqlite => Ok(()),
        BackendKind::Postgres => {
            // `pg_advisory_unlock` returns `bool`, which decodes fine.
            let result = db
                .raw_sql(
                    "SELECT pg_advisory_unlock($1) AS released",
                    vec![Value::Int(PG_MIGRATE_LOCK)],
                )
                .await?;
            match result.rows.first().and_then(|row| row.get("released")) {
                Some(Value::Bool(true)) | Some(Value::Int(1)) => Ok(()),
                // False means the lock was not held by this session, which
                // only happens if the connection was replaced mid-run.
                other => Err(MigrationError::state(format!(
                    "the migration lock was not held on release: {other:?}"
                ))),
            }
        }
        BackendKind::MySql => {
            db.raw_sql(
                "SELECT RELEASE_LOCK(?)",
                vec![Value::Text(MYSQL_LOCK_NAME.into())],
            )
            .await?;
            Ok(())
        }
        other => Err(MigrationError::UnsupportedBackend(other)),
    }
}

/// Whether one migration can be wrapped in a transaction that also undoes
/// its DDL. MySQL commits implicitly on every DDL statement, so wrapping it
/// would only suggest an atomicity the database cannot give.
fn runs_in_transaction(db: &Db, migration: &Migration) -> bool {
    let caps = db.capabilities();
    migration.atomic
        && caps.transactions > TransactionSupport::None
        && caps.kind != BackendKind::MySql
}

fn create_history_sql(kind: BackendKind) -> String {
    // MySQL cannot key a TEXT column without a prefix length.
    let key_type = if kind == BackendKind::MySql {
        "VARCHAR(255)"
    } else {
        "TEXT"
    };
    format!(
        "CREATE TABLE IF NOT EXISTS {} (\n  {} {key_type} PRIMARY KEY,\n  {} TEXT NOT NULL,\n  {} TEXT NOT NULL\n)",
        quote_star(kind, HISTORY_TABLE),
        quote_star(kind, "id"),
        quote_star(kind, "checksum"),
        quote_star(kind, "applied_at"),
    )
}

/// Progress of a non-transactional migration, so a re-run resumes after the
/// last completed *statement* instead of replaying committed statements.
///
/// MySQL commits every DDL statement implicitly, so a failed migration
/// leaves earlier statements applied and no history row. The next `migrate`
/// reads this row, skips the operations it records and, inside the operation
/// it stopped in, skips the statements already committed — one operation can
/// render several (a `CreateModel` with indexes, an `AlterField` changing
/// type *and* an index), and replaying those fails with "already exists".
/// Directions are tracked separately because forward and reverse walk
/// different operation lists.
const PROGRESS_TABLE: &str = "siderite_migration_progress";
const PROGRESS_APPLY: &str = "apply";
const PROGRESS_UNAPPLY: &str = "unapply";

/// How far a re-run has to skip: `ops` operations are complete and `stmts`
/// statements of operation `ops` are already committed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Progress {
    ops: usize,
    stmts: usize,
}

fn progress_ddl(kind: BackendKind) -> String {
    // MySQL cannot key a TEXT column without a prefix length.
    let id_type = if kind == BackendKind::MySql {
        "VARCHAR(255)"
    } else {
        "TEXT"
    };
    let dir_type = if kind == BackendKind::MySql {
        "VARCHAR(16)"
    } else {
        "TEXT"
    };
    format!(
        "CREATE TABLE IF NOT EXISTS {} ({} {id_type} PRIMARY KEY, {} INTEGER NOT NULL, {} INTEGER NOT NULL DEFAULT 0, {} {dir_type} NOT NULL, {} {id_type} NOT NULL)",
        quote_star(kind, PROGRESS_TABLE),
        quote_star(kind, "migration_id"),
        quote_star(kind, "op_index"),
        quote_star(kind, "stmt_index"),
        quote_star(kind, "direction"),
        quote_star(kind, "checksum"),
    )
}

/// Drop a progress row left by the other direction: a half-applied forward
/// run must not skip reverse operations, or vice versa.
async fn reset_progress_direction(
    db: &Db,
    kind: BackendKind,
    id: &str,
    direction: &str,
) -> Result<(), MigrationError> {
    db.execute_script(&progress_ddl(kind)).await?;
    db.raw_execute(
        &format!(
            "DELETE FROM {} WHERE {} = {} AND {} <> {}",
            quote_star(kind, PROGRESS_TABLE),
            quote_star(kind, "migration_id"),
            placeholder(kind, 1),
            quote_star(kind, "direction"),
            placeholder(kind, 2),
        ),
        vec![
            Value::Text(id.to_owned()),
            Value::Text(direction.to_owned()),
        ],
    )
    .await?;
    Ok(())
}

/// Completed-statement progress for `id`/`direction` (zero when no row).
///
/// The row records the checksum of the migration that wrote it. Indices into
/// an edited migration point at different operations, so a mismatch refuses
/// to resume instead of skipping statements that never ran.
async fn load_progress(
    db: &Db,
    kind: BackendKind,
    id: &str,
    checksum: &str,
    direction: &str,
) -> Result<Progress, MigrationError> {
    let rows = db
        .raw_sql(
            &format!(
                "SELECT {}, {}, {} FROM {} WHERE {} = {} AND {} = {}",
                quote_star(kind, "op_index"),
                quote_star(kind, "stmt_index"),
                quote_star(kind, "checksum"),
                quote_star(kind, PROGRESS_TABLE),
                quote_star(kind, "migration_id"),
                placeholder(kind, 1),
                quote_star(kind, "direction"),
                placeholder(kind, 2),
            ),
            vec![
                Value::Text(id.to_owned()),
                Value::Text(direction.to_owned()),
            ],
        )
        .await?;
    let Some(row) = rows.rows.first() else {
        return Ok(Progress::default());
    };
    let stored = match row.get("checksum") {
        Some(Value::Text(stored)) => stored.as_str(),
        _ => "",
    };
    if stored != checksum {
        return Err(MigrationError::state(format!(
            "migration `{id}` changed since a partial {direction} stopped \
             (progress checksum {stored:?}, file {checksum:?}); restore the \
             original file, or reconcile the schema by hand and delete its \
             row from `{PROGRESS_TABLE}`"
        )));
    }
    Ok(Progress {
        ops: int_at(row, "op_index"),
        stmts: int_at(row, "stmt_index"),
    })
}

fn int_at(row: &siderite_orm::Row, key: &str) -> usize {
    match row.get(key) {
        Some(Value::Int(n)) => (*n).max(0) as usize,
        _ => 0,
    }
}

/// Record `progress` for `id`/`direction`. Called after every statement, so a
/// failure inside a multi-statement operation resumes at the next statement
/// rather than replaying the ones MySQL already committed.
async fn save_progress(
    db: &Db,
    kind: BackendKind,
    id: &str,
    checksum: &str,
    direction: &str,
    progress: Progress,
) -> Result<(), MigrationError> {
    // The migrator holds the backend lock, so no concurrent writer can race
    // the UPDATE-then-INSERT.
    let updated = db
        .raw_execute(
            &format!(
                "UPDATE {} SET {} = {}, {} = {} WHERE {} = {} AND {} = {}",
                quote_star(kind, PROGRESS_TABLE),
                quote_star(kind, "op_index"),
                placeholder(kind, 1),
                quote_star(kind, "stmt_index"),
                placeholder(kind, 2),
                quote_star(kind, "migration_id"),
                placeholder(kind, 3),
                quote_star(kind, "direction"),
                placeholder(kind, 4),
            ),
            vec![
                Value::Int(progress.ops as i64),
                Value::Int(progress.stmts as i64),
                Value::Text(id.to_owned()),
                Value::Text(direction.to_owned()),
            ],
        )
        .await?;
    // MySQL reports *changed* rows, not matched ones, so an UPDATE writing the
    // values already stored returns 0. Upsert there so that case cannot turn
    // into a duplicate-key INSERT. Keyed on the connection, not the dialect:
    // unit tests drive the MySQL path over SQLite.
    if updated == 0 {
        let upsert = if db.capabilities().kind == BackendKind::MySql {
            format!(
                " ON DUPLICATE KEY UPDATE {op} = VALUES({op}), {stmt} = VALUES({stmt})",
                op = quote_star(kind, "op_index"),
                stmt = quote_star(kind, "stmt_index"),
            )
        } else {
            String::new()
        };
        db.raw_execute(
            &format!(
                "INSERT INTO {} ({}, {}, {}, {}, {}) VALUES ({}, {}, {}, {}, {}){upsert}",
                quote_star(kind, PROGRESS_TABLE),
                quote_star(kind, "migration_id"),
                quote_star(kind, "op_index"),
                quote_star(kind, "stmt_index"),
                quote_star(kind, "direction"),
                quote_star(kind, "checksum"),
                placeholder(kind, 1),
                placeholder(kind, 2),
                placeholder(kind, 3),
                placeholder(kind, 4),
                placeholder(kind, 5),
            ),
            vec![
                Value::Text(id.to_owned()),
                Value::Int(progress.ops as i64),
                Value::Int(progress.stmts as i64),
                Value::Text(direction.to_owned()),
                Value::Text(checksum.to_owned()),
            ],
        )
        .await?;
    }
    Ok(())
}

async fn clear_progress(
    db: &Db,
    kind: BackendKind,
    id: &str,
    direction: &str,
) -> Result<(), MigrationError> {
    db.raw_execute(
        &format!(
            "DELETE FROM {} WHERE {} = {} AND {} = {}",
            quote_star(kind, PROGRESS_TABLE),
            quote_star(kind, "migration_id"),
            placeholder(kind, 1),
            quote_star(kind, "direction"),
            placeholder(kind, 2),
        ),
        vec![
            Value::Text(id.to_owned()),
            Value::Text(direction.to_owned()),
        ],
    )
    .await?;
    Ok(())
}

async fn apply_ops_in_order(
    db: &Db,
    kind: BackendKind,
    migration: &Migration,
    state: &ProjectState,
    registry: &crate::registry::MigrationRegistry,
) -> Result<(), MigrationError> {
    let total_sql = schema_editor::statements(kind, state, &migration.operations)?.len();
    // MySQL commits every DDL statement implicitly, so a failure leaves
    // earlier statements applied. Track progress per statement and resume
    // after it on re-run instead of replaying committed statements: an
    // operation can render several (a `CreateModel` with indexes, an
    // `AlterField` changing type *and* an index), and replaying the first
    // ones fails with "already exists".
    let resumable = kind == BackendKind::MySql;
    let mut resume = Progress::default();
    if resumable {
        reset_progress_direction(db, kind, &migration.id, PROGRESS_APPLY).await?;
        resume =
            load_progress(db, kind, &migration.id, &migration.checksum, PROGRESS_APPLY).await?;
    }
    let mut current = state.clone();
    let mut sql_index = 0usize;
    let mut prior_ddl = false;
    // Something already committed by this or an earlier attempt, so a failure
    // leaves partial state: earlier DDL, or earlier DML from a `RunRust` that
    // cannot be rolled back.
    let mut prior_committed = resume.ops > 0 || resume.stmts > 0;
    for (op_no, op) in migration.operations.iter().enumerate() {
        if op_no < resume.ops {
            // Applied by an earlier attempt: advance state without executing.
            if !matches!(op, Operation::RunRust { .. }) {
                let stmts = schema_editor::render(kind, op, &current)?;
                prior_ddl |= stmts.iter().any(|sql| is_implicit_commit(sql));
                sql_index += stmts.len();
            }
            prior_committed = true;
            op.apply_to_state(&mut current)?;
            continue;
        }
        // Statements of this operation an earlier attempt already committed.
        let resume_stmt = if resumable && op_no == resume.ops {
            resume.stmts
        } else {
            0
        };
        if let Operation::RunRust { name, .. } = op {
            if let Err(err) = registry.run(name, db).await {
                if resumable && prior_committed {
                    return Err(MigrationError::MysqlOpPartial {
                        id: migration.id.clone(),
                        index: op_no + 1,
                        total: migration.operations.len(),
                        summary: op.summary(),
                        source: Box::new(err),
                    });
                }
                return Err(err);
            }
            prior_committed = true;
        } else {
            let stmts = schema_editor::render(kind, op, &current)?;
            for (stmt_no, sql) in stmts.into_iter().enumerate() {
                sql_index += 1;
                if stmt_no < resume_stmt {
                    // Committed by an earlier attempt: count it, do not replay.
                    prior_ddl |= is_implicit_commit(&sql);
                    prior_committed = true;
                    continue;
                }
                execute_one_sql(
                    db,
                    kind,
                    &migration.id,
                    sql_index,
                    total_sql,
                    prior_ddl,
                    &sql,
                )
                .await?;
                prior_ddl |= is_implicit_commit(&sql);
                prior_committed = true;
                // Not atomic with the statement: MySQL has already committed
                // the DDL, and no transaction can span it and this write. A
                // crash between the two replays the statement on re-run, which
                // fails loudly ("already exists"); see docs/MIGRATIONS.md for
                // manual recovery. That window is inherent to MySQL DDL.
                if resumable {
                    save_progress(
                        db,
                        kind,
                        &migration.id,
                        &migration.checksum,
                        PROGRESS_APPLY,
                        Progress {
                            ops: op_no,
                            stmts: stmt_no + 1,
                        },
                    )
                    .await?;
                }
            }
            op.apply_to_state(&mut current)?;
        }
        if resumable {
            save_progress(
                db,
                kind,
                &migration.id,
                &migration.checksum,
                PROGRESS_APPLY,
                Progress {
                    ops: op_no + 1,
                    stmts: 0,
                },
            )
            .await?;
        }
    }
    record_history(db, kind, migration).await?;
    if resumable {
        clear_progress(db, kind, &migration.id, PROGRESS_APPLY).await?;
    }
    Ok(())
}

async fn unapply_ops_in_order(
    db: &Db,
    kind: BackendKind,
    id: &str,
    checksum: &str,
    operations: &[Operation],
    state_before: &ProjectState,
    registry: &crate::registry::MigrationRegistry,
) -> Result<(), MigrationError> {
    let mut snapshots = vec![state_before.clone()];
    let mut current = state_before.clone();
    for op in operations {
        op.apply_to_state(&mut current)?;
        snapshots.push(current.clone());
    }
    current = snapshots
        .last()
        .cloned()
        .unwrap_or_else(|| state_before.clone());
    let mut rev_ops = Vec::new();
    for i in (0..operations.len()).rev() {
        let Some(rev) = operations[i].reverse(&snapshots[i]) else {
            return Err(MigrationError::Irreversible {
                id: id.to_owned(),
                reason: format!("{} has no reverse", operations[i].summary()),
            });
        };
        rev_ops.push(rev);
    }
    let total_sql = {
        let mut n = 0;
        let mut walk = current.clone();
        for rev in &rev_ops {
            n += schema_editor::render(kind, rev, &walk)?.len();
            rev.apply_to_state(&mut walk)?;
        }
        n
    };
    let mut sql_index = 0usize;
    let resumable = kind == BackendKind::MySql;
    let mut resume = Progress::default();
    if resumable {
        reset_progress_direction(db, kind, id, PROGRESS_UNAPPLY).await?;
        resume = load_progress(db, kind, id, checksum, PROGRESS_UNAPPLY).await?;
    }
    let mut prior_ddl = false;
    // See `apply_ops_in_order`: earlier DDL or an earlier `RunRust` that
    // committed data already left partial state behind.
    let mut prior_committed = resume.ops > 0 || resume.stmts > 0;
    for (op_no, rev) in rev_ops.into_iter().enumerate() {
        if op_no < resume.ops {
            if !matches!(rev, Operation::RunRust { .. }) {
                let stmts = schema_editor::render(kind, &rev, &current)?;
                prior_ddl |= stmts.iter().any(|sql| is_implicit_commit(sql));
                sql_index += stmts.len();
            }
            prior_committed = true;
            rev.apply_to_state(&mut current)?;
            continue;
        }
        // Statements of this reverse operation an earlier attempt committed.
        let resume_stmt = if resumable && op_no == resume.ops {
            resume.stmts
        } else {
            0
        };
        if let Operation::RunRust { name, .. } = &rev {
            if let Err(err) = registry.run(name, db).await {
                if resumable && prior_committed {
                    return Err(MigrationError::MysqlOpPartial {
                        id: id.to_owned(),
                        index: op_no + 1,
                        total: operations.len(),
                        summary: rev.summary(),
                        source: Box::new(err),
                    });
                }
                return Err(err);
            }
            prior_committed = true;
        } else {
            let stmts = schema_editor::render(kind, &rev, &current)?;
            for (stmt_no, sql) in stmts.into_iter().enumerate() {
                sql_index += 1;
                if stmt_no < resume_stmt {
                    prior_ddl |= is_implicit_commit(&sql);
                    prior_committed = true;
                    continue;
                }
                execute_one_sql(db, kind, id, sql_index, total_sql, prior_ddl, &sql).await?;
                prior_ddl |= is_implicit_commit(&sql);
                prior_committed = true;
                if resumable {
                    save_progress(
                        db,
                        kind,
                        id,
                        checksum,
                        PROGRESS_UNAPPLY,
                        Progress {
                            ops: op_no,
                            stmts: stmt_no + 1,
                        },
                    )
                    .await?;
                }
            }
        }
        rev.apply_to_state(&mut current)?;
        if resumable {
            save_progress(
                db,
                kind,
                id,
                checksum,
                PROGRESS_UNAPPLY,
                Progress {
                    ops: op_no + 1,
                    stmts: 0,
                },
            )
            .await?;
        }
    }
    delete_history(db, kind, id).await?;
    if resumable {
        clear_progress(db, kind, id, PROGRESS_UNAPPLY).await?;
    }
    Ok(())
}

async fn execute_one_sql(
    db: &Db,
    kind: BackendKind,
    migration_id: &str,
    index: usize,
    total: usize,
    prior_ddl: bool,
    sql: &str,
) -> Result<(), MigrationError> {
    if let Err(err) = db.execute_script(sql).await {
        // MySQL commits DDL implicitly: with earlier DDL committed, a
        // re-run replays it and gets stuck, so name the failed statement.
        // Without prior DDL (a first-statement failure, or only DML before
        // it) the plain error is accurate.
        if kind == BackendKind::MySql && prior_ddl {
            return Err(MigrationError::MysqlPartial {
                id: migration_id.to_owned(),
                index,
                total,
                source: err,
            });
        }
        return Err(err.into());
    }
    Ok(())
}

/// Whether `sql` is (probably) DDL that MySQL commits implicitly, so a
/// later failure leaves it applied. Matches the statement's first keyword,
/// skipping whitespace and `--` / `/* */` comments.
fn is_implicit_commit(sql: &str) -> bool {
    matches!(
        first_keyword(sql).as_str(),
        "CREATE" | "ALTER" | "DROP" | "TRUNCATE" | "RENAME"
    )
}

fn first_keyword(sql: &str) -> String {
    let mut rest = sql.trim_start();
    loop {
        if let Some(body) = rest.strip_prefix("--") {
            rest = body
                .split_once('\n')
                .map(|(_, tail)| tail)
                .unwrap_or("")
                .trim_start();
        } else if let Some(body) = rest.strip_prefix("/*") {
            rest = body
                .split_once("*/")
                .map(|(_, tail)| tail)
                .unwrap_or("")
                .trim_start();
        } else {
            break;
        }
    }
    rest.split(|c: char| !c.is_ascii_alphabetic())
        .next()
        .unwrap_or("")
        .to_ascii_uppercase()
}

fn placeholder(kind: BackendKind, index: usize) -> String {
    match kind {
        BackendKind::Postgres => format!("${index}"),
        _ => "?".to_owned(),
    }
}

async fn record_history(
    db: &Db,
    kind: BackendKind,
    migration: &Migration,
) -> Result<(), MigrationError> {
    let sql = format!(
        "INSERT INTO {} ({}, {}, {}) VALUES ({}, {}, {})",
        quote_star(kind, HISTORY_TABLE),
        quote_star(kind, "id"),
        quote_star(kind, "checksum"),
        quote_star(kind, "applied_at"),
        placeholder(kind, 1),
        placeholder(kind, 2),
        placeholder(kind, 3),
    );
    let applied_at = Utc::now().format(TIMESTAMP_FORMAT).to_string();
    db.raw_execute(
        &sql,
        vec![
            Value::Text(migration.id.clone()),
            Value::Text(migration.checksum.clone()),
            Value::Text(applied_at),
        ],
    )
    .await?;
    Ok(())
}

async fn delete_history(db: &Db, kind: BackendKind, id: &str) -> Result<(), MigrationError> {
    let sql = format!(
        "DELETE FROM {} WHERE {} = {}",
        quote_star(kind, HISTORY_TABLE),
        quote_star(kind, "id"),
        placeholder(kind, 1),
    );
    db.raw_execute(&sql, vec![Value::Text(id.to_owned())])
        .await?;
    Ok(())
}

fn replay(graph: &MigrationGraph, history: &History) -> Result<ProjectState, MigrationError> {
    let mut state = ProjectState::new();
    for id in &graph.order {
        let Some(migration) = graph.get(id) else {
            continue;
        };
        if !is_applied(migration, &history.ids) {
            continue;
        }
        for op in &migration.operations {
            op.apply_to_state(&mut state)?;
        }
    }
    Ok(state)
}

fn state_before_migration(
    graph: &MigrationGraph,
    history: &History,
    id: &str,
) -> Result<ProjectState, MigrationError> {
    let mut state = ProjectState::new();
    for vis in &graph.order {
        if vis == id {
            break;
        }
        let Some(migration) = graph.get(vis) else {
            continue;
        };
        if !is_applied(migration, &history.ids) {
            continue;
        }
        for op in &migration.operations {
            op.apply_to_state(&mut state)?;
        }
    }
    Ok(state)
}

fn reverse_sql(
    kind: BackendKind,
    migration: &Migration,
    state_before: &ProjectState,
) -> Result<Vec<String>, MigrationError> {
    let mut snapshots = vec![state_before.clone()];
    let mut current = state_before.clone();
    for op in &migration.operations {
        op.apply_to_state(&mut current)?;
        snapshots.push(current.clone());
    }
    let mut sql = Vec::new();
    current = snapshots
        .last()
        .cloned()
        .unwrap_or_else(|| state_before.clone());
    for i in (0..migration.operations.len()).rev() {
        let op = &migration.operations[i];
        let Some(rev) = op.reverse(&snapshots[i]) else {
            return Err(MigrationError::Irreversible {
                id: migration.id.clone(),
                reason: format!("{} has no reverse", op.summary()),
            });
        };
        sql.extend(schema_editor::render(kind, &rev, &current)?);
        rev.apply_to_state(&mut current)?;
    }
    Ok(sql)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::state::{FieldState, ModelState, SqlType};
    use siderite_backends::sqlite::SqliteBackend;

    async fn memory_db() -> Db {
        Db::new(SqliteBackend::connect("sqlite::memory:").await.unwrap())
    }

    fn run_sql_migration(id: &str, bodies: &[(&str, &str)]) -> Migration {
        Migration::new(
            id,
            Vec::new(),
            bodies
                .iter()
                .map(|(sql, reverse)| Operation::RunSQL {
                    sql: (*sql).to_owned(),
                    reverse_sql: Some((*reverse).to_owned()),
                })
                .collect(),
            true,
            Vec::new(),
        )
        .unwrap()
    }

    /// A failed MySQL migration resumes after its committed operations.
    ///
    /// `kind` is MySQL to exercise the resume path, but the statements run
    /// on SQLite: `RunSQL` passes through and the MySQL-dialect
    /// progress/history SQL (backticks, `VARCHAR`) is valid SQLite too.
    #[tokio::test]
    async fn mysql_resume_skips_committed_operations() {
        let db = memory_db().await;
        // `migrate` creates this before applying; direct `apply_ops_in_order`
        // calls must set it up themselves.
        db.execute_script(&create_history_sql(BackendKind::MySql))
            .await
            .unwrap();
        let migration = run_sql_migration(
            "0001_t",
            &[
                ("CREATE TABLE t (n INTEGER)", "DROP TABLE t"),
                ("CREATE TABLE u (n INTEGER)", "DROP TABLE u"),
                ("CREATE TABLE u (n INTEGER)", "DROP TABLE u"),
            ],
        );
        let state = ProjectState::new();
        let registry = crate::registry::MigrationRegistry::new();
        let err = apply_ops_in_order(&db, BackendKind::MySql, &migration, &state, &registry)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                MigrationError::MysqlPartial {
                    index: 3,
                    total: 3,
                    ..
                }
            ),
            "{err:?}"
        );
        // Hand-repair the half-applied tail, then re-run: ops 1-2 are
        // skipped (replaying them would fail), op 3 runs, history lands.
        db.execute_script("DROP TABLE u").await.unwrap();
        apply_ops_in_order(&db, BackendKind::MySql, &migration, &state, &registry)
            .await
            .unwrap();
        db.raw_sql("SELECT n FROM t", Vec::new()).await.unwrap();
        db.raw_sql("SELECT n FROM u", Vec::new()).await.unwrap();
        let history = db
            .raw_sql("SELECT id FROM siderite_migrations", Vec::new())
            .await
            .unwrap();
        assert_eq!(history.rows.len(), 1);
        let progress = load_progress(
            &db,
            BackendKind::MySql,
            "0001_t",
            &migration.checksum,
            PROGRESS_APPLY,
        )
        .await
        .unwrap();
        assert_eq!(progress, Progress::default());
    }

    /// A progress row written by a different version of the migration must
    /// not be resumed: its indices point at other operations, so skipping
    /// them would silently leave statements unapplied.
    #[tokio::test]
    async fn mysql_resume_refuses_edited_migration() {
        let db = memory_db().await;
        db.execute_script(&create_history_sql(BackendKind::MySql))
            .await
            .unwrap();
        let original = run_sql_migration(
            "0005_edit",
            &[
                ("CREATE TABLE a (n INTEGER)", "DROP TABLE a"),
                ("CREATE TABLE a (n INTEGER)", "DROP TABLE a"),
            ],
        );
        let state = ProjectState::new();
        let registry = crate::registry::MigrationRegistry::new();
        apply_ops_in_order(&db, BackendKind::MySql, &original, &state, &registry)
            .await
            .unwrap_err();
        let edited = run_sql_migration(
            "0005_edit",
            &[
                ("CREATE TABLE b (n INTEGER)", "DROP TABLE b"),
                ("CREATE TABLE c (n INTEGER)", "DROP TABLE c"),
            ],
        );
        assert_ne!(original.checksum, edited.checksum);
        let err = apply_ops_in_order(&db, BackendKind::MySql, &edited, &state, &registry)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("changed since a partial"), "{err}");
        // Nothing from the edited file ran: `b` would exist otherwise.
        assert!(db.raw_sql("SELECT n FROM b", Vec::new()).await.is_err());
    }

    /// Progress is per *statement*, not per operation: one operation that
    /// renders several statements must resume at the next statement, or the
    /// re-run replays the ones MySQL already committed and fails with
    /// "already exists".
    #[tokio::test]
    async fn mysql_resume_skips_committed_statements_of_one_operation() {
        let db = memory_db().await;
        db.execute_script(&create_history_sql(BackendKind::MySql))
            .await
            .unwrap();
        // One `CreateModel` operation rendering two statements: the table and
        // an auto index for `code`. `mm_code_idx` already exists on `clash`,
        // so statement 2 fails on a live server (index names are global).
        // MySQL-dialect SQL runs on SQLite here, so no `AUTO_INCREMENT`.
        let pk = || FieldState {
            primary_key: true,
            ..FieldState::new("id", "id", SqlType::BigInt)
        };
        let model = ModelState {
            name: "Multi".into(),
            table: "mm".into(),
            fields: vec![
                pk(),
                FieldState {
                    index: true,
                    ..FieldState::new("code", "code", SqlType::BigInt)
                },
            ],
            indexes: Vec::new(),
            constraints: Vec::new(),
        };
        let migration = Migration::new(
            "0004_multi",
            Vec::new(),
            vec![Operation::CreateModel { model }],
            true,
            Vec::new(),
        )
        .unwrap();
        db.execute_script(
            "CREATE TABLE clash (n INTEGER);\n\
             CREATE INDEX mm_code_idx ON clash (n)",
        )
        .await
        .unwrap();
        let state = ProjectState::new();
        let registry = crate::registry::MigrationRegistry::new();
        let err = apply_ops_in_order(&db, BackendKind::MySql, &migration, &state, &registry)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                MigrationError::MysqlPartial {
                    index: 2,
                    total: 2,
                    ..
                }
            ),
            "{err:?}"
        );
        let progress = load_progress(
            &db,
            BackendKind::MySql,
            "0004_multi",
            &migration.checksum,
            PROGRESS_APPLY,
        )
        .await
        .unwrap();
        assert_eq!(progress.ops, 0, "the operation is not finished");
        assert_eq!(progress.stmts, 1, "its first statement is committed");

        // The re-run must not replay `CREATE TABLE mm`: hand-fix statement 2
        // only and leave the committed table in place.
        db.raw_sql("SELECT id FROM mm", Vec::new()).await.unwrap();
        db.execute_script("DROP INDEX mm_code_idx").await.unwrap();
        apply_ops_in_order(&db, BackendKind::MySql, &migration, &state, &registry)
            .await
            .unwrap();
        let index = db
            .raw_sql(
                "SELECT name FROM sqlite_master WHERE type = 'index' AND name = 'mm_code_idx'",
                Vec::new(),
            )
            .await
            .unwrap();
        assert_eq!(index.rows.len(), 1, "{index:?}");
        let progress = load_progress(
            &db,
            BackendKind::MySql,
            "0004_multi",
            &migration.checksum,
            PROGRESS_APPLY,
        )
        .await
        .unwrap();
        assert_eq!(progress, Progress::default());
    }

    /// A `RunRust` failure after committed statements is a partial failure
    /// that records progress; a `RunRust` failure with nothing committed
    /// passes through untouched.
    #[tokio::test]
    async fn mysql_rust_failure_after_ddl_is_partial() {
        let db = memory_db().await;
        let migration = Migration::new(
            "0002_data",
            Vec::new(),
            vec![
                Operation::RunSQL {
                    sql: "CREATE TABLE t (n INTEGER)".into(),
                    reverse_sql: Some("DROP TABLE t".into()),
                },
                Operation::RunRust {
                    name: "boom".into(),
                    backwards: None,
                },
            ],
            true,
            Vec::new(),
        )
        .unwrap();
        let state = ProjectState::new();
        let mut registry = crate::registry::MigrationRegistry::new();
        registry.register("boom", |_db| {
            Box::pin(
                async move { Err::<(), MigrationError>(MigrationError::usage("seed exploded")) },
            )
        });
        let err = apply_ops_in_order(&db, BackendKind::MySql, &migration, &state, &registry)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                MigrationError::MysqlOpPartial {
                    index: 2,
                    total: 2,
                    ..
                }
            ),
            "{err:?}"
        );
        assert!(err.to_string().contains("RunRust boom"), "{err:?}");
        let progress = load_progress(
            &db,
            BackendKind::MySql,
            "0002_data",
            &migration.checksum,
            PROGRESS_APPLY,
        )
        .await
        .unwrap();
        assert_eq!(progress.ops, 1);
        assert_eq!(progress.stmts, 0);

        // Nothing committed before a first-operation `RunRust`: plain error.
        let lonely = Migration::new(
            "0003_lonely",
            Vec::new(),
            vec![Operation::RunRust {
                name: "missing".into(),
                backwards: None,
            }],
            true,
            Vec::new(),
        )
        .unwrap();
        let err = apply_ops_in_order(
            &db,
            BackendKind::MySql,
            &lonely,
            &state,
            &crate::registry::MigrationRegistry::new(),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, MigrationError::UnregisteredRust(_)),
            "{err:?}"
        );
    }

    /// An earlier `RunRust` that committed data is partial state even when no
    /// DDL ran: a later `RunRust` failure must say so, not return a plain
    /// error that suggests re-running from scratch.
    #[tokio::test]
    async fn mysql_rust_failure_after_committed_rust_is_partial() {
        let db = memory_db().await;
        let migration = Migration::new(
            "0005_rust_then_boom",
            Vec::new(),
            vec![
                Operation::RunRust {
                    name: "seed".into(),
                    backwards: None,
                },
                Operation::RunRust {
                    name: "boom".into(),
                    backwards: None,
                },
            ],
            true,
            Vec::new(),
        )
        .unwrap();
        let state = ProjectState::new();
        let mut registry = crate::registry::MigrationRegistry::new();
        registry.register("seed", |db| {
            Box::pin(async move {
                db.execute_script("CREATE TABLE seeded (n INTEGER)").await?;
                Ok::<(), MigrationError>(())
            })
        });
        registry.register("boom", |_db| {
            Box::pin(
                async move { Err::<(), MigrationError>(MigrationError::usage("seed exploded")) },
            )
        });
        let err = apply_ops_in_order(&db, BackendKind::MySql, &migration, &state, &registry)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                MigrationError::MysqlOpPartial {
                    index: 2,
                    total: 2,
                    ..
                }
            ),
            "{err:?}"
        );
        db.raw_sql("SELECT n FROM seeded", Vec::new())
            .await
            .unwrap();
    }

    #[test]
    fn history_table_ddl_follows_the_dialect() {
        let pg = create_history_sql(BackendKind::Postgres);
        assert!(pg.contains("\"id\" TEXT PRIMARY KEY"), "{pg}");
        let mysql = create_history_sql(BackendKind::MySql);
        assert!(mysql.contains("`id` VARCHAR(255) PRIMARY KEY"), "{mysql}");
        assert!(mysql.contains("`siderite_migrations`"), "{mysql}");
        assert!(!mysql.contains('"'), "{mysql}");
    }

    #[test]
    fn history_statements_quote_per_backend() {
        assert_eq!(
            quote_star(BackendKind::MySql, HISTORY_TABLE),
            "`siderite_migrations`"
        );
        assert_eq!(quote_star(BackendKind::Sqlite, "id"), "\"id\"");
    }

    #[test]
    fn implicit_commit_matches_ddl_first_keyword() {
        for sql in [
            "CREATE TABLE t (n INTEGER)",
            "  alter table t add column m integer",
            "-- a comment\nDROP INDEX i",
            "/* wrapped */ TRUNCATE t",
            "rename table a to b",
        ] {
            assert!(is_implicit_commit(sql), "{sql}");
        }
        for sql in [
            "SELECT 1",
            "INSERT INTO t VALUES (1)",
            "UPDATE t SET n = 2",
            "DELETE FROM t",
            "-- only a comment",
            "",
        ] {
            assert!(!is_implicit_commit(sql), "{sql}");
        }
    }

    /// Without earlier DDL the failure is plain, even past statement 1: the
    /// "earlier DDL committed" claim would be wrong.
    #[tokio::test]
    async fn mysql_plain_error_without_prior_ddl() {
        let db = memory_db().await;
        db.execute_script(&create_history_sql(BackendKind::MySql))
            .await
            .unwrap();
        let migration = run_sql_migration(
            "0004_noddl",
            &[
                ("SELECT 1", "SELECT 1"),
                (
                    "CREATE TABLE q_that_fails (n INTEGER PRIMARY KEY, n INTEGER)",
                    "SELECT 1",
                ),
            ],
        );
        let state = ProjectState::new();
        let registry = crate::registry::MigrationRegistry::new();
        let err = apply_ops_in_order(&db, BackendKind::MySql, &migration, &state, &registry)
            .await
            .unwrap_err();
        assert!(
            !matches!(err, MigrationError::MysqlPartial { .. }),
            "no DDL committed, so no partial: {err:?}"
        );
    }
}
