//! Model metadata sanity checks.

use super::CheckIssue;
use axumapi_orm::ModelMeta;
use std::collections::BTreeMap;

pub(super) fn check(models: &[&'static ModelMeta]) -> Vec<CheckIssue> {
    let mut issues = Vec::new();
    issues.extend(duplicate_names(models));
    issues.extend(duplicate_tables(models));
    for model in models {
        issues.extend(check_model(model, models));
    }
    issues
}

fn is_registered(target: &ModelMeta, models: &[&'static ModelMeta]) -> bool {
    models
        .iter()
        .any(|m| m.name == target.name && m.table == target.table)
}

fn duplicate_names(models: &[&'static ModelMeta]) -> Vec<CheckIssue> {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for model in models {
        *seen.entry(model.name).or_default() += 1;
    }
    seen.into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(name, count)| {
            CheckIssue::error(
                "models.E002",
                format!("model name `{name}` is registered {count} times"),
            )
        })
        .collect()
}

/// Tables claimed by models and by auto-created many-to-many join tables.
fn duplicate_tables(models: &[&'static ModelMeta]) -> Vec<CheckIssue> {
    let mut owners: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for model in models {
        owners
            .entry(model.table)
            .or_default()
            .push(format!("model `{}`", model.name));
        for m2m in model.many_to_many.iter().filter(|m| m.through.is_none()) {
            owners
                .entry(m2m.through_table)
                .or_default()
                .push(format!("`{}.{}`", model.name, m2m.name));
        }
    }
    owners
        .into_iter()
        .filter(|(_, owners)| owners.len() > 1)
        .map(|(table, owners)| {
            CheckIssue::error(
                "models.E001",
                format!("table `{table}` is used by {}", owners.join(" and ")),
            )
        })
        .collect()
}

fn check_model(model: &ModelMeta, models: &[&'static ModelMeta]) -> Vec<CheckIssue> {
    let mut issues = Vec::new();
    let primary_keys = model.fields.iter().filter(|f| f.primary_key).count();
    if primary_keys == 0 {
        issues.push(CheckIssue::error(
            "models.E003",
            format!("model `{}` has no primary key", model.name),
        ));
    } else if primary_keys > 1 {
        issues.push(CheckIssue::error(
            "models.E007",
            format!(
                "model `{}` has {primary_keys} primary-key fields (exactly one is supported)",
                model.name
            ),
        ));
    }
    let mut columns: BTreeMap<&str, usize> = BTreeMap::new();
    for field in model.fields {
        *columns.entry(field.column).or_default() += 1;
    }
    for (column, count) in columns.into_iter().filter(|(_, count)| *count > 1) {
        issues.push(CheckIssue::error(
            "models.E006",
            format!(
                "model `{}` maps {count} fields to column `{column}`",
                model.name
            ),
        ));
    }
    for field in model.fields {
        if let Some(relation) = &field.relation {
            let target = (relation.target)();
            if !is_registered(target, models) {
                issues.push(CheckIssue::error(
                    "models.E004",
                    format!(
                        "`{}.{}` references model `{}`, which is not registered",
                        model.name, field.name, target.name
                    ),
                ));
            }
        }
    }
    for m2m in model.many_to_many {
        let target = (m2m.target)();
        if !is_registered(target, models) {
            issues.push(CheckIssue::error(
                "models.E005",
                format!(
                    "`{}.{}` relates to model `{}`, which is not registered",
                    model.name, m2m.name, target.name
                ),
            ));
        }
        if let Some(through) = m2m.through {
            let through = through();
            if !is_registered(through, models) {
                issues.push(CheckIssue::error(
                    "models.E005",
                    format!(
                        "`{}.{}` goes through model `{}`, which is not registered",
                        model.name, m2m.name, through.name
                    ),
                ));
            }
        }
    }
    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::*;

    fn ids(models: &[&'static ModelMeta]) -> Vec<&'static str> {
        check(models).iter().map(|i| i.id).collect()
    }

    #[test]
    fn a_consistent_set_is_clean() {
        assert!(ids(&[author(), book()]).is_empty());
        assert!(ids(&[author(), tagged()]).is_empty());
    }

    #[test]
    fn duplicate_tables_and_names() {
        assert_eq!(ids(&[author(), same_table()]), ["models.E001"]);
        assert_eq!(ids(&[author(), same_name()]), ["models.E002"]);
        let issues = check(&[author(), same_table()]);
        assert!(issues[0].message.contains("`authors`"));
    }

    #[test]
    fn join_tables_count_as_tables() {
        let issues = check(&[author(), book(), tagged_clash()]);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].id, "models.E001");
        assert!(issues[0].message.contains("`books`"));
        assert!(issues[0].message.contains("Clash.authors"));
    }

    #[test]
    fn missing_or_extra_primary_keys() {
        assert_eq!(ids(&[no_pk()]), ["models.E003"]);
        assert_eq!(ids(&[two_pks()]), ["models.E007"]);
    }

    #[test]
    fn duplicate_columns() {
        assert_eq!(ids(&[dup_column()]), ["models.E006"]);
    }

    #[test]
    fn unregistered_targets() {
        let issues = check(&[book()]);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id, "models.E004");
        assert!(issues[0].message.contains("`Book.author`"));
        assert!(issues[0].message.contains("`Author`"));
        let issues = check(&[tagged()]);
        assert_eq!(issues[0].id, "models.E005");
    }

    #[test]
    fn unmanaged_models_are_still_checked_for_names() {
        assert!(ids(&[unmanaged()]).is_empty());
    }
}
