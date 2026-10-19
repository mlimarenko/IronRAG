//! Snapshot export/import behaviour, against a real database.
//!
//! Both archive surfaces shipped without integration coverage, and the defects
//! that survived into 0.6.0 all share a shape a unit test cannot see: the HTTP
//! layer answers `202`, the restore succeeds by its own account, and the damage
//! is only visible in the rows. Every case here therefore asserts on rows and
//! on the classified failure — never on a status alone.

use anyhow::{Context, Result};
use async_compression::tokio::{bufread::ZstdDecoder, write::ZstdEncoder};
use async_tar::{Archive, Builder, EntryType, Header};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use chrono::Utc;
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;
use uuid::Uuid;

use ironrag_backend::{
    infra::{knowledge_rows::KnowledgeChunkVectorRow, repositories::catalog_repository},
    interfaces::http::router,
    services::{
        catalog_service::{CreateLibraryCommand, CreateWorkspaceCommand},
        content::service::{
            ReplaceInlineMutationCommand, UploadInlineDocumentCommand,
            snapshot::{
                IncludeKind, OverwriteMode, SNAPSHOT_SCHEMA_VERSION, SnapshotEntryDigest,
                SnapshotImportFailureKind, SnapshotSummary, export_library_archive,
                export_workspace_archive, restore_library_archive, restore_workspace_archive,
            },
        },
        maintenance::vector_orphan_reconcile,
    },
};

use crate::{
    content_lifecycle_support::ContentLifecycleFixture, http_response_support::response_json,
    iam_token_support::mint_api_token, mcp_tool_call_support::create_admin_session_cookie,
};

/// Rows a library owns across every plane the defects touched.
#[derive(Debug, PartialEq, Eq)]
struct LibraryFootprint {
    knowledge_documents: i64,
    knowledge_revisions: i64,
    knowledge_chunks: i64,
    vectors: i64,
    vector_manifests: i64,
}

impl LibraryFootprint {
    const EMPTY: Self = Self {
        knowledge_documents: 0,
        knowledge_revisions: 0,
        knowledge_chunks: 0,
        vectors: 0,
        vector_manifests: 0,
    };
}

async fn library_footprint(
    fixture: &ContentLifecycleFixture,
    library_id: Uuid,
) -> Result<LibraryFootprint> {
    let pool = &fixture.state.persistence.postgres;
    let counts: (i64, i64, i64, i64, i64) = sqlx::query_as(
        "select
            (select count(*) from knowledge_document where library_id = $1),
            (select count(*) from knowledge_revision where library_id = $1),
            (select count(*) from knowledge_chunk where library_id = $1),
            (select count(*) from knowledge_chunk_vector_d2 where library_id = $1),
            (select count(*) from knowledge_vector_relation_manifest where library_id = $1)",
    )
    .bind(library_id)
    .fetch_one(pool)
    .await
    .context("failed to count library footprint")?;
    Ok(LibraryFootprint {
        knowledge_documents: counts.0,
        knowledge_revisions: counts.1,
        knowledge_chunks: counts.2,
        vectors: counts.3,
        vector_manifests: counts.4,
    })
}

/// Seeds one source-owned document with a revision, a chunk and a chunk vector —
/// the planes a snapshot carries and the ones that leaked on delete.
async fn seed_library(fixture: &ContentLifecycleFixture, library_id: Uuid) -> Result<Uuid> {
    let (document_id, revision_id, _) = seed_readable_content_document(fixture, library_id).await?;
    let state = &fixture.state;
    sqlx::query("update knowledge_revision set normalized_text='The export service listens on port 8081.' where revision_id=$1")
        .bind(revision_id).execute(&state.persistence.postgres).await?;

    let chunk_id = crate::knowledge_seed::write_ready_chunk(
        state,
        crate::knowledge_seed::ReadyChunkSeed {
            workspace_id: fixture.workspace_id,
            library_id,
            document_id,
            revision_id,
            chunk_index: 0,
            text: "The export service listens on port 8081.",
        },
    )
    .await?;

    state
        .search_store
        .upsert_chunk_vector(&KnowledgeChunkVectorRow {
            vector_id: Uuid::now_v7(),
            workspace_id: fixture.workspace_id,
            library_id,
            chunk_id,
            revision_id,
            embedding_model_key: "test-embedding".to_string(),
            vector_kind: "chunk_embedding".to_string(),
            dimensions: 2,
            vector: vec![0.25, 0.75],
            freshness_generation: 1,
            created_at: Utc::now(),
            occurred_at: None,
            occurred_until: None,
        })
        .await
        .context("failed to upsert chunk vector")?;

    Ok(document_id)
}

/// Plants one orphaned chunk-vector lane: drops both library cascades, inserts
/// a physical row whose library is not in the catalog, and inserts the matching
/// orphaned manifest lane.
async fn plant_orphaned_vector_lane(
    postgres: &sqlx::PgPool,
    orphan_library_id: Uuid,
    orphan_workspace_id: Uuid,
) -> Result<()> {
    sqlx::query(
        "alter table knowledge_chunk_vector_d2
         drop constraint knowledge_chunk_vector_d2_library_id_workspace_id_fkey",
    )
    .execute(postgres)
    .await
    .context("drop canonical chunk vector cascade before planting the orphan")?;
    sqlx::query(
        "alter table knowledge_vector_relation_manifest
         drop constraint knowledge_vector_relation_manifest_library_id_fkey",
    )
    .execute(postgres)
    .await
    .context("drop manifest cascade before planting the orphan lane")?;
    sqlx::query(
        "insert into knowledge_chunk_vector_d2 (
             key, vector_id, workspace_id, library_id, chunk_id, revision_id,
             embedding_model_key, vector_kind, dimensions, embedding,
             freshness_generation, created_at
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, 2, '[0.1,0.2]', 1, now())",
    )
    .bind(format!("orphan-vector-{}", Uuid::now_v7()))
    .bind(Uuid::now_v7())
    .bind(orphan_workspace_id)
    .bind(orphan_library_id)
    .bind(Uuid::now_v7())
    .bind(Uuid::now_v7())
    .bind("orphan-vector-profile")
    .bind("chunk")
    .execute(postgres)
    .await
    .context("plant orphan chunk vector row")?;
    sqlx::query(
        "insert into knowledge_vector_relation_manifest (
             library_id, dim, vector_kind, embedding_model_key, relation_name, row_count
         ) values ($1, 2, 'chunk', 'orphan-vector-profile', 'knowledge_chunk_vector_d2', 1)",
    )
    .bind(orphan_library_id)
    .execute(postgres)
    .await
    .context("plant the orphan lane's manifest row")?;
    Ok(())
}

async fn create_library(fixture: &ContentLifecycleFixture, slug: &str) -> Result<Uuid> {
    let library = fixture
        .state
        .application_services
        .catalog
        .create_library(
            &fixture.state,
            CreateLibraryCommand {
                workspace_id: fixture.workspace_id,
                slug: Some(format!("{slug}-{}", Uuid::now_v7().simple())),
                display_name: "Snapshot transfer library".to_string(),
                description: None,
                created_by_principal_id: None,
            },
        )
        .await
        .context("failed to create library")?;
    Ok(library.id)
}

async fn export_archive(
    fixture: &ContentLifecycleFixture,
    library_id: Uuid,
    include: Vec<IncludeKind>,
) -> Result<Vec<u8>> {
    let (writer, mut reader) = tokio::io::duplex(64 * 1024);
    let state = fixture.state.clone();
    let exporter = tokio::spawn(async move {
        Box::pin(export_library_archive(state, library_id, include, writer)).await
    });
    let mut archive = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut archive)
        .await
        .context("failed to read exported archive")?;
    exporter.await.context("export task panicked")?.context("export failed")?;
    Ok(archive)
}

/// Rewrites every entry of an exported tar.zst, copying an entry unchanged
/// unless `transform` returns replacement bytes for its path.
async fn rewrite_archive_entries<F>(archive: &[u8], mut transform: F) -> Result<Vec<u8>>
where
    F: FnMut(&str, Vec<u8>) -> Result<Vec<u8>>,
{
    let decoder = ZstdDecoder::new(tokio::io::BufReader::new(archive));
    let source = Archive::new(decoder);
    let mut entries = source.entries().context("open exported archive")?;

    let mut rewritten = Vec::new();
    let encoder = ZstdEncoder::new(&mut rewritten);
    let mut builder = Builder::new(encoder);
    let mut postgres_parts = Vec::new();
    let mut blobs = Vec::new();
    let mut row_counts = std::collections::BTreeMap::<String, u64>::new();
    while let Some(entry) = entries.next().await {
        let mut entry = entry.context("read exported archive entry")?;
        let path = entry.path().context("read entry path")?.to_string_lossy().into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).await.with_context(|| format!("read `{path}`"))?;
        let mut bytes = transform(&path, bytes)?;
        let digest = SnapshotEntryDigest {
            path: path.clone(),
            size: bytes.len() as u64,
            sha256: hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&bytes)),
        };
        if let Some(rest) = path.strip_prefix("postgres/") {
            let (table, _) = rest.split_once('/').context("postgres part path missing table")?;
            *row_counts.entry(table.to_owned()).or_default() += bytes
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
                .count() as u64;
            postgres_parts.push(digest);
        } else if path.starts_with("blobs/") {
            blobs.push(digest);
        } else if path == "summary.json" {
            let mut summary: SnapshotSummary = serde_json::from_slice(&bytes)?;
            for (table, count) in &mut summary.inventory.postgres_row_counts {
                *count = row_counts.get(table).copied().unwrap_or(0);
            }
            summary.inventory.postgres_parts = std::mem::take(&mut postgres_parts);
            summary.inventory.blobs = std::mem::take(&mut blobs);
            bytes = serde_json::to_vec(&summary)?;
        }
        let mut header = Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_entry_type(EntryType::Regular);
        header.set_cksum();
        builder
            .append_data(&mut header, path.as_str(), bytes.as_slice())
            .await
            .with_context(|| format!("append `{path}`"))?;
    }
    let mut encoder = builder.into_inner().await.context("finalize tar builder")?;
    encoder.shutdown().await.context("finalize zstd stream")?;
    drop(encoder);
    Ok(rewritten)
}

/// Rewrites only the `schema_version` field of an exported archive's
/// `manifest.json` and re-encodes a tar.zst with every other entry copied
/// byte-for-byte. `summary.json` never digests `manifest.json`, so the result
/// stays structurally valid while naming a format this build cannot read.
async fn rewrite_manifest_schema_version(archive: &[u8], schema_version: u32) -> Result<Vec<u8>> {
    rewrite_archive_entries(archive, move |path, bytes| {
        if path != "manifest.json" {
            return Ok(bytes);
        }
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&bytes).context("parse exported manifest.json")?;
        manifest["schema_version"] = json!(schema_version);
        serde_json::to_vec_pretty(&manifest).context("re-serialize manifest.json")
    })
    .await
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn deleting_a_library_takes_its_knowledge_plane_with_it() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let library_id = create_library(&fixture, "delete-probe").await?;
        seed_library(&fixture, library_id).await?;

        let seeded = library_footprint(&fixture, library_id).await?;
        assert_eq!(
            seeded,
            LibraryFootprint {
                knowledge_documents: 1,
                knowledge_revisions: 1,
                knowledge_chunks: 1,
                vectors: 1,
                vector_manifests: 1,
            },
            "the fixture must seed every plane the delete has to clear"
        );

        fixture
            .state
            .application_services
            .catalog
            .delete_library(&fixture.state, library_id)
            .await
            .map_err(|error| anyhow::anyhow!("delete_library failed: {error:?}"))?;

        let after_delete = library_footprint(&fixture, library_id).await?;
        assert_eq!(
            after_delete,
            LibraryFootprint::EMPTY,
            "deleting a library must leave no rows behind: they outlive the library forever and \
             later collide with a re-import of the same archive"
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn orphaned_vector_rows_fail_closed_and_reconcile_explicitly() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let library_id = create_library(&fixture, "orphan-vector-probe").await?;
        seed_library(&fixture, library_id).await?;
        let postgres = &fixture.state.persistence.postgres;

        // Plant a row whose library no longer exists: drop the canonical
        // cascade, then insert a lane outside every catalog library. The
        // manifest lane is orphaned in the same plant, so the manifest's own
        // cascade must be absent too — its FK would otherwise reject a
        // library that is not in the catalog.
        let orphan_library_id = Uuid::now_v7();
        let orphan_workspace_id = Uuid::now_v7();
        plant_orphaned_vector_lane(postgres, orphan_library_id, orphan_workspace_id).await?;

        let failure = catalog_repository::delete_library(postgres, library_id)
            .await
            .err()
            .context("catalog delete must refuse while an orphaned vector lane exists")?;
        let failure_message = failure.to_string();
        assert!(
            failure_message.contains(&orphan_library_id.to_string()),
            "the refusal must name the orphaned library lane: {failure_message}"
        );
        assert!(
            failure_message.contains("orphan-vector"),
            "the refusal must name the reconcile command: {failure_message}"
        );

        let orphan_rows = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from knowledge_chunk_vector_d2 where library_id = $1",
        )
        .bind(orphan_library_id)
        .fetch_one(postgres)
        .await?;
        assert_eq!(orphan_rows, 1, "a failed delete must not touch the orphaned row");

        // A dry run reports the orphaned lane and writes nothing: the physical
        // row, the manifest lane and both dropped cascades must survive.
        let dry_run_report =
            vector_orphan_reconcile::orphaned_vector_rows(&fixture.state, true).await?;
        assert!(dry_run_report.dry_run, "the dry-run report must say dry_run: {dry_run_report:?}");
        assert!(
            dry_run_report.relations_with_orphans >= 1
                && dry_run_report.lanes >= 1
                && dry_run_report.rows >= 1,
            "the dry run must report the orphaned lane: {dry_run_report:?}"
        );
        let orphan_rows_after_dry_run = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from knowledge_chunk_vector_d2 where library_id = $1",
        )
        .bind(orphan_library_id)
        .fetch_one(postgres)
        .await?;
        assert_eq!(
            orphan_rows_after_dry_run, 1,
            "a dry run must not delete the orphaned physical row"
        );
        let orphan_manifest_rows_after_dry_run = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_vector_relation_manifest
             where library_id = $1",
        )
        .bind(orphan_library_id)
        .fetch_one(postgres)
        .await?;
        assert_eq!(
            orphan_manifest_rows_after_dry_run, 1,
            "a dry run must not delete the orphaned manifest lane"
        );
        let chunk_cascade_after_dry_run = sqlx::query_scalar::<_, bool>(
            "select exists (
                 select 1 from pg_constraint
                 where conname = 'knowledge_chunk_vector_d2_library_id_workspace_id_fkey'
                   and conrelid = 'knowledge_chunk_vector_d2'::regclass
             )",
        )
        .fetch_one(postgres)
        .await?;
        assert!(
            !chunk_cascade_after_dry_run,
            "a dry run must not re-attach the dropped chunk cascade"
        );
        let manifest_cascade_after_dry_run = sqlx::query_scalar::<_, bool>(
            "select exists (
                 select 1 from pg_constraint
                 where conname = 'knowledge_vector_relation_manifest_library_id_fkey'
                   and conrelid = 'knowledge_vector_relation_manifest'::regclass
             )",
        )
        .fetch_one(postgres)
        .await?;
        assert!(
            !manifest_cascade_after_dry_run,
            "a dry run must not re-attach the dropped manifest cascade"
        );

        let report = vector_orphan_reconcile::orphaned_vector_rows(&fixture.state, false).await?;
        assert!(
            report.relations_with_orphans >= 1 && report.lanes >= 1 && report.rows >= 1,
            "reconcile must report the orphaned lane: {report:?}"
        );
        let orphan_rows_after = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from knowledge_chunk_vector_d2 where library_id = $1",
        )
        .bind(orphan_library_id)
        .fetch_one(postgres)
        .await?;
        assert_eq!(orphan_rows_after, 0, "reconcile must remove the orphaned row");
        let canonical_cascade = sqlx::query_scalar::<_, bool>(
            "select exists (
                 select 1 from pg_constraint
                 where conname = 'knowledge_chunk_vector_d2_library_id_workspace_id_fkey'
                   and conrelid = 'knowledge_chunk_vector_d2'::regclass
                   and confdeltype = 'c'
                   and convalidated
             )",
        )
        .fetch_one(postgres)
        .await?;
        assert!(canonical_cascade, "reconcile must re-attach the canonical cascade");

        // A shard that carries no orphaned lane must still get its cascade
        // back: a deployment can have dropped or never attached it, and there
        // is no orphan to make the ensure branch reachable otherwise.
        sqlx::query(
            "alter table knowledge_chunk_vector_d2
             drop constraint knowledge_chunk_vector_d2_library_id_workspace_id_fkey",
        )
        .execute(postgres)
        .await
        .context("drop the chunk cascade to prove reconcile re-attaches it without orphans")?;
        let orphan_free_report =
            vector_orphan_reconcile::orphaned_vector_rows(&fixture.state, false).await?;
        assert_eq!(
            orphan_free_report.relations_with_orphans, 0,
            "a shard without orphaned lanes must report none: {orphan_free_report:?}"
        );
        let cascade_without_orphans = sqlx::query_scalar::<_, bool>(
            "select exists (
                 select 1 from pg_constraint
                 where conname = 'knowledge_chunk_vector_d2_library_id_workspace_id_fkey'
                   and conrelid = 'knowledge_chunk_vector_d2'::regclass
                   and confdeltype = 'c'
                   and convalidated
             )",
        )
        .fetch_one(postgres)
        .await?;
        assert!(
            cascade_without_orphans,
            "reconcile must re-attach the canonical cascade even without orphaned lanes"
        );

        // The orphaned manifest lane must be gone, not left at row_count = 0:
        // a row for a library that does not exist can never be reconciled and
        // blocks the baseline manifest FK.
        let orphan_manifest_rows = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_vector_relation_manifest
             where library_id = $1",
        )
        .bind(orphan_library_id)
        .fetch_one(postgres)
        .await?;
        assert_eq!(
            orphan_manifest_rows, 0,
            "reconcile must delete the orphaned manifest lane, not leave it at row_count = 0"
        );
        sqlx::query(
            "alter table knowledge_vector_relation_manifest
             add constraint knowledge_vector_relation_manifest_library_id_fkey
             foreign key (library_id) references catalog_library(id) on delete cascade",
        )
        .execute(postgres)
        .await
        .context("re-add the baseline manifest foreign key after reconcile")?;
        let manifest_cascade = sqlx::query_scalar::<_, bool>(
            "select exists (
                 select 1 from pg_constraint
                 where conname = 'knowledge_vector_relation_manifest_library_id_fkey'
                   and conrelid = 'knowledge_vector_relation_manifest'::regclass
                   and confdeltype = 'c'
                   and convalidated
             )",
        )
        .fetch_one(postgres)
        .await?;
        assert!(
            manifest_cascade,
            "the baseline manifest foreign key must re-validate after reconcile removed every \
             orphaned manifest lane"
        );

        // A manifest lane can drift with no matching physical row: the
        // pre-reconcile repair decremented such a lane to row_count = 0 and
        // kept the row, and physical discovery cannot see it. Drop the FK
        // again and plant exactly that lane, then prove a real reconcile
        // removes it even though it reports no orphaned relation.
        sqlx::query(
            "alter table knowledge_vector_relation_manifest
             drop constraint knowledge_vector_relation_manifest_library_id_fkey",
        )
        .execute(postgres)
        .await
        .context("drop the manifest foreign key before planting the drifted lane")?;
        let drifted_library_id = Uuid::now_v7();
        sqlx::query(
            "insert into knowledge_vector_relation_manifest (
                 library_id, dim, vector_kind, embedding_model_key, relation_name, row_count
             ) values ($1, 2, 'chunk', 'orphan-vector-profile', 'knowledge_chunk_vector_d2', 0)",
        )
        .bind(drifted_library_id)
        .execute(postgres)
        .await
        .context("plant a manifest-only orphan lane at row_count = 0")?;
        let manifest_only_report =
            vector_orphan_reconcile::orphaned_vector_rows(&fixture.state, false).await?;
        assert_eq!(
            manifest_only_report.relations_with_orphans, 0,
            "a manifest-only drift is invisible to physical discovery: {manifest_only_report:?}"
        );
        let drifted_rows = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_vector_relation_manifest
             where library_id = $1",
        )
        .bind(drifted_library_id)
        .fetch_one(postgres)
        .await?;
        assert_eq!(
            drifted_rows, 0,
            "reconcile must delete a manifest-only orphan lane, not only physical orphans"
        );
        sqlx::query(
            "alter table knowledge_vector_relation_manifest
             add constraint knowledge_vector_relation_manifest_library_id_fkey
             foreign key (library_id) references catalog_library(id) on delete cascade",
        )
        .execute(postgres)
        .await
        .context("re-add the baseline manifest foreign key after the manifest-only reconcile")?;
        let manifest_only_cascade = sqlx::query_scalar::<_, bool>(
            "select exists (
                 select 1 from pg_constraint
                 where conname = 'knowledge_vector_relation_manifest_library_id_fkey'
                   and conrelid = 'knowledge_vector_relation_manifest'::regclass
                   and confdeltype = 'c'
                   and convalidated
             )",
        )
        .fetch_one(postgres)
        .await?;
        assert!(
            manifest_only_cascade,
            "the baseline manifest foreign key must re-validate after the manifest-only reconcile"
        );

        let outcome = catalog_repository::delete_library(postgres, library_id)
            .await
            .context("catalog delete must succeed after reconcile")?;
        assert_eq!(
            outcome,
            catalog_repository::CatalogLibraryDeleteOutcome::Deleted,
            "delete must report Deleted after reconcile"
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

/// Runs the real operator binary against this fixture's database. The child
/// inherits the test process environment and only the settings that must point
/// at the fixture's temporary database and storage are overridden.
async fn run_maintenance(
    fixture: &ContentLifecycleFixture,
    args: &[&str],
) -> Result<std::process::Output> {
    let content_storage_root =
        tempfile::tempdir().context("failed to create CLI maintenance storage root")?;
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ironrag-maintenance"))
        .args(args)
        .env("IRONRAG_DATABASE_URL", &fixture.state.settings.database_url)
        .env("IRONRAG_REDIS_URL", &fixture.state.settings.redis_url)
        .env("IRONRAG_CREDENTIAL_MASTER_KEY", "KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=")
        .env("IRONRAG_DEPENDENCY_OBJECT_STORAGE_MODE", "disabled")
        .env("IRONRAG_CONTENT_STORAGE_PROVIDER", "filesystem")
        .env("IRONRAG_CONTENT_STORAGE_TOPOLOGY", "single_node")
        .env("IRONRAG_CONTENT_STORAGE_ROOT", content_storage_root.path())
        .env("IRONRAG_LOG_FILTER", "error")
        .output()
        .await
        .context("failed to spawn ironrag-maintenance")?;
    Ok(output)
}

/// Slices the JSON report out of the child's stdout. Tracing writes its own
/// lines there, so the report is the span between the first `{` and the last
/// `}` rather than the whole stream.
fn extract_json_report(stdout: &str) -> Result<serde_json::Value> {
    let start = stdout.find('{').context("CLI stdout carried no JSON object")?;
    let end = stdout.rfind('}').context("CLI stdout carried no closing JSON brace")?;
    serde_json::from_str(&stdout[start..=end]).context("CLI stdout JSON did not parse")
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn orphaned_vector_rows_cli_json_reports_and_reconciles() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let library_id = create_library(&fixture, "orphan-vector-cli-probe").await?;
        seed_library(&fixture, library_id).await?;
        let postgres = &fixture.state.persistence.postgres;
        let orphan_library_id = Uuid::now_v7();
        let orphan_workspace_id = Uuid::now_v7();
        plant_orphaned_vector_lane(postgres, orphan_library_id, orphan_workspace_id).await?;

        let dry_run =
            run_maintenance(&fixture, &["repair", "orphan-vector-rows", "--dry-run", "--json"])
                .await?;
        assert!(
            dry_run.status.success(),
            "dry-run CLI failed: {}",
            String::from_utf8_lossy(&dry_run.stderr)
        );
        let dry_run_stdout = String::from_utf8_lossy(&dry_run.stdout);
        let dry_run_json = extract_json_report(&dry_run_stdout)?;
        assert_eq!(dry_run_json["dry_run"], json!(true), "JSON must report dry_run=true");
        assert!(
            dry_run_json["lanes"].as_u64().unwrap_or(0) >= 1,
            "the dry run must report the orphaned lane: {dry_run_json}"
        );
        assert!(
            dry_run_stdout.contains(&orphan_library_id.to_string()),
            "the dry-run output must name the orphaned library: {dry_run_stdout}"
        );
        let orphan_rows_after_dry_run = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from knowledge_chunk_vector_d2 where library_id = $1",
        )
        .bind(orphan_library_id)
        .fetch_one(postgres)
        .await?;
        assert_eq!(
            orphan_rows_after_dry_run, 1,
            "the dry-run CLI must not delete the orphaned physical row"
        );
        let orphan_manifest_rows_after_dry_run = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_vector_relation_manifest
             where library_id = $1",
        )
        .bind(orphan_library_id)
        .fetch_one(postgres)
        .await?;
        assert_eq!(
            orphan_manifest_rows_after_dry_run, 1,
            "the dry-run CLI must not delete the orphaned manifest lane"
        );

        let real = run_maintenance(&fixture, &["repair", "orphan-vector-rows", "--json"]).await?;
        assert!(
            real.status.success(),
            "reconcile CLI failed: {}",
            String::from_utf8_lossy(&real.stderr)
        );
        let real_stdout = String::from_utf8_lossy(&real.stdout);
        let real_json = extract_json_report(&real_stdout)?;
        assert_eq!(real_json["dry_run"], json!(false), "JSON must report dry_run=false");
        assert!(
            real_json["lanes"].as_u64().unwrap_or(0) >= 1,
            "the reconcile must report the orphaned lane: {real_json}"
        );
        let orphan_rows_after = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from knowledge_chunk_vector_d2 where library_id = $1",
        )
        .bind(orphan_library_id)
        .fetch_one(postgres)
        .await?;
        assert_eq!(orphan_rows_after, 0, "the CLI reconcile must remove the orphaned row");
        let orphan_manifest_rows = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_vector_relation_manifest
             where library_id = $1",
        )
        .bind(orphan_library_id)
        .fetch_one(postgres)
        .await?;
        assert_eq!(
            orphan_manifest_rows, 0,
            "the CLI reconcile must delete the orphaned manifest lane, not leave it at \
             row_count = 0"
        );
        sqlx::query(
            "alter table knowledge_vector_relation_manifest
             add constraint knowledge_vector_relation_manifest_library_id_fkey
             foreign key (library_id) references catalog_library(id) on delete cascade",
        )
        .execute(postgres)
        .await
        .context("re-add the baseline manifest foreign key after the CLI reconcile")?;
        let manifest_cascade = sqlx::query_scalar::<_, bool>(
            "select exists (
                 select 1 from pg_constraint
                 where conname = 'knowledge_vector_relation_manifest_library_id_fkey'
                   and conrelid = 'knowledge_vector_relation_manifest'::regclass
                   and confdeltype = 'c'
                   and convalidated
             )",
        )
        .fetch_one(postgres)
        .await?;
        assert!(
            manifest_cascade,
            "the baseline manifest foreign key must re-validate after the CLI reconcile"
        );
        let canonical_cascade = sqlx::query_scalar::<_, bool>(
            "select exists (
                 select 1 from pg_constraint
                 where conname = 'knowledge_chunk_vector_d2_library_id_workspace_id_fkey'
                   and conrelid = 'knowledge_chunk_vector_d2'::regclass
                   and confdeltype = 'c'
                   and convalidated
             )",
        )
        .fetch_one(postgres)
        .await?;
        assert!(canonical_cascade, "the CLI reconcile must re-attach the canonical chunk cascade");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn restoring_beside_a_live_copy_refuses_instead_of_reporting_success() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let source_id = create_library(&fixture, "snapshot-source").await?;
        seed_library(&fixture, source_id).await?;
        let archive = export_archive(&fixture, source_id, vec![IncludeKind::LibraryData]).await?;

        let target_id = create_library(&fixture, "snapshot-target").await?;
        let failure = restore_library_archive(
            &fixture.state,
            target_id,
            std::io::Cursor::new(archive),
            OverwriteMode::Reject,
        )
        .await
        .err()
        .context(
            "restoring an archive beside the live library it came from must fail: the archive \
             carries that library's primary keys",
        )?;
        assert_eq!(
            failure.failure_code(),
            "snapshot_source_already_present",
            "the refusal must name the collision, not read as an internal error: {failure}"
        );

        let target_footprint = library_footprint(&fixture, target_id).await?;
        assert_eq!(
            target_footprint,
            LibraryFootprint::EMPTY,
            "a refused restore must leave the target untouched"
        );
        let source_footprint = library_footprint(&fixture, source_id).await?;
        assert_eq!(
            source_footprint.knowledge_chunks, 1,
            "a refused restore must not disturb the library the archive came from"
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn restoring_into_an_empty_library_moves_every_plane() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let source_id = create_library(&fixture, "snapshot-source").await?;
        seed_library(&fixture, source_id).await?;
        let archive = export_archive(&fixture, source_id, vec![IncludeKind::LibraryData]).await?;

        // Delete the original first: one deployment cannot hold two copies of
        // the same archive, and the delete is what makes room for the restore.
        fixture
            .state
            .application_services
            .catalog
            .delete_library(&fixture.state, source_id)
            .await
            .map_err(|error| anyhow::anyhow!("delete_library failed: {error:?}"))?;

        let target_id = create_library(&fixture, "snapshot-target").await?;
        let report = restore_library_archive(
            &fixture.state,
            target_id,
            std::io::Cursor::new(archive),
            OverwriteMode::Reject,
        )
        .await
        .map_err(|failure| {
            anyhow::anyhow!(
                "restore into an empty library failed: {} — {failure}",
                failure.failure_code()
            )
        })?;

        let restored = library_footprint(&fixture, target_id).await?;
        assert_eq!(
            restored,
            LibraryFootprint {
                knowledge_documents: 1,
                knowledge_revisions: 1,
                knowledge_chunks: 1,
                vectors: 1,
                vector_manifests: 1,
            },
            "a restore that reports success must have moved every plane, not just the ones whose \
             insert has no conflict clause"
        );

        let reported_chunks = report
            .postgres_rows_by_table
            .iter()
            .find(|(table, _)| table == "knowledge_chunk")
            .map(|(_, count)| *count)
            .unwrap_or_default();
        assert_eq!(
            reported_chunks, 1,
            "the report must count rows the database actually wrote: {:?}",
            report.postgres_rows_by_table
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

/// Statuses past which an async operation never moves again.
const TERMINAL_STATUSES: &[&str] = &["ready", "failed", "canceled", "superseded"];

/// Polls the real ops endpoint until the operation settles, then hands back the
/// body. Breaking on *any* terminal status is what keeps a restore that failed
/// from surfacing as a poll deadline instead of as the reason it failed.
async fn poll_operation_to_terminal(
    app: &Router,
    token: &str,
    operation_id: Uuid,
) -> Result<serde_json::Value> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    let path = format!("/v1/ops/operations/{operation_id}");
    let mut last = serde_json::Value::Null;
    while tokio::time::Instant::now() < deadline {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(&path)
                    .header(header::COOKIE, token)
                    .body(Body::empty())?,
            )
            .await
            .context("polling the async operation failed")?;
        let status = response.status();
        last = response_json(response).await?;
        anyhow::ensure!(
            status == StatusCode::OK,
            "polling {path} must be authorized for the token that admitted the import: \
             {status} {last}"
        );
        if last["status"].as_str().is_some_and(|status| TERMINAL_STATUSES.contains(&status)) {
            return Ok(last);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    anyhow::bail!("operation {operation_id} never reached a terminal status: {last}")
}

/// Admits a library snapshot import through the real router and polls its
/// operation to a terminal status, returning the settled operation payload.
/// The tests that drive the HTTP door share this so their accepted-response
/// assertions and poll cannot drift apart.
async fn import_snapshot_through_router(
    fixture: &ContentLifecycleFixture,
    target_id: Uuid,
    archive: Vec<u8>,
    writer: &str,
) -> Result<serde_json::Value> {
    // Source-owned audit history is admitted only by an actual human admin.
    let token =
        create_admin_session_cookie(&fixture.state, writer, "Snapshot restore administrator")
            .await?;

    let app = Router::new().nest("/v1", router()).with_state(fixture.state.clone());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/content/libraries/{target_id}/snapshot?overwrite=reject"))
                .header(header::COOKIE, &token)
                .body(Body::from(archive))?,
        )
        .await
        .context("snapshot import request failed")?;
    let status = response.status();
    let accepted = response_json(response).await?;
    assert_eq!(status, StatusCode::ACCEPTED, "the import must be admitted: {accepted}");
    let operation_id: Uuid = serde_json::from_value(accepted["operationId"].clone())
        .context("the 202 body must carry operationId")?;

    poll_operation_to_terminal(&app, &token, operation_id).await
}

/// A caller who was told `ready` must be able to tell a restore that moved
/// every row from one that silently moved none.
///
/// The service already counts the rows Postgres actually wrote — that is what
/// `restoring_into_an_empty_library_moves_every_plane` asserts one layer down.
/// The gap is at the door: the import worker computes the report, logs it once
/// and drops it, so the only surface a client can poll settles to `ready` with
/// nothing but a status on it. This asserts through the real router, because
/// the report being correct in-process was never the part that was broken.
#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn a_completed_snapshot_import_publishes_the_rows_it_wrote() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let source_id = create_library(&fixture, "snapshot-source").await?;
        seed_library(&fixture, source_id).await?;
        let archive = export_archive(&fixture, source_id, vec![IncludeKind::LibraryData]).await?;

        // The archive carries the source library's primary keys. Without this
        // delete the worker settles `failed`/`snapshot_source_already_present`
        // and the assertion below would never be the reason the test failed.
        fixture
            .state
            .application_services
            .catalog
            .delete_library(&fixture.state, source_id)
            .await
            .map_err(|error| anyhow::anyhow!("delete_library failed: {error:?}"))?;

        let target_id = create_library(&fixture, "snapshot-target").await?;
        let settled =
            import_snapshot_through_router(&fixture, target_id, archive, "snapshot-import-writer")
                .await?;
        assert_eq!(
            settled["status"],
            json!("ready"),
            "the import must succeed before its counters mean anything — failureCode {}: \
             {settled}",
            settled["failureCode"]
        );

        let report = &settled["resultJson"];
        assert_eq!(
            report["libraryId"],
            json!(target_id),
            "resultJson must describe this operation's own restore: {settled}"
        );
        assert_eq!(
            report["postgresRowsByTable"]["knowledge_chunk"],
            json!(1),
            "a settled import must publish the rows Postgres actually wrote: {settled}"
        );
        assert_eq!(
            report["blobsRestored"],
            json!(0),
            "an archive exported without blobs must report zero, not an absent field: {settled}"
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

/// IRG-67: an archive must not carry vectors retrieval cannot select, and
/// the restored manifest must count what actually landed.
///
/// The source advances its readable head to revision 2 while revision 1
/// still owns a vector. That vector is superseded: the export has to leave it
/// out (the sweep would have removed it), the import has to land only the
/// current one, and the manifest has to say one, not two.
#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn snapshot_transfer_keeps_the_current_only_vector_invariant() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = Box::pin(async {
        let source_id = create_library(&fixture, "snapshot-vector-invariant").await?;
        let document_id = seed_library(&fixture, source_id).await?;
        let state = &fixture.state;

        // Advance the readable head: the seeded vector belongs to revision 1,
        // retrieval now selects revision 2.
        let second = admit_snapshot_replacement(&fixture, source_id, document_id, None).await?;
        let second_item = second.items.first().context("advancing item missing")?;
        let second_revision_id = second_item.result_revision_id.context("advancing revision missing")?;
        sqlx::query("update content_mutation_item set item_state='applied' where id=$1").bind(second_item.id).execute(&state.persistence.postgres).await?;
        sqlx::query("update content_mutation set mutation_state='applied',completed_at=now() where id=$1").bind(second.mutation.id).execute(&state.persistence.postgres).await?;
        sqlx::query("update knowledge_revision set text_state='text_readable',text_readable_at=now(),vector_state='ready',vector_ready_at=now(),graph_state='ready',normalized_text='The export service listens on port 8082.' where revision_id=$1").bind(second_revision_id).execute(&state.persistence.postgres).await?;
        sqlx::query("update content_document_head set active_revision_id=$2,readable_revision_id=$2 where document_id=$1").bind(document_id).bind(second_revision_id).execute(&state.persistence.postgres).await?;
        let second_chunk_id = crate::knowledge_seed::write_ready_chunk(
            state,
            crate::knowledge_seed::ReadyChunkSeed {
                workspace_id: fixture.workspace_id,
                library_id: source_id,
                document_id,
                revision_id: second_revision_id,
                chunk_index: 0,
                text: "The export service listens on port 8082.",
            },
        )
        .await?;
        state
            .search_store
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: fixture.workspace_id,
                library_id: source_id,
                chunk_id: second_chunk_id,
                revision_id: second_revision_id,
                embedding_model_key: "test-embedding".to_string(),
                vector_kind: "chunk_embedding".to_string(),
                dimensions: 2,
                vector: vec![0.5, 0.5],
                freshness_generation: 1,
                created_at: Utc::now(),
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to upsert the current chunk vector")?;
        // The ingest publication path advances the document's heads; this
        // fixture writes them directly so revision 1's vector is provably
        // superseded rather than a null-head vector the default sweep keeps.
        sqlx::query(
            "UPDATE knowledge_document
             SET readable_revision_id = $2, active_revision_id = $2
             WHERE document_id = $1",
        )
        .bind(document_id)
        .bind(second_revision_id)
        .execute(&fixture.state.persistence.postgres)
        .await
        .context("failed to advance the document heads")?;

        let archive = export_archive(&fixture, source_id, vec![IncludeKind::LibraryData]).await?;

        fixture
            .state
            .application_services
            .catalog
            .delete_library(&fixture.state, source_id)
            .await
            .map_err(|error| anyhow::anyhow!("delete_library failed: {error:?}"))?;

        let target_id = create_library(&fixture, "snapshot-vector-target").await?;
        let restored = restore_library_archive(
            &fixture.state,
            target_id,
            std::io::Cursor::new(archive),
            OverwriteMode::Replace,
        )
        .await
        .context("restoring the vector-invariant archive must succeed")?;

        let vectors: i64 = sqlx::query_scalar(
            "select count(*) from knowledge_chunk_vector_d2 where library_id = $1",
        )
        .bind(target_id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await
        .context("failed to count restored vectors")?;
        assert_eq!(
            vectors, 1,
            "only the revision retrieval selects may survive the round trip (restore {restored:?})"
        );

        let manifest_count: i64 = sqlx::query_scalar(
            "select coalesce(sum(row_count), 0)::bigint
             from knowledge_vector_relation_manifest
             where library_id = $1 and relation_name = 'knowledge_chunk_vector_d2'",
        )
        .bind(target_id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await
        .context("failed to read the restored manifest count")?;
        assert_eq!(
            manifest_count, 1,
            "the manifest must count the rows that landed, not the rows the source had"
        );
        Ok(())
    })
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn refusals_carry_the_reason_they_happened_for() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let source_id = create_library(&fixture, "snapshot-source").await?;
        seed_library(&fixture, source_id).await?;
        let archive = export_archive(&fixture, source_id, vec![IncludeKind::LibraryData]).await?;

        // A populated target under `reject` is a policy refusal, not a failure.
        let populated = restore_library_archive(
            &fixture.state,
            source_id,
            std::io::Cursor::new(archive.clone()),
            OverwriteMode::Reject,
        )
        .await
        .err()
        .context("restoring into a populated library under reject must fail")?;
        assert_eq!(
            populated.failure_code(),
            "snapshot_target_populated",
            "a refused overwrite must be distinguishable from a crash: {populated}"
        );

        // Garbage in the body is a malformed archive, not an internal error.
        let malformed = restore_library_archive(
            &fixture.state,
            source_id,
            std::io::Cursor::new(b"not-an-archive".to_vec()),
            OverwriteMode::Replace,
        )
        .await
        .err()
        .context("restoring a malformed archive must fail")?;
        assert_eq!(
            malformed.failure_code(),
            "snapshot_archive_invalid",
            "a malformed archive must be distinguishable from a crash: {malformed}"
        );

        let survived = library_footprint(&fixture, source_id).await?;
        assert_eq!(
            survived.knowledge_chunks, 1,
            "a malformed archive under overwrite=replace must not clear the target first"
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn replacing_a_library_clears_the_tables_the_archive_does_not_carry() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let library_id = create_library(&fixture, "snapshot-replace").await?;
        seed_library(&fixture, library_id).await?;
        let archive = export_archive(&fixture, library_id, vec![IncludeKind::LibraryData]).await?;

        // Derived rows a restore rebuilds rather than carries. A replace must
        // still clear them, or the library keeps decisions taken about content
        // it no longer holds.
        sqlx::query(
            "insert into runtime_graph_community (id, library_id, projection_version, member_node_ids, summary_title)
             values ($1, $2, 1, '{}'::uuid[], 'probe')",
        )
        .bind(Uuid::now_v7())
        .bind(library_id)
        .execute(&fixture.state.persistence.postgres)
        .await
        .context("failed to seed a derived community row")?;

        restore_library_archive(
            &fixture.state,
            library_id,
            std::io::Cursor::new(archive),
            OverwriteMode::Replace,
        )
        .await
        .map_err(|failure| anyhow::anyhow!("replace restore failed: {failure}"))?;

        let leftover: i64 = sqlx::query_scalar(
            "select count(*) from runtime_graph_community where library_id = $1",
        )
        .bind(library_id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await
        .context("failed to count derived rows after replace")?;
        assert_eq!(
            leftover, 0,
            "overwrite=replace must clear the library's derived tables, not only the ones the \
             archive carries"
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

/// Tables allowed to keep rows after their library is gone, with the reason.
///
/// An audit trail exists precisely to outlive what it describes: cascading it
/// would erase the record of who deleted the library, and nulling the subject
/// would lose which library an event was about.
const DELETE_COVERAGE_EXEMPTIONS: &[(&str, &str)] =
    &[("audit_event_subject", "an audit trail must outlive the object it describes")];

#[derive(Debug, sqlx::FromRow)]
struct ForeignKeyEdge {
    child: String,
    parent: String,
    child_columns: Vec<String>,
    delete_action: String,
}

/// Every table carrying `library_id` must lose its rows when the library goes.
///
/// This is the gate for the whole class, not the instance: the leak that
/// stranded rows in eight tables was invisible because nothing ever asserted
/// that ownership implies deletion. A table passes when it cascades from
/// `catalog_library` directly, when it reaches it through a chain of cascading
/// foreign keys, when it is a log that deliberately keeps its row and drops the
/// reference (`on delete set null`), or when it is listed above with a written
/// reason. What never passes is a table with no declared behaviour at all: its
/// rows keep pointing at a library that no longer exists.
#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn every_table_a_library_owns_is_deleted_with_it() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        // Force the per-dim shard into existence so runtime-created tables are
        // held to the same rule as the ones the baseline declares.
        let library_id = create_library(&fixture, "coverage-probe").await?;
        seed_library(&fixture, library_id).await?;

        let pool = &fixture.state.persistence.postgres;
        let owned: Vec<String> = sqlx::query_scalar(
            "select c.table_name::text
             from information_schema.columns c
             join information_schema.tables t
               on t.table_schema = c.table_schema and t.table_name = c.table_name
             where c.table_schema = 'public' and c.column_name = 'library_id'
               and t.table_type = 'BASE TABLE'
             order by c.table_name",
        )
        .fetch_all(pool)
        .await
        .context("failed to list tables carrying library_id")?;
        assert!(owned.iter().any(|table| table == "content_document"));
        assert!(owned.iter().any(|table| table == "knowledge_chunk_vector_d2"),
            "runtime-created source-owning tables must stay in deletion coverage");

        let edges: Vec<ForeignKeyEdge> = sqlx::query_as(
            "select
                 child.relname::text as child,
                 parent.relname::text as parent,
                 array(
                     select attname::text
                     from unnest(con.conkey) as k(attnum)
                     join pg_attribute on pg_attribute.attrelid = con.conrelid
                                      and pg_attribute.attnum = k.attnum
                 ) as child_columns,
                 case con.confdeltype
                     when 'c' then 'cascade'
                     when 'n' then 'set null'
                     when 'r' then 'restrict'
                     when 'a' then 'no action'
                     else 'default'
                 end as delete_action
             from pg_constraint con
             join pg_class child on child.oid = con.conrelid
             join pg_class parent on parent.oid = con.confrelid
             where con.contype = 'f'",
        )
        .fetch_all(pool)
        .await
        .context("failed to read the foreign-key graph")?;

        let mut uncovered = Vec::new();
        for table in &owned {
            if DELETE_COVERAGE_EXEMPTIONS.iter().any(|(exempt, _)| exempt == table) {
                continue;
            }
            if !cascades_to_catalog_library(table, &edges, 0)
                && !detaches_from_catalog_library(table, &edges)
            {
                uncovered.push(table.clone());
            }
        }

        assert!(
            uncovered.is_empty(),
            "these tables carry library_id but nothing deletes their rows when the library is              deleted, so the rows outlive it and later collide with a re-import: {uncovered:?}.              Give each one a cascading foreign key, or add it to DELETE_COVERAGE_EXEMPTIONS with              a reason."
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

/// Whether deleting a `catalog_library` row removes this table's rows, directly
/// or through a chain of cascades. A `set null`/`restrict` edge does not count:
/// the row survives, which is exactly the failure this gate exists to catch.
fn cascades_to_catalog_library(table: &str, edges: &[ForeignKeyEdge], depth: usize) -> bool {
    // The schema's ownership chains are a handful of hops deep; the bound only
    // stops a cycle from spinning.
    if depth > 8 {
        return false;
    }
    edges.iter().filter(|edge| edge.child == table && edge.delete_action == "cascade").any(|edge| {
        if edge.parent == "catalog_library" {
            return true;
        }
        // Only a non-nullable link is a real ownership chain — the gate
        // proves rows die with the library, and a nullable parent
        // reference cannot promise that.
        edge.child_columns.iter().any(|column| column != "library_id")
            && cascades_to_catalog_library(&edge.parent, edges, depth + 1)
    })
}

/// Whether the table keeps its row but drops the reference when the library is
/// deleted. Journals (async operations, provider validation logs) do this on
/// purpose: the record is about something that happened, not about content the
/// library owns, and its `library_id` is nullable so nothing is left dangling.
fn detaches_from_catalog_library(table: &str, edges: &[ForeignKeyEdge]) -> bool {
    edges.iter().any(|edge| {
        edge.child == table
            && edge.parent == "catalog_library"
            && edge.delete_action == "set null"
            && edge.child_columns.iter().any(|column| column == "library_id")
    })
}

/// IRG-32 helpers. Creates a library and uploads one real file through the
/// inline-upload service, so a `content_revision` row owns a `storage_key` and
/// a real blob lands in the fixture's filesystem storage. Returns the library
/// id and that storage key.
async fn library_with_uploaded_blob(
    fixture: &ContentLifecycleFixture,
    slug: &str,
) -> Result<(Uuid, String)> {
    let library_id = create_library(fixture, slug).await?;
    fixture
        .state
        .application_services
        .content
        .upload_inline_document(
            &fixture.state,
            UploadInlineDocumentCommand {
                workspace_id: fixture.workspace_id,
                library_id,
                external_key: Some(format!("snapshot-blob-{}", Uuid::now_v7())),
                idempotency_key: None,
                requested_by_principal_id: None,
                request_surface: "rest".to_string(),
                source_identity: Some("snapshot-transfer-upload".to_string()),
                file_name: "alpha.txt".to_string(),
                title: Some("Alpha Blob".to_string()),
                document_hint: None,
                mime_type: Some("text/plain".to_string()),
                file_bytes: b"the export service listens on port 8081".to_vec(),
                parent_external_key: None,
            },
        )
        .await
        .context("failed to upload an inline document with a blob")?;
    let storage_key: String = sqlx::query_scalar(
        "select storage_key from content_revision
         where library_id = $1 and storage_key is not null
         order by created_at limit 1",
    )
    .bind(library_id)
    .fetch_one(&fixture.state.persistence.postgres)
    .await
    .context("failed to read the uploaded revision's storage key")?;
    Ok((library_id, storage_key))
}

fn stored_blob_path(fixture: &ContentLifecycleFixture, storage_key: &str) -> std::path::PathBuf {
    std::path::Path::new(&fixture.state.settings.content_storage_root).join(storage_key)
}

/// IRG-32: an archive whose `summary.json` agrees with tampered blob bytes is
/// refused because prevalidation compares those bytes against the digest the
/// `content_revision.checksum` it carries names. A coherent substitution of
/// both bytes and checksum would still pass; this tampers only the bytes.
///
/// The refusal is raised by prevalidation and the target is observably intact:
/// its knowledge rows, its `content_revision` row and its own stored blob file
/// all survive. That the restore transaction never touched the target rests on
/// prevalidation running before the restore transaction opens — this test
/// proves the observable outcome, not the internal ordering by itself. A
/// clear-then-rollback would restore rows but need not restore the storage
/// file, which is why the blob file is asserted too.
///
/// The source library is deleted between export and import on purpose: while it
/// lives, the archive's identities collide with its rows and
/// `snapshot_source_already_present` refuses every import, so the tampered-blob
/// prevalidation is never the gate that fired and "the target survived" proves
/// nothing. With the source gone that collision cannot occur, so the new check
/// is the only thing that can refuse — and its absence lets the import replace
/// the populated target instead.
#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn a_blob_that_contradicts_its_content_revision_checksum_refuses_before_clearing_the_target()
-> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let (source_id, storage_key) =
            library_with_uploaded_blob(&fixture, "snapshot-tampered-source").await?;
        std::fs::write(
            stored_blob_path(&fixture, &storage_key),
            b"the export service listens on port 9999",
        )
        .context("failed to overwrite the stored blob with tampered bytes")?;
        let archive = export_archive(
            &fixture,
            source_id,
            vec![IncludeKind::LibraryData, IncludeKind::Blobs],
        )
        .await?;

        // Free the archive's identities so `snapshot_source_already_present`
        // cannot fire: only the tampered-blob prevalidation may refuse now.
        fixture
            .state
            .application_services
            .catalog
            .delete_library(&fixture.state, source_id)
            .await
            .map_err(|error| anyhow::anyhow!("delete_library failed: {error:?}"))?;

        // A populated target under `replace` must survive the refusal: the
        // abort has to happen before the target is cleared, not after. The
        // footprint covers knowledge rows only, so the target also carries its
        // own uploaded blob; a clear-then-rollback would restore the rows but
        // need not restore the storage file, so both are checked.
        let (target_id, target_storage_key) =
            library_with_uploaded_blob(&fixture, "snapshot-tampered-target").await?;
        seed_library(&fixture, target_id).await?;
        let seeded = library_footprint(&fixture, target_id).await?;
        let target_blob = stored_blob_path(&fixture, &target_storage_key);
        assert!(target_blob.exists(), "the target must own a stored blob before the refusal");
        let seeded_revisions: i64 = sqlx::query_scalar(
            "select count(*) from content_revision where storage_key = $1",
        )
        .bind(&target_storage_key)
        .fetch_one(&fixture.state.persistence.postgres)
        .await
        .context("failed to count the target's uploaded content revisions")?;
        assert_eq!(seeded_revisions, 1, "the target must own one uploaded content revision");

        let failure = restore_library_archive(
            &fixture.state,
            target_id,
            std::io::Cursor::new(archive),
            OverwriteMode::Replace,
        )
        .await
        .err()
        .context("a blob that contradicts its checksum must refuse the import")?;
        assert_eq!(
            failure.failure_code(),
            "snapshot_archive_invalid",
            "the refusal must classify as an invalid archive: {failure}"
        );
        assert!(
            failure.to_string().contains("content_revision.checksum"),
            "the refusal must name what disagreed: {failure}"
        );

        let after = library_footprint(&fixture, target_id).await?;
        assert_eq!(
            after, seeded,
            "a refused import must leave the target's knowledge rows intact: the mismatch was known before the target was touched"
        );
        let after_revisions: i64 = sqlx::query_scalar(
            "select count(*) from content_revision where storage_key = $1",
        )
        .bind(&target_storage_key)
        .fetch_one(&fixture.state.persistence.postgres)
        .await
        .context("failed to recount the target's uploaded content revisions")?;
        assert_eq!(
            after_revisions, seeded_revisions,
            "a refused import must leave the target's content_revision rows intact"
        );
        assert!(
            target_blob.exists(),
            "a refused import must leave the target's stored blob file intact"
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

/// The positive control for the refusal above: the identical flow with no
/// tampering imports successfully and reports the blob as verified, so the
/// new check cannot be passing by rejecting every archive with a blob.
#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn an_untampered_blob_imports_and_reports_as_verified() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let (source_id, _) = library_with_uploaded_blob(&fixture, "snapshot-clean-source").await?;
        let archive =
            export_archive(&fixture, source_id, vec![IncludeKind::LibraryData, IncludeKind::Blobs])
                .await?;

        fixture
            .state
            .application_services
            .catalog
            .delete_library(&fixture.state, source_id)
            .await
            .map_err(|error| anyhow::anyhow!("delete_library failed: {error:?}"))?;

        let target_id = create_library(&fixture, "snapshot-clean-target").await?;
        let report = restore_library_archive(
            &fixture.state,
            target_id,
            std::io::Cursor::new(archive),
            OverwriteMode::Reject,
        )
        .await
        .map_err(|failure| anyhow::anyhow!("clean blob import failed: {failure}"))?;

        assert_eq!(report.blobs_verified, 1, "the clean blob must be proven against its checksum");
        assert_eq!(report.blobs_restored, 1, "the clean blob must have been restored");
        assert_eq!(report.unverifiable_blobs, 0, "nothing about the clean archive is unverifiable");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

/// Private historical source bytes with an unsupported checksum remain portable,
/// but do not become readable or acquire admission identity during restore.
/// The unverifiable counter reaches the operation's polled `resultJson`.
#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn an_unverifiable_blob_import_succeeds_and_publishes_the_counter() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let source_id = create_library(&fixture, "snapshot-unverifiable-source").await?;
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let source_bytes = b"private historical source bytes";
        let storage_key = format!("content/{}/{source_id}/historical.bin", fixture.workspace_id);
        fixture.state.content_storage.write_revision_source_raw(&storage_key, source_bytes).await?;
        // Seed historical input directly, not an admitted upload with a rewritten
        // descriptor. No mutation/item claims authority over this private revision.
        let mut tx = fixture.state.persistence.postgres.begin().await?;
        sqlx::query("insert into content_document(id,workspace_id,library_id,external_key,source_file_name,revision_sequence) values($1,$2,$3,'private-history','historical.bin',1)")
            .bind(document_id).bind(fixture.workspace_id).bind(source_id).execute(&mut *tx).await?;
        sqlx::query("insert into content_revision(id,document_id,workspace_id,library_id,revision_number,content_source_kind,checksum,mime_type,byte_size,storage_key,admission_identity) values($1,$2,$3,$4,1,'upload','legacy:checksum-unavailable','application/octet-stream',$5,$6,NULL)")
            .bind(revision_id).bind(document_id).bind(fixture.workspace_id).bind(source_id)
            .bind(source_bytes.len() as i64).bind(&storage_key).execute(&mut *tx).await?;
        sqlx::query("insert into content_document_head(document_id) values($1)")
            .bind(document_id).execute(&mut *tx).await?;
        sqlx::query("insert into knowledge_document(document_id,workspace_id,library_id,external_key,file_name,document_state,latest_revision_no,created_at,updated_at) values($1,$2,$3,'private-history','historical.bin','active',1,now(),now())")
            .bind(document_id).bind(fixture.workspace_id).bind(source_id).execute(&mut *tx).await?;
        sqlx::query("insert into knowledge_revision(revision_id,workspace_id,library_id,document_id,revision_number,revision_state,revision_kind,storage_ref,mime_type,checksum,byte_size,text_state,vector_state,graph_state,created_at) select id,workspace_id,library_id,document_id,revision_number,'accepted','upload',storage_key,mime_type,checksum,byte_size,'accepted','accepted','accepted',created_at from content_revision where id=$1")
            .bind(revision_id).execute(&mut *tx).await?;
        tx.commit().await?;
        let archive =
            export_archive(&fixture, source_id, vec![IncludeKind::LibraryData, IncludeKind::Blobs])
                .await?;

        fixture
            .state
            .application_services
            .catalog
            .delete_library(&fixture.state, source_id)
            .await
            .map_err(|error| anyhow::anyhow!("delete_library failed: {error:?}"))?;
        let target_id = create_library(&fixture, "snapshot-unverifiable-target").await?;
        let settled = import_snapshot_through_router(
            &fixture,
            target_id,
            archive,
            "snapshot-unverifiable-writer",
        )
        .await?;
        assert_eq!(
            settled["status"],
            json!("ready"),
            "a blob whose contract cannot be proven must not fail the import: {settled}"
        );
        let report = &settled["resultJson"];
        assert_eq!(
            report["unverifiableBlobs"],
            json!(1),
            "the operation must publish the unverifiable count: {settled}"
        );
        assert_eq!(
            report["blobsVerified"],
            json!(0),
            "an unprovable blob is not verified: {settled}"
        );
        assert_eq!(report["blobsRestored"], json!(1));
        let (restored_key, admission, checksum): (String, Option<Value>, String) = sqlx::query_as(
            "select storage_key,admission_identity,checksum from content_revision where id=$1 and library_id=$2",
        ).bind(revision_id).bind(target_id).fetch_one(&fixture.state.persistence.postgres).await?;
        assert_eq!(admission, None);
        assert_eq!(checksum, "legacy:checksum-unavailable");
        assert_eq!(fixture.state.content_storage.read_revision_source(&restored_key).await?, source_bytes);
        let heads: (Option<Uuid>, Option<Uuid>) = sqlx::query_as(
            "select active_revision_id,readable_revision_id from content_document_head where document_id=$1",
        ).bind(document_id).fetch_one(&fixture.state.persistence.postgres).await?;
        assert_eq!(heads, (None, None));
        let published: i64 = sqlx::query_scalar("select count(*) from content_current_revision where document_id=$1")
            .bind(document_id).fetch_one(&fixture.state.persistence.postgres).await?;
        assert_eq!(published, 0, "unadmitted history must remain private after restore");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

/// IRG-77: an archive this build cannot read must be refused before the target
/// is touched, and the refusal must name the schema version rather than fail
/// generically. The source library is deleted between export and import so the
/// archive's primary keys cannot collide and `snapshot_source_already_present`
/// cannot be the gate that fired (see the IRG-32 test above).
#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn an_unsupported_schema_version_refuses_the_import_atomically() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let source_id = create_library(&fixture, "snapshot-schema-source").await?;
        seed_library(&fixture, source_id).await?;
        let archive = export_archive(&fixture, source_id, vec![IncludeKind::LibraryData]).await?;

        fixture
            .state
            .application_services
            .catalog
            .delete_library(&fixture.state, source_id)
            .await
            .map_err(|error| anyhow::anyhow!("delete_library failed: {error:?}"))?;

        let target_id = create_library(&fixture, "snapshot-schema-target").await?;
        let before = library_footprint(&fixture, target_id).await?;
        assert_eq!(before, LibraryFootprint::EMPTY, "the target must start empty");

        let rewritten =
            rewrite_manifest_schema_version(&archive, SNAPSHOT_SCHEMA_VERSION + 1).await?;
        let settled = import_snapshot_through_router(
            &fixture,
            target_id,
            rewritten,
            "snapshot-schema-writer",
        )
        .await?;

        assert_eq!(
            settled["status"],
            json!("failed"),
            "an archive this build cannot read must settle failed: {settled}"
        );
        assert_eq!(
            settled["failureCode"],
            json!("snapshot_unsupported_schema_version"),
            "the refusal must name the schema version, not a generic failure: {settled}"
        );
        let after = library_footprint(&fixture, target_id).await?;
        assert_eq!(
            after,
            LibraryFootprint::EMPTY,
            "a refused import must leave the target's five planes empty: {after:?}"
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

/// Seeds one library with a readable base revision and then a REQUIRED
/// independent intent and exact bound replacement, clearing both head
/// projections. The replacement is advanced to a terminal failure before the
/// caller exports. Returns `(document_id, suppressed_mutation_id)`.
async fn seed_readable_content_document(
    fixture: &ContentLifecycleFixture,
    library_id: Uuid,
) -> Result<(Uuid, Uuid, Uuid)> {
    let content = &fixture.state.application_services.content;
    let uploaded = content
        .upload_inline_document(
            &fixture.state,
            UploadInlineDocumentCommand {
                workspace_id: fixture.workspace_id,
                library_id,
                external_key: Some(format!("suppressed-{}", Uuid::now_v7().simple())),
                idempotency_key: None,
                requested_by_principal_id: None,
                request_surface: "rest".to_string(),
                source_identity: Some("snapshot-suppression-base".to_string()),
                file_name: "base.txt".to_string(),
                title: Some("Base".to_string()),
                document_hint: None,
                mime_type: Some("text/plain".to_string()),
                file_bytes: b"base readable bytes".to_vec(),
                parent_external_key: None,
            },
        )
        .await
        .context("failed to upload the base document")?;
    let document_id = uploaded.document.document.id;
    let upload_mutation_id = uploaded.mutation.mutation.id;
    let base_revision_id = uploaded
        .mutation
        .items
        .first()
        .and_then(|item| item.result_revision_id)
        .context("upload admission must create a base revision")?;
    // Canonical isolated READY fixture: applied item and readable knowledge
    // projection precede the exact head lineage. No provider or invalid-head
    // promotion shortcut participates in archive correctness.
    let base_item_id = uploaded.mutation.items.first().context("upload item missing")?.id;
    sqlx::query("update content_mutation_item set item_state = 'applied' where id = $1")
        .bind(base_item_id)
        .execute(&fixture.state.persistence.postgres)
        .await?;
    sqlx::query("update knowledge_revision set text_state = 'text_readable', text_readable_at = now(),
        vector_state = 'ready', vector_ready_at = now(), graph_state = 'ready', normalized_text = 'base readable bytes'
        where revision_id = $1")
        .bind(base_revision_id).execute(&fixture.state.persistence.postgres).await?;
    sqlx::query(
        "update content_document_head set active_revision_id = $2, readable_revision_id = $2 where document_id = $1",
    )
    .bind(document_id)
    .bind(base_revision_id)
    .execute(&fixture.state.persistence.postgres)
    .await?;
    sqlx::query("update knowledge_document set active_revision_id = $2, readable_revision_id = $2 where document_id = $1")
        .bind(document_id).bind(base_revision_id).execute(&fixture.state.persistence.postgres).await?;
    sqlx::query(
        "update content_mutation
         set mutation_state = 'applied', completed_at = now()
         where id = $1",
    )
    .bind(upload_mutation_id)
    .execute(&fixture.state.persistence.postgres)
    .await
    .context("failed to settle the upload mutation")?;

    // A chunk-bearing revision so the null-head repair pass has a real
    // candidate to consider; the persisted intent must refuse it.
    sqlx::query(
        "insert into content_chunk (
            id, revision_id, chunk_index, start_offset, end_offset, token_count,
            normalized_text, text_checksum
         ) values ($1, $2, 0, 0, 16, 4, 'base chunk', 'sha256:base-chunk')",
    )
    .bind(Uuid::now_v7())
    .bind(base_revision_id)
    .execute(&fixture.state.persistence.postgres)
    .await
    .context("failed to insert the base revision chunk")?;

    let current: Option<Uuid> = sqlx::query_scalar(
        "select revision_id from content_current_revision where document_id = $1",
    )
    .bind(document_id)
    .fetch_optional(&fixture.state.persistence.postgres)
    .await?;
    assert_eq!(current, Some(base_revision_id), "fixture must seed canonical current membership");
    Ok((document_id, base_revision_id, upload_mutation_id))
}

async fn seed_suppressed_document(
    fixture: &ContentLifecycleFixture,
    library_id: Uuid,
) -> Result<(Uuid, Uuid)> {
    let (document_id, _, _) = seed_readable_content_document(fixture, library_id).await?;
    let content = &fixture.state.application_services.content;

    let bytes = b"restricted replacement bytes".to_vec();
    let accepted_intent = content.admit_visibility_intent(&fixture.state,
        ironrag_backend::infra::repositories::admission_repository::VisibilityAdmissionRequest {
            workspace_id: fixture.workspace_id, library_id, document_id,
            requested_by_principal_id: None, request_surface: "rest".to_owned(),
            intent: ironrag_contracts::documents::CreateVisibilityIntentRequest {
                idempotency_key: format!("snapshot-denial-{}", Uuid::now_v7()),
                source_identity: "snapshot-suppression-replace".to_owned(), materialization_token: None,
                replacement: ironrag_contracts::documents::ReplacementIdentity {
                    checksum: format!("sha256:{}", hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&bytes))),
                    byte_size: bytes.len() as i64, mime_type: "text/plain".to_owned(), file_name: "replacement.txt".to_owned(),
                    title: None, language_code: None, source_uri: None, document_hint: None,
                },
            },
        }).await?.into_bundle().receipt;
    let suppressed = content
        .replace_inline_mutation(
            &fixture.state,
            ReplaceInlineMutationCommand {
                workspace_id: fixture.workspace_id,
                library_id,
                document_id,
                idempotency_key: Some(format!("suppressed-replace-{}", Uuid::now_v7().simple())),
                requested_by_principal_id: None,
                request_surface: "rest".to_string(),
                source_identity: Some("snapshot-suppression-replace".to_string()),
                file_name: "replacement.txt".to_string(),
                title: None,
                language_code: None,
                source_uri: None,
                document_hint: None,
                mime_type: Some("text/plain".to_string()),
                file_bytes: bytes,
                visibility_intent_id: Some(accepted_intent.visibility_intent_id),
            },
        )
        .await
        .context("failed to admit the suppressing replacement")?;

    sqlx::query(
        "update content_mutation
         set mutation_state = 'failed', completed_at = now(), failure_code = 'synthetic_terminal'
         where id = $1",
    )
    .bind(suppressed.mutation.id)
    .execute(&fixture.state.persistence.postgres)
    .await
    .context("failed to advance the suppressing replacement to terminal")?;

    Ok((document_id, suppressed.mutation.id))
}

/// A visibility intent with a failed bound replacement must survive
/// an export → restore, keep the persisted obligation, and survive the
/// real null-head repair pass without reviving the restricted base revision.
#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn suppressed_failed_document_survives_export_restore_and_maintenance() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let source_id = create_library(&fixture, "suppression-source").await?;
        let (document_id, suppressed_mutation_id) =
            seed_suppressed_document(&fixture, source_id).await?;

        let archive =
            export_archive(&fixture, source_id, vec![IncludeKind::LibraryData, IncludeKind::Blobs])
                .await?;

        fixture
            .state
            .application_services
            .catalog
            .delete_library(&fixture.state, source_id)
            .await
            .map_err(|error| anyhow::anyhow!("delete_library failed: {error:?}"))?;

        let target_id = create_library(&fixture, "suppression-target").await?;
        restore_library_archive(
            &fixture.state,
            target_id,
            std::io::Cursor::new(archive),
            OverwriteMode::Reject,
        )
        .await
        .map_err(|failure| {
            anyhow::anyhow!(
                "restore of a suppressed archive failed: {} — {failure}",
                failure.failure_code()
            )
        })?;

        let (head_active, head_readable, head_latest): (Option<Uuid>, Option<Uuid>, Option<Uuid>) =
            sqlx::query_as(
                "select active_revision_id, readable_revision_id, latest_mutation_id
                 from content_document_head
                 where document_id = $1",
            )
            .bind(document_id)
            .fetch_one(&fixture.state.persistence.postgres)
            .await?;
        assert_eq!(head_active, None, "restored head active pointer must stay NULL");
        assert_eq!(head_readable, None, "restored head readable pointer must stay NULL");
        assert_eq!(head_latest, Some(suppressed_mutation_id));

        let (projection_active, projection_readable): (Option<Uuid>, Option<Uuid>) =
            sqlx::query_as(
                "select active_revision_id, readable_revision_id
                 from knowledge_document
                 where document_id = $1",
            )
            .bind(document_id)
            .fetch_one(&fixture.state.persistence.postgres)
            .await?;
        assert_eq!(projection_active, None, "restored projection active pointer must stay NULL");
        assert_eq!(
            projection_readable, None,
            "restored projection readable pointer must stay NULL"
        );

        let persisted_intent: Option<Uuid> = sqlx::query_scalar(
            "select h.visibility_intent_id from content_document_head h
             join content_mutation_item i on i.document_id = h.document_id
             where i.mutation_id = $1",
        )
        .bind(suppressed_mutation_id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        assert!(persisted_intent.is_some(), "the durable intent must ride the snapshot");

        // The real maintenance pass must refuse to revive the restricted base
        // revision even though a chunk-bearing candidate now exists.
        let report = ironrag_backend::services::maintenance::repair::promote_null_heads(
            &fixture.state,
            Some(target_id),
        )
        .await?;
        assert_eq!(
            report.promoted, 0,
            "a suppressed document must never be repromoted after restore"
        );
        let head_readable_after: Option<Uuid> = sqlx::query_scalar(
            "select readable_revision_id from content_document_head where document_id = $1",
        )
        .bind(document_id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(
            head_readable_after, None,
            "maintenance must leave the restored head unreadable"
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

/// Removes immutable identity while recomputing inventory: digest-correct
/// transport must not make a semantically incomplete archive acceptable.
async fn strip_content_mutation_request_identity(archive: &[u8]) -> Result<Vec<u8>> {
    rewrite_archive_entries(archive, |path, bytes| {
        if !path.starts_with("postgres/content_mutation/") {
            return Ok(bytes);
        }
        let mut rewritten_rows = Vec::new();
        for line in bytes.split(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            let mut row: serde_json::Value =
                serde_json::from_slice(line).context("parse content_mutation ndjson row")?;
            if let serde_json::Value::Object(ref mut map) = row {
                map.remove("request_identity");
            }
            serde_json::to_writer(&mut rewritten_rows, &row)
                .context("re-serialize content_mutation row")?;
            rewritten_rows.push(b'\n');
        }
        Ok(rewritten_rows)
    })
    .await
}

/// Missing immutable identity refuses before any target write, never reconstructs
/// visibility from historical policy, and leaves the target empty.
#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn archive_without_request_identity_is_refused_atomically() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let source_id = create_library(&fixture, "legacy-archive-source").await?;
        seed_suppressed_document(&fixture, source_id).await?;
        let archive =
            export_archive(&fixture, source_id, vec![IncludeKind::LibraryData, IncludeKind::Blobs])
                .await?;
        let legacy = strip_content_mutation_request_identity(&archive).await?;

        let target_id = create_library(&fixture, "legacy-archive-target").await?;
        let failure = restore_library_archive(
            &fixture.state,
            target_id,
            std::io::Cursor::new(legacy),
            OverwriteMode::Reject,
        )
        .await
        .expect_err("an archive missing immutable request identity must be refused");

        assert_eq!(
            failure.failure_code(),
            SnapshotImportFailureKind::ArchiveInvalid.code(),
            "a missing required column must be classified as a malformed archive, not an internal failure",
        );

        let documents: i64 =
            sqlx::query_scalar("select count(*) from content_document where library_id = $1")
                .bind(target_id)
                .fetch_one(&fixture.state.persistence.postgres)
                .await?;
        let knowledge: i64 =
            sqlx::query_scalar("select count(*) from knowledge_document where library_id = $1")
                .bind(target_id)
                .fetch_one(&fixture.state.persistence.postgres)
                .await?;
        assert_eq!(
            (documents, knowledge),
            (0, 0),
            "a refused import must not write any content or knowledge rows into the target",
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

async fn restore_content_into_fresh_library(
    fixture: &ContentLifecycleFixture,
    source_id: Uuid,
    target_slug: &str,
) -> Result<Uuid> {
    let archive =
        export_archive(fixture, source_id, vec![IncludeKind::LibraryData, IncludeKind::Blobs])
            .await?;
    fixture
        .state
        .application_services
        .catalog
        .delete_library(&fixture.state, source_id)
        .await
        .map_err(|error| anyhow::anyhow!("delete source fixture failed: {error:?}"))?;
    let target_id = create_library(fixture, target_slug).await?;
    restore_library_archive(
        &fixture.state,
        target_id,
        std::io::Cursor::new(archive),
        OverwriteMode::Reject,
    )
    .await?;
    Ok(target_id)
}

async fn admit_snapshot_replacement(
    fixture: &ContentLifecycleFixture,
    library_id: Uuid,
    document_id: Uuid,
    visibility_intent_id: Option<Uuid>,
) -> Result<ironrag_backend::services::content::service::ContentMutationAdmission> {
    fixture
        .state
        .application_services
        .content
        .replace_inline_mutation(
            &fixture.state,
            ReplaceInlineMutationCommand {
                workspace_id: fixture.workspace_id,
                library_id,
                document_id,
                idempotency_key: Some(format!("snapshot-payload-{}", Uuid::now_v7())),
                requested_by_principal_id: None,
                request_surface: "rest".to_owned(),
                source_identity: Some("snapshot-source-binding".to_owned()),
                file_name: "complete.txt".to_owned(),
                title: Some("Complete".to_owned()),
                language_code: None,
                source_uri: None,
                document_hint: None,
                mime_type: Some("text/plain".to_owned()),
                file_bytes: b"complete replacement bytes".to_vec(),
                visibility_intent_id,
            },
        )
        .await
        .map_err(anyhow::Error::new)
}

async fn admit_snapshot_intent(
    fixture: &ContentLifecycleFixture,
    library_id: Uuid,
    document_id: Uuid,
) -> Result<ironrag_contracts::documents::VisibilityIntentReceipt> {
    let bytes = b"complete replacement bytes";
    Ok(fixture.state.application_services.content.admit_visibility_intent(&fixture.state,
        ironrag_backend::infra::repositories::admission_repository::VisibilityAdmissionRequest {
            workspace_id: fixture.workspace_id, library_id, document_id, requested_by_principal_id: None,
            request_surface: "rest".to_owned(),
            intent: ironrag_contracts::documents::CreateVisibilityIntentRequest {
                idempotency_key: format!("snapshot-intent-{}", Uuid::now_v7()),
                source_identity: "snapshot-source-binding".to_owned(), materialization_token: Some("capture-C".to_owned()),
                replacement: ironrag_contracts::documents::ReplacementIdentity {
                    checksum: format!("sha256:{}", hex::encode(<sha2::Sha256 as sha2::Digest>::digest(bytes))),
                    byte_size: bytes.len() as i64, mime_type: "text/plain".to_owned(), file_name: "complete.txt".to_owned(),
                    title: Some("Complete".to_owned()), language_code: None, source_uri: None, document_hint: None,
                },
            },
        }).await?.into_bundle().receipt)
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn archive4_roundtrip_fresh_ready_preserves_revision_admission() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = Box::pin(async {
        let source_id = create_library(&fixture, "archive3-ready-source").await?;
        let (document_id, revision_id, _) = seed_readable_content_document(&fixture, source_id).await?;
        let expected: Value = sqlx::query_scalar("select admission_identity from content_revision where id=$1")
            .bind(revision_id).fetch_one(&fixture.state.persistence.postgres).await?;
        // Mutable extraction hints do not rewrite unbound source admission.
        sqlx::query("update content_revision set document_hint='updated extraction hint' where id=$1")
            .bind(revision_id).execute(&fixture.state.persistence.postgres).await?;
        let target_id = restore_content_into_fresh_library(&fixture, source_id, "archive3-ready-target").await?;
        let current: (Uuid, Value) = sqlx::query_as("select c.revision_id, r.admission_identity from content_current_revision c join content_revision r on r.id=c.revision_id where c.document_id=$1 and c.library_id=$2")
            .bind(document_id).bind(target_id).fetch_one(&fixture.state.persistence.postgres).await?;
        assert_eq!(current, (revision_id, expected.clone()));
        let key: String = sqlx::query_scalar("select storage_key from content_revision where id=$1")
            .bind(revision_id).fetch_one(&fixture.state.persistence.postgres).await?;
        assert_eq!(fixture.state.content_storage.read_revision_source(&key).await?, b"base readable bytes");
        // Immutable identity retains original authenticated scope after relational
        // remapping, and remains valid in a second schema-3 export/restore.
        let second_target = restore_content_into_fresh_library(&fixture, target_id, "archive3-ready-second-target").await?;
        let current: Uuid = sqlx::query_scalar("select revision_id from content_current_revision where document_id=$1 and library_id=$2")
            .bind(document_id).bind(second_target).fetch_one(&fixture.state.persistence.postgres).await?;
        assert_eq!(current, revision_id);
        let restored: Value = sqlx::query_scalar("select admission_identity from content_revision where id=$1")
            .bind(revision_id).fetch_one(&fixture.state.persistence.postgres).await?;
        assert_eq!(restored, expected);
        // A separate imported fixture receives its immutable identity at INSERT,
        // never by rewriting the mutation-origin revision tested above.
        let import_library = create_library(&fixture, "archive4-import-source").await?;
        let import_document = Uuid::now_v7();
        let import_revision = Uuid::now_v7();
        let import_key = format!("content/{}/{import_library}/certified.bin", fixture.workspace_id);
        fixture.state.content_storage.write_revision_source_raw(&import_key, b"base readable bytes").await?;
        let mut replacement = expected["replacement"].clone();
        replacement["fileName"] = json!("certified.txt");
        let imported = json!({"origin": "certified_import", "replacement": replacement,
            "provenance": {"recoverySnapshotId": "a".repeat(64),
                "identityCertificateSha256": "b".repeat(64),
                "sourceBaselineSha384": ironrag_backend::services::maintenance::baseline_delta::BASELINE_REVISION_DECISION_CATALOG_SHA384,
                "admittedBy": "snapshot-test-operator"}});
        let pool = &fixture.state.persistence.postgres;
        let mut tx = pool.begin().await?;
        sqlx::query("insert into content_document(id,workspace_id,library_id,external_key,source_file_name,revision_sequence) values($1,$2,$3,'certified-fixture','changed-label.txt',1)")
            .bind(import_document).bind(fixture.workspace_id).bind(import_library).execute(&mut *tx).await?;
        sqlx::query("insert into content_revision(id,document_id,workspace_id,library_id,revision_number,content_source_kind,checksum,mime_type,byte_size,title,language_code,source_uri,document_hint,storage_key,admission_identity) select $1,$2,$3,$4,1,content_source_kind,checksum,mime_type,byte_size,title,language_code,source_uri,document_hint,$5,$6 from content_revision where id=$7")
            .bind(import_revision).bind(import_document).bind(fixture.workspace_id).bind(import_library)
            .bind(&import_key).bind(&imported).bind(revision_id).execute(&mut *tx).await?;
        sqlx::query("insert into knowledge_document(document_id,workspace_id,library_id,external_key,file_name,document_state,latest_revision_no,created_at,updated_at) values($1,$2,$3,'certified-fixture','changed-label.txt','active',1,now(),now())")
            .bind(import_document).bind(fixture.workspace_id).bind(import_library).execute(&mut *tx).await?;
        sqlx::query("insert into knowledge_revision(revision_id,workspace_id,library_id,document_id,revision_number,revision_state,revision_kind,storage_ref,mime_type,checksum,byte_size,title,text_state,vector_state,graph_state,text_readable_at,vector_ready_at,created_at) select id,workspace_id,library_id,document_id,revision_number,'completed','upload',storage_key,mime_type,checksum,byte_size,title,'text_readable','ready','ready',now(),now(),created_at from content_revision where id=$1")
            .bind(import_revision).execute(&mut *tx).await?;
        sqlx::query("insert into content_document_head(document_id,active_revision_id,readable_revision_id) values($1,$2,$2)")
            .bind(import_document).bind(import_revision).execute(&mut *tx).await?;
        sqlx::query("update knowledge_document set active_revision_id=$2,readable_revision_id=$2 where document_id=$1")
            .bind(import_document).bind(import_revision).execute(&mut *tx).await?;
        tx.commit().await?;
        let imported_target = restore_content_into_fresh_library(&fixture, import_library, "archive4-import-target").await?;
        let actual: Value = sqlx::query_scalar("select r.admission_identity from content_current_revision c join content_revision r on r.id=c.revision_id where c.document_id=$1 and c.library_id=$2")
            .bind(import_document).bind(imported_target).fetch_one(pool).await?;
        assert_eq!(actual, imported);
        let summary = fixture.state.application_services.content.get_document(&fixture.state, import_document).await?;
        assert_eq!(summary.file_name.as_deref(), Some("certified.txt"));
        Ok(())
    }).await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn archive3_cross_workspace_knowledge_source_reads_remapped_owned_blob() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = Box::pin(async {
        let source_id = create_library(&fixture, "archive3-storage-source").await?;
        let (_, revision_id, _) = seed_readable_content_document(&fixture, source_id).await?;
        let pool = &fixture.state.persistence.postgres;
        sqlx::query("update knowledge_revision k set storage_ref=r.storage_key from content_revision r where k.revision_id=r.id and r.id=$1")
            .bind(revision_id).execute(pool).await?;
        let archive = export_archive(&fixture, source_id, vec![IncludeKind::LibraryData, IncludeKind::Blobs]).await?;
        let workspace = catalog_repository::create_workspace(pool, &format!("archive3-storage-{}", Uuid::now_v7().simple()), "Storage target", None).await?;
        let target = catalog_repository::create_library(pool, workspace.id, "storage-target", "Storage target", None, None).await?;
        fixture.state.application_services.catalog.delete_library(&fixture.state, source_id).await?;
        restore_library_archive(&fixture.state, target.id, std::io::Cursor::new(archive), OverwriteMode::Reject).await?;
        let (knowledge_ref, content_key): (String, String) = sqlx::query_as("select k.storage_ref,r.storage_key from knowledge_revision k join content_revision r on r.id=k.revision_id and r.document_id=k.document_id and r.workspace_id=k.workspace_id and r.library_id=k.library_id where r.id=$1 and r.workspace_id=$2")
            .bind(revision_id).bind(workspace.id).fetch_one(pool).await?;
        assert_eq!(knowledge_ref, content_key);
        assert!(knowledge_ref.starts_with(&format!("content/{}/{}/", workspace.id, target.id)));
        // Exercise the same ContentStorage read boundary used by source consumers,
        // without invoking any extraction or vision provider.
        assert_eq!(fixture.state.content_storage.read_revision_source(&knowledge_ref).await?, b"base readable bytes");
        Ok(())
    }).await;
    fixture.cleanup().await?;
    result
}

#[derive(Debug, PartialEq, sqlx::FromRow)]
struct HistoricalAuditReservation {
    request_identity: Option<serde_json::Value>,
    request_fingerprint: Option<String>,
    origin_workspace_id: Uuid,
    origin_library_id: Uuid,
    reservation_principal_id: Uuid,
    idempotency_scope: Option<String>,
    idempotency_key: Option<String>,
    mutation_state: String,
    item_state: String,
}

async fn read_historical_audit_reservation(
    pool: &sqlx::PgPool,
    mutation_id: Uuid,
) -> Result<HistoricalAuditReservation> {
    Ok(sqlx::query_as("select a.request_identity,a.request_fingerprint,a.origin_workspace_id,a.origin_library_id,r.reservation_principal_id,r.idempotency_scope,r.idempotency_key,a.mutation_state::text,i.item_state::text from content_mutation_predecessor_audit a join content_mutation_reference r on r.id=a.id join content_mutation_item i on i.mutation_id=a.id where a.id=$1")
        .bind(mutation_id).fetch_one(pool).await?)
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn archive3_certified_upgrade_history_survives_modern_publication_and_two_restores()
-> Result<()> {
    Box::pin(certified_upgrade_history_roundtrip("applied", "applied", true)).await
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn archive3_canceled_predecessor_preserves_skipped_item_through_two_restores() -> Result<()> {
    Box::pin(certified_upgrade_history_roundtrip("canceled", "skipped", true)).await
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn archive4_immediate_certified_migration_preserves_published_head_through_two_restores()
-> Result<()> {
    Box::pin(certified_upgrade_history_roundtrip("applied", "applied", false)).await
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn archive4_immediate_migration_preserves_private_active_history_through_two_restores()
-> Result<()> {
    Box::pin(certified_upgrade_history_roundtrip("canceled", "skipped", false)).await
}

async fn certified_upgrade_history_roundtrip(
    mutation_state: &str,
    item_state: &str,
    publish_replacement: bool,
) -> Result<()> {
    use ironrag_backend::services::maintenance::baseline_delta::BASELINE_REVISION_DECISION_CATALOG_SHA384;
    use ironrag_backend::services::maintenance::content_visibility_intent_schema::{
        CertifiedImportAdmission, PREDECESSOR_BASELINE_SHA384,
        migrate_content_visibility_intent_schema,
    };
    let mut fixture = ContentLifecycleFixture::create().await?;
    let result = Box::pin(async {
        let pool = &fixture.state.persistence.postgres;
        // This database was created exclusively by ContentLifecycleFixture.
        // Never permit predecessor setup against the configured/admin database.
        let database: String = sqlx::query_scalar("select current_database()").fetch_one(pool).await?;
        anyhow::ensure!(database.starts_with("content_lifecycle_"), "predecessor setup requires isolated fixture database");
        let predecessor = include_str!("../fixtures/schema/admission_visibility_policy.sql");
        assert_eq!(hex::encode(<sha2::Sha384 as sha2::Digest>::digest(predecessor.as_bytes())), PREDECESSOR_BASELINE_SHA384);
        sqlx::raw_sql("DROP SCHEMA public CASCADE; CREATE SCHEMA public;").execute(pool).await?;
        let mut predecessor_connection = pool.acquire().await?;
        sqlx::raw_sql(predecessor).execute(&mut *predecessor_connection).await?;
        sqlx::raw_sql("SET search_path TO public;").execute(&mut *predecessor_connection).await?;
        drop(predecessor_connection);
        sqlx::raw_sql("CREATE TABLE public._sqlx_migrations (version bigint PRIMARY KEY, description text NOT NULL, installed_on timestamptz NOT NULL DEFAULT now(), success boolean NOT NULL, checksum bytea NOT NULL, execution_time bigint NOT NULL); SET search_path TO public;").execute(pool).await?;
        sqlx::query("insert into _sqlx_migrations(version,description,success,checksum,execution_time) values(1,'baseline v1',true,$1,0)")
            .bind(hex::decode(PREDECESSOR_BASELINE_SHA384)?).execute(pool).await?;
        let workspace = catalog_repository::create_workspace(pool, "archive-predecessor", "Archive predecessor", None).await?;
        fixture.workspace_id = workspace.id;
        let source = catalog_repository::create_library(pool, workspace.id, "predecessor", "Predecessor", None, None).await?;
        let document = Uuid::now_v7();
        let revision = Uuid::now_v7();
        let mutation = Uuid::now_v7();
        let item = Uuid::now_v7();
        let job = Uuid::now_v7();
        let attempt = Uuid::now_v7();
        let actor = Uuid::now_v7();
        let readable_revision = (mutation_state == "applied").then_some(revision);
        sqlx::query("insert into iam_principal(id,principal_kind,display_label,status) values($1,'user','Archive historical actor','active')").bind(actor).execute(pool).await?;
        sqlx::query("insert into content_document(id,workspace_id,library_id,external_key,source_file_name) values($1,$2,$3,'historical','historical.txt')").bind(document).bind(workspace.id).bind(source.id).execute(pool).await?;
        let old_key = format!("content/{}/{}/historical.bin", workspace.id, source.id);
        let old_bytes = b"historical source bytes";
        fixture.state.content_storage.write_revision_source_raw(&old_key, old_bytes).await?;
        sqlx::query("insert into content_revision(id,document_id,workspace_id,library_id,revision_number,content_source_kind,checksum,mime_type,byte_size,storage_key) values($1,$2,$3,$4,7,'upload',$5,'text/plain',$6,$7)")
            .bind(revision).bind(document).bind(workspace.id).bind(source.id)
            .bind(format!("sha256:{}", hex::encode(<sha2::Sha256 as sha2::Digest>::digest(old_bytes))))
            .bind(old_bytes.len() as i64).bind(&old_key).execute(pool).await?;
        sqlx::query("insert into knowledge_document(document_id,workspace_id,library_id,external_key,file_name,document_state,latest_revision_no,created_at,updated_at) values($1,$2,$3,'historical','historical.txt','active',7,now(),now())")
            .bind(document).bind(workspace.id).bind(source.id).execute(pool).await?;
        sqlx::query("insert into knowledge_revision(revision_id,workspace_id,library_id,document_id,revision_number,revision_state,revision_kind,storage_ref,mime_type,checksum,byte_size,text_state,vector_state,graph_state,text_readable_at,created_at) select id,workspace_id,library_id,document_id,revision_number,'completed','upload',storage_key,mime_type,checksum,byte_size,'text_readable','ready','ready',now(),created_at from content_revision where id=$1")
            .bind(revision).execute(pool).await?;
        sqlx::query("update knowledge_document set active_revision_id=$2,readable_revision_id=$3 where document_id=$1").bind(document).bind(revision).bind(readable_revision).execute(pool).await?;
        sqlx::query("insert into content_mutation(id,workspace_id,library_id,operation_kind,request_surface,mutation_state,requested_by_principal_id,idempotency_scope,idempotency_key,request_fingerprint) values($1,$2,$3,'replace','rest',$5::content_mutation_state,$4,'historical-scope','historical-key','historical-original-fingerprint')")
            .bind(mutation).bind(workspace.id).bind(source.id).bind(actor).bind(mutation_state).execute(pool).await?;
        sqlx::query("insert into content_mutation_item(id,mutation_id,document_id,result_revision_id,item_state) values($1,$2,$3,$4,$5::content_mutation_item_state)").bind(item).bind(mutation).bind(document).bind(revision).bind(item_state).execute(pool).await?;
        sqlx::query("insert into ingest_job(id,workspace_id,library_id,mutation_id,mutation_item_id,knowledge_document_id,knowledge_revision_id,job_kind,queue_state) values($1,$2,$3,$4,$5,$6,$7,'content_mutation','completed')").bind(job).bind(workspace.id).bind(source.id).bind(mutation).bind(item).bind(document).bind(revision).execute(pool).await?;
        sqlx::query("insert into ingest_attempt(id,job_id,attempt_number,attempt_state) values($1,$2,1,'succeeded')").bind(attempt).bind(job).execute(pool).await?;
        sqlx::query("insert into content_document_head(document_id,active_revision_id,readable_revision_id,latest_mutation_id,latest_successful_attempt_id) values($1,$2,$5,$3,$4)").bind(document).bind(revision).bind(mutation).bind(readable_revision.map(|_| attempt)).bind(readable_revision).execute(pool).await?;
        let certificate_directory = tempfile::tempdir()?;
        let admission = if readable_revision.is_some() {
            let snapshot = "a".repeat(64);
            let operator = "snapshot-test-operator";
            let certificate = json!({
                "recoverySnapshotId": snapshot,
                "sourceBaselineSha384": BASELINE_REVISION_DECISION_CATALOG_SHA384,
                "admittedBy": operator,
                "revisions": [{
                    "revisionId": revision, "documentId": document,
                    "workspaceId": workspace.id, "libraryId": source.id,
                    "storageKey": old_key,
                    "sourceEvidence": {"kind": "source_blob"},
                    "replacement": {
                        "checksum": format!("sha256:{}", hex::encode(<sha2::Sha256 as sha2::Digest>::digest(old_bytes))),
                        "byteSize": old_bytes.len() as i64, "mimeType": "text/plain",
                        "fileName": "historical.txt", "title": null,
                        "languageCode": null, "sourceUri": null, "documentHint": null
                    }
                }]
            });
            let bytes = serde_json::to_vec(&certificate)?;
            let digest = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&bytes));
            let path = certificate_directory.path().join("import-certificate.json");
            tokio::fs::write(&path, bytes).await?;
            Some(CertifiedImportAdmission::from_file(
                &path, &digest, &snapshot, BASELINE_REVISION_DECISION_CATALOG_SHA384, operator,
            ).await?)
        } else {
            None
        };
        migrate_content_visibility_intent_schema(
            pool, true, &fixture.state.content_storage, admission.as_ref(),
        ).await?;
        let preserved: (Option<Uuid>, Option<Uuid>, Option<Uuid>) = sqlx::query_as("select active_revision_id,readable_revision_id,latest_successful_attempt_id from content_document_head where document_id=$1").bind(document).fetch_one(pool).await?;
        assert_eq!(preserved, (Some(revision), readable_revision, readable_revision.map(|_| attempt)));
        let retained_attempts: Vec<Uuid> = sqlx::query_scalar("select id from ingest_attempt order by id")
            .fetch_all(pool).await?;
        assert_eq!(retained_attempts, vec![attempt], "migration must preserve actual attempt history without fabricating attempts");
        let summary = fixture.state.application_services.content.get_document(&fixture.state, document).await?;
        if readable_revision.is_some() {
            assert_eq!(summary.file_name.as_deref(), Some("historical.txt"));
            let imported: Value = sqlx::query_scalar("select admission_identity from content_revision where id=$1")
                .bind(revision).fetch_one(pool).await?;
            assert_eq!(imported["origin"], "certified_import");
            assert_eq!(imported["replacement"]["fileName"], "historical.txt");
        } else {
            let admission: Option<Value> = sqlx::query_scalar("select admission_identity from content_revision where id=$1")
                .bind(revision).fetch_one(pool).await?;
            assert_eq!(admission, None);
        }
        let historical_admission: Option<Value> = sqlx::query_scalar("select admission_identity from content_revision where id=$1")
            .bind(revision).fetch_one(pool).await?;
        let (expected_active, expected_readable) = if publish_replacement {
            let c = admit_snapshot_replacement(&fixture, source.id, document, None).await?;
            let c_item = c.items.first().context("modern C missing")?;
            let c_revision = c_item.result_revision_id.context("modern C revision missing")?;
            sqlx::query("update content_mutation_item set item_state='applied' where id=$1").bind(c_item.id).execute(pool).await?;
            sqlx::query("update content_mutation set mutation_state='applied',completed_at=now() where id=$1").bind(c.mutation.id).execute(pool).await?;
            sqlx::query("update knowledge_revision set text_state='text_readable',text_readable_at=now(),vector_state='ready',vector_ready_at=now(),graph_state='ready' where revision_id=$1").bind(c_revision).execute(pool).await?;
            sqlx::query("update content_document_head set active_revision_id=$2,readable_revision_id=$2 where document_id=$1").bind(document).bind(c_revision).execute(pool).await?;
            (Some(c_revision), Some(c_revision))
        } else {
            (Some(revision), readable_revision)
        };
        let expected = read_historical_audit_reservation(pool, mutation).await?;
        assert_eq!(expected.request_identity, None);
        assert_eq!(expected.request_fingerprint.as_deref(), Some("historical-original-fingerprint"));
        assert_eq!(expected.mutation_state, mutation_state);
        assert_eq!(expected.item_state, item_state);
        let target_workspace = catalog_repository::create_workspace(pool, "archive-upgrade-target", "Archive target", None).await?;
        let mut current_library = source.id;
        for slug in ["first-restore", "second-restore"] {
            let archive = export_archive(&fixture, current_library, vec![IncludeKind::LibraryData, IncludeKind::Blobs]).await?;
            let target = catalog_repository::create_library(pool, target_workspace.id, slug, slug, None, None).await?;
            fixture.state.application_services.catalog.delete_library(&fixture.state, current_library).await?;
            restore_library_archive(&fixture.state, target.id, std::io::Cursor::new(archive), OverwriteMode::Reject).await?;
            let actual = read_historical_audit_reservation(pool, mutation).await?;
            assert_eq!(actual, expected);
            let published: Option<Uuid> = sqlx::query_scalar("select revision_id from content_current_revision where document_id=$1 and library_id=$2").bind(document).bind(target.id).fetch_optional(pool).await?;
            assert_eq!(published, expected_readable);
            let heads: (Option<Uuid>, Option<Uuid>, Option<Uuid>) = sqlx::query_as("select active_revision_id,readable_revision_id,latest_successful_attempt_id from content_document_head where document_id=$1")
                .bind(document).fetch_one(pool).await?;
            assert_eq!(heads, (expected_active, expected_readable, None));
            let admission_after: Option<Value> = sqlx::query_scalar("select admission_identity from content_revision where id=$1")
                .bind(revision).fetch_one(pool).await?;
            assert_eq!(admission_after, historical_admission);
            let attempts: i64 = sqlx::query_scalar("select count(*) from ingest_attempt")
                .fetch_one(pool).await?;
            assert_eq!(attempts, 0, "restore must not fabricate deployment-local attempts");
            if !publish_replacement && readable_revision.is_some() {
                let summary = fixture.state.application_services.content.get_document(&fixture.state, document).await?;
                assert_eq!(summary.file_name.as_deref(), Some("historical.txt"));
            }
            let key: String = sqlx::query_scalar("select storage_key from content_revision where id=$1").bind(revision).fetch_one(pool).await?;
            assert_eq!(fixture.state.content_storage.read_revision_source(&key).await?, old_bytes);
            current_library = target.id;
        }
        Ok(())
    }).await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn archive3_unresolved_intent_retains_foreign_b_and_denies_old_a_after_restore() -> Result<()>
{
    let fixture = ContentLifecycleFixture::create().await?;
    let result = Box::pin(async {
        let source_id = create_library(&fixture, "archive3-foreign-source").await?;
        let (document_id, old_a, _) = seed_readable_content_document(&fixture, source_id).await?;
        let b = admit_snapshot_replacement(&fixture, source_id, document_id, None).await?;
        let b_item = b.items.first().context("B item missing")?;
        let b_revision = b_item.result_revision_id.context("B revision missing")?;
        let intent = admit_snapshot_intent(&fixture, source_id, document_id).await?;
        assert_eq!(intent.revision_floor, 2);
        let identity_before: serde_json::Value = sqlx::query_scalar("select request_identity from content_mutation where id=$1")
            .bind(b.mutation.id).fetch_one(&fixture.state.persistence.postgres).await?;
        let target_id = restore_content_into_fresh_library(&fixture, source_id, "archive3-foreign-target").await?;
        let lineage: (Option<Uuid>, Option<Uuid>, Option<Uuid>) = sqlx::query_as(
            "select visibility_intent_id, latest_mutation_id, readable_revision_id from content_document_head where document_id=$1")
            .bind(document_id).fetch_one(&fixture.state.persistence.postgres).await?;
        assert_eq!(lineage, (Some(intent.visibility_intent_id), Some(b.mutation.id), None));
        let identity_after: serde_json::Value = sqlx::query_scalar("select request_identity from content_mutation where id=$1")
            .bind(b.mutation.id).fetch_one(&fixture.state.persistence.postgres).await?;
        assert_eq!(identity_after, identity_before, "foreign B identity must not be rewritten");
        for (revision_id, expected_bytes) in [(old_a, b"base readable bytes".as_slice()), (b_revision, b"complete replacement bytes".as_slice())] {
            let key: String = sqlx::query_scalar("select storage_key from content_revision where id=$1 and library_id=$2")
                .bind(revision_id).bind(target_id).fetch_one(&fixture.state.persistence.postgres).await?;
            assert_eq!(fixture.state.content_storage.read_revision_source(&key).await?, expected_bytes);
        }
        let current_count: i64 = sqlx::query_scalar("select count(*) from content_current_revision where document_id=$1")
            .bind(document_id).fetch_one(&fixture.state.persistence.postgres).await?;
        assert_eq!(current_count, 0);
        let repair = ironrag_backend::services::maintenance::repair::promote_null_heads(&fixture.state, Some(target_id)).await?;
        assert_eq!(repair.promoted, 0);
        Ok(())
    }).await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn archive3_published_bound_c_preserves_intent_item_identity_and_metadata() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = Box::pin(async {
        let source_id = create_library(&fixture, "archive3-bound-source").await?;
        let (document_id, _, _) = seed_readable_content_document(&fixture, source_id).await?;
        let intent = admit_snapshot_intent(&fixture, source_id, document_id).await?;
        let c = admit_snapshot_replacement(&fixture, source_id, document_id, Some(intent.visibility_intent_id)).await?;
        let item = c.items.first().context("C item missing")?;
        let revision_id = item.result_revision_id.context("C revision missing")?;
        let pool = &fixture.state.persistence.postgres;
        let mut tx = pool.begin().await?;
        sqlx::query("update content_mutation_item set item_state='applied' where id=$1").bind(item.id).execute(&mut *tx).await?;
        sqlx::query("update content_mutation set mutation_state='applied', completed_at=now() where id=$1").bind(c.mutation.id).execute(&mut *tx).await?;
        sqlx::query("update knowledge_revision set text_state='text_readable', text_readable_at=now(), vector_state='ready', vector_ready_at=now(), graph_state='ready' where revision_id=$1")
            .bind(revision_id).execute(&mut *tx).await?;
        sqlx::query("update content_document_head set active_revision_id=$2, readable_revision_id=$2 where document_id=$1")
            .bind(document_id).bind(revision_id).execute(&mut *tx).await?;
        sqlx::query("update content_document set source_file_name='complete.txt' where id=$1").bind(document_id).execute(&mut *tx).await?;
        sqlx::query("update knowledge_document set active_revision_id=$2, readable_revision_id=$2, file_name='complete.txt', title='Complete' where document_id=$1")
            .bind(document_id).bind(revision_id).execute(&mut *tx).await?;
        tx.commit().await?;
        let current: Uuid = sqlx::query_scalar("select revision_id from content_current_revision where document_id=$1").bind(document_id).fetch_one(pool).await?;
        assert_eq!(current, revision_id, "canonical C must be public before export");
        let expected_identity: serde_json::Value = sqlx::query_scalar("select request_identity from content_mutation where id=$1").bind(c.mutation.id).fetch_one(pool).await?;
        let target_id = restore_content_into_fresh_library(&fixture, source_id, "archive3-bound-target").await?;
        let restored: (Uuid, Uuid) = sqlx::query_as("select revision_id, visibility_intent_id from content_current_revision where document_id=$1 and library_id=$2")
            .bind(document_id).bind(target_id).fetch_one(pool).await?;
        assert_eq!(restored, (revision_id, intent.visibility_intent_id));
        let restored_identity: serde_json::Value = sqlx::query_scalar("select request_identity from content_mutation where id=$1").bind(c.mutation.id).fetch_one(pool).await?;
        assert_eq!(restored_identity, expected_identity);
        let metadata: (Option<String>, Option<String>) = sqlx::query_as("select title, language_code from content_revision where id=$1")
            .bind(revision_id).fetch_one(pool).await?;
        assert_eq!(metadata, (Some("Complete".to_owned()), None));
        let floor: i32 = sqlx::query_scalar("select revision_floor from content_visibility_intent where mutation_item_id=$1")
            .bind(intent.visibility_intent_id).fetch_one(pool).await?;
        assert_eq!(floor, intent.revision_floor);
        Ok(())
    }).await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn archive_two_refuses_before_replacing_populated_target() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let source_id = create_library(&fixture, "archive2-rejected-source").await?;
        seed_readable_content_document(&fixture, source_id).await?;
        let archive =
            export_archive(&fixture, source_id, vec![IncludeKind::LibraryData, IncludeKind::Blobs])
                .await?;
        let old_archive = rewrite_manifest_schema_version(&archive, 2).await?;
        let target_id = create_library(&fixture, "archive2-retained-target").await?;
        let (document_id, revision_id, _) =
            seed_readable_content_document(&fixture, target_id).await?;
        let failure = restore_library_archive(
            &fixture.state,
            target_id,
            std::io::Cursor::new(old_archive),
            OverwriteMode::Replace,
        )
        .await
        .unwrap_err();
        assert_eq!(
            failure.failure_code(),
            SnapshotImportFailureKind::UnsupportedSchemaVersion.code()
        );
        let current: Uuid = sqlx::query_scalar(
            "select revision_id from content_current_revision where document_id=$1",
        )
        .bind(document_id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(current, revision_id);
        let key: String =
            sqlx::query_scalar("select storage_key from content_revision where id=$1")
                .bind(revision_id)
                .fetch_one(&fixture.state.persistence.postgres)
                .await?;
        assert_eq!(
            fixture.state.content_storage.read_revision_source(&key).await?,
            b"base readable bytes"
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn archive3_transaction_failure_compensates_new_source_objects_and_rows() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let source_id = create_library(&fixture, "archive3-rollback-source").await?;
        let (_, revision_id, _) = seed_readable_content_document(&fixture, source_id).await?;
        let source_key: String =
            sqlx::query_scalar("select storage_key from content_revision where id=$1")
                .bind(revision_id)
                .fetch_one(&fixture.state.persistence.postgres)
                .await?;
        let archive =
            export_archive(&fixture, source_id, vec![IncludeKind::LibraryData, IncludeKind::Blobs])
                .await?;
        fixture
            .state
            .application_services
            .catalog
            .delete_library(&fixture.state, source_id)
            .await
            .map_err(|error| anyhow::anyhow!("delete source fixture failed: {error:?}"))?;
        let target_id = create_library(&fixture, "archive3-rollback-target").await?;
        sqlx::query("update catalog_library set source_truth_version=$2 where id=$1")
            .bind(target_id)
            .bind(i64::MAX)
            .execute(&fixture.state.persistence.postgres)
            .await?;
        let failure = restore_library_archive(
            &fixture.state,
            target_id,
            std::io::Cursor::new(archive),
            OverwriteMode::Reject,
        )
        .await;
        assert!(
            failure.is_err(),
            "retained target counter exhaustion must fail after streaming rows and bytes"
        );
        let count: i64 =
            sqlx::query_scalar("select count(*) from content_document where library_id=$1")
                .bind(target_id)
                .fetch_one(&fixture.state.persistence.postgres)
                .await?;
        assert_eq!(count, 0, "transaction must roll back all incoming rows");
        let target_key = source_key.replace(&source_id.to_string(), &target_id.to_string());
        assert!(
            fixture.state.content_storage.read_revision_source(&target_key).await.is_err(),
            "failed restore must not leave an unowned source object"
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn archive3_workspace_roundtrip_retains_visibility_obligation() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let source_id = create_library(&fixture, "archive3-workspace-source").await?;
        let (document_id, _) = seed_suppressed_document(&fixture, source_id).await?;
        let expected_intent: Uuid = sqlx::query_scalar(
            "select visibility_intent_id from content_document_head where document_id=$1",
        )
        .bind(document_id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        let (mut reader, writer) = tokio::io::duplex(1024 * 1024);
        let state = fixture.state.clone();
        let workspace_id = fixture.workspace_id;
        let export = tokio::spawn(async move {
            export_workspace_archive(
                state,
                workspace_id,
                vec![IncludeKind::LibraryData, IncludeKind::Blobs],
                writer,
            )
            .await
        });
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        export.await??;
        fixture
            .state
            .application_services
            .catalog
            .delete_library(&fixture.state, source_id)
            .await
            .map_err(|error| anyhow::anyhow!("delete source fixture failed: {error:?}"))?;
        let workspace = fixture
            .state
            .application_services
            .catalog
            .create_workspace(
                &fixture.state,
                CreateWorkspaceCommand {
                    slug: Some(format!("snapshot-target-{}", Uuid::now_v7())),
                    display_name: "Archive3 Target".to_owned(),
                    created_by_principal_id: None,
                },
            )
            .await?;
        let report = restore_workspace_archive(
            &fixture.state,
            workspace.id,
            std::io::Cursor::new(bytes),
            OverwriteMode::Reject,
        )
        .await?;
        assert!(!report.libraries.is_empty());
        let restored_intent: Uuid = sqlx::query_scalar(
            "select h.visibility_intent_id from content_document_head h
            join content_document d on d.id=h.document_id where d.id=$1 and d.workspace_id=$2",
        )
        .bind(document_id)
        .bind(workspace.id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(restored_intent, expected_intent);
        let public_count: i64 = sqlx::query_scalar(
            "select count(*) from content_current_revision where document_id=$1",
        )
        .bind(document_id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(public_count, 0);
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn archive3_malformed_lineage_refuses_before_rows_and_blobs() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let source_id = create_library(&fixture, "archive3-malformed-source").await?;
        seed_suppressed_document(&fixture, source_id).await?;
        let archive =
            export_archive(&fixture, source_id, vec![IncludeKind::LibraryData, IncludeKind::Blobs])
                .await?;
        let target_id = create_library(&fixture, "archive3-malformed-target").await?;
        for (table, field) in [
            ("content_visibility_intent", "revision_floor"),
            ("content_visibility_intent", "document_id"),
            ("content_visibility_intent", "mutation_item_id"),
            ("knowledge_revision", "revision_id"),
            ("knowledge_revision", "workspace_id"),
        ] {
            let malformed = rewrite_archive_entries(&archive, |path, bytes| {
                if !path.starts_with(&format!("postgres/{table}/")) {
                    return Ok(bytes);
                }
                let mut rewritten = Vec::new();
                for line in bytes.split(|byte| *byte == b'\n').filter(|line| !line.is_empty()) {
                    let mut row: serde_json::Value = serde_json::from_slice(line)?;
                    row[field] = if field == "revision_floor" {
                        json!(i32::MAX)
                    } else {
                        json!(Uuid::now_v7())
                    };
                    serde_json::to_writer(&mut rewritten, &row)?;
                    rewritten.push(b'\n');
                }
                Ok(rewritten)
            })
            .await?;
            let failure = restore_library_archive(
                &fixture.state,
                target_id,
                std::io::Cursor::new(malformed),
                OverwriteMode::Reject,
            )
            .await
            .unwrap_err();
            assert_eq!(failure.failure_code(), SnapshotImportFailureKind::ArchiveInvalid.code());
            let counts: (i64, i64, i64) = sqlx::query_as(
                "select
                (select count(*) from content_document where library_id=$1),
                (select count(*) from content_mutation where library_id=$1),
                (select count(*) from knowledge_document where library_id=$1)",
            )
            .bind(target_id)
            .fetch_one(&fixture.state.persistence.postgres)
            .await?;
            assert_eq!(counts, (0, 0, 0), "malformed schema 3 must leave no partial target rows");
        }
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn archive_history_export_requires_system_administration_for_both_scopes() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let source_id = create_library(&fixture, "archive3-authorized-source").await?;
        seed_suppressed_document(&fixture, source_id).await?;
        let routes = [
            format!("/v1/content/libraries/{source_id}/snapshot"),
            format!("/v1/catalog/workspaces/{}/snapshot", fixture.workspace_id),
        ];
        let app = Router::new().nest("/v1", router()).with_state(fixture.state.clone());
        for permission in
            ["document_read", "document_write", "library_read", "library_write", "workspace_admin"]
        {
            let token = mint_api_token(
                &fixture.state.persistence.postgres,
                Some(fixture.workspace_id),
                "workspace",
                "snapshot-scoped-reader",
                &[permission],
            )
            .await?;
            for route in &routes {
                let response = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .uri(route)
                            .header(header::AUTHORIZATION, format!("Bearer {}", token.plaintext))
                            .body(Body::empty())?,
                    )
                    .await?;
                assert_eq!(
                    response.status(),
                    StatusCode::UNAUTHORIZED,
                    "scoped permissions must not expose source-owned history"
                );
                let method = if route.contains("/content/libraries/") { "POST" } else { "PUT" };
                let response = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .method(method)
                            .uri(route)
                            .header(header::AUTHORIZATION, format!("Bearer {}", token.plaintext))
                            .body(Body::from(
                                "untrusted archive must not be spooled or admitted",
                            ))?,
                    )
                    .await?;
                assert_eq!(
                    response.status(),
                    StatusCode::UNAUTHORIZED,
                    "scoped writers must not admit source-owned audit restore"
                );
            }
        }
        let token = mint_api_token(
            &fixture.state.persistence.postgres,
            None,
            "instance_admin",
            "snapshot-system-backup",
            &["iam_admin"],
        )
        .await?;
        for route in &routes {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(route)
                        .header(header::AUTHORIZATION, format!("Bearer {}", token.plaintext))
                        .body(Body::empty())?,
                )
                .await?;
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "an API-token IAM grant does not confer a human system role"
            );
        }
        let admin_cookie = create_admin_session_cookie(
            &fixture.state,
            "snapshot-system-backup",
            "Snapshot system administrator",
        )
        .await?;
        for route in &routes {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(route)
                        .header(header::COOKIE, &admin_cookie)
                        .body(Body::empty())?,
                )
                .await?;
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await?;
            assert!(!bytes.is_empty(), "authorized source-owned backup must remain available");
        }
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}
