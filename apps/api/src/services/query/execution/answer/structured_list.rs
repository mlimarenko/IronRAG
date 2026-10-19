use super::{
    AnswerCompletionContract, BTreeSet, QueryAct, QueryIR, RuntimeMatchedChunk,
    UpdateProcedureFocusModel, build_structural_identifier_mapping_answer,
    focused_answer_document_id, i18n, line_has_command_signal, normalized_alnum_tokens,
    query_ir_has_typed_table_column_inventory_intent, repair_technical_layout_noise,
    strip_leading_order_marker, update_procedure_focus_model,
    update_procedure_text_target_identity_priority,
};

pub(super) fn build_structured_list_grounded_answer(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if let Some(answer) = build_structural_identifier_mapping_answer(question, query_ir, chunks) {
        return Some(answer);
    }
    if query_ir.source_slice.is_some() || !query_ir_requests_structured_list_answer(query_ir) {
        return None;
    }
    let focus_terms = structured_list_focus_terms(query_ir);
    if focus_terms.is_empty() {
        return None;
    }
    let procedure_focus = matches!(query_ir.act, QueryAct::ConfigureHow)
        .then(|| update_procedure_focus_model(query_ir));
    // Extraction must answer from the document the question is about. Without
    // this bound the best list-shaped chunk anywhere in the bundle wins, so
    // two questions about different documents receive the same list. The
    // resolver returns None for multi-document questions, which keeps
    // cross-document enumerations seeing the whole bundle.
    let focused_document_id = focused_answer_document_id(question, chunks);
    let mut candidates = chunks
        .iter()
        .filter(|chunk| {
            focused_document_id.is_none_or(|document_id| chunk.document_id == document_id)
        })
        .filter(|chunk| {
            procedure_focus.as_ref().is_none_or(|focus| {
                structured_list_chunk_has_acceptable_procedure_identity(chunk, focus)
            })
        })
        .filter_map(|chunk| {
            extract_structured_list_candidate(chunk, &focus_terms)
                .map(|candidate| (chunk.document_id, candidate))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|(_, left), (_, right)| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| right.items.len().cmp(&left.items.len()))
            .then_with(|| left.first_chunk_index.cmp(&right.first_chunk_index))
    });
    let (_, candidate) = candidates.into_iter().next()?;
    if candidate.items.len() < 2 {
        return None;
    }
    if !structured_list_source_addresses_question_focus(query_ir, &candidate, chunks) {
        return None;
    }
    if matches!(query_ir.act, QueryAct::ConfigureHow)
        && candidate
            .items
            .iter()
            .filter(|item| structured_list_item_has_procedure_shape(item))
            .take(2)
            .count()
            < 2
    {
        return None;
    }
    if candidate.items.len() > super::MAX_ANSWER_BLOCKS {
        return None;
    }
    let mut lines = Vec::with_capacity(candidate.items.len());
    for (index, item) in candidate.items.into_iter().enumerate() {
        if candidate.ordered {
            lines.push(format!("{}. {}", index + 1, item));
        } else {
            lines.push(format!("- {item}"));
        }
    }
    let answer = lines.join("\n");
    AnswerCompletionContract::from_query_ir(query_ir).evaluate(&answer).complete.then_some(answer)
}

/// Question-fit gate scoped to the extracted items. A structural focus term
/// must be observed in the evidence bundle, belong to the tier owned by the
/// fewest documents, and occur as a complete word in an item. A sibling chunk
/// cannot vouch for an unrelated list from the same document.
pub(super) fn structured_list_source_addresses_question_focus(
    query_ir: &QueryIR,
    candidate: &StructuredListCandidate,
    chunks: &[RuntimeMatchedChunk],
) -> bool {
    let probe_terms = structured_list_question_probe_terms(query_ir);
    let probe_terms = least_document_frequency_structured_list_terms(&probe_terms, chunks);
    if probe_terms.is_empty() {
        return false;
    }
    let addressed = probe_terms.iter().any(|term| {
        candidate.items.iter().any(|item| structured_list_has_word_boundary_match(item, term))
    });
    if !addressed {
        tracing::info!(
            stage = "answer.structured_list_question_fit",
            probe_term_count = probe_terms.len(),
            item_count = candidate.items.len(),
            "structured list rejected: extracted items do not address the question focus"
        );
    }
    addressed
}

fn least_document_frequency_structured_list_terms(
    focus_terms: &BTreeSet<String>,
    chunks: &[RuntimeMatchedChunk],
) -> Vec<String> {
    let mut frequencies = focus_terms
        .iter()
        .filter_map(|term| {
            let document_count = chunks
                .iter()
                .filter(|chunk| {
                    structured_list_has_word_boundary_match(&chunk.source_text, term)
                        || structured_list_has_word_boundary_match(&chunk.document_label, term)
                })
                .map(|chunk| chunk.document_id)
                .collect::<BTreeSet<_>>()
                .len();
            (document_count > 0).then(|| (document_count, term.clone()))
        })
        .collect::<Vec<_>>();
    let minimum_frequency = frequencies.iter().map(|(count, _)| *count).min();
    frequencies.retain(|(count, _)| Some(*count) == minimum_frequency);
    frequencies.into_iter().map(|(_, term)| term).collect()
}

fn structured_list_has_word_boundary_match(haystack: &str, term: &str) -> bool {
    let term = term.trim();
    if haystack.is_empty() || term.is_empty() {
        return false;
    }
    let haystack = haystack.to_lowercase();
    let term = term.to_lowercase();
    haystack.match_indices(&term).any(|(start, matched)| {
        let end = start + matched.len();
        let before = haystack[..start].chars().next_back();
        let after = haystack[end..].chars().next();
        !before.is_some_and(structured_list_word_character)
            && !after.is_some_and(structured_list_word_character)
    })
}

fn structured_list_word_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

fn structured_list_item_has_procedure_shape(item: &str) -> bool {
    if line_has_command_signal(item) {
        return true;
    }
    let trimmed = item.trim();
    matches!(trimmed.chars().next_back(), Some('.' | ';' | ':' | '!' | '?'))
        && normalized_alnum_tokens(trimmed, 1).len() >= 2
}

fn structured_list_chunk_has_acceptable_procedure_identity(
    chunk: &RuntimeMatchedChunk,
    focus_model: &UpdateProcedureFocusModel,
) -> bool {
    if focus_model.target_identity_sequences.is_empty() {
        return true;
    }
    let text = repair_technical_layout_noise(&chunk.source_text);
    update_procedure_text_target_identity_priority(&chunk.document_label, focus_model) > 0
        || text
            .lines()
            .any(|line| update_procedure_text_target_identity_priority(line, focus_model) > 0)
}

#[derive(Debug)]
pub(super) struct StructuredListCandidate {
    pub(super) items: Vec<String>,
    pub(super) ordered: bool,
    pub(super) score: usize,
    pub(super) first_chunk_index: i32,
}

fn query_ir_requests_structured_list_answer(query_ir: &QueryIR) -> bool {
    if query_ir_has_typed_table_column_inventory_intent(query_ir) {
        return false;
    }

    let has_structured_focus = query_ir.document_focus.is_some()
        || !query_ir.target_types.is_empty()
        || !query_ir.target_entities.is_empty()
        || !query_ir.literal_constraints.is_empty()
        || query_ir.retrieval_query.as_deref().is_some_and(|query| !query.trim().is_empty());
    match query_ir.act {
        QueryAct::Enumerate => has_structured_focus || query_ir.confidence < 0.6,
        QueryAct::ConfigureHow => has_structured_focus,
        QueryAct::Describe | QueryAct::RetrieveValue => {
            has_structured_focus && query_ir.confidence < 0.6
        }
        _ => false,
    }
}

fn structured_list_focus_terms(query_ir: &QueryIR) -> BTreeSet<String> {
    let mut terms = BTreeSet::new();
    if let Some(retrieval_query) = query_ir.retrieval_query.as_deref() {
        terms.extend(normalized_alnum_tokens(retrieval_query, 3));
    }
    for entity in &query_ir.target_entities {
        terms.extend(normalized_alnum_tokens(&entity.label, 3));
    }
    for literal in &query_ir.literal_constraints {
        terms.extend(normalized_alnum_tokens(&literal.text, 3));
    }
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        terms.extend(normalized_alnum_tokens(&document_focus.hint, 3));
    }
    terms
}

fn structured_list_question_probe_terms(query_ir: &QueryIR) -> BTreeSet<String> {
    let mut terms = structured_list_focus_terms(query_ir);
    if let Some(retrieval_query) = query_ir.retrieval_query.as_deref() {
        extend_structured_list_probe_terms(&mut terms, retrieval_query, 3);
    }
    for entity in &query_ir.target_entities {
        extend_structured_list_probe_terms(&mut terms, &entity.label, 2);
    }
    for literal in &query_ir.literal_constraints {
        extend_structured_list_probe_terms(&mut terms, &literal.text, 2);
    }
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        extend_structured_list_probe_terms(&mut terms, &document_focus.hint, 3);
    }
    terms
}

fn extend_structured_list_probe_terms(
    terms: &mut BTreeSet<String>,
    value: &str,
    minimum_chars: usize,
) {
    terms.extend(normalized_alnum_tokens(value, minimum_chars));
    for fragment in value.split_whitespace() {
        let token = fragment.trim_matches(|character: char| {
            !character.is_alphanumeric() && !matches!(character, '_' | '-' | '.' | '/' | ':')
        });
        if token.chars().count() >= minimum_chars
            && token.chars().any(|character| matches!(character, '_' | '-' | '.' | '/' | ':'))
        {
            terms.insert(token.to_lowercase());
        }
    }
}

fn extract_structured_list_candidate(
    chunk: &RuntimeMatchedChunk,
    focus_terms: &BTreeSet<String>,
) -> Option<StructuredListCandidate> {
    let text = repair_technical_layout_noise(&chunk.source_text);
    let lines = text.lines().map(str::trim).collect::<Vec<_>>();
    let marked_lines = lines.iter().copied().filter(|line| !line.is_empty()).collect::<Vec<_>>();
    if marked_lines.is_empty() {
        return None;
    }
    let mut candidates = Vec::new();
    candidates.extend(extract_marked_list_candidates(
        &marked_lines,
        focus_terms,
        chunk.chunk_index,
    ));
    candidates.extend(extract_comment_list_candidates(&lines, focus_terms, chunk.chunk_index));
    candidates.into_iter().max_by(|left, right| {
        left.score
            .cmp(&right.score)
            .then_with(|| left.items.len().cmp(&right.items.len()))
            .then_with(|| right.first_chunk_index.cmp(&left.first_chunk_index))
    })
}

fn extract_marked_list_candidates(
    lines: &[&str],
    focus_terms: &BTreeSet<String>,
    chunk_index: i32,
) -> Vec<StructuredListCandidate> {
    let mut candidates = Vec::new();
    for (line_index, line) in lines.iter().enumerate() {
        let Some(candidate) =
            marked_list_candidate(lines, line_index, line, focus_terms, chunk_index)
        else {
            continue;
        };
        candidates.push(candidate);
    }
    candidates
}

fn marked_list_candidate(
    lines: &[&str],
    line_index: usize,
    line: &str,
    focus_terms: &BTreeSet<String>,
    chunk_index: i32,
) -> Option<StructuredListCandidate> {
    if !line.ends_with(':') {
        return None;
    }
    let heading_score = structured_list_line_focus_score(line, focus_terms);
    if heading_score == 0 && !focus_terms.is_empty() {
        return None;
    }
    let (items, ordered) = marked_list_items_after_heading(lines, line_index);
    if items.len() < 2 {
        return None;
    }
    let item_score =
        items.iter().map(|item| structured_list_line_focus_score(item, focus_terms)).sum();
    Some(StructuredListCandidate {
        items,
        ordered,
        score: heading_score.saturating_mul(4).saturating_add(item_score),
        first_chunk_index: chunk_index,
    })
}

fn marked_list_items_after_heading(lines: &[&str], line_index: usize) -> (Vec<String>, bool) {
    let mut items = Vec::new();
    let mut ordered = false;
    for following in lines.iter().skip(line_index + 1) {
        if let Some((item_ordered, item)) = parse_structured_list_item(following) {
            ordered |= item_ordered;
            push_unique_structured_list_item(&mut items, item);
        } else if !items.is_empty() {
            break;
        }
    }
    (items, ordered)
}

pub(super) fn extract_comment_list_candidates(
    lines: &[&str],
    focus_terms: &BTreeSet<String>,
    chunk_index: i32,
) -> Vec<StructuredListCandidate> {
    let mut items = Vec::new();
    let mut inside_comment_block = false;
    let mut comment_block_ended = false;
    for line in lines {
        let trimmed = line.trim_start();
        let Some(comment) = structured_list_comment_body(trimmed) else {
            if inside_comment_block {
                comment_block_ended = true;
            }
            continue;
        };
        if comment_block_ended {
            return Vec::new();
        }
        inside_comment_block = true;
        let item = comment.trim();
        if item.chars().count() < 3 || item.chars().count() > 120 {
            continue;
        }
        push_unique_structured_list_item(&mut items, item.to_string());
    }
    if items.len() < 3 {
        return Vec::new();
    }
    if !items.iter().any(|item| {
        focus_terms.iter().any(|term| structured_list_has_word_boundary_match(item, term))
    }) {
        return Vec::new();
    }
    let item_score =
        items.iter().map(|item| structured_list_line_focus_score(item, focus_terms)).sum::<usize>();
    if item_score == 0 {
        return Vec::new();
    }
    vec![StructuredListCandidate {
        items,
        ordered: true,
        score: item_score.saturating_add(4),
        first_chunk_index: chunk_index,
    }]
}

fn structured_list_comment_body(line: &str) -> Option<&str> {
    line.strip_prefix("# ")
        .or_else(|| line.strip_prefix("// "))
        .or_else(|| line.strip_prefix("* "))
        .or_else(|| matches!(line, "#" | "//" | "*" | "/**" | "*/").then_some(""))
}

fn parse_structured_list_item(line: &str) -> Option<(bool, String)> {
    let trimmed = line.trim();
    let ordered_body = strip_leading_order_marker(trimmed);
    if ordered_body != trimmed {
        return Some((true, clean_structured_list_item(ordered_body)?));
    }
    let bullet_body = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))?;
    Some((false, clean_structured_list_item(bullet_body)?))
}

fn clean_structured_list_item(item: &str) -> Option<String> {
    let cleaned =
        item.trim().trim_matches('"').trim_matches('`').trim_end_matches(',').trim().to_string();
    if cleaned.chars().count() < 2 || cleaned.chars().count() > 180 {
        return None;
    }
    Some(cleaned)
}

fn push_unique_structured_list_item(items: &mut Vec<String>, item: String) {
    let key = structured_list_item_identity(&item);
    if key.is_empty() || items.iter().any(|existing| structured_list_item_identity(existing) == key)
    {
        return;
    }
    items.push(item);
}

fn structured_list_item_identity(item: &str) -> String {
    normalized_alnum_tokens(item, 2).into_iter().collect::<Vec<_>>().join(" ")
}

fn structured_list_line_focus_score(line: &str, focus_terms: &BTreeSet<String>) -> usize {
    if focus_terms.is_empty() {
        return 0;
    }
    let tokens = normalized_alnum_tokens(line, 3).into_iter().collect::<BTreeSet<_>>();
    focus_terms
        .iter()
        .filter(|term| {
            tokens.contains(term.as_str()) || line.to_lowercase().contains(term.as_str())
        })
        .count()
}

#[derive(Debug, Clone, Copy)]
pub(super) struct DeterministicAnswerLabels {
    pub(super) variants: &'static str,
    pub(super) source: &'static str,
    pub(super) package: &'static str,
    pub(super) reconfigure: &'static str,
    pub(super) path: &'static str,
    pub(super) section: &'static str,
    pub(super) parameter: &'static str,
    pub(super) parameter_details: &'static str,
    pub(super) update_sequence: &'static str,
}

pub(super) fn deterministic_answer_labels(query_ir: &QueryIR) -> DeterministicAnswerLabels {
    let labels = i18n::deterministic_answer_labels(i18n::deterministic_answer_language(query_ir));
    DeterministicAnswerLabels {
        variants: labels.variants,
        source: labels.source,
        package: labels.package,
        reconfigure: labels.reconfigure,
        path: labels.path,
        section: labels.section,
        parameter: labels.parameter,
        parameter_details: labels.parameter_details,
        update_sequence: labels.update_sequence,
    }
}
