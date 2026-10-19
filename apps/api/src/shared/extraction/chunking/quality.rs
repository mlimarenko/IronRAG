use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::{StructuredBlockData, StructuredBlockKind, StructuredChunkWindow};
use crate::shared::extraction::text_quality::text_quality_score;

/// Computes a quality score for a chunk window based on its constituent blocks.
pub(super) fn compute_chunk_quality_score(blocks: &[StructuredBlockData]) -> f32 {
    if blocks.is_empty() {
        return 0.0;
    }

    if blocks.iter().all(|b| b.is_boilerplate) {
        return 0.0;
    }

    let mut score: f32 = 1.0;

    // Bonus for code or endpoint blocks
    if blocks.iter().any(|b| {
        matches!(b.block_kind, StructuredBlockKind::CodeBlock | StructuredBlockKind::EndpointBlock)
    }) {
        score += 0.1;
    }

    // Bonus for headings
    if blocks.iter().any(|b| matches!(b.block_kind, StructuredBlockKind::Heading)) {
        score += 0.1;
    }

    // Bonus for table content
    if blocks
        .iter()
        .any(|b| matches!(b.block_kind, StructuredBlockKind::Table | StructuredBlockKind::TableRow))
    {
        score += 0.1;
    }

    // Penalty for very short text
    let total_chars: usize = blocks.iter().map(|b| b.normalized_text.len()).sum();
    if total_chars < 100 {
        score -= 0.2;
    }

    // Penalty for low unique word ratio
    let words: Vec<&str> =
        blocks.iter().flat_map(|b| b.normalized_text.split_whitespace()).collect();
    if !words.is_empty() {
        let unique: HashSet<&str> = words.iter().copied().collect();
        let ratio = unique.len() as f32 / words.len() as f32;
        if ratio < 0.3 {
            score -= 0.1;
        }
    }

    let combined_text =
        blocks.iter().map(|b| b.normalized_text.as_str()).collect::<Vec<_>>().join("\n");
    score *= text_quality_score(&combined_text);

    score.clamp(0.0, 1.0)
}

/// Computes a 64-bit `SimHash` fingerprint from text using 3-gram word shingles.
pub(super) fn compute_simhash(text: &str) -> u64 {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < 3 {
        // For very short text, hash the whole thing
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        return hasher.finish();
    }

    let mut bit_counts = [0_i64; 64];

    for window in words.windows(3) {
        let mut hasher = DefaultHasher::new();
        window.hash(&mut hasher);
        let hash = hasher.finish();

        for (bit, count) in bit_counts.iter_mut().enumerate() {
            if (hash >> bit) & 1 == 1 {
                *count += 1;
            } else {
                *count -= 1;
            }
        }
    }

    let mut fingerprint: u64 = 0;
    for (bit, count) in bit_counts.iter().enumerate() {
        if *count > 0 {
            fingerprint |= 1 << bit;
        }
    }
    fingerprint
}

/// Marks near-duplicate chunks: if two chunks share the same simhash fingerprint
/// but have different literal digests, the later one is marked as a near-duplicate.
pub(super) fn mark_near_duplicates(chunks: &mut [StructuredChunkWindow]) {
    let mut seen_digests: std::collections::HashMap<u64, String> = std::collections::HashMap::new();

    for chunk in chunks.iter_mut() {
        let Some(fingerprint) = chunk.simhash_fingerprint else {
            continue;
        };

        if let Some(prev_digest) = seen_digests.get(&fingerprint) {
            let current_digest = chunk.literal_digest.as_deref().unwrap_or("");
            if current_digest != prev_digest {
                chunk.is_near_duplicate = true;
            }
        } else if let Some(digest) = &chunk.literal_digest {
            seen_digests.insert(fingerprint, digest.clone());
        }
    }
}
