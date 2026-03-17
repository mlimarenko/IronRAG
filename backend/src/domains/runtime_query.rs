use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domains::{
    provider_profiles::ProviderModelSelection,
    query_intelligence::{
        ContextAssemblyMetadata, GroupedReference, QueryPlanningMetadata, RerankMetadata,
    },
    query_modes::RuntimeQueryMode,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroundingStatus {
    Grounded,
    Partial,
    Weak,
    None,
}

impl GroundingStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Grounded => "grounded",
            Self::Partial => "partial",
            Self::Weak => "weak",
            Self::None => "none",
        }
    }
}

impl std::str::FromStr for GroundingStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "grounded" => Ok(Self::Grounded),
            "partial" => Ok(Self::Partial),
            "weak" => Ok(Self::Weak),
            "none" => Ok(Self::None),
            other => Err(format!("unsupported grounding status: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeQueryExecution {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub mode: RuntimeQueryMode,
    pub question: String,
    pub answer: Option<String>,
    pub grounding_status: GroundingStatus,
    pub provider: ProviderModelSelection,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeQueryReference {
    pub kind: String,
    pub reference_id: Uuid,
    pub excerpt: Option<String>,
    pub rank: usize,
    pub score: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeQueryEnrichment {
    pub planning: QueryPlanningMetadata,
    pub rerank: RerankMetadata,
    pub context_assembly: ContextAssemblyMetadata,
    pub grouped_references: Vec<GroupedReference>,
}
