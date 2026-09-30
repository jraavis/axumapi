//! Process-local background tasks run after the response, and a durable
//! queue trait (adapters come later).
//!
//! # Process-local tasks
//! [`BackgroundTasks`] collects futures during a request. When the extractor
//! is dropped (after the handler returns) they run **sequentially** in a
//! single spawned tokio task.
//!
//! These tasks are **not durable**: they live only in this process and are
//! lost on crash or shutdown. A panic in one task is caught (via
//! [`futures_util::FutureExt::catch_unwind`]) and logged with `tracing`;
//! later tasks still run.
//!
//! # Durable queues
//! [`TaskQueue`] is the interface for a durable backend. Redis / RabbitMQ
//! adapters are not provided in this phase.

use crate::error::ApiError;
use crate::extract::FromRequestParts;
use futures_util::FutureExt;
use http::request::Parts;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use thiserror::Error;
use uuid::Uuid;

type LocalTask = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Extractor that schedules process-local work after the handler returns.
///
/// See the [module docs](self) for durability and panic behaviour.
#[derive(Default)]
pub struct BackgroundTasks {
    tasks: Vec<LocalTask>,
}

impl BackgroundTasks {
    /// Queue `fut` to run after the response is produced.
    pub fn add<F>(&mut self, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.tasks.push(Box::pin(fut));
    }

    /// Queue the future returned by `f`.
    pub fn add_fn<F, Fut>(&mut self, f: F)
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.add(f());
    }
}

impl Drop for BackgroundTasks {
    fn drop(&mut self) {
        let tasks = std::mem::take(&mut self.tasks);
        if tasks.is_empty() {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::error!("no tokio runtime; dropping background tasks");
            return;
        };
        handle.spawn(async move {
            for task in tasks {
                if let Err(panic) = AssertUnwindSafe(task).catch_unwind().await {
                    tracing::error!(error = %panic_message(&panic), "background task panicked");
                }
            }
        });
    }
}

impl FromRequestParts for BackgroundTasks {
    async fn from_request_parts(_parts: &mut Parts) -> Result<Self, ApiError> {
        Ok(Self::default())
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "Box<dyn Any>".to_owned()
    }
}

/// Identifier of an enqueued durable task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TaskId(pub Uuid);

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// A serializable unit of work for a [`TaskQueue`].
///
/// Redis / RabbitMQ adapters that persist and execute these types are added
/// in a later phase.
pub trait Task: Serialize + DeserializeOwned + Send + 'static {
    /// Stable task type name stored alongside the payload.
    const NAME: &'static str;
}

/// Failure to enqueue a durable task.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TaskQueueError {
    /// The backend rejected or failed to persist the task.
    #[error("failed to enqueue task: {0}")]
    Enqueue(String),
    /// The backend is unavailable.
    #[error("task queue unavailable: {0}")]
    Unavailable(String),
}

/// Durable task queue. Implementations (Redis, RabbitMQ, …) ship later.
pub trait TaskQueue: Send + Sync {
    /// Persist `task` and return its identifier.
    fn enqueue<T: Task>(
        &self,
        task: T,
    ) -> impl Future<Output = Result<TaskId, TaskQueueError>> + Send;
}
