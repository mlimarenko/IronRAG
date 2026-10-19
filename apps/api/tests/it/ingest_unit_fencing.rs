use crate::temp_postgres_database;

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use temp_postgres_database::TempPostgresDatabase;

use ironrag_backend::{
    app::config::Settings,
    infra::{
        knowledge_plane::{IngestAttemptMutationOutcome, IngestRevisionWriteAuthority},
        repositories::{
            catalog_repository, content_document_repository,
            content_knowledge_projection_repository, content_revision_repository,
            ingest_repository,
        },
    },
};

const STAGE_NAME: &str = "extract_content";

fn sha256_text(value: &str) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(value.as_bytes())))
}

fn authority(
    workspace_id: Uuid,
    library_id: Uuid,
    attempt_id: Uuid,
    revision_id: Uuid,
) -> IngestRevisionWriteAuthority {
    IngestRevisionWriteAuthority { workspace_id, library_id, attempt_id, revision_id }
}

fn unit_artifact(
    content_text: &str,
    content_checksum: &str,
    provider_kind: &str,
    model_name: &str,
) -> ingest_repository::ContentRevisionIngestUnitCompletedArtifact {
    ingest_repository::ContentRevisionIngestUnitCompletedArtifact {
        stage_name: STAGE_NAME.to_string(),
        unit_ordinal: 1,
        unit_kind: "pdf_page_range".to_string(),
        range_start: 1,
        range_end: 2,
        content_text: Some(content_text.to_string()),
        structure_hints_json: Some(serde_json::json!({ "lines": [] })),
        source_metadata_json: Some(serde_json::json!({ "pageCount": 2 })),
        source_map_json: Some(serde_json::json!({ "adapter": "test" })),
        warnings_json: serde_json::json!([]),
        usage_json: serde_json::json!({}),
        provider_kind: Some(provider_kind.to_string()),
        model_name: Some(model_name.to_string()),
        content_checksum: Some(content_checksum.to_string()),
        details_json: serde_json::json!({
            "pageCount": 2,
            "pageBatchSize": 2,
            "streamed": true,
        }),
        elapsed_ms: Some(7),
        started_at: Some(Utc::now()),
    }
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn newest_attempt_recomputes_units_and_stale_attempt_cannot_replace_them() -> Result<()> {
    let settings = Settings::from_env().context("load ingest unit fencing settings")?;
    let database =
        TempPostgresDatabase::create(&settings.database_url, "ingest_unit_fencing").await?;
    let pool = PgPoolOptions::new().max_connections(4).connect(&database.database_url).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;

    let result = async {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            &pool,
            &format!("unit-fence-workspace-{suffix}"),
            "Unit Fence Workspace",
            None,
        )
        .await?;
        let library = catalog_repository::create_library(
            &pool,
            workspace.id,
            &format!("unit-fence-library-{suffix}"),
            "Unit Fence Library",
            None,
            None,
        )
        .await?;
        let document = content_knowledge_projection_repository::create_document_with_projection(
            &pool,
            &content_document_repository::NewContentDocument {
                workspace_id: workspace.id,
                library_id: library.id,
                external_key: &format!("unit-fence-document-{suffix}"),
                document_state: "active",
                created_by_principal_id: None,
                parent_external_key: None,
                parent_document_id: None,
                document_role: "primary",
            },
            Some("unit-fence.pdf"),
        )
        .await?;
        let revision = match content_revision_repository::create_revision_with_projection(
            &pool,
            &content_revision_repository::NewContentRevisionProjection {
                admission_identity: None,
                document_id: document.id,
                workspace_id: workspace.id,
                library_id: library.id,
                content_source_kind: "upload",
                checksum: "sha256:unit-fence-source",
                mime_type: "application/pdf",
                byte_size: 128,
                title: Some("Unit Fence Fixture"),
                language_code: None,
                source_uri: None,
                document_hint: None,
                storage_key: None,
                created_by_principal_id: None,
            },
        )
        .await?
        {
            content_revision_repository::CreateContentRevisionOutcome::Created(revision) => {
                *revision
            }
            outcome => return Err(anyhow!("revision fixture was not created: {outcome:?}")),
        };
        let job = ingest_repository::create_ingest_job(
            &pool,
            &ingest_repository::NewIngestJob {
                workspace_id: workspace.id,
                library_id: library.id,
                mutation_id: None,
                mutation_item_id: None,
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: Some(document.id),
                knowledge_revision_id: Some(revision.id),
                job_kind: "content_mutation".to_string(),
                queue_state: "leased".to_string(),
                priority: 10,
                dedupe_key: Some(format!("unit-fence:{suffix}")),
                queued_at: Some(Utc::now()),
                available_at: Some(Utc::now()),
                completed_at: None,
            },
        )
        .await?;
        let first_attempt = ingest_repository::create_ingest_attempt(
            &pool,
            &ingest_repository::NewIngestAttempt {
                job_id: job.id,
                attempt_number: 1,
                worker_principal_id: None,
                lease_token: Some("unit-fence-first".to_string()),
                knowledge_generation_id: None,
                attempt_state: "leased".to_string(),
                current_stage: Some(STAGE_NAME.to_string()),
                started_at: Some(Utc::now()),
                heartbeat_at: Some(Utc::now()),
                finished_at: None,
                failure_class: None,
                failure_code: None,
                failure_message: None,
                progress_percent: 10,
                retryable: true,
            },
        )
        .await?;
        let first = authority(workspace.id, library.id, first_attempt.id, revision.id);
        let resumable_bytes = "first attempt resumable bytes\nsecond line";
        let resumable_checksum = sha256_text(resumable_bytes);
        let initial = ingest_repository::upsert_content_revision_ingest_unit_completed_fenced(
            &pool,
            first,
            &unit_artifact(resumable_bytes, &resumable_checksum, "first-provider", "first-model"),
        )
        .await?;
        assert!(matches!(initial, IngestAttemptMutationOutcome::Applied(_)));

        let newest_attempt = ingest_repository::create_ingest_attempt(
            &pool,
            &ingest_repository::NewIngestAttempt {
                job_id: job.id,
                attempt_number: 2,
                worker_principal_id: None,
                lease_token: Some("unit-fence-newest".to_string()),
                knowledge_generation_id: None,
                attempt_state: "leased".to_string(),
                current_stage: Some(STAGE_NAME.to_string()),
                started_at: Some(Utc::now()),
                heartbeat_at: Some(Utc::now()),
                finished_at: None,
                failure_class: None,
                failure_code: None,
                failure_message: None,
                progress_percent: 20,
                retryable: true,
            },
        )
        .await?;
        let newest = authority(workspace.id, library.id, newest_attempt.id, revision.id);

        assert_eq!(
            ingest_repository::touch_attempt_heartbeat_and_load_job_state(
                &pool,
                first_attempt.id,
                Some(STAGE_NAME),
            )
            .await?,
            None,
            "an older attempt heartbeat must observe authority loss"
        );
        assert_eq!(
            ingest_repository::touch_attempt_heartbeat_and_load_job_state(
                &pool,
                newest_attempt.id,
                Some(STAGE_NAME),
            )
            .await?
            .as_deref(),
            Some("leased")
        );

        let current_attempt_rows =
            ingest_repository::list_content_revision_ingest_units_fenced(&pool, newest, STAGE_NAME)
                .await?;
        let current_attempt_rows = match current_attempt_rows {
            IngestAttemptMutationOutcome::Applied(rows) => rows,
            IngestAttemptMutationOutcome::AuthorityLost => {
                return Err(anyhow!("newest attempt unexpectedly lost unit authority"));
            }
        };
        assert!(
            current_attempt_rows.is_empty(),
            "a new attempt must not adopt output whose provider/model/config provenance is unbound"
        );

        let newest_bytes = "newest attempt exact bytes\nreplacement";
        let newest_checksum = sha256_text(newest_bytes);
        let newest_write = ingest_repository::upsert_content_revision_ingest_unit_completed_fenced(
            &pool,
            newest,
            &unit_artifact(newest_bytes, &newest_checksum, "new-provider", "new-model"),
        )
        .await?;
        assert!(matches!(newest_write, IngestAttemptMutationOutcome::Applied(_)));

        let stale_write = ingest_repository::upsert_content_revision_ingest_unit_completed_fenced(
            &pool,
            first,
            &unit_artifact(
                "stale bytes must not persist",
                "sha256:stale-checksum",
                "stale-provider",
                "stale-model",
            ),
        )
        .await?;
        assert!(matches!(stale_write, IngestAttemptMutationOutcome::AuthorityLost));
        assert!(matches!(
            ingest_repository::list_content_revision_ingest_units_fenced(&pool, first, STAGE_NAME,)
                .await?,
            IngestAttemptMutationOutcome::AuthorityLost
        ));
        assert!(matches!(
            ingest_repository::sum_completed_content_revision_ingest_unit_pages_fenced(
                &pool, first, STAGE_NAME,
            )
            .await?,
            IngestAttemptMutationOutcome::AuthorityLost
        ));
        assert!(matches!(
            ingest_repository::get_content_revision_ingest_unit_page_progress_fenced(
                &pool, first, STAGE_NAME,
            )
            .await?,
            IngestAttemptMutationOutcome::AuthorityLost
        ));

        let persisted = ingest_repository::list_content_revision_ingest_units_unfenced(
            &pool,
            revision.id,
            STAGE_NAME,
        )
        .await?;
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].attempt_id, Some(newest_attempt.id));
        assert_eq!(persisted[0].content_text.as_deref(), Some(newest_bytes));
        assert_eq!(
            persisted[0].content_text.as_deref().map(str::as_bytes),
            Some(newest_bytes.as_bytes())
        );
        assert_eq!(persisted[0].content_checksum.as_deref(), Some(newest_checksum.as_str()));
        assert_eq!(persisted[0].provider_kind.as_deref(), Some("new-provider"));
        assert_eq!(persisted[0].model_name.as_deref(), Some("new-model"));

        let newest_progress =
            ingest_repository::get_content_revision_ingest_unit_page_progress_fenced(
                &pool, newest, STAGE_NAME,
            )
            .await?;
        match newest_progress {
            IngestAttemptMutationOutcome::Applied(progress) => {
                assert_eq!(progress.completed_page_count, 2);
                assert_eq!(progress.total_page_count, Some(2));
            }
            IngestAttemptMutationOutcome::AuthorityLost => {
                return Err(anyhow!("newest attempt unexpectedly lost progress authority"));
            }
        }

        Ok(())
    }
    .await;
    pool.close().await;
    database.drop().await?;
    result
}
