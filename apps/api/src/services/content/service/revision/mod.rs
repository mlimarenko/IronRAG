use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use futures::{StreamExt, TryStreamExt, stream};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    agent_runtime::{task::RuntimeTask, tasks::graph_extract::GraphExtractTask},
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    domains::catalog::ChunkingTemplate,
    domains::content::{ContentDocument, ContentDocumentHead, ContentRevision},
    domains::knowledge::TypedTechnicalFact,
    domains::ops::ASYNC_OP_STATUS_READY,
    domains::provider_profiles::ProviderModelSelection,
    domains::recognition::LibraryRecognitionPolicy,
    infra::knowledge_plane::{IngestAttemptMutationOutcome, IngestRevisionWriteAuthority},
    infra::knowledge_rows::{
        KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeRevisionRow, KnowledgeStructuredBlockRow,
        KnowledgeStructuredRevisionRow, KnowledgeTechnicalFactRow,
    },
    infra::repositories::{
        self as repositories, catalog_repository, content_chunk_repository,
        content_document_head_repository::NewContentDocumentHead,
        content_document_repository::{self, NewContentDocument},
        content_knowledge_projection_repository::{self, PromoteDocumentHeadOutcome},
        content_mutation_repository,
        content_revision_repository::{
            self, CreateContentRevisionOutcome, NewContentRevisionProjection,
        },
    },
    interfaces::http::router_support::ApiError,
    services::{
        ai_catalog_service::ResolvedRuntimeBinding,
        content::error::ContentServiceError,
        content::source_access::{
            derive_content_source_file_name, derive_storage_backed_content_file_name,
        },
        content::storage::ContentStorageService,
        graph::extract::{
            GraphEntityTypeClassifierIdentity, GraphExtractionExecutionError,
            GraphExtractionOutcome, GraphExtractionSubTypeHintEntry,
            GraphExtractionSubTypeHintGroup, GraphExtractionSubTypeHints,
            build_graph_extraction_cache_fingerprint, extract_chunk_graph_candidates,
            extraction_lifecycle_from_record, normalized_graph_extraction_json,
            repair_graph_extraction_candidate_set,
        },
        graph::projection::resolve_projection_scope,
        ingest::cancellation::ensure_not_cancelled,
        ingest::runtime::resolve_runtime_task_execution_context,
        ingest::service::{
            INGEST_STAGE_EXTRACT_CONTENT, INGEST_STAGE_EXTRACT_GRAPH, LeaseAttemptCommand,
            RecordStageUnitProgressCommand,
        },
        ingest::structured_preparation::{
            PrepareStructuredRevisionCommand, StructuredPreparationService,
        },
        ingest::technical_facts::ExtractTechnicalFactsCommand,
        ops::billing::CaptureGraphExtractionBillingCommand,
        query::vector_dimensions::invalidate_library_embedding_profile_inventory,
    },
    shared::extraction::file_extract::{
        ExtractionFailureClass, FileExtractError, FileExtractionPlan, FileExtractionRequest,
        UploadAdmissionError, UploadFileKind, VisionOcrInput, append_docling_picture_ocr_fallback,
        augment_docling_output_with_vision_picture_ocr, build_docling_pdf_extraction_plan,
        build_runtime_file_extraction_plan, docling_embedded_picture_vision_enabled,
        validate_upload_file_admission,
    },
    shared::extraction::{
        ExtractionOutput,
        record_jsonl::RECORD_JSONL_SOURCE_FORMAT,
        structured_document::StructuredBlockKind,
        table_graph::{TableGraphProfile, build_table_graph_profile},
        table_summary::{is_table_summary_text, parse_table_column_summary},
    },
};

use super::pipeline::{
    GraphExtractionChunkPolicy, GraphExtractionRequestInput, build_graph_chunk_content,
    build_graph_extraction_request, typed_fact_supports_chunk,
};
use super::{
    ContentMutationAdmission, ContentService, CreateDocumentCommand, CreateRevisionCommand,
    InlineMutationContext, MaterializeRevisionGraphCandidatesCommand, PendingChunkInsert,
    PreparedRevisionPersistenceSummary, PromoteHeadCommand, ReprocessRevisionSource,
    RevisionGraphCandidateMaterialization, locate_chunk_offsets, map_revision_row,
    map_structured_revision_data, sha256_hex_text,
};

mod extraction;
mod graph_cache;
mod graph_materialization;
mod graph_policy;
mod lifecycle;
mod preparation;
mod source;
mod source_guard;

pub(crate) use extraction::DoclingPdfExtractionPlanInput;
pub(crate) use preparation::preparation_fence_text;
pub(crate) use source::InlineFileSourceInput;
pub(crate) use source_guard::PersistedRevisionSourceGuard;

use graph_cache::{
    GraphExtractionCachePlan, GraphMaterializationCacheInput, log_graph_materialization_cache,
};
use graph_materialization::{GraphMaterializationBinding, GraphMaterializationSources};
use graph_policy::{
    GraphMaterializationSelection, RevisionTableGraphContext, build_revision_table_graph_context,
    graph_materialization_selection, load_sub_type_hints_for_extraction,
    log_graph_materialization_selection,
};
