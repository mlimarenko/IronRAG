//! Shared AI binding purpose contracts.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// The eight physical model bindings exposed to operators and runtime stages.
pub enum AiBindingPurpose {
    /// Multimodal document understanding, including text and visual extraction.
    ExtractText,
    /// Entity, relation, and graph-contribution extraction.
    ExtractGraph,
    /// Typed classification of graph entities into their entity type, bound
    /// independently of extraction.
    GraphEntityType,
    /// The single embedding space shared by indexing and query lookup.
    EmbedChunk,
    /// Typed query compilation, `HyDE` expansion, and query consolidation.
    QueryCompile,
    /// Provider-backed semantic reranking of retrieved candidates, bound
    /// independently of query compilation.
    QueryRerank,
    /// Evidence-grounded answer generation and bounded repair.
    QueryAnswer,
    /// Tool-capable assistant turns for the UI and MCP agent contour.
    Agent,
}

impl AiBindingPurpose {
    /// Every purpose in the canonical order the database enum declares and the
    /// runtime consumes: `order by binding_purpose` follows the enum's
    /// declaration order, so a live additive upgrade must insert a new value at
    /// the same position a fresh install declares, never append it.
    pub const ALL: [Self; 8] = [
        Self::ExtractText,
        Self::ExtractGraph,
        Self::GraphEntityType,
        Self::EmbedChunk,
        Self::QueryCompile,
        Self::QueryRerank,
        Self::QueryAnswer,
        Self::Agent,
    ];

    #[must_use]
    /// Returns the stored database, environment, and JSON wire value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExtractText => "extract_text",
            Self::ExtractGraph => "extract_graph",
            Self::GraphEntityType => "graph_entity_type",
            Self::EmbedChunk => "embed_chunk",
            Self::QueryCompile => "query_compile",
            Self::QueryRerank => "query_rerank",
            Self::QueryAnswer => "query_answer",
            Self::Agent => "agent",
        }
    }

    #[must_use]
    /// Whether this purpose is served by a typed decision runtime (`TypeSafe`
    /// System One) rather than a generative chat/completion model. A decision
    /// provider may store or activate a binding only for one of these
    /// purposes; see `crates::contracts::provider::ProviderCapabilities::decision`.
    pub const fn is_decision(self) -> bool {
        matches!(self, Self::QueryRerank | Self::GraphEntityType)
    }
}

impl std::str::FromStr for AiBindingPurpose {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "extract_text" => Ok(Self::ExtractText),
            "extract_graph" => Ok(Self::ExtractGraph),
            "graph_entity_type" => Ok(Self::GraphEntityType),
            "embed_chunk" => Ok(Self::EmbedChunk),
            "query_compile" => Ok(Self::QueryCompile),
            "query_rerank" => Ok(Self::QueryRerank),
            "query_answer" => Ok(Self::QueryAnswer),
            "agent" => Ok(Self::Agent),
            other => Err(format!("unsupported AI binding purpose: {other}")),
        }
    }
}
