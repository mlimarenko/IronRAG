use std::collections::{BTreeMap, BTreeSet};

use crate::domains::query_ir::QueryIR;
use crate::services::query::text_match::normalized_alnum_tokens;

use super::{
    RuntimeMatchedChunk,
    table_row_answer::{ParsedTableRow, parse_table_row_chunk},
    technical_answer_identity::{extend_focus_terms, has_complete_token_or_structural_match},
};

const TYPED_FOCUS_MIN_TOKEN_CHARS: usize = 2;
const RETRIEVAL_FOCUS_MIN_TOKEN_CHARS: usize = 3;

/// Selects the single table row best identified by question literals and cell values.
///
/// Returns `None` when the question has no usable literals, the strongest score is below the
/// established confidence floor, or multiple rows share the strongest score. Equal scores are
/// deliberately treated as ambiguous rather than resolved by row order.
pub(super) fn best_matching_table_row<'a>(
    question: &str,
    rows: &'a [ParsedTableRow],
) -> Option<&'a ParsedTableRow> {
    let literals = crate::services::query::planner::extract_keywords_preserving_case(question)
        .into_iter()
        .map(|token| token.to_lowercase())
        .filter(|token| token.len() >= 3)
        .collect::<Vec<_>>();
    if literals.is_empty() {
        return None;
    }

    let mut ranked = rows
        .iter()
        .map(|row| {
            let literal_score = literals
                .iter()
                .filter(|literal| row.flattened_text.contains(literal.as_str()))
                .map(|literal| {
                    if literal.contains('@')
                        || literal.contains('.')
                        || literal.chars().any(|character| character.is_ascii_digit())
                    {
                        12usize
                    } else {
                        3usize
                    }
                })
                .sum::<usize>();
            let matched_cell_score =
                row_identifier_headers_from_question(question, row).len().saturating_mul(12);
            let score = literal_score.saturating_add(matched_cell_score);
            (row, score)
        })
        .filter(|(_, score)| *score > 0)
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right.1.cmp(&left.1).then_with(|| left.0.row_number.cmp(&right.0.row_number))
    });
    let (best_row, best_score) = ranked.first().copied()?;
    if best_score < 6 {
        return None;
    }
    if ranked.get(1).is_some_and(|(_, second_score)| *second_score == best_score) {
        return None;
    }
    Some(best_row)
}

/// Returns normalized headers for cells whose values are present in the question.
///
/// Values shorter than three characters do not establish row identity. Headers retain their
/// source-field order so rendering can preserve the existing deterministic answer order.
pub(super) fn row_identifier_headers_from_question(
    question: &str,
    row: &ParsedTableRow,
) -> Vec<String> {
    let lowered = question.to_lowercase();
    let question_tokens = normalized_alnum_tokens(question, 3);
    row.fields
        .iter()
        .filter(|(_, value)| {
            let normalized_value = value.trim().to_lowercase();
            if normalized_value.chars().count() < 3 {
                return false;
            }
            lowered.contains(&normalized_value)
                || normalized_alnum_tokens(value, 3)
                    .iter()
                    .any(|token| question_tokens.contains(token))
        })
        .map(|(header, _)| normalize_table_header(header))
        .collect()
}

/// Reports whether question-matched value tokens distinguish this row inside its table.
///
/// Rows are compared only within the same document, sheet, and optional table name, with duplicate
/// row numbers collapsed. The contract requires at least two distinct rows and at least one matched
/// token that is not present in every row; otherwise the identity is intentionally ambiguous.
pub(super) fn row_identifier_match_is_distinctive_within_table(
    question: &str,
    row: &ParsedTableRow,
    rows: &[ParsedTableRow],
) -> bool {
    let question_tokens = normalized_alnum_tokens(question, 3);
    let matched_value_tokens = row
        .fields
        .iter()
        .flat_map(|(_, value)| normalized_alnum_tokens(value, 3))
        .filter(|token| question_tokens.contains(token))
        .collect::<BTreeSet<_>>();
    if matched_value_tokens.is_empty() {
        return false;
    }

    let mut table_rows = BTreeMap::<usize, &ParsedTableRow>::new();
    for candidate in rows.iter().filter(|candidate| {
        candidate.document_id == row.document_id
            && candidate.sheet_name == row.sheet_name
            && candidate.table_name == row.table_name
    }) {
        table_rows.entry(candidate.row_number).or_insert(candidate);
    }
    let table_row_count = table_rows.len();
    table_row_count >= 2
        && matched_value_tokens.iter().any(|token| {
            table_rows
                .values()
                .filter(|candidate| table_row_contains_value_token(candidate, token))
                .count()
                < table_row_count
        })
}

/// Requires a selected row to carry the least-common complete focus tokens among its peer rows.
///
/// Entity/literal focus is proved against peer rows. Retrieval focus uses parsed cells plus
/// non-table evidence to ignore question scaffolding and row-protocol metadata, then requires the
/// selected row to carry every least-frequency term. Missing IR or empty focus is a no-op.
pub(super) fn selected_row_carries_query_focus(
    ir: Option<&QueryIR>,
    selected_row: &ParsedTableRow,
    rows: &[ParsedTableRow],
    chunks: &[&RuntimeMatchedChunk],
) -> bool {
    let Some(ir) = ir else {
        return true;
    };
    let mut typed_focus_tokens = BTreeSet::new();
    for entity in &ir.target_entities {
        extend_focus_terms(&mut typed_focus_tokens, &entity.label, TYPED_FOCUS_MIN_TOKEN_CHARS);
    }
    for literal in &ir.literal_constraints {
        extend_focus_terms(&mut typed_focus_tokens, &literal.text, TYPED_FOCUS_MIN_TOKEN_CHARS);
    }
    let mut retrieval_focus_tokens = BTreeSet::new();
    if let Some(retrieval_query) = ir.retrieval_query.as_deref() {
        extend_focus_terms(
            &mut retrieval_focus_tokens,
            retrieval_query,
            RETRIEVAL_FOCUS_MIN_TOKEN_CHARS,
        );
    }
    if typed_focus_tokens.is_empty() && retrieval_focus_tokens.is_empty() {
        return true;
    }

    let mut table_rows = BTreeMap::<usize, &ParsedTableRow>::new();
    for row in rows.iter().filter(|row| {
        row.document_id == selected_row.document_id
            && row.sheet_name == selected_row.sheet_name
            && row.table_name == selected_row.table_name
    }) {
        table_rows.entry(row.row_number).or_insert(row);
    }
    typed_focus_is_addressed(&typed_focus_tokens, selected_row, &table_rows)
        && retrieval_focus_is_addressed(
            &retrieval_focus_tokens,
            selected_row,
            rows,
            &table_rows,
            chunks,
        )
}

fn typed_focus_is_addressed(
    focus_tokens: &BTreeSet<String>,
    selected_row: &ParsedTableRow,
    table_rows: &BTreeMap<usize, &ParsedTableRow>,
) -> bool {
    if focus_tokens.is_empty() {
        return true;
    }
    let frequencies = focus_tokens
        .iter()
        .map(|token| {
            let carrier_count =
                table_rows.values().filter(|row| table_row_contains_focus_term(row, token)).count();
            (token, carrier_count)
        })
        .collect::<Vec<_>>();
    if frequencies.iter().any(|(_, count)| *count == 0) {
        return false;
    }
    let Some(minimum_frequency) = frequencies.iter().map(|(_, count)| *count).min() else {
        return false;
    };
    frequencies.into_iter().any(|(token, count)| {
        count == minimum_frequency && table_row_contains_focus_term(selected_row, token)
    })
}

fn retrieval_focus_is_addressed(
    focus_tokens: &BTreeSet<String>,
    selected_row: &ParsedTableRow,
    evidence_rows: &[ParsedTableRow],
    table_rows: &BTreeMap<usize, &ParsedTableRow>,
    chunks: &[&RuntimeMatchedChunk],
) -> bool {
    if focus_tokens.is_empty() {
        return true;
    }
    let frequencies = focus_tokens
        .iter()
        .filter_map(|token| {
            let appears_in_evidence =
                evidence_rows.iter().any(|row| table_row_contains_focus_term(row, token))
                    || chunks.iter().any(|chunk| {
                        parse_table_row_chunk(chunk).is_none()
                            && [chunk.source_text.as_str(), chunk.excerpt.as_str()]
                                .into_iter()
                                .any(|field| has_complete_token_or_structural_match(field, token))
                    });
            appears_in_evidence.then(|| {
                let carrier_count = table_rows
                    .values()
                    .filter(|row| table_row_contains_focus_term(row, token))
                    .count();
                (token, carrier_count)
            })
        })
        .collect::<Vec<_>>();
    let Some(minimum_frequency) = frequencies.iter().map(|(_, count)| *count).min() else {
        return true;
    };
    frequencies
        .into_iter()
        .filter(|(_, count)| *count == minimum_frequency)
        .all(|(token, _)| table_row_contains_focus_term(selected_row, token))
}

fn table_row_contains_focus_term(row: &ParsedTableRow, term: &str) -> bool {
    row.fields.iter().any(|(header, value)| {
        has_complete_token_or_structural_match(header, term)
            || has_complete_token_or_structural_match(value, term)
    })
}

fn table_row_contains_value_token(row: &ParsedTableRow, token: &str) -> bool {
    row.fields.iter().any(|(_, value)| normalized_alnum_tokens(value, 3).contains(token))
}

/// Returns normalized row headers referenced by the question, preserving source-field order.
///
/// Matching accepts a normalized full-header occurrence, a complete token match, or the existing
/// partial match for multi-token headers. It does not stem or otherwise expand question tokens.
pub(super) fn requested_table_headers(question: &str, row: &ParsedTableRow) -> Vec<String> {
    let lowered = question.to_lowercase();
    let question_tokens = normalized_alnum_tokens(question, 2);
    row.fields
        .iter()
        .filter(|(header, _)| table_header_matches_question(header, &lowered, &question_tokens))
        .map(|(header, _)| normalize_table_header(header))
        .collect()
}

/// Normalizes a source header for identity comparison without altering internal characters.
pub(super) fn normalize_table_header(value: &str) -> String {
    value.trim().to_lowercase()
}

fn table_header_matches_question(
    header: &str,
    lowered_question: &str,
    question_tokens: &BTreeSet<String>,
) -> bool {
    let normalized_header = normalize_table_header(header);
    if lowered_question.contains(&normalized_header) {
        return true;
    }
    let tokenized_header = split_table_header_for_token_match(header);
    let header_tokens = normalized_alnum_tokens(&tokenized_header, 2);
    if header_tokens.is_empty() {
        return false;
    }
    if header_tokens.iter().all(|token| question_tokens.contains(token)) {
        return true;
    }
    header_tokens.len() >= 2
        && header_tokens.iter().filter(|token| question_tokens.contains(*token)).count() >= 1
}

fn split_table_header_for_token_match(value: &str) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    let mut out = String::with_capacity(value.len());
    for (idx, ch) in chars.iter().copied().enumerate() {
        if idx > 0 && ch.is_uppercase() {
            let prev = chars[idx - 1];
            let next = chars.get(idx + 1).copied();
            if prev.is_lowercase()
                || prev.is_numeric()
                || (prev.is_uppercase() && next.is_some_and(|candidate| candidate.is_lowercase()))
            {
                out.push(' ');
            }
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
#[path = "table_row_identity_tests.rs"]
mod tests;
