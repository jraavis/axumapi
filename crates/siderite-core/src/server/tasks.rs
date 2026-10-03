//! Weak task handles cannot keep their supervisor alive through a cycle.

use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use tokio::sync::{OwnedSemaphorePermit as Permit, Semaphore};
use tokio::task::{JoinError, JoinSet};
use tokio::time::Instant;

struct State {
    workers: JoinSet<()>,
    closed: bool,
}

struct Shared {
    state: Mutex<State>,
    capacity: Arc<Semaphore>,
}

#[derive(Clone)]
pub(crate) struct TaskOwner(Arc<Shared>);

#[derive(Clone)]
pub(crate) struct TaskHandle(Weak<Shared>);

impl TaskOwner {
    pub(crate) fn new(capacity: usize) -> Self {
        Self(Arc::new(Shared {
            state: Mutex::new(State {
                workers: JoinSet::new(),
                closed: false,
            }),
            capacity: Arc::new(Semaphore::new(capacity)),
        }))
    }

    pub(crate) fn handle(&self) -> TaskHandle {
        TaskHandle(Arc::downgrade(&self.0))
    }

    pub(crate) fn abort(&self) {
        self.0
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .workers
            .abort_all();
    }

    pub(crate) async fn join_next(&self) -> Option<Result<(), JoinError>> {
        futures_util::future::poll_fn(|context| {
            self.0
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .workers
                .poll_join_next(context)
        })
        .await
    }

    pub(crate) async fn stop_at(&self, deadline: Instant) -> bool {
        let mut workers = {
            let guard = self.0.state.lock();
            let mut state = guard.unwrap_or_else(PoisonError::into_inner);
            state.closed = true;
            self.0.capacity.close();
            std::mem::replace(&mut state.workers, JoinSet::new())
        };
        while !workers.is_empty() {
            if tokio::time::timeout_at(deadline, workers.join_next())
                .await
                .is_err()
            {
                workers.abort_all();
                while workers.join_next().await.is_some() {}
                return false;
            }
        }
        true
    }
}

impl TaskHandle {
    pub(crate) async fn acquire(&self) -> Option<Permit> {
        let capacity = self.0.upgrade()?.capacity.clone();
        capacity.acquire_owned().await.ok()
    }

    pub(crate) fn reserve(&self) -> Option<Permit> {
        self.0.upgrade()?.capacity.clone().try_acquire_owned().ok()
    }

    pub(crate) fn spawn<F>(&self, permit: Permit, future: F) -> bool
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let Some(shared) = self.0.upgrade() else {
            return false;
        };
        let guard = shared.state.lock();
        let mut state = guard.unwrap_or_else(PoisonError::into_inner);
        if state.closed {
            return false;
        }
        while state.workers.try_join_next().is_some() {}
        state.workers.spawn(async move {
            let _permit = permit;
            future.await;
        });
        true
    }
}

impl<F> hyper::rt::Executor<F> for TaskHandle
where
    F: Future + Send + 'static,
    F::Output: Send,
{
    fn execute(&self, future: F) {
        if let Some(permit) = self.reserve() {
            self.spawn(permit, async move {
                drop(future.await);
            });
        } else {
            tracing::error!("HTTP protocol task admission closed or full");
        }
    }
}
