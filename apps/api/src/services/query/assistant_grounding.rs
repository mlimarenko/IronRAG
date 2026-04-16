use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    domains::content::{ContentSourceAccess, ContentSourceAccessKind},
    mcp_types::{McpContentSourceAccess, McpReadDocumentResponse},
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AssistantGroundingEvidence {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) verification_corpus: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) document_references: Vec<AssistantGroundingDocumentReference>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

impl AssistantGroundingEvidence {
    pub(crate) fn record_tool_result(
        &mut self,
        tool_name: &str,
        raw_tool_message_text: &str,
        is_error: bool,
    ) {
        if is_error {
            return;
        }
        if !raw_tool_message_text.trim().is_empty() {
            // Verification must see the full MCP payload the model grounded on;
            // UI truncation and token budgeting are separate concerns.
            self.verification_corpus.push(raw_tool_message_text.to_string());
        }
        if tool_name != "read_document" {
            return;
        }
        for fragment in parse_read_document_verification_fragments(raw_tool_message_text) {
            if !fragment.trim().is_empty() {
                self.verification_corpus.push(fragment);
            }
        }
        let rank =
            i32::try_from(self.document_references.len().saturating_add(1)).unwrap_or(i32::MAX);
        let Some(reference) = parse_read_document_reference(raw_tool_message_text, rank) else {
            return;
        };
        let duplicate = self.document_references.iter().any(|current| {
            current.document_id == reference.document_id
                && current.revision_id == reference.revision_id
                && current.slice_start_offset == reference.slice_start_offset
                && current.slice_end_offset == reference.slice_end_offset
        });
        if !duplicate {
            self.document_references.push(reference);
        }
    }
}

fn parse_read_document_reference(
    raw_tool_message_text: &str,
    rank: i32,
) -> Option<AssistantGroundingDocumentReference> {
    let response = parse_read_document_response(raw_tool_message_text)?;
    let content = response
        .content
        .as_deref()
        .or(response.visual_description.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    Some(AssistantGroundingDocumentReference {
        document_id: response.document_id,
        revision_id: response.latest_revision_id,
        document_title: response.document_title,
        source_uri: response.source_uri,
        source_access: response.source_access.as_ref().map(map_source_access),
        slice_start_offset: response.slice_start_offset,
        slice_end_offset: response.slice_end_offset,
        excerpt: excerpt_preview(content),
        rank,
    })
}

fn parse_read_document_verification_fragments(raw_tool_message_text: &str) -> Vec<String> {
    let Some(response) = parse_read_document_response(raw_tool_message_text) else {
        return Vec::new();
    };
    let mut fragments = Vec::new();
    if let Some(content) = response.content {
        fragments.push(content);
    }
    if let Some(visual_description) = response.visual_description {
        fragments.push(visual_description);
    }
    fragments.push(response.document_title);
    if let Some(source_uri) = response.source_uri {
        fragments.push(source_uri);
    }
    if let Some(source_access) = response.source_access {
        fragments.push(source_access.href);
    }
    fragments
}

fn parse_read_document_response(raw_tool_message_text: &str) -> Option<McpReadDocumentResponse> {
    let payload = serde_json::from_str::<serde_json::Value>(raw_tool_message_text).ok()?;
    if payload.get("isError").and_then(serde_json::Value::as_bool).unwrap_or(false) {
        return None;
    }
    let structured_content = payload.get("structuredContent")?.clone();
    serde_json::from_value::<McpReadDocumentResponse>(structured_content).ok()
}

fn map_source_access(access: &McpContentSourceAccess) -> ContentSourceAccess {
    ContentSourceAccess {
        kind: match access.kind.trim().to_ascii_lowercase().as_str() {
            "stored_document" => ContentSourceAccessKind::StoredDocument,
            _ => ContentSourceAccessKind::ExternalUrl,
        },
        href: access.href.clone(),
    }
}

fn excerpt_preview(content: &str) -> String {
    let compact = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= 220 {
        return compact;
    }
    let mut excerpt = String::with_capacity(224);
    for (index, ch) in compact.chars().enumerate() {
        if index >= 220 {
            break;
        }
        excerpt.push(ch);
    }
    excerpt.push('…');
    excerpt
}

#[cfg(test)]
mod tests {
    use super::AssistantGroundingEvidence;

    #[test]
    fn record_tool_result_adds_decoded_read_document_content_to_verification_corpus() {
        let mut grounding = AssistantGroundingEvidence::default();
        grounding.record_tool_result(
            "read_document",
            r#"{"isError":false,"structuredContent":{"documentId":"019d9758-e88e-7b30-b15a-a355a029f6f3","documentTitle":"audit_repository.rs","libraryId":"019d9724-4d6f-75a2-87e4-65cc050fa9d0","workspaceId":"019d96c1-77d9-76b3-a33d-92e3c517127c","readMode":"full","readabilityState":"readable","readinessKind":"graph_sparse","graphCoverageKind":"graph_sparse","content":"surface_kind = \"bootstrap\" and result_kind = \"succeeded\"","sliceStartOffset":0,"sliceEndOffset":64,"hasMore":false}}"#,
            false,
        );

        assert!(
            grounding.verification_corpus.iter().any(|fragment| fragment.contains("\"bootstrap\""))
        );
        assert!(
            grounding.verification_corpus.iter().any(|fragment| fragment.contains("\"succeeded\""))
        );
    }
}
