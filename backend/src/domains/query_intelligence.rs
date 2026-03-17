use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domains::query_modes::RuntimeQueryMode;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryIntentFreshness {
    Fresh,
    Stale,
    Invalidated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryIntentCacheStatus {
    Miss,
    HitFresh,
    HitStaleRecomputed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct IntentKeywords {
    pub high_level: Vec<String>,
    pub low_level: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryIntentCacheEntry {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub normalized_question_hash: String,
    pub explicit_mode: RuntimeQueryMode,
    pub planned_mode: RuntimeQueryMode,
    pub keywords: IntentKeywords,
    pub intent_summary: Option<String>,
    pub source_truth_version: i64,
    pub freshness: QueryIntentFreshness,
    pub created_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryPlanningMetadata {
    pub requested_mode: RuntimeQueryMode,
    pub planned_mode: RuntimeQueryMode,
    pub intent_cache_status: QueryIntentCacheStatus,
    pub keywords: IntentKeywords,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RerankStatus {
    NotApplicable,
    Applied,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RerankMetadata {
    pub status: RerankStatus,
    pub candidate_count: usize,
    pub reordered_count: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextAssemblyStatus {
    DocumentOnly,
    GraphOnly,
    BalancedMixed,
    MixedSkewed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextAssemblyMetadata {
    pub status: ContextAssemblyStatus,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupedReferenceKind {
    Document,
    Relationship,
    Entity,
    Mixed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupedReference {
    pub id: String,
    pub kind: GroupedReferenceKind,
    pub rank: usize,
    pub title: String,
    pub excerpt: Option<String>,
    pub evidence_count: usize,
    pub support_ids: Vec<String>,
}
