//! Borrowed write compilation keeps emulation and validation contracts.

use super::{compile_write, compile_write_bare};
use crate::sql::MySql;
use siderite_orm::{DeletePlan, DistinctMode, Expr, InsertPlan, OrmError};
use siderite_orm::{QueryError, QueryPlan, UpdatePlan, Value, WritePlan};

fn clear_returning(plan: &mut WritePlan) {
    match plan {
        WritePlan::Insert(p) => p.returning.clear(),
        WritePlan::Update(p) => p.returning.clear(),
        WritePlan::Delete(p) => p.returning.clear(),
    }
}

#[test]
fn borrowed_write_keeps_sql_and_values() -> Result<(), OrmError> {
    let plans = [
        WritePlan::Insert(InsertPlan {
            table: "odd`table".into(),
            columns: vec!["title".into(), "done".into()],
            rows: vec![
                vec![Value::Text("🪨'\\".repeat(1024)), Value::Bool(false)],
                vec![Value::Null, Value::Bool(true)],
            ],
            returning: vec!["id".into(), "title".into()],
        }),
        WritePlan::Update(UpdatePlan {
            table: "todos".into(),
            assignments: vec![
                ("likes".into(), Expr::col("dislikes")),
                ("dislikes".into(), Expr::Value(Value::Int(0))),
            ],
            filter: Some(Expr::col("id").eq(Expr::Value(Value::Int(42)))),
            returning: vec!["id".into()],
        }),
        WritePlan::Delete(DeletePlan {
            table: "todos".into(),
            filter: Some(Expr::col("id").eq(Expr::Value(Value::Int(42)))),
            returning: vec!["title".into()],
        }),
    ];
    for plan in plans {
        let original = plan.clone();
        let mut old = plan.clone();
        clear_returning(&mut old);
        assert_eq!(
            compile_write_bare(&plan, &MySql)?,
            compile_write(&old, &MySql)?,
        );
        assert_eq!(plan, original);
        assert!(matches!(
            compile_write(&plan, &MySql),
            Err(OrmError::Capability(_)),
        ));
    }
    Ok(())
}

#[test]
fn borrowed_default_values_still_validate() -> Result<(), OrmError> {
    let mut plan = InsertPlan {
        table: "todos".into(),
        columns: vec![],
        rows: vec![vec![]],
        returning: vec!["id".into()],
    };
    let write = WritePlan::Insert(plan.clone());
    let compiled = compile_write_bare(&write, &MySql)?;
    assert_eq!(compiled.sql, "INSERT INTO `todos` () VALUES ()");
    assert!(compiled.params.is_empty());
    plan.rows.push(vec![]);
    assert!(matches!(
        compile_write_bare(&WritePlan::Insert(plan), &MySql),
        Err(OrmError::Query(QueryError::InvalidPlan(_))),
    ));
    Ok(())
}

#[test]
fn borrowed_write_rejects_other_capabilities() {
    let distinct = DistinctMode::On(vec![Expr::col("title")]);
    let unsupported = QueryPlan::from_table("todos").distinct(distinct);
    let plan = WritePlan::Update(UpdatePlan {
        table: "todos".into(),
        assignments: vec![("title".into(), Expr::subquery(unsupported))],
        filter: None,
        returning: vec!["id".into()],
    });
    assert!(matches!(
        compile_write_bare(&plan, &MySql),
        Err(OrmError::Capability(_)),
    ));
}

#[test]
fn borrowed_write_rejects_malformed_rows() {
    let plan = WritePlan::Insert(InsertPlan {
        table: "todos".into(),
        columns: vec!["title".into()],
        rows: vec![vec![]],
        returning: vec!["id".into()],
    });
    assert!(matches!(
        compile_write_bare(&plan, &MySql),
        Err(OrmError::Query(QueryError::InvalidPlan(_))),
    ));
}
