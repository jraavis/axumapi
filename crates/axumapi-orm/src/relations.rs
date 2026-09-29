//! Relation field types.
//!
//! * [`ForeignKey<T>`]: a column holding `T`'s primary key, plus an optional
//!   cached `T` filled by `select_related` / `prefetch_related` or a previous
//!   fetch. `Option<ForeignKey<T>>` is a nullable FK.
//! * [`OneToOne<T>`]: the same storage; the derive marks the column unique.
//! * Many-to-many relations are declared on the model
//!   (`#[model(many_to_many(tags(Tag)))]`), not as struct fields, because they
//!   have no column and no serialized form.

use crate::model::Model;
use crate::types::{DbType, SqlType};
use crate::value::Value;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::Arc;

/// A many-to-one reference to `T` stored as `T`'s primary key.
///
/// Serializes as the bare key (`"author": 7`), like Django REST framework's
/// default. The cached object is never serialized.
pub struct ForeignKey<T: Model> {
    id: T::Pk,
    cached: Option<Arc<T>>,
}

/// A one-to-one reference: a unique [`ForeignKey`].
pub type OneToOne<T> = ForeignKey<T>;

impl<T: Model> ForeignKey<T> {
    /// Reference by primary key (nothing is loaded).
    pub fn new(id: T::Pk) -> Self {
        Self { id, cached: None }
    }

    /// Reference an already loaded object; it becomes the cached value.
    pub fn to(object: T) -> Self {
        Self {
            id: object.pk(),
            cached: Some(Arc::new(object)),
        }
    }

    /// The referenced primary key.
    pub fn id(&self) -> &T::Pk {
        &self.id
    }

    /// The cached object, if it was loaded.
    pub fn cached(&self) -> Option<&T> {
        self.cached.as_deref()
    }

    /// Replace the cached object. Ignored unless its key matches [`id`](Self::id),
    /// so a stale object is never attached.
    pub fn set_cached(&mut self, object: Arc<T>) {
        if object.pk() == self.id {
            self.cached = Some(object);
        }
    }
}

impl<T: Model> Clone for ForeignKey<T> {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            cached: self.cached.clone(),
        }
    }
}

impl<T: Model> std::fmt::Debug for ForeignKey<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForeignKey")
            .field("model", &T::META.name)
            .field("id", &self.id)
            .field("loaded", &self.cached.is_some())
            .finish()
    }
}

/// Equality compares keys only.
impl<T: Model> PartialEq for ForeignKey<T> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl<T: Model> DbType for ForeignKey<T> {
    const SQL_TYPE: SqlType = <T::Pk as DbType>::SQL_TYPE;
    fn to_value(&self) -> Value {
        self.id.to_value()
    }
    fn from_value(value: Value) -> Result<Self, String> {
        T::Pk::from_value(value).map(Self::new)
    }
}

impl<T: Model> Serialize for ForeignKey<T>
where
    T::Pk: Serialize,
{
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.id.serialize(serializer)
    }
}

impl<'de, T: Model> Deserialize<'de> for ForeignKey<T>
where
    T::Pk: Deserialize<'de>,
{
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        T::Pk::deserialize(deserializer).map(Self::new)
    }
}
