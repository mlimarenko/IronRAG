/// Three-tier entity deduplication for a single library.
///
/// Designed as an **offline batch job** invoked via the `consolidate-entities`
/// CLI subcommand. Never called inline from the ingest pipeline.
///
/// # Tiers
/// 1. **Exact** — entities sharing a `canonical_key` already collapsed by
///    `normalize_graph_identity_component`; this tier is a no-op (the upsert
///    path handles it at write time).
/// 2. **Semantic** — batch-embed every non-document entity label with the
///    library's `EmbedChunk` binding; for each same-type pair with cosine ≥
///    0.92 add to candidate list.
/// 3. **LLM verify** — for each semantic candidate check the
///    `entity_dedup_verification_cache` table; on a miss call the `Utility`
///    binding with a YES/NO prompt. On YES merge the pair: keep whichever node
///    has more inbound edges as canonical; retarget all edges from the
///    duplicate to the canonical; delete the duplicate row (cascade removes
///    its own edges).
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    infra::repositories::{
        self, RuntimeGraphEdgeRow, RuntimeGraphNodeRow, get_runtime_graph_snapshot,
    },
    integrations::llm::{ChatRequest, EmbeddingBatchRequest},
    services::graph::{
        identity::runtime_node_type_from_key, projection::active_projection_version,
    },
};

/// Summary returned from [`consolidate_entities`].
#[derive(Debug, Clone, serde::Serialize)]
pub struct DedupSummary {
    /// Tier 1 exact matches — always 0 (handled upstream by key normalization).
    pub tier1_merged: usize,
    /// Semantic candidate pairs from tier 2.
    pub tier2_candidates: usize,
    /// Pairs verified YES by the LLM in tier 3.
    pub tier3_verified: usize,
    /// Total entity pairs that were actually merged.
    pub merged: usize,
    /// Total canonical entities kept (survivors after merge).
    pub kept_canonical: usize,
}

const COSINE_THRESHOLD: f32 = 0.92;

/// Entry point for the offline deduplication job.
///
/// # Errors
/// - `Utility` binding not configured → fails loud.
/// - `EmbedChunk` binding not configured → fails loud.
/// - Any database or LLM call failure propagates.
pub async fn consolidate_entities(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<DedupSummary> {
    let pool = &state.persistence.postgres;

    // ── Resolve active projection version ────────────────────────────
    let snapshot = get_runtime_graph_snapshot(pool, library_id)
        .await
        .map_err(|e| anyhow::anyhow!("failed to fetch graph snapshot: {e}"))?;
    let projection_version = active_projection_version(snapshot.as_ref());

    // ── Fail-loud: require both bindings ────────────────────────────
    let embed_binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
        .await
        .map_err(|e| anyhow::anyhow!("failed to resolve EmbedChunk binding: {e}"))?
        .ok_or_else(|| {
            anyhow::anyhow!("binding=embed_chunk, reason=not_configured, library_id={library_id}")
        })?;

    let utility_binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::Utility)
        .await
        .map_err(|e| anyhow::anyhow!("failed to resolve Utility binding: {e}"))?
        .ok_or_else(|| {
            anyhow::anyhow!("binding=utility, reason=not_configured, library_id={library_id}")
        })?;

    // ── Load all non-document entity nodes ──────────────────────────
    let all_nodes =
        repositories::list_runtime_graph_nodes_by_library(pool, library_id, projection_version)
            .await
            .map_err(|e| anyhow::anyhow!("failed to load graph nodes: {e}"))?;

    // Exclude document nodes — they are 1:1 with content_document rows.
    let entities: Vec<RuntimeGraphNodeRow> = all_nodes
        .into_iter()
        .filter(|node| {
            runtime_node_type_from_key(&node.canonical_key)
                != crate::domains::runtime_graph::RuntimeNodeType::Document
        })
        .collect();

    if entities.is_empty() {
        return Ok(DedupSummary {
            tier1_merged: 0,
            tier2_candidates: 0,
            tier3_verified: 0,
            merged: 0,
            kept_canonical: 0,
        });
    }

    // ── Tier 1: exact — no-op (key normalization handles at upsert time) ──
    let tier1_merged = 0usize;

    // ── Tier 2: semantic ────────────────────────────────────────────
    let labels: Vec<String> = entities.iter().map(|e| e.label.clone()).collect();

    let batch_response = state
        .llm_gateway
        .embed_many(EmbeddingBatchRequest {
            provider_kind: embed_binding.provider_kind.clone(),
            model_name: embed_binding.model_name.clone(),
            inputs: labels,
            api_key_override: embed_binding.api_key.clone(),
            base_url_override: embed_binding.provider_base_url.clone(),
            extra_parameters_json: embed_binding.extra_parameters_json.clone(),
        })
        .await
        .map_err(|e| anyhow::anyhow!("embedding failed during tier-2 dedup: {e}"))?;

    let embeddings = &batch_response.embeddings;
    if embeddings.len() != entities.len() {
        anyhow::bail!(
            "embedding batch returned {} vectors but expected {}",
            embeddings.len(),
            entities.len()
        );
    }

    // Group entities by node_type to block all-pairs cosine to same type only.
    // This keeps the comparison O(k²) per type bucket rather than O(N²) globally.
    let mut type_buckets: std::collections::HashMap<&str, Vec<usize>> =
        std::collections::HashMap::new();
    for (idx, entity) in entities.iter().enumerate() {
        type_buckets.entry(&entity.node_type).or_default().push(idx);
    }

    let mut tier2_candidates: Vec<(usize, usize)> = Vec::new();
    for indices in type_buckets.values() {
        for i in 0..indices.len() {
            for j in (i + 1)..indices.len() {
                let a = indices[i];
                let b = indices[j];
                if cosine_similarity(&embeddings[a], &embeddings[b]) >= COSINE_THRESHOLD {
                    tier2_candidates.push((a, b));
                }
            }
        }
    }

    let tier2_count = tier2_candidates.len();

    // ── Tier 3: LLM verify with cache ────────────────────────────────
    let mut tier3_verified = 0usize;
    let mut merged = 0usize;

    // Track which nodes have already been merged as duplicate in this run
    // so we don't re-process them.
    let mut eliminated: std::collections::HashSet<Uuid> = std::collections::HashSet::new();

    for (idx_a, idx_b) in tier2_candidates {
        let entity_a = &entities[idx_a];
        let entity_b = &entities[idx_b];

        // Skip if either was already merged in this run.
        if eliminated.contains(&entity_a.id) || eliminated.contains(&entity_b.id) {
            continue;
        }

        let verdict =
            llm_verify_or_cache(pool, state, library_id, entity_a, entity_b, &utility_binding)
                .await?;

        if verdict {
            tier3_verified += 1;
            merge_entity_pair(pool, library_id, projection_version, entity_a, entity_b).await?;
            // The pair with fewer inbound edges gets eliminated; we find the
            // duplicate inside merge_entity_pair. We mark both as unsafe to
            // revisit — the canonical survives but may participate in future
            // pairs; to keep the run simple we mark both consumed.
            eliminated.insert(entity_a.id);
            eliminated.insert(entity_b.id);
            merged += 1;
        }
    }

    let kept_canonical = entities.len().saturating_sub(merged);

    Ok(DedupSummary {
        tier1_merged,
        tier2_candidates: tier2_count,
        tier3_verified,
        merged,
        kept_canonical,
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────

/// Cosine similarity between two equal-length f32 vectors.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let mag_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let mag_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if mag_a == 0.0 || mag_b == 0.0 {
        return 0.0;
    }
    dot / (mag_a * mag_b)
}

/// Derive the 32-byte cache key: SHA-256 of `library_id || sorted_keys`.
fn dedup_cache_key(library_id: Uuid, key_a: &str, key_b: &str) -> Vec<u8> {
    let mut ordered = [key_a, key_b];
    ordered.sort_unstable();
    let mut hasher = Sha256::new();
    hasher.update(library_id.as_bytes());
    hasher.update(b"|");
    hasher.update(ordered[0].as_bytes());
    hasher.update(b"|");
    hasher.update(ordered[1].as_bytes());
    hasher.finalize().to_vec()
}

/// Check cache for a prior verdict; on miss call LLM then persist.
///
/// # Errors
/// Returns an error if the LLM returns anything other than "YES" or "NO"
/// (fail-loud per canonical policy).
async fn llm_verify_or_cache(
    pool: &PgPool,
    state: &AppState,
    library_id: Uuid,
    entity_a: &RuntimeGraphNodeRow,
    entity_b: &RuntimeGraphNodeRow,
    utility_binding: &crate::services::ai_catalog_service::ResolvedRuntimeBinding,
) -> anyhow::Result<bool> {
    let cache_key = dedup_cache_key(library_id, &entity_a.canonical_key, &entity_b.canonical_key);

    // Check cache first.
    let cached: Option<bool> = sqlx::query_scalar::<_, bool>(
        "select verdict from entity_dedup_verification_cache where cache_key = $1",
    )
    .bind(&cache_key)
    .fetch_optional(pool)
    .await
    .map_err(|e| anyhow::anyhow!("cache lookup failed: {e}"))?;

    if let Some(verdict) = cached {
        return Ok(verdict);
    }

    // Call LLM.
    let prompt = format!(
        "In the context of technical documentation, are these two entities the same?\n\
         Entity A: \"{}\" (type: {})\n\
         Entity B: \"{}\" (type: {})\n\
         Answer YES or NO only.",
        entity_a.label, entity_a.node_type, entity_b.label, entity_b.node_type,
    );

    let response = state
        .llm_gateway
        .generate(ChatRequest {
            provider_kind: utility_binding.provider_kind.clone(),
            model_name: utility_binding.model_name.clone(),
            prompt,
            api_key_override: utility_binding.api_key.clone(),
            base_url_override: utility_binding.provider_base_url.clone(),
            system_prompt: utility_binding.system_prompt.clone(),
            temperature: utility_binding.temperature,
            top_p: utility_binding.top_p,
            max_output_tokens_override: Some(
                utility_binding.max_output_tokens_override.unwrap_or(8),
            ),
            response_format: None,
            extra_parameters_json: utility_binding.extra_parameters_json.clone(),
        })
        .await
        .map_err(|e| anyhow::anyhow!("LLM verify call failed: {e}"))?;

    let raw = response.output_text.trim().to_uppercase();
    let verdict = if raw == "YES" {
        true
    } else if raw == "NO" {
        false
    } else {
        anyhow::bail!(
            "utility LLM returned unexpected verdict {:?} for pair ({}, {}); expected YES or NO",
            response.output_text.trim(),
            entity_a.canonical_key,
            entity_b.canonical_key,
        );
    };

    // Persist to cache.
    sqlx::query(
        "insert into entity_dedup_verification_cache \
             (cache_key, library_id, entity_a_key, entity_b_key, verdict) \
         values ($1, $2, $3, $4, $5) \
         on conflict (cache_key) do nothing",
    )
    .bind(&cache_key)
    .bind(library_id)
    .bind(&entity_a.canonical_key)
    .bind(&entity_b.canonical_key)
    .bind(verdict)
    .execute(pool)
    .await
    .map_err(|e| anyhow::anyhow!("failed to persist dedup verdict to cache: {e}"))?;

    Ok(verdict)
}

/// Merge `entity_a` and `entity_b`: keep whichever has more inbound edges as
/// canonical; retarget all edges from the duplicate to the canonical key;
/// delete the duplicate row (ON DELETE CASCADE removes its edges).
async fn merge_entity_pair(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    entity_a: &RuntimeGraphNodeRow,
    entity_b: &RuntimeGraphNodeRow,
) -> anyhow::Result<()> {
    // Count inbound edges for each node.
    let inbound_a = count_inbound_edges(pool, library_id, projection_version, entity_a.id).await?;
    let inbound_b = count_inbound_edges(pool, library_id, projection_version, entity_b.id).await?;

    let (canonical, duplicate) =
        if inbound_a >= inbound_b { (entity_a, entity_b) } else { (entity_b, entity_a) };

    // Load all edges referencing the duplicate node.
    let dup_edges = load_edges_for_node(pool, library_id, projection_version, duplicate.id).await?;

    for edge in &dup_edges {
        // Compute new endpoints: replace duplicate with canonical.
        let new_from_id =
            if edge.from_node_id == duplicate.id { canonical.id } else { edge.from_node_id };
        let new_to_id =
            if edge.to_node_id == duplicate.id { canonical.id } else { edge.to_node_id };

        // Skip self-loops that would result from the merge.
        if new_from_id == new_to_id {
            continue;
        }

        // Compute new canonical key for the retargeted edge.
        let new_canonical_key =
            retargeted_edge_canonical_key(edge, &canonical.canonical_key, &duplicate.canonical_key);

        // Try to find an existing edge with the new canonical key.
        let existing: Option<(Uuid, i32)> = sqlx::query_as::<_, (Uuid, i32)>(
            "select id, support_count from runtime_graph_edge \
             where library_id = $1 and canonical_key = $2 and projection_version = $3",
        )
        .bind(library_id)
        .bind(&new_canonical_key)
        .bind(projection_version)
        .fetch_optional(pool)
        .await
        .map_err(|e| anyhow::anyhow!("failed to check existing retargeted edge: {e}"))?;

        if let Some((existing_id, existing_support)) = existing {
            // Merge: accumulate support_count into the existing edge, drop current.
            sqlx::query(
                "update runtime_graph_edge set support_count = $1, updated_at = now() where id = $2",
            )
            .bind(existing_support + edge.support_count)
            .bind(existing_id)
            .execute(pool)
            .await
            .map_err(|e| anyhow::anyhow!("failed to merge edge support counts: {e}"))?;

            // Delete the duplicate edge.
            sqlx::query("delete from runtime_graph_edge where id = $1")
                .bind(edge.id)
                .execute(pool)
                .await
                .map_err(|e| anyhow::anyhow!("failed to delete duplicate edge: {e}"))?;
        } else {
            // Retarget the edge to the canonical node.
            sqlx::query(
                "update runtime_graph_edge \
                 set from_node_id = $1, to_node_id = $2, canonical_key = $3, updated_at = now() \
                 where id = $4",
            )
            .bind(new_from_id)
            .bind(new_to_id)
            .bind(&new_canonical_key)
            .bind(edge.id)
            .execute(pool)
            .await
            .map_err(|e| anyhow::anyhow!("failed to retarget edge {}: {e}", edge.id))?;
        }
    }

    // Delete the duplicate node — CASCADE removes any remaining edges.
    sqlx::query("delete from runtime_graph_node where id = $1 and library_id = $2")
        .bind(duplicate.id)
        .bind(library_id)
        .execute(pool)
        .await
        .map_err(|e| anyhow::anyhow!("failed to delete duplicate node {}: {e}", duplicate.id))?;

    tracing::info!(
        canonical_key = %canonical.canonical_key,
        duplicate_key = %duplicate.canonical_key,
        canonical_id = %canonical.id,
        duplicate_id = %duplicate.id,
        "dedup: merged duplicate into canonical"
    );

    Ok(())
}

/// Recompute the canonical edge key after replacing `duplicate_key` with
/// `canonical_key` in `edge.canonical_key`.
fn retargeted_edge_canonical_key(
    edge: &RuntimeGraphEdgeRow,
    canonical_key: &str,
    duplicate_key: &str,
) -> String {
    edge.canonical_key.replace(duplicate_key, canonical_key)
}

async fn count_inbound_edges(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    node_id: Uuid,
) -> anyhow::Result<i64> {
    sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint from runtime_graph_edge \
         where library_id = $1 and projection_version = $2 and to_node_id = $3",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(node_id)
    .fetch_one(pool)
    .await
    .map_err(|e| anyhow::anyhow!("failed to count inbound edges for node {node_id}: {e}"))
}

async fn load_edges_for_node(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    node_id: Uuid,
) -> anyhow::Result<Vec<RuntimeGraphEdgeRow>> {
    sqlx::query_as::<_, RuntimeGraphEdgeRow>(
        "select id, library_id, from_node_id, to_node_id, relation_type, canonical_key, \
                summary, weight, support_count, metadata_json, projection_version, \
                created_at, updated_at \
         from runtime_graph_edge \
         where library_id = $1 and projection_version = $2 \
           and (from_node_id = $3 or to_node_id = $3)",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(node_id)
    .fetch_all(pool)
    .await
    .map_err(|e| anyhow::anyhow!("failed to load edges for node {node_id}: {e}"))
}
