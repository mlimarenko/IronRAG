use super::*;

pub(crate) fn map_session_list_item_with_turn_count(
    session: QueryConversation,
    turn_count: usize,
) -> ironrag_contracts::assistant::AssistantSessionListItem {
    ironrag_contracts::assistant::AssistantSessionListItem {
        id: session.id,
        workspace_id: session.workspace_id,
        library_id: session.library_id,
        title: session.title.unwrap_or_default(),
        updated_at: session.updated_at,
        created_at: session.created_at,
        conversation_state: session.conversation_state.as_str().to_string(),
        turn_count: i32::try_from(turn_count).unwrap_or(i32::MAX),
    }
}

pub(super) async fn map_session_detail(
    state: &AppState,
    detail: QueryConversationDetail,
) -> Result<ironrag_contracts::assistant::AssistantHydratedConversation, ApiError> {
    let QueryConversationDetail { conversation, turns, executions } = detail;
    let workspace_id = conversation.workspace_id;
    let library_id = conversation.library_id;
    let turn_count = turns.len();
    let evidence_by_turn_id =
        hydrate_session_message_evidence(state, &turns, workspace_id, library_id).await?;
    let messages =
        conversation_messages_from_turns(turns, evidence_by_turn_id, executions, Utc::now());
    Ok(ironrag_contracts::assistant::AssistantHydratedConversation {
        session: map_session_list_item_with_turn_count(conversation, turn_count),
        messages,
    })
}

pub(super) fn conversation_messages_from_turns(
    turns: Vec<QueryTurn>,
    mut evidence_by_turn_id: HashMap<Uuid, ironrag_contracts::assistant::AssistantEvidenceBundle>,
    executions: Vec<QueryExecution>,
    observed_at: chrono::DateTime<Utc>,
) -> Vec<ironrag_contracts::assistant::AssistantConversationMessage> {
    let mut pending_execution_by_request_turn_id = HashMap::new();
    let mut unanswered_terminal_by_request_turn_id = HashMap::new();
    for execution in executions {
        let Some(request_turn_id) = execution.request_turn_id else {
            continue;
        };
        if is_pending_session_execution(&execution) {
            pending_execution_by_request_turn_id.insert(request_turn_id, execution);
        } else if is_unanswered_terminal_session_execution(&execution) {
            unanswered_terminal_by_request_turn_id.insert(request_turn_id, execution);
        }
    }
    let mut messages = Vec::with_capacity(
        turns.len()
            + pending_execution_by_request_turn_id.len()
            + unanswered_terminal_by_request_turn_id.len(),
    );
    for turn in turns {
        let turn_id = turn.id;
        let evidence = evidence_by_turn_id.remove(&turn_id);
        messages.push(map_turn_to_message(turn, evidence));
        if let Some(execution) = pending_execution_by_request_turn_id.get(&turn_id) {
            messages.push(map_pending_execution_to_message(execution, observed_at));
        } else if let Some(execution) = unanswered_terminal_by_request_turn_id.get(&turn_id) {
            messages.push(map_unanswered_terminal_execution_to_message(execution));
        }
    }
    messages
}

pub(super) const fn is_pending_session_execution(execution: &QueryExecution) -> bool {
    execution.request_turn_id.is_some()
        && execution.response_turn_id.is_none()
        && !execution.lifecycle_state.is_terminal()
}

pub(super) const fn is_unanswered_terminal_session_execution(execution: &QueryExecution) -> bool {
    execution.request_turn_id.is_some()
        && execution.response_turn_id.is_none()
        && execution.lifecycle_state.is_terminal()
}

pub(super) const UNANSWERED_TURN_FALLBACK: &str =
    "The assistant could not complete that request. Please try again.";

pub(super) async fn hydrate_session_message_evidence(
    state: &AppState,
    turns: &[QueryTurn],
    workspace_id: Uuid,
    library_id: Uuid,
) -> Result<HashMap<Uuid, ironrag_contracts::assistant::AssistantEvidenceBundle>, ApiError> {
    let mut evidence_by_turn_id = HashMap::new();
    for turn in turns {
        if !matches!(turn.turn_kind, crate::domains::query::QueryTurnKind::Assistant) {
            continue;
        }
        let Some(execution_id) = turn.execution_id else {
            continue;
        };
        let detail = state.application_services.query.get_execution(state, execution_id).await?;
        if detail.execution.workspace_id != workspace_id
            || detail.execution.library_id != library_id
        {
            return Err(ApiError::internal_with_log(
                format!(
                    "query turn {} points to execution {} outside workspace/library scope",
                    turn.id, execution_id
                ),
                "query session evidence ownership mismatch",
            ));
        }
        evidence_by_turn_id.insert(turn.id, map_execution_detail_to_evidence(detail));
    }
    Ok(evidence_by_turn_id)
}

pub(super) fn map_execution_detail(
    detail: QueryExecutionDetail,
) -> ironrag_contracts::assistant::AssistantExecutionDetail {
    let QueryExecutionDetail {
        execution,
        runtime_summary,
        runtime_stage_summaries,
        request_turn,
        response_turn,
        chunk_references,
        prepared_segment_references,
        technical_fact_references,
        graph_node_references,
        graph_edge_references,
        verification_state,
        verification_warnings,
        answer_disposition,
        clarification,
        query_ir: _,
    } = detail;
    let context_bundle_id = execution.context_bundle_id;
    let execution = map_execution(execution);
    let request_turn = request_turn.map(map_turn);
    let response_turn = response_turn.map(map_turn);
    let evidence = map_execution_evidence_parts(ExecutionEvidenceParts {
        runtime_summary,
        runtime_stage_summaries,
        chunk_references,
        prepared_segment_references,
        technical_fact_references,
        graph_node_references,
        graph_edge_references,
        verification_state,
        verification_warnings,
        answer_disposition,
        clarification,
    });
    ironrag_contracts::assistant::AssistantExecutionDetail {
        context_bundle_id,
        execution,
        runtime_summary: evidence.runtime_summary,
        runtime_stage_summaries: evidence.runtime_stage_summaries,
        request_turn,
        response_turn,
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

pub(super) fn map_execution_detail_to_evidence(
    detail: QueryExecutionDetail,
) -> ironrag_contracts::assistant::AssistantEvidenceBundle {
    project_ui_evidence_bundle(map_execution_evidence_parts(ExecutionEvidenceParts {
        runtime_summary: detail.runtime_summary,
        runtime_stage_summaries: detail.runtime_stage_summaries,
        chunk_references: detail.chunk_references,
        prepared_segment_references: detail.prepared_segment_references,
        technical_fact_references: detail.technical_fact_references,
        graph_node_references: detail.graph_node_references,
        graph_edge_references: detail.graph_edge_references,
        verification_state: detail.verification_state,
        verification_warnings: detail.verification_warnings,
        answer_disposition: detail.answer_disposition,
        clarification: detail.clarification,
    }))
}

/// The already-resolved reference collections plus the verification and
/// answer-disposition metadata of one execution, grouped so the evidence
/// mapper takes a single cohesive value instead of eleven positional ones.
struct ExecutionEvidenceParts {
    runtime_summary: RuntimeExecutionSummary,
    runtime_stage_summaries: Vec<QueryRuntimeStageSummary>,
    chunk_references: Vec<QueryChunkReference>,
    prepared_segment_references: Vec<PreparedSegmentReference>,
    technical_fact_references: Vec<TechnicalFactReference>,
    graph_node_references: Vec<QueryGraphNodeReference>,
    graph_edge_references: Vec<QueryGraphEdgeReference>,
    verification_state: QueryVerificationState,
    verification_warnings: Vec<QueryVerificationWarning>,
    answer_disposition: crate::domains::query::QueryAnswerDisposition,
    clarification: QueryClarification,
}

fn map_execution_evidence_parts(
    parts: ExecutionEvidenceParts,
) -> ironrag_contracts::assistant::AssistantEvidenceBundle {
    let ExecutionEvidenceParts {
        runtime_summary,
        runtime_stage_summaries,
        chunk_references,
        prepared_segment_references,
        technical_fact_references,
        graph_node_references,
        graph_edge_references,
        verification_state,
        verification_warnings,
        answer_disposition,
        clarification,
    } = parts;
    ironrag_contracts::assistant::AssistantEvidenceBundle {
        chunk_references: chunk_references.into_iter().map(map_chunk_reference).collect(),
        prepared_segment_references: prepared_segment_references
            .into_iter()
            .map(map_prepared_segment_reference)
            .collect(),
        technical_fact_references: technical_fact_references
            .into_iter()
            .map(map_technical_fact_reference)
            .collect(),
        entity_references: graph_node_references
            .into_iter()
            .map(map_graph_node_reference)
            .collect(),
        relation_references: graph_edge_references
            .into_iter()
            .map(map_graph_edge_reference)
            .collect(),
        reference_summary: None,
        verification_state: map_verification_state(verification_state),
        verification_warnings: verification_warnings
            .into_iter()
            .map(map_verification_warning)
            .collect(),
        answer_disposition: map_answer_disposition(answer_disposition),
        clarification: map_query_clarification(clarification),
        runtime_summary: map_runtime_summary(runtime_summary),
        runtime_stage_summaries: runtime_stage_summaries
            .into_iter()
            .map(map_runtime_stage_summary)
            .collect(),
    }
}

pub(crate) fn map_turn_execution_response(
    outcome: crate::services::query::service::QueryTurnExecutionResult,
) -> ironrag_contracts::assistant::AssistantExecutionDetail {
    ironrag_contracts::assistant::AssistantExecutionDetail {
        context_bundle_id: outcome.context_bundle_id,
        execution: map_execution(outcome.execution),
        runtime_summary: map_runtime_summary(outcome.runtime_summary),
        runtime_stage_summaries: outcome
            .runtime_stage_summaries
            .into_iter()
            .map(map_runtime_stage_summary)
            .collect(),
        request_turn: Some(map_turn(outcome.request_turn)),
        response_turn: outcome.response_turn.map(map_turn),
        chunk_references: outcome.chunk_references.into_iter().map(map_chunk_reference).collect(),
        prepared_segment_references: outcome
            .prepared_segment_references
            .into_iter()
            .map(map_prepared_segment_reference)
            .collect(),
        technical_fact_references: outcome
            .technical_fact_references
            .into_iter()
            .map(map_technical_fact_reference)
            .collect(),
        entity_references: outcome
            .graph_node_references
            .into_iter()
            .map(map_graph_node_reference)
            .collect(),
        relation_references: outcome
            .graph_edge_references
            .into_iter()
            .map(map_graph_edge_reference)
            .collect(),
        reference_summary: None,
        verification_state: map_verification_state(outcome.verification_state),
        verification_warnings: outcome
            .verification_warnings
            .into_iter()
            .map(map_verification_warning)
            .collect(),
        answer_disposition: map_answer_disposition(outcome.answer_disposition),
        clarification: map_query_clarification(outcome.clarification),
    }
}

pub(super) fn map_ui_turn_execution_response(
    outcome: crate::services::query::service::QueryTurnExecutionResult,
) -> ironrag_contracts::assistant::AssistantExecutionDetail {
    project_ui_execution_detail(map_turn_execution_response(outcome))
}

pub(super) fn map_query_clarification(
    clarification: QueryClarification,
) -> ironrag_contracts::assistant::AssistantClarification {
    ironrag_contracts::assistant::AssistantClarification {
        required: clarification.required,
        question: clarification.question,
        answer_candidates: clarification
            .answer_candidates
            .into_iter()
            .map(map_query_answer_candidate)
            .collect(),
    }
}

pub(super) fn map_query_answer_candidate(
    candidate: QueryAnswerCandidate,
) -> ironrag_contracts::assistant::AssistantAnswerCandidate {
    ironrag_contracts::assistant::AssistantAnswerCandidate {
        label: candidate.label,
        kind: candidate.kind,
        confidence: candidate.confidence,
        provenance: ironrag_contracts::assistant::AssistantAnswerCandidateProvenance {
            entity_id: candidate.provenance.entity_id,
            document_id: candidate.provenance.document_id,
            chunk_id: candidate.provenance.chunk_id,
        },
    }
}

pub(super) fn map_turn_to_message(
    turn: QueryTurn,
    evidence: Option<ironrag_contracts::assistant::AssistantEvidenceBundle>,
) -> ironrag_contracts::assistant::AssistantConversationMessage {
    ironrag_contracts::assistant::AssistantConversationMessage {
        id: turn.id,
        role: map_turn_role(turn.turn_kind),
        content: turn.content_text,
        timestamp: turn.created_at,
        elapsed_ms: None,
        execution_id: turn.execution_id,
        evidence,
    }
}

pub(super) fn map_unanswered_terminal_execution_to_message(
    execution: &QueryExecution,
) -> ironrag_contracts::assistant::AssistantConversationMessage {
    let content = execution
        .failure_code
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(UNANSWERED_TURN_FALLBACK)
        .to_string();
    ironrag_contracts::assistant::AssistantConversationMessage {
        id: execution.id,
        role: ironrag_contracts::assistant::AssistantTurnRole::Assistant,
        content,
        timestamp: execution.started_at,
        elapsed_ms: None,
        execution_id: Some(execution.id),
        evidence: None,
    }
}

pub(super) fn map_pending_execution_to_message(
    execution: &QueryExecution,
    observed_at: chrono::DateTime<Utc>,
) -> ironrag_contracts::assistant::AssistantConversationMessage {
    ironrag_contracts::assistant::AssistantConversationMessage {
        id: execution.id,
        role: ironrag_contracts::assistant::AssistantTurnRole::Assistant,
        content: String::new(),
        timestamp: execution.started_at,
        elapsed_ms: Some(
            u64::try_from(
                observed_at.signed_duration_since(execution.started_at).num_milliseconds(),
            )
            .unwrap_or_default(),
        ),
        execution_id: Some(execution.id),
        evidence: None,
    }
}

pub(super) fn map_turn(turn: QueryTurn) -> ironrag_contracts::assistant::AssistantTurn {
    ironrag_contracts::assistant::AssistantTurn {
        id: turn.id,
        conversation_id: turn.conversation_id,
        turn_index: turn.turn_index,
        turn_kind: map_turn_role(turn.turn_kind),
        author_principal_id: turn.author_principal_id,
        content_text: turn.content_text,
        execution_id: turn.execution_id,
        created_at: turn.created_at,
    }
}

pub(super) const fn map_turn_role(
    turn_kind: crate::domains::query::QueryTurnKind,
) -> ironrag_contracts::assistant::AssistantTurnRole {
    match turn_kind {
        crate::domains::query::QueryTurnKind::User => {
            ironrag_contracts::assistant::AssistantTurnRole::User
        }
        crate::domains::query::QueryTurnKind::Assistant => {
            ironrag_contracts::assistant::AssistantTurnRole::Assistant
        }
        crate::domains::query::QueryTurnKind::System => {
            ironrag_contracts::assistant::AssistantTurnRole::System
        }
        crate::domains::query::QueryTurnKind::Tool => {
            ironrag_contracts::assistant::AssistantTurnRole::Tool
        }
    }
}

pub(super) fn map_execution(
    execution: QueryExecution,
) -> ironrag_contracts::assistant::AssistantExecution {
    ironrag_contracts::assistant::AssistantExecution {
        id: execution.id,
        workspace_id: execution.workspace_id,
        library_id: execution.library_id,
        conversation_id: execution.conversation_id,
        context_bundle_id: execution.context_bundle_id,
        request_turn_id: execution.request_turn_id,
        response_turn_id: execution.response_turn_id,
        binding_id: execution.binding_id,
        runtime_execution_id: execution.runtime_execution_id,
        lifecycle_state: execution.lifecycle_state.as_str().to_string(),
        active_stage: execution.active_stage.map(|stage| stage.as_str().to_string()),
        query_text: execution.query_text,
        failure_code: execution.failure_code,
        started_at: execution.started_at,
    }
}

pub(super) fn map_runtime_summary(
    runtime_summary: RuntimeExecutionSummary,
) -> ironrag_contracts::assistant::AssistantRuntimeSummary {
    let runtime_accepted_at = runtime_summary.accepted_at;
    ironrag_contracts::assistant::AssistantRuntimeSummary {
        runtime_execution_id: runtime_summary.runtime_execution_id,
        lifecycle_state: runtime_summary.lifecycle_state.as_str().to_string(),
        active_stage: runtime_summary.active_stage.map(|stage| stage.as_str().to_string()),
        turn_budget: runtime_summary.turn_budget,
        turn_count: runtime_summary.turn_count,
        parallel_action_limit: runtime_summary.parallel_action_limit,
        failure_code: runtime_summary.failure_code,
        failure_summary_redacted: runtime_summary.failure_summary_redacted,
        policy_summary: map_policy_summary(runtime_summary.policy_summary, runtime_accepted_at),
        accepted_at: runtime_summary.accepted_at,
        completed_at: runtime_summary.completed_at,
    }
}

pub(super) fn map_runtime_stage_summary(
    summary: QueryRuntimeStageSummary,
) -> ironrag_contracts::assistant::AssistantRuntimeStageSummary {
    ironrag_contracts::assistant::AssistantRuntimeStageSummary {
        stage_kind: summary.stage_kind.as_str().to_string(),
        stage_label: summary.stage_label,
        duration_ms: summary.duration_ms,
    }
}

pub(super) fn map_policy_summary(
    policy_summary: RuntimePolicySummary,
    decision_timestamp: chrono::DateTime<Utc>,
) -> ironrag_contracts::assistant::AssistantPolicySummary {
    ironrag_contracts::assistant::AssistantPolicySummary {
        allow_count: policy_summary.allow_count.try_into().unwrap_or(i32::MAX),
        reject_count: policy_summary.reject_count.try_into().unwrap_or(i32::MAX),
        terminate_count: policy_summary.terminate_count.try_into().unwrap_or(i32::MAX),
        recent_decisions: policy_summary
            .recent_decisions
            .into_iter()
            .map(|decision| map_policy_decision_summary(decision, decision_timestamp))
            .collect(),
    }
}

pub(super) fn map_policy_decision_summary(
    policy_decision: RuntimePolicyDecisionSummary,
    decision_timestamp: chrono::DateTime<Utc>,
) -> ironrag_contracts::assistant::AssistantPolicyDecisionSummary {
    ironrag_contracts::assistant::AssistantPolicyDecisionSummary {
        target_kind: policy_decision.target_kind.as_str().to_string(),
        decision_kind: policy_decision.decision_kind.as_str().to_string(),
        reason_code: policy_decision.reason_code,
        target_id: policy_decision.reason_summary_redacted,
        decided_at: decision_timestamp,
    }
}

pub(super) const fn map_verification_state(
    state: QueryVerificationState,
) -> ironrag_contracts::assistant::AssistantVerificationState {
    match state {
        QueryVerificationState::NotRun => {
            ironrag_contracts::assistant::AssistantVerificationState::NotRun
        }
        QueryVerificationState::Verified => {
            ironrag_contracts::assistant::AssistantVerificationState::Verified
        }
        QueryVerificationState::PartiallySupported => {
            ironrag_contracts::assistant::AssistantVerificationState::PartiallySupported
        }
        QueryVerificationState::Conflicting => {
            ironrag_contracts::assistant::AssistantVerificationState::Conflicting
        }
        QueryVerificationState::InsufficientEvidence => {
            ironrag_contracts::assistant::AssistantVerificationState::InsufficientEvidence
        }
        QueryVerificationState::Failed => {
            ironrag_contracts::assistant::AssistantVerificationState::Failed
        }
    }
}

pub(super) const fn map_answer_disposition(
    disposition: crate::domains::query::QueryAnswerDisposition,
) -> ironrag_contracts::assistant::AssistantAnswerDisposition {
    match disposition {
        crate::domains::query::QueryAnswerDisposition::NonTerminal => {
            ironrag_contracts::assistant::AssistantAnswerDisposition::NonTerminal
        }
        crate::domains::query::QueryAnswerDisposition::FactualReady => {
            ironrag_contracts::assistant::AssistantAnswerDisposition::FactualReady
        }
        crate::domains::query::QueryAnswerDisposition::SafeFallback => {
            ironrag_contracts::assistant::AssistantAnswerDisposition::SafeFallback
        }
        crate::domains::query::QueryAnswerDisposition::Clarification => {
            ironrag_contracts::assistant::AssistantAnswerDisposition::Clarification
        }
    }
}

pub(super) fn map_verification_warning(
    warning: QueryVerificationWarning,
) -> ironrag_contracts::assistant::AssistantVerificationWarning {
    ironrag_contracts::assistant::AssistantVerificationWarning {
        code: warning.code,
        message: warning.message,
        related_segment_id: warning.related_segment_id,
        related_fact_id: warning.related_fact_id,
    }
}

pub(super) fn map_chunk_reference(
    reference: QueryChunkReference,
) -> ironrag_contracts::assistant::AssistantChunkReference {
    ironrag_contracts::assistant::AssistantChunkReference {
        execution_id: reference.execution_id,
        chunk_id: reference.chunk_id,
        rank: reference.rank,
        score: reference.score,
        document_id: reference.document_id,
        document_title: reference.document_title,
        section_path: reference.section_path,
        excerpt: reference.excerpt,
    }
}

pub(super) fn map_prepared_segment_reference(
    reference: PreparedSegmentReference,
) -> ironrag_contracts::assistant::AssistantPreparedSegmentReference {
    ironrag_contracts::assistant::AssistantPreparedSegmentReference {
        execution_id: reference.execution_id,
        segment_id: reference.segment_id,
        revision_id: reference.revision_id,
        block_kind: reference.block_kind.as_str().to_string(),
        rank: reference.rank,
        score: reference.score,
        heading_trail: reference.heading_trail,
        section_path: reference.section_path,
        document_id: reference.document_id,
        document_title: reference.document_title,
        document_hint: reference.document_hint,
        source_access: reference.source_access.map(|access| {
            ironrag_contracts::assistant::AssistantContentSourceAccess {
                kind: match access.kind {
                    crate::domains::content::ContentSourceAccessKind::StoredDocument => {
                        "stored_document".to_string()
                    }
                    crate::domains::content::ContentSourceAccessKind::ExternalUrl => {
                        "external_url".to_string()
                    }
                },
                href: access.href,
            }
        }),
    }
}

pub(super) fn map_technical_fact_reference(
    reference: TechnicalFactReference,
) -> ironrag_contracts::assistant::AssistantTechnicalFactReference {
    ironrag_contracts::assistant::AssistantTechnicalFactReference {
        execution_id: reference.execution_id,
        fact_id: reference.fact_id,
        revision_id: reference.revision_id,
        fact_kind: reference.fact_kind.as_str().to_string(),
        normalized_value: reference.normalized_value,
        display_value: reference.display_value,
        rank: reference.rank,
        score: reference.score,
    }
}

pub(super) fn map_graph_node_reference(
    reference: QueryGraphNodeReference,
) -> ironrag_contracts::assistant::AssistantEntityReference {
    ironrag_contracts::assistant::AssistantEntityReference {
        execution_id: reference.execution_id,
        node_id: reference.node_id,
        rank: reference.rank,
        score: reference.score,
        label: reference.label,
        entity_type: reference.entity_type,
        summary: reference.summary,
    }
}

pub(super) fn map_graph_edge_reference(
    reference: QueryGraphEdgeReference,
) -> ironrag_contracts::assistant::AssistantRelationReference {
    ironrag_contracts::assistant::AssistantRelationReference {
        execution_id: reference.execution_id,
        edge_id: reference.edge_id,
        rank: reference.rank,
        score: reference.score,
        predicate: reference.relation_type,
        normalized_assertion: reference.summary,
    }
}

pub(super) fn append_query_execution_audit(
    state: AppState,
    principal_id: Uuid,
    surface_kind: &'static str,
    outcome: &crate::services::query::service::QueryTurnExecutionResult,
) -> futures::future::BoxFuture<'static, ()> {
    let command = AppendQueryExecutionAuditCommand {
        actor_principal_id: principal_id,
        surface_kind: surface_kind.to_string(),
        request_id: None,
        query_session_id: outcome.conversation.id,
        query_execution_id: outcome.execution.id,
        runtime_execution_id: Some(outcome.execution.runtime_execution_id),
        context_bundle_id: outcome.context_bundle_id,
        workspace_id: outcome.execution.workspace_id,
        library_id: outcome.execution.library_id,
        question_preview: Some(outcome.request_turn.content_text.clone()),
    };
    let audit_service = state.application_services.audit.clone();
    Box::pin(async move {
        if let Err(error) = audit_service.append_query_execution_event(&state, command).await {
            tracing::warn!(stage = "audit", error = %error, "audit append failed");
        }
    })
}
