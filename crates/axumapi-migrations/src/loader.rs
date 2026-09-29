//! Load, validate and write JSON migration files.

use crate::error::MigrationError;
use crate::migration::Migration;
use crate::state::ProjectState;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Load every `*.json` file in `dir`. Missing directories yield an empty list.
/// Each file's checksum is recomputed from its content.
///
/// # Errors
/// IO, JSON, or a checksum body that cannot be encoded.
pub fn load_dir(dir: &Path) -> Result<Vec<Migration>, MigrationError> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut files: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                Some(path)
            } else {
                None
            }
        })
        .collect();
    files.sort();
    let mut migrations = Vec::with_capacity(files.len());
    for path in files {
        let text = fs::read_to_string(&path)?;
        let mut migration: Migration = serde_json::from_str(&text)?;
        migration.checksum = migration.compute_checksum()?;
        migrations.push(migration);
    }
    Ok(migrations)
}

/// Pretty-print `migration` as `{id}.json` in `dir`.
///
/// # Errors
/// IO or JSON encoding.
pub fn write_migration(dir: &Path, migration: &Migration) -> Result<PathBuf, MigrationError> {
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.json", migration.id));
    let mut body = serde_json::to_string_pretty(migration)?;
    if !body.ends_with('\n') {
        body.push('\n');
    }
    fs::write(&path, body)?;
    Ok(path)
}

/// Graph of visible (non-replaced) migrations, in topological order.
#[derive(Debug, Clone)]
pub struct MigrationGraph {
    /// All loaded migrations, including those replaced by a squash.
    pub all: Vec<Migration>,
    /// Ids in topological order (replaced ids omitted).
    pub order: Vec<String>,
    /// `replaced_id → squash_id`.
    pub replaced_by: HashMap<String, String>,
}

impl MigrationGraph {
    /// Validate dependencies, reject cycles and multiple heads, hide ids
    /// listed in any loaded migration's `replaces`.
    ///
    /// # Errors
    /// [`MigrationError::MissingDependency`], [`MigrationError::Cycle`],
    /// [`MigrationError::MultipleHeads`].
    pub fn build(migrations: Vec<Migration>) -> Result<Self, MigrationError> {
        let by_id: BTreeMap<String, Migration> = {
            let mut m = BTreeMap::new();
            for migration in &migrations {
                m.insert(migration.id.clone(), migration.clone());
            }
            m
        };
        let mut replaced_by: HashMap<String, String> = HashMap::new();
        for migration in &migrations {
            for old in &migration.replaces {
                replaced_by.insert(old.clone(), migration.id.clone());
            }
        }
        let visible: BTreeSet<String> = by_id
            .keys()
            .filter(|id| !replaced_by.contains_key(id.as_str()))
            .cloned()
            .collect();

        for id in &visible {
            let migration = &by_id[id];
            for dep in &migration.dependencies {
                let resolved = resolve_dep(dep, &replaced_by);
                if !by_id.contains_key(dep) && !by_id.contains_key(&resolved) {
                    return Err(MigrationError::MissingDependency {
                        id: id.clone(),
                        dependency: dep.clone(),
                    });
                }
            }
        }

        let mut deps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for id in &visible {
            let mut d = BTreeSet::new();
            for dep in &by_id[id].dependencies {
                let resolved = resolve_dep(dep, &replaced_by);
                if visible.contains(&resolved) {
                    d.insert(resolved);
                }
            }
            deps.insert(id.clone(), d);
        }

        let order = topo_ids(&visible, &deps)?;
        let heads: Vec<String> = {
            let mut incoming: BTreeMap<String, usize> =
                visible.iter().map(|id| (id.clone(), 0usize)).collect();
            for id in &visible {
                for dep in &deps[id] {
                    if let Some(n) = incoming.get_mut(dep) {
                        *n += 1;
                    }
                }
            }
            let mut heads: Vec<String> = incoming
                .into_iter()
                .filter(|(_, n)| *n == 0)
                .map(|(id, _)| id)
                .collect();
            heads.sort();
            heads
        };
        if heads.len() > 1 {
            return Err(MigrationError::MultipleHeads { heads });
        }

        Ok(Self {
            all: migrations,
            order,
            replaced_by,
        })
    }

    /// Migration by id (including replaced ones).
    pub fn get(&self, id: &str) -> Option<&Migration> {
        self.all.iter().find(|m| m.id == id)
    }

    /// Visible migrations in topological order.
    pub fn visible(&self) -> impl Iterator<Item = &Migration> {
        self.order.iter().filter_map(|id| self.get(id))
    }

    /// Replay visible migrations into a [`ProjectState`].
    ///
    /// # Errors
    /// [`MigrationError::State`] if an operation does not apply.
    pub fn project_state(&self) -> Result<ProjectState, MigrationError> {
        let mut state = ProjectState::new();
        for migration in self.visible() {
            for op in &migration.operations {
                op.apply_to_state(&mut state)?;
            }
        }
        Ok(state)
    }
}

fn resolve_dep(dep: &str, replaced_by: &HashMap<String, String>) -> String {
    replaced_by
        .get(dep)
        .cloned()
        .unwrap_or_else(|| dep.to_owned())
}

fn topo_ids(
    names: &BTreeSet<String>,
    deps: &BTreeMap<String, BTreeSet<String>>,
) -> Result<Vec<String>, MigrationError> {
    let mut remaining: BTreeMap<String, usize> = names
        .iter()
        .map(|id| (id.clone(), deps.get(id).map(BTreeSet::len).unwrap_or(0)))
        .collect();
    let mut ready: BTreeSet<String> = remaining
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(id, _)| id.clone())
        .collect();
    let mut order = Vec::new();
    let mut visited = HashSet::new();
    while let Some(id) = ready.iter().next().cloned() {
        ready.remove(&id);
        order.push(id.clone());
        visited.insert(id.clone());
        remaining.remove(&id);
        for (node, ds) in deps {
            if ds.contains(&id)
                && let Some(deg) = remaining.get_mut(node)
            {
                *deg = deg.saturating_sub(1);
                if *deg == 0 {
                    ready.insert(node.clone());
                }
            }
        }
    }
    if order.len() != names.len() {
        let leftover: Vec<String> = names
            .difference(&visited.into_iter().collect())
            .cloned()
            .collect();
        let cycle = find_cycle(deps, &leftover).unwrap_or(leftover);
        return Err(MigrationError::Cycle { nodes: cycle });
    }
    Ok(order)
}

fn find_cycle(
    deps: &BTreeMap<String, BTreeSet<String>>,
    leftover: &[String],
) -> Option<Vec<String>> {
    fn visit<'a>(
        node: &'a str,
        deps: &'a BTreeMap<String, BTreeSet<String>>,
        leftover: &HashSet<&str>,
        stack: &mut Vec<&'a str>,
        seen: &mut HashSet<&'a str>,
    ) -> Option<Vec<String>> {
        if let Some(i) = stack.iter().position(|n| *n == node) {
            let mut cycle: Vec<String> = stack[i..].iter().map(|s| (*s).to_owned()).collect();
            cycle.push(node.to_owned());
            return Some(cycle);
        }
        if !seen.insert(node) {
            return None;
        }
        stack.push(node);
        if let Some(ds) = deps.get(node) {
            for dep in ds {
                if leftover.contains(dep.as_str())
                    && let Some(c) = visit(dep, deps, leftover, stack, seen)
                {
                    return Some(c);
                }
            }
        }
        stack.pop();
        None
    }
    let set: HashSet<&str> = leftover.iter().map(String::as_str).collect();
    let mut seen = HashSet::new();
    for id in leftover {
        if let Some(c) = visit(id, deps, &set, &mut Vec::new(), &mut seen) {
            return Some(c);
        }
    }
    None
}

/// Whether `migration` should be treated as already applied given `history`.
pub fn is_applied(migration: &Migration, history: &HashSet<String>) -> bool {
    if history.contains(&migration.id) {
        return true;
    }
    !migration.replaces.is_empty() && migration.replaces.iter().all(|id| history.contains(id))
}
