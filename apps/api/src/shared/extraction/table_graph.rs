use std::collections::BTreeSet;

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
    key.to_ascii_lowercase()
        .chars()
        .map(|character| if character.is_ascii_alphanumeric() { character } else { ' ' })
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
        .filter(|segment| {
            !segment.is_empty()
                && !segment.starts_with("Sheet: ")
                && !segment.starts_with("Table: ")
                && !segment.starts_with("Row ")
        })
        .collect::<Vec<_>>();

    let mut key_value_pairs = Vec::new();
    for segment in &segments {
        let Some((key, value)) = segment.split_once(": ") else {
            continue;
        };
        if key == "Index" && is_numeric_index_value(value) {
            continue;
        }
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
        .filter(|(key, value)| {
            let normalized_key = normalize_table_graph_key(key);
            subject.as_ref().is_none_or(|subject| {
                if matches!(normalized_key.as_str(), "first name" | "last name" | "full name") {
                    return false;
                }
                !subject.eq_ignore_ascii_case(&format!("{key}: {value}"))
                    && !subject.ends_with(value.as_str())
            })
        })
        .map(|(key, value)| format!("{key}: {value}"))
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

    !is_ignored_graph_key(key)
}

fn is_graph_subject_candidate(summary: &TableColumnSummary) -> bool {
    if matches!(summary.value_kind, TableSummaryValueKind::Numeric)
        || matches!(
            summary.value_shape,
            TableSummaryValueShape::Identifier | TableSummaryValueShape::Url
        )
    {
        return false;
    }

    summary.non_empty_count > 0
        && summary.distinct_count.saturating_mul(5) >= summary.non_empty_count.saturating_mul(4)
}

fn is_graph_attribute_candidate(summary: &TableColumnSummary) -> bool {
    if matches!(summary.value_kind, TableSummaryValueKind::Numeric)
        || matches!(
            summary.value_shape,
            TableSummaryValueShape::Identifier | TableSummaryValueShape::Url
        )
    {
        return false;
    }

    summary.most_frequent_count > 1
}

fn is_numeric_index_value(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty() && trimmed.chars().all(|character| character.is_ascii_digit())
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

fn is_ignored_graph_key(key: &str) -> bool {
    let normalized = normalize_table_graph_key(key);
    matches!(
        normalized.as_str(),
        "index"
            | "row"
            | "sheet"
            | "table"
            | "sex"
            | "gender"
            | "email"
            | "phone"
            | "mobile"
            | "fax"
            | "website"
            | "url"
            | "date"
            | "date of birth"
            | "birth date"
            | "dob"
            | "created at"
            | "updated at"
    ) || normalized.ends_with(" id")
        || normalized.ends_with(" code")
        || normalized.ends_with(" email")
        || normalized.ends_with(" phone")
        || normalized.ends_with(" date")
        || normalized.ends_with(" url")
}

fn build_graph_subject(
    pairs: &[(String, String)],
    profile: Option<&TableGraphProfile>,
) -> Option<String> {
    let value_for = |target: &str| {
        pairs.iter().find_map(|(key, value)| {
            if normalize_table_graph_key(key) == target { Some(value.as_str()) } else { None }
        })
    };

    if let (Some(first_name), Some(last_name)) = (value_for("first name"), value_for("last name")) {
        let subject = format!("{first_name} {last_name}").trim().to_string();
        if !subject.is_empty() {
            return Some(format!("Person: {subject}"));
        }
    }

    if let Some(full_name) = value_for("full name") {
        return Some(format!("Person: {full_name}"));
    }

    for (target, label) in [
        ("product name", "Product"),
        ("product", "Product"),
        ("organization name", "Organization"),
        ("organization", "Organization"),
        ("company name", "Organization"),
        ("company", "Organization"),
        ("customer name", "Customer"),
        ("customer", "Customer"),
        ("lead name", "Lead"),
        ("lead", "Lead"),
        ("name", "Name"),
    ] {
        if let Some(value) = value_for(target) {
            return Some(format!("{label}: {value}"));
        }
    }

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
    fn graph_text_falls_back_to_header_heuristics_without_profile() {
        let text = build_graph_table_row_text(
            "Sheet: people-100 | Row 1 | Index: 1 | User Id: 88F7B33d2bcf9f5 | First Name: Shelby | Last Name: Terrell | Sex: Male | Email: elijah57@example.net | Phone: 001-084-906-7849x73518 | Date of birth: 1945-10-26 | Job Title: Games developer",
            None,
        )
        .expect("graph text");

        assert_eq!(text, "Person: Shelby Terrell | Job Title: Games developer");
        assert_eq!(normalize_table_graph_key("Job Title"), "job title");
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
