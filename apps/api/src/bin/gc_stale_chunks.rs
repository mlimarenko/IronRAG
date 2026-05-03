//! Garbage-collect stale chunk and chunk-vector rows whose revision no
//! longer matches the document's canonical heads in
//! `knowledge_document`.
//!
//! Canonical rule:
//! - keep rows where `revision_id == readable_revision_id`
//! - keep rows where `revision_id == active_revision_id`
//! - skip documents where both heads are null
//! - delete everything else
//!
//! Usage:
//!   ironrag-gc-stale-chunks                         # all libraries
//!   ironrag-gc-stale-chunks <library-uuid>          # one library
//!
//! Set `IRONRAG_GC_DRY_RUN=1` to count without deleting.
//!
//! Per-document batching: the original library-wide AQL OOMed on Arango's
//! per-query memory cap (256 MB on stage) for large record-stream
//! libraries with ~16k stale vectors. Each per-document AQL is bounded
//! by chunks-per-document and stays well under the cap, so the tool now
//! scales linearly with library size without operator-side
//! `--query.memory-limit` overrides.

use anyhow::Context;
use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::{
        arangodb::{
            client::ArangoClient,
            collections::{
                KNOWLEDGE_CHUNK_COLLECTION, KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
                KNOWLEDGE_DOCUMENT_COLLECTION,
            },
        },
        repositories::catalog_repository,
    },
};
use serde::de::DeserializeOwned;
use tracing::{info, warn};
use uuid::Uuid;

const COUNT_SKIPPED_NULL_HEAD_DOCS_AQL: &str = r"
RETURN LENGTH(
    FOR doc IN @@document_collection
        FILTER doc.library_id == @library_id
        FILTER doc.readable_revision_id == null
            AND doc.active_revision_id == null
        RETURN 1
)";

const DELETE_STALE_CHUNKS_FOR_DOC_AQL: &str = r"
RETURN LENGTH(
    FOR chunk IN @@chunk_collection
        FILTER chunk.document_id == @document_id
        FILTER chunk.revision_id NOT IN @canonical_revision_ids
        REMOVE chunk IN @@chunk_collection
        RETURN 1
)";

const COUNT_STALE_CHUNKS_FOR_DOC_AQL: &str = r"
RETURN LENGTH(
    FOR chunk IN @@chunk_collection
        FILTER chunk.document_id == @document_id
        FILTER chunk.revision_id NOT IN @canonical_revision_ids
        RETURN 1
)";

const DELETE_STALE_VECTORS_FOR_DOC_AQL: &str = r"
RETURN LENGTH(
    FOR vector IN @@vector_collection
        FILTER vector.library_id == @library_id
        FILTER vector.revision_id IN @stale_revision_ids
        REMOVE vector IN @@vector_collection
        RETURN 1
)";

const COUNT_STALE_VECTORS_FOR_DOC_AQL: &str = r"
RETURN LENGTH(
    FOR vector IN @@vector_collection
        FILTER vector.library_id == @library_id
        FILTER vector.revision_id IN @stale_revision_ids
        RETURN 1
)";

#[derive(Debug, Clone, Copy, Default)]
struct LibraryGcCounts {
    stale_chunks_removed: i64,
    stale_vectors_removed: i64,
    skipped_null_head_docs: i64,
    documents_visited: i64,
    documents_with_stale: i64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let settings = Settings::from_env()?;
    ironrag_backend::shared::telemetry::init(&settings.log_filter);
    let state = AppState::new(settings).await?;

    let mut args = std::env::args().skip(1);
    let target_library_id = args.next().map(|value| Uuid::parse_str(&value)).transpose()?;
    if args.next().is_some() {
        anyhow::bail!("usage: ironrag-gc-stale-chunks [library-uuid]");
    }

    let dry_run = matches!(std::env::var("IRONRAG_GC_DRY_RUN").as_deref(), Ok("1"));
    let libraries = catalog_repository::list_libraries(&state.persistence.postgres, None).await?;
    let libraries: Vec<_> = match target_library_id {
        Some(library_id) => {
            libraries.into_iter().filter(|library| library.id == library_id).collect()
        }
        None => libraries,
    };
    if libraries.is_empty() {
        anyhow::bail!("no libraries matched stale chunk gc target");
    }

    info!(dry_run, library_count = libraries.len(), "starting stale chunk gc");

    let mut totals = LibraryGcCounts::default();
    for library in libraries {
        match gc_library(&state, library.workspace_id, library.id, dry_run)
            .await
            .with_context(|| format!("failed stale chunk gc for library {}", library.id))
        {
            Ok(counts) => {
                totals.stale_chunks_removed += counts.stale_chunks_removed;
                totals.stale_vectors_removed += counts.stale_vectors_removed;
                totals.skipped_null_head_docs += counts.skipped_null_head_docs;
                totals.documents_visited += counts.documents_visited;
                totals.documents_with_stale += counts.documents_with_stale;
                info!(
                    library_id = %library.id,
                    workspace_id = %library.workspace_id,
                    library_name = %library.display_name,
                    dry_run,
                    documents_visited = counts.documents_visited,
                    documents_with_stale = counts.documents_with_stale,
                    stale_chunks_removed = counts.stale_chunks_removed,
                    stale_vectors_removed = counts.stale_vectors_removed,
                    skipped_null_head_docs = counts.skipped_null_head_docs,
                    "stale chunk gc completed",
                );
            }
            Err(error) => {
                warn!(
                    library_id = %library.id,
                    workspace_id = %library.workspace_id,
                    library_name = %library.display_name,
                    dry_run,
                    ?error,
                    "stale chunk gc failed; continuing with next library",
                );
            }
        }
    }

    info!(
        dry_run,
        total_documents_visited = totals.documents_visited,
        total_documents_with_stale = totals.documents_with_stale,
        total_stale_chunks_removed = totals.stale_chunks_removed,
        total_stale_vectors_removed = totals.stale_vectors_removed,
        total_skipped_null_head_docs = totals.skipped_null_head_docs,
        "stale chunk gc finished"
    );

    Ok(())
}

async fn gc_library(
    state: &AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    dry_run: bool,
) -> anyhow::Result<LibraryGcCounts> {
    let skipped_null_head_docs = query_scalar_i64(
        state.arango_document_store.client(),
        COUNT_SKIPPED_NULL_HEAD_DOCS_AQL,
        serde_json::json!({
            "@document_collection": KNOWLEDGE_DOCUMENT_COLLECTION,
            "library_id": library_id,
        }),
    )
    .await
    .context("failed to count skipped null-head documents")?;

    let documents = state
        .arango_document_store
        .list_documents_by_library(workspace_id, library_id, false)
        .await
        .context("failed to list documents for stale chunk gc")?;

    let mut counts = LibraryGcCounts { skipped_null_head_docs, ..LibraryGcCounts::default() };
    for document in &documents {
        counts.documents_visited += 1;
        match gc_document(state, library_id, document, dry_run).await {
            Ok(per_doc) => {
                if per_doc.stale_chunks_removed > 0 || per_doc.stale_vectors_removed > 0 {
                    counts.documents_with_stale += 1;
                }
                counts.stale_chunks_removed += per_doc.stale_chunks_removed;
                counts.stale_vectors_removed += per_doc.stale_vectors_removed;
            }
            Err(error) => {
                warn!(
                    library_id = %library_id,
                    document_id = %document.document_id,
                    ?error,
                    "stale chunk gc failed for document; continuing",
                );
            }
        }
    }

    Ok(counts)
}

#[derive(Debug, Clone, Copy, Default)]
struct DocumentGcCounts {
    stale_chunks_removed: i64,
    stale_vectors_removed: i64,
}

async fn gc_document(
    state: &AppState,
    library_id: Uuid,
    document: &ironrag_backend::infra::arangodb::document_store::KnowledgeDocumentRow,
    dry_run: bool,
) -> anyhow::Result<DocumentGcCounts> {
    let canonical_revision_ids: Vec<Uuid> =
        [document.readable_revision_id, document.active_revision_id]
            .into_iter()
            .flatten()
            .collect();
    if canonical_revision_ids.is_empty() {
        return Ok(DocumentGcCounts::default());
    }

    let revisions = state
        .arango_document_store
        .list_revisions_by_document(document.document_id)
        .await
        .with_context(|| {
            format!("failed to list revisions for document {}", document.document_id)
        })?;
    let stale_revision_ids: Vec<Uuid> = revisions
        .into_iter()
        .map(|revision| revision.revision_id)
        .filter(|revision_id| !canonical_revision_ids.contains(revision_id))
        .collect();

    let stale_chunks_removed = query_scalar_i64(
        state.arango_document_store.client(),
        if dry_run { COUNT_STALE_CHUNKS_FOR_DOC_AQL } else { DELETE_STALE_CHUNKS_FOR_DOC_AQL },
        serde_json::json!({
            "@chunk_collection": KNOWLEDGE_CHUNK_COLLECTION,
            "document_id": document.document_id,
            "canonical_revision_ids": canonical_revision_ids,
        }),
    )
    .await
    .with_context(|| {
        format!("failed to count/delete stale chunks for document {}", document.document_id)
    })?;

    let stale_vectors_removed = if stale_revision_ids.is_empty() {
        0
    } else {
        query_scalar_i64(
            state.arango_search_store.client(),
            if dry_run { COUNT_STALE_VECTORS_FOR_DOC_AQL } else { DELETE_STALE_VECTORS_FOR_DOC_AQL },
            serde_json::json!({
                "@vector_collection": KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
                "library_id": library_id,
                "stale_revision_ids": stale_revision_ids,
            }),
        )
        .await
        .with_context(|| {
            format!(
                "failed to count/delete stale chunk vectors for document {}",
                document.document_id
            )
        })?
    };

    Ok(DocumentGcCounts { stale_chunks_removed, stale_vectors_removed })
}

async fn query_scalar_i64(
    client: &ArangoClient,
    query: &str,
    bind_vars: serde_json::Value,
) -> anyhow::Result<i64> {
    query_single_row(client, query, bind_vars).await
}

async fn query_single_row<T: DeserializeOwned>(
    client: &ArangoClient,
    query: &str,
    bind_vars: serde_json::Value,
) -> anyhow::Result<T> {
    let cursor = client.query_json(query, bind_vars).await.with_context(|| {
        format!("arangodb query failed: {}", query.chars().take(96).collect::<String>())
    })?;
    let rows =
        cursor.get("result").cloned().context("arangodb cursor payload missing result field")?;
    let mut rows: Vec<T> =
        serde_json::from_value(rows).context("failed to deserialize arangodb query result")?;
    rows.pop().context("expected one arangodb result row")
}
