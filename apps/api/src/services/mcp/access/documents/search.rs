use crate::infra::knowledge_plane::{ChunkLexicalQuery, VectorSimilaritySearchInput};
use crate::interfaces::http::router_support::current_embedding_search_guard;

use super::grounding::saturating_rank;
use super::*;
use crate::infra::knowledge_plane::LexicalQueryOrigin;

use std::collections::{BTreeSet, HashSet};

const MCP_SEARCH_REVALIDATION_HEADROOM_MULTIPLIER: usize = 3;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct SearchEvidenceScope {
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    readable_revision_id: Option<Uuid>,
}

impl SearchEvidenceScope {
    const fn new(
        workspace_id: Uuid,
        library_id: Uuid,
        document_id: Uuid,
        readable_revision_id: Option<Uuid>,
    ) -> Self {
        Self { workspace_id, library_id, document_id, readable_revision_id }
    }

    fn from_hit(hit: &McpDocumentHit) -> Self {
        Self::new(hit.workspace_id, hit.library_id, hit.document_id, hit.latest_revision_id)
    }
}

fn accumulator_matches_scope(
    accumulator: &McpDocumentAccumulator,
    scope: SearchEvidenceScope,
) -> bool {
    accumulator.workspace_id == scope.workspace_id
        && accumulator.library_id == scope.library_id
        && accumulator.document_id == scope.document_id
        && accumulator.readable_revision_id == scope.readable_revision_id
}

fn accumulator_accepts_scope(
    accumulators: &HashMap<Uuid, McpDocumentAccumulator>,
    scope: SearchEvidenceScope,
) -> bool {
    accumulators
        .get(&scope.document_id)
        .is_none_or(|accumulator| accumulator_matches_scope(accumulator, scope))
}

fn accumulator_for_exact_scope(
    accumulators: &mut HashMap<Uuid, McpDocumentAccumulator>,
    scope: SearchEvidenceScope,
    create: impl FnOnce() -> McpDocumentAccumulator,
) -> Option<&mut McpDocumentAccumulator> {
    use std::collections::hash_map::Entry;

    match accumulators.entry(scope.document_id) {
        Entry::Occupied(entry) => {
            accumulator_matches_scope(entry.get(), scope).then(|| entry.into_mut())
        }
        Entry::Vacant(entry) => {
            let accumulator = create();
            accumulator_matches_scope(&accumulator, scope).then(|| entry.insert(accumulator))
        }
    }
}

struct SearchLibraryMaterial {
    metadata_hits:
        Vec<crate::infra::repositories::content_metadata_search_repository::ContentDocumentMetadataSearchRow>,
    structured_block_hits: Vec<
        crate::infra::repositories::content_structured_block_search_repository::ContentStructuredBlockSearchRow,
    >,
    lexical_chunk_hits: Vec<KnowledgeChunkSearchRow>,
    vector_chunk_hits: Vec<KnowledgeChunkVectorSearchRow>,
    chunk_map: HashMap<Uuid, KnowledgeChunkRow>,
    document_map: HashMap<Uuid, KnowledgeDocumentRow>,
    revision_map: HashMap<Uuid, KnowledgeRevisionRow>,
}

async fn load_search_library_material(
    state: &AppState,
    library: &VisibleLibraryContext,
    query: &str,
    lexical_limit: usize,
) -> Result<SearchLibraryMaterial, ApiError> {
    let metadata_hits = content_metadata_search_repository::search_document_metadata_rows(
        &state.persistence.postgres,
        library.library.id,
        query,
        lexical_limit as u32,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let structured_block_hits =
        content_structured_block_search_repository::search_readable_structured_blocks_without_ready_chunks(
            &state.persistence.postgres,
            library.library.id,
            query,
            lexical_limit as u32,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let lexical_chunk_hits =
        search_chunks_with_query_variants(state, library.library.id, query, lexical_limit).await?;
    let embedding_context =
        resolve_search_embedding_context(state, library.library.id, query).await?;
    let vector_chunk_hits = load_vector_search_hits(
        state,
        library.library.id,
        embedding_context.as_ref(),
        lexical_limit,
    )
    .await?;
    let all_chunk_ids = lexical_chunk_hits
        .iter()
        .map(|hit| hit.chunk_id)
        .chain(vector_chunk_hits.iter().map(|hit| hit.chunk_id))
        .collect::<Vec<_>>();
    let chunk_map = load_knowledge_chunks_by_ids(state, &all_chunk_ids)
        .await?
        .into_iter()
        .map(|row| (row.chunk_id, row))
        .collect::<HashMap<_, _>>();
    let chunk_document_ids = unique_chunk_document_ids(&chunk_map);
    let chunk_revision_ids = unique_chunk_revision_ids(&chunk_map);
    let (document_rows, revision_rows) = tokio::try_join!(
        state.document_store.list_documents_by_ids(&chunk_document_ids),
        state.document_store.list_revisions_by_ids(&chunk_revision_ids),
    )
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    Ok(SearchLibraryMaterial {
        metadata_hits,
        structured_block_hits,
        lexical_chunk_hits,
        vector_chunk_hits,
        chunk_map,
        document_map: document_rows.into_iter().map(|row| (row.document_id, row)).collect(),
        revision_map: revision_rows.into_iter().map(|row| (row.revision_id, row)).collect(),
    })
}

fn accumulate_structured_block_search_hits(
    accumulators: &mut HashMap<Uuid, McpDocumentAccumulator>,
    prepared_evidence_scopes: &mut HashSet<SearchEvidenceScope>,
    hits: &[crate::infra::repositories::content_structured_block_search_repository::ContentStructuredBlockSearchRow],
    query: &str,
    read_window_chars: usize,
) {
    let document_ranks = dense_document_ranks_by_best_hit(hits.iter().filter_map(|hit| {
        let scope = SearchEvidenceScope::new(
            hit.workspace_id,
            hit.library_id,
            hit.document_id,
            Some(hit.readable_revision_id),
        );
        accumulator_accepts_scope(accumulators, scope).then_some((hit.document_id, hit.text_score))
    }));
    for hit in hits {
        let scope = SearchEvidenceScope::new(
            hit.workspace_id,
            hit.library_id,
            hit.document_id,
            Some(hit.readable_revision_id),
        );
        let Some(document_rank) = document_ranks.get(&hit.document_id).copied() else {
            continue;
        };
        let Some(accumulator) = accumulator_for_exact_scope(accumulators, scope, || {
            McpDocumentAccumulator::from_structured_block(hit)
        }) else {
            continue;
        };
        accumulator.observe_lane(SearchLane::StructuredBlock, document_rank);
        accumulator.merge_structured_block_match(hit, query, read_window_chars);
        prepared_evidence_scopes.insert(scope);
    }
}

async fn load_vector_search_hits(
    state: &AppState,
    library_id: Uuid,
    embedding_context: Option<&McpSearchEmbeddingContext>,
    lexical_limit: usize,
) -> Result<Vec<KnowledgeChunkVectorSearchRow>, ApiError> {
    let Some(context) = embedding_context else {
        return Ok(Vec::new());
    };
    let _vector_guard = current_embedding_search_guard(
        state,
        library_id,
        &context.embedding_profile_key,
        context.inventory_version,
    )
    .await?;
    validate_embedding_vector_dimensions(
        context.dimensions,
        &context.query_vector,
        "MCP document search query",
    )
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    match state
        .search_store
        .search_chunk_vectors_by_document_similarity(
            VectorSimilaritySearchInput {
                dim: context.dimensions,
                library_id,
                embedding_model_key: &context.embedding_profile_key,
                query_vector: &context.query_vector,
                n_probe: None,
            },
            lexical_limit,
            None,
            None,
        )
        .await
    {
        Ok(rows) => Ok(rows),
        Err(error) => {
            warn!(
                %library_id,
                model_catalog_id = %context.model_catalog_id,
                error = ?error,
                "mcp search vector lookup failed; degrading to lexical-only MCP search",
            );
            Ok(Vec::new())
        }
    }
}

fn unique_chunk_document_ids(chunk_map: &HashMap<Uuid, KnowledgeChunkRow>) -> Vec<Uuid> {
    chunk_map
        .values()
        .map(|chunk| chunk.document_id)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn unique_chunk_revision_ids(chunk_map: &HashMap<Uuid, KnowledgeChunkRow>) -> Vec<Uuid> {
    chunk_map
        .values()
        .map(|chunk| chunk.revision_id)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn accumulate_metadata_search_hits(
    accumulators: &mut HashMap<Uuid, McpDocumentAccumulator>,
    metadata_hits: &[crate::infra::repositories::content_metadata_search_repository::ContentDocumentMetadataSearchRow],
    query: &str,
) {
    for (index, hit) in metadata_hits.iter().enumerate() {
        let scope = SearchEvidenceScope::new(
            hit.workspace_id,
            hit.library_id,
            hit.document_id,
            hit.readable_revision_id,
        );
        let Some(accumulator) = accumulator_for_exact_scope(accumulators, scope, || {
            McpDocumentAccumulator::from_metadata(hit)
        }) else {
            continue;
        };
        accumulator.observe_lane(SearchLane::Metadata, saturating_rank(index));
        accumulator.populate_excerpt_from_text(&hit.matched_text, query);
    }
}

/// Assign one dense RRF rank per document from a chunk-level retrieval lane.
///
/// A document may contribute many leading chunks. Ranking the next document
/// by its raw chunk position would penalize it for the first document's
/// fragmentation (for example, chunk ranks 1/2/3 followed by another
/// document would incorrectly become document ranks 1 and 4). Collapse each
/// document to its best chunk score first, order equal scores by document ID,
/// and only then assign contiguous 1-based ranks.
fn dense_document_ranks_by_best_hit(
    hits: impl IntoIterator<Item = (Uuid, f64)>,
) -> HashMap<Uuid, i32> {
    let mut best_score_by_document = HashMap::<Uuid, f64>::new();
    for (document_id, score) in hits {
        best_score_by_document
            .entry(document_id)
            .and_modify(|best_score| {
                if score.total_cmp(best_score).is_gt() {
                    *best_score = score;
                }
            })
            .or_insert(score);
    }

    let mut ranked_documents = best_score_by_document.into_iter().collect::<Vec<_>>();
    ranked_documents.sort_by(|(left_id, left_score), (right_id, right_score)| {
        right_score.total_cmp(left_score).then_with(|| left_id.cmp(right_id))
    });
    ranked_documents
        .into_iter()
        .enumerate()
        .map(|(index, (document_id, _))| (document_id, saturating_rank(index)))
        .collect()
}

trait ChunkSearchHit {
    const LANE: SearchLane;
    const REFERENCE_SOURCE: &'static str;

    fn material(&self) -> ChunkSearchHitMaterial;
    fn excerpt_text<'a>(&'a self, chunk: &'a KnowledgeChunkRow) -> &'a str;
}

#[derive(Clone, Copy)]
struct ChunkSearchHitMaterial {
    chunk_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    revision_id: Uuid,
    score: f64,
}

impl ChunkSearchHitMaterial {
    const fn new(
        chunk_id: Uuid,
        workspace_id: Uuid,
        library_id: Uuid,
        revision_id: Uuid,
        score: f64,
    ) -> Self {
        Self { chunk_id, workspace_id, library_id, revision_id, score }
    }
}

impl ChunkSearchHit for KnowledgeChunkSearchRow {
    const LANE: SearchLane = SearchLane::LexicalChunk;
    const REFERENCE_SOURCE: &'static str = "lexical_chunk";

    fn material(&self) -> ChunkSearchHitMaterial {
        ChunkSearchHitMaterial::new(
            self.chunk_id,
            self.workspace_id,
            self.library_id,
            self.revision_id,
            self.score,
        )
    }

    fn excerpt_text<'a>(&'a self, _chunk: &'a KnowledgeChunkRow) -> &'a str {
        &self.normalized_text
    }
}

impl ChunkSearchHit for KnowledgeChunkVectorSearchRow {
    const LANE: SearchLane = SearchLane::VectorChunk;
    const REFERENCE_SOURCE: &'static str = "vector_chunk";

    fn material(&self) -> ChunkSearchHitMaterial {
        ChunkSearchHitMaterial::new(
            self.chunk_id,
            self.workspace_id,
            self.library_id,
            self.revision_id,
            self.score,
        )
    }

    fn excerpt_text<'a>(&'a self, chunk: &'a KnowledgeChunkRow) -> &'a str {
        &chunk.normalized_text
    }
}

/// Chunk/document/revision lookup maps shared by every chunk-derived
/// retrieval lane.
struct SearchLaneMaterials<'a> {
    chunk_map: &'a HashMap<Uuid, KnowledgeChunkRow>,
    document_map: &'a HashMap<Uuid, KnowledgeDocumentRow>,
    revision_map: &'a HashMap<Uuid, KnowledgeRevisionRow>,
}

/// Excerpt projection inputs shared by every chunk-derived retrieval lane.
struct SearchExcerptContext<'a> {
    query: &'a str,
    read_window_chars: usize,
}

fn accumulate_chunk_search_hits<H: ChunkSearchHit>(
    accumulators: &mut HashMap<Uuid, McpDocumentAccumulator>,
    prepared_evidence_scopes: &mut HashSet<SearchEvidenceScope>,
    hits: &[H],
    materials: SearchLaneMaterials<'_>,
    excerpt: SearchExcerptContext<'_>,
) {
    let document_ranks = dense_document_ranks_by_best_hit(hits.iter().filter_map(|hit| {
        chunk_search_hit_context(
            hit,
            materials.chunk_map,
            materials.document_map,
            materials.revision_map,
        )
        .and_then(|(_, document, revision)| {
            let scope = SearchEvidenceScope::new(
                document.workspace_id,
                document.library_id,
                document.document_id,
                Some(revision.revision_id),
            );
            accumulator_accepts_scope(accumulators, scope)
                .then_some((document.document_id, hit.material().score))
        })
    }));
    for (index, hit) in hits.iter().enumerate() {
        let Some((chunk, document, revision)) = chunk_search_hit_context(
            hit,
            materials.chunk_map,
            materials.document_map,
            materials.revision_map,
        ) else {
            continue;
        };
        let scope = SearchEvidenceScope::new(
            document.workspace_id,
            document.library_id,
            document.document_id,
            Some(revision.revision_id),
        );
        let Some(document_rank) = document_ranks.get(&document.document_id).copied() else {
            continue;
        };
        let Some(accumulator) = accumulator_for_exact_scope(accumulators, scope, || {
            McpDocumentAccumulator::from_knowledge(document, revision)
        }) else {
            continue;
        };
        accumulator.observe_lane(H::LANE, document_rank);
        accumulator.merge_chunk_span_anchor(
            chunk.span_start,
            hit.material().score,
            excerpt.read_window_chars,
        );
        accumulator.merge_chunk_reference(
            chunk.chunk_id,
            saturating_rank(index),
            hit.material().score,
            Some(H::REFERENCE_SOURCE.to_string()),
        );
        accumulator.populate_excerpt_from_text(hit.excerpt_text(chunk), excerpt.query);
        prepared_evidence_scopes.insert(scope);
    }
}

fn accumulate_lexical_search_hits(
    accumulators: &mut HashMap<Uuid, McpDocumentAccumulator>,
    prepared_evidence_scopes: &mut HashSet<SearchEvidenceScope>,
    hits: &[KnowledgeChunkSearchRow],
    materials: SearchLaneMaterials<'_>,
    excerpt: SearchExcerptContext<'_>,
) {
    accumulate_chunk_search_hits(accumulators, prepared_evidence_scopes, hits, materials, excerpt);
}

fn accumulate_vector_search_hits(
    accumulators: &mut HashMap<Uuid, McpDocumentAccumulator>,
    prepared_evidence_scopes: &mut HashSet<SearchEvidenceScope>,
    hits: &[KnowledgeChunkVectorSearchRow],
    materials: SearchLaneMaterials<'_>,
    excerpt: SearchExcerptContext<'_>,
) {
    accumulate_chunk_search_hits(accumulators, prepared_evidence_scopes, hits, materials, excerpt);
}

fn chunk_search_hit_context<'a, H: ChunkSearchHit>(
    hit: &H,
    chunk_map: &'a HashMap<Uuid, KnowledgeChunkRow>,
    document_map: &'a HashMap<Uuid, KnowledgeDocumentRow>,
    revision_map: &'a HashMap<Uuid, KnowledgeRevisionRow>,
) -> Option<(&'a KnowledgeChunkRow, &'a KnowledgeDocumentRow, &'a KnowledgeRevisionRow)> {
    let material = hit.material();
    let context = search_hit_context(material.chunk_id, chunk_map, document_map, revision_map)?;
    let (chunk, _, _) = context;
    if material.workspace_id != chunk.workspace_id
        || material.library_id != chunk.library_id
        || material.revision_id != chunk.revision_id
    {
        return None;
    }
    Some(context)
}

fn search_hit_context<'a>(
    chunk_id: Uuid,
    chunk_map: &'a HashMap<Uuid, KnowledgeChunkRow>,
    document_map: &'a HashMap<Uuid, KnowledgeDocumentRow>,
    revision_map: &'a HashMap<Uuid, KnowledgeRevisionRow>,
) -> Option<(&'a KnowledgeChunkRow, &'a KnowledgeDocumentRow, &'a KnowledgeRevisionRow)> {
    let chunk = chunk_map.get(&chunk_id)?;
    let document = document_map.get(&chunk.document_id)?;
    let revision = revision_map.get(&chunk.revision_id)?;
    if document.workspace_id != chunk.workspace_id
        || document.library_id != chunk.library_id
        || revision.document_id != chunk.document_id
        || revision.workspace_id != chunk.workspace_id
        || revision.library_id != chunk.library_id
    {
        return None;
    }
    Some((chunk, document, revision))
}

#[derive(sqlx::FromRow)]
struct CurrentSearchScopeRow {
    document_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    readable_revision_id: Option<Uuid>,
    is_prepared: bool,
    external_key: String,
}

const CURRENT_SEARCH_SCOPES_SQL: &str = concat!(
    "select document.id as document_id, document.workspace_id, document.library_id,
        current_revision.revision_id as readable_revision_id,
        (structured_revision.revision_id is not null) as is_prepared,
        document.external_key
     from content_document as document
      left join content_document_head h on h.document_id = document.id
      left join content_current_revision current_revision
        on current_revision.document_id = document.id
       and current_revision.workspace_id = document.workspace_id
       and current_revision.library_id = document.library_id
      left join content_revision as revision
       on revision.id = current_revision.revision_id
      and revision.document_id = document.id
      and revision.workspace_id = document.workspace_id
      and revision.library_id = document.library_id
     join knowledge_document as knowledge_document
       on knowledge_document.document_id = document.id
      and knowledge_document.workspace_id = document.workspace_id
      and knowledge_document.library_id = document.library_id
      and knowledge_document.document_state = 'active'
      and knowledge_document.deleted_at is null
     left join knowledge_structured_revision as structured_revision
       on structured_revision.revision_id = current_revision.revision_id
      and structured_revision.document_id = document.id
      and structured_revision.workspace_id = document.workspace_id
      and structured_revision.library_id = document.library_id
      and structured_revision.preparation_state = 'prepared'
     where document.id = any($1::uuid[])
        and document.document_state = 'active'
         and document.deleted_at is null
         and not (",
    crate::infra::repositories::content_document_head_repository::unresolved_head_suppression!(),
    ") and (current_revision.revision_id is null or revision.id is not null)",
);

async fn load_current_search_scopes(
    state: &AppState,
    document_ids: impl IntoIterator<Item = Uuid>,
) -> Result<HashMap<Uuid, CurrentSearchScopeRow>, ApiError> {
    let document_ids =
        document_ids.into_iter().collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>();
    if document_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = sqlx::query_as::<_, CurrentSearchScopeRow>(CURRENT_SEARCH_SCOPES_SQL)
        .bind(&document_ids)
        .fetch_all(&state.persistence.postgres)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    Ok(rows.into_iter().map(|row| (row.document_id, row)).collect())
}

fn retain_current_search_hits(
    hits: &mut Vec<McpDocumentHit>,
    current_scopes: &HashMap<Uuid, CurrentSearchScopeRow>,
    prepared_evidence_scopes: &HashSet<SearchEvidenceScope>,
) {
    hits.retain(|hit| {
        let hit_scope = SearchEvidenceScope::from_hit(hit);
        current_scopes.get(&hit.document_id).is_some_and(|current| {
            current.workspace_id == hit.workspace_id
                && current.library_id == hit.library_id
                && current.readable_revision_id == hit_scope.readable_revision_id
                && (current.is_prepared || !prepared_evidence_scopes.contains(&hit_scope))
                && (hit.latest_revision_id.is_some()
                    || operational_hit_has_no_content(hit, &current.external_key))
        })
    });
}

fn operational_hit_has_no_content(hit: &McpDocumentHit, external_key: &str) -> bool {
    hit.readability_state != McpReadabilityState::Readable
        && matches!(hit.readiness_kind.as_str(), "empty" | "processing" | "failed")
        && matches!(hit.graph_coverage_kind.as_str(), "empty" | "processing" | "failed")
        && hit.document_title == external_key
        && operational_excerpt_matches_key(hit, external_key)
        && hit.suggested_start_offset.is_none()
        && hit.chunk_references.is_empty()
        && hit.technical_fact_references.is_empty()
        && hit.entity_references.is_empty()
        && hit.relation_references.is_empty()
        && hit.evidence_references.is_empty()
}

fn operational_excerpt_matches_key(hit: &McpDocumentHit, external_key: &str) -> bool {
    match (&hit.excerpt, hit.excerpt_start_offset, hit.excerpt_end_offset) {
        (None, None, None) => true,
        (Some(excerpt), Some(start), Some(end)) if start <= end => {
            end <= external_key.chars().count()
                && external_key.chars().skip(start).take(end - start).eq(excerpt.chars())
        }
        _ => false,
    }
}

fn search_revalidation_candidate_cap(limit: usize, max_search_limit: usize) -> usize {
    let max_search_limit = max_search_limit.max(1);
    let bounded_limit = limit.clamp(1, max_search_limit);
    let max_candidate_cap =
        max_search_limit.saturating_mul(MCP_SEARCH_REVALIDATION_HEADROOM_MULTIPLIER);
    bounded_limit
        .checked_mul(MCP_SEARCH_REVALIDATION_HEADROOM_MULTIPLIER)
        .unwrap_or(max_candidate_cap)
        .min(max_candidate_cap)
}

fn merge_bounded_search_reservoir<T>(
    reservoir: &mut Vec<T>,
    candidates: impl IntoIterator<Item = T>,
    candidate_cap: usize,
    mut compare: impl FnMut(&T, &T) -> std::cmp::Ordering,
) {
    reservoir.extend(candidates);
    reservoir.sort_by(&mut compare);
    reservoir.truncate(candidate_cap);
}

/// Projects one accumulated document into a public search hit, or `None`
/// when the document is deleted, not readable, or filtered out by
/// `include_unreadable`. Readiness comes from the knowledge-plane row, so its
/// absence means the content and knowledge planes disagree about deletion —
/// a routine convergence state, not an internal error.
async fn build_search_document_hit(
    state: &AppState,
    accumulator: McpDocumentAccumulator,
    include_unreadable: bool,
) -> Result<Option<McpDocumentHit>, ApiError> {
    let fused_score = accumulator.fused_score();
    let chunk_references = accumulator.clone().into_chunk_references();
    let content_summary =
        match state.application_services.content.get_document(state, accumulator.document_id).await
        {
            Ok(summary) => summary,
            // A candidate withdrawn during retrieval is not a search failure.
            Err(ApiError::NotFound(_)) => return Ok(None),
            Err(error) => return Err(error),
        };
    let Some(readiness_summary) = content_summary.readiness_summary else {
        tracing::warn!(
            document_id = %accumulator.document_id,
            library_id = %accumulator.library_id,
            "search hit has no readiness summary: content and knowledge planes disagree about deletion"
        );
        return Ok(None);
    };
    let readability_state = readability_state_from_kind(readiness_summary.readiness_kind);
    if !include_unreadable && readability_state != McpReadabilityState::Readable {
        return Ok(None);
    }
    let grounding = match accumulator.readable_revision_id {
        Some(revision_id) => {
            collect_revision_grounding_references(
                state,
                revision_id,
                &accumulator.chunk_reference_ids(),
                8,
            )
            .await?
        }
        None => McpRevisionGroundingReferences::default(),
    };
    let status_reason = match readability_state {
        McpReadabilityState::Readable => readable_status_reason(&readiness_summary, &grounding),
        McpReadabilityState::Failed => {
            Some("latest readable revision extraction failed".to_string())
        }
        McpReadabilityState::Processing => {
            Some("latest revision is still being extracted".to_string())
        }
        McpReadabilityState::Unavailable => {
            Some("document has no readable revision yet".to_string())
        }
    };
    Ok(Some(McpDocumentHit {
        document_id: accumulator.document_id,
        library_id: accumulator.library_id,
        workspace_id: accumulator.workspace_id,
        document_title: accumulator.document_title,
        latest_revision_id: accumulator.readable_revision_id,
        score: fused_score,
        excerpt: accumulator.excerpt,
        excerpt_start_offset: accumulator.excerpt_start_offset,
        excerpt_end_offset: accumulator.excerpt_end_offset,
        suggested_start_offset: accumulator.suggested_start_offset,
        readability_state,
        readiness_kind: readiness_summary.readiness_kind.as_str().to_string(),
        graph_coverage_kind: readiness_summary.graph_coverage_kind.clone(),
        status_reason,
        chunk_references,
        technical_fact_references: grounding.technical_fact_references,
        entity_references: grounding.entity_references,
        relation_references: grounding.relation_references,
        evidence_references: grounding.evidence_references,
    }))
}

fn clear_search_reference_evidence(hits: &mut [McpDocumentHit]) {
    for hit in hits {
        hit.chunk_references.clear();
        hit.technical_fact_references.clear();
        hit.entity_references.clear();
        hit.relation_references.clear();
        hit.evidence_references.clear();
    }
}

pub(crate) async fn search_documents(
    auth: &AuthContext,
    state: &AppState,
    request: McpSearchDocumentsRequest,
) -> Result<McpSearchDocumentsResponse, ApiError> {
    auth.require_any_scope(POLICY_MCP_MEMORY_READ)?;
    let settings = &state.mcp_memory;
    let include_references = request.include_references.unwrap_or(false);
    let include_unreadable = request.include_unreadable.unwrap_or(false);
    let query = request.query.trim();
    if query.is_empty() {
        return Err(ApiError::BadRequest("query must not be empty".into()));
    }

    let limit =
        request.limit.unwrap_or(settings.default_search_limit).clamp(1, settings.max_search_limit);
    let revalidation_candidate_cap =
        search_revalidation_candidate_cap(limit, settings.max_search_limit);
    let requested_library_refs = request.requested_library_refs();
    let libraries =
        resolve_search_libraries(auth, state, requested_library_refs.as_deref()).await?;
    let library_refs =
        libraries.iter().map(|item| item.descriptor.catalog_ref.clone()).collect::<Vec<_>>();
    let mut hits = Vec::new();
    let mut all_prepared_evidence_scopes = HashSet::new();
    for library in libraries {
        let lexical_limit = limit.saturating_mul(3).max(6);
        let SearchLibraryMaterial {
            metadata_hits,
            structured_block_hits,
            lexical_chunk_hits,
            vector_chunk_hits,
            chunk_map,
            document_map,
            revision_map,
        } = load_search_library_material(state, &library, query, lexical_limit).await?;
        let mut prepared_evidence_scopes = HashSet::new();
        let mut document_accumulators = HashMap::<Uuid, McpDocumentAccumulator>::new();

        // Metadata reflects the content-plane readable head. Freeze that
        // exact identity first, then admit knowledge/body lanes only when
        // their complete scope and revision agree with it.
        accumulate_metadata_search_hits(&mut document_accumulators, &metadata_hits, query);
        accumulate_structured_block_search_hits(
            &mut document_accumulators,
            &mut prepared_evidence_scopes,
            &structured_block_hits,
            query,
            settings.default_read_window_chars,
        );
        accumulate_lexical_search_hits(
            &mut document_accumulators,
            &mut prepared_evidence_scopes,
            &lexical_chunk_hits,
            SearchLaneMaterials {
                chunk_map: &chunk_map,
                document_map: &document_map,
                revision_map: &revision_map,
            },
            SearchExcerptContext { query, read_window_chars: settings.default_read_window_chars },
        );
        accumulate_vector_search_hits(
            &mut document_accumulators,
            &mut prepared_evidence_scopes,
            &vector_chunk_hits,
            SearchLaneMaterials {
                chunk_map: &chunk_map,
                document_map: &document_map,
                revision_map: &revision_map,
            },
            SearchExcerptContext { query, read_window_chars: settings.default_read_window_chars },
        );

        let mut library_hits = Vec::new();
        for accumulator in document_accumulators.into_values() {
            if let Some(hit) =
                build_search_document_hit(state, accumulator, include_unreadable).await?
            {
                library_hits.push(hit);
            }
        }
        // Do not let a stale high-ranked hit consume this library's bound.
        // A second global validation below closes advances that happen while
        // later libraries are being searched.
        let current_scopes =
            load_current_search_scopes(state, library_hits.iter().map(|hit| hit.document_id))
                .await?;
        retain_current_search_hits(&mut library_hits, &current_scopes, &prepared_evidence_scopes);
        library_hits.sort_by(search_document_hit_order);
        library_hits.truncate(revalidation_candidate_cap);
        all_prepared_evidence_scopes.extend(
            library_hits
                .iter()
                .map(SearchEvidenceScope::from_hit)
                .filter(|scope| prepared_evidence_scopes.contains(scope)),
        );
        merge_bounded_search_reservoir(
            &mut hits,
            library_hits,
            revalidation_candidate_cap,
            search_document_hit_order,
        );
        let retained_scopes =
            hits.iter().map(SearchEvidenceScope::from_hit).collect::<HashSet<_>>();
        all_prepared_evidence_scopes.retain(|scope| retained_scopes.contains(scope));
    }
    // Every retrieval lane and every asynchronous hydration above used a valid
    // scope when it ran, but a concurrent publication can advance A to B
    // between those statements. Re-read the canonical published view only after
    // every library and await, then reject any hit whose frozen revision/scope
    // no longer matches. include_unreadable never grants access to an unpublished
    // revision; body-derived evidence additionally requires a prepared artifact.
    let current_scopes =
        load_current_search_scopes(state, hits.iter().map(|hit| hit.document_id)).await?;
    retain_current_search_hits(&mut hits, &current_scopes, &all_prepared_evidence_scopes);
    hits.sort_by(search_document_hit_order);
    hits.truncate(limit);

    if !include_references {
        clear_search_reference_evidence(&mut hits);
    }

    Ok(McpSearchDocumentsResponse {
        query: query.to_string(),
        limit,
        libraries: library_refs,
        hits,
    })
}

const fn search_readability_rank(state: &McpReadabilityState) -> u8 {
    match state {
        McpReadabilityState::Readable => 0,
        McpReadabilityState::Processing => 1,
        McpReadabilityState::Failed => 2,
        McpReadabilityState::Unavailable => 3,
    }
}

pub(super) fn search_document_hit_order(
    left: &McpDocumentHit,
    right: &McpDocumentHit,
) -> std::cmp::Ordering {
    search_readability_rank(&left.readability_state)
        .cmp(&search_readability_rank(&right.readability_state))
        .then_with(|| right.score.total_cmp(&left.score))
        .then_with(|| {
            dotted_version_key(&right.document_title).cmp(&dotted_version_key(&left.document_title))
        })
        .then_with(|| left.document_id.cmp(&right.document_id))
}

async fn search_chunks_with_query_variants(
    state: &AppState,
    library_id: Uuid,
    query: &str,
    limit: usize,
) -> Result<Vec<crate::infra::knowledge_rows::KnowledgeChunkSearchRow>, ApiError> {
    let variants = chunk_search_query_variants(query);
    let mut rows_by_chunk = std::collections::HashMap::<
        Uuid,
        crate::infra::knowledge_rows::KnowledgeChunkSearchRow,
    >::new();

    for (variant_index, variant) in variants.iter().enumerate() {
        let mut rows = state
            .search_store
            .search_chunks(ChunkLexicalQuery {
                library_id,
                query: variant,
                query_origin: LexicalQueryOrigin::DerivedProbe,
                limit,
                temporal_start: None,
                temporal_end: None,
                text_search_config: None,
            })
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        if variant_index > 0 {
            for row in &mut rows {
                row.score *= 0.82;
            }
        }
        for row in rows {
            match rows_by_chunk.get_mut(&row.chunk_id) {
                Some(existing) if row.score > existing.score => {
                    *existing = row;
                }
                Some(_) => {}
                None => {
                    rows_by_chunk.insert(row.chunk_id, row);
                }
            }
        }
        if rows_by_chunk.len() >= limit && variant_index == 0 {
            break;
        }
    }

    let mut rows = rows_by_chunk.into_values().collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right.score.total_cmp(&left.score).then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    rows.truncate(limit);
    Ok(rows)
}

pub(super) fn chunk_search_query_variants(query: &str) -> Vec<String> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    vec![trimmed.to_string()]
}

#[cfg(test)]
mod tests {
    use super::{
        CurrentSearchScopeRow, MCP_SEARCH_REVALIDATION_HEADROOM_MULTIPLIER, SearchEvidenceScope,
        SearchExcerptContext, SearchLaneMaterials, accumulate_lexical_search_hits,
        accumulate_metadata_search_hits, accumulator_accepts_scope,
        dense_document_ranks_by_best_hit, merge_bounded_search_reservoir,
        retain_current_search_hits, search_revalidation_candidate_cap,
    };
    use crate::infra::{
        knowledge_rows::{
            KnowledgeChunkRow, KnowledgeChunkSearchRow, KnowledgeDocumentRow, KnowledgeRevisionRow,
        },
        repositories::content_metadata_search_repository::ContentDocumentMetadataSearchRow,
    };
    use crate::mcp_types::{McpChunkReference, McpDocumentHit, McpReadabilityState};
    use chrono::Utc;
    use std::collections::{HashMap, HashSet};
    use uuid::Uuid;

    fn operational_hit() -> McpDocumentHit {
        McpDocumentHit {
            document_id: Uuid::from_u128(3),
            workspace_id: Uuid::from_u128(1),
            library_id: Uuid::from_u128(2),
            document_title: "beacon".to_string(),
            latest_revision_id: None,
            score: 1.0,
            excerpt: Some("beacon".to_string()),
            excerpt_start_offset: Some(0),
            excerpt_end_offset: Some(6),
            suggested_start_offset: None,
            readability_state: McpReadabilityState::Unavailable,
            readiness_kind: "empty".to_string(),
            graph_coverage_kind: "empty".to_string(),
            status_reason: Some("document has no readable revision yet".to_string()),
            chunk_references: Vec::new(),
            technical_fact_references: Vec::new(),
            entity_references: Vec::new(),
            relation_references: Vec::new(),
            evidence_references: Vec::new(),
        }
    }

    fn current_operational_scope(hit: &McpDocumentHit) -> HashMap<Uuid, CurrentSearchScopeRow> {
        HashMap::from([(
            hit.document_id,
            CurrentSearchScopeRow {
                document_id: hit.document_id,
                workspace_id: hit.workspace_id,
                library_id: hit.library_id,
                readable_revision_id: None,
                is_prepared: false,
                external_key: hit.document_title.clone(),
            },
        )])
    }

    #[test]
    fn operational_metadata_is_retained_only_while_scope_and_null_publication_agree() {
        let hit = operational_hit();
        let mut scopes = current_operational_scope(&hit);
        let mut hits = vec![hit.clone()];
        retain_current_search_hits(&mut hits, &scopes, &HashSet::new());
        assert_eq!(hits.len(), 1);
        if let Some(scope) = scopes.get_mut(&hit.document_id) {
            scope.readable_revision_id = Some(Uuid::from_u128(4));
        }
        retain_current_search_hits(&mut hits, &scopes, &HashSet::new());
        assert!(hits.is_empty(), "None-to-Some publication must withdraw stale metadata");
        let mut scopes = current_operational_scope(&hit);
        if let Some(scope) = scopes.get_mut(&hit.document_id) {
            scope.library_id = Uuid::from_u128(99);
        }
        let mut hits = vec![hit.clone()];
        retain_current_search_hits(&mut hits, &scopes, &HashSet::new());
        assert!(hits.is_empty(), "scope reassignment must withdraw metadata");
        let mut hits = vec![hit];
        retain_current_search_hits(&mut hits, &HashMap::new(), &HashSet::new());
        assert!(hits.is_empty(), "operational deletion must withdraw metadata");
    }

    #[test]
    fn null_publication_rejects_content_readability_references_and_non_key_excerpt() {
        let hit = operational_hit();
        let scopes = current_operational_scope(&hit);
        let mut readable = hit.clone();
        readable.readability_state = McpReadabilityState::Readable;
        let mut body = hit.clone();
        body.excerpt = Some("hidden candidate body".to_string());
        let mut references = hit.clone();
        references.chunk_references.push(McpChunkReference {
            chunk_id: Uuid::from_u128(4),
            rank: 1,
            score: 1.0,
            inclusion_reason: None,
        });
        let mut hits = vec![readable, body, references];
        retain_current_search_hits(&mut hits, &scopes, &HashSet::new());
        assert!(hits.is_empty());
        let mut failed = hit;
        failed.readability_state = McpReadabilityState::Failed;
        failed.readiness_kind = "failed".to_string();
        let mut hits = vec![failed];
        retain_current_search_hits(&mut hits, &scopes, &HashSet::new());
        assert_eq!(hits.len(), 1, "failure metadata remains an operational member");
    }

    #[test]
    fn operational_metadata_never_accepts_a_published_body_scope() {
        let hit = ContentDocumentMetadataSearchRow {
            document_id: Uuid::from_u128(3),
            workspace_id: Uuid::from_u128(1),
            library_id: Uuid::from_u128(2),
            external_key: "beacon".to_string(),
            readable_revision_id: None,
            revision_title: None,
            metadata_score: 1.0,
            matched_text: "beacon".to_string(),
        };
        let mut accumulators = HashMap::new();
        accumulate_metadata_search_hits(&mut accumulators, &[hit], "beacon");
        assert!(!accumulator_accepts_scope(
            &accumulators,
            SearchEvidenceScope::new(
                Uuid::from_u128(1),
                Uuid::from_u128(2),
                Uuid::from_u128(3),
                Some(Uuid::from_u128(4)),
            )
        ));
        assert!(accumulators[&Uuid::from_u128(3)].chunk_references.is_empty());
    }

    #[test]
    fn chunk_lane_assigns_dense_document_ranks_from_each_documents_best_hit() {
        let document_a = Uuid::from_u128(1);
        let document_b = Uuid::from_u128(2);

        let ranks = dense_document_ranks_by_best_hit([
            (document_a, 0.99),
            (document_a, 0.98),
            (document_a, 0.97),
            (document_b, 0.96),
        ]);

        assert_eq!(ranks.get(&document_a), Some(&1));
        assert_eq!(ranks.get(&document_b), Some(&2));
    }

    #[test]
    fn equal_best_chunk_scores_use_document_id_as_stable_tie_break() {
        let lower_document_id = Uuid::from_u128(10);
        let higher_document_id = Uuid::from_u128(20);

        let ranks =
            dense_document_ranks_by_best_hit([(higher_document_id, 0.8), (lower_document_id, 0.8)]);

        assert_eq!(ranks.get(&lower_document_id), Some(&1));
        assert_eq!(ranks.get(&higher_document_id), Some(&2));
    }

    #[test]
    fn metadata_revision_never_absorbs_stale_body_evidence_from_another_revision() {
        let workspace_id = Uuid::from_u128(1);
        let library_id = Uuid::from_u128(2);
        let document_id = Uuid::from_u128(3);
        let stale_revision_id = Uuid::from_u128(4);
        let metadata_revision_id = Uuid::from_u128(5);
        let stale_chunk_id = Uuid::from_u128(6);
        let query = "stable-metadata-anchor";
        let metadata_hit = ContentDocumentMetadataSearchRow {
            document_id,
            workspace_id,
            library_id,
            external_key: query.to_string(),
            readable_revision_id: Some(metadata_revision_id),
            revision_title: Some(query.to_string()),
            metadata_score: 1_400.0,
            matched_text: query.to_string(),
        };
        let mut accumulators = HashMap::new();
        accumulate_metadata_search_hits(&mut accumulators, &[metadata_hit], query);
        let metadata_score = accumulators[&document_id].fused_score();
        let metadata_excerpt = accumulators[&document_id].excerpt.clone();

        let now = Utc::now();
        let stale_document = KnowledgeDocumentRow {
            document_id,
            workspace_id,
            library_id,
            external_key: query.to_string(),
            file_name: None,
            title: Some("retired title".to_string()),
            source_uri: None,
            document_hint: None,
            document_state: "active".to_string(),
            active_revision_id: Some(stale_revision_id),
            readable_revision_id: Some(stale_revision_id),
            latest_revision_no: Some(1),
            parent_document_id: None,
            document_role: crate::domains::content::DOCUMENT_ROLE_PRIMARY.to_string(),
            created_at: now,
            updated_at: now,
            deleted_at: None,
        };
        let stale_revision = KnowledgeRevisionRow {
            revision_id: stale_revision_id,
            workspace_id,
            library_id,
            document_id,
            revision_number: 1,
            revision_state: "active".to_string(),
            revision_kind: "upload".to_string(),
            storage_ref: None,
            source_uri: None,
            document_hint: None,
            mime_type: "text/plain".to_string(),
            checksum: "stale-checksum".to_string(),
            title: Some("retired title".to_string()),
            byte_size: 64,
            normalized_text: Some(format!("{query} retired-body-only-marker")),
            text_checksum: Some("stale-text-checksum".to_string()),
            image_checksum: None,
            text_state: "text_readable".to_string(),
            vector_state: "ready".to_string(),
            graph_state: "accepted".to_string(),
            text_readable_at: Some(now),
            vector_ready_at: Some(now),
            graph_ready_at: None,
            superseded_by_revision_id: None,
            created_at: now,
        };
        let stale_chunk = KnowledgeChunkRow {
            chunk_id: stale_chunk_id,
            workspace_id,
            library_id,
            document_id,
            revision_id: stale_revision_id,
            chunk_index: 0,
            chunk_kind: Some("paragraph".to_string()),
            content_text: format!("{query} retired-body-only-marker"),
            normalized_text: format!("{query} retired-body-only-marker"),
            span_start: Some(0),
            span_end: Some(46),
            token_count: Some(2),
            support_block_ids: Vec::new(),
            section_path: Vec::new(),
            heading_trail: Vec::new(),
            literal_digest: None,
            chunk_state: "ready".to_string(),
            text_generation: Some(1),
            vector_generation: Some(1),
            quality_score: Some(1.0),
            window_text: None,
            occurred_at: None,
            occurred_until: None,
        };
        let stale_hit = KnowledgeChunkSearchRow {
            chunk_id: stale_chunk_id,
            workspace_id,
            library_id,
            revision_id: stale_revision_id,
            content_text: stale_chunk.content_text.clone(),
            normalized_text: stale_chunk.normalized_text.clone(),
            section_path: Vec::new(),
            heading_trail: Vec::new(),
            score: 100.0,
            exact_identifier_rarity: 0.0,
            exact_identifier_terms: Vec::new(),
            rare_exact_identifier_terms: Vec::new(),
            quality_score: Some(1.0),
        };
        let mut prepared_evidence_scopes = HashSet::new();
        let chunk_map = HashMap::from([(stale_chunk_id, stale_chunk)]);
        let document_map = HashMap::from([(document_id, stale_document)]);
        let revision_map = HashMap::from([(stale_revision_id, stale_revision)]);
        accumulate_lexical_search_hits(
            &mut accumulators,
            &mut prepared_evidence_scopes,
            &[stale_hit],
            SearchLaneMaterials {
                chunk_map: &chunk_map,
                document_map: &document_map,
                revision_map: &revision_map,
            },
            SearchExcerptContext { query, read_window_chars: 512 },
        );

        let accumulator = &accumulators[&document_id];
        assert_eq!(accumulator.readable_revision_id, Some(metadata_revision_id));
        assert_eq!(accumulator.fused_score(), metadata_score);
        assert_eq!(accumulator.excerpt, metadata_excerpt);
        assert!(accumulator.chunk_references.is_empty());
        assert!(prepared_evidence_scopes.is_empty());
    }

    #[test]
    fn final_revalidation_headroom_is_clamped_by_the_configured_search_limit() {
        assert_eq!(search_revalidation_candidate_cap(2, 25), 6);
        assert_eq!(search_revalidation_candidate_cap(usize::MAX, 25), 75);
        assert_eq!(
            search_revalidation_candidate_cap(2, 25),
            2 * MCP_SEARCH_REVALIDATION_HEADROOM_MULTIPLIER,
        );
    }

    #[test]
    fn repeated_library_merges_never_grow_the_global_candidate_reservoir() {
        let candidate_cap = search_revalidation_candidate_cap(2, 25);
        let mut reservoir = Vec::new();
        for library in 0..10_000 {
            merge_bounded_search_reservoir(
                &mut reservoir,
                (0..100).map(|rank| library * 100 + rank),
                candidate_cap,
                Ord::cmp,
            );
            assert!(reservoir.len() <= candidate_cap);
        }
        assert_eq!(reservoir, (0..candidate_cap).collect::<Vec<_>>());
    }
}
