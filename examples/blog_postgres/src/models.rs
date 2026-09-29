//! Database models of the blog.
//!
//! Table names are explicit so the migrations do not depend on how the Rust
//! type names are converted (`user` is a reserved word in PostgreSQL).

use axumapi::prelude::*;

/// A registered account.
#[derive(Debug, Clone, Model)]
#[model(table = "users", ordering = ["id"])]
pub struct User {
    /// Database key.
    #[field(primary_key, auto)]
    pub id: i64,
    /// Unique login name.
    #[field(unique, max_length = 32)]
    pub username: String,
    /// Salted password digest (see [`crate::auth::hash_password`]).
    pub password_hash: String,
    /// Space-separated scopes this account may be granted.
    pub scopes: String,
    /// When the account was created.
    #[field(auto_now_add)]
    pub created_at: DateTime<Utc>,
}

/// A blog post; tags are a many-to-many relation.
#[derive(Debug, Clone, Model)]
#[model(
    table = "posts",
    ordering = ["-id"],
    many_to_many(tags(Tag, through_table = "post_tags", related_name = "posts")),
)]
pub struct Post {
    /// Database key.
    #[field(primary_key, auto)]
    pub id: i64,
    /// Headline.
    #[field(max_length = 200)]
    pub title: String,
    /// URL-safe unique identifier derived from the title.
    #[field(unique, max_length = 220)]
    pub slug: String,
    /// Post text.
    pub body: String,
    /// Drafts (`false`) are only visible to their author.
    pub published: bool,
    /// The account that wrote the post.
    #[field(related_name = "posts")]
    pub author: ForeignKey<User>,
    /// When the post was created.
    #[field(auto_now_add)]
    pub created_at: DateTime<Utc>,
    /// When the post was last saved.
    #[field(auto_now)]
    pub updated_at: DateTime<Utc>,
}

/// A label that can be attached to many posts.
#[derive(Debug, Clone, Model)]
#[model(table = "tags", ordering = ["name"])]
pub struct Tag {
    /// Database key.
    #[field(primary_key, auto)]
    pub id: i64,
    /// Unique lower-case name.
    #[field(unique, max_length = 50)]
    pub name: String,
}

/// A reader's comment on a post.
#[derive(Debug, Clone, Model)]
#[model(table = "comments", ordering = ["id"])]
pub struct Comment {
    /// Database key.
    #[field(primary_key, auto)]
    pub id: i64,
    /// The commented post; comments go away with it.
    #[field(related_name = "comments", on_delete = "cascade")]
    pub post: ForeignKey<Post>,
    /// The commenting account.
    #[field(related_name = "comments")]
    pub author: ForeignKey<User>,
    /// Comment text.
    pub body: String,
    /// When the comment was written.
    #[field(auto_now_add)]
    pub created_at: DateTime<Utc>,
}

/// An opaque bearer token issued by `POST /token`.
#[derive(Debug, Clone, Model)]
#[model(table = "access_tokens")]
pub struct AccessToken {
    /// Database key.
    #[field(primary_key, auto)]
    pub id: i64,
    /// The random token value the client sends back.
    #[field(unique, max_length = 64)]
    pub token: String,
    /// The account the token belongs to.
    #[field(related_name = "tokens", on_delete = "cascade")]
    pub user: ForeignKey<User>,
    /// Space-separated scopes granted to this token.
    pub scopes: String,
    /// The token stops working at this instant.
    pub expires_at: DateTime<Utc>,
}

/// One line of the audit trail written by the signal receivers.
#[derive(Debug, Clone, Model)]
#[model(table = "audit_log", ordering = ["id"])]
pub struct AuditEntry {
    /// Database key.
    #[field(primary_key, auto)]
    pub id: i64,
    /// What happened, e.g. `post.created`.
    pub action: String,
    /// Primary key of the affected row.
    pub entity_id: i64,
    /// When it happened.
    #[field(auto_now_add)]
    pub created_at: DateTime<Utc>,
}

/// Every model, in dependency order, for `makemigrations` and tests.
pub fn all_models() -> [&'static axumapi::orm::ModelMeta; 6] {
    [
        User::META,
        Tag::META,
        Post::META,
        Comment::META,
        AccessToken::META,
        AuditEntry::META,
    ]
}
