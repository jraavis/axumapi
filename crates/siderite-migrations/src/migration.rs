//! A single migration file: id, dependencies, operations, checksum.

use crate::error::MigrationError;
use crate::hash::fnv1a_hex;
use crate::operation::Operation;
use serde::{Deserialize, Serialize};

/// On-disk (and in-memory) migration.
///
/// `id` is a zero-padded sequence plus a slug (`0001_initial`). `checksum` is
/// a 16-digit hex FNV-1a of the canonical JSON of id, dependencies,
/// operations, atomic and replaces (the checksum field itself is excluded).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Migration {
    /// `0001_initial`.
    pub id: String,
    /// Optional app label. Single-app projects leave this unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// Ids of migrations that must be applied first.
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// Ordered operations.
    pub operations: Vec<Operation>,
    /// Wrap execution in a transaction when the backend supports it.
    ///
    /// No effect on SQLite: the whole `migrate`/`rollback` run is already
    /// one transaction there (the lock), so a run is all-or-nothing. MySQL
    /// never wraps DDL regardless of this flag.
    #[serde(default = "default_true")]
    pub atomic: bool,
    /// Ids this squash replaces. The loader treats this migration as applied
    /// when every replaced id is in the history table.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replaces: Vec<String>,
    /// Content checksum (see module docs).
    pub checksum: String,
}

fn default_true() -> bool {
    true
}

#[derive(Serialize)]
struct ChecksumBody<'a> {
    id: &'a str,
    dependencies: &'a [String],
    operations: &'a [Operation],
    atomic: bool,
    replaces: &'a [String],
}

impl Migration {
    /// Build a migration and fill in its checksum.
    ///
    /// # Errors
    /// JSON encoding of the checksum body (does not fail for these types).
    pub fn new(
        id: impl Into<String>,
        dependencies: Vec<String>,
        operations: Vec<Operation>,
        atomic: bool,
        replaces: Vec<String>,
    ) -> Result<Self, MigrationError> {
        let mut migration = Self {
            id: id.into(),
            app: None,
            dependencies,
            operations,
            atomic,
            replaces,
            checksum: String::new(),
        };
        migration.checksum = migration.compute_checksum()?;
        Ok(migration)
    }

    /// FNV-1a of the canonical checksum body.
    ///
    /// # Errors
    /// JSON encoding failure.
    pub fn compute_checksum(&self) -> Result<String, MigrationError> {
        let body = ChecksumBody {
            id: &self.id,
            dependencies: &self.dependencies,
            operations: &self.operations,
            atomic: self.atomic,
            replaces: &self.replaces,
        };
        let bytes = serde_json::to_vec(&body)?;
        Ok(fnv1a_hex(&bytes))
    }

    /// Refresh [`checksum`](Self::checksum) from current fields.
    ///
    /// # Errors
    /// JSON encoding failure.
    pub fn refresh_checksum(&mut self) -> Result<(), MigrationError> {
        self.checksum = self.compute_checksum()?;
        Ok(())
    }

    /// Whether any operation is unconditionally irreversible.
    pub fn is_irreversible(&self) -> bool {
        self.operations
            .iter()
            .any(Operation::is_unconditionally_irreversible)
    }

    /// Leading sequence number from `0001_initial` → `1`. `None` if the id
    /// does not start with digits.
    pub fn sequence(&self) -> Option<u32> {
        parse_sequence(&self.id)
    }
}

/// Leading decimal digits of a migration id.
pub fn parse_sequence(id: &str) -> Option<u32> {
    let digits: String = id.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

/// Next id: max existing sequence + 1, zero-padded to at least 4 digits, plus `slug`.
pub fn next_id(existing: &[Migration], slug: &str) -> String {
    let max = existing
        .iter()
        .filter_map(Migration::sequence)
        .max()
        .unwrap_or(0);
    let slug = sanitize_slug(slug);
    format!("{:04}_{slug}", max.saturating_add(1))
}

/// Keep ASCII alphanumeric and underscores; everything else becomes `_`.
pub fn sanitize_slug(slug: &str) -> String {
    let mut out = String::new();
    let mut last_underscore = false;
    for ch in slug.chars() {
        let ok = ch.is_ascii_alphanumeric() || ch == '_';
        if ok {
            out.push(ch.to_ascii_lowercase());
            last_underscore = ch == '_';
        } else if !last_underscore && !out.is_empty() {
            out.push('_');
            last_underscore = true;
        }
    }
    let out = out.trim_matches('_').to_owned();
    if out.is_empty() {
        "auto".to_owned()
    } else {
        out
    }
}

/// Slug derived from the first operation (`initial`, `add_book_isbn`, …).
pub fn slug_from_operations(operations: &[Operation], is_first: bool) -> String {
    if operations.is_empty() {
        return "empty".to_owned();
    }
    if is_first
        && operations
            .iter()
            .all(|op| matches!(op, Operation::CreateModel { .. }))
    {
        return "initial".to_owned();
    }
    match &operations[0] {
        Operation::CreateModel { model } => format!("create_{}", snake(&model.name)),
        Operation::DeleteModel { name } => format!("delete_{}", snake(name)),
        Operation::RenameModel { new_name, .. } => format!("rename_{}", snake(new_name)),
        Operation::AddField { model, field } => {
            format!("add_{}_{}", snake(model), snake(&field.name))
        }
        Operation::RemoveField { model, name } => {
            format!("remove_{}_{}", snake(model), snake(name))
        }
        Operation::AlterField { model, name, .. } => {
            format!("alter_{}_{}", snake(model), snake(name))
        }
        Operation::RenameField {
            model, new_name, ..
        } => format!("rename_{}_{}", snake(model), snake(new_name)),
        Operation::CreateIndex { model, index } => {
            format!("index_{}_{}", snake(model), snake(&index.name))
        }
        Operation::DeleteIndex { model, name } => {
            format!("drop_index_{}_{}", snake(model), snake(name))
        }
        Operation::AddConstraint { model, constraint } => {
            format!("constraint_{}_{}", snake(model), snake(constraint.name()))
        }
        Operation::DeleteConstraint { model, name } => {
            format!("drop_constraint_{}_{}", snake(model), snake(name))
        }
        Operation::RunSQL { .. } => "run_sql".to_owned(),
        Operation::RunRust { name, .. } => format!("run_{}", snake(name)),
    }
}

fn snake(name: &str) -> String {
    let mut out = String::new();
    for (i, ch) in name.chars().enumerate() {
        if ch.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else if ch == '-' {
            out.push('_');
        } else {
            out.push(ch);
        }
    }
    sanitize_slug(&out)
}
