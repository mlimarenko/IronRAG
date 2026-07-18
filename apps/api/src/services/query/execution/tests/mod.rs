use std::collections::{HashMap, HashSet};

use super::*;
use serde_json::json;

use super::context::build_grouped_reference_candidates;
use super::preflight::select_technical_literal_chunks;
use super::retrieve::expanded_candidate_limit;
use super::technical_literals::{TechnicalLiteralIntent, technical_literal_candidate_limit};
use super::types::QueryExecutionEnrichment;
use crate::domains::query_ir::{
    ComparisonSpec, DocumentHint, EntityMention, EntityRole, LiteralKind, LiteralSpan, QueryAct,
    QueryIR, QueryLanguage, QueryScope, QueryTargetKind, TemporalConstraint,
};
use crate::infra::knowledge_rows::{
    KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeEvidenceRow, KnowledgeLibraryGenerationRow,
    KnowledgeStructuredBlockRow, KnowledgeTechnicalFactRow,
};
use crate::services::query::execution::technical_literals::{
    detect_technical_literal_intent_from_query_ir, extract_config_section_literals,
    extract_explicit_path_literals, extract_http_methods, extract_parameter_literals,
    extract_url_literals,
};
use crate::services::query::{
    assistant_grounding::AssistantGroundingEvidence,
    planner::{QueryIntentProfile, RuntimeQueryPlan},
    support::RerankOutcome,
};
use crate::shared::extraction::text_render::repair_technical_layout_noise;

mod answer_pipeline_tests;
mod focused_document_tests;
mod grounded_answer_tests;
mod misc_tests;
mod post_retrieval_selection_tests;
mod preflight_tests;
mod setup_configuration_variant_tests;
mod technical_literal_tests;
mod verification_tests;

/// Descriptive/lenient QueryIR for test callsites that don't care about
/// IR-driven filtering.
fn generic_query_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::Describe,
        scope: QueryScope::MultiDocument,
        language: QueryLanguage::Auto,
        target_types: Vec::new(),
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.0,
    }
}

fn query_ir_with_scope_and_target_types<const N: usize>(
    scope: QueryScope,
    target_types: [&str; N],
) -> QueryIR {
    QueryIR {
        scope,
        target_types: target_types
            .into_iter()
            .map(|value| {
                QueryTargetKind::from_wire(value).expect("test target kind must use the wire enum")
            })
            .collect(),
        ..generic_query_ir()
    }
}

fn query_ir_with_act_scope_and_target_types<const N: usize>(
    act: QueryAct,
    scope: QueryScope,
    target_types: [&str; N],
) -> QueryIR {
    QueryIR { act, ..query_ir_with_scope_and_target_types(scope, target_types) }
}

fn query_ir_with_act_scope_literals_and_target_types<const L: usize, const T: usize>(
    act: QueryAct,
    scope: QueryScope,
    phrases: [&str; L],
    target_types: [&str; T],
) -> QueryIR {
    QueryIR { act, ..query_ir_with_scope_literals_and_target_types(scope, phrases, target_types) }
}

fn query_ir_with_literals_and_target_types<const L: usize, const T: usize>(
    phrases: [&str; L],
    target_types: [&str; T],
) -> QueryIR {
    query_ir_with_scope_literals_and_target_types(QueryScope::SingleDocument, phrases, target_types)
}

fn query_ir_with_scope_literals_and_target_types<const L: usize, const T: usize>(
    scope: QueryScope,
    phrases: [&str; L],
    target_types: [&str; T],
) -> QueryIR {
    QueryIR {
        literal_constraints: phrases
            .into_iter()
            .map(|phrase| LiteralSpan { text: phrase.to_string(), kind: LiteralKind::Other })
            .collect(),
        target_types: target_types
            .into_iter()
            .map(|value| {
                QueryTargetKind::from_wire(value).expect("test target kind must use the wire enum")
            })
            .collect(),
        scope,
        ..generic_query_ir()
    }
}

fn sample_document_row_for_preflight(document_id: Uuid, file_name: &str) -> KnowledgeDocumentRow {
    KnowledgeDocumentRow {
        document_id,
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        external_key: document_id.to_string(),
        file_name: Some(file_name.to_string()),
        title: Some(file_name.to_string()),
        source_uri: None,
        document_hint: None,
        document_state: "active".to_string(),
        active_revision_id: Some(Uuid::now_v7()),
        readable_revision_id: Some(Uuid::now_v7()),
        latest_revision_no: Some(1),
        parent_document_id: None,
        document_role: crate::domains::content::DOCUMENT_ROLE_PRIMARY.to_string(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        deleted_at: None,
    }
}
