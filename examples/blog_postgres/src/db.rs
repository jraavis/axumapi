//! Access to the application database from handlers and authentication.

use http::request::Parts;
use siderite::orm::Databases;
use siderite::prelude::*;
use std::sync::Arc;

/// Handler argument holding the `"default"` database.
///
/// The model signal receivers are attached where the database is created:
/// `AppCli::configure_db` in the binary, `TestDatabase::with_signals` in tests.
#[derive(Debug, Clone)]
pub struct Conn(pub Db);

impl Conn {
    /// Look the database up in the request extensions.
    ///
    /// # Errors
    /// `500` when the app has no `"default"` database registered.
    pub fn from_parts_ref(parts: &Parts) -> Result<Self, ApiError> {
        let db = parts
            .extensions
            .get::<Arc<Databases>>()
            .and_then(|databases| databases.default_db())
            .ok_or_else(|| ApiError::internal("no `default` database registered"))?
            .clone();
        Ok(Self(db))
    }
}

impl FromRequestParts for Conn {
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        Self::from_parts_ref(parts)
    }
}
