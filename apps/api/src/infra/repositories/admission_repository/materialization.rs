use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::{
    AdmissionError, AdmissionFailpoint, AdmissionOutcome, ContentAdmissionBundle,
    ContentAdmissionRequest, ContentAdmissionTarget, RevisionAdmission,
    WebCaptureMaterializationAdmissionBundle, WebCaptureMaterializationAdmissionRequest,
    check_failpoint, idempotency_scope, lock_library,
};
use crate::infra::repositories::content_mutation_repository::{
    self, ContentMutationRow, NewContentMutationItem,
};
use crate::infra::repositories::content_revision_repository;

/// Duplicate skips remain owned by discovery and never acquire a revision or
/// payload/readability authority. The caller retains commit ownership.
async fn admit_web_duplicate_skip(
    transaction: &mut Transaction<'_, Postgres>,
    request: &WebCaptureMaterializationAdmissionRequest,
    mutation_id: Uuid,
    failpoint: Option<AdmissionFailpoint>,
) -> Result<Option<AdmissionOutcome<WebCaptureMaterializationAdmissionBundle>>, AdmissionError> {
    let Some(document_id) = readable_duplicate(transaction, request).await? else {
        return Ok(None);
    };
    let message = format!("skipped: identical readable content for {}", request.final_url);
    let existing = sqlx::query_as::<_, content_mutation_repository::ContentMutationItemRow>(
        "select id, mutation_id, document_id, base_revision_id, result_revision_id,
            item_state::text as item_state, message from content_mutation_item
         where mutation_id=$1 and document_id=$2 and item_state='skipped'
            and result_revision_id is null and message=$3 for update",
    )
    .bind(mutation_id)
    .bind(document_id)
    .bind(&message)
    .fetch_optional(&mut **transaction)
    .await?;
    let replayed = existing.is_some();
    let item = match existing {
        Some(item) => item,
        None => {
            content_mutation_repository::create_mutation_item_with_executor(
                &mut **transaction,
                &NewContentMutationItem {
                    mutation_id,
                    document_id,
                    base_revision_id: None,
                    result_revision_id: None,
                    item_state: "skipped",
                    message: Some(&message),
                },
            )
            .await?
        }
    };
    check_failpoint(failpoint, AdmissionFailpoint::AfterMutationItem)?;
    let bundle = WebCaptureMaterializationAdmissionBundle::DuplicateContent {
        existing_document_id: document_id,
        item,
    };
    Ok(Some(if replayed {
        AdmissionOutcome::Replayed(bundle)
    } else {
        AdmissionOutcome::Created(bundle)
    }))
}

#[derive(sqlx::FromRow)]
struct MaterializationRevisionProjectionRow {
    revision_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_number: i64,
    revision_state: String,
    revision_kind: String,
    storage_ref: Option<String>,
    source_uri: Option<String>,
    document_hint: Option<String>,
    mime_type: String,
    checksum: String,
    title: Option<String>,
    byte_size: i64,
    text_state: String,
    vector_state: String,
    graph_state: String,
    superseded_by_revision_id: Option<Uuid>,
}

async fn load_original_page_identity(
    transaction: &mut Transaction<'_, Postgres>,
    key: super::MutationKey<'_>,
    parent_operation_id: Uuid,
) -> Result<Option<serde_json::Value>, AdmissionError> {
    let Some(owner) = super::find_mutation_for_update(transaction, key).await? else {
        return Ok(None);
    };
    let identity = sqlx::query_scalar(
        "select m.request_identity from content_mutation m
         join ops_async_operation o on o.subject_kind = 'content_mutation' and o.subject_id = m.id
         where m.id = $1 and o.parent_async_operation_id = $2",
    )
    .bind(owner.id)
    .bind(parent_operation_id)
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(AdmissionError::IdempotencyConflict { mutation_id: owner.id })?;
    Ok(Some(identity))
}

/// Admits a fetched snapshot through the canonical content owner while its web
/// parent is locked. Each page has its own immutable descriptor and operation;
/// the discovery mutation never acquires a mutable, run-wide replacement.
pub async fn admit_web_capture_materialization_with_failpoint(
    postgres: &sqlx::PgPool,
    request: &WebCaptureMaterializationAdmissionRequest,
    failpoint: Option<AdmissionFailpoint>,
) -> Result<AdmissionOutcome<WebCaptureMaterializationAdmissionBundle>, AdmissionError> {
    let replacement = request.replacement.clone().normalize()?;
    if replacement.source_uri.as_deref() != Some(request.final_url.as_str()) {
        return Err(AdmissionError::InvariantViolation(
            "web snapshot source URI differs from final URL",
        ));
    }
    let mut transaction = postgres.begin().await?;
    lock_library(&mut transaction, request.workspace_id, request.library_id).await?;
    let (parent, parent_operation_id, run_state) =
        lock_web_parent(&mut transaction, request).await?;
    let key = page_idempotency_key(parent.id, &request.final_url);
    let scope = idempotency_scope(request.workspace_id, request.requested_by_principal_id);
    let original = load_original_page_identity(
        &mut transaction,
        super::MutationKey {
            workspace_id: request.workspace_id,
            principal_id: request.requested_by_principal_id,
            scope: &scope,
            surface: &parent.request_surface,
            key: Some(&key),
        },
        parent_operation_id,
    )
    .await?;
    let target = if let Some(identity) = original.as_ref() {
        replay_target(identity)?
    } else {
        ensure_open_web_parent(&parent.mutation_state, &run_state)?;
        if let Some(outcome) =
            admit_web_duplicate_skip(&mut transaction, request, parent.id, failpoint).await?
        {
            check_failpoint(failpoint, AdmissionFailpoint::BeforeCommit)?;
            transaction.commit().await?;
            return Ok(outcome);
        }
        let document_id: Option<Uuid> = sqlx::query_scalar(
            "select id from content_document where workspace_id = $1 and library_id = $2
             and external_key = $3 and document_state = 'active' and deleted_at is null for update",
        )
        .bind(request.workspace_id)
        .bind(request.library_id)
        .bind(&request.final_url)
        .fetch_optional(&mut *transaction)
        .await?;
        document_id.map_or_else(
            || ContentAdmissionTarget::New {
                external_key: Some(request.final_url.clone()),
                file_name: Some(replacement.file_name.clone()),
                parent_external_key: None,
            },
            |document_id| ContentAdmissionTarget::Existing { document_id },
        )
    };
    let content_request = ContentAdmissionRequest {
        workspace_id: request.workspace_id,
        library_id: request.library_id,
        operation_kind: "web_capture".to_owned(),
        requested_by_principal_id: request.requested_by_principal_id,
        request_surface: parent.request_surface.clone(),
        idempotency_key: Some(key),
        source_identity: Some(request.final_url.clone()),
        target,
        revision: Some(RevisionAdmission {
            content_source_kind: "web_page".to_owned(),
            replacement,
            visibility_intent_id: None,
            base_revision_id: None,
            storage_key: Some(request.storage_key.clone()),
        }),
        parent_async_operation_id: Some(parent_operation_id),
        priority: request.priority,
    };
    let outcome =
        super::content::admit_content_in_transaction(&mut transaction, &content_request, failpoint)
            .await?;
    let replayed = matches!(outcome, AdmissionOutcome::Replayed(_));
    let bundle = outcome.into_bundle();
    ensure_materialization_owner(&bundle, request, parent_operation_id)?;
    let revision = bundle
        .revision
        .as_ref()
        .ok_or(AdmissionError::InvariantViolation("web child revision missing"))?;
    ensure_materialization_revision_matches(revision, request)?;
    let projection = sqlx::query_as::<_, MaterializationRevisionProjectionRow>(
        "select revision_id, workspace_id, library_id, document_id, revision_number,
                revision_state, revision_kind, storage_ref, source_uri, document_hint,
                mime_type, checksum, title, byte_size, text_state, vector_state, graph_state,
                superseded_by_revision_id
         from knowledge_revision where revision_id = $1 for share",
    )
    .bind(revision.id)
    .fetch_one(&mut *transaction)
    .await?;
    ensure_revision_projection_matches(revision, &projection)?;
    if replayed {
        ensure_replay_owner_state(
            &parent.mutation_state,
            &run_state,
            &bundle
                .item
                .as_ref()
                .ok_or(AdmissionError::InvariantViolation("web child item missing"))?
                .item_state,
        )?;
    } else {
        ensure_revision_projection_is_pending(&projection)?;
    }
    check_failpoint(failpoint, AdmissionFailpoint::BeforeCommit)?;
    transaction.commit().await?;
    let bundle = WebCaptureMaterializationAdmissionBundle::Ingested(Box::new(bundle));
    Ok(if replayed {
        AdmissionOutcome::Replayed(bundle)
    } else {
        AdmissionOutcome::Created(bundle)
    })
}

fn ensure_materialization_owner(
    bundle: &ContentAdmissionBundle,
    request: &WebCaptureMaterializationAdmissionRequest,
    parent_operation_id: Uuid,
) -> Result<(), AdmissionError> {
    if !bundle.is_complete()
        || bundle.async_operation.parent_async_operation_id != Some(parent_operation_id)
        || bundle.mutation.workspace_id != request.workspace_id
        || bundle.mutation.library_id != request.library_id
        || bundle.mutation.requested_by_principal_id != request.requested_by_principal_id
        || bundle.document.workspace_id != request.workspace_id
        || bundle.document.library_id != request.library_id
        || bundle.revision.is_none()
    {
        return Err(AdmissionError::InvariantViolation("web child admission ownership disagrees"));
    }
    Ok(())
}

fn page_idempotency_key(parent_id: Uuid, final_url: &str) -> String {
    use sha2::{Digest as _, Sha256};
    format!("web-page:{parent_id}:{}", hex::encode(Sha256::digest(final_url.as_bytes())))
}

fn replay_target(identity: &serde_json::Value) -> Result<ContentAdmissionTarget, AdmissionError> {
    let target = &identity["target"];
    match target["kind"].as_str() {
        Some("new") => Ok(ContentAdmissionTarget::New {
            external_key: Some(
                target["externalKey"]
                    .as_str()
                    .ok_or(AdmissionError::InvariantViolation("web replay external key missing"))?
                    .to_owned(),
            ),
            file_name: target["fileName"].as_str().map(ToOwned::to_owned),
            parent_external_key: None,
        }),
        Some("existing") => Ok(ContentAdmissionTarget::Existing {
            document_id: target["documentId"]
                .as_str()
                .and_then(|id| Uuid::parse_str(id).ok())
                .ok_or(AdmissionError::InvariantViolation(
                    "web replay document identity missing",
                ))?,
        }),
        _ => Err(AdmissionError::InvariantViolation("web replay target is not canonical")),
    }
}

async fn lock_web_parent(
    transaction: &mut Transaction<'_, Postgres>,
    request: &WebCaptureMaterializationAdmissionRequest,
) -> Result<(ContentMutationRow, Uuid, String), AdmissionError> {
    let query = format!(
        "select {} from content_mutation m join content_mutation_reference r on r.id=m.id where m.id = $1 for update of m",
        super::MUTATION_COLUMNS
    );
    let mutation = sqlx::query_as::<_, ContentMutationRow>(sqlx::AssertSqlSafe(query))
        .bind(request.mutation_id)
        .fetch_optional(&mut **transaction)
        .await?
        .ok_or(AdmissionError::InvariantViolation("web parent mutation missing"))?;
    if mutation.workspace_id != request.workspace_id
        || mutation.library_id != request.library_id
        || mutation.requested_by_principal_id != request.requested_by_principal_id
        || mutation.operation_kind != "web_capture"
    {
        return Err(AdmissionError::InvariantViolation("web parent is outside request scope"));
    }
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "select o.id, r.run_state::text from content_web_ingest_run r
         join ops_async_operation o on o.id = r.async_operation_id
         where r.mutation_id = $1 and r.workspace_id = $2 and r.library_id = $3
           and r.requested_by_principal_id is not distinct from $4
           and o.workspace_id = r.workspace_id and o.library_id = r.library_id
           and o.subject_kind = 'content_web_ingest_run' and o.subject_id = r.id
           and o.parent_async_operation_id is null for share of r, o",
    )
    .bind(mutation.id)
    .bind(request.workspace_id)
    .bind(request.library_id)
    .bind(request.requested_by_principal_id)
    .fetch_all(&mut **transaction)
    .await?;
    let [(operation_id, state)] = rows.as_slice() else {
        return Err(AdmissionError::InvariantViolation(
            "web parent operation/run ownership disagrees",
        ));
    };
    Ok((mutation, *operation_id, state.clone()))
}

fn ensure_open_web_parent(mutation_state: &str, run_state: &str) -> Result<(), AdmissionError> {
    if !matches!(mutation_state, "accepted" | "running")
        || !matches!(run_state, "accepted" | "discovering" | "processing")
    {
        return Err(AdmissionError::InvariantViolation("web parent no longer accepts pages"));
    }
    Ok(())
}

fn ensure_replay_owner_state(
    mutation_state: &str,
    run_state: &str,
    item_state: &str,
) -> Result<(), AdmissionError> {
    if item_state == "pending" {
        return ensure_open_web_parent(mutation_state, run_state);
    }
    if matches!(
        (mutation_state, run_state),
        ("accepted" | "running", "accepted" | "discovering" | "processing")
            | ("applied", "completed" | "completed_partial")
            | ("failed", "failed")
            | ("canceled", "canceled")
    ) {
        return Ok(());
    }
    Err(AdmissionError::InvariantViolation("replayed web parent states disagree"))
}

fn ensure_materialization_revision_matches(
    revision: &content_revision_repository::ContentRevisionRow,
    request: &WebCaptureMaterializationAdmissionRequest,
) -> Result<(), AdmissionError> {
    let replacement = request.replacement.clone().normalize()?;
    if revision.workspace_id != request.workspace_id
        || revision.library_id != request.library_id
        || revision.created_by_principal_id != request.requested_by_principal_id
        || revision.content_source_kind != "web_page"
        || revision.checksum != replacement.checksum
        || revision.mime_type != replacement.mime_type
        || revision.byte_size != replacement.byte_size
        || revision.title != replacement.title
        || revision.language_code != replacement.language_code
        || revision.source_uri != replacement.source_uri
        || revision.document_hint != replacement.document_hint
    {
        return Err(AdmissionError::InvariantViolation(
            "web child revision differs from immutable snapshot",
        ));
    }
    Ok(())
}

fn ensure_revision_projection_matches(
    revision: &content_revision_repository::ContentRevisionRow,
    projection: &MaterializationRevisionProjectionRow,
) -> Result<(), AdmissionError> {
    if projection.revision_id != revision.id
        || projection.document_id != revision.document_id
        || projection.workspace_id != revision.workspace_id
        || projection.library_id != revision.library_id
        || projection.revision_number != i64::from(revision.revision_number)
        || projection.revision_kind != revision.content_source_kind
        || projection.storage_ref != revision.storage_key
        || projection.source_uri != revision.source_uri
        || projection.document_hint != revision.document_hint
        || projection.mime_type != revision.mime_type
        || projection.checksum != revision.checksum
        || projection.title != revision.title
        || projection.byte_size != revision.byte_size
    {
        return Err(AdmissionError::InvariantViolation(
            "web child revision and knowledge projection disagree",
        ));
    }
    Ok(())
}

fn ensure_revision_projection_is_pending(
    projection: &MaterializationRevisionProjectionRow,
) -> Result<(), AdmissionError> {
    if projection.revision_state != "accepted"
        || projection.text_state != "accepted"
        || projection.vector_state != "accepted"
        || projection.graph_state != "accepted"
        || projection.superseded_by_revision_id.is_some()
    {
        return Err(AdmissionError::InvariantViolation(
            "web child projection is not pending admission",
        ));
    }
    Ok(())
}

async fn readable_duplicate(
    transaction: &mut Transaction<'_, Postgres>,
    request: &WebCaptureMaterializationAdmissionRequest,
) -> Result<Option<Uuid>, AdmissionError> {
    Ok(sqlx::query_scalar(
        "select d.id from content_document d join content_current_revision c on c.document_id = d.id
         join content_revision r on r.id = c.revision_id
         where d.workspace_id = $1 and d.library_id = $2 and r.checksum = $3
         order by d.created_at, d.id limit 1 for update of d",
    ).bind(request.workspace_id).bind(request.library_id).bind(&request.replacement.checksum)
        .fetch_optional(&mut **transaction).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn revision() -> content_revision_repository::ContentRevisionRow {
        content_revision_repository::ContentRevisionRow {
            id: Uuid::nil(),
            document_id: Uuid::max(),
            workspace_id: Uuid::nil(),
            library_id: Uuid::max(),
            revision_number: 3,
            parent_revision_id: None,
            content_source_kind: "web_page".to_owned(),
            checksum: format!("sha256:{}", "a".repeat(64)),
            mime_type: "text/html".to_owned(),
            byte_size: 256,
            title: Some("Alpha page".to_owned()),
            language_code: None,
            source_uri: Some("https://example.test/alpha".to_owned()),
            document_hint: Some("alpha".to_owned()),
            storage_key: Some("captures/alpha".to_owned()),
            created_by_principal_id: None,
            created_at: chrono::Utc::now(),
        }
    }

    fn projection(
        revision: &content_revision_repository::ContentRevisionRow,
    ) -> MaterializationRevisionProjectionRow {
        MaterializationRevisionProjectionRow {
            revision_id: revision.id,
            workspace_id: revision.workspace_id,
            library_id: revision.library_id,
            document_id: revision.document_id,
            revision_number: i64::from(revision.revision_number),
            revision_state: "accepted".to_owned(),
            revision_kind: revision.content_source_kind.clone(),
            storage_ref: revision.storage_key.clone(),
            source_uri: revision.source_uri.clone(),
            document_hint: revision.document_hint.clone(),
            mime_type: revision.mime_type.clone(),
            checksum: revision.checksum.clone(),
            title: revision.title.clone(),
            byte_size: revision.byte_size,
            text_state: "accepted".to_owned(),
            vector_state: "accepted".to_owned(),
            graph_state: "accepted".to_owned(),
            superseded_by_revision_id: None,
        }
    }

    #[test]
    fn revision_projection_requires_current_identity_match() {
        let revision = revision();
        let projection = projection(&revision);
        assert!(ensure_revision_projection_matches(&revision, &projection).is_ok());
        let mismatched = MaterializationRevisionProjectionRow {
            checksum: "checksum-004".to_owned(),
            ..projection
        };
        assert!(ensure_revision_projection_matches(&revision, &mismatched).is_err());
    }

    #[test]
    fn revision_projection_must_be_pending_in_every_plane() {
        let revision = revision();
        let projection = projection(&revision);
        assert!(ensure_revision_projection_is_pending(&projection).is_ok());
        let already_materialized =
            MaterializationRevisionProjectionRow { graph_state: "ready".to_owned(), ..projection };
        assert!(ensure_revision_projection_is_pending(&already_materialized).is_err());
    }

    #[test]
    fn materialization_owner_requires_active_mutation_and_run() {
        assert!(ensure_open_web_parent("accepted", "processing").is_ok());
        assert!(ensure_open_web_parent("running", "discovering").is_ok());
        assert!(ensure_open_web_parent("applied", "completed").is_err());
        assert!(ensure_open_web_parent("accepted", "failed").is_err());
    }

    #[test]
    fn replayed_owner_states_accept_matching_terminal_outcomes() {
        assert!(ensure_replay_owner_state("applied", "completed", "applied").is_ok());
        assert!(ensure_replay_owner_state("failed", "failed", "failed").is_ok());
        assert!(ensure_replay_owner_state("canceled", "canceled", "canceled").is_ok());
        assert!(ensure_replay_owner_state("applied", "failed", "applied").is_err());
    }

    #[test]
    fn materialization_revision_must_match_its_document_and_scope() {
        let revision = revision();
        let matching = WebCaptureMaterializationAdmissionRequest {
            workspace_id: revision.workspace_id,
            library_id: revision.library_id,
            mutation_id: Uuid::nil(),
            final_url: revision.source_uri.clone().expect("fixture URL"),
            replacement: ironrag_contracts::documents::ReplacementIdentity {
                checksum: revision.checksum.clone(),
                byte_size: revision.byte_size,
                mime_type: revision.mime_type.clone(),
                file_name: "alpha.html".to_owned(),
                title: revision.title.clone(),
                language_code: revision.language_code.clone(),
                source_uri: revision.source_uri.clone(),
                document_hint: revision.document_hint.clone(),
            },
            storage_key: "captures/alpha".to_owned(),
            requested_by_principal_id: revision.created_by_principal_id,
            priority: 0,
        };
        assert!(ensure_materialization_revision_matches(&revision, &matching).is_ok());
        let foreign_workspace =
            WebCaptureMaterializationAdmissionRequest { workspace_id: Uuid::max(), ..matching };
        assert!(ensure_materialization_revision_matches(&revision, &foreign_workspace).is_err());
    }

    #[test]
    fn page_keys_are_stable_and_parent_scoped() {
        let parent = Uuid::from_u128(1);
        let url = "https://example.com/page";
        assert_eq!(page_idempotency_key(parent, url), page_idempotency_key(parent, url));
        assert_ne!(
            page_idempotency_key(parent, url),
            page_idempotency_key(Uuid::from_u128(2), url)
        );
        assert_ne!(
            page_idempotency_key(parent, url),
            page_idempotency_key(parent, "https://example.com/other")
        );
    }

    #[test]
    fn terminal_web_parent_cannot_admit_new_payloads() {
        for state in ["applied", "failed", "canceled", "conflict"] {
            assert!(ensure_open_web_parent(state, "processing").is_err());
        }
        for state in ["completed", "completed_partial", "failed", "canceled"] {
            assert!(ensure_open_web_parent("running", state).is_err());
        }
        assert!(ensure_open_web_parent("running", "processing").is_ok());
    }

    #[test]
    fn replay_target_rejects_missing_owner_identity() {
        assert!(replay_target(&serde_json::json!({})).is_err());
        assert!(
            replay_target(
                &serde_json::json!({"target": {"kind": "existing", "documentId": "invalid"}})
            )
            .is_err()
        );
        assert!(replay_target(&serde_json::json!({"target": {"kind": "new"}})).is_err());
    }
}
