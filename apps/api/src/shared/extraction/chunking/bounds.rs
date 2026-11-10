use std::{collections::HashMap, ops::Range};

use thiserror::Error;
use uuid::Uuid;

use super::*;
use crate::shared::extraction::structured_document::{ChunkSourceProvenance, StructuredSourceSpan};

#[derive(Debug, Error)]
pub enum StructuredChunkingError {
    #[error("chunk character and byte bounds must be non-zero")]
    InvalidBounds,
    #[error("a Unicode scalar cannot fit within the chunk byte bound")]
    UnrepresentableScalar,
    #[error("chunk {chunk_index} does not match its supporting blocks")]
    InvalidWindow { chunk_index: i32 },
    #[error("chunk {chunk_index} requires a semantic split before input bounding")]
    AmbiguousSplit { chunk_index: i32 },
    #[error("structured block {block_id} has no unambiguous source anchor")]
    UnmappedSource { block_id: Uuid },
    #[error("chunk source offsets exceed the supported range")]
    OffsetOverflow,
}

struct WindowPart<'a> {
    block: &'a StructuredBlockData,
    range: Range<usize>,
}

pub(super) fn constrain_chunk_windows(
    windows: Vec<StructuredChunkWindow>,
    blocks: &[StructuredBlockData],
    profile: StructuredChunkingProfile,
    source: Option<&str>,
) -> Result<Vec<StructuredChunkWindow>, StructuredChunkingError> {
    if profile.max_chars == 0 || profile.max_input_bytes == Some(0) {
        return Err(StructuredChunkingError::InvalidBounds);
    }
    let by_id: HashMap<_, _> = blocks.iter().map(|block| (block.block_id, block)).collect();
    let mut result = Vec::with_capacity(windows.len());
    for window in windows {
        let parts = window_parts(&window, &by_id)?;
        let literal = window.content_text == window.normalized_text
            && parts.iter().all(|part| part.block.text.trim() == part.block.normalized_text.trim());
        let semantic_unit = matches!(
            window.chunk_kind,
            StructuredBlockKind::TableRow
                | StructuredBlockKind::SourceProfile
                | StructuredBlockKind::SourceUnit
        );
        let too_many_bytes =
            profile.max_input_bytes.is_some_and(|limit| window.normalized_text.len() > limit);
        // max_chars is the existing grouping target, not the model's hard limit.
        // Repacking at that target would unnecessarily increase provider fanout.
        let split = too_many_bytes;
        if split && (!literal || semantic_unit) {
            return Err(StructuredChunkingError::AmbiguousSplit {
                chunk_index: window.chunk_index,
            });
        }
        let ranges = if split {
            text_windows(&window.normalized_text, profile)?
        } else {
            std::iter::once(0..window.normalized_text.len()).collect()
        };
        for range in ranges {
            let mut fragment = window.clone();
            if split {
                let text = &window.normalized_text[range.clone()];
                if text.trim().is_empty() {
                    return Err(StructuredChunkingError::InvalidWindow {
                        chunk_index: window.chunk_index,
                    });
                }
                fragment.content_text = text.to_owned();
                fragment.normalized_text = text.to_owned();
                fragment.support_block_ids = parts
                    .iter()
                    .filter(|part| intersects(&part.range, &range))
                    .map(|part| part.block.block_id)
                    .collect();
                let mut quality_blocks = Vec::new();
                for part in parts.iter().filter(|part| intersects(&part.range, &range)) {
                    let mut block = part.block.clone();
                    let start = range.start.max(part.range.start) - part.range.start;
                    let end = range.end.min(part.range.end) - part.range.start;
                    block.text = part.block.normalized_text.trim()[start..end].to_owned();
                    block.normalized_text = block.text.clone();
                    quality_blocks.push(block);
                }
                fragment.chunk_kind = dominant_chunk_kind(&quality_blocks);
                fragment.quality_score = compute_chunk_quality_score(&quality_blocks);
                fragment.token_count = i32::try_from(text.split_whitespace().count()).ok();
                fragment.literal_digest =
                    Some(format!("sha256:{}", hex::encode(Sha256::digest(text.as_bytes()))));
                fragment.simhash_fingerprint = Some(compute_simhash(text));
                fragment.is_near_duplicate = false;
                fragment.window_text = None;
            }
            fragment.chunk_index =
                i32::try_from(result.len()).map_err(|_| StructuredChunkingError::OffsetOverflow)?;
            fragment.runtime_provenance = source
                .map(|source| source_provenance(&parts, &range, &by_id, source, split))
                .transpose()?;
            result.push(fragment);
        }
    }
    Ok(result)
}

fn window_parts<'a>(
    window: &StructuredChunkWindow,
    by_id: &HashMap<Uuid, &'a StructuredBlockData>,
) -> Result<Vec<WindowPart<'a>>, StructuredChunkingError> {
    let mut parts = Vec::new();
    let mut normalized = String::new();
    let mut content = String::new();
    for id in &window.support_block_ids {
        let parent_anchor = window.support_block_ids.iter().any(|other| {
            by_id.get(other).is_some_and(|child| {
                child.parent_block_id == Some(*id)
                    && (child.block_kind == StructuredBlockKind::TableRow
                        || is_table_summary_text(&child.normalized_text))
            })
        });
        if parent_anchor {
            continue;
        }
        let block = by_id
            .get(id)
            .copied()
            .ok_or(StructuredChunkingError::InvalidWindow { chunk_index: window.chunk_index })?;
        if !parts.is_empty() {
            normalized.push_str("\n\n");
            content.push_str("\n\n");
        }
        let start = normalized.len();
        normalized.push_str(block.normalized_text.trim());
        content.push_str(block.text.trim());
        parts.push(WindowPart { block, range: start..normalized.len() });
    }
    if normalized != window.normalized_text || content != window.content_text {
        return Err(StructuredChunkingError::InvalidWindow { chunk_index: window.chunk_index });
    }
    Ok(parts)
}

fn text_windows(
    text: &str,
    profile: StructuredChunkingProfile,
) -> Result<Vec<Range<usize>>, StructuredChunkingError> {
    let byte_limit = profile.max_input_bytes.ok_or(StructuredChunkingError::InvalidBounds)?;
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = start;
        let mut count = 0usize;
        for (offset, ch) in text[start..].char_indices() {
            if offset + ch.len_utf8() > byte_limit {
                break;
            }
            end = start + offset + ch.len_utf8();
            count += 1;
        }
        if end == start {
            return Err(StructuredChunkingError::UnrepresentableScalar);
        }
        ranges.push(start..end);
        if end == text.len() {
            break;
        }
        let overlap = profile.overlap_chars.min(count.saturating_sub(1));
        start = if overlap == 0 {
            end
        } else {
            start
                + text[start..end]
                    .char_indices()
                    .rev()
                    .nth(overlap - 1)
                    .map_or(end - start, |(offset, _)| offset)
        };
    }
    Ok(ranges)
}

fn source_provenance(
    parts: &[WindowPart<'_>],
    range: &Range<usize>,
    by_id: &HashMap<Uuid, &StructuredBlockData>,
    source: &str,
    split: bool,
) -> Result<ChunkSourceProvenance, StructuredChunkingError> {
    let mut spans = Vec::new();
    let mut exact = true;
    for part in parts.iter().filter(|part| intersects(&part.range, range)) {
        let anchor = source_anchor(part.block, by_id, source)?;
        let anchor_text = slice_chars(source, &anchor)
            .ok_or(StructuredChunkingError::UnmappedSource { block_id: part.block.block_id })?;
        let normalized = part.block.normalized_text.trim();
        if anchor_text.trim() == normalized && part.block.text.trim() == normalized {
            let prefix = anchor_text.len() - anchor_text.trim_start().len();
            let start = range.start.max(part.range.start) - part.range.start;
            let end = range.end.min(part.range.end) - part.range.start;
            let base = anchor.start_offset
                + i32::try_from(anchor_text[..prefix].chars().count())
                    .map_err(|_| StructuredChunkingError::OffsetOverflow)?;
            spans.push(StructuredSourceSpan {
                start_offset: base
                    + i32::try_from(normalized[..start].chars().count())
                        .map_err(|_| StructuredChunkingError::OffsetOverflow)?,
                end_offset: base
                    + i32::try_from(normalized[..end].chars().count())
                        .map_err(|_| StructuredChunkingError::OffsetOverflow)?,
            });
        } else {
            if split {
                return Err(StructuredChunkingError::AmbiguousSplit { chunk_index: 0 });
            }
            exact = false;
            spans.push(anchor);
        }
    }
    let start = spans
        .iter()
        .map(|span| span.start_offset)
        .min()
        .ok_or(StructuredChunkingError::InvalidBounds)?;
    let end = spans
        .iter()
        .map(|span| span.end_offset)
        .max()
        .ok_or(StructuredChunkingError::InvalidBounds)?;
    let span = StructuredSourceSpan { start_offset: start, end_offset: end };
    Ok(if exact && spans.len() == 1 {
        ChunkSourceProvenance::Exact(span)
    } else {
        ChunkSourceProvenance::Covering(span)
    })
}

fn source_anchor(
    block: &StructuredBlockData,
    by_id: &HashMap<Uuid, &StructuredBlockData>,
    source: &str,
) -> Result<StructuredSourceSpan, StructuredChunkingError> {
    let mut current = block;
    for _ in 0..=by_id.len() {
        let text = current.text.trim();
        if let Some(span) = &current.source_span
            && let Some(slice) = slice_chars(source, span)
            && span.end_offset > span.start_offset
            && (slice.trim() == text
                || slice.trim() == current.normalized_text.trim()
                || (current.block_kind == StructuredBlockKind::SourceUnit
                    && crate::shared::extraction::record_jsonl::is_record_unit_excerpt(
                        slice,
                        &current.normalized_text,
                    )))
        {
            return Ok(span.clone());
        }
        if !text.is_empty() {
            let mut matches = source.match_indices(text);
            if let Some((start, _)) = matches.next()
                && matches.next().is_none()
            {
                return Ok(StructuredSourceSpan {
                    start_offset: i32::try_from(source[..start].chars().count())
                        .map_err(|_| StructuredChunkingError::OffsetOverflow)?,
                    end_offset: i32::try_from(source[..start + text.len()].chars().count())
                        .map_err(|_| StructuredChunkingError::OffsetOverflow)?,
                });
            }
        }
        if current.block_kind != StructuredBlockKind::TableRow
            && !is_table_summary_text(&current.normalized_text)
        {
            return Err(StructuredChunkingError::UnmappedSource { block_id: block.block_id });
        }
        current = current
            .parent_block_id
            .and_then(|id| by_id.get(&id).copied())
            .filter(|parent| parent.block_kind == StructuredBlockKind::Table)
            .ok_or(StructuredChunkingError::UnmappedSource { block_id: block.block_id })?;
    }
    Err(StructuredChunkingError::UnmappedSource { block_id: block.block_id })
}

fn slice_chars<'a>(source: &'a str, span: &StructuredSourceSpan) -> Option<&'a str> {
    let start = usize::try_from(span.start_offset).ok()?;
    let end = usize::try_from(span.end_offset).ok()?;
    if end < start {
        return None;
    }
    let start = source.char_indices().nth(start).map_or_else(
        || (start == source.chars().count()).then_some(source.len()),
        |(offset, _)| Some(offset),
    )?;
    let length = usize::try_from(span.end_offset - span.start_offset).ok()?;
    let end = source[start..].char_indices().nth(length).map_or_else(
        || (length == source[start..].chars().count()).then_some(source.len()),
        |(offset, _)| Some(start + offset),
    )?;
    source.get(start..end)
}

fn intersects(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(text: &str, start: i32) -> StructuredBlockData {
        StructuredBlockData {
            block_id: Uuid::now_v7(),
            ordinal: 0,
            block_kind: StructuredBlockKind::Paragraph,
            text: text.into(),
            normalized_text: text.into(),
            heading_trail: vec![],
            section_path: vec![],
            page_number: None,
            source_span: Some(StructuredSourceSpan {
                start_offset: start,
                end_offset: start + i32::try_from(text.chars().count()).expect("fixture size"),
            }),
            parent_block_id: None,
            table_coordinates: None,
            code_language: None,
            is_boilerplate: false,
        }
    }

    #[test]
    fn unicode_byte_bound_and_overlap_preserve_real_source_spans() {
        let text = "🦀".repeat(12);
        let source = format!("αβ\n{text}");
        let input = block(&text, 3);
        let chunks = build_structured_chunk_windows(
            std::slice::from_ref(&input),
            StructuredChunkingProfile { max_chars: 8, overlap_chars: 2, max_input_bytes: Some(12) },
            Some(&source),
        )
        .expect("valid source mapping");
        let mut covered = std::collections::BTreeSet::new();
        let mut previous_end = None;
        for chunk in chunks {
            assert!(chunk.normalized_text.len() <= 12);
            let provenance = chunk.runtime_provenance.expect("runtime source evidence");
            assert!(matches!(provenance, ChunkSourceProvenance::Exact(_)));
            let span = provenance.span();
            assert_eq!(slice_chars(&source, span), Some(chunk.normalized_text.as_str()));
            assert!(chunk.support_block_ids.contains(&input.block_id));
            if let Some(end) = previous_end {
                assert!(end - span.start_offset <= 2);
            }
            previous_end = Some(span.end_offset);
            covered.extend(span.start_offset..span.end_offset);
        }
        assert_eq!(covered, (3..15).collect());
    }

    #[test]
    fn generated_separators_keep_covering_not_guessed_source_bounds() {
        let tail = "x".repeat(20);
        let source = format!("one\n{tail}");
        let mut second = block(&tail, 4);
        second.ordinal = 1;
        let chunks = build_structured_chunk_windows(
            &[block("one", 0), second],
            StructuredChunkingProfile { max_chars: 8, overlap_chars: 4, max_input_bytes: Some(8) },
            Some(&source),
        )
        .expect("each supporting block has a real source span");
        let mixed = chunks
            .iter()
            .find(|chunk| chunk.content_text.starts_with("one\n\n"))
            .expect("overlapping window retains both blocks");
        assert!(matches!(mixed.runtime_provenance, Some(ChunkSourceProvenance::Covering(_))));
        let span = mixed.runtime_provenance.as_ref().expect("covering span").span();
        assert_eq!(span, &StructuredSourceSpan { start_offset: 0, end_offset: 7 });
        assert_ne!(slice_chars(&source, span), Some(mixed.content_text.as_str()));
        assert!(chunks.iter().all(|chunk| chunk.normalized_text.len() <= 8));
    }

    #[test]
    fn transformed_table_is_anchored_but_never_split_by_an_unrelated_offset() {
        let source = "r\n|a|";
        let mut row = block("|a|", 2);
        row.block_kind = StructuredBlockKind::TableRow;
        row.normalized_text = "column: a".into();
        let profile =
            StructuredChunkingProfile { max_chars: 8, overlap_chars: 0, max_input_bytes: Some(12) };
        let chunks =
            build_structured_chunk_windows(std::slice::from_ref(&row), profile, Some(source))
                .expect("bounded transformed row retains its source anchor");
        assert!(matches!(chunks[0].runtime_provenance, Some(ChunkSourceProvenance::Covering(_))));
        assert_eq!(
            chunks[0].runtime_provenance.as_ref().expect("span").span(),
            &StructuredSourceSpan { start_offset: 2, end_offset: 5 }
        );
        let value = serde_json::to_value(&chunks[0]).expect("public representation");
        assert!(value.get("runtimeProvenance").is_none());
        let restored: StructuredChunkWindow =
            serde_json::from_value(value).expect("old stored representation");
        assert!(restored.runtime_provenance.is_none());
        assert_eq!(restored.normalized_text, row.normalized_text);
        row.normalized_text = "column: ".repeat(4);
        assert!(matches!(
            build_structured_chunk_windows(&[row], profile, Some(source)),
            Err(StructuredChunkingError::AmbiguousSplit { .. })
        ));
    }

    #[test]
    fn repeated_text_without_a_source_anchor_is_not_assigned_a_guessed_offset() {
        let mut input = block("repeated", 0);
        input.source_span = None;
        let result = build_structured_chunk_windows(
            &[input],
            StructuredChunkingProfile {
                max_chars: 20,
                overlap_chars: 0,
                max_input_bytes: Some(20),
            },
            Some("repeated\nrepeated"),
        );
        assert!(matches!(result, Err(StructuredChunkingError::UnmappedSource { .. })));
    }

    #[test]
    fn model_byte_bound_does_not_turn_a_soft_grouping_target_into_extra_sends() {
        let text = "x".repeat(18_480);
        let chunks = build_structured_chunk_windows(
            &[block(&text, 0)],
            StructuredChunkingProfile {
                max_chars: 2800,
                overlap_chars: 280,
                max_input_bytes: Some(8192),
            },
            Some(&text),
        )
        .expect("lossless bounded input plan");
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|chunk| chunk.normalized_text.len() <= 8192));
        let mut reconstructed = chunks[0].normalized_text.clone();
        for chunk in &chunks[1..] {
            reconstructed.push_str(&chunk.normalized_text[280..]);
        }
        assert_eq!(reconstructed, text);
    }

    #[test]
    fn record_segments_require_verbatim_fields_for_the_covering_source_anchor() {
        let source = "[source_unit id=one] fields: a=first; b=second; c=third";
        let mut input = block(source, 0);
        input.block_kind = StructuredBlockKind::SourceUnit;
        input.text = "[source_unit id=one] fields: b=second".into();
        input.normalized_text = input.text.clone();
        let profile = StructuredChunkingProfile {
            max_chars: 100,
            overlap_chars: 0,
            max_input_bytes: Some(100),
        };
        let chunks =
            build_structured_chunk_windows(std::slice::from_ref(&input), profile, Some(source))
                .expect("the native record grammar proves this generated segment");
        assert!(matches!(chunks[0].runtime_provenance, Some(ChunkSourceProvenance::Covering(_))));
        input.text = "[source_unit id=one] fields: b=changed".into();
        input.normalized_text = input.text.clone();
        assert!(matches!(
            build_structured_chunk_windows(&[input], profile, Some(source)),
            Err(StructuredChunkingError::UnmappedSource { .. })
        ));
    }
}
