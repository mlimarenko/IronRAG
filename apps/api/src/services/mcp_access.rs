use serde::Deserialize;
use tracing::warn;
use uuid::Uuid;

use crate::{
    agent_runtime::trace::{RuntimeExecutionTraceView, build_policy_summary, policy_summary},
    app::state::AppState,
    domains::{
        agent_runtime::{RuntimePolicyDecision, RuntimePolicySummary},
        ai::AiBindingPurpose,
        catalog::CatalogLibraryIngestionReadiness,
        content::revision_text_state_is_readable,
    },
    infra::repositories::{
        catalog_repository::{self, CatalogLibraryRow, CatalogWorkspaceRow},
        runtime_repository,
    },
    integrations::llm::EmbeddingRequest,
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_LIBRARY_WRITE, POLICY_MCP_MEMORY_READ, POLICY_RUNTIME_READ,
            POLICY_WORKSPACE_ADMIN, authorize_library_discovery, authorize_workspace_discovery,
            authorize_workspace_permission, load_runtime_execution_and_authorize,
        },
        router_support::{
            ApiError, map_library_create_error, map_runtime_execution_row, map_runtime_trace_view,
            map_workspace_create_error,
        },
    },
    mcp_types::{
        McpChunkReference, McpCreateLibraryRequest, McpCreateWorkspaceRequest, McpDocumentHit,
        McpEntityReference, McpEvidenceReference, McpLibraryDescriptor,
        McpLibraryIngestionReadiness, McpReadDocumentRequest, McpReadDocumentResponse,
        McpReadabilityState, McpRelationReference, McpRuntimeActionSummary,
        McpRuntimeExecutionSummary, McpRuntimeExecutionTrace, McpRuntimePolicySummary,
        McpRuntimeStageSummary, McpSearchDocumentsRequest, McpSearchDocumentsResponse,
        McpTechnicalFactReference, McpWorkspaceDescriptor,
    },
    services::mcp_support::{
        char_slice, encode_continuation_token, normalize_read_request, preview_hit, saturating_rank,
    },
    shared::slugs::slugify,
};

#[derive(Debug, Clone)]
pub(crate) struct VisibleLibraryContext {
    pub(crate) library: CatalogLibraryRow,
    pub(crate) descriptor: McpLibraryDescriptor,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedDocumentState {
    pub(crate) document_id: Uuid,
    pub(crate) document_title: String,
    pub(crate) library: CatalogLibraryRow,
    pub(crate) latest_revision_id: Option<Uuid>,
    pub(crate) readability_state: McpReadabilityState,
    pub(crate) readiness_kind: String,
    pub(crate) graph_coverage_kind: String,
    pub(crate) status_reason: Option<String>,
    pub(crate) content: Option<String>,
    pub(crate) chunk_references: Vec<McpChunkReference>,
    pub(crate) technical_fact_references: Vec<McpTechnicalFactReference>,
    pub(crate) entity_references: Vec<McpEntityReference>,
    pub(crate) relation_references: Vec<McpRelationReference>,
    pub(crate) evidence_references: Vec<McpEvidenceReference>,
}

#[derive(Debug, Clone)]
pub(crate) struct McpSearchEmbeddingContext {
    pub(crate) model_catalog_id: Uuid,
    pub(crate) freshness_generation: i64,
    pub(crate) query_vector: Vec<f32>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct McpRevisionGroundingReferences {
    pub(crate) technical_fact_references: Vec<McpTechnicalFactReference>,
    pub(crate) entity_references: Vec<McpEntityReference>,
    pub(crate) relation_references: Vec<McpRelationReference>,
    pub(crate) evidence_references: Vec<McpEvidenceReference>,
}

#[derive(Debug, Clone, Deserialize)]
struct ArangoChunkMentionReferenceRow {
    entity_id: Uuid,
    rank: i32,
    score: f64,
    inclusion_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ArangoRelationSupportReferenceRow {
    relation_id: Uuid,
    rank: i32,
    score: f64,
    inclusion_reason: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct RankedSearchReference {
    rank: i32,
    score: f64,
    inclusion_reason: Option<String>,
}

#[derive(Debug, Clone)]
struct McpDocumentAccumulator {
    document_id: Uuid,
    library_id: Uuid,
    workspace_id: Uuid,
    readable_revision_id: Uuid,
    document_title: String,
    score: f64,
    excerpt: Option<String>,
    excerpt_start_offset: Option<usize>,
    excerpt_end_offset: Option<usize>,
    chunk_references: std::collections::HashMap<Uuid, RankedSearchReference>,
}

impl McpDocumentAccumulator {
    fn from_knowledge(
        document: &crate::infra::arangodb::document_store::KnowledgeDocumentRow,
        revision: &crate::infra::arangodb::document_store::KnowledgeRevisionRow,
        hit: &crate::infra::arangodb::search_store::KnowledgeChunkSearchRow,
    ) -> Self {
        let document_title = revision
            .title
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| document.external_key.clone());
        Self {
            document_id: document.document_id,
            library_id: document.library_id,
            workspace_id: document.workspace_id,
            readable_revision_id: revision.revision_id,
            document_title,
            score: hit.score,
            excerpt: None,
            excerpt_start_offset: None,
            excerpt_end_offset: None,
            chunk_references: std::collections::HashMap::new(),
        }
    }

    fn bump_score(&mut self, score: f64) {
        self.score = self.score.max(score);
    }

    fn merge_chunk_reference(
        &mut self,
        chunk_id: Uuid,
        rank: i32,
        score: f64,
        inclusion_reason: Option<String>,
    ) {
        let entry = self.chunk_references.entry(chunk_id).or_insert_with(|| {
            RankedSearchReference { rank, score, inclusion_reason: inclusion_reason.clone() }
        });
        entry.rank = entry.rank.min(rank);
        if score > entry.score {
            entry.score = score;
        }
        if entry.inclusion_reason.is_none() {
            entry.inclusion_reason = inclusion_reason;
        }
    }

    fn populate_excerpt_from_text(&mut self, text: &str, query: &str) {
        if self.excerpt.is_some() {
            return;
        }
        let query_lower = query.to_ascii_lowercase();
        if let Some((excerpt, start, end, _)) = preview_hit(text, &query_lower) {
            self.excerpt = Some(excerpt);
            self.excerpt_start_offset = Some(start);
            self.excerpt_end_offset = Some(end);
        }
    }

    fn chunk_reference_ids(&self) -> Vec<Uuid> {
        self.chunk_references.keys().copied().collect()
    }

    fn into_chunk_references(self) -> Vec<McpChunkReference> {
        let mut rows = self.chunk_references.into_iter().collect::<Vec<_>>();
        rows.sort_by(|(left_id, left), (right_id, right)| {
            left.rank
                .cmp(&right.rank)
                .then_with(|| right.score.total_cmp(&left.score))
                .then_with(|| left_id.cmp(right_id))
        });
        rows.into_iter()
            .map(|(chunk_id, reference)| McpChunkReference {
                chunk_id,
                rank: reference.rank,
                score: reference.score,
                inclusion_reason: reference.inclusion_reason,
            })
            .collect()
    }
}

fn resolve_mcp_slug(requested_slug: Option<&str>, name: &str) -> String {
    requested_slug
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(slugify)
        .unwrap_or_else(|| slugify(name))
}

pub async fn visible_workspaces(
    auth: &AuthContext,
    state: &AppState,
) -> Result<Vec<McpWorkspaceDescriptor>, ApiError> {
    let rows = load_visible_workspace_rows(auth, state).await?;
    let mut items = Vec::with_capacity(rows.len());
    for workspace in rows {
        let libraries = visible_libraries(auth, state, Some(workspace.id)).await?;
        let can_write_any_library = libraries.iter().any(|item| item.supports_write);
        items.push(McpWorkspaceDescriptor {
            workspace_id: workspace.id,
            slug: workspace.slug,
            name: workspace.display_name,
            status: workspace.lifecycle_state,
            visible_library_count: libraries.len(),
            can_write_any_library,
        });
    }
    Ok(items)
}

pub async fn visible_libraries(
    auth: &AuthContext,
    state: &AppState,
    workspace_filter: Option<Uuid>,
) -> Result<Vec<McpLibraryDescriptor>, ApiError> {
    let libraries = load_visible_library_contexts(auth, state, workspace_filter).await?;
    Ok(libraries.into_iter().map(|item| item.descriptor).collect())
}

pub async fn search_documents(
    auth: &AuthContext,
    state: &AppState,
    request: McpSearchDocumentsRequest,
) -> Result<McpSearchDocumentsResponse, ApiError> {
    auth.require_any_scope(POLICY_MCP_MEMORY_READ)?;
    let settings = &state.mcp_memory;
    let query = request.query.trim();
    if query.is_empty() {
        return Err(ApiError::BadRequest("query must not be empty".into()));
    }

    let limit =
        request.limit.unwrap_or(settings.default_search_limit).clamp(1, settings.max_search_limit);
    let requested_library_ids = request.requested_library_ids();
    let libraries = resolve_search_libraries(auth, state, requested_library_ids.as_deref()).await?;
    let library_ids = libraries.iter().map(|item| item.library.id).collect::<Vec<_>>();
    let mut hits = Vec::new();
    for library in libraries {
        let lexical_limit = limit.saturating_mul(3).max(6);
        let lexical_chunk_hits = state
            .arango_search_store
            .search_chunks(library.library.id, query, lexical_limit)
            .await
            .map_err(|_| ApiError::Internal)?;
        let embedding_context: Option<McpSearchEmbeddingContext> =
            resolve_search_embedding_context(state, library.library.id, query).await?;
        let vector_chunk_hits = if let Some(context) = embedding_context.as_ref() {
            match state
                .arango_search_store
                .search_chunk_vectors_by_similarity(
                    library.library.id,
                    &context.model_catalog_id.to_string(),
                    context.freshness_generation,
                    &context.query_vector,
                    lexical_limit.saturating_mul(2),
                    Some(16),
                )
                .await
            {
                Ok(rows) => rows,
                Err(error) => {
                    warn!(
                        library_id = %library.library.id,
                        model_catalog_id = %context.model_catalog_id,
                        freshness_generation = context.freshness_generation,
                        error = ?error,
                        "mcp search vector lookup failed; degrading to lexical-only MCP search",
                    );
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        let all_chunk_ids = lexical_chunk_hits
            .iter()
            .map(|hit| hit.chunk_id)
            .chain(vector_chunk_hits.iter().map(|hit| hit.chunk_id))
            .collect::<Vec<_>>();
        let chunk_rows = load_knowledge_chunks_by_ids(state, &all_chunk_ids).await?;
        let chunk_map = chunk_rows
            .into_iter()
            .map(|row| (row.chunk_id, row))
            .collect::<std::collections::HashMap<_, _>>();
        let mut document_accumulators =
            std::collections::HashMap::<Uuid, McpDocumentAccumulator>::new();

        for (index, hit) in lexical_chunk_hits.iter().enumerate() {
            let Some(chunk) = chunk_map.get(&hit.chunk_id) else {
                continue;
            };
            let document = state
                .arango_document_store
                .get_document(chunk.document_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("document", chunk.document_id))?;
            let revision = state
                .arango_document_store
                .get_revision(chunk.revision_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("revision", chunk.revision_id))?;
            let accumulator =
                document_accumulators.entry(document.document_id).or_insert_with(|| {
                    McpDocumentAccumulator::from_knowledge(&document, &revision, hit)
                });
            accumulator.bump_score(hit.score);
            accumulator.merge_chunk_reference(
                chunk.chunk_id,
                saturating_rank(index),
                hit.score,
                Some("lexical_chunk".to_string()),
            );
            accumulator.populate_excerpt_from_text(&hit.normalized_text, query);
        }

        for (index, hit) in vector_chunk_hits.iter().enumerate() {
            let Some(chunk) = chunk_map.get(&hit.chunk_id) else {
                continue;
            };
            let document = state
                .arango_document_store
                .get_document(chunk.document_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("document", chunk.document_id))?;
            let revision = state
                .arango_document_store
                .get_revision(chunk.revision_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("revision", chunk.revision_id))?;
            let accumulator =
                document_accumulators.entry(document.document_id).or_insert_with(|| {
                    McpDocumentAccumulator::from_knowledge(
                        &document,
                        &revision,
                        &crate::infra::arangodb::search_store::KnowledgeChunkSearchRow {
                            chunk_id: chunk.chunk_id,
                            workspace_id: chunk.workspace_id,
                            library_id: chunk.library_id,
                            revision_id: chunk.revision_id,
                            content_text: chunk.content_text.clone(),
                            normalized_text: chunk.normalized_text.clone(),
                            section_path: chunk.section_path.clone(),
                            heading_trail: chunk.heading_trail.clone(),
                            score: hit.score,
                        },
                    )
                });
            accumulator.bump_score(hit.score);
            accumulator.merge_chunk_reference(
                chunk.chunk_id,
                saturating_rank(index),
                hit.score,
                Some("vector_chunk".to_string()),
            );
            accumulator.populate_excerpt_from_text(&chunk.normalized_text, query);
        }

        let mut library_hits = document_accumulators.into_values().collect::<Vec<_>>();
        library_hits.sort_by(|left, right| right.score.total_cmp(&left.score));
        library_hits.truncate(limit);
        for accumulator in library_hits {
            let chunk_references = accumulator.clone().into_chunk_references();
            let content_summary = state
                .canonical_services
                .content
                .get_document(state, accumulator.document_id)
                .await?;
            let readiness_summary = content_summary.readiness_summary.ok_or(ApiError::Internal)?;
            let grounding = collect_revision_grounding_references(
                state,
                accumulator.readable_revision_id,
                &accumulator.chunk_reference_ids(),
                8,
            )
            .await?;
            let status_reason = readable_status_reason(&readiness_summary, &grounding);
            hits.push(McpDocumentHit {
                document_id: accumulator.document_id,
                logical_document_id: accumulator.document_id,
                library_id: accumulator.library_id,
                workspace_id: accumulator.workspace_id,
                document_title: accumulator.document_title,
                latest_revision_id: Some(accumulator.readable_revision_id),
                score: accumulator.score,
                excerpt: accumulator.excerpt,
                excerpt_start_offset: accumulator.excerpt_start_offset,
                excerpt_end_offset: accumulator.excerpt_end_offset,
                readability_state: readability_state_from_kind(&readiness_summary.readiness_kind),
                readiness_kind: readiness_summary.readiness_kind.clone(),
                graph_coverage_kind: readiness_summary.graph_coverage_kind.clone(),
                status_reason,
                chunk_references,
                technical_fact_references: grounding.technical_fact_references,
                entity_references: grounding.entity_references,
                relation_references: grounding.relation_references,
                evidence_references: grounding.evidence_references,
            });
        }
    }
    hits.sort_by(|left, right| right.score.total_cmp(&left.score));
    hits.truncate(limit);

    Ok(McpSearchDocumentsResponse { query: query.to_string(), limit, library_ids, hits })
}

pub async fn create_workspace(
    auth: &AuthContext,
    state: &AppState,
    request: McpCreateWorkspaceRequest,
) -> Result<McpWorkspaceDescriptor, ApiError> {
    if !auth.is_system_admin {
        return Err(ApiError::Unauthorized);
    }
    let name = request.name.trim();
    if name.is_empty() {
        return Err(ApiError::BadRequest("workspace name must not be empty".into()));
    }
    let slug = resolve_mcp_slug(request.slug.as_deref(), name);

    let workspace = state
        .canonical_services
        .catalog
        .create_workspace(
            state,
            crate::services::catalog_service::CreateWorkspaceCommand {
                slug: Some(slug.clone()),
                display_name: name.to_string(),
                created_by_principal_id: Some(auth.principal_id),
            },
        )
        .await
        .map_err(|error| match error {
            ApiError::Conflict(_) => error,
            _ => map_workspace_create_error(sqlx::Error::Protocol(error.to_string()), &slug),
        })?;

    Ok(McpWorkspaceDescriptor {
        workspace_id: workspace.id,
        slug: workspace.slug,
        name: workspace.display_name,
        status: "active".to_string(),
        visible_library_count: 0,
        can_write_any_library: auth.is_system_admin,
    })
}

pub async fn create_library(
    auth: &AuthContext,
    state: &AppState,
    request: McpCreateLibraryRequest,
) -> Result<McpLibraryDescriptor, ApiError> {
    authorize_workspace_permission(auth, request.workspace_id, POLICY_WORKSPACE_ADMIN)?;

    let name = request.name.trim();
    if name.is_empty() {
        return Err(ApiError::BadRequest("library name must not be empty".into()));
    }
    let slug = resolve_mcp_slug(request.slug.as_deref(), name);

    let library = state
        .canonical_services
        .catalog
        .create_library(
            state,
            crate::services::catalog_service::CreateLibraryCommand {
                workspace_id: request.workspace_id,
                slug: Some(slug.clone()),
                display_name: name.to_string(),
                description: request.description,
                created_by_principal_id: Some(auth.principal_id),
            },
        )
        .await
        .map_err(|error| match error {
            ApiError::Conflict(_) => error,
            _ => map_library_create_error(
                sqlx::Error::Protocol(error.to_string()),
                request.workspace_id,
                &slug,
            ),
        })?;

    let row = catalog_repository::get_library_by_id(&state.persistence.postgres, library.id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("library", library.id))?;
    let readiness =
        state.canonical_services.catalog.get_library_ingestion_readiness(state, row.id).await?;
    let context = describe_library(auth, state, row, readiness).await?;
    Ok(context.descriptor)
}

pub async fn read_document(
    auth: &AuthContext,
    state: &AppState,
    request: McpReadDocumentRequest,
) -> Result<McpReadDocumentResponse, ApiError> {
    auth.require_any_scope(POLICY_MCP_MEMORY_READ)?;
    let settings = &state.mcp_memory;
    let normalized = normalize_read_request(
        auth,
        request.document_id,
        request.mode,
        request.start_offset,
        request.length,
        request.continuation_token.as_deref(),
        settings.default_read_window_chars,
        settings.max_read_window_chars,
    )?;
    let state_view = resolve_document_state(auth, state, normalized.document_id).await?;
    let latest_revision_id = state_view.latest_revision_id;

    if state_view.readability_state != McpReadabilityState::Readable {
        return Ok(McpReadDocumentResponse {
            document_id: state_view.document_id,
            document_title: state_view.document_title,
            library_id: state_view.library.id,
            workspace_id: state_view.library.workspace_id,
            latest_revision_id,
            read_mode: normalized.read_mode,
            readability_state: state_view.readability_state,
            readiness_kind: state_view.readiness_kind,
            graph_coverage_kind: state_view.graph_coverage_kind,
            status_reason: state_view.status_reason,
            content: None,
            slice_start_offset: normalized.start_offset,
            slice_end_offset: normalized.start_offset,
            total_content_length: None,
            continuation_token: None,
            has_more: false,
            chunk_references: Vec::new(),
            technical_fact_references: Vec::new(),
            entity_references: Vec::new(),
            relation_references: Vec::new(),
            evidence_references: Vec::new(),
        });
    }

    let content = state_view.content.clone().unwrap_or_default();
    let total_content_length = content.chars().count();
    let slice = char_slice(&content, normalized.start_offset, normalized.window_chars);
    let slice_len = slice.chars().count();
    let slice_end_offset = normalized.start_offset.saturating_add(slice_len);
    let has_more = slice_end_offset < total_content_length;
    let continuation_token = has_more.then(|| {
        encode_continuation_token(
            auth,
            normalized.document_id,
            latest_revision_id.unwrap_or(normalized.document_id),
            latest_revision_id,
            slice_end_offset,
            normalized.window_chars,
            normalized.read_mode.clone(),
        )
    });

    Ok(McpReadDocumentResponse {
        document_id: state_view.document_id,
        document_title: state_view.document_title,
        library_id: state_view.library.id,
        workspace_id: state_view.library.workspace_id,
        latest_revision_id,
        read_mode: normalized.read_mode,
        readability_state: state_view.readability_state,
        readiness_kind: state_view.readiness_kind,
        graph_coverage_kind: state_view.graph_coverage_kind,
        status_reason: state_view.status_reason,
        content: Some(slice),
        slice_start_offset: normalized.start_offset.min(total_content_length),
        slice_end_offset,
        total_content_length: Some(total_content_length),
        continuation_token,
        has_more,
        chunk_references: state_view.chunk_references,
        technical_fact_references: state_view.technical_fact_references,
        entity_references: state_view.entity_references,
        relation_references: state_view.relation_references,
        evidence_references: state_view.evidence_references,
    })
}

pub async fn get_runtime_execution(
    auth: &AuthContext,
    state: &AppState,
    execution_id: Uuid,
) -> Result<McpRuntimeExecutionSummary, ApiError> {
    let row = load_runtime_execution_and_authorize(auth, state, execution_id, POLICY_RUNTIME_READ)
        .await?;
    let policy_rows = runtime_repository::list_runtime_policy_decisions(
        &state.persistence.postgres,
        execution_id,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    Ok(map_mcp_runtime_execution(
        map_runtime_execution_row(row)?,
        map_runtime_policy_summary(&policy_rows),
    ))
}

pub async fn get_runtime_execution_trace(
    auth: &AuthContext,
    state: &AppState,
    execution_id: Uuid,
) -> Result<McpRuntimeExecutionTrace, ApiError> {
    let execution_row =
        load_runtime_execution_and_authorize(auth, state, execution_id, POLICY_RUNTIME_READ)
            .await?;
    let stage_rows =
        runtime_repository::list_runtime_stage_records(&state.persistence.postgres, execution_id)
            .await
            .map_err(|_| ApiError::Internal)?;
    let action_rows =
        runtime_repository::list_runtime_action_records(&state.persistence.postgres, execution_id)
            .await
            .map_err(|_| ApiError::Internal)?;
    let policy_rows = runtime_repository::list_runtime_policy_decisions(
        &state.persistence.postgres,
        execution_id,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    Ok(map_mcp_runtime_trace(map_runtime_trace_view(
        execution_row,
        stage_rows,
        action_rows,
        policy_rows,
    )?))
}

pub(crate) async fn resolve_document_state(
    auth: &AuthContext,
    state: &AppState,
    document_id: Uuid,
) -> Result<ResolvedDocumentState, ApiError> {
    let knowledge_document = state
        .arango_document_store
        .get_document(document_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
    let library = catalog_repository::get_library_by_id(
        &state.persistence.postgres,
        knowledge_document.library_id,
    )
    .await
    .map_err(|_| ApiError::Internal)?
    .ok_or_else(|| ApiError::resource_not_found("library", knowledge_document.library_id))?;
    authorize_library_discovery(auth, library.workspace_id, library.id)?;
    let latest_revision_id = knowledge_document.readable_revision_id;
    let content_summary = state.canonical_services.content.get_document(state, document_id).await?;
    let readiness_summary = content_summary.readiness_summary.ok_or(ApiError::Internal)?;
    let readable_revision = match latest_revision_id {
        Some(revision_id) => state
            .arango_document_store
            .get_revision(revision_id)
            .await
            .map_err(|_| ApiError::Internal)?,
        None => None,
    };
    let document_title = readable_revision
        .as_ref()
        .and_then(|revision| revision.title.clone())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| knowledge_document.external_key.clone());
    let (
        readability_state,
        status_reason,
        content,
        chunk_references,
        technical_fact_references,
        entity_references,
        relation_references,
        evidence_references,
    ) = match readable_revision {
        Some(revision)
            if revision.normalized_text.as_deref().is_some_and(|text| !text.trim().is_empty())
                && revision_text_state_is_readable(&revision.text_state) =>
        {
            let chunks = state
                .arango_document_store
                .list_chunks_by_revision(revision.revision_id)
                .await
                .map_err(|_| ApiError::Internal)?;
            let chunk_references = chunks
                .iter()
                .map(|chunk| McpChunkReference {
                    chunk_id: chunk.chunk_id,
                    rank: chunk.chunk_index.saturating_add(1),
                    score: 1.0,
                    inclusion_reason: Some("revision_chunk".to_string()),
                })
                .collect::<Vec<_>>();
            let grounding = collect_revision_grounding_references(
                state,
                revision.revision_id,
                &chunks.iter().map(|chunk| chunk.chunk_id).collect::<Vec<_>>(),
                16,
            )
            .await?;
            let status_reason = readable_status_reason(&readiness_summary, &grounding);
            (
                readability_state_from_kind(&readiness_summary.readiness_kind),
                status_reason,
                revision.normalized_text,
                chunk_references,
                grounding.technical_fact_references,
                grounding.entity_references,
                grounding.relation_references,
                grounding.evidence_references,
            )
        }
        Some(revision) if revision.text_state == "failed" => (
            readability_state_from_kind(&readiness_summary.readiness_kind),
            Some("latest readable revision extraction failed".to_string()),
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ),
        _ if knowledge_document.active_revision_id.is_some() => (
            readability_state_from_kind(&readiness_summary.readiness_kind),
            Some("latest revision is still being extracted".to_string()),
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ),
        _ => (
            readability_state_from_kind(&readiness_summary.readiness_kind),
            Some("document has no readable revision yet".to_string()),
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ),
    };
    Ok(ResolvedDocumentState {
        document_id,
        document_title,
        library,
        latest_revision_id,
        readability_state,
        readiness_kind: readiness_summary.readiness_kind,
        graph_coverage_kind: readiness_summary.graph_coverage_kind,
        status_reason,
        content,
        chunk_references,
        technical_fact_references,
        entity_references,
        relation_references,
        evidence_references,
    })
}

fn map_mcp_runtime_execution(
    execution: crate::domains::agent_runtime::RuntimeExecution,
    policy_summary: RuntimePolicySummary,
) -> McpRuntimeExecutionSummary {
    McpRuntimeExecutionSummary {
        runtime_execution_id: execution.id,
        owner_kind: execution.owner_kind,
        owner_id: execution.owner_id,
        task_kind: execution.task_kind,
        surface_kind: execution.surface_kind,
        contract_name: execution.contract_name,
        contract_version: execution.contract_version,
        lifecycle_state: execution.lifecycle_state,
        active_stage: execution.active_stage,
        turn_budget: execution.turn_budget,
        turn_count: execution.turn_count,
        parallel_action_limit: execution.parallel_action_limit,
        failure_code: execution.failure_code,
        failure_summary: execution.failure_summary_redacted,
        policy_summary,
        accepted_at: execution.accepted_at,
        completed_at: execution.completed_at,
    }
}

fn map_mcp_runtime_trace(trace: RuntimeExecutionTraceView) -> McpRuntimeExecutionTrace {
    let execution_policy_summary = policy_summary(&trace);
    McpRuntimeExecutionTrace {
        execution: map_mcp_runtime_execution(trace.execution, execution_policy_summary),
        stages: trace
            .stages
            .into_iter()
            .map(|record| McpRuntimeStageSummary {
                stage_record_id: record.id,
                stage_kind: record.stage_kind,
                stage_ordinal: record.stage_ordinal,
                attempt_no: record.attempt_no,
                stage_state: record.stage_state,
                deterministic: record.deterministic,
                started_at: record.started_at,
                completed_at: record.completed_at,
                failure_code: record.failure_code,
                input_summary: record.input_summary_json,
                output_summary: record.output_summary_json,
            })
            .collect(),
        actions: trace
            .actions
            .into_iter()
            .map(|record| McpRuntimeActionSummary {
                action_id: record.id,
                stage_record_id: record.stage_record_id,
                action_kind: record.action_kind,
                action_ordinal: record.action_ordinal,
                action_state: record.action_state,
                provider_binding_id: record.provider_binding_id,
                tool_name: record.tool_name,
                usage: record.usage_json,
                summary: record.summary_json,
                created_at: record.created_at,
            })
            .collect(),
        policy_decisions: trace
            .policy_decisions
            .into_iter()
            .map(|decision| McpRuntimePolicySummary {
                decision_id: decision.id,
                stage_record_id: decision.stage_record_id,
                action_record_id: decision.action_record_id,
                target_kind: decision.target_kind,
                decision_kind: decision.decision_kind,
                reason_code: decision.reason_code,
                reason_summary: decision.reason_summary_redacted,
                created_at: decision.created_at,
            })
            .collect(),
    }
}

fn map_runtime_policy_summary(
    rows: &[runtime_repository::RuntimePolicyDecisionRow],
) -> RuntimePolicySummary {
    build_policy_summary(
        &rows
            .iter()
            .map(|row| RuntimePolicyDecision {
                id: row.id,
                runtime_execution_id: row.runtime_execution_id,
                stage_record_id: row.stage_record_id,
                action_record_id: row.action_record_id,
                target_kind: row.target_kind,
                decision_kind: row.decision_kind,
                reason_code: row.reason_code.clone(),
                reason_summary_redacted: row.reason_summary_redacted.clone(),
                created_at: row.created_at,
            })
            .collect::<Vec<_>>(),
    )
}

pub(crate) async fn resolve_search_libraries(
    auth: &AuthContext,
    state: &AppState,
    requested_library_ids: Option<&[Uuid]>,
) -> Result<Vec<VisibleLibraryContext>, ApiError> {
    if let Some(library_ids) = requested_library_ids {
        if library_ids.is_empty() {
            return Err(ApiError::invalid_mcp_tool_call(
                "libraryIds must not be empty when provided",
            ));
        }
        let mut rows = Vec::with_capacity(library_ids.len());
        for library_id in library_ids {
            let library = crate::interfaces::http::authorization::load_library_and_authorize(
                auth,
                state,
                *library_id,
                POLICY_MCP_MEMORY_READ,
            )
            .await?;
            rows.push(library);
        }
        return describe_libraries(auth, state, rows).await;
    }

    let libraries = load_visible_library_contexts(auth, state, None).await?;
    Ok(libraries
        .into_iter()
        .filter(|item| {
            auth.has_library_permission(
                item.library.workspace_id,
                item.library.id,
                POLICY_MCP_MEMORY_READ,
            )
        })
        .collect())
}

pub(crate) async fn load_visible_workspace_rows(
    auth: &AuthContext,
    state: &AppState,
) -> Result<Vec<CatalogWorkspaceRow>, ApiError> {
    let rows = catalog_repository::list_workspaces(&state.persistence.postgres)
        .await
        .map_err(|_| ApiError::Internal)?;

    Ok(rows.into_iter().filter(|row| authorize_workspace_discovery(auth, row.id).is_ok()).collect())
}

pub(crate) async fn load_visible_library_contexts(
    auth: &AuthContext,
    state: &AppState,
    workspace_filter: Option<Uuid>,
) -> Result<Vec<VisibleLibraryContext>, ApiError> {
    let workspace_ids = if let Some(workspace_id) = workspace_filter {
        authorize_workspace_discovery(auth, workspace_id)?;
        vec![workspace_id]
    } else {
        load_visible_workspace_rows(auth, state)
            .await?
            .into_iter()
            .map(|workspace| workspace.id)
            .collect::<Vec<_>>()
    };

    let mut libraries = Vec::new();
    for workspace_id in workspace_ids {
        let rows =
            catalog_repository::list_libraries(&state.persistence.postgres, Some(workspace_id))
                .await
                .map_err(|_| ApiError::Internal)?;
        for library in rows {
            if authorize_library_discovery(auth, workspace_id, library.id).is_ok() {
                libraries.push(library);
            }
        }
    }
    describe_libraries(auth, state, libraries).await
}

async fn describe_libraries(
    auth: &AuthContext,
    state: &AppState,
    libraries: Vec<CatalogLibraryRow>,
) -> Result<Vec<VisibleLibraryContext>, ApiError> {
    let readiness_by_library = state
        .canonical_services
        .catalog
        .list_library_ingestion_readiness(
            state,
            &libraries.iter().map(|library| library.id).collect::<Vec<_>>(),
        )
        .await?;

    let mut items = Vec::with_capacity(libraries.len());
    for library in libraries {
        let readiness = readiness_by_library.get(&library.id).cloned().unwrap_or(
            CatalogLibraryIngestionReadiness {
                ready: false,
                missing_binding_purposes: vec![AiBindingPurpose::ExtractGraph],
            },
        );
        items.push(describe_library(auth, state, library, readiness).await?);
    }
    Ok(items)
}

pub(crate) async fn describe_library(
    auth: &AuthContext,
    state: &AppState,
    library: CatalogLibraryRow,
    ingestion_readiness: CatalogLibraryIngestionReadiness,
) -> Result<VisibleLibraryContext, ApiError> {
    let supports_search =
        auth.has_library_permission(library.workspace_id, library.id, POLICY_MCP_MEMORY_READ);
    let supports_write =
        auth.has_library_permission(library.workspace_id, library.id, POLICY_LIBRARY_WRITE);
    let coverage = state
        .canonical_services
        .knowledge
        .get_library_knowledge_coverage(state, library.id)
        .await?;
    let document_count =
        usize::try_from(coverage.document_counts_by_readiness.values().copied().sum::<i64>())
            .unwrap_or(usize::MAX);
    let readable_document_count = readiness_count(&coverage, "readable")
        .saturating_add(usize::try_from(coverage.graph_sparse_document_count).unwrap_or(usize::MAX))
        .saturating_add(usize::try_from(coverage.graph_ready_document_count).unwrap_or(usize::MAX));
    let processing_document_count = readiness_count(&coverage, "processing");
    let descriptor = McpLibraryDescriptor {
        library_id: library.id,
        workspace_id: library.workspace_id,
        slug: library.slug.clone(),
        name: library.display_name.trim().to_string(),
        description: library.description.clone(),
        ingestion_readiness: map_ingestion_readiness(ingestion_readiness),
        document_count,
        readable_document_count,
        processing_document_count,
        failed_document_count: readiness_count(&coverage, "failed"),
        document_counts_by_readiness: coverage
            .document_counts_by_readiness
            .iter()
            .map(|(kind, count)| (kind.clone(), usize::try_from(*count).unwrap_or(usize::MAX)))
            .collect(),
        graph_ready_document_count: usize::try_from(coverage.graph_ready_document_count)
            .unwrap_or(usize::MAX),
        graph_sparse_document_count: usize::try_from(coverage.graph_sparse_document_count)
            .unwrap_or(usize::MAX),
        typed_fact_document_count: usize::try_from(coverage.typed_fact_document_count)
            .unwrap_or(usize::MAX),
        supports_search,
        supports_read: auth.has_document_or_library_read_scope_for_library(
            library.workspace_id,
            library.id,
            POLICY_MCP_MEMORY_READ,
        ),
        supports_write,
    };
    Ok(VisibleLibraryContext { library, descriptor })
}

fn map_ingestion_readiness(
    readiness: CatalogLibraryIngestionReadiness,
) -> McpLibraryIngestionReadiness {
    McpLibraryIngestionReadiness {
        ready: readiness.ready,
        missing_binding_purposes: readiness.missing_binding_purposes,
    }
}

pub(crate) async fn resolve_search_embedding_context(
    state: &AppState,
    library_id: Uuid,
    query_text: &str,
) -> Result<Option<McpSearchEmbeddingContext>, ApiError> {
    let Some(binding) = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
        .await?
    else {
        return Ok(None);
    };

    let generations = state
        .canonical_services
        .knowledge
        .derive_library_generation_rows(state, library_id)
        .await
        .map_err(|_| ApiError::Internal)?;
    let Some(generation) = generations.first() else {
        return Ok(None);
    };
    if generation.active_vector_generation <= 0 {
        return Ok(None);
    }

    let embedding = state
        .llm_gateway
        .embed(EmbeddingRequest {
            provider_kind: binding.provider_kind.clone(),
            model_name: binding.model_name.clone(),
            input: query_text.to_string(),
            api_key_override: Some(binding.api_key),
            base_url_override: binding.provider_base_url,
        })
        .await
        .map_err(|error| {
            ApiError::ProviderFailure(format!("failed to embed MCP memory search query: {error}"))
        })?;

    Ok(Some(McpSearchEmbeddingContext {
        model_catalog_id: binding.model_catalog_id,
        freshness_generation: generation.active_vector_generation,
        query_vector: embedding.embedding,
    }))
}

pub(crate) async fn load_knowledge_chunks_by_ids(
    state: &AppState,
    chunk_ids: &[Uuid],
) -> Result<Vec<crate::infra::arangodb::document_store::KnowledgeChunkRow>, ApiError> {
    if chunk_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut rows = Vec::with_capacity(chunk_ids.len());
    for chunk_id in chunk_ids {
        let cursor = state
            .arango_document_store
            .client()
            .query_json(
                "FOR chunk IN @@collection
                 FILTER chunk.chunk_id == @chunk_id
                 LIMIT 1
                 RETURN chunk",
                serde_json::json!({
                    "@collection": crate::infra::arangodb::collections::KNOWLEDGE_CHUNK_COLLECTION,
                    "chunk_id": chunk_id,
                }),
            )
            .await
            .map_err(|_| ApiError::Internal)?;
        let result = cursor.get("result").cloned().ok_or(ApiError::Internal)?;
        let mut decoded: Vec<crate::infra::arangodb::document_store::KnowledgeChunkRow> =
            serde_json::from_value(result).map_err(|_| ApiError::Internal)?;
        if let Some(row) = decoded.pop() {
            rows.push(row);
        }
    }
    Ok(rows)
}

pub(crate) async fn collect_revision_grounding_references(
    state: &AppState,
    revision_id: Uuid,
    chunk_ids: &[Uuid],
    limit: usize,
) -> Result<McpRevisionGroundingReferences, ApiError> {
    let technical_facts = state
        .arango_document_store
        .list_technical_facts_by_revision(revision_id)
        .await
        .map_err(|_| ApiError::Internal)?;
    let mut technical_fact_rows = technical_facts;
    technical_fact_rows.sort_by(|left, right| {
        technical_fact_support_score(right, chunk_ids)
            .cmp(&technical_fact_support_score(left, chunk_ids))
            .then_with(|| {
                right.confidence.unwrap_or(0.0).total_cmp(&left.confidence.unwrap_or(0.0))
            })
            .then_with(|| left.created_at.cmp(&right.created_at))
            .then_with(|| left.fact_id.cmp(&right.fact_id))
    });
    technical_fact_rows.truncate(limit);
    let technical_fact_references = technical_fact_rows
        .into_iter()
        .enumerate()
        .map(|(index, fact)| McpTechnicalFactReference {
            fact_id: fact.fact_id,
            fact_kind: fact.fact_kind,
            canonical_value: fact.canonical_value_text,
            display_value: fact.display_value,
            rank: saturating_rank(index),
            score: fact.confidence.unwrap_or(1.0),
            inclusion_reason: Some(
                if fact_supports_requested_chunks(&fact.support_chunk_ids, chunk_ids) {
                    "chunk_supported_fact"
                } else {
                    "revision_fact"
                }
                .to_string(),
            ),
        })
        .collect::<Vec<_>>();
    let evidence_rows = state
        .arango_graph_store
        .list_evidence_by_revision(revision_id)
        .await
        .map_err(|_| ApiError::Internal)?;
    let mut evidence_rows = evidence_rows;
    evidence_rows.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.evidence_id.cmp(&right.evidence_id))
    });
    evidence_rows.truncate(limit);
    let evidence_references = evidence_rows
        .iter()
        .enumerate()
        .map(|(index, evidence)| McpEvidenceReference {
            evidence_id: evidence.evidence_id,
            rank: saturating_rank(index),
            score: evidence.confidence.unwrap_or(1.0),
            inclusion_reason: Some("revision_evidence".to_string()),
        })
        .collect::<Vec<_>>();
    let evidence_ids = evidence_rows.iter().map(|row| row.evidence_id).collect::<Vec<_>>();

    let entity_references = if chunk_ids.is_empty() {
        Vec::new()
    } else {
        let cursor = state
            .arango_graph_store
            .client()
            .query_json(
                "FOR edge IN @@collection
                 FILTER edge.chunk_id IN @chunk_ids
                 COLLECT entity_id = edge.entity_id
                 AGGREGATE rank = MIN(edge.rank), score = MAX(edge.score)
                 LET reason = FIRST(
                    FOR item IN @@collection
                    FILTER item.entity_id == entity_id AND item.chunk_id IN @chunk_ids
                    SORT item.rank ASC, item.created_at ASC, item._key ASC
                    LIMIT 1
                    RETURN item.inclusionReason
                 )
                 SORT rank ASC, score DESC, entity_id ASC
                 LIMIT @limit
                 RETURN {
                    entity_id,
                    rank,
                    score,
                    inclusion_reason: reason
                 }",
                serde_json::json!({
                    "@collection": crate::infra::arangodb::collections::KNOWLEDGE_CHUNK_MENTIONS_ENTITY_EDGE,
                    "chunk_ids": chunk_ids,
                    "limit": limit.max(1),
                }),
            )
            .await
            .map_err(|_| ApiError::Internal)?;
        let result = cursor.get("result").cloned().ok_or(ApiError::Internal)?;
        let rows: Vec<ArangoChunkMentionReferenceRow> =
            serde_json::from_value(result).map_err(|_| ApiError::Internal)?;
        rows.into_iter()
            .map(|row| McpEntityReference {
                entity_id: row.entity_id,
                rank: row.rank,
                score: row.score,
                inclusion_reason: row.inclusion_reason,
            })
            .collect()
    };

    let relation_references = if evidence_ids.is_empty() {
        Vec::new()
    } else {
        let cursor = state
            .arango_graph_store
            .client()
            .query_json(
                "FOR edge IN @@collection
                 FILTER edge.evidence_id IN @evidence_ids
                 COLLECT relation_id = edge.relation_id
                 AGGREGATE rank = MIN(edge.rank), score = MAX(edge.score)
                 LET reason = FIRST(
                    FOR item IN @@collection
                    FILTER item.relation_id == relation_id AND item.evidence_id IN @evidence_ids
                    SORT item.rank ASC, item.created_at ASC, item._key ASC
                    LIMIT 1
                    RETURN item.inclusionReason
                 )
                 SORT rank ASC, score DESC, relation_id ASC
                 LIMIT @limit
                 RETURN {
                    relation_id,
                    rank,
                    score,
                    inclusion_reason: reason
                 }",
                serde_json::json!({
                    "@collection": crate::infra::arangodb::collections::KNOWLEDGE_EVIDENCE_SUPPORTS_RELATION_EDGE,
                    "evidence_ids": evidence_ids,
                    "limit": limit.max(1),
                }),
            )
            .await
            .map_err(|_| ApiError::Internal)?;
        let result = cursor.get("result").cloned().ok_or(ApiError::Internal)?;
        let rows: Vec<ArangoRelationSupportReferenceRow> =
            serde_json::from_value(result).map_err(|_| ApiError::Internal)?;
        rows.into_iter()
            .map(|row| McpRelationReference {
                relation_id: row.relation_id,
                rank: row.rank,
                score: row.score,
                inclusion_reason: row.inclusion_reason,
            })
            .collect()
    };

    Ok(McpRevisionGroundingReferences {
        technical_fact_references,
        entity_references,
        relation_references,
        evidence_references,
    })
}

fn readable_status_reason(
    readiness_summary: &crate::domains::content::DocumentReadinessSummary,
    grounding: &McpRevisionGroundingReferences,
) -> Option<String> {
    if readiness_summary.readiness_kind == "readable" {
        return Some(
            "document text is readable, but canonical preparation and graph extraction are still processing"
                .to_string(),
        );
    }
    if readiness_summary.graph_coverage_kind == "graph_sparse"
        && grounding.technical_fact_references.is_empty()
        && grounding.entity_references.is_empty()
        && grounding.relation_references.is_empty()
        && grounding.evidence_references.is_empty()
    {
        return Some(
            "document text is readable, but graph coverage is still sparse for this revision"
                .to_string(),
        );
    }
    (grounding.technical_fact_references.is_empty()
        && grounding.entity_references.is_empty()
        && grounding.relation_references.is_empty()
        && grounding.evidence_references.is_empty())
        .then_some(
            "document text is readable, but canonical technical facts and graph evidence are not available yet"
                .to_string(),
        )
}

fn readability_state_from_kind(readiness_kind: &str) -> McpReadabilityState {
    match readiness_kind {
        "failed" => McpReadabilityState::Failed,
        "processing" => McpReadabilityState::Processing,
        "readable" | "graph_sparse" | "graph_ready" => McpReadabilityState::Readable,
        _ => McpReadabilityState::Unavailable,
    }
}

fn readiness_count(
    coverage: &crate::domains::content::LibraryKnowledgeCoverage,
    readiness_kind: &str,
) -> usize {
    coverage
        .document_counts_by_readiness
        .get(readiness_kind)
        .copied()
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or_default()
}

fn fact_supports_requested_chunks(support_chunk_ids: &[Uuid], chunk_ids: &[Uuid]) -> bool {
    !support_chunk_ids.is_empty()
        && support_chunk_ids.iter().any(|support_chunk_id| chunk_ids.contains(support_chunk_id))
}

fn technical_fact_support_score(
    fact: &crate::infra::arangodb::document_store::KnowledgeTechnicalFactRow,
    chunk_ids: &[Uuid],
) -> (bool, usize, usize) {
    (
        fact_supports_requested_chunks(&fact.support_chunk_ids, chunk_ids),
        fact.support_chunk_ids.len(),
        fact.support_block_ids.len(),
    )
}
