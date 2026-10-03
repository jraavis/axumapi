//! Bounded ownership of accepted process-local background batches.

use super::{BackgroundTaskError, BackgroundTaskLimits, LocalTask};
use crate::ServerError;
use futures_util::FutureExt;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;
use tokio::time::Instant;
use tracing::Instrument;
use tracing::instrument::WithSubscriber;

type Outcome = Result<(), ServerError>;
type Admission = Result<OwnedSemaphorePermit, BackgroundTaskError>;

pub(super) type AcceptedTask = (LocalTask, OwnedSemaphorePermit);

struct State {
    workers: JoinSet<()>,
    closed: bool,
}

pub(super) struct Shared {
    state: Mutex<State>,
    slots: Arc<Semaphore>,
    active: Arc<Semaphore>,
    pub(super) per_request: usize,
}

#[derive(Clone)]
pub(crate) struct TaskManager(Arc<Shared>);

impl TaskManager {
    pub(crate) fn new(limits: BackgroundTaskLimits) -> Self {
        Self(Arc::new(Shared {
            state: Mutex::new(State {
                workers: JoinSet::new(),
                closed: false,
            }),
            slots: Arc::new(Semaphore::new(limits.max_tasks)),
            active: Arc::new(Semaphore::new(limits.max_active_batches)),
            per_request: limits.max_tasks_per_request,
        }))
    }

    pub(super) fn downgrade(&self) -> Weak<Shared> {
        Arc::downgrade(&self.0)
    }

    pub(crate) async fn shutdown_at(&self, deadline: Instant) -> Outcome {
        let mut workers = {
            let guard = self.0.state.lock();
            let mut state = guard.unwrap_or_else(PoisonError::into_inner);
            state.closed = true;
            self.0.slots.close();
            std::mem::replace(&mut state.workers, JoinSet::new())
        };
        while !workers.is_empty() {
            let stopped = workers.join_next();
            let result = tokio::time::timeout_at(deadline, stopped).await;
            match result {
                Ok(Some(Ok(()))) | Ok(None) => {}
                Ok(Some(Err(error))) => {
                    tracing::error!(%error, "background worker failed");
                }
                Err(_) => {
                    // JoinSet's drop aborts every remaining owned worker.
                    workers.abort_all();
                    return Err(ServerError::ShutdownTimeout);
                }
            }
        }
        Ok(())
    }
}

impl Shared {
    pub(super) fn reserve(&self) -> Admission {
        if self.slots.is_closed() {
            return Err(BackgroundTaskError::Closed);
        }
        Arc::clone(&self.slots)
            .try_acquire_owned()
            .map_err(|_| BackgroundTaskError::Capacity)
    }

    pub(super) fn accept(
        &self,
        tasks: Vec<AcceptedTask>,
        span: tracing::Span,
    ) -> Result<(), BackgroundTaskError> {
        if tasks.is_empty() {
            return Ok(());
        }
        let runtime = tokio::runtime::Handle::try_current();
        let runtime = runtime.map_err(|_| BackgroundTaskError::Unavailable)?;
        let guard = self.state.lock();
        let mut state = guard.unwrap_or_else(PoisonError::into_inner);
        if state.closed {
            return Err(BackgroundTaskError::Closed);
        }
        while state.workers.try_join_next().is_some() {}
        let active = Arc::clone(&self.active);
        state.workers.spawn_on(
            async move {
                let Ok(_active) = active.acquire_owned().await else {
                    return;
                };
                for (task, _slot) in tasks {
                    let result = AssertUnwindSafe(task).catch_unwind().await;
                    if let Err(panic) = result {
                        let message = super::panic_message(&panic);
                        tracing::error!(%message, "background task panicked");
                    }
                }
            }
            .instrument(span)
            .with_current_subscriber(),
            &runtime,
        );
        Ok(())
    }
}
