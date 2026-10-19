use ironrag_contracts::documents::DocumentReadiness;
use serde_json::json;
use tracing::warn;
use uuid::Uuid;

use std::collections::HashMap;

use crate::{
    app::state::AppState,
    domains::{ai::AiBindingPurpose, content::revision_text_state_is_readable},
    infra::{
        knowledge_rows::{
            KnowledgeChunkRow, KnowledgeChunkSearchRow, KnowledgeChunkVectorSearchRow,
            KnowledgeDocumentRow, KnowledgeRevisionRow,
        },
        repositories::{
            catalog_repository, content_document_repository, content_metadata_search_repository,
            content_structured_block_search_repository,
        },
    },
    integrations::llm::EmbeddingRequest,
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_DOCUMENTS_WRITE, POLICY_MCP_MEMORY_READ, authorize_document_permission,
            load_library_and_authorize,
        },
        router_support::{ApiError, map_runtime_lifecycle_error},
    },
    mcp_types::{
        McpChunkReference, McpContentSourceAccess, McpDocumentHit, McpEntityReference,
        McpEvidenceReference, McpReadDocumentRequest, McpReadDocumentResponse, McpReadMode,
        McpReadabilityState, McpRelationReference, McpSearchDocumentsRequest,
        McpSearchDocumentsResponse, McpTechnicalFactReference,
    },
    services::{
        mcp::tokens::{
            McpReadCursor, ReadWindowBounds, char_slice, encode_continuation_token,
            normalize_read_request,
        },
        query::{
            error::QueryServiceError,
            vector_dimensions::{
                EmbeddingProfileIndexState, ensure_active_embedding_profile_key,
                ensure_embedding_profile_inventory_version_current,
                ensure_library_embedding_profile_indexed, load_embedding_profile_inventory_version,
                validate_embedding_vector_dimensions,
            },
        },
    },
    shared::versioning::dotted_version_key,
};

use super::{
    catalog::{describe_libraries, load_library_by_catalog_ref, load_visible_library_contexts},
    fusion::SearchLane,
    types::{
        McpDocumentAccumulator, McpRevisionGroundingReferences, McpSearchEmbeddingContext,
        ResolvedDocumentState, VisibleLibraryContext,
    },
};

mod grounding;
mod management;
mod read;
mod resolution;
mod search;

pub(crate) use grounding::{
    collect_revision_grounding_references, readability_state_from_kind, readable_status_reason,
};
pub(crate) use management::{authorize_library_for_mcp, delete_document, list_documents};
pub(crate) use read::read_document;
pub(crate) use resolution::{
    load_knowledge_chunks_by_ids, resolve_search_embedding_context, resolve_search_libraries,
};
pub(crate) use search::search_documents;

#[cfg(test)]
mod tests;
