use chrono::{DateTime, Utc};
use sqlx::{Executor, FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct ContentMutationRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub operation_kind: String,
    pub requested_by_principal_id: Option<Uuid>,
    pub request_surface: String,
    pub idempotency_key: Option<String>,
    pub source_identity: Option<String>,
    pub mutation_state: String,
    pub requested_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub failure_code: Option<String>,
    pub conflict_code: Option<String>,
    pub request_fingerprint: Option<String>,
}

macro_rules! content_mutation_projection {
    () => {
        "m.id, m.workspace_id, m.library_id, m.operation_kind::text as operation_kind,
            m.requested_by_principal_id, r.request_surface::text as request_surface,
            r.idempotency_key, m.source_identity, m.mutation_state::text as mutation_state,
            m.requested_at, m.completed_at, m.failure_code, m.conflict_code, m.request_fingerprint"
    };
}
pub(super) use content_mutation_projection;

#[derive(Debug, Clone, FromRow)]
pub struct ContentMutationItemRow {
    pub id: Uuid,
    pub mutation_id: Uuid,
    pub document_id: Option<Uuid>,
    pub base_revision_id: Option<Uuid>,
    pub result_revision_id: Option<Uuid>,
    pub item_state: String,
    pub message: Option<String>,
}

macro_rules! content_mutation_item_projection {
    () => {
        "id,
            mutation_id,
            document_id,
            base_revision_id,
            result_revision_id,
            item_state::text as item_state,
            message"
    };
}

#[derive(Debug, Clone)]
pub struct NewContentMutation<'a> {
    /// Complete immutable request metadata; never reconstructed from mutable rows.
    pub request_identity: &'a serde_json::Value,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub operation_kind: &'a str,
    pub requested_by_principal_id: Option<Uuid>,
    pub request_surface: &'a str,
    pub idempotency_key: Option<&'a str>,
    pub source_identity: Option<&'a str>,
    pub mutation_state: &'a str,
    pub failure_code: Option<&'a str>,
    pub conflict_code: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct NewContentMutationItem<'a> {
    pub mutation_id: Uuid,
    pub document_id: Uuid,
    pub base_revision_id: Option<Uuid>,
    pub result_revision_id: Option<Uuid>,
    pub item_state: &'a str,
    pub message: Option<&'a str>,
}
pub async fn list_mutations_by_library(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<Vec<ContentMutationRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentMutationRow>(concat!(
        "select
            ",
        content_mutation_projection!(),
        "
          from content_mutation m join content_mutation_reference r on r.id=m.id
          where m.library_id = $1
          order by m.requested_at desc"
    ))
    .bind(library_id)
    .fetch_all(postgres)
    .await
}

pub async fn get_mutation_by_id(
    postgres: &PgPool,
    mutation_id: Uuid,
) -> Result<Option<ContentMutationRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentMutationRow>(concat!(
        "select
            ",
        content_mutation_projection!(),
        "
          from content_mutation m join content_mutation_reference r on r.id=m.id
          where m.id = $1"
    ))
    .bind(mutation_id)
    .fetch_optional(postgres)
    .await
}

pub async fn list_mutations_by_ids(
    postgres: &PgPool,
    mutation_ids: &[Uuid],
) -> Result<Vec<ContentMutationRow>, sqlx::Error> {
    if mutation_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, ContentMutationRow>(concat!(
        "select
            ",
        content_mutation_projection!(),
        "
          from content_mutation m join content_mutation_reference r on r.id=m.id
          where m.id = any($1)"
    ))
    .bind(mutation_ids)
    .fetch_all(postgres)
    .await
}

pub(super) struct MutationReferenceReservation<'a> {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub principal_id: Option<Uuid>,
    pub surface: &'a str,
    pub scope: &'a str,
    pub key: Option<&'a str>,
}

pub(super) async fn reserve_mutation_reference(
    transaction: &mut Transaction<'_, Postgres>,
    input: MutationReferenceReservation<'_>,
) -> Result<(), sqlx::Error> {
    lock_mutation_key_namespace(
        transaction,
        input.scope,
        input.surface,
        input.key,
        input.principal_id,
    )
    .await?;
    sqlx::query("insert into content_mutation_reference(id,workspace_id,library_id,request_surface,idempotency_scope,idempotency_key,reservation_principal_id) values($1,$2,$3,$4::surface_kind,$5,$6,$7)")
        .bind(input.id).bind(input.workspace_id).bind(input.library_id).bind(input.surface)
        .bind(input.scope).bind(input.key).bind(input.principal_id)
        .execute(&mut **transaction).await?;
    Ok(())
}

/// Serializes actor then scoped reservations before looking up or inserting a
/// registry owner. JSON preserves tuple boundaries for opaque namespace strings.
pub(super) async fn lock_mutation_key_namespace(
    transaction: &mut Transaction<'_, Postgres>,
    scope: &str,
    surface: &str,
    key: Option<&str>,
    principal_id: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    let Some(key) = key else {
        return Ok(());
    };
    for namespace in [
        serde_json::json!(["actor", principal_id, surface, key]),
        serde_json::json!(["scope", scope, surface, key]),
    ] {
        sqlx::query("select pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(namespace.to_string())
            .execute(&mut **transaction)
            .await?;
    }
    Ok(())
}

pub async fn create_mutation(
    postgres: &PgPool,
    new_mutation: &NewContentMutation<'_>,
) -> Result<ContentMutationRow, sqlx::Error> {
    let request_fingerprint =
        super::admission_repository::fingerprint(new_mutation.request_identity)
            .map_err(|error| sqlx::Error::Encode(Box::new(error)))?;
    let idempotency_scope = super::admission_repository::idempotency_scope(
        new_mutation.workspace_id,
        new_mutation.requested_by_principal_id,
    );
    let mut transaction = postgres.begin().await?;
    let id = Uuid::now_v7();
    reserve_mutation_reference(
        &mut transaction,
        MutationReferenceReservation {
            id,
            workspace_id: new_mutation.workspace_id,
            library_id: new_mutation.library_id,
            principal_id: new_mutation.requested_by_principal_id,
            surface: new_mutation.request_surface,
            scope: &idempotency_scope,
            key: new_mutation.idempotency_key,
        },
    )
    .await?;
    let result = sqlx::query_as::<_, ContentMutationRow>(concat!(
        "with inserted as (insert into content_mutation(id,workspace_id,library_id,operation_kind,
            requested_by_principal_id,source_identity,mutation_state,requested_at,completed_at,
            failure_code,conflict_code,request_identity,request_fingerprint)
        values($1,$2,$3,$4::content_mutation_operation_kind,$5,$6,$7::content_mutation_state,
            now(),null,$8,$9,$10,$11) returning *) select ",
        content_mutation_projection!(),
        " from inserted m join content_mutation_reference r on r.id=m.id"
    ))
    .bind(id)
    .bind(new_mutation.workspace_id)
    .bind(new_mutation.library_id)
    .bind(new_mutation.operation_kind)
    .bind(new_mutation.requested_by_principal_id)
    .bind(new_mutation.source_identity)
    .bind(new_mutation.mutation_state)
    .bind(new_mutation.failure_code)
    .bind(new_mutation.conflict_code)
    .bind(new_mutation.request_identity)
    .bind(request_fingerprint)
    .fetch_one(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(result)
}

pub async fn acquire_content_mutation_lock(
    postgres: &PgPool,
    mutation_id: Uuid,
) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind(format!("content.mutation:{mutation_id}"))
        .execute(&mut *transaction)
        .await?;
    Ok(transaction)
}

pub async fn acquire_content_document_lock(
    postgres: &PgPool,
    document_id: Uuid,
) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind(format!("content.document:{document_id}"))
        .execute(&mut *transaction)
        .await?;
    Ok(transaction)
}

pub async fn acquire_content_library_storage_lock(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    acquire_content_library_storage_lock_in_tx(&mut transaction, library_id).await?;
    Ok(transaction)
}

pub async fn acquire_content_library_storage_lock_in_tx(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind(format!("content.library.storage:{library_id}"))
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

pub async fn release_content_mutation_lock(
    transaction: Transaction<'static, Postgres>,
    _mutation_id: Uuid,
) -> Result<(), sqlx::Error> {
    transaction.commit().await
}

pub async fn release_content_document_lock(
    transaction: Transaction<'static, Postgres>,
    _document_id: Uuid,
) -> Result<(), sqlx::Error> {
    transaction.commit().await
}

pub async fn update_mutation_status(
    postgres: &PgPool,
    mutation_id: Uuid,
    mutation_state: &str,
    completed_at: Option<DateTime<Utc>>,
    failure_code: Option<&str>,
    conflict_code: Option<&str>,
) -> Result<Option<ContentMutationRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentMutationRow>(concat!(
        "update content_mutation m
         set mutation_state = $2::content_mutation_state,
             completed_at = $3,
             failure_code = $4,
             conflict_code = $5
          from content_mutation_reference r where m.id = $1 and r.id=m.id
         returning
            ",
        content_mutation_projection!()
    ))
    .bind(mutation_id)
    .bind(mutation_state)
    .bind(completed_at)
    .bind(failure_code)
    .bind(conflict_code)
    .fetch_optional(postgres)
    .await
}

pub async fn list_mutation_items(
    postgres: &PgPool,
    mutation_id: Uuid,
) -> Result<Vec<ContentMutationItemRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentMutationItemRow>(concat!(
        "select
            ",
        content_mutation_item_projection!(),
        "
         from content_mutation_item
         where mutation_id = $1
         order by id asc"
    ))
    .bind(mutation_id)
    .fetch_all(postgres)
    .await
}

pub async fn get_mutation_item_by_id(
    postgres: &PgPool,
    item_id: Uuid,
) -> Result<Option<ContentMutationItemRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentMutationItemRow>(concat!(
        "select
            ",
        content_mutation_item_projection!(),
        "
         from content_mutation_item
         where id = $1"
    ))
    .bind(item_id)
    .fetch_optional(postgres)
    .await
}

pub async fn create_mutation_item(
    postgres: &PgPool,
    new_item: &NewContentMutationItem<'_>,
) -> Result<ContentMutationItemRow, sqlx::Error> {
    create_mutation_item_with_executor(postgres, new_item).await
}

/// Creates one mutation item on a caller-owned connection or transaction.
///
/// Admission paths use this executor form so an item cannot become visible
/// without its exact queue job and pending document-head ownership.
pub async fn create_mutation_item_with_executor<'e, E>(
    executor: E,
    new_item: &NewContentMutationItem<'_>,
) -> Result<ContentMutationItemRow, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, ContentMutationItemRow>(concat!(
        "insert into content_mutation_item (
            id,
            mutation_id,
            document_id,
            base_revision_id,
            result_revision_id,
            item_state,
            message
        )
        values ($1, $2, $3, $4, $5, $6::content_mutation_item_state, $7)
        returning
            ",
        content_mutation_item_projection!()
    ))
    .bind(Uuid::now_v7())
    .bind(new_item.mutation_id)
    .bind(new_item.document_id)
    .bind(new_item.base_revision_id)
    .bind(new_item.result_revision_id)
    .bind(new_item.item_state)
    .bind(new_item.message)
    .fetch_one(executor)
    .await
}

/// Replacement values for a mutation item; optional fields set SQL NULL when absent.
pub struct MutationItemUpdate<'a> {
    pub item_id: Uuid,
    pub document_id: Option<Uuid>,
    pub base_revision_id: Option<Uuid>,
    pub result_revision_id: Option<Uuid>,
    pub item_state: &'a str,
    pub message: Option<&'a str>,
}

pub async fn update_mutation_item(
    postgres: &PgPool,
    input: MutationItemUpdate<'_>,
) -> Result<Option<ContentMutationItemRow>, sqlx::Error> {
    let MutationItemUpdate {
        item_id,
        document_id,
        base_revision_id,
        result_revision_id,
        item_state,
        message,
    } = input;
    sqlx::query_as::<_, ContentMutationItemRow>(concat!(
        "update content_mutation_item
         set document_id = $2,
             base_revision_id = $3,
             result_revision_id = $4,
             item_state = $5::content_mutation_item_state,
             message = $6
         where id = $1
         returning
            ",
        content_mutation_item_projection!()
    ))
    .bind(item_id)
    .bind(document_id)
    .bind(base_revision_id)
    .bind(result_revision_id)
    .bind(item_state)
    .bind(message)
    .fetch_optional(postgres)
    .await
}

/// Rows settled by one [`reconcile_orphan_content_mutations_once`] sweep.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContentMutationReconcileCounts {
    pub mutations: u64,
    pub items: u64,
    pub operations: u64,
}

/// Bounded, idempotent automatic counterpart of passes 3 and 4 of
/// `scripts/ops/reconcile-nonterminal-owners.sql`: classifies non-terminal
/// `content_mutation` rows with no live ingest job and terminalizes the
/// mutation, its pending items and its async operation.
///
/// A document counts as deleted when `document.document_state = 'deleted'`
/// or `document.deleted_at is not null`; a live document is
/// `document_state <> 'deleted' and deleted_at is null`. Pass 3 mirrors the
/// script exactly — one statement whose materialized `targets` snapshot is
/// read by all three updates, so the
/// `mutation_state IN ('accepted','running')` guard is evaluated against the
/// pre-update state. Classification priority is `document_deleted`, then
/// `superseded_by_newer_revision`, then `reconciled_orphaned_attempt`. Pass 4
/// is the backstop for operations whose subject mutation is already terminal or
/// absent. Both run in one transaction; the state guards make a repeated run
/// over the same backlog report zero.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the mutation rows.
pub async fn reconcile_orphan_content_mutations_once(
    postgres: &PgPool,
    stale_before: DateTime<Utc>,
    batch_limit: i64,
) -> Result<ContentMutationReconcileCounts, sqlx::Error> {
    let limit = batch_limit.max(1);
    let mut transaction = postgres.begin().await?;

    let (mutations, items, operations_from_pass3) = sqlx::query_as::<_, (i64, i64, i64)>(
        "with targets as materialized (
                 select
                     mutation.id as mutation_id,
                     case
                         when exists (
                             select 1
                             from content_mutation_item as item
                             join content_document as document
                               on document.id = item.document_id
                             where item.mutation_id = mutation.id
                               and (document.document_state = 'deleted'
                                    or document.deleted_at is not null)
                         ) then 'document_deleted'
                         when exists (
                             select 1
                             from content_mutation_item as item
                             join content_document as document
                               on document.id = item.document_id
                             where item.mutation_id = mutation.id
                               and document.document_state <> 'deleted'
                               and document.deleted_at is null
                               and exists (
                                   select 1
                                   from content_revision as revision
                                   where revision.document_id = item.document_id
                                     and revision.created_at > mutation.requested_at
                               )
                         ) then 'superseded_by_newer_revision'
                         else 'reconciled_orphaned_attempt'
                     end as failure_class
                 from content_mutation as mutation
                 where mutation.mutation_state in ('accepted', 'running')
                   and mutation.requested_at < $1
                   and not exists (
                       select 1
                       from ingest_job as job
                       where job.mutation_id = mutation.id
                         and job.queue_state in ('queued', 'leased', 'paused')
                         and job.completed_at is null
                   )
                   -- A mutation spanning more than one document cannot be
                   -- classified from a single document's outcome; leave it to
                   -- an explicit operator decision.
                   and (
                       select count(distinct item.document_id)
                       from content_mutation_item as item
                       where item.mutation_id = mutation.id
                   ) <= 1
                 order by mutation.requested_at, mutation.id
                 limit $2
             ),
             updated_mutations as (
                 update content_mutation as mutation
                 set mutation_state = case
                         when targets.failure_class = 'document_deleted' then 'canceled'
                         else 'failed'
                     end::content_mutation_state,
                     completed_at = now(),
                     failure_code = targets.failure_class
                 from targets
                 where mutation.id = targets.mutation_id
                   and mutation.mutation_state in ('accepted', 'running')
                 returning mutation.id
             ),
             updated_items as (
                 update content_mutation_item as item
                 set item_state = case
                         when targets.failure_class = 'document_deleted' then 'skipped'
                         else 'failed'
                     end::content_mutation_item_state,
                     message = coalesce(
                         item.message,
                         case
                             when targets.failure_class = 'document_deleted'
                                 then 'mutation skipped because document was deleted'
                             else 'mutation failed: ' || targets.failure_class
                         end
                     )
                 from targets, content_mutation as mutation
                 where item.mutation_id = targets.mutation_id
                   and mutation.id = item.mutation_id
                   and mutation.mutation_state in ('accepted', 'running')
                   and item.item_state = 'pending'
                 returning item.id
             ),
             updated_ops as (
                 update ops_async_operation as operation
                 set status = case
                         when targets.failure_class = 'superseded_by_newer_revision'
                             then 'superseded'
                         else 'failed'
                     end::ops_async_operation_status,
                     completed_at = now(),
                     failure_code = targets.failure_class
                 from targets, content_mutation as mutation
                 where operation.subject_kind = 'content_mutation'
                   and operation.subject_id = targets.mutation_id
                   and mutation.id = targets.mutation_id
                   and mutation.mutation_state in ('accepted', 'running')
                   and operation.status in ('accepted', 'processing')
                 returning operation.id
             )
             select
                 (select count(*) from updated_mutations) as mutations,
                 (select count(*) from updated_items) as items,
                 (select count(*) from updated_ops) as operations",
    )
    .bind(stale_before)
    .bind(limit)
    .fetch_one(&mut *transaction)
    .await?;

    let operations_from_pass4 = sqlx::query_scalar::<_, i64>(
        "with subjects as materialized (
             select
                 operation.id as operation_id,
                 mutation.mutation_state as mutation_state,
                 mutation.failure_code as mutation_failure_code
             from ops_async_operation as operation
             left join content_mutation as mutation
               on mutation.id = operation.subject_id
             where operation.subject_kind = 'content_mutation'
               and operation.status in ('accepted', 'processing')
               and operation.created_at < $1
               and (
                   mutation.id is null
                   or mutation.mutation_state in ('applied', 'failed', 'conflicted', 'canceled')
               )
             order by operation.created_at, operation.id
             limit $2
         ),
         updated as (
             update ops_async_operation as operation
             set status = case
                     when subjects.mutation_state = 'canceled' then 'canceled'
                     else 'failed'
                 end::ops_async_operation_status,
                 completed_at = now(),
                 failure_code = coalesce(
                     subjects.mutation_failure_code,
                     'reconciled_orphaned_attempt'
                 )
             from subjects
             where operation.id = subjects.operation_id
               and operation.subject_kind = 'content_mutation'
               and operation.status in ('accepted', 'processing')
             returning operation.id
         )
         select count(*) from updated",
    )
    .bind(stale_before)
    .bind(limit)
    .fetch_one(&mut *transaction)
    .await?;

    transaction.commit().await?;

    Ok(ContentMutationReconcileCounts {
        mutations: u64::try_from(mutations).unwrap_or(u64::MAX),
        items: u64::try_from(items).unwrap_or(u64::MAX),
        operations: u64::try_from(operations_from_pass3.saturating_add(operations_from_pass4))
            .unwrap_or(u64::MAX),
    })
}
