//! [`TestClientBuilder`]: configure an [`App`] for testing, then build.

use crate::TestClient;
use siderite_core::di::{Dependency, RequestHead};
use siderite_core::{ApiError, App, ServerError};
use siderite_orm::Db;
use std::future::Future;

/// Applies test-only configuration to an [`App`] before it becomes a
/// [`TestClient`].
///
/// A built client owns an immutable service, so overrides and databases must
/// be set here rather than on the client.
#[derive(Debug)]
pub struct TestClientBuilder {
    app: App,
}

impl TestClientBuilder {
    pub(crate) fn new(app: App) -> Self {
        Self { app }
    }

    /// Replace `T::resolve` with `f` for every request (see
    /// [`App::override_dependency`]).
    #[must_use]
    pub fn override_dependency<T, F, Fut>(self, f: F) -> Self
    where
        T: Dependency,
        F: Fn(RequestHead) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<T, ApiError>> + Send + 'static,
    {
        Self {
            app: self.app.override_dependency(f),
        }
    }

    /// Replace `T::resolve` with a clone of `value`.
    #[must_use]
    pub fn override_value<T: Dependency + Clone>(self, value: T) -> Self {
        Self {
            app: self.app.override_value(value),
        }
    }

    /// Register `db` under `alias`, replacing any database the app already
    /// registered with that alias (see [`App::database`]).
    #[must_use]
    pub fn with_database(self, alias: impl Into<String>, db: Db) -> Self {
        Self {
            app: self.app.database(alias, db),
        }
    }

    /// Build the client without running startup hooks.
    ///
    /// # Errors
    /// The app's configuration error.
    pub fn try_build(self) -> Result<TestClient, ServerError> {
        TestClient::try_new(self.app)
    }

    /// Build the client, for use in tests.
    ///
    /// # Panics
    /// Panics if the app is misconfigured; use [`try_build`](Self::try_build)
    /// to assert on configuration errors.
    pub fn build(self) -> TestClient {
        TestClient::new(self.app)
    }

    /// Build the client and run the app's startup hooks (see
    /// [`TestClient::start`]).
    ///
    /// # Errors
    /// Configuration or startup-hook failure.
    pub async fn start(self) -> Result<TestClient, ServerError> {
        TestClient::start(self.app).await
    }
}
