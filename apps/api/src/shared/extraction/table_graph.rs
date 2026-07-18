use std::collections::BTreeSet;

use unicode_normalization::UnicodeNormalization;

use super::{
    table_markdown::normalize_table_cell_text,
    table_summary::{TableColumnSummary, TableSummaryValueKind, TableSummaryValueShape},
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableGraphProfile {
    attribute_keys: BTreeSet<String>,
    subject_candidate_keys: BTreeSet<String>,
}

impl TableGraphProfile {
    #[must_use]
    pub fn allows_attribute(&self, key: &str) -> bool {
        self.attribute_keys.contains(&normalize_table_graph_key(key))
    }

    #[must_use]
    pub fn prefers_subject(&self, key: &str) -> bool {
        self.subject_candidate_keys.contains(&normalize_table_graph_key(key))
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.attribute_keys.is_empty() && self.subject_candidate_keys.is_empty()
    }
}

#[must_use]
pub fn build_table_graph_profile(summaries: &[TableColumnSummary]) -> TableGraphProfile {
    let mut profile = TableGraphProfile::default();

    for summary in summaries {
        let normalized_key = normalize_table_graph_key(&summary.column_name);
        if normalized_key.is_empty() {
            continue;
        }

        if is_graph_subject_candidate(summary) {
            profile.subject_candidate_keys.insert(normalized_key.clone());
        }
        if is_graph_attribute_candidate(summary) {
            profile.attribute_keys.insert(normalized_key);
        }
    }

    profile
}

#[must_use]
pub fn normalize_table_graph_key(key: &str) -> String {
    key.nfkc()
        .flat_map(char::to_lowercase)
        .map(|character| if character.is_alphanumeric() { character } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[must_use]
pub fn build_graph_table_row_text(
    semantic_text: &str,
    profile: Option<&TableGraphProfile>,
) -> Option<String> {
    let segments = semantic_text
        .split(" | ")
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    let data_start =
        segments.iter().position(|segment| !segment.contains(": ")).map_or(0, |index| index + 1);

    let mut key_value_pairs = Vec::new();
    for segment in &segments[data_start..] {
        let Some((key, value)) = segment.split_once(": ") else {
            continue;
        };
        let value = normalize_table_cell_text(value);
        if value.is_empty() {
            continue;
        }
        key_value_pairs.push((key.trim().to_string(), value));
    }

    let subject = build_graph_subject(&key_value_pairs, profile);
    let attributes = key_value_pairs
        .iter()
        .filter(|(key, value)| attribute_allowed_for_graph(key, value, profile))
        .map(|(key, value)| format!("{key}: {value}"))
        .filter(|attribute| subject.as_ref() != Some(attribute))
        .collect::<Vec<_>>();

    let filtered = subject.into_iter().chain(attributes).collect::<Vec<_>>();
    (!filtered.is_empty()).then(|| filtered.join(" | "))
}

fn attribute_allowed_for_graph(
    key: &str,
    value: &str,
    profile: Option<&TableGraphProfile>,
) -> bool {
    let normalized_key = normalize_table_graph_key(key);
    if value.is_empty()
        || is_synthetic_column_key(&normalized_key)
        || is_numeric_like_literal(value)
    {
        return false;
    }

    if let Some(profile) = profile {
        return profile.allows_attribute(key);
    }

    true
}

fn is_graph_subject_candidate(summary: &TableColumnSummary) -> bool {
    if summary.value_kind == TableSummaryValueKind::Numeric
        || summary.value_shape != TableSummaryValueShape::Label
    {
        return false;
    }

    ratio_at_least(summary.non_empty_count, summary.row_count, 4, 5)
        && ratio_at_least(summary.distinct_count, summary.non_empty_count, 4, 5)
}

fn is_graph_attribute_candidate(summary: &TableColumnSummary) -> bool {
    if is_low_signal_graph_column(summary) {
        return false;
    }

    ratio_at_least(summary.non_empty_count, summary.row_count, 1, 2)
        && summary.most_frequent_count > 1
}

const fn is_low_signal_graph_column(summary: &TableColumnSummary) -> bool {
    matches!(summary.value_kind, TableSummaryValueKind::Numeric)
        || matches!(
            summary.value_shape,
            TableSummaryValueShape::Identifier | TableSummaryValueShape::Url
        )
}

const fn ratio_at_least(
    value: usize,
    total: usize,
    minimum_numerator: usize,
    minimum_denominator: usize,
) -> bool {
    total > 0
        && value.saturating_mul(minimum_denominator) >= total.saturating_mul(minimum_numerator)
}

fn is_synthetic_column_key(key: &str) -> bool {
    key.strip_prefix("col ").or_else(|| key.strip_prefix("col_")).is_some_and(|suffix| {
        !suffix.is_empty() && suffix.chars().all(|character| character.is_ascii_digit())
    })
}

fn is_numeric_like_literal(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && trimmed.chars().all(|character| {
            character.is_ascii_digit()
                || matches!(character, '.' | ',' | '-' | '+' | '/' | ':' | '%' | ' ')
        })
}

fn build_graph_subject(
    pairs: &[(String, String)],
    profile: Option<&TableGraphProfile>,
) -> Option<String> {
    profile.and_then(|profile| {
        pairs.iter().find_map(|(key, value)| {
            profile.prefers_subject(key).then(|| format!("{key}: {value}"))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::{build_graph_table_row_text, build_table_graph_profile, normalize_table_graph_key};
    use crate::shared::extraction::table_summary::build_table_column_summaries;

    #[test]
    fn builds_profile_from_column_statistics_instead_of_header_lists() {
        let summaries = build_table_column_summaries(
            Some("products"),
            None,
            &[
                "Product Name".to_string(),
                "Category".to_string(),
                "Price".to_string(),
                "Website".to_string(),
                "Availability".to_string(),
            ],
            &[
                vec![
                    "AWM181".to_string(),
                    "Games".to_string(),
                    "451.19".to_string(),
                    "https://example.com/a".to_string(),
                    "pre_order".to_string(),
                ],
                vec![
                    "BWM182".to_string(),
                    "Games".to_string(),
                    "499.99".to_string(),
                    "https://example.com/b".to_string(),
                    "pre_order".to_string(),
                ],
                vec![
                    "CWM183".to_string(),
                    "Books".to_string(),
                    "299.99".to_string(),
                    "https://example.com/c".to_string(),
                    "in_stock".to_string(),
                ],
            ],
        );
        let profile = build_table_graph_profile(&summaries);

        assert!(profile.prefers_subject("Product Name"));
        assert!(profile.allows_attribute("Category"));
        assert!(profile.allows_attribute("Availability"));
        assert!(!profile.allows_attribute("Price"));
        assert!(!profile.allows_attribute("Website"));
    }

    #[test]
    fn graph_text_uses_profile_to_drop_numeric_identifier_and_unique_noise() {
        let summaries = build_table_column_summaries(
            Some("organizations"),
            None,
            &[
                "Name".to_string(),
                "Country".to_string(),
                "Industry".to_string(),
                "Founded".to_string(),
                "Website".to_string(),
            ],
            &[
                vec![
                    "Ferrell LLC".to_string(),
                    "Papua New Guinea".to_string(),
                    "Plastics".to_string(),
                    "1972".to_string(),
                    "https://price.net".to_string(),
                ],
                vec![
                    "Meyer Group".to_string(),
                    "Papua New Guinea".to_string(),
                    "Plastics".to_string(),
                    "1991".to_string(),
                    "https://meyer.test".to_string(),
                ],
                vec![
                    "Adams LLC".to_string(),
                    "Sweden".to_string(),
                    "Retail".to_string(),
                    "2012".to_string(),
                    "https://adams.test".to_string(),
                ],
            ],
        );
        let profile = build_table_graph_profile(&summaries);
        let text = build_graph_table_row_text(
            "Sheet: organizations-100 | Row 1 | Index: 1 | Name: Ferrell LLC | Country: Papua New Guinea | Industry: Plastics | Founded: 1972 | Website: https://price.net/",
            Some(&profile),
        )
        .expect("graph text");

        assert_eq!(text, "Name: Ferrell LLC | Country: Papua New Guinea | Industry: Plastics");
    }

    #[test]
    fn graph_text_profile_prefers_well_covered_subject_over_sparse_named_column() {
        let summaries = build_table_column_summaries(
            None,
            None,
            &["Name".to_string(), "Primary".to_string(), "Group".to_string()],
            &[
                vec!["Legacy".to_string(), "Alpha".to_string(), "Shared".to_string()],
                vec![String::new(), "Beta".to_string(), "Shared".to_string()],
                vec![String::new(), "Gamma".to_string(), "Shared".to_string()],
                vec![String::new(), "Delta".to_string(), "Shared".to_string()],
                vec![String::new(), "Epsilon".to_string(), "Shared".to_string()],
            ],
        );
        let profile = build_table_graph_profile(&summaries);
        let text = build_graph_table_row_text(
            "Row 1 | Name: Legacy | Primary: Alpha | Group: Shared",
            Some(&profile),
        )
        .expect("graph text");

        assert!(!profile.prefers_subject("Name"));
        assert!(profile.prefers_subject("Primary"));
        assert_eq!(text, "Primary: Alpha | Group: Shared");
    }

    #[test]
    fn graph_text_profile_does_not_use_narrative_column_as_subject() {
        let summaries = build_table_column_summaries(
            None,
            None,
            &["Narrative".to_string(), "Label".to_string(), "Group".to_string()],
            &[
                vec![
                    "segment alpha carries eight distinct narrative tokens now".to_string(),
                    "Alpha".to_string(),
                    "Shared".to_string(),
                ],
                vec![
                    "segment beta carries eight distinct narrative tokens now".to_string(),
                    "Beta".to_string(),
                    "Shared".to_string(),
                ],
                vec![
                    "segment gamma carries eight distinct narrative tokens now".to_string(),
                    "Gamma".to_string(),
                    "Shared".to_string(),
                ],
            ],
        );
        let profile = build_table_graph_profile(&summaries);
        let text = build_graph_table_row_text(
            "Row 1 | Narrative: segment alpha carries eight distinct narrative tokens now | Label: Alpha | Group: Shared",
            Some(&profile),
        )
        .expect("graph text");

        assert!(!profile.prefers_subject("Narrative"));
        assert_eq!(text, "Label: Alpha | Group: Shared");
    }

    #[test]
    fn graph_text_without_profile_preserves_source_pairs_without_synthesizing_ontology() {
        let text = build_graph_table_row_text(
            "Sheet: sample | Row 1 | First Name: Shelby | Last Name: Terrell | Category: Analyst",
            None,
        )
        .expect("graph text");

        assert_eq!(text, "First Name: Shelby | Last Name: Terrell | Category: Analyst");
    }

    #[test]
    fn graph_text_preserves_source_header_that_starts_like_row_metadata() {
        let text = build_graph_table_row_text("Row 1 | Row Label: Alpha | Secondary: Beta", None)
            .expect("graph text");

        assert_eq!(text, "Row Label: Alpha | Secondary: Beta");
    }

    #[test]
    fn table_graph_key_normalization_preserves_unicode_letters() {
        assert_eq!(normalize_table_graph_key("Δείγμα Поле"), "δείγμα поле");
    }

    #[test]
    fn graph_text_skips_synthetic_single_column_rows_without_profile() {
        assert_eq!(build_graph_table_row_text("Sheet: test1 | Row 1 | col_1: test1", None), None);
        assert_eq!(
            build_graph_table_row_text("Sheet: sample-heavy-1 | Row 1 | col_1: 1", None),
            None
        );
    }
}
