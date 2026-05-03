use anyhow::Context;
use futures::stream::{self, StreamExt};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    integrations::llm::EmbeddingBatchRequest,
    shared::extraction::chunking::{BlockSentenceEmbeddings, split_into_sentences},
    shared::extraction::structured_document::StructuredBlockData,
};

const SENTENCE_EMBEDDING_BATCH_SIZE: usize = 16;

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
) -> anyhow::Result<(BlockSentenceEmbeddings, usize)> {
    let embedding_binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
        .await
        .context("failed to resolve EmbedChunk binding for sentence embeddings")?
        .ok_or_else(|| {
            anyhow::anyhow!("active EmbedChunk binding is not configured for library {library_id}")
        })?;

    // Collect only blocks that are large enough to need semantic splitting.
    // A block with ≤ max_tokens_per_chunk tokens can be emitted as one chunk
    // without any sentence-level embeddings.
    struct BlockEntry {
        block_id: Uuid,
        sentences: Vec<String>,
    }

    let block_entries: Vec<BlockEntry> = blocks
        .iter()
        .filter(|b| !b.is_boilerplate)
        .filter_map(|b| {
            let token_count = b.normalized_text.split_whitespace().count();
            if token_count <= max_tokens_per_chunk {
                return None;
            }
            let sentences: Vec<String> = split_into_sentences(b.normalized_text.trim())
                .into_iter()
                .map(str::to_string)
                .collect();
            if sentences.len() < 2 {
                return None;
            }
            Some(BlockEntry { block_id: b.block_id, sentences })
        })
        .collect();

    if block_entries.is_empty() {
        return Ok((BlockSentenceEmbeddings::new(), 0));
    }

    // Flatten all sentences into a single indexed list so we can batch
    // across block boundaries.
    struct FlatEntry {
        block_id: Uuid,
        sentence_idx: usize,
        text: String,
    }

    let flat: Vec<FlatEntry> = block_entries
        .iter()
        .flat_map(|entry| {
            entry.sentences.iter().enumerate().map(|(idx, s)| FlatEntry {
                block_id: entry.block_id,
                sentence_idx: idx,
                text: s.clone(),
            })
        })
        .collect();

    let total_sentences = flat.len();

    // Split into batches.
    type Batch = Vec<(Uuid, usize, String)>; // (block_id, sentence_idx, text)
    let mut batches: Vec<Batch> = Vec::new();
    let mut current: Batch = Vec::with_capacity(SENTENCE_EMBEDDING_BATCH_SIZE);
    for entry in flat {
        current.push((entry.block_id, entry.sentence_idx, entry.text));
        if current.len() == SENTENCE_EMBEDDING_BATCH_SIZE {
            batches.push(std::mem::replace(
                &mut current,
                Vec::with_capacity(SENTENCE_EMBEDDING_BATCH_SIZE),
            ));
        }
    }
    if !current.is_empty() {
        batches.push(current);
    }

    let parallelism = state.settings.ingestion_embedding_parallelism.max(1);

    let provider_kind = embedding_binding.provider_kind.clone();
    let model_name = embedding_binding.model_name.clone();
    let api_key = embedding_binding.api_key.clone();
    let base_url = embedding_binding.provider_base_url.clone();
    let extra_parameters_json = embedding_binding.extra_parameters_json.clone();

    let batch_results = stream::iter(batches.into_iter().map(|batch| {
        let provider_kind = provider_kind.clone();
        let model_name = model_name.clone();
        let api_key = api_key.clone();
        let base_url = base_url.clone();
        let extra_parameters_json = extra_parameters_json.clone();
        async move {
            let inputs: Vec<String> = batch.iter().map(|(_, _, text)| text.clone()).collect();
            let response = state
                .llm_gateway
                .embed_many(EmbeddingBatchRequest {
                    provider_kind,
                    model_name,
                    inputs,
                    api_key_override: api_key,
                    base_url_override: base_url,
                    extra_parameters_json,
                })
                .await
                .context("failed to embed sentence batch")?;
            anyhow::Ok((batch, response))
        }
    }))
    .buffer_unordered(parallelism)
    .collect::<Vec<_>>()
    .await;

    // Assemble per-block sentence embedding maps.
    // First pass: determine sentence count per block so we can pre-allocate.
    let mut block_sentence_counts: std::collections::HashMap<Uuid, usize> =
        std::collections::HashMap::new();
    for entry in &block_entries {
        block_sentence_counts.insert(entry.block_id, entry.sentences.len());
    }

    let mut result: BlockSentenceEmbeddings =
        block_entries.iter().map(|e| (e.block_id, vec![Vec::new(); e.sentences.len()])).collect();

    for batch_result in batch_results {
        let (batch, response) = batch_result?;
        if response.embeddings.len() != batch.len() {
            anyhow::bail!(
                "sentence embedding batch returned {} vectors for {} inputs",
                response.embeddings.len(),
                batch.len()
            );
        }
        for ((block_id, sentence_idx, _), embedding) in
            batch.into_iter().zip(response.embeddings.into_iter())
        {
            if let Some(block_embeddings) = result.get_mut(&block_id) {
                if sentence_idx < block_embeddings.len() {
                    block_embeddings[sentence_idx] = embedding;
                }
            }
        }
    }

    // Remove blocks where any sentence ended up with an empty embedding
    // (shouldn't happen in practice, but guard against partial failures).
    result.retain(|_, embeddings| embeddings.iter().all(|e| !e.is_empty()));

    Ok((result, total_sentences))
}
