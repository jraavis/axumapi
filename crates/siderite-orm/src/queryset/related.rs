//! Loading related objects: [`Relation`] descriptors, `select_related`
//! (one query with joins) and `prefetch_related` (one extra query per
//! relation).
//!
//! A [`Relation`] names a foreign key *and* how to reach its slot in the
//! model struct, because generic code cannot otherwise get at
//! `book.author`:
//!
//! ```ignore
//! let relation = Relation::new(Book::author, |book| &mut book.author);
//! let books = Book::objects(&db).select_related(relation).all().await?;
//! books[0].author.cached(); // Some(&Author)
//! ```
//!
//! Chain relations with [`Relation::then`] for multi-hop `select_related`
//! (`author` then `author.team`).

use super::QuerySet;
use crate::backend::Row;
use crate::db::Db;
use crate::error::{OrmError, QueryError};
use crate::expr::{Expr, Field, FkSlot, Ident, RelHop, RelatedColumn, path_alias_of, pk_column};
use crate::model::{FieldMeta, Model};
use crate::persist::value_key;
use crate::plan::SelectExpr;
use crate::types::DbType;
use crate::value::Value;
use async_trait::async_trait;
use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::sync::Arc;

type Loaded = Box<dyn Any + Send + Sync>;
type Load = fn(&Row, &str) -> Result<Loaded, QueryError>;
type Attach = Arc<dyn Fn(&mut dyn Any, Arc<dyn Any + Send + Sync>) + Send + Sync>;

fn load<T: Model>(row: &Row, prefix: &str) -> Result<Loaded, QueryError> {
    Ok(Box::new(T::from_row(row, prefix)?))
}

/// One foreign-key hop with the type-erased operations to load and attach it.
#[derive(Clone)]
struct Step {
    hop: RelHop,
    /// Columns of the target model (projected by `select_related`).
    fields: &'static [FieldMeta],
    load: Load,
    attach: Attach,
}

/// A foreign key of model `M` pointing at `T`, together with access to its
/// slot in `M`, so loaders can store the related object in the struct:
///
/// ```ignore
/// let relation = Relation::new(Book::author, |book| &mut book.author);
/// let books = Book::objects(&db).select_related(relation).all().await?;
/// books[0].author.cached(); // Some(&Author)
/// ```
///
/// Chain relations with [`then`](Self::then) for multi-hop `select_related`.
pub struct Relation<M, T> {
    steps: Vec<Step>,
    _marker: PhantomData<fn() -> (M, T)>,
}

impl<M, T> Clone for Relation<M, T> {
    fn clone(&self) -> Self {
        Self {
            steps: self.steps.clone(),
            _marker: PhantomData,
        }
    }
}

impl<M: Model, T: Model> Relation<M, T> {
    /// Relation through `field`; `slot` returns the struct field holding it:
    /// `Relation::new(Book::author, |b| &mut b.author)`. Works for
    /// `ForeignKey<T>` and `Option<ForeignKey<T>>` slots.
    pub fn new<S: FkSlot<T> + 'static>(field: Field<M, S>, slot: fn(&mut M) -> &mut S) -> Self {
        let attach: Attach = Arc::new(move |parent, child| {
            if let (Some(parent), Ok(child)) = (parent.downcast_mut::<M>(), child.downcast::<T>())
                && let Some(fk) = slot(parent).fk()
            {
                fk.set_cached(child);
            }
        });
        Self {
            steps: vec![Step {
                hop: RelHop {
                    fk_column: field.name().into(),
                    table: T::META.table.into(),
                    pk_column: pk_column::<T>(),
                },
                fields: T::META.fields,
                load: load::<T>,
                attach,
            }],
            _marker: PhantomData,
        }
    }

    /// Continue through a relation of `T`: `author.then(team)` reaches
    /// `book.author.team`.
    pub fn then<U: Model>(mut self, next: Relation<T, U>) -> Relation<M, U> {
        self.steps.extend(next.steps);
        Relation {
            steps: self.steps,
            _marker: PhantomData,
        }
    }
}

/// A node of the `select_related` tree: one joined model and what hangs off it.
#[derive(Clone)]
pub(super) struct RelNode {
    step: Step,
    alias: String,
    children: Vec<RelNode>,
}

impl RelNode {
    /// Decode this node's object from `row` (with its own children attached),
    /// or `None` when the `LEFT JOIN` found no row.
    fn decode(&self, row: &Row) -> Result<Option<Loaded>, QueryError> {
        let prefix = format!("{}__", self.alias);
        let key = format!("{prefix}{}", self.step.hop.pk_column);
        if row.get(&key).is_none_or(Value::is_null) {
            return Ok(None);
        }
        let mut object = (self.step.load)(row, &prefix)?;
        attach_all(&mut *object, &self.children, row)?;
        Ok(Some(object))
    }
}

/// Decode every node under `parent` and store the results in its slots.
pub(super) fn attach_all(
    parent: &mut dyn Any,
    nodes: &[RelNode],
    row: &Row,
) -> Result<(), QueryError> {
    for node in nodes {
        if let Some(object) = node.decode(row)? {
            (node.step.attach)(parent, Arc::from(object));
        }
    }
    Ok(())
}

/// Insert `steps` into the tree, sharing nodes with earlier paths.
fn insert_path(nodes: &mut Vec<RelNode>, steps: &[Step], path: &mut Vec<RelHop>) {
    let Some((step, rest)) = steps.split_first() else {
        return;
    };
    path.push(step.hop.clone());
    let position = match nodes.iter().position(|n| n.step.hop == step.hop) {
        Some(position) => position,
        None => {
            nodes.push(RelNode {
                step: step.clone(),
                alias: path_alias_of(path),
                children: Vec::new(),
            });
            nodes.len() - 1
        }
    };
    insert_path(&mut nodes[position].children, rest, path);
    path.pop();
}

/// Projection of every column of the joined models in `nodes`, named
/// `alias__column`, with the path each column is reached through.
fn related_projection(nodes: &[RelNode], path: &mut Vec<RelHop>, out: &mut Vec<SelectExpr>) {
    for node in nodes {
        path.push(node.step.hop.clone());
        for field in node.step.fields {
            let column = RelatedColumn {
                path: path.clone(),
                column: field.column.into(),
            };
            let alias: Ident = column.output_name().into();
            out.push(SelectExpr::new(Expr::Related(column), Some(alias)));
        }
        related_projection(&node.children, path, out);
        path.pop();
    }
}

impl<M: Model> QuerySet<M> {
    /// Load `relation`'s target in the same query (`LEFT JOIN`), so
    /// `ForeignKey::cached()` is filled without further queries. Repeated
    /// calls share joins of a common path (`author`, then `author` → `team`).
    #[must_use]
    pub fn select_related<T: Model>(mut self, relation: Relation<M, T>) -> Self {
        let mut earlier = Vec::new();
        related_projection(&self.related, &mut Vec::new(), &mut earlier);
        insert_path(&mut self.related, &relation.steps, &mut Vec::new());
        let mut columns = Vec::new();
        related_projection(&self.related, &mut Vec::new(), &mut columns);
        self.with_plan(|mut plan| {
            // Drop the columns of earlier calls; they are re-added below.
            plan.projection
                .retain(|s| !earlier.iter().any(|e| e.alias == s.alias));
            if plan.projection.is_empty() {
                plan.projection = super::model_projection::<M>();
            }
            plan.projection.extend(columns);
            plan
        })
    }

    /// Load `relation`'s targets with one extra query (`WHERE pk IN (..)`)
    /// after the main query, instead of one query per row. Only single-hop
    /// relations can be prefetched; use [`select_related`](Self::select_related)
    /// for longer paths. Reverse foreign keys and many-to-many relations are
    /// not prefetched (the model has no slot to hold them).
    #[must_use]
    pub fn prefetch_related<T: Model>(mut self, prefetch: impl Into<Prefetch<M, T>>) -> Self {
        let prefetch = prefetch.into();
        if prefetch.relation.steps.len() != 1 {
            return self.fail("prefetch_related supports single-hop relations only");
        }
        self.prefetches.push(Arc::new(prefetch));
        self
    }

    /// Run the prefetch queries for `models`.
    pub(super) async fn run_prefetches(&self, models: &mut [M]) -> Result<(), OrmError> {
        for prefetch in &self.prefetches {
            prefetch.run(&self.db, models).await?;
        }
        Ok(())
    }
}

/// `prefetch_related` request: a relation and, optionally, the queryset used
/// to load the targets (to filter them, order them or use another handle).
pub struct Prefetch<M: Model, T: Model> {
    relation: Relation<M, T>,
    queryset: Option<QuerySet<T>>,
}

impl<M: Model, T: Model> Prefetch<M, T> {
    /// Prefetch `relation` with a plain queryset over the target.
    pub fn new(relation: Relation<M, T>) -> Self {
        Self {
            relation,
            queryset: None,
        }
    }

    /// Load targets through `queryset`. Its filters apply; targets it does
    /// not return stay unloaded. Its database capabilities and compiled
    /// existing bind count govern prefetch batching. SQL backends must
    /// implement Backend::read_parameter_count.
    #[must_use]
    pub fn queryset(mut self, queryset: QuerySet<T>) -> Self {
        self.queryset = Some(queryset);
        self
    }
}

impl<M: Model, T: Model> From<Relation<M, T>> for Prefetch<M, T> {
    fn from(relation: Relation<M, T>) -> Self {
        Self::new(relation)
    }
}

/// Type-erased prefetch, stored inside the queryset.
#[async_trait]
pub(super) trait Prefetcher<M: Model>: Send + Sync {
    async fn run(&self, db: &Db, models: &mut [M]) -> Result<(), OrmError>;
}

#[async_trait]
impl<M: Model, T: Model> Prefetcher<M> for Prefetch<M, T> {
    async fn run(&self, db: &Db, models: &mut [M]) -> Result<(), OrmError> {
        let Some(step) = self.relation.steps.first() else {
            return Ok(());
        };
        let column = step.hop.fk_column.as_ref();
        let mut seen = HashSet::new();
        let mut wanted = Vec::new();
        let keys: Vec<Option<String>> = models
            .iter()
            .map(|model| {
                let value = model
                    .to_values()
                    .into_iter()
                    .find(|(c, _)| *c == column)
                    .map(|(_, v)| v)
                    .filter(|v| !v.is_null())?;
                let key = value_key(&value);
                if seen.insert(key.clone()) {
                    wanted.push(value);
                }
                Some(key)
            })
            .collect();
        if wanted.is_empty() {
            return Ok(());
        }
        let base = self.queryset.clone().unwrap_or_else(|| T::objects(db));
        base.ready()?;
        if base.empty {
            return Ok(());
        }
        let caps = base.db.capabilities();
        let used = match base.db.read_parameter_count(base.plan())? {
            Some(count) => count,
            None if matches!(caps.kind, crate::BackendKind::MongoDb) => 0,
            None => {
                return Err(QueryError::InvalidPlan(
                    "prefetch requires compiled parameter counting".into(),
                )
                .into());
            }
        };
        let chunk = caps
            .max_params
            .checked_sub(used)
            .filter(|remaining| *remaining > 0)
            .ok_or_else(|| QueryError::InvalidPlan("prefetch bind capacity exhausted".into()))?;
        if wanted.len() > chunk && (base.plan.limit.is_some() || base.plan.offset.is_some()) {
            return Err(
                QueryError::InvalidPlan("sliced prefetch cannot span bind batches".into()).into(),
            );
        }
        let mut loaded: HashMap<String, Arc<T>> = HashMap::new();
        for wanted in wanted.chunks(chunk) {
            let pk = Expr::col(step.hop.pk_column.clone());
            let predicate = pk.is_in(wanted.iter().cloned());
            for target in base.clone().filter(predicate).all().await? {
                loaded.insert(value_key(&target.pk().to_value()), Arc::new(target));
            }
        }
        for (model, key) in models.iter_mut().zip(&keys) {
            let target = key.as_ref().and_then(|key| loaded.get(key));
            if let Some(target) = target {
                (step.attach)(model, Arc::<T>::clone(target));
            }
        }
        Ok(())
    }
}
