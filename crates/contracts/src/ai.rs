//! Canonical AI binding purpose contracts.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// The six physical model bindings exposed to operators and runtime stages.
pub enum AiBindingPurpose {
    /// Multimodal document understanding, including text and visual extraction.
    ExtractText,
    /// Entity, relation, and graph-contribution extraction.
    ExtractGraph,
    /// The single embedding space shared by indexing and query lookup.
    EmbedChunk,
    /// Typed query compilation and provider-backed semantic reranking.
    QueryCompile,
    /// Evidence-grounded answer generation and bounded repair.
    QueryAnswer,
    /// Tool-capable assistant turns for the UI and MCP agent contour.
    Agent,
}

impl AiBindingPurpose {
    #[must_use]
    /// Returns the canonical database, environment, and JSON wire value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExtractText => "extract_text",
            Self::ExtractGraph => "extract_graph",
            Self::EmbedChunk => "embed_chunk",
            Self::QueryCompile => "query_compile",
            Self::QueryAnswer => "query_answer",
            Self::Agent => "agent",
        }
    }
}

impl std::str::FromStr for AiBindingPurpose {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "extract_text" => Ok(Self::ExtractText),
            "extract_graph" => Ok(Self::ExtractGraph),
            "embed_chunk" => Ok(Self::EmbedChunk),
            "query_compile" => Ok(Self::QueryCompile),
            "query_answer" => Ok(Self::QueryAnswer),
            "agent" => Ok(Self::Agent),
            other => Err(format!("unsupported AI binding purpose: {other}")),
        }
    }
}
