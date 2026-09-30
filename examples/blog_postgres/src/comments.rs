//! Comments on posts.

use crate::auth::{CurrentUser, WriteComments};
use crate::db::Conn;
use crate::models::Comment;
use crate::pagination::{PageParams, Paginated, paginate};
use crate::posts::visible_post;
use siderite::orm::ModelOps;
use siderite::prelude::*;
use siderite::security::Security;

/// A comment as returned to clients.
#[derive(Debug, Serialize, Schema)]
pub struct CommentOut {
    /// Database key.
    pub id: i64,
    /// Primary key of the commenting account.
    pub author_id: i64,
    /// Comment text.
    pub body: String,
    /// When the comment was written (RFC 3339).
    pub created_at: String,
}

impl From<Comment> for CommentOut {
    fn from(comment: Comment) -> Self {
        Self {
            id: comment.id,
            author_id: *comment.author.id(),
            body: comment.body,
            created_at: comment.created_at.to_rfc3339(),
        }
    }
}

/// Body of `POST /posts/{slug}/comments`.
#[derive(Debug, Deserialize, Validate, Schema)]
pub struct NewComment {
    /// Comment text, 1 to 2000 characters.
    #[field(min_length = 1, max_length = 2000)]
    pub body: String,
}

/// List the comments of a post, oldest first.
#[get("/posts/{slug}/comments", tag = "comments")]
async fn list_comments(
    Conn(db): Conn,
    viewer: Option<Security<CurrentUser>>,
    Path(slug): Path<String>,
    Query(page): Query<PageParams>,
) -> Result<Json<Paginated<CommentOut>>, ApiError> {
    let viewer = viewer.map(|Security(user, _)| user.id);
    let post = visible_post(&db, &slug, viewer).await?;
    let queryset = Comment::objects(&db).filter(Comment::post.eq(post.id));
    let out = paginate(queryset, page, |comment| async move { Ok(comment.into()) }).await?;
    Ok(Json(out))
}

/// Comment on a post; requires the `comments:write` scope.
#[post("/posts/{slug}/comments", status = 201, tag = "comments")]
async fn create_comment(
    Security(user, _): Security<CurrentUser, WriteComments>,
    Conn(db): Conn,
    Path(slug): Path<String>,
    Json(body): Json<NewComment>,
) -> Result<Json<CommentOut>, ApiError> {
    let post = visible_post(&db, &slug, Some(user.id)).await?;
    let mut comment = Comment {
        id: 0,
        post: ForeignKey::new(post.id),
        author: ForeignKey::new(user.id),
        body: body.body,
        created_at: Utc::now(),
    };
    comment.save(&db).await?;
    Ok(Json(comment.into()))
}

/// Routes of this module.
pub fn routes() -> Vec<Route> {
    routes![list_comments, create_comment].into_iter().collect()
}
