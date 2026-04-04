use std::collections::{HashMap, HashSet};

use anyhow::{Context, anyhow};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    domains::query::{
        ContextAssemblyMetadata, ContextAssemblyStatus, GroupedReference, GroupedReferenceKind,
        IntentKeywords, QueryIntentCacheStatus, QueryPlanningMetadata, RerankMetadata,
        RerankStatus, RuntimeQueryMode,
    },
    services::query_planner::extract_keywords,
};

#[derive(Debug, Clone)]
pub struct IntentResolutionRequest {
    pub library_id: Uuid,
    pub question: String,
    pub explicit_mode: RuntimeQueryMode,
    pub source_truth_version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RerankRequest {
    pub question: String,
    pub requested_mode: RuntimeQueryMode,
    pub candidate_count: usize,
    pub enabled: bool,
    pub result_limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RerankCandidate {
    pub id: String,
    pub text: String,
    pub score: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RerankOutcome {
    pub entities: Vec<String>,
    pub relationships: Vec<String>,
    pub chunks: Vec<String>,
    pub metadata: RerankMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryRerankTaskInput {
    pub request: RerankRequest,
    pub entity_candidates: Vec<RerankCandidate>,
    pub relationship_candidates: Vec<RerankCandidate>,
    pub chunk_candidates: Vec<RerankCandidate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryRerankFailureCode {
    InvalidResultLimit,
    DuplicateCandidateId,
}

impl QueryRerankFailureCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidResultLimit => "invalid_result_limit",
            Self::DuplicateCandidateId => "duplicate_candidate_id",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryRerankFailure {
    pub code: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextAssemblyRequest {
    pub requested_mode: RuntimeQueryMode,
    pub graph_support_count: usize,
    pub document_support_count: usize,
}

#[derive(Debug, Clone)]
pub struct GroupedReferenceCandidate {
    pub dedupe_key: String,
    pub kind: GroupedReferenceKind,
    pub rank: usize,
    pub title: String,
    pub excerpt: Option<String>,
    pub support_id: String,
}

pub async fn invalidate_library_source_truth(
    state: &crate::app::state::AppState,
    library_id: Uuid,
) -> anyhow::Result<i64> {
    let source_truth_version = crate::infra::repositories::touch_project_source_truth_version(
        &state.persistence.postgres,
        library_id,
    )
    .await
    .context("failed to touch project source-truth version")?;
    Ok(source_truth_version)
}

#[must_use]
pub fn derive_query_planning_metadata(request: &IntentResolutionRequest) -> QueryPlanningMetadata {
    let _ = (request.library_id, request.source_truth_version);
    derive_planning_metadata(request, QueryIntentCacheStatus::Miss)
}

#[must_use]
pub fn derive_rerank_metadata(request: &RerankRequest) -> RerankMetadata {
    let status =
        if matches!(request.requested_mode, RuntimeQueryMode::Hybrid | RuntimeQueryMode::Mix) {
            RerankStatus::Skipped
        } else {
            RerankStatus::NotApplicable
        };
    RerankMetadata { status, candidate_count: request.candidate_count, reordered_count: None }
}

pub fn rerank_query_candidates(
    input: &QueryRerankTaskInput,
) -> Result<RerankOutcome, QueryRerankFailure> {
    if input.request.result_limit == 0 {
        return Err(QueryRerankFailure {
            code: QueryRerankFailureCode::InvalidResultLimit.as_str().to_string(),
            summary: "query rerank result limit must be greater than zero".to_string(),
        });
    }

    rerank_candidate_bundle(
        &input.request,
        &input.entity_candidates,
        &input.relationship_candidates,
        &input.chunk_candidates,
    )
    .map_err(map_query_rerank_failure)
}

#[must_use]
pub fn assemble_context_metadata(request: &ContextAssemblyRequest) -> ContextAssemblyMetadata {
    let (status, warning) = match request.requested_mode {
        RuntimeQueryMode::Document => (ContextAssemblyStatus::DocumentOnly, None),
        RuntimeQueryMode::Local | RuntimeQueryMode::Global => {
            (ContextAssemblyStatus::GraphOnly, None)
        }
        RuntimeQueryMode::Hybrid | RuntimeQueryMode::Mix => {
            if request.graph_support_count == 0 || request.document_support_count == 0 {
                (
                    ContextAssemblyStatus::MixedSkewed,
                    Some(
                        "Combined mode returned uneven support; inspect both graph and document evidence before relying on the answer."
                            .to_string(),
                    ),
                )
            } else if request.graph_support_count.abs_diff(request.document_support_count) > 2 {
                (
                    ContextAssemblyStatus::MixedSkewed,
                    Some(
                        "One evidence source dominated the combined context, so the answer may reflect only part of the library."
                            .to_string(),
                    ),
                )
            } else {
                (ContextAssemblyStatus::BalancedMixed, None)
            }
        }
    };
    ContextAssemblyMetadata { status, warning }
}

#[must_use]
pub fn group_visible_references(
    candidates: &[GroupedReferenceCandidate],
    limit: usize,
) -> Vec<GroupedReference> {
    let mut grouped = HashMap::<String, GroupAccumulator>::new();
    for candidate in candidates {
        let entry =
            grouped.entry(candidate.dedupe_key.clone()).or_insert_with(|| GroupAccumulator {
                dedupe_key: candidate.dedupe_key.clone(),
                kind: candidate.kind.clone(),
                rank: candidate.rank,
                title: candidate.title.clone(),
                excerpt: candidate.excerpt.clone(),
                support_ids: Vec::new(),
            });
        if entry.kind != candidate.kind {
            entry.kind = GroupedReferenceKind::Mixed;
        }
        if candidate.rank < entry.rank {
            entry.rank = candidate.rank;
            entry.title = candidate.title.clone();
        }
        if entry.excerpt.is_none() {
            entry.excerpt = candidate.excerpt.clone();
        }
        if !entry.support_ids.iter().any(|value| value == &candidate.support_id) {
            entry.support_ids.push(candidate.support_id.clone());
        }
    }

    let mut grouped = grouped.into_values().collect::<Vec<_>>();
    grouped.sort_by(|left, right| {
        left.rank.cmp(&right.rank).then_with(|| left.title.cmp(&right.title))
    });
    grouped.truncate(limit);
    grouped
        .into_iter()
        .enumerate()
        .map(|(index, group)| GroupedReference {
            id: group.dedupe_key,
            kind: group.kind,
            rank: index + 1,
            title: group.title,
            excerpt: group.excerpt,
            evidence_count: group.support_ids.len(),
            support_ids: group.support_ids,
        })
        .collect()
}

#[derive(Debug, Clone)]
struct GroupAccumulator {
    dedupe_key: String,
    kind: GroupedReferenceKind,
    rank: usize,
    title: String,
    excerpt: Option<String>,
    support_ids: Vec<String>,
}

fn derive_planning_metadata(
    request: &IntentResolutionRequest,
    cache_status: QueryIntentCacheStatus,
) -> QueryPlanningMetadata {
    let keywords = extract_keywords(&request.question);
    let high_level = keywords.iter().take(3).cloned().collect::<Vec<_>>();
    let low_level = keywords.iter().skip(3).cloned().collect::<Vec<_>>();

    QueryPlanningMetadata {
        requested_mode: request.explicit_mode,
        planned_mode: request.explicit_mode,
        intent_cache_status: cache_status,
        keywords: IntentKeywords { high_level, low_level },
        warnings: Vec::new(),
    }
}

fn rerank_candidate_bundle(
    request: &RerankRequest,
    entities: &[RerankCandidate],
    relationships: &[RerankCandidate],
    chunks: &[RerankCandidate],
) -> anyhow::Result<RerankOutcome> {
    let candidate_count = entities.len() + relationships.len() + chunks.len();
    if !request.enabled || candidate_count == 0 || candidate_count <= request.result_limit {
        return Ok(RerankOutcome {
            entities: entities.iter().map(|item| item.id.clone()).collect(),
            relationships: relationships.iter().map(|item| item.id.clone()).collect(),
            chunks: chunks.iter().map(|item| item.id.clone()).collect(),
            metadata: RerankMetadata {
                status: RerankStatus::Skipped,
                candidate_count,
                reordered_count: None,
            },
        });
    }

    validate_unique_candidate_ids(entities)?;
    validate_unique_candidate_ids(relationships)?;
    validate_unique_candidate_ids(chunks)?;

    let keywords = extract_keywords(&request.question);
    if keywords.is_empty() {
        return Ok(RerankOutcome {
            entities: entities.iter().map(|item| item.id.clone()).collect(),
            relationships: relationships.iter().map(|item| item.id.clone()).collect(),
            chunks: chunks.iter().map(|item| item.id.clone()).collect(),
            metadata: RerankMetadata {
                status: RerankStatus::Skipped,
                candidate_count,
                reordered_count: None,
            },
        });
    }

    let (entity_ids, entity_reordered) = rerank_candidate_list(entities, &keywords);
    let (relationship_ids, relationship_reordered) =
        rerank_candidate_list(relationships, &keywords);
    let (chunk_ids, chunk_reordered) = rerank_candidate_list(chunks, &keywords);

    Ok(RerankOutcome {
        entities: entity_ids,
        relationships: relationship_ids,
        chunks: chunk_ids,
        metadata: RerankMetadata {
            status: RerankStatus::Applied,
            candidate_count,
            reordered_count: Some(entity_reordered + relationship_reordered + chunk_reordered),
        },
    })
}

pub(crate) fn build_failed_rerank_outcome(
    entities: &[RerankCandidate],
    relationships: &[RerankCandidate],
    chunks: &[RerankCandidate],
) -> RerankOutcome {
    RerankOutcome {
        entities: entities.iter().map(|item| item.id.clone()).collect(),
        relationships: relationships.iter().map(|item| item.id.clone()).collect(),
        chunks: chunks.iter().map(|item| item.id.clone()).collect(),
        metadata: RerankMetadata {
            status: RerankStatus::Failed,
            candidate_count: entities.len() + relationships.len() + chunks.len(),
            reordered_count: None,
        },
    }
}

fn map_query_rerank_failure(error: anyhow::Error) -> QueryRerankFailure {
    let summary = error.to_string();
    let code = if summary.contains("duplicate rerank candidate id") {
        QueryRerankFailureCode::DuplicateCandidateId
    } else {
        QueryRerankFailureCode::InvalidResultLimit
    };
    QueryRerankFailure { code: code.as_str().to_string(), summary }
}

fn validate_unique_candidate_ids(candidates: &[RerankCandidate]) -> anyhow::Result<()> {
    let mut seen = HashSet::new();
    for candidate in candidates {
        if !seen.insert(candidate.id.as_str()) {
            return Err(anyhow!("duplicate rerank candidate id {}", candidate.id));
        }
    }
    Ok(())
}

fn rerank_candidate_list(
    candidates: &[RerankCandidate],
    keywords: &[String],
) -> (Vec<String>, usize) {
    let original_ids = candidates.iter().map(|item| item.id.clone()).collect::<Vec<_>>();
    let mut ranked = candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let overlap = lexical_overlap_score(&candidate.text, keywords);
            let combined_score = score_value(candidate.score) * 0.35 + overlap * 0.65;
            (index, candidate.id.clone(), combined_score)
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| right.2.total_cmp(&left.2).then_with(|| left.0.cmp(&right.0)));
    let ordered_ids = ranked.into_iter().map(|(_, id, _)| id).collect::<Vec<_>>();
    let reordered_count =
        ordered_ids.iter().zip(original_ids.iter()).filter(|(left, right)| left != right).count();
    (ordered_ids, reordered_count)
}

fn lexical_overlap_score(text: &str, keywords: &[String]) -> f32 {
    if keywords.is_empty() {
        return 0.0;
    }
    let normalized = text.to_ascii_lowercase();
    let matched = keywords.iter().filter(|keyword| normalized.contains(keyword.as_str())).count();
    matched as f32 / keywords.len() as f32
}

fn score_value(score: Option<f32>) -> f32 {
    score.unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rerank_bundle_skips_when_disabled() {
        let request = RerankRequest {
            question: "budget approval".to_string(),
            requested_mode: RuntimeQueryMode::Hybrid,
            candidate_count: 3,
            enabled: false,
            result_limit: 2,
        };
        let entities = [RerankCandidate {
            id: "e1".to_string(),
            text: "Budget committee".to_string(),
            score: Some(0.5),
        }];
        let chunks = [RerankCandidate {
            id: "c1".to_string(),
            text: "Approval memo".to_string(),
            score: Some(0.4),
        }];
        let outcome = rerank_query_candidates(&QueryRerankTaskInput {
            request,
            entity_candidates: entities.to_vec(),
            relationship_candidates: Vec::new(),
            chunk_candidates: chunks.to_vec(),
        })
        .unwrap_or_else(|_| build_failed_rerank_outcome(&entities, &[], &chunks));

        assert_eq!(outcome.metadata.status, RerankStatus::Skipped);
    }

    #[test]
    fn rerank_bundle_reorders_candidates_by_keyword_overlap() {
        let request = RerankRequest {
            question: "budget approval".to_string(),
            requested_mode: RuntimeQueryMode::Mix,
            candidate_count: 4,
            enabled: true,
            result_limit: 2,
        };
        let entities = [RerankCandidate {
            id: "e1".to_string(),
            text: "General project node".to_string(),
            score: Some(0.9),
        }];
        let chunks = [
            RerankCandidate {
                id: "c2".to_string(),
                text: "Unrelated rollout draft".to_string(),
                score: Some(0.8),
            },
            RerankCandidate {
                id: "c1".to_string(),
                text: "Budget approval memo".to_string(),
                score: Some(0.2),
            },
        ];
        let outcome = rerank_query_candidates(&QueryRerankTaskInput {
            request,
            entity_candidates: entities.to_vec(),
            relationship_candidates: Vec::new(),
            chunk_candidates: chunks.to_vec(),
        })
        .unwrap_or_else(|_| build_failed_rerank_outcome(&entities, &[], &chunks));

        assert_eq!(outcome.metadata.status, RerankStatus::Applied);
        assert_eq!(outcome.chunks.first().map(String::as_str), Some("c1"));
        assert!(outcome.metadata.reordered_count.unwrap_or_default() > 0);
    }

    #[test]
    fn rerank_bundle_returns_failed_metadata_when_candidates_are_invalid() {
        let request = RerankRequest {
            question: "budget approval".to_string(),
            requested_mode: RuntimeQueryMode::Hybrid,
            candidate_count: 3,
            enabled: true,
            result_limit: 1,
        };
        let entities = [
            RerankCandidate {
                id: "dup".to_string(),
                text: "Budget committee".to_string(),
                score: Some(0.5),
            },
            RerankCandidate {
                id: "dup".to_string(),
                text: "Approval committee".to_string(),
                score: Some(0.4),
            },
        ];
        let outcome = rerank_query_candidates(&QueryRerankTaskInput {
            request,
            entity_candidates: entities.to_vec(),
            relationship_candidates: Vec::new(),
            chunk_candidates: Vec::new(),
        })
        .unwrap_or_else(|_| build_failed_rerank_outcome(&entities, &[], &[]));

        assert_eq!(outcome.metadata.status, RerankStatus::Failed);
        assert_eq!(outcome.entities, vec!["dup".to_string(), "dup".to_string()]);
    }

    #[test]
    fn group_visible_references_deduplicates_support_ids_by_key() {
        let grouped = group_visible_references(
            &[
                GroupedReferenceCandidate {
                    dedupe_key: "document:1".to_string(),
                    kind: GroupedReferenceKind::Document,
                    rank: 2,
                    title: "Roadmap".to_string(),
                    excerpt: Some("Q2 delivery plan".to_string()),
                    support_id: "chunk:1".to_string(),
                },
                GroupedReferenceCandidate {
                    dedupe_key: "document:1".to_string(),
                    kind: GroupedReferenceKind::Document,
                    rank: 1,
                    title: "Roadmap".to_string(),
                    excerpt: Some("Q2 delivery plan".to_string()),
                    support_id: "chunk:2".to_string(),
                },
            ],
            8,
        );

        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].rank, 1);
        assert_eq!(grouped[0].evidence_count, 2);
        assert_eq!(grouped[0].support_ids, vec!["chunk:1".to_string(), "chunk:2".to_string()]);
    }

    #[test]
    fn group_visible_references_marks_mixed_when_sources_collide() {
        let grouped = group_visible_references(
            &[
                GroupedReferenceCandidate {
                    dedupe_key: "focus:alpha".to_string(),
                    kind: GroupedReferenceKind::Entity,
                    rank: 1,
                    title: "Alpha".to_string(),
                    excerpt: None,
                    support_id: "node:1".to_string(),
                },
                GroupedReferenceCandidate {
                    dedupe_key: "focus:alpha".to_string(),
                    kind: GroupedReferenceKind::Relationship,
                    rank: 2,
                    title: "Alpha depends on Beta".to_string(),
                    excerpt: None,
                    support_id: "edge:1".to_string(),
                },
            ],
            8,
        );

        assert_eq!(grouped[0].kind, GroupedReferenceKind::Mixed);
        assert_eq!(grouped[0].evidence_count, 2);
    }
}
