use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domains::content::ContentSourceAccess;

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
