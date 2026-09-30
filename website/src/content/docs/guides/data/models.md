---
title: Models
description: Derive Model, table metadata, field attributes, generated field constants, and persistence.
---

Models are structs with `#[derive(Model)]`. The derive generates typed field
constants, static metadata, and persistence helpers. Queries are lazy
`QuerySet`s; see [QuerySets](/axumapi/guides/data/querysets/).

```rust
use axumapi::prelude::*;

#[derive(Model, Serialize, Deserialize, Schema, Debug, Clone)]
#[model(table = "users", ordering = ["name"])]
pub struct User {
    #[field(primary_key, auto)]
    pub id: i64,
    #[field(max_length = 100, index)]
    pub name: String,
    #[field(max_length = 255, unique)]
    pub email: String,
    pub age: Option<i32>,
    #[field(auto_now_add)]
    pub created_at: DateTime<Utc>,
    pub team: Option<ForeignKey<Team>>,
}

let user = User::objects(&db).get(User::id.eq(1)).await?;
let team = user.fetch_team(&db).await?; // Option<Arc<Team>>
```

One struct can derive `Model, Validate, Schema, Serialize, Deserialize`.
`Validate` and `Schema` ignore ORM keys; `Model` ignores validation keys
except `max_length`, `max_digits`, and `decimal_places`, which become
column metadata. Unknown keys are compile errors.

## `#[model(...)]`

| Key | Meaning |
|---|---|
| `table = "users"` | Table name. Default: `snake_case(TypeName)` (`BlogPost` → `blog_post`). No app prefix, no pluralisation |
| `ordering = ["name", "-created_at"]` | Default ordering. `-` means descending. Entries are field names; an unknown name is a compile error |
| `indexes(idx(columns = ["a", "b"], unique))` | Named multi-column indexes. Relation fields become `{field}_id` |
| `unique_together(["a", "b"], ..)` | Unique constraints, named `{table}_{columns}_uniq` |
| `checks(age_positive = "age >= 0")` | Named `CHECK` constraints. The SQL is written by the author |
| `managed = false` | Migrations skip the table |
| `many_to_many(tags(Tag, through_table = "..", source_column = "..", target_column = "..", related_name = ".."))` | Many-to-many. Defaults: `through_table = {table}_{name}`, `source_column = {snake(Model)}_id`, `target_column = {snake(Target)}_id`. Set both columns for a self-referential relation. `through = ThroughModel` uses an explicit through model and then `through_table` must be omitted |

## `#[field(...)]` ORM keys

| Key | Meaning |
|---|---|
| `primary_key` | Exactly one field |
| `auto` | Database-generated integer primary key (`i16`, `i32`, `i64`). A model is unsaved while the key equals its default. A primary key without `auto` is never unsaved |
| `unique`, `index` | Constraint and single-column index |
| `column = "name"` | Column name. Default: the field name, or `{field}_id` for relations |
| `db_default = 7 \| true \| "text"` | Server-side default |
| `auto_now_add`, `auto_now` | `DbDefault::Now` at insert, and refresh on every save |
| `on_delete = "cascade" \| "protect" \| "set_null" \| "set_default" \| "do_nothing"` | Relations only. Default: `cascade`. `set_null` requires `Option<ForeignKey<..>>` |
| `related_name = ".."` | Reverse accessor on the target |
| `skip` | Not a column. Loaded as `Default::default()` |
| `max_length`, `max_digits`, `decimal_places` | Column metadata |

`#[field(default = ..)]` is the *validation* default (a Rust expression).
Timestamps use `auto_now_add` / `auto_now`, Django’s names.

## Generated API

- `User::name` is a typed `Field<User, String>` constant per column.
  `Book::author` is a `Field<Book, ForeignKey<Author>>` over `author_id`.
- Lookups exist only on suitable types, so `User::age.icontains(..)` does
  not compile. Why: [Typed field constants](/axumapi/internals/typed-fields/).
- Foreign keys get `book.fetch_author(&db)` returning `Arc<Author>`, or
  `Option<Arc<Author>>` for a nullable key. It is not named `author`
  because an associated constant and a method cannot share a name.
- Foreign keys are indexed unless they are unique. A `OneToOne` is unique.
- `related_name` on a foreign key adds `author.books(&db)`, a
  `QuerySet<Book>`. For a `OneToOne`, it adds an async `profile(&db)`
  returning `Option<Profile>`. The target model must be in the same crate
  (Rust’s orphan rule). Across crates, filter:
  `Book::objects(&db).filter(Book::author.eq(id))`.
- Each many-to-many adds `book.tags(&db)`, a `ManyToManyManager`. With
  `related_name`, it also adds a reverse queryset (a correlated `EXISTS`
  over the join table).
- Generic structs are not supported.
- `ForeignKey<T>` implements `Validate`, `Schema`, and `Dump` by delegating
  to `T::Pk`. OpenAPI shows the key type.

## Persistence

An `auto` primary key that still holds its default (`0`) means the object
is unsaved, so `save()` `INSERT`s. For a manual primary key, `save()` tries
an `UPDATE` and falls back to an `INSERT`, as Django does.

```rust
let mut user = User { id: 0, name: "Ann".into(), /* .. */ };
user.save(&db).await?;          // INSERT, fills user.id
user.name = "Annabel".into();
user.save(&db).await?;          // UPDATE
user.delete(&db).await?;
user.refresh(&db).await?;       // reload columns
```

`QuerySet::create`, `get_or_create`, `update_or_create`, `bulk_create`, and
`bulk_update` are on the queryset. Bulk writes send no signals. See
[QuerySets](/axumapi/guides/data/querysets/) and [Signals](/axumapi/guides/data/signals/).

## Column types

`DbType` maps a Rust type to SQL: integers and floats, `bool`, `String`,
`Vec<u8>`, `Decimal`, `Uuid`, `NaiveDate`, `NaiveTime`, `DateTime<Utc>`,
`TimeDelta`, `IpAddr`, JSON, and `Option<T>`. Canonical storage forms per
backend are in [Backends](/axumapi/guides/data/backends/).

## See also

- [Relations](/axumapi/guides/data/relations/)
- [Migrations](/axumapi/guides/data/migrations/)
- [Field attributes](/axumapi/reference/field-attributes/)
