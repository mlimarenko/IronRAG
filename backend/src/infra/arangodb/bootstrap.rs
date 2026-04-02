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
        KNOWLEDGE_GRAPH_NAME, KNOWLEDGE_PERSISTENT_INDEXES, KNOWLEDGE_RELATION_COLLECTION,
        KNOWLEDGE_RELATION_OBJECT_EDGE, KNOWLEDGE_RELATION_SUBJECT_EDGE,
        KNOWLEDGE_REVISION_BLOCK_EDGE, KNOWLEDGE_REVISION_CHUNK_EDGE,
        KNOWLEDGE_REVISION_COLLECTION, KNOWLEDGE_SEARCH_VIEW,
        KNOWLEDGE_STRUCTURED_BLOCK_COLLECTION, KNOWLEDGE_TECHNICAL_FACT_COLLECTION,
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

    Ok(())
}

fn knowledge_text_analyzers() -> serde_json::Value {
    serde_json::json!(["text_en", "text_ru"])
}

fn knowledge_search_view_links() -> serde_json::Value {
    let text_analyzers = knowledge_text_analyzers();
    serde_json::json!({
        KNOWLEDGE_DOCUMENT_COLLECTION: {
            "includeAllFields": false,
            "fields": {
                "external_key": { "analyzers": ["identity"] }
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
