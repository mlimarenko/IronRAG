use super::transport_config::build_transport_config_assignment_answer;
use super::*;
use crate::domains::query_ir::{
    DocumentHint, EntityMention, EntityRole, QueryLanguage, QueryScope, SourceSliceSpec,
    UnresolvedRef,
};
use crate::services::query::execution::RuntimeChunkScoreKind;

#[path = "technical_answer_tests/configuration_setup.rs"]
mod configuration_setup;
#[path = "technical_answer_tests/transport_assignments.rs"]
mod transport_assignments;

fn configuration_setup_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::ConfigureHow,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::En,
        target_types: vec![
            crate::domains::query_ir::QueryTargetKind::Package,
            crate::domains::query_ir::QueryTargetKind::ConfigurationFile,
            crate::domains::query_ir::QueryTargetKind::Parameter,
        ],
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::<UnresolvedRef>::new(),
        needs_clarification: None,
        source_slice: Option::<SourceSliceSpec>::None,
        retrieval_query: None,
        confidence: 1.0,
    }
}

fn literal_constraint(text: &str) -> crate::domains::query_ir::LiteralSpan {
    crate::domains::query_ir::LiteralSpan {
        kind: crate::domains::query_ir::LiteralKind::Identifier,
        text: text.to_string(),
    }
}

fn empty_evidence() -> AuthoritativeAnswerEvidence {
    evidence_with_blocks(Vec::new())
}

fn evidence_with_blocks(blocks: Vec<KnowledgeStructuredBlockRow>) -> AuthoritativeAnswerEvidence {
    AuthoritativeAnswerEvidence {
        bundle: None,
        chunk_rows: Vec::new(),
        structured_blocks: blocks,
        technical_facts: Vec::new(),
    }
}

fn runtime_chunk(document_id: Uuid, index: i32, label: &str, text: &str) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id,
        revision_id: Uuid::now_v7(),
        chunk_index: index,
        chunk_kind: None,
        document_label: label.to_string(),
        excerpt: text.to_string(),
        score_kind: RuntimeChunkScoreKind::Relevance,
        score: Some(1.0),
        source_text: text.to_string(),
    }
}
