//! Request-scoped authorization and cancellation of deferred side effects.

use super::manager::{AcceptedTask, Shared, TaskManager};
use super::{BackgroundTaskError, LocalTask};
use http::{Extensions, StatusCode};
use std::sync::{Arc, Mutex, PoisonError, Weak};

type QueueResult = Result<(), BackgroundTaskError>;

struct Pending {
    tasks: Vec<AcceptedTask>,
    closed: bool,
}

#[derive(Clone)]
pub(super) struct RequestTasks {
    pending: Arc<Mutex<Pending>>,
    manager: Weak<Shared>,
}

impl RequestTasks {
    pub(super) fn add(&self, task: LocalTask) -> QueueResult {
        let manager = self.manager.upgrade();
        let manager = manager.ok_or(BackgroundTaskError::Closed)?;
        let guard = self.pending.lock();
        let mut pending = guard.unwrap_or_else(PoisonError::into_inner);
        if pending.closed {
            return Err(BackgroundTaskError::Closed);
        }
        if pending.tasks.len() >= manager.per_request {
            return Err(BackgroundTaskError::Capacity);
        }
        let permit = manager.reserve()?;
        pending.tasks.push((task, permit));
        Ok(())
    }
}

pub(crate) struct RequestGuard {
    tasks: Option<RequestTasks>,
    span: tracing::Span,
}

impl RequestGuard {
    pub(crate) fn install(extensions: &mut Extensions) -> Self {
        let manager = extensions.get::<TaskManager>();
        let tasks = manager.map(|manager| RequestTasks {
            pending: Arc::new(Mutex::new(Pending {
                tasks: Vec::new(),
                closed: false,
            })),
            manager: manager.downgrade(),
        });
        if let Some(tasks) = &tasks {
            extensions.insert(tasks.clone());
        }
        Self {
            tasks,
            span: tracing::Span::current(),
        }
    }

    pub(crate) fn finish(mut self, status: StatusCode) {
        let Some(queue) = self.tasks.take() else {
            return;
        };
        let tasks = {
            let guard = queue.pending.lock();
            let mut pending = guard.unwrap_or_else(PoisonError::into_inner);
            pending.closed = true;
            std::mem::take(&mut pending.tasks)
        };
        if status.is_success() && !tasks.is_empty() {
            let result = queue
                .manager
                .upgrade()
                .ok_or(BackgroundTaskError::Closed)
                .and_then(|manager| manager.accept(tasks, self.span.clone()));
            if let Err(error) = result {
                tracing::warn!(%error, "background batch rejected");
            }
        }
    }
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        if let Some(queue) = self.tasks.take() {
            let guard = queue.pending.lock();
            let mut pending = guard.unwrap_or_else(PoisonError::into_inner);
            pending.closed = true;
            pending.tasks.clear();
        }
    }
}
