//! Model signal receivers: an audit trail for posts.

use crate::models::{AuditEntry, Post};
use axumapi::orm::signals::{SignalError, SignalEvent, SignalKind, Signals};
use axumapi::orm::{Db, ModelOps};
use axumapi::prelude::*;
use axumapi::receiver;
use std::sync::{Mutex, PoisonError};

/// Append one [`AuditEntry`] through the handle of the operation, so the
/// entry commits or rolls back together with the change it describes.
async fn record(event: &SignalEvent<'_>, action: &str, post: &Post) -> Result<(), SignalError> {
    AuditEntry {
        id: 0,
        action: action.to_owned(),
        entity_id: post.id,
        created_at: Utc::now(),
    }
    .save(event.db)
    .await?;
    Ok(())
}

/// Log `post.created` and `post.updated`.
#[receiver(post_save, model = Post)]
pub async fn audit_post_saved(post: &Post, event: &SignalEvent<'_>) -> Result<(), SignalError> {
    let action = match event.kind {
        SignalKind::PostSave { created: true } => "post.created",
        _ => "post.updated",
    };
    record(event, action, post).await
}

/// Log `post.deleted`.
#[receiver(post_delete, model = Post)]
pub async fn audit_post_deleted(post: &Post, event: &SignalEvent<'_>) -> Result<(), SignalError> {
    record(event, "post.deleted", post).await
}

/// The registry with every receiver of the blog connected.
pub fn signals() -> Signals {
    let signals = Signals::new();
    connect_all(&signals);
    signals
}

fn connect_all(signals: &Signals) {
    signals.connect(audit_post_saved_receiver());
    signals.connect(audit_post_deleted_receiver());
}

/// Connect the blog's receivers to `db` unless that was already done.
///
/// The registry is shared by every clone of the handle, so this is safe to
/// call on each request; the lock only serializes the first calls.
pub fn install(db: &Db) {
    static INSTALL: Mutex<()> = Mutex::new(());
    let _guard = INSTALL.lock().unwrap_or_else(PoisonError::into_inner);
    let registry = db.signals();
    if registry.is_empty() {
        connect_all(registry);
    }
}
