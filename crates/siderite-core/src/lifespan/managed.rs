//! A single cleanup supervisor owns hooks across caller cancellation.

use super::Lifespan;
use crate::{ApiError, ServerError};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;

type Outcome = Result<(), ServerError>;
type Ready = oneshot::Receiver<Outcome>;
type Worker = JoinHandle<Outcome>;

/// Owns a running lifespan and its cancellation-safe cleanup supervisor.
///
/// Dropping this owner requests teardown. Cleanup continues on its existing
/// supervisor until the deadline; no per-resource cleanup tasks are spawned.
/// The Tokio runtime must stay alive for cleanup to run. A process crash or
/// runtime destruction cannot guarantee asynchronous teardown.
#[derive(Debug)]
pub struct ManagedLifespan {
    ready: Option<Ready>,
    stop: Option<oneshot::Sender<Instant>>,
    worker: Option<Worker>,
    started: bool,
    budget: Duration,
}

impl ManagedLifespan {
    pub(super) fn new(mut lifespan: Lifespan) -> Result<Self, ServerError> {
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| {
            let message = "supervision needs a Tokio runtime";
            ServerError::Configuration(message.into())
        })?;
        let budget = lifespan.shutdown_budget;
        let (ready_tx, ready) = oneshot::channel();
        let (stop, mut stop_rx) = oneshot::channel();
        let worker = runtime.spawn(async move {
            let startup = tokio::select! {
                result = lifespan.start_hooks() => result,
                deadline = &mut stop_rx => {
                    let deadline = deadline.unwrap_or_else(|_| {
                        Instant::now() + budget
                    });
                    let _ = ready_tx.send(Err(ServerError::Configuration(
                        "startup was cancelled".into(),
                    )));
                    return lifespan.shutdown_at(deadline).await;
                }
            };
            match startup {
                Ok(()) => {
                    if ready_tx.send(Ok(())).is_err() {
                        let at = Instant::now() + budget;
                        return lifespan.shutdown_at(at).await;
                    }
                    let received = stop_rx.await;
                    let at = received.unwrap_or_else(|_| Instant::now() + budget);
                    lifespan.shutdown_at(at).await
                }
                Err(error) => {
                    // Preserve the startup error after all cleanup attempts.
                    let at = Instant::now() + budget;
                    let _ = lifespan.shutdown_at(at).await;
                    let _ = ready_tx.send(Err(error));
                    Ok(())
                }
            }
        });
        Ok(Self {
            ready: Some(ready),
            stop: Some(stop),
            worker: Some(worker),
            started: false,
            budget,
        })
    }

    /// Wait for successful startup, including cleanup on startup failure.
    ///
    /// Args:
    ///     self: The supervisor owner.
    ///
    /// Returns:
    ///     Success after initialization, or the original startup error.
    ///
    /// # Errors
    /// Startup or supervisor failure. Dropping this future requests cleanup
    /// only if its owner is dropped too; otherwise call [`Self::shutdown`].
    pub async fn ready(&mut self) -> Result<(), ServerError> {
        if self.started {
            return Ok(());
        }
        let receiver = self.ready.take().ok_or_else(|| {
            let message = "readiness consumed";
            ServerError::Configuration(message.into())
        })?;
        receiver.await.map_err(|_| supervisor_error())??;
        self.started = true;
        Ok(())
    }

    /// Request shutdown and await reverse-order, bounded resource cleanup.
    ///
    /// Args:
    ///     self: Owner consumed by shutdown.
    ///
    /// Returns:
    ///     The first cleanup error, if any.
    ///
    /// # Errors
    /// Cleanup failure or exceeded budget. Cancelling this future leaves the
    /// supervisor running its bounded cleanup.
    pub async fn shutdown(self) -> Result<(), ServerError> {
        let deadline = Instant::now() + self.budget;
        self.shutdown_at(deadline).await
    }

    pub(crate) async fn shutdown_at(mut self, at: Instant) -> Outcome {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(at);
        }
        let worker = self.worker.take().ok_or_else(supervisor_error)?;
        worker.await.map_err(|_| supervisor_error())?
    }
}

fn supervisor_error() -> ServerError {
    ServerError::Lifespan(ApiError::internal("lifespan supervisor failed"))
}
