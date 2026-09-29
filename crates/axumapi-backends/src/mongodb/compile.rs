//! Pure compiler: plans to MongoDB filters, update documents and pipelines.
//!
//! Nothing here touches a server; every function returns an error for a plan
//! the backend cannot express, so unsupported plans fail before any I/O.

use super::value::to_bson;
use ::mongodb::bson::{Bson, Document, doc};
use axumapi_orm::expr::{
    AggFunc, Aggregate, BinaryOp, Column, DatePart, Function, Lookup, UnaryOp,
};
use axumapi_orm::{
    BackendCapabilities, BackendCapabilityError, BackendKind, DeletePlan, DistinctMode, Expr,
    Feature, InsertPlan, OrderDirection, OrmError, QueryError, QueryPlan, SelectExpr, SqlType,
    UpdatePlan, Value, WritePlan,
};
use std::collections::HashMap;

/// Which column of a collection is stored as `_id`.
///
/// Plans carry no model metadata, so the adapter is told: the default is a
/// column named `id`, overridable per collection.
#[derive(Debug, Clone)]
pub struct Keys {
    default: String,
    per_collection: HashMap<String, String>,
}

impl Default for Keys {
    fn default() -> Self {
        Self {
            default: "id".into(),
            per_collection: HashMap::new(),
        }
    }
}

impl Keys {
    /// Use `column` as the primary key of `collection`.
    #[must_use]
    pub fn with(mut self, collection: impl Into<String>, column: impl Into<String>) -> Self {
        self.per_collection.insert(collection.into(), column.into());
        self
    }

    /// Use `column` for every collection without an explicit entry.
    #[must_use]
    pub fn with_default(mut self, column: impl Into<String>) -> Self {
        self.default = column.into();
        self
    }

    /// Primary-key column of `collection`.
    pub fn pk(&self, collection: &str) -> &str {
        self.per_collection
            .get(collection)
            .map_or(self.default.as_str(), String::as_str)
    }
}

/// A compiled read: run `pipeline` on `collection`.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledQuery {
    /// Collection the pipeline runs on.
    pub collection: String,
    /// Aggregation pipeline.
    pub pipeline: Vec<Document>,
    /// Output column names, in order. Empty means "every field of the
    /// document" (`_id` is renamed to [`pk`](Self::pk)).
    pub columns: Vec<String>,
    /// Primary-key column of the root collection.
    pub pk: String,
    /// The single row an ungrouped aggregate produces over no input (SQL
    /// returns one row, `$group` returns none).
    pub empty_row: Option<Vec<Value>>,
}

/// A compiled insert.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledInsert {
    /// Target collection.
    pub collection: String,
    /// Primary-key column, stored as `_id`.
    pub pk: String,
    /// Documents in input order. `_id` is present only when the plan
    /// supplied the key column; otherwise the executor assigns keys.
    pub docs: Vec<Document>,
    /// Columns to return.
    pub returning: Vec<String>,
}

/// How an update changes documents.
#[derive(Debug, Clone, PartialEq)]
pub enum UpdateSpec {
    /// `{"$set": {..}}` with literal values.
    Set(Document),
    /// An update pipeline (`$set` stage) for F-expression assignments.
    Pipeline(Vec<Document>),
}

/// A compiled update.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledUpdate {
    /// Target collection.
    pub collection: String,
    /// Primary-key column, stored as `_id`.
    pub pk: String,
    /// Filter for `update_many`.
    pub filter: Document,
    /// Update to apply.
    pub update: UpdateSpec,
    /// Columns to return.
    pub returning: Vec<String>,
}

/// A compiled delete.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledDelete {
    /// Target collection.
    pub collection: String,
    /// Primary-key column, stored as `_id`.
    pub pk: String,
    /// Filter for `delete_many`.
    pub filter: Document,
    /// Columns to return.
    pub returning: Vec<String>,
}

/// A compiled write.
#[derive(Debug, Clone, PartialEq)]
pub enum CompiledWrite {
    /// Insert.
    Insert(CompiledInsert),
    /// Update.
    Update(CompiledUpdate),
    /// Delete.
    Delete(CompiledDelete),
}

fn invalid(message: impl Into<String>) -> OrmError {
    QueryError::InvalidPlan(message.into()).into()
}

fn unsupported(feature: Feature) -> OrmError {
    BackendCapabilityError::from_feature(BackendKind::MongoDb, feature).into()
}

type Res<T> = Result<T, OrmError>;

/// Compile a read plan.
///
/// # Errors
/// A capability error for features MongoDB lacks, or
/// [`QueryError::InvalidPlan`] for plans it cannot express.
pub fn compile_query(plan: &QueryPlan, keys: &Keys) -> Res<CompiledQuery> {
    plan.check(&BackendCapabilities::mongodb())?;
    let built = build(plan, keys)?;
    Ok(CompiledQuery {
        collection: built.collection,
        pipeline: built.pipeline,
        columns: built.columns,
        pk: built.pk,
        empty_row: built.empty_row,
    })
}

/// Compile a write plan.
///
/// # Errors
/// As [`compile_query`].
pub fn compile_write(plan: &WritePlan, keys: &Keys) -> Res<CompiledWrite> {
    plan.check(&BackendCapabilities::mongodb())?;
    Ok(match plan {
        WritePlan::Insert(p) => CompiledWrite::Insert(compile_insert(p, keys)?),
        WritePlan::Update(p) => CompiledWrite::Update(compile_update(p, keys)?),
        WritePlan::Delete(p) => CompiledWrite::Delete(compile_delete(p, keys)?),
    })
}

fn returning_columns(columns: &[axumapi_orm::expr::Ident]) -> Vec<String> {
    columns.iter().map(ToString::to_string).collect()
}

fn compile_insert(plan: &InsertPlan, keys: &Keys) -> Res<CompiledInsert> {
    let pk = keys.pk(&plan.table).to_owned();
    check_name(&plan.table)?;
    let mut docs = Vec::with_capacity(plan.rows.len());
    for row in &plan.rows {
        if row.len() != plan.columns.len() {
            return Err(invalid(format!(
                "insert row has {} values for {} columns",
                row.len(),
                plan.columns.len()
            )));
        }
        let mut doc = Document::new();
        for (column, value) in plan.columns.iter().zip(row) {
            let name = if *column == pk {
                "_id".to_owned()
            } else {
                check_name(column)?;
                column.to_string()
            };
            let value = to_bson(value)?;
            if name == "_id" && value == Bson::Null {
                return Err(invalid("the primary key cannot be NULL"));
            }
            doc.insert(name, value);
        }
        docs.push(doc);
    }
    Ok(CompiledInsert {
        collection: plan.table.to_string(),
        pk,
        docs,
        returning: returning_columns(&plan.returning),
    })
}

fn compile_update(plan: &UpdatePlan, keys: &Keys) -> Res<CompiledUpdate> {
    check_name(&plan.table)?;
    let pk = keys.pk(&plan.table).to_owned();
    let ctx = Ctx {
        pk: Some(&pk),
        root: plan.table.as_ref(),
    };
    if plan.assignments.is_empty() {
        return Err(invalid("an update needs at least one assignment"));
    }
    let filter = match &plan.filter {
        Some(f) => filter_doc(f, false, &ctx)?,
        None => Document::new(),
    };
    let literal_only = plan
        .assignments
        .iter()
        .all(|(_, e)| matches!(e, Expr::Value(_)));
    let mut set = Document::new();
    for (column, expr) in &plan.assignments {
        if *column == pk {
            return Err(invalid("the primary key cannot be updated"));
        }
        check_name(column)?;
        let value = if literal_only {
            match expr {
                Expr::Value(v) => to_bson(v)?,
                _ => Bson::Null,
            }
        } else {
            expr_bson(expr, &Scope::row(&ctx))?
        };
        set.insert(column.to_string(), value);
    }
    let update = if literal_only {
        UpdateSpec::Set(doc! { "$set": set })
    } else {
        UpdateSpec::Pipeline(vec![doc! { "$set": set }])
    };
    Ok(CompiledUpdate {
        collection: plan.table.to_string(),
        pk,
        filter,
        update,
        returning: returning_columns(&plan.returning),
    })
}

fn compile_delete(plan: &DeletePlan, keys: &Keys) -> Res<CompiledDelete> {
    check_name(&plan.table)?;
    let pk = keys.pk(&plan.table).to_owned();
    let ctx = Ctx {
        pk: Some(&pk),
        root: plan.table.as_ref(),
    };
    let filter = match &plan.filter {
        Some(f) => filter_doc(f, false, &ctx)?,
        None => Document::new(),
    };
    Ok(CompiledDelete {
        collection: plan.table.to_string(),
        pk,
        filter,
        returning: returning_columns(&plan.returning),
    })
}

// ---------------------------------------------------------------------
// Names and contexts
// ---------------------------------------------------------------------

fn check_name(name: &str) -> Res<()> {
    if name.is_empty() || name.starts_with('$') || name.contains('.') || name.contains('\0') {
        return Err(invalid(format!(
            "`{name}` is not a valid MongoDB field or collection name for this backend"
        )));
    }
    Ok(())
}

/// Naming context of one pipeline level.
struct Ctx<'a> {
    /// Column stored as `_id`; `None` for the output of a derived table.
    pk: Option<&'a str>,
    /// Name the plan uses for its source (table or alias).
    root: &'a str,
}

impl Ctx<'_> {
    fn field(&self, column: &Column) -> Res<String> {
        if let Some(source) = &column.source
            && source.as_ref() != self.root
        {
            return Err(invalid(format!(
                "column `{source}.{}` refers to a source other than `{}`",
                column.name, self.root
            )));
        }
        if self.pk == Some(column.name.as_ref()) {
            return Ok("_id".into());
        }
        check_name(&column.name)?;
        Ok(column.name.to_string())
    }
}

/// Post-`$group` naming: group keys and aggregates replace source columns.
struct GroupScope {
    keys: Vec<Expr>,
    aggs: Vec<Aggregate>,
    aliases: Vec<(String, Expr)>,
}

struct Scope<'a> {
    ctx: &'a Ctx<'a>,
    group: Option<&'a GroupScope>,
}

impl<'a> Scope<'a> {
    fn row(ctx: &'a Ctx<'a>) -> Self {
        Self { ctx, group: None }
    }
}

// ---------------------------------------------------------------------
// Filters (query language, `$expr` where an operator has no query form)
// ---------------------------------------------------------------------

fn false_doc() -> Document {
    doc! { "$expr": false }
}

/// SQL three-valued logic: a row passes only when the predicate is *true*.
/// `neg` compiles `NOT expr`, keeping rows with a NULL operand excluded
/// (MongoDB's `$ne`, `$nin`, `$not` and `$nor` would include them).
fn filter_doc(expr: &Expr, neg: bool, ctx: &Ctx<'_>) -> Res<Document> {
    if let Some(doc) = query_form(expr, neg, ctx)? {
        return Ok(doc);
    }
    let e = if neg {
        Expr::Unary {
            op: UnaryOp::Not,
            expr: Box::new(expr.clone()),
        }
    } else {
        expr.clone()
    };
    Ok(doc! { "$expr": expr_bson(&e, &Scope::row(ctx))? })
}

fn children_filters(items: &[Expr], neg: bool, ctx: &Ctx<'_>) -> Res<Vec<Bson>> {
    items
        .iter()
        .map(|e| filter_doc(e, neg, ctx).map(Bson::Document))
        .collect()
}

fn column_field(expr: &Expr, ctx: &Ctx<'_>) -> Res<Option<String>> {
    match expr {
        Expr::Column(c) => ctx.field(c).map(Some),
        _ => Ok(None),
    }
}

fn flip(op: BinaryOp) -> BinaryOp {
    match op {
        BinaryOp::Lt => BinaryOp::Gt,
        BinaryOp::Le => BinaryOp::Ge,
        BinaryOp::Gt => BinaryOp::Lt,
        BinaryOp::Ge => BinaryOp::Le,
        other => other,
    }
}

fn negate_cmp(op: BinaryOp) -> BinaryOp {
    match op {
        BinaryOp::Lt => BinaryOp::Ge,
        BinaryOp::Le => BinaryOp::Gt,
        BinaryOp::Gt => BinaryOp::Le,
        BinaryOp::Ge => BinaryOp::Lt,
        BinaryOp::Eq => BinaryOp::Ne,
        BinaryOp::Ne => BinaryOp::Eq,
        other => other,
    }
}

fn cmp_key(op: BinaryOp) -> Option<&'static str> {
    Some(match op {
        BinaryOp::Lt => "$lt",
        BinaryOp::Le => "$lte",
        BinaryOp::Gt => "$gt",
        BinaryOp::Ge => "$gte",
        _ => return None,
    })
}

fn is_null_doc(field: &str, is_null: bool) -> Document {
    if is_null {
        doc! { field: Bson::Null }
    } else {
        doc! { field: { "$ne": Bson::Null } }
    }
}

/// `Some(document)` when `expr` has a query-language form.
#[allow(clippy::too_many_lines)]
fn query_form(expr: &Expr, neg: bool, ctx: &Ctx<'_>) -> Res<Option<Document>> {
    Ok(Some(match expr {
        Expr::And(items) if items.is_empty() => {
            if neg {
                false_doc()
            } else {
                Document::new()
            }
        }
        Expr::Or(items) if items.is_empty() => {
            if neg {
                Document::new()
            } else {
                false_doc()
            }
        }
        Expr::And(items) => {
            let children = children_filters(items, neg, ctx)?;
            if neg {
                doc! { "$or": children }
            } else {
                doc! { "$and": children }
            }
        }
        Expr::Or(items) => {
            let children = children_filters(items, neg, ctx)?;
            if neg {
                doc! { "$and": children }
            } else {
                doc! { "$or": children }
            }
        }
        Expr::Unary {
            op: UnaryOp::Not,
            expr,
        } => filter_doc(expr, !neg, ctx)?,
        Expr::Value(Value::Bool(b)) => {
            if *b != neg {
                Document::new()
            } else {
                false_doc()
            }
        }
        Expr::Value(Value::Null) => false_doc(),
        Expr::Column(c) => {
            let field = ctx.field(c)?;
            doc! { field: !neg }
        }
        Expr::Binary { op, lhs, rhs } => {
            let (field, op, value) = match (&**lhs, &**rhs) {
                (Expr::Column(c), Expr::Value(v)) => (ctx.field(c)?, *op, v),
                (Expr::Value(v), Expr::Column(c)) => (ctx.field(c)?, flip(*op), v),
                _ => return Ok(None),
            };
            let op = if neg { negate_cmp(op) } else { op };
            match (op, value) {
                (BinaryOp::Eq, Value::Null) => is_null_doc(&field, true),
                (BinaryOp::Ne, Value::Null) => is_null_doc(&field, false),
                // Comparing with NULL is never true, negated or not.
                (
                    BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod,
                    _,
                ) => {
                    return Ok(None);
                }
                (_, Value::Null) => false_doc(),
                (BinaryOp::Eq, v) => doc! { field: { "$eq": to_bson(v)? } },
                (BinaryOp::Ne, v) => {
                    // x <> v excludes NULL and missing values, like SQL.
                    doc! { field: { "$nin": [to_bson(v)?, Bson::Null] } }
                }
                (op, v) => match cmp_key(op) {
                    Some(key) => doc! { field: { key: to_bson(v)? } },
                    None => return Ok(None),
                },
            }
        }
        Expr::Lookup {
            expr: target,
            lookup,
        } => {
            let Some(field) = column_field(target, ctx)? else {
                return Ok(None);
            };
            match lookup_doc(&field, lookup, neg)? {
                Some(doc) => doc,
                None => return Ok(None),
            }
        }
        _ => return Ok(None),
    }))
}

fn escape_regex(text: &str) -> Res<String> {
    if text.contains('\0') {
        return Err(invalid("a pattern cannot contain a NUL character"));
    }
    let mut out = String::with_capacity(text.len() + 4);
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || !c.is_ascii() {
            out.push(c);
        } else {
            out.push('\\');
            out.push(c);
        }
    }
    Ok(out)
}

fn check_pattern(pattern: &str) -> Res<()> {
    if pattern.contains('\0') {
        return Err(invalid("a pattern cannot contain a NUL character"));
    }
    Ok(())
}

fn regex_bson(pattern: &str, insensitive: bool) -> Bson {
    Bson::RegularExpression(::mongodb::bson::Regex {
        pattern: pattern.into(),
        options: if insensitive {
            "i".into()
        } else {
            String::new()
        },
    })
}

fn regex_doc(field: &str, pattern: &str, insensitive: bool, neg: bool) -> Document {
    if neg {
        // `$not` alone would also match NULL and missing values.
        doc! { field: { "$ne": Bson::Null, "$not": regex_bson(pattern, insensitive) } }
    } else if insensitive {
        doc! { field: { "$regex": pattern, "$options": "i" } }
    } else {
        doc! { field: { "$regex": pattern } }
    }
}

fn lookup_doc(field: &str, lookup: &Lookup, neg: bool) -> Res<Option<Document>> {
    Ok(Some(match lookup {
        Lookup::IExact(Value::Text(text)) => {
            regex_doc(field, &format!("^{}\\z", escape_regex(text)?), true, neg)
        }
        Lookup::IExact(Value::Null) | Lookup::IsNull(true) => is_null_doc(field, !neg),
        Lookup::IExact(_) => return Ok(None),
        Lookup::Contains {
            needle,
            case_insensitive,
        } => regex_doc(field, &escape_regex(needle)?, *case_insensitive, neg),
        Lookup::StartsWith {
            needle,
            case_insensitive,
        } => regex_doc(
            field,
            &format!("^{}", escape_regex(needle)?),
            *case_insensitive,
            neg,
        ),
        Lookup::EndsWith {
            needle,
            case_insensitive,
        } => regex_doc(
            field,
            &format!("{}\\z", escape_regex(needle)?),
            *case_insensitive,
            neg,
        ),
        Lookup::Regex(pattern) => {
            check_pattern(pattern)?;
            regex_doc(field, pattern, false, neg)
        }
        Lookup::IsNull(false) => is_null_doc(field, neg),
        Lookup::In(values) => {
            let mut list: Vec<Bson> = values
                .iter()
                .filter(|v| !v.is_null())
                .map(to_bson)
                .collect::<Result<_, _>>()?;
            if neg {
                list.push(Bson::Null);
                doc! { field: { "$nin": list } }
            } else if list.is_empty() {
                false_doc()
            } else {
                doc! { field: { "$in": list } }
            }
        }
        Lookup::Range(lo, hi) => {
            if lo.is_null() || hi.is_null() {
                false_doc()
            } else if neg {
                doc! { "$or": [
                    { field: { "$lt": to_bson(lo)? } },
                    { field: { "$gt": to_bson(hi)? } },
                ] }
            } else {
                doc! { field: { "$gte": to_bson(lo)?, "$lte": to_bson(hi)? } }
            }
        }
        Lookup::InSubquery(_) => return Err(unsupported(Feature::Subqueries)),
    }))
}

// ---------------------------------------------------------------------
// Aggregation expressions
// ---------------------------------------------------------------------

fn isnull(x: Bson) -> Bson {
    Bson::Document(doc! { "$eq": [ { "$ifNull": [x, Bson::Null] }, Bson::Null ] })
}

fn literal(x: Bson) -> Bson {
    Bson::Document(doc! { "$literal": x })
}

/// `x` unless it is NULL, then NULL (`inner` computes the non-NULL case).
fn null_guard(x: &Bson, inner: Bson) -> Bson {
    Bson::Document(doc! { "$cond": [isnull(x.clone()), Bson::Null, inner] })
}

fn let_in(var: &str, value: Bson, body: Bson) -> Bson {
    Bson::Document(doc! { "$let": { "vars": { var: value }, "in": body } })
}

fn is_nonnull_literal(expr: &Expr) -> bool {
    matches!(expr, Expr::Value(v) if !v.is_null())
}

fn expr_bson(expr: &Expr, scope: &Scope<'_>) -> Res<Bson> {
    if let Some(group) = scope.group {
        if let Some(i) = group.keys.iter().position(|k| k == expr) {
            return Ok(Bson::String(format!("$_id.k{i}")));
        }
        if let Expr::Aggregate(agg) = expr {
            let i = group
                .aggs
                .iter()
                .position(|a| a == agg)
                .ok_or_else(|| invalid("aggregate was not collected"))?;
            return Ok(aggregate_plan(i, agg, scope.ctx)?.finish);
        }
        if let Expr::Column(c) = expr {
            if c.source.is_none()
                && let Some((_, e)) = group
                    .aliases
                    .iter()
                    .find(|(a, e)| *a == c.name.as_ref() && e != expr)
            {
                return expr_bson(e, scope);
            }
            return Err(invalid(format!(
                "column `{}` must appear in the grouping or inside an aggregate",
                c.name
            )));
        }
    }
    match expr {
        Expr::Column(c) => Ok(Bson::String(format!("${}", scope.ctx.field(c)?))),
        Expr::Value(Value::Null) => Ok(Bson::Null),
        Expr::Value(v) => Ok(literal(to_bson(v)?)),
        Expr::Binary { op, lhs, rhs } => binary(*op, lhs, rhs, scope),
        Expr::Unary { op, expr } => {
            let x = expr_bson(expr, scope)?;
            Ok(match op {
                UnaryOp::Neg => Bson::Document(doc! { "$multiply": [x, -1_i64] }),
                UnaryOp::Not => let_in(
                    "v",
                    x,
                    Bson::Document(doc! { "$cond": [
                        isnull(Bson::String("$$v".into())),
                        Bson::Null,
                        { "$not": ["$$v"] },
                    ] }),
                ),
            })
        }
        Expr::And(items) => kleene(items, scope, false),
        Expr::Or(items) => kleene(items, scope, true),
        Expr::Lookup { expr, lookup } => lookup_bson(expr, lookup, scope),
        Expr::Func { func, args } => function(*func, args, scope),
        Expr::Cast { expr, ty } => cast(expr, *ty, scope),
        Expr::Case {
            branches,
            otherwise,
        } => {
            let mut cases = Vec::with_capacity(branches.len());
            for (when, then) in branches {
                cases.push(Bson::Document(doc! {
                    "case": expr_bson(when, scope)?,
                    "then": expr_bson(then, scope)?,
                }));
            }
            let default = match otherwise {
                Some(e) => expr_bson(e, scope)?,
                None => Bson::Null,
            };
            Ok(Bson::Document(
                doc! { "$switch": { "branches": cases, "default": default } },
            ))
        }
        Expr::DatePart { part, expr } => date_part(*part, &expr_bson(expr, scope)?),
        Expr::Aggregate(_) => Err(invalid(
            "aggregates are only allowed in the projection, HAVING or ordering of a grouped query",
        )),
        Expr::Window(_) => Err(unsupported(Feature::WindowFunctions)),
        Expr::Exists(_) | Expr::Subquery(_) => Err(unsupported(Feature::Subqueries)),
        Expr::Related(_) => Err(unsupported(Feature::Joins)),
        Expr::OuterRef(_) => Err(invalid("OuterRef is only valid inside a subquery")),
    }
}

fn binary(op: BinaryOp, lhs: &Expr, rhs: &Expr, scope: &Scope<'_>) -> Res<Bson> {
    let a = expr_bson(lhs, scope)?;
    let b = expr_bson(rhs, scope)?;
    let arithmetic = |key: &str| Bson::Document(doc! { key: [a.clone(), b.clone()] });
    let key = match op {
        BinaryOp::Add => return Ok(arithmetic("$add")),
        BinaryOp::Sub => return Ok(arithmetic("$subtract")),
        BinaryOp::Mul => return Ok(arithmetic("$multiply")),
        BinaryOp::Div => return Ok(arithmetic("$divide")),
        BinaryOp::Mod => return Ok(arithmetic("$mod")),
        BinaryOp::Eq => "$eq",
        BinaryOp::Ne => "$ne",
        BinaryOp::Lt => "$lt",
        BinaryOp::Le => "$lte",
        BinaryOp::Gt => "$gt",
        BinaryOp::Ge => "$gte",
    };
    // Comparison with the NULL literal follows the plan docs: `= NULL` is
    // `IS NULL`.
    let lnull = matches!(lhs, Expr::Value(Value::Null));
    let rnull = matches!(rhs, Expr::Value(Value::Null));
    if lnull || rnull {
        let other = if lnull { b } else { a };
        return Ok(match op {
            BinaryOp::Eq => isnull(other),
            BinaryOp::Ne => Bson::Document(doc! { "$not": [isnull(other)] }),
            _ => Bson::Null,
        });
    }
    let mut guards = Vec::new();
    if !is_nonnull_literal(lhs) {
        guards.push(isnull(a.clone()));
    }
    if !is_nonnull_literal(rhs) {
        guards.push(isnull(b.clone()));
    }
    let cmp = Bson::Document(doc! { key: [a, b] });
    Ok(match guards.len() {
        0 => cmp,
        1 => Bson::Document(doc! { "$cond": [guards.remove(0), Bson::Null, cmp] }),
        _ => Bson::Document(doc! { "$cond": [ { "$or": guards }, Bson::Null, cmp ] }),
    })
}

/// Three-valued AND / OR: NULL when undecided.
fn kleene(items: &[Expr], scope: &Scope<'_>, or: bool) -> Res<Bson> {
    let mut parts = Vec::with_capacity(items.len());
    for item in items {
        parts.push(expr_bson(item, scope)?);
    }
    match parts.len() {
        0 => return Ok(Bson::Boolean(!or)),
        1 => return Ok(parts.remove(0)),
        _ => {}
    }
    let (decisive, otherwise) = (or, !or);
    let v = || Bson::String("$$v".into());
    Ok(let_in(
        "v",
        Bson::Array(parts),
        Bson::Document(doc! { "$cond": [
            { "$in": [decisive, v()] },
            decisive,
            { "$cond": [ { "$in": [Bson::Null, v()] }, Bson::Null, otherwise ] },
        ] }),
    ))
}

fn text_pattern(lookup: &Lookup) -> Res<Option<(String, bool)>> {
    Ok(match lookup {
        Lookup::IExact(Value::Text(t)) => Some((format!("^{}\\z", escape_regex(t)?), true)),
        Lookup::Contains {
            needle,
            case_insensitive,
        } => Some((escape_regex(needle)?, *case_insensitive)),
        Lookup::StartsWith {
            needle,
            case_insensitive,
        } => Some((format!("^{}", escape_regex(needle)?), *case_insensitive)),
        Lookup::EndsWith {
            needle,
            case_insensitive,
        } => Some((format!("{}\\z", escape_regex(needle)?), *case_insensitive)),
        Lookup::Regex(p) => {
            check_pattern(p)?;
            Some((p.clone(), false))
        }
        _ => None,
    })
}

fn lookup_bson(target: &Expr, lookup: &Lookup, scope: &Scope<'_>) -> Res<Bson> {
    let x = expr_bson(target, scope)?;
    if let Some((pattern, insensitive)) = text_pattern(lookup)? {
        let mut spec = doc! { "input": x.clone(), "regex": pattern };
        if insensitive {
            spec.insert("options", "i");
        }
        return Ok(Bson::Document(doc! { "$cond": [
            { "$eq": [ { "$type": x }, "string" ] },
            { "$regexMatch": spec },
            Bson::Null,
        ] }));
    }
    Ok(match lookup {
        Lookup::IsNull(yes) => {
            if *yes {
                isnull(x)
            } else {
                Bson::Document(doc! { "$not": [isnull(x)] })
            }
        }
        Lookup::IExact(Value::Null) => isnull(x),
        Lookup::IExact(v) => {
            let inner = Bson::Document(doc! { "$eq": [x.clone(), literal(to_bson(v)?)] });
            null_guard(&x, inner)
        }
        Lookup::In(values) => {
            let list: Vec<Bson> = values
                .iter()
                .filter(|v| !v.is_null())
                .map(to_bson)
                .collect::<Result<_, _>>()?;
            if list.is_empty() {
                Bson::Boolean(false)
            } else {
                let inner = Bson::Document(doc! { "$in": [x.clone(), literal(Bson::Array(list))] });
                null_guard(&x, inner)
            }
        }
        Lookup::Range(lo, hi) => {
            if lo.is_null() || hi.is_null() {
                Bson::Boolean(false)
            } else {
                let inner = Bson::Document(doc! { "$and": [
                    { "$gte": [x.clone(), literal(to_bson(lo)?)] },
                    { "$lte": [x.clone(), literal(to_bson(hi)?)] },
                ] });
                null_guard(&x, inner)
            }
        }
        Lookup::InSubquery(_) => return Err(unsupported(Feature::Subqueries)),
        // Text lookups returned above.
        _ => return Err(invalid("unsupported lookup")),
    })
}

fn arity(func: Function, args: &[Expr], wanted: std::ops::RangeInclusive<usize>) -> Res<()> {
    if wanted.contains(&args.len()) {
        Ok(())
    } else {
        Err(invalid(format!(
            "{func:?} takes {}..={} arguments, got {}",
            wanted.start(),
            wanted.end(),
            args.len()
        )))
    }
}

fn function(func: Function, args: &[Expr], scope: &Scope<'_>) -> Res<Bson> {
    let mut compiled = Vec::with_capacity(args.len());
    for arg in args {
        compiled.push(expr_bson(arg, scope)?);
    }
    Ok(match func {
        Function::Lower | Function::Upper | Function::Length | Function::Trim => {
            arity(func, args, 1..=1)?;
            let x = compiled.remove(0);
            let inner = match func {
                Function::Lower => doc! { "$toLower": x.clone() },
                Function::Upper => doc! { "$toUpper": x.clone() },
                Function::Length => doc! { "$strLenCP": x.clone() },
                _ => doc! { "$trim": { "input": x.clone(), "chars": " " } },
            };
            null_guard(&x, Bson::Document(inner))
        }
        Function::Coalesce => {
            arity(func, args, 1..=usize::MAX)?;
            if compiled.len() == 1 {
                compiled.remove(0)
            } else {
                Bson::Document(doc! { "$ifNull": compiled })
            }
        }
        Function::Concat => {
            let parts: Vec<Bson> = compiled
                .into_iter()
                .map(|c| Bson::Document(doc! { "$ifNull": [c, ""] }))
                .collect();
            if parts.is_empty() {
                Bson::String(String::new())
            } else {
                Bson::Document(doc! { "$concat": parts })
            }
        }
        Function::Substr => {
            arity(func, args, 2..=3)?;
            let x = compiled.remove(0);
            let start = Bson::Document(doc! { "$subtract": [compiled.remove(0), 1_i64] });
            let len = if compiled.is_empty() {
                Bson::Int64(i64::from(i32::MAX))
            } else {
                compiled.remove(0)
            };
            let inner = Bson::Document(doc! { "$substrCP": [x.clone(), start, len] });
            null_guard(&x, inner)
        }
        Function::Replace => {
            arity(func, args, 3..=3)?;
            let x = compiled.remove(0);
            let from = compiled.remove(0);
            let to = compiled.remove(0);
            Bson::Document(doc! { "$replaceAll": { "input": x, "find": from, "replacement": to } })
        }
    })
}

fn cast(expr: &Expr, ty: SqlType, scope: &Scope<'_>) -> Res<Bson> {
    let input = expr_bson(expr, scope)?;
    let to = match ty {
        SqlType::SmallInt | SqlType::Integer | SqlType::BigInt => "long",
        SqlType::Real | SqlType::Double => "double",
        SqlType::Decimal => "decimal",
        SqlType::Bool => "bool",
        SqlType::Text => "string",
        SqlType::Timestamp => "date",
        other => {
            return Err(invalid(format!(
                "CAST to {other:?} is not supported by MongoDB"
            )));
        }
    };
    Ok(Bson::Document(
        doc! { "$convert": { "input": input, "to": to } },
    ))
}

/// Dates and times are stored as canonical text, so a string operand is
/// parsed first; timestamps are native dates.
fn as_date(x: &Bson) -> Bson {
    Bson::Document(doc! { "$cond": [
        { "$eq": [ { "$type": x.clone() }, "string" ] },
        { "$dateFromString": { "dateString": x.clone(), "onError": Bson::Null, "onNull": Bson::Null } },
        x.clone(),
    ] })
}

fn date_part(part: DatePart, x: &Bson) -> Res<Bson> {
    let date = as_date(x);
    let clock = |start: i64, native: &str| {
        // `HH:MM:SS.ffffff` time-of-day strings have ':' at index 2.
        Bson::Document(doc! { "$cond": [
            { "$and": [
                { "$eq": [ { "$type": x.clone() }, "string" ] },
                { "$eq": [ { "$indexOfCP": [x.clone(), ":"] }, 2_i64 ] },
            ] },
            { "$toInt": { "$substrCP": [x.clone(), start, 2_i64] } },
            { native: date.clone() },
        ] })
    };
    Ok(match part {
        DatePart::Year => Bson::Document(doc! { "$year": date }),
        DatePart::Month => Bson::Document(doc! { "$month": date }),
        DatePart::Day => Bson::Document(doc! { "$dayOfMonth": date }),
        DatePart::Week => Bson::Document(doc! { "$isoWeek": date }),
        DatePart::Quarter => Bson::Document(doc! { "$toInt": { "$ceil": { "$divide": [
            { "$month": date }, 3_i64
        ] } } }),
        DatePart::Hour => clock(0, "$hour"),
        DatePart::Minute => clock(3, "$minute"),
        DatePart::Second => clock(6, "$second"),
        DatePart::Date => {
            Bson::Document(doc! { "$dateToString": { "format": "%Y-%m-%d", "date": date } })
        }
    })
}

// ---------------------------------------------------------------------
// Aggregates
// ---------------------------------------------------------------------

struct AggPlan {
    /// `$group` fields for this aggregate.
    accumulators: Vec<(String, Bson)>,
    /// Expression over the group's fields producing the SQL result.
    finish: Bson,
}

fn acc(name: &str, op: &str, arg: Bson) -> (String, Bson) {
    (name.to_owned(), Bson::Document(doc! { op: arg }))
}

fn non_null_items(array: &str) -> Bson {
    Bson::Document(doc! { "$filter": {
        "input": array,
        "cond": { "$ne": ["$$this", Bson::Null] },
    } })
}

#[allow(clippy::too_many_lines)]
fn aggregate_plan(i: usize, agg: &Aggregate, ctx: &Ctx<'_>) -> Res<AggPlan> {
    let main = format!("a{i}");
    let num = format!("a{i}n");
    let field = format!("${main}");
    let row = Scope::row(ctx);
    let raw = match &agg.arg {
        Some(arg) => Some(expr_bson(arg, &row)?),
        None => None,
    };
    let filter = match &agg.filter {
        Some(f) => Some(expr_bson(f, &row)?),
        None => None,
    };
    // Rows failing FILTER contribute NULL, which every accumulator ignores.
    let arg = raw.map(|a| match &filter {
        Some(f) => Bson::Document(doc! { "$cond": [f.clone(), a, Bson::Null] }),
        None => a,
    });
    let func = &agg.func;
    let Some(arg) = arg else {
        return match func {
            AggFunc::Count if !agg.distinct => {
                let one = match &filter {
                    Some(f) => Bson::Document(doc! { "$cond": [f.clone(), 1_i64, 0_i64] }),
                    None => Bson::Int64(1),
                };
                Ok(AggPlan {
                    accumulators: vec![acc(&main, "$sum", one)],
                    finish: Bson::String(field),
                })
            }
            _ => Err(invalid("only COUNT(*) may omit its argument")),
        };
    };
    let distinct_items = || {
        (
            main.clone(),
            Bson::Document(doc! { "$addToSet": arg.clone() }),
        )
    };
    let items = non_null_items(&field);
    let over_items = |op: &str| Bson::Document(doc! { op: items.clone() });
    // Empty input to an aggregate that SQL defines as NULL.
    let nonempty = |value: Bson| {
        Bson::Document(doc! { "$cond": [
            { "$eq": [ { "$size": items.clone() }, 0_i64 ] },
            Bson::Null,
            value,
        ] })
    };
    Ok(match func {
        AggFunc::Count if agg.distinct => AggPlan {
            accumulators: vec![distinct_items()],
            finish: Bson::Document(doc! { "$size": items }),
        },
        AggFunc::Count => AggPlan {
            accumulators: vec![acc(
                &main,
                "$sum",
                Bson::Document(doc! { "$cond": [isnull(arg), 0_i64, 1_i64] }),
            )],
            finish: Bson::String(field),
        },
        AggFunc::Sum if agg.distinct => AggPlan {
            accumulators: vec![distinct_items()],
            finish: nonempty(over_items("$sum")),
        },
        AggFunc::Sum => AggPlan {
            accumulators: vec![
                acc(&main, "$sum", arg.clone()),
                acc(
                    &num,
                    "$sum",
                    Bson::Document(doc! { "$cond": [ { "$isNumber": arg }, 1_i64, 0_i64 ] }),
                ),
            ],
            finish: Bson::Document(doc! { "$cond": [
                { "$eq": [format!("${num}"), 0_i64] },
                Bson::Null,
                field,
            ] }),
        },
        AggFunc::Avg if agg.distinct => AggPlan {
            accumulators: vec![distinct_items()],
            finish: over_items("$avg"),
        },
        AggFunc::Avg => AggPlan {
            accumulators: vec![acc(&main, "$avg", arg)],
            finish: Bson::String(field),
        },
        AggFunc::Min => AggPlan {
            accumulators: vec![acc(&main, "$min", arg)],
            finish: Bson::String(field),
        },
        AggFunc::Max => AggPlan {
            accumulators: vec![acc(&main, "$max", arg)],
            finish: Bson::String(field),
        },
        AggFunc::StdDev { sample } | AggFunc::Variance { sample } => {
            let op = if *sample { "$stdDevSamp" } else { "$stdDevPop" };
            let (accumulators, deviation) = if agg.distinct {
                (vec![distinct_items()], over_items(op))
            } else {
                (vec![acc(&main, op, arg)], Bson::String(field.clone()))
            };
            let finish = if matches!(func, AggFunc::Variance { .. }) {
                Bson::Document(doc! { "$pow": [deviation, 2_i64] })
            } else {
                deviation
            };
            AggPlan {
                accumulators,
                finish,
            }
        }
        AggFunc::StringAgg { separator } => {
            let collect = if agg.distinct {
                distinct_items()
            } else {
                (main.clone(), Bson::Document(doc! { "$push": arg }))
            };
            let joined = let_in(
                "arr",
                items,
                Bson::Document(doc! { "$cond": [
                    { "$eq": [ { "$size": "$$arr" }, 0_i64 ] },
                    Bson::Null,
                    { "$reduce": {
                        "input": { "$slice": ["$$arr", 1_i64, { "$size": "$$arr" }] },
                        "initialValue": { "$arrayElemAt": ["$$arr", 0_i64] },
                        "in": { "$concat": ["$$value", separator.as_str(), "$$this"] },
                    } },
                ] }),
            );
            AggPlan {
                accumulators: vec![collect],
                finish: joined,
            }
        }
        AggFunc::ArrayAgg => return Err(unsupported(Feature::Arrays)),
    })
}

/// Collect every distinct aggregate of `expr` (not descending into their
/// arguments), in first-seen order.
fn collect_aggregates(expr: &Expr, out: &mut Vec<Aggregate>) {
    if let Expr::Aggregate(agg) = expr {
        if !out.contains(agg) {
            out.push(agg.clone());
        }
        return;
    }
    for child in expr.children() {
        collect_aggregates(child, out);
    }
}

// ---------------------------------------------------------------------
// Pipelines
// ---------------------------------------------------------------------

struct Built {
    collection: String,
    pipeline: Vec<Document>,
    columns: Vec<String>,
    pk: String,
    empty_row: Option<Vec<Value>>,
}

fn output_name(select: &SelectExpr, index: usize) -> String {
    match (&select.alias, &select.expr) {
        (Some(alias), _) => alias.to_string(),
        (None, Expr::Column(c)) => c.name.to_string(),
        (None, _) => format!("col{index}"),
    }
}

fn to_i64(n: u64, what: &str) -> Res<i64> {
    i64::try_from(n).map_err(|_| invalid(format!("{what} {n} is too large")))
}

#[allow(clippy::too_many_lines)]
fn build(plan: &QueryPlan, keys: &Keys) -> Res<Built> {
    // Rejections in case the caller skipped `check`.
    if !plan.joins.is_empty() {
        return Err(unsupported(Feature::Joins));
    }
    if plan.lock.is_some() {
        return Err(unsupported(Feature::RowLocking));
    }
    if !plan.compound.is_empty() {
        return Err(unsupported(Feature::SetOperations));
    }
    if matches!(plan.distinct, DistinctMode::On(_)) {
        return Err(unsupported(Feature::DistinctOn));
    }

    let mut pipeline: Vec<Document> = Vec::new();
    let (collection, root_pk, ctx_pk) = match &plan.source.subquery {
        Some(inner) => {
            let inner = build(inner, keys)?;
            pipeline = inner.pipeline;
            if inner.columns.is_empty() {
                // Expose the key under its column name.
                pipeline.push(doc! { "$addFields": { inner.pk.as_str(): "$_id" } });
                pipeline.push(doc! { "$unset": "_id" });
            }
            (inner.collection, inner.pk, None)
        }
        None => {
            check_name(&plan.source.name)?;
            let pk = keys.pk(&plan.source.name).to_owned();
            (plan.source.name.to_string(), pk.clone(), Some(pk))
        }
    };
    let ctx = Ctx {
        pk: ctx_pk.as_deref(),
        root: plan.source.reference().as_ref(),
    };

    if let Some(filter) = &plan.filter {
        let doc = filter_doc(filter, false, &ctx)?;
        if !doc.is_empty() {
            pipeline.push(doc! { "$match": doc });
        }
    }

    let has_aggregate = plan
        .projection
        .iter()
        .map(|s| &s.expr)
        .chain(plan.having.iter())
        .chain(plan.ordering.iter().map(|o| &o.expr))
        .any(Expr::contains_aggregate);
    let grouped = has_aggregate || !plan.grouping.is_empty() || plan.having.is_some();

    let columns: Vec<String> = plan
        .projection
        .iter()
        .enumerate()
        .map(|(i, s)| output_name(s, i))
        .collect();
    let distinct = plan.distinct == DistinctMode::All && !columns.is_empty();
    let mut empty_row = None;

    if grouped {
        if plan.projection.is_empty() {
            return Err(invalid("a grouped query needs an explicit projection"));
        }
        let mut aggs = Vec::new();
        plan.projection
            .iter()
            .map(|s| &s.expr)
            .chain(plan.having.iter())
            .chain(plan.ordering.iter().map(|o| &o.expr))
            .for_each(|e| collect_aggregates(e, &mut aggs));
        let group = GroupScope {
            keys: plan.grouping.clone(),
            aggs,
            aliases: plan
                .projection
                .iter()
                .zip(&columns)
                .map(|(s, name)| (name.clone(), s.expr.clone()))
                .collect(),
        };
        let row_scope = Scope::row(&ctx);
        let key = if group.keys.is_empty() {
            Bson::Null
        } else {
            let mut key = Document::new();
            for (i, k) in group.keys.iter().enumerate() {
                key.insert(format!("k{i}"), expr_bson(k, &row_scope)?);
            }
            Bson::Document(key)
        };
        let mut group_doc = doc! { "_id": key };
        for (i, agg) in group.aggs.iter().enumerate() {
            for (name, spec) in aggregate_plan(i, agg, &ctx)?.accumulators {
                group_doc.insert(name, spec);
            }
        }

        let only_count_star = group.keys.is_empty()
            && plan.having.is_none()
            && plan.projection.len() == 1
            && matches!(&plan.projection[0].expr, Expr::Aggregate(a)
                if a.func == AggFunc::Count && a.arg.is_none() && a.filter.is_none() && !a.distinct);
        if only_count_star {
            pipeline.push(doc! { "$count": columns[0].as_str() });
        } else {
            pipeline.push(doc! { "$group": group_doc });
        }

        let post = Scope {
            ctx: &ctx,
            group: Some(&group),
        };
        if !only_count_star && let Some(having) = &plan.having {
            pipeline.push(doc! { "$match": { "$expr": expr_bson(having, &post)? } });
        }
        let mut project = doc! { "_id": 0_i64 };
        for (select, name) in plan.projection.iter().zip(&columns) {
            let value = if only_count_star {
                Bson::String(format!("${name}"))
            } else {
                expr_bson(&select.expr, &post)?
            };
            project.insert(name.clone(), value);
        }
        let mut temps = Vec::new();
        let sort = sort_document(plan, &columns, |i, expr| {
            let name = format!("__sort{i}");
            project.insert(name.clone(), expr_bson(expr, &post)?);
            temps.push(name.clone());
            Ok(name)
        })?;
        if !only_count_star {
            pipeline.push(doc! { "$project": project });
        }
        if group.keys.is_empty()
            && plan.having.is_none()
            && plan.offset.unwrap_or(0) == 0
            && plan.limit != Some(0)
        {
            empty_row = Some(
                plan.projection
                    .iter()
                    .map(|s| match &s.expr {
                        Expr::Aggregate(a) if a.func == AggFunc::Count => Value::Int(0),
                        _ => Value::Null,
                    })
                    .collect(),
            );
        }
        finish_pipeline(&mut pipeline, plan, distinct, &columns, sort, &temps)?;
    } else if distinct {
        // DISTINCT: de-duplicate the projected rows, then order them.
        let scope = Scope::row(&ctx);
        pipeline.push(doc! { "$project": projection_doc(plan, &columns, &scope)? });
        let sort = sort_document(plan, &columns, |_, _| {
            Err(invalid(
                "with DISTINCT, ORDER BY must use projected expressions",
            ))
        })?;
        finish_pipeline(&mut pipeline, plan, true, &columns, sort, &[])?;
    } else {
        let scope = Scope::row(&ctx);
        let mut temps = Vec::new();
        let mut add_fields = Document::new();
        let sort = sort_document_source(plan, &scope, &mut add_fields, &mut temps)?;
        if !add_fields.is_empty() {
            pipeline.push(doc! { "$addFields": add_fields });
        }
        if let Some(sort) = sort {
            pipeline.push(doc! { "$sort": sort });
        }
        if let Some(n) = plan.offset
            && n > 0
        {
            pipeline.push(doc! { "$skip": to_i64(n, "offset")? });
        }
        if let Some(n) = plan.limit {
            pipeline.push(doc! { "$limit": to_i64(n, "limit")? });
        }
        if columns.is_empty() {
            if !temps.is_empty() {
                pipeline.push(doc! { "$unset": temps });
            }
        } else {
            pipeline.push(doc! { "$project": projection_doc(plan, &columns, &scope)? });
        }
    }

    Ok(Built {
        collection,
        pipeline,
        columns,
        pk: root_pk,
        empty_row,
    })
}

fn projection_doc(plan: &QueryPlan, columns: &[String], scope: &Scope<'_>) -> Res<Document> {
    let mut project = doc! { "_id": 0_i64 };
    for (select, name) in plan.projection.iter().zip(columns) {
        check_name(name)?;
        project.insert(name.clone(), expr_bson(&select.expr, scope)?);
    }
    Ok(project)
}

/// Sort keys evaluated on source documents (ungrouped, non-distinct).
fn sort_document_source(
    plan: &QueryPlan,
    scope: &Scope<'_>,
    add_fields: &mut Document,
    temps: &mut Vec<String>,
) -> Res<Option<Document>> {
    if plan.ordering.is_empty() {
        return Ok(None);
    }
    let mut sort = Document::new();
    for (i, order) in plan.ordering.iter().enumerate() {
        let direction = match order.direction {
            OrderDirection::Asc => 1_i32,
            OrderDirection::Desc => -1_i32,
        };
        let name = match &order.expr {
            Expr::Column(c) => scope.ctx.field(c)?,
            other => {
                let name = format!("__sort{i}");
                add_fields.insert(name.clone(), expr_bson(other, scope)?);
                temps.push(name.clone());
                name
            }
        };
        sort.insert(name, direction);
    }
    Ok(Some(sort))
}

/// Sort keys over projected output (grouped or distinct): an ordering that
/// equals a projected expression or names an output column uses that
/// output; anything else is handed to `extra`.
fn sort_document(
    plan: &QueryPlan,
    columns: &[String],
    mut extra: impl FnMut(usize, &Expr) -> Res<String>,
) -> Res<Option<Document>> {
    if plan.ordering.is_empty() {
        return Ok(None);
    }
    let mut sort = Document::new();
    for (i, order) in plan.ordering.iter().enumerate() {
        let direction = match order.direction {
            OrderDirection::Asc => 1_i32,
            OrderDirection::Desc => -1_i32,
        };
        let by_expr = plan
            .projection
            .iter()
            .position(|s| s.expr == order.expr)
            .map(|p| columns[p].clone());
        let by_name = match &order.expr {
            Expr::Column(c) if c.source.is_none() => columns
                .iter()
                .find(|name| **name == c.name.as_ref())
                .cloned(),
            _ => None,
        };
        let name = match by_expr.or(by_name) {
            Some(name) => name,
            None => extra(i, &order.expr)?,
        };
        sort.insert(name, direction);
    }
    Ok(Some(sort))
}

fn finish_pipeline(
    pipeline: &mut Vec<Document>,
    plan: &QueryPlan,
    distinct: bool,
    columns: &[String],
    sort: Option<Document>,
    temps: &[String],
) -> Res<()> {
    if distinct {
        let mut key = Document::new();
        for name in columns {
            key.insert(name.clone(), format!("${name}"));
        }
        pipeline.push(doc! { "$group": { "_id": key } });
        pipeline.push(doc! { "$replaceRoot": { "newRoot": "$_id" } });
    }
    if let Some(sort) = sort {
        pipeline.push(doc! { "$sort": sort });
    }
    if let Some(n) = plan.offset
        && n > 0
    {
        pipeline.push(doc! { "$skip": to_i64(n, "offset")? });
    }
    if let Some(n) = plan.limit {
        pipeline.push(doc! { "$limit": to_i64(n, "limit")? });
    }
    if !temps.is_empty() {
        pipeline.push(doc! { "$unset": temps.to_vec() });
    }
    Ok(())
}
