//! Owned, serializable snapshots of model metadata.
//!
//! [`ModelState::from_meta`] copies a `&'static ModelMeta`. [`ProjectState::from_metas`]
//! also synthesizes join-table models for auto many-to-many relations
//! (`ManyToManyMeta::through == None`). Unmanaged models (`managed: false`)
//! are skipped.
//!
//! [`SqlType`], [`OnDelete`] and [`DbDefault`] here are serde mirrors of the
//! ORM types, which do not implement serde.

use crate::error::MigrationError;
use axumapi_orm::{
    ConstraintMeta, DbDefault as OrmDbDefault, FieldMeta, ModelMeta, OnDelete as OrmOnDelete,
    SqlType as OrmSqlType,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Portable column type family (serde mirror of [`axumapi_orm::SqlType`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SqlType {
    /// 16-bit integer.
    SmallInt,
    /// 32-bit integer.
    Integer,
    /// 64-bit integer.
    BigInt,
    /// 32-bit float.
    Real,
    /// 64-bit float.
    Double,
    /// Exact decimal.
    Decimal,
    /// Boolean.
    Bool,
    /// Text / `VARCHAR(n)`.
    Text,
    /// Binary data.
    Binary,
    /// Calendar date.
    Date,
    /// Time of day.
    Time,
    /// UTC timestamp.
    Timestamp,
    /// Duration stored as 64-bit microseconds.
    Duration,
    /// UUID.
    Uuid,
    /// JSON document.
    Json,
    /// IP address stored as text.
    IpAddr,
}

impl From<OrmSqlType> for SqlType {
    fn from(value: OrmSqlType) -> Self {
        match value {
            OrmSqlType::SmallInt => Self::SmallInt,
            OrmSqlType::Integer => Self::Integer,
            OrmSqlType::BigInt => Self::BigInt,
            OrmSqlType::Real => Self::Real,
            OrmSqlType::Double => Self::Double,
            OrmSqlType::Decimal => Self::Decimal,
            OrmSqlType::Bool => Self::Bool,
            OrmSqlType::Text => Self::Text,
            OrmSqlType::Binary => Self::Binary,
            OrmSqlType::Date => Self::Date,
            OrmSqlType::Time => Self::Time,
            OrmSqlType::Timestamp => Self::Timestamp,
            OrmSqlType::Duration => Self::Duration,
            OrmSqlType::Uuid => Self::Uuid,
            OrmSqlType::Json => Self::Json,
            OrmSqlType::IpAddr => Self::IpAddr,
            other => unknown_sql_type(other),
        }
    }
}

impl From<SqlType> for OrmSqlType {
    fn from(value: SqlType) -> Self {
        match value {
            SqlType::SmallInt => Self::SmallInt,
            SqlType::Integer => Self::Integer,
            SqlType::BigInt => Self::BigInt,
            SqlType::Real => Self::Real,
            SqlType::Double => Self::Double,
            SqlType::Decimal => Self::Decimal,
            SqlType::Bool => Self::Bool,
            SqlType::Text => Self::Text,
            SqlType::Binary => Self::Binary,
            SqlType::Date => Self::Date,
            SqlType::Time => Self::Time,
            SqlType::Timestamp => Self::Timestamp,
            SqlType::Duration => Self::Duration,
            SqlType::Uuid => Self::Uuid,
            SqlType::Json => Self::Json,
            SqlType::IpAddr => Self::IpAddr,
        }
    }
}

fn unknown_sql_type(other: OrmSqlType) -> SqlType {
    // `SqlType` is `#[non_exhaustive]` on the ORM side; new variants fall
    // back to Text so an older migrations crate still serializes.
    let _ = other;
    SqlType::Text
}

/// Deletion policy (serde mirror of [`axumapi_orm::OnDelete`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OnDelete {
    /// Delete referencing rows too.
    #[default]
    Cascade,
    /// Refuse the delete while references exist (`RESTRICT` in DDL).
    Protect,
    /// Set the reference to `NULL`.
    SetNull,
    /// Set the reference to the column default.
    SetDefault,
    /// Leave referencing rows untouched (`NO ACTION` in DDL).
    DoNothing,
}

impl From<OrmOnDelete> for OnDelete {
    fn from(value: OrmOnDelete) -> Self {
        match value {
            OrmOnDelete::Cascade => Self::Cascade,
            OrmOnDelete::Protect => Self::Protect,
            OrmOnDelete::SetNull => Self::SetNull,
            OrmOnDelete::SetDefault => Self::SetDefault,
            OrmOnDelete::DoNothing => Self::DoNothing,
        }
    }
}

impl From<OnDelete> for OrmOnDelete {
    fn from(value: OnDelete) -> Self {
        match value {
            OnDelete::Cascade => Self::Cascade,
            OnDelete::Protect => Self::Protect,
            OnDelete::SetNull => Self::SetNull,
            OnDelete::SetDefault => Self::SetDefault,
            OnDelete::DoNothing => Self::DoNothing,
        }
    }
}

/// Server-side column default (serde mirror of [`axumapi_orm::DbDefault`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DbDefault {
    /// Current timestamp at insert time (`auto_now_add`).
    Now,
    /// A literal integer.
    Int(i64),
    /// A literal boolean.
    Bool(bool),
    /// A literal string.
    Text(String),
}

impl From<OrmDbDefault> for DbDefault {
    fn from(value: OrmDbDefault) -> Self {
        match value {
            OrmDbDefault::Now => Self::Now,
            OrmDbDefault::Int(i) => Self::Int(i),
            OrmDbDefault::Bool(b) => Self::Bool(b),
            OrmDbDefault::Text(s) => Self::Text(s.to_owned()),
        }
    }
}

/// Foreign-key target stored on a field snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignKeyState {
    /// Referenced table.
    pub target_table: String,
    /// Referenced column (the target's primary key).
    pub target_column: String,
    /// `ON DELETE` policy.
    pub on_delete: OnDelete,
}

/// One concrete column.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldState {
    /// Rust field name.
    pub name: String,
    /// Column name.
    pub column: String,
    /// Column type family.
    pub sql_type: SqlType,
    /// Accepts `NULL`.
    pub nullable: bool,
    /// Part of the primary key.
    pub primary_key: bool,
    /// Value generated by the database (autoincrement / identity).
    pub auto: bool,
    /// `UNIQUE`.
    pub unique: bool,
    /// Single-column index.
    pub index: bool,
    /// `VARCHAR(n)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u32>,
    /// Decimal precision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_digits: Option<u32>,
    /// Decimal scale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decimal_places: Option<u32>,
    /// Database default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<DbDefault>,
    /// Foreign-key relation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fk: Option<ForeignKeyState>,
}

impl FieldState {
    /// A plain non-null column of `sql_type`.
    pub fn new(name: impl Into<String>, column: impl Into<String>, sql_type: SqlType) -> Self {
        Self {
            name: name.into(),
            column: column.into(),
            sql_type,
            nullable: false,
            primary_key: false,
            auto: false,
            unique: false,
            index: false,
            max_length: None,
            max_digits: None,
            decimal_places: None,
            default: None,
            fk: None,
        }
    }

    /// Copy a [`FieldMeta`], resolving the FK target table and primary-key column.
    pub fn from_meta(field: &FieldMeta) -> Self {
        let fk = field.relation.map(|rel| {
            let target = (rel.target)();
            let target_column = target
                .pk()
                .map(|pk| pk.column.to_owned())
                .unwrap_or_else(|| "id".to_owned());
            ForeignKeyState {
                target_table: target.table.to_owned(),
                target_column,
                on_delete: rel.on_delete.into(),
            }
        });
        Self {
            name: field.name.to_owned(),
            column: field.column.to_owned(),
            sql_type: field.sql_type.into(),
            nullable: field.nullable,
            primary_key: field.primary_key,
            auto: field.auto,
            unique: field.unique,
            index: field.index,
            max_length: field.max_length,
            max_digits: field.max_digits,
            decimal_places: field.decimal_places,
            default: field.default.map(Into::into),
            fk,
        }
    }
}

/// A named multi-column index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexState {
    /// Index name.
    pub name: String,
    /// Columns in order.
    pub columns: Vec<String>,
    /// `UNIQUE` index.
    pub unique: bool,
}

impl IndexState {
    /// Copy an [`axumapi_orm::IndexMeta`].
    pub fn from_meta(meta: &axumapi_orm::IndexMeta) -> Self {
        Self {
            name: meta.name.to_owned(),
            columns: meta.columns.iter().map(|c| (*c).to_owned()).collect(),
            unique: meta.unique,
        }
    }
}

/// A table-level constraint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConstraintState {
    /// `UNIQUE (a, b)`.
    Unique {
        /// Constraint name.
        name: String,
        /// Columns.
        columns: Vec<String>,
    },
    /// `CHECK (<sql>)`. The SQL is written by the model author.
    Check {
        /// Constraint name.
        name: String,
        /// Boolean SQL expression.
        sql: String,
    },
}

impl ConstraintState {
    /// Copy a [`ConstraintMeta`].
    pub fn from_meta(meta: &ConstraintMeta) -> Self {
        match *meta {
            ConstraintMeta::Unique { name, columns } => Self::Unique {
                name: name.to_owned(),
                columns: columns.iter().map(|c| (*c).to_owned()).collect(),
            },
            ConstraintMeta::Check { name, sql } => Self::Check {
                name: name.to_owned(),
                sql: sql.to_owned(),
            },
        }
    }

    /// Constraint name.
    pub fn name(&self) -> &str {
        match self {
            Self::Unique { name, .. } | Self::Check { name, .. } => name,
        }
    }
}

/// Snapshot of one model (or a synthesized M2M join table).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelState {
    /// Rust type name (`User`); also the migration model name.
    pub name: String,
    /// Table name.
    pub table: String,
    /// Concrete columns in declaration order.
    pub fields: Vec<FieldState>,
    /// Named indexes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<IndexState>,
    /// Table constraints.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<ConstraintState>,
}

impl ModelState {
    /// Copy a managed [`ModelMeta`]. Returns `None` when `managed` is false.
    pub fn from_meta(meta: &ModelMeta) -> Option<Self> {
        if !meta.managed {
            return None;
        }
        Some(Self {
            name: meta.name.to_owned(),
            table: meta.table.to_owned(),
            fields: meta.fields.iter().map(FieldState::from_meta).collect(),
            indexes: meta.indexes.iter().map(IndexState::from_meta).collect(),
            constraints: meta
                .constraints
                .iter()
                .map(ConstraintState::from_meta)
                .collect(),
        })
    }

    /// Field by Rust name.
    pub fn field(&self, name: &str) -> Option<&FieldState> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// Mutable field by Rust name.
    pub fn field_mut(&mut self, name: &str) -> Option<&mut FieldState> {
        self.fields.iter_mut().find(|f| f.name == name)
    }

    /// Field by column name.
    pub fn column(&self, column: &str) -> Option<&FieldState> {
        self.fields.iter().find(|f| f.column == column)
    }

    /// Primary-key field, if declared.
    pub fn pk(&self) -> Option<&FieldState> {
        self.fields.iter().find(|f| f.primary_key)
    }

    /// Named index.
    pub fn index(&self, name: &str) -> Option<&IndexState> {
        self.indexes.iter().find(|i| i.name == name)
    }

    /// Named constraint.
    pub fn constraint(&self, name: &str) -> Option<&ConstraintState> {
        self.constraints.iter().find(|c| c.name() == name)
    }
}

/// The whole project's managed schema.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ProjectState {
    /// Models keyed by [`ModelState::name`].
    pub models: BTreeMap<String, ModelState>,
}

impl ProjectState {
    /// Empty project.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from compiled metadata, synthesizing auto M2M join tables.
    pub fn from_metas(metas: &[&ModelMeta]) -> Self {
        let mut models = BTreeMap::new();
        for meta in metas {
            if let Some(state) = ModelState::from_meta(meta) {
                models.insert(state.name.clone(), state);
            }
        }
        for meta in metas {
            if !meta.managed {
                continue;
            }
            for m2m in meta.many_to_many {
                if m2m.through.is_some() {
                    continue;
                }
                let join = synthesize_m2m(meta, m2m);
                models.insert(join.name.clone(), join);
            }
        }
        Self { models }
    }

    /// Model by name.
    pub fn model(&self, name: &str) -> Option<&ModelState> {
        self.models.get(name)
    }

    /// Mutable model by name.
    pub fn model_mut(&mut self, name: &str) -> Option<&mut ModelState> {
        self.models.get_mut(name)
    }

    /// Model whose table is `table`.
    pub fn model_for_table(&self, table: &str) -> Option<&ModelState> {
        self.models.values().find(|m| m.table == table)
    }

    /// Model named `name`, or a [`MigrationError::State`].
    pub fn require(&self, name: &str) -> Result<&ModelState, MigrationError> {
        self.model(name).ok_or_else(|| {
            MigrationError::state(format!("model `{name}` is not in the project state"))
        })
    }

    /// Mutable model named `name`, or a [`MigrationError::State`].
    pub fn require_mut(&mut self, name: &str) -> Result<&mut ModelState, MigrationError> {
        self.model_mut(name).ok_or_else(|| {
            MigrationError::state(format!("model `{name}` is not in the project state"))
        })
    }
}

fn synthesize_m2m(source: &ModelMeta, m2m: &axumapi_orm::ManyToManyMeta) -> ModelState {
    let target = (m2m.target)();
    let source_pk = source.pk();
    let target_pk = target.pk();
    let source_type = source_pk
        .map(|f| SqlType::from(f.sql_type))
        .unwrap_or(SqlType::BigInt);
    let target_type = target_pk
        .map(|f| SqlType::from(f.sql_type))
        .unwrap_or(SqlType::BigInt);
    let source_column_pk = source_pk.map(|f| f.column).unwrap_or("id");
    let target_column_pk = target_pk.map(|f| f.column).unwrap_or("id");

    let source_field = FieldState {
        index: true,
        fk: Some(ForeignKeyState {
            target_table: source.table.to_owned(),
            target_column: source_column_pk.to_owned(),
            on_delete: OnDelete::Cascade,
        }),
        ..FieldState::new(m2m.source_column, m2m.source_column, source_type)
    };
    let target_field = FieldState {
        index: true,
        fk: Some(ForeignKeyState {
            target_table: target.table.to_owned(),
            target_column: target_column_pk.to_owned(),
            on_delete: OnDelete::Cascade,
        }),
        ..FieldState::new(m2m.target_column, m2m.target_column, target_type)
    };

    ModelState {
        name: m2m.through_table.to_owned(),
        table: m2m.through_table.to_owned(),
        fields: vec![source_field, target_field],
        indexes: Vec::new(),
        constraints: vec![ConstraintState::Unique {
            name: format!("{}_uniq", m2m.through_table),
            columns: vec![m2m.source_column.to_owned(), m2m.target_column.to_owned()],
        }],
    }
}

/// Deterministic name for the single-column index implied by `field.index`.
pub fn auto_index_name(table: &str, column: &str) -> String {
    format!("{table}_{column}_idx")
}
