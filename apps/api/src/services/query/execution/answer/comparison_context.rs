use crate::domains::query_ir::{QueryAct, QueryIR};

use super::super::{
    RuntimeMatchedChunk, document_target::comparison_target_document_ids_from_values, score_value,
    source_excerpt::top_level_declaration_signatures,
    technical_literal_focus::technical_chunk_selection_score,
};

const DECLARATION_CHUNKS_PER_DOCUMENT: usize = 1;

pub(super) fn comparison_declaration_chunks(
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
    question_keywords: &[String],
    max_total_chunks: usize,
) -> Vec<RuntimeMatchedChunk> {
    if !matches!(query_ir.act, QueryAct::Compare) || max_total_chunks == 0 {
        return Vec::new();
    }
    let target_document_ids = comparison_target_document_ids_from_values(
        query_ir,
        chunks.iter().map(|chunk| (chunk.document_id, chunk.document_label.as_str())),
    );
    if target_document_ids.len() < 2 {
        return Vec::new();
    }

    let mut selected = Vec::new();
    for document_id in target_document_ids {
        let mut candidates = chunks
            .iter()
            .enumerate()
            .filter(|(_, chunk)| chunk.document_id == document_id)
            .filter_map(|(rank, chunk)| {
                let declarations = top_level_declaration_signatures(&chunk.source_text);
                if declarations.is_empty() {
                    return None;
                }
                let focus_score = technical_chunk_selection_score(
                    &declarations.join("\n"),
                    question_keywords,
                    false,
                );
                Some((focus_score, declarations.len(), score_value(chunk.score), rank, chunk))
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| right.1.cmp(&left.1))
                .then_with(|| right.2.total_cmp(&left.2))
                .then_with(|| left.3.cmp(&right.3))
        });
        selected.extend(
            candidates
                .into_iter()
                .take(DECLARATION_CHUNKS_PER_DOCUMENT)
                .map(|(_, _, _, _, chunk)| chunk.clone()),
        );
        if selected.len() >= max_total_chunks {
            break;
        }
    }
    selected
}
