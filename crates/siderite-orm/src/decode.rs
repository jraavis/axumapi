//! Typed decoding of dynamic result rows: [`FromValues`] for `values_list`,
//! [`Row::get_as`] and [`QueryResult::decode`] for raw SQL.

use crate::backend::{QueryResult, Row};
use crate::error::QueryError;
use crate::model::Model;
use crate::types::{DbType, decode};
use crate::value::Value;

/// A type decoded from the columns of a row by position: a single
/// [`DbType`] for one column, or a tuple of up to six.
///
/// ```ignore
/// let rows: Vec<(String, i64)> = Book::objects(&db)
///     .values_list([Book::title.select(), Book::likes.select()])
///     .await?;
/// ```
pub trait FromValues: Sized {
    /// Decode `row`.
    ///
    /// # Errors
    /// [`QueryError::Decode`] if a column is missing or has the wrong type.
    fn from_values(row: &Row) -> Result<Self, QueryError>;
}

impl<T: DbType> FromValues for T {
    fn from_values(row: &Row) -> Result<Self, QueryError> {
        row.decode_at(0)
    }
}

macro_rules! tuple_from_values {
    ($($name:ident : $index:tt),+) => {
        impl<$($name: DbType),+> FromValues for ($($name,)+) {
            fn from_values(row: &Row) -> Result<Self, QueryError> {
                Ok(($(row.decode_at::<$name>($index)?,)+))
            }
        }
    };
}
tuple_from_values!(A: 0);
tuple_from_values!(A: 0, B: 1);
tuple_from_values!(A: 0, B: 1, C: 2);
tuple_from_values!(A: 0, B: 1, C: 2, D: 3);
tuple_from_values!(A: 0, B: 1, C: 2, D: 3, E: 4);
tuple_from_values!(A: 0, B: 1, C: 2, D: 3, E: 4, F: 5);

impl Row {
    /// Value of `column`, decoded as `T`.
    ///
    /// # Errors
    /// [`QueryError::Decode`] if the column is missing or has the wrong type.
    pub fn get_as<T: DbType>(&self, column: &str) -> Result<T, QueryError> {
        let value = self
            .get(column)
            .cloned()
            .ok_or_else(|| QueryError::Decode {
                column: column.to_owned(),
                reason: "column missing from result".into(),
            })?;
        decode(column, value)
    }

    /// The `index`-th column, decoded as `T`.
    ///
    /// # Errors
    /// [`QueryError::Decode`] if there is no such column or it has the wrong type.
    pub fn decode_at<T: DbType>(&self, index: usize) -> Result<T, QueryError> {
        let (name, value) = self.iter().nth(index).ok_or_else(|| QueryError::Decode {
            column: format!("#{index}"),
            reason: format!("row has only {} columns", self.len()),
        })?;
        decode(name, value.clone())
    }
}

impl QueryResult {
    /// Decode every row as model `M` (columns looked up by name).
    ///
    /// # Errors
    /// [`QueryError::Decode`] for the first row that does not fit.
    pub fn decode<M: Model>(&self) -> Result<Vec<M>, QueryError> {
        self.rows.iter().map(|row| M::from_row(row, "")).collect()
    }

    /// Decode every row by column position as `T` (a scalar or a tuple).
    ///
    /// # Errors
    /// [`QueryError::Decode`] for the first row that does not fit.
    pub fn decode_values<T: FromValues>(&self) -> Result<Vec<T>, QueryError> {
        self.rows.iter().map(T::from_values).collect()
    }

    /// The first column of the first row, if any.
    pub fn scalar(&self) -> Option<&Value> {
        self.rows.first()?.iter().next().map(|(_, v)| v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> Row {
        Row::new(vec![
            ("name".into(), Value::Text("ann".into())),
            ("age".into(), Value::Int(30)),
            ("nick".into(), Value::Null),
        ])
    }

    #[test]
    fn tuples_decode_by_position() {
        let (name, age, nick) =
            <(String, i64, Option<String>)>::from_values(&row()).unwrap_or_default();
        assert_eq!((name.as_str(), age, nick), ("ann", 30, None));
        assert_eq!(String::from_values(&row()), Ok("ann".to_owned()));
    }

    #[test]
    fn errors_name_the_offending_column() {
        let err = <(i64,)>::from_values(&row());
        assert!(matches!(err, Err(QueryError::Decode { column, .. }) if column == "name"));
        let short = <(String, i64, Option<String>, i64)>::from_values(&row());
        assert!(matches!(short, Err(QueryError::Decode { column, .. }) if column == "#3"));
    }

    #[test]
    fn rows_decode_by_name_and_decimals_fit_integers() {
        assert_eq!(row().get_as::<i64>("age"), Ok(30));
        assert!(row().get_as::<i64>("missing").is_err());
        let sum = Row::new(vec![(
            "s".into(),
            Value::Decimal(rust_decimal::Decimal::new(27, 0)),
        )]);
        assert_eq!(sum.get_as::<i64>("s"), Ok(27));
        assert_eq!(sum.get_as::<f64>("s"), Ok(27.0));
        let fractional = Row::new(vec![(
            "s".into(),
            Value::Decimal(rust_decimal::Decimal::new(275, 1)),
        )]);
        assert!(fractional.get_as::<i64>("s").is_err());
    }
}
