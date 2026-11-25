use std::collections::{BTreeMap, HashMap, HashSet};

use ironrag_contracts::assistant::semantic_verification::{
    SemanticClaimSupport, SemanticSourceEntry, SemanticSourceLocator, SemanticVerificationOutcome,
    SemanticVerificationReport,
};
use uuid::Uuid;

use crate::services::query::reference_selection::{
    ReferenceSelectionCandidate, chunk_group_key, document_group_key, entity_group_key,
    relation_group_key, revision_group_key, segment_group_key, select_reference_indices,
};

use super::super::super::resync_tool_result_text;
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
    let previous_content = result.structured_content.clone();
    if let Some(structured_content) = result.structured_content.as_object_mut() {
        structured_content.insert("queryLanguage".to_string(), json!(query_ir.language));
    }
    resync_tool_result_text(&mut result, &previous_content);
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
    let mut content = match response_profile {
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
    };
    content["sequenceBudget"] = json!(envelope.sequence_budget);
    content
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

const GROUNDED_ANSWER_COMPACT_REFERENCE_PATH_LIMIT: usize = 8;

fn grounded_answer_compact_structured_content(
    answer_text: &str,
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
    reference_limit: usize,
    envelope: &GroundedAnswerCompletionEnvelope,
) -> Value {
    let clarification = &execution_detail.clarification;
    let reference_selection = grounded_answer_compact_references(execution_detail, reference_limit);
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

    let mut payload = json!({
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
            "totalCount": reference_selection.total_count,
            "returnedCount": reference_selection.references.len(),
            "truncated": reference_selection.total_count > reference_selection.references.len(),
            "attribution": reference_selection.attribution,
            "references": reference_selection.references,
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
    });
    if let Some(report) = execution_detail.semantic_verification.as_ref() {
        payload["semanticVerification"] =
            super::semantic_projection::compact_semantic_verification(report, reference_limit);
    }
    payload
}

fn grounded_answer_compact_references(
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
    limit: usize,
) -> CompactReferenceSelection {
    let report = execution_detail.semantic_verification.as_ref();
    let revision_documents = grounded_answer_compact_revision_documents(execution_detail);
    let mut candidates =
        grounded_answer_compact_reference_candidates(execution_detail, &revision_documents);
    let durable_count = candidates.len();

    let source_lookup: HashMap<&str, &SemanticSourceEntry> = report
        .map(|report| {
            report.sources.iter().map(|source| (source.source_id.as_str(), source)).collect()
        })
        .unwrap_or_default();
    let mut identity_index: HashMap<CompactReferenceIdentity, usize> = HashMap::new();
    for (index, candidate) in candidates.iter().enumerate() {
        if let Some(identity) = candidate.identity {
            identity_index.entry(identity).or_insert(index);
        }
    }
    let mut projected_sources: HashMap<String, usize> = HashMap::new();
    let mut linked_units: Vec<Vec<usize>> = Vec::new();

    if let Some(report) = report
        && let SemanticVerificationOutcome::Completed { assessment } = &report.outcome
    {
        for unit in &assessment.units {
            if unit.support == SemanticClaimSupport::NoFactualClaims {
                continue;
            }
            let mut unit_candidates = Vec::new();
            let mut unit_seen = HashSet::new();
            for evidence in &unit.evidence {
                let Some(source) = source_lookup.get(evidence.source_id.as_str()).copied() else {
                    continue;
                };
                let resolved = match &source.locator {
                    SemanticSourceLocator::Chunk { chunk_id } => {
                        identity_index.get(&CompactReferenceIdentity::Chunk(*chunk_id)).copied()
                    }
                    SemanticSourceLocator::Segment { segment_id } => {
                        identity_index.get(&CompactReferenceIdentity::Segment(*segment_id)).copied()
                    }
                    SemanticSourceLocator::DocumentSlice { .. } => None,
                };
                let index = match resolved {
                    Some(index) => index,
                    None => match projected_sources.get(&source.source_id) {
                        Some(&index) => index,
                        None => {
                            let index = candidates.len();
                            candidates.push(grounded_answer_compact_source_candidate(
                                source,
                                &evidence.quote,
                            ));
                            projected_sources.insert(source.source_id.clone(), index);
                            index
                        }
                    },
                };
                if !candidates[index].claim_unit_ids.iter().any(|unit_id| unit_id == &unit.unit_id)
                {
                    candidates[index].claim_unit_ids.push(unit.unit_id.clone());
                }
                if !candidates[index]
                    .source_ids
                    .iter()
                    .any(|source_id| source_id == &source.source_id)
                {
                    candidates[index].source_ids.push(source.source_id.clone());
                }
                let trimmed_quote = evidence.quote.trim();
                if !trimmed_quote.is_empty()
                    && !candidates[index].evidence_quotes.iter().any(|quote| quote == trimmed_quote)
                {
                    candidates[index].evidence_quotes.push(trimmed_quote.to_string());
                }
                if unit_seen.insert(index) {
                    unit_candidates.push(index);
                }
            }
            linked_units.push(unit_candidates);
        }
    }

    let claim_unit_count = linked_units.len();
    let selection_candidates = candidates
        .iter()
        .map(|candidate| ReferenceSelectionCandidate {
            rank: candidate.rank,
            kind_order: candidate.kind_order,
            group: candidate.group.clone(),
            document_backed: candidate.document_backed,
        })
        .collect::<Vec<_>>();
    let selection = select_reference_indices(&selection_candidates, &linked_units, limit);
    let selected = selection.indices;
    let attributed_unit_count = selection.attributed_unit_count;

    let references = selected
        .iter()
        .map(|&index| {
            let candidate = &candidates[index];
            let mut value = candidate.value.clone();
            if let Some(object) = value.as_object_mut() {
                if !candidate.claim_unit_ids.is_empty() {
                    object.insert("claimUnitIds".to_string(), json!(candidate.claim_unit_ids));
                }
                if !candidate.source_ids.is_empty() {
                    object.insert("sourceIds".to_string(), json!(candidate.source_ids));
                }
                if !candidate.evidence_quotes.is_empty() {
                    let supporting_excerpt =
                        grounded_answer_compact_text(&candidate.evidence_quotes.join(" "));
                    object.insert("excerpt".to_string(), json!(supporting_excerpt));
                }
            }
            value
        })
        .collect::<Vec<_>>();
    let total_count = durable_count.saturating_add(projected_sources.len());
    let attribution = grounded_answer_compact_attribution(
        report,
        claim_unit_count,
        attributed_unit_count,
        projected_sources.len(),
    );
    CompactReferenceSelection { total_count, references, attribution }
}

struct CompactReferenceSelection {
    total_count: usize,
    references: Vec<Value>,
    attribution: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum CompactReferenceIdentity {
    Chunk(Uuid),
    Segment(Uuid),
}

struct CompactReferenceCandidate {
    rank: i32,
    kind_order: u8,
    group: String,
    document_backed: bool,
    identity: Option<CompactReferenceIdentity>,
    claim_unit_ids: Vec<String>,
    source_ids: Vec<String>,
    evidence_quotes: Vec<String>,
    value: Value,
}

fn grounded_answer_compact_revision_documents(
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
) -> BTreeMap<Uuid, (Option<Uuid>, Option<String>)> {
    let mut documents = BTreeMap::new();
    for reference in &execution_detail.prepared_segment_references {
        let entry = documents.entry(reference.revision_id).or_insert((None, None));
        if entry.0.is_none() {
            entry.0 = reference.document_id;
        }
        if entry.1.is_none() {
            entry.1 = reference.document_title.clone();
        }
    }
    if let Some(report) = execution_detail.semantic_verification.as_ref() {
        for source in &report.sources {
            let entry = documents.entry(source.document_revision_id).or_insert((None, None));
            if entry.0.is_none() {
                entry.0 = Some(source.document_id);
            }
        }
    }
    documents
}

fn grounded_answer_compact_reference_candidates(
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
    revision_documents: &BTreeMap<Uuid, (Option<Uuid>, Option<String>)>,
) -> Vec<CompactReferenceCandidate> {
    let mut candidates = Vec::new();

    candidates.extend(execution_detail.chunk_references.iter().map(|reference| {
        CompactReferenceCandidate {
            rank: reference.rank,
            kind_order: 0,
            group: reference
                .document_id
                .map(document_group_key)
                .unwrap_or_else(|| chunk_group_key(reference.chunk_id)),
            document_backed: true,
            identity: Some(CompactReferenceIdentity::Chunk(reference.chunk_id)),
            claim_unit_ids: Vec::new(),
            source_ids: Vec::new(),
            evidence_quotes: Vec::new(),
            value: json!({
                "kind": "chunk",
                "chunkId": reference.chunk_id,
                "documentId": reference.document_id,
                "documentTitle": reference
                    .document_title
                    .as_deref()
                    .map(grounded_answer_compact_text),
                "revisionId": reference.revision_id,
                "sectionPath": grounded_answer_compact_path(&reference.section_path),
                "excerpt": reference.excerpt.as_deref().map(grounded_answer_compact_text),
                "rank": reference.rank,
                "score": reference.score,
            }),
        }
    }));
    candidates.extend(execution_detail.prepared_segment_references.iter().map(|reference| {
        CompactReferenceCandidate {
            rank: reference.rank,
            kind_order: 1,
            group: reference
                .document_id
                .map(document_group_key)
                .unwrap_or_else(|| segment_group_key(reference.segment_id)),
            document_backed: true,
            identity: Some(CompactReferenceIdentity::Segment(reference.segment_id)),
            claim_unit_ids: Vec::new(),
            source_ids: Vec::new(),
            evidence_quotes: Vec::new(),
            value: json!({
                "kind": "prepared_segment",
                "segmentId": reference.segment_id,
                "revisionId": reference.revision_id,
                "documentId": reference.document_id,
                "documentTitle": reference
                    .document_title
                    .as_deref()
                    .map(grounded_answer_compact_text),
                "blockKind": reference.block_kind,
                "sectionPath": grounded_answer_compact_path(&reference.section_path),
                "headingTrail": grounded_answer_compact_path(&reference.heading_trail),
                "rank": reference.rank,
                "score": reference.score,
            }),
        }
    }));
    candidates.extend(execution_detail.technical_fact_references.iter().map(|reference| {
        let document = revision_documents.get(&reference.revision_id);
        CompactReferenceCandidate {
            rank: reference.rank,
            kind_order: 2,
            group: revision_group_key(reference.revision_id),
            document_backed: false,
            identity: None,
            claim_unit_ids: Vec::new(),
            source_ids: Vec::new(),
            evidence_quotes: Vec::new(),
            value: json!({
                "kind": "technical_fact",
                "factId": reference.fact_id,
                "revisionId": reference.revision_id,
                "documentId": document.and_then(|(document_id, _)| *document_id),
                "documentTitle": document
                    .and_then(|(_, title)| title.as_deref())
                    .map(grounded_answer_compact_text),
                "factKind": reference.fact_kind,
                "displayValue": grounded_answer_compact_text(&reference.display_value),
                "rank": reference.rank,
                "score": reference.score,
            }),
        }
    }));
    candidates.extend(execution_detail.entity_references.iter().map(|reference| {
        CompactReferenceCandidate {
            rank: reference.rank,
            kind_order: 3,
            group: entity_group_key(reference.node_id),
            document_backed: false,
            identity: None,
            claim_unit_ids: Vec::new(),
            source_ids: Vec::new(),
            evidence_quotes: Vec::new(),
            value: json!({
                "kind": "entity",
                "nodeId": reference.node_id,
                "label": grounded_answer_compact_text(&reference.label),
                "entityType": reference.entity_type,
                "rank": reference.rank,
                "score": reference.score,
            }),
        }
    }));
    candidates.extend(execution_detail.relation_references.iter().map(|reference| {
        CompactReferenceCandidate {
            rank: reference.rank,
            kind_order: 4,
            group: relation_group_key(reference.edge_id),
            document_backed: false,
            identity: None,
            claim_unit_ids: Vec::new(),
            source_ids: Vec::new(),
            evidence_quotes: Vec::new(),
            value: json!({
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
        }
    }));
    candidates
}

fn grounded_answer_compact_source_candidate(
    source: &SemanticSourceEntry,
    quote: &str,
) -> CompactReferenceCandidate {
    CompactReferenceCandidate {
        rank: i32::MAX,
        kind_order: 1,
        group: format!("document:{}", source.document_id),
        document_backed: true,
        identity: None,
        claim_unit_ids: Vec::new(),
        source_ids: Vec::new(),
        evidence_quotes: Vec::new(),
        value: json!({
            "kind": "document_source",
            "sourceId": source.source_id,
            "documentId": source.document_id,
            "revisionId": source.document_revision_id,
            "locator": source.locator,
            "scope": source.scope,
            "excerpt": grounded_answer_compact_text(quote),
        }),
    }
}

fn grounded_answer_compact_attribution(
    report: Option<&SemanticVerificationReport>,
    claim_unit_count: usize,
    attributed_unit_count: usize,
    projected_source_count: usize,
) -> Value {
    let (basis, status, semantic_reason) = match report.map(|report| &report.outcome) {
        Some(SemanticVerificationOutcome::Completed { .. }) => {
            ("semantic_verification", "completed", None)
        }
        Some(SemanticVerificationOutcome::NotRun { reason }) => {
            ("ranked_retrieval", "not_run", Some(json!(reason)))
        }
        Some(SemanticVerificationOutcome::Failed { code }) => {
            ("ranked_retrieval", "failed", Some(json!(code)))
        }
        None => ("ranked_retrieval", "absent", None),
    };
    json!({
        "basis": basis,
        "claimSupportAvailable": basis == "semantic_verification",
        "claimUnitCount": claim_unit_count,
        "attributedUnitCount": attributed_unit_count,
        "unattributedUnitCount": claim_unit_count.saturating_sub(attributed_unit_count),
        "projectedSourceCount": projected_source_count,
        "semanticStatus": status,
        "semanticReason": semantic_reason,
    })
}

fn grounded_answer_compact_path(path: &[String]) -> Vec<String> {
    path.iter()
        .take(GROUNDED_ANSWER_COMPACT_REFERENCE_PATH_LIMIT)
        .map(|value| grounded_answer_compact_text(value))
        .collect()
}

pub(super) fn grounded_answer_compact_text(value: &str) -> String {
    let mut chars = value.trim().chars();
    let mut compact =
        chars.by_ref().take(GROUNDED_ANSWER_COMPACT_TEXT_MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        compact.push('…');
    }
    compact
}

#[cfg(test)]
#[path = "payload_tests.rs"]
mod payload_tests;
