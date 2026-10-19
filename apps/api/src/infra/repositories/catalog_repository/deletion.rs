use super::*;

pub async fn delete_library(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<CatalogLibraryDeleteOutcome, sqlx::Error> {
    delete_library_scoped(postgres, library_id, None).await
}

/// Deletes one library only while it belongs to `workspace_id`.
///
/// The workspace-restore compensation path captures `(workspace_id, library_id)`
/// when it provisions a library and removes it by the same pair later. A pair
/// whose two coordinates do not belong together is a `NotFound`, so the
/// compensation cannot remove a row outside the workspace it is unwinding even
/// if it is handed a wrong id. It never loads the library row, so it derives no
/// readiness — the property that lets compensation run while that source is
/// down.
pub async fn delete_library_for_restore(
    postgres: &PgPool,
    library_id: Uuid,
    workspace_id: Uuid,
) -> Result<CatalogLibraryDeleteOutcome, sqlx::Error> {
    delete_library_scoped(postgres, library_id, Some(workspace_id)).await
}

/// Durable catalog delete shared by both entry points; the optional
/// `workspace_id` is the exact workspace fence of [`delete_library_for_restore`].
async fn delete_library_scoped(
    postgres: &PgPool,
    library_id: Uuid,
    workspace_id: Option<Uuid>,
) -> Result<CatalogLibraryDeleteOutcome, sqlx::Error> {
    repair_vector_shard_cascades_before_catalog_delete(postgres).await?;
    let mut transaction = postgres.begin().await?;
    sqlx::query(
        "select pg_advisory_xact_lock(
            hashtextextended('ironrag:ai-config-generation', 0)
         )",
    )
    .execute(&mut *transaction)
    .await?;
    let locked_library_id = sqlx::query_scalar::<_, Uuid>(
        "select id
         from catalog_library
         where id = $1
           and ($2::uuid is null or workspace_id = $2)
         for update",
    )
    .bind(library_id)
    .bind(workspace_id)
    .fetch_optional(&mut *transaction)
    .await?;

    if locked_library_id.is_none() {
        transaction.rollback().await?;
        return Ok(CatalogLibraryDeleteOutcome::NotFound);
    }

    let library_ids = [library_id];
    quiesce_locked_library_for_deletion(&mut transaction, library_id).await?;
    let blockers = lock_catalog_delete_blockers(&mut transaction, &library_ids).await?;
    if !blockers.is_empty() {
        // Preserve the quiesced state while durable webhook work drains. A
        // blocked delete must not leave content or repair jobs able to outpace
        // the next deletion attempt.
        transaction.commit().await?;
        return Ok(CatalogLibraryDeleteOutcome::Blocked(blockers));
    }

    delete_locked_catalog_target(
        &mut transaction,
        &library_ids,
        LockedCatalogDeleteTarget::Library(library_id),
    )
    .await?;
    transaction.commit().await?;
    Ok(CatalogLibraryDeleteOutcome::Deleted)
}

pub(super) async fn repair_vector_shard_cascades_before_catalog_delete(
    postgres: &PgPool,
) -> Result<(), sqlx::Error> {
    crate::infra::postgres::pg_vector_config::lock_and_ensure_all_vector_relation_library_cascades(
        postgres,
    )
    .await
    .map_err(|error| {
        sqlx::Error::Protocol(format!(
            "failed to repair vector shard catalog cascades before catalog deletion: {error:#}"
        ))
    })
}

async fn quiesce_locked_library_for_deletion(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update catalog_library
         set lifecycle_state = 'archived'::catalog_library_lifecycle_state,
             updated_at = now()
         where id = $1
           and lifecycle_state <> 'archived'::catalog_library_lifecycle_state",
    )
    .bind(library_id)
    .execute(&mut **transaction)
    .await?;

    sqlx::query(
        "with target_jobs as materialized (
             select job.id
             from ingest_job job
             where job.library_id = $1
               and job.job_kind <> 'webhook_delivery'::ingest_job_kind
               and job.queue_state in ('queued', 'leased', 'paused')
               and job.completed_at is null
             order by job.id
             for update of job
         ),
         jobs_canceled as (
             update ingest_job job
             set queue_state = 'canceled',
                 completed_at = now(),
                 queue_leased_at = null,
                 queue_lease_token = null,
                 queue_lease_owner = null
             where job.id in (select id from target_jobs)
             returning job.id
         )
         update ingest_attempt attempt
         set attempt_state = 'canceled',
             failure_class = 'ingest_queue',
             failure_code = 'library_deleting',
             failure_message = 'Processing was canceled because its library is being deleted',
             retryable = false,
             finished_at = now()
         where attempt.job_id in (select id from jobs_canceled)
           and attempt.attempt_state in ('leased', 'running')",
    )
    .bind(library_id)
    .execute(&mut **transaction)
    .await?;

    Ok(())
}

#[derive(Clone, Copy)]
pub(super) enum LockedCatalogDeleteTarget {
    Workspace(Uuid),
    Library(Uuid),
}

pub(super) async fn delete_locked_catalog_target(
    transaction: &mut Transaction<'_, Postgres>,
    library_ids: &[Uuid],
    target: LockedCatalogDeleteTarget,
) -> Result<(), sqlx::Error> {
    sqlx::query("delete from content_mutation_reference where library_id = any($1)")
        .bind(library_ids)
        .execute(&mut **transaction)
        .await?;

    let (result, missing_parent_message) = match target {
        LockedCatalogDeleteTarget::Workspace(workspace_id) => (
            sqlx::query("delete from catalog_workspace where id = $1")
                .bind(workspace_id)
                .execute(&mut **transaction)
                .await?,
            "locked catalog workspace disappeared before deletion",
        ),
        LockedCatalogDeleteTarget::Library(library_id) => (
            sqlx::query("delete from catalog_library where id = $1")
                .bind(library_id)
                .execute(&mut **transaction)
                .await?,
            "locked catalog library disappeared before deletion",
        ),
    };
    if result.rows_affected() != 1 {
        return Err(sqlx::Error::Protocol(missing_parent_message.to_string()));
    }
    Ok(())
}

pub(super) async fn lock_catalog_delete_blockers(
    transaction: &mut Transaction<'_, Postgres>,
    library_ids: &[Uuid],
) -> Result<CatalogDeleteBlockers, sqlx::Error> {
    if library_ids.is_empty() {
        return Ok(CatalogDeleteBlockers {
            undispatched_webhook_lifecycle_events: 0,
            active_webhook_delivery_jobs: 0,
            unresolved_webhook_delivery_attempts: 0,
        });
    }

    // Lock every unresolved delivery job before evaluating the scope. Parent
    // row locks prevent new FK-backed jobs or outbox rows from being inserted.
    // Only the two non-resumable terminal queue states are allowlisted; a new
    // enum value therefore fails closed until its lifecycle is reviewed here.
    let active_webhook_delivery_jobs = sqlx::query_scalar::<_, i64>(
        "with blockers as materialized (
            select id
            from ingest_job
            where job_kind = 'webhook_delivery'
              and queue_state::text not in ('completed', 'canceled')
              and (
                  library_id = any($1)
                  or id in (
                      select job_id
                      from webhook_delivery_attempt
                      where library_id = any($1) and job_id is not null
                  )
              )
            for update
         )
         select count(*)::bigint from blockers",
    )
    .bind(library_ids)
    .fetch_one(&mut **transaction)
    .await?;
    let unresolved_webhook_delivery_attempts = sqlx::query_scalar::<_, i64>(
        "with blockers as materialized (
            select delivery.id
            from webhook_delivery_attempt delivery
            where delivery.library_id = any($1)
              and (
                  delivery.delivery_state in ('pending', 'delivering')
                  or (
                      delivery.delivery_state = 'failed'
                      and delivery.next_attempt_at is not null
                  )
              )
              and not exists (
                  select 1
                  from ingest_job job
                  where job.id = delivery.job_id
                    and job.job_kind = 'webhook_delivery'
                    and job.queue_state::text not in ('completed', 'canceled')
              )
            for update
         )
         select count(*)::bigint from blockers",
    )
    .bind(library_ids)
    .fetch_one(&mut **transaction)
    .await?;
    let undispatched_webhook_lifecycle_events = sqlx::query_scalar::<_, i64>(
        "with blockers as materialized (
            select id
            from webhook_lifecycle_outbox
            where library_id = any($1)
              and dispatch_state not in ('dispatched', 'resolved')
            for update
         )
         select count(*)::bigint from blockers",
    )
    .bind(library_ids)
    .fetch_one(&mut **transaction)
    .await?;

    Ok(CatalogDeleteBlockers {
        undispatched_webhook_lifecycle_events: usize::try_from(
            undispatched_webhook_lifecycle_events,
        )
        .map_err(|_| sqlx::Error::Protocol("negative webhook outbox blocker count".to_string()))?,
        active_webhook_delivery_jobs: usize::try_from(active_webhook_delivery_jobs).map_err(
            |_| sqlx::Error::Protocol("negative webhook delivery blocker count".to_string()),
        )?,
        unresolved_webhook_delivery_attempts: usize::try_from(unresolved_webhook_delivery_attempts)
            .map_err(|_| {
                sqlx::Error::Protocol("negative webhook delivery-attempt blocker count".to_string())
            })?,
    })
}
