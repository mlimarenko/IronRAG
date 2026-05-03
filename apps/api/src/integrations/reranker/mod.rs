mod cohere;
mod jina;

use anyhow::{Result, anyhow};
use uuid::Uuid;

use crate::{
    app::state::AppState, domains::ai::AiBindingPurpose, services::query::support::RerankCandidate,
};

use self::{cohere::cohere_rerank, jina::jina_rerank};

#[derive(Debug, Clone)]
pub struct RankedChunk {
    pub id: String,
    pub relevance_score: f32,
}

/// Resolves the `Rerank` binding for `library_id`, dispatches to the
/// appropriate provider (jina or cohere), and returns candidates sorted
/// by descending cross-encoder relevance score.
///
/// Fail-loud: if no binding is configured or the provider request fails,
/// returns `Err`. There is no keyword-overlap fallback.
pub async fn rerank_candidates(
    state: &AppState,
    library_id: Uuid,
    query: &str,
    candidates: &[RerankCandidate],
    top_n: usize,
) -> Result<Vec<RankedChunk>> {
    let binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::Rerank)
        .await
        .map_err(|e| anyhow!("failed to resolve rerank binding: {e:?}"))?
        .ok_or_else(|| {
            anyhow!(
                "no rerank binding configured for library {library_id}; \
                 configure an AiBinding with purpose=rerank or disable query_rerank_enabled"
            )
        })?;

    let api_key = binding.api_key.as_deref().unwrap_or("").to_string();
    let documents: Vec<String> = candidates.iter().map(|c| c.text.clone()).collect();

    let ranked_pairs = match binding.provider_kind.as_str() {
        "jina" => jina_rerank(&api_key, query, &documents, top_n).await?,
        "cohere" => cohere_rerank(&api_key, query, &documents, top_n).await?,
        other => return Err(anyhow!("unsupported reranker provider: {other}")),
    };

    let ranked = ranked_pairs
        .into_iter()
        .filter_map(|(idx, score)| {
            candidates.get(idx).map(|c| RankedChunk { id: c.id.clone(), relevance_score: score })
        })
        .collect();

    Ok(ranked)
}
