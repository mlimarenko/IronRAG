use chrono::{DateTime, TimeZone, Utc};
use ironrag_backend::interfaces::http::mcp::grounded_answer_contract_payload;
use ironrag_contracts::assistant::{
    AssistantAnswerDisposition, AssistantChunkReference, AssistantClarification,
    AssistantContentSourceAccess, AssistantEntityReference, AssistantExecution,
    AssistantExecutionDetail, AssistantPolicySummary, AssistantPreparedSegmentReference,
    AssistantRelationReference, AssistantRuntimeStageSummary, AssistantRuntimeSummary,
    AssistantTechnicalFactReference, AssistantTurn, AssistantTurnRole, AssistantVerificationState,
    AssistantVerificationWarning,
};
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

const FULL_RUNTIME_STAGES: &[&str] = &["retrieve", "assemble_context", "answer", "persist"];
const VERIFY_RUNTIME_STAGES: &[&str] =
    &["retrieve", "assemble_context", "answer", "verify", "persist"];

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GroundedAnswerInput {
    library: &'static str,
    query: &'static str,
    top_k: Option<usize>,
    include_debug: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    conversation_turns: Vec<ConversationTurn>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConversationTurn {
    role: &'static str,
    content: &'static str,
}

#[derive(Clone, Copy)]
struct EvidenceCounts {
    chunks: usize,
    prepared_segments: usize,
    technical_facts: usize,
    entities: usize,
    relations: usize,
}

struct Scenario {
    name: &'static str,
    seed: u128,
    input: GroundedAnswerInput,
    evidence: EvidenceCounts,
    verification_state: AssistantVerificationState,
    answer_disposition: AssistantAnswerDisposition,
    warnings: Vec<WarningSpec>,
    runtime_stages: &'static [&'static str],
}

struct WarningSpec {
    code: &'static str,
    related_segment: bool,
    related_fact: bool,
}

#[test]
fn grounded_answer_mcp_response_shape_stays_canonical() {
    let provider = DeterministicGroundedAnswerProvider;
    let shapes = scenarios()
        .iter()
        .map(|scenario| {
            let answer_text = provider.answer_text(scenario);
            let detail = execution_detail_for(scenario, &answer_text);
            let response = grounded_answer_contract_payload(&answer_text, &detail);
            response_shape(scenario, &response)
        })
        .collect::<Vec<_>>();

    insta::with_settings!({ sort_maps => true }, {
        insta::assert_json_snapshot!("grounded_answer_contract_shapes", shapes);
    });
}

struct DeterministicGroundedAnswerProvider;

impl DeterministicGroundedAnswerProvider {
    fn answer_text(&self, scenario: &Scenario) -> String {
        format!("Synthetic grounded answer for contract case `{}`.", scenario.name)
    }
}

fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "verified_endpoint_answer",
            seed: 1,
            input: GroundedAnswerInput {
                library: "alpha-workspace/adapter-library",
                query: "Which endpoint does the demo adapter call for inventory sync?",
                top_k: Some(8),
                include_debug: true,
                conversation_turns: Vec::new(),
            },
            evidence: EvidenceCounts {
                chunks: 2,
                prepared_segments: 1,
                technical_facts: 1,
                entities: 0,
                relations: 0,
            },
            verification_state: AssistantVerificationState::Verified,
            answer_disposition: AssistantAnswerDisposition::FactualReady,
            warnings: Vec::new(),
            runtime_stages: FULL_RUNTIME_STAGES,
        },
        Scenario {
            name: "followup_with_external_turns",
            seed: 2,
            input: GroundedAnswerInput {
                library: "alpha-workspace/adapter-library",
                query: "Which setting changes when the retry window is raised?",
                top_k: Some(6),
                include_debug: true,
                conversation_turns: vec![
                    ConversationTurn {
                        role: "user",
                        content: "Summarize the synthetic retry policy.",
                    },
                    ConversationTurn {
                        role: "assistant",
                        content: "The synthetic retry policy uses a bounded retry window.",
                    },
                ],
            },
            evidence: EvidenceCounts {
                chunks: 1,
                prepared_segments: 2,
                technical_facts: 2,
                entities: 0,
                relations: 0,
            },
            verification_state: AssistantVerificationState::Verified,
            answer_disposition: AssistantAnswerDisposition::FactualReady,
            warnings: Vec::new(),
            runtime_stages: VERIFY_RUNTIME_STAGES,
        },
        Scenario {
            name: "insufficient_evidence_answer",
            seed: 3,
            input: GroundedAnswerInput {
                library: "beta-workspace/ops-library",
                query: "What retention window does the missing sample note specify?",
                top_k: Some(4),
                include_debug: false,
                conversation_turns: Vec::new(),
            },
            evidence: EvidenceCounts {
                chunks: 0,
                prepared_segments: 0,
                technical_facts: 0,
                entities: 0,
                relations: 0,
            },
            verification_state: AssistantVerificationState::InsufficientEvidence,
            answer_disposition: AssistantAnswerDisposition::NonTerminal,
            warnings: vec![WarningSpec {
                code: "insufficient_evidence",
                related_segment: false,
                related_fact: false,
            }],
            runtime_stages: &[],
        },
        Scenario {
            name: "conflicting_prepared_segments",
            seed: 4,
            input: GroundedAnswerInput {
                library: "beta-workspace/ops-library",
                query: "Compare the two retry limits in the sample operations guide.",
                top_k: Some(10),
                include_debug: true,
                conversation_turns: Vec::new(),
            },
            evidence: EvidenceCounts {
                chunks: 2,
                prepared_segments: 2,
                technical_facts: 0,
                entities: 0,
                relations: 0,
            },
            verification_state: AssistantVerificationState::Conflicting,
            answer_disposition: AssistantAnswerDisposition::NonTerminal,
            warnings: vec![WarningSpec {
                code: "conflicting_evidence",
                related_segment: true,
                related_fact: false,
            }],
            runtime_stages: VERIFY_RUNTIME_STAGES,
        },
        Scenario {
            name: "graph_grounded_answer",
            seed: 5,
            input: GroundedAnswerInput {
                library: "gamma-workspace/topology-library",
                query: "What does the deployment graph connect between the worker and queue?",
                top_k: Some(12),
                include_debug: true,
                conversation_turns: Vec::new(),
            },
            evidence: EvidenceCounts {
                chunks: 1,
                prepared_segments: 1,
                technical_facts: 0,
                entities: 2,
                relations: 1,
            },
            verification_state: AssistantVerificationState::Verified,
            answer_disposition: AssistantAnswerDisposition::FactualReady,
            warnings: Vec::new(),
            runtime_stages: FULL_RUNTIME_STAGES,
        },
        Scenario {
            name: "partially_supported_fact_answer",
            seed: 6,
            input: GroundedAnswerInput {
                library: "gamma-workspace/changelog-library",
                query: "What does the synthetic changelog say about upload validation?",
                top_k: Some(5),
                include_debug: true,
                conversation_turns: Vec::new(),
            },
            evidence: EvidenceCounts {
                chunks: 1,
                prepared_segments: 1,
                technical_facts: 1,
                entities: 1,
                relations: 0,
            },
            verification_state: AssistantVerificationState::PartiallySupported,
            answer_disposition: AssistantAnswerDisposition::NonTerminal,
            warnings: vec![WarningSpec {
                code: "unsupported_literal",
                related_segment: false,
                related_fact: true,
            }],
            runtime_stages: VERIFY_RUNTIME_STAGES,
        },
    ]
}

fn execution_detail_for(scenario: &Scenario, answer_text: &str) -> AssistantExecutionDetail {
    let now = fixed_time();
    let workspace_id = deterministic_id(scenario.seed, 1);
    let library_id = deterministic_id(scenario.seed, 2);
    let conversation_id = deterministic_id(scenario.seed, 3);
    let context_bundle_id = deterministic_id(scenario.seed, 4);
    let execution_id = deterministic_id(scenario.seed, 5);
    let runtime_execution_id = deterministic_id(scenario.seed, 6);
    let request_turn_id = deterministic_id(scenario.seed, 7);
    let response_turn_id = deterministic_id(scenario.seed, 8);

    AssistantExecutionDetail {
        context_bundle_id,
        execution: AssistantExecution {
            id: execution_id,
            workspace_id,
            library_id,
            conversation_id,
            context_bundle_id,
            request_turn_id: Some(request_turn_id),
            response_turn_id: Some(response_turn_id),
            binding_id: Some(deterministic_id(scenario.seed, 9)),
            runtime_execution_id: Some(runtime_execution_id),
            lifecycle_state: "completed".to_string(),
            active_stage: None,
            query_text: scenario.input.query.to_string(),
            failure_code: None,
            started_at: now,
            completed_at: Some(now),
        },
        runtime_summary: AssistantRuntimeSummary {
            runtime_execution_id,
            lifecycle_state: "completed".to_string(),
            active_stage: None,
            turn_budget: 1,
            turn_count: 1,
            parallel_action_limit: 1,
            failure_code: None,
            failure_summary_redacted: None,
            policy_summary: AssistantPolicySummary {
                allow_count: 0,
                reject_count: 0,
                terminate_count: 0,
                recent_decisions: Vec::new(),
            },
            accepted_at: now,
            completed_at: Some(now),
        },
        runtime_stage_summaries: scenario
            .runtime_stages
            .iter()
            .map(|stage_kind| AssistantRuntimeStageSummary {
                stage_kind: (*stage_kind).to_string(),
                stage_label: stage_label(stage_kind).to_string(),
                duration_ms: None,
            })
            .collect(),
        request_turn: Some(AssistantTurn {
            id: request_turn_id,
            conversation_id,
            turn_index: 0,
            turn_kind: AssistantTurnRole::User,
            author_principal_id: Some(deterministic_id(scenario.seed, 10)),
            content_text: scenario.input.query.to_string(),
            execution_id: Some(execution_id),
            created_at: now,
        }),
        response_turn: Some(AssistantTurn {
            id: response_turn_id,
            conversation_id,
            turn_index: 1,
            turn_kind: AssistantTurnRole::Assistant,
            author_principal_id: None,
            content_text: answer_text.to_string(),
            execution_id: Some(execution_id),
            created_at: now,
        }),
        chunk_references: chunk_references(scenario.seed, execution_id, scenario.evidence.chunks),
        prepared_segment_references: prepared_segment_references(
            scenario.seed,
            execution_id,
            scenario.evidence.prepared_segments,
        ),
        technical_fact_references: technical_fact_references(
            scenario.seed,
            execution_id,
            scenario.evidence.technical_facts,
        ),
        entity_references: entity_references(
            scenario.seed,
            execution_id,
            scenario.evidence.entities,
        ),
        relation_references: relation_references(
            scenario.seed,
            execution_id,
            scenario.evidence.relations,
        ),
        reference_summary: None,
        verification_state: scenario.verification_state,
        verification_warnings: scenario
            .warnings
            .iter()
            .map(|warning| AssistantVerificationWarning {
                code: warning.code.to_string(),
                message: format!("Synthetic verifier warning for `{}`.", warning.code),
                related_segment_id: warning
                    .related_segment
                    .then(|| deterministic_id(scenario.seed, 200)),
                related_fact_id: warning.related_fact.then(|| deterministic_id(scenario.seed, 300)),
            })
            .collect(),
        answer_disposition: scenario.answer_disposition,
        // Scenarios represent unambiguous answers; clarification is default.
        clarification: AssistantClarification::default(),
    }
}

fn chunk_references(seed: u128, execution_id: Uuid, count: usize) -> Vec<AssistantChunkReference> {
    (0..count)
        .map(|index| AssistantChunkReference {
            execution_id,
            chunk_id: deterministic_id(seed, offset(100, index)),
            rank: rank(index),
            score: score(index),
        })
        .collect()
}

fn prepared_segment_references(
    seed: u128,
    execution_id: Uuid,
    count: usize,
) -> Vec<AssistantPreparedSegmentReference> {
    (0..count)
        .map(|index| {
            let source_uri = format!("urn:synthetic:segment:{seed}:{index}");
            let document_hint = format!("Synthetic document {seed}-{index}");
            AssistantPreparedSegmentReference {
                execution_id,
                segment_id: deterministic_id(seed, offset(200, index)),
                revision_id: deterministic_id(seed, offset(220, index)),
                block_kind: "synthetic_section".to_string(),
                rank: rank(index),
                score: score(index),
                heading_trail: vec!["Synthetic guide".to_string(), format!("Section {index}")],
                section_path: vec!["synthetic".to_string(), format!("case-{seed}")],
                document_id: Some(deterministic_id(seed, offset(240, index))),
                document_title: Some(document_hint.clone()),
                document_hint: Some(document_hint),
                source_access: Some(AssistantContentSourceAccess {
                    kind: "stored_document".to_string(),
                    href: source_uri,
                }),
            }
        })
        .collect()
}

fn technical_fact_references(
    seed: u128,
    execution_id: Uuid,
    count: usize,
) -> Vec<AssistantTechnicalFactReference> {
    (0..count)
        .map(|index| AssistantTechnicalFactReference {
            execution_id,
            fact_id: deterministic_id(seed, offset(300, index)),
            revision_id: deterministic_id(seed, offset(320, index)),
            fact_kind: "synthetic_fact".to_string(),
            canonical_value: format!("synthetic-value-{seed}-{index}"),
            display_value: format!("Synthetic value {seed}-{index}"),
            rank: rank(index),
            score: score(index),
        })
        .collect()
}

fn entity_references(
    seed: u128,
    execution_id: Uuid,
    count: usize,
) -> Vec<AssistantEntityReference> {
    (0..count)
        .map(|index| AssistantEntityReference {
            execution_id,
            node_id: deterministic_id(seed, offset(400, index)),
            rank: rank(index),
            score: score(index),
            label: format!("Synthetic entity {seed}-{index}"),
            entity_type: Some("synthetic_node".to_string()),
            summary: Some(format!("Synthetic entity summary {seed}-{index}")),
        })
        .collect()
}

fn relation_references(
    seed: u128,
    execution_id: Uuid,
    count: usize,
) -> Vec<AssistantRelationReference> {
    (0..count)
        .map(|index| AssistantRelationReference {
            execution_id,
            edge_id: deterministic_id(seed, offset(500, index)),
            rank: rank(index),
            score: score(index),
            predicate: "synthetic_relation".to_string(),
            normalized_assertion: Some(format!("Synthetic relation assertion {seed}-{index}")),
        })
        .collect()
}

#[allow(
    clippy::expect_used,
    reason = "snapshot construction must fail at the exact missing required response field"
)]
fn response_shape(scenario: &Scenario, response: &Value) -> Value {
    let structured = response.get("structuredContent").expect("structuredContent");
    let detail = structured.get("executionDetail").expect("executionDetail");
    let content = response.get("content").and_then(Value::as_array).expect("content array");
    let warnings =
        detail.get("verificationWarnings").and_then(Value::as_array).expect("warnings array");
    let runtime_stages = detail
        .get("runtimeStageSummaries")
        .and_then(Value::as_array)
        .expect("runtime stages array");
    let citation_counts = citation_counts(detail);

    json!({
        "case": scenario.name,
        "input": &scenario.input,
        "topLevelKeys": object_keys(response),
        "isError": response.get("isError"),
        "contentShape": {
            "count": content.len(),
            "blocks": content.iter().map(content_block_shape).collect::<Vec<_>>(),
        },
        "structuredContentKeys": object_keys(structured),
        "hasTopLevelCitations": structured.get("citations").is_some(),
        "shortcutMatchesExecutionDetail": {
            "runtimeExecutionId": structured.get("runtimeExecutionId") == detail.pointer("/execution/runtimeExecutionId"),
            "executionId": structured.get("executionId") == detail.pointer("/execution/id"),
            "conversationId": structured.get("conversationId") == detail.pointer("/execution/conversationId"),
            "libraryId": structured.get("libraryId") == detail.pointer("/execution/libraryId"),
            "workspaceId": structured.get("workspaceId") == detail.pointer("/execution/workspaceId"),
            "lifecycleState": structured.get("lifecycleState") == detail.pointer("/execution/lifecycleState"),
            "finalAnswerReady": structured.get("finalAnswerReady")
                == Some(&json!(matches!(scenario.answer_disposition, AssistantAnswerDisposition::FactualReady))),
        },
        "answerDisposition": {
            "executionDetail": detail.get("answerDisposition"),
            "readiness": structured.pointer("/readiness/answerDisposition"),
            "matches": detail.get("answerDisposition")
                == structured.pointer("/readiness/answerDisposition"),
        },
        "executionDetailKeys": object_keys(detail),
        // Prove the new typed candidate surface is present and correct for
        // unambiguous answers: answerCandidates is an empty array,
        // clarification.required is false, question is absent/null.
        "clarificationShape": {
            "candidateCount": structured.get("answerCandidates").and_then(Value::as_array).map(Vec::len),
            "required": structured.pointer("/clarification/required"),
            "questionPresent": structured.pointer("/clarification/question").is_some_and(|v| !v.is_null()),
        },
        "citationCounts": citation_counts,
        "verifier": {
            "state": detail.get("verificationState"),
            "finalAnswerReady": structured.get("finalAnswerReady"),
            "warningCount": warnings.len(),
            "warningShapes": warnings.iter().map(verification_warning_shape).collect::<Vec<_>>(),
        },
        "runtimeStageSummaries": {
            "count": runtime_stages.len(),
            "items": runtime_stages.iter().map(runtime_stage_shape).collect::<Vec<_>>(),
        },
        "turnShape": {
            "requestTurnKeys": detail.get("requestTurn").map(object_keys),
            "responseTurnKeys": detail.get("responseTurn").map(object_keys),
        },
    })
}

fn citation_counts(detail: &Value) -> Value {
    let chunks = array_count(detail, "chunkReferences");
    let prepared_segments = array_count(detail, "preparedSegmentReferences");
    let technical_facts = array_count(detail, "technicalFactReferences");
    let entities = array_count(detail, "entityReferences");
    let relations = array_count(detail, "relationReferences");
    json!({
        "chunkReferences": chunks,
        "preparedSegmentReferences": prepared_segments,
        "technicalFactReferences": technical_facts,
        "entityReferences": entities,
        "relationReferences": relations,
        "total": chunks + prepared_segments + technical_facts + entities + relations,
    })
}

fn content_block_shape(block: &Value) -> Value {
    json!({
        "keys": object_keys(block),
        "type": block.get("type"),
        "textPresent": block.get("text").and_then(Value::as_str).is_some_and(|text| !text.is_empty()),
    })
}

fn verification_warning_shape(warning: &Value) -> Value {
    json!({
        "keys": object_keys(warning),
        "code": warning.get("code"),
        "messagePresent": warning.get("message").and_then(Value::as_str).is_some_and(|message| !message.is_empty()),
        "relatedSegmentIdPresent": warning.get("relatedSegmentId").is_some_and(|value| !value.is_null()),
        "relatedFactIdPresent": warning.get("relatedFactId").is_some_and(|value| !value.is_null()),
    })
}

fn runtime_stage_shape(stage: &Value) -> Value {
    json!({
        "keys": object_keys(stage),
        "stageKind": stage.get("stageKind"),
        "stageLabelPresent": stage.get("stageLabel").and_then(Value::as_str).is_some_and(|label| !label.is_empty()),
    })
}

#[allow(
    clippy::expect_used,
    reason = "snapshot construction must fail when a required citation array is absent"
)]
fn array_count(value: &Value, key: &str) -> usize {
    value.get(key).and_then(Value::as_array).expect("array field").len()
}

#[allow(
    clippy::expect_used,
    reason = "snapshot construction accepts only the required JSON object shape"
)]
fn object_keys(value: &Value) -> Vec<String> {
    let mut keys = value
        .as_object()
        .expect("object value")
        .keys()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>();
    keys.sort();
    keys
}

fn deterministic_id(seed: u128, offset: u128) -> Uuid {
    Uuid::from_u128((seed << 64) + offset)
}

#[allow(
    clippy::expect_used,
    reason = "fixture cardinalities are deliberately bounded below usize-to-u128 overflow"
)]
fn offset(base: u128, index: usize) -> u128 {
    base + u128::try_from(index).expect("fixture index fits u128")
}

#[allow(
    clippy::expect_used,
    reason = "fixture cardinalities are deliberately bounded below i32 rank overflow"
)]
fn rank(index: usize) -> i32 {
    i32::try_from(index + 1).expect("fixture rank fits i32")
}

fn score(index: usize) -> f64 {
    1.0 - (index as f64 * 0.05)
}

#[allow(
    clippy::expect_used,
    reason = "the compile-time fixture timestamp is fixed and intentionally invariant"
)]
fn fixed_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).single().expect("fixed timestamp is valid")
}

fn stage_label(stage_kind: &str) -> &str {
    match stage_kind {
        "retrieve" => "retrieve",
        "assemble_context" => "assembling_context",
        "answer" => "answer",
        "verify" => "verify",
        "persist" => "persist",
        other => other,
    }
}
