use crate::shared::extraction::{
    ExtractionLineHint, ExtractionLineSignal, ExtractionStructureHints,
    build_text_layout_from_content,
};

fn first_token(text: &str) -> Option<&str> {
    text.split_whitespace().next()
}

fn last_token(text: &str) -> Option<&str> {
    text.split_whitespace().last()
}

fn is_protocol_split(left: &str, right: &str) -> bool {
    matches!(left, "http" | "https") && right.starts_with("://")
}

fn is_path_continuation(left: &str, right: &str) -> bool {
    (left.starts_with('/') || left.contains("://")) && right.starts_with('/')
}

fn is_ascii_fragment_split(left: &str, right: &str) -> bool {
    if !left.is_ascii() || !right.is_ascii() {
        return false;
    }
    if left.len() > 32 || right.len() > 32 {
        return false;
    }

    let Some(left_last) = left.chars().last() else {
        return false;
    };
    let Some(right_first) = right.chars().next() else {
        return false;
    };

    let left_joinable = left_last.is_ascii_lowercase()
        || left_last.is_ascii_digit()
        || matches!(left_last, '_' | '/' | ':' | '.');
    let right_joinable =
        right_first.is_ascii_lowercase() || right_first.is_ascii_digit() || right_first == '_';
    if !left_joinable || !right_joinable {
        return false;
    }

    if left.chars().all(|ch| !ch.is_ascii_lowercase()) {
        return false;
    }

    let left_tail_since_uppercase = left
        .char_indices()
        .rev()
        .find(|(_, ch)| ch.is_ascii_uppercase())
        .map_or(left.len(), |(index, _)| left.len().saturating_sub(index));
    let left_all_lower_or_digits =
        left.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit());
    let left_fragment_like = (left_all_lower_or_digits
        && (left.len() <= 4 || right.starts_with('_')))
        || left_tail_since_uppercase <= 3;
    let right_fragment_like = is_right_fragment_like(right);

    left_fragment_like && right_fragment_like
}

fn is_right_fragment_like(token: &str) -> bool {
    token.starts_with('_')
        || (token.len() <= 5
            && token.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit()))
        || (token.len() <= 8
            && token.chars().next().is_some_and(|ch| ch.is_ascii_lowercase())
            && token.chars().skip(1).any(|ch| ch.is_ascii_uppercase()))
}

fn line_has_structural_boundary_signal(line: &ExtractionLineHint) -> bool {
    line.signals.iter().any(|signal| {
        matches!(
            signal,
            ExtractionLineSignal::CodeFence
                | ExtractionLineSignal::CodeLine
                | ExtractionLineSignal::EndpointCandidate
                | ExtractionLineSignal::TableRow
                | ExtractionLineSignal::MetadataCandidate
                | ExtractionLineSignal::ListItem
        )
    })
}

fn line_has_ascii_structural_action_shape(text: &str) -> bool {
    let mut tokens = text.split_whitespace();
    let Some(first) = tokens.next() else {
        return false;
    };
    let Some(_) = tokens.next() else {
        return false;
    };
    let first = first
        .trim_matches(|ch: char| ch.is_ascii_punctuation() && !matches!(ch, '/' | '.' | '-' | '_'));
    if first.is_empty() || !first.is_ascii() {
        return false;
    }
    let first_action_like = first.starts_with('/')
        || first.starts_with("./")
        || first.contains('-')
        || first.contains('.')
        || first.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit());
    first_action_like && text.split_whitespace().skip(1).any(is_structural_argument_token)
}

fn is_structural_argument_token(token: &str) -> bool {
    token.contains("://")
        || token.starts_with('/')
        || token.starts_with("./")
        || token.starts_with('-')
        || token.starts_with('+')
        || token.contains('=')
}

fn is_independent_structural_line_boundary(
    previous: &ExtractionLineHint,
    current: &ExtractionLineHint,
) -> bool {
    line_has_structural_boundary_signal(previous)
        || line_has_structural_boundary_signal(current)
        || line_has_ascii_structural_action_shape(previous.text.trim())
        || line_has_ascii_structural_action_shape(current.text.trim())
}

fn should_join_without_separator(
    previous: &ExtractionLineHint,
    current: &ExtractionLineHint,
) -> bool {
    let previous_text = previous.text.trim();
    let current_text = current.text.trim();
    let Some(left) = last_token(previous_text) else {
        return false;
    };
    let Some(right) = first_token(current_text) else {
        return false;
    };

    if is_protocol_split(left, right) || is_path_continuation(left, right) {
        return true;
    }
    if is_independent_structural_line_boundary(previous, current) {
        return false;
    }
    is_ascii_fragment_split(left, right)
}

#[derive(Debug, Clone)]
pub struct PreStructuringNormalization {
    pub normalized_text: String,
    pub normalization_profile: String,
    pub structure_hints: ExtractionStructureHints,
}

fn normalize_line_text(mut line: ExtractionLineHint) -> ExtractionLineHint {
    line.text =
        if line.text.trim().is_empty() { String::new() } else { line.text.trim_end().to_string() };
    line
}

fn merge_line_into_previous(previous: &mut ExtractionLineHint, current: ExtractionLineHint) {
    previous.text.push_str(current.text.trim());
    previous.source_ordinals.extend(current.source_ordinals);
    previous.source_ordinals.sort_unstable();
    previous.source_ordinals.dedup();
    previous.signals.extend(current.signals);
    previous.signals.sort_unstable_by_key(|signal| *signal as u8);
    previous.signals.dedup();
}

fn repair_structured_lines(
    source_hints: ExtractionStructureHints,
) -> (Vec<ExtractionLineHint>, usize) {
    let mut repaired_lines = Vec::<ExtractionLineHint>::new();
    let mut joined_line_count = 0_usize;

    for line in source_hints.lines {
        let current = normalize_line_text(line);
        if current.text.is_empty() {
            repaired_lines.push(current);
            continue;
        }

        let should_join = repaired_lines.last().is_some_and(|previous| {
            previous.page_number == current.page_number
                && should_join_without_separator(previous, &current)
        });
        if should_join && let Some(previous) = repaired_lines.last_mut() {
            merge_line_into_previous(previous, current);
            joined_line_count = joined_line_count.saturating_add(1);
        } else {
            repaired_lines.push(current);
        }
    }
    (repaired_lines, joined_line_count)
}

fn render_normalized_lines(lines: &mut [ExtractionLineHint]) -> String {
    let mut normalized_text = String::new();
    let mut offset = 0_i32;
    for (index, line) in lines.iter_mut().enumerate() {
        if index > 0 {
            normalized_text.push('\n');
            offset = offset.saturating_add(1);
        }
        let start_offset = offset;
        normalized_text.push_str(&line.text);
        offset =
            offset.saturating_add(i32::try_from(line.text.chars().count()).unwrap_or(i32::MAX));
        line.ordinal = i32::try_from(index).unwrap_or(i32::MAX);
        line.start_offset = Some(start_offset);
        line.end_offset = Some(offset);
    }
    normalized_text
}

#[must_use]
pub fn normalize_for_structured_preparation(
    content: &str,
    structure_hints: Option<&ExtractionStructureHints>,
) -> PreStructuringNormalization {
    let source_hints = structure_hints
        .cloned()
        .unwrap_or_else(|| build_text_layout_from_content(content).structure_hints);
    let (mut repaired_lines, joined_line_count) = repair_structured_lines(source_hints);
    let normalized_text = render_normalized_lines(&mut repaired_lines);

    PreStructuringNormalization {
        normalized_text,
        normalization_profile: if joined_line_count == 0 {
            "pre_structuring_verbatim_v1".to_string()
        } else {
            "pre_structuring_layout_repair_v1".to_string()
        },
        structure_hints: ExtractionStructureHints { lines: repaired_lines },
    }
}

#[must_use]
pub fn repair_technical_layout_noise(content: &str) -> String {
    repair_structural_display_noise(
        &normalize_for_structured_preparation(content, None).normalized_text,
    )
}

fn repair_structural_display_noise(content: &str) -> String {
    content.lines().map(repair_structural_display_line).collect::<Vec<_>>().join("\n")
}

fn repair_structural_display_line(line: &str) -> String {
    let tokens = line.split_whitespace().collect::<Vec<_>>();
    let mut repaired = Vec::<String>::new();
    let mut changed = false;
    for (index, token) in tokens.iter().copied().enumerate() {
        let previous = index.checked_sub(1).and_then(|previous| tokens.get(previous)).copied();
        let next = tokens.get(index + 1).copied();
        let expanded = expand_concatenated_display_token(token, previous, next);
        changed |= expanded.len() != 1 || expanded.first().is_none_or(|value| value != token);
        repaired.extend(expanded);
    }
    if changed { repaired.join(" ") } else { line.to_string() }
}

fn expand_concatenated_display_token(
    token: &str,
    previous: Option<&str>,
    next: Option<&str>,
) -> Vec<String> {
    if let Some((left, right)) = split_repeated_short_prefix_token(token, previous, next) {
        return vec![left, right];
    }
    if let Some((left, right)) = split_concatenated_path_like_token(token) {
        return vec![left, right];
    }
    vec![token.to_string()]
}

fn split_repeated_short_prefix_token(
    token: &str,
    previous: Option<&str>,
    next: Option<&str>,
) -> Option<(String, String)> {
    let previous = previous?
        .trim_matches(|ch: char| ch.is_ascii_punctuation() && !matches!(ch, '/' | '.' | '-' | '_'));
    let next = next?;
    if !is_structural_argument_token(next)
        || previous.chars().count() > 4
        || !ascii_lower_alnum(previous)
        || !ascii_lower_alnum(token)
    {
        return None;
    }

    for prefix_len in (2..=3).rev() {
        let prefix = token.get(..prefix_len)?;
        let suffix = token.get(prefix_len..)?;
        if previous.starts_with(prefix) && suffix.chars().count() >= 3 {
            return Some((prefix.to_string(), suffix.to_string()));
        }
    }
    None
}

fn split_concatenated_path_like_token(token: &str) -> Option<(String, String)> {
    if token.contains("://") {
        return None;
    }
    for (index, character) in token.char_indices().skip(1) {
        if character != '/' {
            continue;
        }
        let (left, right) = token.split_at(index);
        if path_segment_has_file_marker(left)
            && right.get(1..).is_some_and(|suffix| suffix.contains('/'))
            && path_segment_has_file_marker(right)
        {
            return Some((left.to_string(), right.to_string()));
        }
    }
    None
}

fn path_segment_has_file_marker(token: &str) -> bool {
    token
        .rsplit('/')
        .next()
        .is_some_and(|segment| segment.contains('.') && segment.chars().any(char::is_alphanumeric))
}

fn ascii_lower_alnum(token: &str) -> bool {
    !token.is_empty() && token.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use crate::shared::extraction::{ExtractionLineHint, ExtractionStructureHints};

    use super::{normalize_for_structured_preparation, repair_technical_layout_noise};

    #[test]
    fn repair_technical_layout_noise_joins_ascii_identifier_fragments() {
        let repaired = repair_technical_layout_noise(
            "pageNu\nmber\nwithCar\nds\nnumber\n_starting\ninte\nger\nboo\nlean",
        );

        assert!(repaired.contains("pageNumber"));
        assert!(repaired.contains("withCards"));
        assert!(repaired.contains("number_starting"));
        assert!(repaired.contains("integer"));
        assert!(repaired.contains("boolean"));
    }

    #[test]
    fn repair_technical_layout_noise_joins_protocol_and_paths() {
        let repaired = repair_technical_layout_noise(
            "http\n://example.invalid:8080/records-api/rest/v1/items\n/bypage\n/system/info",
        );

        assert!(repaired.contains("http://example.invalid:8080/records-api/rest/v1/items/bypage"));
        assert!(repaired.contains("/system/info"));
    }

    #[test]
    fn repair_technical_layout_noise_does_not_join_uppercase_headings() {
        let repaired = repair_technical_layout_noise("REST\nAPI\nGET");

        assert_eq!(repaired, "REST\nAPI\nGET");
    }

    #[test]
    fn repair_technical_layout_noise_preserves_independent_structural_lines() {
        let repaired = repair_technical_layout_noise(
            "axis ax\nbeta https://example.invalid/group/item.ext -p /tmp/item.ext",
        );

        assert!(repaired.contains("axis ax\nbeta "));
        assert!(!repaired.contains("axbeta"));
    }

    #[test]
    fn repair_technical_layout_noise_splits_fused_structural_tokens() {
        let repaired = repair_technical_layout_noise(
            "axis axbeta https://example.invalid/group/item.ext -p /tmp/item.ext\n\
             mark +x /tmp/item.ext/tmp/item.ext",
        );

        assert!(repaired.contains("axis ax beta "));
        assert!(repaired.contains("mark +x /tmp/item.ext /tmp/item.ext"));
    }

    #[test]
    fn normalize_for_structured_preparation_preserves_page_boundaries() {
        let normalized = normalize_for_structured_preparation(
            "",
            Some(&ExtractionStructureHints {
                lines: vec![
                    ExtractionLineHint {
                        ordinal: 0,
                        source_ordinals: vec![0],
                        page_number: Some(1),
                        text: "pageNu".to_string(),
                        start_offset: None,
                        end_offset: None,
                        signals: Vec::new(),
                    },
                    ExtractionLineHint {
                        ordinal: 1,
                        source_ordinals: vec![1],
                        page_number: Some(1),
                        text: "mber".to_string(),
                        start_offset: None,
                        end_offset: None,
                        signals: Vec::new(),
                    },
                    ExtractionLineHint {
                        ordinal: 2,
                        source_ordinals: vec![2],
                        page_number: Some(2),
                        text: "withCar".to_string(),
                        start_offset: None,
                        end_offset: None,
                        signals: Vec::new(),
                    },
                    ExtractionLineHint {
                        ordinal: 3,
                        source_ordinals: vec![3],
                        page_number: Some(2),
                        text: "ds".to_string(),
                        start_offset: None,
                        end_offset: None,
                        signals: Vec::new(),
                    },
                ],
            }),
        );

        assert_eq!(normalized.normalized_text, "pageNumber\nwithCards");
        assert_eq!(normalized.structure_hints.lines[0].page_number, Some(1));
        assert_eq!(normalized.structure_hints.lines[1].page_number, Some(2));
        assert_eq!(normalized.structure_hints.lines[0].source_ordinals, vec![0, 1]);
        assert_eq!(normalized.structure_hints.lines[1].source_ordinals, vec![2, 3]);
    }
}
