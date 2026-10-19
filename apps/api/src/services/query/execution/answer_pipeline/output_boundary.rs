use super::{
    AssistantGroundingEvidence, HashSet, PreparedAnswerQueryResult, QueryIR, RuntimeChunkScoreKind,
    RuntimeMatchedChunk, SINGLE_SHOT_CONFIDENT_ANSWER_CHARS, Uuid,
    strip_trailing_media_source_token,
};

pub(super) fn enforce_hard_output_boundary(
    execution_id: Uuid,
    source_stage: &'static str,
    query_ir: &QueryIR,
    answer: String,
) -> String {
    let original_chars = answer.chars().count();
    let answer = strip_trailing_media_source_token(&answer).unwrap_or(answer);
    let expected_inventory_count =
        query_ir.source_slice.as_ref().and_then(|source_slice| source_slice.count).map(usize::from);
    let trimmed = strip_trailing_inventory_meta_paragraph(&answer, expected_inventory_count)
        .unwrap_or(answer);
    if trimmed.chars().count() == original_chars {
        return trimmed;
    }
    tracing::info!(
        stage = "answer.hard_boundary_trim",
        %execution_id,
        source_stage,
        trimmed_chars = original_chars.saturating_sub(trimmed.chars().count()),
        "trimmed trailing non-substantive generated-answer material"
    );
    trimmed
}

pub(super) fn strip_trailing_inventory_meta_paragraph(
    answer: &str,
    expected_inventory_count: Option<usize>,
) -> Option<String> {
    let trimmed = answer.trim_end();
    if trimmed.is_empty() {
        return None;
    }
    let lines = trimmed.lines().collect::<Vec<_>>();
    let (trailing_start, trailing_end) = trailing_paragraph_bounds(&lines)?;
    let trailing_lines = &lines[trailing_start..trailing_end];
    let trailing_paragraph =
        trailing_lines.iter().map(|line| line.trim()).collect::<Vec<_>>().join(" ");
    if !inventory_meta_paragraph_is_removable(&trailing_paragraph, trailing_lines) {
        return None;
    }
    let previous_end = previous_non_empty_line_end(&lines, trailing_start)?;
    if !previous_paragraph_contains_inventory(&lines, previous_end) {
        return None;
    }
    let list_item_count = markdown_inventory_item_count(&lines[..previous_end]);
    let expected_count_satisfied = expected_inventory_count
        .filter(|expected| *expected > 0)
        .is_some_and(|expected| list_item_count >= expected);
    if !ends_with_question_mark(&trailing_paragraph) && !expected_count_satisfied {
        return None;
    }
    Some(lines[..previous_end].join("\n").trim_end().to_string())
}

pub(super) fn trailing_paragraph_bounds(lines: &[&str]) -> Option<(usize, usize)> {
    let trailing_end = lines.iter().rposition(|line| !line.trim().is_empty())?.saturating_add(1);
    let trailing_start = lines[..trailing_end]
        .iter()
        .rposition(|line| line.trim().is_empty())
        .map_or(0, |index| index + 1);
    (trailing_start > 0).then_some((trailing_start, trailing_end))
}

pub(super) fn inventory_meta_paragraph_is_removable(paragraph: &str, lines: &[&str]) -> bool {
    !paragraph.is_empty()
        && paragraph.chars().count() <= 300
        && !paragraph.contains('`')
        && !paragraph.contains("://")
        && !paragraph.contains("```")
        && sentence_terminal_count(paragraph) <= 2
        && !lines.iter().any(|line| is_markdown_inventory_item(line.trim_start()))
}

pub(super) fn previous_non_empty_line_end(lines: &[&str], before: usize) -> Option<usize> {
    lines[..before].iter().rposition(|line| !line.trim().is_empty()).map(|index| index + 1)
}

pub(super) fn previous_paragraph_contains_inventory(lines: &[&str], end: usize) -> bool {
    let start =
        lines[..end].iter().rposition(|line| line.trim().is_empty()).map_or(0, |index| index + 1);
    lines[start..end].iter().any(|line| is_markdown_inventory_item(line.trim_start()))
}

pub(super) fn markdown_inventory_item_count(lines: &[&str]) -> usize {
    lines.iter().filter(|line| is_markdown_inventory_item(line.trim_start())).count()
}

pub(super) fn is_markdown_inventory_item(line: &str) -> bool {
    let mut chars = line.chars();
    match chars.next() {
        Some('-' | '*' | '•') => chars.next().is_some_and(char::is_whitespace),
        Some(first) if first.is_ascii_digit() => {
            let mut saw_separator = false;
            for ch in chars {
                if ch.is_ascii_digit() {
                    continue;
                }
                saw_separator = matches!(ch, '.' | ')');
                if !saw_separator {
                    return false;
                }
                break;
            }
            saw_separator
                && line
                    .chars()
                    .skip_while(|ch| ch.is_ascii_digit())
                    .nth(1)
                    .is_some_and(char::is_whitespace)
        }
        _ => false,
    }
}

pub(super) fn ends_with_question_mark(value: &str) -> bool {
    value.trim_end().chars().next_back().is_some_and(|ch| matches!(ch, '?' | '？' | '؟'))
}

pub(super) fn sentence_terminal_count(value: &str) -> usize {
    value
        .chars()
        .filter(|ch| matches!(ch, '.' | '!' | '?' | '…' | '。' | '！' | '？' | '؟'))
        .count()
}

pub(super) fn looks_like_internal_effective_query_block(value: &str) -> bool {
    let mut has_scope = false;
    let mut has_question = false;
    let mut has_entities = false;
    let mut line_count = 0usize;
    for line in value.lines().map(str::trim).filter(|line| !line.is_empty()) {
        line_count += 1;
        has_scope |= line.starts_with("scope:");
        has_question |= line.starts_with("question:");
        has_entities |= line.starts_with("entities:");
    }
    has_scope && has_question && (has_entities || line_count <= 4)
}

pub(super) fn selected_runtime_answer_chunks(
    prepared: &PreparedAnswerQueryResult,
) -> Vec<RuntimeMatchedChunk> {
    let mut seen = HashSet::<Uuid>::new();
    let mut chunks = Vec::<RuntimeMatchedChunk>::new();
    for chunk in prepared
        .structured
        .ordered_source_units
        .iter()
        .chain(prepared.structured.technical_literal_chunks.iter())
        .chain(prepared.structured.context_chunks.iter())
    {
        if seen.insert(chunk.chunk_id) {
            chunks.push(chunk.clone());
        }
    }
    chunks
}

pub(super) fn build_update_procedure_answer_with_source_context_fallback(
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<(String, Vec<RuntimeMatchedChunk>)> {
    let primary_chunks = chunks
        .iter()
        .filter(|chunk| chunk.score_kind != RuntimeChunkScoreKind::SourceContext)
        .cloned()
        .collect::<Vec<_>>();
    if let Some(answer) =
        super::super::answer::build_update_procedure_sequence_answer(query_ir, &primary_chunks)
    {
        return Some((answer, primary_chunks));
    }
    if primary_chunks.len() == chunks.len() {
        return None;
    }

    let fallback_chunks = chunks.to_vec();
    super::super::answer::build_update_procedure_sequence_answer(query_ir, &fallback_chunks)
        .map(|answer| (answer, fallback_chunks))
}

pub(super) fn selected_runtime_grounding_evidence(
    prepared: &PreparedAnswerQueryResult,
    mut grounding: AssistantGroundingEvidence,
) -> AssistantGroundingEvidence {
    let mut seen = grounding
        .verification_corpus
        .iter()
        .map(|fragment| fragment.trim().to_string())
        .collect::<HashSet<_>>();

    push_grounding_fragment(
        &mut grounding.verification_corpus,
        &mut seen,
        &prepared.structured.context_text,
    );
    if let Some(text) = &prepared.structured.technical_literals_text {
        push_grounding_fragment(&mut grounding.verification_corpus, &mut seen, text);
    }
    for line in &prepared.structured.graph_evidence_context_lines {
        push_grounding_fragment(&mut grounding.verification_corpus, &mut seen, line);
    }
    for chunk in prepared
        .structured
        .ordered_source_units
        .iter()
        .chain(prepared.structured.technical_literal_chunks.iter())
        .chain(prepared.structured.context_chunks.iter())
    {
        push_grounding_fragment(&mut grounding.verification_corpus, &mut seen, &chunk.source_text);
        push_grounding_fragment(&mut grounding.verification_corpus, &mut seen, &chunk.excerpt);
    }
    for document in &prepared.structured.retrieved_documents {
        push_grounding_fragment(
            &mut grounding.verification_corpus,
            &mut seen,
            &document.preview_excerpt,
        );
        push_grounding_fragment(&mut grounding.verification_corpus, &mut seen, &document.title);
        if let Some(document_hint) = &document.document_hint {
            push_grounding_fragment(&mut grounding.verification_corpus, &mut seen, document_hint);
        }
    }
    push_grounding_fragment(
        &mut grounding.verification_corpus,
        &mut seen,
        &prepared.answer_context,
    );
    grounding
}

pub(super) fn push_grounding_fragment(
    corpus: &mut Vec<String>,
    seen: &mut HashSet<String>,
    fragment: &str,
) {
    let trimmed = fragment.trim();
    if trimmed.is_empty() {
        return;
    }
    if seen.insert(trimmed.to_string()) {
        corpus.push(trimmed.to_string());
    }
}

pub(super) fn literal_revision_context(
    prompt_context: &str,
    assistant_grounding: &AssistantGroundingEvidence,
) -> String {
    let mut context = prompt_context.trim().to_string();
    if assistant_grounding.verification_corpus.is_empty() {
        return context;
    }
    if !context.is_empty() {
        context.push_str("\n\n");
    }
    context.push_str("Additional tool evidence observed by the answer generator:\n");
    for (index, fragment) in assistant_grounding.verification_corpus.iter().enumerate() {
        let trimmed = fragment.trim();
        if trimmed.is_empty() {
            continue;
        }
        context.push_str(&format!("\n[TOOL_EVIDENCE {}]\n{}\n", index + 1, trimmed));
    }
    context
}

pub(super) fn merge_generation_usage(
    mut primary: serde_json::Value,
    additional: &serde_json::Value,
) -> serde_json::Value {
    crate::services::query::agent_loop::merge_usage_into(&mut primary, additional);
    primary
}

pub(super) fn source_slice_single_shot_min_chars(query_ir: &QueryIR) -> Option<usize> {
    let requested = super::super::source_slice_requested_count(query_ir)?;
    Some((requested.saturating_mul(48)).max(SINGLE_SHOT_CONFIDENT_ANSWER_CHARS))
}
