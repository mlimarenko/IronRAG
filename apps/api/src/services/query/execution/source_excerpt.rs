use std::collections::BTreeSet;

use crate::shared::{
    extraction::text_render::repair_technical_layout_noise,
    structural_declarations::{
        declaration_block_end, declaration_has_indented_body, declaration_key_matches_focus,
        line_indentation_width, structural_declaration_has_indented_body,
        structural_declaration_line_has_shape, structural_declaration_line_matches_focus,
        structural_declaration_surface_is_eligible,
    },
};

use super::{
    retrieve::excerpt_for,
    technical_literals::{
        extract_config_assignment_literals, extract_config_section_literals,
        extract_explicit_path_literals, extract_package_command_literals,
        extract_parameter_literals,
    },
};

const EXCERPT_GAP_MARKER: &str = "...";
const EXCERPT_LINE_BREAK_CHARS: usize = 1;

#[derive(Clone, Copy)]
enum ExcerptBudget {
    Compact,
    Rendered,
}

pub(super) fn structured_literal_excerpt_for(
    content: &str,
    keywords: &[String],
    max_chars: usize,
) -> Option<String> {
    let repaired = repair_technical_layout_noise(content);
    let lines = excerpt_source_lines(&repaired, max_chars, true)?;
    let normalized_keywords = normalized_excerpt_keywords(keywords);
    let focused_declaration_lines =
        select_focused_declaration_line_indexes(&lines, &normalized_keywords, max_chars);
    if focused_declaration_lines.is_empty() {
        let compact_lines = excerpt_source_lines(&repaired, max_chars, false)?;
        let scored = score_structured_excerpt_lines(&compact_lines, &normalized_keywords);
        let selected = select_excerpt_line_indexes(
            &compact_lines,
            scored,
            BTreeSet::new(),
            ExcerptSelectionLimits {
                max_chars,
                candidate_limit: 24,
                budget: ExcerptBudget::Compact,
            },
            |line| {
                structured_literal_line_score(line) > 0
                    || line_is_short_context_label(line, &normalized_keywords)
            },
        );
        return render_selected_excerpt(&compact_lines, selected, max_chars);
    }
    let scored = score_structured_excerpt_lines(&lines, &normalized_keywords);
    let selected = select_excerpt_line_indexes(
        &lines,
        scored,
        focused_declaration_lines,
        ExcerptSelectionLimits { max_chars, candidate_limit: 24, budget: ExcerptBudget::Rendered },
        |line| {
            structured_literal_line_score(line) > 0
                || line_is_short_context_label(line, &normalized_keywords)
        },
    );
    render_selected_excerpt(&lines, selected, max_chars)
}

fn excerpt_source_lines(
    content: &str,
    max_chars: usize,
    preserve_indentation: bool,
) -> Option<Vec<&str>> {
    if max_chars == 0 {
        return None;
    }
    let lines: Vec<_> = content
        .trim_matches('\n')
        .lines()
        .map(|line| if preserve_indentation { line.trim_end() } else { line.trim() })
        .filter(|line| !line.trim().is_empty())
        .collect();
    (!lines.is_empty()).then_some(lines)
}

pub(super) fn focused_declaration_depth(content: &str, keywords: &[String]) -> Option<usize> {
    let normalized_keywords = normalized_excerpt_keywords(keywords);
    let lines = content.lines().collect::<Vec<_>>();
    lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let depth = matching_declaration_depth(line, &normalized_keywords)?;
            let end = declaration_block_end(&lines, index, depth);
            declaration_has_indented_body(&lines, index, end, depth).then_some(depth)
        })
        .min()
}

pub(super) fn focused_declaration_reach_score(
    content: &str,
    keywords: &[String],
    document_start_offset: usize,
) -> usize {
    let normalized_keywords = normalized_excerpt_keywords(keywords);
    let lines = content.lines().collect::<Vec<_>>();
    let mut line_offset = 0usize;
    let mut best_score = 0usize;
    for (index, line) in lines.iter().enumerate() {
        if structural_declaration_line_matches_focus(line, &normalized_keywords)
            && structural_declaration_has_indented_body(&lines, index)
        {
            best_score =
                best_score.max(document_start_offset.saturating_add(line_offset).saturating_add(1));
        }
        line_offset = line_offset.saturating_add(line.len()).saturating_add(1);
    }
    best_score
}

pub(super) fn top_level_declaration_signatures(content: &str) -> Vec<String> {
    let repaired = repair_technical_layout_noise(content);
    let lines = repaired.lines().collect::<Vec<_>>();
    lines
        .iter()
        .enumerate()
        .filter(|(index, line)| {
            line_indentation_width(line) == 0
                && structural_declaration_line_has_shape(line)
                && structural_declaration_has_indented_body(&lines, *index)
        })
        .map(|(_, line)| line.trim_end().to_string())
        .collect()
}

fn select_focused_declaration_line_indexes(
    lines: &[&str],
    normalized_keywords: &[String],
    max_chars: usize,
) -> BTreeSet<usize> {
    let mut declarations = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let depth = matching_declaration_depth(line, normalized_keywords)?;
            let end = declaration_block_end(lines, index, depth);
            declaration_has_indented_body(lines, index, end, depth).then_some((depth, index, end))
        })
        .collect::<Vec<_>>();
    declarations.sort_by(|(left_depth, left_index, _), (right_depth, right_index, _)| {
        left_depth.cmp(right_depth).then_with(|| left_index.cmp(right_index))
    });

    let mut selected = BTreeSet::new();
    for (_, index, end) in declarations {
        try_select_excerpt_block(lines, index, end, max_chars, &mut selected);
    }
    selected
}

fn matching_declaration_depth(line: &str, normalized_keywords: &[String]) -> Option<usize> {
    let key = structural_declaration_key(line)?;
    declaration_key_matches_focus(key, normalized_keywords).then_some(line_indentation_width(line))
}

fn structural_declaration_key(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    let key = trimmed.strip_suffix(':')?.trim();
    structural_declaration_surface_is_eligible(key).then(|| key.trim_matches(['\'', '"']))
}

fn score_structured_excerpt_lines(
    lines: &[&str],
    normalized_keywords: &[String],
) -> Vec<(usize, usize)> {
    let mut scored = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let literal_score = structured_literal_line_score(line);
            if literal_score == 0 {
                return None;
            }
            let lowered = line.to_lowercase();
            let keyword_score = normalized_keywords
                .iter()
                .filter(|keyword| lowered.contains(keyword.as_str()))
                .map(|keyword| keyword.chars().count().min(24))
                .sum::<usize>();
            Some((literal_score.saturating_mul(100).saturating_add(keyword_score), index))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|(left_score, left_index), (right_score, right_index)| {
        right_score.cmp(left_score).then_with(|| left_index.cmp(right_index))
    });
    scored
}

/// How much of one excerpt the line selector may keep.
struct ExcerptSelectionLimits {
    max_chars: usize,
    candidate_limit: usize,
    budget: ExcerptBudget,
}

fn select_excerpt_line_indexes(
    lines: &[&str],
    scored: Vec<(usize, usize)>,
    mut selected: BTreeSet<usize>,
    limits: ExcerptSelectionLimits,
    neighbor_is_relevant: impl Fn(&str) -> bool,
) -> BTreeSet<usize> {
    let ExcerptSelectionLimits { max_chars, candidate_limit, budget } = limits;
    for (_, index) in scored.into_iter().take(candidate_limit) {
        try_select_excerpt_line(lines, index, max_chars, budget, &mut selected);
        for neighbor in adjacent_line_indexes(index, lines.len()) {
            if neighbor_is_relevant(lines[neighbor]) {
                try_select_excerpt_line(lines, neighbor, max_chars, budget, &mut selected);
            }
        }
    }
    selected
}

fn adjacent_line_indexes(index: usize, line_count: usize) -> impl Iterator<Item = usize> {
    [index.checked_sub(1), index.checked_add(1).filter(|neighbor| *neighbor < line_count)]
        .into_iter()
        .flatten()
}

fn try_select_excerpt_line(
    lines: &[&str],
    index: usize,
    max_chars: usize,
    budget: ExcerptBudget,
    selected: &mut BTreeSet<usize>,
) {
    let mut prospective = selected.clone();
    if !prospective.insert(index) {
        return;
    }
    if selected_excerpt_char_count(lines, &prospective, budget) > max_chars && !selected.is_empty()
    {
        return;
    }
    *selected = prospective;
}

fn try_select_excerpt_block(
    lines: &[&str],
    start: usize,
    end: usize,
    max_chars: usize,
    selected: &mut BTreeSet<usize>,
) {
    let mut prospective = selected.clone();
    prospective.extend(start..end);
    if selected_excerpt_char_count(lines, &prospective, ExcerptBudget::Rendered) <= max_chars {
        *selected = prospective;
        return;
    }
    if selected.is_empty() {
        for index in start..end {
            try_select_excerpt_line(lines, index, max_chars, ExcerptBudget::Rendered, selected);
        }
    }
}

fn selected_excerpt_char_count(
    lines: &[&str],
    selected: &BTreeSet<usize>,
    budget: ExcerptBudget,
) -> usize {
    if matches!(budget, ExcerptBudget::Compact) {
        return selected.iter().fold(0usize, |chars, index| {
            chars.saturating_add(lines[*index].chars().count()).saturating_add(1)
        });
    }
    let mut chars = 0usize;
    let mut previous = None;
    for index in selected {
        if let Some(previous_index) = previous {
            chars = chars.saturating_add(EXCERPT_LINE_BREAK_CHARS);
            if previous_index + 1 < *index {
                chars = chars
                    .saturating_add(EXCERPT_GAP_MARKER.chars().count())
                    .saturating_add(EXCERPT_LINE_BREAK_CHARS);
            }
        }
        chars = chars.saturating_add(lines[*index].chars().count());
        previous = Some(*index);
    }
    chars
}

fn render_selected_excerpt(
    lines: &[&str],
    selected: BTreeSet<usize>,
    max_chars: usize,
) -> Option<String> {
    if selected.is_empty() {
        return None;
    }
    let mut excerpt_lines = Vec::new();
    let mut previous = None;
    for index in selected {
        if previous.is_some_and(|previous| previous + 1 < index) {
            excerpt_lines.push(EXCERPT_GAP_MARKER);
        }
        excerpt_lines.push(lines[index]);
        previous = Some(index);
    }
    let excerpt = excerpt_for(&excerpt_lines.join("\n"), max_chars);
    (!excerpt.trim().is_empty()).then_some(excerpt)
}

pub(super) fn salient_source_excerpt_for(
    content: &str,
    keywords: &[String],
    max_chars: usize,
) -> Option<String> {
    let repaired = repair_technical_layout_noise(content);
    let lines = excerpt_source_lines(&repaired, max_chars, false)?;
    let normalized_keywords = normalized_excerpt_keywords(keywords);
    let scored = score_salient_excerpt_lines(&lines, &normalized_keywords);
    let selected = select_excerpt_line_indexes(
        &lines,
        scored,
        BTreeSet::new(),
        ExcerptSelectionLimits { max_chars, candidate_limit: 32, budget: ExcerptBudget::Compact },
        |line| source_local_evidence_line_score(line, &normalized_keywords) > 0,
    );
    render_selected_excerpt(&lines, selected, max_chars)
}

fn score_salient_excerpt_lines(
    lines: &[&str],
    normalized_keywords: &[String],
) -> Vec<(usize, usize)> {
    let mut scored = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let score = source_local_evidence_line_score(line, normalized_keywords);
            (score > 0).then_some((score, index))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|(left_score, left_index), (right_score, right_index)| {
        right_score.cmp(left_score).then_with(|| left_index.cmp(right_index))
    });
    scored
}

pub(super) fn source_local_evidence_line_score(line: &str, keywords: &[String]) -> usize {
    let line = line.trim();
    if line.is_empty() {
        return 0;
    }
    let lowered = line.to_lowercase();
    let keyword_score = keywords
        .iter()
        .map(|keyword| keyword.trim())
        .filter(|keyword| keyword.chars().count() >= 3)
        .filter(|keyword| lowered.contains(*keyword))
        .map(|keyword| keyword.chars().count().min(24))
        .sum::<usize>();
    let structural_score = structured_literal_line_score(line);
    let surface_score = source_local_salience_score(line);
    if structural_score == 0 && surface_score == 0 && keyword_score == 0 {
        return 0;
    }
    structural_score
        .saturating_mul(100)
        .saturating_add(surface_score.saturating_mul(10))
        .saturating_add(keyword_score)
}

fn normalized_excerpt_keywords(keywords: &[String]) -> Vec<String> {
    keywords
        .iter()
        .map(|keyword| keyword.trim())
        .filter(|keyword| keyword.chars().count() >= 3)
        .map(|keyword| keyword.to_lowercase())
        .collect()
}

pub(super) fn structured_literal_line_score(line: &str) -> usize {
    extract_config_assignment_literals(line, 4).len().saturating_mul(8)
        + extract_config_section_literals(line, 4).len().saturating_mul(6)
        + extract_explicit_path_literals(line, 4).len().saturating_mul(5)
        + extract_package_command_literals(line, 2).len().saturating_mul(5)
        + extract_parameter_literals(line, 8).len().saturating_mul(3)
        + usize::from(line_has_key_value_literal_surface(line)).saturating_mul(3)
        + usize::from(line_has_table_like_literal_surface(line)).saturating_mul(2)
}

fn line_has_table_like_literal_surface(line: &str) -> bool {
    let alphanumeric_count = line.chars().filter(|ch| ch.is_alphanumeric()).count();
    alphanumeric_count >= 3
        && (line.matches('|').count() >= 2
            || line.split('\t').filter(|cell| !cell.trim().is_empty()).count() >= 3)
}

fn source_local_salience_score(line: &str) -> usize {
    let line = line.trim();
    let alphanumeric_count = line.chars().filter(|ch| ch.is_alphanumeric()).count();
    if alphanumeric_count < 3 {
        return 0;
    }
    usize::from(line_has_list_marker_surface(line)).saturating_mul(4)
        + usize::from(line_has_label_value_surface(line)).saturating_mul(4)
        + usize::from(line_has_identifier_token_surface(line)).saturating_mul(3)
        + usize::from(line_has_bracket_or_quote_surface(line)).saturating_mul(2)
        + usize::from(line_has_numeric_marker_surface(line)).saturating_mul(2)
        + usize::from(line_has_compact_fact_surface(line)).saturating_mul(1)
}

fn line_has_list_marker_surface(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
        return true;
    }
    let mut chars = trimmed.chars().peekable();
    let mut digits = 0usize;
    while chars.peek().is_some_and(|ch| ch.is_ascii_digit()) {
        digits = digits.saturating_add(1);
        chars.next();
    }
    digits > 0 && chars.next().is_some_and(|ch| matches!(ch, '.' | ')' | ':'))
}

fn line_has_label_value_surface(line: &str) -> bool {
    let Some((left, right)) =
        line.split_once(':').or_else(|| line.split_once(" - ")).or_else(|| line.split_once(" | "))
    else {
        return false;
    };
    let left = left.trim().trim_start_matches(['-', '*', '+']).trim();
    let right = right.trim();
    !left.is_empty()
        && !right.is_empty()
        && left.chars().count() <= 120
        && left.chars().any(|ch| ch.is_alphanumeric())
        && right.chars().any(|ch| ch.is_alphanumeric())
}

fn line_has_identifier_token_surface(line: &str) -> bool {
    line.split_whitespace().any(token_has_identifier_surface)
}

fn token_has_identifier_surface(token: &str) -> bool {
    let token = token.trim_matches(|ch: char| {
        ch.is_whitespace()
            || matches!(ch, ',' | ';' | '"' | '\'' | '`' | '(' | ')' | '{' | '}' | '<' | '>')
    });
    let mut alphanumeric_count = 0usize;
    let mut has_alpha = false;
    let mut has_digit = false;
    let mut has_connector = false;
    for ch in token.chars() {
        if ch.is_alphanumeric() {
            alphanumeric_count = alphanumeric_count.saturating_add(1);
            has_alpha |= ch.is_alphabetic();
            has_digit |= ch.is_ascii_digit();
        } else if matches!(ch, '_' | '-' | '.' | '/' | ':' | '#' | '@' | '[' | ']') {
            has_connector = true;
        }
    }
    alphanumeric_count >= 3 && (has_connector || (has_alpha && has_digit))
}

fn line_has_bracket_or_quote_surface(line: &str) -> bool {
    (line.contains('[') && line.contains(']'))
        || (line.contains('(') && line.contains(')'))
        || (line.contains('"') && line.matches('"').count() >= 2)
        || (line.contains('`') && line.matches('`').count() >= 2)
}

fn line_has_numeric_marker_surface(line: &str) -> bool {
    let has_digit = line.chars().any(|ch| ch.is_ascii_digit());
    has_digit && line.chars().any(|ch| matches!(ch, ':' | '.' | '-' | '/' | '#'))
}

fn line_has_compact_fact_surface(line: &str) -> bool {
    let char_count = line.chars().count();
    if !(24..=240).contains(&char_count) {
        return false;
    }
    let alphanumeric_count = line.chars().filter(|ch| ch.is_alphanumeric()).count();
    alphanumeric_count >= 12
        && line.chars().any(|ch| matches!(ch, ':' | ';' | '.' | '!' | '?' | '|' | ')' | ']'))
}

fn line_is_short_context_label(line: &str, normalized_keywords: &[String]) -> bool {
    let char_count = line.chars().count();
    char_count <= 160
        && (line.ends_with(':')
            || line.starts_with('#')
            || normalized_keywords
                .iter()
                .any(|keyword| line.to_lowercase().contains(keyword.as_str())))
}

pub(super) fn line_has_key_value_literal_surface(line: &str) -> bool {
    let Some((key, value)) = line.split_once('=') else {
        return false;
    };
    let key = key.trim().trim_start_matches(['-', '*']).trim();
    let value = value.trim();
    key.chars().any(|ch| ch.is_alphabetic())
        && key.chars().filter(|ch| ch.is_alphanumeric() || *ch == '_' || *ch == '-').count() >= 2
        && !value.is_empty()
        && !key.contains("==")
}

#[cfg(test)]
mod tests {
    use super::{
        focused_declaration_reach_score, structured_literal_excerpt_for,
        top_level_declaration_signatures,
    };

    const TEST_EXCERPT_LIMIT_CHARS: usize = 420;
    const TEST_REPEATED_SECTION_COUNT: usize = 18;

    fn long_structured_document() -> String {
        let repeated_sections = (0..TEST_REPEATED_SECTION_COUNT)
            .map(|index| {
                format!(
                    "  worker_{index}:\n    image: example.invalid/worker_{index}\n    bindings:\n      - private\n"
                )
            })
            .collect::<String>();
        format!(
            "components:\n{repeated_sections}\nbindings:\n  public:\n    driver: shared\n  private:\n    driver: isolated\n    restricted: true\n"
        )
    }

    #[test]
    fn focused_structural_excerpt_reaches_a_declaration_block_at_the_document_tail() {
        let content = long_structured_document();

        let excerpt = structured_literal_excerpt_for(
            &content,
            &["bindings".to_string(), "restricted".to_string()],
            TEST_EXCERPT_LIMIT_CHARS,
        )
        .expect("matching declaration block");

        assert!(excerpt.contains("\nbindings:\n  public:"));
        assert!(
            excerpt.contains("  private:\n    driver: isolated\n    restricted: true"),
            "{excerpt}"
        );
    }

    #[test]
    fn focused_structural_excerpt_does_not_widen_to_unrelated_declarations() {
        let content = long_structured_document();

        let excerpt = structured_literal_excerpt_for(
            &content,
            &["retention".to_string()],
            TEST_EXCERPT_LIMIT_CHARS,
        )
        .unwrap_or_default();

        assert!(!excerpt.contains("\nbindings:\n  public:"));
        assert!(!excerpt.contains("restricted: true"));
    }

    #[test]
    fn structured_excerpt_without_a_matching_declaration_is_indentation_stable() {
        let flat = "FlowStage::Queued => &[FlowStage::Running, FlowStage::Aborted],\nFlowStage::Running => &[FlowStage::Finished],";
        let indented = "    FlowStage::Queued => &[FlowStage::Running, FlowStage::Aborted],\n        FlowStage::Running => &[FlowStage::Finished],";
        let keywords = ["Queued".to_string(), "FlowStage".to_string()];

        let flat_excerpt =
            structured_literal_excerpt_for(flat, &keywords, TEST_EXCERPT_LIMIT_CHARS);
        let indented_excerpt =
            structured_literal_excerpt_for(indented, &keywords, TEST_EXCERPT_LIMIT_CHARS);

        assert_eq!(indented_excerpt, flat_excerpt);
    }

    #[test]
    fn declaration_reach_score_accepts_structural_shapes_across_languages() {
        let declarations = [
            ("type LedgerReader interface {\n    Read() error\n}", "ledgerreader"),
            (
                "CREATE FUNCTION reconcile_ledger(input_id integer)\n  RETURNS integer\n  AS $$\n    SELECT input_id;\n  $$;",
                "reconcile_ledger",
            ),
            ("ledger_defaults: &ledger_defaults\n  retries: 3", "ledger_defaults"),
        ];

        for (content, focus) in declarations {
            assert!(
                focused_declaration_reach_score(content, &[focus.to_string()], 40) > 0,
                "{content}"
            );
        }
    }

    #[test]
    fn declaration_inventory_harvests_language_neutral_top_level_signatures_only() {
        let content = "type LedgerReader interface {\n    Read() error\n}\n\nCREATE FUNCTION reconcile_ledger(input_id integer)\n  RETURNS integer\n  AS $$\n    SELECT input_id;\n  $$;\n\nledger_defaults: &ledger_defaults\n  retries: 3\n  nested:\n    enabled: true";

        assert_eq!(
            top_level_declaration_signatures(content),
            vec![
                "type LedgerReader interface {",
                "CREATE FUNCTION reconcile_ledger(input_id integer)",
                "ledger_defaults: &ledger_defaults",
            ]
        );
    }
}
