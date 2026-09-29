//! Account registration and the profile of the caller.

use crate::auth::{CurrentUser, DEFAULT_SCOPES, hash_password};
use crate::db::Conn;
use crate::models::User;
use axumapi::orm::ModelOps;
use axumapi::prelude::*;
use axumapi::security::Security;

/// Body of `POST /users`.
#[derive(Debug, Deserialize, Validate, Schema)]
pub struct NewUser {
    /// Login name, 3 to 32 characters.
    #[field(min_length = 3, max_length = 32)]
    pub username: String,
    /// Password, at least 8 characters. Never stored or logged in the clear.
    #[field(min_length = 8, max_length = 128)]
    pub password: String,
}

/// Public view of an account: never includes the password digest.
#[derive(Debug, Serialize, Schema)]
pub struct UserOut {
    /// Database key.
    pub id: i64,
    /// Login name.
    pub username: String,
    /// Scopes the account may be granted.
    pub scopes: Vec<String>,
    /// When the account was created (RFC 3339).
    pub created_at: String,
}

impl From<User> for UserOut {
    fn from(user: User) -> Self {
        Self {
            id: user.id,
            username: user.username,
            scopes: user.scopes.split_whitespace().map(str::to_owned).collect(),
            created_at: user.created_at.to_rfc3339(),
        }
    }
}

/// Register an account.
#[post("/users", status = 201, tag = "users")]
async fn register(Conn(db): Conn, Json(body): Json<NewUser>) -> Result<Json<UserOut>, ApiError> {
    let mut user = User {
        id: 0,
        username: body.username,
        password_hash: hash_password(&body.password),
        scopes: DEFAULT_SCOPES.to_owned(),
        created_at: Utc::now(),
    };
    user.save(&db).await?;
    Ok(Json(user.into()))
}

/// The account behind the bearer token.
#[get("/users/me", tag = "users")]
async fn me(
    Security(current, _): Security<CurrentUser>,
    Conn(db): Conn,
) -> Result<Json<UserOut>, ApiError> {
    let user = User::objects(&db).get(User::id.eq(current.id)).await?;
    Ok(Json(user.into()))
}

/// Routes of this module.
pub fn routes() -> Vec<Route> {
    routes![register, me].into_iter().collect()
}
