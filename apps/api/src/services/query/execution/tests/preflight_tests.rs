use super::*;
use crate::services::query::execution::preflight::setup_structured_stage::{
    merge_setup_preflight_structured_blocks, setup_preflight_document_ids,
};
use crate::services::query::execution::preflight::{
    TechnicalLiteralSelection, extend_setup_preflight_chunks_from_setup_lanes,
    extend_setup_preflight_chunks_from_structured_context, preflight_exact_literal_document_scope,
    query_ir_requests_low_confidence_setup_preflight, query_ir_requests_setup_literal_context,
    question_prefers_single_exact_literal_scope, select_technical_literal_chunks,
};

mod authoritative_evidence;
mod broad_setup;
mod comparison;
mod exact_literal_scope;
mod literal_selection;
mod preflight_answers;

/// Evidence fixture for the single-endpoint preflight answers: one endpoint
/// technical fact with a method qualifier, no chunk or structured rows.
fn single_endpoint_answer_evidence(
    document_id: Uuid,
    revision_id: Uuid,
) -> AuthoritativeAnswerEvidence {
    AuthoritativeAnswerEvidence {
        bundle: None,
        chunk_rows: Vec::new(),
        structured_blocks: Vec::new(),
        technical_facts: vec![KnowledgeTechnicalFactRow {
            fact_kind: "endpoint_path".to_string(),
            normalized_value_text: "/system/info".to_string(),
            normalized_value_exact: "/system/info".to_string(),
            normalized_value_json: json!("/system/info"),
            display_value: "/system/info".to_string(),
            qualifiers_json: json!([{ "key": "method", "value": "GET" }]),
            ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
        }],
    }
}
