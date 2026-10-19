use crate::infra::knowledge_plane::ChunkLexicalQuery;

use anyhow::Context;

use super::{
    BTreeSet, CONTENT_ANCHOR_CHUNK_CAP, CONTENT_ANCHOR_CHUNKS_PER_REVISION,
    CONTENT_ANCHOR_MIN_TOKEN_OVERLAP, CONTENT_ANCHOR_SEARCH_PREFIX_CHARS,
    CONTENT_ANCHOR_SEARCH_PREFIX_MIN_CHARS, CONTENT_ANCHOR_SEARCH_TERM_CAP,
    CONTENT_ANCHOR_SEQUENCE_CAP, CONTENT_ANCHOR_SEQUENCE_MIN_CHARS, CONTENT_ANCHOR_TOKEN_MIN_CHARS,
    DOCUMENT_EVIDENCE_ANCHOR_CANDIDATE_LIMIT, DOCUMENT_EVIDENCE_ANCHOR_CHUNKS_PER_DOCUMENT,
    HashMap, KnowledgeChunkRow, KnowledgeDocumentRow, LexicalQueryOrigin,
    QUERY_IR_FOCUS_CHUNKS_PER_QUERY, QueryIR, RetrievalLaneContext, RuntimeChunkScoreKind,
    RuntimeMatchedChunk, Uuid, batch_hydrate_hits, combine_query_ir_focus_search_results,
    content_anchor_chunk_score, current_document_revision_id, current_question_segment,
    document_evidence_anchor_candidate_document_ids, document_evidence_anchor_focus_terms,
    document_identity_chunk_score, is_source_profile_chunk_row, join_all, map_chunk_hit,
    near_token_match, normalized_alnum_token_sequence, normalized_alnum_tokens,
    prefix_token_sequence_contains_tokens, query_ir_focus_search_queries,
    repair_technical_layout_noise,
};

pub(super) async fn load_query_ir_focus_chunks(
    context: RetrievalLaneContext<'_>,
    library_id: Uuid,
    focus_queries: &[String],
    targeted_document_ids: &BTreeSet<Uuid>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let RetrievalLaneContext {
        state,
        question,
        document_index,
        plan_keywords,
        temporal_start,
        temporal_end,
    } = context;
    let search_queries = query_ir_focus_search_queries(question, focus_queries);
    if search_queries.is_empty() {
        return Ok(Vec::new());
    }

    let per_query_futures = search_queries.iter().cloned().map(|focus_query| async move {
        state
            .search_store
            .search_chunks(ChunkLexicalQuery {
                library_id,
                query: &focus_query,
                query_origin: LexicalQueryOrigin::DerivedProbe,
                limit: QUERY_IR_FOCUS_CHUNKS_PER_QUERY,
                temporal_start,
                temporal_end,
                text_search_config: None,
            })
            .await
            .map(|rows| {
                rows.into_iter().map(|row| (row.chunk_id, row.score as f32)).collect::<Vec<_>>()
            })
            .with_context(|| format!("failed to run query-IR focus chunk search: {focus_query}"))
    });
    let per_query_results: Vec<Result<Vec<_>, anyhow::Error>> = join_all(per_query_futures).await;
    let hits = combine_query_ir_focus_search_results(per_query_results, search_queries.len())?;
    if hits.is_empty() {
        return Ok(Vec::new());
    }

    let mut chunks =
        batch_hydrate_hits(state, hits, document_index, plan_keywords, targeted_document_ids)
            .await
            .context("failed to hydrate query-IR focus chunks")?;
    for chunk in &mut chunks {
        chunk.score_kind = RuntimeChunkScoreKind::QueryIrFocus;
    }
    tracing::info!(
        stage = "retrieval.query_ir_focus",
        focus_query_count = search_queries.len(),
        focus_chunk_count = chunks.len(),
        "query-IR focus chunks loaded for rare exact retrieval signals",
    );
    Ok(chunks)
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RetrievalContentAnchorModel {
    pub(super) search_terms: Vec<String>,
    pub(super) focus_tokens: BTreeSet<String>,
    pub(super) phrase_sequences: Vec<Vec<String>>,
}

pub(super) fn collect_content_anchor_query_ir_values(values: &mut Vec<String>, query_ir: &QueryIR) {
    query_ir
        .retrieval_query
        .as_deref()
        .into_iter()
        .chain(query_ir.document_focus.as_ref().map(|focus| focus.hint.as_str()))
        .chain(query_ir.target_entities.iter().map(|entity| entity.label.as_str()))
        .chain(query_ir.literal_constraints.iter().map(|literal| literal.text.as_str()))
        .for_each(|value| collect_content_anchor_text_value(values, value));
}

pub(super) fn content_anchor_focus(values: &[String]) -> (BTreeSet<String>, Vec<Vec<String>>) {
    let mut focus_tokens = BTreeSet::new();
    let mut phrase_sequences = Vec::new();
    let mut seen_sequences = BTreeSet::new();
    for value in values {
        focus_tokens.extend(normalized_alnum_tokens(value, CONTENT_ANCHOR_TOKEN_MIN_CHARS));
        for sequence in quoted_content_anchor_sequences(value)
            .into_iter()
            .chain(adjacent_content_anchor_sequences(value))
        {
            push_content_anchor_sequence(&mut phrase_sequences, &mut seen_sequences, sequence);
        }
    }
    (focus_tokens, phrase_sequences)
}

impl RetrievalContentAnchorModel {
    pub(crate) fn new(question: &str, query_ir: Option<&QueryIR>) -> Self {
        let mut values = Vec::new();
        collect_content_anchor_text_value(&mut values, question);
        if let Some(query_ir) = query_ir {
            collect_content_anchor_query_ir_values(&mut values, query_ir);
        }
        let (focus_tokens, phrase_sequences) = content_anchor_focus(&values);
        let search_terms = content_anchor_search_terms(&focus_tokens);
        Self { search_terms, focus_tokens, phrase_sequences }
    }

    fn is_empty(&self) -> bool {
        self.search_terms.is_empty()
            && self.focus_tokens.is_empty()
            && self.phrase_sequences.is_empty()
    }
}

pub(super) fn content_anchor_search_terms(focus_tokens: &BTreeSet<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut terms = Vec::new();
    for token in focus_tokens {
        push_content_anchor_search_term(&mut terms, &mut seen, token.clone());
        let token_len = token.chars().count();
        if token_len >= CONTENT_ANCHOR_SEARCH_PREFIX_MIN_CHARS {
            let prefix = token.chars().take(CONTENT_ANCHOR_SEARCH_PREFIX_CHARS).collect::<String>();
            if prefix.chars().count() >= CONTENT_ANCHOR_TOKEN_MIN_CHARS && prefix != *token {
                push_content_anchor_search_term(&mut terms, &mut seen, prefix);
            }
        }
        if terms.len() >= CONTENT_ANCHOR_SEARCH_TERM_CAP {
            break;
        }
    }
    terms
}

pub(super) fn push_content_anchor_search_term(
    terms: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    term: String,
) {
    if terms.len() >= CONTENT_ANCHOR_SEARCH_TERM_CAP {
        return;
    }
    if seen.insert(term.clone()) {
        terms.push(term);
    }
}

pub(super) fn collect_content_anchor_text_value(values: &mut Vec<String>, value: &str) {
    let current = current_question_segment(value).trim();
    if !current.is_empty() {
        values.push(current.to_string());
    }
}

pub(super) fn quoted_content_anchor_sequences(value: &str) -> Vec<Vec<String>> {
    let mut sequences = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in value.chars() {
        if content_anchor_quote_matches(quote, ch) {
            let sequence =
                normalized_alnum_token_sequence(&current, CONTENT_ANCHOR_SEQUENCE_MIN_CHARS);
            if sequence.len() >= 2 {
                sequences.push(sequence);
            }
            current.clear();
            quote = None;
            continue;
        }
        if quote.is_some() {
            current.push(ch);
            continue;
        }
        if content_anchor_opening_quote(ch) {
            quote = Some(ch);
            current.clear();
        }
    }
    sequences
}

pub(super) fn content_anchor_opening_quote(ch: char) -> bool {
    matches!(ch, '"' | '\'' | '`' | '\u{00ab}' | '\u{201c}' | '\u{2018}')
}

pub(super) fn content_anchor_quote_matches(opening: Option<char>, closing: char) -> bool {
    matches!(
        (opening, closing),
        (Some('"'), '"')
            | (Some('\''), '\'')
            | (Some('`'), '`')
            | (Some('\u{00ab}'), '\u{00bb}')
            | (Some('\u{201c}'), '\u{201d}')
            | (Some('\u{2018}'), '\u{2019}')
    )
}

pub(super) fn adjacent_content_anchor_sequences(value: &str) -> Vec<Vec<String>> {
    let tokens = normalized_alnum_token_sequence(value, CONTENT_ANCHOR_SEQUENCE_MIN_CHARS);
    let mut sequences = Vec::new();
    for window_size in 2..=4 {
        for window in tokens.windows(window_size) {
            sequences.push(window.to_vec());
            if sequences.len() >= CONTENT_ANCHOR_SEQUENCE_CAP {
                return sequences;
            }
        }
    }
    sequences
}

pub(super) fn push_content_anchor_sequence(
    sequences: &mut Vec<Vec<String>>,
    seen: &mut BTreeSet<Vec<String>>,
    sequence: Vec<String>,
) {
    if sequences.len() >= CONTENT_ANCHOR_SEQUENCE_CAP || sequence.len() < 2 {
        return;
    }
    if seen.insert(sequence.clone()) {
        sequences.push(sequence);
    }
}

pub(super) fn content_anchor_revision_ids(
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    targeted_document_ids: &BTreeSet<Uuid>,
) -> Vec<Uuid> {
    let mut revision_ids = document_index
        .values()
        .filter(|document| {
            targeted_document_ids.is_empty()
                || targeted_document_ids.contains(&document.document_id)
        })
        .filter_map(|document| {
            current_document_revision_id(document).map(|id| (document.document_id, id))
        })
        .collect::<Vec<_>>();
    revision_ids.sort_by_key(|(document_id, revision_id)| (*revision_id, *document_id));
    revision_ids.dedup_by_key(|(_, revision_id)| *revision_id);
    revision_ids.into_iter().map(|(_, revision_id)| revision_id).collect()
}

pub(crate) fn content_anchor_row_score(
    row: &KnowledgeChunkRow,
    model: &RetrievalContentAnchorModel,
) -> usize {
    if is_source_profile_chunk_row(row)
        || matches!(row.chunk_kind.as_deref(), Some("DocumentIdentity" | "LatestVersion"))
    {
        return 0;
    }
    let mut text = String::new();
    if !row.heading_trail.is_empty() {
        text.push_str(&row.heading_trail.join(" "));
        text.push('\n');
    }
    if !row.section_path.is_empty() {
        text.push_str(&row.section_path.join(" "));
        text.push('\n');
    }
    text.push_str(&row.content_text);
    if !row.normalized_text.trim().is_empty() {
        text.push('\n');
        text.push_str(&row.normalized_text);
    }
    if let Some(window_text) = row.window_text.as_deref().filter(|value| !value.trim().is_empty()) {
        text.push('\n');
        text.push_str(window_text);
    }
    let text = repair_technical_layout_noise(&text);
    let text_sequence = normalized_alnum_token_sequence(&text, CONTENT_ANCHOR_SEQUENCE_MIN_CHARS);
    let text_tokens = normalized_alnum_tokens(&text, CONTENT_ANCHOR_TOKEN_MIN_CHARS)
        .into_iter()
        .collect::<BTreeSet<_>>();

    let phrase_hits = model
        .phrase_sequences
        .iter()
        .filter(|sequence| prefix_token_sequence_contains_tokens(&text_sequence, sequence, 5))
        .count();
    let token_overlap = model
        .focus_tokens
        .iter()
        .filter(|focus| {
            text_tokens.contains(*focus)
                || text_tokens.iter().any(|token| near_token_match(focus, token))
        })
        .count();
    if phrase_hits == 0 && token_overlap < CONTENT_ANCHOR_MIN_TOKEN_OVERLAP {
        return 0;
    }
    let longest_sequence = model
        .phrase_sequences
        .iter()
        .filter(|sequence| prefix_token_sequence_contains_tokens(&text_sequence, sequence, 5))
        .map(Vec::len)
        .max()
        .unwrap_or(0);
    phrase_hits
        .saturating_mul(1024)
        .saturating_add(token_overlap.saturating_mul(128))
        .saturating_add(longest_sequence.saturating_mul(16))
}

pub(super) async fn load_content_anchor_chunks(
    context: RetrievalLaneContext<'_>,
    query_ir: Option<&QueryIR>,
    targeted_document_ids: &BTreeSet<Uuid>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let RetrievalLaneContext {
        state,
        question,
        document_index,
        plan_keywords,
        temporal_start,
        temporal_end,
    } = context;
    if temporal_start.is_some() || temporal_end.is_some() {
        return Ok(Vec::new());
    }
    let model = RetrievalContentAnchorModel::new(question, query_ir);
    if model.is_empty() {
        return Ok(Vec::new());
    }
    let revision_ids = content_anchor_revision_ids(document_index, targeted_document_ids);
    if revision_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = state
        .document_store
        .list_chunks_by_revisions_matching_terms(
            &revision_ids,
            &model.search_terms,
            CONTENT_ANCHOR_CHUNKS_PER_REVISION,
        )
        .await
        .context("failed to load content-anchor chunks")?;
    let mut scored_rows = rows
        .into_iter()
        .filter_map(|row| {
            let score = content_anchor_row_score(&row, &model);
            (score > 0).then_some((score, row))
        })
        .collect::<Vec<_>>();
    scored_rows.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    let mut seen = BTreeSet::new();
    let mut chunks = Vec::new();
    for (rank, (evidence_score, row)) in scored_rows.into_iter().enumerate() {
        if chunks.len() >= CONTENT_ANCHOR_CHUNK_CAP {
            break;
        }
        if !seen.insert(row.chunk_id) {
            continue;
        }
        if let Some(mut chunk) = map_chunk_hit(
            row,
            content_anchor_chunk_score(rank, evidence_score),
            document_index,
            plan_keywords,
        ) {
            chunk.score_kind = RuntimeChunkScoreKind::ContentAnchor;
            chunks.push(chunk);
        }
    }
    tracing::info!(
        stage = "retrieval.content_anchor",
        content_anchor_chunk_count = chunks.len(),
        content_anchor_search_term_count = model.search_terms.len(),
        "content-anchor chunks loaded from body evidence"
    );
    Ok(chunks)
}

pub(super) async fn load_document_evidence_anchor_chunks(
    context: RetrievalLaneContext<'_>,
    query_ir: Option<&QueryIR>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let RetrievalLaneContext {
        state,
        question,
        document_index,
        plan_keywords,
        temporal_start,
        temporal_end,
    } = context;
    if temporal_start.is_some() || temporal_end.is_some() {
        return Ok(Vec::new());
    }
    let candidate_document_ids = document_evidence_anchor_candidate_document_ids(
        question,
        query_ir,
        document_index,
        DOCUMENT_EVIDENCE_ANCHOR_CANDIDATE_LIMIT,
    );
    if candidate_document_ids.is_empty() {
        return Ok(Vec::new());
    }
    let focus_terms = document_evidence_anchor_focus_terms(question, query_ir);
    let mut chunks = Vec::new();
    for (document_rank, document_id) in candidate_document_ids.iter().enumerate() {
        let Some(document) = document_index.get(document_id) else {
            continue;
        };
        let Some(revision_id) = current_document_revision_id(document) else {
            continue;
        };
        let rows = state
            .document_store
            .list_chunks_by_revision_matching_terms(
                revision_id,
                &focus_terms,
                DOCUMENT_EVIDENCE_ANCHOR_CHUNKS_PER_DOCUMENT,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to load document-evidence anchor chunks for document {} revision {}",
                    document_id, revision_id
                )
            })?;
        let rows = if rows.is_empty() {
            state
                .document_store
                .list_chunks_by_revision_range(
                    revision_id,
                    0,
                    DOCUMENT_EVIDENCE_ANCHOR_CHUNKS_PER_DOCUMENT.saturating_sub(1) as i32,
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to load fallback document-evidence anchor chunks for document {} revision {}",
                        document_id, revision_id
                    )
                })?
        } else {
            rows
        };
        for (chunk_rank, row) in rows.into_iter().enumerate() {
            let score = document_identity_chunk_score(document_rank, chunk_rank);
            if let Some(mut chunk) = map_chunk_hit(row, score, document_index, plan_keywords) {
                chunk.score = Some(score);
                chunk.score_kind = RuntimeChunkScoreKind::FocusedDocument;
                chunks.push(chunk);
            }
        }
    }
    if !chunks.is_empty() {
        tracing::info!(
            stage = "retrieval.document_evidence_anchor",
            anchor_document_count = candidate_document_ids.len(),
            anchor_chunk_count = chunks.len(),
            "document identity evidence anchored into answer context"
        );
    }
    Ok(chunks)
}
