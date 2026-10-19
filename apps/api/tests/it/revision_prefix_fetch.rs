use crate::temp_postgres_database;

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use temp_postgres_database::TempPostgresDatabase;

use ironrag_backend::{
    app::config::Settings,
    infra::{
        knowledge_plane::DocumentStore,
        knowledge_rows::{
            KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeRevisionRow,
            KnowledgeStructuredBlockRow, KnowledgeStructuredRevisionRow,
        },
        postgres::pg_document_store::PgDocumentStore,
        repositories::catalog_repository,
    },
};

const HEAD_LIMIT: usize = 400;
const WINDOW_FETCH_LIMIT: usize = 2_000;
const FIXTURE_CHUNKS: i32 = 2_005;

fn chunk_identity(row: &KnowledgeChunkRow) -> (Uuid, i32, Option<String>, Uuid, Uuid, String) {
    (
        row.chunk_id,
        row.chunk_index,
        row.chunk_kind.clone(),
        row.revision_id,
        row.document_id,
        row.content_text.clone(),
    )
}

fn synthetic_chunk(
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    chunk_index: i32,
) -> KnowledgeChunkRow {
    KnowledgeChunkRow {
        chunk_id: Uuid::new_v5(&revision_id, format!("chunk:{chunk_index}").as_bytes()),
        workspace_id,
        library_id,
        document_id,
        revision_id,
        chunk_index,
        content_text: format!("function item_{chunk_index}() {{ return {chunk_index}; }}"),
        normalized_text: format!("function item_{chunk_index}() {{ return {chunk_index}; }}"),
        span_start: Some(0),
        span_end: Some(16),
        token_count: Some(6),
        chunk_kind: Some("source_unit".to_string()),
        support_block_ids: Vec::new(),
        section_path: vec!["body".to_string()],
        heading_trail: vec!["Body".to_string()],
        literal_digest: None,
        chunk_state: "ready".to_string(),
        text_generation: Some(1),
        vector_generation: None,
        quality_score: None,
        window_text: None,
        occurred_at: None,
        occurred_until: None,
    }
}

async fn seed_document_revision(
    store: &PgDocumentStore,
    workspace_id: Uuid,
    library_id: Uuid,
    external_key: &str,
    preparation_state: &str,
) -> Result<(Uuid, Uuid)> {
    let now = Utc::now();
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    store
        .upsert_document(&KnowledgeDocumentRow {
            document_id,
            workspace_id,
            library_id,
            external_key: external_key.to_string(),
            file_name: Some(format!("{external_key}.txt")),
            title: Some(external_key.to_string()),
            source_uri: None,
            document_hint: None,
            document_state: "active".to_string(),
            active_revision_id: Some(revision_id),
            readable_revision_id: Some(revision_id),
            latest_revision_no: Some(1),
            created_at: now,
            updated_at: now,
            deleted_at: None,
            parent_document_id: None,
            document_role: "primary".to_string(),
        })
        .await
        .context("upsert document")?;
    store
        .upsert_revision(&KnowledgeRevisionRow {
            revision_id,
            workspace_id,
            library_id,
            document_id,
            revision_number: 1,
            revision_state: "active".to_string(),
            revision_kind: "upload".to_string(),
            storage_ref: Some(format!("memory://{external_key}")),
            source_uri: Some(format!("memory://{external_key}/source")),
            document_hint: None,
            mime_type: "text/plain".to_string(),
            checksum: format!("checksum-{external_key}"),
            title: Some(external_key.to_string()),
            byte_size: 32,
            normalized_text: Some(external_key.to_string()),
            text_checksum: Some(format!("text-{external_key}")),
            image_checksum: None,
            text_state: "text_readable".to_string(),
            vector_state: "ready".to_string(),
            graph_state: "ready".to_string(),
            text_readable_at: Some(now),
            vector_ready_at: Some(now),
            graph_ready_at: Some(now),
            superseded_by_revision_id: None,
            created_at: now,
        })
        .await
        .context("upsert revision")?;
    store
        .replace_structured_revision_artifact(
            &KnowledgeStructuredRevisionRow {
                revision_id,
                workspace_id,
                library_id,
                document_id,
                preparation_state: preparation_state.to_string(),
                normalization_profile: "revision-prefix-test-v1".to_string(),
                source_format: "text".to_string(),
                language_code: None,
                block_count: 1,
                chunk_count: FIXTURE_CHUNKS,
                typed_fact_count: 0,
                outline_json: json!([]),
                prepared_at: now,
                updated_at: now,
            },
            &[KnowledgeStructuredBlockRow {
                block_id: Uuid::now_v7(),
                workspace_id,
                library_id,
                document_id,
                revision_id,
                ordinal: 0,
                block_kind: "paragraph".to_string(),
                text: external_key.to_string(),
                normalized_text: external_key.to_string(),
                heading_trail: Vec::new(),
                section_path: Vec::new(),
                page_number: None,
                span_start: Some(0),
                span_end: Some(8),
                parent_block_id: None,
                table_coordinates_json: None,
                code_language: None,
                created_at: now,
                updated_at: now,
            }],
        )
        .await
        .context("replace structured revision")?;
    Ok((document_id, revision_id))
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn list_head_matches_fetch_all_then_truncate_on_prepared_fat_revision() -> Result<()> {
    let settings = Settings::from_env().context("load revision-prefix settings")?;
    let database =
        TempPostgresDatabase::create(&settings.database_url, "revision_prefix_fetch").await?;
    let pool = PgPoolOptions::new().max_connections(4).connect(&database.database_url).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;

    let result = Box::pin(async {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            &pool,
            &format!("revision-prefix-workspace-{suffix}"),
            "Revision Prefix Workspace",
            None,
        )
        .await?;
        let library = catalog_repository::create_library(
            &pool,
            workspace.id,
            &format!("revision-prefix-library-{suffix}"),
            "Revision Prefix Library",
            None,
            None,
        )
        .await?;
        let store = PgDocumentStore { pool: pool.clone() };
        let (document_id, revision_id) = seed_document_revision(
            &store,
            workspace.id,
            library.id,
            "prepared-fat-head",
            "prepared",
        )
        .await?;

        let mut rows = (0..FIXTURE_CHUNKS)
            .rev()
            .map(|chunk_index| {
                synthetic_chunk(workspace.id, library.id, document_id, revision_id, chunk_index)
            })
            .collect::<Vec<_>>();
        let mut duplicate_index_row =
            synthetic_chunk(workspace.id, library.id, document_id, revision_id, 1_000);
        duplicate_index_row.chunk_id = Uuid::new_v5(&revision_id, b"duplicate-index:1000");
        duplicate_index_row.content_text = "duplicate chunk index row".to_string();
        duplicate_index_row.normalized_text = duplicate_index_row.content_text.clone();
        rows.push(duplicate_index_row);
        rows.push(synthetic_chunk(
            workspace.id,
            library.id,
            document_id,
            revision_id,
            i32::MAX - 1,
        ));
        rows.push(synthetic_chunk(
            workspace.id,
            library.id,
            document_id,
            revision_id,
            i32::MAX,
        ));
        store.insert_chunks(&rows).await.context("insert shuffled prepared chunks")?;

        assert!(store.list_head_chunks_by_revision(revision_id, 0).await?.is_empty());

        let full = store.list_chunks_by_revision(revision_id).await?;
        let head = store.list_head_chunks_by_revision(revision_id, HEAD_LIMIT).await?;
        let tail = store.list_tail_chunks_by_revision(revision_id, 12).await?;
        let range = store.list_chunks_by_revision_range(revision_id, 390, 404).await?;
        let windows = store
            .list_chunks_by_revision_windows(
                revision_id,
                &[(398, 404), (0, 2), (2, 4), (100, 99)],
            )
            .await?;
        assert_eq!(full.len(), usize::try_from(FIXTURE_CHUNKS)? + 3);
        assert_eq!(full.iter().map(chunk_identity).collect::<Vec<_>>(), {
            rows.sort_by_key(|row| (row.chunk_index, row.chunk_id));
            rows.iter().map(chunk_identity).collect::<Vec<_>>()
        });
        assert_eq!(
            head.iter().map(chunk_identity).collect::<Vec<_>>(),
            full.iter().take(HEAD_LIMIT).map(chunk_identity).collect::<Vec<_>>(),
            "SQL LIMIT prefix must equal fetch-all-then-truncate"
        );
        let head_limit = i32::try_from(HEAD_LIMIT)?;
        assert_eq!(head.last().map(|row| row.chunk_index), Some(head_limit - 1));
        assert_eq!(full[HEAD_LIMIT].chunk_index, head_limit);
        assert!(head.iter().all(|row| row.chunk_index < head_limit));
        assert!(head.iter().any(|row| row.content_text.contains("item_399")));
        assert!(!head.iter().any(|row| row.content_text.contains("item_400")));
        assert_eq!(
            tail.iter().map(chunk_identity).collect::<Vec<_>>(),
            full.iter().rev().take(12).rev().map(chunk_identity).collect::<Vec<_>>(),
        );
        assert_eq!(
            range.iter().map(chunk_identity).collect::<Vec<_>>(),
            full.iter()
                .filter(|row| (390..=404).contains(&row.chunk_index))
                .map(chunk_identity)
                .collect::<Vec<_>>(),
            "revision-first range must preserve the exact bounded subset and order"
        );
        assert_eq!(
            windows.iter().map(chunk_identity).collect::<Vec<_>>(),
            full.iter()
                .filter(|row| (0..=4).contains(&row.chunk_index) || (398..=404).contains(&row.chunk_index))
                .map(chunk_identity)
                .collect::<Vec<_>>(),
            "merged and disjoint windows must preserve the exact chunk set and order"
        );
        let capped_single = store
            .list_chunks_by_revision_windows(revision_id, &[(0, 998), (1_000, 2_004)])
            .await?;
        let expected_capped_single = full
            .iter()
            .filter(|row| {
                (0..=998).contains(&row.chunk_index)
                    || (1_000..=2_004).contains(&row.chunk_index)
            })
            .take(WINDOW_FETCH_LIMIT)
            .map(chunk_identity)
            .collect::<Vec<_>>();
        assert_eq!(
            capped_single.iter().map(chunk_identity).collect::<Vec<_>>(),
            expected_capped_single,
            "disjoint single-revision windows must retain one global 2000-row cap"
        );

        let (secondary_document_id, secondary_revision_id) = seed_document_revision(
            &store,
            workspace.id,
            library.id,
            "prepared-secondary-windows",
            "prepared",
        )
        .await?;
        let secondary_rows = (0..7)
            .rev()
            .map(|chunk_index| {
                synthetic_chunk(
                    workspace.id,
                    library.id,
                    secondary_document_id,
                    secondary_revision_id,
                    chunk_index,
                )
            })
            .collect::<Vec<_>>();
        store.insert_chunks(&secondary_rows).await?;
        let secondary_full = store.list_chunks_by_revision(secondary_revision_id).await?;
        let multi = store
            .list_chunks_by_revisions_windows(&[
                (secondary_revision_id, 4, 6),
                (revision_id, 399, 404),
                (secondary_revision_id, 0, 2),
                (secondary_revision_id, 2, 4),
            ])
            .await?;
        let expected_multi = secondary_full
            .iter()
            .chain(full.iter().filter(|row| (399..=404).contains(&row.chunk_index)))
            .map(chunk_identity)
            .collect::<Vec<_>>();
        assert_eq!(
            multi.iter().map(chunk_identity).collect::<Vec<_>>(),
            expected_multi,
            "multi-revision windows must deduplicate overlaps and retain first-request revision order"
        );

        let capped_multi = store
            .list_chunks_by_revisions_windows(&[
                (revision_id, 0, FIXTURE_CHUNKS - 1),
                (revision_id, 0, FIXTURE_CHUNKS - 1),
                (secondary_revision_id, 0, 6),
            ])
            .await?;
        let expected_capped_multi = full
            .iter()
            .take(WINDOW_FETCH_LIMIT)
            .chain(secondary_full.iter())
            .map(chunk_identity)
            .collect::<Vec<_>>();
        assert_eq!(
            capped_multi.iter().map(chunk_identity).collect::<Vec<_>>(),
            expected_capped_multi,
            "overlap dedup must precede the 2000-row per-revision cap without collapsing equal indexes"
        );
        let boundary_multi = store
            .list_chunks_by_revisions_windows(&[
                (revision_id, i32::MAX - 1, i32::MAX - 1),
                (revision_id, i32::MAX, i32::MAX),
            ])
            .await?;
        let expected_boundary_multi = full
            .iter()
            .filter(|row| row.chunk_index >= i32::MAX - 1)
            .map(chunk_identity)
            .collect::<Vec<_>>();
        assert_eq!(
            boundary_multi.iter().map(chunk_identity).collect::<Vec<_>>(),
            expected_boundary_multi,
            "inclusive adjacent windows at i32::MAX must merge without overflow"
        );

        let (unprepared_document_id, unprepared_revision_id) =
            seed_document_revision(&store, workspace.id, library.id, "unprepared-head", "pending")
                .await?;
        store
            .insert_chunks(&[synthetic_chunk(
                workspace.id,
                library.id,
                unprepared_document_id,
                unprepared_revision_id,
                0,
            )])
            .await?;
        assert!(store.list_chunks_by_revision(unprepared_revision_id).await?.is_empty());
        assert!(
            store
                .list_head_chunks_by_revision(unprepared_revision_id, HEAD_LIMIT)
                .await?
                .is_empty()
        );
        assert!(store.list_tail_chunks_by_revision(unprepared_revision_id, 12).await?.is_empty());
        assert!(
            store
                .list_chunks_by_revision_range(unprepared_revision_id, 0, 0)
                .await?
                .is_empty()
        );
        assert!(
            store
                .list_chunks_by_revision_windows(unprepared_revision_id, &[(0, 0)])
                .await?
                .is_empty()
        );
        assert!(
            store
                .list_chunks_by_revisions_windows(&[(unprepared_revision_id, 0, 0)])
                .await?
                .is_empty()
        );
        Ok(())
    })
    .await;

    pool.close().await;
    database.drop().await?;
    result
}
