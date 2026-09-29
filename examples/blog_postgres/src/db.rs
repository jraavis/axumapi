//! Access to the application database from handlers and authentication.

use crate::receivers;
use axumapi::orm::Databases;
use axumapi::prelude::*;
use http::request::Parts;
use std::sync::Arc;

/// Handler argument holding the `"default"` database.
///
/// The command line registers the databases (`App::database`), so the model
/// signal receivers cannot be attached when the app is built. `Conn` attaches
/// them the first time a request reaches the database
/// ([`receivers::install`], idempotent).
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
        receivers::install(&db);
        Ok(Self(db))
    }
}

impl FromRequestParts for Conn {
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        Self::from_parts_ref(parts)
    }
}
