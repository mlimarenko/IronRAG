//! Typed projection of direct tool results onto the text that may ground an
//! answer.
//!
//! Evidence is a passage or a recorded statement, never a request, a name or a
//! presentation: queries, titles, URIs, wrapper headers, answer bodies, entity
//! label/type without a summary, `relationType` hints, document links and
//! counters are dropped. `read_document` is admitted by the caller through its
//! typed reference, so its body is not repeated here. Runtime evidence is the
//! typed lifecycle/stage/action/policy values; free-form `inputSummary`,
//! `outputSummary`, `summary`, `usage`, `reasonSummary` and `failureSummary`
//! are dropped because they may echo the question or the answer.

use serde_json::Value;

use crate::mcp_types::{
    McpReadabilityState, McpRuntimeExecutionSummary, McpRuntimeExecutionTrace,
    McpSearchDocumentsResponse,
};

use super::super::{McpToolResult, tools::documents::SEARCH_DOCUMENTS_TOOL_NAME};
use super::runtime::describe_runtime_execution_summary;

/// Source texts carried by one successful direct tool result.
pub(crate) fn admitted_source_texts(tool_name: &str, result: &McpToolResult) -> Vec<String> {
    if result.is_error {
        return Vec::new();
    }
    match tool_name {
        SEARCH_DOCUMENTS_TOOL_NAME => search_document_source_texts(&result.structured_content),
        "search_entities" => graph_entity_source_texts(&result.structured_content),
        "get_graph_topology" => graph_topology_source_texts(&result.structured_content),
        "list_relations" => relation_summary_texts(&result.structured_content),
        "get_communities" => graph_community_source_texts(&result.structured_content),
        "get_runtime_execution" => runtime_summary_source_texts(&result.structured_content),
        "get_runtime_execution_trace" => runtime_trace_source_texts(&result.structured_content),
        _ => Vec::new(),
    }
}

fn search_document_source_texts(structured_content: &Value) -> Vec<String> {
    let Ok(response) =
        serde_json::from_value::<McpSearchDocumentsResponse>(structured_content.clone())
    else {
        return Vec::new();
    };
    response
        .hits
        .into_iter()
        .filter(|hit| hit.readability_state == McpReadabilityState::Readable)
        .filter_map(|hit| hit.excerpt)
        .map(|excerpt| excerpt.trim().to_string())
        .filter(|excerpt| !excerpt.is_empty())
        .collect()
}

fn non_empty_str<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    non_empty_option(value.get(field).and_then(Value::as_str))
}

fn non_empty_option(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|text| !text.is_empty())
}

/// An entity is evidence through its recorded summary. The label and type are
/// observed values, never JSON field names or hints.
fn entity_evidence_text(entity: &Value) -> Option<String> {
    let summary = non_empty_str(entity, "summary")?;
    Some(match (non_empty_str(entity, "label"), non_empty_str(entity, "entityType")) {
        (Some(label), Some(entity_type)) => format!("{label} ({entity_type}): {summary}"),
        (Some(label), None) => format!("{label}: {summary}"),
        (None, _) => summary.to_string(),
    })
}

fn graph_entity_source_texts(structured_content: &Value) -> Vec<String> {
    array_field(structured_content, "entities")
        .into_iter()
        .filter_map(entity_evidence_text)
        .collect()
}

fn graph_topology_source_texts(structured_content: &Value) -> Vec<String> {
    let mut texts: Vec<String> = graph_entity_source_texts(structured_content);
    texts.extend(relation_summary_texts(structured_content));
    texts
}

/// A relation is evidence only through its recorded summary. Endpoint labels
/// and `relationType` form a hint, never a statement.
fn relation_summary_texts(structured_content: &Value) -> Vec<String> {
    array_field(structured_content, "relations")
        .into_iter()
        .filter_map(|relation| non_empty_str(relation, "summary").map(str::to_string))
        .collect()
}

fn graph_community_source_texts(structured_content: &Value) -> Vec<String> {
    array_field(structured_content, "communities")
        .into_iter()
        .filter_map(|community| non_empty_str(community, "summary").map(str::to_string))
        .collect()
}

fn array_field<'a>(value: &'a Value, field: &str) -> &'a [Value] {
    match value.get(field).and_then(Value::as_array) {
        Some(items) => items.as_slice(),
        None => &[],
    }
}

fn runtime_summary_source_texts(structured_content: &Value) -> Vec<String> {
    let Ok(execution) =
        serde_json::from_value::<McpRuntimeExecutionSummary>(structured_content.clone())
    else {
        return Vec::new();
    };
    runtime_execution_texts(&execution)
}

fn runtime_trace_source_texts(structured_content: &Value) -> Vec<String> {
    let Ok(trace) = serde_json::from_value::<McpRuntimeExecutionTrace>(structured_content.clone())
    else {
        return Vec::new();
    };
    let mut texts = runtime_execution_texts(&trace.execution);
    texts.extend(trace.stages.iter().map(|stage| {
        format!(
            "Runtime stage {} {} is {}.{}",
            stage.stage_ordinal,
            stage.stage_kind.as_str(),
            stage.stage_state.as_str(),
            failure_code_suffix(stage.failure_code.as_deref())
        )
    }));
    texts.extend(trace.actions.iter().map(|action| {
        format!(
            "Runtime action {} {} is {}.{}",
            action.action_ordinal,
            action.action_kind.as_str(),
            action.action_state.as_str(),
            tool_name_suffix(action.tool_name.as_deref())
        )
    }));
    texts.extend(trace.policy_decisions.iter().map(|decision| {
        format!(
            "Runtime policy decision {} on {} recorded reason {}.",
            decision.decision_kind.as_str(),
            decision.target_kind.as_str(),
            decision.reason_code.trim()
        )
    }));
    texts
}

fn runtime_execution_texts(execution: &McpRuntimeExecutionSummary) -> Vec<String> {
    let mut texts = vec![describe_runtime_execution_summary(execution)];
    if let Some(failure_code) = non_empty_option(execution.failure_code.as_deref()) {
        texts.push(format!(
            "Runtime execution {} failure code {failure_code}.",
            execution.runtime_execution_id
        ));
    }
    texts
}

fn failure_code_suffix(failure_code: Option<&str>) -> String {
    match non_empty_option(failure_code) {
        Some(failure_code) => format!(" Failure code {failure_code}."),
        None => String::new(),
    }
}

fn tool_name_suffix(tool_name: Option<&str>) -> String {
    match non_empty_option(tool_name) {
        Some(tool_name) => format!(" Tool {tool_name}."),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use super::*;

    fn tool_result(structured_content: Value) -> McpToolResult {
        McpToolResult { content: Vec::new(), structured_content, is_error: false }
    }

    fn search_hit(document_title: &str, excerpt: Option<&str>, readability: &str) -> Value {
        json!({
            "documentId": Uuid::from_u128(1),
            "libraryId": Uuid::from_u128(2),
            "workspaceId": Uuid::from_u128(3),
            "documentTitle": document_title,
            "score": 0.9,
            "excerpt": excerpt,
            "readabilityState": readability,
            "readinessKind": "ready",
            "graphCoverageKind": "complete"
        })
    }

    #[test]
    fn search_excerpts_are_evidence_but_query_titles_and_unreadable_hits_are_not() {
        let result = tool_result(json!({
            "query": "literalOnlyInQuery",
            "limit": 2,
            "libraries": ["workspace/library"],
            "hits": [
                search_hit("literalOnlyInTitle", Some("The real adjacent passage."), "readable"),
                search_hit("Second Guide", Some("literalOnlyInUnreadableExcerpt"), "processing"),
                search_hit("Empty Excerpt", Some("   "), "readable")
            ]
        }));

        let texts = admitted_source_texts(SEARCH_DOCUMENTS_TOOL_NAME, &result);

        assert_eq!(texts, vec!["The real adjacent passage.".to_string()]);
        assert!(!texts.iter().any(|text| text.contains("literalOnlyInQuery")));
        assert!(!texts.iter().any(|text| text.contains("literalOnlyInTitle")));
        assert!(!texts.iter().any(|text| text.contains("literalOnlyInUnreadableExcerpt")));
    }

    #[test]
    fn grounded_answer_bodies_errors_and_empty_arrays_are_not_evidence() {
        let generated = tool_result(json!({"answerBody": "Generated literal"}));
        assert!(admitted_source_texts("grounded_answer", &generated).is_empty());

        let mut error = tool_result(json!({
            "query": "failed query",
            "limit": 1,
            "libraries": ["workspace/library"],
            "hits": [search_hit("Failed Guide", Some("failed passage"), "readable")]
        }));
        error.is_error = true;
        assert!(admitted_source_texts(SEARCH_DOCUMENTS_TOOL_NAME, &error).is_empty());

        let empty_search = tool_result(json!({
            "query": "nothing",
            "limit": 1,
            "libraries": ["workspace/library"],
            "hits": []
        }));
        assert!(admitted_source_texts(SEARCH_DOCUMENTS_TOOL_NAME, &empty_search).is_empty());
        assert!(
            admitted_source_texts("list_documents", &tool_result(json!({"items": []}))).is_empty()
        );
    }

    #[test]
    fn graph_titles_names_hints_and_empty_graphs_are_not_evidence() {
        let empty_graph = tool_result(json!({
            "documents": [],
            "entities": [],
            "relations": [],
            "documentLinks": [],
            "truncation": {
                "entityLimit": 10,
                "relationLimit": 10,
                "totalEntities": 0,
                "totalRelations": 0,
                "entitiesTruncated": false,
                "relationsTruncated": false
            }
        }));
        assert!(admitted_source_texts("get_graph_topology", &empty_graph).is_empty());

        let names_only = tool_result(json!({
            "documents": [{
                "documentId": Uuid::from_u128(1),
                "workspaceId": Uuid::from_u128(2),
                "libraryId": Uuid::from_u128(3),
                "title": "literalOnlyInTitle"
            }],
            "entities": [{
                "entityId": Uuid::from_u128(4),
                "label": "Alpha controller",
                "entityType": "service",
                "summary": null,
                "supportCount": 3
            }],
            "relations": [{
                "relationId": Uuid::from_u128(5),
                "relationType": "part_of",
                "summary": null,
                "supportCount": 1
            }],
            "documentLinks": [{
                "documentId": Uuid::from_u128(1),
                "targetNodeId": Uuid::from_u128(4),
                "targetNodeType": "entity",
                "relationType": "mentions",
                "supportCount": 1
            }]
        }));
        assert!(admitted_source_texts("get_graph_topology", &names_only).is_empty());
    }

    #[test]
    fn entity_type_and_label_are_observed_values_not_claim_material() {
        let supported = tool_result(json!({
            "entities": [{
                "entityId": Uuid::from_u128(4),
                "label": "Alpha controller",
                "entityType": "service",
                "summary": "Controls standby mode.",
                "supportCount": 3
            }]
        }));

        let texts = admitted_source_texts("search_entities", &supported);

        assert_eq!(texts, vec!["Alpha controller (service): Controls standby mode.".to_string()]);
        assert!(!texts.iter().any(|text| text.contains("entityType") || text.contains("label")));
    }

    #[test]
    fn relation_triple_without_a_recorded_summary_is_not_evidence() {
        let triple_only = tool_result(json!({
            "relations": [{
                "relationId": Uuid::from_u128(5),
                "sourceLabel": "Alpha controller",
                "targetLabel": "Beta controller",
                "relationType": "part_of",
                "summary": null
            }]
        }));

        assert!(admitted_source_texts("list_relations", &triple_only).is_empty());
    }

    #[test]
    fn relation_evidence_is_the_recorded_summary_alone() {
        let summary = "The standby cluster is documented in the operations guide.";
        let listed = tool_result(json!({
            "relations": [{
                "relationId": Uuid::from_u128(5),
                "sourceLabel": "Alpha controller",
                "targetLabel": "Beta controller",
                "relationType": "part_of",
                "summary": summary
            }]
        }));
        assert_eq!(admitted_source_texts("list_relations", &listed), vec![summary.to_string()]);

        let topology = tool_result(json!({
            "relations": [{
                "relationId": Uuid::from_u128(6),
                "relationType": "part_of",
                "summary": summary
            }]
        }));
        assert_eq!(
            admitted_source_texts("get_graph_topology", &topology),
            vec![summary.to_string()]
        );
    }

    #[test]
    fn community_summary_is_the_only_claim_text() {
        let names_only = tool_result(json!({
            "communities": [{
                "communityId": Uuid::from_u128(7),
                "summary": null,
                "topEntities": ["Alpha", "Beta"],
                "nodeCount": 2,
                "edgeCount": 1
            }]
        }));
        assert!(admitted_source_texts("get_communities", &names_only).is_empty());

        let supported = tool_result(json!({
            "communities": [{
                "communityId": Uuid::from_u128(8),
                "summary": "Standby services share the Alpha cluster.",
                "topEntities": ["Alpha"],
                "nodeCount": 3,
                "edgeCount": 2
            }]
        }));
        assert_eq!(
            admitted_source_texts("get_communities", &supported),
            vec!["Standby services share the Alpha cluster.".to_string()]
        );
    }

    fn runtime_execution_fixture() -> Value {
        json!({
            "runtimeExecutionId": Uuid::from_u128(9),
            "ownerKind": "query_execution",
            "ownerId": Uuid::from_u128(10),
            "taskKind": "query_answer",
            "surfaceKind": "ui",
            "contractName": "query-answer",
            "contractVersion": "1",
            "lifecycleState": "failed",
            "activeStage": "verify",
            "turnBudget": 4,
            "turnCount": 2,
            "parallelActionLimit": 1,
            "failureCode": "verification_failed",
            "failureSummary": "literalOnlyInFailureSummary",
            "policySummary": {
                "allowCount": 1,
                "rejectCount": 0,
                "terminateCount": 0,
                "recentDecisions": []
            },
            "acceptedAt": "2026-11-24T00:00:00Z",
            "completedAt": "2026-11-24T00:00:01Z"
        })
    }

    #[test]
    fn runtime_typed_states_are_evidence_but_arbitrary_summaries_are_not() {
        let summary = tool_result(runtime_execution_fixture());
        let texts = admitted_source_texts("get_runtime_execution", &summary);
        assert!(
            texts
                .iter()
                .any(|text| text.contains("failed") && text.contains("verification_failed"))
        );
        assert!(!texts.iter().any(|text| text.contains("literalOnlyInFailureSummary")));
        assert!(!texts.iter().any(|text| text.starts_with('{')));

        let wrapper_only = tool_result(json!({
            "execution": {"question": "literalOnlyInWrapper"}
        }));
        assert!(admitted_source_texts("get_runtime_execution_trace", &wrapper_only).is_empty());
        assert!(admitted_source_texts("get_runtime_execution", &wrapper_only).is_empty());
    }

    #[test]
    fn runtime_trace_keeps_typed_operations_and_drops_free_form_values() {
        let trace = tool_result(json!({
            "execution": runtime_execution_fixture(),
            "stages": [{
                "stageRecordId": Uuid::from_u128(11),
                "stageKind": "verify",
                "stageOrdinal": 4,
                "attemptNo": 1,
                "stageState": "completed",
                "deterministic": true,
                "startedAt": "2026-11-24T00:00:00Z",
                "completedAt": "2026-11-24T00:00:01Z",
                "failureCode": null,
                "inputSummary": {"question": "literalOnlyInInputSummary"},
                "outputSummary": {"answer": "literalOnlyInOutputSummary"}
            }],
            "actions": [{
                "actionId": Uuid::from_u128(12),
                "stageRecordId": Uuid::from_u128(11),
                "actionKind": "tool_request",
                "actionOrdinal": 1,
                "actionState": "completed",
                "providerBindingId": null,
                "toolName": "search_documents",
                "usage": {"prompt": "literalOnlyInUsage"},
                "summary": {"answer": "literalOnlyInActionSummary"},
                "createdAt": "2026-11-24T00:00:00Z"
            }],
            "policyDecisions": [{
                "decisionId": Uuid::from_u128(13),
                "stageRecordId": Uuid::from_u128(11),
                "actionRecordId": Uuid::from_u128(12),
                "targetKind": "tool_request",
                "decisionKind": "allow",
                "reasonCode": "policy_allow",
                "reasonSummary": "literalOnlyInReasonSummary",
                "createdAt": "2026-11-24T00:00:00Z"
            }]
        }));

        let texts = admitted_source_texts("get_runtime_execution_trace", &trace);

        assert!(texts.iter().any(|text| text.as_str() == "Runtime stage 4 verify is completed."));
        assert!(texts.iter().any(|text| text.as_str()
            == "Runtime action 1 tool_request is completed. Tool search_documents."));
        assert!(texts.iter().any(|text| text.as_str()
            == "Runtime policy decision allow on tool_request recorded reason policy_allow."));
        for literal in [
            "literalOnlyInInputSummary",
            "literalOnlyInOutputSummary",
            "literalOnlyInUsage",
            "literalOnlyInActionSummary",
            "literalOnlyInReasonSummary",
            "literalOnlyInFailureSummary",
        ] {
            assert!(
                !texts.iter().any(|text| text.contains(literal)),
                "unexpected literal {literal} in {texts:?}"
            );
        }
    }
}
