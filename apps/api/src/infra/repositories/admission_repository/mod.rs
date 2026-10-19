mod content;
mod materialization;
mod types;
mod web;

pub(crate) use content::admit_staged_upload_with_failpoint;
pub use content::{admit_content_with_failpoint, admit_visibility_intent_with_failpoint};
pub use materialization::admit_web_capture_materialization_with_failpoint;
pub use types::{
    AdmissionError, AdmissionFailpoint, AdmissionIngestJobRow, AdmissionOutcome,
    ContentAdmissionBundle, ContentAdmissionRequest, ContentAdmissionTarget, RevisionAdmission,
    VisibilityAdmissionBundle, VisibilityAdmissionRequest,
    WebCaptureMaterializationAdmissionBundle, WebCaptureMaterializationAdmissionRequest,
    WebRunAdmissionBundle, WebRunAdmissionRequest,
};
pub use web::admit_web_run_with_failpoint;

use sha2::{Digest as _, Sha256};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use self::types::{AdmissionIngestJobRow as JobRow, MutationIdentityRow};
use super::{content_mutation_repository, ops_repository};

const MUTATION_COLUMNS: &str = content_mutation_repository::content_mutation_projection!();

pub(super) struct MutationKey<'a> {
    workspace_id: Uuid,
    principal_id: Option<Uuid>,
    scope: &'a str,
    surface: &'a str,
    key: Option<&'a str>,
}

const KEY_OWNERS: &str = "select r.id, r.workspace_id, r.idempotency_scope,
    exists(select 1 from content_mutation_predecessor_audit a where a.id=r.id) as historical
    from content_mutation_reference r where r.request_surface=$1::surface_kind and r.idempotency_key=$2
    and (r.idempotency_scope=$3 or r.reservation_principal_id=$4) order by r.id";

async fn lock_library(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    library_id: Uuid,
) -> Result<(), AdmissionError> {
    let locked = sqlx::query_as::<_, (Uuid, String)>(
        "select id, lifecycle_state::text as lifecycle_state
         from catalog_library
         where id = $1 and workspace_id = $2
         for no key update",
    )
    .bind(library_id)
    .bind(workspace_id)
    .fetch_optional(&mut **transaction)
    .await?;
    let Some((locked_id, lifecycle_state)) = locked else {
        return Err(AdmissionError::InvariantViolation("workspace/library scope does not exist"));
    };
    if lifecycle_state != "active" {
        return Err(AdmissionError::LibraryArchived { library_id: locked_id });
    }
    Ok(())
}

pub(super) fn idempotency_scope(workspace_id: Uuid, principal_id: Option<Uuid>) -> String {
    principal_id.map_or_else(
        || format!("v1:system:{workspace_id}"),
        |principal_id| format!("v1:principal:{workspace_id}:{principal_id}"),
    )
}

/// Finds a bound key only to bypass fresh-content deduplication. Callers must
/// still use atomic admission to validate and replay its immutable request.
pub async fn has_bound_content_key(
    postgres: &sqlx::PgPool,
    workspace_id: Uuid,
    principal_id: Option<Uuid>,
    request_surface: &str,
    key: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let Some(key) = key.map(str::trim).filter(|key| !key.is_empty()) else {
        return Ok(false);
    };
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!("select exists({KEY_OWNERS})")))
        .bind(request_surface)
        .bind(key)
        .bind(idempotency_scope(workspace_id, principal_id))
        .bind(principal_id)
        .fetch_one(postgres)
        .await
}

pub(super) fn fingerprint(value: &serde_json::Value) -> Result<String, AdmissionError> {
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
    key: MutationKey<'_>,
) -> Result<Option<MutationIdentityRow>, AdmissionError> {
    let Some(value) = key.key else {
        return Ok(None);
    };
    content_mutation_repository::lock_mutation_key_namespace(
        transaction,
        key.scope,
        key.surface,
        key.key,
        key.principal_id,
    )
    .await?;
    let owners: Vec<(Uuid, Uuid, Option<String>, bool)> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("{KEY_OWNERS} for update of r")))
            .bind(key.surface)
            .bind(value)
            .bind(key.scope)
            .bind(key.principal_id)
            .fetch_all(&mut **transaction)
            .await?;
    let Some(first) = owners.first() else {
        return Ok(None);
    };
    if owners.len() != 1
        || first.3
        || first.1 != key.workspace_id
        || first.2.as_deref() != Some(key.scope)
    {
        return Err(AdmissionError::IdempotencyConflict { mutation_id: first.0 });
    }
    let query = format!(
        "select {MUTATION_COLUMNS}
         from content_mutation m join content_mutation_reference r on r.id=m.id
         where m.id=$1 for update of m"
    );
    let modern = sqlx::query_as::<_, MutationIdentityRow>(sqlx::AssertSqlSafe(query))
        .bind(first.0)
        .fetch_optional(&mut **transaction)
        .await?;
    modern.map(Some).ok_or(AdmissionError::IdempotencyConflict { mutation_id: first.0 })
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
    pub request_identity: &'a serde_json::Value,
}

async fn insert_mutation(
    transaction: &mut Transaction<'_, Postgres>,
    input: NewMutation<'_>,
) -> Result<content_mutation_repository::ContentMutationRow, AdmissionError> {
    let id = Uuid::now_v7();
    content_mutation_repository::reserve_mutation_reference(
        transaction,
        content_mutation_repository::MutationReferenceReservation {
            id,
            workspace_id: input.workspace_id,
            library_id: input.library_id,
            principal_id: input.requested_by_principal_id,
            surface: input.request_surface,
            scope: input.idempotency_scope,
            key: input.idempotency_key,
        },
    )
    .await?;
    let query = format!(
        "with inserted as (insert into content_mutation (
            id, workspace_id, library_id, operation_kind,
            requested_by_principal_id,
            source_identity, mutation_state, requested_at,
             request_fingerprint, request_identity
         ) values (
            $1, $2, $3, $4::content_mutation_operation_kind,
            $5, $6, 'accepted', now(), $7, $8
         ) returning *) select {MUTATION_COLUMNS} from inserted m
         join content_mutation_reference r on r.id=m.id"
    );
    let mutation = sqlx::query_as::<_, MutationIdentityRow>(sqlx::AssertSqlSafe(query))
        .bind(id)
        .bind(input.workspace_id)
        .bind(input.library_id)
        .bind(input.operation_kind)
        .bind(input.requested_by_principal_id)
        .bind(input.source_identity)
        .bind(input.request_fingerprint)
        .bind(input.request_identity)
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
            created_at, completed_at, failure_code, failure_disposition, result_json
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
            created_at, completed_at, failure_code, failure_disposition, result_json",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idempotency_scope_separates_system_and_principal_requests() {
        let workspace_id = Uuid::nil();
        let principal_id = Uuid::max();

        assert_eq!(
            idempotency_scope(workspace_id, None),
            "v1:system:00000000-0000-0000-0000-000000000000"
        );
        assert_eq!(
            idempotency_scope(workspace_id, Some(principal_id)),
            "v1:principal:00000000-0000-0000-0000-000000000000:ffffffff-ffff-ffff-ffff-ffffffffffff"
        );
    }

    #[test]
    fn fingerprint_is_stable_and_uses_versioned_sha256_format() {
        let value = serde_json::json!({ "target": "alpha", "count": 2 });

        let computed_fingerprint = fingerprint(&value).unwrap();

        assert_eq!(computed_fingerprint, fingerprint(&value).unwrap());
        assert!(computed_fingerprint.starts_with("v1:sha256:"));
        assert_eq!(computed_fingerprint.len(), "v1:sha256:".len() + 64);
    }

    #[test]
    fn check_failpoint_rejects_only_the_requested_stage() {
        assert!(
            check_failpoint(Some(AdmissionFailpoint::AfterJob), AdmissionFailpoint::AfterJob)
                .is_err()
        );
        assert!(
            check_failpoint(Some(AdmissionFailpoint::AfterJob), AdmissionFailpoint::AfterHead)
                .is_ok()
        );
        assert!(check_failpoint(None, AdmissionFailpoint::AfterJob).is_ok());
    }
}
