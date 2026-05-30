#![allow(
    clippy::missing_errors_doc,
    clippy::redundant_clone,
    clippy::struct_excessive_bools,
    clippy::too_many_lines
)]

use anyhow::Context;

use crate::infra::arangodb::{
    client::ArangoClient,
    collections::{
        DOCUMENT_COLLECTIONS, EDGE_COLLECTIONS, KNOWLEDGE_BLOCK_CHUNK_EDGE,
        KNOWLEDGE_BUNDLE_CHUNK_EDGE, KNOWLEDGE_BUNDLE_ENTITY_EDGE, KNOWLEDGE_BUNDLE_EVIDENCE_EDGE,
        KNOWLEDGE_BUNDLE_RELATION_EDGE, KNOWLEDGE_CHUNK_COLLECTION,
        KNOWLEDGE_CHUNK_MENTIONS_ENTITY_EDGE, KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
        KNOWLEDGE_CHUNK_VECTOR_INDEX, KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION,
        KNOWLEDGE_DOCUMENT_COLLECTION, KNOWLEDGE_DOCUMENT_REVISION_EDGE,
        KNOWLEDGE_ENTITY_COLLECTION, KNOWLEDGE_ENTITY_VECTOR_COLLECTION,
        KNOWLEDGE_ENTITY_VECTOR_INDEX, KNOWLEDGE_EVIDENCE_COLLECTION,
        KNOWLEDGE_EVIDENCE_SOURCE_EDGE, KNOWLEDGE_EVIDENCE_SUPPORTS_ENTITY_EDGE,
        KNOWLEDGE_EVIDENCE_SUPPORTS_RELATION_EDGE, KNOWLEDGE_FACT_EVIDENCE_EDGE,
        KNOWLEDGE_GRAPH_NAME, KNOWLEDGE_NGRAM_ANALYZER, KNOWLEDGE_PERSISTENT_INDEXES,
        KNOWLEDGE_RELATION_COLLECTION, KNOWLEDGE_RELATION_OBJECT_EDGE,
        KNOWLEDGE_RELATION_SUBJECT_EDGE, KNOWLEDGE_REVISION_BLOCK_EDGE,
        KNOWLEDGE_REVISION_CHUNK_EDGE, KNOWLEDGE_REVISION_COLLECTION, KNOWLEDGE_SEARCH_VIEW,
        KNOWLEDGE_STRUCTURED_BLOCK_COLLECTION, KNOWLEDGE_TECHNICAL_FACT_COLLECTION,
        VectorShardKind, chunk_vector_index_for_dim, chunk_vector_index_for_library,
        entity_vector_index_for_dim, parse_library_vector_shard,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArangoBootstrapOptions {
    pub collections: bool,
    pub views: bool,
    pub graph: bool,
    pub vector_indexes: bool,
    pub vector_dimensions: u64,
    pub vector_index_n_lists: u64,
    pub vector_index_default_n_probe: u64,
    pub vector_index_training_iterations: u64,
}

impl ArangoBootstrapOptions {
    #[must_use]
    pub const fn any_enabled(&self) -> bool {
        self.collections || self.views || self.graph || self.vector_indexes
    }
}

pub async fn bootstrap_knowledge_plane(
    client: &ArangoClient,
    options: &ArangoBootstrapOptions,
) -> anyhow::Result<()> {
    if options.collections {
        for collection in DOCUMENT_COLLECTIONS {
            client
                .ensure_document_collection(collection)
                .await
                .with_context(|| format!("failed to ensure knowledge collection {collection}"))?;
        }
        for collection in EDGE_COLLECTIONS {
            client.ensure_edge_collection(collection).await.with_context(|| {
                format!("failed to ensure knowledge edge collection {collection}")
            })?;
        }
        for index in KNOWLEDGE_PERSISTENT_INDEXES {
            client
                .ensure_persistent_index(
                    index.collection,
                    index.name,
                    index.fields,
                    index.unique,
                    index.sparse,
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to ensure persistent Arango index {} on {}",
                        index.name, index.collection
                    )
                })?;
        }
    }

    if options.views {
        // Custom trigram analyzer used by the title-match subquery to
        // stay tolerant to small spelling variants and single-character
        // typos in document titles. The default text stemmers produce
        // different stems for variant forms so a plain TOKENS()-based
        // SEARCH misses; NGRAM_MATCH against a 3-gram index of the same
        // field catches them. Must exist before `ensure_view` so the
        // view link can reference it.
        client
            .ensure_analyzer(
                KNOWLEDGE_NGRAM_ANALYZER,
                "ngram",
                serde_json::json!({
                    "min": 3,
                    "max": 3,
                    "preserveOriginal": false,
                    "streamType": "utf8"
                }),
                &["frequency", "norm", "position"],
            )
            .await
            .context("failed to ensure ironrag_ngram analyzer")?;
        let links = knowledge_search_view_links();
        client
            .ensure_view(KNOWLEDGE_SEARCH_VIEW, links)
            .await
            .context("failed to ensure ArangoSearch knowledge view")?;
    }

    if options.graph {
        let edge_definitions = serde_json::json!([
            {
                "collection": KNOWLEDGE_DOCUMENT_REVISION_EDGE,
                "from": [KNOWLEDGE_DOCUMENT_COLLECTION],
                "to": [KNOWLEDGE_REVISION_COLLECTION]
            },
            {
                "collection": KNOWLEDGE_REVISION_BLOCK_EDGE,
                "from": [KNOWLEDGE_REVISION_COLLECTION],
                "to": [KNOWLEDGE_STRUCTURED_BLOCK_COLLECTION]
            },
            {
                "collection": KNOWLEDGE_REVISION_CHUNK_EDGE,
                "from": [KNOWLEDGE_REVISION_COLLECTION],
                "to": [KNOWLEDGE_CHUNK_COLLECTION]
            },
            {
                "collection": KNOWLEDGE_BLOCK_CHUNK_EDGE,
                "from": [KNOWLEDGE_STRUCTURED_BLOCK_COLLECTION],
                "to": [KNOWLEDGE_CHUNK_COLLECTION]
            },
            {
                "collection": KNOWLEDGE_CHUNK_MENTIONS_ENTITY_EDGE,
                "from": [KNOWLEDGE_CHUNK_COLLECTION],
                "to": [KNOWLEDGE_ENTITY_COLLECTION]
            },
            {
                "collection": KNOWLEDGE_RELATION_SUBJECT_EDGE,
                "from": [KNOWLEDGE_RELATION_COLLECTION],
                "to": [KNOWLEDGE_ENTITY_COLLECTION]
            },
            {
                "collection": KNOWLEDGE_RELATION_OBJECT_EDGE,
                "from": [KNOWLEDGE_RELATION_COLLECTION],
                "to": [KNOWLEDGE_ENTITY_COLLECTION]
            },
            {
                "collection": KNOWLEDGE_EVIDENCE_SOURCE_EDGE,
                "from": [KNOWLEDGE_EVIDENCE_COLLECTION],
                "to": [KNOWLEDGE_CHUNK_COLLECTION]
            },
            {
                "collection": KNOWLEDGE_FACT_EVIDENCE_EDGE,
                "from": [KNOWLEDGE_TECHNICAL_FACT_COLLECTION],
                "to": [KNOWLEDGE_EVIDENCE_COLLECTION]
            },
            {
                "collection": KNOWLEDGE_EVIDENCE_SUPPORTS_ENTITY_EDGE,
                "from": [KNOWLEDGE_EVIDENCE_COLLECTION],
                "to": [KNOWLEDGE_ENTITY_COLLECTION]
            },
            {
                "collection": KNOWLEDGE_EVIDENCE_SUPPORTS_RELATION_EDGE,
                "from": [KNOWLEDGE_EVIDENCE_COLLECTION],
                "to": [KNOWLEDGE_RELATION_COLLECTION]
            },
            {
                "collection": KNOWLEDGE_BUNDLE_CHUNK_EDGE,
                "from": [KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION],
                "to": [KNOWLEDGE_CHUNK_COLLECTION]
            },
            {
                "collection": KNOWLEDGE_BUNDLE_ENTITY_EDGE,
                "from": [KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION],
                "to": [KNOWLEDGE_ENTITY_COLLECTION]
            },
            {
                "collection": KNOWLEDGE_BUNDLE_RELATION_EDGE,
                "from": [KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION],
                "to": [KNOWLEDGE_RELATION_COLLECTION]
            },
            {
                "collection": KNOWLEDGE_BUNDLE_EVIDENCE_EDGE,
                "from": [KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION],
                "to": [KNOWLEDGE_EVIDENCE_COLLECTION]
            }
        ]);
        client
            .ensure_named_graph(KNOWLEDGE_GRAPH_NAME, edge_definitions)
            .await
            .context("failed to ensure knowledge named graph")?;
    }

    if options.vector_indexes {
        let chunk_dimensions = client
            .vector_index_dimensions(
                KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
                KNOWLEDGE_CHUNK_VECTOR_INDEX,
                "vector",
            )
            .await
            .context("failed to inspect chunk vector index dimensions")?;
        let entity_dimensions = client
            .vector_index_dimensions(
                KNOWLEDGE_ENTITY_VECTOR_COLLECTION,
                KNOWLEDGE_ENTITY_VECTOR_INDEX,
                "vector",
            )
            .await
            .context("failed to inspect entity vector index dimensions")?;
        match (chunk_dimensions, entity_dimensions) {
            (Some(chunk), Some(entity)) if chunk == entity => {}
            (None, None) => {
                client
                    .ensure_vector_index(
                        KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
                        KNOWLEDGE_CHUNK_VECTOR_INDEX,
                        "vector",
                        options.vector_dimensions,
                        options.vector_index_n_lists,
                        options.vector_index_default_n_probe,
                        options.vector_index_training_iterations,
                    )
                    .await
                    .context("failed to ensure chunk vector index")?;
                client
                    .ensure_vector_index(
                        KNOWLEDGE_ENTITY_VECTOR_COLLECTION,
                        KNOWLEDGE_ENTITY_VECTOR_INDEX,
                        "vector",
                        options.vector_dimensions,
                        options.vector_index_n_lists,
                        options.vector_index_default_n_probe,
                        options.vector_index_training_iterations,
                    )
                    .await
                    .context("failed to ensure entity vector index")?;
            }
            (Some(chunk), Some(entity)) => {
                anyhow::bail!(
                    "Arango vector indexes have different dimensions: chunk index has {chunk}, entity index has {entity}"
                );
            }
            (Some(chunk), None) => {
                anyhow::bail!(
                    "Arango vector indexes are incomplete: chunk index has {chunk} dimensions, entity index is missing"
                );
            }
            (None, Some(entity)) => {
                anyhow::bail!(
                    "Arango vector indexes are incomplete: chunk index is missing, entity index has {entity} dimensions"
                );
            }
        }

        // Ensure vector indexes on every per-dim shard that already exists in
        // the database.  The legacy single-dim collections above are handled by
        // the match block; these shards are the post-migration per-dim layout.
        // `ensure_vector_index` is idempotent: if the index is already present
        // with an adequate nLists it returns immediately.
        let chunk_shards = client
            .list_per_dim_chunk_vector_collections()
            .await
            .context("failed to list per-dim chunk vector collections")?;
        for shard in &chunk_shards {
            // `list_per_dim_chunk_vector_collections` matches on the
            // `knowledge_chunk_vector_d` prefix, which also covers the
            // per-(library, dim) shards (`..._d{dim}_l{lib}`). Those are
            // ensured by the per-library loop below; skip them here so
            // `parse_per_dim_suffix` only sees true per-dim names.
            if parse_library_vector_shard(shard).is_some() {
                continue;
            }
            let dim = parse_per_dim_suffix(shard).with_context(|| {
                format!("unexpected per-dim chunk vector collection name: {shard}")
            })?;
            let index_name = chunk_vector_index_for_dim(dim);
            tracing::info!(collection = shard, dim, "ensuring per-dim chunk vector index");
            client
                .ensure_vector_index(
                    shard,
                    &index_name,
                    "vector",
                    dim,
                    options.vector_index_n_lists,
                    options.vector_index_default_n_probe,
                    options.vector_index_training_iterations,
                )
                .await
                .with_context(|| {
                    format!("failed to ensure chunk vector index on per-dim shard {shard}")
                })?;
        }

        // Ensure IVF indexes on every per-(library, dim) chunk shard. Each
        // shard holds one library's chunk vectors, so nLists is sized from
        // that shard's own (small) row count — IVF training fails if nLists
        // exceeds the available sample points. `ensure_vector_index` further
        // clamps to the live row count and seeds synthetic training rows for
        // an empty shard, so a brand-new shard never aborts bootstrap.
        let per_library_chunk_shards = client
            .list_per_library_chunk_vector_collections()
            .await
            .context("failed to list per-library chunk vector collections")?;
        for shard in &per_library_chunk_shards {
            let parsed = parse_library_vector_shard(shard).with_context(|| {
                format!("unexpected per-library chunk vector collection name: {shard}")
            })?;
            anyhow::ensure!(
                parsed.kind == VectorShardKind::Chunk,
                "per-library chunk shard listing returned a non-chunk shard: {shard}",
            );
            let row_count = client
                .count_chunk_vector_rows(shard)
                .await
                .with_context(|| format!("failed to count rows in per-library shard {shard}"))?;
            let n_lists = per_library_shard_index_n_lists(options.vector_index_n_lists, row_count);
            let index_name = chunk_vector_index_for_library(parsed.dim, parsed.library_id);
            tracing::info!(
                collection = shard,
                dim = parsed.dim,
                row_count,
                n_lists,
                "ensuring per-library chunk vector index"
            );
            client
                .ensure_vector_index(
                    shard,
                    &index_name,
                    "vector",
                    parsed.dim,
                    n_lists,
                    options.vector_index_default_n_probe,
                    options.vector_index_training_iterations,
                )
                .await
                .with_context(|| {
                    format!("failed to ensure chunk vector index on per-library shard {shard}")
                })?;
        }

        let entity_shards = client
            .list_per_dim_entity_vector_collections()
            .await
            .context("failed to list per-dim entity vector collections")?;
        for shard in &entity_shards {
            let dim = parse_per_dim_suffix(shard).with_context(|| {
                format!("unexpected per-dim entity vector collection name: {shard}")
            })?;
            let index_name = entity_vector_index_for_dim(dim);
            tracing::info!(collection = shard, dim, "ensuring per-dim entity vector index");
            client
                .ensure_vector_index(
                    shard,
                    &index_name,
                    "vector",
                    dim,
                    options.vector_index_n_lists,
                    options.vector_index_default_n_probe,
                    options.vector_index_training_iterations,
                )
                .await
                .with_context(|| {
                    format!("failed to ensure entity vector index on per-dim shard {shard}")
                })?;
        }
    }

    Ok(())
}

/// Extract the numeric dimension suffix from a per-dim shard collection name.
///
/// Examples:
/// - `"knowledge_chunk_vector_d3072"` → `Some(3072)`
/// - `"knowledge_entity_vector_d1536"` → `Some(1536)`
/// - `"knowledge_chunk_vector"` (legacy) → `None`
/// - `"something_else_d42"` → `None`
fn parse_per_dim_suffix(name: &str) -> Option<u64> {
    // Both per-dim prefixes end with `_d` before the decimal digits.
    let after_d = name
        .strip_prefix("knowledge_chunk_vector_d")
        .or_else(|| name.strip_prefix("knowledge_entity_vector_d"))?;
    // Must be a non-empty all-digit suffix.
    if after_d.is_empty() || !after_d.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    after_d.parse().ok()
}

/// Rows-per-IVF-list target for a per-library chunk shard's index, mirroring
/// the write-path sizing in `search_store`. A per-library shard is small, so
/// nLists is `min(configured, rows / 40)` floored at 1: IVF training needs at
/// least as many sample points as lists.
const PER_LIBRARY_CHUNK_SHARD_ROWS_PER_LIST: u64 = 40;
fn per_library_shard_index_n_lists(configured: u64, row_count: u64) -> u64 {
    let by_rows = row_count / PER_LIBRARY_CHUNK_SHARD_ROWS_PER_LIST;
    configured.min(by_rows).max(1)
}

fn knowledge_text_analyzers() -> serde_json::Value {
    serde_json::json!(["text_en", "text_ru"])
}

/// Analyzers applied to document title / file_name — the same text_en /
/// text_ru pair as chunk content (for exact stemmed hits) plus a
/// trigram analyzer so the title subquery also fires on close spelling
/// variants of a term.
fn knowledge_title_analyzers() -> serde_json::Value {
    serde_json::json!(["text_en", "text_ru", KNOWLEDGE_NGRAM_ANALYZER])
}

fn knowledge_search_view_links() -> serde_json::Value {
    let text_analyzers = knowledge_text_analyzers();
    let title_analyzers = knowledge_title_analyzers();
    serde_json::json!({
        KNOWLEDGE_DOCUMENT_COLLECTION: {
            "includeAllFields": false,
            "fields": {
                "external_key": { "analyzers": ["identity"] },
                "library_id": { "analyzers": ["identity"] },
                "workspace_id": { "analyzers": ["identity"] },
                "document_state": { "analyzers": ["identity"] },
                "title": { "analyzers": title_analyzers.clone() },
                "file_name": { "analyzers": title_analyzers.clone() }
            }
        },
        KNOWLEDGE_CHUNK_COLLECTION: {
            "includeAllFields": true,
            "fields": {
                "content_text": { "analyzers": text_analyzers.clone() },
                "normalized_text": { "analyzers": text_analyzers.clone() },
                "section_path[*]": { "analyzers": text_analyzers.clone() },
                "heading_trail[*]": { "analyzers": text_analyzers.clone() }
            }
        },
        KNOWLEDGE_STRUCTURED_BLOCK_COLLECTION: {
            "includeAllFields": true,
            "fields": {
                "text": { "analyzers": text_analyzers.clone() },
                "normalized_text": { "analyzers": text_analyzers.clone() },
                "heading_trail[*]": { "analyzers": text_analyzers.clone() },
                "section_path[*]": { "analyzers": text_analyzers.clone() },
                "block_kind": { "analyzers": ["identity"] }
            }
        },
        KNOWLEDGE_TECHNICAL_FACT_COLLECTION: {
            "includeAllFields": true,
            "fields": {
                "canonical_value_text": { "analyzers": text_analyzers.clone() },
                "canonical_value_exact": { "analyzers": ["identity"] },
                "display_value": { "analyzers": text_analyzers.clone() },
                "fact_kind": { "analyzers": ["identity"] }
            }
        },
        KNOWLEDGE_ENTITY_COLLECTION: {
            "includeAllFields": true,
            "fields": {
                "canonical_label": { "analyzers": text_analyzers.clone() },
                "summary": { "analyzers": text_analyzers.clone() }
            }
        },
        KNOWLEDGE_RELATION_COLLECTION: {
            "includeAllFields": true,
            "fields": {
                "predicate": { "analyzers": text_analyzers.clone() },
                "normalized_assertion": { "analyzers": text_analyzers.clone() },
                "summary": { "analyzers": text_analyzers.clone() }
            }
        },
        KNOWLEDGE_EVIDENCE_COLLECTION: {
            "includeAllFields": true,
            "fields": {
                "quote_text": { "analyzers": text_analyzers.clone() },
                "summary": { "analyzers": text_analyzers.clone() }
            }
        },
        KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION: {
            "includeAllFields": true
        }
    })
}

#[cfg(test)]
mod tests {
    use super::parse_per_dim_suffix;

    #[test]
    fn parse_per_dim_suffix_chunk() {
        assert_eq!(parse_per_dim_suffix("knowledge_chunk_vector_d3072"), Some(3072));
        assert_eq!(parse_per_dim_suffix("knowledge_chunk_vector_d1536"), Some(1536));
        assert_eq!(parse_per_dim_suffix("knowledge_chunk_vector_d768"), Some(768));
    }

    #[test]
    fn parse_per_dim_suffix_entity() {
        assert_eq!(parse_per_dim_suffix("knowledge_entity_vector_d3072"), Some(3072));
        assert_eq!(parse_per_dim_suffix("knowledge_entity_vector_d1536"), Some(1536));
    }

    #[test]
    fn parse_per_dim_suffix_legacy_returns_none() {
        // Legacy single-dim collections have no _d<N> suffix.
        assert_eq!(parse_per_dim_suffix("knowledge_chunk_vector"), None);
        assert_eq!(parse_per_dim_suffix("knowledge_entity_vector"), None);
    }

    #[test]
    fn parse_per_dim_suffix_unrelated_returns_none() {
        assert_eq!(parse_per_dim_suffix("something_else_d42"), None);
        assert_eq!(parse_per_dim_suffix("knowledge_chunk_vector_d"), None);
        assert_eq!(parse_per_dim_suffix("knowledge_chunk_vector_dabc"), None);
    }
}
