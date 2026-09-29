//! Checks of the models against the backend of the `default` database.
//!
//! Models live on the `default` alias. The backend is chosen by the URL
//! scheme, so no connection is opened.

use super::{CheckIssue, has_managed_models};
use crate::connect::backend_kind;
use crate::settings::{CliSettings, DEFAULT_DATABASE};
use axumapi_migrations::{MigrationError, ProjectState, diff, schema_editor};
use axumapi_orm::{BackendCapabilities, BackendKind, Feature, ModelMeta};

fn capabilities(kind: BackendKind) -> Option<BackendCapabilities> {
    match kind {
        BackendKind::Postgres => Some(BackendCapabilities::postgres()),
        BackendKind::Sqlite => Some(BackendCapabilities::sqlite()),
        BackendKind::MySql => Some(BackendCapabilities::mysql()),
        BackendKind::MongoDb => Some(BackendCapabilities::mongodb()),
        BackendKind::Redis => Some(BackendCapabilities::redis()),
        _ => None,
    }
}

pub(super) fn check(models: &[&'static ModelMeta], settings: &CliSettings) -> Vec<CheckIssue> {
    if models.is_empty() {
        return Vec::new();
    }
    // A missing database or an unknown scheme is reported by the config check.
    let Some(kind) = settings
        .database_url(DEFAULT_DATABASE)
        .and_then(backend_kind)
    else {
        return Vec::new();
    };
    let Some(caps) = capabilities(kind) else {
        return Vec::new();
    };
    if kind == BackendKind::Redis {
        return vec![CheckIssue::error(
            "backend.E001",
            format!(
                "the `{DEFAULT_DATABASE}` database is Redis, a key/value store; \
                 models cannot be stored in it"
            ),
        )];
    }
    let mut issues = check_relations(models, &caps, kind);
    if has_managed_models(models) {
        issues.extend(check_schema(models, kind));
    }
    issues
}

/// Foreign keys and many-to-many relations need joins.
fn check_relations(
    models: &[&'static ModelMeta],
    caps: &BackendCapabilities,
    kind: BackendKind,
) -> Vec<CheckIssue> {
    let Err(err) = caps.require(Feature::Joins) else {
        return Vec::new();
    };
    models
        .iter()
        .filter(|m| !m.many_to_many.is_empty() || m.fields.iter().any(|f| f.relation.is_some()))
        .map(|m| {
            CheckIssue::error(
                "backend.E002",
                format!(
                    "model `{}` has relations, which need joins on {kind:?}: {err}",
                    m.name
                ),
            )
        })
        .collect()
}

/// Render the initial schema for the backend; whatever it cannot express
/// surfaces as an error before a migration is ever written.
fn check_schema(models: &[&'static ModelMeta], kind: BackendKind) -> Vec<CheckIssue> {
    let operations = diff(&ProjectState::new(), &ProjectState::from_metas(models));
    match schema_editor::statements(kind, &ProjectState::new(), &operations) {
        Ok(_) => Vec::new(),
        Err(MigrationError::UnsupportedBackend(_)) => vec![CheckIssue::warning(
            "backend.W001",
            format!(
                "schema migrations are not supported by {kind:?}; `migrate` will refuse to run"
            ),
        )],
        Err(err) => vec![CheckIssue::error(
            "backend.E003",
            format!("the schema cannot be created on {kind:?}: {err}"),
        )],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::*;

    fn settings(url: &str) -> CliSettings {
        CliSettings::new().database("default", url)
    }

    fn ids(models: &[&'static ModelMeta], url: &str) -> Vec<&'static str> {
        check(models, &settings(url)).iter().map(|i| i.id).collect()
    }

    #[test]
    fn sql_backends_accept_relations() {
        let models = [author(), book(), tagged()];
        for url in ["sqlite::memory:", "postgres://h/db", "mysql://h/db"] {
            assert!(ids(&models, url).is_empty(), "{url}");
        }
    }

    #[test]
    fn redis_cannot_hold_models() {
        assert_eq!(ids(&[author()], "redis://h"), ["backend.E001"]);
    }

    #[test]
    fn mongodb_rejects_relations_and_warns_about_migrations() {
        let found = ids(&[author(), book(), tagged()], "mongodb://h/db");
        assert_eq!(
            found,
            ["backend.E002", "backend.E002", "backend.W001"],
            "{found:?}"
        );
        assert_eq!(ids(&[author()], "mongodb://h/db"), ["backend.W001"]);
    }

    #[test]
    fn mysql_rejects_a_unique_text_without_length() {
        let issues = check(&[unique_text()], &settings("mysql://h/db"));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id, "backend.E003");
        assert!(issues[0].message.contains("max_length"), "{}", issues[0]);
        // The same model is fine on the other SQL backends.
        assert!(ids(&[unique_text()], "postgres://h/db").is_empty());
        assert!(ids(&[unique_text()], "sqlite::memory:").is_empty());
    }

    #[test]
    fn missing_or_unknown_databases_are_left_to_the_config_check() {
        assert!(check(&[author()], &CliSettings::new()).is_empty());
        assert!(ids(&[author()], "ftp://h").is_empty());
        assert!(check(&[], &settings("redis://h")).is_empty());
    }

    #[test]
    fn unmanaged_models_have_no_schema_to_check() {
        assert!(ids(&[unmanaged()], "mongodb://h/db").is_empty());
    }
}
