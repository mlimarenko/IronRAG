use super::coverage::query_focus_support_segments;
use super::output_boundary::looks_like_internal_effective_query_block;
use super::{
    AnswerVerificationStage, BTreeSet, HashSet, QueryAct, QueryIR, QueryScope,
    TechnicalLiteralIntent, detect_technical_literal_intent_from_query_ir,
    extract_config_assignment_literals, extract_config_section_literals,
    extract_explicit_path_literals, extract_http_methods, extract_parameter_literals,
    extract_prefix_literals, extract_url_literals,
};

pub(super) const EVIDENCE_LITERAL_RESTORATION_LIMIT: usize = 8;

pub(super) fn answer_omits_expected_technical_literals(
    answer: &str,
    query_ir: &QueryIR,
    grounding_context: &str,
) -> bool {
    let intent = detect_technical_literal_intent_from_query_ir("", query_ir);
    let omits_assignment_examples =
        answer_omits_focused_assignment_examples(answer, query_ir, grounding_context);
    if !intent.any() {
        return omits_assignment_examples;
    }
    let context_literals = collect_intended_technical_literals(grounding_context, intent, 8);
    if context_literals.is_empty() && !omits_assignment_examples {
        return false;
    }
    if answer_omits_focused_configuration_paths(answer, query_ir, grounding_context) {
        return true;
    }
    if answer_omits_focused_section_listing(answer, query_ir, grounding_context) {
        return true;
    }
    if answer_omits_focused_parameter_listing(answer, query_ir, grounding_context) {
        return true;
    }
    if omits_assignment_examples {
        return true;
    }
    collect_intended_technical_literals(answer, intent, 2).is_empty()
}

pub(super) fn answer_omits_focused_assignment_examples(
    answer: &str,
    query_ir: &QueryIR,
    grounding_context: &str,
) -> bool {
    if !matches!(query_ir.scope, QueryScope::SingleDocument)
        || !matches!(query_ir.act, QueryAct::ConfigureHow | QueryAct::Describe)
    {
        return false;
    }
    let expected = collect_context_config_assignment_literals(grounding_context, 24);
    if expected.len() < 2 {
        return false;
    }
    let actual = extract_config_assignment_literals(answer, expected.len().max(24))
        .into_iter()
        .collect::<HashSet<_>>();
    expected.iter().any(|literal| !actual.contains(literal))
}

pub(super) fn answer_omits_focused_configuration_paths(
    answer: &str,
    query_ir: &QueryIR,
    grounding_context: &str,
) -> bool {
    if !matches!(query_ir.act, QueryAct::ConfigureHow) {
        return false;
    }
    let expected = collect_focused_context_path_literals(grounding_context, query_ir, 8);
    if expected.is_empty() {
        return false;
    }
    let actual = collect_intended_technical_literals(
        answer,
        TechnicalLiteralIntent { wants_paths: true, ..TechnicalLiteralIntent::default() },
        expected.len().max(8),
    );
    expected.iter().any(|literal| !actual.contains(literal))
}

pub(super) fn answer_omits_focused_section_listing(
    answer: &str,
    query_ir: &QueryIR,
    grounding_context: &str,
) -> bool {
    if !matches!(query_ir.act, QueryAct::ConfigureHow) {
        return false;
    }
    let expected = collect_focused_context_section_literals(grounding_context, query_ir, 16);
    if expected.is_empty() {
        return false;
    }
    let actual = collect_intended_technical_literals(
        answer,
        TechnicalLiteralIntent { wants_parameters: true, ..TechnicalLiteralIntent::default() },
        expected.len().max(16),
    );
    expected.iter().any(|literal| !actual.contains(literal))
}

pub(super) fn answer_omits_focused_parameter_listing(
    answer: &str,
    query_ir: &QueryIR,
    grounding_context: &str,
) -> bool {
    if !matches!(query_ir.act, QueryAct::ConfigureHow) {
        return false;
    }
    let expected = collect_focused_context_parameter_literals(grounding_context, query_ir, 32);
    if expected.len() < 4 {
        return false;
    }
    let actual = collect_intended_technical_literals(
        answer,
        TechnicalLiteralIntent { wants_parameters: true, ..TechnicalLiteralIntent::default() },
        expected.len().max(32),
    );
    expected.iter().any(|literal| !actual.contains(literal))
}

pub(super) fn collect_focused_context_section_literals(
    grounding_context: &str,
    query_ir: &QueryIR,
    limit: usize,
) -> HashSet<String> {
    collect_focused_context_literals(
        grounding_context,
        query_ir,
        limit,
        "Sections:",
        extract_config_section_literals,
    )
}

pub(super) fn collect_focused_context_parameter_literals(
    grounding_context: &str,
    query_ir: &QueryIR,
    limit: usize,
) -> HashSet<String> {
    collect_focused_context_literals(
        grounding_context,
        query_ir,
        limit,
        "Parameters:",
        extract_parameter_literals,
    )
}

pub(super) fn collect_focused_context_path_literals(
    grounding_context: &str,
    query_ir: &QueryIR,
    limit: usize,
) -> HashSet<String> {
    collect_focused_context_literals(
        grounding_context,
        query_ir,
        limit,
        "Paths:",
        extract_explicit_path_literals,
    )
}

pub(super) fn collect_context_config_assignment_literals(
    grounding_context: &str,
    limit: usize,
) -> HashSet<String> {
    extract_config_assignment_literals(grounding_context, limit).into_iter().collect()
}

pub(super) fn collect_focused_context_literals(
    grounding_context: &str,
    query_ir: &QueryIR,
    limit: usize,
    label: &str,
    extractor: fn(&str, usize) -> Vec<String>,
) -> HashSet<String> {
    let focus_segments = query_focus_support_segments(query_ir);
    if focus_segments.is_empty() {
        return HashSet::new();
    }
    let mut literals = HashSet::<String>::new();
    let mut current_label: Option<String> = None;
    let mut current_body = String::new();
    for line in grounding_context.lines() {
        if let Some(document_label) = exact_technical_literal_document_label(line) {
            push_focused_context_literals(
                current_label.as_deref(),
                &FocusedLiteralExtraction {
                    body: &current_body,
                    focus_segments: &focus_segments,
                    limit,
                    label,
                    extractor,
                },
                &mut literals,
            );
            current_label = Some(document_label);
            current_body.clear();
            continue;
        }
        if current_label.is_some() {
            current_body.push_str(line);
            current_body.push('\n');
        }
    }
    push_focused_context_literals(
        current_label.as_deref(),
        &FocusedLiteralExtraction {
            body: &current_body,
            focus_segments: &focus_segments,
            limit,
            label,
            extractor,
        },
        &mut literals,
    );
    literals
}

/// The extraction parameters shared by every labelled context section:
/// the body to scan, the focus segments to match, the literal limit, the
/// section label and the literal extractor.
pub(super) struct FocusedLiteralExtraction<'a> {
    pub(super) body: &'a str,
    pub(super) focus_segments: &'a [BTreeSet<String>],
    pub(super) limit: usize,
    pub(super) label: &'a str,
    pub(super) extractor: fn(&str, usize) -> Vec<String>,
}

pub(super) fn push_focused_context_literals(
    document_label: Option<&str>,
    extraction: &FocusedLiteralExtraction<'_>,
    out: &mut HashSet<String>,
) {
    let Some(document_label) = document_label else {
        return;
    };
    if out.len() >= extraction.limit
        || !document_label_matches_focus(document_label, extraction.focus_segments)
    {
        return;
    }
    let literal_text = collect_focused_literal_candidate_text(extraction.body, extraction.label);
    push_focused_literals_from_text(&literal_text, extraction.limit, extraction.extractor, out);
}

pub(super) fn push_focused_literals_from_text(
    text: &str,
    limit: usize,
    extractor: fn(&str, usize) -> Vec<String>,
    out: &mut HashSet<String>,
) {
    let remaining = limit.saturating_sub(out.len());
    if remaining == 0 {
        return;
    }
    for literal in extractor(text, remaining) {
        out.insert(literal);
        if out.len() >= limit {
            return;
        }
    }
}

pub(super) fn collect_labelled_literal_inventory(body: &str, label: &str) -> String {
    let mut lines = Vec::new();
    let mut source_lines = body.lines().map(str::trim);
    for line in source_lines.by_ref() {
        let Some(rest) = line.strip_prefix(label) else {
            continue;
        };
        lines.push(line.to_string());
        let suffix = rest.trim();
        if !suffix.is_empty() {
            lines.push(suffix.to_string());
        }
        break;
    }
    lines.extend(
        source_lines.take_while(|line| labelled_inventory_line_continues(line)).map(str::to_string),
    );
    lines.join("\n")
}

pub(super) fn labelled_inventory_line_continues(line: &str) -> bool {
    line.is_empty() || line.starts_with("- `")
}

pub(super) fn collect_focused_literal_candidate_text(body: &str, label: &str) -> String {
    let mut lines = Vec::new();
    let labelled = collect_labelled_literal_inventory(body, label);
    if !labelled.is_empty() {
        lines.push(labelled);
    }
    if label == "Parameters:" {
        for line in body.lines().map(str::trim) {
            if line.is_empty() {
                continue;
            }
            if let Some(table_row) = line.strip_prefix("- table_row ") {
                push_structured_parameter_table_row(table_row, &mut lines);
                continue;
            }
        }
    }
    lines.join("\n")
}

pub(super) fn push_structured_parameter_table_row(row: &str, out: &mut Vec<String>) {
    let Some((_, cells)) = row.split_once(": ") else {
        return;
    };
    let candidate_cells =
        cells.split('|').map(str::trim).filter(|cell| !cell.is_empty()).collect::<Vec<_>>();
    if candidate_cells.len() < 2 {
        return;
    }
    let first = candidate_cells[0];
    if first.chars().any(char::is_whitespace) {
        return;
    }
    if crate::domains::query_ir::literal_text_is_identifier_shaped(first) {
        out.push(first.to_string());
    }
}

pub(super) fn exact_technical_literal_document_label(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix("- Document: `")?;
    let (label, _) = rest.split_once('`')?;
    let label = label.trim();
    (!label.is_empty()).then(|| label.to_string())
}

pub(super) fn document_label_matches_focus(
    label: &str,
    focus_segments: &[BTreeSet<String>],
) -> bool {
    let label_tokens = crate::services::query::text_match::normalized_alnum_tokens(label, 4);
    focus_segments.iter().any(|segment| {
        let overlap =
            crate::services::query::text_match::near_token_overlap_count(segment, &label_tokens);
        overlap >= segment.len().min(2)
    })
}

pub(super) fn collect_intended_technical_literals(
    text: &str,
    intent: TechnicalLiteralIntent,
    limit: usize,
) -> HashSet<String> {
    let mut literals = HashSet::<String>::new();
    if intent.wants_urls {
        literals.extend(extract_url_literals(text, limit));
    }
    if intent.wants_prefixes {
        literals.extend(extract_prefix_literals(text, limit));
    }
    if intent.wants_paths {
        literals.extend(extract_explicit_path_literals(text, limit));
    }
    if intent.wants_methods {
        literals.extend(extract_http_methods(text, limit));
    }
    if intent.wants_parameters {
        literals.extend(extract_config_section_literals(text, limit));
        literals.extend(extract_parameter_literals(text, limit));
    }
    literals
}

pub(super) fn answer_needs_literal_revision(verification: &AnswerVerificationStage) -> bool {
    verification.verification.warnings.iter().any(|warning| {
        warning.code == "unsupported_literal" || warning.code == "unsupported_\u{63}anonical_claim"
    })
}

pub(super) fn answer_has_partial_coverage_warning(verification: &AnswerVerificationStage) -> bool {
    verification.verification.warnings.iter().any(|warning| {
        matches!(warning.code.as_str(), "partial_coverage" | "variant_coverage_incomplete")
    })
}

pub(crate) fn literal_revision_targets(
    answer: &str,
    unsupported_literals: &[String],
) -> Vec<String> {
    if unsupported_literals.is_empty() {
        return Vec::new();
    }
    let mut targets = unsupported_literals.to_vec();
    let mut seen = targets.iter().cloned().collect::<HashSet<_>>();
    for block in fenced_code_blocks(answer) {
        if unsupported_literals.iter().any(|literal| fenced_block_contains_literal(&block, literal))
            && seen.insert(block.clone())
        {
            targets.push(block);
        }
    }
    targets
}

pub(super) fn fenced_code_blocks(answer: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let bytes = answer.as_bytes();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if !bytes[cursor..].starts_with(b"```") {
            cursor += utf8_char_len(bytes[cursor]);
            continue;
        }
        let body_start = cursor + 3;
        let Some(relative_end) = find_subslice(&bytes[body_start..], b"```") else {
            break;
        };
        let body_end = body_start + relative_end;
        let body = answer[body_start..body_end]
            .strip_prefix('\n')
            .or_else(|| answer[body_start..body_end].strip_prefix("\r\n"))
            .unwrap_or(&answer[body_start..body_end]);
        let mut lines = body.lines().collect::<Vec<_>>();
        if lines.first().is_some_and(|line| is_fenced_language_hint(line.trim())) {
            lines.remove(0);
        }
        let normalized = lines
            .into_iter()
            .map(|line| line.trim_end_matches('\r'))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();
        if !normalized.is_empty() {
            blocks.push(normalized);
        }
        cursor = body_end + 3;
    }
    blocks
}

pub(super) fn fenced_block_contains_literal(block: &str, literal: &str) -> bool {
    let literal = literal.trim();
    !literal.is_empty()
        && block.lines().map(str::trim).any(|line| line == literal || line.contains(literal))
}

pub(super) fn is_fenced_language_hint(candidate: &str) -> bool {
    !candidate.is_empty()
        && candidate.chars().count() <= 20
        && candidate
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '+' | '_' | '.'))
}

pub(super) fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|window| window == needle)
}

pub(super) fn utf8_char_len(byte: u8) -> usize {
    if byte < 0x80 {
        1
    } else if byte < 0xE0 {
        2
    } else if byte < 0xF0 {
        3
    } else {
        4
    }
}

pub(super) fn answer_question_for_disposition<'a>(
    effective_question: &'a str,
    user_question: &'a str,
) -> &'a str {
    let effective_question = effective_question.trim();
    if effective_question.is_empty() { user_question } else { effective_question }
}

pub(super) fn requires_no_evidence_candidate(
    has_no_preflight_candidate: bool,
    attempted_answer_generation: bool,
) -> bool {
    has_no_preflight_candidate && !attempted_answer_generation
}

pub(super) fn answer_generation_question<'a>(
    effective_question: &'a str,
    user_question: &'a str,
) -> &'a str {
    let user_question = user_question.trim();
    if !user_question.is_empty() {
        return user_question;
    }
    effective_question.trim()
}

pub(super) fn literal_revision_can_replace_answer(
    _draft_answer: &str,
    revision_answer: &str,
) -> bool {
    let revision = revision_answer.trim();
    if revision.is_empty() {
        return false;
    }
    if looks_like_internal_effective_query_block(revision) {
        return false;
    }
    true
}
