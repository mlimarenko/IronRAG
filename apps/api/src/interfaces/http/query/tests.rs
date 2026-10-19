use super::turn_stream::ASSISTANT_TURN_TERMINAL_EVENT_RESERVE;
use super::ui_projection::UI_VISIBLE_REFERENCE_LIMIT;
use super::*;
use crate::domains::query::QueryTurn;
use std::collections::HashMap;

fn activity_event() -> AssistantActivityEvent {
    AssistantActivityEvent {
        event_type: "working",
        deadline_ms: None,
        iteration: None,
        provider_kind: None,
        model_name: None,
        tool_call_count: None,
        has_final_answer: None,
        tool_name: None,
        elapsed_ms: None,
        is_error: None,
        child_execution_id: None,
        result_preview: None,
    }
}

fn assistant_runtime_summary() -> ironrag_contracts::assistant::AssistantRuntimeSummary {
    let now = Utc::now();
    ironrag_contracts::assistant::AssistantRuntimeSummary {
        runtime_execution_id: Uuid::new_v4(),
        lifecycle_state: "completed".to_string(),
        active_stage: None,
        turn_budget: 1,
        turn_count: 1,
        parallel_action_limit: 1,
        failure_code: None,
        failure_summary_redacted: None,
        policy_summary: ironrag_contracts::assistant::AssistantPolicySummary {
            allow_count: 0,
            reject_count: 0,
            terminate_count: 0,
            recent_decisions: Vec::new(),
        },
        accepted_at: now,
        completed_at: Some(now),
    }
}

fn chunk_reference(rank: i32, id: u128) -> ironrag_contracts::assistant::AssistantChunkReference {
    ironrag_contracts::assistant::AssistantChunkReference {
        execution_id: Uuid::from_u128(1),
        chunk_id: Uuid::from_u128(id),
        rank,
        score: 1.0 / f64::from(rank.max(1)),
        document_id: None,
        document_title: None,
        section_path: Vec::new(),
        excerpt: None,
    }
}

fn prepared_segment_reference(
    rank: i32,
    id: u128,
) -> ironrag_contracts::assistant::AssistantPreparedSegmentReference {
    ironrag_contracts::assistant::AssistantPreparedSegmentReference {
        execution_id: Uuid::from_u128(1),
        segment_id: Uuid::from_u128(id),
        revision_id: Uuid::from_u128(id + 100),
        block_kind: "paragraph".to_string(),
        rank,
        score: 1.0 / f64::from(rank.max(1)),
        heading_trail: Vec::new(),
        section_path: Vec::new(),
        document_id: None,
        document_title: None,
        document_hint: None,
        source_access: None,
    }
}

fn technical_fact_reference(
    rank: i32,
    id: u128,
) -> ironrag_contracts::assistant::AssistantTechnicalFactReference {
    ironrag_contracts::assistant::AssistantTechnicalFactReference {
        execution_id: Uuid::from_u128(1),
        fact_id: Uuid::from_u128(id),
        revision_id: Uuid::from_u128(id + 100),
        fact_kind: "scalar".to_string(),
        normalized_value: id.to_string(),
        display_value: id.to_string(),
        rank,
        score: 1.0 / f64::from(rank.max(1)),
    }
}

fn entity_reference(rank: i32, id: u128) -> ironrag_contracts::assistant::AssistantEntityReference {
    ironrag_contracts::assistant::AssistantEntityReference {
        execution_id: Uuid::from_u128(1),
        node_id: Uuid::from_u128(id),
        rank,
        score: 1.0 / f64::from(rank.max(1)),
        label: id.to_string(),
        entity_type: None,
        summary: None,
    }
}

fn relation_reference(
    rank: i32,
    id: u128,
) -> ironrag_contracts::assistant::AssistantRelationReference {
    ironrag_contracts::assistant::AssistantRelationReference {
        execution_id: Uuid::from_u128(1),
        edge_id: Uuid::from_u128(id),
        rank,
        score: 1.0 / f64::from(rank.max(1)),
        predicate: id.to_string(),
        normalized_assertion: None,
    }
}

fn evidence_bundle_with_references(
    chunk_references: Vec<ironrag_contracts::assistant::AssistantChunkReference>,
    prepared_segment_references: Vec<
        ironrag_contracts::assistant::AssistantPreparedSegmentReference,
    >,
    technical_fact_references: Vec<ironrag_contracts::assistant::AssistantTechnicalFactReference>,
    entity_references: Vec<ironrag_contracts::assistant::AssistantEntityReference>,
    relation_references: Vec<ironrag_contracts::assistant::AssistantRelationReference>,
) -> ironrag_contracts::assistant::AssistantEvidenceBundle {
    ironrag_contracts::assistant::AssistantEvidenceBundle {
        chunk_references,
        prepared_segment_references,
        technical_fact_references,
        entity_references,
        relation_references,
        reference_summary: None,
        verification_state: ironrag_contracts::assistant::AssistantVerificationState::Verified,
        verification_warnings: Vec::new(),
        answer_disposition: ironrag_contracts::assistant::AssistantAnswerDisposition::FactualReady,
        clarification: ironrag_contracts::assistant::AssistantClarification::default(),
        runtime_summary: assistant_runtime_summary(),
        runtime_stage_summaries: Vec::new(),
    }
}

fn execution_detail_from_evidence(
    evidence: ironrag_contracts::assistant::AssistantEvidenceBundle,
) -> ironrag_contracts::assistant::AssistantExecutionDetail {
    let now = Utc::now();
    let context_bundle_id = Uuid::new_v4();
    let execution_id = Uuid::new_v4();
    ironrag_contracts::assistant::AssistantExecutionDetail {
        context_bundle_id,
        execution: ironrag_contracts::assistant::AssistantExecution {
            id: execution_id,
            workspace_id: Uuid::new_v4(),
            library_id: Uuid::new_v4(),
            conversation_id: Uuid::new_v4(),
            context_bundle_id,
            request_turn_id: None,
            response_turn_id: None,
            binding_id: None,
            runtime_execution_id: evidence.runtime_summary.runtime_execution_id,
            lifecycle_state: "completed".to_string(),
            active_stage: None,
            query_text: "neutral question".to_string(),
            failure_code: None,
            started_at: now,
        },
        runtime_summary: evidence.runtime_summary,
        runtime_stage_summaries: evidence.runtime_stage_summaries,
        request_turn: None,
        response_turn: None,
        chunk_references: evidence.chunk_references,
        prepared_segment_references: evidence.prepared_segment_references,
        technical_fact_references: evidence.technical_fact_references,
        entity_references: evidence.entity_references,
        relation_references: evidence.relation_references,
        reference_summary: evidence.reference_summary,
        verification_state: evidence.verification_state,
        verification_warnings: evidence.verification_warnings,
        answer_disposition: evidence.answer_disposition,
        clarification: evidence.clarification,
    }
}

fn query_execution_detail_with_chunks(count: usize) -> QueryExecutionDetail {
    let now = Utc::now();
    let execution = query_execution_with_state(
        crate::domains::agent_runtime::RuntimeLifecycleState::Completed,
        None,
        None,
    );
    let execution_id = execution.id;
    QueryExecutionDetail {
        runtime_summary: RuntimeExecutionSummary {
            runtime_execution_id: execution.runtime_execution_id,
            lifecycle_state: crate::domains::agent_runtime::RuntimeLifecycleState::Completed,
            active_stage: None,
            turn_budget: 1,
            turn_count: 1,
            parallel_action_limit: 1,
            failure_code: None,
            failure_summary_redacted: None,
            policy_summary: RuntimePolicySummary::default(),
            accepted_at: now,
            completed_at: Some(now),
        },
        runtime_stage_summaries: Vec::new(),
        request_turn: None,
        response_turn: None,
        chunk_references: (0..count)
            .map(|index| QueryChunkReference {
                execution_id,
                chunk_id: Uuid::from_u128(1_000 + index as u128),
                rank: i32::try_from(index + 1).expect("test rank fits i32"),
                score: 1.0 / (index + 1) as f64,
                document_id: None,
                document_title: None,
                section_path: Vec::new(),
                excerpt: None,
            })
            .collect(),
        prepared_segment_references: Vec::new(),
        technical_fact_references: Vec::new(),
        graph_node_references: Vec::new(),
        graph_edge_references: Vec::new(),
        verification_state: QueryVerificationState::Verified,
        verification_warnings: Vec::new(),
        answer_disposition: crate::domains::query::QueryAnswerDisposition::FactualReady,
        clarification: QueryClarification::default(),
        query_ir: None,
        execution,
    }
}

fn query_execution_with_state(
    lifecycle_state: crate::domains::agent_runtime::RuntimeLifecycleState,
    request_turn_id: Option<Uuid>,
    response_turn_id: Option<Uuid>,
) -> QueryExecution {
    QueryExecution {
        id: Uuid::new_v4(),
        workspace_id: Uuid::new_v4(),
        library_id: Uuid::new_v4(),
        conversation_id: Uuid::new_v4(),
        context_bundle_id: Uuid::new_v4(),
        request_turn_id,
        response_turn_id,
        binding_id: None,
        runtime_execution_id: Uuid::new_v4(),
        lifecycle_state,
        active_stage: None,
        query_text: "neutral question".to_string(),
        failure_code: None,
        started_at: Utc::now(),
    }
}

#[test]
fn pending_session_execution_maps_to_empty_assistant_message() {
    let request_turn_id = Uuid::new_v4();
    let observed_at = Utc::now();
    let mut execution = query_execution_with_state(
        crate::domains::agent_runtime::RuntimeLifecycleState::Running,
        Some(request_turn_id),
        None,
    );
    execution.started_at = observed_at - chrono::Duration::milliseconds(12_345);

    assert!(is_pending_session_execution(&execution));

    let message = map_pending_execution_to_message(&execution, observed_at);
    assert_eq!(message.id, execution.id);
    assert_eq!(message.role, ironrag_contracts::assistant::AssistantTurnRole::Assistant);
    assert!(message.content.is_empty());
    assert_eq!(message.timestamp, execution.started_at);
    assert_eq!(message.elapsed_ms, Some(12_345));
    assert_eq!(message.execution_id, Some(execution.id));
    assert!(message.evidence.is_none());
}

#[test]
fn unanswered_terminal_execution_maps_to_visible_assistant_error() {
    let request_turn_id = Uuid::new_v4();
    let mut execution = query_execution_with_state(
        crate::domains::agent_runtime::RuntimeLifecycleState::Failed,
        Some(request_turn_id),
        None,
    );
    execution.failure_code = Some("llm_timeout".to_string());

    assert!(is_unanswered_terminal_session_execution(&execution));
    assert!(!is_pending_session_execution(&execution));

    let message = map_unanswered_terminal_execution_to_message(&execution);
    assert_eq!(message.id, execution.id);
    assert_eq!(message.role, ironrag_contracts::assistant::AssistantTurnRole::Assistant);
    assert_eq!(message.content, "llm_timeout");
    assert_eq!(message.execution_id, Some(execution.id));
    assert!(message.elapsed_ms.is_none());
}

#[test]
fn conversation_messages_insert_followups_after_the_request_turn() {
    let conversation_id = Uuid::new_v4();
    let user_turn_id = Uuid::new_v4();
    let now = Utc::now();
    let turns = vec![QueryTurn {
        id: user_turn_id,
        conversation_id,
        turn_index: 1,
        turn_kind: crate::domains::query::QueryTurnKind::User,
        author_principal_id: None,
        content_text: "how do I update the register?".to_string(),
        execution_id: None,
        created_at: now,
    }];
    let mut failed = query_execution_with_state(
        crate::domains::agent_runtime::RuntimeLifecycleState::Failed,
        Some(user_turn_id),
        None,
    );
    failed.failure_code = Some("provider_unavailable".to_string());

    let messages = conversation_messages_from_turns(turns, HashMap::new(), vec![failed], now);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, ironrag_contracts::assistant::AssistantTurnRole::User);
    assert_eq!(messages[1].role, ironrag_contracts::assistant::AssistantTurnRole::Assistant);
    assert_eq!(messages[1].content, "provider_unavailable");
}

#[test]
fn terminal_or_answered_execution_is_not_session_pending() {
    let request_turn_id = Uuid::new_v4();
    let response_turn_id = Uuid::new_v4();
    let answered = query_execution_with_state(
        crate::domains::agent_runtime::RuntimeLifecycleState::Running,
        Some(request_turn_id),
        Some(response_turn_id),
    );
    let failed = query_execution_with_state(
        crate::domains::agent_runtime::RuntimeLifecycleState::Failed,
        Some(request_turn_id),
        None,
    );
    let orphan = query_execution_with_state(
        crate::domains::agent_runtime::RuntimeLifecycleState::Running,
        None,
        None,
    );

    assert!(!is_pending_session_execution(&answered));
    assert!(!is_pending_session_execution(&failed));
    assert!(!is_pending_session_execution(&orphan));
}

#[tokio::test]
async fn assistant_activity_stream_reserves_terminal_capacity() {
    let (sender, mut receiver) = tokio::sync::mpsc::channel::<AssistantTurnStreamEvent>(
        ASSISTANT_TURN_TERMINAL_EVENT_RESERVE + 2,
    );

    for _ in 0..100 {
        send_assistant_activity(&sender, activity_event());
    }

    assert_eq!(sender.capacity(), ASSISTANT_TURN_TERMINAL_EVENT_RESERVE);
    tokio::time::timeout(
        Duration::from_millis(50),
        send_required_turn_stream_event(
            &sender,
            AssistantTurnStreamEvent::Failed { code: "internal", message: "panic".to_string() },
        ),
    )
    .await
    .expect("terminal event should not wait behind activity backlog");

    let mut saw_failure = false;
    while let Ok(Some(event)) =
        tokio::time::timeout(Duration::from_millis(50), receiver.recv()).await
    {
        if matches!(event, AssistantTurnStreamEvent::Failed { .. }) {
            saw_failure = true;
            break;
        }
    }
    assert!(saw_failure);
}

#[test]
fn map_turn_to_message_preserves_hydrated_evidence() {
    let now = Utc::now();
    let execution_id = Uuid::new_v4();
    let evidence = ironrag_contracts::assistant::AssistantEvidenceBundle {
        chunk_references: Vec::new(),
        prepared_segment_references: Vec::new(),
        technical_fact_references: Vec::new(),
        entity_references: Vec::new(),
        relation_references: Vec::new(),
        reference_summary: None,
        verification_state: ironrag_contracts::assistant::AssistantVerificationState::Verified,
        verification_warnings: Vec::new(),
        answer_disposition: ironrag_contracts::assistant::AssistantAnswerDisposition::FactualReady,
        clarification: ironrag_contracts::assistant::AssistantClarification::default(),
        runtime_summary: ironrag_contracts::assistant::AssistantRuntimeSummary {
            runtime_execution_id: Uuid::new_v4(),
            lifecycle_state: "completed".to_string(),
            active_stage: None,
            turn_budget: 4,
            turn_count: 1,
            parallel_action_limit: 2,
            failure_code: None,
            failure_summary_redacted: None,
            policy_summary: ironrag_contracts::assistant::AssistantPolicySummary {
                allow_count: 0,
                reject_count: 0,
                terminate_count: 0,
                recent_decisions: Vec::new(),
            },
            accepted_at: now,
            completed_at: Some(now),
        },
        runtime_stage_summaries: vec![ironrag_contracts::assistant::AssistantRuntimeStageSummary {
            stage_kind: "retrieve".to_string(),
            stage_label: "Retrieve".to_string(),
            duration_ms: None,
        }],
    };

    let message = map_turn_to_message(
        QueryTurn {
            id: Uuid::new_v4(),
            conversation_id: Uuid::new_v4(),
            turn_index: 1,
            turn_kind: crate::domains::query::QueryTurnKind::Assistant,
            author_principal_id: None,
            content_text: "answer".to_string(),
            execution_id: Some(execution_id),
            created_at: now,
        },
        Some(evidence),
    );

    let hydrated_evidence =
        message.evidence.expect("hydrated session messages must retain assistant evidence");
    assert_eq!(message.execution_id, Some(execution_id));
    assert_eq!(
        hydrated_evidence.verification_state,
        ironrag_contracts::assistant::AssistantVerificationState::Verified
    );
    assert_eq!(hydrated_evidence.runtime_stage_summaries.len(), 1);
    assert_eq!(hydrated_evidence.runtime_stage_summaries[0].stage_kind, "retrieve");
}

#[test]
fn ui_reference_projection_caps_all_reference_kinds_globally() {
    let evidence = evidence_bundle_with_references(
        (0..8).map(|index| chunk_reference(20 + index, 10 + index as u128)).collect(),
        Vec::new(),
        (0..5).map(|index| technical_fact_reference(1 + index, 30 + index as u128)).collect(),
        Vec::new(),
        Vec::new(),
    );

    let projected = project_ui_evidence_bundle(evidence);
    let returned_count = projected.chunk_references.len()
        + projected.prepared_segment_references.len()
        + projected.technical_fact_references.len()
        + projected.entity_references.len()
        + projected.relation_references.len();

    assert_eq!(returned_count, UI_VISIBLE_REFERENCE_LIMIT);
    assert_eq!(projected.technical_fact_references.len(), 5);
    assert_eq!(projected.chunk_references.len(), 7);
    assert_eq!(
        projected.reference_summary,
        Some(ironrag_contracts::assistant::AssistantReferenceSummary {
            total_count: 13,
            returned_count: UI_VISIBLE_REFERENCE_LIMIT,
            truncated: true,
        })
    );
}

#[test]
fn ui_reference_projection_keeps_small_sets_without_truncation() {
    let evidence = evidence_bundle_with_references(
        vec![chunk_reference(2, 10)],
        vec![prepared_segment_reference(1, 20)],
        vec![technical_fact_reference(3, 30)],
        vec![entity_reference(4, 40)],
        vec![relation_reference(5, 50)],
    );

    let projected = project_ui_evidence_bundle(evidence);

    assert_eq!(projected.chunk_references.len(), 1);
    assert_eq!(projected.prepared_segment_references.len(), 1);
    assert_eq!(projected.technical_fact_references.len(), 1);
    assert_eq!(projected.entity_references.len(), 1);
    assert_eq!(projected.relation_references.len(), 1);
    assert_eq!(
        projected.reference_summary,
        Some(ironrag_contracts::assistant::AssistantReferenceSummary {
            total_count: 5,
            returned_count: 5,
            truncated: false,
        })
    );
}

#[test]
fn ui_reference_projection_breaks_rank_ties_by_kind_then_original_order() {
    let evidence = evidence_bundle_with_references(
        vec![chunk_reference(1, 12), chunk_reference(1, 11), chunk_reference(1, 10)],
        vec![
            prepared_segment_reference(1, 22),
            prepared_segment_reference(1, 21),
            prepared_segment_reference(1, 20),
        ],
        vec![
            technical_fact_reference(1, 32),
            technical_fact_reference(1, 31),
            technical_fact_reference(1, 30),
        ],
        vec![entity_reference(1, 42), entity_reference(1, 41), entity_reference(1, 40)],
        vec![relation_reference(1, 50)],
    );

    let projected = project_ui_evidence_bundle(evidence);

    assert_eq!(
        projected.chunk_references.iter().map(|reference| reference.chunk_id).collect::<Vec<_>>(),
        vec![Uuid::from_u128(12), Uuid::from_u128(11), Uuid::from_u128(10)]
    );
    assert_eq!(projected.entity_references.len(), 3);
    assert!(projected.relation_references.is_empty());
}

#[test]
fn ui_completion_and_hydration_serialize_bounded_reference_summaries() {
    let evidence = evidence_bundle_with_references(
        (0..13).map(|index| chunk_reference(index + 1, 100 + index as u128)).collect(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    let projected_evidence = project_ui_evidence_bundle(evidence.clone());
    let projected_detail = project_ui_execution_detail(execution_detail_from_evidence(evidence));

    let json_completion = serde_json::to_value(&projected_detail).expect("serialize completion");
    assert_eq!(json_completion["referenceSummary"]["totalCount"], 13);
    assert_eq!(json_completion["referenceSummary"]["returnedCount"], 12);
    assert_eq!(json_completion["chunkReferences"].as_array().map(Vec::len), Some(12));

    let stream_completion = serde_json::to_value(AssistantTurnStreamEvent::Completed {
        detail: Box::new(projected_detail),
    })
    .expect("serialize terminal stream event");
    assert_eq!(stream_completion["type"], "completed");
    assert_eq!(stream_completion["detail"]["referenceSummary"]["totalCount"], 13);
    assert_eq!(stream_completion["detail"]["chunkReferences"].as_array().map(Vec::len), Some(12));

    let hydrated = serde_json::to_value(projected_evidence).expect("serialize hydration");
    assert_eq!(hydrated["referenceSummary"]["totalCount"], 13);
    assert_eq!(hydrated["referenceSummary"]["returnedCount"], 12);
}

#[test]
fn unprojected_debug_detail_remains_unbounded_and_omits_ui_summary() {
    let evidence = evidence_bundle_with_references(
        (0..13).map(|index| chunk_reference(index + 1, 200 + index as u128)).collect(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    let debug_detail = execution_detail_from_evidence(evidence);

    let serialized = serde_json::to_value(debug_detail).expect("serialize debug detail");

    assert_eq!(serialized["chunkReferences"].as_array().map(Vec::len), Some(13));
    assert!(serialized.get("referenceSummary").is_none());
}

#[test]
fn session_hydration_projects_references_while_debug_mapping_stays_complete() {
    let durable_detail = query_execution_detail_with_chunks(13);

    let debug_detail = map_execution_detail(durable_detail.clone());
    let hydrated_evidence = map_execution_detail_to_evidence(durable_detail);

    assert_eq!(debug_detail.chunk_references.len(), 13);
    assert!(debug_detail.reference_summary.is_none());
    assert_eq!(hydrated_evidence.chunk_references.len(), 12);
    assert_eq!(
        hydrated_evidence.reference_summary,
        Some(ironrag_contracts::assistant::AssistantReferenceSummary {
            total_count: 13,
            returned_count: 12,
            truncated: true,
        })
    );
}

#[test]
fn session_hydration_preserves_typed_clarification_and_candidates() {
    let mut durable_detail = query_execution_detail_with_chunks(0);
    let candidate = QueryAnswerCandidate {
        label: "Neutral variant".to_string(),
        kind: "document".to_string(),
        confidence: Some(0.75),
        provenance: crate::domains::query::QueryAnswerCandidateProvenance::default(),
    };
    durable_detail.answer_disposition =
        crate::domains::query::QueryAnswerDisposition::Clarification;
    durable_detail.clarification = QueryClarification {
        required: true,
        question: Some("Which neutral variant?".to_string()),
        answer_candidates: vec![candidate],
    };

    let hydrated = map_execution_detail_to_evidence(durable_detail);

    assert_eq!(
        hydrated.answer_disposition,
        ironrag_contracts::assistant::AssistantAnswerDisposition::Clarification,
    );
    assert!(hydrated.clarification.required);
    assert_eq!(hydrated.clarification.question.as_deref(), Some("Which neutral variant?"));
    assert_eq!(hydrated.clarification.answer_candidates.len(), 1);
    assert_eq!(hydrated.clarification.answer_candidates[0].label, "Neutral variant");
}

#[test]
fn panic_payload_message_accepts_dyn_any_payload() {
    let payload: Box<dyn std::any::Any + Send> = Box::new("boom");

    assert_eq!(panic_payload_message(payload.as_ref()), "boom");
}

#[test]
fn a_failed_turn_event_carries_the_same_code_every_other_error_surface_uses() {
    // The screen has to say what happened in the reader's language. Prose alone
    // left it with nothing to key a translation off, so it apologised and said
    // "try again" — which is wrong advice for a library that is still indexing,
    // for a missing binding, and for every other reason a turn fails.
    let error = ApiError::knowledge_not_ready("library content is still being indexed");
    let event = AssistantTurnStreamEvent::Failed { code: error.kind(), message: error.to_string() };
    let encoded = serde_json::to_value(&event).expect("event serializes");

    assert_eq!(encoded["type"], "failed");
    assert_eq!(encoded["code"], "knowledge_not_ready");
    assert!(
        encoded["message"].as_str().is_some_and(|message| message.contains("still being indexed")),
        "the server's own words stay available as the fallback"
    );
}

#[tokio::test(start_paused = true)]
async fn progress_reporter_guard_aborts_loop_on_drop() {
    // `ASSISTANT_TURN_ACTIVITY_INTERVAL` is 5s; advance well past four
    // intervals. The 16-slot channel stays above the terminal-event reserve, so
    // a still-running loop would send and be observed. The test therefore fails
    // if the `AbortOnDrop` guard is replaced by a bare detached `JoinHandle`.
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let guard = super::turn_stream::spawn_progress_reporter(tx, tokio::time::Instant::now());

    // Let the loop register its first `sleep` interval before the drop, so the
    // clock advance below has a live timer to fire if the guard detaches
    // instead of aborting.
    tokio::task::yield_now().await;
    drop(guard);

    tokio::time::advance(std::time::Duration::from_secs(20)).await;
    tokio::task::yield_now().await;

    assert!(
        rx.try_recv().is_err(),
        "dropping the AbortOnDrop guard must abort the progress loop before it reports",
    );
}
