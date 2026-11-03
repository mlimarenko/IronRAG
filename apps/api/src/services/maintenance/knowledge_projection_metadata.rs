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
//! Preview hashes the exact before/source metadata and exposes only IDs and
//! changed columns. Apply requires one library and that digest, revalidates under
//! row locks in a repeatable-read transaction, and writes only the approved IDs.

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracing::info;
use uuid::Uuid;

use crate::{app::state::AppState, infra::repositories::catalog_repository};

#[derive(Debug, Default, Clone, Serialize)]
pub struct KnowledgeProjectionMetadataReport {
    pub dry_run: bool,
    pub libraries_reconciled: usize,
    pub document_rows_updated: u64,
    pub revision_rows_updated: u64,
    pub plans: Vec<KnowledgeProjectionMetadataPlan>,
}

/// An exact, non-disclosing approval for one library's metadata changes.
#[derive(Debug, Clone, Serialize)]
pub struct KnowledgeProjectionMetadataPlan {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub sha256: String,
    pub documents: Vec<KnowledgeProjectionMetadataChange>,
    pub revisions: Vec<KnowledgeProjectionMetadataChange>,
}

/// Identifies the affected row and columns without exposing metadata values.
#[derive(Debug, Clone, Serialize)]
pub struct KnowledgeProjectionMetadataChange {
    pub id: Uuid,
    pub fields: Vec<String>,
}

/// Apply accepts only the digest returned by a fresh native preview.
#[derive(Debug, Clone)]
pub enum KnowledgeProjectionMetadataAction {
    Preview,
    Apply { expected_plan: String },
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct MetadataRow {
    id: Uuid,
    before: Value,
    desired: Value,
}

#[derive(Serialize)]
struct MetadataWitness<'a> {
    workspace_id: Uuid,
    library_id: Uuid,
    documents: &'a [MetadataRow],
    revisions: &'a [MetadataRow],
}

/// Preview metadata changes, or apply one explicitly approved library plan.
pub async fn knowledge_projection_metadata(
    state: &AppState,
    library_filter: Option<Uuid>,
    action: KnowledgeProjectionMetadataAction,
) -> Result<KnowledgeProjectionMetadataReport> {
    let expected_plan = match &action {
        KnowledgeProjectionMetadataAction::Preview => None,
        KnowledgeProjectionMetadataAction::Apply { expected_plan } => {
            anyhow::ensure!(
                library_filter.is_some(),
                "metadata apply requires an explicit library"
            );
            anyhow::ensure!(
                expected_plan.len() == 64
                    && expected_plan
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                "expected metadata plan must be a lowercase SHA-256 digest"
            );
            Some(expected_plan.as_str())
        }
    };
    let mut libraries =
        catalog_repository::list_libraries(&state.persistence.postgres, None).await?;
    if let Some(target) = library_filter {
        libraries.retain(|library| library.id == target);
    }
    if libraries.is_empty() {
        anyhow::bail!("no libraries matched knowledge-projection-metadata repair target");
    }

    let mut report = KnowledgeProjectionMetadataReport {
        dry_run: expected_plan.is_none(),
        ..KnowledgeProjectionMetadataReport::default()
    };
    for library in libraries {
        let plan =
            reconcile_library(state, library.workspace_id, library.id, expected_plan).await?;
        let documents =
            u64::try_from(plan.documents.len()).context("document plan count overflow")?;
        let revisions =
            u64::try_from(plan.revisions.len()).context("revision plan count overflow")?;
        report.plans.push(plan);
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
            dry_run = expected_plan.is_none(),
            "knowledge projection metadata reconciled from authoritative content rows",
        );
    }
    Ok(report)
}

const DRIFTED_DOCUMENT_PREDICATE: &str = "k.library_id = $1
       and cd.workspace_id = $2
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
       and cd.workspace_id = $2
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

async fn metadata_plan(
    connection: &mut sqlx::PgConnection,
    workspace_id: Uuid,
    library_id: Uuid,
) -> Result<KnowledgeProjectionMetadataPlan> {
    let documents: Vec<MetadataRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select k.document_id as id,
            jsonb_build_object('external_key', k.external_key, 'parent_document_id', k.parent_document_id,
                'document_role', k.document_role, 'document_state', k.document_state, 'deleted_at', k.deleted_at) as before,
            jsonb_build_object('external_key', cd.external_key, 'parent_document_id', cd.parent_document_id,
                'document_role', cd.document_role::text, 'document_state', cd.document_state::text, 'deleted_at', cd.deleted_at) as desired
         from knowledge_document k
         join content_document cd on cd.id = k.document_id
         where {DRIFTED_DOCUMENT_PREDICATE} order by k.document_id",
    )))
    .bind(library_id)
    .bind(workspace_id)
    .fetch_all(&mut *connection)
    .await
    .context("failed to plan drifted knowledge documents")?;
    let revisions: Vec<MetadataRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select kr.revision_id as id,
            jsonb_build_object('revision_number', kr.revision_number, 'revision_kind', kr.revision_kind,
                'source_uri', kr.source_uri, 'document_hint', kr.document_hint, 'mime_type', kr.mime_type,
                'checksum', kr.checksum, 'title', kr.title, 'byte_size', kr.byte_size) as before,
            jsonb_build_object('revision_number', cr.revision_number::bigint, 'revision_kind', cr.content_source_kind::text,
                'source_uri', cr.source_uri, 'document_hint', cr.document_hint, 'mime_type', cr.mime_type,
                'checksum', cr.checksum, 'title', cr.title, 'byte_size', cr.byte_size) as desired
         from knowledge_revision kr
         join content_revision cr on cr.id = kr.revision_id
         join content_document cd on cd.id = kr.document_id
         where {DRIFTED_REVISION_PREDICATE} order by kr.revision_id",
    )))
    .bind(library_id)
    .bind(workspace_id)
    .fetch_all(&mut *connection)
    .await
    .context("failed to plan drifted knowledge revisions")?;
    let witness =
        MetadataWitness { workspace_id, library_id, documents: &documents, revisions: &revisions };
    let sha256 = hex::encode(Sha256::digest(serde_json::to_vec(&witness)?));
    Ok(KnowledgeProjectionMetadataPlan {
        workspace_id,
        library_id,
        sha256,
        documents: metadata_changes(&documents)?,
        revisions: metadata_changes(&revisions)?,
    })
}

fn metadata_changes(rows: &[MetadataRow]) -> Result<Vec<KnowledgeProjectionMetadataChange>> {
    rows.iter()
        .map(|row| {
            let before =
                row.before.as_object().context("metadata before witness is not an object")?;
            let desired =
                row.desired.as_object().context("metadata source witness is not an object")?;
            anyhow::ensure!(
                before.len() == desired.len() && before.keys().all(|key| desired.contains_key(key)),
                "metadata witness key sets differ"
            );
            let mut fields: Vec<_> = before
                .iter()
                .filter(|(key, value)| desired.get(*key) != Some(*value))
                .map(|(key, _)| key.clone())
                .collect();
            fields.sort();
            Ok(KnowledgeProjectionMetadataChange { id: row.id, fields })
        })
        .collect()
}

async fn reconcile_library(
    state: &AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    expected_plan: Option<&str>,
) -> Result<KnowledgeProjectionMetadataPlan> {
    let mut transaction = state
        .persistence
        .postgres
        .begin()
        .await
        .context("failed to start knowledge-projection-metadata transaction")?;
    sqlx::query(if expected_plan.is_some() {
        "set transaction isolation level repeatable read"
    } else {
        "set transaction isolation level repeatable read, read only"
    })
    .execute(&mut *transaction)
    .await
    .context("failed to set metadata transaction mode")?;

    if expected_plan.is_some() {
        let parent_locked = catalog_repository::lock_library_for_lifecycle_event_with_executor(
            &mut *transaction,
            workspace_id,
            library_id,
        )
        .await
        .context("failed to lock library for projection metadata reconcile")?;
        anyhow::ensure!(
            parent_locked,
            "library {library_id} disappeared during metadata reconcile"
        );

        sqlx::query(sqlx::AssertSqlSafe(format!(
            "select k.document_id from knowledge_document k
         join content_document cd on cd.id = k.document_id
         where {DRIFTED_DOCUMENT_PREDICATE} order by k.document_id for update of cd, k"
        )))
        .bind(library_id)
        .bind(workspace_id)
        .fetch_all(&mut *transaction)
        .await
        .context("failed to lock metadata document rows")?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "select kr.revision_id from knowledge_revision kr
         join content_revision cr on cr.id = kr.revision_id
         join content_document cd on cd.id = kr.document_id
         where {DRIFTED_REVISION_PREDICATE} order by kr.revision_id for update of cd, cr, kr"
        )))
        .bind(library_id)
        .bind(workspace_id)
        .fetch_all(&mut *transaction)
        .await
        .context("failed to lock metadata revision rows")?;
    }

    let plan = metadata_plan(&mut transaction, workspace_id, library_id).await?;
    let Some(expected_plan) = expected_plan else {
        transaction.rollback().await.context("failed to finish metadata preview")?;
        return Ok(plan);
    };
    anyhow::ensure!(
        plan.sha256 == expected_plan,
        "metadata plan changed; obtain and review a fresh preview"
    );
    let document_ids: Vec<_> = plan.documents.iter().map(|row| row.id).collect();
    let revision_ids: Vec<_> = plan.revisions.iter().map(|row| row.id).collect();

    let documents = sqlx::query(sqlx::AssertSqlSafe(format!(
        "update knowledge_document k
         set external_key = cd.external_key,
             parent_document_id = cd.parent_document_id,
             document_role = cd.document_role::text,
             document_state = cd.document_state::text,
             deleted_at = cd.deleted_at
         from content_document cd
          where cd.id = k.document_id and {DRIFTED_DOCUMENT_PREDICATE}
            and k.document_id = any($3)",
    )))
    .bind(library_id)
    .bind(workspace_id)
    .bind(&document_ids)
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
            and {DRIFTED_REVISION_PREDICATE} and kr.revision_id = any($3)",
    )))
    .bind(library_id)
    .bind(workspace_id)
    .bind(&revision_ids)
    .execute(&mut *transaction)
    .await
    .context("failed to reconcile knowledge revision metadata")?
    .rows_affected();

    anyhow::ensure!(
        documents == u64::try_from(document_ids.len())?
            && revisions == u64::try_from(revision_ids.len())?,
        "metadata apply did not match the approved row set"
    );
    if documents > 0 || revisions > 0 {
        catalog_repository::touch_library_source_truth_version_with_executor(
            &mut *transaction,
            library_id,
        )
        .await
        .context("failed to advance library source truth version after metadata reconcile")?;
    }
    transaction.commit().await.context("failed to commit knowledge-projection-metadata repair")?;
    Ok(plan)
}
