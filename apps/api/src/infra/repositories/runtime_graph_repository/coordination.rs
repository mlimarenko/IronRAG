use sqlx::{PgPool, Postgres, Transaction, pool::PoolConnection};
use uuid::Uuid;

use crate::infra::knowledge_plane::IngestRevisionWriteAuthority;

use super::super::{catalog_repository, ingest_repository};

/// Owned authority boundary spanning graph reconcile through lifecycle
/// publication.
///
/// The transaction is intentionally opaque: only the ingest publication
/// repository consumes it. Dropping it rolls back the guard transaction and
/// releases its row/advisory locks without changing lifecycle state.
pub struct IngestRevisionGraphPublicationGuard {
    transaction: Transaction<'static, Postgres>,
    pub authority: IngestRevisionWriteAuthority,
    pub document_id: Uuid,
}

pub enum IngestRevisionGraphGuardAcquireOutcome {
    Acquired(IngestRevisionGraphPublicationGuard),
    AuthorityLost,
    Superseded,
    Busy,
}

impl IngestRevisionGraphPublicationGuard {
    /// Runs an authorized graph operation inside a savepoint while retaining
    /// the outer authority and advisory locks for lifecycle publication.
    ///
    /// The savepoint is always explicitly resolved: successful work is
    /// released, while errors (including cancellation/timeout mapped by the
    /// caller) are rolled back before this guard is returned. This keeps a
    /// degraded lifecycle publication from committing partial graph state.
    pub async fn run_graph_savepoint<T, E, F>(
        &mut self,
        operation: F,
    ) -> Result<T, GraphSavepointError<E>>
    where
        F: for<'connection> FnOnce(
            &'connection mut sqlx::PgConnection,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<T, E>> + Send + 'connection>,
        >,
    {
        sqlx::query("savepoint ingest_graph_reconcile")
            .execute(&mut *self.transaction)
            .await
            .map_err(GraphSavepointError::Sqlx)?;
        match operation(&mut self.transaction).await {
            Ok(value) => {
                sqlx::query("release savepoint ingest_graph_reconcile")
                    .execute(&mut *self.transaction)
                    .await
                    .map_err(GraphSavepointError::Sqlx)?;
                Ok(value)
            }
            Err(error) => {
                sqlx::query("rollback to savepoint ingest_graph_reconcile")
                    .execute(&mut *self.transaction)
                    .await
                    .map_err(GraphSavepointError::Sqlx)?;
                Err(GraphSavepointError::Operation(error))
            }
        }
    }

    #[must_use]
    pub fn connection(&mut self) -> &mut sqlx::PgConnection {
        &mut self.transaction
    }

    #[must_use]
    pub fn into_transaction(self) -> Transaction<'static, Postgres> {
        self.transaction
    }

    /// Explicitly abandons every graph mutation performed under this guard.
    ///
    /// Callers use this when the in-process cancellation signal arrives after
    /// reconcile finished but before lifecycle publication. Relying on async
    /// transaction drop would eventually roll back too, but an explicit
    /// rollback makes the cancellation boundary deterministic before the
    /// worker reports that it stopped.
    pub async fn rollback(self) -> Result<(), sqlx::Error> {
        self.transaction.rollback().await
    }
}

pub enum GraphSavepointError<E> {
    Operation(E),
    Sqlx(sqlx::Error),
}

/// Acquires the complete authority guard for a live ingest-owned graph
/// reconcile.
///
/// Lock order is deliberately fixed as library -> content document advisory
/// lock -> graph merge advisory lock -> raw graph advisory lock. The current
/// job/attempt are deliberately left unlocked until lifecycle publication, as
/// described below. The returned
/// transaction stages graph/evidence/projection writes on this transaction's
/// connection. Authority may move while those writes are still private: pause,
/// cancellation, and lease recovery must remain responsive. Lifecycle
/// publication locks and revalidates the job/attempt before commit and rolls
/// the complete guard transaction back when authority moved. The returned guard
/// must be consumed by lifecycle publication; releasing it earlier would reopen
/// the reconcile-to-publication race.
///
/// `None` is a normal stale-owner outcome; callers must not perform any graph
/// mutation in that case.
pub async fn acquire_ingest_revision_graph_reconcile_guard(
    pool: &PgPool,
    authority: IngestRevisionWriteAuthority,
    document_id: Uuid,
) -> Result<IngestRevisionGraphGuardAcquireOutcome, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let job_kind =
        match resolve_reconcile_authority(&mut transaction, authority, document_id).await? {
            GuardResolution::Abort(abort) => {
                finalize_reconcile_guard_abort(transaction, abort).await?;
                return Ok(abort.into_outcome());
            }
            GuardResolution::Resolved { job_kind } => job_kind,
        };
    if let Some(abort) =
        acquire_reconcile_advisory_locks(&mut transaction, authority, document_id).await?
    {
        finalize_reconcile_guard_abort(transaction, abort).await?;
        return Ok(abort.into_outcome());
    }
    if let Some(abort) =
        reconcile_head_guard(&mut transaction, authority.revision_id, document_id, &job_kind)
            .await?
    {
        finalize_reconcile_guard_abort(transaction, abort).await?;
        return Ok(abort.into_outcome());
    }
    Ok(IngestRevisionGraphGuardAcquireOutcome::Acquired(IngestRevisionGraphPublicationGuard {
        transaction,
        authority,
        document_id,
    }))
}

/// Ordered guard result while acquiring the reconcile guard.
///
/// `Resolved` carries the job kind needed to decide whether the head lock is
/// required; `Abort` carries the first unsatisfied guard's outcome.
enum GuardResolution {
    Resolved { job_kind: String },
    Abort(ReconcileGuardAbort),
}

/// First unsatisfied guard in the fixed reconcile order.
///
/// `AuthorityLost` commits the already-staged no-op transaction; `Busy` and
/// `Superseded` roll back because an advisory/head lock was refused. The order
/// and finalization rules are pure so they can be unit-tested without a
/// database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReconcileGuardAbort {
    AuthorityLost,
    Busy,
    Superseded,
}

impl ReconcileGuardAbort {
    const fn aborts_with_rollback(self) -> bool {
        matches!(self, Self::Busy | Self::Superseded)
    }

    fn into_outcome(self) -> IngestRevisionGraphGuardAcquireOutcome {
        match self {
            Self::AuthorityLost => IngestRevisionGraphGuardAcquireOutcome::AuthorityLost,
            Self::Busy => IngestRevisionGraphGuardAcquireOutcome::Busy,
            Self::Superseded => IngestRevisionGraphGuardAcquireOutcome::Superseded,
        }
    }
}

/// Pure first-failure decision for an ordered list of guard observations.
///
/// Each element is `None` when the guard is satisfied and `Some(abort)`
/// otherwise; the earliest unsatisfied guard decides the outcome, so the abort
/// precedence cannot silently drift.
fn first_reconcile_guard_abort(
    checks: &[Option<ReconcileGuardAbort>],
) -> Option<ReconcileGuardAbort> {
    checks.iter().find_map(|check| *check)
}

/// Commits an authority-loss guard and rolls back a refused-lock guard.
async fn finalize_reconcile_guard_abort(
    transaction: Transaction<'static, Postgres>,
    abort: ReconcileGuardAbort,
) -> Result<(), sqlx::Error> {
    if abort.aborts_with_rollback() {
        transaction.rollback().await
    } else {
        transaction.commit().await
    }
}

/// Locks the library and verifies the revision/attempt still own authority.
///
/// KEY SHARE is the strongest parent lifetime lock compatible with the
/// snapshot writer's FOR NO KEY UPDATE on another pooled connection. It still
/// conflicts with catalog deletion's FOR UPDATE, so the library cannot
/// disappear while reconcile is publishing graph artifacts. The job/attempt
/// are deliberately not row-locked here: pause/cancel/reaper are control-plane
/// operations and must be able to move authority immediately. The staged graph
/// writes stay invisible until guarded lifecycle publication takes the job
/// lock; if authority moved first, publication rolls this transaction back.
async fn resolve_reconcile_authority(
    transaction: &mut Transaction<'static, Postgres>,
    authority: IngestRevisionWriteAuthority,
    document_id: Uuid,
) -> Result<GuardResolution, sqlx::Error> {
    let library_locked = sqlx::query_scalar::<_, Uuid>(
        "select id
         from catalog_library
         where id = $1 and workspace_id = $2
         for key share",
    )
    .bind(authority.library_id)
    .bind(authority.workspace_id)
    .fetch_optional(&mut **transaction)
    .await?;
    let job_scope = sqlx::query_as::<_, (Uuid, String)>(
        "select job.id, job.job_kind::text
         from ingest_job as job
         join ingest_attempt as attempt on attempt.job_id = job.id
         where attempt.id = $1
           and job.workspace_id = $2
           and job.library_id = $3
           and job.knowledge_revision_id = $4
           and job.queue_state = 'leased'",
    )
    .bind(authority.attempt_id)
    .bind(authority.workspace_id)
    .bind(authority.library_id)
    .bind(authority.revision_id)
    .fetch_optional(&mut **transaction)
    .await?;
    let attempt_is_current = match job_scope.as_ref() {
        Some((job_id, _)) => sqlx::query_scalar::<_, bool>(
            "select true
             from ingest_attempt as attempt
             where attempt.id = $1
               and attempt.job_id = $2
               and attempt.attempt_state = 'leased'
               and not exists (
                   select 1
                   from ingest_attempt as newer
                   where newer.job_id = attempt.job_id
                     and newer.attempt_number > attempt.attempt_number
               )",
        )
        .bind(authority.attempt_id)
        .bind(job_id)
        .fetch_optional(&mut **transaction)
        .await?
        .unwrap_or(false),
        None => false,
    };
    // The revision `FOR SHARE` lock must not be taken once authority is
    // already lost, so the already-decided guards short-circuit here.
    let decided = [
        library_locked.is_none().then_some(ReconcileGuardAbort::AuthorityLost),
        job_scope.is_none().then_some(ReconcileGuardAbort::AuthorityLost),
        (!attempt_is_current).then_some(ReconcileGuardAbort::AuthorityLost),
    ];
    if let Some(abort) = first_reconcile_guard_abort(&decided) {
        return Ok(GuardResolution::Abort(abort));
    }
    let revision_document = sqlx::query_scalar::<_, Uuid>(
        "select document_id
         from content_revision
         where id = $1
           and workspace_id = $2
           and library_id = $3
           and document_id = $4
         for share",
    )
    .bind(authority.revision_id)
    .bind(authority.workspace_id)
    .bind(authority.library_id)
    .bind(document_id)
    .fetch_optional(&mut **transaction)
    .await?;
    if revision_document.is_none() {
        return Ok(GuardResolution::Abort(ReconcileGuardAbort::AuthorityLost));
    }
    match job_scope {
        Some((_, job_kind)) => Ok(GuardResolution::Resolved { job_kind }),
        None => Ok(GuardResolution::Abort(ReconcileGuardAbort::AuthorityLost)),
    }
}

/// Takes the document, graph-merge and raw-graph advisory locks in the fixed
/// shared order; any refusal returns `Busy`.
async fn acquire_reconcile_advisory_locks(
    transaction: &mut Transaction<'static, Postgres>,
    authority: IngestRevisionWriteAuthority,
    document_id: Uuid,
) -> Result<Option<ReconcileGuardAbort>, sqlx::Error> {
    let document_lock = sqlx::query_scalar::<_, bool>(
        "select pg_try_advisory_xact_lock(hashtextextended($1::text, 0))",
    )
    .bind(format!("content.document:{document_id}"))
    .fetch_one(&mut **transaction)
    .await?;
    // Each refused lock aborts before the next is attempted, so a Busy guard
    // never takes advisory locks the original acquire order would not reach.
    if !document_lock {
        return Ok(Some(ReconcileGuardAbort::Busy));
    }
    let graph_lock = sqlx::query_scalar::<_, bool>(
        "select pg_try_advisory_xact_lock(
            hashtextextended('graph-merge:' || $1::text, 0)
         )",
    )
    .bind(authority.library_id.to_string())
    .fetch_one(&mut **transaction)
    .await?;
    if !graph_lock {
        return Ok(Some(ReconcileGuardAbort::Busy));
    }
    let raw_graph_lock = sqlx::query_scalar::<_, bool>(
        "select pg_try_advisory_xact_lock(hashtextextended($1::text, 0))",
    )
    .bind(authority.library_id.to_string())
    .fetch_one(&mut **transaction)
    .await?;
    if !raw_graph_lock {
        return Ok(Some(ReconcileGuardAbort::Busy));
    }
    Ok(None)
}

/// A `graph_refresh` job additionally requires the content document head to
/// still point at this revision. Head locks always follow the shared advisory
/// sequence: taking this lock before the document advisory would deadlock a
/// stale refresh against content publication for its replacement revision.
async fn reconcile_head_guard(
    transaction: &mut Transaction<'static, Postgres>,
    revision_id: Uuid,
    document_id: Uuid,
    job_kind: &str,
) -> Result<Option<ReconcileGuardAbort>, sqlx::Error> {
    if job_kind != "graph_refresh" {
        return Ok(None);
    }
    let current_readable = sqlx::query_scalar::<_, bool>(
        "select true
         from content_document_head
         where document_id = $1
           and active_revision_id = $2
           and readable_revision_id = $2
         for update",
    )
    .bind(document_id)
    .bind(revision_id)
    .fetch_optional(&mut **transaction)
    .await?
    .unwrap_or(false);
    Ok((!current_readable).then_some(ReconcileGuardAbort::Superseded))
}

/// Acquires a short graph-artifact mutation guard in the shared parent-first
/// order. Terminal extraction/cache writes use this helper so the authority
/// check and graph mutation commit in the same transaction.
pub(super) const SHORT_GRAPH_WRITE_GUARD_IDLE_TIMEOUT_SQL: &str =
    "set local idle_in_transaction_session_timeout = '60s'";

pub(super) async fn acquire_ingest_revision_graph_write_guard(
    pool: &PgPool,
    authority: IngestRevisionWriteAuthority,
) -> Result<Option<Transaction<'static, Postgres>>, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    // These guards only fence one small extraction/materialization write. If
    // an application task is stranded between statements, retaining the
    // library and attempt row locks stalls every graph-refresh worker for the
    // library. Let PostgreSQL close only that abandoned session instead of
    // allowing an unbounded cross-worker lock chain.
    sqlx::query(SHORT_GRAPH_WRITE_GUARD_IDLE_TIMEOUT_SQL).execute(&mut *transaction).await?;
    if !lock_ingest_revision_graph_authority(&mut transaction, authority).await? {
        transaction.commit().await?;
        return Ok(None);
    }
    Ok(Some(transaction))
}

async fn lock_ingest_revision_graph_authority(
    transaction: &mut Transaction<'_, Postgres>,
    authority: IngestRevisionWriteAuthority,
) -> Result<bool, sqlx::Error> {
    if !catalog_repository::lock_library_for_lifecycle_event_with_executor(
        &mut **transaction,
        authority.workspace_id,
        authority.library_id,
    )
    .await?
    {
        return Ok(false);
    }
    ingest_repository::lock_latest_leased_revision_attempt(
        transaction,
        authority.library_id,
        authority.attempt_id,
        authority.revision_id,
    )
    .await
}

/// Opaque transaction boundary for a whole-library runtime-graph rebuild.
///
/// The rebuild transaction and every admission/write lock share one dedicated
/// PostgreSQL session. Session advisory locks therefore cannot be released
/// independently of an in-flight `COMMIT` or `ROLLBACK`: PostgreSQL resolves
/// the transaction outcome before closing that session and releasing its
/// locks. The pooled connection is armed with `close_on_drop` before the first
/// lock is acquired, so cancellation and runtime shutdown close the session
/// instead of returning partially-finalized protocol state to the pool.
#[must_use = "the rebuild guard must be committed or rolled back"]
pub struct RuntimeGraphRebuildGuard {
    connection: PoolConnection<Postgres>,
}

impl RuntimeGraphRebuildGuard {
    /// Returns the connection that owns the repeatable-read rebuild snapshot.
    ///
    /// Every rebuild read and write must use this connection so the complete
    /// replacement is derived from and published within one snapshot.
    #[must_use]
    pub fn connection(&mut self) -> &mut sqlx::PgConnection {
        &mut self.connection
    }

    /// Commits rebuild data before closing the session and releasing its locks.
    ///
    /// # Errors
    /// Returns any `SQLx` error raised while committing the transaction or
    /// closing its dedicated session.
    pub async fn commit(self) -> Result<(), sqlx::Error> {
        finalize_runtime_graph_rebuild_connection(self.connection, "COMMIT").await
    }

    /// Rolls back rebuild data before closing the session and releasing locks.
    ///
    /// # Errors
    /// Returns any `SQLx` error raised while rolling back the transaction or
    /// closing its dedicated session.
    pub async fn rollback(self) -> Result<(), sqlx::Error> {
        finalize_runtime_graph_rebuild_connection(self.connection, "ROLLBACK").await
    }
}

async fn finalize_runtime_graph_rebuild_connection(
    mut connection: PoolConnection<Postgres>,
    statement: &'static str,
) -> Result<(), sqlx::Error> {
    let transaction_result = sqlx::query(statement).execute(&mut *connection).await.map(|_| ());
    let close_result = connection.close().await;
    match (transaction_result, close_result) {
        (Err(transaction_error), Err(close_error)) => {
            tracing::error!(
                error = %close_error,
                statement,
                "runtime graph rebuild session close also failed after transaction finalization error"
            );
            Err(transaction_error)
        }
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

/// Result of attempting to enter the globally serialized graph-rebuild lane.
pub enum RuntimeGraphRebuildGuardAcquireOutcome {
    /// Both the control-plane locks and repeatable-read data transaction are
    /// ready for rebuild work.
    Acquired(RuntimeGraphRebuildGuard),
    /// The target library disappeared before its lifetime lock was acquired.
    LibraryNotFound,
    /// Another runtime-graph rebuild already owns global admission.
    Busy,
}

/// Acquires the guard that fences a whole-library runtime-graph rebuild.
///
/// One dedicated session first takes a global, non-blocking rebuild admission
/// lock so multiple rebuild waiters cannot occupy pooled connections while
/// waiting. It then
/// takes the blocking graph-merge and raw-graph session locks in that fixed
/// order and enters `REPEATABLE READ`. The first snapshot query locks the target
/// library `FOR KEY SHARE`, keeping it alive through publication. No document
/// row is locked, which avoids reversing the document-first ordering used by
/// ingest reconcile.
///
/// Returns [`RuntimeGraphRebuildGuardAcquireOutcome::Busy`] without waiting
/// when another rebuild owns global admission, and
/// [`RuntimeGraphRebuildGuardAcquireOutcome::LibraryNotFound`] when the target
/// library no longer exists.
///
/// # Errors
/// Returns any `SQLx` error raised while acquiring or closing the dedicated
/// session, starting its transaction, locking the library row, or acquiring an
/// advisory lock.
pub async fn acquire_runtime_library_graph_rebuild_guard(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<RuntimeGraphRebuildGuardAcquireOutcome, sqlx::Error> {
    let mut connection = pool.acquire().await?;
    connection.close_on_drop();
    let admitted = sqlx::query_scalar::<_, bool>(
        "select pg_try_advisory_lock(
            hashtextextended('graph-rebuild-admission', 0)
         )",
    )
    .fetch_one(&mut *connection)
    .await?;
    if !admitted {
        connection.close().await?;
        return Ok(RuntimeGraphRebuildGuardAcquireOutcome::Busy);
    }

    sqlx::query(
        "select pg_advisory_lock(
            hashtextextended('graph-merge:' || $1::text, 0)
         )",
    )
    .bind(library_id.to_string())
    .execute(&mut *connection)
    .await?;
    sqlx::query("select pg_advisory_lock(hashtextextended($1::text, 0))")
        .bind(library_id.to_string())
        .execute(&mut *connection)
        .await?;

    sqlx::query("begin isolation level repeatable read").execute(&mut *connection).await?;
    let library_locked = sqlx::query_scalar::<_, Uuid>(
        "select id
         from catalog_library
         where id = $1
         for key share",
    )
    .bind(library_id)
    .fetch_optional(&mut *connection)
    .await?;
    if library_locked.is_none() {
        finalize_runtime_graph_rebuild_connection(connection, "ROLLBACK").await?;
        return Ok(RuntimeGraphRebuildGuardAcquireOutcome::LibraryNotFound);
    }

    Ok(RuntimeGraphRebuildGuardAcquireOutcome::Acquired(RuntimeGraphRebuildGuard { connection }))
}

/// Acquires a library-scoped `PostgreSQL` advisory lock for runtime-graph writes.
///
/// The returned transaction keeps the lock alive until commit/rollback.
/// Transaction-scoped locks are used deliberately so a cancelled future cannot
/// leak a session lock back into the pool.
pub async fn acquire_runtime_library_graph_lock(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind(library_id.to_string())
        .execute(&mut *transaction)
        .await?;
    Ok(transaction)
}

pub enum RuntimeGraphGcQuiescenceAcquireOutcome {
    Acquired(RuntimeGraphGcQuiescenceGuard),
    LibraryNotFound,
    LibraryBusy,
    ActiveIngest { active_jobs: i64 },
}

/// Opaque transaction owning the complete runtime-graph GC critical interval.
pub struct RuntimeGraphGcQuiescenceGuard {
    transaction: Transaction<'static, Postgres>,
}

impl RuntimeGraphGcQuiescenceGuard {
    pub fn transaction_mut(&mut self) -> &mut Transaction<'static, Postgres> {
        &mut self.transaction
    }

    pub async fn commit(self) -> Result<(), sqlx::Error> {
        self.transaction.commit().await
    }

    pub async fn rollback(self) -> Result<(), sqlx::Error> {
        self.transaction.rollback().await
    }
}

/// Acquires the complete runtime-graph GC quiescence guard.
///
/// Lock order is canonical: raw graph advisory transaction, catalog library
/// `FOR NO KEY UPDATE NOWAIT`, active-ingest count, graph/evidence mutations.
/// The catalog lock remains held through the caller's mutation commit. The
/// ingest-job projection trigger updates that same row after INSERT, so a new
/// job cannot commit between the count and the graph delete commit. Catalog
/// contention is a soft busy outcome rather than a blocking lock inversion.
pub async fn acquire_runtime_graph_gc_quiescence_guard(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<RuntimeGraphGcQuiescenceAcquireOutcome, sqlx::Error> {
    let mut transaction = acquire_runtime_library_graph_lock(pool, library_id).await?;
    let library = sqlx::query_scalar::<_, Uuid>(
        "select id
         from catalog_library
         where id = $1
         for no key update nowait",
    )
    .bind(library_id)
    .fetch_optional(&mut *transaction)
    .await;
    let library = match library {
        Ok(library) => library,
        Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("55P03") => {
            transaction.rollback().await?;
            return Ok(RuntimeGraphGcQuiescenceAcquireOutcome::LibraryBusy);
        }
        Err(error) => {
            transaction.rollback().await?;
            return Err(error);
        }
    };
    if library.is_none() {
        transaction.rollback().await?;
        return Ok(RuntimeGraphGcQuiescenceAcquireOutcome::LibraryNotFound);
    }
    let active_jobs = sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from ingest_job
         where library_id = $1
           and queue_state in ('queued', 'leased', 'paused')
           and completed_at is null",
    )
    .bind(library_id)
    .fetch_one(&mut *transaction)
    .await?;
    if active_jobs > 0 {
        transaction.rollback().await?;
        return Ok(RuntimeGraphGcQuiescenceAcquireOutcome::ActiveIngest { active_jobs });
    }
    Ok(RuntimeGraphGcQuiescenceAcquireOutcome::Acquired(RuntimeGraphGcQuiescenceGuard {
        transaction,
    }))
}

/// Releases a library-scoped `PostgreSQL` advisory lock for runtime-graph writes.
pub async fn release_runtime_library_graph_lock(
    transaction: Transaction<'static, Postgres>,
    _library_id: Uuid,
) -> Result<(), sqlx::Error> {
    transaction.commit().await
}

/// Acquires the cross-process library merge lock. It deliberately uses a
/// separate advisory namespace from projection publication so a reconcile can
/// publish its projection without self-deadlocking on a second connection.
pub async fn acquire_runtime_library_graph_merge_lock(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    sqlx::query(
        "select pg_advisory_xact_lock(
            hashtextextended('graph-merge:' || $1::text, 0)
         )",
    )
    .bind(library_id.to_string())
    .execute(&mut *transaction)
    .await?;
    Ok(transaction)
}

/// Commits the merge-lock transaction, releasing its transaction-scoped lock.
pub async fn release_runtime_library_graph_merge_lock(
    transaction: Transaction<'static, Postgres>,
    _library_id: Uuid,
) -> Result<(), sqlx::Error> {
    transaction.commit().await
}

/// Counts distinct filtered graph artifacts written for one ingestion attempt.
pub async fn count_runtime_graph_filtered_artifacts_by_ingestion_run(
    pool: &PgPool,
    library_id: Uuid,
    ingestion_run_id: Uuid,
    revision_id: Option<Uuid>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "select count(distinct concat_ws(
                ':',
                coalesce(revision_id::text, 'none'),
                coalesce(ingestion_run_id::text, 'none'),
                target_kind,
                candidate_key,
                filter_reason
            ))
         from runtime_graph_filtered_artifact
         where library_id = $1
           and ingestion_run_id = $2
           and ($3::uuid is null or revision_id = $3)",
    )
    .bind(library_id)
    .bind(ingestion_run_id)
    .bind(revision_id)
    .fetch_one(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::{ReconcileGuardAbort, first_reconcile_guard_abort};

    #[test]
    fn first_unsatisfied_guard_decides_the_abort() {
        let checks =
            [None, Some(ReconcileGuardAbort::AuthorityLost), Some(ReconcileGuardAbort::Busy)];
        assert_eq!(first_reconcile_guard_abort(&checks), Some(ReconcileGuardAbort::AuthorityLost));
    }

    #[test]
    fn satisfied_guards_produce_no_abort() {
        assert_eq!(first_reconcile_guard_abort(&[None, None, None]), None);
        assert_eq!(first_reconcile_guard_abort(&[]), None);
    }

    #[test]
    fn authority_loss_commits_while_refused_locks_roll_back() {
        assert!(!ReconcileGuardAbort::AuthorityLost.aborts_with_rollback());
        assert!(ReconcileGuardAbort::Busy.aborts_with_rollback());
        assert!(ReconcileGuardAbort::Superseded.aborts_with_rollback());
    }
}
