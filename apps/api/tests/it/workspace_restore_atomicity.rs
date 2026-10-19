//! IRG-76: a failed workspace-bundle import must leave ZERO libraries it
//! created behind.
//!
//! The workspace restore has no single transaction: each embedded library is a
//! separate `PostgreSQL` transaction and its blobs are written outside
//! `PostgreSQL` entirely, so the all-or-nothing property is kept by
//! compensating — deleting every library the import minted when any library
//! fails. This suite proves that boundary really spans the whole loop: one
//! intact library commits FIRST, the next fails mid-restore, and both must be
//! gone. It also checks that a library in a neighbouring workspace is untouched.
//!
//! The failure is injected inside the second library's own restore (not in
//! prevalidation): its `postgres/knowledge_revision` part gets a duplicated row
//! and the inner `summary.json` is brought back into agreement, so the archive
//! still prevalidates. `knowledge_revision` inserts with
//! `ON CONFLICT (revision_id) DO NOTHING` and a `MustWriteEveryRow` policy, so
//! the duplicate is dropped by Postgres and the restore refuses with
//! `snapshot_rows_lost_rejection` — a per-library restore failure after the
//! target library row was already created.

use anyhow::{Context, Result};
use async_compression::tokio::{bufread::ZstdDecoder, write::ZstdEncoder};
use async_tar::{Archive, Builder, EntryType, Header};
use chrono::Utc;
use futures::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

use ironrag_backend::{
    app::state::AppState,
    infra::repositories::{
        catalog_repository::{self, CatalogLibraryDeleteOutcome},
        content_revision_repository::{self, NewContentRevision},
    },
    services::{
        catalog_service::{CreateLibraryCommand, CreateWorkspaceCommand},
        content::service::snapshot::{
            IncludeKind, OverwriteMode, SNAPSHOT_SCHEMA_VERSION, SnapshotEntryDigest,
            WorkspaceManifestLibrary, WorkspaceSnapshotInventory, WorkspaceSnapshotManifest,
            WorkspaceSnapshotSummary, export_library_archive, restore_workspace_archive,
        },
        knowledge::service::{CreateKnowledgeDocumentCommand, CreateKnowledgeRevisionCommand},
    },
};

use crate::content_lifecycle_support::ContentLifecycleFixture;

async fn create_workspace(state: &AppState, label: &str) -> Result<Uuid> {
    let workspace = state
        .application_services
        .catalog
        .create_workspace(
            state,
            CreateWorkspaceCommand {
                slug: Some(format!("irg76-{label}-{}", Uuid::now_v7().simple())),
                display_name: format!("IRG-76 {label} workspace"),
                created_by_principal_id: None,
            },
        )
        .await
        .with_context(|| format!("create {label} workspace"))?;
    Ok(workspace.id)
}

async fn create_library_in(
    state: &AppState,
    workspace_id: Uuid,
    label: &str,
) -> Result<(Uuid, String)> {
    let library = state
        .application_services
        .catalog
        .create_library(
            state,
            CreateLibraryCommand {
                workspace_id,
                slug: Some(format!("irg76-{label}-{}", Uuid::now_v7().simple())),
                display_name: format!("{label} library"),
                description: None,
                created_by_principal_id: None,
            },
        )
        .await
        .with_context(|| format!("create {label} library"))?;
    Ok((library.id, library.slug))
}

/// Seeds one document + revision so the library has a `knowledge_revision`
/// row the exporter writes as an NDJSON part.
async fn seed_library(state: &AppState, workspace_id: Uuid, library_id: Uuid) -> Result<()> {
    let document_id = Uuid::now_v7();
    let source_id = Uuid::now_v7();
    let storage_key = format!("content/{workspace_id}/{library_id}/irg76/{source_id}.txt");
    let source_uri = format!("memory://irg76/{source_id}");
    let checksum = format!("sha256:{0}{0}", source_id.simple());
    // The archive closes over the source plane, not merely the derived rows.
    // Reserve numbering while the source's owning lifecycle transaction is held.
    let mut transaction = state.persistence.postgres.begin().await?;
    anyhow::ensure!(
        catalog_repository::lock_library_for_lifecycle_event_with_executor(
            &mut *transaction,
            workspace_id,
            library_id,
        )
        .await?,
        "IRG-76 source library is unavailable"
    );
    sqlx::query(
        "insert into content_document (id, workspace_id, library_id, external_key, document_state)
         values ($1, $2, $3, $4, 'active')",
    )
    .bind(document_id)
    .bind(workspace_id)
    .bind(library_id)
    .bind(format!("irg76/{document_id}"))
    .execute(&mut *transaction)
    .await?;
    let revision_number = content_revision_repository::allocate_revision_number_with_executor(
        &mut *transaction,
        document_id,
    )
    .await?;
    let revision = content_revision_repository::create_revision_with_executor(
        &mut *transaction,
        &NewContentRevision {
            admission_identity: Some(&crate::content_repository::synthetic_import_admission(
                ironrag_contracts::documents::ReplacementIdentity {
                    checksum: checksum.clone(),
                    mime_type: "text/plain".to_owned(),
                    byte_size: 64,
                    file_name: "irg76.md".to_owned(),
                    title: Some("IRG-76".to_owned()),
                    language_code: None,
                    source_uri: Some(source_uri.clone()),
                    document_hint: None,
                },
            )?),
            document_id,
            workspace_id,
            library_id,
            revision_number,
            parent_revision_id: None,
            content_source_kind: "upload",
            checksum: &checksum,
            mime_type: "text/plain",
            byte_size: 64,
            title: Some("IRG-76"),
            language_code: None,
            source_uri: Some(&source_uri),
            document_hint: None,
            storage_key: Some(&storage_key),
            created_by_principal_id: None,
        },
    )
    .await?;
    transaction.commit().await?;
    let revision_id = revision.id;
    state
        .application_services
        .knowledge
        .create_document_shell(
            state,
            CreateKnowledgeDocumentCommand {
                document_id,
                workspace_id,
                library_id,
                external_key: format!("irg76/{document_id}"),
                file_name: Some("irg76.md".to_string()),
                title: Some("IRG-76".to_string()),
                document_state: "active".to_string(),
            },
        )
        .await
        .context("create IRG-76 document shell")?;
    state
        .application_services
        .knowledge
        .write_revision(
            state,
            CreateKnowledgeRevisionCommand {
                revision_id,
                workspace_id,
                library_id,
                document_id,
                revision_number: i64::from(revision.revision_number),
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: Some(storage_key),
                source_uri: Some(source_uri),
                document_hint: None,
                mime_type: "text/plain".to_string(),
                checksum,
                byte_size: 64,
                title: Some("IRG-76".to_string()),
                normalized_text: Some("IRG-76 atomicity probe.".to_string()),
                text_checksum: Some(format!("sha256:text:{revision_id}")),
                text_state: "text_readable".to_string(),
                vector_state: "ready".to_string(),
                graph_state: "accepted".to_string(),
                text_readable_at: Some(Utc::now()),
                vector_ready_at: Some(Utc::now()),
                graph_ready_at: None,
                superseded_by_revision_id: None,
            },
        )
        .await
        .context("write IRG-76 knowledge revision")?;
    crate::content_lifecycle_support::seed_readable_revision_head(
        &state.persistence.postgres,
        document_id,
        revision_id,
        "irg76.md",
    )
    .await?;
    crate::content_lifecycle_support::assert_current_revision(
        &state.persistence.postgres,
        document_id,
        revision_id,
    )
    .await?;
    Ok(())
}

async fn export_library_bytes(state: &AppState, library_id: Uuid) -> Result<Vec<u8>> {
    let (writer, mut reader) = tokio::io::duplex(64 * 1024);
    let state = state.clone();
    let exporter = tokio::spawn(async move {
        Box::pin(export_library_archive(state, library_id, vec![IncludeKind::LibraryData], writer))
            .await
    });
    let mut bytes = Vec::new();
    AsyncReadExt::read_to_end(&mut reader, &mut bytes)
        .await
        .context("read exported library archive")?;
    exporter.await.context("export task panicked")?.context("export failed")?;
    Ok(bytes)
}

async fn append_entry<W>(builder: &mut Builder<W>, path: &str, bytes: &[u8]) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin + Send + Sync,
{
    let mut header = Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_entry_type(EntryType::Regular);
    header.set_cksum();
    builder
        .append_data(&mut header, path, bytes)
        .await
        .with_context(|| format!("append workspace tar entry `{path}`"))?;
    Ok(())
}

/// Rewrites the library archive so its `knowledge_revision` part carries a
/// duplicated row and its inner `summary.json` still matches the observed
/// inventory (count and part digest), keeping the tamper invisible to
/// prevalidation while it deterministically fails the `DO NOTHING` insert.
async fn duplicate_first_knowledge_revision_row(archive: &[u8]) -> Result<Vec<u8>> {
    let decoder = ZstdDecoder::new(tokio::io::BufReader::new(archive));
    let source = Archive::new(decoder);
    let mut entries = source.entries().context("open exported library archive")?;
    let mut collected: Vec<(String, Vec<u8>)> = Vec::new();
    while let Some(entry) = entries.next().await {
        let mut entry = entry.context("read exported library archive entry")?;
        let path = entry.path().context("read entry path")?.to_string_lossy().into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).await.with_context(|| format!("read `{path}`"))?;
        collected.push((path, bytes));
    }

    let mut inflated: Option<(String, u64, String)> = None;
    for (path, bytes) in &mut collected {
        if path.starts_with("postgres/knowledge_revision/part-") {
            let first_line_end =
                bytes.iter().position(|byte| *byte == b'\n').map_or_else(|| bytes.len(), |i| i + 1);
            anyhow::ensure!(first_line_end > 0, "knowledge_revision part `{path}` is empty");
            let first_line = bytes[..first_line_end].to_vec();
            bytes.extend_from_slice(&first_line);
            let size = bytes.len() as u64;
            let sha256 = hex::encode(Sha256::digest(bytes.as_slice()));
            inflated = Some((path.clone(), size, sha256));
            break;
        }
    }
    let (part_path, part_size, part_sha256) =
        inflated.context("archive has no knowledge_revision ndjson part")?;

    let mut summary_rewritten = false;
    for (path, bytes) in &mut collected {
        if path == "summary.json" {
            let mut summary: serde_json::Value =
                serde_json::from_slice(bytes).context("parse exported summary.json")?;
            let previous =
                summary["postgres_row_counts"]["knowledge_revision"].as_u64().unwrap_or(0);
            summary["postgres_row_counts"]["knowledge_revision"] = serde_json::json!(previous + 1);
            let parts =
                summary["postgres_parts"].as_array_mut().context("summary.postgres_parts array")?;
            let mut updated = false;
            for part in parts.iter_mut() {
                if part.get("path").and_then(serde_json::Value::as_str) == Some(part_path.as_str())
                {
                    part["size"] = serde_json::json!(part_size);
                    part["sha256"] = serde_json::json!(part_sha256);
                    updated = true;
                }
            }
            anyhow::ensure!(updated, "summary.json does not declare part `{part_path}`");
            *bytes = serde_json::to_vec_pretty(&summary).context("re-serialize summary.json")?;
            summary_rewritten = true;
        }
    }
    anyhow::ensure!(summary_rewritten, "archive has no summary.json");

    let mut rewritten = Vec::new();
    let encoder = ZstdEncoder::new(&mut rewritten);
    let mut builder = Builder::new(encoder);
    for (path, bytes) in &collected {
        append_entry(&mut builder, path, bytes).await?;
    }
    let mut encoder = builder.into_inner().await.context("finalize tampered tar")?;
    encoder.shutdown().await.context("finalize tampered zstd stream")?;
    drop(encoder);
    Ok(rewritten)
}

async fn build_workspace_bundle(
    source_workspace_id: Uuid,
    libraries: &[(Uuid, String, String)],
    archives: &[(Uuid, Vec<u8>)],
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    // Production reads a plain (uncompressed) outer tar whose inner library
    // entries are already zstd archives, so the fixture must write the same
    // shape. A zstd-wrapped outer would fail prevalidation before any library
    // is provisioned and silently make this suite vacuous.
    let mut builder = Builder::new(&mut out);

    let manifest = WorkspaceSnapshotManifest {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        workspace_id: source_workspace_id,
        workspace_slug: "irg76-source".to_string(),
        exported_at: Utc::now(),
        source_version: "0.0.0-test".to_string(),
        include_kinds: vec![IncludeKind::LibraryData],
        libraries: libraries
            .iter()
            .map(|(id, slug, display_name)| WorkspaceManifestLibrary {
                id: *id,
                slug: slug.clone(),
                display_name: display_name.clone(),
            })
            .collect(),
    };
    append_entry(
        &mut builder,
        "workspace-manifest.json",
        &serde_json::to_vec_pretty(&manifest).context("serialize workspace manifest")?,
    )
    .await?;

    let mut inventory = Vec::with_capacity(archives.len());
    for (id, bytes) in archives {
        let path = format!("libraries/{id}.tar.zst");
        inventory.push(SnapshotEntryDigest {
            path: path.clone(),
            size: bytes.len() as u64,
            sha256: hex::encode(Sha256::digest(bytes.as_slice())),
        });
        append_entry(&mut builder, &path, bytes).await?;
    }
    let summary =
        WorkspaceSnapshotSummary { inventory: WorkspaceSnapshotInventory { libraries: inventory } };
    append_entry(
        &mut builder,
        "workspace-summary.json",
        &serde_json::to_vec_pretty(&summary).context("serialize workspace summary")?,
    )
    .await?;

    builder.into_inner().await.context("finalize workspace tar")?;
    Ok(out)
}

async fn library_count(state: &AppState, workspace_id: Uuid) -> Result<i64> {
    sqlx::query_scalar::<_, i64>("select count(*) from catalog_library where workspace_id = $1")
        .bind(workspace_id)
        .fetch_one(&state.persistence.postgres)
        .await
        .context("count libraries in workspace")
}

/// Makes every readiness derivation fail for as long as the outage is in
/// effect by renaming the projection column its query selects
/// (`ai_binding.binding_purpose`).
///
/// Renaming the column rather than the relation is deliberate. The readiness
/// query is the only consumer of that column, while the relation itself stays
/// readable by the delete's own cascade: `ai_account`'s AFTER DELETE trigger
/// reads `ai_binding`, so a whole-relation outage would break an unrelated
/// dependency and test the wrong thing. This models the readiness derivation
/// being unavailable — the outage the compensation must survive — not the
/// catalog delete machinery being absent.
async fn begin_readiness_outage(postgres: &sqlx::PgPool) -> Result<()> {
    sqlx::query(
        "alter table ai_binding rename column binding_purpose to binding_purpose_irg76_outage",
    )
    .execute(postgres)
    .await
    .context("rename ai_binding.binding_purpose to begin the readiness outage")?;
    Ok(())
}

async fn end_readiness_outage(postgres: &sqlx::PgPool) -> Result<()> {
    sqlx::query(
        "alter table ai_binding rename column binding_purpose_irg76_outage to binding_purpose",
    )
    .execute(postgres)
    .await
    .context("rename ai_binding.binding_purpose back to end the readiness outage")?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn a_failed_library_restore_leaves_zero_imported_libraries() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let source_workspace_id = create_workspace(&fixture.state, "source").await?;
        let control_workspace_id = create_workspace(&fixture.state, "control").await?;

        let (control_library_id, _) =
            create_library_in(&fixture.state, control_workspace_id, "control").await?;
        seed_library(&fixture.state, control_workspace_id, control_library_id).await?;

        let (alpha_id, alpha_slug) =
            create_library_in(&fixture.state, source_workspace_id, "alpha").await?;
        seed_library(&fixture.state, source_workspace_id, alpha_id).await?;
        let (zulu_id, zulu_slug) =
            create_library_in(&fixture.state, source_workspace_id, "zulu").await?;
        seed_library(&fixture.state, source_workspace_id, zulu_id).await?;

        let alpha_archive = export_library_bytes(&fixture.state, alpha_id).await?;
        let zulu_archive = export_library_bytes(&fixture.state, zulu_id).await?;
        let zulu_tampered = duplicate_first_knowledge_revision_row(&zulu_archive).await?;
        assert_ne!(zulu_tampered, zulu_archive, "the tamper must change the archive");

        // Drop both sources before importing. While they live, the archive's
        // global primary keys (`knowledge_document.document_id`,
        // `knowledge_revision.revision_id`) collide with their rows, so the
        // FIRST library's own restore would refuse with
        // `snapshot_source_already_present` and the tamper below would never be
        // reached. With the sources gone, alpha restores and commits cleanly
        // and the tampered zulu is the only thing that can fail. The control
        // library lives in another workspace and is deliberately untouched.
        for source_id in [alpha_id, zulu_id] {
            fixture
                .state
                .application_services
                .catalog
                .delete_library(&fixture.state, source_id)
                .await
                .map_err(|error| anyhow::anyhow!("delete_library failed: {error:?}"))?;
        }

        // Manifest order is the restore order: alpha restores and commits
        // first, then zulu fails. The test therefore proves the boundary
        // removes an already-successful library too.
        let libraries = vec![
            (alpha_id, alpha_slug, "alpha library".to_string()),
            (zulu_id, zulu_slug, "zulu library".to_string()),
        ];
        let archives = vec![(alpha_id, alpha_archive), (zulu_id, zulu_tampered)];
        let bundle = build_workspace_bundle(source_workspace_id, &libraries, &archives).await?;

        let target_workspace_id = create_workspace(&fixture.state, "target").await?;
        assert_eq!(
            library_count(&fixture.state, target_workspace_id).await?,
            0,
            "target workspace must start empty"
        );

        let restore = restore_workspace_archive(
            &fixture.state,
            target_workspace_id,
            &bundle[..],
            OverwriteMode::Replace,
        )
        .await;
        assert!(
            restore.is_err(),
            "a library whose restore fails must fail the workspace import: {restore:?}"
        );

        assert_eq!(
            library_count(&fixture.state, target_workspace_id).await?,
            0,
            "a failed workspace import must leave zero libraries it created behind"
        );
        assert_eq!(
            library_count(&fixture.state, control_workspace_id).await?,
            1,
            "a neighbouring workspace's library must survive the failure"
        );
        let control_survived: bool =
            sqlx::query_scalar("select exists(select 1 from catalog_library where id = $1)")
                .bind(control_library_id)
                .fetch_one(&fixture.state.persistence.postgres)
                .await
                .context("check control library survived")?;
        assert!(control_survived, "control library {control_library_id} must still exist");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

/// IRG-76 residual path: the compensation delete must not derive readiness.
///
/// `unwind_workspace_import_libraries` used to delete through the public
/// `delete_library`, which loads the library through `get_library` first and so
/// derives readiness from the active `ai_binding` rows. While that derivation
/// is unavailable the delete failed and the library the failed import minted
/// stayed behind. This run keeps the readiness derivation failing for the whole
/// import: the import still fails after a library was already committed, and
/// that library must still be gone afterwards.
#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn a_readiness_outage_does_not_strand_imported_libraries() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let source_workspace_id = create_workspace(&fixture.state, "outage-source").await?;

        let (alpha_id, alpha_slug) =
            create_library_in(&fixture.state, source_workspace_id, "outage-alpha").await?;
        seed_library(&fixture.state, source_workspace_id, alpha_id).await?;
        let (zulu_id, zulu_slug) =
            create_library_in(&fixture.state, source_workspace_id, "outage-zulu").await?;
        seed_library(&fixture.state, source_workspace_id, zulu_id).await?;

        let alpha_archive = export_library_bytes(&fixture.state, alpha_id).await?;
        let zulu_archive = export_library_bytes(&fixture.state, zulu_id).await?;
        let zulu_tampered = duplicate_first_knowledge_revision_row(&zulu_archive).await?;

        // As in the first scenario, drop both sources so their global primary
        // keys do not collide with the restored rows. This runs before the
        // outage begins, so the operator-facing delete still derives readiness.
        for source_id in [alpha_id, zulu_id] {
            fixture
                .state
                .application_services
                .catalog
                .delete_library(&fixture.state, source_id)
                .await
                .map_err(|error| anyhow::anyhow!("delete_library failed: {error:?}"))?;
        }

        let libraries = vec![
            (alpha_id, alpha_slug, "outage alpha library".to_string()),
            (zulu_id, zulu_slug, "outage zulu library".to_string()),
        ];
        let archives = vec![(alpha_id, alpha_archive), (zulu_id, zulu_tampered)];
        let bundle = build_workspace_bundle(source_workspace_id, &libraries, &archives).await?;

        let target_workspace_id = create_workspace(&fixture.state, "outage-target").await?;
        assert_eq!(
            library_count(&fixture.state, target_workspace_id).await?,
            0,
            "target workspace must start empty"
        );

        begin_readiness_outage(&fixture.state.persistence.postgres).await?;
        let restore = restore_workspace_archive(
            &fixture.state,
            target_workspace_id,
            &bundle[..],
            OverwriteMode::Replace,
        )
        .await;
        end_readiness_outage(&fixture.state.persistence.postgres).await?;

        assert!(
            restore.is_err(),
            "a workspace import must fail while the readiness source is out: {restore:?}"
        );
        assert_eq!(
            library_count(&fixture.state, target_workspace_id).await?,
            0,
            "a readiness outage must not strand a library the failed import created"
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

/// IRG-76 residual path: the restore-only delete is fenced by the exact
/// `(library_id, workspace_id)` pair the restore captured.
///
/// Compensation runs with only that pair, so a wrong workspace id must be a
/// `NotFound` rather than a delete: otherwise a compensation bug could remove a
/// library that lives in a neighbouring workspace. The same call with the
/// correct pair removes the library, so the witness also shows the fence is a
/// scope predicate and not a delete that always refuses.
#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn a_restore_delete_outside_its_workspace_is_not_found() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let target_workspace_id = create_workspace(&fixture.state, "fence-target").await?;
        let control_workspace_id = create_workspace(&fixture.state, "fence-control").await?;
        let (control_library_id, _) =
            create_library_in(&fixture.state, control_workspace_id, "fence-control").await?;
        seed_library(&fixture.state, control_workspace_id, control_library_id).await?;

        let postgres = &fixture.state.persistence.postgres;
        let mismatched = catalog_repository::delete_library_for_restore(
            postgres,
            control_library_id,
            target_workspace_id,
        )
        .await
        .context("mismatched restore delete must be answered, not error")?;
        assert!(
            matches!(mismatched, CatalogLibraryDeleteOutcome::NotFound),
            "a control library under another workspace must be NotFound, got {mismatched:?}"
        );
        assert_eq!(
            library_count(&fixture.state, control_workspace_id).await?,
            1,
            "the mismatched delete must not remove the control library"
        );

        let matched = catalog_repository::delete_library_for_restore(
            postgres,
            control_library_id,
            control_workspace_id,
        )
        .await
        .context("matching restore delete must be answered")?;
        assert!(
            matches!(matched, CatalogLibraryDeleteOutcome::Deleted),
            "the matching pair must still delete, got {matched:?}"
        );
        assert_eq!(
            library_count(&fixture.state, control_workspace_id).await?,
            0,
            "the matching delete must remove the control library"
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}
