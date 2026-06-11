//! `audit.*` read-only sweepers.
//!
//! Audit entry points only inspect cross-store state — they never
//! mutate Postgres or ArangoDB. Their destructive counterparts (e.g.
//! `gc.orphan-libraries` with `--purge`) live in the [`crate::services::
//! maintenance::gc`] module so the read/write split is visible in the
//! module layout.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use anyhow::{Context, anyhow};
use serde::Serialize;
use tracing::info;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::{
        arangodb::{
            client::ArangoClient,
            collections::{
                KNOWLEDGE_BLOCK_CHUNK_EDGE, KNOWLEDGE_BUNDLE_CHUNK_EDGE,
                KNOWLEDGE_BUNDLE_ENTITY_EDGE, KNOWLEDGE_BUNDLE_EVIDENCE_EDGE,
                KNOWLEDGE_BUNDLE_RELATION_EDGE, KNOWLEDGE_CHUNK_COLLECTION,
                KNOWLEDGE_CHUNK_MENTIONS_ENTITY_EDGE, KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
                KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION, KNOWLEDGE_DOCUMENT_COLLECTION,
                KNOWLEDGE_DOCUMENT_REVISION_EDGE, KNOWLEDGE_ENTITY_COLLECTION,
                KNOWLEDGE_ENTITY_VECTOR_COLLECTION, KNOWLEDGE_EVIDENCE_COLLECTION,
                KNOWLEDGE_EVIDENCE_SOURCE_EDGE, KNOWLEDGE_EVIDENCE_SUPPORTS_ENTITY_EDGE,
                KNOWLEDGE_EVIDENCE_SUPPORTS_RELATION_EDGE, KNOWLEDGE_FACT_EVIDENCE_EDGE,
                KNOWLEDGE_RELATION_COLLECTION, KNOWLEDGE_RELATION_OBJECT_EDGE,
                KNOWLEDGE_RELATION_SUBJECT_EDGE, KNOWLEDGE_REVISION_BLOCK_EDGE,
                KNOWLEDGE_REVISION_CHUNK_EDGE, KNOWLEDGE_REVISION_COLLECTION,
                KNOWLEDGE_STRUCTURED_BLOCK_COLLECTION, KNOWLEDGE_STRUCTURED_REVISION_COLLECTION,
                KNOWLEDGE_TECHNICAL_FACT_COLLECTION,
            },
        },
        repositories::catalog_repository,
    },
};

const KNOWLEDGE_DOC_COLLECTIONS: &[&str] = &[
    KNOWLEDGE_DOCUMENT_COLLECTION,
    KNOWLEDGE_REVISION_COLLECTION,
    KNOWLEDGE_CHUNK_COLLECTION,
    KNOWLEDGE_STRUCTURED_REVISION_COLLECTION,
    KNOWLEDGE_STRUCTURED_BLOCK_COLLECTION,
    KNOWLEDGE_TECHNICAL_FACT_COLLECTION,
    KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
    KNOWLEDGE_ENTITY_VECTOR_COLLECTION,
    KNOWLEDGE_ENTITY_COLLECTION,
    KNOWLEDGE_RELATION_COLLECTION,
    KNOWLEDGE_EVIDENCE_COLLECTION,
    KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION,
];

const KNOWLEDGE_EDGE_COLLECTIONS: &[&str] = &[
    KNOWLEDGE_DOCUMENT_REVISION_EDGE,
    KNOWLEDGE_REVISION_BLOCK_EDGE,
    KNOWLEDGE_REVISION_CHUNK_EDGE,
    KNOWLEDGE_BLOCK_CHUNK_EDGE,
    KNOWLEDGE_CHUNK_MENTIONS_ENTITY_EDGE,
    KNOWLEDGE_RELATION_SUBJECT_EDGE,
    KNOWLEDGE_RELATION_OBJECT_EDGE,
    KNOWLEDGE_EVIDENCE_SOURCE_EDGE,
    KNOWLEDGE_FACT_EVIDENCE_EDGE,
    KNOWLEDGE_EVIDENCE_SUPPORTS_ENTITY_EDGE,
    KNOWLEDGE_EVIDENCE_SUPPORTS_RELATION_EDGE,
    KNOWLEDGE_BUNDLE_CHUNK_EDGE,
    KNOWLEDGE_BUNDLE_ENTITY_EDGE,
    KNOWLEDGE_BUNDLE_RELATION_EDGE,
    KNOWLEDGE_BUNDLE_EVIDENCE_EDGE,
];

/// Full audit report. `orphan_libraries` enumerates the per-collection
/// row counts for each `library_id` present in Arango that is not
/// (anymore) in Postgres `catalog_library`. `totals` rolls those up by
/// collection.
#[derive(Debug, Default, Clone, Serialize)]
pub struct OrphanLibrariesAudit {
    pub orphan_libraries: Vec<OrphanLibraryEntry>,
    pub totals: BTreeMap<String, u64>,
    pub live_library_count: usize,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrphanLibraryEntry {
    pub library_id: String,
    pub collections: BTreeMap<String, u64>,
}

/// Scan every ArangoDB `knowledge_*` collection for rows whose
/// `library_id` does not match a live PostgreSQL `catalog_library` row.
/// Read-only — does not mutate either store. Used by the operator CLI
/// `audit orphan-libraries` and by the destructive `gc orphan-libraries`
/// path to decide what to purge.
pub async fn orphan_libraries(state: &AppState) -> anyhow::Result<OrphanLibrariesAudit> {
    let live_libraries =
        catalog_repository::list_libraries(&state.persistence.postgres, None).await?;
    let live_ids: HashSet<Uuid> = live_libraries.iter().map(|row| row.id).collect();

    match state.settings.knowledge_plane_backend.as_str() {
        "arango" => {}
        "postgres" => {
            let note =
                "not applicable for postgres knowledge plane: Arango orphan-library audit skipped";
            info!(
                knowledge_plane_backend = "postgres",
                "skipping Arango orphan-library audit on postgres knowledge plane",
            );
            return Ok(OrphanLibrariesAudit {
                orphan_libraries: Vec::new(),
                totals: BTreeMap::new(),
                live_library_count: live_ids.len(),
                note: Some(note.to_string()),
            });
        }
        backend => return Err(anyhow!("unsupported knowledge_plane_backend `{backend}`")),
    }

    let arango = state.arango_client.as_ref();
    let mut doc_collections: Vec<String> =
        KNOWLEDGE_DOC_COLLECTIONS.iter().map(|s| (*s).to_string()).collect();
    doc_collections.extend(
        arango
            .list_per_dim_chunk_vector_collections()
            .await
            .context("list per-dim chunk vector shards")?,
    );
    doc_collections.extend(
        arango
            .list_per_dim_entity_vector_collections()
            .await
            .context("list per-dim entity vector shards")?,
    );
    let edge_collections: Vec<String> =
        KNOWLEDGE_EDGE_COLLECTIONS.iter().map(|s| (*s).to_string()).collect();

    let mut totals: BTreeMap<String, u64> = BTreeMap::new();
    let mut per_library: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let mut all_collections = doc_collections;
    all_collections.extend(edge_collections);
    for collection in &all_collections {
        let counts = count_rows_by_library(arango, collection)
            .await
            .with_context(|| format!("count rows by library in {collection}"))?;
        for (library_id, count) in counts {
            if let Ok(uuid) = Uuid::parse_str(&library_id)
                && live_ids.contains(&uuid)
            {
                continue;
            }
            *totals.entry(collection.clone()).or_default() += count;
            *per_library.entry(library_id).or_default().entry(collection.clone()).or_default() +=
                count;
        }
    }

    let mut orphan_libraries = Vec::with_capacity(per_library.len());
    for (library_id, collections) in per_library {
        orphan_libraries.push(OrphanLibraryEntry { library_id, collections });
    }
    Ok(OrphanLibrariesAudit {
        orphan_libraries,
        totals,
        live_library_count: live_ids.len(),
        note: None,
    })
}

/// Set of orphan library ids parsed from an [`OrphanLibrariesAudit`].
/// Used by the destructive `gc.orphan_libraries` purge to know which
/// library footprints to clear without re-running the full audit.
#[must_use]
pub fn orphan_library_ids(audit: &OrphanLibrariesAudit) -> BTreeSet<Uuid> {
    audit
        .orphan_libraries
        .iter()
        .filter_map(|entry| Uuid::parse_str(&entry.library_id).ok())
        .collect()
}

async fn count_rows_by_library(
    arango: &ArangoClient,
    collection: &str,
) -> anyhow::Result<Vec<(String, u64)>> {
    let cursor = arango
        .query_json_bulk(
            "FOR row IN @@collection \
                COLLECT library_id = row.library_id WITH COUNT INTO count \
                RETURN { library_id, count }",
            serde_json::json!({"@collection": collection}),
        )
        .await
        .with_context(|| format!("aql COLLECT WITH COUNT against {collection}"))?;
    let result = cursor
        .get("result")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow!("arango COLLECT response missing result array for {collection}"))?;
    let mut out = Vec::with_capacity(result.len());
    for row in result {
        let library_id = match row.get("library_id") {
            Some(serde_json::Value::String(value)) if !value.is_empty() => value.clone(),
            Some(serde_json::Value::Null) | None => "(no library_id)".to_string(),
            Some(other) => other.to_string(),
        };
        let count = row
            .get("count")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| anyhow!("arango COLLECT row missing numeric count for {collection}"))?;
        out.push((library_id, count));
    }
    Ok(out)
}
