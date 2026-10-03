//! Bounded process-local tasks authorized by successful endpoint responses.
//!
//! Tasks run sequentially within a request batch after the endpoint produces
//! a 2xx response. Failed extraction, errors, cancellation and panic discard
//! queued work. This boundary does not guarantee streaming-body delivery or
//! client receipt; middleware may subsequently replace the response.
//! Accepted work is tracked by the application's lifespan and drained before
//! resource teardown within its shutdown deadline. Task panic is logged and
//! later tasks in the batch continue. A crash still loses process-local work.
//!
//! # Durable queues
//! [`TaskQueue`] is the interface for a durable backend. Redis / RabbitMQ
//! adapters are not provided in this phase.

use crate::error::ApiError;
use crate::extract::FromRequestParts;
use http::request::Parts;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::future::Future;
use std::pin::Pin;
use thiserror::Error;
use uuid::Uuid;

mod manager;
mod request;
pub(crate) use manager::TaskManager;
pub(crate) use request::RequestGuard;
use request::RequestTasks;

/// Limits for each app's process-local background queue.
///
/// Mounted apps have separate queues. Admission counts pending and running
/// tasks, including batches awaiting an execution slot.
#[derive(Debug, Clone, Copy)]
pub struct BackgroundTaskLimits {
    /// Maximum concurrently running request batches (default: 64).
    pub max_active_batches: usize,
    /// Maximum admitted tasks across requests (default: 1024).
    pub max_tasks: usize,
    /// Maximum tasks queued by one request (default: 100).
    pub max_tasks_per_request: usize,
}

impl Default for BackgroundTaskLimits {
    fn default() -> Self {
        Self {
            max_active_batches: 64,
            max_tasks: 1024,
            max_tasks_per_request: 100,
        }
    }
}

/// Rejection while queuing process-local work.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum BackgroundTaskError {
    /// The request or application is no longer accepting work.
    #[error("background queue is closed")]
    Closed,
    /// A request or app admission limit has been reached.
    #[error("background queue capacity reached")]
    Capacity,
    /// No endpoint request or Tokio runtime owns this work.
    #[error("background execution is unavailable")]
    Unavailable,
}

type LocalTask = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Collect bounded process-local work for a successful endpoint response.
///
/// Default-constructed instances are inert. Extract through a handler to
/// obtain request ownership. Use [`Self::try_add`] to handle overload;
/// [`Self::add`] logs a rejection and drops the rejected future.
#[derive(Default)]
pub struct BackgroundTasks {
    queue: Option<RequestTasks>,
}

impl BackgroundTasks {
    /// Queue work, logging and discarding it when admission is rejected.
    ///
    /// Args:
    ///     fut: Future executed after a successful endpoint response.
    ///
    /// Returns:
    ///     Nothing; use [`Self::try_add`] to inspect rejection.
    pub fn add<F>(&mut self, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        if let Err(error) = self.try_add(fut) {
            tracing::warn!(%error, "background task rejected");
        }
    }

    /// Reserve bounded admission for work within this request.
    ///
    /// Args:
    ///     fut: Future whose execution requires a successful endpoint.
    ///
    /// Returns:
    ///     Success after admission; cancellation still discards queued work.
    ///
    /// # Errors
    /// Closed request/app, unavailable ownership, or exhausted capacity.
    pub fn try_add<F>(&mut self, fut: F) -> Result<(), BackgroundTaskError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.queue
            .as_ref()
            .ok_or(BackgroundTaskError::Unavailable)?
            .add(Box::pin(fut))
    }

    /// Queue the future returned by `f`, with [`Self::add`] admission policy.
    ///
    /// Args:
    ///     f: Factory called immediately to construct the queued future.
    ///
    /// Returns:
    ///     Nothing; the future is discarded if admission is rejected.
    pub fn add_fn<F, Fut>(&mut self, f: F)
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.add(f());
    }
}

impl FromRequestParts for BackgroundTasks {
    const BACKGROUND_TASKS: bool = true;

    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        let queue = parts
            .extensions
            .get::<RequestTasks>()
            .cloned()
            .ok_or_else(|| {
                let message = "background extractor needs endpoint ownership";
                ApiError::internal(message)
            })?;
        Ok(Self { queue: Some(queue) })
    }
}

impl crate::App {
    /// Configure admission and execution limits for this app's task queue.
    ///
    /// Args:
    ///     limits: Positive limits within Tokio semaphore capacity.
    ///
    /// Returns:
    ///     Configured app; invalid limits become a configuration error.
    #[must_use]
    pub fn background_tasks(mut self, limits: BackgroundTaskLimits) -> Self {
        let capacity = tokio::sync::Semaphore::MAX_PERMITS;
        if limits.max_active_batches == 0
            || limits.max_tasks == 0
            || limits.max_tasks_per_request == 0
            || limits.max_active_batches > capacity
            || limits.max_tasks > capacity
            || limits.max_tasks_per_request > limits.max_tasks
        {
            self.config_errors
                .push("invalid background task limits".into());
        } else {
            self.background_limits = limits;
        }
        self
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
