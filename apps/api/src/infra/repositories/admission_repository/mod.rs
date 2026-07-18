mod content;
mod materialization;
mod types;
mod web;

pub use content::admit_content_with_failpoint;
pub use materialization::admit_web_capture_materialization_with_failpoint;
pub use types::{
    AdmissionError, AdmissionFailpoint, AdmissionIngestJobRow, AdmissionOutcome,
    ContentAdmissionBundle, ContentAdmissionRequest, ContentAdmissionTarget, RevisionAdmission,
    WebCaptureMaterializationAdmissionBundle, WebCaptureMaterializationAdmissionRequest,
    WebRunAdmissionBundle, WebRunAdmissionRequest,
};
pub use web::admit_web_run_with_failpoint;

use sha2::{Digest as _, Sha256};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use self::types::{AdmissionIngestJobRow as JobRow, MutationIdentityRow};
use super::{content_repository, ops_repository};

const MUTATION_COLUMNS: &str = "
    id,
    workspace_id,
    library_id,
    operation_kind::text as operation_kind,
    requested_by_principal_id,
    request_surface::text as request_surface,
    idempotency_key,
    source_identity,
    mutation_state::text as mutation_state,
    requested_at,
    completed_at,
    failure_code,
    conflict_code,
    request_fingerprint";

async fn lock_library(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    library_id: Uuid,
) -> Result<(), AdmissionError> {
    let locked = sqlx::query_scalar::<_, Uuid>(
        "select id
         from catalog_library
         where id = $1 and workspace_id = $2
         for no key update",
    )
    .bind(library_id)
    .bind(workspace_id)
    .fetch_optional(&mut **transaction)
    .await?;
    if locked.is_none() {
        return Err(AdmissionError::InvariantViolation("workspace/library scope does not exist"));
    }
    Ok(())
}

fn idempotency_scope(workspace_id: Uuid, principal_id: Option<Uuid>) -> String {
    principal_id.map_or_else(
        || format!("v1:system:{workspace_id}"),
        |principal_id| format!("v1:principal:{principal_id}"),
    )
}

fn fingerprint(value: &serde_json::Value) -> Result<String, AdmissionError> {
    let encoded = serde_json::to_vec(value)?;
    Ok(format!("v1:sha256:{}", hex::encode(Sha256::digest(encoded))))
}

fn check_failpoint(
    requested: Option<AdmissionFailpoint>,
    current: AdmissionFailpoint,
) -> Result<(), AdmissionError> {
    if requested == Some(current) {
        return Err(AdmissionError::InjectedFailure(current));
    }
    Ok(())
}

async fn find_mutation_for_update(
    transaction: &mut Transaction<'_, Postgres>,
    scope: &str,
    request_surface: &str,
    idempotency_key: Option<&str>,
) -> Result<Option<MutationIdentityRow>, AdmissionError> {
    let Some(idempotency_key) = idempotency_key else {
        return Ok(None);
    };
    let query = format!(
        "select {MUTATION_COLUMNS}
         from content_mutation
         where idempotency_scope = $1
           and request_surface = $2::surface_kind
           and idempotency_key = $3
         for update"
    );
    Ok(sqlx::query_as::<_, MutationIdentityRow>(sqlx::AssertSqlSafe(query))
        .bind(scope)
        .bind(request_surface)
        .bind(idempotency_key)
        .fetch_optional(&mut **transaction)
        .await?)
}

struct NewMutation<'a> {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub operation_kind: &'a str,
    pub requested_by_principal_id: Option<Uuid>,
    pub request_surface: &'a str,
    pub idempotency_key: Option<&'a str>,
    pub source_identity: Option<&'a str>,
    pub idempotency_scope: &'a str,
    pub request_fingerprint: &'a str,
}

async fn insert_mutation(
    transaction: &mut Transaction<'_, Postgres>,
    input: NewMutation<'_>,
) -> Result<content_repository::ContentMutationRow, AdmissionError> {
    let query = format!(
        "insert into content_mutation (
            id, workspace_id, library_id, operation_kind,
            requested_by_principal_id, request_surface, idempotency_key,
            source_identity, mutation_state, requested_at,
            idempotency_scope, request_fingerprint
         ) values (
            $1, $2, $3, $4::content_mutation_operation_kind,
            $5, $6::surface_kind, $7, $8, 'accepted', now(), $9, $10
         )
         returning {MUTATION_COLUMNS}"
    );
    let mutation = sqlx::query_as::<_, MutationIdentityRow>(sqlx::AssertSqlSafe(query))
        .bind(Uuid::now_v7())
        .bind(input.workspace_id)
        .bind(input.library_id)
        .bind(input.operation_kind)
        .bind(input.requested_by_principal_id)
        .bind(input.request_surface)
        .bind(input.idempotency_key)
        .bind(input.source_identity)
        .bind(input.idempotency_scope)
        .bind(input.request_fingerprint)
        .fetch_one(&mut **transaction)
        .await?;
    Ok(mutation.into_row())
}

async fn load_operation_by_subject(
    transaction: &mut Transaction<'_, Postgres>,
    subject_kind: &str,
    subject_id: Uuid,
) -> Result<Option<ops_repository::OpsAsyncOperationRow>, AdmissionError> {
    Ok(sqlx::query_as::<_, ops_repository::OpsAsyncOperationRow>(
        "select
            id, workspace_id, library_id, operation_kind,
            surface_kind::text as surface_kind, status::text as status,
            subject_kind, subject_id, parent_async_operation_id,
            created_at, completed_at, failure_code
         from ops_async_operation
         where subject_kind = $1 and subject_id = $2
         order by created_at desc, id desc
         limit 1
         for update",
    )
    .bind(subject_kind)
    .bind(subject_id)
    .fetch_optional(&mut **transaction)
    .await?)
}

struct NewOperation<'a> {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub operation_kind: &'a str,
    pub surface_kind: &'a str,
    pub requested_by_principal_id: Option<Uuid>,
    pub subject_kind: &'a str,
    pub subject_id: Uuid,
    pub parent_async_operation_id: Option<Uuid>,
}

async fn insert_operation(
    transaction: &mut Transaction<'_, Postgres>,
    input: NewOperation<'_>,
) -> Result<ops_repository::OpsAsyncOperationRow, AdmissionError> {
    Ok(sqlx::query_as::<_, ops_repository::OpsAsyncOperationRow>(
        "insert into ops_async_operation (
            id, workspace_id, library_id, operation_kind, surface_kind,
            requested_by_principal_id, status, subject_kind, subject_id,
            parent_async_operation_id, created_at
         ) values ($1, $2, $3, $4, $5::surface_kind, $6, 'accepted', $7, $8, $9, now())
         returning
            id, workspace_id, library_id, operation_kind,
            surface_kind::text as surface_kind, status::text as status,
            subject_kind, subject_id, parent_async_operation_id,
            created_at, completed_at, failure_code",
    )
    .bind(Uuid::now_v7())
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.operation_kind)
    .bind(input.surface_kind)
    .bind(input.requested_by_principal_id)
    .bind(input.subject_kind)
    .bind(input.subject_id)
    .bind(input.parent_async_operation_id)
    .fetch_one(&mut **transaction)
    .await?)
}

async fn load_job(
    transaction: &mut Transaction<'_, Postgres>,
    mutation_id: Uuid,
    job_kind: &str,
) -> Result<Option<JobRow>, AdmissionError> {
    Ok(sqlx::query_as::<_, JobRow>(
        "select
            id, mutation_id, mutation_item_id, async_operation_id,
            knowledge_document_id, knowledge_revision_id,
            job_kind::text as job_kind, dedupe_key
         from ingest_job
         where mutation_id = $1 and job_kind = $2::ingest_job_kind
         order by queued_at asc, id asc
         limit 1
         for update",
    )
    .bind(mutation_id)
    .bind(job_kind)
    .fetch_optional(&mut **transaction)
    .await?)
}

struct NewJob<'a> {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub mutation_id: Uuid,
    pub mutation_item_id: Option<Uuid>,
    pub async_operation_id: Uuid,
    pub knowledge_document_id: Option<Uuid>,
    pub knowledge_revision_id: Option<Uuid>,
    pub job_kind: &'a str,
    pub priority: i32,
    pub dedupe_key: &'a str,
}

async fn insert_job(
    transaction: &mut Transaction<'_, Postgres>,
    input: NewJob<'_>,
) -> Result<JobRow, AdmissionError> {
    Ok(sqlx::query_as::<_, JobRow>(
        "insert into ingest_job (
            id, workspace_id, library_id, mutation_id, mutation_item_id,
            connector_id, async_operation_id, knowledge_document_id,
            knowledge_revision_id, job_kind, queue_state, priority,
            dedupe_key, queued_at, available_at
         ) values (
            $1, $2, $3, $4, $5, null, $6, $7, $8,
            $9::ingest_job_kind, 'queued', $10, $11, now(), now()
         )
         returning
            id, mutation_id, mutation_item_id, async_operation_id,
            knowledge_document_id, knowledge_revision_id,
            job_kind::text as job_kind, dedupe_key",
    )
    .bind(Uuid::now_v7())
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.mutation_id)
    .bind(input.mutation_item_id)
    .bind(input.async_operation_id)
    .bind(input.knowledge_document_id)
    .bind(input.knowledge_revision_id)
    .bind(input.job_kind)
    .bind(input.priority)
    .bind(input.dedupe_key)
    .fetch_one(&mut **transaction)
    .await?)
}
