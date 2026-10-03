//! Explicit composition of SQLx connection initialization.
//!
//! PoolOptions stores its after_connect callback privately. Constructors
//! cannot recover or transparently chain a callback already installed there.
//! Pass custom setup explicitly; adapters run it before mandatory settings.

use sqlx::Database;
use sqlx::pool::PoolConnectionMetadata;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

type Outcome = Result<(), sqlx::Error>;
type Conn<DB> = <DB as Database>::Connection;
type Meta = PoolConnectionMetadata;

type Pinned<'a> = Pin<Box<dyn Future<Output = Outcome> + Send + 'a>>;
/// Borrowed connection initialization future.
pub type InitFuture<'a> = Pinned<'a>;

/// Shared custom callback invoked on every fresh physical connection.
///
/// Return an error to reject the connection; SQLx closes it and retries
/// within its acquisition deadline. Leave no transaction or lock open.
/// Mandatory adapter settings run afterwards and may override conflicting
/// values. Callback messages are subject to SQLx's own error logging.
pub type ConnectionInit<DB> =
    Arc<dyn for<'a> Fn(&'a mut Conn<DB>, Meta) -> Pinned<'a> + Send + Sync>;
