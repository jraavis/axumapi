//! Authentication: the OAuth2 password flow with opaque tokens kept in a table.
//!
//! * `POST /token` exchanges a username and password for a bearer token.
//! * [`CurrentUser`] resolves that token back to an account, and
//!   `Security<CurrentUser, S>` also enforces the scopes `S` and documents the
//!   scheme in the OpenAPI document.
//!
//! # Security notes
//!
//! **[`hash_password`] is a placeholder and is NOT production password
//! hashing.** It is a salted, iterated FNV-1a digest, chosen only because the
//! example may not add dependencies. A real application must store passwords
//! with a memory-hard algorithm such as Argon2id, scrypt or bcrypt, and
//! compare with a vetted constant-time primitive.
//!
//! Tokens are random (two UUIDv4 values), stored in the clear and expire after
//! [`TOKEN_TTL_HOURS`]. Neither passwords nor tokens are ever logged.

use crate::db::Conn;
use crate::models::{AccessToken, User};
use axumapi::http::header::WWW_AUTHENTICATE;
use axumapi::http::{HeaderValue, StatusCode};
use axumapi::orm::uuid::Uuid;
use axumapi::prelude::*;
use axumapi::scopes;
use axumapi::security::{
    Authenticate, OAuth2PasswordBearer, OAuth2PasswordRequestForm, OAuth2Spec, check_scopes,
};
use http::request::Parts;

/// How long an issued token stays valid.
pub const TOKEN_TTL_HOURS: i64 = 1;

/// Scope required to write posts.
pub const POSTS_WRITE: &str = "posts:write";
/// Scope required to write comments.
pub const COMMENTS_WRITE: &str = "comments:write";
/// Scopes every new account may be granted.
pub const DEFAULT_SCOPES: &str = "posts:write comments:write";

const HASH_PREFIX: &str = "placeholder";
const HASH_ROUNDS: u32 = 10_000;
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0100_0000_01b3;

/// OAuth2 password flow of this API.
#[derive(Debug, Clone, Copy)]
pub struct BlogAuth;

impl OAuth2Spec for BlogAuth {
    const TOKEN_URL: &'static str = "/token";
    const SCOPES: &'static [(&'static str, &'static str)] = &[
        (POSTS_WRITE, "Create, edit and delete your own posts"),
        (COMMENTS_WRITE, "Comment on posts"),
    ];
    const SCHEME: &'static str = "OAuth2PasswordBearer";
}

scopes!(
    /// Required to create, edit and delete posts.
    pub WritePosts = ["posts:write"]
);
scopes!(
    /// Required to comment.
    pub WriteComments = ["comments:write"]
);

/// The authenticated account behind a valid, unexpired token.
#[derive(Debug, Clone)]
pub struct CurrentUser {
    /// Primary key of the account.
    pub id: i64,
    /// Login name.
    pub username: String,
    /// Scopes granted to the presented token.
    pub scopes: Vec<String>,
}

/// `401` with a `Bearer` challenge.
fn unauthorized(detail: &'static str) -> ApiError {
    ApiError::new(StatusCode::UNAUTHORIZED, detail)
        .with_header(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"))
}

impl Authenticate for CurrentUser {
    type Credentials = OAuth2PasswordBearer<BlogAuth>;

    async fn authenticate(
        credentials: Self::Credentials,
        required: &[&'static str],
        parts: &Parts,
    ) -> Result<Self, ApiError> {
        let Conn(db) = Conn::from_parts_ref(parts)?;
        let token = AccessToken::objects(&db)
            .filter(AccessToken::token.eq(credentials.token))
            .first()
            .await?
            .filter(|token| token.expires_at > Utc::now())
            .ok_or_else(|| unauthorized("Invalid or expired token."))?;
        check_scopes(required, token.scopes.split_whitespace())?;
        let user = User::objects(&db)
            .filter(User::id.eq(*token.user.id()))
            .first()
            .await?
            .ok_or_else(|| unauthorized("Invalid or expired token."))?;
        Ok(Self {
            id: user.id,
            username: user.username,
            scopes: token.scopes.split_whitespace().map(str::to_owned).collect(),
        })
    }
}

/// Digest of `password` under `salt`: [`HASH_ROUNDS`] rounds of FNV-1a.
///
/// Not a password hash; see the module documentation.
fn digest(salt: &str, password: &str) -> u64 {
    (0..HASH_ROUNDS).fold(FNV_OFFSET, |state, _| {
        salt.bytes()
            .chain(password.bytes())
            .chain(state.to_le_bytes())
            .fold(FNV_OFFSET, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
            })
    })
}

/// Hash `password` for storage as `placeholder$<salt>$<digest>`.
///
/// **Placeholder only, NOT production password hashing.** See the module
/// documentation.
pub fn hash_password(password: &str) -> String {
    let salt = Uuid::new_v4().simple().to_string();
    format!("{HASH_PREFIX}${salt}${:016x}", digest(&salt, password))
}

/// Compare two byte strings without stopping at the first difference.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0_u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Whether `password` matches a value produced by [`hash_password`].
pub fn verify_password(password: &str, stored: &str) -> bool {
    let mut fields = stored.split('$');
    let (Some(HASH_PREFIX), Some(salt), Some(expected), None) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return false;
    };
    let actual = format!("{:016x}", digest(salt, password));
    constant_time_eq(actual.as_bytes(), expected.as_bytes())
}

/// Response of `POST /token` (RFC 6749 section 5.1).
#[derive(Debug, Serialize, Schema)]
pub struct TokenResponse {
    /// The bearer token.
    pub access_token: String,
    /// Always `bearer`.
    pub token_type: String,
    /// Granted scopes, space-separated.
    pub scope: String,
}

/// Scopes to grant: the requested ones, or all the account holds when none
/// were requested.
///
/// # Errors
/// `400` when a requested scope is not one the account holds.
fn granted_scopes(held: &str, requested: &[String]) -> Result<String, ApiError> {
    if requested.is_empty() {
        return Ok(held.to_owned());
    }
    let held: Vec<&str> = held.split_whitespace().collect();
    match requested
        .iter()
        .find(|scope| !held.contains(&scope.as_str()))
    {
        Some(scope) => Err(ApiError::bad_request(format!(
            "Scope `{scope}` is not allowed."
        ))),
        None => Ok(requested.join(" ")),
    }
}

/// Exchange a username and password for a bearer token.
#[post("/token", tag = "auth")]
async fn issue_token(
    Conn(db): Conn,
    form: OAuth2PasswordRequestForm,
) -> Result<Json<TokenResponse>, ApiError> {
    let user = User::objects(&db)
        .filter(User::username.eq(form.username.clone()))
        .first()
        .await?;
    // Verify against a dummy value for unknown users so both failures cost the same.
    let stored = user
        .as_ref()
        .map_or("placeholder$0$0", |user| user.password_hash.as_str());
    let valid = verify_password(&form.password, stored);
    let user = match user {
        Some(user) if valid => user,
        _ => return Err(unauthorized("Incorrect username or password.")),
    };
    let scope = granted_scopes(&user.scopes, &form.scopes)?;
    let token = Uuid::new_v4().simple().to_string() + &Uuid::new_v4().simple().to_string();
    AccessToken::objects(&db)
        .create(AccessToken {
            id: 0,
            token: token.clone(),
            user: ForeignKey::new(user.id),
            scopes: scope.clone(),
            expires_at: Utc::now() + axumapi::orm::chrono::Duration::hours(TOKEN_TTL_HOURS),
        })
        .await?;
    Ok(Json(TokenResponse {
        access_token: token,
        token_type: "bearer".to_owned(),
        scope,
    }))
}

/// Routes of this module.
pub fn routes() -> Vec<Route> {
    routes![issue_token].into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_verify_and_are_salted() {
        let first = hash_password("correct horse");
        assert!(verify_password("correct horse", &first));
        assert!(!verify_password("wrong", &first));
        assert_ne!(first, hash_password("correct horse"));
        assert!(!first.contains("correct horse"));
    }

    #[test]
    fn malformed_hashes_never_verify() {
        for stored in [
            "",
            "placeholder",
            "x$y$z",
            "placeholder$a$b$c",
            "placeholder$0$0",
        ] {
            assert!(!verify_password("pw", stored));
        }
    }

    #[test]
    fn requested_scopes_must_be_held() {
        let held = "posts:write comments:write";
        assert_eq!(granted_scopes(held, &[]).unwrap_or_default(), held);
        assert_eq!(
            granted_scopes(held, &["posts:write".to_owned()]).unwrap_or_default(),
            "posts:write"
        );
        assert!(granted_scopes(held, &["admin".to_owned()]).is_err());
    }
}
