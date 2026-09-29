//! Static file serving.

use crate::app::App;
use std::path::PathBuf;

impl App {
    /// Serve files from `dir` under URL `prefix` (for example `/assets`).
    ///
    /// The mount is not documented in OpenAPI. Path traversal (`..` segments)
    /// is rejected by `tower-http`'s `ServeDir`. A root prefix (`/` or empty)
    /// serves `dir` as the fallback for every path no route matches.
    #[must_use]
    pub fn static_files(mut self, prefix: &str, dir: impl Into<PathBuf>) -> Self {
        let prefix = prefix.trim_end_matches('/').to_owned();
        let dir = dir.into();
        if prefix.is_empty() {
            // Unknown paths that are not files still get the 404 problem document.
            let not_found =
                axum::handler::HandlerWithoutStateExt::into_service(crate::app::route_not_found);
            let service = tower_http::services::ServeDir::new(dir).fallback(not_found);
            self.fallback = Some(Box::new(move |router: axum::Router| {
                router.fallback_service(service)
            }));
        } else {
            let service = tower_http::services::ServeDir::new(dir);
            self.services.push(Box::new(move |router: axum::Router| {
                router.nest_service(&prefix, service)
            }));
        }
        self
    }
}
