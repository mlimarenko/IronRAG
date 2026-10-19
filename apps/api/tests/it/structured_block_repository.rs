use crate::temp_postgres_database;

use anyhow::Context;
use chrono::Utc;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use temp_postgres_database::TempPostgresDatabase;

use ironrag_backend::{
    app::config::Settings,
    infra::{
        knowledge_plane::DocumentStore,
        knowledge_rows::{
            KnowledgeDocumentRow, KnowledgeRevisionRow, KnowledgeStructuredBlockRow,
            KnowledgeStructuredRevisionRow,
        },
        postgres::pg_document_store::PgDocumentStore,
    },
};

fn structured_revision_row(
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    block_count: i32,
    preparation_state: impl Into<String>,
) -> KnowledgeStructuredRevisionRow {
    let now = Utc::now();
    KnowledgeStructuredRevisionRow {
        revision_id,
        workspace_id,
        library_id,
        document_id,
        preparation_state: preparation_state.into(),
        normalization_profile: "structured-artifact-test-v1".to_string(),
        source_format: "text".to_string(),
        language_code: None,
        block_count,
        chunk_count: 0,
        typed_fact_count: 0,
        outline_json: serde_json::json!({ "blockCount": block_count }),
        prepared_at: now,
        updated_at: now,
    }
}

struct StructuredBlockRowSeed<'a> {
    block_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    ordinal: i32,
    text: &'a str,
}

fn structured_block_row(seed: StructuredBlockRowSeed<'_>) -> KnowledgeStructuredBlockRow {
    let StructuredBlockRowSeed {
        block_id,
        workspace_id,
        library_id,
        document_id,
        revision_id,
        ordinal,
        text,
    } = seed;
    let now = Utc::now();
    let text: String = text.into();
    KnowledgeStructuredBlockRow {
        block_id,
        workspace_id,
        library_id,
        document_id,
        revision_id,
        ordinal,
        block_kind: "paragraph".to_string(),
        normalized_text: text.clone(),
        text,
        heading_trail: Vec::new(),
        section_path: Vec::new(),
        page_number: None,
        span_start: Some(ordinal),
        span_end: Some(ordinal.saturating_add(1)),
        parent_block_id: None,
        table_coordinates_json: None,
        code_language: None,
        created_at: now,
        updated_at: now,
    }
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn structured_revision_artifact_replacement_is_atomic_scoped_and_batched()
-> anyhow::Result<()> {
    let settings = Settings::from_env().context("load structured block replacement settings")?;
    let temp_database =
        TempPostgresDatabase::create(&settings.database_url, "structured_block_repository").await?;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&temp_database.database_url)
        .await
        .context("connect structured block replacement postgres")?;

    let test_result = async {
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("apply structured block replacement migrations")?;

        let store = PgDocumentStore { pool: pool.clone() };
        // Knowledge rows are owned by `catalog_library`; seed the owner rather
        // than inventing an id the product cannot produce.
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = ironrag_backend::infra::repositories::catalog_repository::create_workspace(
            &pool,
            &format!("structured-workspace-{suffix}"),
            "Structured Replacement Workspace",
            None,
        )
        .await?;
        let library = ironrag_backend::infra::repositories::catalog_repository::create_library(
            &pool,
            workspace.id,
            &format!("structured-library-{suffix}"),
            "Structured Replacement Library",
            None,
            None,
        )
        .await?;
        let workspace_id = workspace.id;
        let library_id = library.id;
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let original_block_id = Uuid::now_v7();
        let now = Utc::now();

        store
            .upsert_document(&KnowledgeDocumentRow {
                document_id,
                workspace_id,
                library_id,
                external_key: format!("structured-replacement-{document_id}"),
                file_name: None,
                title: Some("Structured replacement fixture".to_string()),
                source_uri: None,
                document_hint: None,
                document_state: "active".to_string(),
                active_revision_id: Some(revision_id),
                readable_revision_id: Some(revision_id),
                latest_revision_no: Some(1),
                parent_document_id: None,
                document_role: "primary".to_string(),
                created_at: now,
                updated_at: now,
                deleted_at: None,
            })
            .await?;
        store
            .upsert_revision(&KnowledgeRevisionRow {
                revision_id,
                workspace_id,
                library_id,
                document_id,
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: None,
                source_uri: None,
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum: format!("checksum-{revision_id}"),
                title: Some("Structured replacement fixture".to_string()),
                byte_size: 8,
                normalized_text: Some("original".to_string()),
                text_checksum: Some(format!("text-checksum-{revision_id}")),
                image_checksum: None,
                text_state: "text_readable".to_string(),
                vector_state: "accepted".to_string(),
                graph_state: "accepted".to_string(),
                text_readable_at: Some(now),
                vector_ready_at: None,
                graph_ready_at: None,
                superseded_by_revision_id: None,
                created_at: now,
            })
            .await?;

        let original = structured_block_row(StructuredBlockRowSeed {
            block_id: original_block_id,
            workspace_id,
            library_id,
            document_id,
            revision_id,
            ordinal: 0,
            text: "original",
        });
        let original_metadata = structured_revision_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            1,
            "prepared",
        );
        store.replace_structured_revision_artifact(&original_metadata, &[original]).await?;

        let duplicate_block_id = Uuid::now_v7();
        let invalid_replacement = vec![
            structured_block_row(StructuredBlockRowSeed {
                block_id: duplicate_block_id,
                workspace_id,
                library_id,
                document_id,
                revision_id,
                ordinal: 0,
                text: "replacement one",
            }),
            structured_block_row(StructuredBlockRowSeed {
                block_id: duplicate_block_id,
                workspace_id,
                library_id,
                document_id,
                revision_id,
                ordinal: 1,
                text: "replacement two",
            }),
        ];
        let mut invalid_metadata = structured_revision_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            2,
            "prepared",
        );
        invalid_metadata.chunk_count = 99;
        assert!(
            store
                .replace_structured_revision_artifact(&invalid_metadata, &invalid_replacement)
                .await
                .is_err(),
            "duplicate primary keys must fail after both metadata upsert and block delete",
        );
        let persisted_metadata = store
            .get_structured_revision(revision_id)
            .await?
            .context("original structured revision metadata must survive rollback")?;
        assert_eq!(persisted_metadata.preparation_state, "prepared");
        assert_eq!(persisted_metadata.block_count, 1);
        assert_eq!(persisted_metadata.chunk_count, 0);
        let persisted = store.list_structured_blocks_by_revision(revision_id).await?;
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].block_id, original_block_id);
        assert_eq!(persisted[0].text, "original");

        let mismatched_metadata = structured_revision_row(
            workspace_id,
            Uuid::now_v7(),
            document_id,
            revision_id,
            1,
            "prepared",
        );
        let valid_replacement = structured_block_row(StructuredBlockRowSeed {
            block_id: Uuid::now_v7(),
            workspace_id,
            library_id,
            document_id,
            revision_id,
            ordinal: 0,
            text: "valid block",
        });
        assert!(
            store
                .replace_structured_revision_artifact(
                    &mismatched_metadata,
                    std::slice::from_ref(&valid_replacement),
                )
                .await
                .is_err(),
            "metadata outside the locked revision scope must be rejected",
        );

        let valid_metadata = structured_revision_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            1,
            "prepared",
        );
        let mismatched_block = structured_block_row(StructuredBlockRowSeed {
            block_id: Uuid::now_v7(),
            workspace_id,
            library_id: Uuid::now_v7(),
            document_id,
            revision_id,
            ordinal: 0,
            text: "wrong block library",
        });
        assert!(
            store
                .replace_structured_revision_artifact(&valid_metadata, &[mismatched_block])
                .await
                .is_err(),
            "blocks outside the locked revision scope must be rejected",
        );

        let incorrect_count_metadata = structured_revision_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            2,
            "prepared",
        );
        assert!(
            store
                .replace_structured_revision_artifact(
                    &incorrect_count_metadata,
                    std::slice::from_ref(&valid_replacement),
                )
                .await
                .is_err(),
            "metadata block count must exactly describe the replacement",
        );
        let persisted_metadata = store
            .get_structured_revision(revision_id)
            .await?
            .context("original structured revision metadata must survive validation errors")?;
        assert_eq!(persisted_metadata.preparation_state, "prepared");
        assert_eq!(persisted_metadata.block_count, 1);
        let persisted = store.list_structured_blocks_by_revision(revision_id).await?;
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].block_id, original_block_id);

        let batched_replacement = (0..1_001)
            .map(|ordinal| {
                structured_block_row(StructuredBlockRowSeed {
                    block_id: Uuid::now_v7(),
                    workspace_id,
                    library_id,
                    document_id,
                    revision_id,
                    ordinal,
                    text: &format!("replacement {ordinal}"),
                })
            })
            .collect::<Vec<_>>();
        let batched_metadata = structured_revision_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            1_001,
            "prepared",
        );
        let persisted_metadata = store
            .replace_structured_revision_artifact(&batched_metadata, &batched_replacement)
            .await?;
        assert_eq!(persisted_metadata.preparation_state, "prepared");
        assert_eq!(persisted_metadata.block_count, 1_001);
        let persisted = store.list_structured_blocks_by_revision(revision_id).await?;
        assert_eq!(persisted.len(), 1_001);
        assert_eq!(persisted.first().map(|row| row.ordinal), Some(0));
        assert_eq!(persisted.last().map(|row| row.ordinal), Some(1_000));

        Ok::<(), anyhow::Error>(())
    }
    .await;

    pool.close().await;
    let cleanup_result = temp_database.drop().await;
    test_result?;
    cleanup_result
}
