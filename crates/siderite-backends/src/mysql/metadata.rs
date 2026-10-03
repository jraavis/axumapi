//! Cached MySQL keys, column storage and trigger visibility.

use super::io::MySqlIo;
use siderite_orm::{DbType, OrmError, QueryError, Row, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

/// Keys, columns and triggers of a table.
#[derive(Debug)]
pub(super) struct TableInfo {
    pub(super) table: String,
    pub(super) primary_key: Vec<String>,
    pub(super) auto_increment: Option<String>,
    pub(super) columns: Vec<ColumnInfo>,
    /// Whether an `INSERT` trigger exists (it may change the stored row), or
    /// the user cannot see the table's triggers.
    pub(super) insert_triggers: bool,
}

/// What [`super::returning::synthesize`] needs to know about a column.
#[derive(Debug)]
pub(super) struct ColumnInfo {
    pub(super) name: String,
    /// `DATA_TYPE`, e.g. `bigint`, `varchar`.
    data_type: String,
    /// `COLUMN_TYPE`, e.g. `tinyint(1)`, `int unsigned`.
    column_type: String,
    /// `CHARACTER_MAXIMUM_LENGTH` of a string column.
    max_length: Option<u64>,
    utf8mb4: bool,
    nullable: bool,
}

impl ColumnInfo {
    /// Range of a signed integer column that is not `TINYINT(1)` (which reads
    /// back as a boolean).
    fn integer_range(&self) -> Option<(i64, i64)> {
        let unsigned = self.column_type.contains("unsigned");
        let boolean = self.column_type.starts_with("tinyint(1)");
        if unsigned || boolean {
            return None;
        }
        match self.data_type.as_str() {
            "tinyint" => Some((i64::from(i8::MIN), i64::from(i8::MAX))),
            "smallint" => Some((i64::from(i16::MIN), i64::from(i16::MAX))),
            "mediumint" => Some((-(1 << 23), (1 << 23) - 1)),
            "int" => Some((i64::from(i32::MIN), i64::from(i32::MAX))),
            "bigint" => Some((i64::MIN, i64::MAX)),
            _ => None,
        }
    }

    /// Whether `text` is stored as it is: no truncation, padding or character
    /// set conversion.
    fn stores_text(&self, text: &str) -> bool {
        let Some(max) = self.max_length else {
            return false;
        };
        let fits = match self.data_type.as_str() {
            // Counted in characters.
            "varchar" => text.chars().count() as u64 <= max,
            // Counted in bytes.
            "text" | "mediumtext" | "longtext" => text.len() as u64 <= max,
            _ => false,
        };
        fits && (self.utf8mb4 || text.is_ascii())
    }

    /// Whether reading the column after writing `value` gives `value` back.
    pub(super) fn stores_exactly(&self, value: &Value) -> bool {
        match value {
            Value::Null => {
                self.nullable
                    && (self.integer_range().is_some()
                        || self.column_type == "tinyint(1)"
                        || self.stores_text(""))
            }
            Value::Bool(_) => self.column_type == "tinyint(1)",
            Value::Int(v) => self
                .integer_range()
                .is_some_and(|(min, max)| (min..=max).contains(v)),
            Value::Text(text) => self.stores_text(text),
            _ => false,
        }
    }
}

/// Per-backend cache of [`TableInfo`].
#[derive(Debug, Default)]
pub(super) struct TableCache {
    tables: Mutex<HashMap<String, Arc<TableInfo>>>,
}

impl TableCache {
    pub(super) fn clear(&self) {
        self.tables
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    pub(super) async fn table<C: MySqlIo>(
        &self,
        ex: &mut C,
        name: &str,
    ) -> Result<Arc<TableInfo>, OrmError> {
        let cached = self
            .tables
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .cloned();
        if let Some(info) = cached {
            return Ok(info);
        }
        let rows = ex
            .fetch(
                "SELECT CAST(COLUMN_NAME AS CHAR) AS c,
             CAST(COLUMN_KEY AS CHAR) AS k,
             CAST(EXTRA AS CHAR) AS e, CAST(DATA_TYPE AS CHAR) AS d,
             CAST(COLUMN_TYPE AS CHAR) AS t,
             CAST(CHARACTER_MAXIMUM_LENGTH AS UNSIGNED) AS m,
             CAST(CHARACTER_SET_NAME AS CHAR) AS s,
             CAST(IS_NULLABLE AS CHAR) AS n,
             (SELECT COUNT(*) FROM information_schema.TRIGGERS
              WHERE EVENT_OBJECT_SCHEMA = DATABASE()
              AND EVENT_OBJECT_TABLE = ?
              AND EVENT_MANIPULATION = 'INSERT') AS g,
             (SELECT COUNT(*) FROM information_schema.USER_PRIVILEGES p
              WHERE p.PRIVILEGE_TYPE = 'TRIGGER' AND p.GRANTEE = me.grantee) +
             (SELECT COUNT(*) FROM information_schema.SCHEMA_PRIVILEGES p
              WHERE p.PRIVILEGE_TYPE = 'TRIGGER' AND p.GRANTEE = me.grantee
              AND DATABASE() LIKE p.TABLE_SCHEMA) +
             (SELECT COUNT(*) FROM information_schema.TABLE_PRIVILEGES p
              WHERE p.PRIVILEGE_TYPE = 'TRIGGER' AND p.GRANTEE = me.grantee
              AND p.TABLE_SCHEMA = DATABASE() AND p.TABLE_NAME = ?) AS v
             FROM information_schema.COLUMNS,
             (SELECT CONCAT('''',
              SUBSTRING_INDEX(CURRENT_USER(), '@', 1), '''@''',
              SUBSTRING_INDEX(CURRENT_USER(), '@', -1), '''') AS grantee) me
             WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ?
             ORDER BY ORDINAL_POSITION",
                vec![Value::Text(name.to_owned()); 3],
            )
            .await?
            .rows;
        if rows.is_empty() {
            let message = format!("table `{name}` does not exist");
            return Err(QueryError::Model(message).into());
        }
        let mut info = TableInfo {
            table: name.to_owned(),
            primary_key: Vec::new(),
            auto_increment: None,
            columns: Vec::with_capacity(rows.len()),
            insert_triggers: false,
        };
        for row in &rows {
            let column: String = get(row, "c")?;
            let key: String = get(row, "k")?;
            let extra: String = get(row, "e")?;
            let data_type: String = get(row, "d")?;
            let column_type: String = get(row, "t")?;
            let charset: Option<String> = get(row, "s")?;
            let nullable: String = get(row, "n")?;
            // `information_schema.TRIGGERS` only lists the triggers of tables
            // the user has the `TRIGGER` privilege on. Without it (or with it
            // only through a role), assume there is one.
            let visible = get::<i64>(row, "v")? > 0;
            info.insert_triggers = !visible || get::<i64>(row, "g")? > 0;
            info.columns.push(ColumnInfo {
                name: column.clone(),
                data_type: data_type.to_ascii_lowercase(),
                column_type: column_type.to_ascii_lowercase(),
                max_length: unsigned(row, "m")?,
                utf8mb4: charset.as_deref().is_some_and(is_utf8mb4),
                nullable: nullable.eq_ignore_ascii_case("YES"),
            });
            if extra.to_ascii_lowercase().contains("auto_increment") {
                info.auto_increment = Some(column.clone());
            }
            if key == "PRI" {
                info.primary_key.push(column);
            }
        }
        if info.primary_key.is_empty() {
            let message = format!(
                concat!(
                    "table `{name}` has no primary key, so MySQL cannot ",
                    "return the rows it writes",
                ),
                name = name,
            );
            return Err(QueryError::Model(message).into());
        }
        let info = Arc::new(info);
        self.tables
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.to_owned(), Arc::clone(&info));
        Ok(info)
    }
}

/// Decode a named metadata or generated-key result column.
pub(super) fn get<T: DbType>(row: &Row, name: &str) -> Result<T, OrmError> {
    let value = row.get(name).cloned().ok_or_else(|| QueryError::Decode {
        column: name.into(),
        reason: "missing result column".into(),
    })?;
    siderite_orm::types::decode(name, value).map_err(Into::into)
}

fn unsigned(row: &Row, name: &str) -> Result<Option<u64>, OrmError> {
    let value: Option<i64> = get(row, name)?;
    value.map(u64::try_from).transpose().map_err(|error| {
        QueryError::Decode {
            column: name.into(),
            reason: error.to_string(),
        }
        .into()
    })
}

fn is_utf8mb4(charset: &str) -> bool {
    charset.eq_ignore_ascii_case("utf8mb4")
}
