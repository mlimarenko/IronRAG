use super::*;

const GRAPH_EXTRACTION_MIN_CHUNK_QUALITY_SCORE: f32 = 0.35;
const CHUNK_KIND_SOURCE_PROFILE: &str = "source_profile";
const CHUNK_KIND_SOURCE_UNIT: &str = "source_unit";

#[derive(Debug, Clone, Default)]
pub(in super::super) struct GraphExtractionChunkPolicy {
    record_stream: bool,
    selected_record_stream_source_units: BTreeSet<Uuid>,
}

impl GraphExtractionChunkPolicy {
    #[must_use]
    pub(in super::super) fn standard() -> Self {
        Self::default()
    }

    #[must_use]
    pub(in super::super) const fn record_stream(selected_source_units: BTreeSet<Uuid>) -> Self {
        Self { record_stream: true, selected_record_stream_source_units: selected_source_units }
    }

    #[must_use]
    pub(in super::super) const fn is_record_stream(&self) -> bool {
        self.record_stream
    }

    #[must_use]
    pub(in super::super) fn selected_source_unit_count(&self) -> usize {
        self.selected_record_stream_source_units.len()
    }

    #[must_use]
    fn admits_source_unit(&self, chunk_id: Uuid) -> bool {
        !self.record_stream || self.selected_record_stream_source_units.contains(&chunk_id)
    }
}

pub(in super::super) fn build_graph_chunk_content(
    chunk: &KnowledgeChunkRow,
    table_graph_profile: Option<&TableGraphProfile>,
    row_only_table_graph: bool,
    policy: &GraphExtractionChunkPolicy,
) -> Option<String> {
    // The source_profile line is pure machine bookkeeping (source_format token,
    // record/key counters, top_keys, time bounds) — it carries no entities, so
    // it never reaches graph extraction. Search/embeddings keep the stored chunk
    // text; only this graph-fed projection drops it.
    if chunk.chunk_kind.as_deref() == Some(CHUNK_KIND_SOURCE_PROFILE) {
        return None;
    }
    if chunk.quality_score.is_some_and(|score| score < GRAPH_EXTRACTION_MIN_CHUNK_QUALITY_SCORE) {
        return None;
    }
    if row_only_table_graph && chunk.chunk_kind.as_deref() != Some("table_row") {
        return None;
    }
    // Admitted source_unit chunks feed a values-only projection: the
    // [unit_ordinal=N occurred_at=ISO] header, the dotted-path field KEYS, and
    // timestamp VALUES are stripped so the graph anchors on real record values
    // instead of the renderer's scaffolding. The full field-aware text stays in
    // the stored chunk for search/embeddings.
    if chunk.chunk_kind.as_deref() == Some(CHUNK_KIND_SOURCE_UNIT) {
        if !policy.admits_source_unit(chunk.chunk_id) {
            return None;
        }
        return project_record_unit_values_for_graph(&chunk.normalized_text);
    }
    if chunk.chunk_kind.as_deref() == Some("metadata_block")
        && is_table_summary_text(&chunk.normalized_text)
    {
        return None;
    }
    if !is_graph_extraction_text_eligible(&chunk.normalized_text) {
        return None;
    }
    if chunk.chunk_kind.as_deref() == Some("table_row") {
        return build_graph_table_row_text(&chunk.normalized_text, table_graph_profile);
    }

    Some(chunk.normalized_text.clone())
}

/// Inputs for [`build_graph_extraction_request`]. Grouped into one cohesive
/// value so the eight-argument request builder reads as a named domain input
/// rather than a positional bag.
pub(in super::super) struct GraphExtractionRequestInput<'a> {
    pub(in super::super) document: &'a KnowledgeDocumentRow,
    pub(in super::super) revision: &'a KnowledgeRevisionRow,
    pub(in super::super) chunk: &'a KnowledgeChunkRow,
    pub(in super::super) chunk_content: String,
    pub(in super::super) technical_facts: &'a [TypedTechnicalFact],
    pub(in super::super) attempt_id: Option<Uuid>,
    pub(in super::super) library_extraction_prompt: Option<String>,
    pub(in super::super) sub_type_hints: GraphExtractionSubTypeHints,
}

pub(in super::super) fn build_graph_extraction_request(
    input: GraphExtractionRequestInput<'_>,
) -> GraphExtractionRequest {
    let GraphExtractionRequestInput {
        document,
        revision,
        chunk,
        chunk_content,
        technical_facts,
        attempt_id,
        library_extraction_prompt,
        sub_type_hints,
    } = input;
    let graph_technical_facts = if chunk.chunk_kind.as_deref() == Some("table_row") {
        Vec::new()
    } else {
        technical_facts
            .iter()
            .map(|fact| GraphExtractionTechnicalFact {
                fact_kind: fact.fact_kind.as_str().to_string(),
                normalized_value: fact.normalized_value.normalized_string(),
                display_value: fact.display_value.clone(),
                qualifiers: fact.qualifiers.clone(),
            })
            .collect()
    };

    GraphExtractionRequest {
        library_id: revision.library_id,
        document: repositories::DocumentRow {
            id: document.document_id,
            library_id: document.library_id,
            source_id: None,
            external_key: document.external_key.clone(),
            title: document.title.clone(),
            mime_type: Some(revision.mime_type.clone()),
            checksum: Some(revision.checksum.clone()),
            active_revision_id: Some(revision.revision_id),
            document_state: document.document_state.clone(),
            mutation_kind: None,
            mutation_status: None,
            deleted_at: document.deleted_at,
            created_at: document.created_at,
            updated_at: document.updated_at,
        },
        chunk: repositories::ChunkRow {
            id: chunk.chunk_id,
            document_id: chunk.document_id,
            library_id: chunk.library_id,
            ordinal: chunk.chunk_index,
            content: chunk_content,
            token_count: chunk.token_count,
            metadata_json: serde_json::json!({
                "chunk_kind": chunk.chunk_kind,
                "support_block_ids": chunk.support_block_ids,
                "section_path": chunk.section_path,
                "heading_trail": chunk.heading_trail,
                "literal_digest": chunk.literal_digest,
                "chunk_state": chunk.chunk_state,
                "text_generation": chunk.text_generation,
                "vector_generation": chunk.vector_generation,
            }),
            created_at: revision.created_at,
        },
        structured_chunk: GraphExtractionStructuredChunkContext {
            chunk_kind: chunk.chunk_kind.clone(),
            section_path: chunk.section_path.clone(),
            heading_trail: chunk.heading_trail.clone(),
            support_block_ids: chunk.support_block_ids.clone(),
            literal_digest: chunk.literal_digest.clone(),
        },
        technical_facts: graph_technical_facts,
        revision_id: Some(revision.revision_id),
        activated_by_attempt_id: attempt_id,
        resume_hint: None,
        library_extraction_prompt,
        sub_type_hints,
    }
}

pub(in super::super) fn typed_fact_supports_chunk(
    fact: &TypedTechnicalFact,
    chunk: &KnowledgeChunkRow,
) -> bool {
    fact.support_chunk_ids.contains(&chunk.chunk_id)
        || fact.support_block_ids.iter().any(|block_id| chunk.support_block_ids.contains(block_id))
}
