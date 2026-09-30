//! Cross-field resolution: primary key, table name, ordering, indexes,
//! constraints and many-to-many relations.

use super::field::{ModelField, resolve};
use super::options::ModelOptions;
use crate::attrs::rename::snake_from_pascal;
use crate::diag::Errors;
use std::collections::HashSet;
use syn::{DeriveInput, FieldsNamed, Ident, LitStr, Path, Type};

/// Where a join table's name comes from.
pub enum Through {
    /// An auto-created table with this name.
    Table(String),
    /// An explicit through model; the table is that model's.
    Model(Path),
}

/// A resolved many-to-many relation.
pub struct ManyToMany {
    pub name: Ident,
    pub target: Type,
    pub through: Through,
    pub source_column: String,
    pub target_column: String,
    pub related_name: Option<LitStr>,
}

/// A table constraint.
pub enum Constraint {
    Unique { name: String, columns: Vec<String> },
    Check { name: String, sql: LitStr },
}

/// A named multi-column index.
pub struct Index {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
}

/// Everything the expansion needs to know about the model.
pub struct ModelPlan<'a> {
    pub ident: &'a Ident,
    pub table: String,
    pub fields: Vec<ModelField<'a>>,
    /// Index of the primary key in `fields`.
    pub pk: usize,
    /// `(column, descending)`.
    pub ordering: Vec<(String, bool)>,
    pub indexes: Vec<Index>,
    pub constraints: Vec<Constraint>,
    pub managed: bool,
    pub many_to_many: Vec<ManyToMany>,
}

impl<'a> ModelPlan<'a> {
    /// The primary-key field.
    pub fn pk_field(&self) -> &ModelField<'a> {
        &self.fields[self.pk]
    }

    /// The fields stored in columns, in declaration order.
    pub fn columns(&self) -> impl Iterator<Item = &ModelField<'a>> {
        self.fields.iter().filter(|f| f.is_column())
    }
}

/// Resolve `input`; `None` after reporting errors that make expansion pointless.
pub fn build<'a>(
    input: &'a DeriveInput,
    fields: &'a FieldsNamed,
    options: ModelOptions,
    errors: &mut Errors,
) -> Option<ModelPlan<'a>> {
    let fields = resolve(fields, errors);
    let pk = primary_key(input, &fields, errors)?;
    let table = options.table.as_ref().map_or_else(
        || snake_from_pascal(&input.ident.to_string()),
        LitStr::value,
    );
    let mut plan = ModelPlan {
        ident: &input.ident,
        table,
        fields,
        pk,
        ordering: Vec::new(),
        indexes: Vec::new(),
        constraints: Vec::new(),
        managed: options.managed,
        many_to_many: Vec::new(),
    };
    plan.ordering = options
        .ordering
        .iter()
        .filter_map(|lit| {
            let value = lit.value();
            let (name, descending) = value
                .strip_prefix('-')
                .map_or((value.as_str(), false), |rest| (rest, true));
            let column = column_of(&plan.fields, name, lit, errors)?;
            Some((column, descending))
        })
        .collect();
    for spec in &options.indexes {
        let columns = columns_of(&plan.fields, &spec.columns, errors);
        plan.indexes.push(Index {
            name: spec.name.to_string(),
            columns,
            unique: spec.unique,
        });
    }
    for group in &options.unique_together {
        let columns = columns_of(&plan.fields, group, errors);
        let name = format!("{}_{}_uniq", plan.table, columns.join("_"));
        plan.constraints.push(Constraint::Unique { name, columns });
    }
    for (name, sql) in &options.checks {
        plan.constraints.push(Constraint::Check {
            name: name.to_string(),
            sql: sql.clone(),
        });
    }
    check_constraint_names(&plan, errors);
    plan.many_to_many = options
        .many_to_many
        .into_iter()
        .filter_map(|spec| many_to_many(&plan, spec, errors))
        .collect();
    Some(plan)
}

fn primary_key(
    input: &DeriveInput,
    fields: &[ModelField<'_>],
    errors: &mut Errors,
) -> Option<usize> {
    let mut found: Option<usize> = None;
    for (index, field) in fields.iter().enumerate() {
        if !field.options.orm.primary_key {
            continue;
        }
        if field.options.orm.skip {
            errors.spanned(field.ident, "the primary key cannot be skipped");
        } else if found.is_some() {
            errors.spanned(
                field.ident,
                "a model has exactly one primary key (composite keys are not supported)",
            );
        } else {
            found = Some(index);
        }
    }
    if found.is_none() {
        errors.spanned(
            &input.ident,
            format!(
                "model `{}` has no primary key: mark one field with `#[field(primary_key)]`, \
                 e.g. `#[field(primary_key, auto)] id: i64`",
                input.ident
            ),
        );
    }
    found
}

/// Column of the field called `name` (or, failing that, the column `name`).
fn column_of(
    fields: &[ModelField<'_>],
    name: &str,
    at: &LitStr,
    errors: &mut Errors,
) -> Option<String> {
    let column = fields
        .iter()
        .filter(|f| f.is_column())
        .find(|f| f.name == name)
        .or_else(|| {
            fields
                .iter()
                .filter(|f| f.is_column())
                .find(|f| f.column == name)
        })
        .map(|f| f.column.clone());
    if column.is_none() {
        errors.spanned(at, format!("`{name}` is not a field of this model"));
    }
    column
}

fn columns_of(fields: &[ModelField<'_>], names: &[LitStr], errors: &mut Errors) -> Vec<String> {
    names
        .iter()
        .filter_map(|lit| column_of(fields, &lit.value(), lit, errors))
        .collect()
}

fn check_constraint_names(plan: &ModelPlan<'_>, errors: &mut Errors) {
    let mut seen = HashSet::new();
    let names = plan
        .indexes
        .iter()
        .map(|i| &i.name)
        .chain(plan.constraints.iter().map(|c| match c {
            Constraint::Unique { name, .. } | Constraint::Check { name, .. } => name,
        }));
    for name in names {
        if !seen.insert(name) {
            errors.error(
                plan.ident.span(),
                format!("duplicate index or constraint name `{name}`"),
            );
        }
    }
}

fn many_to_many(
    plan: &ModelPlan<'_>,
    spec: super::options::ManyToManySpec,
    errors: &mut Errors,
) -> Option<ManyToMany> {
    if plan.fields.iter().any(|f| spec.name == f.name) {
        errors.spanned(
            &spec.name,
            "a many-to-many relation cannot share its name with a field",
        );
    }
    let Type::Path(target_path) = &spec.target else {
        errors.spanned(&spec.target, "expected the path of the target model");
        return None;
    };
    let target_name = target_path.path.segments.last()?.ident.to_string();
    let source_name = plan.ident.to_string();
    let source_column = spec.source_column.as_ref().map_or_else(
        || format!("{}_id", snake_from_pascal(&source_name)),
        LitStr::value,
    );
    let target_column = spec.target_column.as_ref().map_or_else(
        || format!("{}_id", snake_from_pascal(&target_name)),
        LitStr::value,
    );
    if source_column == target_column {
        errors.spanned(
            &spec.name,
            "source and target columns coincide (a self-referential relation?): \
             set `source_column` and `target_column` explicitly",
        );
    }
    let through = match (spec.through, spec.through_table) {
        (Some(model), None) => Through::Model(model),
        (None, table) => Through::Table(table.map_or_else(
            || format!("{}_{}", plan.table, spec.name),
            |lit| lit.value(),
        )),
        (Some(model), Some(table)) => {
            errors.spanned(
                table,
                "`through_table` conflicts with `through`: the table is the through model's",
            );
            Through::Model(model)
        }
    };
    Some(ManyToMany {
        name: spec.name,
        target: spec.target,
        through,
        source_column,
        target_column,
        related_name: spec.related_name,
    })
}
