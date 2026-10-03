//! Shared live RETURNING/storage contracts for both MySQL adapters.

use super::common::{Book, Team, seed};
use super::fixture::{self, TestDb};
use siderite_orm::{Expr, Model, OrmError, Value};

type Columns = Vec<&'static str>;

#[tokio::test]
#[ignore = "requires explicit MYSQL_URL and an isolated live service"]
async fn returning_is_emulated_for_updates_deletes_and_inserts() {
    use siderite_orm::{DeletePlan, InsertPlan, UpdatePlan, WritePlan};
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let s = seed(db).await;
    let columns = || vec!["id".into(), "title".into(), "likes".into()];

    // Multi-row UPDATE .. RETURNING: rows come back as updated.
    let updated = db
        .execute(&WritePlan::Update(UpdatePlan {
            table: "books".into(),
            assignments: vec![("likes".into(), Expr::col("likes") + 100_i64)],
            filter: Some(Book::author.eq(s.ann.id)),
            returning: columns(),
        }))
        .await
        .unwrap();
    assert_eq!(updated.rows_affected, 2);
    let mut likes: Vec<i64> = updated
        .returning
        .iter()
        .map(|r| r.get_as::<i64>("likes").unwrap())
        .collect();
    likes.sort_unstable();
    assert_eq!(likes, [105, 110]);

    // The filter may stop matching after the update: rows are read by key.
    let moved = db
        .execute(&WritePlan::Update(UpdatePlan {
            table: "books".into(),
            assignments: vec![("likes".into(), Expr::val(0_i64))],
            filter: Some(Book::likes.gt(100_i64)),
            returning: columns(),
        }))
        .await
        .unwrap();
    assert_eq!(moved.returning.len(), 2);
    assert!(
        moved
            .returning
            .iter()
            .all(|r| r.get_as::<i64>("likes").unwrap() == 0)
    );

    // No match: nothing changes, nothing is returned.
    let none = db
        .execute(&WritePlan::Update(UpdatePlan {
            table: "books".into(),
            assignments: vec![("likes".into(), Expr::val(1_i64))],
            filter: Some(Book::likes.gt(9_999_i64)),
            returning: columns(),
        }))
        .await
        .unwrap();
    assert_eq!((none.rows_affected, none.returning.len()), (0, 0));

    // DELETE .. RETURNING: the rows are read (and locked) before they vanish.
    let deleted = db
        .execute(&WritePlan::Delete(DeletePlan {
            table: "books".into(),
            filter: Some(Book::author.eq(s.dee.id)),
            returning: columns(),
        }))
        .await
        .unwrap();
    assert_eq!(deleted.rows_affected, 2);
    let mut gone: Vec<String> = deleted
        .returning
        .iter()
        .map(|r| r.get_as::<String>("title").unwrap())
        .collect();
    gone.sort();
    assert_eq!(gone, ["Go", "Zig"]);
    assert_eq!(Book::objects(db).count().await.unwrap(), 3);

    // The same inside a transaction, rolled back with it.
    let inside = db
        .transaction(|tx| async move {
            let rows = ["x", "y", "z"].map(|name| vec![name.into()]);
            let inserted = tx
                .execute(&WritePlan::Insert(InsertPlan {
                    table: "teams".into(),
                    columns: vec!["name".into()],
                    rows: rows.to_vec(),
                    returning: vec!["id".into(), "name".into()],
                }))
                .await?;
            let ids: Vec<i64> = inserted
                .returning
                .iter()
                .map(|r| r.get_as::<i64>("id").unwrap())
                .collect();
            assert_eq!(ids.windows(2).filter(|w| w[1] == w[0] + 1).count(), 2);
            let names: Vec<String> = inserted
                .returning
                .iter()
                .map(|r| r.get_as::<String>("name").unwrap())
                .collect();
            assert_eq!(names, ["x", "y", "z"]);
            Err::<(), _>(OrmError::from(siderite_orm::QueryError::InvalidPlan(
                "undo".into(),
            )))
        })
        .await;
    assert!(inside.is_err());
    assert_eq!(Team::objects(db).count().await.unwrap(), 2);

    // A table without a primary key cannot use the emulation; it says so.
    db.execute_script("CREATE TABLE loose (a INT)")
        .await
        .unwrap();
    let err = db
        .execute(&WritePlan::Insert(InsertPlan {
            table: "loose".into(),
            columns: vec!["a".into()],
            rows: vec![vec![Value::Int(1)]],
            returning: vec!["a".into()],
        }))
        .await;
    assert!(matches!(err, Err(OrmError::Query(_))), "{err:?}");
    t.cleanup().await;
}

/// A single-row insert returns its row without reading it back when the
/// stored values are known; the row must equal what a read gives.
#[tokio::test]
#[ignore = "requires explicit MYSQL_URL and an isolated live service"]
async fn single_row_inserts_return_the_stored_row() {
    use siderite_orm::{InsertPlan, WritePlan};
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    db.execute_script(
        "CREATE TABLE plain (
            id BIGINT AUTO_INCREMENT PRIMARY KEY,
            name VARCHAR(8) NOT NULL, body TEXT,
            n INT, small TINYINT, flag BOOLEAN NOT NULL,
            legacy VARCHAR(8) CHARACTER SET latin1);
         CREATE TABLE defaults (
            id BIGINT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(8) NOT NULL,
            state VARCHAR(8) NOT NULL DEFAULT 'new',
            shout VARCHAR(8) AS (UPPER(name)),
            price DECIMAL(6,2), tag CHAR(4));
         CREATE TABLE triggered (
            id BIGINT AUTO_INCREMENT PRIMARY KEY,
            name VARCHAR(8) NOT NULL);
         CREATE TRIGGER triggered_bi BEFORE INSERT ON triggered
            FOR EACH ROW SET NEW.name = UPPER(NEW.name);",
    )
    .await
    .unwrap();
    let insert = |table: &'static str, columns: Columns, row: Vec<Value>| {
        let returning: Vec<&'static str> = match table {
            "plain" => {
                let fields = ["id", "name", "body", "n", "small", "flag"];
                fields.into_iter().chain(["legacy"]).collect()
            }
            "defaults" => vec!["id", "name", "state", "shout", "price", "tag"],
            _ => vec!["id", "name"],
        };
        WritePlan::Insert(InsertPlan {
            table: table.into(),
            columns: columns.into_iter().map(Into::into).collect(),
            rows: vec![row],
            returning: returning.into_iter().map(Into::into).collect(),
        })
    };
    // What a read of the row with the returned key gives.
    let stored = |table: &'static str, row: &siderite_orm::Row| {
        let columns: Vec<&str> = row.iter().map(|(c, _)| c).collect();
        let projection = columns.join(", ");
        let sql = format!("SELECT {projection} FROM {table} WHERE id = ?");
        let id = row.get("id").cloned().unwrap();
        async move { db.raw_sql(&sql, vec![id]).await.unwrap().rows.remove(0) }
    };
    let plain_columns = || {
        let fields = ["name", "body", "n", "small", "flag"];
        fields.into_iter().chain(["legacy"]).collect()
    };

    // Known without a read-back: integers, booleans, text, NULLs.
    for row in [
        vec![
            "ünï 🦀".into(),
            "long text".into(),
            Value::Int(-7),
            Value::Int(127),
            Value::Bool(true),
            "ascii".into(),
        ],
        vec![
            "".into(),
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Bool(false),
            Value::Null,
        ],
        // Not ASCII in a latin1 column: read back.
        vec![
            "x".into(),
            Value::Null,
            Value::Int(1),
            Value::Int(1),
            Value::Bool(true),
            "é".into(),
        ],
    ] {
        let done = db
            .execute(&insert("plain", plain_columns(), row))
            .await
            .unwrap();
        assert_eq!(done.rows_affected, 1);
        assert_eq!(done.returning.len(), 1);
        let expected = stored("plain", &done.returning[0]).await;
        assert_eq!(done.returning[0], expected);
    }
    let ids: Vec<i64> = db
        .raw_sql("SELECT id FROM plain ORDER BY id", vec![])
        .await
        .unwrap()
        .rows
        .iter()
        .map(|r| r.get_as::<i64>("id").unwrap())
        .collect();
    assert_eq!(ids, [1, 2, 3]);

    // Text that does not fit is the server's to reject or truncate.
    let long = db
        .execute(&insert(
            "plain",
            vec!["name", "flag"],
            vec!["123456789".into(), Value::Bool(true)],
        ))
        .await;
    assert!(long.is_err(), "{long:?}");

    // Defaults, generated columns and lossy types are read back.
    let done = db
        .execute(&insert(
            "defaults",
            vec!["name", "price", "tag"],
            vec![
                "ab".into(),
                Value::Decimal("1.005".parse().unwrap()),
                "t ".into(),
            ],
        ))
        .await
        .unwrap();
    let row = &done.returning[0];
    assert_eq!(row.get_as::<String>("state").unwrap(), "new");
    assert_eq!(row.get_as::<String>("shout").unwrap(), "AB");
    assert_eq!(row.get_as::<String>("tag").unwrap(), "t");
    assert_eq!(row, &stored("defaults", row).await);

    // A trigger may rewrite the row.
    let done = db
        .execute(&insert("triggered", vec!["name"], vec!["abc".into()]))
        .await
        .unwrap();
    assert_eq!(done.returning[0].get_as::<String>("name").unwrap(), "ABC");

    // A user without the TRIGGER privilege cannot see triggers in
    // `information_schema`; its rows are read back all the same.
    let user = format!("sid_{}", &t.name[t.name.len() - 12..]);
    for sql in [
        format!("CREATE USER '{user}'@'%' IDENTIFIED BY 'pw'"),
        format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON {}.* TO '{user}'@'%'",
            t.name
        ),
    ] {
        sqlx::query(&sql).execute(&t.admin).await.unwrap();
    }
    let url = ["MYSQL_URL", "DATABASE_URL"]
        .into_iter()
        .filter_map(|var| std::env::var(var).ok())
        .find(|u| u.starts_with("mysql"))
        .unwrap();
    let host = url.rsplit_once('@').unwrap().1.rsplit_once('/').unwrap().0;
    let url = format!("mysql://{user}:pw@{host}/{}", t.name);
    let limited = fixture::database(&url).await;
    let done = limited
        .execute(&insert("triggered", vec!["name"], vec!["low".into()]))
        .await
        .unwrap();
    assert_eq!(done.returning[0].get_as::<String>("name").unwrap(), "LOW");
    sqlx::query(&format!("DROP USER '{user}'@'%'"))
        .execute(&t.admin)
        .await
        .unwrap();

    // Inside a transaction the row is the same, and rolls back with it.
    let inside = db
        .transaction(|tx| async move {
            let done = tx
                .execute(&insert(
                    "triggered",
                    vec!["id", "name"],
                    vec![Value::Int(50), "x".into()],
                ))
                .await?;
            assert_eq!(done.returning[0].get("id"), Some(&Value::Int(50)));
            let team = Team {
                id: 0,
                name: "tx".into(),
            };
            let team = Team::objects(&tx).create(team).await?;
            let saved = Team::objects(&tx).get(Team::id.eq(team.id)).await?;
            assert_eq!(saved, team);
            Err::<(), _>(OrmError::from(siderite_orm::QueryError::InvalidPlan(
                "undo".into(),
            )))
        })
        .await;
    assert!(inside.is_err());
    assert_eq!(Team::objects(db).count().await.unwrap(), 0);
    t.cleanup().await;
}
