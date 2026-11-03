use super::answer_snapshot::AnswerSnapshotContext;
use super::test_support::{prepared_for_single_shot, sample_ir};
use crate::services::query::turn_spans::TurnSpan;
use uuid::Uuid;

fn retrieval_span(name: &str) -> TurnSpan {
    TurnSpan {
        name: name.to_string(),
        kind: "db".to_string(),
        duration_ms: 12,
        started_offset_ms: 3,
        detail: None,
        rows: Some(7),
    }
}

/// An execution whose retrieval actually recorded timings.
///
/// The pipeline fixture builds a prepared result with `retrieval_spans` empty,
/// which is also what the defect leaves behind — a test that reuses the fixture
/// as-is passes whether or not the spans survive, so it proves nothing. Seed
/// the spans explicitly.
fn context_with_recorded_spans(execution_id: Uuid) -> AnswerSnapshotContext {
    let mut prepared = prepared_for_single_shot(sample_ir(0.9, None));
    prepared.retrieval_spans =
        vec![retrieval_span("db.knowledge_chunk"), retrieval_span("retrieve.vector")];
    AnswerSnapshotContext::new(
        execution_id,
        Uuid::now_v7(),
        "how is it configured?",
        serde_json::json!({ "act": "configure_how" }),
        &prepared,
    )
}

#[test]
fn every_write_of_one_execution_carries_the_retrieval_spans() {
    // The deterministic-override path and the agent verification path both
    // write the row for one execution_id twice: a candidate answer, then the
    // verified one. Each write replaces the row wholesale, so the second must
    // still carry the retrieval timings the first one did.
    let execution_id = Uuid::now_v7();
    let context = context_with_recorded_spans(execution_id);

    let first = context.snapshot("candidate answer").expect("snapshot must be available");
    let second = context.snapshot_with_iterations("verified answer", Vec::new(), 1);

    let first_names: Vec<&str> = first.spans.iter().map(|span| span.name.as_str()).collect();
    assert_eq!(
        first_names,
        vec!["db.knowledge_chunk", "retrieve.vector"],
        "first write must carry the retrieval spans"
    );
    let second_names: Vec<&str> = second.spans.iter().map(|span| span.name.as_str()).collect();
    assert_eq!(
        second_names,
        vec!["db.knowledge_chunk", "retrieve.vector"],
        "second write of the same execution must not blank the retrieval spans"
    );
    assert_eq!(second.execution_id, execution_id);
    assert_eq!(second.spans[0].rows, Some(7));
}

#[test]
fn every_write_of_one_execution_carries_the_compiled_query_ir() {
    // Same replace-whole-row hazard for the other durable field the caller must
    // supply on every write.
    let context = context_with_recorded_spans(Uuid::now_v7());

    let first = context.snapshot("candidate answer").expect("snapshot must be available");
    let second = context.snapshot_with_iterations("verified answer", Vec::new(), 1);

    assert_eq!(first.query_ir, Some(serde_json::json!({ "act": "configure_how" })));
    assert_eq!(second.query_ir, Some(serde_json::json!({ "act": "configure_how" })));
    assert_eq!(second.final_answer.as_deref(), Some("verified answer"));
}

#[test]
fn selected_fallback_snapshot_retains_semantic_iteration_and_retrieval_spans() {
    let context = context_with_recorded_spans(Uuid::now_v7());
    let semantic = crate::services::query::llm_context_debug::LlmIterationDebug {
        iteration: 0,
        provider_kind: "neutral".into(),
        model_name: "neutral-model".into(),
        request_messages: vec![crate::integrations::llm::ChatMessage::user("A semantic request.")],
        response_text: Some("A paid structured judgment.".into()),
        response_tool_calls: Vec::new(),
        usage: serde_json::json!({"prompt_tokens":10,"completion_tokens":11}),
        duration_ms: Some(1),
        child_runtime_execution_ids: Vec::new(),
        child_query_execution_ids: Vec::new(),
    };
    let snapshot = context.snapshot_with_iterations("A final visible fallback.", vec![semantic], 1);
    assert_eq!(snapshot.final_answer.as_deref(), Some("A final visible fallback."));
    assert_eq!(snapshot.total_iterations, 1);
    assert_eq!(
        snapshot.iterations[0].response_text.as_deref(),
        Some("A paid structured judgment.")
    );
    assert_eq!(snapshot.iterations[0].usage["completion_tokens"], 11);
    assert_eq!(snapshot.spans.len(), 2);
    assert_eq!(snapshot.spans[0].name, "db.knowledge_chunk");
}

#[test]
fn rejected_draft_transcripts_survive_replacement_by_the_selected_candidate() {
    let context = context_with_recorded_spans(Uuid::now_v7());
    let iteration = |text: &str| crate::services::query::llm_context_debug::LlmIterationDebug {
        iteration: 0,
        provider_kind: "neutral".into(),
        model_name: "neutral-model".into(),
        request_messages: Vec::new(),
        response_text: Some(text.into()),
        response_tool_calls: Vec::new(),
        usage: serde_json::json!({"completion_tokens":1}),
        duration_ms: Some(1),
        child_runtime_execution_ids: Vec::new(),
        child_query_execution_ids: Vec::new(),
    };
    context
        .record_iterations(vec![iteration("Rejected draft.")])
        .expect("retain rejected paid draft");
    context
        .record_iterations(vec![iteration("Selected candidate.")])
        .expect("retain selected paid draft");
    let mut all = context.take_iterations().expect("selected owner drains transcript");
    all.push(iteration("Semantic judgment."));
    let snapshot = context.snapshot_with_iterations("A visible fallback.", all, 3);
    assert_eq!(
        snapshot
            .iterations
            .iter()
            .map(|iteration| iteration.response_text.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("Rejected draft."), Some("Selected candidate."), Some("Semantic judgment.")]
    );
    assert_eq!(snapshot.iterations[0].iteration, 1);
    assert_eq!(snapshot.iterations[1].iteration, 2);
    assert_eq!(snapshot.final_answer.as_deref(), Some("A visible fallback."));
    assert_eq!(snapshot.spans.len(), 2);
}
