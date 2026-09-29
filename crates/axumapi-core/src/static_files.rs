//! Static file serving.

use crate::app::App;
use std::path::PathBuf;

impl App {
    /// Serve files from `dir` under URL `prefix` (for example `/assets`).
    ///
    /// The mount is not documented in OpenAPI. Path traversal (`..` segments)
    /// is rejected by `tower-http`'s `ServeDir`. `prefix` must be a non-root
    /// path; `/` is ignored because the underlying nest panics at the root.
    #[must_use]
    pub fn static_files(mut self, prefix: &str, dir: impl Into<PathBuf>) -> Self {
        let prefix = prefix.to_owned();
        let dir = dir.into();
        self.services.push(Box::new(move |router: axum::Router| {
            if prefix.is_empty() || prefix == "/" {
                router
            } else {
                router.nest_service(&prefix, tower_http::services::ServeDir::new(dir))
            }
        }));
        self
    }
}
