use super::*;

pub(super) fn grounded_answer_passthrough_question(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n").trim().to_string()
}

pub(super) fn verified_grounded_answer_guard_candidate<'a>(
    last_verified_grounded_answer: Option<&'a str>,
    verified_grounded_answer_guard_text: Option<&'a str>,
    verified_grounded_answer_count: usize,
    successful_tool_call_count: usize,
) -> Option<&'a str> {
    verified_grounded_answer_fallback_candidate(
        last_verified_grounded_answer,
        verified_grounded_answer_count,
        successful_tool_call_count,
    )
    .or({
        if verified_grounded_answer_count > 1 { verified_grounded_answer_guard_text } else { None }
    })
}

pub(super) fn prefer_verified_grounded_answer_on_ordered_source_loss(
    answer: String,
    verified_grounded_answer: Option<&str>,
    request_id: &str,
    library_id: Uuid,
) -> String {
    let Some(verified_grounded_answer) =
        verified_grounded_answer.map(str::trim).filter(|value| !value.is_empty())
    else {
        return answer;
    };
    let grounded_marker_count = ordered_source_marker_count(verified_grounded_answer);
    if grounded_marker_count < 2 {
        return answer;
    }
    if ordered_source_marker_count(&answer) >= grounded_marker_count
        || answer_ordered_item_count(&answer) >= grounded_marker_count
    {
        return answer;
    }
    tracing::debug!(
        request_id,
        library_id = %library_id,
        "query.agent_loop.verified_grounded_ordered_source_guard"
    );
    verified_grounded_answer.to_string()
}

pub(super) fn ordered_source_marker_count(answer: &str) -> usize {
    answer
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("source=`") || trimmed.contains(" source=`")
        })
        .count()
}

pub(super) fn prefer_verified_grounded_answer_on_unverified_procedure_rewrite(
    answer: String,
    verified_grounded_answer: Option<&str>,
    request_id: &str,
    library_id: Uuid,
) -> String {
    let Some(verified_grounded_answer) =
        verified_grounded_answer.map(str::trim).filter(|value| !value.is_empty())
    else {
        return answer;
    };
    if !answer_rewrites_unverified_procedure(&answer, verified_grounded_answer) {
        return answer;
    }
    tracing::debug!(
        request_id,
        library_id = %library_id,
        "query.agent_loop.verified_grounded_procedure_rewrite_guard"
    );
    verified_grounded_answer.to_string()
}

pub(super) fn answer_rewrites_unverified_procedure(answer: &str, grounded: &str) -> bool {
    if grounded_answer_passthrough_question(answer)
        == grounded_answer_passthrough_question(grounded)
    {
        return false;
    }

    let grounded_step_count = answer_ordered_item_count(grounded);
    grounded_step_count >= 2
        && ordered_source_marker_count(grounded) < 2
        && answer_ordered_item_count(answer) != grounded_step_count
}

pub(super) fn prefer_verified_grounded_answer_on_literal_drift(
    answer: String,
    user_question: &str,
    verified_grounded_answer: Option<&str>,
    request_id: &str,
    library_id: Uuid,
) -> String {
    let Some(verified_grounded_answer) =
        verified_grounded_answer.map(str::trim).filter(|value| !value.is_empty())
    else {
        return answer;
    };

    let grounded_literals = verified_grounded_answer_literal_set(verified_grounded_answer);
    if grounded_literals.len() < VERIFIED_GROUNDED_LITERAL_GUARD_MIN_LITERALS {
        return prefer_verified_grounded_answer_on_surface_anchor_loss(
            answer,
            verified_grounded_answer,
            request_id,
            library_id,
        );
    }
    let answer_literals = verified_grounded_answer_literal_set(&answer);
    let user_literals = verified_grounded_answer_literal_set(user_question);
    let missing_count = grounded_literals.difference(&answer_literals).count();
    let unsupported_count = answer_literals
        .difference(&grounded_literals)
        .filter(|literal| !user_literals.contains(*literal))
        .count();
    if missing_count == 0 && unsupported_count == 0 {
        return prefer_verified_grounded_answer_on_surface_anchor_loss(
            answer,
            verified_grounded_answer,
            request_id,
            library_id,
        );
    }

    tracing::debug!(
        request_id,
        library_id = %library_id,
        grounded_literal_count = grounded_literals.len(),
        answer_literal_count = answer_literals.len(),
        missing_count,
        unsupported_count,
        "query.agent_loop.verified_grounded_literal_guard"
    );
    verified_grounded_answer.to_string()
}

pub(super) fn prefer_verified_grounded_answer_on_surface_anchor_loss(
    answer: String,
    verified_grounded_answer: &str,
    request_id: &str,
    library_id: Uuid,
) -> String {
    let grounded_anchors = verified_grounded_answer_surface_anchor_set(verified_grounded_answer);
    if grounded_anchors.len() < VERIFIED_GROUNDED_LITERAL_GUARD_MIN_LITERALS {
        return answer;
    }
    let answer_anchors = verified_grounded_answer_surface_anchor_set(&answer);
    let high_signal_missing_count = grounded_anchors
        .iter()
        .filter(|anchor| is_high_signal_grounded_answer_anchor(anchor))
        .filter(|anchor| {
            !answer_anchors.contains(*anchor) && !answer_contains_guard_anchor(&answer, anchor)
        })
        .count();
    if high_signal_missing_count >= GROUNDED_EVIDENCE_LEDGER_GUARD_MIN_MISSING_HIGH_SIGNAL_ANCHORS {
        log_verified_grounded_high_signal_anchor_guard(
            request_id,
            library_id,
            grounded_anchors.len(),
            answer_anchors.len(),
            high_signal_missing_count,
        );
        return verified_grounded_answer.to_string();
    }
    let missing_count = grounded_anchors
        .iter()
        .filter(|anchor| {
            !answer_anchors.contains(*anchor) && !answer_contains_guard_anchor(&answer, anchor)
        })
        .count();
    let missing_threshold = grounded_anchors.len().div_ceil(3).max(2);
    if missing_count < missing_threshold {
        return answer;
    }

    tracing::debug!(
        request_id,
        library_id = %library_id,
        grounded_anchor_count = grounded_anchors.len(),
        answer_anchor_count = answer_anchors.len(),
        missing_count,
        missing_threshold,
        "query.agent_loop.verified_grounded_surface_anchor_guard"
    );
    verified_grounded_answer.to_string()
}

fn log_verified_grounded_high_signal_anchor_guard(
    request_id: &str,
    library_id: Uuid,
    grounded_anchor_count: usize,
    answer_anchor_count: usize,
    high_signal_missing_count: usize,
) {
    tracing::debug!(
        request_id,
        library_id = %library_id,
        grounded_anchor_count,
        answer_anchor_count,
        high_signal_missing_count,
        "query.agent_loop.verified_grounded_high_signal_anchor_guard"
    );
}

pub(super) fn verified_grounded_answer_literal_set(text: &str) -> BTreeSet<String> {
    let mut literals = BTreeSet::new();
    for span in backtick_literal_spans(text) {
        push_verified_grounded_answer_literal_span(&mut literals, &span);
    }
    literals
}

pub(super) fn verified_grounded_answer_surface_anchor_set(text: &str) -> BTreeSet<String> {
    let mut anchors = verified_grounded_answer_literal_set(text);
    for candidate in split_literal_anchor_candidates(text) {
        let candidate = trim_literal_anchor_candidate(&candidate);
        if is_structural_literal_anchor(candidate) {
            anchors.insert(candidate.to_string());
        }
    }
    anchors
}

pub(super) fn push_verified_grounded_answer_literal_span(
    literals: &mut BTreeSet<String>,
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
            push_verified_grounded_answer_literal_line(literals, first_line);
        }
        for line in lines {
            push_verified_grounded_answer_literal_line(literals, line);
        }
        return;
    }
    push_verified_grounded_answer_literal_candidate(literals, span);
}

pub(super) fn is_probable_code_fence_info(line: &str) -> bool {
    let line = line.trim();
    !line.is_empty()
        && line.chars().count() <= 32
        && line
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '+' | '.' | '#'))
}

pub(super) fn push_verified_grounded_answer_literal_line(
    literals: &mut BTreeSet<String>,
    line: &str,
) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    push_verified_grounded_answer_literal_candidate(literals, line);
    if let Some((left, right)) = line.split_once('=') {
        push_verified_grounded_answer_literal_candidate(literals, left.trim());
        push_verified_grounded_answer_literal_candidate(literals, right.trim());
    }
}

pub(super) fn push_verified_grounded_answer_literal_candidate(
    literals: &mut BTreeSet<String>,
    candidate: &str,
) {
    let candidate = candidate.trim();
    if candidate.is_empty() {
        return;
    }
    if is_structural_literal_anchor(candidate) || is_plain_code_literal(candidate) {
        literals.insert(candidate.to_string());
    }
}

pub(super) fn is_identifier_shaped_fragment(candidate: &str) -> bool {
    let char_count = candidate.chars().count();
    if !(STRUCTURAL_IDENTIFIER_MIN_CHARS..=STRUCTURAL_IDENTIFIER_MAX_CHARS).contains(&char_count) {
        return false;
    }
    let alnum_count = candidate.chars().filter(|ch| ch.is_alphanumeric()).count();
    if alnum_count < 2 {
        return false;
    }
    let has_underscore = candidate.contains('_');
    let has_hyphen = candidate.contains('-');
    let has_dot = candidate.contains('.');
    let has_slash = candidate.contains('/');
    if !(has_underscore || has_hyphen || has_dot || has_slash) {
        return false;
    }
    if has_underscore {
        return true;
    }
    if has_slash {
        return candidate.starts_with('/')
            || has_dot
            || candidate.chars().any(|ch| ch.is_numeric());
    }
    let has_digit = candidate.chars().any(|ch| ch.is_numeric());
    let has_upper = candidate.chars().any(|ch| ch.is_uppercase());
    let has_lower = candidate.chars().any(|ch| ch.is_lowercase());
    let all_cased_upper = has_upper && !has_lower;
    if has_hyphen {
        return has_digit || all_cased_upper;
    }
    has_dot && (has_digit || all_cased_upper)
}
