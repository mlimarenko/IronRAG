//! RAPTOR: Recursive Abstractive Processing for Tree-Organized Retrieval.
//!
//! This module builds a one-level (or multi-level) hierarchical summary tree
//! over an existing library's chunks. The tree is built offline as a CLI batch
//! job — it is never triggered automatically during ingest.
//!
//! Algorithm
//! ---------
//! 1. Load all leaf (non-RAPTOR) chunks for the library from ArangoDB.
//! 2. Partition the chunks into `k` clusters.
//!    Clustering uses a simple sliding-window bucketing strategy so that
//!    neighbouring chunks (which share context) end up in the same cluster.
//! 3. For each cluster, call the `QueryAnswer` LLM binding to produce a
//!    free-text summary of the cluster's content.
//! 4. Persist each summary as a synthetic `KnowledgeChunkRow` with
//!    `chunk_kind = "raptor_summary"` and `raptor_level = Some(level)`.
//!
//! The RAPTOR synthetic chunks are visible to the retrieval pipeline.  The
//! `map_chunk_hit` function in `retrieve.rs` bypasses the revision-staleness
//! filter for any chunk whose `raptor_level` is `Some(_)`, so they survive
//! document re-ingests without becoming invisible.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    infra::arangodb::document_store::KnowledgeChunkRow,
    integrations::llm::{ChatRequestSeed, build_text_chat_request},
};

/// Minimum number of chunks in a cluster before we ask the LLM to summarise it.
/// Clusters smaller than this are merged with a neighbour.
const MIN_CLUSTER_SIZE: usize = 2;

/// Default target number of chunks per cluster when the caller does not specify
/// a cluster size.
pub const DEFAULT_CLUSTER_SIZE: usize = 10;

/// Result of a [`build_raptor_tree`] run.
#[derive(Debug)]
pub struct RaptorBuildResult {
    /// Number of summary chunks inserted at this level.
    pub summaries_inserted: usize,
    /// Level number that was built (1-based).
    pub level: u32,
}

/// Build one level of the RAPTOR tree for a library.
///
/// # Arguments
/// * `state`        – Application state (Arango store + LLM gateway + AI catalog).
/// * `library_id`   – Library to build the tree for.
/// * `level`        – Tree level to build (1 = first summary layer over raw chunks).
/// * `cluster_size` – Target number of source chunks per cluster.
///
/// # Errors
/// Returns an error when ArangoDB or the LLM provider call fails.
pub async fn build_raptor_tree(
    state: &AppState,
    library_id: Uuid,
    level: u32,
    cluster_size: usize,
) -> Result<RaptorBuildResult> {
    info!(
        library_id = %library_id,
        level,
        cluster_size,
        "raptor: loading chunks for tree build",
    );

    // 1. Load all non-RAPTOR chunks for the library.
    let chunks = state
        .arango_document_store
        .list_chunks_by_library(library_id)
        .await
        .context("failed to load library chunks for RAPTOR")?;

    if chunks.is_empty() {
        info!(library_id = %library_id, "raptor: no chunks found; nothing to do");
        return Ok(RaptorBuildResult { summaries_inserted: 0, level });
    }

    info!(library_id = %library_id, chunk_count = chunks.len(), "raptor: chunks loaded");

    // 2. Resolve the LLM binding.
    let binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::QueryAnswer)
        .await
        .context("failed to resolve QueryAnswer binding for RAPTOR")?
        .context("no QueryAnswer binding configured for library")?;

    // 3. Partition chunks into clusters.
    let clusters = partition_into_clusters(&chunks, cluster_size.max(1));
    info!(library_id = %library_id, cluster_count = clusters.len(), "raptor: partitioned into clusters");

    // 4. Summarise each cluster and persist the result.
    let mut summaries_inserted = 0_usize;
    for (cluster_idx, cluster) in clusters.iter().enumerate() {
        let cluster_text = cluster
            .iter()
            .map(|c| c.content_text.trim())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");

        if cluster_text.trim().is_empty() {
            warn!(
                library_id = %library_id,
                cluster = cluster_idx,
                "raptor: skipping empty cluster",
            );
            continue;
        }

        let prompt = format!(
            "Summarise the following text passages into a single coherent paragraph that captures the main topics, key facts, and relationships:\n\n{cluster_text}"
        );

        let request = build_text_chat_request(
            ChatRequestSeed {
                provider_kind: binding.provider_kind.clone(),
                model_name: binding.model_name.clone(),
                api_key_override: binding.api_key.clone(),
                base_url_override: binding.provider_base_url.clone(),
                system_prompt: binding.system_prompt.clone(),
                temperature: binding.temperature,
                top_p: binding.top_p,
                max_output_tokens_override: binding.max_output_tokens_override,
                extra_parameters_json: binding.extra_parameters_json.clone(),
            },
            prompt,
        );

        let response = state
            .llm_gateway
            .generate(request)
            .await
            .with_context(|| format!("RAPTOR cluster {cluster_idx} LLM call failed"))?;

        let summary_text = response.output_text.trim().to_string();
        if summary_text.is_empty() {
            warn!(
                library_id = %library_id,
                cluster = cluster_idx,
                "raptor: LLM returned empty summary; skipping",
            );
            continue;
        }

        // Derive workspace_id and document_id from the first chunk in the cluster.
        // For a library-level synthetic chunk we use the same workspace/library but
        // a stable deterministic document_id derived from (library_id, level, cluster_idx).
        let first = &cluster[0];
        let summary_chunk_id = Uuid::now_v7();
        let literal_digest =
            format!("sha256:{}", hex::encode(Sha256::digest(summary_text.as_bytes())));

        let row = KnowledgeChunkRow {
            key: summary_chunk_id.to_string(),
            arango_id: None,
            arango_rev: None,
            chunk_id: summary_chunk_id,
            workspace_id: first.workspace_id,
            library_id: first.library_id,
            document_id: first.document_id,
            revision_id: first.revision_id,
            chunk_index: i32::try_from(cluster_idx).unwrap_or(i32::MAX),
            chunk_kind: Some("raptor_summary".to_string()),
            content_text: summary_text.clone(),
            normalized_text: summary_text.clone(),
            span_start: None,
            span_end: None,
            token_count: i32::try_from(summary_text.split_whitespace().count()).ok(),
            support_block_ids: Vec::new(),
            section_path: Vec::new(),
            heading_trail: Vec::new(),
            literal_digest: Some(literal_digest),
            chunk_state: "ready".to_string(),
            text_generation: None,
            vector_generation: None,
            quality_score: None,
            window_text: None,
            raptor_level: Some(i32::try_from(level).unwrap_or(1)),
            // RAPTOR summary chunks aggregate cluster content, not original
            // records, so they have no canonical temporal interpretation.
            occurred_at: None,
            occurred_until: None,
        };

        state.arango_document_store.upsert_chunk(&row).await.with_context(|| {
            format!("failed to persist RAPTOR summary chunk for cluster {cluster_idx}")
        })?;

        summaries_inserted += 1;
    }

    info!(
        library_id = %library_id,
        level,
        summaries_inserted,
        "raptor: tree build complete",
    );

    Ok(RaptorBuildResult { summaries_inserted, level })
}

/// Partition a slice of chunks into clusters of approximately `target_size` chunks.
///
/// Adjacent chunks are kept together so that each cluster has topical coherence.
/// If the final bucket would be smaller than [`MIN_CLUSTER_SIZE`], it is merged
/// into the preceding bucket.
fn partition_into_clusters(
    chunks: &[KnowledgeChunkRow],
    target_size: usize,
) -> Vec<Vec<&KnowledgeChunkRow>> {
    if chunks.is_empty() {
        return Vec::new();
    }

    let target = target_size.max(MIN_CLUSTER_SIZE);
    let mut clusters: Vec<Vec<&KnowledgeChunkRow>> = Vec::new();
    let mut current: Vec<&KnowledgeChunkRow> = Vec::new();

    for chunk in chunks {
        current.push(chunk);
        if current.len() >= target {
            clusters.push(std::mem::take(&mut current));
        }
    }

    // Handle leftover chunks.
    if !current.is_empty() {
        if current.len() < MIN_CLUSTER_SIZE && !clusters.is_empty() {
            // Merge small tail into the last cluster.
            // clusters is non-empty (checked above), so last_mut() is always Some.
            #[allow(clippy::unwrap_used)]
            clusters.last_mut().unwrap().extend(current);
        } else {
            clusters.push(current);
        }
    }

    clusters
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partition_into_clusters_produces_expected_count() {
        // 25 dummy chunks with target_size=10 → 2 full clusters + 1 tail cluster
        // merged into the 2nd → 2 clusters total (10 + 15).
        // Actually with MIN_CLUSTER_SIZE=2, the tail of 5 ≥ 2 so we get 3 clusters.
        let chunks: Vec<KnowledgeChunkRow> = (0_i32..25)
            .map(|i| KnowledgeChunkRow {
                key: Uuid::nil().to_string(),
                arango_id: None,
                arango_rev: None,
                chunk_id: Uuid::now_v7(),
                workspace_id: Uuid::nil(),
                library_id: Uuid::nil(),
                document_id: Uuid::nil(),
                revision_id: Uuid::nil(),
                chunk_index: i,
                chunk_kind: None,
                content_text: format!("chunk {i}"),
                normalized_text: format!("chunk {i}"),
                span_start: None,
                span_end: None,
                token_count: None,
                support_block_ids: Vec::new(),
                section_path: Vec::new(),
                heading_trail: Vec::new(),
                literal_digest: None,
                chunk_state: "ready".to_string(),
                text_generation: None,
                vector_generation: None,
                quality_score: None,
                window_text: None,
                raptor_level: None,
                occurred_at: None,
                occurred_until: None,
            })
            .collect();

        let clusters = partition_into_clusters(&chunks, 10);
        // 25 chunks / 10 per cluster = 2 full + 5 remainder ≥ MIN_CLUSTER_SIZE(2) → 3 clusters.
        assert_eq!(clusters.len(), 3, "expected 3 clusters for 25 chunks with target_size=10");
        // Total chunk count must be preserved.
        let total: usize = clusters.iter().map(|c| c.len()).sum();
        assert_eq!(total, 25);
    }

    #[test]
    fn partition_small_tail_merges_into_previous() {
        // 11 chunks with target_size=10 → 1 full (10) + tail of 1 < MIN_CLUSTER_SIZE → merged.
        let chunks: Vec<KnowledgeChunkRow> = (0_i32..11)
            .map(|i| KnowledgeChunkRow {
                key: Uuid::nil().to_string(),
                arango_id: None,
                arango_rev: None,
                chunk_id: Uuid::now_v7(),
                workspace_id: Uuid::nil(),
                library_id: Uuid::nil(),
                document_id: Uuid::nil(),
                revision_id: Uuid::nil(),
                chunk_index: i,
                chunk_kind: None,
                content_text: format!("chunk {i}"),
                normalized_text: format!("chunk {i}"),
                span_start: None,
                span_end: None,
                token_count: None,
                support_block_ids: Vec::new(),
                section_path: Vec::new(),
                heading_trail: Vec::new(),
                literal_digest: None,
                chunk_state: "ready".to_string(),
                text_generation: None,
                vector_generation: None,
                quality_score: None,
                window_text: None,
                raptor_level: None,
                occurred_at: None,
                occurred_until: None,
            })
            .collect();

        let clusters = partition_into_clusters(&chunks, 10);
        assert_eq!(clusters.len(), 1, "tail of 1 should be merged → 1 cluster of 11");
        assert_eq!(clusters[0].len(), 11);
    }
}
