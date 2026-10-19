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

    let first = context.snapshot("candidate answer");
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

    let first = context.snapshot("candidate answer");
    let second = context.snapshot_with_iterations("verified answer", Vec::new(), 1);

    assert_eq!(first.query_ir, Some(serde_json::json!({ "act": "configure_how" })));
    assert_eq!(second.query_ir, Some(serde_json::json!({ "act": "configure_how" })));
    assert_eq!(second.final_answer.as_deref(), Some("verified answer"));
}
