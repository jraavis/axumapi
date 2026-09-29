//! Apply and reverse migrations against an [`axumapi_orm::Db`].

use crate::error::MigrationError;
use crate::loader::{MigrationGraph, is_applied};
use crate::migration::Migration;
use crate::operation::Operation;
use crate::registry::MigrationRegistry;
use crate::schema_editor;
use crate::state::ProjectState;
use axumapi_orm::types::TIMESTAMP_FORMAT;
use axumapi_orm::{BackendKind, Db, TransactionSupport, Value};
use chrono::Utc;
use std::collections::{HashMap, HashSet};

/// History table written by the migrator.
pub const HISTORY_TABLE: &str = "axumapi_migrations";

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
        let history = self.load_history().await?;
        self.verify_checksums(&history)?;
        let plan = self.forward_plan(&history, target)?;
        let mut state = replay(self.graph, &history)?;
        let mut sql = Vec::new();
        if !history.table_exists {
            sql.push(create_history_sql(self.kind()));
        }
        for id in &plan {
            let migration = self.require_migration(id)?;
            sql.extend(schema_editor::statements(
                self.kind(),
                &state,
                &migration.operations,
            )?);
            for op in &migration.operations {
                op.apply_to_state(&mut state)?;
            }
        }
        if dry_run {
            return Ok(Report {
                planned: plan,
                sql,
                applied: Vec::new(),
                dry_run: true,
            });
        }
        if !history.table_exists {
            self.db
                .execute_script(&create_history_sql(self.kind()))
                .await?;
        }
        let mut applied = Vec::new();
        let mut state = replay(self.graph, &history)?;
        for id in &plan {
            let migration = self.require_migration(id)?.clone();
            self.apply_one(&migration, &mut state).await?;
            applied.push(id.clone());
        }
        Ok(Report {
            planned: plan,
            sql,
            applied,
            dry_run: false,
        })
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
        let history = self.load_history().await?;
        self.verify_checksums(&history)?;
        let plan = self.rollback_plan(&history, target, steps)?;
        let mut sql = Vec::new();
        for id in &plan {
            let migration = self.require_migration(id)?;
            let state_before = state_before_migration(self.graph, &history, id)?;
            sql.extend(reverse_sql(self.kind(), migration, &state_before)?);
        }
        if dry_run {
            return Ok(Report {
                planned: plan,
                sql,
                applied: Vec::new(),
                dry_run: true,
            });
        }
        if !history.table_exists {
            self.db
                .execute_script(&create_history_sql(self.kind()))
                .await?;
        }
        let mut unapplied = Vec::new();
        for id in &plan {
            let migration = self.require_migration(id)?.clone();
            let state_before = state_before_migration(self.graph, &history, id)?;
            self.unapply_one(&migration, &state_before).await?;
            unapplied.push(id.clone());
        }
        Ok(Report {
            planned: plan,
            sql,
            applied: unapplied,
            dry_run: false,
        })
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
        match self
            .db
            .raw_sql(
                &format!(
                    "SELECT {} as id, {} as checksum FROM {}",
                    quote_star(self.kind(), "id"),
                    quote_star(self.kind(), "checksum"),
                    quote_star(self.kind(), HISTORY_TABLE)
                ),
                Vec::new(),
            )
            .await
        {
            Ok(result) => {
                let mut ids = HashSet::new();
                let mut checksums = HashMap::new();
                for row in result.rows {
                    let id = match row.get("id") {
                        Some(Value::Text(s)) => s.clone(),
                        _ => continue,
                    };
                    let checksum = match row.get("checksum") {
                        Some(Value::Text(s)) => s.clone(),
                        _ => String::new(),
                    };
                    ids.insert(id.clone());
                    checksums.insert(id, checksum);
                }
                Ok(History {
                    ids,
                    checksums,
                    table_exists: true,
                })
            }
            Err(_) => Ok(History {
                ids: HashSet::new(),
                checksums: HashMap::new(),
                table_exists: false,
            }),
        }
    }

    async fn apply_one(
        &self,
        migration: &Migration,
        state: &mut ProjectState,
    ) -> Result<(), MigrationError> {
        let kind = self.kind();
        let sqls = schema_editor::statements(kind, state, &migration.operations)?;
        let rust_ops: Vec<&Operation> = migration
            .operations
            .iter()
            .filter(|op| matches!(op, Operation::RunRust { .. }))
            .collect();
        let run = |db: Db| {
            let sqls = sqls.clone();
            let rust_ops = rust_ops.iter().map(|op| (*op).clone()).collect::<Vec<_>>();
            let registry = self.registry.clone();
            async move {
                for sql in &sqls {
                    db.execute_script(sql).await?;
                }
                for op in &rust_ops {
                    if let Operation::RunRust { name, .. } = op {
                        registry.run(name, &db).await?;
                    }
                }
                record_history(&db, kind, migration).await?;
                Ok::<_, MigrationError>(())
            }
        };
        if runs_in_transaction(self.db, migration) {
            self.db.transaction(run).await?;
        } else {
            run(self.db.clone()).await?;
        }
        for op in &migration.operations {
            op.apply_to_state(state)?;
        }
        Ok(())
    }

    async fn unapply_one(
        &self,
        migration: &Migration,
        state_before: &ProjectState,
    ) -> Result<(), MigrationError> {
        let kind = self.kind();
        let sqls = reverse_sql(kind, migration, state_before)?;
        let rust_ops = reverse_rust(migration, state_before)?;
        let run = |db: Db| {
            let sqls = sqls.clone();
            let rust_ops = rust_ops.clone();
            let registry = self.registry.clone();
            let id = migration.id.clone();
            async move {
                for sql in &sqls {
                    db.execute_script(sql).await?;
                }
                for name in &rust_ops {
                    registry.run(name, &db).await?;
                }
                delete_history(&db, kind, &id).await?;
                Ok::<_, MigrationError>(())
            }
        };
        if runs_in_transaction(self.db, migration) {
            self.db.transaction(run).await?;
        } else {
            run(self.db.clone()).await?;
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

fn reverse_rust(
    migration: &Migration,
    state_before: &ProjectState,
) -> Result<Vec<String>, MigrationError> {
    let mut snapshots = vec![state_before.clone()];
    let mut current = state_before.clone();
    for op in &migration.operations {
        op.apply_to_state(&mut current)?;
        snapshots.push(current.clone());
    }
    let mut names = Vec::new();
    for i in (0..migration.operations.len()).rev() {
        let op = &migration.operations[i];
        let Some(rev) = op.reverse(&snapshots[i]) else {
            return Err(MigrationError::Irreversible {
                id: migration.id.clone(),
                reason: format!("{} has no reverse", op.summary()),
            });
        };
        if let Operation::RunRust { name, .. } = rev {
            names.push(name);
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_table_ddl_follows_the_dialect() {
        let pg = create_history_sql(BackendKind::Postgres);
        assert!(pg.contains("\"id\" TEXT PRIMARY KEY"), "{pg}");
        let mysql = create_history_sql(BackendKind::MySql);
        assert!(mysql.contains("`id` VARCHAR(255) PRIMARY KEY"), "{mysql}");
        assert!(mysql.contains("`axumapi_migrations`"), "{mysql}");
        assert!(!mysql.contains('"'), "{mysql}");
    }

    #[test]
    fn history_statements_quote_per_backend() {
        assert_eq!(
            quote_star(BackendKind::MySql, HISTORY_TABLE),
            "`axumapi_migrations`"
        );
        assert_eq!(quote_star(BackendKind::Sqlite, "id"), "\"id\"");
    }
}
