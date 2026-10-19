use crate::infra::knowledge_plane::ChunkLexicalQuery;

use super::{ranking::*, *};

pub(super) async fn search_expanded_lexical_chunks(
    state: &AppState,
    library_id: Uuid,
    query_text: &str,
    internal_candidate_limit: usize,
) -> Result<Vec<KnowledgeChunkSearchRow>, ApiError> {
    let search_queries = expand_document_search_queries(query_text);
    let parallelism = SEARCH_LEXICAL_QUERY_PARALLELISM.min(search_queries.len()).max(1);
    let search_results =
        stream::iter(search_queries.into_iter().enumerate().map(|(query_index, search_query)| {
            let state = state.clone();
            let query_origin = if query_index == 0 {
                LexicalQueryOrigin::UserQuery
            } else {
                LexicalQueryOrigin::DerivedProbe
            };
            async move {
                state
                    .search_store
                    .search_chunks(ChunkLexicalQuery {
                        library_id,
                        query: &search_query,
                        query_origin,
                        limit: internal_candidate_limit,
                        temporal_start: None,
                        temporal_end: None,
                        text_search_config: None,
                    })
                    .await
                    .map_err(|e| ApiError::internal_with_log(e, "internal"))
            }
        }))
        .buffer_unordered(parallelism)
        .collect::<Vec<_>>()
        .await;

    let mut lexical_chunk_hit_map = HashMap::<Uuid, KnowledgeChunkSearchRow>::new();
    for result in search_results {
        for row in result? {
            match lexical_chunk_hit_map.entry(row.chunk_id) {
                std::collections::hash_map::Entry::Occupied(mut occupied) => {
                    let exact_identifier_rarity =
                        occupied.get().exact_identifier_rarity.max(row.exact_identifier_rarity);
                    let mut exact_identifier_terms = occupied.get().exact_identifier_terms.clone();
                    extend_unique_identifier_terms(
                        &mut exact_identifier_terms,
                        &row.exact_identifier_terms,
                    );
                    let mut rare_exact_identifier_terms =
                        occupied.get().rare_exact_identifier_terms.clone();
                    extend_unique_identifier_terms(
                        &mut rare_exact_identifier_terms,
                        &row.rare_exact_identifier_terms,
                    );
                    if row.score > occupied.get().score {
                        let mut stronger = row;
                        stronger.exact_identifier_rarity = exact_identifier_rarity;
                        stronger.exact_identifier_terms = exact_identifier_terms;
                        stronger.rare_exact_identifier_terms = rare_exact_identifier_terms;
                        occupied.insert(stronger);
                    } else {
                        occupied.get_mut().exact_identifier_rarity = exact_identifier_rarity;
                        occupied.get_mut().exact_identifier_terms = exact_identifier_terms;
                        occupied.get_mut().rare_exact_identifier_terms =
                            rare_exact_identifier_terms;
                    }
                }
                std::collections::hash_map::Entry::Vacant(vacant) => {
                    vacant.insert(row);
                }
            }
        }
    }

    let mut lexical_chunk_hits = lexical_chunk_hit_map.into_values().collect::<Vec<_>>();
    lexical_chunk_hits.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    Ok(lexical_chunk_hits)
}

pub(super) async fn load_evidence_samples_by_chunk_ids(
    state: &AppState,
    document_id: Uuid,
    chunk_ids: &[Uuid],
) -> Result<HashMap<Uuid, Vec<KnowledgeEvidenceRow>>, ApiError> {
    if chunk_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let parallelism = SEARCH_CHUNK_EVIDENCE_PARALLELISM.min(chunk_ids.len()).max(1);
    let evidence_results = stream::iter(chunk_ids.iter().copied().map(|chunk_id| {
        let state = state.clone();
        async move {
            let evidence_rows = state
                .graph_store
                .list_evidence_by_chunk(chunk_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
            Ok::<(Uuid, Vec<KnowledgeEvidenceRow>), ApiError>((chunk_id, evidence_rows))
        }
    }))
    .buffer_unordered(parallelism)
    .collect::<Vec<_>>()
    .await;

    let mut evidence_by_chunk: HashMap<Uuid, Vec<KnowledgeEvidenceRow>> = HashMap::new();
    for result in evidence_results {
        let (chunk_id, evidence_rows) = result?;
        let rows = evidence_rows
            .into_iter()
            .filter(|evidence| evidence.document_id == document_id)
            .collect::<Vec<_>>();
        if !rows.is_empty() {
            evidence_by_chunk.insert(chunk_id, rows);
        }
    }
    Ok(evidence_by_chunk)
}

pub(super) async fn enrich_document_search_hit(
    state: AppState,
    query_text: String,
    index: usize,
    mut accumulator: KnowledgeDocumentAccumulator,
) -> Result<(usize, KnowledgeSearchDocumentHit), ApiError> {
    focus_document_chunk_hits_for_query(&query_text, &mut accumulator.chunk_hits);
    let technical_fact_samples = state
        .application_services
        .knowledge
        .list_typed_technical_facts(&state, accumulator.revision.revision_id)
        .await?;
    Ok((
        index,
        KnowledgeSearchDocumentHit {
            provenance_summary: KnowledgeDocumentProvenanceSummary {
                supporting_evidence_count: accumulator.evidence_samples.len(),
                lexical_chunk_count: accumulator.chunk_hits.len(),
                vector_chunk_count: accumulator.vector_chunk_hits.len(),
            },
            technical_fact_summary: summarize_typed_technical_facts(&technical_fact_samples),
            graph_evidence_summary: summarize_graph_evidence(&accumulator.evidence_samples),
            document: accumulator.document.into(),
            revision: map_search_revision_summary(accumulator.revision),
            score: accumulator.score,
            lexical_rank: accumulator.lexical_rank,
            vector_rank: accumulator.vector_rank,
            lexical_score: accumulator.lexical_score,
            vector_score: accumulator.vector_score,
            chunk_hits: accumulator.chunk_hits.into_iter().map(Into::into).collect(),
            vector_chunk_hits: accumulator.vector_chunk_hits.into_iter().map(Into::into).collect(),
            evidence_samples: accumulator.evidence_samples.into_iter().map(Into::into).collect(),
            technical_fact_samples,
        },
    ))
}

pub(super) fn focus_document_chunk_hits_for_query(
    query_text: &str,
    chunk_hits: &mut [KnowledgeChunkSearchRow],
) {
    let focus_terms = document_search_focus_terms(query_text);
    if focus_terms.is_empty() {
        return;
    }
    for hit in chunk_hits {
        let haystack = format!("{}\n{}", hit.content_text, hit.normalized_text).to_lowercase();
        if !focus_terms.iter().any(|term| haystack.contains(term)) {
            continue;
        }
        let focused_content = focused_search_hit_text(&hit.content_text, &focus_terms, 1_600);
        if !focused_content.trim().is_empty() {
            hit.content_text = focused_content;
        }
        let focused_normalized = focused_search_hit_text(&hit.normalized_text, &focus_terms, 1_600);
        if !focused_normalized.trim().is_empty() {
            hit.normalized_text = focused_normalized;
        }
    }
}

pub(super) fn document_search_focus_terms(query_text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    document_search_exact_anchor_surfaces(query_text)
        .into_iter()
        .chain(document_search_anchor_tokens(query_text))
        .chain(document_search_anchor_phrases(query_text))
        .chain(document_search_keywords(query_text))
        .map(|term| term.trim().to_lowercase())
        .filter(|term| term.chars().count() >= 2)
        .filter(|term| seen.insert(term.clone()))
        .collect()
}

pub(super) fn focused_search_hit_text(
    text: &str,
    focus_terms: &[String],
    max_chars: usize,
) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() || max_chars == 0 {
        return String::new();
    }
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let lowered = trimmed.to_lowercase();
    let best_match = focus_terms
        .iter()
        .filter_map(|term| {
            let term = term.trim().to_lowercase();
            if term.chars().count() < 2 {
                return None;
            }
            lowered
                .find(term.as_str())
                .map(|byte_index| (document_search_focus_term_score(&term), byte_index))
        })
        .max_by(|left, right| left.0.cmp(&right.0).then_with(|| right.1.cmp(&left.1)));

    let Some((_, match_byte_index)) = best_match else {
        return crate::services::query::execution::focused_excerpt_for(
            trimmed,
            focus_terms,
            max_chars,
        );
    };
    centered_char_window(trimmed, match_byte_index, max_chars)
}

pub(super) fn document_search_focus_term_score(term: &str) -> usize {
    let has_structural_separator =
        term.chars().any(|ch| matches!(ch, '_' | '-' | '.' | '/' | ':' | '@'));
    let has_digit = term.chars().any(|ch| ch.is_ascii_digit());
    let has_uppercase_shape = term.chars().any(|ch| ch.is_ascii_uppercase());
    let token_count = term.split_whitespace().count();
    usize::from(has_structural_separator) * 1_000
        + usize::from(has_digit) * 700
        + usize::from(has_uppercase_shape) * 500
        + token_count.saturating_sub(1) * 100
        + term.chars().count().min(80)
}

pub(super) fn centered_char_window(
    text: &str,
    center_byte_index: usize,
    max_chars: usize,
) -> String {
    let char_positions = text.char_indices().map(|(index, _)| index).collect::<Vec<_>>();
    if char_positions.len() <= max_chars {
        return text.to_string();
    }
    let center_char_index = char_positions
        .binary_search(&center_byte_index)
        .unwrap_or_else(|index| index.saturating_sub(1));
    let half_window = max_chars / 2;
    let start_char = center_char_index.saturating_sub(half_window);
    let end_char = (start_char + max_chars).min(char_positions.len());
    let start_byte = char_positions[start_char];
    let end_byte = char_positions.get(end_char).copied().unwrap_or(text.len());
    let prefix = if start_char > 0 { "... " } else { "" };
    let suffix = if end_char < char_positions.len() { " ..." } else { "" };
    format!("{prefix}{}{suffix}", text[start_byte..end_byte].trim())
}

pub(super) fn resolved_chunk_hit(
    chunk_id: &Uuid,
    chunk_map: &HashMap<Uuid, KnowledgeChunkRow>,
    revision_map: &HashMap<Uuid, KnowledgeRevisionRow>,
    document_map: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> bool {
    let Some(chunk) = chunk_map.get(chunk_id) else {
        return false;
    };
    revision_map.contains_key(&chunk.revision_id) && document_map.contains_key(&chunk.document_id)
}

pub(super) fn map_search_revision_summary(
    revision: KnowledgeRevisionRow,
) -> KnowledgeSearchRevisionSummary {
    KnowledgeSearchRevisionSummary {
        revision_id: revision.revision_id,
        document_id: revision.document_id,
        revision_number: revision.revision_number,
        revision_state: revision.revision_state,
        revision_kind: revision.revision_kind,
        mime_type: revision.mime_type,
        title: revision.title,
        byte_size: revision.byte_size,
        text_state: revision.text_state,
        vector_state: revision.vector_state,
        graph_state: revision.graph_state,
        created_at: revision.created_at,
    }
}

pub(super) async fn prepare_document_search_candidate(
    state: AppState,
    query_text: String,
    query_keywords: Vec<String>,
    chunk_hit_limit_per_document: usize,
    evidence_sample_limit: usize,
    mut accumulator: KnowledgeDocumentAccumulator,
) -> Result<KnowledgeDocumentAccumulator, ApiError> {
    backfill_document_chunk_hits(
        &state,
        &query_text,
        chunk_hit_limit_per_document,
        &mut accumulator,
    )
    .await?;

    truncate_document_candidate_chunk_hits(&mut accumulator, chunk_hit_limit_per_document);
    let deduped_chunk_ids = document_candidate_chunk_ids(&accumulator);
    append_document_evidence_samples(
        &state,
        evidence_sample_limit,
        &deduped_chunk_ids,
        &mut accumulator,
    )
    .await?;

    rescore_document_accumulator(&mut accumulator, &query_keywords, &query_text);
    Ok(accumulator)
}

pub(super) fn truncate_document_candidate_chunk_hits(
    accumulator: &mut KnowledgeDocumentAccumulator,
    chunk_hit_limit_per_document: usize,
) {
    accumulator.chunk_hits.truncate(chunk_hit_limit_per_document);
    accumulator.vector_chunk_hits.truncate(chunk_hit_limit_per_document);
}

pub(super) fn document_candidate_chunk_ids(
    accumulator: &KnowledgeDocumentAccumulator,
) -> Vec<Uuid> {
    let mut seen_chunk_ids = HashSet::new();
    accumulator
        .chunk_hits
        .iter()
        .map(|hit| hit.chunk_id)
        .chain(accumulator.vector_chunk_hits.iter().map(|hit| hit.chunk_id))
        .filter(|chunk_id| seen_chunk_ids.insert(*chunk_id))
        .collect()
}

pub(super) async fn append_document_evidence_samples(
    state: &AppState,
    evidence_sample_limit: usize,
    chunk_ids: &[Uuid],
    accumulator: &mut KnowledgeDocumentAccumulator,
) -> Result<(), ApiError> {
    if evidence_sample_limit == 0 {
        return Ok(());
    }

    let evidence_by_chunk =
        load_evidence_samples_by_chunk_ids(state, accumulator.document.document_id, chunk_ids)
            .await?;
    for chunk_id in chunk_ids {
        let Some(evidence_rows) = evidence_by_chunk.get(chunk_id) else {
            continue;
        };
        append_evidence_rows(evidence_rows, evidence_sample_limit, accumulator);
        if accumulator.evidence_samples.len() >= evidence_sample_limit {
            break;
        }
    }
    Ok(())
}

pub(super) fn append_evidence_rows(
    evidence_rows: &[KnowledgeEvidenceRow],
    evidence_sample_limit: usize,
    accumulator: &mut KnowledgeDocumentAccumulator,
) {
    for evidence in evidence_rows {
        if accumulator.evidence_ids.insert(evidence.evidence_id) {
            accumulator.evidence_samples.push(evidence.clone());
        }
        if accumulator.evidence_samples.len() >= evidence_sample_limit {
            break;
        }
    }
}

pub(super) async fn backfill_document_chunk_hits(
    state: &AppState,
    query_text: &str,
    chunk_hit_limit_per_document: usize,
    accumulator: &mut KnowledgeDocumentAccumulator,
) -> Result<(), ApiError> {
    let keywords = crate::services::query::planner::extract_keywords(query_text);
    if keywords.is_empty() {
        return Ok(());
    }

    let existing_chunk_ids =
        accumulator.chunk_hits.iter().map(|chunk| chunk.chunk_id).collect::<HashSet<_>>();
    let mut candidates = accumulator.chunk_hits.clone();
    let mut backfill_terms = keywords.clone();
    backfill_terms.extend(document_search_anchor_tokens(query_text));
    backfill_terms.extend(document_search_anchor_phrases(query_text));
    candidates.extend(
        state
            .document_store
            .list_chunks_by_revision_matching_terms(
                accumulator.revision.revision_id,
                &backfill_terms,
                chunk_hit_limit_per_document.saturating_mul(4).max(chunk_hit_limit_per_document),
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .into_iter()
            .filter(|chunk| !existing_chunk_ids.contains(&chunk.chunk_id))
            .filter_map(|chunk| {
                let haystack =
                    format!("{} {}", chunk.content_text, chunk.normalized_text).to_lowercase();
                let score = keywords
                    .iter()
                    .map(|keyword| haystack.matches(keyword.as_str()).count() as f64)
                    .sum::<f64>();
                (score > 0.0).then_some(KnowledgeChunkSearchRow {
                    chunk_id: chunk.chunk_id,
                    workspace_id: chunk.workspace_id,
                    library_id: chunk.library_id,
                    revision_id: chunk.revision_id,
                    content_text: repair_technical_layout_noise(&chunk.content_text),
                    normalized_text: repair_technical_layout_noise(&chunk.normalized_text),
                    section_path: chunk.section_path,
                    heading_trail: chunk.heading_trail,
                    score,
                    exact_identifier_rarity: 0.0,
                    exact_identifier_terms: Vec::new(),
                    rare_exact_identifier_terms: Vec::new(),
                    quality_score: chunk.quality_score,
                })
            }),
    );
    candidates.sort_by(|left, right| {
        document_search_chunk_relevance(query_text, right)
            .cmp(&document_search_chunk_relevance(query_text, left))
            .then_with(|| right.score.partial_cmp(&left.score).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    candidates.dedup_by(|left, right| left.chunk_id == right.chunk_id);
    accumulator.chunk_hits = candidates.into_iter().take(chunk_hit_limit_per_document).collect();
    Ok(())
}
