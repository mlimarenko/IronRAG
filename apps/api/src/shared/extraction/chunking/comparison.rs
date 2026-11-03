//! Benchmark treatments over already prepared canonical blocks, never production defaults.

use std::collections::HashSet;

use uuid::Uuid;

use super::{
    StructuredBlockData, StructuredBlockKind, StructuredChunkWindow, StructuredChunkingProfile,
    compute_overlap_start, compute_window_text_pass, count_window_chars, filter_chunkable_blocks,
    is_standalone_chunk_block, mark_near_duplicates, push_structured_chunk_window,
};
use crate::shared::extraction::structured_document::{
    StructuredSourceSpan, validate_ordered_semantic_blocks,
};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Treatment {
    Recursive,
    Page,
    Semantic,
}

#[derive(Debug)]
pub(crate) struct ComparisonChunks {
    /// Originals (including table parents), followed by nonempty derived prose fragments.
    /// Original IDs and ordinals never change. Chunk support IDs resolve in this collection.
    pub(crate) blocks: Vec<StructuredBlockData>,
    pub(crate) chunks: Vec<StructuredChunkWindow>,
    /// Whitespace-only split ranges are provenance, never semantic blocks or rendered evidence.
    pub(crate) whitespace_spans: Vec<WhitespaceSpan>,
    /// Interior page/semantic boundaries, excluding atomic-unit and size boundaries.
    pub(crate) boundary_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WhitespaceSpan {
    pub(crate) canonical_block_id: Uuid,
    /// Absolute character offsets in the same normalized source as the canonical block.
    pub(crate) source_span: StructuredSourceSpan,
}

/// `semantic_breaks` contains strictly increasing interior *canonical block positions*:
/// a break at `k` prohibits grouping, overlap and context between blocks `k-1` and `k`.
/// Both document endpoints are implicit. Page admission additionally requires the caller
/// to establish PDF source identity; this block-only API enforces complete page metadata.
///
/// Split prose must have identical source/normalized text and an exact character span.
/// Without those facts this API cannot invent a lossless normalized-source mapping.
pub(crate) fn prepare_treatment(
    blocks: &[StructuredBlockData],
    profile: StructuredChunkingProfile,
    treatment: Treatment,
    semantic_breaks: &[usize],
) -> Result<ComparisonChunks, String> {
    if profile.max_chars == 0 || profile.overlap_chars >= profile.max_chars {
        return Err("invalid character profile".into());
    }
    if blocks.is_empty() {
        return Err("treatment requires canonical blocks".into());
    }
    validate_ordered_semantic_blocks(blocks).map_err(|error| error.to_string())?;
    let mut ids = HashSet::new();
    for block in blocks {
        if !ids.insert(block.block_id) {
            return Err("duplicate canonical block ID makes support identity ambiguous".into());
        }
    }
    let boundaries = hard_boundaries(blocks, treatment, semantic_breaks)?;
    // Table parents can sit on the other side of a hard boundary from their rows.
    // Apply the production filter globally before partitioning, not once per page.
    let filtered = filter_chunkable_blocks(blocks);
    let mut result = ComparisonChunks {
        blocks: blocks.to_vec(),
        chunks: Vec::new(),
        whitespace_spans: Vec::new(),
        boundary_count: boundaries.len(),
    };
    let mut start = 0;
    for end in boundaries.into_iter().chain(std::iter::once(blocks.len())) {
        let mut selected = Vec::new();
        for block in filtered.iter().filter(|block| {
            usize::try_from(block.ordinal).is_ok_and(|position| position >= start && position < end)
        }) {
            if split_capable(block) && block.normalized_text.chars().count() > profile.max_chars {
                selected.extend(split_prose(
                    block,
                    profile.max_chars,
                    &mut result.blocks,
                    &mut result.whitespace_spans,
                    &mut ids,
                )?);
            } else {
                selected.push(block.clone());
            }
        }
        group_segment(&selected, blocks.len(), profile, &mut result.chunks);
        start = end;
    }
    mark_near_duplicates(&mut result.chunks);
    Ok(result)
}

fn hard_boundaries(
    blocks: &[StructuredBlockData],
    treatment: Treatment,
    semantic_breaks: &[usize],
) -> Result<Vec<usize>, String> {
    if treatment != Treatment::Semantic && !semantic_breaks.is_empty() {
        return Err("semantic boundaries supplied to a non-semantic treatment".into());
    }
    match treatment {
        Treatment::Recursive => Ok(Vec::new()),
        Treatment::Semantic => {
            if semantic_breaks.is_empty()
                || semantic_breaks.iter().any(|&index| index == 0 || index >= blocks.len())
                || semantic_breaks.windows(2).any(|pair| pair[0] >= pair[1])
            {
                return Err(
                    "semantic treatment requires ordered, nonempty interior boundaries".into()
                );
            }
            Ok(semantic_breaks.to_vec())
        }
        Treatment::Page => {
            if blocks.iter().any(|block| block.page_number.is_none_or(|page| page <= 0)) {
                return Err("page treatment requires positive page metadata on every block".into());
            }
            if blocks.windows(2).any(|pair| pair[0].page_number > pair[1].page_number) {
                return Err("page metadata must be nondecreasing".into());
            }
            Ok((1..blocks.len())
                .filter(|&index| blocks[index - 1].page_number != blocks[index].page_number)
                .collect())
        }
    }
}

fn split_capable(block: &StructuredBlockData) -> bool {
    // List items, parented records, tables and code remain indivisible canonical units.
    matches!(block.block_kind, StructuredBlockKind::Paragraph | StructuredBlockKind::QuoteBlock)
        && block.parent_block_id.is_none()
}

fn split_prose(
    block: &StructuredBlockData,
    max_chars: usize,
    support: &mut Vec<StructuredBlockData>,
    whitespace_spans: &mut Vec<WhitespaceSpan>,
    ids: &mut HashSet<Uuid>,
) -> Result<Vec<StructuredBlockData>, String> {
    let chars = block.normalized_text.chars().collect::<Vec<_>>();
    let span = block.source_span.as_ref().ok_or("split prose requires a source span")?;
    if block.text != block.normalized_text
        || span.start_offset < 0
        || i64::from(span.end_offset) - i64::from(span.start_offset)
            != i64::try_from(chars.len()).map_err(|_| "prose length exceeds span capacity")?
    {
        return Err("split prose requires an exact normalized character source mapping".into());
    }
    let mut ranges = Vec::new();
    recursive_ranges(&chars, 0, chars.len(), 0, max_chars, &mut ranges);
    let mut fragments = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        let source_span = StructuredSourceSpan {
            start_offset: span.start_offset
                + i32::try_from(start).map_err(|_| "fragment offset exceeds span capacity")?,
            end_offset: span.start_offset
                + i32::try_from(end).map_err(|_| "fragment offset exceeds span capacity")?,
        };
        let text: String = chars[start..end].iter().collect();
        if text.trim().is_empty() {
            // Retain exact coverage without manufacturing a blank semantic block that
            // the production replay validator must reject. Originals retain the text.
            whitespace_spans
                .push(WhitespaceSpan { canonical_block_id: block.block_id, source_span });
            continue;
        }
        let mut digest = Sha256::new();
        digest.update(b"irg256-prose-fragment/1");
        digest.update(block.block_id.as_bytes());
        digest.update((start as u64).to_be_bytes());
        digest.update((end as u64).to_be_bytes());
        let digest = digest.finalize();
        let mut bytes = [0_u8; 16];
        bytes.copy_from_slice(&digest[..16]);
        let id = Uuid::from_bytes(bytes);
        if !ids.insert(id) {
            return Err("derived support identity collision".into());
        }
        let mut fragment = block.clone();
        fragment.block_id = id;
        fragment.ordinal = i32::try_from(support.len()).map_err(|_| "too many support blocks")?;
        fragment.text = text;
        fragment.normalized_text.clone_from(&fragment.text);
        fragment.parent_block_id = Some(block.block_id);
        fragment.source_span = Some(source_span);
        support.push(fragment.clone());
        fragments.push(fragment);
    }
    Ok(fragments)
}

fn is_recursive_separator(
    chars: &[char],
    start: usize,
    end: usize,
    index: usize,
    level: usize,
) -> bool {
    match level {
        0 => chars[index] == '\n' && index > start && chars[index - 1] == '\n',
        1 => chars[index] == '\n',
        2 => {
            matches!(chars[index], '.' | '!' | '?' | '。' | '！' | '？')
                && (index + 1 == end || chars[index + 1].is_whitespace())
        }
        _ => chars[index].is_whitespace(),
    }
}

/// Five bounded levels: paragraph, newline, sentence, word, then character fallback.
/// Delimiters belong to the preceding piece, including all trailing whitespace.
fn recursive_ranges(
    chars: &[char],
    start: usize,
    end: usize,
    level: usize,
    max_chars: usize,
    out: &mut Vec<(usize, usize)>,
) {
    if end - start <= max_chars {
        out.push((start, end));
        return;
    }
    if level == 4 {
        let mut cursor = start;
        while cursor < end {
            let next = cursor.saturating_add(max_chars).min(end);
            out.push((cursor, next));
            cursor = next;
        }
        return;
    }
    let mut cuts = Vec::new();
    let mut index = start;
    while index < end {
        if is_recursive_separator(chars, start, end, index, level) {
            let mut next = index + 1;
            // Sentence punctuation and word separators retain their whitespace suffix.
            if level >= 2 {
                while next < end && chars[next].is_whitespace() {
                    next += 1;
                }
            }
            if next < end && cuts.last().is_none_or(|&previous| previous < next) {
                cuts.push(next);
            }
            index = next;
        } else {
            index += 1;
        }
    }
    if cuts.is_empty() {
        recursive_ranges(chars, start, end, level + 1, max_chars, out);
        return;
    }
    cuts.push(end);
    let mut piece_start = start;
    let mut pending_start = start;
    let mut pending_end = start;
    for piece_end in cuts {
        if piece_end - piece_start > max_chars {
            if pending_start < pending_end {
                out.push((pending_start, pending_end));
            }
            recursive_ranges(chars, piece_start, piece_end, level + 1, max_chars, out);
            pending_start = piece_end;
            pending_end = piece_end;
        } else {
            if piece_end - pending_start > max_chars {
                out.push((pending_start, pending_end));
                pending_start = piece_start;
            }
            pending_end = piece_end;
        }
        piece_start = piece_end;
    }
    if pending_start < pending_end {
        out.push((pending_start, pending_end));
    }
}

fn group_segment(
    blocks: &[StructuredBlockData],
    canonical_block_count: usize,
    profile: StructuredChunkingProfile,
    out: &mut Vec<StructuredChunkWindow>,
) {
    let mut run_start = 0;
    for (index, block) in blocks.iter().enumerate() {
        let is_fragment =
            usize::try_from(block.ordinal).is_ok_and(|ordinal| ordinal >= canonical_block_count);
        if is_standalone_chunk_block(block) || (!split_capable(block) && !is_fragment) {
            group_prose(&blocks[run_start..index], profile, out);
            // Atomic units are barriers for both overlap and context, not just grouping.
            push_structured_chunk_window(out, std::slice::from_ref(block));
            run_start = index + 1;
        }
    }
    group_prose(&blocks[run_start..], profile, out);
}

fn group_prose(
    blocks: &[StructuredBlockData],
    profile: StructuredChunkingProfile,
    out: &mut Vec<StructuredChunkWindow>,
) {
    let first_chunk = out.len();
    let mut start = 0;
    for index in 0..blocks.len() {
        if start < index && count_window_chars(&blocks[start..=index]) > profile.max_chars {
            push_structured_chunk_window(out, &blocks[start..index]);
            start = compute_overlap_start(blocks, start, index, profile.overlap_chars);
            // Overlap is optional context, never permission to exceed the common bound.
            while start < index && count_window_chars(&blocks[start..=index]) > profile.max_chars {
                start += 1;
            }
        }
    }
    if start < blocks.len() {
        push_structured_chunk_window(out, &blocks[start..]);
    }
    compute_window_text_pass(&mut out[first_chunk..]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::extraction::structured_document::{
        StructuredTableCoordinates, validate_structured_chunk_windows,
    };

    fn prose(ordinal: i32, text: &str, page: Option<i32>) -> StructuredBlockData {
        StructuredBlockData {
            block_id: Uuid::from_u128(u128::try_from(ordinal).unwrap() + 1),
            ordinal,
            block_kind: StructuredBlockKind::Paragraph,
            text: text.into(),
            normalized_text: text.into(),
            heading_trail: vec!["Scope".into()],
            section_path: vec!["scope".into()],
            page_number: page,
            source_span: Some(StructuredSourceSpan {
                start_offset: ordinal * 1_000,
                end_offset: ordinal * 1_000 + i32::try_from(text.chars().count()).unwrap(),
            }),
            parent_block_id: None,
            table_coordinates: None,
            code_language: None,
            is_boilerplate: false,
        }
    }

    fn profile(max_chars: usize, overlap_chars: usize) -> StructuredChunkingProfile {
        StructuredChunkingProfile { max_chars, overlap_chars }
    }

    fn assert_support_resolves(result: &ComparisonChunks) {
        validate_ordered_semantic_blocks(&result.blocks).unwrap();
        validate_structured_chunk_windows(&result.blocks, &result.chunks).unwrap();
        let ids = result.blocks.iter().map(|block| block.block_id).collect::<HashSet<_>>();
        assert_eq!(ids.len(), result.blocks.len());
        for (index, chunk) in result.chunks.iter().enumerate() {
            assert_eq!(usize::try_from(chunk.chunk_index).unwrap(), index);
            assert!(chunk.support_block_ids.iter().all(|id| ids.contains(id)));
        }
        for block in &result.blocks {
            assert!(block.parent_block_id.is_none_or(|id| ids.contains(&id)));
        }
    }

    fn assert_lossless_split(block: &StructuredBlockData, result: &ComparisonChunks) {
        assert_eq!(result.blocks[0], *block);
        let source = block.normalized_text.chars().collect::<Vec<_>>();
        let original_span = block.source_span.as_ref().unwrap();
        let mut intervals = Vec::new();
        for fragment in
            result.blocks.iter().filter(|part| part.parent_block_id == Some(block.block_id))
        {
            let span = fragment.source_span.as_ref().unwrap();
            let start = usize::try_from(span.start_offset - original_span.start_offset).unwrap();
            let end = usize::try_from(span.end_offset - original_span.start_offset).unwrap();
            assert_eq!(fragment.text, source[start..end].iter().collect::<String>());
            assert_eq!(fragment.text, fragment.normalized_text);
            assert!(!fragment.normalized_text.trim().is_empty());
            intervals.push(span);
        }
        for whitespace in &result.whitespace_spans {
            assert_eq!(whitespace.canonical_block_id, block.block_id);
            let span = &whitespace.source_span;
            let start = usize::try_from(span.start_offset - original_span.start_offset).unwrap();
            let end = usize::try_from(span.end_offset - original_span.start_offset).unwrap();
            assert!(start < end);
            assert!(source[start..end].iter().all(|ch| ch.is_whitespace()));
            intervals.push(span);
        }
        intervals.sort_unstable_by_key(|span| span.start_offset);
        let mut next = original_span.start_offset;
        let mut recombined = String::new();
        for span in intervals {
            // Equality proves both no gaps and no overlaps in the full interval union.
            assert_eq!(span.start_offset, next);
            let start = usize::try_from(span.start_offset - original_span.start_offset).unwrap();
            let end = usize::try_from(span.end_offset - original_span.start_offset).unwrap();
            recombined.extend(source[start..end].iter().copied());
            next = span.end_offset;
        }
        assert_eq!(next, original_span.end_offset);
        assert_eq!(recombined, block.normalized_text);
        assert_support_resolves(result);
    }

    #[test]
    fn token_boundary_whitespace_is_provenance_not_a_blank_semantic_block() {
        let block = prose(0, "abcd e", None);
        let result = prepare_treatment(
            std::slice::from_ref(&block),
            profile(4, 0),
            Treatment::Recursive,
            &[],
        )
        .unwrap();
        assert_eq!(result.blocks.len(), 3);
        assert_eq!(result.blocks[1].normalized_text, "abcd");
        assert_eq!(result.blocks[2].normalized_text, "e");
        assert_eq!(
            result.whitespace_spans,
            [WhitespaceSpan {
                canonical_block_id: block.block_id,
                source_span: StructuredSourceSpan { start_offset: 4, end_offset: 5 },
            }]
        );
        assert_eq!(
            result.chunks.iter().map(|chunk| chunk.normalized_text.as_str()).collect::<Vec<_>>(),
            ["abcd", "e"]
        );
        assert_lossless_split(&block, &result);
    }

    #[test]
    fn multiple_unicode_and_trailing_whitespace_has_exact_replayable_coverage() {
        for text in [
            "abcd    e",
            "αβγδ\u{2003}\u{a0}界",
            "abcd     ",
            "abcd \t\n\u{2003}\u{a0}",
            "if enabled: 条件=真   ",
        ] {
            let mut block = prose(0, text, Some(1));
            let span = block.source_span.as_mut().unwrap();
            span.start_offset += 71;
            span.end_offset += 71;
            let result = prepare_treatment(
                std::slice::from_ref(&block),
                profile(4, 1),
                Treatment::Recursive,
                &[],
            )
            .unwrap();
            assert!(!result.whitespace_spans.is_empty(), "{text:?}");
            assert!(result.chunks.iter().all(|chunk| chunk.normalized_text.chars().count() <= 4));
            assert_lossless_split(&block, &result);
        }
    }

    #[test]
    fn whitespace_only_canonical_input_is_refused_without_mutating_it() {
        let block = prose(0, " \t\u{2003}\n", None);
        assert!(validate_ordered_semantic_blocks(std::slice::from_ref(&block)).is_err());
        assert!(
            prepare_treatment(
                std::slice::from_ref(&block),
                profile(2, 0),
                Treatment::Recursive,
                &[]
            )
            .is_err()
        );
        assert_eq!(block.text, " \t\u{2003}\n");
    }

    #[test]
    fn oversized_unmarked_unicode_prose_has_lossless_character_spans() {
        let text = "α界🙂e\u{301}条件".repeat(19) + "  \n";
        let mut block = prose(0, &text, Some(2));
        block.source_span = Some(StructuredSourceSpan {
            start_offset: 73,
            end_offset: 73 + i32::try_from(text.chars().count()).unwrap(),
        });
        let result = prepare_treatment(
            std::slice::from_ref(&block),
            profile(17, 5),
            Treatment::Recursive,
            &[],
        )
        .unwrap();
        assert!(result.chunks.len() > 1);
        assert_eq!(result.blocks[0], block);
        let fragments = &result.blocks[1..];
        let source = text.chars().collect::<Vec<_>>();
        for fragment in fragments {
            let span = fragment.source_span.as_ref().unwrap();
            let start = usize::try_from(span.start_offset - 73).unwrap();
            let end = usize::try_from(span.end_offset - 73).unwrap();
            assert_eq!(fragment.normalized_text, source[start..end].iter().collect::<String>());
            assert_eq!(fragment.text, fragment.normalized_text);
            assert_eq!(fragment.heading_trail, block.heading_trail);
            assert_eq!(fragment.section_path, block.section_path);
            assert_eq!(fragment.page_number, block.page_number);
            assert_eq!(fragment.parent_block_id, Some(block.block_id));
        }
        assert!(result.chunks.iter().all(|chunk| chunk.normalized_text.chars().count() <= 17));
        assert_lossless_split(&block, &result);
    }

    fn pieces(text: &str, max_chars: usize) -> Vec<String> {
        let chars = text.chars().collect::<Vec<_>>();
        let mut ranges = Vec::new();
        recursive_ranges(&chars, 0, chars.len(), 0, max_chars, &mut ranges);
        ranges.into_iter().map(|(start, end)| chars[start..end].iter().collect()).collect()
    }

    #[test]
    fn recursive_separator_hierarchy_precedes_character_fallback() {
        // A lower-level separator fitting the first piece must not override its paragraph cut.
        assert_eq!(pieces("aaaa\n\nbbbb\ncccc", 10), ["aaaa\n\n", "bbbb\ncccc"]);
        assert_eq!(pieces("aaaa\nbbbb cccc", 9), ["aaaa\n", "bbbb cccc"]);
        assert_eq!(pieces("aaaa. bbbb cccc", 10), ["aaaa. ", "bbbb cccc"]);
        assert_eq!(pieces("aaaa bbbb cccc", 9), ["aaaa ", "bbbb cccc"]);
        assert_eq!(pieces("αβγδεζη", 3), ["αβγ", "δεζ", "η"]);
        for text in ["a\n\n\n\nb  \n", "a.   b!  c?  ", "     ", "a\r\nb\r\nc"] {
            let parts = pieces(text, 2);
            assert_eq!(parts.concat(), text);
            assert!(parts.iter().all(|part| part.chars().count() <= 2));
        }
    }

    #[test]
    fn ragged_table_rows_keep_values_labels_coordinates_and_parent_support() {
        let mut parent = prose(0, "full table", Some(1));
        parent.block_kind = StructuredBlockKind::Table;
        let mut row = prose(1, "| alpha | 7 |", Some(2));
        row.block_kind = StructuredBlockKind::TableRow;
        row.normalized_text =
            "Table Ref: 0; Region: alpha; Limit: 7; Condition: only if enabled".into();
        row.parent_block_id = Some(parent.block_id);
        row.table_coordinates = Some(StructuredTableCoordinates {
            row_index: 1,
            column_index: 0,
            row_span: 1,
            column_span: 2,
        });
        let mut ragged = prose(2, "| beta |", Some(2));
        ragged.block_kind = StructuredBlockKind::TableRow;
        ragged.normalized_text = "Table Ref: 0; Region: beta".into();
        ragged.parent_block_id = Some(parent.block_id);
        let blocks = vec![parent.clone(), row.clone(), ragged.clone()];
        // The parent precedes its rows across a page boundary: global filtering is essential.
        for treatment in [Treatment::Recursive, Treatment::Page, Treatment::Semantic] {
            let breaks: &[usize] = if treatment == Treatment::Semantic { &[1] } else { &[] };
            let result = prepare_treatment(&blocks, profile(12, 5), treatment, breaks).unwrap();
            assert_eq!(result.blocks, blocks);
            assert_eq!(result.chunks.len(), 2);
            for (chunk, original) in result.chunks.iter().zip([&row, &ragged]) {
                assert_eq!(chunk.content_text, original.text);
                assert_eq!(chunk.normalized_text, original.normalized_text);
                assert_eq!(chunk.support_block_ids, [original.block_id, parent.block_id]);
                assert!(chunk.window_text.is_none());
            }
            assert_support_resolves(&result);
        }
    }

    #[test]
    fn code_records_and_table_rows_are_overlap_and_context_barriers() {
        for kind in [
            StructuredBlockKind::CodeBlock,
            StructuredBlockKind::SourceUnit,
            StructuredBlockKind::TableRow,
        ] {
            let mut atomic = prose(1, "atomic value condition must remain intact", Some(1));
            atomic.block_kind = kind;
            let blocks = [prose(0, "before", Some(1)), atomic.clone(), prose(2, "after", Some(1))];
            let result =
                prepare_treatment(&blocks, profile(12, 8), Treatment::Recursive, &[]).unwrap();
            assert_eq!(result.chunks.len(), 3);
            assert_eq!(result.chunks[1].normalized_text, atomic.normalized_text);
            assert!(result.chunks.iter().all(|chunk| chunk.window_text.is_none()));
            assert_eq!(result.blocks, blocks);
        }
    }

    #[test]
    fn page_boundaries_prohibit_coalescing_overlap_and_context() {
        let blocks = [
            prose(0, "aaa", Some(1)),
            prose(1, "bbb", Some(1)),
            prose(2, "ccc", Some(2)),
            prose(3, "ddd", Some(2)),
        ];
        let recursive =
            prepare_treatment(&blocks, profile(13, 8), Treatment::Recursive, &[]).unwrap();
        assert_eq!(recursive.chunks[0].normalized_text, "aaa\n\nbbb\n\nccc");
        let page = prepare_treatment(&blocks, profile(13, 8), Treatment::Page, &[]).unwrap();
        assert_eq!(page.boundary_count, 1);
        assert_eq!(page.chunks.len(), 2);
        assert_eq!(page.chunks[0].normalized_text, "aaa\n\nbbb");
        assert_eq!(page.chunks[1].normalized_text, "ccc\n\nddd");
        assert!(page.chunks.iter().all(|chunk| chunk.window_text.is_none()));
        assert_eq!(page.blocks, blocks);
        assert_support_resolves(&page);
    }

    #[test]
    fn incomplete_invalid_or_reversed_page_metadata_is_refused() {
        for pages in [[Some(1), None], [Some(1), Some(0)], [Some(2), Some(1)]] {
            let blocks = [prose(0, "alpha", pages[0]), prose(1, "beta", pages[1])];
            assert!(prepare_treatment(&blocks, profile(20, 5), Treatment::Page, &[]).is_err());
        }
    }

    #[test]
    fn supplied_semantic_boundary_changes_grouping_and_overlap_without_changing_values() {
        let blocks =
            (0..5).map(|index| prose(index, &format!("v{index}."), None)).collect::<Vec<_>>();
        let before = prepare_treatment(&blocks, profile(13, 3), Treatment::Recursive, &[]).unwrap();
        assert_eq!(
            before.chunks[0].support_block_ids,
            [blocks[0].block_id, blocks[1].block_id, blocks[2].block_id]
        );
        assert_eq!(before.chunks[1].support_block_ids[0], blocks[2].block_id);
        let after = prepare_treatment(&blocks, profile(13, 3), Treatment::Semantic, &[3]).unwrap();
        assert_eq!(after.boundary_count, 1);
        assert_eq!(after.chunks.len(), 2);
        assert_eq!(after.chunks[1].support_block_ids, [blocks[3].block_id, blocks[4].block_id]);
        assert_eq!(after.blocks, blocks);
        assert!(after.chunks.iter().all(|chunk| chunk.window_text.is_none()));
        assert_eq!(after.chunks[0].normalized_text, "v0.\n\nv1.\n\nv2.");
        assert_eq!(after.chunks[1].normalized_text, "v3.\n\nv4.");
        assert_support_resolves(&after);
    }

    #[test]
    fn missing_duplicate_out_of_order_and_endpoint_semantic_breaks_are_refused() {
        let blocks = [prose(0, "a", None), prose(1, "b", None), prose(2, "c", None)];
        for breaks in [vec![], vec![0], vec![3], vec![4], vec![1, 1], vec![2, 1]] {
            assert!(
                prepare_treatment(&blocks, profile(20, 5), Treatment::Semantic, &breaks).is_err()
            );
        }
        assert!(prepare_treatment(&blocks, profile(20, 5), Treatment::Recursive, &[1]).is_err());
    }

    #[test]
    fn repeated_ids_are_rejected_and_derived_support_identity_is_deterministic() {
        let block = prose(0, "αβγδεζηθικλμνξοπ", None);
        let first = prepare_treatment(
            std::slice::from_ref(&block),
            profile(4, 1),
            Treatment::Recursive,
            &[],
        )
        .unwrap();
        let second = prepare_treatment(
            std::slice::from_ref(&block),
            profile(4, 1),
            Treatment::Recursive,
            &[],
        )
        .unwrap();
        assert_eq!(first.blocks, second.blocks);
        assert_eq!(first.chunks, second.chunks);
        assert_support_resolves(&first);
        let mut repeated = block.clone();
        repeated.ordinal = 1;
        assert!(
            prepare_treatment(&[block, repeated], profile(4, 1), Treatment::Recursive, &[])
                .is_err()
        );
    }

    #[test]
    fn unknown_parents_and_unprovable_prose_spans_are_refused() {
        let block = prose(0, "a long paragraph that must be divided", None);
        let mut missing = block.clone();
        missing.source_span = None;
        let mut wrong_span = block.clone();
        wrong_span.source_span.as_mut().unwrap().end_offset += 1;
        let mut changed_text = block.clone();
        changed_text.text.push(' ');
        let mut unknown_parent = block;
        unknown_parent.parent_block_id = Some(Uuid::from_u128(99));
        for invalid in [missing, wrong_span, changed_text, unknown_parent] {
            assert!(
                prepare_treatment(&[invalid], profile(4, 1), Treatment::Recursive, &[]).is_err()
            );
        }
    }
}
