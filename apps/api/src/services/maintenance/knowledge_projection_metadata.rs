//! `repair knowledge-projection-metadata` — reconcile drifted knowledge-plane
//! metadata from stored content.
//!
//! The readable-snapshot status compares document and revision metadata
//! field-by-field. Historical backfills that touched only stored content leave
//! the projection stale and cause answers to carry a catching-up warning.
//!
//! Content is authoritative for lifecycle as well as metadata. This repair copies
//! document fields `external_key`, `parent_document_id`, `document_role`,
//! `document_state`, `deleted_at`; revision fields:
//! `revision_number`, `revision_kind`, `source_uri`, `document_hint`,
//! `mime_type`, `checksum`, `title`, `byte_size` — from stored rows to
//! their existing, same-scope projection counterparts for active, non-deleted
//! content, only where a field
//! actually differs. Chunk-level divergence (text, spans, checksums) is out of
//! scope on purpose: stale chunk text means the projection genuinely needs
//! re-ingest, not a metadata copy. The library source-truth generation is
//! advanced when anything changed so every process re-evaluates parity.
//! Idempotent.

use anyhow::{Context, Result};
use serde::Serialize;
use tracing::info;
use uuid::Uuid;

use crate::{app::state::AppState, infra::repositories::catalog_repository};

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct KnowledgeProjectionMetadataReport {
    pub libraries_reconciled: usize,
    pub document_rows_updated: u64,
    pub revision_rows_updated: u64,
}

/// Reconcile projection metadata for one library or every library.
pub async fn knowledge_projection_metadata(
    state: &AppState,
    library_filter: Option<Uuid>,
    dry_run: bool,
) -> Result<KnowledgeProjectionMetadataReport> {
    let mut libraries =
        catalog_repository::list_libraries(&state.persistence.postgres, None).await?;
    if let Some(target) = library_filter {
        libraries.retain(|library| library.id == target);
    }
    if libraries.is_empty() {
        anyhow::bail!("no libraries matched knowledge-projection-metadata repair target");
    }

    let mut report = KnowledgeProjectionMetadataReport::default();
    for library in libraries {
        let (documents, revisions) = if dry_run {
            count_drifted_rows(state, library.id).await?
        } else {
            reconcile_library(state, library.workspace_id, library.id).await?
        };
        if documents == 0 && revisions == 0 {
            continue;
        }
        report.libraries_reconciled += 1;
        report.document_rows_updated += documents;
        report.revision_rows_updated += revisions;
        info!(
            library_id = %library.id,
            document_rows = documents,
            revision_rows = revisions,
            dry_run,
            "knowledge projection metadata reconciled from authoritative content rows",
        );
    }
    Ok(report)
}

const DRIFTED_DOCUMENT_PREDICATE: &str = "k.library_id = $1
       and cd.workspace_id = k.workspace_id
       and cd.library_id = k.library_id
       and cd.document_state = 'active'
       and cd.deleted_at is null
       and (cd.external_key is distinct from k.external_key
            or cd.parent_document_id is distinct from k.parent_document_id
            or cd.document_role::text is distinct from k.document_role
            or cd.document_state::text is distinct from k.document_state
            or cd.deleted_at is distinct from k.deleted_at)";

const DRIFTED_REVISION_PREDICATE: &str = "kr.library_id = $1
       and cd.workspace_id = kr.workspace_id
       and cd.library_id = kr.library_id
       and cr.workspace_id = kr.workspace_id
       and cr.library_id = kr.library_id
       and cr.document_id = kr.document_id
       and cd.document_state = 'active'
       and cd.deleted_at is null
       and (cr.revision_number::bigint is distinct from kr.revision_number
            or cr.content_source_kind::text is distinct from kr.revision_kind
            or cr.source_uri is distinct from kr.source_uri
            or cr.document_hint is distinct from kr.document_hint
            or cr.mime_type is distinct from kr.mime_type
            or cr.checksum is distinct from kr.checksum
            or cr.title is distinct from kr.title
            or cr.byte_size is distinct from kr.byte_size)";

async fn count_drifted_rows(state: &AppState, library_id: Uuid) -> Result<(u64, u64)> {
    let documents: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select count(*) from knowledge_document k
         join content_document cd on cd.id = k.document_id
         where {DRIFTED_DOCUMENT_PREDICATE}",
    )))
    .bind(library_id)
    .fetch_one(&state.persistence.postgres)
    .await
    .context("failed to count drifted knowledge documents")?;
    let revisions: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select count(*) from knowledge_revision kr
         join content_revision cr on cr.id = kr.revision_id
         join content_document cd on cd.id = kr.document_id
         where {DRIFTED_REVISION_PREDICATE}",
    )))
    .bind(library_id)
    .fetch_one(&state.persistence.postgres)
    .await
    .context("failed to count drifted knowledge revisions")?;
    Ok((u64::try_from(documents).unwrap_or_default(), u64::try_from(revisions).unwrap_or_default()))
}

async fn reconcile_library(
    state: &AppState,
    workspace_id: Uuid,
    library_id: Uuid,
) -> Result<(u64, u64)> {
    let mut transaction = state
        .persistence
        .postgres
        .begin()
        .await
        .context("failed to start knowledge-projection-metadata transaction")?;
    let parent_locked = catalog_repository::lock_library_for_lifecycle_event_with_executor(
        &mut *transaction,
        workspace_id,
        library_id,
    )
    .await
    .context("failed to lock library for projection metadata reconcile")?;
    anyhow::ensure!(parent_locked, "library {library_id} disappeared during metadata reconcile");

    let documents = sqlx::query(sqlx::AssertSqlSafe(format!(
        "update knowledge_document k
         set external_key = cd.external_key,
             parent_document_id = cd.parent_document_id,
             document_role = cd.document_role::text,
             document_state = cd.document_state::text,
             deleted_at = cd.deleted_at
         from content_document cd
         where cd.id = k.document_id and {DRIFTED_DOCUMENT_PREDICATE}",
    )))
    .bind(library_id)
    .execute(&mut *transaction)
    .await
    .context("failed to reconcile knowledge document metadata")?
    .rows_affected();

    let revisions = sqlx::query(sqlx::AssertSqlSafe(format!(
        "update knowledge_revision kr
         set revision_number = cr.revision_number::bigint,
             revision_kind = cr.content_source_kind::text,
             source_uri = cr.source_uri,
             document_hint = cr.document_hint,
             mime_type = cr.mime_type,
             checksum = cr.checksum,
             title = cr.title,
             byte_size = cr.byte_size
         from content_revision cr, content_document cd
         where cr.id = kr.revision_id and cd.id = kr.document_id
           and {DRIFTED_REVISION_PREDICATE}",
    )))
    .bind(library_id)
    .execute(&mut *transaction)
    .await
    .context("failed to reconcile knowledge revision metadata")?
    .rows_affected();

    if documents > 0 || revisions > 0 {
        catalog_repository::touch_library_source_truth_version_with_executor(
            &mut *transaction,
            library_id,
        )
        .await
        .context("failed to advance library source truth version after metadata reconcile")?;
    }
    transaction.commit().await.context("failed to commit knowledge-projection-metadata repair")?;
    Ok((documents, revisions))
}
