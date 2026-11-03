use super::*;

#[test]
fn pre_verification_transcript_has_no_final_body_and_final_snapshot_retains_all_paid_iterations() {
    use crate::{
        integrations::llm::ChatMessage, services::query::llm_context_debug::LlmIterationDebug,
    };
    let iteration = |number| LlmIterationDebug {
        iteration: number,
        provider_kind: "fixture".into(),
        model_name: "fixture".into(),
        request_messages: vec![ChatMessage::user("Question")],
        response_text: Some("Paid candidate response".into()),
        response_tool_calls: Vec::new(),
        usage: serde_json::json!({"prompt_tokens":11,"completion_tokens":7}),
        duration_ms: Some(3),
        child_runtime_execution_ids: Vec::new(),
        child_query_execution_ids: Vec::new(),
    };
    let mut result = AgentTurnResult {
        answer: "Raw candidate".into(),
        answer_provenance: AgentAnswerProvenance::Composed,
        grounded_answer_outcome: None,
        usage_json: serde_json::json!({}),
        provider_calls: Vec::new(),
        assistant_grounding: AssistantGroundingEvidence::default(),
        child_query_execution_ids: Vec::new(),
        debug_iterations: vec![iteration(1)],
        agent_loop: None,
    };
    let id = Uuid::from_u128(1);
    let initial = agent_debug_snapshot(id, id, "Question", &result, None);
    assert_eq!(initial.total_iterations, 1);
    assert_eq!(initial.iterations[0].usage["completion_tokens"], 7);
    assert_eq!(initial.final_answer, None);
    result.debug_iterations.push(iteration(2));
    result.answer = "Final visible body".into();
    let final_snapshot = agent_debug_snapshot(id, id, "Question", &result, Some(&result.answer));
    assert_eq!(final_snapshot.total_iterations, 2);
    assert_eq!(final_snapshot.iterations.len(), 2);
    assert_eq!(final_snapshot.final_answer.as_deref(), Some("Final visible body"));
    assert_eq!(initial.iterations.len(), 1);
    assert_eq!(initial.final_answer, None);
}
