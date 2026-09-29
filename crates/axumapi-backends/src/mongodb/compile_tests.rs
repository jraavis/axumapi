//! Unit tests of the pure compiler (no server).
#![allow(clippy::unwrap_used)]

use super::compile::{
    CompiledQuery, CompiledWrite, Keys, UpdateSpec, compile_query, compile_write,
};
use ::mongodb::bson::{Bson, Document, Regex, doc};
use axumapi_orm::expr::{Column, DatePart, Field};
use axumapi_orm::functions::{lower, substr};
use axumapi_orm::{
    BackendCapabilityError, Count, DeletePlan, DistinctMode, Expr, Feature, InsertPlan, JoinKind,
    LockMode, Max, OrderDirection, OrmError, QueryPlan, QuerySource, SetOp, Sum, UpdatePlan, Value,
    Window, WindowFunc, WritePlan,
};

struct T;
#[allow(non_upper_case_globals)]
impl T {
    const name: Field<T, String> = Field::new("name");
    const age: Field<T, Option<i32>> = Field::new("age");
    const team: Field<T, Option<i32>> = Field::new("team");
}

fn q(plan: &QueryPlan) -> CompiledQuery {
    compile_query(plan, &Keys::default()).unwrap()
}

fn matched(plan: &QueryPlan) -> Document {
    q(plan).pipeline[0].get_document("$match").unwrap().clone()
}

fn base() -> QueryPlan {
    QueryPlan::from_table("users")
}

#[test]
fn key_column_maps_to_id_everywhere() {
    let plan = base()
        .filter(Expr::col("id").eq(5_i64))
        .order_by(Expr::col("id"), OrderDirection::Desc);
    let c = q(&plan);
    assert_eq!(
        c.pipeline[0],
        doc! { "$match": { "_id": { "$eq": 5_i64 } } }
    );
    assert_eq!(c.pipeline[1], doc! { "$sort": { "_id": -1 } });
    let custom = compile_query(
        &base().filter(Expr::col("slug").eq("a")),
        &Keys::default().with("users", "slug"),
    )
    .unwrap();
    assert_eq!(
        custom.pipeline[0],
        doc! { "$match": { "_id": { "$eq": "a" } } }
    );
    assert_eq!(custom.pk, "slug");
}

#[test]
fn comparisons_use_query_operators_with_sql_null_semantics() {
    assert_eq!(
        matched(&base().filter(T::age.ge(3))),
        doc! { "age": { "$gte": 3_i64 } }
    );
    assert_eq!(
        matched(&base().filter(T::age.ne(3))),
        doc! { "age": { "$nin": [3_i64, Bson::Null] } }
    );
    assert_eq!(
        matched(&base().filter(T::age.eq(None::<i32>))),
        doc! { "age": Bson::Null }
    );
    assert_eq!(
        matched(&base().filter(T::age.ne(None::<i32>))),
        doc! { "age": { "$ne": Bson::Null } }
    );
    // `3 < age` flips to `age > 3`.
    assert_eq!(
        matched(&base().filter(Expr::val(3_i64).lt(Expr::col("age")))),
        doc! { "age": { "$gt": 3_i64 } }
    );
    // exclude(age > 3) is age <= 3 (NULL stays out), not `$not`.
    assert_eq!(
        matched(&base().exclude(T::age.gt(3))),
        doc! { "age": { "$lte": 3_i64 } }
    );
    assert_eq!(
        matched(&base().exclude(T::age.eq(3))),
        doc! { "age": { "$nin": [3_i64, Bson::Null] } }
    );
    assert_eq!(
        matched(&base().exclude(T::age.is_in([1, 2]))),
        doc! { "age": { "$nin": [1_i64, 2_i64, Bson::Null] } }
    );
    assert_eq!(
        matched(&base().exclude(T::age.range(1, 5))),
        doc! { "$or": [{ "age": { "$lt": 1_i64 } }, { "age": { "$gt": 5_i64 } }] }
    );
    assert_eq!(
        matched(&base().exclude(T::age.is_null())),
        doc! { "age": { "$ne": Bson::Null } }
    );
}

#[test]
fn boolean_structure_applies_de_morgan_when_negated() {
    let both = T::age.gt(3).and(T::team.eq(1));
    assert_eq!(
        matched(&base().filter(both.clone())),
        doc! { "$and": [{ "age": { "$gt": 3_i64 } }, { "team": { "$eq": 1_i64 } }] }
    );
    assert_eq!(
        matched(&base().exclude(both)),
        doc! { "$or": [
            { "age": { "$lte": 3_i64 } },
            { "team": { "$nin": [1_i64, Bson::Null] } },
        ] }
    );
    assert_eq!(
        matched(&base().filter(Expr::Or(vec![]))),
        doc! { "$expr": false }
    );
    assert!(q(&base().filter(Expr::And(vec![]))).pipeline.is_empty());
}

#[test]
fn text_lookups_are_escaped_regexes() {
    assert_eq!(
        matched(&base().filter(T::name.icontains("a.b"))),
        doc! { "name": { "$regex": "a\\.b", "$options": "i" } }
    );
    assert_eq!(
        matched(&base().filter(T::name.starts_with("x+"))),
        doc! { "name": { "$regex": "^x\\+" } }
    );
    assert_eq!(
        matched(&base().filter(T::name.ends_with("$"))),
        doc! { "name": { "$regex": "\\$\\z" } }
    );
    assert_eq!(
        matched(&base().filter(T::name.iexact("Bob"))),
        doc! { "name": { "$regex": "^Bob\\z", "$options": "i" } }
    );
    assert_eq!(
        matched(&base().filter(T::name.regex("^a.*"))),
        doc! { "name": { "$regex": "^a.*" } }
    );
    // Negation keeps NULL out.
    let negated = Bson::RegularExpression(Regex {
        pattern: "q".into(),
        options: String::new(),
    });
    assert_eq!(
        matched(&base().exclude(T::name.contains("q"))),
        doc! { "name": { "$ne": Bson::Null, "$not": negated } }
    );
    assert!(compile_query(&base().filter(T::name.contains("\0")), &Keys::default()).is_err());
}

#[test]
fn other_predicates_fall_back_to_expr_with_null_guards() {
    let m = matched(&base().filter(T::age.gt(T::team)));
    let expr = m.get_document("$expr").unwrap();
    assert!(expr.contains_key("$cond"), "{expr:?}");
    let m = matched(&base().filter(lower(T::name.expr()).eq("x")));
    assert!(m.contains_key("$expr"));
    let m = matched(&base().exclude(T::age.gt(T::team)));
    let text = format!("{m:?}");
    assert!(text.contains("$let") && text.contains("$not"), "{text}");
}

#[test]
fn projection_and_ordering_compile_to_project_and_sort() {
    let plan = base()
        .select(Expr::col("id"), Some("id"))
        .select(T::name.expr(), Some("n"))
        .select(Expr::val(1_i64), Some("one"))
        .order_by(lower(T::name.expr()), OrderDirection::Asc)
        .order_by(Expr::col("age"), OrderDirection::Desc)
        .offset(2)
        .limit(3);
    let c = q(&plan);
    assert_eq!(c.columns, ["id", "n", "one"]);
    assert_eq!(c.pipeline.len(), 5);
    assert!(c.pipeline[0].contains_key("$addFields"));
    assert_eq!(c.pipeline[1], doc! { "$sort": { "__sort0": 1, "age": -1 } });
    assert_eq!(c.pipeline[2], doc! { "$skip": 2_i64 });
    assert_eq!(c.pipeline[3], doc! { "$limit": 3_i64 });
    assert_eq!(
        c.pipeline[4],
        doc! { "$project": {
            "_id": 0_i64, "id": "$_id", "n": "$name", "one": { "$literal": 1_i64 },
        } }
    );
    // Whole documents: temporaries are dropped afterwards.
    let c = q(&base().order_by(lower(T::name.expr()), OrderDirection::Asc));
    assert_eq!(c.pipeline.last().unwrap(), &doc! { "$unset": ["__sort0"] });
}

#[test]
fn count_star_is_count_and_reports_the_empty_row() {
    let plan = base()
        .filter(T::age.gt(1))
        .select(Count::all(), Some("count"));
    let c = q(&plan);
    assert_eq!(c.pipeline[1], doc! { "$count": "count" });
    assert_eq!(c.empty_row, Some(vec![Value::Int(0)]));
    let sum = base().select(Sum::of(Expr::col("age")), Some("s"));
    assert_eq!(q(&sum).empty_row, Some(vec![Value::Null]));
    assert!(q(&sum).pipeline[0].contains_key("$group"));
}

#[test]
fn grouping_having_and_ordering_use_group_output() {
    let mut plan = base()
        .select(Expr::col("team"), Some("team"))
        .select(Count::all(), Some("n"))
        .select(Max::of(Expr::col("age")), Some("oldest"))
        .order_by(Expr::from(Count::all()), OrderDirection::Desc);
    plan.grouping = vec![Expr::col("team")];
    plan.having = Some(Expr::from(Count::all()).gt(1_i64));
    let c = q(&plan);
    let group = c.pipeline[0].get_document("$group").unwrap();
    assert_eq!(group.get_document("_id").unwrap(), &doc! { "k0": "$team" });
    assert!(group.contains_key("a0") && group.contains_key("a1"));
    assert!(
        c.pipeline[1]
            .get_document("$match")
            .unwrap()
            .contains_key("$expr")
    );
    let project = c.pipeline[2].get_document("$project").unwrap();
    assert_eq!(project.get_str("team").unwrap(), "$_id.k0");
    assert_eq!(project.get_str("n").unwrap(), "$a0");
    assert_eq!(c.pipeline[3], doc! { "$sort": { "n": -1 } });
    assert!(c.empty_row.is_none());
    // A bare column outside the grouping is rejected.
    let mut bad = base()
        .select(Expr::col("name"), Some("name"))
        .select(Count::all(), Some("n"));
    bad.grouping = vec![Expr::col("team")];
    assert!(compile_query(&bad, &Keys::default()).is_err());
}

#[test]
fn distinct_and_derived_tables_compile_to_stages() {
    let plan = base()
        .select(Expr::col("team"), Some("team"))
        .distinct(DistinctMode::All)
        .order_by(Expr::col("team"), OrderDirection::Asc)
        .limit(4);
    let c = q(&plan);
    let names: Vec<&str> = c
        .pipeline
        .iter()
        .map(|s| s.keys().next().unwrap().as_str())
        .collect();
    assert_eq!(
        names,
        ["$project", "$group", "$replaceRoot", "$sort", "$limit"]
    );
    let counted =
        QueryPlan::from_subquery(base().limit(3), "counted").select(Count::all(), Some("count"));
    let c = q(&counted);
    assert_eq!(c.collection, "users");
    assert_eq!(c.pipeline[0], doc! { "$limit": 3_i64 });
    // The inner whole-document output exposes the key column.
    assert_eq!(c.pipeline[1], doc! { "$addFields": { "id": "$_id" } });
    assert_eq!(c.pipeline[2], doc! { "$unset": "_id" });
    assert_eq!(c.pipeline.last().unwrap(), &doc! { "$count": "count" });
}

#[test]
fn unsupported_plans_are_rejected_at_compile_time() {
    let keys = Keys::default();
    let feature = |plan: &QueryPlan| match compile_query(plan, &keys) {
        Err(OrmError::Capability(BackendCapabilityError::Unsupported { feature, .. })) => {
            Some(feature)
        }
        Err(OrmError::Capability(BackendCapabilityError::RowLockingUnsupported { .. })) => {
            Some(Feature::RowLocking)
        }
        _ => None,
    };
    let joined = base().join(
        JoinKind::Inner,
        QuerySource::table("teams"),
        Expr::col("a").eq(Expr::col("b")),
    );
    assert_eq!(feature(&joined), Some(Feature::Joins));
    assert_eq!(
        feature(&base().lock(LockMode::ForUpdate)),
        Some(Feature::RowLocking)
    );
    assert_eq!(
        feature(&base().distinct(DistinctMode::On(vec![Expr::col("a")]))),
        Some(Feature::DistinctOn)
    );
    let mut union = base();
    union.compound.push(axumapi_orm::Compound {
        op: SetOp::Union,
        plan: base(),
    });
    assert_eq!(feature(&union), Some(Feature::SetOperations));
    let window = base().select(
        Expr::Window(Box::new(Window::over(WindowFunc::RowNumber))),
        Some("rn"),
    );
    assert_eq!(feature(&window), Some(Feature::WindowFunctions));
    assert_eq!(
        feature(&base().filter(Expr::exists(base()))),
        Some(Feature::Subqueries)
    );
    assert_eq!(
        feature(&base().filter(Expr::col("a").in_subquery(base()))),
        Some(Feature::Subqueries)
    );
    assert_eq!(
        feature(&base().select(axumapi_orm::ArrayAgg::of(Expr::col("a")), Some("x"))),
        Some(Feature::Arrays)
    );
    // Structural problems are InvalidPlan, not capability errors.
    let outer = base().filter(Expr::outer("a").eq(1_i64));
    assert!(feature(&outer).is_none());
    assert!(compile_query(&outer, &keys).is_err());
    assert!(compile_query(&base().filter(Expr::col("a.b").eq(1_i64)), &keys).is_err());
    assert!(compile_query(&base().filter(Expr::col("$a").eq(1_i64)), &keys).is_err());
    assert!(compile_query(&QueryPlan::from_table("a.b"), &keys).is_err());
    let qualified = base().filter(Expr::Column(Column::qualified("other", "a")).eq(1_i64));
    assert!(compile_query(&qualified, &keys).is_err());
}

#[test]
fn expressions_compile_to_aggregation_operators() {
    let plan = base()
        .select(substr(T::name.expr(), 2, Some(3)), Some("s"))
        .select(T::age.expr() + 1_i64, Some("next"))
        .select(
            Expr::Case {
                branches: vec![(T::age.gt(1), Expr::val("old"))],
                otherwise: None,
            },
            Some("c"),
        );
    let text = format!("{:?}", q(&plan).pipeline[0]);
    for needle in ["$substrCP", "$subtract", "$add", "$switch"] {
        assert!(text.contains(needle), "{needle} missing in {text}");
    }
    let dates = base().select(Expr::col("d").date_part(DatePart::Quarter), Some("q"));
    assert!(format!("{:?}", q(&dates).pipeline[0]).contains("$dateFromString"));
}

#[test]
fn writes_compile_with_key_mapping() {
    let keys = Keys::default();
    let insert = InsertPlan {
        table: "users".into(),
        columns: vec!["id".into(), "name".into()],
        rows: vec![
            vec![Value::Int(1), "a".into()],
            vec![Value::Int(2), "b".into()],
        ],
        returning: vec!["id".into()],
    };
    let CompiledWrite::Insert(i) = compile_write(&WritePlan::Insert(insert), &keys).unwrap() else {
        panic!("not an insert");
    };
    assert_eq!(i.docs[0], doc! { "_id": 1_i64, "name": "a" });
    let generated = InsertPlan {
        table: "users".into(),
        columns: vec!["name".into()],
        rows: vec![vec!["a".into()]],
        returning: vec![],
    };
    let CompiledWrite::Insert(i) = compile_write(&WritePlan::Insert(generated), &keys).unwrap()
    else {
        panic!("not an insert");
    };
    assert_eq!(i.docs[0], doc! { "name": "a" });

    let literal = UpdatePlan {
        table: "users".into(),
        assignments: vec![
            ("age".into(), Expr::val(3_i64)),
            ("nick".into(), Expr::val(Value::Null)),
        ],
        filter: Some(Expr::col("id").eq(1_i64)),
        returning: vec![],
    };
    let CompiledWrite::Update(u) = compile_write(&WritePlan::Update(literal), &keys).unwrap()
    else {
        panic!("not an update");
    };
    assert_eq!(u.filter, doc! { "_id": { "$eq": 1_i64 } });
    assert_eq!(
        u.update,
        UpdateSpec::Set(doc! { "$set": { "age": 3_i64, "nick": Bson::Null } })
    );
    let f = UpdatePlan {
        table: "users".into(),
        assignments: vec![("age".into(), Expr::col("age") + 1_i64)],
        filter: None,
        returning: vec!["age".into()],
    };
    let CompiledWrite::Update(u) = compile_write(&WritePlan::Update(f), &keys).unwrap() else {
        panic!("not an update");
    };
    assert!(u.filter.is_empty());
    assert!(matches!(u.update, UpdateSpec::Pipeline(ref p) if p.len() == 1));
    let pk_update = UpdatePlan {
        table: "users".into(),
        assignments: vec![("id".into(), Expr::val(3_i64))],
        filter: None,
        returning: vec![],
    };
    assert!(compile_write(&WritePlan::Update(pk_update), &keys).is_err());
    let sub_delete = DeletePlan {
        table: "users".into(),
        filter: Some(Expr::col("id").in_subquery(base())),
        returning: vec![],
    };
    assert!(matches!(
        compile_write(&WritePlan::Delete(sub_delete), &keys),
        Err(OrmError::Capability(_))
    ));
}
