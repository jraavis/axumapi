//! Two databases behind one API.
//!
//! * `default` holds the `users` table.
//! * `analytics` holds the `events` table (SQLite file or PostgreSQL).
//!
//! A [`DatabaseRouter`] sends each model to its database by table name.
//! Handlers ask [`Databases`] for a queryset (`objects`) or a write handle
//! (`for_write`) and never name an alias, except where they deliberately
//! bypass the router with `using(alias)`.
//!
//! A queryset lives on one database, so a SQL join between `users` and
//! `events` is impossible: `user_id` on an event is a plain number, and
//! `GET /users/{id}/events` joins in the application. Combining querysets of
//! different databases is rejected (`GET /cross-database-union`).

use siderite::orm::router::DatabaseRouter;
use siderite::orm::{Databases, ModelMeta, OrmError, QueryError};
use siderite::prelude::*;
use siderite_migrations::{ProjectState, diff, schema_editor};

/// Alias of the users database.
pub const USERS: &str = "default";
/// Alias of the analytics database.
pub const ANALYTICS: &str = "analytics";

/// An account, stored in the `default` database.
#[derive(Debug, Clone, Model, Serialize, Deserialize, Validate, Schema)]
#[model(table = "users", ordering = ["id"])]
pub struct User {
    /// Database key.
    #[field(primary_key, auto)]
    #[serde(default)]
    pub id: i64,
    /// Unique display name.
    #[field(unique, min_length = 1, max_length = 100)]
    pub name: String,
}

/// A tracked event, stored in the `analytics` database.
#[derive(Debug, Clone, Model, Serialize, Deserialize, Validate, Schema)]
#[model(table = "events", ordering = ["id"])]
pub struct Event {
    /// Database key.
    #[field(primary_key, auto)]
    #[serde(default)]
    pub id: i64,
    /// Key of the user in the *other* database (no foreign key is possible).
    pub user_id: i64,
    /// What happened.
    #[field(min_length = 1, max_length = 100)]
    pub kind: String,
}

/// Every model of the application.
pub fn all_models() -> [&'static ModelMeta; 2] {
    [User::META, Event::META]
}

/// Routes `events` to `analytics` and everything else to `default`.
#[derive(Debug, Clone, Copy)]
pub struct AnalyticsRouter;

impl AnalyticsRouter {
    fn alias(model: &ModelMeta) -> Option<&'static str> {
        (model.table == "events").then_some(ANALYTICS)
    }
}

impl DatabaseRouter for AnalyticsRouter {
    fn db_for_read(&self, model: &ModelMeta) -> Option<&str> {
        Self::alias(model)
    }

    fn db_for_write(&self, model: &ModelMeta) -> Option<&str> {
        Self::alias(model)
    }

    fn allow_migrate(&self, alias: &str, model: &ModelMeta) -> bool {
        Self::alias(model).unwrap_or(USERS) == alias
    }
}

/// Register the two databases under their aliases with the router attached.
pub fn registry(users: Db, analytics: Db) -> Databases {
    Databases::new()
        .with(USERS, users)
        .with(ANALYTICS, analytics)
        .with_router(AnalyticsRouter)
}

/// Whether `table` exists on `db`.
async fn table_exists(db: &Db, table: &str) -> bool {
    db.raw_sql(&format!("SELECT * FROM {table} WHERE 1 = 0"), vec![])
        .await
        .is_ok()
}

/// Create, on every alias, the tables of the models the router allows there.
/// Tables that already exist are left alone, so this is safe on every start.
///
/// # Errors
/// DDL rendering or execution failures.
pub async fn provision(databases: &Databases) -> Result<(), ApiError> {
    for alias in databases.aliases() {
        let Some(db) = databases.get(alias) else {
            continue;
        };
        let mut missing = Vec::new();
        for model in all_models() {
            if databases.allow_migrate(alias, model) && !table_exists(db, model.table).await {
                missing.push(model);
            }
        }
        let operations = diff(&ProjectState::new(), &ProjectState::from_metas(&missing));
        let statements =
            schema_editor::statements(db.capabilities().kind, &ProjectState::new(), &operations)
                .map_err(ApiError::internal)?;
        for sql in statements {
            db.execute_script(&sql).await?;
        }
    }
    Ok(())
}

/// Application serving the API on `databases`.
pub fn app(databases: Databases) -> App {
    App::new()
        .title("Polyglot")
        .version("1.0.0")
        .description("Users on one database, analytics events on another.")
        .databases(databases)
        .routes(routes![
            create_user,
            list_users,
            create_event,
            user_events,
            count_events,
            cross_database_union
        ])
}

/// Body of `POST /events`.
#[derive(Debug, Deserialize, Validate, Schema)]
pub struct NewEvent {
    /// The user the event belongs to.
    pub user_id: i64,
    /// What happened.
    #[field(min_length = 1, max_length = 100)]
    pub kind: String,
}

/// Body of `POST /users`.
#[derive(Debug, Deserialize, Validate, Schema)]
pub struct NewUser {
    /// Unique display name.
    #[field(min_length = 1, max_length = 100)]
    pub name: String,
}

/// Query of `GET /events/count`.
#[derive(Debug, Deserialize, Validate, Schema)]
pub struct CountParams {
    /// Alias to read from, bypassing the router (default `analytics`).
    pub database: Option<String>,
}

/// Response of `GET /events/count`.
#[derive(Debug, Serialize, Schema)]
pub struct Count {
    /// The alias that was read.
    pub database: String,
    /// Number of events found there.
    pub events: u64,
}

/// Response of `GET /users/{id}/events`.
#[derive(Debug, Serialize, Schema)]
pub struct UserEvents {
    /// The user, from the `default` database.
    pub user: User,
    /// Their events, from the `analytics` database.
    pub events: Vec<Event>,
}

/// An unknown alias is the caller's mistake here, not a server fault.
fn alias_error(err: OrmError) -> ApiError {
    match err {
        OrmError::UnknownDatabase(alias) => {
            ApiError::not_found(format!("Unknown database `{alias}`."))
        }
        other => other.into(),
    }
}

/// Create a user (written where the router says: `default`).
#[post("/users", status = 201)]
async fn create_user(
    State(databases): State<Databases>,
    Json(body): Json<NewUser>,
) -> Result<Json<User>, ApiError> {
    let db = databases.for_write::<User>()?;
    let user = User::objects(db)
        .create(User {
            id: 0,
            name: body.name,
        })
        .await?;
    Ok(Json(user))
}

/// List users.
#[get("/users")]
async fn list_users(State(databases): State<Databases>) -> Result<Json<Vec<User>>, ApiError> {
    Ok(Json(databases.objects::<User>()?.all().await?))
}

/// Record an event in the analytics database after checking the user in the
/// users database (an application-level reference check).
#[post("/events", status = 201)]
async fn create_event(
    State(databases): State<Databases>,
    Json(body): Json<NewEvent>,
) -> Result<Json<Event>, ApiError> {
    if !databases
        .objects::<User>()?
        .filter(User::id.eq(body.user_id))
        .exists()
        .await?
    {
        return Err(ApiError::not_found("user not found"));
    }
    let event = Event::objects(databases.for_write::<Event>()?)
        .create(Event {
            id: 0,
            user_id: body.user_id,
            kind: body.kind,
        })
        .await?;
    Ok(Json(event))
}

/// A user and their events: two queries on two databases, joined in code.
#[get("/users/{id}/events")]
async fn user_events(
    State(databases): State<Databases>,
    Path(id): Path<i64>,
) -> Result<Json<UserEvents>, ApiError> {
    let user = databases.objects::<User>()?.get(User::id.eq(id)).await?;
    let events = databases
        .objects::<Event>()?
        .filter(Event::user_id.eq(id))
        .all()
        .await?;
    Ok(Json(UserEvents { user, events }))
}

/// Count events on the routed database, or on `?database=ALIAS` with
/// `using(alias)`.
#[get("/events/count")]
async fn count_events(
    State(databases): State<Databases>,
    Query(params): Query<CountParams>,
) -> Result<Json<Count>, ApiError> {
    let (alias, queryset) = match params.database {
        Some(alias) => {
            let queryset = databases.using::<Event>(&alias).map_err(alias_error)?;
            (alias, queryset)
        }
        None => (ANALYTICS.to_owned(), databases.objects::<Event>()?),
    };
    Ok(Json(Count {
        database: alias,
        events: queryset.count().await?,
    }))
}

/// Try to `UNION` a query on `default` with one on `analytics`: rejected,
/// because a queryset cannot span databases.
#[get("/cross-database-union")]
async fn cross_database_union(
    State(databases): State<Databases>,
) -> Result<Json<Vec<User>>, ApiError> {
    let local = databases.using::<User>(USERS)?;
    let remote = databases.using::<User>(ANALYTICS)?;
    match local.union(remote) {
        Ok(combined) => Ok(Json(combined.all().await?)),
        Err(QueryError::InvalidPlan(message)) => Err(ApiError::bad_request(message)),
        Err(other) => Err(OrmError::from(other).into()),
    }
}
