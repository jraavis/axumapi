//! Posts and tags.

use crate::auth::{CurrentUser, WritePosts};
use crate::db::Conn;
use crate::models::{Post, Tag};
use crate::pagination::{PageParams, Paginated, paginate};
use axumapi::orm::ModelOps;
use axumapi::prelude::*;
use axumapi::security::Security;

/// Longest slug stem kept from a title.
const MAX_SLUG_LEN: usize = 200;
/// Most tags one post may carry.
const MAX_TAGS: usize = 10;

/// A post as returned to clients.
#[derive(Debug, Serialize, Schema)]
pub struct PostOut {
    /// Database key.
    pub id: i64,
    /// Headline.
    pub title: String,
    /// URL-safe unique identifier used in `/posts/{slug}`.
    pub slug: String,
    /// Post text.
    pub body: String,
    /// Whether the post is visible to everyone.
    pub published: bool,
    /// Primary key of the author.
    pub author_id: i64,
    /// Tag names.
    pub tags: Vec<String>,
    /// When the post was created (RFC 3339).
    pub created_at: String,
    /// When the post was last saved (RFC 3339).
    pub updated_at: String,
}

/// A tag as returned to clients.
#[derive(Debug, Serialize, Schema)]
pub struct TagOut {
    /// Database key.
    pub id: i64,
    /// Tag name.
    pub name: String,
}

/// Body of `POST /posts`.
#[derive(Debug, Deserialize, Validate, Schema)]
pub struct NewPost {
    /// Headline.
    #[field(min_length = 1, max_length = 200)]
    pub title: String,
    /// Post text.
    #[field(min_length = 1)]
    pub body: String,
    /// Publish immediately (default `false`: a draft).
    #[serde(default)]
    pub published: bool,
    /// Tag names; created on demand (at most 10).
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Body of `PATCH /posts/{slug}`; absent fields stay unchanged.
#[derive(Debug, Deserialize, Validate, Schema)]
pub struct PatchPost {
    /// Replacement headline (the slug does not change).
    #[field(min_length = 1, max_length = 200)]
    pub title: Option<String>,
    /// Replacement text.
    #[field(min_length = 1)]
    pub body: Option<String>,
    /// Replacement visibility.
    pub published: Option<bool>,
    /// Replacement tag names (at most 10).
    pub tags: Option<Vec<String>>,
}

/// Query parameters of `GET /posts`.
#[derive(Debug, Deserialize, Validate, Schema)]
pub struct PostFilter {
    /// Only posts carrying this tag.
    pub tag: Option<String>,
}

/// Lower-case `title`, keep letters and digits, join words with `-`.
pub fn slugify(title: &str) -> String {
    let mut slug = String::new();
    for c in title.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug
        .trim_end_matches('-')
        .chars()
        .take(MAX_SLUG_LEN)
        .collect();
    if slug.is_empty() {
        "post".to_owned()
    } else {
        slug
    }
}

/// A slug for `title` that no post uses yet (`my-title`, `my-title-2`, ...).
async fn unique_slug(db: &Db, title: &str) -> Result<String, ApiError> {
    let stem = slugify(title);
    let mut candidate = stem.clone();
    for n in 2_u32.. {
        if !Post::objects(db)
            .filter(Post::slug.eq(candidate.clone()))
            .exists()
            .await?
        {
            break;
        }
        candidate = format!("{stem}-{n}");
    }
    Ok(candidate)
}

/// Normalize tag names: trimmed, lower-case, no blanks, no duplicates.
///
/// # Errors
/// `400` when more than [`MAX_TAGS`] remain or one is longer than 50 characters.
fn clean_tags(names: &[String]) -> Result<Vec<String>, ApiError> {
    let mut cleaned: Vec<String> = Vec::new();
    for name in names {
        let name = name.trim().to_lowercase();
        if name.is_empty() || cleaned.contains(&name) {
            continue;
        }
        if name.chars().count() > 50 {
            return Err(ApiError::bad_request("A tag is at most 50 characters."));
        }
        cleaned.push(name);
    }
    if cleaned.len() > MAX_TAGS {
        return Err(ApiError::bad_request(format!(
            "A post has at most {MAX_TAGS} tags."
        )));
    }
    Ok(cleaned)
}

/// Make `names` the complete tag set of `post`, creating missing tags.
async fn replace_tags(db: &Db, post: &Post, names: &[String]) -> Result<(), ApiError> {
    let mut keys = Vec::with_capacity(names.len());
    for name in names {
        let (tag, _) = Tag::objects(db)
            .get_or_create(Tag::name.eq(name.clone()), || Tag {
                id: 0,
                name: name.clone(),
            })
            .await?;
        keys.push(tag.id);
    }
    post.tags(db).set_pks(keys).await?;
    Ok(())
}

/// Convert `post` to its response form.
pub async fn post_out(db: &Db, post: Post) -> Result<PostOut, ApiError> {
    let tags = post
        .tags(db)
        .queryset()
        .all()
        .await?
        .into_iter()
        .map(|tag| tag.name)
        .collect();
    Ok(PostOut {
        id: post.id,
        title: post.title,
        slug: post.slug,
        body: post.body,
        published: post.published,
        author_id: *post.author.id(),
        tags,
        created_at: post.created_at.to_rfc3339(),
        updated_at: post.updated_at.to_rfc3339(),
    })
}

/// The post `slug` as seen by `viewer`: drafts exist only for their author.
///
/// # Errors
/// `404` when there is no such post or the viewer may not see it.
pub async fn visible_post(db: &Db, slug: &str, viewer: Option<i64>) -> Result<Post, ApiError> {
    Post::objects(db)
        .filter(Post::slug.eq(slug.to_owned()))
        .first()
        .await?
        .filter(|post| post.published || viewer == Some(*post.author.id()))
        .ok_or_else(|| ApiError::not_found("Post not found."))
}

/// The post `slug`, which `user` must have written.
///
/// # Errors
/// `404` for an unknown or invisible post, `403` when it belongs to someone else.
async fn owned_post(db: &Db, slug: &str, user: &CurrentUser) -> Result<Post, ApiError> {
    let post = visible_post(db, slug, Some(user.id)).await?;
    if *post.author.id() == user.id {
        Ok(post)
    } else {
        Err(ApiError::new(
            axumapi::http::StatusCode::FORBIDDEN,
            "Only the author may change this post.",
        ))
    }
}

/// List published posts, newest first, optionally by tag.
#[get("/posts", tag = "posts")]
async fn list_posts(
    Conn(db): Conn,
    Query(page): Query<PageParams>,
    Query(filter): Query<PostFilter>,
) -> Result<Json<Paginated<PostOut>>, ApiError> {
    let queryset = match filter.tag {
        Some(name) => match Tag::objects(&db).filter(Tag::name.eq(name)).first().await? {
            Some(tag) => tag.posts(&db),
            None => Post::objects(&db).none(),
        },
        None => Post::objects(&db),
    };
    let queryset = queryset.filter(Post::published.eq(true));
    let out = paginate(queryset, page, |post| post_out(&db, post)).await?;
    Ok(Json(out))
}

/// Create a post; requires the `posts:write` scope.
#[post("/posts", status = 201, tag = "posts")]
async fn create_post(
    Security(user, _): Security<CurrentUser, WritePosts>,
    Conn(db): Conn,
    Json(body): Json<NewPost>,
) -> Result<Json<PostOut>, ApiError> {
    let tags = clean_tags(&body.tags)?;
    let author = user.id;
    let post = db
        .transaction(|tx| async move {
            let mut post = Post {
                id: 0,
                slug: unique_slug(&tx, &body.title).await?,
                title: body.title,
                body: body.body,
                published: body.published,
                author: ForeignKey::new(author),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            };
            post.save(&tx).await?;
            replace_tags(&tx, &post, &tags).await?;
            Ok::<_, ApiError>(post)
        })
        .await?;
    Ok(Json(post_out(&db, post).await?))
}

/// Fetch one post; drafts are visible to their author only.
#[get("/posts/{slug}", tag = "posts")]
async fn get_post(
    Conn(db): Conn,
    viewer: Option<Security<CurrentUser>>,
    Path(slug): Path<String>,
) -> Result<Json<PostOut>, ApiError> {
    let viewer = viewer.map(|Security(user, _)| user.id);
    let post = visible_post(&db, &slug, viewer).await?;
    Ok(Json(post_out(&db, post).await?))
}

/// Edit your own post; requires the `posts:write` scope.
#[patch("/posts/{slug}", tag = "posts")]
async fn patch_post(
    Security(user, _): Security<CurrentUser, WritePosts>,
    Conn(db): Conn,
    Path(slug): Path<String>,
    Json(body): Json<PatchPost>,
) -> Result<Json<PostOut>, ApiError> {
    let tags = body.tags.as_deref().map(clean_tags).transpose()?;
    let mut post = owned_post(&db, &slug, &user).await?;
    if let Some(title) = body.title {
        post.title = title;
    }
    if let Some(text) = body.body {
        post.body = text;
    }
    if let Some(published) = body.published {
        post.published = published;
    }
    let post = db
        .transaction(|tx| async move {
            post.save(&tx).await?;
            if let Some(tags) = tags {
                replace_tags(&tx, &post, &tags).await?;
            }
            Ok::<_, ApiError>(post)
        })
        .await?;
    Ok(Json(post_out(&db, post).await?))
}

/// Delete your own post and its comments; requires the `posts:write` scope.
#[delete("/posts/{slug}", tag = "posts")]
async fn delete_post(
    Security(user, _): Security<CurrentUser, WritePosts>,
    Conn(db): Conn,
    Path(slug): Path<String>,
) -> Result<NoContent, ApiError> {
    let post = owned_post(&db, &slug, &user).await?;
    db.transaction(|tx| async move { post.delete(&tx).await })
        .await?;
    Ok(NoContent)
}

/// List tags alphabetically.
#[get("/tags", tag = "tags")]
async fn list_tags(
    Conn(db): Conn,
    Query(page): Query<PageParams>,
) -> Result<Json<Paginated<TagOut>>, ApiError> {
    let out = paginate(Tag::objects(&db), page, |tag| async move {
        Ok(TagOut {
            id: tag.id,
            name: tag.name,
        })
    })
    .await?;
    Ok(Json(out))
}

/// Routes of this module.
pub fn routes() -> Vec<Route> {
    routes![
        list_posts,
        create_post,
        get_post,
        patch_post,
        delete_post,
        list_tags
    ]
    .into_iter()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_lowercase_words_joined_by_dashes() {
        assert_eq!(slugify("Hello, World!"), "hello-world");
        assert_eq!(slugify("  Rust -- 2024  "), "rust-2024");
        assert_eq!(slugify("???"), "post");
    }

    #[test]
    fn tags_are_normalized_and_limited() {
        let names = ["Rust ", "rust", "", "Web"].map(String::from);
        assert_eq!(
            clean_tags(&names).ok(),
            Some(vec!["rust".into(), "web".into()])
        );
        let many: Vec<String> = (0..=MAX_TAGS).map(|n| format!("t{n}")).collect();
        assert!(clean_tags(&many).is_err());
        assert!(clean_tags(&["x".repeat(51)]).is_err());
    }
}
