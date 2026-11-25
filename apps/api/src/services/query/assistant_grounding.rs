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
    /// Stored corpora may include model-facing envelopes from earlier turns.
    pub(crate) fn source_texts(&self) -> impl Iterator<Item = &str> {
        self.verification_corpus
            .iter()
            .map(String::as_str)
            .filter(|fragment| !fragment.trim_start().starts_with(MCP_TOOL_RESULT_PREFIX))
            .chain(self.document_references.iter().map(|reference| reference.excerpt.as_str()))
            .filter(|text| !text.trim().is_empty())
    }
}

/// One canonical chunk identity admitted to the agent evidence budget before a
/// model-visible source is emitted. `text` is exactly the canonical chunk text
/// the verifier re-reads; it is never reconstructed from rendered prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AdmittedAgentSource {
    pub(crate) chunk_id: Uuid,
    pub(crate) document_id: Uuid,
    pub(crate) document_revision_id: Uuid,
    pub(crate) text: String,
}

/// Sequential agent-turn evidence manifest. It starts from the typed replay
/// loader and grows only through the shared
/// [`EvidenceBudget`](crate::services::query::execution::semantic_verification::EvidenceBudget); the verifier and
/// persistence consume exactly these identities.
#[derive(Debug, Clone, Default)]
pub(crate) struct AgentEvidenceManifest {
    pub(crate) sources: Vec<AdmittedAgentSource>,
    /// True when a source was rejected by the shared budget or a replay cap.
    /// The final warning owner reports partial coverage; nothing is dropped
    /// after generation.
    pub(crate) selection_bounded: bool,
}

impl AgentEvidenceManifest {
    /// Rechecked chunks and direct tool evidence shared by verification and literal repair.
    pub(crate) fn source_texts<'a>(
        &'a self,
        grounding: &'a AssistantGroundingEvidence,
    ) -> impl Iterator<Item = &'a str> {
        self.sources.iter().map(|source| source.text.as_str()).chain(grounding.source_texts())
    }

    pub(crate) fn source_context(&self, grounding: &AssistantGroundingEvidence) -> String {
        self.source_texts(grounding).collect::<Vec<_>>().join("\n\n")
    }

    /// Adds a distinct admitted source. Returns false when the identity was
    /// already present, so callers can keep the budget and manifest aligned.
    pub(crate) fn push_source(&mut self, source: AdmittedAgentSource) -> bool {
        if self.sources.iter().any(|prior| prior.chunk_id == source.chunk_id) {
            return false;
        }
        self.sources.push(source);
        true
    }
}

const MCP_TOOL_RESULT_PREFIX: &str = "[MCP tool result: ";

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
