use std::collections::BTreeMap;

use super::table_markdown::{canonicalize_table_headers, normalize_table_cell_text};

pub const TABLE_SUMMARY_PREFIX: &str = "Table Summary";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableSummaryValueKind {
    Numeric,
    Categorical,
}

impl TableSummaryValueKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Numeric => "numeric",
            Self::Categorical => "categorical",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableSummaryValueShape {
    Label,
    Identifier,
    Url,
    Narrative,
}

impl TableSummaryValueShape {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Label => "label",
            Self::Identifier => "identifier",
            Self::Url => "url",
            Self::Narrative => "narrative",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TableColumnSummary {
    pub sheet_name: Option<String>,
    pub table_name: Option<String>,
    pub column_name: String,
    pub value_kind: TableSummaryValueKind,
    pub value_shape: TableSummaryValueShape,
    pub aggregation_priority: u8,
    pub row_count: usize,
    pub non_empty_count: usize,
    pub distinct_count: usize,
    pub average: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub most_frequent_values: Vec<String>,
    pub most_frequent_count: usize,
    pub most_frequent_tie_count: usize,
}

#[must_use]
pub fn is_table_summary_text(text: &str) -> bool {
    text.trim_start().starts_with(TABLE_SUMMARY_PREFIX)
}

#[must_use]
pub fn build_table_column_summaries(
    sheet_name: Option<&str>,
    table_name: Option<&str>,
    headers: &[String],
    rows: &[Vec<String>],
) -> Vec<TableColumnSummary> {
    if rows.len() < 2 {
        return Vec::new();
    }

    let width = headers.len().max(rows.iter().map(Vec::len).max().unwrap_or(0));
    let headers = canonicalize_table_headers(headers, width);
    let mut summaries = Vec::new();

    for (column_index, header) in headers.iter().enumerate() {
        let values = rows
            .iter()
            .filter_map(|row| row.get(column_index))
            .map(|value| normalize_table_cell_text(value))
            .filter(|value| !value.trim().is_empty())
            .collect::<Vec<_>>();
        if values.is_empty() {
            continue;
        }

        let distinct_count = values
            .iter()
            .map(|value| value.trim().to_string())
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        let numeric_values =
            values.iter().filter_map(|value| parse_numeric_value(value)).collect::<Vec<_>>();
        let is_numeric = numeric_values.len() >= 2
            && numeric_values.len().saturating_mul(5) >= values.len().saturating_mul(4);
        let value_shape = infer_table_value_shape(&values);

        let mut counts = BTreeMap::<String, usize>::new();
        for value in &values {
            *counts.entry(value.trim().to_string()).or_insert(0) += 1;
        }
        let most_frequent_count = counts.values().copied().max().unwrap_or(0);
        let all_most_frequent_values = counts
            .into_iter()
            .filter_map(|(value, count)| (count == most_frequent_count).then_some(value))
            .collect::<Vec<_>>();
        let most_frequent_tie_count = all_most_frequent_values.len();
        let most_frequent_values = all_most_frequent_values.into_iter().take(5).collect::<Vec<_>>();

        summaries.push(TableColumnSummary {
            sheet_name: sheet_name
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            table_name: table_name
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            column_name: header.replace("\\|", "|"),
            value_kind: if is_numeric {
                TableSummaryValueKind::Numeric
            } else {
                TableSummaryValueKind::Categorical
            },
            value_shape,
            aggregation_priority: infer_aggregation_priority(
                if is_numeric {
                    TableSummaryValueKind::Numeric
                } else {
                    TableSummaryValueKind::Categorical
                },
                value_shape,
                values.len(),
                distinct_count,
                most_frequent_count,
                most_frequent_tie_count,
            ),
            row_count: rows.len(),
            non_empty_count: values.len(),
            distinct_count,
            average: is_numeric
                .then(|| numeric_values.iter().sum::<f64>() / numeric_values.len() as f64),
            min: is_numeric.then(|| numeric_values.iter().copied().fold(f64::INFINITY, f64::min)),
            max: is_numeric
                .then(|| numeric_values.iter().copied().fold(f64::NEG_INFINITY, f64::max)),
            most_frequent_values,
            most_frequent_count,
            most_frequent_tie_count,
        });
    }

    summaries
}

#[must_use]
pub fn render_table_column_summary(summary: &TableColumnSummary) -> String {
    let mut segments = vec![TABLE_SUMMARY_PREFIX.to_string()];
    if let Some(sheet_name) = summary.sheet_name.as_deref() {
        segments.push(format!("Sheet: {sheet_name}"));
    }
    if let Some(table_name) = summary.table_name.as_deref() {
        segments.push(format!("Table: {table_name}"));
    }
    segments.push(format!("Column: {}", summary.column_name));
    segments.push(format!("Value Kind: {}", summary.value_kind.as_str()));
    segments.push(format!("Value Shape: {}", summary.value_shape.as_str()));
    segments.push(format!("Aggregation Priority: {}", summary.aggregation_priority));
    segments.push(format!("Row Count: {}", summary.row_count));
    segments.push(format!("Non-empty Count: {}", summary.non_empty_count));
    segments.push(format!("Distinct Count: {}", summary.distinct_count));
    match summary.value_kind {
        TableSummaryValueKind::Numeric => {
            if let Some(average) = summary.average {
                segments.push(format!("Average: {}", format_numeric_value(average)));
            }
            if let Some(min) = summary.min {
                segments.push(format!("Min: {}", format_numeric_value(min)));
            }
            if let Some(max) = summary.max {
                segments.push(format!("Max: {}", format_numeric_value(max)));
            }
        }
        TableSummaryValueKind::Categorical => {
            segments.push(format!("Most Frequent Count: {}", summary.most_frequent_count));
            segments.push(format!("Most Frequent Tie Count: {}", summary.most_frequent_tie_count));
            if summary.most_frequent_count > 1 || summary.most_frequent_values.len() <= 5 {
                if !summary.most_frequent_values.is_empty() {
                    segments.push(format!(
                        "Most Frequent Values: {}",
                        summary.most_frequent_values.join("; ")
                    ));
                }
            }
        }
    }
    segments.join(" | ")
}

#[must_use]
pub fn parse_table_column_summary(text: &str) -> Option<TableColumnSummary> {
    if !is_table_summary_text(text) {
        return None;
    }

    let mut sheet_name = None::<String>;
    let mut table_name = None::<String>;
    let mut column_name = None::<String>;
    let mut value_kind = None::<TableSummaryValueKind>;
    let mut value_shape = None::<TableSummaryValueShape>;
    let mut aggregation_priority = None::<u8>;
    let mut row_count = None::<usize>;
    let mut non_empty_count = None::<usize>;
    let mut distinct_count = None::<usize>;
    let mut average = None::<f64>;
    let mut min = None::<f64>;
    let mut max = None::<f64>;
    let mut most_frequent_values = Vec::new();
    let mut most_frequent_count = 0usize;
    let mut most_frequent_tie_count = 0usize;

    for segment in text.split(" | ") {
        let trimmed = segment.trim();
        if trimmed == TABLE_SUMMARY_PREFIX {
            continue;
        }
        let Some((key, value)) = trimmed.split_once(": ") else {
            continue;
        };
        match key.trim() {
            "Sheet" => sheet_name = Some(value.trim().to_string()),
            "Table" => table_name = Some(value.trim().to_string()),
            "Column" => column_name = Some(value.trim().to_string()),
            "Value Kind" => {
                value_kind = match value.trim() {
                    "numeric" => Some(TableSummaryValueKind::Numeric),
                    "categorical" => Some(TableSummaryValueKind::Categorical),
                    _ => None,
                };
            }
            "Value Shape" => {
                value_shape = match value.trim() {
                    "label" => Some(TableSummaryValueShape::Label),
                    "identifier" => Some(TableSummaryValueShape::Identifier),
                    "url" => Some(TableSummaryValueShape::Url),
                    "narrative" => Some(TableSummaryValueShape::Narrative),
                    _ => None,
                };
            }
            "Aggregation Priority" => {
                aggregation_priority = value.trim().parse::<u8>().ok();
            }
            "Row Count" => row_count = value.trim().parse::<usize>().ok(),
            "Non-empty Count" => non_empty_count = value.trim().parse::<usize>().ok(),
            "Distinct Count" => distinct_count = value.trim().parse::<usize>().ok(),
            "Average" => average = parse_numeric_value(value),
            "Min" => min = parse_numeric_value(value),
            "Max" => max = parse_numeric_value(value),
            "Most Frequent Count" => {
                most_frequent_count = value.trim().parse::<usize>().ok().unwrap_or(0)
            }
            "Most Frequent Tie Count" => {
                most_frequent_tie_count = value.trim().parse::<usize>().ok().unwrap_or(0)
            }
            "Most Frequent Values" => {
                most_frequent_values = value
                    .split(';')
                    .map(str::trim)
                    .filter(|entry| !entry.is_empty())
                    .map(str::to_string)
                    .collect();
            }
            _ => {}
        }
    }

    Some(TableColumnSummary {
        sheet_name,
        table_name,
        column_name: column_name?,
        value_kind: value_kind?,
        value_shape: value_shape.unwrap_or(TableSummaryValueShape::Label),
        aggregation_priority: aggregation_priority.unwrap_or(0),
        row_count: row_count?,
        non_empty_count: non_empty_count?,
        distinct_count: distinct_count?,
        average,
        min,
        max,
        most_frequent_values,
        most_frequent_count,
        most_frequent_tie_count,
    })
}

#[must_use]
pub fn parse_numeric_value(value: &str) -> Option<f64> {
    let normalized = value.trim().replace(' ', "");
    if normalized.is_empty() {
        return None;
    }
    normalized.parse::<f64>().ok().or_else(|| normalized.replace(',', ".").parse::<f64>().ok())
}

#[must_use]
pub fn format_numeric_value(value: f64) -> String {
    let rounded = (value * 100.0).round() / 100.0;
    if (rounded.fract()).abs() < f64::EPSILON {
        format!("{rounded:.0}")
    } else {
        format!("{rounded:.2}")
    }
}

fn infer_table_value_shape(values: &[String]) -> TableSummaryValueShape {
    if values.is_empty() {
        return TableSummaryValueShape::Label;
    }

    let total = values.len() as f64;
    let url_ratio =
        values.iter().filter(|value| looks_like_url_value(value)).count() as f64 / total;
    if url_ratio >= 0.5 {
        return TableSummaryValueShape::Url;
    }

    let identifier_ratio =
        values.iter().filter(|value| looks_like_identifier_value(value)).count() as f64 / total;
    if identifier_ratio >= 0.6 {
        return TableSummaryValueShape::Identifier;
    }

    let narrative_ratio =
        values.iter().filter(|value| looks_like_narrative_value(value)).count() as f64 / total;
    if narrative_ratio >= 0.6 {
        return TableSummaryValueShape::Narrative;
    }

    TableSummaryValueShape::Label
}

fn infer_aggregation_priority(
    value_kind: TableSummaryValueKind,
    value_shape: TableSummaryValueShape,
    non_empty_count: usize,
    distinct_count: usize,
    most_frequent_count: usize,
    most_frequent_tie_count: usize,
) -> u8 {
    match value_kind {
        TableSummaryValueKind::Numeric => 3,
        TableSummaryValueKind::Categorical => match value_shape {
            TableSummaryValueShape::Url | TableSummaryValueShape::Identifier => 0,
            TableSummaryValueShape::Narrative => {
                usize::from(most_frequent_count > 1 && distinct_count < non_empty_count) as u8
            }
            TableSummaryValueShape::Label => {
                if most_frequent_count > 1 && most_frequent_tie_count > 0 {
                    3
                } else if distinct_count == non_empty_count {
                    2
                } else {
                    1
                }
            }
        },
    }
}

fn looks_like_url_value(value: &str) -> bool {
    let trimmed = value.trim().to_ascii_lowercase();
    trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("www.")
        || trimmed.contains("://")
}

fn looks_like_identifier_value(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.len() < 8 || trimmed.contains(' ') {
        return looks_like_structured_low_signal_value(trimmed);
    }
    if looks_like_structured_low_signal_value(trimmed) {
        return true;
    }
    let has_ascii_letter = trimmed.chars().any(|character| character.is_ascii_alphabetic());
    let has_ascii_digit = trimmed.chars().any(|character| character.is_ascii_digit());
    let ascii_symbol_count =
        trimmed.chars().filter(|character| matches!(character, '-' | '_' | '/' | '.')).count();
    (has_ascii_letter && has_ascii_digit) || ascii_symbol_count >= 2
}

fn looks_like_structured_low_signal_value(value: &str) -> bool {
    looks_like_email_value(value)
        || looks_like_phone_value(value)
        || looks_like_temporal_value(value)
}

fn looks_like_email_value(value: &str) -> bool {
    let trimmed = value.trim();
    let Some((local, domain)) = trimmed.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && domain
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '-'))
}

fn looks_like_phone_value(value: &str) -> bool {
    let trimmed = value.trim();
    let digit_count = trimmed.chars().filter(|character| character.is_ascii_digit()).count();
    digit_count >= 7
        && trimmed.chars().all(|character| {
            character.is_ascii_digit()
                || matches!(character, '+' | '-' | '(' | ')' | ' ' | '.' | 'x' | 'X')
        })
}

fn looks_like_temporal_value(value: &str) -> bool {
    let trimmed = value.trim();
    let digit_count = trimmed.chars().filter(|character| character.is_ascii_digit()).count();
    let separator_count = trimmed
        .chars()
        .filter(|character| matches!(character, '-' | '/' | ':' | 'T' | 'Z' | '+' | '.'))
        .count();
    digit_count >= 4
        && separator_count >= 1
        && trimmed.chars().all(|character| {
            character.is_ascii_digit()
                || matches!(character, '-' | '/' | ':' | 'T' | 'Z' | '+' | '.' | ' ')
        })
}

fn looks_like_narrative_value(value: &str) -> bool {
    let trimmed = value.trim();
    let token_count = trimmed.split_whitespace().count();
    token_count >= 3 || trimmed.chars().count() >= 24
}

#[cfg(test)]
mod tests {
    use super::{
        TableSummaryValueKind, TableSummaryValueShape, build_table_column_summaries,
        parse_table_column_summary, render_table_column_summary,
    };

    #[test]
    fn builds_numeric_and_categorical_table_summaries() {
        let summaries = build_table_column_summaries(
            Some("products"),
            None,
            &["Category".to_string(), "Stock".to_string()],
            &[
                vec!["Books".to_string(), "10".to_string()],
                vec!["Books".to_string(), "20".to_string()],
                vec!["Games".to_string(), "30".to_string()],
            ],
        );

        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].value_kind, TableSummaryValueKind::Categorical);
        assert_eq!(summaries[0].most_frequent_values, vec!["Books".to_string()]);
        assert_eq!(summaries[0].most_frequent_count, 2);
        assert_eq!(summaries[0].value_shape, TableSummaryValueShape::Label);
        assert_eq!(summaries[0].aggregation_priority, 3);
        assert_eq!(summaries[1].value_kind, TableSummaryValueKind::Numeric);
        assert_eq!(summaries[1].aggregation_priority, 3);
        assert_eq!(summaries[1].average, Some(20.0));
    }

    #[test]
    fn renders_and_parses_summary_round_trip() {
        let summary = build_table_column_summaries(
            Some("organizations"),
            None,
            &["Country".to_string()],
            &[vec!["Sweden".to_string()], vec!["Benin".to_string()], vec!["Sweden".to_string()]],
        )
        .into_iter()
        .next()
        .expect("summary");
        let rendered = render_table_column_summary(&summary);
        let parsed = parse_table_column_summary(&rendered).expect("parsed");

        assert_eq!(parsed.column_name, "Country");
        assert_eq!(parsed.most_frequent_values, vec!["Sweden".to_string()]);
        assert_eq!(parsed.most_frequent_count, 2);
        assert_eq!(parsed.value_shape, TableSummaryValueShape::Label);
        assert_eq!(parsed.aggregation_priority, 3);
    }

    #[test]
    fn classifies_identifier_url_and_narrative_columns_as_low_priority() {
        let summaries = build_table_column_summaries(
            Some("organizations"),
            None,
            &[
                "Organization Id".to_string(),
                "Website".to_string(),
                "Description".to_string(),
                "City".to_string(),
            ],
            &[
                vec![
                    "61BDeCfeFD0cEF5".to_string(),
                    "https://simon-pearson.com/".to_string(),
                    "Cross-platform secondary hub".to_string(),
                    "East Jill".to_string(),
                ],
                vec![
                    "0a0bfFbBbB8eC7c".to_string(),
                    "https://zimmerman.com/".to_string(),
                    "Adaptive bi-directional hierarchy".to_string(),
                    "New Tony".to_string(),
                ],
            ],
        );

        assert_eq!(summaries[0].value_shape, TableSummaryValueShape::Identifier);
        assert_eq!(summaries[0].aggregation_priority, 0);
        assert_eq!(summaries[1].value_shape, TableSummaryValueShape::Url);
        assert_eq!(summaries[1].aggregation_priority, 0);
        assert_eq!(summaries[2].value_shape, TableSummaryValueShape::Narrative);
        assert_eq!(summaries[2].aggregation_priority, 0);
        assert_eq!(summaries[3].value_shape, TableSummaryValueShape::Label);
        assert_eq!(summaries[3].aggregation_priority, 2);
    }

    #[test]
    fn classifies_email_phone_and_temporal_columns_as_identifier_shape() {
        let summaries = build_table_column_summaries(
            Some("people"),
            None,
            &["Email".to_string(), "Phone".to_string(), "Date of birth".to_string()],
            &[
                vec![
                    "alice@example.com".to_string(),
                    "+1 (555) 010-1111".to_string(),
                    "1945-10-26".to_string(),
                ],
                vec![
                    "bob@example.com".to_string(),
                    "+1 (555) 010-2222".to_string(),
                    "1950-03-14".to_string(),
                ],
            ],
        );

        assert!(
            summaries
                .iter()
                .all(|summary| summary.value_shape == TableSummaryValueShape::Identifier)
        );
    }
}
