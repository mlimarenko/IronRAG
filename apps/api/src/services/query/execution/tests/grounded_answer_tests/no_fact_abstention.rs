use super::*;

#[test]
fn build_deterministic_grounded_answer_does_not_infer_wsdl_from_chunks_without_fact() {
    let document_id = Uuid::now_v7();

    let answer = build_deterministic_grounded_answer(
        "Which WSDL does the inventory SOAP API use?",
        &generic_query_ir(),
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &[RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "inventory_soap_api_contract.md".to_string(),
            excerpt: "WSDL URL: http://demo.local:8080/inventory-api/ws/inventory.wsdl".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.98),
            source_text: repair_technical_layout_noise(
                "Inventory SOAP API Contract\nWSDL URL: http://demo.local:8080/inventory-api/ws/inventory.wsdl",
            ),
        }],
    );

    assert!(answer.is_none());
}

#[test]
fn build_deterministic_grounded_answer_does_not_infer_single_endpoint_from_chunks_without_fact() {
    let document_id = Uuid::now_v7();

    let answer = build_deterministic_grounded_answer(
        "Which endpoint returns current information for the checkout server?",
        &generic_query_ir(),
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &[RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "checkout_runtime_contract.md".to_string(),
            excerpt: "GET /system/info returns current information for the checkout server."
                .to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.94),
            source_text: repair_technical_layout_noise(
                "Checkout Runtime Contract\nGET\n/system/info\ncurrent checkout server system information",
            ),
        }],
    );

    assert!(answer.is_none());
}

#[test]
fn build_deterministic_grounded_answer_does_not_infer_multi_document_endpoints_from_chunks_without_facts()
 {
    let checkout_document_id = Uuid::now_v7();
    let rewards_document_id = Uuid::now_v7();

    let answer = build_deterministic_grounded_answer(
        "If an agent needs the current Checkout Server status and separately the Rewards Service account list, which two endpoints are needed?",
        &generic_query_ir(),
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
                document_label: "rewards_service_reference.pdf".to_string(),
                excerpt: "GET /v1/accounts returns the Rewards Service account list.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.94),
                source_text: repair_technical_layout_noise(
                    "/v1/accounts\nGET\nGet Rewards Service account list.",
                ),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: checkout_document_id,
                document_label: "checkout_server_reference.pdf".to_string(),
                excerpt:
                    "To get the current Checkout Server status, send a GET request to /system/info."
                        .to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.71),
                source_text: repair_technical_layout_noise(
                    "Public checkout server API.\nhttp://demo.local:8080/checkout-api/rest/system/info\nGET\n/system/info\nGet current Checkout Server status.",
                ),
            },
        ],
    );

    assert!(answer.is_none());
}

#[test]
fn build_deterministic_grounded_answer_does_not_infer_parameter_from_chunks_without_fact() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let block_id = Uuid::now_v7();

    let answer = build_deterministic_grounded_answer(
        "Does a withCards parameter exist?",
        &generic_query_ir(),
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: vec![KnowledgeStructuredBlockRow {
                normalized_text: "| `withCards` | include linked card records in the response |"
                    .to_string(),
                text: "| `withCards` | include linked card records in the response |".to_string(),
                ..sample_structured_block_row(block_id, document_id, revision_id)
            }],
            technical_facts: Vec::new(),
        },
        &[RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "rewards_accounts_api_contract.md".to_string(),
            excerpt: "withCards includes linked card records.".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.95),
            source_text: repair_technical_layout_noise(
                "| `withCards` | include linked card records in the response |",
            ),
        }],
    );

    assert!(answer.is_none());
}
