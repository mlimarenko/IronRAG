use super::{
    HashMap, HashSet, KnowledgeChunkRow, KnowledgeDocumentRow, RuntimeMatchedChunk,
    SOURCE_PROFILE_CHUNK_KIND, Uuid, excerpt_for, extract_config_assignment_literals,
    extract_config_section_literals, extract_explicit_path_literals,
    extract_package_command_literals, extract_parameter_literals, focused_excerpt_for,
    is_source_profile_chunk_row, is_source_profile_runtime_chunk, map_chunk_hit,
    repair_technical_layout_noise, score_value, structured_literal_excerpt_for,
    structured_literal_line_score,
    types::{
        SOURCE_CONTEXT_EXCERPT_CHARS, SOURCE_CONTEXT_PROFILE_ANCHOR_SCORE_CAP,
        SOURCE_CONTEXT_STRUCTURED_PROFILE_SCORE_BONUS, StructuredSourceCompanion,
        StructuredSourceCompanionKind, StructuredSourceContextDiagnostics,
    },
};

pub(super) fn map_companion_chunk(
    row: KnowledgeChunkRow,
    score: f32,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
) -> Option<RuntimeMatchedChunk> {
    let is_source_profile = is_source_profile_chunk_row(&row);
    let content_text = row.content_text.clone();
    let companion_visible_text =
        (!is_source_profile).then(|| source_context_companion_visible_text(&row));
    let mut chunk = map_chunk_hit(row, score, document_index, plan_keywords)?;
    if is_source_profile {
        chunk.chunk_kind = Some(SOURCE_PROFILE_CHUNK_KIND.to_string());
    }
    if let Some(companion_visible_text) = companion_visible_text
        && source_context_companion_text_should_replace(&companion_visible_text, &chunk.source_text)
    {
        chunk.source_text = companion_visible_text;
    }
    let repaired_content_text = repair_technical_layout_noise(&content_text);
    if source_context_content_preserves_missing_paths(&repaired_content_text, &chunk.source_text) {
        chunk.source_text = repaired_content_text;
    }
    chunk.score = Some(score);
    chunk.score_kind = crate::services::query::execution::RuntimeChunkScoreKind::SourceContext;
    chunk.excerpt = if is_source_profile_runtime_chunk(&chunk) {
        source_profile_excerpt(&chunk.source_text)
    } else {
        structured_literal_excerpt_for(
            &chunk.source_text,
            plan_keywords,
            SOURCE_CONTEXT_EXCERPT_CHARS,
        )
        .unwrap_or_else(|| {
            let excerpt = focused_excerpt_for(
                &chunk.source_text,
                plan_keywords,
                SOURCE_CONTEXT_EXCERPT_CHARS,
            );
            if excerpt.trim().is_empty() {
                excerpt_for(&chunk.source_text, SOURCE_CONTEXT_EXCERPT_CHARS)
            } else {
                excerpt
            }
        })
    };
    Some(chunk)
}

pub(super) fn source_context_content_preserves_missing_paths(
    content_text: &str,
    source_text: &str,
) -> bool {
    let paths = extract_explicit_path_literals(content_text, 8);
    !paths.is_empty() && paths.iter().any(|path| !source_text.contains(path))
}

pub(super) fn source_context_companion_visible_text(row: &KnowledgeChunkRow) -> String {
    const MAX_VISIBLE_TEXT_CHARS: usize = 16_000;

    let mut parts = Vec::new();
    let mut seen = HashSet::new();
    for value in [
        row.window_text.as_deref(),
        Some(row.content_text.as_str()),
        Some(row.normalized_text.as_str()),
    ]
    .into_iter()
    .flatten()
    {
        let repaired = repair_technical_layout_noise(value);
        let trimmed = repaired.trim();
        if trimmed.is_empty() {
            continue;
        }
        let normalized = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
        if seen.insert(normalized) {
            parts.push(trimmed.to_string());
        }
    }
    let joined = parts.join("\n");
    if joined.chars().count() <= MAX_VISIBLE_TEXT_CHARS {
        return joined;
    }
    structured_literal_excerpt_for(&joined, &[], MAX_VISIBLE_TEXT_CHARS)
        .unwrap_or_else(|| excerpt_for(&joined, MAX_VISIBLE_TEXT_CHARS))
}

pub(super) fn source_context_companion_text_should_replace(candidate: &str, current: &str) -> bool {
    let candidate_score = structured_literal_text_score(candidate);
    if candidate_score == 0 {
        return false;
    }
    let current_score = structured_literal_text_score(current);
    candidate_score > current_score
        || source_context_text_preserves_missing_structured_literals(candidate, current)
}

pub(super) fn source_context_text_preserves_missing_structured_literals(
    candidate: &str,
    current: &str,
) -> bool {
    source_context_structured_literals(candidate, 32)
        .iter()
        .any(|literal| !current.contains(literal))
}

pub(super) fn structured_literal_text_score(text: &str) -> usize {
    text.lines().map(str::trim).map(structured_literal_line_score).sum()
}

pub(super) fn source_context_structured_literals(text: &str, limit: usize) -> Vec<String> {
    let mut values = Vec::new();
    let mut seen = HashSet::new();
    for value in extract_config_assignment_literals(text, limit) {
        push_unique_literal(&mut values, &mut seen, value, limit);
    }
    for value in extract_config_section_literals(text, limit) {
        push_unique_literal(&mut values, &mut seen, value, limit);
    }
    for value in extract_explicit_path_literals(text, limit) {
        push_unique_literal(&mut values, &mut seen, value, limit);
    }
    for value in extract_package_command_literals(text, limit) {
        push_unique_literal(&mut values, &mut seen, value, limit);
    }
    for value in extract_parameter_literals(text, limit) {
        push_unique_literal(&mut values, &mut seen, value, limit);
    }
    values
}

pub(super) fn push_unique_literal(
    values: &mut Vec<String>,
    seen: &mut HashSet<String>,
    value: String,
    limit: usize,
) {
    if values.len() >= limit {
        return;
    }
    if seen.insert(value.to_lowercase()) {
        values.push(value);
    }
}

pub(super) fn apply_structured_source_companions(
    chunks: &mut Vec<RuntimeMatchedChunk>,
    companions: Vec<StructuredSourceCompanion>,
) -> StructuredSourceContextDiagnostics {
    if companions.is_empty() {
        return StructuredSourceContextDiagnostics::default();
    }

    let original_rank = chunks
        .iter()
        .enumerate()
        .map(|(rank, chunk)| (chunk.chunk_id, rank))
        .collect::<HashMap<_, _>>();
    let mut merged =
        chunks.drain(..).map(|chunk| (chunk.chunk_id, chunk)).collect::<HashMap<_, _>>();
    let mut diagnostics = StructuredSourceContextDiagnostics::default();

    for companion in companions {
        match companion.kind {
            StructuredSourceCompanionKind::SourceProfile => {
                diagnostics.source_profile_count += 1;
            }
            StructuredSourceCompanionKind::Neighbor => {
                diagnostics.neighbor_count += 1;
            }
            StructuredSourceCompanionKind::FocusedMatch => {
                diagnostics.focused_match_count += 1;
            }
            StructuredSourceCompanionKind::ProceduralStructuredSibling => {
                diagnostics.procedural_structured_sibling_count += 1;
            }
            StructuredSourceCompanionKind::LibrarySourceProfile => {
                diagnostics.source_profile_count += 1;
                diagnostics.library_profile_count += 1;
            }
        }
        merged
            .entry(companion.chunk.chunk_id)
            .and_modify(|existing| {
                if score_value(companion.chunk.score) > score_value(existing.score) {
                    *existing = companion.chunk.clone();
                } else if source_context_content_preserves_missing_paths(
                    &companion.chunk.source_text,
                    &existing.source_text,
                ) {
                    existing.source_text = companion.chunk.source_text.clone();
                    existing.excerpt = companion.chunk.excerpt.clone();
                }
            })
            .or_insert(companion.chunk);
    }

    let mut values = merged.into_values().collect::<Vec<_>>();
    values.sort_by(|left, right| {
        score_value(right.score)
            .total_cmp(&score_value(left.score))
            .then_with(|| {
                let left_rank = original_rank.get(&left.chunk_id).copied().unwrap_or(usize::MAX);
                let right_rank = original_rank.get(&right.chunk_id).copied().unwrap_or(usize::MAX);
                left_rank.cmp(&right_rank)
            })
            .then_with(|| left.document_id.cmp(&right.document_id))
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    *chunks = values;
    diagnostics
}

pub(super) fn structured_inventory_source_profile_score(anchor_score: f32, rank: usize) -> f32 {
    anchor_score.clamp(0.0, SOURCE_CONTEXT_PROFILE_ANCHOR_SCORE_CAP)
        + SOURCE_CONTEXT_STRUCTURED_PROFILE_SCORE_BONUS
        - rank as f32 * 0.01
}

pub(super) fn current_source_profile_revision_ids(
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    limit: usize,
) -> Vec<Uuid> {
    if limit == 0 {
        return Vec::new();
    }
    let mut rows = document_index
        .values()
        .filter(|document| document.is_query_readable())
        .filter_map(|document| {
            document
                .readable_revision_id
                .or(document.active_revision_id)
                .map(|revision_id| (document.updated_at, document.document_id, revision_id))
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| right.1.cmp(&left.1)));
    rows.truncate(limit);
    rows.into_iter().map(|(_, _, revision_id)| revision_id).collect()
}

pub(super) fn source_profile_excerpt(text: &str) -> String {
    text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or(text.trim()).to_string()
}
