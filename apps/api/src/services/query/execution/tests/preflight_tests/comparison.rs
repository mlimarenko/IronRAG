use super::*;

#[test]
fn cross_document_comparison_never_gets_a_deterministic_answer() {
    let rewards_document_id = Uuid::now_v7();
    let inventory_document_id = Uuid::now_v7();
    let question = "How does the REST API for rewards accounts differ from the inventory WSDL transport contract?";
    let answer = build_evidence_preflight_answer(
        question,
        &query_ir_with_act_scope_and_target_types(
            QueryAct::Compare,
            QueryScope::MultiDocument,
            ["protocol"],
        ),
        &QueryIntentProfile::default(),
        None,
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: rewards_document_id,
                document_label: "rewards_accounts_api_contract.md".to_string(),
                excerpt: "REST JSON over HTTP".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.99),
                source_text:
                    "The rewards accounts surface is a REST API that returns JSON over HTTP."
                        .to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: inventory_document_id,
                document_label: "inventory_soap_api_contract.md".to_string(),
                excerpt: "SOAP WSDL over HTTP".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.97),
                source_text:
                    "The inventory integration surface is SOAP over HTTP and described by WSDL."
                        .to_string(),
            },
        ],
    );

    assert!(
        answer.is_none(),
        "a cross-document comparison is a synthesis task; collated excerpts \
         must never finalize it: {answer:?}"
    );
}
