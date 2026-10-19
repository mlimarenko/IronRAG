use super::*;

pub(super) fn grounded_answer_must_preserve_spans(
    answer_text: &str,
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
    include_source_titles: bool,
) -> Vec<String> {
    let graph_spans = if include_source_titles {
        grounded_answer_graph_preserve_span_candidates(execution_detail)
    } else {
        Default::default()
    };
    let source_titles = include_source_titles.then_some(()).into_iter().flat_map(|()| {
        execution_detail
            .prepared_segment_references
            .iter()
            .filter_map(|reference| reference.document_title.as_deref())
    });
    grounded_answer_must_preserve_spans_for_evidence(answer_text, graph_spans, source_titles)
}

#[cfg(test)]
pub(crate) fn grounded_answer_must_preserve_spans_for_source_titles<'a>(
    answer_text: &str,
    source_titles: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    grounded_answer_must_preserve_spans_for_evidence(answer_text, std::iter::empty(), source_titles)
}

pub(crate) fn grounded_answer_must_preserve_spans_for_evidence<'a>(
    answer_text: &str,
    graph_spans: impl IntoIterator<Item = &'a str>,
    source_titles: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let mut spans = Vec::new();
    let mut seen = BTreeSet::new();
    for span in backtick_literal_spans(answer_text) {
        push_grounded_answer_preserve_span(&mut spans, &mut seen, &span);
        if spans.len() >= GROUNDED_ANSWER_MUST_PRESERVE_SPAN_LIMIT {
            break;
        }
    }
    for span in adjacent_code_span_assignments(answer_text) {
        push_grounded_answer_preserve_candidate(&mut spans, &mut seen, &span);
        if spans.len() >= GROUNDED_ANSWER_MUST_PRESERVE_SPAN_LIMIT {
            break;
        }
    }
    for graph_span in graph_spans {
        push_grounded_answer_preserve_evidence_span(&mut spans, &mut seen, graph_span);
        if spans.len() >= GROUNDED_ANSWER_MUST_PRESERVE_SPAN_LIMIT {
            break;
        }
    }
    for title in source_titles {
        push_grounded_answer_preserve_source_title(&mut spans, &mut seen, title);
        if spans.len() >= GROUNDED_ANSWER_MUST_PRESERVE_SPAN_LIMIT {
            break;
        }
    }
    spans
}

pub(super) fn grounded_answer_graph_preserve_span_candidates(
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
) -> Vec<&str> {
    let mut candidates = Vec::new();
    for relation in execution_detail.relation_references.iter().filter(|reference| {
        reference.rank > 0 && reference.rank <= GROUNDED_ANSWER_GRAPH_PRESERVE_MAX_RANK
    }) {
        if let Some(assertion) = relation.normalized_assertion.as_deref() {
            candidates.push(assertion);
        }
        if candidates.len() >= GROUNDED_ANSWER_GRAPH_PRESERVE_SPAN_LIMIT {
            return candidates;
        }
    }
    for entity in execution_detail.entity_references.iter().filter(|reference| {
        reference.rank > 0 && reference.rank <= GROUNDED_ANSWER_GRAPH_PRESERVE_MAX_RANK
    }) {
        candidates.push(entity.label.as_str());
        if let Some(summary) = entity.summary.as_deref() {
            candidates.push(summary);
        }
        if candidates.len() >= GROUNDED_ANSWER_GRAPH_PRESERVE_SPAN_LIMIT {
            candidates.truncate(GROUNDED_ANSWER_GRAPH_PRESERVE_SPAN_LIMIT);
            return candidates;
        }
    }
    candidates
}

pub(super) fn push_grounded_answer_preserve_evidence_span(
    spans: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    evidence: &str,
) {
    if spans.len() >= GROUNDED_ANSWER_MUST_PRESERVE_SPAN_LIMIT {
        return;
    }
    let evidence = evidence.trim();
    if evidence.is_empty()
        || evidence.chars().count() > GROUNDED_ANSWER_MUST_PRESERVE_SPAN_MAX_CHARS
        || !evidence.chars().any(char::is_alphanumeric)
    {
        return;
    }
    if seen.insert(evidence.to_string()) {
        spans.push(evidence.to_string());
    }
}

pub(super) fn push_grounded_answer_preserve_source_title(
    spans: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    title: &str,
) {
    if spans.len() >= GROUNDED_ANSWER_MUST_PRESERVE_SPAN_LIMIT {
        return;
    }
    let title = title.trim();
    if title.is_empty()
        || title.chars().count() > GROUNDED_ANSWER_MUST_PRESERVE_SPAN_MAX_CHARS
        || !title.chars().any(char::is_alphanumeric)
    {
        return;
    }
    if seen.insert(title.to_string()) {
        spans.push(title.to_string());
    }
}

pub(super) fn push_grounded_answer_preserve_span(
    spans: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    span: &str,
) {
    let span = span.trim();
    if span.is_empty() {
        return;
    }
    if span.contains('\n') {
        let mut lines = span.lines();
        if let Some(first_line) = lines.next()
            && !is_probable_code_fence_info(first_line)
        {
            push_grounded_answer_preserve_line(spans, seen, first_line);
        }
        for line in lines {
            push_grounded_answer_preserve_line(spans, seen, line);
            if spans.len() >= GROUNDED_ANSWER_MUST_PRESERVE_SPAN_LIMIT {
                break;
            }
        }
        return;
    }
    push_grounded_answer_preserve_candidate(spans, seen, span);
}

pub(super) fn push_grounded_answer_preserve_line(
    spans: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    line: &str,
) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    push_grounded_answer_preserve_candidate(spans, seen, line);
    if let Some((left, right)) = line.split_once('=') {
        push_grounded_answer_preserve_candidate(spans, seen, left.trim());
        push_grounded_answer_preserve_candidate(spans, seen, right.trim());
    }
}

pub(super) fn push_grounded_answer_preserve_candidate(
    spans: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    candidate: &str,
) {
    if spans.len() >= GROUNDED_ANSWER_MUST_PRESERVE_SPAN_LIMIT {
        return;
    }
    let candidate = candidate.trim();
    if !is_grounded_answer_preserve_candidate(candidate) {
        return;
    }
    if seen.insert(candidate.to_string()) {
        spans.push(candidate.to_string());
    }
}

pub(super) fn is_grounded_answer_preserve_candidate(candidate: &str) -> bool {
    let candidate = candidate.trim();
    if candidate.is_empty()
        || candidate.chars().count() > GROUNDED_ANSWER_MUST_PRESERVE_SPAN_MAX_CHARS
        || !candidate.chars().any(char::is_alphanumeric)
    {
        return false;
    }
    if candidate.starts_with('/') || candidate.starts_with('\\') {
        return true;
    }
    if candidate.contains('/') || candidate.contains('\\') || candidate.contains("://") {
        return true;
    }
    if candidate.contains('=') {
        return candidate.split_once('=').is_some_and(|(left, right)| {
            !left.trim().is_empty()
                && !right.trim().is_empty()
                && left.trim().chars().any(char::is_alphanumeric)
        });
    }
    let unwrapped =
        candidate.trim_matches('[').trim_matches(']').trim_matches('`').trim_matches('"');
    literal_text_is_identifier_shaped(unwrapped) || is_plain_code_span(unwrapped)
}

pub(super) fn is_plain_code_span(candidate: &str) -> bool {
    let candidate = candidate.trim();
    if candidate.is_empty() || candidate.chars().any(char::is_whitespace) {
        return false;
    }
    let alnum_count = candidate.chars().filter(|ch| ch.is_alphanumeric()).count();
    alnum_count >= 2
        && candidate.chars().all(|ch| {
            ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '\\' | ':' | '=')
        })
}

pub(super) fn adjacent_code_span_assignments(text: &str) -> Vec<String> {
    let mut assignments = Vec::new();
    let mut seen = BTreeSet::new();
    let mut pending_key: Option<(String, usize)> = None;
    for (line_index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            pending_key = None;
            continue;
        }
        let spans = backtick_literal_spans(line);
        if spans.is_empty() {
            continue;
        }
        let keys = spans
            .iter()
            .map(String::as_str)
            .filter(|span| is_assignment_key_span(span))
            .collect::<Vec<_>>();
        let values = spans
            .iter()
            .map(String::as_str)
            .filter(|span| is_assignment_value_span(span))
            .collect::<Vec<_>>();
        append_pending_code_span_assignment(
            &mut assignments,
            &mut seen,
            pending_key.as_ref(),
            line_index,
            &keys,
            &values,
        );
        pending_key = current_code_span_assignment_key(
            &mut assignments,
            &mut seen,
            line_index,
            &keys,
            &values,
        )
        .or(pending_key);
    }
    assignments
}

pub(super) fn append_pending_code_span_assignment(
    assignments: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    pending_key: Option<&(String, usize)>,
    line_index: usize,
    keys: &[&str],
    values: &[&str],
) {
    let Some((key, key_line)) = pending_key else {
        return;
    };
    if keys.is_empty() && line_index.saturating_sub(*key_line) <= 6 && values.len() == 1 {
        push_adjacent_code_span_assignment(assignments, seen, key, values[0]);
    }
}

pub(super) fn current_code_span_assignment_key(
    assignments: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    line_index: usize,
    keys: &[&str],
    values: &[&str],
) -> Option<(String, usize)> {
    let [key] = keys else {
        return None;
    };
    if let [value] = values {
        push_adjacent_code_span_assignment(assignments, seen, key, value);
    }
    Some((key.trim().to_string(), line_index))
}

pub(super) fn push_adjacent_code_span_assignment(
    assignments: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    key: &str,
    value: &str,
) {
    let assignment = format!("{} = {}", key.trim(), value.trim());
    if seen.insert(assignment.clone()) {
        assignments.push(assignment);
    }
}

pub(super) fn is_assignment_key_span(span: &str) -> bool {
    let span = span.trim();
    if span.is_empty()
        || is_assignment_value_span(span)
        || span.starts_with('[')
        || span.starts_with('/')
        || span.starts_with('\\')
        || span.contains('/')
        || span.contains('\\')
        || span.contains("://")
        || span.contains('=')
    {
        return false;
    }
    let Some(first) = span.chars().next() else {
        return false;
    };
    first.is_alphabetic()
        && span.chars().all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        && literal_text_is_identifier_shaped(span)
}

pub(super) fn is_assignment_value_span(span: &str) -> bool {
    let span = span.trim();
    if span.is_empty() || span.chars().any(char::is_whitespace) {
        return false;
    }
    let lowered = span.to_ascii_lowercase();
    matches!(lowered.as_str(), "true" | "false")
        || span.contains("://")
        || span.starts_with('/')
        || span.starts_with('\\')
        || span.chars().all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | '-' | '+'))
}

pub(super) fn is_probable_code_fence_info(line: &str) -> bool {
    let line = line.trim();
    !line.is_empty()
        && line.chars().count() <= 32
        && line
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '+' | '.' | '#'))
}
