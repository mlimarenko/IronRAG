use std::collections::HashMap;

use anyhow::{Context, bail};
use serde_json::json;
use uuid::Uuid;

use crate::{
    agent_runtime::task::RuntimeTaskSpec,
    app::state::AppState,
    domains::{
        agent_runtime::RuntimeOverrideBudget,
        ai::AiBindingPurpose,
        provider_profiles::{EffectiveProviderProfile, ProviderModelSelection},
    },
    infra::repositories::{self, RuntimeGraphEdgeRow, RuntimeGraphNodeRow},
    integrations::llm::{EmbeddingBatchRequest, EmbeddingBatchResponse},
    shared::json_coercion::from_value_or_default,
};

const EMBEDDING_BATCH_SIZE: usize = 16;

#[derive(Debug, Clone, Default)]
pub struct RuntimeStageUsageSummary {
    pub call_count: usize,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub total_tokens: Option<i32>,
    prompt_token_sum: i64,
    completion_token_sum: i64,
    total_token_sum: i64,
    saw_prompt_tokens: bool,
    saw_completion_tokens: bool,
    saw_total_tokens: bool,
}

#[derive(Debug, Clone)]
pub struct RuntimeTaskExecutionContext {
    pub provider_profile: EffectiveProviderProfile,
    pub runtime_overrides: RuntimeOverrideBudget,
}

impl RuntimeStageUsageSummary {
    #[must_use]
    pub fn with_model(provider_kind: &str, model_name: &str) -> Self {
        Self {
            provider_kind: Some(provider_kind.to_string()),
            model_name: Some(model_name.to_string()),
            ..Self::default()
        }
    }

    pub fn absorb_usage_json(&mut self, usage_json: &serde_json::Value) {
        self.call_count += 1;
        if let Some(prompt_tokens) =
            usage_json.get("prompt_tokens").and_then(serde_json::Value::as_i64)
        {
            self.prompt_token_sum += prompt_tokens;
            self.saw_prompt_tokens = true;
        }
        if let Some(completion_tokens) =
            usage_json.get("completion_tokens").and_then(serde_json::Value::as_i64)
        {
            self.completion_token_sum += completion_tokens;
            self.saw_completion_tokens = true;
        }
        if let Some(total_tokens) =
            usage_json.get("total_tokens").and_then(serde_json::Value::as_i64)
        {
            self.total_token_sum += total_tokens;
            self.saw_total_tokens = true;
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn prompt_tokens(&self) -> Option<i32> {
        self.finalized_clone().prompt_tokens
    }

    #[cfg(test)]
    #[must_use]
    pub fn completion_tokens(&self) -> Option<i32> {
        self.finalized_clone().completion_tokens
    }

    #[cfg(test)]
    #[must_use]
    pub fn total_tokens(&self) -> Option<i32> {
        self.finalized_clone().total_tokens
    }

    #[cfg(test)]
    #[must_use]
    pub fn has_token_usage(&self) -> bool {
        self.total_tokens().is_some()
            || self.prompt_tokens().is_some()
            || self.completion_tokens().is_some()
    }

    /// Merges another usage summary into this one, combining token counts.
    pub fn merge(&mut self, other: &Self) {
        self.call_count += other.call_count;
        self.prompt_token_sum += other.prompt_token_sum;
        self.completion_token_sum += other.completion_token_sum;
        self.total_token_sum += other.total_token_sum;
        self.saw_prompt_tokens |= other.saw_prompt_tokens;
        self.saw_completion_tokens |= other.saw_completion_tokens;
        self.saw_total_tokens |= other.saw_total_tokens;
        // Keep provider_kind / model_name from self (they should match).
    }

    #[must_use]
    #[allow(clippy::wrong_self_convention)]
    pub fn into_usage_json(mut self) -> serde_json::Value {
        self.finalize();
        json!({
            "aggregation": "sum",
            "call_count": self.call_count,
            "provider_kind": self.provider_kind,
            "model_name": self.model_name,
            "prompt_tokens": self.prompt_tokens,
            "completion_tokens": self.completion_tokens,
            "total_tokens": self.total_tokens,
        })
    }

    #[allow(dead_code)]
    fn finalized_clone(&self) -> Self {
        let mut clone = self.clone();
        clone.finalize();
        clone
    }

    fn finalize(&mut self) {
        self.prompt_tokens = self
            .saw_prompt_tokens
            .then(|| i32::try_from(self.prompt_token_sum).unwrap_or(i32::MAX));
        self.completion_tokens = self
            .saw_completion_tokens
            .then(|| i32::try_from(self.completion_token_sum).unwrap_or(i32::MAX));
        let total_tokens = if self.saw_total_tokens {
            Some(i32::try_from(self.total_token_sum).unwrap_or(i32::MAX))
        } else if self.saw_prompt_tokens || self.saw_completion_tokens {
            Some(
                i32::try_from(self.prompt_token_sum.saturating_add(self.completion_token_sum))
                    .unwrap_or(i32::MAX),
            )
        } else {
            None
        };
        self.total_tokens = total_tokens;
    }
}

fn binding_purpose_label(binding_purpose: AiBindingPurpose) -> &'static str {
    match binding_purpose {
        AiBindingPurpose::ExtractText => "extract_text",
        AiBindingPurpose::ExtractGraph => "extract_graph",
        AiBindingPurpose::EmbedChunk => "embed_chunk",
        AiBindingPurpose::QueryRetrieve => "query_retrieve",
        AiBindingPurpose::QueryAnswer => "query_answer",
        AiBindingPurpose::Vision => "vision",
    }
}

async fn resolve_library_binding_selection(
    state: &AppState,
    library_id: Uuid,
    binding_purpose: AiBindingPurpose,
) -> anyhow::Result<ProviderModelSelection> {
    let binding_label = binding_purpose_label(binding_purpose);
    let binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, binding_purpose)
        .await
        .with_context(|| format!("failed to resolve active {binding_label} binding"))?
        .with_context(|| {
            format!("active {binding_label} binding is not configured for library {library_id}")
        })?;
    let provider_kind = binding.provider_kind.parse().map_err(|error: String| {
        anyhow::anyhow!("invalid provider kind for {binding_label}: {error}")
    })?;

    Ok(ProviderModelSelection { provider_kind, model_name: binding.model_name })
}

pub async fn resolve_effective_provider_profile(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<EffectiveProviderProfile> {
    Ok(EffectiveProviderProfile {
        indexing: resolve_library_binding_selection(
            state,
            library_id,
            AiBindingPurpose::ExtractGraph,
        )
        .await?,
        embedding: resolve_library_binding_selection(
            state,
            library_id,
            AiBindingPurpose::EmbedChunk,
        )
        .await?,
        answer: resolve_library_binding_selection(state, library_id, AiBindingPurpose::QueryAnswer)
            .await?,
        vision: resolve_library_binding_selection(state, library_id, AiBindingPurpose::Vision)
            .await?,
    })
}

#[must_use]
pub fn bounded_runtime_overrides(
    state: &AppState,
    task_spec: &RuntimeTaskSpec,
) -> RuntimeOverrideBudget {
    RuntimeOverrideBudget {
        max_turns: Some(state.agent_runtime_settings.max_turns.min(task_spec.max_turns)),
        max_parallel_actions: Some(
            state.agent_runtime_settings.max_parallel_actions.min(task_spec.max_parallel_actions),
        ),
    }
}

pub async fn resolve_effective_runtime_task_context(
    state: &AppState,
    library_id: Uuid,
    task_spec: &RuntimeTaskSpec,
) -> anyhow::Result<RuntimeTaskExecutionContext> {
    Ok(RuntimeTaskExecutionContext {
        provider_profile: resolve_effective_provider_profile(state, library_id).await?,
        runtime_overrides: bounded_runtime_overrides(state, task_spec),
    })
}

fn build_runtime_graph_node_vector_target_inputs(
    nodes: &[&RuntimeGraphNodeRow],
    batch_response: &EmbeddingBatchResponse,
) -> Vec<repositories::RuntimeVectorTargetUpsertInput> {
    nodes
        .iter()
        .zip(batch_response.embeddings.iter())
        .map(|(node, embedding)| repositories::RuntimeVectorTargetUpsertInput {
            library_id: node.library_id,
            target_kind: "entity".to_string(),
            target_id: node.id,
            provider_kind: batch_response.provider_kind.clone(),
            model_name: batch_response.model_name.clone(),
            dimensions: i32::try_from(embedding.len()).ok(),
            embedding_json: serde_json::to_value(embedding).unwrap_or_else(|_| json!([])),
        })
        .collect()
}

fn build_runtime_graph_edge_vector_target_inputs(
    edges: &[RuntimeGraphEdgeRow],
    batch_response: &EmbeddingBatchResponse,
) -> Vec<repositories::RuntimeVectorTargetUpsertInput> {
    edges
        .iter()
        .zip(batch_response.embeddings.iter())
        .map(|(edge, embedding)| repositories::RuntimeVectorTargetUpsertInput {
            library_id: edge.library_id,
            target_kind: "relation".to_string(),
            target_id: edge.id,
            provider_kind: batch_response.provider_kind.clone(),
            model_name: batch_response.model_name.clone(),
            dimensions: i32::try_from(embedding.len()).ok(),
            embedding_json: serde_json::to_value(embedding).unwrap_or_else(|_| json!([])),
        })
        .collect()
}

pub async fn embed_runtime_graph_nodes(
    state: &AppState,
    provider_profile: &EffectiveProviderProfile,
    nodes: &[RuntimeGraphNodeRow],
) -> anyhow::Result<RuntimeStageUsageSummary> {
    let nodes_to_embed =
        nodes.iter().filter(|node| node.node_type != "document").collect::<Vec<_>>();
    let Some(first_node) = nodes_to_embed.first() else {
        return Ok(RuntimeStageUsageSummary::with_model(
            provider_profile.embedding.provider_kind.as_str(),
            &provider_profile.embedding.model_name,
        ));
    };
    let embedding_binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, first_node.library_id, AiBindingPurpose::EmbedChunk)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "active embedding binding is not configured for library {}",
                first_node.library_id
            )
        })?;
    let mut usage = RuntimeStageUsageSummary::with_model(
        &embedding_binding.provider_kind,
        &embedding_binding.model_name,
    );
    for node_batch in nodes_to_embed.chunks(EMBEDDING_BATCH_SIZE) {
        let batch_response = state
            .llm_gateway
            .embed_many(EmbeddingBatchRequest {
                provider_kind: embedding_binding.provider_kind.clone(),
                model_name: embedding_binding.model_name.clone(),
                inputs: node_batch
                    .iter()
                    .map(|node| build_graph_node_embedding_input(node))
                    .collect::<Vec<_>>(),
                api_key_override: Some(embedding_binding.api_key.clone()),
                base_url_override: embedding_binding.provider_base_url.clone(),
            })
            .await
            .with_context(|| {
                format!(
                    "failed to embed graph node batch starting with {}",
                    node_batch.first().map(|node| node.id).unwrap_or_default()
                )
            })?;

        if batch_response.embeddings.len() != node_batch.len() {
            bail!(
                "embedding batch returned {} vectors for {} graph nodes",
                batch_response.embeddings.len(),
                node_batch.len(),
            );
        }

        repositories::upsert_runtime_vector_targets(
            &state.persistence.postgres,
            &build_runtime_graph_node_vector_target_inputs(node_batch, &batch_response),
        )
        .await
        .with_context(|| {
            format!(
                "failed to persist graph node embedding batch starting with {}",
                node_batch.first().map(|node| node.id).unwrap_or_default()
            )
        })?;
        usage.absorb_usage_json(&batch_response.usage_json);
    }

    Ok(usage)
}

pub async fn embed_runtime_graph_edges(
    state: &AppState,
    provider_profile: &EffectiveProviderProfile,
    nodes: &[RuntimeGraphNodeRow],
    edges: &[RuntimeGraphEdgeRow],
) -> anyhow::Result<RuntimeStageUsageSummary> {
    let node_index = nodes.iter().map(|node| (node.id, node)).collect::<HashMap<_, _>>();
    let Some(first_edge) = edges.first() else {
        return Ok(RuntimeStageUsageSummary::with_model(
            provider_profile.embedding.provider_kind.as_str(),
            &provider_profile.embedding.model_name,
        ));
    };
    let embedding_binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, first_edge.library_id, AiBindingPurpose::EmbedChunk)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "active embedding binding is not configured for library {}",
                first_edge.library_id
            )
        })?;
    let mut usage = RuntimeStageUsageSummary::with_model(
        &embedding_binding.provider_kind,
        &embedding_binding.model_name,
    );
    for edge_batch in edges.chunks(EMBEDDING_BATCH_SIZE) {
        let batch_response = state
            .llm_gateway
            .embed_many(EmbeddingBatchRequest {
                provider_kind: embedding_binding.provider_kind.clone(),
                model_name: embedding_binding.model_name.clone(),
                inputs: edge_batch
                    .iter()
                    .map(|edge| build_graph_edge_embedding_input(edge, &node_index))
                    .collect::<Vec<_>>(),
                api_key_override: Some(embedding_binding.api_key.clone()),
                base_url_override: embedding_binding.provider_base_url.clone(),
            })
            .await
            .with_context(|| {
                format!(
                    "failed to embed graph edge batch starting with {}",
                    edge_batch.first().map(|edge| edge.id).unwrap_or_default()
                )
            })?;

        if batch_response.embeddings.len() != edge_batch.len() {
            bail!(
                "embedding batch returned {} vectors for {} graph edges",
                batch_response.embeddings.len(),
                edge_batch.len(),
            );
        }

        repositories::upsert_runtime_vector_targets(
            &state.persistence.postgres,
            &build_runtime_graph_edge_vector_target_inputs(edge_batch, &batch_response),
        )
        .await
        .with_context(|| {
            format!(
                "failed to persist graph edge embedding batch starting with {}",
                edge_batch.first().map(|edge| edge.id).unwrap_or_default()
            )
        })?;
        usage.absorb_usage_json(&batch_response.usage_json);
    }

    Ok(usage)
}

fn build_graph_node_embedding_input(node: &RuntimeGraphNodeRow) -> String {
    let aliases: Vec<String> =
        from_value_or_default("runtime_graph_node.aliases_json", &node.aliases_json);
    let alias_text = aliases
        .into_iter()
        .filter(|alias| alias.trim() != node.label.trim())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "node_type: {}\nlabel: {}\naliases: {}\nsummary: {}\nmetadata: {}",
        node.node_type,
        node.label,
        alias_text,
        node.summary.clone().unwrap_or_default(),
        node.metadata_json,
    )
}

fn build_graph_edge_embedding_input(
    edge: &RuntimeGraphEdgeRow,
    node_index: &HashMap<Uuid, &RuntimeGraphNodeRow>,
) -> String {
    let from_label =
        node_index.get(&edge.from_node_id).map_or("unknown", |node| node.label.as_str());
    let to_label = node_index.get(&edge.to_node_id).map_or("unknown", |node| node.label.as_str());
    format!(
        "relation_type: {}\nsource: {}\ntarget: {}\nsummary: {}\nmetadata: {}",
        edge.relation_type,
        from_label,
        to_label,
        edge.summary.clone().unwrap_or_default(),
        edge.metadata_json,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn stage_usage_summary_exposes_finalized_tokens_without_consuming() {
        let mut usage = RuntimeStageUsageSummary::with_model("openai", "text-embedding-3-small");
        usage.absorb_usage_json(&json!({
            "prompt_tokens": 120,
        }));
        usage.absorb_usage_json(&json!({
            "prompt_tokens": 30,
            "completion_tokens": 5,
        }));

        assert_eq!(usage.prompt_tokens(), Some(150));
        assert_eq!(usage.completion_tokens(), Some(5));
        assert_eq!(usage.total_tokens(), Some(155));
        assert!(usage.has_token_usage());
    }

    #[test]
    fn graph_target_batches_keep_target_identity() {
        let library_id = Uuid::now_v7();
        let nodes = vec![RuntimeGraphNodeRow {
            id: Uuid::now_v7(),
            library_id,
            canonical_key: "entity::acme-corp".to_string(),
            label: "Acme Corp".to_string(),
            node_type: "entity".to_string(),
            aliases_json: json!([]),
            summary: Some("Budget owner".to_string()),
            metadata_json: json!({}),
            support_count: 1,
            projection_version: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }];
        let batch_response = EmbeddingBatchResponse {
            provider_kind: "openai".to_string(),
            model_name: "text-embedding-3-small".to_string(),
            dimensions: 1536,
            embeddings: vec![vec![0.2; 1536]],
            usage_json: json!({}),
        };

        let node_refs = nodes.iter().collect::<Vec<_>>();
        let target_rows =
            build_runtime_graph_node_vector_target_inputs(node_refs.as_slice(), &batch_response);

        assert_eq!(target_rows.len(), 1);
        assert_eq!(target_rows[0].target_kind, "entity");
        assert_eq!(target_rows[0].target_id, nodes[0].id);
        assert_eq!(target_rows[0].dimensions, Some(1536));
    }
}
