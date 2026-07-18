use anyhow::Context;
use futures::stream::{self, StreamExt};
use std::collections::HashMap;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    integrations::llm::{EmbeddingBatchRequest, EmbeddingBatchResponse},
    services::{ai_catalog_service::ResolvedRuntimeBinding, ingest::error::IngestServiceError},
    shared::extraction::chunking::{BlockSentenceEmbeddings, split_into_sentences},
    shared::extraction::structured_document::StructuredBlockData,
};

const SENTENCE_EMBEDDING_BATCH_SIZE: usize = 16;

type SentenceEmbeddingBatch = Vec<(Uuid, usize, String)>;
type SentenceEmbeddingBatchResult =
    anyhow::Result<(SentenceEmbeddingBatch, EmbeddingBatchResponse)>;

struct BlockSentenceEntry {
    block_id: Uuid,
    sentences: Vec<String>,
}

/// Embeds all sentences from the provided blocks in batches of
/// [`SENTENCE_EMBEDDING_BATCH_SIZE`] using the library's `EmbedChunk` binding.
///
/// Returns a map from `block_id` → per-sentence embedding vectors, aligned
/// with the sentence list produced by [`split_into_sentences`] on each block's
/// `normalized_text`. Blocks whose text splits into fewer than 2 sentences are
/// excluded (they don't need semantic splitting, so spending tokens on them
/// would be waste).
///
/// Emits the metric `chunk.sentence_embed_count` (total sentences embedded)
/// via the returned `usize`.
pub async fn embed_sentences_for_blocks(
    state: &AppState,
    library_id: Uuid,
    blocks: &[StructuredBlockData],
    max_tokens_per_chunk: usize,
) -> Result<(BlockSentenceEmbeddings, usize), IngestServiceError> {
    let embedding_binding = resolve_sentence_embedding_binding(state, library_id).await?;
    let block_entries = collect_block_sentence_entries(blocks, max_tokens_per_chunk);
    if block_entries.is_empty() {
        return Ok((BlockSentenceEmbeddings::new(), 0));
    }

    let batches = build_sentence_embedding_batches(&block_entries);
    let total_sentences = batches.iter().map(Vec::len).sum();
    let batch_results = embed_sentence_batches(state, &embedding_binding, batches).await;
    let result = assemble_sentence_embeddings(&block_entries, batch_results)?;
    Ok((result, total_sentences))
}

async fn resolve_sentence_embedding_binding(
    state: &AppState,
    library_id: Uuid,
) -> Result<ResolvedRuntimeBinding, IngestServiceError> {
    state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
        .await
        .context("failed to resolve EmbedChunk binding for sentence embeddings")?
        .ok_or_else(|| {
            anyhow::anyhow!("active EmbedChunk binding is not configured for library {library_id}")
                .into()
        })
}

fn collect_block_sentence_entries(
    blocks: &[StructuredBlockData],
    max_tokens_per_chunk: usize,
) -> Vec<BlockSentenceEntry> {
    blocks
        .iter()
        .filter(|block| !block.is_boilerplate)
        .filter_map(|block| block_sentence_entry(block, max_tokens_per_chunk))
        .collect()
}

fn block_sentence_entry(
    block: &StructuredBlockData,
    max_tokens_per_chunk: usize,
) -> Option<BlockSentenceEntry> {
    let token_count = block.normalized_text.split_whitespace().count();
    if token_count <= max_tokens_per_chunk {
        return None;
    }
    let sentences = split_into_sentences(block.normalized_text.trim())
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    if sentences.len() < 2 {
        return None;
    }
    Some(BlockSentenceEntry { block_id: block.block_id, sentences })
}

fn build_sentence_embedding_batches(
    block_entries: &[BlockSentenceEntry],
) -> Vec<SentenceEmbeddingBatch> {
    let flattened = block_entries
        .iter()
        .flat_map(|entry| {
            entry
                .sentences
                .iter()
                .enumerate()
                .map(|(index, sentence)| (entry.block_id, index, sentence.clone()))
        })
        .collect::<Vec<_>>();
    flattened.chunks(SENTENCE_EMBEDDING_BATCH_SIZE).map(<[_]>::to_vec).collect()
}

async fn embed_sentence_batches(
    state: &AppState,
    binding: &ResolvedRuntimeBinding,
    batches: Vec<SentenceEmbeddingBatch>,
) -> Vec<SentenceEmbeddingBatchResult> {
    let parallelism = state.settings.ingestion_embedding_parallelism.max(1);
    stream::iter(batches.into_iter().map(|batch| {
        let request = sentence_embedding_batch_request(binding, &batch);
        async move {
            let response = state
                .llm_gateway
                .embed_many(request)
                .await
                .context("failed to embed sentence batch")?;
            Ok((batch, response))
        }
    }))
    .buffer_unordered(parallelism)
    .collect()
    .await
}

fn sentence_embedding_batch_request(
    binding: &ResolvedRuntimeBinding,
    batch: &SentenceEmbeddingBatch,
) -> EmbeddingBatchRequest {
    EmbeddingBatchRequest {
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
        inputs: batch.iter().map(|(_, _, text)| text.clone()).collect(),
        api_key_override: binding.api_key.clone(),
        base_url_override: binding.provider_base_url.clone(),
        extra_parameters_json: binding.extra_parameters_json.clone(),
    }
}

fn assemble_sentence_embeddings(
    block_entries: &[BlockSentenceEntry],
    batch_results: Vec<SentenceEmbeddingBatchResult>,
) -> Result<BlockSentenceEmbeddings, IngestServiceError> {
    let mut result = block_entries
        .iter()
        .map(|entry| (entry.block_id, vec![Vec::new(); entry.sentences.len()]))
        .collect::<HashMap<_, _>>();
    for batch_result in batch_results {
        let (batch, response) = batch_result?;
        merge_sentence_embedding_batch(&mut result, batch, response)?;
    }
    result.retain(|_, embeddings| embeddings.iter().all(|embedding| !embedding.is_empty()));
    Ok(result)
}

fn merge_sentence_embedding_batch(
    result: &mut BlockSentenceEmbeddings,
    batch: SentenceEmbeddingBatch,
    response: EmbeddingBatchResponse,
) -> Result<(), IngestServiceError> {
    if response.embeddings.len() != batch.len() {
        return Err(IngestServiceError::ProviderUnavailable {
            message: format!(
                "sentence embedding batch returned {} vectors for {} inputs",
                response.embeddings.len(),
                batch.len()
            ),
        });
    }
    for ((block_id, sentence_index, _), embedding) in batch.into_iter().zip(response.embeddings) {
        assign_sentence_embedding(result, block_id, sentence_index, embedding);
    }
    Ok(())
}

fn assign_sentence_embedding(
    result: &mut BlockSentenceEmbeddings,
    block_id: Uuid,
    sentence_index: usize,
    embedding: Vec<f32>,
) {
    let Some(block_embeddings) = result.get_mut(&block_id) else {
        return;
    };
    let Some(sentence_embedding) = block_embeddings.get_mut(sentence_index) else {
        return;
    };
    *sentence_embedding = embedding;
}
