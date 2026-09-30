//! Collapse a contiguous range of migrations into one.

use crate::error::MigrationError;
use crate::loader::MigrationGraph;
use crate::migration::{Migration, next_id, sanitize_slug};
use crate::operation::Operation;
use crate::state::ProjectState;

/// Squash the inclusive range `from_id..=to_id` (in graph order) into one
/// migration.
///
/// Concatenates operations and applies:
/// * `CreateModel` followed by `AddField` on the same model merges the field
///   into the create
/// * `CreateModel` + later `DeleteModel` of the same model (and ops on it in
///   between) cancel
///
/// The result has `replaces` set to the squashed ids. Dependencies are those
/// of the first migration in the range that are not themselves replaced.
///
/// # Errors
/// Unknown ids, a non-contiguous range, or checksum encoding.
pub fn squash(
    graph: &MigrationGraph,
    from_id: &str,
    to_id: &str,
    name: Option<&str>,
) -> Result<Migration, MigrationError> {
    let order = &graph.order;
    let start = order
        .iter()
        .position(|id| id == from_id)
        .ok_or_else(|| MigrationError::usage(format!("unknown migration `{from_id}`")))?;
    let end = order
        .iter()
        .position(|id| id == to_id)
        .ok_or_else(|| MigrationError::usage(format!("unknown migration `{to_id}`")))?;
    if start > end {
        return Err(MigrationError::usage(format!(
            "cannot squash `{from_id}`..=`{to_id}`: `{from_id}` is after `{to_id}`"
        )));
    }
    let range: Vec<&Migration> = order[start..=end]
        .iter()
        .filter_map(|id| graph.get(id))
        .collect();
    if range.is_empty() {
        return Err(MigrationError::usage("nothing to squash"));
    }
    let replaced: Vec<String> = range.iter().map(|m| m.id.clone()).collect();
    let mut dependencies: Vec<String> = Vec::new();
    let replaced_set: std::collections::HashSet<&str> =
        replaced.iter().map(String::as_str).collect();
    for dep in &range[0].dependencies {
        if !replaced_set.contains(dep.as_str()) {
            dependencies.push(dep.clone());
        }
    }
    let mut operations = Vec::new();
    let mut atomic = true;
    for migration in &range {
        operations.extend(migration.operations.iter().cloned());
        atomic = atomic && migration.atomic;
    }
    operations = optimize(operations);
    let slug = match name {
        Some(n) => sanitize_slug(n),
        None => format!(
            "squashed_{}_{}",
            sanitize_slug(from_id),
            sanitize_slug(to_id)
        ),
    };
    let id = next_id(&graph.all, &slug);
    Migration::new(id, dependencies, operations, atomic, replaced)
}

/// Reduce `ops` with the documented local optimizations.
pub fn optimize(mut ops: Vec<Operation>) -> Vec<Operation> {
    let mut changed = true;
    while changed {
        changed = false;
        // Merge CreateModel + later AddField on the same model.
        let mut i = 0;
        while i < ops.len() {
            let model_name = match &ops[i] {
                Operation::CreateModel { model } => Some(model.name.clone()),
                _ => None,
            };
            if let Some(name) = model_name {
                let mut j = i + 1;
                while j < ops.len() {
                    let merge_field = match &ops[j] {
                        Operation::AddField { model, field } if model == &name => {
                            Some(field.clone())
                        }
                        Operation::DeleteModel { name: n } if n == &name => break,
                        _ => None,
                    };
                    if let Some(field) = merge_field {
                        if let Operation::CreateModel { model } = &mut ops[i] {
                            model.fields.push(field);
                        }
                        ops.remove(j);
                        changed = true;
                        continue;
                    }
                    j += 1;
                }
            }
            i += 1;
        }
        // Cancel CreateModel + DeleteModel of the same model, dropping ops on it.
        if let Some((c, d, name)) = find_create_delete(&ops) {
            let mut remove = Vec::new();
            for (idx, op) in ops.iter().enumerate() {
                if idx >= c && idx <= d && targets_model(op, &name) {
                    remove.push(idx);
                }
            }
            for idx in remove.into_iter().rev() {
                ops.remove(idx);
            }
            changed = true;
        }
    }
    ops
}

fn find_create_delete(ops: &[Operation]) -> Option<(usize, usize, String)> {
    for (c, op) in ops.iter().enumerate() {
        if let Operation::CreateModel { model } = op {
            let name = &model.name;
            if let Some(d) = ops.iter().enumerate().skip(c + 1).find_map(|(i, o)| {
                if let Operation::DeleteModel { name: n } = o {
                    (n == name).then_some(i)
                } else {
                    None
                }
            }) {
                return Some((c, d, name.clone()));
            }
        }
    }
    None
}

fn targets_model(op: &Operation, name: &str) -> bool {
    match op {
        Operation::CreateModel { model } => model.name == name,
        Operation::DeleteModel { name: n } => n == name,
        Operation::AddField { model, .. }
        | Operation::RemoveField { model, .. }
        | Operation::AlterField { model, .. }
        | Operation::RenameField { model, .. }
        | Operation::CreateIndex { model, .. }
        | Operation::DeleteIndex { model, .. }
        | Operation::AddConstraint { model, .. }
        | Operation::DeleteConstraint { model, .. } => model == name,
        Operation::RunSQL { .. } | Operation::RunRust { .. } => false,
    }
}

/// Apply `operations` to an empty state (used by tests).
pub fn final_state(operations: &[Operation]) -> Result<ProjectState, MigrationError> {
    let mut state = ProjectState::new();
    for op in operations {
        op.apply_to_state(&mut state)?;
    }
    Ok(state)
}
