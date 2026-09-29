//! [`WritePlan`]: backend-neutral INSERT / UPDATE / DELETE.
//!
//! Like [`QueryPlan`](crate::QueryPlan), a write plan is pure data. Backends
//! check it against their capabilities and compile it; values are always
//! bound parameters.

use crate::capabilities::{BackendCapabilities, Feature};
use crate::error::BackendCapabilityError;
use crate::expr::{Expr, Ident};
use crate::value::Value;

/// Multi-row insert.
#[derive(Debug, Clone, PartialEq)]
pub struct InsertPlan {
    /// Target table.
    pub table: Ident,
    /// Inserted columns (auto columns are omitted by the caller).
    pub columns: Vec<Ident>,
    /// One `Vec` per row, each with `columns.len()` values.
    pub rows: Vec<Vec<Value>>,
    /// Columns to return (typically the generated primary key).
    pub returning: Vec<Ident>,
}

/// `UPDATE table SET .. WHERE ..`.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdatePlan {
    /// Target table.
    pub table: Ident,
    /// `column = expression` pairs (expressions allow `F`-style updates).
    pub assignments: Vec<(Ident, Expr)>,
    /// Row filter; `None` updates every row.
    pub filter: Option<Expr>,
    /// Columns to return.
    pub returning: Vec<Ident>,
}

/// `DELETE FROM table WHERE ..`.
#[derive(Debug, Clone, PartialEq)]
pub struct DeletePlan {
    /// Target table.
    pub table: Ident,
    /// Row filter; `None` deletes every row.
    pub filter: Option<Expr>,
    /// Columns to return.
    pub returning: Vec<Ident>,
}

/// A data-modifying statement.
#[derive(Debug, Clone, PartialEq)]
pub enum WritePlan {
    /// INSERT.
    Insert(InsertPlan),
    /// UPDATE.
    Update(UpdatePlan),
    /// DELETE.
    Delete(DeletePlan),
}

impl WritePlan {
    /// Columns requested back from the statement.
    pub fn returning(&self) -> &[Ident] {
        match self {
            WritePlan::Insert(p) => &p.returning,
            WritePlan::Update(p) => &p.returning,
            WritePlan::Delete(p) => &p.returning,
        }
    }

    /// Features this plan needs from a backend (deduplicated).
    pub fn required_features(&self) -> Vec<Feature> {
        let mut out = Vec::new();
        if !self.returning().is_empty() {
            out.push(Feature::Returning);
        }
        let exprs: Vec<&Expr> = match self {
            WritePlan::Insert(_) => Vec::new(),
            WritePlan::Update(p) => p
                .assignments
                .iter()
                .map(|(_, e)| e)
                .chain(p.filter.iter())
                .collect(),
            WritePlan::Delete(p) => p.filter.iter().collect(),
        };
        for feature in exprs.into_iter().flat_map(Expr::required_features) {
            if !out.contains(&feature) {
                out.push(feature);
            }
        }
        out
    }

    /// Check the plan against backend capabilities.
    ///
    /// # Errors
    /// The first unsupported feature.
    pub fn check(&self, caps: &BackendCapabilities) -> Result<(), BackendCapabilityError> {
        self.required_features()
            .into_iter()
            .try_for_each(|f| caps.require(f))
    }
}
