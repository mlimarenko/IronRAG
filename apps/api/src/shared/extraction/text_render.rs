use crate::shared::extraction::{
    ExtractionLineHint, ExtractionStructureHints, build_text_layout_from_content,
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

fn should_join_without_separator(previous: &str, current: &str) -> bool {
    let Some(left) = last_token(previous) else {
        return false;
    };
    let Some(right) = first_token(current) else {
        return false;
    };

    is_protocol_split(left, right)
        || is_path_continuation(left, right)
        || is_ascii_fragment_split(left, right)
}

#[derive(Debug, Clone)]
pub struct PreStructuringNormalization {
    pub normalized_text: String,
    pub normalization_profile: String,
    pub structure_hints: ExtractionStructureHints,
}

#[must_use]
pub fn normalize_for_structured_preparation(
    content: &str,
    structure_hints: Option<&ExtractionStructureHints>,
) -> PreStructuringNormalization {
    let source_hints = structure_hints
        .cloned()
        .unwrap_or_else(|| build_text_layout_from_content(content).structure_hints);
    let mut repaired_lines = Vec::<ExtractionLineHint>::new();
    let mut joined_line_count = 0_usize;

    for line in source_hints.lines {
        let mut current = line;
        let trimmed_text = current.text.trim().to_string();
        current.text = if trimmed_text.is_empty() {
            String::new()
        } else {
            current.text.trim_end().to_string()
        };

        if trimmed_text.is_empty() {
            repaired_lines.push(current);
            continue;
        }

        if let Some(previous) = repaired_lines.last_mut() {
            let same_page = previous.page_number == current.page_number;
            if same_page && should_join_without_separator(previous.text.trim(), current.text.trim())
            {
                previous.text.push_str(current.text.trim());
                previous.source_ordinals.extend(current.source_ordinals);
                previous.source_ordinals.sort_unstable();
                previous.source_ordinals.dedup();
                previous.signals.extend(current.signals);
                previous.signals.sort_unstable_by_key(|signal| *signal as u8);
                previous.signals.dedup();
                joined_line_count = joined_line_count.saturating_add(1);
                continue;
            }
        }

        repaired_lines.push(current);
    }

    let mut normalized_text = String::new();
    let mut offset = 0_i32;
    for (index, line) in repaired_lines.iter_mut().enumerate() {
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
    normalize_for_structured_preparation(content, None).normalized_text
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
            "http\n://demo.local:8080/rewards-api/rest/v1/accounts\n/bypage\n/system/info",
        );

        assert!(repaired.contains("http://demo.local:8080/rewards-api/rest/v1/accounts/bypage"));
        assert!(repaired.contains("/system/info"));
    }

    #[test]
    fn repair_technical_layout_noise_does_not_join_uppercase_headings() {
        let repaired = repair_technical_layout_noise("REST\nAPI\nGET");

        assert_eq!(repaired, "REST\nAPI\nGET");
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
