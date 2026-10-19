use super::test_support::sample_chunk;
use super::*;

mod broad_procedure_tests;
mod focus_decision_tests;
mod runbook_tests;
mod topical_prune_tests;
mod winner_chunks_tests;

use crate::domains::query_ir::{
    DocumentHint, EntityMention, EntityRole, QueryAct, QueryIR, QueryScope,
};
use crate::infra::knowledge_rows::KnowledgeChunkRow;

const DEFAULT_TEST_QUESTION: &str = "Which focused document contains the requested evidence?";

fn ir(scope: QueryScope) -> QueryIR {
    QueryIR::test_with_act_scope(QueryAct::ConfigureHow, scope)
}

fn chunk_row(
    document_id: Uuid,
    revision_id: Uuid,
    chunk_index: i32,
    content: &str,
) -> KnowledgeChunkRow {
    chunk_row_with_kind(document_id, revision_id, chunk_index, Some("paragraph"), content)
}

fn chunk_row_with_kind(
    document_id: Uuid,
    revision_id: Uuid,
    chunk_index: i32,
    chunk_kind: Option<&str>,
    content: &str,
) -> KnowledgeChunkRow {
    KnowledgeChunkRow {
        chunk_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id,
        revision_id,
        chunk_index,
        chunk_kind: chunk_kind.map(str::to_string),
        content_text: content.to_string(),
        normalized_text: content.to_string(),
        span_start: Some(0),
        span_end: Some(content.len() as i32),
        token_count: Some(1),
        support_block_ids: Vec::new(),
        section_path: Vec::new(),
        heading_trail: Vec::new(),
        literal_digest: None,
        chunk_state: "ready".to_string(),
        text_generation: Some(1),
        vector_generation: Some(1),
        quality_score: None,

        window_text: None,

        occurred_at: None,
        occurred_until: None,
    }
}

fn source_profile_chunk(
    document_id: Uuid,
    revision_id: Uuid,
    chunk_index: i32,
    document_label: &str,
    score: f32,
) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_kind: Some("source_profile".to_string()),
        source_text:
            "[source_profile source_format=record_jsonl sequence_kind=record_stream unit_count=600]"
                .to_string(),
        excerpt: "[source_profile source_format=record_jsonl unit_count=600]".to_string(),
        ..sample_chunk(document_id, revision_id, chunk_index, document_label, score)
    }
}

fn text_chunk(
    document_id: Uuid,
    revision_id: Uuid,
    chunk_index: i32,
    document_label: &str,
    score: f32,
    text: &str,
) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        excerpt: text.to_string(),
        source_text: text.to_string(),
        ..sample_chunk(document_id, revision_id, chunk_index, document_label, score)
    }
}

fn focused_chunk(
    document_id: Uuid,
    revision_id: Uuid,
    chunk_index: i32,
    document_label: &str,
    score: f32,
) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::FocusedDocument,
        ..sample_chunk(document_id, revision_id, chunk_index, document_label, score)
    }
}

fn run_apply(
    bundle: &mut RetrievalBundle,
    query_ir: &QueryIR,
    top_k: usize,
    revision_rows: Vec<KnowledgeChunkRow>,
) -> ConsolidationDiagnostics {
    run_apply_with_question(bundle, query_ir, DEFAULT_TEST_QUESTION, top_k, revision_rows)
}

fn run_apply_with_question(
    bundle: &mut RetrievalBundle,
    query_ir: &QueryIR,
    question: &str,
    top_k: usize,
    revision_rows: Vec<KnowledgeChunkRow>,
) -> ConsolidationDiagnostics {
    let Some((winner, reason, budget)) = decide_focus(bundle, query_ir, question, top_k) else {
        return ConsolidationDiagnostics::noop();
    };
    // Pure path also validates the anchor windows are well-formed
    // for the chosen winner before we apply; mirrors the async
    // orchestrator's guard.
    assert!(!winner_anchor_windows(&winner, budget).is_empty());
    apply_winner_chunks(bundle, &winner, reason, budget, top_k, revision_rows)
}
