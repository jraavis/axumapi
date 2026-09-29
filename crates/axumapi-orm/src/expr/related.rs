//! Fields reached through foreign keys: `Book::author.join(Author::name)`.
//!
//! A [`Joined`] handle behaves like a [`Field`](super::Field) of the *root*
//! model. Using it in a filter or ordering makes the queryset add the needed
//! `LEFT JOIN`s (deduplicated, aliased `author`, `author__team`, ...).

use super::{Expr, Field, Ident, pk_column};
use crate::model::Model;
use crate::plan::SelectExpr;
use crate::relations::ForeignKey;
use std::marker::PhantomData;

/// One foreign-key hop: `<parent>.<fk_column> = <table>.<pk_column>`.
#[derive(Debug, Clone, PartialEq)]
pub struct RelHop {
    /// Foreign-key column on the parent side.
    pub fk_column: Ident,
    /// Table the foreign key points at.
    pub table: Ident,
    /// Primary-key column of that table.
    pub pk_column: Ident,
}

impl RelHop {
    /// Alias of the joined table: the column without its `_id` suffix.
    pub fn alias_part(&self) -> &str {
        self.fk_column
            .strip_suffix("_id")
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.fk_column)
    }
}

/// A column of a related model, addressed by its foreign-key path.
#[derive(Debug, Clone, PartialEq)]
pub struct RelatedColumn {
    /// Hops from the root model to the model owning `column`.
    pub path: Vec<RelHop>,
    /// Column name on the last model of the path.
    pub column: Ident,
}

impl RelatedColumn {
    /// Alias of the join table holding the column (`author__team`).
    pub fn alias(&self) -> String {
        path_alias_of(&self.path)
    }

    /// Result-column name: `author__name`.
    pub fn output_name(&self) -> String {
        format!("{}__{}", self.alias(), self.column)
    }
}

/// Alias of the table reached by `path`.
pub fn path_alias_of(path: &[RelHop]) -> String {
    path.iter()
        .map(RelHop::alias_part)
        .collect::<Vec<_>>()
        .join("__")
}

/// A foreign-key slot of a model struct: `ForeignKey<T>` or `Option<ForeignKey<T>>`.
pub trait FkSlot<T: Model> {
    /// The foreign key, if set.
    fn fk(&mut self) -> Option<&mut ForeignKey<T>>;
}

impl<T: Model> FkSlot<T> for ForeignKey<T> {
    fn fk(&mut self) -> Option<&mut ForeignKey<T>> {
        Some(self)
    }
}

impl<T: Model> FkSlot<T> for Option<ForeignKey<T>> {
    fn fk(&mut self) -> Option<&mut ForeignKey<T>> {
        self.as_mut()
    }
}

/// Column of related model `T` that can be reached through more hops:
/// a [`Field`] of `T`, or a [`Joined`] handle already rooted at `T`.
pub trait Related<T, X> {
    /// Hops (relative to `T`) and the column at their end.
    fn into_parts(self) -> (Vec<RelHop>, &'static str);
}

impl<T, X> Related<T, X> for Field<T, X> {
    fn into_parts(self) -> (Vec<RelHop>, &'static str) {
        (Vec::new(), self.name())
    }
}

impl<T, X> Related<T, X> for Joined<T, X> {
    fn into_parts(self) -> (Vec<RelHop>, &'static str) {
        (self.path, self.column)
    }
}

/// A column of a related model, usable like a field of the root model `M`.
pub struct Joined<M, T> {
    path: Vec<RelHop>,
    column: &'static str,
    _marker: PhantomData<fn() -> (M, T)>,
}

impl<M, T> Clone for Joined<M, T> {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            column: self.column,
            _marker: PhantomData,
        }
    }
}

impl<M, T> std::fmt::Debug for Joined<M, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Joined")
            .field("path", &self.path)
            .field("column", &self.column)
            .finish()
    }
}

impl<M, T> Joined<M, T> {
    fn related_column(self) -> RelatedColumn {
        RelatedColumn {
            path: self.path,
            column: self.column.into(),
        }
    }

    /// Project this column in `values(..)` as `author__name`.
    pub fn select(self) -> SelectExpr {
        let related = self.related_column();
        SelectExpr::new(
            Expr::Related(related.clone()),
            Some(related.output_name().into()),
        )
    }
}

impl<M, T> From<Joined<M, T>> for Expr {
    fn from(j: Joined<M, T>) -> Self {
        Expr::Related(j.related_column())
    }
}

impl<M, T> From<Joined<M, T>> for SelectExpr {
    fn from(j: Joined<M, T>) -> Self {
        j.select()
    }
}

impl<M, S> Field<M, S> {
    /// Reach `inner`, a field (or further joined field) of the model this
    /// foreign key points at: `Book::author.join(Author::name)`.
    pub fn join<T: Model, X>(self, inner: impl Related<T, X>) -> Joined<M, X>
    where
        S: FkSlot<T>,
    {
        let (mut path, column) = inner.into_parts();
        path.insert(
            0,
            RelHop {
                fk_column: self.name().into(),
                table: T::META.table.into(),
                pk_column: pk_column::<T>(),
            },
        );
        Joined {
            path,
            column,
            _marker: PhantomData,
        }
    }
}
