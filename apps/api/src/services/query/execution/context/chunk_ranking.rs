use std::collections::HashSet;

use uuid::Uuid;

use crate::{
    domains::query_ir::QueryIR,
    services::query::text_match::{normalized_alnum_tokens, token_sequence_exact_or_contains},
    shared::extraction::text_render::repair_technical_layout_noise,
};

use super::super::{
    MAX_CHUNKS_PER_DOCUMENT,
    command_shape::procedure_line_has_command_start,
    retrieve::{query_ir_requests_versioned_update_procedure_context, score_value},
    source_profile::is_source_profile_runtime_chunk,
    technical_literals::{DocumentBalancedChunkSelection, select_document_balanced_chunks},
    types::{RuntimeChunkScoreKind, RuntimeMatchedChunk},
};

const CONTENT_ANCHOR_TOKEN_MIN_CHARS: usize = 4;
const CONTENT_ANCHOR_PRIORITY_LIMIT: usize = 8;
const CONTENT_ANCHOR_MIN_TOKEN_OVERLAP: usize = 2;

pub(super) fn order_bounded_context_chunks<'a>(
    question: &str,
    query_ir: &QueryIR,
    chunks: &'a [RuntimeMatchedChunk],
    keywords: &[String],
) -> Vec<&'a RuntimeMatchedChunk> {
    if chunks.is_empty() {
        return Vec::new();
    }
    let selected = select_document_balanced_chunks(DocumentBalancedChunkSelection {
        question,
        ir: Some(query_ir),
        chunks,
        keywords,
        pagination_requested: false,
        max_total_chunks: chunks.len(),
        max_chunks_per_document: MAX_CHUNKS_PER_DOCUMENT,
    });
    let mut ordered = Vec::<&RuntimeMatchedChunk>::with_capacity(chunks.len());
    let mut seen = HashSet::<Uuid>::with_capacity(chunks.len());

    extend_unseen_chunks(
        &mut ordered,
        &mut seen,
        chunks.iter().filter(|chunk| is_source_profile_runtime_chunk(chunk)),
    );
    if query_ir_requests_versioned_update_procedure_context(question, query_ir) {
        extend_unseen_chunks(
            &mut ordered,
            &mut seen,
            procedure_context_priority_chunks(question, query_ir, chunks),
        );
    }
    extend_unseen_chunks(
        &mut ordered,
        &mut seen,
        content_anchor_priority_chunks(question, query_ir, chunks),
    );
    extend_unseen_chunks(&mut ordered, &mut seen, sorted_identity_chunks(chunks));
    extend_unseen_chunks(&mut ordered, &mut seen, selected);
    extend_unseen_chunks(&mut ordered, &mut seen, chunks.iter());
    ordered
}

fn extend_unseen_chunks<'a>(
    ordered: &mut Vec<&'a RuntimeMatchedChunk>,
    seen: &mut HashSet<Uuid>,
    chunks: impl IntoIterator<Item = &'a RuntimeMatchedChunk>,
) {
    for chunk in chunks {
        if seen.insert(chunk.chunk_id) {
            ordered.push(chunk);
        }
    }
}

fn sorted_identity_chunks(chunks: &[RuntimeMatchedChunk]) -> Vec<&RuntimeMatchedChunk> {
    let mut identity_chunks = chunks
        .iter()
        .filter(|chunk| {
            matches!(
                chunk.score_kind,
                RuntimeChunkScoreKind::DocumentIdentity | RuntimeChunkScoreKind::LatestVersion
            )
        })
        .collect::<Vec<_>>();
    identity_chunks.sort_by(|left, right| {
        score_value(right.score)
            .total_cmp(&score_value(left.score))
            .then_with(|| left.document_id.cmp(&right.document_id))
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    identity_chunks
}

fn content_anchor_priority_chunks<'a>(
    question: &str,
    query_ir: &QueryIR,
    chunks: &'a [RuntimeMatchedChunk],
) -> Vec<&'a RuntimeMatchedChunk> {
    let model = ContentAnchorModel::new(question, query_ir);
    if model.is_empty() {
        return Vec::new();
    }
    let mut scored = chunks
        .iter()
        .filter(|chunk| content_anchor_candidate(chunk))
        .filter_map(|chunk| {
            let score = content_anchor_priority_score(chunk, &model);
            (score > 0).then_some((score, chunk))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| score_value(right.score).total_cmp(&score_value(left.score)))
            .then_with(|| left.document_id.cmp(&right.document_id))
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    scored.into_iter().take(CONTENT_ANCHOR_PRIORITY_LIMIT).map(|(_, chunk)| chunk).collect()
}

fn content_anchor_candidate(chunk: &RuntimeMatchedChunk) -> bool {
    !is_source_profile_runtime_chunk(chunk)
        && !matches!(
            chunk.score_kind,
            RuntimeChunkScoreKind::DocumentIdentity | RuntimeChunkScoreKind::LatestVersion
        )
}

struct ContentAnchorModel {
    focus_tokens: HashSet<String>,
    quoted_phrases: Vec<String>,
}

impl ContentAnchorModel {
    fn new(question: &str, query_ir: &QueryIR) -> Self {
        let mut focus_tokens = HashSet::<String>::new();
        let mut quoted_phrases = Vec::<String>::new();
        let mut seen_phrases = HashSet::<String>::new();

        let mut add_source = |value: &str| {
            let current = crate::services::query::effective_query::current_question_segment(value);
            for token in normalized_alnum_tokens(current, CONTENT_ANCHOR_TOKEN_MIN_CHARS) {
                focus_tokens.insert(token);
            }
            for phrase in quoted_content_anchor_phrases(current) {
                if seen_phrases.insert(phrase.clone()) {
                    quoted_phrases.push(phrase);
                }
            }
        };

        add_source(question);
        if let Some(retrieval_query) = query_ir.retrieval_query.as_deref() {
            add_source(retrieval_query);
        }
        if let Some(document_focus) = query_ir.document_focus.as_ref() {
            add_source(&document_focus.hint);
        }
        for entity in &query_ir.target_entities {
            add_source(&entity.label);
        }
        for literal in &query_ir.literal_constraints {
            add_source(&literal.text);
        }

        Self { focus_tokens, quoted_phrases }
    }

    fn is_empty(&self) -> bool {
        self.focus_tokens.is_empty() && self.quoted_phrases.is_empty()
    }
}

fn quoted_content_anchor_phrases(value: &str) -> Vec<String> {
    let mut phrases = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    for (open, close) in [('«', '»'), ('“', '”'), ('"', '"'), ('`', '`'), ('\'', '\'')] {
        for phrase in quoted_spans(value, open, close) {
            if normalized_alnum_tokens(&phrase, 3).len() < 2 {
                continue;
            }
            if seen.insert(phrase.clone()) {
                phrases.push(phrase);
            }
        }
    }
    phrases
}

fn quoted_spans(value: &str, open: char, close: char) -> Vec<String> {
    let mut spans = Vec::<String>::new();
    let mut start: Option<usize> = None;
    for (index, ch) in value.char_indices() {
        if let Some(open_index) = start {
            if ch == close {
                let phrase = value[open_index..index].trim();
                if !phrase.is_empty() {
                    spans.push(phrase.to_string());
                }
                start = None;
            }
            continue;
        }
        if ch == open {
            start = Some(index + ch.len_utf8());
        }
    }
    spans
}

fn content_anchor_priority_score(chunk: &RuntimeMatchedChunk, model: &ContentAnchorModel) -> usize {
    let text = repair_technical_layout_noise(&format!("{}\n{}", chunk.excerpt, chunk.source_text));
    let phrase_hits = model
        .quoted_phrases
        .iter()
        .filter(|phrase| token_sequence_exact_or_contains(&text, phrase, 3))
        .count();
    let text_tokens =
        normalized_alnum_tokens(&text, CONTENT_ANCHOR_TOKEN_MIN_CHARS).into_iter().collect();
    let token_overlap = soft_context_overlap_count(&model.focus_tokens, &text_tokens);
    if phrase_hits == 0 && token_overlap < CONTENT_ANCHOR_MIN_TOKEN_OVERLAP {
        return 0;
    }

    phrase_hits
        .saturating_mul(512)
        .saturating_add(token_overlap.saturating_mul(64))
        .saturating_add((chunk.score_kind == RuntimeChunkScoreKind::FocusedDocument) as usize * 24)
        .saturating_add((chunk.score_kind == RuntimeChunkScoreKind::SourceContext) as usize * 12)
}

fn procedure_context_priority_chunks<'a>(
    question: &str,
    query_ir: &QueryIR,
    chunks: &'a [RuntimeMatchedChunk],
) -> Vec<&'a RuntimeMatchedChunk> {
    let model = ProcedureContextModel::new(question, query_ir);
    let mut scored = chunks
        .iter()
        .filter_map(|chunk| {
            let score = procedure_context_priority_score(chunk, &model);
            (score > 0).then_some((score, chunk))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| score_value(right.score).total_cmp(&score_value(left.score)))
            .then_with(|| left.document_id.cmp(&right.document_id))
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    scored.into_iter().map(|(_, chunk)| chunk).collect()
}

pub(super) struct ProcedureContextModel {
    pub(super) subject_terms: HashSet<String>,
    pub(super) action_terms: HashSet<String>,
}

impl ProcedureContextModel {
    pub(super) fn new(question: &str, query_ir: &QueryIR) -> Self {
        let current_question =
            crate::services::query::effective_query::current_question_segment(question);
        let mut subject_terms = HashSet::<String>::new();
        for entity in &query_ir.target_entities {
            subject_terms.extend(normalized_alnum_tokens(&entity.label, 2));
        }
        if let Some(document_focus) = query_ir.document_focus.as_ref() {
            subject_terms.extend(normalized_alnum_tokens(&document_focus.hint, 2));
        }

        let mut action_terms = normalized_alnum_tokens(current_question, 5)
            .into_iter()
            .filter(|term| {
                !subject_terms.iter().any(|subject| soft_context_terms_match(term, subject))
            })
            .collect::<HashSet<_>>();
        if let Some(retrieval_query) = query_ir.retrieval_query.as_deref() {
            let current_retrieval_query =
                crate::services::query::effective_query::current_question_segment(retrieval_query);
            action_terms.extend(
                normalized_alnum_tokens(current_retrieval_query, 5).into_iter().filter(|term| {
                    !subject_terms.iter().any(|subject| soft_context_terms_match(term, subject))
                }),
            );
        }

        Self { subject_terms, action_terms }
    }
}

fn procedure_context_priority_score(
    chunk: &RuntimeMatchedChunk,
    model: &ProcedureContextModel,
) -> usize {
    let text = repair_technical_layout_noise(&format!(
        "{}\n{}\n{}",
        chunk.document_label, chunk.excerpt, chunk.source_text
    ));
    let command_score = procedure_context_command_signal_score(&text);
    if command_score == 0 {
        return 0;
    }

    let tokens = normalized_alnum_tokens(&text, 2).into_iter().collect::<HashSet<_>>();
    let action_overlap = soft_context_overlap_count(&model.action_terms, &tokens);
    let subject_overlap = soft_context_overlap_count(&model.subject_terms, &tokens);
    if action_overlap == 0 && subject_overlap == 0 {
        return 0;
    }

    action_overlap
        .saturating_mul(96)
        .saturating_add(subject_overlap.saturating_mul(24))
        .saturating_add(command_score.saturating_mul(8))
        .saturating_add((chunk.score_kind == RuntimeChunkScoreKind::FocusedDocument) as usize * 16)
}

fn soft_context_overlap_count(expected: &HashSet<String>, available: &HashSet<String>) -> usize {
    expected
        .iter()
        .filter(|term| available.iter().any(|candidate| soft_context_terms_match(term, candidate)))
        .count()
}

fn soft_context_terms_match(left: &str, right: &str) -> bool {
    left == right || soft_context_common_prefix_len(left, right) >= 5
}

fn soft_context_common_prefix_len(left: &str, right: &str) -> usize {
    let mut count = 0usize;
    for (left_ch, right_ch) in left.chars().zip(right.chars()) {
        if left_ch != right_ch {
            break;
        }
        count += 1;
    }
    count
}

fn procedure_context_command_signal_score(text: &str) -> usize {
    let mut score = 0usize;
    for line in text.lines() {
        if procedure_line_has_command_start(line) {
            score = score.saturating_add(1);
        }
    }
    score.min(8)
}
