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

use anyhow::Context;
use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::{
        arangodb::{
            client::ArangoClient,
            collections::{
                KNOWLEDGE_CHUNK_COLLECTION, KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
                KNOWLEDGE_DOCUMENT_COLLECTION, KNOWLEDGE_REVISION_COLLECTION,
            },
        },
        repositories::catalog_repository,
    },
};
use serde::{Deserialize, de::DeserializeOwned};
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

const DELETE_STALE_CHUNKS_AQL: &str = r"
RETURN LENGTH(
    FOR chunk IN @@chunk_collection
        FILTER chunk.library_id == @library_id
        LET doc = DOCUMENT(CONCAT(@document_collection_name, '/', TO_STRING(chunk.document_id)))
        FILTER doc != null
        FILTER doc.readable_revision_id != null
            OR doc.active_revision_id != null
        FILTER chunk.revision_id != doc.readable_revision_id
            AND chunk.revision_id != doc.active_revision_id
        REMOVE chunk IN @@chunk_collection
        RETURN 1
)";

const COUNT_STALE_CHUNKS_AQL: &str = r"
RETURN LENGTH(
    FOR chunk IN @@chunk_collection
        FILTER chunk.library_id == @library_id
        LET doc = DOCUMENT(CONCAT(@document_collection_name, '/', TO_STRING(chunk.document_id)))
        FILTER doc != null
        FILTER doc.readable_revision_id != null
            OR doc.active_revision_id != null
        FILTER chunk.revision_id != doc.readable_revision_id
            AND chunk.revision_id != doc.active_revision_id
        RETURN 1
)";

macro_rules! stale_vector_scan_aql {
    () => {
        r"
    FOR vector IN @@vector_collection
        FILTER vector.library_id == @library_id
        LET live_chunk = DOCUMENT(CONCAT(@chunk_collection_name, '/', TO_STRING(vector.chunk_id)))
        LET revision = DOCUMENT(CONCAT(@revision_collection_name, '/', TO_STRING(vector.revision_id)))
        LET doc = revision == null
            ? null
            : DOCUMENT(CONCAT(@document_collection_name, '/', TO_STRING(revision.document_id)))
        LET has_document_heads = doc != null
            AND (doc.readable_revision_id != null OR doc.active_revision_id != null)
        LET is_stale_revision = has_document_heads
            AND vector.revision_id != doc.readable_revision_id
            AND vector.revision_id != doc.active_revision_id
"
    };
}

const DELETE_STALE_VECTORS_AQL: &str = concat!(
    "RETURN LENGTH(\n",
    stale_vector_scan_aql!(),
    "        FILTER is_stale_revision\n",
    "        REMOVE vector IN @@vector_collection\n",
    "        RETURN 1\n",
    ")"
);

const COUNT_STALE_VECTORS_AQL: &str = concat!(
    "RETURN LENGTH(\n",
    stale_vector_scan_aql!(),
    "        FILTER is_stale_revision\n",
    "        RETURN 1\n",
    ")"
);

const STATS_STALE_VECTORS_AQL: &str = concat!(
    "LET stats = FIRST(\n",
    stale_vector_scan_aql!(),
    "        COLLECT AGGREGATE\n",
    "            total_vectors = COUNT(1),\n",
    "            live_chunk_vectors = SUM(live_chunk != null ? 1 : 0),\n",
    "            orphan_vectors = SUM(live_chunk == null ? 1 : 0),\n",
    "            missing_revision_vectors = SUM(revision == null ? 1 : 0),\n",
    "            missing_document_vectors = SUM(revision != null AND doc == null ? 1 : 0),\n",
    "            headless_document_vectors = SUM(doc != null AND has_document_heads == false ? 1 : 0),\n",
    "            stale_revision_vectors = SUM(is_stale_revision ? 1 : 0)\n",
    "        RETURN {\n",
    "            total_vectors,\n",
    "            live_chunk_vectors,\n",
    "            orphan_vectors,\n",
    "            missing_revision_vectors,\n",
    "            missing_document_vectors,\n",
    "            headless_document_vectors,\n",
    "            stale_revision_vectors\n",
    "        }\n",
    ")\n",
    "RETURN stats == null ? {\n",
    "    total_vectors: 0,\n",
    "    live_chunk_vectors: 0,\n",
    "    orphan_vectors: 0,\n",
    "    missing_revision_vectors: 0,\n",
    "    missing_document_vectors: 0,\n",
    "    headless_document_vectors: 0,\n",
    "    stale_revision_vectors: 0\n",
    "} : stats"
);

#[derive(Debug, Clone, Copy, Default)]
struct LibraryGcCounts {
    stale_chunks_removed: i64,
    stale_vectors_removed: i64,
    skipped_null_head_docs: i64,
}

#[derive(Debug, Clone, Deserialize)]
struct StaleVectorStats {
    total_vectors: i64,
    live_chunk_vectors: i64,
    orphan_vectors: i64,
    missing_revision_vectors: i64,
    missing_document_vectors: i64,
    headless_document_vectors: i64,
    stale_revision_vectors: i64,
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
        match gc_library(&state, library.id, dry_run)
            .await
            .with_context(|| format!("failed stale chunk gc for library {}", library.id))
        {
            Ok(counts) => {
                totals.stale_chunks_removed += counts.stale_chunks_removed;
                totals.stale_vectors_removed += counts.stale_vectors_removed;
                totals.skipped_null_head_docs += counts.skipped_null_head_docs;
                info!(
                    library_id = %library.id,
                    workspace_id = %library.workspace_id,
                    library_name = %library.display_name,
                    dry_run,
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
        total_stale_chunks_removed = totals.stale_chunks_removed,
        total_stale_vectors_removed = totals.stale_vectors_removed,
        total_skipped_null_head_docs = totals.skipped_null_head_docs,
        "stale chunk gc finished"
    );

    Ok(())
}

async fn gc_library(
    state: &AppState,
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

    // Diagnostic stats query — purely observational. On large libraries
    // the multi-step LET/COLLECT chain over `knowledge_chunk_vector` blows
    // Arango's per-query memory cap (256 MB on prod). Failing the stats
    // pass must NOT block the actual GC, so we log the breakdown when it
    // succeeds and swallow the error otherwise.
    match query_single_row::<StaleVectorStats>(
        state.arango_search_store.client(),
        STATS_STALE_VECTORS_AQL,
        vector_gc_bind_vars(library_id),
    )
    .await
    {
        Ok(vector_stats) => info!(
            library_id = %library_id,
            dry_run,
            total_vectors = vector_stats.total_vectors,
            live_chunk_vectors = vector_stats.live_chunk_vectors,
            orphan_vectors = vector_stats.orphan_vectors,
            missing_revision_vectors = vector_stats.missing_revision_vectors,
            missing_document_vectors = vector_stats.missing_document_vectors,
            headless_document_vectors = vector_stats.headless_document_vectors,
            stale_revision_vectors = vector_stats.stale_revision_vectors,
            "stale chunk gc vector stats",
        ),
        Err(error) => warn!(
            library_id = %library_id,
            ?error,
            "vector stats query failed; continuing without diagnostic breakdown",
        ),
    }

    let stale_vectors_removed = query_scalar_i64(
        state.arango_search_store.client(),
        if dry_run { COUNT_STALE_VECTORS_AQL } else { DELETE_STALE_VECTORS_AQL },
        vector_gc_bind_vars(library_id),
    )
    .await
    .context("failed to count/delete stale chunk vectors")?;

    let stale_chunks_removed = query_scalar_i64(
        state.arango_document_store.client(),
        if dry_run { COUNT_STALE_CHUNKS_AQL } else { DELETE_STALE_CHUNKS_AQL },
        chunk_gc_bind_vars(library_id),
    )
    .await
    .context("failed to count/delete stale chunks")?;

    Ok(LibraryGcCounts { stale_chunks_removed, stale_vectors_removed, skipped_null_head_docs })
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

fn chunk_gc_bind_vars(library_id: Uuid) -> serde_json::Value {
    serde_json::json!({
        "@chunk_collection": KNOWLEDGE_CHUNK_COLLECTION,
        "document_collection_name": KNOWLEDGE_DOCUMENT_COLLECTION,
        "library_id": library_id,
    })
}

fn vector_gc_bind_vars(library_id: Uuid) -> serde_json::Value {
    serde_json::json!({
        "@vector_collection": KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
        "document_collection_name": KNOWLEDGE_DOCUMENT_COLLECTION,
        "chunk_collection_name": KNOWLEDGE_CHUNK_COLLECTION,
        "revision_collection_name": KNOWLEDGE_REVISION_COLLECTION,
        "library_id": library_id,
    })
}
