//! Deterministic schema diff: [`diff`] compares two [`ProjectState`]s.
//!
//! Renames are emitted only when listed in [`RenameHints`]. Without a hint a
//! renamed field is `RemoveField` + `AddField` (the column is dropped and
//! recreated; data is not preserved) and a renamed model is
//! `DeleteModel` + `CreateModel`. Those unhinted pairs are **refused** when
//! the dropped and added table or column have the same shape.

use crate::error::MigrationError;
use crate::operation::Operation;
use crate::state::{FieldState, ModelState, ProjectState, auto_index_name};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Explicit rename pairs. The autodetector never infers renames from
/// similarity; a missing hint becomes remove+add (and is refused when the
/// two sides have the same shape).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenameHints {
    /// `(model_name, old_field_name) → new_field_name`.
    pub fields: BTreeMap<(String, String), String>,
    /// `old_model_name → new_model_name`.
    pub models: BTreeMap<String, String>,
}

impl RenameHints {
    /// No hints.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `model.old_name` became `new_name`.
    #[must_use]
    pub fn rename_field(
        mut self,
        model: impl Into<String>,
        old_name: impl Into<String>,
        new_name: impl Into<String>,
    ) -> Self {
        self.fields
            .insert((model.into(), old_name.into()), new_name.into());
        self
    }

    /// Record that model `old_name` became `new_name`.
    #[must_use]
    pub fn rename_model(
        mut self,
        old_name: impl Into<String>,
        new_name: impl Into<String>,
    ) -> Self {
        self.models.insert(old_name.into(), new_name.into());
        self
    }
}

/// Operations that turn `from` into `to`, in a deterministic order:
///
/// 1. Delete constraints, indexes, fields
/// 2. Delete models (reverse FK dependency order)
/// 3. Rename fields
/// 4. Create models (topological by FK dependencies; cyclic FKs are
///    deferred as [`Operation::AddField`])
/// 5. Add / alter fields
/// 6. Create indexes and constraints
pub fn diff(from: &ProjectState, to: &ProjectState) -> Result<Vec<Operation>, MigrationError> {
    diff_with(from, to, &RenameHints::default())
}

/// [`diff`] honouring explicit field- and model-rename hints.
///
/// # Errors
/// [`MigrationError::State`] when a model or field is deleted and another of
/// the same shape is created without a rename hint (that would drop data).
pub fn diff_with(
    from: &ProjectState,
    to: &ProjectState,
    hints: &RenameHints,
) -> Result<Vec<Operation>, MigrationError> {
    let mut ops = Vec::new();

    let from_names: BTreeSet<&str> = from.models.keys().map(String::as_str).collect();
    let to_names: BTreeSet<&str> = to.models.keys().map(String::as_str).collect();

    let renamed_models: BTreeMap<&str, &str> = hints
        .models
        .iter()
        .filter(|(old, new)| from.models.contains_key(*old) && to.models.contains_key(*new))
        .map(|(old, new)| (old.as_str(), new.as_str()))
        .collect();
    let renamed_from: BTreeSet<&str> = renamed_models.keys().copied().collect();
    let renamed_to: BTreeSet<&str> = renamed_models.values().copied().collect();

    let deleted: Vec<&str> = from_names
        .difference(&to_names)
        .copied()
        .filter(|n| !renamed_from.contains(n))
        .collect();
    let created: Vec<&str> = to_names
        .difference(&from_names)
        .copied()
        .filter(|n| !renamed_to.contains(n))
        .collect();
    let common: Vec<&str> = from_names.intersection(&to_names).copied().collect();

    refuse_unhinted_model_renames(from, to, &deleted, &created)?;

    let pairs: Vec<(&str, &str)> = common
        .iter()
        .map(|n| (*n, *n))
        .chain(renamed_models.iter().map(|(o, n)| (*o, *n)))
        .collect();

    // --- deletes on models that survive (including renamed), then deleted models ---
    for (old_name, new_name) in &pairs {
        let old = &from.models[*old_name];
        let new = &to.models[*new_name];
        let renamed = hints_for(hints, old_name);
        refuse_unhinted_field_renames(old, new, &renamed)?;
        push_deletes(&mut ops, old, new, &renamed);
    }
    for name in reverse_topo(&deleted, from) {
        ops.push(Operation::DeleteModel {
            name: name.to_owned(),
        });
    }

    for (old_name, new_name) in &renamed_models {
        ops.push(Operation::RenameModel {
            old_name: (*old_name).to_owned(),
            new_name: (*new_name).to_owned(),
            table: to.models[*new_name].table.clone(),
        });
    }

    // --- field renames ---
    for (old_name, new_name) in &pairs {
        let renamed = hints_for(hints, old_name);
        for (from_field, to_field) in &renamed {
            if from.models[*old_name].field(from_field).is_some()
                && to.models[*new_name].field(to_field).is_some()
            {
                ops.push(Operation::RenameField {
                    model: (*new_name).to_owned(),
                    old_name: from_field.clone(),
                    new_name: to_field.clone(),
                });
            }
        }
    }

    // --- creates ---
    let created_states: Vec<&ModelState> = created.iter().map(|n| &to.models[*n]).collect();
    let (ordered, deferred) = order_creates(&created_states, to);
    for model in ordered {
        let mut create = model.clone();
        let extra = deferred
            .get(model.name.as_str())
            .cloned()
            .unwrap_or_default();
        if !extra.is_empty() {
            let extra_names: BTreeSet<&str> = extra.iter().map(|f| f.name.as_str()).collect();
            create
                .fields
                .retain(|f| !extra_names.contains(f.name.as_str()));
            create.indexes.retain(|i| {
                i.columns
                    .iter()
                    .all(|c| create.fields.iter().any(|f| f.column == *c))
            });
            create.constraints.retain(|c| match c {
                crate::state::ConstraintState::Unique { columns, .. } => columns
                    .iter()
                    .all(|col| create.fields.iter().any(|f| f.column == *col)),
                crate::state::ConstraintState::Check { .. } => extra.is_empty(),
            });
        }
        ops.push(Operation::CreateModel { model: create });
        if let Some(fields) = deferred.get(model.name.as_str()) {
            for field in fields {
                ops.push(Operation::AddField {
                    model: model.name.clone(),
                    field: field.clone(),
                });
            }
        }
    }

    // --- adds / alters on surviving models ---
    for (old_name, new_name) in &pairs {
        let old = &from.models[*old_name];
        let new = &to.models[*new_name];
        let renamed = hints_for(hints, old_name);
        push_adds_and_alters(&mut ops, old, new, &renamed);
    }

    Ok(ops)
}

fn refuse_unhinted_model_renames(
    from: &ProjectState,
    to: &ProjectState,
    deleted: &[&str],
    created: &[&str],
) -> Result<(), MigrationError> {
    for old_name in deleted {
        let old = &from.models[*old_name];
        for new_name in created {
            let new = &to.models[*new_name];
            if same_model_shape(old, new) {
                return Err(MigrationError::state(format!(
                    "model `{old_name}` was deleted and `{new_name}` was created with the same columns; \
                     pass RenameHints::rename_model(\"{old_name}\", \"{new_name}\") to preserve data, \
                     or split the drop and the create into two migrations"
                )));
            }
        }
    }
    Ok(())
}

fn refuse_unhinted_field_renames(
    old: &ModelState,
    new: &ModelState,
    renamed: &BTreeMap<String, String>,
) -> Result<(), MigrationError> {
    let renamed_from: BTreeSet<&str> = renamed.keys().map(String::as_str).collect();
    let renamed_to: BTreeSet<&str> = renamed.values().map(String::as_str).collect();
    let removed: Vec<&FieldState> = old
        .fields
        .iter()
        .filter(|f| !renamed_from.contains(f.name.as_str()) && new.field(&f.name).is_none())
        .collect();
    let added: Vec<&FieldState> = new
        .fields
        .iter()
        .filter(|f| !renamed_to.contains(f.name.as_str()) && old.field(&f.name).is_none())
        .collect();
    for lost in &removed {
        for gained in &added {
            if same_column_shape(lost, gained) {
                return Err(MigrationError::state(format!(
                    "field `{}.{}` was removed and `{}.{}` was added with the same type; \
                     pass RenameHints::rename_field(\"{}\", \"{}\", \"{}\") to preserve data, \
                     or split the drop and the add into two migrations",
                    old.name, lost.name, new.name, gained.name, old.name, lost.name, gained.name
                )));
            }
        }
    }
    Ok(())
}

fn same_model_shape(a: &ModelState, b: &ModelState) -> bool {
    if a.fields.is_empty() {
        return false;
    }
    let cols = |m: &ModelState| {
        let mut v: Vec<_> = m
            .fields
            .iter()
            .map(|f| (f.column.clone(), f.sql_type, f.nullable, f.max_length))
            .collect();
        v.sort_by(|x, y| x.0.cmp(&y.0));
        v
    };
    cols(a) == cols(b)
}

fn same_column_shape(a: &FieldState, b: &FieldState) -> bool {
    a.sql_type == b.sql_type
        && a.nullable == b.nullable
        && a.max_length == b.max_length
        && a.primary_key == b.primary_key
        && a.unique == b.unique
}

fn hints_for(hints: &RenameHints, model: &str) -> BTreeMap<String, String> {
    hints
        .fields
        .iter()
        .filter_map(|((m, old), new)| {
            if m == model {
                Some((old.clone(), new.clone()))
            } else {
                None
            }
        })
        .collect()
}

fn push_deletes(
    ops: &mut Vec<Operation>,
    old: &ModelState,
    new: &ModelState,
    renamed: &BTreeMap<String, String>,
) {
    let renamed_from: BTreeSet<&str> = renamed.keys().map(String::as_str).collect();

    for constraint in &old.constraints {
        if new.constraint(constraint.name()).is_none() {
            ops.push(Operation::DeleteConstraint {
                model: old.name.clone(),
                name: constraint.name().to_owned(),
            });
        }
    }

    let old_auto_indexes = auto_index_names(old);
    let new_auto_indexes = auto_index_names(new);
    for index in &old.indexes {
        if new.index(&index.name).is_none() {
            ops.push(Operation::DeleteIndex {
                model: old.name.clone(),
                name: index.name.clone(),
            });
        }
    }
    for name in &old_auto_indexes {
        if !new_auto_indexes.contains(name) && old.index(name).is_none() {
            ops.push(Operation::DeleteIndex {
                model: old.name.clone(),
                name: name.clone(),
            });
        }
    }

    for field in &old.fields {
        if renamed_from.contains(field.name.as_str()) {
            continue;
        }
        if new.field(&field.name).is_none() {
            ops.push(Operation::RemoveField {
                model: old.name.clone(),
                name: field.name.clone(),
            });
        }
    }
}

fn push_adds_and_alters(
    ops: &mut Vec<Operation>,
    old: &ModelState,
    new: &ModelState,
    renamed: &BTreeMap<String, String>,
) {
    let old_by_new_name: HashMap<&str, &FieldState> = {
        let mut m = HashMap::new();
        for field in &old.fields {
            let key = renamed
                .get(&field.name)
                .map(String::as_str)
                .unwrap_or(field.name.as_str());
            m.insert(key, field);
        }
        m
    };
    let renamed_from: BTreeSet<&str> = renamed.keys().map(String::as_str).collect();

    for field in &new.fields {
        if renamed.values().any(|n| n == &field.name) {
            if let Some(old_field) = old_by_new_name.get(field.name.as_str()) {
                let mut comparable = (*old_field).clone();
                comparable.name = field.name.clone();
                if comparable.column
                    == renamed
                        .iter()
                        .find_map(|(o, n)| (n == &field.name).then_some(o.as_str()))
                        .unwrap_or("")
                {
                    comparable.column = field.column.clone();
                }
                if &comparable != field {
                    ops.push(Operation::AlterField {
                        model: new.name.clone(),
                        name: field.name.clone(),
                        field: field.clone(),
                    });
                }
            }
            continue;
        }
        if old.field(&field.name).is_none() && !renamed_from.contains(field.name.as_str()) {
            ops.push(Operation::AddField {
                model: new.name.clone(),
                field: field.clone(),
            });
        } else if let Some(old_field) = old.field(&field.name)
            && old_field != field
        {
            ops.push(Operation::AlterField {
                model: new.name.clone(),
                name: field.name.clone(),
                field: field.clone(),
            });
        }
    }

    let old_auto = auto_index_names(old);
    let new_auto = auto_index_names(new);
    for name in &new_auto {
        if !old_auto.contains(name)
            && new.index(name).is_none()
            && let Some(field) = new
                .fields
                .iter()
                .find(|f| &auto_index_name(&new.table, &f.column) == name)
        {
            ops.push(Operation::CreateIndex {
                model: new.name.clone(),
                index: crate::state::IndexState {
                    name: name.clone(),
                    columns: vec![field.column.clone()],
                    unique: false,
                },
            });
        }
    }
    for index in &new.indexes {
        if old.index(&index.name).is_none() {
            ops.push(Operation::CreateIndex {
                model: new.name.clone(),
                index: index.clone(),
            });
        }
    }
    for constraint in &new.constraints {
        if old.constraint(constraint.name()).is_none() {
            ops.push(Operation::AddConstraint {
                model: new.name.clone(),
                constraint: constraint.clone(),
            });
        }
    }
}

fn auto_index_names(model: &ModelState) -> BTreeSet<String> {
    model
        .fields
        .iter()
        .filter(|f| f.index && !f.unique && !f.primary_key)
        .map(|f| auto_index_name(&model.table, &f.column))
        .collect()
}

/// Topological create order and FK fields that had to be deferred to break cycles.
fn order_creates<'a>(
    created: &[&'a ModelState],
    project: &ProjectState,
) -> (Vec<&'a ModelState>, BTreeMap<String, Vec<FieldState>>) {
    let names: BTreeSet<&str> = created.iter().map(|m| m.name.as_str()).collect();
    let by_name: BTreeMap<&str, &ModelState> =
        created.iter().map(|m| (m.name.as_str(), *m)).collect();

    let mut deps: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for model in created {
        let mut d = BTreeSet::new();
        for field in &model.fields {
            if let Some(fk) = &field.fk
                && let Some(target) = project.model_for_table(&fk.target_table)
                && names.contains(target.name.as_str())
                && target.name != model.name
            {
                d.insert(target.name.as_str());
            }
        }
        deps.insert(model.name.as_str(), d);
    }

    let mut deferred: BTreeMap<String, Vec<FieldState>> = BTreeMap::new();
    let mut ordered_names = match kahn(&names, &deps) {
        Ok(order) => order,
        Err(rest) => {
            // Strip FKs that point at other uncreated models in the leftover set.
            let leftover: BTreeSet<&str> = rest.iter().copied().collect();
            for name in &leftover {
                let model = by_name[name];
                let extra: Vec<FieldState> = model
                    .fields
                    .iter()
                    .filter(|f| {
                        f.fk.as_ref().is_some_and(|fk| {
                            project.model_for_table(&fk.target_table).is_some_and(|t| {
                                leftover.contains(t.name.as_str()) && t.name != model.name
                            })
                        })
                    })
                    .cloned()
                    .collect();
                if !extra.is_empty() {
                    deferred.insert((*name).to_owned(), extra);
                    if let Some(d) = deps.get_mut(name) {
                        d.retain(|dep| !leftover.contains(dep));
                    }
                }
            }
            match kahn(&names, &deps) {
                Ok(order) => order,
                Err(rest) => {
                    let mut all: Vec<&str> = names.iter().copied().collect();
                    all.sort_unstable();
                    let mut rest_sorted = rest;
                    rest_sorted.sort_unstable();
                    let mut order: Vec<&str> = all
                        .iter()
                        .copied()
                        .filter(|n| !rest_sorted.contains(n))
                        .collect();
                    order.extend(rest_sorted);
                    order
                }
            }
        }
    };

    let ordered: Vec<&ModelState> = ordered_names.drain(..).map(|n| by_name[n]).collect();
    (ordered, deferred)
}

fn kahn<'a>(
    names: &BTreeSet<&'a str>,
    deps: &BTreeMap<&'a str, BTreeSet<&'a str>>,
) -> Result<Vec<&'a str>, Vec<&'a str>> {
    let mut indeg: BTreeMap<&str, usize> = BTreeMap::new();
    for n in names {
        indeg.insert(*n, deps.get(n).map(BTreeSet::len).unwrap_or(0));
    }
    let mut ready_set: BTreeSet<&str> = indeg
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(n, _)| *n)
        .collect();
    let mut order = Vec::new();
    let mut remaining = indeg;
    while let Some(n) = ready_set.iter().next().copied() {
        ready_set.remove(n);
        order.push(n);
        remaining.remove(n);
        for (node, ds) in deps {
            if ds.contains(n)
                && let Some(deg) = remaining.get_mut(node)
            {
                *deg = deg.saturating_sub(1);
                if *deg == 0 {
                    ready_set.insert(node);
                }
            }
        }
    }
    if remaining.is_empty() {
        Ok(order)
    } else {
        Err(remaining.keys().copied().collect())
    }
}

fn reverse_topo<'a>(deleted: &[&'a str], from: &'a ProjectState) -> Vec<&'a str> {
    let names: BTreeSet<&str> = deleted.iter().copied().collect();
    let mut deps: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for name in &names {
        let mut d = BTreeSet::new();
        if let Some(model) = from.model(name) {
            for field in &model.fields {
                if let Some(fk) = &field.fk
                    && let Some(target) = from.model_for_table(&fk.target_table)
                    && names.contains(target.name.as_str())
                    && target.name != model.name
                {
                    d.insert(target.name.as_str());
                }
            }
        }
        deps.insert(*name, d);
    }
    match kahn(&names, &deps) {
        Ok(mut order) => {
            order.reverse();
            order
        }
        Err(_) => {
            let mut all: Vec<&str> = names.iter().copied().collect();
            all.sort_unstable();
            all.reverse();
            all
        }
    }
}
