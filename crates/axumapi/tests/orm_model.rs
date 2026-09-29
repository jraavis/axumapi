//! `#[derive(Model)]` end to end: metadata, the generated `Model` impl, the
//! relation accessors, and SQLite round trips.
#![allow(clippy::unwrap_used, dead_code)]

use axumapi::orm::chrono::{NaiveDate, TimeDelta};
use axumapi::orm::rust_decimal::Decimal;
use axumapi::orm::uuid::Uuid;
use axumapi::orm::{
    ConstraintMeta, DbDefault, InsertPlan, OnDelete, OrderDirection, RelationKind, SqlType,
    WritePlan,
};
use axumapi::prelude::*;
use axumapi::validation::{ValidationContext, parse_value};
use axumapi_backends::sqlite::SqliteBackend;
use serde_json::json;
use std::net::IpAddr;

// ---------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Model)]
#[model(table = "authors", ordering = ["name"])]
struct Author {
    #[field(primary_key, auto)]
    id: i64,
    #[field(max_length = 100)]
    name: String,
}

#[derive(Debug, Clone, PartialEq, Model)]
#[model(
    table = "books",
    ordering = ["-pages", "title"],
    indexes(idx_title_author(columns = ["title", "author"], unique)),
    unique_together(["title", "pages"]),
    checks(pages_positive = "pages IS NULL OR pages > 0"),
    many_to_many(tags(Tag, through_table = "book_tags", related_name = "tagged_books")),
)]
struct Book {
    #[field(primary_key, auto)]
    id: i64,
    title: String,
    #[field(related_name = "books", on_delete = "protect")]
    author: ForeignKey<Author>,
    pages: Option<i32>,
    #[field(on_delete = "set_null", column = "editor")]
    reviewer: Option<ForeignKey<Author>>,
}

#[derive(Debug, Clone, PartialEq, Model)]
#[model(table = "profiles")]
struct Profile {
    #[field(primary_key, auto)]
    id: i64,
    #[field(related_name = "profile")]
    owner: OneToOne<Author>,
    bio: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Model)]
struct Tag {
    #[field(primary_key, auto)]
    id: i64,
    #[field(unique)]
    label: String,
}

#[derive(Debug, Clone, PartialEq, Model)]
#[model(table = "everything", managed = false)]
struct Everything {
    #[field(primary_key, auto)]
    id: i64,
    small: i16,
    medium: i32,
    big: i64,
    ratio: f64,
    flag: bool,
    text: String,
    maybe: Option<String>,
    #[field(max_digits = 10, decimal_places = 2)]
    price: Decimal,
    uid: Uuid,
    day: NaiveDate,
    #[field(auto_now_add)]
    created: DateTime<Utc>,
    #[field(auto_now)]
    updated: DateTime<Utc>,
    span: TimeDelta,
    doc: serde_json::Value,
    blob: Vec<u8>,
    ip: IpAddr,
    #[field(db_default = 7)]
    level: i32,
    #[field(db_default = "new")]
    state: String,
    #[field(db_default = true)]
    active: bool,
    #[field(skip)]
    scratch: String,
}

/// A manual (non-auto) primary key.
#[derive(Debug, Clone, PartialEq, Model)]
struct Code {
    #[field(primary_key, max_length = 8)]
    code: String,
}

// ---------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------

#[test]
fn table_names_default_to_snake_case_of_the_type() {
    assert_eq!(Tag::META.table, "tag");
    assert_eq!(Code::META.table, "code");
    assert_eq!(Author::META.table, "authors");
    assert_eq!(Author::META.name, "Author");
}

#[test]
fn author_meta_matches_the_hand_written_reference() {
    let meta = Author::META;
    assert!(meta.managed);
    assert_eq!(meta.ordering, [("name", OrderDirection::Asc)]);
    let id = meta.field("id").unwrap();
    assert!(id.primary_key && id.auto && !id.nullable);
    assert_eq!(id.sql_type, SqlType::BigInt);
    let name = meta.field("name").unwrap();
    assert_eq!((name.sql_type, name.max_length), (SqlType::Text, Some(100)));
    assert_eq!(meta.pk().unwrap().column, "id");
}

#[test]
fn relations_columns_and_constraints_are_described() {
    let meta = Book::META;
    let columns: Vec<_> = meta.fields.iter().map(|f| f.column).collect();
    assert_eq!(columns, ["id", "title", "author_id", "pages", "editor"]);

    let author = meta.field("author").unwrap();
    let relation = author.relation.unwrap();
    assert_eq!(relation.kind, RelationKind::ForeignKey);
    assert_eq!(relation.on_delete, OnDelete::Protect);
    assert_eq!(relation.related_name, Some("books"));
    assert_eq!((relation.target)().name, "Author");
    assert!(author.index && !author.unique && !author.nullable);
    assert_eq!(author.sql_type, SqlType::BigInt);

    let reviewer = meta.field("reviewer").unwrap();
    assert!(reviewer.nullable);
    assert_eq!(reviewer.column, "editor");
    assert_eq!(reviewer.relation.unwrap().on_delete, OnDelete::SetNull);

    assert!(meta.field("pages").unwrap().nullable);
    assert_eq!(
        meta.ordering,
        [
            ("pages", OrderDirection::Desc),
            ("title", OrderDirection::Asc)
        ]
    );
    assert_eq!(meta.indexes.len(), 1);
    let index = meta.indexes[0];
    assert_eq!(index.name, "idx_title_author");
    assert_eq!(
        index.columns,
        ["title", "author_id"],
        "field names map to columns"
    );
    assert!(index.unique);
    assert_eq!(
        meta.constraints,
        [
            ConstraintMeta::Unique {
                name: "books_title_pages_uniq",
                columns: &["title", "pages"],
            },
            ConstraintMeta::Check {
                name: "pages_positive",
                sql: "pages IS NULL OR pages > 0",
            },
        ]
    );
}

#[test]
fn one_to_one_is_unique_without_an_extra_index() {
    let owner = Profile::META.field("owner").unwrap();
    assert_eq!(owner.column, "owner_id");
    assert!(owner.unique && !owner.index);
    assert_eq!(owner.relation.unwrap().kind, RelationKind::OneToOne);
}

#[test]
fn many_to_many_metadata_uses_defaults_and_overrides() {
    let tags = &Book::META.many_to_many[0];
    assert_eq!(tags.name, "tags");
    assert_eq!(tags.through_table, "book_tags");
    assert_eq!(tags.source_column, "book_id");
    assert_eq!(tags.target_column, "tag_id");
    assert_eq!(tags.related_name, Some("tagged_books"));
    assert!(tags.through.is_none());
    assert_eq!((tags.target)().table, "tag");
}

#[test]
fn field_families_map_to_sql_types_and_flags() {
    let meta = Everything::META;
    assert!(!meta.managed);
    let sql = |name: &str| meta.field(name).unwrap().sql_type;
    assert_eq!(sql("small"), SqlType::SmallInt);
    assert_eq!(sql("medium"), SqlType::Integer);
    assert_eq!(sql("big"), SqlType::BigInt);
    assert_eq!(sql("ratio"), SqlType::Double);
    assert_eq!(sql("flag"), SqlType::Bool);
    assert_eq!(sql("text"), SqlType::Text);
    assert_eq!(sql("price"), SqlType::Decimal);
    assert_eq!(sql("uid"), SqlType::Uuid);
    assert_eq!(sql("day"), SqlType::Date);
    assert_eq!(sql("created"), SqlType::Timestamp);
    assert_eq!(sql("span"), SqlType::Duration);
    assert_eq!(sql("doc"), SqlType::Json);
    assert_eq!(sql("blob"), SqlType::Binary);
    assert_eq!(sql("ip"), SqlType::IpAddr);
    assert!(meta.field("maybe").unwrap().nullable);
    assert!(!meta.field("text").unwrap().nullable);
    let price = meta.field("price").unwrap();
    assert_eq!(
        (price.max_digits, price.decimal_places),
        (Some(10), Some(2))
    );
    assert_eq!(meta.field("created").unwrap().default, Some(DbDefault::Now));
    assert!(meta.field("updated").unwrap().auto_now);
    assert_eq!(
        meta.field("level").unwrap().default,
        Some(DbDefault::Int(7))
    );
    assert_eq!(
        meta.field("state").unwrap().default,
        Some(DbDefault::Text("new"))
    );
    assert_eq!(
        meta.field("active").unwrap().default,
        Some(DbDefault::Bool(true))
    );
    assert!(
        meta.field("scratch").is_none(),
        "skipped fields are not columns"
    );
}

#[test]
fn manual_primary_keys_are_never_unsaved() {
    let code = Code {
        code: String::new(),
    };
    assert!(!code.is_unsaved());
    assert_eq!(Code::META.pk().unwrap().max_length, Some(8));
    assert!(
        Author {
            id: 0,
            name: "x".into()
        }
        .is_unsaved()
    );
    assert!(
        !Author {
            id: 3,
            name: "x".into()
        }
        .is_unsaved()
    );
}

#[test]
fn field_constants_are_typed_column_handles() {
    assert_eq!(Author::name.name(), "name");
    assert_eq!(Book::author.name(), "author_id");
    assert_eq!(Book::reviewer.name(), "editor");
    let _: Field<Book, ForeignKey<Author>> = Book::author;
    let _: Field<Book, Option<ForeignKey<Author>>> = Book::reviewer;
}

// ---------------------------------------------------------------------
// SQLite round trips
// ---------------------------------------------------------------------

async fn db() -> Db {
    let db = Db::new(SqliteBackend::connect("sqlite::memory:").await.unwrap());
    db.execute_script(
        "CREATE TABLE authors (id INTEGER PRIMARY KEY AUTOINCREMENT, name VARCHAR(100) NOT NULL);
         CREATE TABLE books (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL,
             author_id INTEGER NOT NULL REFERENCES authors(id), pages INTEGER,
             editor INTEGER REFERENCES authors(id) ON DELETE SET NULL);
         CREATE TABLE profiles (id INTEGER PRIMARY KEY AUTOINCREMENT,
             owner_id INTEGER NOT NULL UNIQUE REFERENCES authors(id), bio TEXT);
         CREATE TABLE tag (id INTEGER PRIMARY KEY AUTOINCREMENT, label TEXT NOT NULL UNIQUE);
         CREATE TABLE book_tags (book_id INTEGER NOT NULL REFERENCES books(id),
             tag_id INTEGER NOT NULL REFERENCES tag(id));
         CREATE TABLE everything (id INTEGER PRIMARY KEY AUTOINCREMENT, small INTEGER NOT NULL,
             medium INTEGER NOT NULL, big INTEGER NOT NULL, ratio REAL NOT NULL,
             flag INTEGER NOT NULL, text TEXT NOT NULL, maybe TEXT, price TEXT NOT NULL,
             uid TEXT NOT NULL, day TEXT NOT NULL, created TEXT NOT NULL, updated TEXT NOT NULL,
             span INTEGER NOT NULL, doc TEXT NOT NULL, blob BLOB NOT NULL, ip TEXT NOT NULL,
             level INTEGER NOT NULL, state TEXT NOT NULL, active INTEGER NOT NULL);
         CREATE TABLE code (code VARCHAR(8) PRIMARY KEY);",
    )
    .await
    .unwrap();
    db
}

/// Minimal insert until `save()` is used: the contract, not the API.
async fn insert<M: Model>(db: &Db, mut model: M) -> Result<M, OrmError> {
    let values: Vec<_> = model
        .to_values()
        .into_iter()
        .filter(|(col, _)| M::META.column(col).is_some_and(|f| !f.auto))
        .collect();
    let plan = WritePlan::Insert(InsertPlan {
        table: M::META.table.into(),
        columns: values.iter().map(|(c, _)| (*c).into()).collect(),
        rows: vec![values.into_iter().map(|(_, v)| v).collect()],
        returning: vec![M::META.pk().unwrap().column.into()],
    });
    let result = db.execute(&plan).await?;
    let pk = result.returning[0].iter().next().unwrap().1.clone();
    model.set_pk(pk)?;
    Ok(model)
}

async fn author(db: &Db, name: &str) -> Author {
    insert(
        db,
        Author {
            id: 0,
            name: name.into(),
        },
    )
    .await
    .unwrap()
}

async fn book(db: &Db, author: &Author, title: &str, pages: Option<i32>) -> Book {
    insert(
        db,
        Book {
            id: 0,
            title: title.into(),
            author: ForeignKey::new(author.id),
            pages,
            reviewer: None,
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn default_ordering_comes_from_the_meta() {
    let db = db().await;
    author(&db, "Zed").await;
    author(&db, "Ann").await;
    let names: Vec<_> = Author::objects(&db)
        .all()
        .await
        .unwrap()
        .into_iter()
        .map(|a| a.name)
        .collect();
    assert_eq!(names, ["Ann", "Zed"]);
    let found = Author::objects(&db)
        .get(Author::name.eq("Zed"))
        .await
        .unwrap();
    assert!(found.id > 0);
}

#[tokio::test]
async fn foreign_key_accessors_forward_and_reverse() {
    let db = db().await;
    let ann = author(&db, "Ann").await;
    let bob = author(&db, "Bob").await;
    let rust = book(&db, &ann, "Rust", Some(300)).await;
    let mut sql = book(&db, &ann, "SQL", None).await;
    book(&db, &bob, "Other", Some(1)).await;

    assert_eq!(rust.fetch_author(&db).await.unwrap().name, "Ann");
    assert!(rust.fetch_reviewer(&db).await.unwrap().is_none());
    sql.reviewer = Some(ForeignKey::new(bob.id));
    assert_eq!(sql.fetch_reviewer(&db).await.unwrap().unwrap().name, "Bob");

    let titles: Vec<_> = ann
        .books(&db)
        .all()
        .await
        .unwrap()
        .into_iter()
        .map(|b| b.title)
        .collect();
    assert_eq!(
        titles,
        ["Rust", "SQL"],
        "default ordering: pages desc, then title"
    );
    assert_eq!(bob.books(&db).all().await.unwrap().len(), 1);
    let loaded = Book::objects(&db).get(Book::title.eq("SQL")).await.unwrap();
    assert_eq!(loaded.pages, None);
    assert_eq!(loaded.author.id(), &ann.id);
}

#[tokio::test]
async fn one_to_one_reverse_accessor_returns_an_option() {
    let db = db().await;
    let ann = author(&db, "Ann").await;
    let bob = author(&db, "Bob").await;
    let profile = insert(
        &db,
        Profile {
            id: 0,
            owner: OneToOne::new(ann.id),
            bio: Some("hi".into()),
        },
    )
    .await
    .unwrap();
    assert_eq!(ann.profile(&db).await.unwrap(), Some(profile));
    assert_eq!(bob.profile(&db).await.unwrap(), None);
}

#[tokio::test]
async fn many_to_many_forward_and_reverse() {
    let db = db().await;
    let ann = author(&db, "Ann").await;
    let rust = book(&db, &ann, "Rust", Some(300)).await;
    let sql = book(&db, &ann, "SQL", Some(200)).await;
    book(&db, &ann, "Untagged", Some(100)).await;
    let systems = insert(
        &db,
        Tag {
            id: 0,
            label: "systems".into(),
        },
    )
    .await
    .unwrap();
    let data = insert(
        &db,
        Tag {
            id: 0,
            label: "data".into(),
        },
    )
    .await
    .unwrap();
    db.execute_script(&format!(
        "INSERT INTO book_tags (book_id, tag_id) VALUES ({r}, {s}), ({r}, {d}), ({q}, {d});",
        r = rust.id,
        q = sql.id,
        s = systems.id,
        d = data.id,
    ))
    .await
    .unwrap();

    let manager = rust.tags(&db);
    assert_eq!(manager.meta().through_table, "book_tags");
    assert_eq!(manager.source_pk(), &axumapi::orm::Value::Int(rust.id));

    let mut titles: Vec<_> = data
        .tagged_books(&db)
        .all()
        .await
        .unwrap()
        .into_iter()
        .map(|b| b.title)
        .collect();
    titles.sort();
    assert_eq!(titles, ["Rust", "SQL"]);
    let systems_books = systems.tagged_books(&db).all().await.unwrap();
    assert_eq!(systems_books, [rust]);
}

#[tokio::test]
async fn every_field_family_round_trips_through_sqlite() {
    let db = db().await;
    let now = Utc::now();
    let original = Everything {
        id: 0,
        small: -3,
        medium: 70_000,
        big: 5_000_000_000,
        ratio: 2.5,
        flag: true,
        text: "hello".into(),
        maybe: None,
        price: Decimal::new(1250, 2),
        uid: Uuid::from_u128(0x1234_5678_9abc_def0_1234_5678_9abc_def0),
        day: NaiveDate::from_ymd_opt(2024, 2, 29).unwrap(),
        created: now,
        updated: now,
        span: TimeDelta::microseconds(1_500_000),
        doc: json!({"a": [1, 2, {"b": null}]}),
        blob: vec![0, 1, 255],
        ip: "::1".parse().unwrap(),
        level: 7,
        state: "new".into(),
        active: true,
        scratch: "not stored".into(),
    };
    let saved = insert(&db, original.clone()).await.unwrap();
    assert!(saved.id > 0);
    let loaded = Everything::objects(&db)
        .get(Everything::id.eq(saved.id))
        .await
        .unwrap();
    let expected = Everything {
        id: saved.id,
        scratch: String::new(),
        ..original
    };
    assert_eq!(
        loaded.created.timestamp_micros(),
        expected.created.timestamp_micros()
    );
    assert_eq!(
        Everything {
            created: expected.created,
            updated: expected.updated,
            ..loaded
        },
        Everything {
            created: expected.created,
            updated: expected.updated,
            ..expected
        }
    );
}

#[tokio::test]
async fn manual_primary_keys_round_trip() {
    let db = db().await;
    let code = Code {
        code: "ab-12".into(),
    };
    let plan = WritePlan::Insert(InsertPlan {
        table: Code::META.table.into(),
        columns: vec!["code".into()],
        rows: vec![vec![code.pk().into()]],
        returning: Vec::new(),
    });
    db.execute(&plan).await.unwrap();
    assert_eq!(Code::objects(&db).all().await.unwrap(), [code]);
}

// ---------------------------------------------------------------------
// One struct, all derives
// ---------------------------------------------------------------------

#[derive(Debug, PartialEq, Model, Validate, Schema, Serialize, Deserialize)]
#[model(table = "accounts", ordering = ["-age"])]
struct Account {
    #[field(primary_key, auto)]
    #[serde(default)]
    id: i64,
    #[field(min_length = 3, max_length = 20, unique, description = "Login name")]
    username: String,
    #[field(ge = 0, le = 150, db_default = 18, index)]
    #[serde(default)]
    age: i32,
    #[field(skip)]
    #[serde(skip)]
    scratch: String,
}

#[test]
fn model_composes_with_validate_schema_and_serde() {
    let meta = Account::META;
    let username = meta.field("username").unwrap();
    assert_eq!((username.max_length, username.unique), (Some(20), true));
    let age = meta.field("age").unwrap();
    assert_eq!((age.index, age.default), (true, Some(DbDefault::Int(18))));

    let ok: Account = parse_value(
        json!({"username": "alice", "age": 30}),
        ValidationContext::new(),
    )
    .unwrap();
    assert_eq!(ok.username, "alice");
    assert!(ok.is_unsaved());
    let short = parse_value::<Account>(json!({"username": "al"}), ValidationContext::new());
    assert!(short.is_err(), "validation keys still apply");
    let old = parse_value::<Account>(
        json!({"username": "alice", "age": 200}),
        ValidationContext::new(),
    );
    assert!(old.is_err());

    let value = serde_json::to_value(&ok).unwrap();
    assert_eq!(value, json!({"id": 0, "username": "alice", "age": 30}));
    let schema = <Account as Schema>::schema(&mut SchemaRegistry::new());
    assert_eq!(
        schema.get("properties").unwrap()["username"]["maxLength"],
        json!(20)
    );
}
