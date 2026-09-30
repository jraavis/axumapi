//! Route and OpenAPI checks.

use super::CheckIssue;
use siderite_core::App;

pub(super) fn check(app: &App) -> Vec<CheckIssue> {
    match app.openapi() {
        Ok(_) => Vec::new(),
        Err(err) => vec![CheckIssue::error(
            "openapi.E001",
            format!("the OpenAPI document cannot be generated: {err}"),
        )],
    }
}

pub(super) fn check_build(app: App) -> Vec<CheckIssue> {
    match app.into_router_service() {
        Ok(_) => Vec::new(),
        Err(err) => vec![CheckIssue::error(
            "routes.E001",
            format!("the app cannot be built: {err}"),
        )],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use siderite_core::get;

    #[test]
    fn a_clean_app_has_no_issues() {
        let app = App::new().route("/a", get(|| async { "a" }));
        assert!(check(&app).is_empty());
        assert!(check_build(app).is_empty());
    }

    #[test]
    fn duplicate_operation_ids_are_reported() {
        let app = App::new()
            .route("/a", get(|| async { "a" }).operation_id("dup"))
            .route("/b", get(|| async { "b" }).operation_id("dup"));
        let issues = check(&app);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id, "openapi.E001");
        assert!(issues[0].message.contains("dup"), "{}", issues[0]);
    }

    #[test]
    fn duplicate_routes_fail_the_build_check() {
        let app = App::new()
            .route("/a", get(|| async { "1" }))
            .route("/a", get(|| async { "2" }));
        let issues = check_build(app);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id, "routes.E001");
    }
}
