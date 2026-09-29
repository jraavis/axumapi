//! Migration graph checks. No database is needed.

use super::CheckIssue;
use axumapi_migrations::{MigrationGraph, ProjectState, diff, load_dir};
use axumapi_orm::ModelMeta;
use std::path::Path;

pub(super) fn check(models: &[&'static ModelMeta], dir: &Path) -> Vec<CheckIssue> {
    let loaded = match load_dir(dir) {
        Ok(loaded) => loaded,
        Err(err) => {
            return vec![CheckIssue::error(
                "migrations.E001",
                format!("migrations in `{}` cannot be loaded: {err}", dir.display()),
            )];
        }
    };
    let recorded = if loaded.is_empty() {
        ProjectState::new()
    } else {
        let graph = match MigrationGraph::build(loaded) {
            Ok(graph) => graph,
            Err(err) => {
                return vec![CheckIssue::error(
                    "migrations.E002",
                    format!("the migration graph is invalid: {err}"),
                )];
            }
        };
        match graph.project_state() {
            Ok(state) => state,
            Err(err) => {
                return vec![CheckIssue::error(
                    "migrations.E003",
                    format!("the migrations do not replay cleanly: {err}"),
                )];
            }
        }
    };
    let pending = diff(&recorded, &ProjectState::from_metas(models));
    if pending.is_empty() {
        return Vec::new();
    }
    vec![CheckIssue::warning(
        "migrations.W001",
        format!(
            "models have {} change(s) that no migration records; run `makemigrations`",
            pending.len()
        ),
    )]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::fixtures::*;
    use axumapi_migrations::make_migrations;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "axumapi-cli-check-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, json: &str) {
        std::fs::write(dir.join(name), json).unwrap();
    }

    #[test]
    fn missing_directory_with_models_warns_to_make_migrations() {
        let dir = temp_dir("missing").join("nope");
        let issues = check(&[author()], &dir);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id, "migrations.W001");
        assert!(issues[0].message.contains("makemigrations"));
    }

    #[test]
    fn no_models_and_no_migrations_is_clean() {
        assert!(check(&[], &temp_dir("empty")).is_empty());
    }

    #[test]
    fn up_to_date_migrations_are_clean() {
        let dir = temp_dir("clean");
        let models = [author(), book()];
        make_migrations(&models, &dir, None, false)
            .unwrap()
            .unwrap();
        assert!(check(&models, &dir).is_empty());
    }

    #[test]
    fn new_model_changes_are_a_warning() {
        let dir = temp_dir("stale");
        make_migrations(&[author()], &dir, None, false)
            .unwrap()
            .unwrap();
        let issues = check(&[author(), book()], &dir);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id, "migrations.W001");
    }

    #[test]
    fn unreadable_json_is_an_error() {
        let dir = temp_dir("badjson");
        write(&dir, "0001_bad.json", "{ not json");
        let issues = check(&[], &dir);
        assert_eq!(issues[0].id, "migrations.E001");
    }

    #[test]
    fn missing_dependencies_are_an_error() {
        let dir = temp_dir("dep");
        let first = make_migrations(&[author()], &dir, None, false)
            .unwrap()
            .unwrap();
        let json = std::fs::read_to_string(dir.join(format!("{}.json", first.id))).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
        value["id"] = "0002_orphan".into();
        value["dependencies"] = serde_json::json!(["0001_gone"]);
        write(&dir, "0002_orphan.json", &value.to_string());
        let issues = check(&[author()], &dir);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].id, "migrations.E002");
        assert!(issues[0].message.contains("0001_gone"));
    }
}
