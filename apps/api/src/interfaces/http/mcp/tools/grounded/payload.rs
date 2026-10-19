use super::*;

// --- grounded_answer contract payload / response construction --------
//
// Split out of the former `interfaces/http/mcp.rs` god-file (plan
// §6.4): this is `grounded_answer`-specific text and contract-payload
// construction that had ended up living in the transport file instead
// of alongside the tool it belongs to. `grounded_answer_contract_payload`,
// `_with_profile`, and `_for_query_ir` are re-exported at
// `crate::interfaces::http::mcp` (see `mcp.rs`) since an external
// integration test (`tests/mcp_grounded_answer_contract.rs`) imports
// them at that path — the move keeps that public contract stable.

pub(super) const GROUNDED_ANSWER_MUST_PRESERVE_SPAN_LIMIT: usize = 128;
pub(super) const GROUNDED_ANSWER_MUST_PRESERVE_SPAN_MAX_CHARS: usize = 240;
pub(super) const GROUNDED_ANSWER_GRAPH_PRESERVE_SPAN_LIMIT: usize = 24;
pub(super) const GROUNDED_ANSWER_GRAPH_PRESERVE_MAX_RANK: i32 = 32;

/// Builds the JSON-serializable MCP `grounded_answer` tool result from
/// the same assistant execution detail returned by the UI query API.
///
/// The live MCP handler calls `grounded_answer_tool_result` directly.
/// This public JSON form gives integration tests a DB-free contract path
/// for snapshotting the MCP wrapper without duplicating the production
/// serializer. It is a test contract surface, not a stable application API.
#[doc(hidden)]
#[must_use]
pub fn grounded_answer_contract_payload(
    answer_text: &str,
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
) -> Value {
    json!(grounded_answer_tool_result(answer_text, execution_detail))
}

/// DB-free contract helper for the optional grounded-answer response profile.
/// Production callers validate `max_references` before reaching the serializer;
/// this helper defensively applies the public compact-profile bounds.
#[doc(hidden)]
#[must_use]
pub fn grounded_answer_contract_payload_with_profile(
    answer_text: &str,
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
    response_profile: McpGroundedAnswerResponseProfile,
    max_references: Option<usize>,
) -> Value {
    let compact_reference_limit = match response_profile {
        McpGroundedAnswerResponseProfile::Full => None,
        McpGroundedAnswerResponseProfile::Compact => Some(
            max_references
                .unwrap_or(MCP_COMPACT_DEFAULT_REFERENCES)
                .clamp(1, MCP_COMPACT_MAX_REFERENCES),
        ),
    };
    json!(grounded_answer_tool_result_with_profile(
        answer_text,
        execution_detail,
        response_profile,
        compact_reference_limit,
    ))
}

/// DB-free contract helper that applies the same typed completion policy as
/// the live grounded-answer MCP handler.
#[doc(hidden)]
#[must_use]
pub fn grounded_answer_contract_payload_for_query_ir(
    answer_text: &str,
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
    query_ir: &QueryIR,
    response_profile: McpGroundedAnswerResponseProfile,
    max_references: Option<usize>,
) -> Value {
    let compact_reference_limit = match response_profile {
        McpGroundedAnswerResponseProfile::Full => None,
        McpGroundedAnswerResponseProfile::Compact => Some(
            max_references
                .unwrap_or(MCP_COMPACT_DEFAULT_REFERENCES)
                .clamp(1, MCP_COMPACT_MAX_REFERENCES),
        ),
    };
    let completion = AnswerCompletionContract::from_query_ir(query_ir).evaluate(answer_text);
    let mut result = grounded_answer_tool_result_with_profile_and_completion(
        answer_text,
        execution_detail,
        response_profile,
        compact_reference_limit,
        &completion,
    );
    if let Some(structured_content) = result.structured_content.as_object_mut() {
        structured_content.insert("queryLanguage".to_string(), json!(query_ir.language));
    }
    json!(result)
}

pub(crate) fn grounded_answer_tool_result(
    answer_text: &str,
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
) -> McpToolResult {
    grounded_answer_tool_result_with_profile(
        answer_text,
        execution_detail,
        McpGroundedAnswerResponseProfile::Full,
        None,
    )
}

pub(crate) fn grounded_answer_tool_result_with_profile(
    answer_text: &str,
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
    response_profile: McpGroundedAnswerResponseProfile,
    compact_reference_limit: Option<usize>,
) -> McpToolResult {
    grounded_answer_tool_result_with_profile_and_completion(
        answer_text,
        execution_detail,
        response_profile,
        compact_reference_limit,
        &AnswerCompletionAssessment::complete(),
    )
}

pub(crate) fn grounded_answer_tool_result_with_profile_and_completion(
    answer_text: &str,
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
    response_profile: McpGroundedAnswerResponseProfile,
    compact_reference_limit: Option<usize>,
    completion: &AnswerCompletionAssessment,
) -> McpToolResult {
    ok_tool_result(
        &grounded_answer_human_text(answer_text),
        grounded_answer_structured_content(
            answer_text,
            execution_detail,
            response_profile,
            compact_reference_limit,
            completion,
        ),
    )
}

fn grounded_answer_human_text(answer_text: &str) -> String {
    if answer_text.is_empty() {
        "The grounded-answer pipeline returned no answer text (execution may have failed or degraded). Inspect runtimeExecutionId via get_runtime_execution_trace for details.".to_string()
    } else {
        answer_text.to_string()
    }
}

fn grounded_answer_structured_content(
    answer_text: &str,
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
    response_profile: McpGroundedAnswerResponseProfile,
    compact_reference_limit: Option<usize>,
    completion: &AnswerCompletionAssessment,
) -> Value {
    let envelope = GroundedAnswerCompletionEnvelope::new(
        execution_detail.answer_disposition,
        answer_text,
        completion.clone(),
        &execution_detail.execution.lifecycle_state,
        execution_detail.execution.failure_code.clone(),
    );
    match response_profile {
        McpGroundedAnswerResponseProfile::Full => {
            grounded_answer_full_structured_content(answer_text, execution_detail, &envelope)
        }
        McpGroundedAnswerResponseProfile::Compact => grounded_answer_compact_structured_content(
            answer_text,
            execution_detail,
            compact_reference_limit
                .unwrap_or(MCP_COMPACT_DEFAULT_REFERENCES)
                .clamp(1, MCP_COMPACT_MAX_REFERENCES),
            &envelope,
        ),
    }
}

fn grounded_answer_full_structured_content(
    answer_text: &str,
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
    envelope: &GroundedAnswerCompletionEnvelope,
) -> Value {
    let mut sanitized_execution_detail = json!(execution_detail);
    if let Some(references) = sanitized_execution_detail
        .get_mut("preparedSegmentReferences")
        .and_then(Value::as_array_mut)
    {
        for reference in references {
            if let Some(object) = reference.as_object_mut() {
                object.remove("sourceUri");
                object.remove("sourceAccess");
            }
        }
    }
    let clarification = &execution_detail.clarification;
    json!({
        "answerBody": answer_text,
        "responseProfile": "full",
        "executionDetail": sanitized_execution_detail,
        "finalAnswerReady": envelope.final_answer_ready,
        "finalizable": envelope.finalizable,
        "completion": &envelope.completion,
        "repairPolicy": &envelope.repair_policy,
        "readiness": &envelope.readiness,
        "mustPreserveSpans": grounded_answer_must_preserve_spans(
            answer_text,
            execution_detail,
            envelope.finalizable,
        ),
        // Typed disambiguation surface for agent callers: the prose answer
        // body stays authoritative, while `answerCandidates` lets an agent
        // route a follow-up without parsing the clarification tail. Empty
        // for unambiguous answers. `clarification` reports whether this turn
        // asked the user to choose and the question it asked.
        "answerCandidates": clarification.answer_candidates,
        "clarification": json!({
            "required": clarification.required,
            "question": clarification.question,
        }),
        "runtimeExecutionId": execution_detail.execution.runtime_execution_id,
        "executionId": execution_detail.execution.id,
        "conversationId": execution_detail.execution.conversation_id,
        "libraryId": execution_detail.execution.library_id,
        "workspaceId": execution_detail.execution.workspace_id,
        "lifecycleState": execution_detail.execution.lifecycle_state,
    })
}

pub(super) const GROUNDED_ANSWER_COMPACT_WARNING_LIMIT: usize = 16;
pub(super) const GROUNDED_ANSWER_COMPACT_PRESERVE_SPAN_LIMIT: usize = 16;
pub(super) const GROUNDED_ANSWER_COMPACT_CANDIDATE_LIMIT: usize = 8;
pub(super) const GROUNDED_ANSWER_COMPACT_TEXT_MAX_CHARS: usize = 240;

fn grounded_answer_compact_structured_content(
    answer_text: &str,
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
    reference_limit: usize,
    envelope: &GroundedAnswerCompletionEnvelope,
) -> Value {
    let clarification = &execution_detail.clarification;
    let (total_reference_count, references) =
        grounded_answer_compact_references(execution_detail, reference_limit);
    let warning_count = execution_detail.verification_warnings.len();
    let warnings = execution_detail
        .verification_warnings
        .iter()
        .take(GROUNDED_ANSWER_COMPACT_WARNING_LIMIT)
        .map(|warning| {
            json!({
                "code": warning.code,
                "message": grounded_answer_compact_text(&warning.message),
                "relatedSegmentId": warning.related_segment_id,
                "relatedFactId": warning.related_fact_id,
            })
        })
        .collect::<Vec<_>>();
    let preserve_spans =
        grounded_answer_must_preserve_spans(answer_text, execution_detail, envelope.finalizable);
    let preserve_span_count = preserve_spans.len();
    let bounded_preserve_spans = preserve_spans
        .into_iter()
        .take(GROUNDED_ANSWER_COMPACT_PRESERVE_SPAN_LIMIT)
        .collect::<Vec<_>>();
    let answer_candidate_count = clarification.answer_candidates.len();
    let answer_candidates = clarification
        .answer_candidates
        .iter()
        .take(GROUNDED_ANSWER_COMPACT_CANDIDATE_LIMIT)
        .collect::<Vec<_>>();

    json!({
        "answerBody": answer_text,
        "responseProfile": "compact",
        "finalAnswerReady": envelope.final_answer_ready,
        "finalizable": envelope.finalizable,
        "completion": &envelope.completion,
        "repairPolicy": &envelope.repair_policy,
        "readiness": &envelope.readiness,
        "verifier": {
            "state": execution_detail.verification_state,
            "warningCount": warning_count,
            "returnedWarningCount": warnings.len(),
            "warningsTruncated": warning_count > warnings.len(),
        },
        "warnings": warnings,
        "referenceSummary": {
            "totalCount": total_reference_count,
            "returnedCount": references.len(),
            "truncated": total_reference_count > references.len(),
            "references": references,
        },
        "preserveSpanSummary": {
            "totalCount": preserve_span_count,
            "returnedCount": bounded_preserve_spans.len(),
            "truncated": preserve_span_count > bounded_preserve_spans.len(),
        },
        "mustPreserveSpans": bounded_preserve_spans,
        "answerCandidateSummary": {
            "totalCount": answer_candidate_count,
            "returnedCount": answer_candidates.len(),
            "truncated": answer_candidate_count > answer_candidates.len(),
            "candidates": answer_candidates,
        },
        "clarification": {
            "required": clarification.required,
            "question": clarification.question,
        },
        "runtimeExecutionId": execution_detail.execution.runtime_execution_id,
        "executionId": execution_detail.execution.id,
        "conversationId": execution_detail.execution.conversation_id,
        "libraryId": execution_detail.execution.library_id,
        "workspaceId": execution_detail.execution.workspace_id,
        "lifecycleState": execution_detail.execution.lifecycle_state,
    })
}

fn grounded_answer_compact_references(
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
    limit: usize,
) -> (usize, Vec<Value>) {
    let total_count = execution_detail
        .chunk_references
        .len()
        .saturating_add(execution_detail.prepared_segment_references.len())
        .saturating_add(execution_detail.technical_fact_references.len())
        .saturating_add(execution_detail.entity_references.len())
        .saturating_add(execution_detail.relation_references.len());
    let mut ranked = Vec::with_capacity(total_count.min(limit.saturating_mul(2)));

    ranked.extend(execution_detail.chunk_references.iter().map(|reference| {
        (
            reference.rank,
            0_u8,
            json!({
                "kind": "chunk",
                "chunkId": reference.chunk_id,
                "rank": reference.rank,
                "score": reference.score,
            }),
        )
    }));
    ranked.extend(execution_detail.prepared_segment_references.iter().map(|reference| {
        (
            reference.rank,
            1_u8,
            json!({
                "kind": "prepared_segment",
                "segmentId": reference.segment_id,
                "revisionId": reference.revision_id,
                "documentId": reference.document_id,
                "documentTitle": reference
                    .document_title
                    .as_deref()
                    .map(grounded_answer_compact_text),
                "blockKind": reference.block_kind,
                "rank": reference.rank,
                "score": reference.score,
            }),
        )
    }));
    ranked.extend(execution_detail.technical_fact_references.iter().map(|reference| {
        (
            reference.rank,
            2_u8,
            json!({
                "kind": "technical_fact",
                "factId": reference.fact_id,
                "revisionId": reference.revision_id,
                "factKind": reference.fact_kind,
                "displayValue": grounded_answer_compact_text(&reference.display_value),
                "rank": reference.rank,
                "score": reference.score,
            }),
        )
    }));
    ranked.extend(execution_detail.entity_references.iter().map(|reference| {
        (
            reference.rank,
            3_u8,
            json!({
                "kind": "entity",
                "nodeId": reference.node_id,
                "label": grounded_answer_compact_text(&reference.label),
                "entityType": reference.entity_type,
                "rank": reference.rank,
                "score": reference.score,
            }),
        )
    }));
    ranked.extend(execution_detail.relation_references.iter().map(|reference| {
        (
            reference.rank,
            4_u8,
            json!({
                "kind": "relation",
                "edgeId": reference.edge_id,
                "predicate": grounded_answer_compact_text(&reference.predicate),
                "assertion": reference
                    .normalized_assertion
                    .as_deref()
                    .map(grounded_answer_compact_text),
                "rank": reference.rank,
                "score": reference.score,
            }),
        )
    }));
    ranked.sort_by_key(|(rank, kind_order, _)| (*rank, *kind_order));

    (total_count, ranked.into_iter().take(limit).map(|(_, _, reference)| reference).collect())
}

fn grounded_answer_compact_text(value: &str) -> String {
    let mut chars = value.trim().chars();
    let mut compact =
        chars.by_ref().take(GROUNDED_ANSWER_COMPACT_TEXT_MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        compact.push('…');
    }
    compact
}
