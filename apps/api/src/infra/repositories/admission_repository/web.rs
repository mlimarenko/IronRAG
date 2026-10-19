use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::{
    AdmissionError, AdmissionFailpoint, AdmissionOutcome, NewJob, NewMutation, NewOperation,
    WebRunAdmissionBundle, WebRunAdmissionRequest, check_failpoint, find_mutation_for_update,
    fingerprint, idempotency_scope, insert_job, insert_mutation, insert_operation, load_job,
    load_operation_by_subject, lock_library,
};
use crate::infra::repositories::ingest_repository;

pub async fn admit_web_run_with_failpoint(
    postgres: &sqlx::PgPool,
    request: &WebRunAdmissionRequest,
    failpoint: Option<AdmissionFailpoint>,
) -> Result<AdmissionOutcome<WebRunAdmissionBundle>, AdmissionError> {
    let request_identity = web_request_identity(request);
    let request_fingerprint = fingerprint(&request_identity)?;
    let scope = idempotency_scope(request.workspace_id, request.requested_by_principal_id);
    let mut transaction = postgres.begin().await?;
    lock_library(&mut transaction, request.workspace_id, request.library_id).await?;

    if let Some(existing) = find_mutation_for_update(
        &mut transaction,
        super::MutationKey {
            workspace_id: request.workspace_id,
            principal_id: request.requested_by_principal_id,
            scope: &scope,
            surface: &request.request_surface,
            key: request.idempotency_key.as_deref(),
        },
    )
    .await?
    {
        if existing.request_fingerprint.as_deref() != Some(request_fingerprint.as_str()) {
            return Err(AdmissionError::IdempotencyConflict { mutation_id: existing.id });
        }
        let mutation = existing.into_row();
        let bundle = load_web_bundle(&mut transaction, mutation).await?;
        transaction.commit().await?;
        return Ok(AdmissionOutcome::Replayed(bundle));
    }

    let mutation = insert_mutation(
        &mut transaction,
        NewMutation {
            request_identity: &request_identity,
            workspace_id: request.workspace_id,
            library_id: request.library_id,
            operation_kind: "web_capture",
            requested_by_principal_id: request.requested_by_principal_id,
            request_surface: &request.request_surface,
            idempotency_key: request.idempotency_key.as_deref(),
            source_identity: request
                .source_identity
                .as_deref()
                .or(Some(request.normalized_seed_url.as_str())),
            idempotency_scope: &scope,
            request_fingerprint: &request_fingerprint,
        },
    )
    .await?;
    check_failpoint(failpoint, AdmissionFailpoint::AfterMutation)?;

    let run_id = Uuid::now_v7();
    let async_operation = insert_operation(
        &mut transaction,
        NewOperation {
            workspace_id: request.workspace_id,
            library_id: request.library_id,
            operation_kind: "web_capture",
            surface_kind: &request.request_surface,
            requested_by_principal_id: request.requested_by_principal_id,
            subject_kind: "content_web_ingest_run",
            subject_id: run_id,
            parent_async_operation_id: None,
        },
    )
    .await?;
    check_failpoint(failpoint, AdmissionFailpoint::AfterAsyncOperation)?;

    let run =
        insert_web_run(&mut transaction, request, mutation.id, async_operation.id, run_id).await?;
    check_failpoint(failpoint, AdmissionFailpoint::AfterWebRun)?;
    let seed = insert_web_seed(&mut transaction, request, run.id).await?;
    check_failpoint(failpoint, AdmissionFailpoint::AfterWebSeed)?;

    let dedupe_key = format!("web-discovery:{}:{}", mutation.id, run.id);
    let job = insert_job(
        &mut transaction,
        NewJob {
            workspace_id: request.workspace_id,
            library_id: request.library_id,
            mutation_id: mutation.id,
            mutation_item_id: None,
            async_operation_id: async_operation.id,
            knowledge_document_id: None,
            knowledge_revision_id: None,
            job_kind: "web_discovery",
            priority: 40,
            dedupe_key: &dedupe_key,
        },
    )
    .await?;
    check_failpoint(failpoint, AdmissionFailpoint::AfterJob)?;

    let bundle = WebRunAdmissionBundle { mutation, async_operation, run, seed, job: Some(job) };
    if !bundle.is_complete() {
        return Err(AdmissionError::InvariantViolation(
            "new web admission is not internally complete",
        ));
    }
    check_failpoint(failpoint, AdmissionFailpoint::BeforeCommit)?;
    transaction.commit().await?;
    Ok(AdmissionOutcome::Created(bundle))
}

#[cfg(test)]
#[test]
fn semantic_fingerprint_excludes_actor_surface_and_caller_key() -> Result<(), AdmissionError> {
    let mut request = WebRunAdmissionRequest {
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        seed_url: "https://example.invalid/".to_owned(),
        normalized_seed_url: "https://example.invalid/".to_owned(),
        mode: "single_page".to_owned(),
        boundary_policy: "same_host".to_owned(),
        max_depth: 0,
        max_pages: 1,
        crawl_allow_patterns: serde_json::json!([]),
        crawl_block_patterns: serde_json::json!([]),
        materialization_allow_patterns: serde_json::json!([]),
        materialization_block_patterns: serde_json::json!([]),
        requested_by_principal_id: None,
        request_surface: "rest".to_owned(),
        idempotency_key: Some("first-key".to_owned()),
        source_identity: Some("web-source".to_owned()),
    };
    let original = fingerprint(&web_request_identity(&request))?;
    request.requested_by_principal_id = Some(Uuid::now_v7());
    request.request_surface = "mcp".to_owned();
    request.idempotency_key = Some("another-key".to_owned());
    assert_eq!(fingerprint(&web_request_identity(&request))?, original);
    request.max_pages = 2;
    assert_ne!(fingerprint(&web_request_identity(&request))?, original);
    Ok(())
}
fn web_request_identity(request: &WebRunAdmissionRequest) -> serde_json::Value {
    serde_json::json!({
        "workspaceId": request.workspace_id,
        "libraryId": request.library_id,
        "operationKind": "web_capture",
        "sourceIdentity": request.source_identity.as_deref().unwrap_or(&request.normalized_seed_url),
        "seedUrl": request.seed_url,
        "normalizedSeedUrl": request.normalized_seed_url,
        "mode": request.mode,
        "boundaryPolicy": request.boundary_policy,
        "maxDepth": request.max_depth,
        "maxPages": request.max_pages,
        "crawlAllowPatterns": request.crawl_allow_patterns,
        "crawlBlockPatterns": request.crawl_block_patterns,
        "materializationAllowPatterns": request.materialization_allow_patterns,
        "materializationBlockPatterns": request.materialization_block_patterns,
    })
}

async fn insert_web_run(
    transaction: &mut Transaction<'_, Postgres>,
    request: &WebRunAdmissionRequest,
    mutation_id: Uuid,
    async_operation_id: Uuid,
    run_id: Uuid,
) -> Result<ingest_repository::WebIngestRunRow, AdmissionError> {
    Ok(sqlx::query_as::<_, ingest_repository::WebIngestRunRow>(
        "insert into content_web_ingest_run (
            id, mutation_id, async_operation_id, workspace_id, library_id,
            mode, seed_url, normalized_seed_url, boundary_policy,
            max_depth, max_pages, crawl_allow_patterns, crawl_block_patterns,
            materialization_allow_patterns, materialization_block_patterns,
            run_state, requested_by_principal_id, requested_at
         ) values (
            $1, $2, $3, $4, $5, $6::web_ingest_mode, $7, $8,
            $9::web_boundary_policy, $10, $11, $12, $13, $14, $15,
            'accepted', $16, now()
         )
         returning
            id, mutation_id, async_operation_id, workspace_id, library_id,
            mode::text as mode, seed_url, normalized_seed_url,
            boundary_policy::text as boundary_policy, max_depth, max_pages,
            crawl_allow_patterns, crawl_block_patterns,
            materialization_allow_patterns, materialization_block_patterns,
            run_state::text as run_state, requested_by_principal_id,
            requested_at, completed_at, failure_code, cancel_requested_at",
    )
    .bind(run_id)
    .bind(mutation_id)
    .bind(async_operation_id)
    .bind(request.workspace_id)
    .bind(request.library_id)
    .bind(&request.mode)
    .bind(&request.seed_url)
    .bind(&request.normalized_seed_url)
    .bind(&request.boundary_policy)
    .bind(request.max_depth)
    .bind(request.max_pages)
    .bind(&request.crawl_allow_patterns)
    .bind(&request.crawl_block_patterns)
    .bind(&request.materialization_allow_patterns)
    .bind(&request.materialization_block_patterns)
    .bind(request.requested_by_principal_id)
    .fetch_one(&mut **transaction)
    .await?)
}

async fn insert_web_seed(
    transaction: &mut Transaction<'_, Postgres>,
    request: &WebRunAdmissionRequest,
    run_id: Uuid,
) -> Result<ingest_repository::WebDiscoveredPageRow, AdmissionError> {
    Ok(sqlx::query_as::<_, ingest_repository::WebDiscoveredPageRow>(
        "insert into content_web_discovered_page (
            id, run_id, discovered_url, normalized_url, final_url,
            \u{63}anonical_url, depth, referrer_candidate_id, host_classification,
            candidate_state, classification_reason, classification_detail,
            content_type, http_status, snapshot_storage_key,
            discovered_at, updated_at, document_id, result_revision_id,
            mutation_item_id
         ) values (
            $1, $2, $3, $4, null, $4, 0, null, 'same_host',
            'eligible', 'seed_accepted', null, null, null, null,
            now(), now(), null, null, null
         )
         returning
            id, run_id, discovered_url, normalized_url, final_url,
            \u{63}anonical_url, depth, referrer_candidate_id,
            host_classification::text as host_classification,
            candidate_state::text as candidate_state, classification_reason,
            classification_detail, content_type, http_status,
            snapshot_storage_key, discovered_at, updated_at,
            document_id, result_revision_id, mutation_item_id",
    )
    .bind(Uuid::now_v7())
    .bind(run_id)
    .bind(&request.seed_url)
    .bind(&request.normalized_seed_url)
    .fetch_one(&mut **transaction)
    .await?)
}

async fn load_web_bundle(
    transaction: &mut Transaction<'_, Postgres>,
    mutation: crate::infra::repositories::content_mutation_repository::ContentMutationRow,
) -> Result<WebRunAdmissionBundle, AdmissionError> {
    let run = sqlx::query_as::<_, ingest_repository::WebIngestRunRow>(
        "select
            id, mutation_id, async_operation_id, workspace_id, library_id,
            mode::text as mode, seed_url, normalized_seed_url,
            boundary_policy::text as boundary_policy, max_depth, max_pages,
            crawl_allow_patterns, crawl_block_patterns,
            materialization_allow_patterns, materialization_block_patterns,
            run_state::text as run_state, requested_by_principal_id,
            requested_at, completed_at, failure_code, cancel_requested_at
         from content_web_ingest_run
         where mutation_id = $1
         for update",
    )
    .bind(mutation.id)
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(AdmissionError::InvariantViolation("web run is missing"))?;
    let operation = load_operation_by_subject(transaction, "content_web_ingest_run", run.id)
        .await?
        .ok_or(AdmissionError::InvariantViolation("web async operation is missing"))?;
    let seed = sqlx::query_as::<_, ingest_repository::WebDiscoveredPageRow>(
        "select
            id, run_id, discovered_url, normalized_url, final_url,
            \u{63}anonical_url, depth, referrer_candidate_id,
            host_classification::text as host_classification,
            candidate_state::text as candidate_state, classification_reason,
            classification_detail, content_type, http_status,
            snapshot_storage_key, discovered_at, updated_at,
            document_id, result_revision_id, mutation_item_id
         from content_web_discovered_page
         where run_id = $1 and normalized_url = $2
         order by discovered_at asc, id asc
         limit 1
         for update",
    )
    .bind(run.id)
    .bind(&run.normalized_seed_url)
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(AdmissionError::InvariantViolation("web seed is missing"))?;
    let job = load_job(transaction, mutation.id, "web_discovery")
        .await?
        .ok_or(AdmissionError::InvariantViolation("web discovery job is missing"))?;
    let bundle =
        WebRunAdmissionBundle { mutation, async_operation: operation, run, seed, job: Some(job) };
    if !bundle.is_complete() {
        return Err(AdmissionError::InvariantViolation("web admission rows disagree on identity"));
    }
    Ok(bundle)
}
