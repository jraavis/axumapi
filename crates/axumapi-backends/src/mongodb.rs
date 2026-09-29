//! MongoDB backend: compiles the supported subset of a
//! [`QueryPlan`](axumapi_orm::QueryPlan) / [`WritePlan`](axumapi_orm::WritePlan)
//! to filters, update documents and aggregation pipelines.
//!
//! * [`compile`] is a pure compiler (no server needed, unit-tested).
//! * [`MongoBackend`] executes compiled plans (MongoDB 5.0+; transactions
//!   need a replica set) and implements the same
//!   [`Backend`](axumapi_orm::Backend) / [`Executor`](axumapi_orm::Executor)
//!   traits as the SQL adapters, so it plugs into [`Db`](axumapi_orm::Db).
//! * [`value`] maps [`Value`](axumapi_orm::Value) to and from BSON.
//!
//! Anything the backend cannot express fails **before any I/O**, with a
//! capability error ([`BackendCapabilities::mongodb`](axumapi_orm::BackendCapabilities::mongodb),
//! checked through `QueryPlan::check` / `WritePlan::check`) or
//! [`QueryError::InvalidPlan`](axumapi_orm::QueryError::InvalidPlan).
//!
//! # Collections and keys
//!
//! A table is a collection of the same name. Plans carry no model metadata,
//! so the adapter needs to know which column is the primary key: it is the
//! column named `id` unless [`Keys`] says otherwise
//! (`MongoBackend::with_keys(Keys::default().with("tags", "slug"))`). That
//! column is stored as `_id` and nowhere else; filters, projections, sort
//! keys and update targets are translated, and `_id` is renamed back on
//! read. A key the plan does not supply (an insert that omits it) is
//! generated as an `i64` from a counter document in `axumapi_counters`
//! (`$inc` by the batch size, so a bulk insert gets consecutive keys in
//! input order; the counter lives outside the transaction like a sequence,
//! so a rollback leaves a gap). Explicitly supplied keys do not advance the
//! counter. A collection whose key column is not registered still works, but
//! its key column is then an ordinary field next to a generated `_id`, and
//! duplicates of it are not rejected.
//!
//! # Mapping table
//!
//! ## `QueryPlan` fields
//!
//! | Field | MongoDB |
//! |---|---|
//! | `source` (table) | collection |
//! | `source` (derived table, `from_subquery`) | inner pipeline followed by the outer stages (used by `count`, `exists`, `aggregate` on limited / distinct / grouped querysets) |
//! | `projection` | final `$project` (`_id: 0`, one computed field per output name); empty = whole documents with `_id` renamed to the key column |
//! | `joins` | capability error `Feature::Joins` (also `select_related` / `Related` columns) |
//! | `filter` | leading `$match`: query operators where a predicate has a query form, otherwise `{$expr: ..}` |
//! | `grouping` | `$group` with `_id: {k0: .., k1: ..}` (`_id: null` when only aggregates are projected) |
//! | `having` | `$match` with `$expr` after `$group` |
//! | `ordering` | `$sort` (plain columns sort directly, other expressions through a temporary `$addFields`; NULLs sort first ascending, as on SQLite) |
//! | `limit` / `offset` | `$limit` / `$skip` |
//! | `distinct: All` | `$group` on the projected fields, then `$replaceRoot`; ordering must then use projected expressions |
//! | `distinct: On` | capability error `Feature::DistinctOn` |
//! | `lock` | capability error `Feature::RowLocking` / `LockModifiers` |
//! | `compound` (set operations) | capability error `Feature::SetOperations` |
//!
//! A grouped plan needs an explicit projection, and every non-aggregate
//! expression in it, in `having` and in `ordering` must be a grouping key or
//! a projected alias (anything else is `InvalidPlan`). A single ungrouped
//! `COUNT(*)` compiles to `$count`. An ungrouped aggregate over no documents
//! still yields one row (`COUNT` is 0, the rest `NULL`), as in SQL.
//!
//! ## `WritePlan`
//!
//! | Plan | MongoDB |
//! |---|---|
//! | `Insert` | `insert_many` (ordered); the key column becomes `_id`; keys are generated when absent (see above); `returning` echoes the stored documents in input order |
//! | `Update` | `update_many`; literal assignments use `{$set: {..}}`, `F`-expression assignments use an update pipeline (`[{$set: {..}}]`, every expression sees the original document); `rows_affected` is the matched count; `returning` first pins the matching `_id`s, updates them, then reads them back by `_id`; the key column cannot be assigned |
//! | `Delete` | `delete_many`; `returning` reads the documents first |
//!
//! ## `Expr` nodes
//!
//! Predicates follow SQL three-valued logic: a row passes only when the
//! predicate is *true*. MongoDB's `$ne`, `$nin`, `$not` and `$nor` match
//! NULL and missing fields and `$expr` comparisons use BSON type order, so
//! negation is pushed down (De Morgan) with explicit non-NULL guards, and
//! `$expr` comparisons, `NOT`, `AND` and `OR` are compiled with a NULL
//! branch.
//!
//! | Node | Filter (query form) | Aggregation expression (`$expr`, projections, sorts, updates) |
//! |---|---|---|
//! | `Column` | field (`_id` for the key) | `"$field"`; boolean column as predicate: `{f: true}` |
//! | `Value` | operand | `{$literal: bson}`; `NULL` is `null` |
//! | `Binary` comparison | `{f: {$eq/$lt/$lte/$gt/$gte: v}}`; `<>` is `{$nin: [v, null]}`; `= NULL` is `{f: null}` | `$eq/$ne/$lt/$lte/$gt/$gte` guarded by `$cond` so a NULL operand gives NULL |
//! | `Binary` arithmetic | not a predicate | `$add`, `$subtract`, `$multiply`, `$divide`, `$mod` (`$divide` is always floating point, unlike SQL integer division) |
//! | `Unary` | `Not` pushes negation down; `Neg` n/a | `Not`: NULL-preserving `$not`; `Neg`: `$multiply: [x, -1]` |
//! | `And` / `Or` | `$and` / `$or`; empty `And` is `{}`, empty `Or` is `{$expr: false}` | Kleene logic with `$in: [false/true/null, [..]]` |
//! | `Lookup::IExact` | `{f: {$regex: "^escaped\\z", $options: "i"}}` | `$regexMatch` (text only, else NULL) |
//! | `Lookup::Contains` / `StartsWith` / `EndsWith` | `$regex` on the escaped needle, `$options: "i"` when case-insensitive, anchored with `^` / `\z` | `$regexMatch` |
//! | `Lookup::Regex` | `{f: {$regex: p}}` | `$regexMatch` |
//! | `Lookup::In` | `$in` (`$nin` with `null` when negated) | `$in` with a literal array |
//! | `Lookup::Range` | `{$gte, $lte}` (`$or` of `$lt` / `$gt` when negated) | `$and` of `$gte` / `$lte` |
//! | `Lookup::IsNull` | `{f: null}` / `{f: {$ne: null}}` | `$eq: [{$ifNull: [x, null]}, null]` |
//! | `Lookup::InSubquery`, `Exists`, `Subquery` | capability error `Feature::Subqueries` | same |
//! | `OuterRef` | `InvalidPlan` (only valid inside a subquery) | same |
//! | `Related` | capability error `Feature::Joins` | same |
//! | `Func::Lower` / `Upper` / `Length` / `Trim` | via `$expr` | `$toLower`, `$toUpper`, `$strLenCP`, `$trim` (NULL in, NULL out) |
//! | `Func::Coalesce` | via `$expr` | `$ifNull` |
//! | `Func::Concat` | via `$expr` | `$concat` with each argument wrapped in `$ifNull: [x, ""]` |
//! | `Func::Substr` | via `$expr` | `$substrCP` with `start - 1` (1-based), length defaults to the rest |
//! | `Func::Replace` | via `$expr` | `$replaceAll` |
//! | `Cast` | via `$expr` | `$convert` to `long` / `double` / `decimal` / `bool` / `string` / `date` (`SmallInt`, `Integer`, `BigInt`, `Real`, `Double`, `Decimal`, `Bool`, `Text`, `Timestamp`); other targets are `InvalidPlan` |
//! | `Case` | via `$expr` | `$switch` (default NULL) |
//! | `DatePart` | via `$expr` | `$year`, `$month`, `$dayOfMonth`, `$isoWeek`, quarter from `$month`, `$hour`, `$minute`, `$second`, `$dateToString` (`%Y-%m-%d`) for `Date`. Dates and times are stored as text, so a string operand is parsed with `$dateFromString` (dates) or sliced (times) |
//! | `Aggregate::Count(*)` | | `{$sum: 1}` (`$count` alone) |
//! | `Aggregate::Count(x)` | | `$sum` of 1 per non-NULL value; `DISTINCT` via `$addToSet` and `$size` |
//! | `Aggregate::Sum` | | `$sum` plus a count of numeric values so an all-NULL group is NULL, not 0 |
//! | `Aggregate::Avg` / `Min` / `Max` | | `$avg` / `$min` / `$max` |
//! | `Aggregate::StdDev` / `Variance` | | `$stdDevSamp` / `$stdDevPop`; variance is `$pow` of the deviation by 2 (statistical aggregates are supported) |
//! | `Aggregate::StringAgg` | | `$push` (or `$addToSet`) then `$reduce` with `$concat` (element order is unspecified) |
//! | `Aggregate::ArrayAgg` | | capability error `Feature::Arrays` |
//! | `Aggregate` `filter` | | the argument becomes `{$cond: [filter, x, null]}`, which every accumulator ignores |
//! | `Window` | capability error `Feature::WindowFunctions` | same |
//!
//! # Values
//!
//! | `Value` | BSON |
//! |---|---|
//! | `Null` | null |
//! | `Bool` | boolean |
//! | `Int` | int64 (int32 and int64 both decode) |
//! | `Float` | double |
//! | `Decimal` | Decimal128 (exact, sums and averages stay decimal) |
//! | `Text` | string |
//! | `Bytes` | binary, generic subtype |
//! | `Uuid` | binary subtype 4 (16 bytes) |
//! | `Timestamp` | BSON datetime, **millisecond precision**: microseconds are truncated on write, so a value read back can differ from the one written; `insert`/`update` `returning` rows reflect what was stored |
//! | `Date`, `Time` | canonical text as on SQLite (`%Y-%m-%d`, `%H:%M:%S%.6f`), which sorts and compares correctly |
//! | `Json` | objects and arrays are embedded documents / arrays (numbers become int64 or double; integers above `i64::MAX` are rejected); JSON scalars are stored as their JSON text (`5`, `"x"`) so they decode back unchanged |
//!
//! Decoding is by BSON type: documents and arrays become `Value::Json`,
//! ObjectIds become hex text, other unsupported types are decode errors.
//!
//! # Transactions
//!
//! `begin` starts a session and a transaction (`TransactionSupport::Flat`).
//! There are no savepoints, so a nested `Db::transaction` (and therefore
//! `bulk_create` on a transactional handle) is a capability error, and no
//! isolation level can be requested. Dropping a transaction aborts it.
//! MongoDB has no foreign keys: `ON DELETE` actions do nothing, and unique
//! constraints exist only where [`MongoBackend::create_unique_index`] made
//! one (plus `_id`). Duplicate-key and document-validation failures map to
//! [`BackendError::Constraint`](axumapi_orm::BackendError::Constraint).
//!
//! # Raw access
//!
//! `raw_sql`, `raw_execute` and `execute_script` are capability errors
//! (`Feature::RawSql`); [`MongoBackend::raw_command`] runs a database
//! command instead.

pub mod compile;
#[cfg(test)]
mod compile_tests;
mod exec;
pub mod value;

pub use compile::Keys;
pub use exec::MongoBackend;
