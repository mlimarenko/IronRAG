use super::*;

use crate::services::query::execution::answer_kind::AnswerKind;
use crate::services::query::execution::preflight::{
    DeterministicAnswerOverride, build_primary_preflight_answer_override,
};

fn broad_setup_chunk(
    document_id: Uuid,
    document_label: &str,
    source_text: &str,
    score: f32,
) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: Some("paragraph".to_string()),
        document_id,
        document_label: document_label.to_string(),
        excerpt: source_text.to_string(),
        score_kind: RuntimeChunkScoreKind::Relevance,
        score: Some(score),
        source_text: source_text.to_string(),
    }
}

fn broad_setup_chunks(
    primary_document_id: Uuid,
    secondary_document_id: Uuid,
) -> Vec<RuntimeMatchedChunk> {
    vec![
        broad_setup_chunk(
            primary_document_id,
            "Sample Connector Atlas setup",
            "Install atlas-adapter.pkg and set atlasMode=true.",
            10.0,
        ),
        broad_setup_chunk(
            secondary_document_id,
            "Sample Connector Boreal setup",
            "Install boreal-adapter.pkg and set borealMode=true.",
            1.0,
        ),
    ]
}

fn broad_setup_query_ir() -> QueryIR {
    let mut query_ir = query_ir_with_act_scope_and_target_types(
        QueryAct::ConfigureHow,
        QueryScope::SingleDocument,
        ["configuration_file", "config_key", "procedure"],
    );
    query_ir.target_entities =
        vec![EntityMention { label: "Sample Connector".to_string(), role: EntityRole::Subject }];
    query_ir
}

#[test]
fn broad_setup_preflight_keeps_multiple_document_variants_unscoped() {
    let primary_document_id = Uuid::now_v7();
    let secondary_document_id = Uuid::now_v7();
    let chunks = broad_setup_chunks(primary_document_id, secondary_document_id);
    let query_ir = broad_setup_query_ir();
    let profile = QueryIntentProfile { act: Some(QueryAct::ConfigureHow), ..Default::default() };

    assert!(
        preflight_exact_literal_document_scope(
            "How do I configure Sample Connector?",
            &query_ir,
            &profile,
            &chunks,
        )
        .is_none(),
        "a broad setup question must not scope preflight to one variant"
    );
    assert_eq!(
        setup_preflight_document_ids(
            "How do I configure Sample Connector?",
            &query_ir,
            &chunks,
            None,
        ),
        vec![primary_document_id, secondary_document_id],
    );
}

#[test]
fn broad_setup_literal_selection_preserves_document_variants() {
    let primary_document_id = Uuid::now_v7();
    let secondary_document_id = Uuid::now_v7();
    let chunks = broad_setup_chunks(primary_document_id, secondary_document_id);
    let query_ir = broad_setup_query_ir();

    let selected = select_technical_literal_chunks(TechnicalLiteralSelection {
        question: "How do I configure Sample Connector?",
        query_ir: &query_ir,
        chunks: &chunks,
        technical_literal_intent: TechnicalLiteralIntent {
            wants_paths: true,
            wants_parameters: true,
            ..Default::default()
        },
        top_k: 8,
        literal_focus_keywords: &[],
        preferred_document_ids: &[],
        pagination_requested: false,
    });
    let selected_document_ids =
        selected.iter().map(|chunk| chunk.document_id).collect::<HashSet<_>>();

    assert_eq!(selected_document_ids, HashSet::from([primary_document_id, secondary_document_id]),);
}

#[test]
fn broad_setup_preflight_caps_actionable_variants_and_rejects_noise() {
    let actionable_document_ids = (0..7).map(|_| Uuid::now_v7()).collect::<Vec<_>>();
    let noise_document_ids = (0..16).map(|_| Uuid::now_v7()).collect::<Vec<_>>();
    let mut chunks = actionable_document_ids
        .iter()
        .enumerate()
        .map(|(index, document_id)| RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: index as i32,
            chunk_kind: Some("paragraph".to_string()),
            document_id: *document_id,
            document_label: format!("Sample Connector variant {index}"),
            excerpt: format!("Install adapter-{index}.pkg and set mode{index}=true."),
            score_kind: RuntimeChunkScoreKind::Relevance,
            score: Some(10.0 - index as f32),
            source_text: format!("Install adapter-{index}.pkg and set mode{index}=true."),
        })
        .collect::<Vec<_>>();
    chunks.extend(noise_document_ids.iter().enumerate().map(|(index, document_id)| {
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: index as i32,
            chunk_kind: Some("paragraph".to_string()),
            document_id: *document_id,
            document_label: format!("Sample Connector overview {index}"),
            excerpt: "General architecture and background.".to_string(),
            score_kind: RuntimeChunkScoreKind::Relevance,
            score: Some(100.0 - index as f32),
            source_text: "General architecture and background.".to_string(),
        }
    }));
    let mut query_ir = query_ir_with_act_scope_and_target_types(
        QueryAct::ConfigureHow,
        QueryScope::SingleDocument,
        ["configuration_file", "config_key", "procedure"],
    );
    query_ir.target_entities =
        vec![EntityMention { label: "Sample Connector".to_string(), role: EntityRole::Subject }];

    let selected = setup_preflight_document_ids(
        "How do I configure Sample Connector?",
        &query_ir,
        &chunks,
        None,
    );

    assert_eq!(selected, actionable_document_ids[..5]);
    assert_eq!(selected.len(), 5);
    assert!(selected.iter().all(|document_id| !noise_document_ids.contains(document_id)));
}

#[test]
fn setup_preflight_keeps_explicit_single_document_scope() {
    let focused_document_id = Uuid::now_v7();
    let other_document_id = Uuid::now_v7();
    let chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: Some("paragraph".to_string()),
            document_id: focused_document_id,
            document_label: "Sample Connector focused setup".to_string(),
            excerpt: "Install focused-adapter.pkg and set focusedMode=true.".to_string(),
            score_kind: RuntimeChunkScoreKind::Relevance,
            score: Some(1.0),
            source_text: "Install focused-adapter.pkg and set focusedMode=true.".to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: Some("paragraph".to_string()),
            document_id: other_document_id,
            document_label: "Sample Connector other setup".to_string(),
            excerpt: "Install other-adapter.pkg and set otherMode=true.".to_string(),
            score_kind: RuntimeChunkScoreKind::Relevance,
            score: Some(10.0),
            source_text: "Install other-adapter.pkg and set otherMode=true.".to_string(),
        },
    ];
    let mut query_ir = query_ir_with_act_scope_and_target_types(
        QueryAct::ConfigureHow,
        QueryScope::SingleDocument,
        ["configuration_file", "config_key", "procedure"],
    );
    query_ir.document_focus =
        Some(DocumentHint { hint: "Sample Connector focused setup".to_string() });
    let scoped_document_ids = HashSet::from([focused_document_id]);

    assert_eq!(
        setup_preflight_document_ids(
            "How do I configure the focused setup?",
            &query_ir,
            &chunks,
            Some(&scoped_document_ids),
        ),
        vec![focused_document_id],
    );
}

#[test]
fn primary_preflight_answer_prefers_missing_explicit_document_before_other_paths() {
    let missing_override = DeterministicAnswerOverride {
        answer: "Document `missing-contract.md` is not present in the active library.".to_string(),
        answer_kind: AnswerKind::MissingExplicitDocument,
    };

    let answer =
        build_primary_preflight_answer_override(Some(missing_override), Some("table answer"))
            .expect("missing explicit document answer");

    assert!(answer.answer.contains("missing-contract.md"));
    assert_eq!(answer.answer_kind, AnswerKind::MissingExplicitDocument);
}

#[test]
fn evidence_preflight_answer_reuses_single_endpoint_override_for_live_path() {
    let document_id = Uuid::now_v7();
    let chunks = vec![RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id,
        document_label: "checkout_runtime_contract.md".to_string(),
        excerpt: "Send a GET request to /system/info to fetch the current checkout server info."
            .to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.97),
        source_text: repair_technical_layout_noise(
            "http://demo.local:8080/checkout-api/rest/system/info\nGET\n/system/info",
        ),
    }];

    let revision_id = Uuid::now_v7();
    let answer = build_evidence_preflight_answer(
        "Which endpoint returns the current checkout server info?",
        &query_ir_with_act_scope_literals_and_target_types(
            QueryAct::RetrieveValue,
            QueryScope::SingleDocument,
            ["current info", "checkout server"],
            ["endpoint"],
        ),
        &QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() },
        None,
        &single_endpoint_answer_evidence(document_id, revision_id),
        &chunks,
    )
    .expect("single endpoint preflight answer");

    assert_eq!(answer.answer, "The endpoint is `GET /system/info`.");
}

#[test]
fn evidence_preflight_answer_keeps_open_enumerations_generative() {
    let document_id = Uuid::now_v7();
    let mut query_ir = query_ir_with_act_scope_literals_and_target_types(
        QueryAct::Enumerate,
        QueryScope::SingleDocument,
        ["service plans"],
        ["concept"],
    );
    query_ir.retrieval_query = Some("which service plans are available".to_string());
    query_ir.confidence = 0.98;
    let chunks = vec![RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 8,
        chunk_kind: None,
        document_id,
        document_label: "service_overview.md".to_string(),
        excerpt: "Service plans include Free, Personal, and Business.".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::QueryIrFocus,
        score: Some(0.99),
        source_text: "Service plans:\n- Free plan\n- Personal plan\n- Business plan".to_string(),
    }];

    let answer = build_evidence_preflight_answer(
        "Which service plans are available?",
        &query_ir,
        &QueryIntentProfile::default(),
        None,
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &chunks,
    );

    assert!(answer.is_none(), "open content enumerations should use the generative answer path");
}
