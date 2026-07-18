use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domains::content::ContentSourceAccess;

#[derive(Debug, Clone, Default, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AssistantGroundingEvidence {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) verification_corpus: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) document_references: Vec<AssistantGroundingDocumentReference>,
}

impl AssistantGroundingEvidence {
    pub(crate) fn has_verifier_grade_evidence(&self) -> bool {
        self.document_references.iter().any(|reference| reference.has_excerpt())
            || self
                .verification_corpus
                .iter()
                .any(|fragment| verification_fragment_is_verifier_grade(fragment))
    }

    pub(crate) fn verifier_grade_corpus(&self) -> impl Iterator<Item = &str> {
        self.verification_corpus
            .iter()
            .map(String::as_str)
            .filter(|fragment| verification_fragment_is_verifier_grade(fragment))
    }

    pub(crate) fn verifier_grade_document_references(
        &self,
    ) -> impl Iterator<Item = &AssistantGroundingDocumentReference> {
        self.document_references.iter().filter(|reference| reference.has_excerpt())
    }
}

fn verification_fragment_is_verifier_grade(fragment: &str) -> bool {
    const MCP_TOOL_RESULT_PREFIX: &str = "[MCP tool result: ";
    if !fragment.starts_with(MCP_TOOL_RESULT_PREFIX) {
        return true;
    }
    let Some(tool_name) = fragment
        .strip_prefix(MCP_TOOL_RESULT_PREFIX)
        .and_then(|tail| tail.split_once(']').map(|(tool_name, _)| tool_name))
    else {
        return false;
    };
    match tool_name {
        "grounded_answer" | "read_document" => true,
        "search_documents" => search_documents_fragment_has_excerpt(fragment),
        "search_entities"
        | "get_graph_topology"
        | "list_relations"
        | "get_communities"
        | "get_runtime_execution"
        | "get_runtime_execution_trace" => true,
        _ => false,
    }
}

fn search_documents_fragment_has_excerpt(fragment: &str) -> bool {
    let Some((_, result_body)) = fragment.split_once(']') else {
        return false;
    };
    let Some(json_start) = result_body.find(['{', '[']) else {
        return false;
    };
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(&result_body[json_start..]) else {
        return false;
    };
    json_value_has_non_empty_excerpt(&payload)
}

fn json_value_has_non_empty_excerpt(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(object) => {
            object
                .get("excerpt")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|excerpt| !excerpt.trim().is_empty())
                || object.values().any(json_value_has_non_empty_excerpt)
        }
        serde_json::Value::Array(values) => values.iter().any(json_value_has_non_empty_excerpt),
        _ => false,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AssistantGroundingDocumentReference {
    pub(crate) document_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) revision_id: Option<Uuid>,
    pub(crate) document_title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_access: Option<ContentSourceAccess>,
    pub(crate) slice_start_offset: usize,
    pub(crate) slice_end_offset: usize,
    pub(crate) excerpt: String,
    pub(crate) rank: i32,
}

impl AssistantGroundingDocumentReference {
    fn has_excerpt(&self) -> bool {
        !self.excerpt.trim().is_empty()
    }
}
