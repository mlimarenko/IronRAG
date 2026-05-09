use std::collections::HashMap;

use uuid::Uuid;

use crate::domains::query_ir::QueryIR;

use super::{
    document_target::{focused_answer_document_id, question_requests_multi_document_scope},
    endpoint_answer::select_multi_document_scope_ids,
    question_intent::{
        query_ir_allows_deterministic_endpoint_lookup,
        query_ir_disallows_graph_id_like_endpoint_candidate,
    },
    retrieve::{focused_excerpt_for, score_value},
    technical_answer::prioritized_technical_chunk_score,
    technical_literal_context::{TechnicalLiteralDocumentGroup, infer_endpoint_subject_label},
    technical_literals::{
        document_local_focus_keywords, extract_explicit_path_literals, extract_http_methods,
        extract_url_literals, technical_chunk_selection_score, technical_literal_focus_keywords,
    },
    types::RuntimeMatchedChunk,
};

pub(crate) fn build_multi_document_endpoint_answer_from_chunks(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if !query_ir_allows_deterministic_endpoint_lookup(query_ir) {
        return None;
    }
    if !question_requests_multi_document_scope(question, Some(query_ir)) {
        return None;
    }

    let question_keywords = technical_literal_focus_keywords(question, Some(query_ir));
    if question_keywords.is_empty() {
        return None;
    }
    let pagination_requested = false;

    let per_document_chunks = chunks_by_document(chunks);
    let mut ordered_document_ids = Vec::<Uuid>::new();
    for chunk in chunks {
        if !ordered_document_ids.contains(&chunk.document_id) {
            ordered_document_ids.push(chunk.document_id);
        }
    }
    let scoped_document_ids = select_multi_document_scope_ids(
        question,
        query_ir,
        &ordered_document_ids,
        &per_document_chunks,
    );

    let mut lines = Vec::new();
    for document_id in scoped_document_ids {
        let Some(document_chunks) = per_document_chunks.get(&document_id) else {
            continue;
        };
        let local_keywords = document_local_focus_keywords(
            question,
            Some(query_ir),
            document_chunks,
            &question_keywords,
        );
        let mut ranked_chunks = document_chunks.clone();
        ranked_chunks.sort_by(|left, right| {
            let left_match = technical_chunk_selection_score(
                &format!("{} {}", left.excerpt, left.source_text),
                &local_keywords,
                pagination_requested,
            );
            let right_match = technical_chunk_selection_score(
                &format!("{} {}", right.excerpt, right.source_text),
                &local_keywords,
                pagination_requested,
            );
            right_match
                .cmp(&left_match)
                .then_with(|| score_value(right.score).total_cmp(&score_value(left.score)))
        });

        let Some(best_chunk) = ranked_chunks.into_iter().find(|chunk| {
            let focused = focused_excerpt_for(&chunk.source_text, &local_keywords, 900);
            let literal_source = if focused.trim().is_empty() {
                chunk.source_text.as_str()
            } else {
                focused.as_str()
            };
            extract_allowed_endpoint_literal(query_ir, literal_source).is_some()
        }) else {
            continue;
        };

        let focused = focused_excerpt_for(&best_chunk.source_text, &local_keywords, 900);
        let literal_source = if focused.trim().is_empty() {
            best_chunk.source_text.as_str()
        } else {
            focused.as_str()
        };
        let endpoint = extract_allowed_endpoint_literal(query_ir, literal_source)?;
        let subject = infer_endpoint_subject_label(&TechnicalLiteralDocumentGroup {
            document_label: best_chunk.document_label.clone(),
            ..TechnicalLiteralDocumentGroup::default()
        });
        let literal = extract_http_methods(literal_source, 3)
            .into_iter()
            .next()
            .map_or_else(|| format!("`{endpoint}`"), |method| format!("`{method} {endpoint}`"));
        lines.push(format!("- {subject}: {literal}"));
    }

    (lines.len() >= 2).then(|| format!("Required endpoints:\n\n{}", lines.join("\n")))
}

pub(crate) fn build_single_endpoint_answer_from_chunks(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if !query_ir_allows_deterministic_endpoint_lookup(query_ir) {
        return None;
    }
    if question_requests_multi_document_scope(question, Some(query_ir)) {
        return None;
    }
    if chunks.is_empty() {
        return None;
    }

    let question_keywords = technical_literal_focus_keywords(question, Some(query_ir));
    if question_keywords.is_empty() {
        return None;
    }

    let focused_document_id = focused_answer_document_id(question, chunks);
    let candidate_chunks = chunks.iter().collect::<Vec<_>>();
    if candidate_chunks.is_empty() {
        return None;
    }

    let pagination_requested = false;
    let local_keywords = document_local_focus_keywords(
        question,
        Some(query_ir),
        &candidate_chunks,
        &question_keywords,
    );
    let mut ranked_chunks = candidate_chunks;
    ranked_chunks.sort_by(|left, right| {
        let left_match = prioritized_technical_chunk_score(
            &format!("{} {}", left.excerpt, left.source_text),
            left.document_id,
            &local_keywords,
            pagination_requested,
            focused_document_id,
        );
        let right_match = prioritized_technical_chunk_score(
            &format!("{} {}", right.excerpt, right.source_text),
            right.document_id,
            &local_keywords,
            pagination_requested,
            focused_document_id,
        );
        right_match
            .cmp(&left_match)
            .then_with(|| score_value(right.score).total_cmp(&score_value(left.score)))
    });

    let best_chunk = ranked_chunks.into_iter().find(|chunk| {
        let focused = focused_excerpt_for(&chunk.source_text, &local_keywords, 900);
        let focused_literals_present = !focused.trim().is_empty()
            && extract_allowed_endpoint_literal(query_ir, &focused).is_some();
        let literal_source =
            if focused_literals_present { focused.as_str() } else { chunk.source_text.as_str() };
        extract_allowed_endpoint_literal(query_ir, literal_source).is_some()
    })?;

    let focused = focused_excerpt_for(&best_chunk.source_text, &local_keywords, 900);
    let focused_literals_present = !focused.trim().is_empty()
        && extract_allowed_endpoint_literal(query_ir, &focused).is_some();
    let literal_source =
        if focused_literals_present { focused.as_str() } else { best_chunk.source_text.as_str() };
    let endpoint = extract_allowed_endpoint_literal(query_ir, literal_source)?;
    let literal = extract_http_methods(literal_source, 3)
        .into_iter()
        .next()
        .map_or_else(|| format!("`{endpoint}`"), |method| format!("`{method} {endpoint}`"));

    Some(format!("The endpoint is {literal}."))
}

fn extract_allowed_endpoint_literal(query_ir: &QueryIR, source_text: &str) -> Option<String> {
    extract_explicit_path_literals(source_text, 6)
        .into_iter()
        .find(|literal| !query_ir_disallows_graph_id_like_endpoint_candidate(query_ir, literal))
        .or_else(|| {
            extract_url_literals(source_text, 4).into_iter().find(|literal| {
                !query_ir_disallows_graph_id_like_endpoint_candidate(query_ir, literal)
            })
        })
}

fn chunks_by_document(chunks: &[RuntimeMatchedChunk]) -> HashMap<Uuid, Vec<&RuntimeMatchedChunk>> {
    let mut per_document_chunks = HashMap::<Uuid, Vec<&RuntimeMatchedChunk>>::new();
    for chunk in chunks {
        per_document_chunks.entry(chunk.document_id).or_default().push(chunk);
    }
    per_document_chunks
}
