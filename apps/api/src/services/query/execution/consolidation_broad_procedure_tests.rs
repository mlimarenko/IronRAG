use super::{RetrievalBundle, decide_focus};
use crate::domains::query_ir::{EntityMention, EntityRole, QueryScope, QueryTargetKind};
use uuid::Uuid;

fn ir(scope: QueryScope) -> crate::domains::query_ir::QueryIR {
    crate::domains::query_ir::QueryIR {
        act: crate::domains::query_ir::QueryAct::ConfigureHow,
        scope,
        language: crate::domains::query_ir::QueryLanguage::Auto,
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
        confidence: 0.9,
    }
}

fn text_chunk(
    document_id: Uuid,
    revision_id: Uuid,
    chunk_index: i32,
    document_label: &str,
    score: f32,
    text: &str,
) -> super::RuntimeMatchedChunk {
    super::RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id,
        revision_id,
        chunk_index,
        chunk_kind: None,
        document_label: document_label.to_string(),
        excerpt: text.to_string(),
        score_kind: super::RuntimeChunkScoreKind::Relevance,
        score: Some(score),
        source_text: text.to_string(),
    }
}

fn broad_procedure_bundle(
    dominant_title: &str,
    dominant_text: &str,
    companion_title: &str,
    companion_text: &str,
) -> RetrievalBundle {
    let dominant_document_id = Uuid::now_v7();
    let dominant_revision_id = Uuid::now_v7();
    RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            text_chunk(
                dominant_document_id,
                dominant_revision_id,
                0,
                dominant_title,
                10.0,
                dominant_text,
            ),
            text_chunk(
                dominant_document_id,
                dominant_revision_id,
                1,
                dominant_title,
                9.0,
                "Additional evidence from the dominant variant.",
            ),
            text_chunk(Uuid::now_v7(), Uuid::now_v7(), 0, companion_title, 1.0, companion_text),
        ],
    }
}

fn broad_procedure_query_ir() -> crate::domains::query_ir::QueryIR {
    let mut query_ir = ir(QueryScope::SingleDocument);
    query_ir.target_types = vec![QueryTargetKind::Concept, QueryTargetKind::Procedure];
    query_ir.target_entities =
        vec![EntityMention { label: "Sample Connector".to_string(), role: EntityRole::Subject }];
    query_ir
}

#[test]
fn broad_concept_procedure_stays_broad_despite_subject_title_dominance() {
    let bundle = broad_procedure_bundle(
        "Sample Connector Atlas setup",
        "Sample Connector provides an ordered Atlas procedure.",
        "Connector Boreal setup",
        "Boreal provides another ordered procedure.",
    );
    let query_ir = broad_procedure_query_ir();

    assert!(
        decide_focus(&bundle, &query_ir, "How do I configure Sample Connector?", 8).is_none(),
        "soft subject-title dominance must not collapse a broad typed procedure"
    );
}

#[test]
fn broad_concept_procedure_does_not_collapse_ranked_variants() {
    let bundle = broad_procedure_bundle(
        "Sample Connector Atlas setup",
        "1. Prepare Atlas. 2. Configure Atlas. 3. Validate Atlas.",
        "Sample Connector Boreal setup",
        "1. Prepare Boreal. 2. Configure Boreal. 3. Validate Boreal.",
    );
    let query_ir = broad_procedure_query_ir();

    assert!(
        decide_focus(&bundle, &query_ir, "How do I configure Sample Connector?", 8).is_none(),
        "a broad typed procedure must preserve distinct retrieved variants"
    );
}
