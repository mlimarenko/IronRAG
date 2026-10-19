use chrono::{DateTime, Utc};
use sqlx::{Executor, FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

// Durable admission determines membership. An item authorizes a new mutation
// publication only; preserving a head never depends on retained attempt history.
macro_rules! published_item_eligibility {
    () => {
        "exists (
            select 1 from content_document d
            join content_revision r on r.document_id=d.id and r.workspace_id=d.workspace_id and r.library_id=d.library_id
            left join knowledge_revision k on k.revision_id=r.id and k.document_id=d.id and k.workspace_id=d.workspace_id and k.library_id=d.library_id
            left join content_document_head h on h.document_id=d.id
            left join content_visibility_intent intent on intent.mutation_item_id=h.visibility_intent_id
            left join content_mutation_item obligation on obligation.id=intent.mutation_item_id and obligation.document_id=d.id
            left join content_mutation intent_mutation on intent_mutation.id=obligation.mutation_id
            where d.id=$1 and r.id=$3
              and d.document_state='active' and d.deleted_at is null
              and r.admission_identity is not null
              and (
                ($7::uuid is null and r.admission_identity->>'origin'='certified_import')
                or (r.admission_identity->>'origin'='mutation' and exists (
                  select 1 from content_mutation_item i
                  join content_mutation payload on payload.id=i.mutation_id
                    and payload.workspace_id=d.workspace_id and payload.library_id=d.library_id
                  where i.id=$7 and i.document_id=d.id and i.result_revision_id=r.id
                    and i.item_state='applied'
                    and r.admission_identity->'replacement'=payload.request_identity->'replacement'
                    and r.admission_identity->'sourceIdentity'=payload.request_identity->'sourceIdentity'
                    and r.admission_identity->'materializationToken'=payload.request_identity->'materializationToken'
                    and r.admission_identity->'visibilityIntentId'=coalesce(to_jsonb(i.visibility_intent_id),'null'::jsonb)
                    and (h.visibility_intent_id is null or payload.operation_kind='replace')
                ))
              )
              and not exists (select 1 from content_revision newer
                  where newer.id=h.readable_revision_id and newer.document_id=d.id and newer.revision_number>r.revision_number)
              and (h.visibility_intent_id is null or (
                r.admission_identity->>'origin'='mutation'
                and r.admission_identity->>'visibilityIntentId'=h.visibility_intent_id::text
                and k.text_state='text_readable' and k.text_readable_at is not null
                and intent.document_id=d.id
                and k.vector_state='ready' and k.vector_ready_at is not null and k.graph_state in ('ready','graph_degraded')
                and r.revision_number>intent.revision_floor
                and intent_mutation.workspace_id=d.workspace_id and intent_mutation.library_id=d.library_id
                and r.admission_identity->'replacement'=intent_mutation.request_identity->'replacement'
                and r.admission_identity->'sourceIdentity'=intent_mutation.request_identity->'sourceIdentity'
                and r.admission_identity->'materializationToken'=intent_mutation.request_identity->'materializationToken'
                and r.admission_identity->'replacement'->>'checksum'=r.checksum
                and r.admission_identity->'replacement'->'byteSize'=to_jsonb(r.byte_size)
                and r.admission_identity->'replacement'->>'mimeType'=r.mime_type
                and length(r.admission_identity->'replacement'->>'fileName')>0
                and (r.admission_identity->'replacement'->>'title') is not distinct from r.title
                and (r.admission_identity->'replacement'->>'languageCode') is not distinct from r.language_code
                and (r.admission_identity->'replacement'->>'sourceUri') is not distinct from r.source_uri
                and (r.admission_identity->'replacement'->>'documentHint') is not distinct from r.document_hint
              ))
        )"
    };
}

/// Tests membership after document/head/item locks; denial is not lost job authority.
pub(crate) async fn publication_item_is_visible(
    transaction: &mut Transaction<'_, Postgres>,
    document_id: Uuid,
    revision_id: Uuid,
    item_id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(concat!(
        "select ",
        published_item_eligibility!(),
        " and ($2::uuid is null and $4::uuid is null and $5::uuid is null and not $6::boolean)"
    ))
    .bind(document_id)
    .bind(Option::<Uuid>::None)
    .bind(revision_id)
    .bind(Option::<Uuid>::None)
    .bind(Option::<Uuid>::None)
    .bind(false)
    .bind(item_id)
    .fetch_one(&mut **transaction)
    .await
}

/// Current intent remains unresolved until its exact bound item is public.
/// Callers use alias `h`. Historical policy and revision chronology cannot
/// discharge the retained current intent; only canonical public membership can.
macro_rules! unresolved_head_suppression {
    () => {
        "h.visibility_intent_id is not null and not exists (
         select 1 from content_current_revision current_revision
         where current_revision.document_id = h.document_id
      )"
    };
}
pub(crate) use unresolved_head_suppression;

macro_rules! null_head_repair_eligibility {
    () => {
        concat!(
            "h.active_revision_id is null and h.readable_revision_id is null
             and (h.latest_mutation_id is null or exists (
                 select 1 from content_mutation latest where latest.id = h.latest_mutation_id
             )) and not (",
            $crate::infra::repositories::content_document_head_repository::unresolved_head_suppression!(),
            ")",
        )
    };
}
pub(crate) use null_head_repair_eligibility;

/// Checks durable suppression before an ordinary reader accesses revision history.
///
/// # Errors
/// Returns database errors rather than granting source access on an unknown state.
pub async fn document_has_unresolved_suppression(
    postgres: &PgPool,
    document_id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(concat!(
        "select exists (select 1 from content_document_head h where h.document_id = $1 and (",
        unresolved_head_suppression!(),
        "))",
    ))
    .bind(document_id)
    .fetch_one(postgres)
    .await
}

#[derive(Debug, Clone, FromRow)]
pub struct ContentDocumentHeadRow {
    pub document_id: Uuid,
    pub visibility_intent_id: Option<Uuid>,
    pub active_revision_id: Option<Uuid>,
    pub readable_revision_id: Option<Uuid>,
    pub latest_mutation_id: Option<Uuid>,
    pub latest_successful_attempt_id: Option<Uuid>,
    pub head_updated_at: DateTime<Utc>,
    pub document_summary: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
pub(crate) struct ContentDocumentReadSnapshot {
    #[sqlx(flatten)]
    pub(crate) head: ContentDocumentHeadRow,
    pub(crate) workspace_id: Uuid,
    pub(crate) library_id: Uuid,
    pub(crate) document_state: String,
    pub(crate) deleted_at: Option<DateTime<Utc>>,
    pub(crate) suppressed: bool,
    pub(crate) source_file_name: Option<String>,
}

impl ContentDocumentReadSnapshot {
    pub(crate) fn is_active(&self) -> bool {
        self.document_state == "active" && self.deleted_at.is_none()
    }

    pub(crate) fn has_same_publication(&self, other: &Self) -> bool {
        self.head.document_id == other.head.document_id
            && self.workspace_id == other.workspace_id
            && self.library_id == other.library_id
            && self.document_state == other.document_state
            && self.deleted_at == other.deleted_at
            && self.suppressed == other.suppressed
            && self.head.readable_revision_id == other.head.readable_revision_id
            && self.head.visibility_intent_id == other.head.visibility_intent_id
    }
}

/// Operational membership and published content are selected in one statement.
/// Absent publication redacts source fields, not the document's progress receipt.
pub(crate) async fn list_document_read_snapshots(
    postgres: &PgPool,
    document_ids: &[Uuid],
) -> Result<Vec<ContentDocumentReadSnapshot>, sqlx::Error> {
    sqlx::query_as(concat!(
        "select d.id as document_id, d.workspace_id, d.library_id,
             d.document_state::text as document_state, d.deleted_at, (",
        unresolved_head_suppression!(),
        ") as suppressed,
            current_revision.revision_id as active_revision_id,
            current_revision.revision_id as readable_revision_id,
            h.visibility_intent_id, h.latest_mutation_id,
            case when current_revision.revision_id is not null
                then h.latest_successful_attempt_id end as latest_successful_attempt_id,
            coalesce(h.head_updated_at, d.created_at) as head_updated_at,
            case when current_revision.revision_id is not null
                then h.document_summary end as document_summary,
            current_revision.source_file_name
         from content_document d
         left join content_document_head h on h.document_id = d.id
         left join content_current_revision current_revision
            on current_revision.document_id = d.id
           and current_revision.workspace_id = d.workspace_id
           and current_revision.library_id = d.library_id
         where d.id = any($1)",
    ))
    .bind(document_ids)
    .fetch_all(postgres)
    .await
}

pub(crate) async fn document_read_snapshot_is_current(
    postgres: &PgPool,
    snapshot: &ContentDocumentReadSnapshot,
) -> Result<bool, sqlx::Error> {
    Ok(list_document_read_snapshots(postgres, &[snapshot.head.document_id])
        .await?
        .first()
        .is_some_and(|current| snapshot.has_same_publication(current)))
}
#[derive(Debug, Clone)]
pub(crate) enum ValidatedDocumentHeadWriteOutcome {
    Updated(ContentDocumentHeadRow),
    DocumentNotFound,
    ReferenceIntegrityViolation,
}
#[derive(Debug, Clone)]
pub struct NewContentDocumentHead {
    /// Ephemeral authority for a new mutation publication, never stored on the head.
    pub publishing_mutation_item_id: Option<Uuid>,
    pub document_id: Uuid,
    pub active_revision_id: Option<Uuid>,
    pub readable_revision_id: Option<Uuid>,
    pub latest_mutation_id: Option<Uuid>,
    pub latest_successful_attempt_id: Option<Uuid>,
}

/// Loads one document-head row without taking a row lock.
///
/// # Errors
/// Returns any `SQLx` error raised while reading the row.
pub async fn get_document_head(
    postgres: &PgPool,
    document_id: Uuid,
) -> Result<Option<ContentDocumentHeadRow>, sqlx::Error> {
    get_document_head_with_executor(postgres, document_id).await
}

/// Loads one document-head row with the supplied executor without taking a row
/// lock, allowing callers to keep the read inside a wider transaction snapshot.
///
/// # Errors
/// Returns any `SQLx` error raised while reading the row.
pub async fn get_document_head_with_executor<'e, E>(
    executor: E,
    document_id: Uuid,
) -> Result<Option<ContentDocumentHeadRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, ContentDocumentHeadRow>(
        "select
            document_id,
            visibility_intent_id,
            active_revision_id,
            readable_revision_id,
            latest_mutation_id,
            latest_successful_attempt_id,
            head_updated_at,
            document_summary
         from content_document_head
         where document_id = $1",
    )
    .bind(document_id)
    .fetch_optional(executor)
    .await
}

/// Loads and locks one document-head row for a lifecycle transaction.
///
/// # Errors
/// Returns any `SQLx` error raised while reading or locking the row.
pub async fn get_document_head_for_update_with_executor<'e, E>(
    executor: E,
    document_id: Uuid,
) -> Result<Option<ContentDocumentHeadRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, ContentDocumentHeadRow>(
        "select
            document_id,
            visibility_intent_id,
            active_revision_id,
            readable_revision_id,
            latest_mutation_id,
            latest_successful_attempt_id,
            head_updated_at,
            document_summary
         from content_document_head
         where document_id = $1
         for update",
    )
    .bind(document_id)
    .fetch_optional(executor)
    .await
}

pub async fn list_document_heads_by_document_ids(
    postgres: &PgPool,
    document_ids: &[Uuid],
) -> Result<Vec<ContentDocumentHeadRow>, sqlx::Error> {
    if document_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, ContentDocumentHeadRow>(
        "select
            document_id,
            visibility_intent_id,
            active_revision_id,
            readable_revision_id,
            latest_mutation_id,
            latest_successful_attempt_id,
            head_updated_at,
            document_summary
         from content_document_head
         where document_id = any($1)",
    )
    .bind(document_ids)
    .fetch_all(postgres)
    .await
}

pub async fn upsert_document_head(
    postgres: &PgPool,
    new_head: &NewContentDocumentHead,
) -> Result<ContentDocumentHeadRow, sqlx::Error> {
    upsert_document_head_with_executor(postgres, new_head).await
}

pub async fn upsert_document_head_with_executor<'e, E>(
    executor: E,
    new_head: &NewContentDocumentHead,
) -> Result<ContentDocumentHeadRow, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    try_upsert_document_head_with_generation_policy(executor, new_head, true)
        .await?
        .ok_or(sqlx::Error::RowNotFound)
}

/// Writes a head inside a wider lifecycle transaction without independently
/// advancing the library generation. The caller must advance it exactly once
/// after every answer-visible source/projection write has succeeded.
pub async fn upsert_document_head_without_generation_with_executor<'e, E>(
    executor: E,
    new_head: &NewContentDocumentHead,
) -> Result<ContentDocumentHeadRow, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    try_upsert_document_head_with_generation_policy(executor, new_head, false)
        .await?
        .ok_or(sqlx::Error::RowNotFound)
}

/// Applies the structurally validated head write and reports why its guarded
/// input CTE produced no row.
///
/// Lifecycle callers must lock the target document before invoking this
/// function. The caller-owned transaction retains that lock while the guarded
/// write locks the library. Therefore an existing document with a rejected
/// revision, mutation, or attempt reference is a persistent integrity
/// violation, not a retryable concurrent-head conflict.
pub(crate) async fn upsert_document_head_without_generation_outcome(
    transaction: &mut Transaction<'_, Postgres>,
    new_head: &NewContentDocumentHead,
) -> Result<ValidatedDocumentHeadWriteOutcome, sqlx::Error> {
    if let Some(head) =
        try_upsert_document_head_with_generation_policy(&mut **transaction, new_head, false).await?
    {
        return Ok(ValidatedDocumentHeadWriteOutcome::Updated(head));
    }

    let document_exists = sqlx::query_scalar::<_, bool>(
        "select exists(select 1 from content_document where id = $1)",
    )
    .bind(new_head.document_id)
    .fetch_one(&mut **transaction)
    .await?;
    Ok(if document_exists {
        ValidatedDocumentHeadWriteOutcome::ReferenceIntegrityViolation
    } else {
        ValidatedDocumentHeadWriteOutcome::DocumentNotFound
    })
}

async fn try_upsert_document_head_with_generation_policy<'e, E>(
    executor: E,
    new_head: &NewContentDocumentHead,
    touch_generation: bool,
) -> Result<Option<ContentDocumentHeadRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    // Parent lock -> prior-head lock -> input validation -> conditional generation
    // update is one statement and one lock order. Foreign revision/mutation/
    // attempt ids therefore cannot be laundered through this denormalized head.
    // Only readable-pointer transitions invalidate answers; shell creation,
    // active work-in-progress pointers, operational ids, and exact no-ops do
    // not churn cache generations.
    sqlx::query_as::<_, ContentDocumentHeadRow>(
        concat!("with locked_document as materialized (
            select
                document.id as document_id,
                document.workspace_id,
                document.library_id
            from content_document as document
            join catalog_library as library on library.id = document.library_id
            where document.id = $1
            for no key update of library
         ), previous_head as materialized (
             select head.document_id, head.readable_revision_id, head.latest_mutation_id
             from content_document_head as head
             join locked_document as document on document.document_id = head.document_id
             for update of head
         ), validated_input as materialized (
             select document.*
             from locked_document as document
             left join previous_head as prior on prior.document_id = document.document_id
            where (
                $2::uuid is null
                or exists (
                    select 1
                    from content_revision as revision
                    where revision.id = $2
                      and revision.document_id = document.document_id
                      and revision.workspace_id = document.workspace_id
                      and revision.library_id = document.library_id
                )
            )
              and (
                $3::uuid is null
                or exists (
                    select 1
                    from content_revision as revision
                    where revision.id = $3
                      and revision.document_id = document.document_id
                      and revision.workspace_id = document.workspace_id
                      and revision.library_id = document.library_id
                )
            )
              and (
                $4::uuid is null
                -- Operational history may lose its item/job or become an audit.
                -- Preserve only this locked head's already accepted pointer;
                -- newly supplied IDs still require document-bound work below.
                or ($4::uuid = prior.latest_mutation_id and exists (
                    select 1
                    from content_mutation_reference as registry
                    where registry.id = $4
                       and registry.workspace_id = document.workspace_id
                       and registry.library_id = document.library_id
                       and (exists (
                           select 1 from content_mutation as mutation
                           where mutation.id = registry.id
                             and mutation.workspace_id = registry.workspace_id
                             and mutation.library_id = registry.library_id
                       ) or exists (
                           select 1 from content_mutation_predecessor_audit as history
                           where history.id = registry.id
                             and history.workspace_id = registry.workspace_id
                             and history.library_id = registry.library_id
                       ))
                ))
                or exists (
                    select 1
                    from content_mutation as mutation
                    where mutation.id = $4
                      and mutation.workspace_id = document.workspace_id
                      and mutation.library_id = document.library_id
                      and (
                        exists (
                            select 1
                            from content_mutation_item as item
                            where item.mutation_id = mutation.id
                              and (
                                item.document_id = document.document_id
                                or exists (
                                    select 1
                                    from content_revision as item_revision
                                    where item_revision.id in (
                                        item.base_revision_id,
                                        item.result_revision_id
                                    )
                                      and item_revision.document_id = document.document_id
                                      and item_revision.workspace_id = document.workspace_id
                                      and item_revision.library_id = document.library_id
                                )
                              )
                        )
                        or exists (
                            select 1
                            from ingest_job as job
                            where job.mutation_id = mutation.id
                              and job.workspace_id = document.workspace_id
                              and job.library_id = document.library_id
                              and (
                                job.knowledge_document_id = document.document_id
                                or exists (
                                    select 1
                                    from content_revision as job_revision
                                    where job_revision.id = job.knowledge_revision_id
                                      and job_revision.document_id = document.document_id
                                      and job_revision.workspace_id = document.workspace_id
                                      and job_revision.library_id = document.library_id
                                )
                              )
                        )
                        or (
                            $2::uuid is null
                            and $3::uuid is null
                            and not exists (
                                select 1
                                from content_mutation_item as item
                                where item.mutation_id = mutation.id
                                  and (
                                    item.document_id is not null
                                    or item.base_revision_id is not null
                                    or item.result_revision_id is not null
                                  )
                            )
                            and not exists (
                                select 1
                                from ingest_job as job
                                where job.mutation_id = mutation.id
                                  and (
                                    job.knowledge_document_id is not null
                                    or job.knowledge_revision_id is not null
                                  )
                            )
                        )
                      )
                )
            )
              and (
                $5::uuid is null
                -- Attempt rows are retention-pruned operational history; a
                -- pointer whose row is gone entirely is tolerated (and written
                -- back as null below), while an attempt that still exists but
                -- fails the ownership join stays a laundering violation.
                or not exists (
                    select 1
                    from ingest_attempt as absent_attempt
                    where absent_attempt.id = $5
                )
                or exists (
                    select 1
                    from ingest_attempt as attempt
                    join ingest_job as job on job.id = attempt.job_id
                    where attempt.id = $5
                      and job.workspace_id = document.workspace_id
                      and job.library_id = document.library_id
                      and (
                        job.knowledge_document_id = document.document_id
                        or exists (
                            select 1
                            from content_revision as job_revision
                            where job_revision.id = job.knowledge_revision_id
                              and job_revision.document_id = document.document_id
                              and job_revision.workspace_id = document.workspace_id
                              and job_revision.library_id = document.library_id
                        )
                        or exists (
                            select 1
                            from content_mutation_item as item
                            where item.id = job.mutation_item_id
                              and item.mutation_id = job.mutation_id
                              and item.document_id = document.document_id
                        )
                      )
                )
            )
              and (($3::uuid is null and $7::uuid is null) or ($3::uuid is not null and ",
              published_item_eligibility!(),
              ") or ($3::uuid is not null and $7::uuid is null and exists (
                  select 1 from previous_head preserved
                  join content_revision revision on revision.id=preserved.readable_revision_id
                  where preserved.document_id=$1 and preserved.readable_revision_id=$3
                    and revision.admission_identity->>'origin' in ('mutation','certified_import')
              )))
              and ($2::uuid is null or not exists (
                  select 1 from content_document_head protected where protected.document_id=$1 and protected.visibility_intent_id is not null
              ) or $2::uuid=$3::uuid)
          ), resolved_attempt_pointer as materialized (
            select
                input.document_id,
                case when exists (
                    select 1
                    from ingest_attempt as attempt
                    join ingest_job as job on job.id = attempt.job_id
                    where attempt.id = $5
                      and job.workspace_id = input.workspace_id
                      and job.library_id = input.library_id
                      and (
                        job.knowledge_document_id = input.document_id
                        or exists (
                            select 1
                            from content_revision as job_revision
                            where job_revision.id = job.knowledge_revision_id
                              and job_revision.document_id = input.document_id
                              and job_revision.workspace_id = input.workspace_id
                              and job_revision.library_id = input.library_id
                        )
                        or exists (
                            select 1
                            from content_mutation_item as item
                            where item.id = job.mutation_item_id
                              and item.mutation_id = job.mutation_id
                              and item.document_id = input.document_id
                        )
                      )
                ) then $5::uuid end as effective_attempt_id
            from validated_input as input
         ), answer_transition as materialized (
            select input.library_id
            from validated_input as input
            left join previous_head as previous on true
            where (
                previous.document_id is null
                and $3::uuid is not null
            )
               or (
                previous.document_id is not null
                and previous.readable_revision_id is distinct from $3::uuid
            )
         ), updated_head as (
         insert into content_document_head (
            document_id,
            active_revision_id,
            readable_revision_id,
            latest_mutation_id,
            latest_successful_attempt_id,
            head_updated_at
        )
        select $1, $2, $3, $4, pointer.effective_attempt_id, now()
        from validated_input
        join resolved_attempt_pointer as pointer
          on pointer.document_id = validated_input.document_id
        left join previous_head on true
        on conflict (document_id) do update
        set active_revision_id = excluded.active_revision_id,
            readable_revision_id = excluded.readable_revision_id,
            latest_mutation_id = excluded.latest_mutation_id,
             latest_successful_attempt_id = excluded.latest_successful_attempt_id,
             document_summary = case
                 when content_document_head.readable_revision_id is not distinct from excluded.readable_revision_id
                 then content_document_head.document_summary else null end,
             head_updated_at = now()
         returning
            document_id,
            visibility_intent_id,
            active_revision_id,
            readable_revision_id,
            latest_mutation_id,
            latest_successful_attempt_id,
            head_updated_at,
            document_summary
         ), bumped_library as (
            update catalog_library as library
            set source_truth_version = greatest(
                    coalesce(library.source_truth_version, 0) + 1,
                    (extract(epoch from clock_timestamp()) * 1000000)::bigint
                )
            from answer_transition, updated_head
            where library.id = answer_transition.library_id
              and $6::boolean
            returning library.id
         )
         select
            head.document_id,
            head.active_revision_id,
            head.readable_revision_id,
            head.latest_mutation_id,
            head.latest_successful_attempt_id,
            head.head_updated_at,
            head.visibility_intent_id,
            head.document_summary
         from updated_head as head
          left join bumped_library on true"),
    )
    .bind(new_head.document_id)
    .bind(new_head.active_revision_id)
    .bind(new_head.readable_revision_id)
    .bind(new_head.latest_mutation_id)
    .bind(new_head.latest_successful_attempt_id)
    .bind(touch_generation)
    .bind(new_head.publishing_mutation_item_id)
    .fetch_optional(executor)
    .await
}

#[cfg(test)]
mod tests {
    use super::{ContentDocumentHeadRow, ContentDocumentReadSnapshot};
    use chrono::Utc;
    use uuid::Uuid;

    fn published_snapshot() -> ContentDocumentReadSnapshot {
        let revision_id = Uuid::new_v4();
        ContentDocumentReadSnapshot {
            head: ContentDocumentHeadRow {
                document_id: Uuid::new_v4(),
                visibility_intent_id: None,
                active_revision_id: Some(revision_id),
                readable_revision_id: Some(revision_id),
                latest_mutation_id: Some(Uuid::new_v4()),
                latest_successful_attempt_id: Some(Uuid::new_v4()),
                head_updated_at: Utc::now(),
                document_summary: None,
            },
            workspace_id: Uuid::new_v4(),
            library_id: Uuid::new_v4(),
            document_state: "active".to_owned(),
            deleted_at: None,
            suppressed: false,
            source_file_name: Some("admitted.txt".to_owned()),
        }
    }

    #[test]
    fn publication_survives_pruned_operational_history() {
        let original = published_snapshot();
        let mut current = original.clone();
        current.head.latest_successful_attempt_id = None;
        current.head.latest_mutation_id = None;
        current.head.active_revision_id = Some(Uuid::new_v4());
        assert!(original.has_same_publication(&current));
    }

    #[test]
    fn publication_changes_with_revision_or_visibility_obligation() {
        let original = published_snapshot();
        let mut current = original.clone();
        current.head.readable_revision_id = Some(Uuid::new_v4());
        assert!(!original.has_same_publication(&current));

        let mut current = original.clone();
        current.head.visibility_intent_id = Some(Uuid::new_v4());
        assert!(!original.has_same_publication(&current));
    }

    #[test]
    fn publication_changes_when_access_is_suppressed_or_deleted() {
        let original = published_snapshot();
        let mut current = original.clone();
        current.suppressed = true;
        assert!(!original.has_same_publication(&current));

        let mut current = original.clone();
        current.deleted_at = Some(Utc::now());
        current.document_state = "deleted".to_owned();
        assert!(!original.has_same_publication(&current));
    }
}
