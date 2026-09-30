//! Schema operations recorded in a migration file.

use crate::error::MigrationError;
use crate::state::{ConstraintState, FieldState, IndexState, ModelState, ProjectState};
use serde::{Deserialize, Serialize};

/// One atomic schema change (or a data operation).
///
/// [`RunSQL`](Self::RunSQL) without `reverse_sql` and [`RunRust`](Self::RunRust)
/// without `backwards` are irreversible. [`DeleteModel`](Self::DeleteModel) and
/// [`RemoveField`](Self::RemoveField) reverse by recreating the object from
/// `state_before`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Operation {
    /// Create a table (and its indexes / table constraints).
    CreateModel {
        /// Full model snapshot after the create.
        model: ModelState,
    },
    /// Drop a table.
    DeleteModel {
        /// Model name.
        name: String,
    },
    /// Rename a model (and its table when `table` differs from the current
    /// table). Detected only when listed in [`crate::autodetector::RenameHints`].
    RenameModel {
        /// Current model name.
        old_name: String,
        /// New model name.
        new_name: String,
        /// Table name after the rename.
        table: String,
    },
    /// Add a column.
    AddField {
        /// Model name.
        model: String,
        /// New field.
        field: FieldState,
    },
    /// Drop a column.
    RemoveField {
        /// Model name.
        model: String,
        /// Field name.
        name: String,
    },
    /// Replace a field's schema (type, nullability, default, …).
    AlterField {
        /// Model name.
        model: String,
        /// Field name (after any rename in the same migration).
        name: String,
        /// New field snapshot.
        field: FieldState,
    },
    /// Rename a field. Detected only when listed in
    /// [`crate::autodetector::RenameHints`]; otherwise a same-shape
    /// remove+add is refused.
    RenameField {
        /// Model name.
        model: String,
        /// Current field name.
        old_name: String,
        /// New field name. The column is renamed when it currently equals
        /// `old_name`.
        new_name: String,
    },
    /// Create a named index.
    CreateIndex {
        /// Model name.
        model: String,
        /// Index snapshot.
        index: IndexState,
    },
    /// Drop a named index.
    DeleteIndex {
        /// Model name.
        model: String,
        /// Index name.
        name: String,
    },
    /// Add a table-level constraint.
    AddConstraint {
        /// Model name.
        model: String,
        /// Constraint snapshot.
        constraint: ConstraintState,
    },
    /// Drop a table-level constraint.
    DeleteConstraint {
        /// Model name.
        model: String,
        /// Constraint name.
        name: String,
    },
    /// Developer-authored SQL. Irreversible unless `reverse_sql` is set.
    RunSQL {
        /// Forward SQL (may contain several statements).
        sql: String,
        /// SQL that undoes `sql`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reverse_sql: Option<String>,
    },
    /// A function registered at runtime in a
    /// [`crate::registry::MigrationRegistry`]. The function itself cannot be
    /// serialized; only the name is stored.
    RunRust {
        /// Registered forward name.
        name: String,
        /// Registered backwards name, if reversible.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        backwards: Option<String>,
    },
}

impl Operation {
    /// Apply this operation to `state`.
    ///
    /// # Errors
    /// [`MigrationError::State`] when a referenced model or field is missing,
    /// or when creating a duplicate.
    pub fn apply_to_state(&self, state: &mut ProjectState) -> Result<(), MigrationError> {
        match self {
            Self::CreateModel { model } => {
                if state.models.contains_key(&model.name) {
                    return Err(MigrationError::state(format!(
                        "model `{}` already exists",
                        model.name
                    )));
                }
                state.models.insert(model.name.clone(), model.clone());
            }
            Self::DeleteModel { name } => {
                state.models.remove(name).ok_or_else(|| {
                    MigrationError::state(format!("model `{name}` is not in the project state"))
                })?;
            }
            Self::RenameModel {
                old_name,
                new_name,
                table,
            } => {
                let mut model = state.models.remove(old_name).ok_or_else(|| {
                    MigrationError::state(format!("model `{old_name}` is not in the project state"))
                })?;
                if old_name != new_name && state.models.contains_key(new_name) {
                    return Err(MigrationError::state(format!(
                        "model `{new_name}` already exists"
                    )));
                }
                model.name = new_name.clone();
                model.table = table.clone();
                state.models.insert(new_name.clone(), model);
            }
            Self::AddField { model, field } => {
                let m = state.require_mut(model)?;
                if m.field(&field.name).is_some() {
                    return Err(MigrationError::state(format!(
                        "field `{}` already exists on `{model}`",
                        field.name
                    )));
                }
                m.fields.push(field.clone());
            }
            Self::RemoveField { model, name } => {
                let m = state.require_mut(model)?;
                let before = m.fields.len();
                m.fields.retain(|f| f.name != *name);
                if m.fields.len() == before {
                    return Err(MigrationError::state(format!(
                        "field `{name}` not on model `{model}`"
                    )));
                }
            }
            Self::AlterField { model, name, field } => {
                let m = state.require_mut(model)?;
                let existing = m.field_mut(name).ok_or_else(|| {
                    MigrationError::state(format!("field `{name}` not on model `{model}`"))
                })?;
                *existing = field.clone();
            }
            Self::RenameField {
                model,
                old_name,
                new_name,
            } => rename_field(state.require_mut(model)?, old_name, new_name)?,
            Self::CreateIndex { model, index } => {
                let m = state.require_mut(model)?;
                if m.index(&index.name).is_some() {
                    return Err(MigrationError::state(format!(
                        "index `{}` already exists on `{model}`",
                        index.name
                    )));
                }
                m.indexes.push(index.clone());
            }
            Self::DeleteIndex { model, name } => {
                let m = state.require_mut(model)?;
                let before = m.indexes.len();
                m.indexes.retain(|i| i.name != *name);
                if m.indexes.len() == before {
                    return Err(MigrationError::state(format!(
                        "index `{name}` not on model `{model}`"
                    )));
                }
            }
            Self::AddConstraint { model, constraint } => {
                let m = state.require_mut(model)?;
                if m.constraint(constraint.name()).is_some() {
                    return Err(MigrationError::state(format!(
                        "constraint `{}` already exists on `{model}`",
                        constraint.name()
                    )));
                }
                m.constraints.push(constraint.clone());
            }
            Self::DeleteConstraint { model, name } => {
                let m = state.require_mut(model)?;
                let before = m.constraints.len();
                m.constraints.retain(|c| c.name() != *name);
                if m.constraints.len() == before {
                    return Err(MigrationError::state(format!(
                        "constraint `{name}` not on model `{model}`"
                    )));
                }
            }
            Self::RunSQL { .. } | Self::RunRust { .. } => {}
        }
        Ok(())
    }

    /// Inverse operation, or `None` if this step is irreversible.
    ///
    /// `state_before` is the project state immediately before this operation
    /// was applied. [`DeleteModel`](Self::DeleteModel) and
    /// [`RemoveField`](Self::RemoveField) recreate the dropped object from it.
    pub fn reverse(&self, state_before: &ProjectState) -> Option<Operation> {
        match self {
            Self::CreateModel { model } => Some(Self::DeleteModel {
                name: model.name.clone(),
            }),
            Self::DeleteModel { name } => {
                let model = state_before.model(name)?.clone();
                Some(Self::CreateModel { model })
            }
            Self::RenameModel {
                old_name, new_name, ..
            } => {
                let table = state_before.model(old_name)?.table.clone();
                Some(Self::RenameModel {
                    old_name: new_name.clone(),
                    new_name: old_name.clone(),
                    table,
                })
            }
            Self::AddField { model, field } => Some(Self::RemoveField {
                model: model.clone(),
                name: field.name.clone(),
            }),
            Self::RemoveField { model, name } => {
                let field = state_before.model(model)?.field(name)?.clone();
                Some(Self::AddField {
                    model: model.clone(),
                    field,
                })
            }
            Self::AlterField { model, name, .. } => {
                let field = state_before.model(model)?.field(name)?.clone();
                Some(Self::AlterField {
                    model: model.clone(),
                    name: field.name.clone(),
                    field,
                })
            }
            Self::RenameField {
                model,
                old_name,
                new_name,
            } => Some(Self::RenameField {
                model: model.clone(),
                old_name: new_name.clone(),
                new_name: old_name.clone(),
            }),
            Self::CreateIndex { model, index } => Some(Self::DeleteIndex {
                model: model.clone(),
                name: index.name.clone(),
            }),
            Self::DeleteIndex { model, name } => {
                let index = state_before.model(model)?.index(name)?.clone();
                Some(Self::CreateIndex {
                    model: model.clone(),
                    index,
                })
            }
            Self::AddConstraint { model, constraint } => Some(Self::DeleteConstraint {
                model: model.clone(),
                name: constraint.name().to_owned(),
            }),
            Self::DeleteConstraint { model, name } => {
                let constraint = state_before.model(model)?.constraint(name)?.clone();
                Some(Self::AddConstraint {
                    model: model.clone(),
                    constraint,
                })
            }
            Self::RunSQL { reverse_sql, .. } => reverse_sql.as_ref().map(|sql| Self::RunSQL {
                sql: sql.clone(),
                reverse_sql: None,
            }),
            Self::RunRust { name, backwards } => backwards.as_ref().map(|back| Self::RunRust {
                name: back.clone(),
                backwards: Some(name.clone()),
            }),
        }
    }

    /// Whether reversal is impossible regardless of project state
    /// (`RunSQL` without `reverse_sql`, `RunRust` without `backwards`).
    pub fn is_unconditionally_irreversible(&self) -> bool {
        match self {
            Self::RunSQL { reverse_sql, .. } => reverse_sql.is_none(),
            Self::RunRust { backwards, .. } => backwards.is_none(),
            _ => false,
        }
    }

    /// Short label for CLI output (`CreateModel Author`).
    pub fn summary(&self) -> String {
        match self {
            Self::CreateModel { model } => format!("CreateModel {}", model.name),
            Self::DeleteModel { name } => format!("DeleteModel {name}"),
            Self::RenameModel {
                old_name, new_name, ..
            } => format!("RenameModel {old_name} -> {new_name}"),
            Self::AddField { model, field } => format!("AddField {model}.{}", field.name),
            Self::RemoveField { model, name } => format!("RemoveField {model}.{name}"),
            Self::AlterField { model, name, .. } => format!("AlterField {model}.{name}"),
            Self::RenameField {
                model,
                old_name,
                new_name,
            } => format!("RenameField {model}.{old_name} -> {new_name}"),
            Self::CreateIndex { model, index } => format!("CreateIndex {model}.{}", index.name),
            Self::DeleteIndex { model, name } => format!("DeleteIndex {model}.{name}"),
            Self::AddConstraint { model, constraint } => {
                format!("AddConstraint {model}.{}", constraint.name())
            }
            Self::DeleteConstraint { model, name } => format!("DeleteConstraint {model}.{name}"),
            Self::RunSQL { .. } => "RunSQL".to_owned(),
            Self::RunRust { name, .. } => format!("RunRust {name}"),
        }
    }
}

fn rename_field(
    model: &mut ModelState,
    old_name: &str,
    new_name: &str,
) -> Result<(), MigrationError> {
    if model.field(new_name).is_some() && old_name != new_name {
        return Err(MigrationError::state(format!(
            "field `{new_name}` already exists on `{}`",
            model.name
        )));
    }
    let rename_column = {
        let field = model.field(old_name).ok_or_else(|| {
            MigrationError::state(format!("field `{old_name}` not on model `{}`", model.name))
        })?;
        field.column == old_name
    };
    let old_column = model
        .field(old_name)
        .map(|f| f.column.clone())
        .unwrap_or_default();
    let model_name = model.name.clone();
    let field = model.field_mut(old_name).ok_or_else(|| {
        MigrationError::state(format!("field `{old_name}` not on model `{model_name}`"))
    })?;
    field.name = new_name.to_owned();
    if rename_column {
        field.column = new_name.to_owned();
    }
    if rename_column {
        let new_column = new_name.to_owned();
        for index in &mut model.indexes {
            for col in &mut index.columns {
                if *col == old_column {
                    *col = new_column.clone();
                }
            }
        }
        for constraint in &mut model.constraints {
            if let crate::state::ConstraintState::Unique { columns, .. } = constraint {
                for col in columns {
                    if *col == old_column {
                        *col = new_column.clone();
                    }
                }
            }
        }
    }
    Ok(())
}
