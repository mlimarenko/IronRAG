//! Compensating guard for a revision-source blob written before admission
//! commits (IRG-203 SITE-6).
//!
//! The inline and reprocess paths write the deterministic, checksum-derived
//! object *before* the admission transaction commits a `content_revision` row
//! that references it. When the caller future is dropped — a client disconnect,
//! a proxy abandoning the request, a runtime shutdown — the blob is left with
//! no referencing revision and nothing deletes it. This guard is armed *before*
//! the blob write and disarmed only once admission commits; a drop while still
//! armed best-effort reclaims the orphan.
//!
//! Safety against deleting a shared key: the key is derived from the content
//! checksum, so two concurrent admissions of identical bytes share it. The
//! guard therefore carries the per-library content storage advisory lock
//! (`content.library.storage:<library>`) from before the write until admission
//! commits. Reclamation takes the *same* lock before deciding, so it cannot run
//! between a concurrent admission's blob write and that admission's commit:
//! either the reclaimer holds the lock first (the competitor's write happens
//! after the delete and recreates the object), or the competitor committed
//! first (the reference check sees the committed row and the object is kept).
//! A check error is treated as "assume referenced": the object is never deleted
//! on uncertainty.

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::{PgPool, Postgres, Transaction};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{content_mutation_repository, content_revision_repository},
    interfaces::http::router_support::ApiError,
    services::content::storage::ContentStorageService,
};

/// Reclamation backend for a staged revision-source blob.
///
/// Production is [`DatabaseStagedRevisionSourceReclaimer`]. The trait exists so
/// the guard's drop decision is unit-testable without Postgres: a test double
/// records whether the single guarded reclamation ran and can simulate a
/// still-referenced key.
#[async_trait]
pub(crate) trait StagedRevisionSourceReclaimer: Send + Sync + 'static {
    /// Reclaims `storage_key` only when no committed revision references it.
    ///
    /// Production holds the per-library content storage advisory lock for the
    /// whole decision: acquire the lock, probe [`storage_key_is_referenced`],
    /// delete via [`ContentStorageService::delete_revision_source`] only when
    /// unreferenced, then release the lock. Returns whether an object was
    /// removed. Any error leaves the object in place (fail closed).
    ///
    /// [`storage_key_is_referenced`]: crate::infra::repositories::content_revision_repository::storage_key_is_referenced
    async fn reclaim_if_unreferenced(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
        storage_key: &str,
    ) -> Result<bool, ApiError>;
}

/// Production reclaimer: library storage lock plus reference check plus delete.
struct DatabaseStagedRevisionSourceReclaimer {
    postgres: PgPool,
    storage: ContentStorageService,
}

#[async_trait]
impl StagedRevisionSourceReclaimer for DatabaseStagedRevisionSourceReclaimer {
    async fn reclaim_if_unreferenced(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
        storage_key: &str,
    ) -> Result<bool, ApiError> {
        // The advisory lock serializes this decision against every admission
        // that writes and commits the same content-addressed key: an admission
        // holds the lock from before its blob write until its commit, so once
        // we hold the lock no such admission is between those two steps.
        let storage_lock = content_mutation_repository::acquire_content_library_storage_lock(
            &self.postgres,
            library_id,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        let referenced =
            content_revision_repository::storage_key_is_referenced(&self.postgres, storage_key)
                .await
                .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        if referenced {
            storage_lock
                .commit()
                .await
                .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
            return Ok(false);
        }
        let deleted = self
            .storage
            .delete_revision_source(workspace_id, library_id, storage_key)
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        storage_lock
            .commit()
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        Ok(deleted)
    }
}

/// Owns a just-started revision-source write and reclaims it on abnormal drop.
///
/// Arm with the key and the per-library content storage lock *before* awaiting
/// the blob write, disarm only after the admission transaction commits. While
/// armed it holds the advisory lock so a concurrent reclaimer cannot delete the
/// key mid-write; `disarm` releases the lock and a disarmed guard never touches
/// storage.
pub(crate) struct PersistedRevisionSourceGuard {
    reclaimer: Option<Arc<dyn StagedRevisionSourceReclaimer>>,
    workspace_id: Uuid,
    library_id: Uuid,
    storage_key: String,
    storage_lock: Option<Transaction<'static, Postgres>>,
    armed: bool,
}

impl PersistedRevisionSourceGuard {
    /// Arms a guard for the blob about to be written to content storage.
    ///
    /// `storage_lock` is the per-library content storage advisory lock, taken
    /// before the write and released on [`Self::disarm`] or on armed drop.
    pub(crate) fn arm(
        state: &AppState,
        workspace_id: Uuid,
        library_id: Uuid,
        storage_key: String,
        storage_lock: Transaction<'static, Postgres>,
    ) -> Self {
        Self {
            reclaimer: Some(Arc::new(DatabaseStagedRevisionSourceReclaimer {
                postgres: state.persistence.postgres.clone(),
                storage: state.content_storage.clone(),
            })),
            workspace_id,
            library_id,
            storage_key,
            storage_lock: Some(storage_lock),
            armed: true,
        }
    }

    /// Test seam: arm against a caller-supplied reclaimer and no real lock.
    #[cfg(test)]
    pub(crate) fn arm_with(
        reclaimer: Arc<dyn StagedRevisionSourceReclaimer>,
        workspace_id: Uuid,
        library_id: Uuid,
        storage_key: String,
    ) -> Self {
        Self {
            reclaimer: Some(reclaimer),
            workspace_id,
            library_id,
            storage_key,
            storage_lock: None,
            armed: true,
        }
    }

    /// Marks the blob as admitted: drop no longer reclaims it, and the
    /// per-library storage lock is released.
    pub(crate) fn disarm(&mut self) {
        self.armed = false;
        // The transaction carries only the advisory lock and performs no
        // writes, so dropping it rolls back and releases the lock without an
        // await; this keeps `disarm` synchronous.
        drop(self.storage_lock.take());
    }
}

impl Drop for PersistedRevisionSourceGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(reclaimer) = self.reclaimer.take() else {
            return;
        };
        // A pooled `Transaction` can only be released through the Tokio runtime
        // — its `Drop` schedules the connection's return — so resolve the
        // runtime before touching the lock: dropping it with no runtime panics
        // before the fallback below could warn.
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            // No runtime: keep the connection alive instead of panicking during
            // unwinding. The process is exiting and the pool dies with it, so
            // the leak is bounded by shutdown.
            std::mem::forget(self.storage_lock.take());
            warn!(
                storage_key = %self.storage_key,
                "staged revision source abandoned with no runtime to reclaim it; object storage keeps an orphan blob",
            );
            return;
        };
        // Release the lock before reclaiming: the reclaimer re-acquires the
        // same advisory lock, so holding it here would make the reclaimer wait
        // on ourselves.
        drop(self.storage_lock.take());
        let workspace_id = self.workspace_id;
        let library_id = self.library_id;
        let storage_key = std::mem::take(&mut self.storage_key);
        runtime.spawn(async move {
            match reclaimer.reclaim_if_unreferenced(workspace_id, library_id, &storage_key).await {
                Ok(true) => debug!(
                    %storage_key,
                    "reclaimed orphaned staged revision source under the library storage lock",
                ),
                Ok(false) => debug!(
                    %storage_key,
                    "staged revision source is referenced or already gone; not deleting",
                ),
                Err(error) => {
                    // Fail closed: an unverifiable reference means an admission
                    // may already own this blob, so never delete on uncertainty.
                    warn!(
                        %storage_key,
                        %error,
                        "could not verify staged revision source is unreferenced under the library storage lock; leaving the object in place",
                    );
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::Duration;

    use tokio::sync::mpsc;

    use super::*;

    struct FakeReclaimer {
        referenced: bool,
        guarded_entrypoint_used: AtomicBool,
        events: mpsc::UnboundedSender<String>,
        deleted_keys: Mutex<Vec<String>>,
    }

    impl FakeReclaimer {
        fn new(referenced: bool, events: mpsc::UnboundedSender<String>) -> Arc<Self> {
            Arc::new(Self {
                referenced,
                guarded_entrypoint_used: AtomicBool::new(false),
                events,
                deleted_keys: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl StagedRevisionSourceReclaimer for FakeReclaimer {
        async fn reclaim_if_unreferenced(
            &self,
            _workspace_id: Uuid,
            _library_id: Uuid,
            storage_key: &str,
        ) -> Result<bool, ApiError> {
            self.guarded_entrypoint_used.store(true, Ordering::SeqCst);
            let _ = self.events.send(storage_key.to_string());
            if self.referenced {
                return Ok(false);
            }
            self.deleted_keys.lock().expect("deleted_keys lock").push(storage_key.to_string());
            Ok(true)
        }
    }

    fn guard_for(reclaimer: Arc<FakeReclaimer>) -> PersistedRevisionSourceGuard {
        PersistedRevisionSourceGuard::arm_with(
            reclaimer,
            Uuid::now_v7(),
            Uuid::now_v7(),
            "content/ws/lib/digest-source.txt".to_string(),
        )
    }

    #[tokio::test]
    async fn armed_guard_reclaims_unreferenced_blob_through_the_guarded_entrypoint() {
        let (events, mut rx) = mpsc::unbounded_channel();
        let reclaimer = FakeReclaimer::new(false, events);

        drop(guard_for(Arc::clone(&reclaimer)));

        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("guard must run the guarded reclamation")
            .expect("reclamation event");
        assert_eq!(event, "content/ws/lib/digest-source.txt");
        assert!(
            tokio::time::timeout(Duration::from_millis(100), rx.recv()).await.is_err(),
            "the guard must reclaim through the single guarded entrypoint, not a second call",
        );
        assert!(
            reclaimer.guarded_entrypoint_used.load(Ordering::SeqCst),
            "reclaim must be invoked through reclaim_if_unreferenced, which holds the library lock",
        );
        let deleted = reclaimer.deleted_keys.lock().expect("deleted_keys lock");
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0], "content/ws/lib/digest-source.txt");
    }

    #[tokio::test]
    async fn reclaim_is_invoked_through_the_single_guarded_entrypoint() {
        let (events, mut rx) = mpsc::unbounded_channel();
        let reclaimer = FakeReclaimer::new(false, events);

        drop(guard_for(Arc::clone(&reclaimer)));

        let _ = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await;
        assert!(
            reclaimer.guarded_entrypoint_used.load(Ordering::SeqCst),
            "reclaim must be invoked through reclaim_if_unreferenced, which holds the library storage lock",
        );
    }

    #[tokio::test]
    async fn disarmed_guard_retains_blob_on_drop() {
        let (events, mut rx) = mpsc::unbounded_channel();
        let reclaimer = FakeReclaimer::new(false, events);

        let mut guard = guard_for(Arc::clone(&reclaimer));
        guard.disarm();
        drop(guard);

        assert!(
            tokio::time::timeout(Duration::from_millis(100), rx.recv()).await.is_err(),
            "a disarmed guard must not check or delete anything",
        );
        assert!(
            !reclaimer.guarded_entrypoint_used.load(Ordering::SeqCst),
            "a disarmed guard must not invoke the reclaimer",
        );
        assert!(reclaimer.deleted_keys.lock().expect("deleted_keys lock").is_empty());
    }

    #[tokio::test]
    async fn referenced_key_is_checked_but_not_deleted() {
        let (events, mut rx) = mpsc::unbounded_channel();
        let reclaimer = FakeReclaimer::new(true, events);

        drop(guard_for(Arc::clone(&reclaimer)));

        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("guard must run the guarded reclamation")
            .expect("reclamation event");
        assert_eq!(event, "content/ws/lib/digest-source.txt");
        assert!(
            tokio::time::timeout(Duration::from_millis(100), rx.recv()).await.is_err(),
            "a referenced key must never be deleted",
        );
        assert!(
            reclaimer.guarded_entrypoint_used.load(Ordering::SeqCst),
            "reclaim must be invoked through reclaim_if_unreferenced, which holds the library lock",
        );
        assert!(reclaimer.deleted_keys.lock().expect("deleted_keys lock").is_empty());
    }
}
