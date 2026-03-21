use anyhow::Context;

use crate::infra::arangodb::{
    client::ArangoClient,
    collections::{
        DOCUMENT_COLLECTIONS, EDGE_COLLECTIONS, KNOWLEDGE_BUNDLE_CHUNK_EDGE,
        KNOWLEDGE_BUNDLE_ENTITY_EDGE, KNOWLEDGE_BUNDLE_EVIDENCE_EDGE,
        KNOWLEDGE_BUNDLE_RELATION_EDGE, KNOWLEDGE_CHUNK_COLLECTION,
        KNOWLEDGE_CHUNK_VECTOR_COLLECTION, KNOWLEDGE_CHUNK_VECTOR_INDEX,
        KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION, KNOWLEDGE_DOCUMENT_COLLECTION,
        KNOWLEDGE_ENTITY_COLLECTION, KNOWLEDGE_ENTITY_VECTOR_COLLECTION,
        KNOWLEDGE_ENTITY_VECTOR_INDEX, KNOWLEDGE_EVIDENCE_COLLECTION, KNOWLEDGE_GRAPH_NAME,
        KNOWLEDGE_RELATION_COLLECTION, KNOWLEDGE_SEARCH_VIEW,
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
                "collection": KNOWLEDGE_BUNDLE_CHUNK_EDGE,
                "from": [KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION],
                "to": [KNOWLEDGE_CHUNK_COLLECTION]
            },
            {
                "collection": KNOWLEDGE_BUNDLE_ENTITY_EDGE,
                "from": [KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION],
                "to": ["knowledge_entity"]
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

fn knowledge_search_view_links() -> serde_json::Value {
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
                "content_text": { "analyzers": ["text_en"] },
                "normalized_text": { "analyzers": ["text_en"] }
            }
        },
        KNOWLEDGE_ENTITY_COLLECTION: {
            "includeAllFields": true,
            "fields": {
                "canonical_name": { "analyzers": ["text_en"] },
                "summary": { "analyzers": ["text_en"] }
            }
        },
        KNOWLEDGE_RELATION_COLLECTION: {
            "includeAllFields": true,
            "fields": {
                "predicate": { "analyzers": ["text_en"] },
                "canonical_label": { "analyzers": ["text_en"] },
                "summary": { "analyzers": ["text_en"] }
            }
        },
        KNOWLEDGE_EVIDENCE_COLLECTION: {
            "includeAllFields": true,
            "fields": {
                "quote_text": { "analyzers": ["text_en"] },
                "summary": { "analyzers": ["text_en"] }
            }
        },
        KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION: {
            "includeAllFields": true
        }
    })
}
