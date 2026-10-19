use crate::infra::knowledge_plane::ChunkLexicalQuery;

use super::{
    AppState, Context, HashMap, HashSet, KnowledgeChunkRow, KnowledgeDocumentRow,
    LexicalQueryOrigin, LiteralKind, QueryAct, QueryIR, QueryScope, QueryTargetKind,
    QuestionIntent, RuntimeMatchedChunk, Uuid, classify_query_ir_intents,
    detect_technical_literal_intent_from_query_ir, has_question_intent,
    is_source_profile_chunk_row, question_asks_table_aggregation,
    rendering::{map_companion_chunk, structured_inventory_source_profile_score},
    score_value,
    selection::select_fallback_structured_search_rows,
    types::{
        SOURCE_CONTEXT_ANCHOR_LIMIT_PER_DOCUMENT, SOURCE_CONTEXT_CODE_PATTERN_HIT_LIMIT,
        SOURCE_CONTEXT_CODE_PATTERN_SCORE_BONUS, SOURCE_CONTEXT_CODE_PATTERN_TERM_LIMIT,
        SOURCE_CONTEXT_DEFAULT_NEIGHBOR_FORWARD, SOURCE_CONTEXT_DOCUMENT_LIMIT,
        SOURCE_CONTEXT_FALLBACK_STRUCTURED_SEARCH_COMPANION_LIMIT,
        SOURCE_CONTEXT_FALLBACK_STRUCTURED_SEARCH_HIT_LIMIT_PER_TERM,
        SOURCE_CONTEXT_FALLBACK_STRUCTURED_SEARCH_SCORE_BONUS,
        SOURCE_CONTEXT_FALLBACK_STRUCTURED_SEARCH_TERM_LIMIT,
        SOURCE_CONTEXT_FALLBACK_STRUCTURED_TOP_K_FLOOR,
        SOURCE_CONTEXT_FOCUSED_MATCH_LIMIT_PER_DOCUMENT,
        SOURCE_CONTEXT_INVENTORY_PROFILE_HIT_LIMIT_PER_TERM,
        SOURCE_CONTEXT_INVENTORY_PROFILE_LIMIT, SOURCE_CONTEXT_INVENTORY_PROFILE_TERM_LIMIT,
        SOURCE_CONTEXT_PROCEDURAL_NEIGHBOR_BACKWARD,
        SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_LIMIT_PER_DOCUMENT, SOURCE_CONTEXT_PROFILE_HEADROOM,
        SOURCE_CONTEXT_TRANSPORT_PATTERN_HIT_LIMIT, SOURCE_CONTEXT_TRANSPORT_PATTERN_SCORE_BONUS,
        SOURCE_CONTEXT_TRANSPORT_PATTERN_TERM_LIMIT, StructuredSourceCompanion,
        StructuredSourceCompanionKind,
    },
};

/// Common inputs for the source-context loaders in this module.
pub(super) struct SourceContextLoadScope<'a> {
    pub(super) state: &'a AppState,
    pub(super) library_id: Uuid,
    pub(super) document_index: &'a HashMap<Uuid, KnowledgeDocumentRow>,
    pub(super) focus_keywords: &'a [String],
}

/// Time and focus bounds of the fallback structured search.
pub(super) struct FallbackStructuredSearchInput {
    pub(super) temporal_start: Option<chrono::DateTime<chrono::Utc>>,
    pub(super) temporal_end: Option<chrono::DateTime<chrono::Utc>>,
    pub(super) include_long_focus_terms: bool,
}

pub(super) async fn load_fallback_structured_search_source_context(
    scope: SourceContextLoadScope<'_>,
    input: FallbackStructuredSearchInput,
    chunks: &[RuntimeMatchedChunk],
    companions: &[StructuredSourceCompanion],
) -> anyhow::Result<Vec<StructuredSourceCompanion>> {
    let SourceContextLoadScope { state, library_id, document_index, focus_keywords } = scope;
    let FallbackStructuredSearchInput { temporal_start, temporal_end, include_long_focus_terms } =
        input;
    let search_terms = fallback_structured_search_terms(focus_keywords, include_long_focus_terms);
    if search_terms.is_empty() {
        return Ok(Vec::new());
    }

    let mut score_by_chunk = HashMap::<Uuid, f32>::new();
    for term in search_terms {
        let rows = state
            .search_store
            .search_chunks(ChunkLexicalQuery {
                library_id,
                query: &term,
                query_origin: LexicalQueryOrigin::DerivedProbe,
                limit: SOURCE_CONTEXT_FALLBACK_STRUCTURED_SEARCH_HIT_LIMIT_PER_TERM,
                temporal_start,
                temporal_end,
                text_search_config: None,
            })
            .await
            .with_context(|| {
                format!("failed to run fallback structured source context search for {term}")
            })?;
        for row in rows {
            score_by_chunk
                .entry(row.chunk_id)
                .and_modify(|existing| {
                    *existing = existing.max(row.score as f32);
                })
                .or_insert(row.score as f32);
        }
    }
    if score_by_chunk.is_empty() {
        return Ok(Vec::new());
    }

    let existing_chunk_ids = chunks
        .iter()
        .map(|chunk| chunk.chunk_id)
        .chain(companions.iter().map(|companion| companion.chunk.chunk_id))
        .collect::<HashSet<_>>();
    let chunk_ids = score_by_chunk.keys().copied().collect::<Vec<_>>();
    let rows = state
        .document_store
        .list_chunks_by_ids(&chunk_ids)
        .await
        .context("failed to hydrate fallback structured source context chunks")?;
    let candidates = rows
        .into_iter()
        .filter_map(|row| {
            let search_score = score_by_chunk.get(&row.chunk_id).copied()?;
            Some((row, search_score))
        })
        .collect::<Vec<_>>();
    let selected = select_fallback_structured_search_rows(
        candidates,
        focus_keywords,
        &existing_chunk_ids,
        SOURCE_CONTEXT_FALLBACK_STRUCTURED_SEARCH_COMPANION_LIMIT,
    );
    if selected.is_empty() {
        return Ok(Vec::new());
    }

    let global_best_score = chunks.iter().map(|chunk| score_value(chunk.score)).fold(0.0, f32::max);
    let selected_rows = selected.iter().map(|(row, _)| row.clone()).collect::<Vec<_>>();
    let mut companion_document_index = document_index.clone();
    hydrate_missing_companion_documents(state, &selected_rows, &mut companion_document_index)
        .await?;
    let mut companions = Vec::with_capacity(selected.len());
    for (rank, (row, structural_score)) in selected.into_iter().enumerate() {
        let score = global_best_score
            + SOURCE_CONTEXT_FALLBACK_STRUCTURED_SEARCH_SCORE_BONUS
            + structural_score as f32 * 0.001
            - rank as f32 * 0.01;
        if let Some(chunk) =
            map_companion_chunk(row, score, &companion_document_index, focus_keywords)
        {
            companions.push(StructuredSourceCompanion {
                chunk,
                kind: StructuredSourceCompanionKind::FocusedMatch,
            });
        }
    }
    Ok(companions)
}

pub(super) fn fallback_structured_search_terms(
    focus_keywords: &[String],
    include_long_focus_terms: bool,
) -> Vec<String> {
    let mut seen = HashSet::new();
    focus_keywords
        .iter()
        .map(|keyword| keyword.trim().to_lowercase())
        .filter(|keyword| {
            let char_count = keyword.chars().count();
            if include_long_focus_terms { char_count >= 3 } else { (2..4).contains(&char_count) }
        })
        .filter(|keyword| {
            keyword.chars().any(|ch| ch.is_alphabetic() || ch.is_ascii_digit())
                && seen.insert(keyword.clone())
        })
        .take(SOURCE_CONTEXT_FALLBACK_STRUCTURED_SEARCH_TERM_LIMIT)
        .collect()
}

pub(super) async fn load_structured_inventory_profile_source_context(
    state: &AppState,
    library_id: Uuid,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    focus_keywords: &[String],
    chunks: &[RuntimeMatchedChunk],
    companions: &[StructuredSourceCompanion],
) -> anyhow::Result<Vec<StructuredSourceCompanion>> {
    let search_terms = focus_keywords
        .iter()
        .filter(|keyword| keyword.chars().count() >= 3)
        .take(SOURCE_CONTEXT_INVENTORY_PROFILE_TERM_LIMIT)
        .cloned()
        .collect::<Vec<_>>();
    if search_terms.is_empty() {
        return Ok(Vec::new());
    }

    let mut revision_scores = HashMap::<Uuid, f32>::new();
    for term in search_terms {
        let rows = state
            .search_store
            .search_chunks(ChunkLexicalQuery {
                library_id,
                query: &term,
                query_origin: LexicalQueryOrigin::DerivedProbe,
                limit: SOURCE_CONTEXT_INVENTORY_PROFILE_HIT_LIMIT_PER_TERM,
                temporal_start: None,
                temporal_end: None,
                text_search_config: None,
            })
            .await
            .with_context(|| {
                format!("failed to search source profiles for structured inventory term {term}")
            })?;
        for row in rows {
            revision_scores
                .entry(row.revision_id)
                .and_modify(|score| *score = score.max(row.score as f32))
                .or_insert(row.score as f32);
        }
    }
    if revision_scores.is_empty() {
        return Ok(Vec::new());
    }

    let mut revision_ids = revision_scores.keys().copied().collect::<Vec<_>>();
    revision_ids.sort_by(|left, right| {
        revision_scores
            .get(right)
            .copied()
            .unwrap_or_default()
            .total_cmp(&revision_scores.get(left).copied().unwrap_or_default())
            .then_with(|| left.cmp(right))
    });
    revision_ids.truncate(SOURCE_CONTEXT_INVENTORY_PROFILE_LIMIT * 2);

    let existing_chunk_ids = chunks
        .iter()
        .map(|chunk| chunk.chunk_id)
        .chain(companions.iter().map(|companion| companion.chunk.chunk_id))
        .collect::<HashSet<_>>();
    let rows = state
        .document_store
        .list_source_profile_chunks_by_revisions(
            library_id,
            &revision_ids,
            SOURCE_CONTEXT_INVENTORY_PROFILE_LIMIT,
        )
        .await
        .context("failed to load structured inventory source profile chunks")?;
    let global_best_score = chunks.iter().map(|chunk| score_value(chunk.score)).fold(0.0, f32::max);
    let mut selected = rows
        .into_iter()
        .filter(|row| !existing_chunk_ids.contains(&row.chunk_id))
        .filter(|row| source_profile_matches_focus(row, focus_keywords))
        .map(|row| {
            let search_score = revision_scores.get(&row.revision_id).copied().unwrap_or_default();
            (row, search_score)
        })
        .collect::<Vec<_>>();
    selected.sort_by(|(left_row, left_score), (right_row, right_score)| {
        right_score
            .total_cmp(left_score)
            .then_with(|| left_row.document_id.cmp(&right_row.document_id))
            .then_with(|| left_row.chunk_id.cmp(&right_row.chunk_id))
    });
    selected.truncate(SOURCE_CONTEXT_INVENTORY_PROFILE_LIMIT);

    let mut companions = Vec::with_capacity(selected.len());
    for (rank, (row, search_score)) in selected.into_iter().enumerate() {
        let score = structured_inventory_source_profile_score(global_best_score, rank)
            + search_score * 0.001;
        if let Some(profile) = map_companion_chunk(row, score, document_index, focus_keywords) {
            companions.push(StructuredSourceCompanion {
                chunk: profile,
                kind: StructuredSourceCompanionKind::SourceProfile,
            });
        }
    }
    Ok(companions)
}

pub(super) fn source_profile_matches_focus(
    row: &KnowledgeChunkRow,
    focus_keywords: &[String],
) -> bool {
    if !is_source_profile_chunk_row(row) {
        return false;
    }
    let text = format!("{}\n{}", row.content_text, row.window_text.as_deref().unwrap_or_default())
        .to_lowercase();
    focus_keywords
        .iter()
        .filter(|keyword| keyword.chars().count() >= 3)
        .any(|keyword| text.contains(&keyword.to_lowercase()))
}

pub(super) fn query_ir_requests_focused_configuration_source_context(query_ir: &QueryIR) -> bool {
    matches!(query_ir.act, QueryAct::Describe | QueryAct::RetrieveValue)
        && query_ir.targets_any(&[QueryTargetKind::ConfigurationFile, QueryTargetKind::ConfigKey])
        && query_ir_has_strong_source_context_anchor(query_ir)
}

pub(super) fn query_ir_has_strong_source_context_anchor(query_ir: &QueryIR) -> bool {
    query_ir.document_focus.is_some() || !query_ir.literal_constraints.is_empty()
}

pub(super) fn requests_error_code_source_context(query_ir: &QueryIR) -> bool {
    query_ir.source_slice.is_none()
        && has_question_intent(&classify_query_ir_intents(query_ir), QuestionIntent::ErrorCode)
}

pub(super) fn requests_code_pattern_source_context(query_ir: &QueryIR) -> bool {
    requests_error_code_source_context(query_ir) && !code_pattern_literal_terms(query_ir).is_empty()
}

pub(super) fn code_pattern_literal_terms(query_ir: &QueryIR) -> Vec<String> {
    let mut seen = HashSet::new();
    query_ir
        .literal_constraints
        .iter()
        .filter_map(|literal| {
            let term = literal.text.trim();
            code_pattern_literal_is_structured(literal.kind, term).then(|| term.to_lowercase())
        })
        .filter(|term| seen.insert(term.clone()))
        .take(SOURCE_CONTEXT_CODE_PATTERN_TERM_LIMIT)
        .collect()
}

pub(super) fn code_pattern_literal_is_structured(kind: LiteralKind, text: &str) -> bool {
    let char_count = text.chars().count();
    if !(2..=160).contains(&char_count) || !text.is_ascii() {
        return false;
    }
    match kind {
        LiteralKind::NumericCode => text.chars().all(|ch| ch.is_ascii_digit()),
        LiteralKind::Identifier => code_pattern_identifier_is_structured(text),
        LiteralKind::Url | LiteralKind::Path | LiteralKind::Version | LiteralKind::Other => false,
    }
}

pub(super) fn code_pattern_identifier_is_structured(text: &str) -> bool {
    if !text.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        || !text.chars().any(|ch| ch.is_ascii_alphabetic())
    {
        return false;
    }

    let has_digit = text.chars().any(|ch| ch.is_ascii_digit());
    let has_strong_separator = text.chars().any(|ch| matches!(ch, '_' | '.'));
    let has_lowercase = text.chars().any(|ch| ch.is_ascii_lowercase());
    let has_uppercase = text.chars().any(|ch| ch.is_ascii_uppercase());
    let has_camel_transition = text
        .chars()
        .fold((false, false), |(seen_lowercase, transition), ch| {
            (
                seen_lowercase || ch.is_ascii_lowercase(),
                transition || (seen_lowercase && ch.is_ascii_uppercase()),
            )
        })
        .1;
    let uppercase_acronym = has_uppercase && !has_lowercase;

    has_digit || has_strong_separator || has_camel_transition || uppercase_acronym
}

pub(super) fn code_pattern_candidate_document_ids(chunks: &[RuntimeMatchedChunk]) -> Vec<Uuid> {
    let mut seen = HashSet::new();
    chunks
        .iter()
        .map(|chunk| chunk.document_id)
        .filter(|document_id| seen.insert(*document_id))
        .take(SOURCE_CONTEXT_DOCUMENT_LIMIT)
        .collect()
}

pub(super) fn preserve_primary_context_on_code_pattern_failure<T>(
    result: anyhow::Result<Vec<T>>,
    has_primary_evidence: bool,
) -> anyhow::Result<Vec<T>> {
    match result {
        Err(_) if has_primary_evidence => Ok(Vec::new()),
        result => result,
    }
}

pub(super) fn transport_pattern_candidate_document_ids(
    chunks: &[RuntimeMatchedChunk],
) -> Vec<Uuid> {
    code_pattern_candidate_document_ids(chunks)
}

pub(super) fn preserve_primary_context_on_transport_pattern_failure<T>(
    result: anyhow::Result<Vec<T>>,
    has_primary_evidence: bool,
) -> anyhow::Result<Vec<T>> {
    preserve_primary_context_on_code_pattern_failure(result, has_primary_evidence)
}

pub(super) fn requests_transport_source_context(query_ir: &QueryIR) -> bool {
    if query_ir.source_slice.is_some() || !query_ir.literal_constraints.is_empty() {
        return false;
    }
    let intents = classify_query_ir_intents(query_ir);
    has_question_intent(&intents, QuestionIntent::Port)
        || has_question_intent(&intents, QuestionIntent::Protocol)
        || query_ir.targets(QueryTargetKind::Connection)
}

pub(super) fn requests_table_row_source_context(query_ir: &QueryIR) -> bool {
    if query_ir.source_slice.is_some()
        || !matches!(query_ir.scope, QueryScope::SingleDocument)
        || !matches!(
            query_ir.act,
            QueryAct::Describe | QueryAct::Enumerate | QueryAct::RetrieveValue
        )
        || question_asks_table_aggregation("", Some(query_ir))
    {
        return false;
    }
    query_ir.targets(QueryTargetKind::TableRow) && query_ir.targets(QueryTargetKind::TableSummary)
}

pub(super) fn requests_structured_inventory_profile_context(query_ir: &QueryIR) -> bool {
    if query_ir.source_slice.is_some() {
        return false;
    }
    if requests_transport_source_context(query_ir)
        || requests_configuration_file_path_source_context(query_ir)
    {
        return true;
    }
    let intents = classify_query_ir_intents(query_ir);
    has_question_intent(&intents, QuestionIntent::ConfigKey)
        || has_question_intent(&intents, QuestionIntent::EnvVar)
        || query_ir.targets_any(&[
            QueryTargetKind::Connection,
            QueryTargetKind::Network,
            QueryTargetKind::Service,
            QueryTargetKind::ConfigurationFile,
            QueryTargetKind::ConfigKey,
        ])
}

pub(super) fn requests_path_source_context(query_ir: &QueryIR) -> bool {
    query_ir.source_slice.is_none()
        && (detect_technical_literal_intent_from_query_ir("", query_ir).wants_paths
            || requests_configuration_file_path_source_context(query_ir))
}

pub(super) fn requests_configuration_file_path_source_context(query_ir: &QueryIR) -> bool {
    query_ir.targets(QueryTargetKind::ConfigurationFile)
}

pub(super) async fn load_code_pattern_source_context(
    scope: SourceContextLoadScope<'_>,
    code_pattern_terms: &[String],
    candidate_document_ids: &[Uuid],
    chunks: &[RuntimeMatchedChunk],
    _companions: &[StructuredSourceCompanion],
) -> anyhow::Result<Vec<StructuredSourceCompanion>> {
    let SourceContextLoadScope { state, library_id, document_index, focus_keywords } = scope;
    let terms =
        code_pattern_query_terms(code_pattern_terms, SOURCE_CONTEXT_CODE_PATTERN_TERM_LIMIT);
    if terms.is_empty() || candidate_document_ids.is_empty() {
        return Ok(Vec::new());
    }
    let global_best_score = chunks.iter().map(|chunk| score_value(chunk.score)).fold(0.0, f32::max);
    let rows = state
        .document_store
        .search_code_pattern_chunks_by_terms(
            library_id,
            candidate_document_ids,
            &terms,
            SOURCE_CONTEXT_CODE_PATTERN_HIT_LIMIT,
        )
        .await?;
    let row_count = rows.len();
    let mut companion_document_index = document_index.clone();
    hydrate_missing_companion_documents(state, &rows, &mut companion_document_index).await?;
    let mut mapped = rows
        .into_iter()
        .enumerate()
        .filter_map(|(rank, row)| {
            let score =
                global_best_score + SOURCE_CONTEXT_CODE_PATTERN_SCORE_BONUS - rank as f32 * 0.01;
            map_companion_chunk(row, score, &companion_document_index, focus_keywords).map(
                |chunk| StructuredSourceCompanion {
                    chunk,
                    kind: StructuredSourceCompanionKind::FocusedMatch,
                },
            )
        })
        .collect::<Vec<_>>();
    tracing::info!(
        stage = "retrieval.structured_source_context.code_pattern",
        term_count = terms.len(),
        row_count = row_count,
        mapped_count = mapped.len(),
        "code-pattern source context candidates mapped"
    );
    mapped.sort_by(|left, right| {
        score_value(right.chunk.score)
            .total_cmp(&score_value(left.chunk.score))
            .then_with(|| left.chunk.document_id.cmp(&right.chunk.document_id))
            .then_with(|| left.chunk.chunk_index.cmp(&right.chunk.chunk_index))
            .then_with(|| left.chunk.chunk_id.cmp(&right.chunk.chunk_id))
    });
    Ok(mapped)
}

pub(super) async fn load_transport_pattern_source_context(
    scope: SourceContextLoadScope<'_>,
    candidate_document_ids: &[Uuid],
    chunks: &[RuntimeMatchedChunk],
    _companions: &[StructuredSourceCompanion],
) -> anyhow::Result<Vec<StructuredSourceCompanion>> {
    let SourceContextLoadScope { state, library_id, document_index, focus_keywords } = scope;
    let terms =
        code_pattern_query_terms(focus_keywords, SOURCE_CONTEXT_TRANSPORT_PATTERN_TERM_LIMIT);
    if terms.is_empty() || candidate_document_ids.is_empty() {
        return Ok(Vec::new());
    }
    let global_best_score = chunks.iter().map(|chunk| score_value(chunk.score)).fold(0.0, f32::max);
    let rows = state
        .document_store
        .search_transport_pattern_chunks_by_terms(
            library_id,
            candidate_document_ids,
            &terms,
            SOURCE_CONTEXT_TRANSPORT_PATTERN_HIT_LIMIT,
        )
        .await?;
    let row_count = rows.len();
    let mut companion_document_index = document_index.clone();
    hydrate_missing_companion_documents(state, &rows, &mut companion_document_index).await?;
    let mut mapped = rows
        .into_iter()
        .enumerate()
        .filter_map(|(rank, row)| {
            let score = global_best_score + SOURCE_CONTEXT_TRANSPORT_PATTERN_SCORE_BONUS
                - rank as f32 * 0.01;
            map_companion_chunk(row, score, &companion_document_index, focus_keywords).map(
                |chunk| StructuredSourceCompanion {
                    chunk,
                    kind: StructuredSourceCompanionKind::FocusedMatch,
                },
            )
        })
        .collect::<Vec<_>>();
    tracing::info!(
        stage = "retrieval.structured_source_context.transport_pattern",
        term_count = terms.len(),
        row_count = row_count,
        mapped_count = mapped.len(),
        "transport-pattern source context candidates mapped"
    );
    mapped.sort_by(|left, right| {
        score_value(right.chunk.score)
            .total_cmp(&score_value(left.chunk.score))
            .then_with(|| left.chunk.document_id.cmp(&right.chunk.document_id))
            .then_with(|| left.chunk.chunk_index.cmp(&right.chunk.chunk_index))
            .then_with(|| left.chunk.chunk_id.cmp(&right.chunk.chunk_id))
    });
    Ok(mapped)
}

pub(super) async fn hydrate_missing_companion_documents(
    state: &AppState,
    rows: &[KnowledgeChunkRow],
    document_index: &mut HashMap<Uuid, KnowledgeDocumentRow>,
) -> anyhow::Result<()> {
    let mut missing_document_ids = rows
        .iter()
        .map(|row| row.document_id)
        .filter(|document_id| !document_index.contains_key(document_id))
        .collect::<Vec<_>>();
    missing_document_ids.sort_unstable();
    missing_document_ids.dedup();
    if missing_document_ids.is_empty() {
        return Ok(());
    }
    let documents = state
        .document_store
        .list_documents_by_ids(&missing_document_ids)
        .await
        .context("failed to hydrate source-context companion documents")?;
    for document in documents {
        document_index.insert(document.document_id, document);
    }
    Ok(())
}

pub(super) fn code_pattern_query_terms(focus_keywords: &[String], limit: usize) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut terms = Vec::new();
    for keyword in focus_keywords {
        let term = keyword.trim().to_lowercase();
        let alphabetic_count = term.chars().filter(|ch| ch.is_alphabetic()).count();
        let digit_count = term.chars().filter(|ch| ch.is_ascii_digit()).count();
        if term.chars().count() < 2
            || (alphabetic_count < 2 && digit_count < 2)
            || !seen.insert(term.clone())
        {
            continue;
        }
        terms.push(term);
        if terms.len() >= limit {
            break;
        }
    }
    terms
}

pub(super) fn procedural_source_context_chunk_floor() -> usize {
    let span = SOURCE_CONTEXT_PROCEDURAL_NEIGHBOR_BACKWARD
        .saturating_add(SOURCE_CONTEXT_DEFAULT_NEIGHBOR_FORWARD)
        .saturating_add(1)
        .max(0) as usize;
    SOURCE_CONTEXT_PROFILE_HEADROOM
        .saturating_add(SOURCE_CONTEXT_ANCHOR_LIMIT_PER_DOCUMENT.saturating_mul(span))
        .saturating_add(SOURCE_CONTEXT_FOCUSED_MATCH_LIMIT_PER_DOCUMENT)
        .saturating_add(SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_LIMIT_PER_DOCUMENT)
}

pub(super) fn fallback_structured_source_context_chunk_floor() -> usize {
    procedural_source_context_chunk_floor().max(SOURCE_CONTEXT_FALLBACK_STRUCTURED_TOP_K_FLOOR)
}
