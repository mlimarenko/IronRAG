use std::collections::HashSet;

use anyhow::{Context, anyhow};
use chrono::{Duration, Utc};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        query_experience::{AssistantExperienceConfig, QueryModeDescriptor},
        query_intelligence::{
            ContextAssemblyMetadata, ContextAssemblyStatus, IntentKeywords, QueryIntentCacheStatus,
            QueryPlanningMetadata, RerankMetadata, RerankStatus,
        },
        query_modes::RuntimeQueryMode,
    },
    infra::repositories,
    services::query_planner::extract_keywords,
};

#[derive(Debug, Clone, Default)]
pub struct QueryIntelligenceService;

#[derive(Debug, Clone)]
pub struct IntentResolutionRequest {
    pub library_id: Uuid,
    pub question: String,
    pub explicit_mode: RuntimeQueryMode,
    pub source_truth_version: i64,
}

#[derive(Debug, Clone)]
pub struct RerankRequest {
    pub question: String,
    pub requested_mode: RuntimeQueryMode,
    pub candidate_count: usize,
    pub enabled: bool,
    pub result_limit: usize,
}

#[derive(Debug, Clone)]
pub struct RerankCandidate {
    pub id: String,
    pub text: String,
    pub score: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct RerankOutcome {
    pub entities: Vec<String>,
    pub relationships: Vec<String>,
    pub chunks: Vec<String>,
    pub metadata: RerankMetadata,
}

impl QueryIntelligenceService {
    #[must_use]
    pub fn assistant_config(&self) -> AssistantExperienceConfig {
        AssistantExperienceConfig {
            scope_hint_key: "graph.assistantSubtitle".to_string(),
            default_prompt_keys: vec![
                "graph.defaultPrompts.connectedEntities".to_string(),
                "graph.defaultPrompts.topEvidence".to_string(),
                "graph.defaultPrompts.mainThemes".to_string(),
                "graph.defaultPrompts.isolatedItems".to_string(),
            ],
            modes: vec![
                descriptor(
                    RuntimeQueryMode::Document,
                    "graph.queryModes.document",
                    "graph.queryModeHelp.document.description",
                    "graph.queryModeHelp.document.bestFor",
                    Some("graph.queryModeHelp.document.caution"),
                    "graph.queryModeHelp.document.example",
                ),
                descriptor(
                    RuntimeQueryMode::Local,
                    "graph.queryModes.local",
                    "graph.queryModeHelp.local.description",
                    "graph.queryModeHelp.local.bestFor",
                    Some("graph.queryModeHelp.local.caution"),
                    "graph.queryModeHelp.local.example",
                ),
                descriptor(
                    RuntimeQueryMode::Global,
                    "graph.queryModes.global",
                    "graph.queryModeHelp.global.description",
                    "graph.queryModeHelp.global.bestFor",
                    Some("graph.queryModeHelp.global.caution"),
                    "graph.queryModeHelp.global.example",
                ),
                descriptor(
                    RuntimeQueryMode::Hybrid,
                    "graph.queryModes.hybrid",
                    "graph.queryModeHelp.hybrid.description",
                    "graph.queryModeHelp.hybrid.bestFor",
                    Some("graph.queryModeHelp.hybrid.caution"),
                    "graph.queryModeHelp.hybrid.example",
                ),
                descriptor(
                    RuntimeQueryMode::Mix,
                    "graph.queryModes.mix",
                    "graph.queryModeHelp.mix.description",
                    "graph.queryModeHelp.mix.bestFor",
                    Some("graph.queryModeHelp.mix.caution"),
                    "graph.queryModeHelp.mix.example",
                ),
            ],
        }
    }

    pub async fn resolve_intent(
        &self,
        state: &AppState,
        request: &IntentResolutionRequest,
    ) -> anyhow::Result<QueryPlanningMetadata> {
        let normalized_hash = normalized_question_hash(&request.question);
        let postgres = &state.persistence.postgres;
        let expires_at = cache_expiry(state);

        repositories::mark_query_intent_cache_entries_stale_for_project(
            postgres,
            request.library_id,
            request.source_truth_version,
        )
        .await
        .context("failed to mark stale query intent cache rows")?;

        if let Some(entry) = repositories::get_query_intent_cache_entry_for_reuse(
            postgres,
            request.library_id,
            &normalized_hash,
            request.explicit_mode.as_str(),
            request.source_truth_version,
            Utc::now(),
        )
        .await
        .context("failed to load reusable query intent cache entry")?
        {
            repositories::touch_query_intent_cache_entry(postgres, entry.id, expires_at)
                .await
                .context("failed to touch query intent cache entry")?;
            return Ok(metadata_from_row(entry, QueryIntentCacheStatus::HitFresh));
        }

        let latest_entry = repositories::find_latest_query_intent_cache_entry(
            postgres,
            request.library_id,
            &normalized_hash,
            request.explicit_mode.as_str(),
        )
        .await
        .context("failed to inspect latest query intent cache entry")?;

        let cache_status = if latest_entry.is_some() {
            QueryIntentCacheStatus::HitStaleRecomputed
        } else {
            QueryIntentCacheStatus::Miss
        };
        let metadata = build_fallback_metadata(request, cache_status);

        let workspace_id = repositories::get_project_by_id(postgres, request.library_id)
            .await
            .context("failed to load project while persisting query intent cache")?
            .map(|project| project.workspace_id)
            .ok_or_else(|| anyhow::anyhow!("project {} not found", request.library_id))?;

        repositories::upsert_query_intent_cache_entry(
            postgres,
            workspace_id,
            request.library_id,
            &normalized_hash,
            request.explicit_mode.as_str(),
            metadata.planned_mode.as_str(),
            serde_json::to_value(&metadata.keywords.high_level)
                .unwrap_or_else(|_| serde_json::json!([])),
            serde_json::to_value(&metadata.keywords.low_level)
                .unwrap_or_else(|_| serde_json::json!([])),
            None,
            request.source_truth_version,
            "fresh",
            expires_at,
        )
        .await
        .context("failed to upsert query intent cache entry")?;

        repositories::prune_query_intent_cache_entries_for_project(
            postgres,
            request.library_id,
            i64::try_from(state.retrieval_intelligence.query_intent_cache_max_entries_per_library)
                .unwrap_or(i64::MAX),
        )
        .await
        .context("failed to prune query intent cache rows")?;

        Ok(metadata)
    }

    pub fn rerank_stub(&self, request: &RerankRequest) -> RerankMetadata {
        let status = match request.requested_mode {
            RuntimeQueryMode::Hybrid | RuntimeQueryMode::Mix if request.enabled => {
                RerankStatus::Skipped
            }
            RuntimeQueryMode::Hybrid | RuntimeQueryMode::Mix => RerankStatus::Skipped,
            _ => RerankStatus::NotApplicable,
        };
        RerankMetadata { status, candidate_count: request.candidate_count, reordered_count: None }
    }

    pub fn rerank_hybrid_candidates(
        &self,
        request: &RerankRequest,
        entities: &[RerankCandidate],
        relationships: &[RerankCandidate],
        chunks: &[RerankCandidate],
    ) -> RerankOutcome {
        rerank_candidate_bundle(request, entities, relationships, chunks)
            .unwrap_or_else(|_| fallback_failed_rerank_outcome(entities, relationships, chunks))
    }

    pub fn rerank_mix_candidates(
        &self,
        request: &RerankRequest,
        entities: &[RerankCandidate],
        relationships: &[RerankCandidate],
        chunks: &[RerankCandidate],
    ) -> RerankOutcome {
        rerank_candidate_bundle(request, entities, relationships, chunks)
            .unwrap_or_else(|_| fallback_failed_rerank_outcome(entities, relationships, chunks))
    }

    #[must_use]
    pub fn context_assembly_stub(
        &self,
        requested_mode: RuntimeQueryMode,
        graph_support_count: usize,
        document_support_count: usize,
    ) -> ContextAssemblyMetadata {
        let (status, warning) = match requested_mode {
            RuntimeQueryMode::Document => (ContextAssemblyStatus::DocumentOnly, None),
            RuntimeQueryMode::Local | RuntimeQueryMode::Global => {
                (ContextAssemblyStatus::GraphOnly, None)
            }
            RuntimeQueryMode::Hybrid | RuntimeQueryMode::Mix => {
                if graph_support_count == 0 || document_support_count == 0 {
                    (
                        ContextAssemblyStatus::MixedSkewed,
                        Some(
                            "Combined mode returned uneven support; inspect both graph and document evidence before relying on the answer."
                                .to_string(),
                        ),
                    )
                } else if graph_support_count.abs_diff(document_support_count) > 2 {
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
}

fn build_fallback_metadata(
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

fn metadata_from_row(
    row: repositories::QueryIntentCacheEntryRow,
    cache_status: QueryIntentCacheStatus,
) -> QueryPlanningMetadata {
    QueryPlanningMetadata {
        requested_mode: row
            .explicit_mode
            .parse::<RuntimeQueryMode>()
            .unwrap_or(RuntimeQueryMode::Hybrid),
        planned_mode: row
            .planned_mode
            .parse::<RuntimeQueryMode>()
            .unwrap_or(RuntimeQueryMode::Hybrid),
        intent_cache_status: cache_status,
        keywords: IntentKeywords {
            high_level: serde_json::from_value(row.high_level_keywords_json).unwrap_or_default(),
            low_level: serde_json::from_value(row.low_level_keywords_json).unwrap_or_default(),
        },
        warnings: Vec::new(),
    }
}

fn cache_expiry(state: &AppState) -> chrono::DateTime<Utc> {
    Utc::now()
        + Duration::hours(
            i64::try_from(state.retrieval_intelligence.query_intent_cache_ttl_hours).unwrap_or(24),
        )
}

fn normalized_question_hash(question: &str) -> String {
    let normalized = question
        .split_whitespace()
        .map(|token| token.trim_matches(|ch: char| !ch.is_alphanumeric()).to_lowercase())
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    hex::encode(hasher.finalize())
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

fn fallback_failed_rerank_outcome(
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
        let service = QueryIntelligenceService;
        let outcome = service.rerank_hybrid_candidates(
            &RerankRequest {
                question: "budget approval".to_string(),
                requested_mode: RuntimeQueryMode::Hybrid,
                candidate_count: 3,
                enabled: false,
                result_limit: 2,
            },
            &[RerankCandidate {
                id: "e1".to_string(),
                text: "Budget committee".to_string(),
                score: Some(0.5),
            }],
            &[],
            &[RerankCandidate {
                id: "c1".to_string(),
                text: "Approval memo".to_string(),
                score: Some(0.4),
            }],
        );

        assert_eq!(outcome.metadata.status, RerankStatus::Skipped);
    }

    #[test]
    fn rerank_bundle_reorders_candidates_by_keyword_overlap() {
        let service = QueryIntelligenceService;
        let outcome = service.rerank_mix_candidates(
            &RerankRequest {
                question: "budget approval".to_string(),
                requested_mode: RuntimeQueryMode::Mix,
                candidate_count: 4,
                enabled: true,
                result_limit: 2,
            },
            &[RerankCandidate {
                id: "e1".to_string(),
                text: "General project node".to_string(),
                score: Some(0.9),
            }],
            &[],
            &[
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
            ],
        );

        assert_eq!(outcome.metadata.status, RerankStatus::Applied);
        assert_eq!(outcome.chunks.first().map(String::as_str), Some("c1"));
        assert!(outcome.metadata.reordered_count.unwrap_or_default() > 0);
    }

    #[test]
    fn rerank_bundle_returns_failed_metadata_when_candidates_are_invalid() {
        let service = QueryIntelligenceService;
        let outcome = service.rerank_hybrid_candidates(
            &RerankRequest {
                question: "budget approval".to_string(),
                requested_mode: RuntimeQueryMode::Hybrid,
                candidate_count: 3,
                enabled: true,
                result_limit: 1,
            },
            &[
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
            ],
            &[],
            &[],
        );

        assert_eq!(outcome.metadata.status, RerankStatus::Failed);
        assert_eq!(outcome.entities, vec!["dup".to_string(), "dup".to_string()]);
    }
}

fn descriptor(
    mode: RuntimeQueryMode,
    label_key: &str,
    short_description_key: &str,
    best_for_key: &str,
    caution_key: Option<&str>,
    example_question_key: &str,
) -> QueryModeDescriptor {
    QueryModeDescriptor {
        mode,
        label_key: label_key.to_string(),
        short_description_key: short_description_key.to_string(),
        best_for_key: best_for_key.to_string(),
        caution_key: caution_key.map(std::string::ToString::to_string),
        example_question_key: example_question_key.to_string(),
    }
}
