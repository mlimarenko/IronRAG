use std::{collections::BTreeSet, sync::LazyLock};

use uuid::Uuid;

use crate::domains::query_ir::{QueryAct, QueryIR, QueryScope};
use crate::infra::arangodb::document_store::KnowledgeStructuredBlockRow;

use super::question_intent::{
    QuestionIntent, canonical_target_type_tag, classify_question_or_ir_intents, has_question_intent,
};
#[cfg(test)]
use super::technical_literals::technical_chunk_selection_score;
use super::technical_literals::{
    extract_config_section_literals, extract_explicit_path_literals,
    extract_package_command_literals, extract_parameter_literals,
};
use super::technical_parameter_answer::build_exact_parameter_answer;
use super::technical_url_answer::build_exact_url_answer;
use super::{CanonicalAnswerEvidence, RuntimeMatchedChunk};
use crate::services::query::text_match::{
    near_token_match, near_token_overlap_count, normalized_alnum_tokens,
};

static ERROR_CODE_ASSIGNMENT_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used)]
    regex::RegexBuilder::new(
        r"^\s*([A-Za-z][A-Za-z0-9_.-]{2,160})\s*=\s*((?:-?[0-9]+(?:[.][0-9]+)?\s*[,;]\s*)*-?[0-9]+(?:[.][0-9]+)?)\s*$",
    )
    .case_insensitive(true)
    .multi_line(true)
    .build()
    .expect("error-code assignment regex must compile")
});

static ERROR_CODE_MAPPING_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used)]
    regex::RegexBuilder::new(r"^\s*(-?[0-9]+(?:[.][0-9]+)?)\s*=\s*(\S[^\r\n]{0,160})$")
        .multi_line(true)
        .build()
        .expect("error-code mapping regex must compile")
});

static CONFIG_ASSIGNMENT_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used)]
    regex::RegexBuilder::new(
        r"(?:^|[;\r\n])\s*[#;]?\s*([A-Za-z][A-Za-z0-9_.-]{2,160})\s*=\s*([^;\r\n]{1,220})",
    )
    .build()
    .expect("config assignment regex must compile")
});

static MARKDOWN_TABLE_FIRST_CELL_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used)]
    regex::RegexBuilder::new(r"(?m)^\s*\|\s*`?([A-Za-z][A-Za-z0-9_.-]{1,160})`?\s*\|")
        .build()
        .expect("markdown table first-cell regex must compile")
});

pub(super) fn build_exact_technical_literal_answer(
    question: &str,
    query_ir: &QueryIR,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if !query_ir_allows_exact_technical_literal_answer(query_ir) {
        return None;
    }
    build_module_configuration_setup_answer(question, query_ir, evidence, chunks)
        .or_else(|| build_transport_config_assignment_answer(question, query_ir, chunks))
        .or_else(|| build_error_code_mapping_answer(question, query_ir, chunks))
        .or_else(|| build_exact_parameter_answer(question, query_ir, evidence, chunks))
        .or_else(|| build_exact_url_answer(question, query_ir, evidence, chunks))
}

fn query_ir_allows_exact_technical_literal_answer(query_ir: &QueryIR) -> bool {
    !classify_question_or_ir_intents("", query_ir).is_empty()
        || query_ir_requests_module_configuration_setup(query_ir)
}

pub(super) fn build_module_configuration_setup_answer(
    question: &str,
    query_ir: &QueryIR,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if module_configuration_inventory_question(question, query_ir) {
        return None;
    }
    let explicitly_requested = query_ir_requests_module_configuration_setup(query_ir);
    if !explicitly_requested
        && !query_ir_allows_evidence_driven_module_configuration_setup(question, query_ir)
    {
        return None;
    }
    let scoped_chunks = module_configuration_scope_chunks(question, chunks);
    let candidate_chunks = if scoped_chunks.is_empty() { chunks } else { scoped_chunks.as_slice() };
    let low_confidence_evidence_driven = (low_confidence_unfocused_configuration_ir(query_ir)
        && query_text_has_configuration_setup_anchor(question))
        || low_confidence_structural_configuration_ir(query_ir);
    if low_confidence_evidence_driven
        && !module_configuration_candidate_matches_question(question, candidate_chunks)
    {
        return None;
    }
    let packages = collect_module_packages_with_structured_blocks(
        candidate_chunks,
        &evidence.structured_blocks,
        4,
    );
    let config_paths = collect_configuration_paths_with_structured_blocks(
        candidate_chunks,
        &evidence.structured_blocks,
        16,
    );
    let config_path = select_module_configuration_path_from_paths(&config_paths, &packages)?;
    let config_sections = collect_configuration_sections_with_structured_blocks(
        candidate_chunks,
        &evidence.structured_blocks,
        8,
    );
    let parameter_focus_terms = query_ir
        .literal_constraints
        .iter()
        .map(|literal| literal.text.as_str())
        .collect::<Vec<_>>();
    let parameters = collect_visible_parameter_rows_with_structured_blocks(
        candidate_chunks,
        &evidence.structured_blocks,
        &parameter_focus_terms,
        32,
    );
    if !explicitly_requested
        && !evidence_supports_module_configuration_answer(
            &packages,
            &config_paths,
            &config_sections,
            &parameters,
        )
    {
        return None;
    }
    if packages.is_empty() && parameters.is_empty() {
        return None;
    }
    let document_label = candidate_chunks
        .iter()
        .map(|chunk| chunk.document_label.trim())
        .find(|label| !label.is_empty())
        .unwrap_or("source");
    // Language-neutral deterministic structure. The grounded values — source
    // document, package commands, configuration paths, parameter rows — carry
    // the meaning and are rendered verbatim from the evidence (whatever the
    // source language is). There are NO hardcoded natural-language section
    // labels here: phrasing and language are the LLM's responsibility, never a
    // code-side per-language dictionary. Markdown grouping (a code-span header
    // plus blank-line-separated bullet blocks) keeps the sections distinct
    // without embedding prose in any language.
    let mut answer = format!("`{document_label}`\n");
    if !packages.is_empty() {
        for package in packages {
            answer.push_str(&format!("\n- `{package}`"));
        }
        answer.push('\n');
    }
    let additional_config_paths =
        config_paths.iter().filter(|path| path.as_str() != config_path).take(6).collect::<Vec<_>>();
    answer.push_str(&format!("\n- `{config_path}`"));
    for path in additional_config_paths {
        answer.push_str(&format!("\n- `{path}`"));
    }
    answer.push('\n');
    if !config_sections.is_empty() {
        for section in config_sections {
            answer.push_str(&format!("\n- `{section}`"));
        }
        answer.push('\n');
    }
    if !parameters.is_empty() {
        for parameter in parameters {
            answer.push_str("\n- ");
            answer.push_str(&render_parameter_bullet(&parameter));
        }
        answer.push('\n');
    }
    Some(answer.trim_end().to_string())
}

fn query_ir_requests_module_configuration_setup(query_ir: &QueryIR) -> bool {
    if !matches!(
        query_ir.act,
        QueryAct::ConfigureHow | QueryAct::Describe | QueryAct::RetrieveValue
    ) {
        return false;
    }
    let has_focus_signal = query_ir.document_focus.is_some()
        || !query_ir.target_entities.is_empty()
        || !query_ir.literal_constraints.is_empty()
        || !query_ir.conversation_refs.is_empty();
    let requests_configuration = query_ir.target_types.iter().any(|target_type| {
        matches!(
            canonical_target_type_tag(target_type).as_str(),
            "configuration_file" | "config_key"
        )
    });
    let requests_module_or_parameter = query_ir.target_types.iter().any(|target_type| {
        matches!(canonical_target_type_tag(target_type).as_str(), "package" | "parameter")
    });
    requests_configuration && (requests_module_or_parameter || has_focus_signal)
}

fn module_configuration_inventory_question(_question: &str, query_ir: &QueryIR) -> bool {
    if matches!(query_ir.act, QueryAct::ConfigureHow) {
        return false;
    }
    query_ir.target_types.iter().any(|target_type| {
        matches!(
            canonical_target_type_tag(target_type).as_str(),
            "port"
                | "service"
                | "endpoint"
                | "http_method"
                | "error_code"
                | "relationship"
                | "protocol"
        )
    })
}

fn query_ir_allows_evidence_driven_module_configuration_setup(
    question: &str,
    query_ir: &QueryIR,
) -> bool {
    (matches!(query_ir.act, QueryAct::ConfigureHow)
        && (matches!(query_ir.scope, crate::domains::query_ir::QueryScope::SingleDocument)
            || query_ir.document_focus.is_some()
            || !query_ir.target_entities.is_empty()))
        || (low_confidence_unfocused_configuration_ir(query_ir)
            && query_text_has_configuration_setup_anchor(question))
        || low_confidence_structural_configuration_ir(query_ir)
}

fn low_confidence_unfocused_configuration_ir(query_ir: &QueryIR) -> bool {
    query_ir.confidence <= 0.35
        && matches!(query_ir.act, QueryAct::Describe | QueryAct::RetrieveValue)
        && query_ir.source_slice.is_none()
        && query_ir.document_focus.is_none()
        && query_ir.target_types.is_empty()
        && query_ir.target_entities.is_empty()
        && query_ir.literal_constraints.is_empty()
        && query_ir.temporal_constraints.is_empty()
        && query_ir.conversation_refs.is_empty()
}

fn low_confidence_structural_configuration_ir(query_ir: &QueryIR) -> bool {
    query_ir.confidence <= 0.35
        && matches!(query_ir.scope, QueryScope::SingleDocument | QueryScope::MultiDocument)
        && matches!(
            query_ir.act,
            QueryAct::Describe | QueryAct::ConfigureHow | QueryAct::RetrieveValue
        )
        && query_ir.source_slice.is_none()
        && query_ir.document_focus.is_none()
        && query_ir.target_types.is_empty()
        && query_ir.comparison.is_none()
        && query_ir.temporal_constraints.is_empty()
        && query_ir.conversation_refs.is_empty()
        && (!query_ir.target_entities.is_empty() || !query_ir.literal_constraints.is_empty())
}

fn evidence_supports_module_configuration_answer(
    packages: &[String],
    config_paths: &[String],
    config_sections: &[String],
    parameters: &[String],
) -> bool {
    (!packages.is_empty() && !config_paths.is_empty())
        || (!config_paths.is_empty() && !config_sections.is_empty() && parameters.len() >= 2)
        || (!config_paths.is_empty() && parameters.len() >= 4)
}

fn query_text_has_configuration_setup_anchor(question: &str) -> bool {
    !extract_explicit_path_literals(question, 1).is_empty()
        || !extract_config_section_literals(question, 1).is_empty()
        || extract_parameter_literals(question, 2).len() >= 2
}

fn module_configuration_candidate_matches_question(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> bool {
    let question_tokens = normalized_alnum_tokens(question, 3);
    if question_tokens.len() < 2 {
        return false;
    }
    let question_uppercase_codes = uppercase_code_tokens(question);
    chunks.iter().any(|chunk| {
        let label_tokens = normalized_alnum_tokens(&chunk.document_label, 3);
        let overlap = near_token_overlap_count(&question_tokens, &label_tokens);
        if overlap >= 3 {
            return true;
        }
        if overlap < 2 {
            return false;
        }
        if document_label_has_distinctive_overlap(&question_tokens, &label_tokens) {
            return true;
        }
        if question_uppercase_codes.is_empty() {
            return true;
        }
        let label_uppercase_codes = uppercase_code_tokens(&chunk.document_label);
        question_uppercase_codes.iter().any(|code| label_uppercase_codes.contains(code))
    })
}

fn document_label_has_distinctive_overlap(
    question_tokens: &BTreeSet<String>,
    label_tokens: &BTreeSet<String>,
) -> bool {
    question_tokens.iter().any(|question_token| {
        question_token.chars().count() >= 8
            && label_tokens.iter().any(|label_token| near_token_match(question_token, label_token))
    })
}

fn uppercase_code_tokens(text: &str) -> BTreeSet<String> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .filter_map(|token| {
            let letters = token.chars().filter(|ch| ch.is_alphabetic()).collect::<Vec<_>>();
            if letters.len() < 2 || !letters.iter().all(|ch| ch.is_uppercase()) {
                return None;
            }
            Some(token.chars().flat_map(char::to_lowercase).collect::<String>())
        })
        .collect()
}

fn module_configuration_scope_chunks(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Vec<RuntimeMatchedChunk> {
    if let Some(document_id) = module_configuration_setup_document_id(chunks) {
        return chunks.iter().filter(|chunk| chunk.document_id == document_id).cloned().collect();
    }
    let Some(document_id) = super::focused_answer_document_id(question, chunks) else {
        return Vec::new();
    };
    chunks.iter().filter(|chunk| chunk.document_id == document_id).cloned().collect::<Vec<_>>()
}

fn module_configuration_setup_document_id(chunks: &[RuntimeMatchedChunk]) -> Option<Uuid> {
    let mut by_document = std::collections::BTreeMap::<Uuid, Vec<RuntimeMatchedChunk>>::new();
    for chunk in chunks {
        by_document.entry(chunk.document_id).or_default().push(chunk.clone());
    }
    by_document
        .into_iter()
        .filter_map(|(document_id, document_chunks)| {
            let packages = collect_module_packages(&document_chunks, 4);
            let path_count = collect_configuration_paths(&document_chunks, 8).len();
            if packages.is_empty() || path_count == 0 {
                return None;
            }
            let parameter_count = collect_visible_parameter_rows(&document_chunks, &[], 32).len();
            let earliest_chunk =
                document_chunks.iter().map(|chunk| chunk.chunk_index).min().unwrap_or_default();
            let score = packages
                .len()
                .saturating_mul(32)
                .saturating_add(path_count.saturating_mul(16))
                .saturating_add(parameter_count);
            Some((score, std::cmp::Reverse(earliest_chunk), document_id))
        })
        .max()
        .map(|(_, _, document_id)| document_id)
}

fn collect_module_packages(chunks: &[RuntimeMatchedChunk], limit: usize) -> Vec<String> {
    collect_module_packages_from_texts(chunks.iter().map(runtime_chunk_text), limit)
}

fn collect_module_packages_with_structured_blocks(
    chunks: &[RuntimeMatchedChunk],
    blocks: &[KnowledgeStructuredBlockRow],
    limit: usize,
) -> Vec<String> {
    let chunk_document_ids =
        chunks.iter().map(|chunk| chunk.document_id).collect::<std::collections::BTreeSet<_>>();
    collect_module_packages_from_texts(
        chunks.iter().map(runtime_chunk_text).chain(blocks.iter().filter_map(|block| {
            (chunk_document_ids.is_empty() || chunk_document_ids.contains(&block.document_id))
                .then_some(block.text.as_str())
        })),
        limit,
    )
}

fn collect_module_packages_from_texts<'a>(
    texts: impl IntoIterator<Item = &'a str>,
    limit: usize,
) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut packages = Vec::new();
    for text in texts {
        for package in extract_package_command_literals(text, limit) {
            if seen.insert(package.to_ascii_lowercase()) {
                packages.push(package);
                if packages.len() >= limit {
                    return packages;
                }
            }
        }
    }
    packages
}

fn select_module_configuration_path_from_paths(
    paths: &[String],
    packages: &[String],
) -> Option<String> {
    let package_stems =
        packages.iter().filter_map(|package| package_module_stem(package)).collect::<Vec<_>>();
    paths
        .iter()
        .find(|path| {
            let lowered = path.to_ascii_lowercase();
            package_stems.iter().any(|stem| lowered.contains(stem))
        })
        .cloned()
        .or_else(|| (paths.len() == 1).then(|| paths[0].clone()))
}

fn collect_configuration_paths(chunks: &[RuntimeMatchedChunk], limit: usize) -> Vec<String> {
    collect_configuration_paths_from_texts(chunks.iter().map(runtime_chunk_text), limit)
}

fn collect_configuration_paths_with_structured_blocks(
    chunks: &[RuntimeMatchedChunk],
    blocks: &[KnowledgeStructuredBlockRow],
    limit: usize,
) -> Vec<String> {
    let chunk_document_ids =
        chunks.iter().map(|chunk| chunk.document_id).collect::<std::collections::BTreeSet<_>>();
    collect_configuration_paths_from_texts(
        chunks.iter().map(runtime_chunk_text).chain(blocks.iter().filter_map(|block| {
            (chunk_document_ids.is_empty() || chunk_document_ids.contains(&block.document_id))
                .then_some(block.text.as_str())
        })),
        limit,
    )
}

fn collect_configuration_paths_from_texts<'a>(
    texts: impl IntoIterator<Item = &'a str>,
    limit: usize,
) -> Vec<String> {
    let mut paths = Vec::<String>::new();
    let mut seen = std::collections::BTreeSet::new();
    for text in texts {
        for path in extract_explicit_path_literals(text, limit) {
            if !is_configuration_path(&path) || !seen.insert(path.to_ascii_lowercase()) {
                continue;
            }
            paths.push(path);
            if paths.len() >= limit {
                return paths;
            }
        }
    }
    paths
}

fn collect_configuration_sections_with_structured_blocks(
    chunks: &[RuntimeMatchedChunk],
    blocks: &[KnowledgeStructuredBlockRow],
    limit: usize,
) -> Vec<String> {
    let chunk_document_ids =
        chunks.iter().map(|chunk| chunk.document_id).collect::<std::collections::BTreeSet<_>>();
    collect_configuration_sections_from_texts(
        chunks.iter().map(runtime_chunk_text).chain(blocks.iter().filter_map(|block| {
            (chunk_document_ids.is_empty() || chunk_document_ids.contains(&block.document_id))
                .then_some(block.text.as_str())
        })),
        limit,
    )
}

fn collect_configuration_sections_from_texts<'a>(
    texts: impl IntoIterator<Item = &'a str>,
    limit: usize,
) -> Vec<String> {
    let mut sections = Vec::<String>::new();
    let mut seen = std::collections::BTreeSet::new();
    for text in texts {
        for section in extract_config_section_literals(text, limit) {
            if seen.insert(section.to_ascii_lowercase()) {
                sections.push(section);
                if sections.len() >= limit {
                    return sections;
                }
            }
        }
    }
    sections
}

fn package_module_stem(package: &str) -> Option<String> {
    package
        .split(|ch: char| !(ch.is_ascii_alphanumeric()))
        .rev()
        .find(|part| part.chars().count() >= 4)
        .map(str::to_ascii_lowercase)
}

fn is_configuration_path(path: &str) -> bool {
    let lowered = path.to_ascii_lowercase();
    lowered.ends_with(".conf") || lowered.ends_with(".ini")
}

fn collect_visible_parameter_rows(
    chunks: &[RuntimeMatchedChunk],
    focus_terms: &[&str],
    limit: usize,
) -> Vec<String> {
    collect_visible_parameter_rows_from_sources(
        chunks
            .iter()
            .enumerate()
            .map(|(rank, chunk)| (rank, runtime_chunk_text(chunk).to_string())),
        focus_terms,
        limit,
    )
}

fn collect_visible_parameter_rows_with_structured_blocks(
    chunks: &[RuntimeMatchedChunk],
    blocks: &[KnowledgeStructuredBlockRow],
    focus_terms: &[&str],
    limit: usize,
) -> Vec<String> {
    let chunk_document_ids =
        chunks.iter().map(|chunk| chunk.document_id).collect::<std::collections::BTreeSet<_>>();
    let block_rank_offset = chunks.len().saturating_add(1);
    let chunk_sources = chunks
        .iter()
        .enumerate()
        .map(|(rank, chunk)| (rank, runtime_chunk_text(chunk).to_string()));
    let block_sources = blocks
        .iter()
        .filter(|block| {
            chunk_document_ids.is_empty() || chunk_document_ids.contains(&block.document_id)
        })
        .flat_map(|block| {
            let block_rank = block_rank_offset.saturating_add(block.ordinal.max(0) as usize);
            let mut texts = vec![(block_rank, block.text.clone())];
            if block.normalized_text != block.text {
                texts.push((block_rank.saturating_add(1), block.normalized_text.clone()));
            }
            texts
        });
    collect_visible_parameter_rows_from_sources(
        chunk_sources.chain(block_sources),
        focus_terms,
        limit,
    )
}

fn collect_visible_parameter_rows_from_sources(
    sources: impl IntoIterator<Item = (usize, String)>,
    focus_terms: &[&str],
    limit: usize,
) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut candidates = Vec::<(usize, usize, String, String)>::new();
    for (chunk_rank, text) in sources {
        for line in text.lines() {
            let Some(row) = parse_visible_parameter_row(line) else {
                continue;
            };
            if seen.insert(row.parameter_key.to_ascii_lowercase()) {
                let score = focused_parameter_row_score(&row, focus_terms);
                candidates.push((score, chunk_rank, row.parameter_key, row.rendered));
            }
        }
    }
    candidates.sort_by(
        |(left_score, left_rank, left_key, _), (right_score, right_rank, right_key, _)| {
            right_score
                .cmp(left_score)
                .then_with(|| left_rank.cmp(right_rank))
                .then_with(|| left_key.cmp(right_key))
        },
    );
    candidates.into_iter().take(limit).map(|(_, _, _, row)| row).collect()
}

struct VisibleParameterRow {
    parameter_key: String,
    rendered: String,
}

fn parse_visible_parameter_row(line: &str) -> Option<VisibleParameterRow> {
    parse_markdown_parameter_row(line)
        .or_else(|| parse_structured_table_parameter_row(line))
        .or_else(|| parse_config_assignment_parameter_row(line))
}

fn parse_markdown_parameter_row(line: &str) -> Option<VisibleParameterRow> {
    let capture = MARKDOWN_TABLE_FIRST_CELL_REGEX.captures(line)?;
    let parameter = capture.get(1)?.as_str().trim();
    Some(VisibleParameterRow {
        parameter_key: parameter.to_string(),
        rendered: render_delimited_table_row(line).unwrap_or_else(|| parameter.to_string()),
    })
}

fn parse_structured_table_parameter_row(line: &str) -> Option<VisibleParameterRow> {
    let cells =
        line.split(" | ").map(str::trim).filter(|cell| !cell.is_empty()).collect::<Vec<_>>();
    let row_position = cells.iter().position(|cell| {
        cell.strip_prefix("Row ")
            .is_some_and(|suffix| suffix.chars().next().is_some_and(|ch| ch.is_ascii_digit()))
    })?;
    let first_data_cell = cells.get(row_position.saturating_add(1))?;
    let parameter = first_data_cell
        .split_once(':')
        .map_or(*first_data_cell, |(_, value)| value)
        .trim()
        .trim_matches('`');
    if parameter.is_empty() {
        return None;
    }
    let rest_cells = cells
        .iter()
        .skip(row_position.saturating_add(2))
        .map(|cell| cell.trim_matches('`').trim())
        .filter(|cell| !cell.is_empty())
        .collect::<Vec<_>>();
    let mut rendered_parts = Vec::new();
    if let Some(value) = infer_boolean_assignment_value_from_cells(&rest_cells) {
        rendered_parts.push(format!("{parameter} = {value}"));
    } else {
        rendered_parts.push(parameter.to_string());
    }
    rendered_parts.extend(rest_cells.iter().map(|cell| (*cell).to_string()));
    let rendered = rendered_parts.join(" — ");
    Some(VisibleParameterRow { parameter_key: parameter.to_string(), rendered })
}

fn infer_boolean_assignment_value_from_cells(cells: &[&str]) -> Option<&'static str> {
    let mut last_value = None;
    let mut true_count = 0usize;
    let mut false_count = 0usize;
    for cell in cells {
        for token in cell.split(|ch: char| !ch.is_ascii_alphanumeric()) {
            if token.eq_ignore_ascii_case("true") {
                true_count = true_count.saturating_add(1);
                last_value = Some("true");
            } else if token.eq_ignore_ascii_case("false") {
                false_count = false_count.saturating_add(1);
                last_value = Some("false");
            }
        }
    }
    if true_count == 0 && false_count == 0 {
        return None;
    }
    let boolean_token_count = true_count.saturating_add(false_count);
    let carries_boolean_domain = true_count > 0 && false_count > 0;
    (carries_boolean_domain && boolean_token_count >= 3).then_some(last_value).flatten()
}

fn parse_config_assignment_parameter_row(line: &str) -> Option<VisibleParameterRow> {
    let capture = CONFIG_ASSIGNMENT_REGEX.captures(line)?;
    let parameter = capture.get(1)?.as_str().trim().trim_matches('`');
    let value = clean_config_assignment_value(capture.get(2)?.as_str());
    if parameter.is_empty() || value.is_empty() {
        return None;
    }
    Some(VisibleParameterRow {
        parameter_key: parameter.to_string(),
        rendered: format!("{parameter} = {value}"),
    })
}

fn focused_parameter_row_score(row: &VisibleParameterRow, focus_terms: &[&str]) -> usize {
    if focus_terms.is_empty() {
        return 0;
    }
    let haystack = format!("{} {}", row.parameter_key, row.rendered).to_ascii_lowercase();
    focus_terms
        .iter()
        .map(|term| term.trim().to_ascii_lowercase())
        .filter(|term| !term.is_empty() && haystack.contains(term))
        .count()
}

fn render_delimited_table_row(line: &str) -> Option<String> {
    let cells = line
        .split('|')
        .map(|cell| cell.trim().trim_matches('`').trim())
        .filter(|cell| !cell.is_empty() && !cell.chars().all(|ch| ch == '-' || ch == ':'))
        .collect::<Vec<_>>();
    if cells.is_empty() {
        return None;
    }
    let rest_cells = cells.iter().skip(1).copied().collect::<Vec<_>>();
    let mut rendered = if let Some(value) = infer_boolean_assignment_value_from_cells(&rest_cells) {
        format!("{} = {value}", cells[0])
    } else {
        cells[0].to_string()
    };
    for cell in cells.iter().skip(1) {
        rendered.push_str(" — ");
        rendered.push_str(cell);
    }
    Some(rendered)
}

fn render_parameter_bullet(row: &str) -> String {
    let row = row.trim();
    if row.is_empty() {
        return String::new();
    }
    if let Some((parameter, value)) = row.split_once(" = ") {
        let (value, rest) = value
            .split_once(" — ")
            .map_or((value, ""), |(value, rest)| (value.trim(), rest.trim()));
        let rendered = format!(
            "`{} = {}`",
            parameter.trim().trim_matches('`'),
            value.trim().trim_matches('`')
        );
        if !rest.is_empty() {
            return format!("{rendered} — {rest}");
        }
        return format!(
            "`{} = {}`",
            parameter.trim().trim_matches('`'),
            value.trim().trim_matches('`')
        );
    }
    let Some((parameter, rest)) = row.split_once(" — ") else {
        return format!("`{}`", row.trim_matches('`'));
    };
    format!("`{}` — {}", parameter.trim().trim_matches('`'), rest.trim())
}

fn runtime_chunk_text(chunk: &RuntimeMatchedChunk) -> &str {
    if chunk.source_text.trim().is_empty() { &chunk.excerpt } else { &chunk.source_text }
}

#[derive(Debug)]
struct ConfigAssignmentCandidate {
    document_label: String,
    entries: Vec<(String, String)>,
    source_excerpt: String,
    score: usize,
}

#[derive(Debug)]
struct ErrorCodeMappingCandidate {
    document_label: String,
    parameter: String,
    codes: Vec<(String, Option<String>)>,
    score: usize,
}

fn build_transport_config_assignment_answer(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let intents = classify_question_or_ir_intents(question, query_ir);
    if query_ir_requests_port_inventory_without_connection(query_ir, &intents) {
        return None;
    }
    if !query_ir_requests_transport_config_assignment(query_ir, &intents) {
        return None;
    }

    let mut candidates = chunks
        .iter()
        .enumerate()
        .filter_map(|(rank, chunk)| config_assignment_candidate_from_chunk(rank, chunk))
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right.score.cmp(&left.score).then_with(|| left.document_label.cmp(&right.document_label))
    });
    let candidate = candidates.into_iter().next()?;
    let mut answer = format!("`{}`", candidate.document_label);
    for (name, value) in candidate.entries.into_iter().take(8) {
        answer.push_str(&format!("\n- `{name}` = `{value}`"));
    }
    if !candidate.source_excerpt.is_empty() {
        answer.push_str("\n```text\n");
        answer.push_str(&candidate.source_excerpt);
        answer.push_str("\n```");
    }
    Some(answer)
}

fn query_ir_requests_transport_config_assignment(
    query_ir: &QueryIR,
    intents: &[QuestionIntent],
) -> bool {
    if !matches!(query_ir.act, QueryAct::RetrieveValue | QueryAct::ConfigureHow) {
        return false;
    }

    let has_transport_intent = has_question_intent(intents, QuestionIntent::Port)
        || has_question_intent(intents, QuestionIntent::Protocol);
    let mut has_connection_target = false;
    let mut has_configuration_target = false;
    for target_type in &query_ir.target_types {
        match canonical_target_type_tag(target_type).as_str() {
            "connection" | "endpoint" | "url" | "base_url" | "wsdl" => {
                has_connection_target = true;
            }
            "configuration_file" | "config_key" | "env_var" | "parameter" => {
                has_configuration_target = true;
            }
            _ => {}
        }
    }

    has_connection_target || has_transport_intent && has_configuration_target
}

fn query_ir_requests_port_inventory_without_connection(
    query_ir: &QueryIR,
    intents: &[QuestionIntent],
) -> bool {
    let has_port = has_question_intent(intents, QuestionIntent::Port);
    let mut has_non_port_target = false;
    let mut has_connection = has_question_intent(intents, QuestionIntent::Protocol);
    for target_type in &query_ir.target_types {
        match canonical_target_type_tag(target_type).as_str() {
            "connection" | "endpoint" | "url" | "base_url" | "wsdl" | "protocol" => {
                has_connection = true;
            }
            "port" => {}
            _ => has_non_port_target = true,
        }
    }
    has_port && has_non_port_target && !has_connection
}

fn config_assignment_candidate_from_chunk(
    rank: usize,
    chunk: &RuntimeMatchedChunk,
) -> Option<ConfigAssignmentCandidate> {
    let text = if chunk.source_text.trim().is_empty() {
        chunk.excerpt.as_str()
    } else {
        chunk.source_text.as_str()
    };
    let mut seen = std::collections::HashSet::new();
    let entries = CONFIG_ASSIGNMENT_REGEX
        .captures_iter(text)
        .filter_map(|capture| {
            let name = capture.get(1)?.as_str().trim().trim_matches('`');
            let value = clean_config_assignment_value(capture.get(2)?.as_str());
            if name.is_empty()
                || value.is_empty()
                || value.len() > 160
                || !seen.insert(name.to_ascii_lowercase())
            {
                return None;
            }
            Some((name.to_string(), value))
        })
        .collect::<Vec<_>>();
    if entries.len() < 2 {
        return None;
    }
    let score = (64usize.saturating_sub(rank.min(64)) * 1000)
        + entries
            .iter()
            .map(|(name, value)| config_assignment_entry_score(name, value))
            .sum::<usize>()
            .saturating_add(entries.len() * 12);
    (score > 0).then_some(ConfigAssignmentCandidate {
        document_label: chunk.document_label.clone(),
        entries,
        source_excerpt: config_assignment_source_excerpt(text),
        score,
    })
}

fn clean_config_assignment_value(raw: &str) -> String {
    raw.trim()
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'')
        .trim_end_matches(['.', ',', ':'])
        .trim()
        .to_string()
}

fn config_assignment_entry_score(name: &str, value: &str) -> usize {
    let lowered_name = name.to_ascii_lowercase();
    let lowered_value = value.to_ascii_lowercase();
    usize::from(lowered_value.contains("://")) * 120
        + usize::from(url_value_contains_port(&lowered_value)) * 240
        + usize::from(lowered_name.contains("port")) * 80
        + usize::from(lowered_name.contains("url")) * 70
        + usize::from(lowered_name.contains("timeout")) * 40
        + usize::from(matches!(lowered_value.as_str(), "true" | "false")) * 20
        + usize::from(value.chars().all(|ch| ch.is_ascii_digit())) * 20
}

fn config_assignment_source_excerpt(text: &str) -> String {
    let cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut excerpt = cleaned.chars().take(1200).collect::<String>();
    if cleaned.chars().count() > 1200 {
        excerpt.push_str("...");
    }
    excerpt.replace("```", "'''")
}

fn url_value_contains_port(value: &str) -> bool {
    let Some(scheme_index) = value.find("://") else {
        return false;
    };
    let remainder = &value[(scheme_index + 3)..];
    let authority = remainder.split(['/', '?', '#']).next().unwrap_or_default();
    authority
        .rsplit_once(':')
        .is_some_and(|(_, port)| !port.is_empty() && port.chars().all(|ch| ch.is_ascii_digit()))
}

fn build_error_code_mapping_answer(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let intents = classify_question_or_ir_intents(question, query_ir);
    if !has_question_intent(&intents, QuestionIntent::ErrorCode) {
        return None;
    }
    let mut candidates =
        chunks.iter().flat_map(error_code_mapping_candidates_from_chunk).collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.document_label.cmp(&right.document_label))
            .then_with(|| left.parameter.cmp(&right.parameter))
    });
    let candidate = candidates.into_iter().next()?;
    let mut answer = format!("`{}` (`{}`)", candidate.parameter, candidate.document_label);
    for (code, message) in candidate.codes.into_iter().take(12) {
        match message {
            Some(message) if !message.trim().is_empty() => {
                answer.push_str(&format!("\n- `{code}` — {}.", clean_mapping_message(&message)));
            }
            _ => answer.push_str(&format!("\n- `{code}`.")),
        }
    }
    Some(answer)
}

fn error_code_mapping_candidates_from_chunk(
    chunk: &RuntimeMatchedChunk,
) -> Vec<ErrorCodeMappingCandidate> {
    let text = if chunk.source_text.trim().is_empty() {
        chunk.excerpt.as_str()
    } else {
        chunk.source_text.as_str()
    };
    let mappings = ERROR_CODE_MAPPING_REGEX
        .captures_iter(text)
        .filter_map(|capture| {
            let code = capture.get(1)?.as_str().trim().to_string();
            let message = capture.get(2)?.as_str().trim().to_string();
            Some((code, message))
        })
        .collect::<std::collections::HashMap<_, _>>();
    ERROR_CODE_ASSIGNMENT_REGEX
        .captures_iter(text)
        .filter_map(|capture| {
            let parameter = capture.get(1)?.as_str().trim().to_string();
            let codes = parse_error_code_list(capture.get(2)?.as_str());
            if codes.len() < 2 {
                return None;
            }
            let mapped_count = codes.iter().filter(|code| mappings.contains_key(*code)).count();
            if mapped_count == 0 && mappings.len() < 2 {
                return None;
            }
            let codes = codes
                .into_iter()
                .map(|code| {
                    let message = mappings.get(&code).cloned();
                    (code, message)
                })
                .collect::<Vec<_>>();
            let score = mapped_count * 100 + codes.len() * 10;
            Some(ErrorCodeMappingCandidate {
                document_label: chunk.document_label.clone(),
                parameter,
                codes,
                score,
            })
        })
        .collect()
}

fn parse_error_code_list(value: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    value
        .split([',', ';', ' ', '\t'])
        .filter_map(|part| {
            let code = part.trim();
            if code.is_empty() || !seen.insert(code.to_string()) {
                return None;
            }
            Some(code.to_string())
        })
        .collect()
}

fn clean_mapping_message(value: &str) -> String {
    value.trim().trim_end_matches(['.', ';', ',']).to_string()
}

#[cfg(test)]
pub(super) fn prioritized_technical_chunk_score(
    text: &str,
    candidate_document_id: Uuid,
    keywords: &[String],
    pagination_requested: bool,
    focused_document_id: Option<Uuid>,
) -> isize {
    technical_chunk_selection_score(text, keywords, pagination_requested)
        + document_focus_preference(candidate_document_id, focused_document_id)
}

pub(super) fn document_focus_preference(
    candidate_document_id: Uuid,
    focused_document_id: Option<Uuid>,
) -> isize {
    if focused_document_id == Some(candidate_document_id) { 24 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::query_ir::{
        DocumentHint, EntityMention, EntityRole, QueryLanguage, QueryScope, SourceSliceSpec,
        UnresolvedRef,
    };
    use crate::services::query::execution::RuntimeChunkScoreKind;

    #[test]
    fn module_configuration_setup_answer_prefers_package_owned_config_path() {
        let target_document_id = Uuid::now_v7();
        let distractor_document_id = Uuid::now_v7();
        let chunks = vec![
            runtime_chunk(
                distractor_document_id,
                0,
                "Widget Beta setup",
                r#"
Install the module:
sample-install beta-widget

Settings are stored in /opt/beta/display/display.ini.

| settingOne | string | Wrong setting |
"#,
            ),
            runtime_chunk(
                target_document_id,
                1,
                "Widget Alpha setup",
                r#"
Install the module:
sample-install alpha-connector

Configure the module:
sample-configure alpha-connector

Connector settings are stored in /opt/alpha/modules/connector/connector.conf under [Main].
"#,
            ),
            runtime_chunk(
                target_document_id,
                2,
                "Widget Alpha setup",
                r#"
| settingOne | string | First connector setting |
| settingTwo | string | Second connector setting |
| settingThree | string | Third connector setting |

Display settings use /opt/alpha/display/display.ini.
"#,
            ),
        ];
        let answer = build_module_configuration_setup_answer(
            "Configure Widget Alpha",
            &configuration_setup_ir(),
            &empty_evidence(),
            &chunks,
        )
        .expect("setup answer");

        assert!(answer.contains("`alpha-connector`"));
        assert!(answer.contains("`/opt/alpha/modules/connector/connector.conf`"));
        assert!(answer.contains("`settingOne`"));
        assert!(answer.contains("`settingTwo`"));
        // The package-owned config path must be selected as the primary, i.e.
        // rendered ahead of the unrelated display-settings path (which may still
        // surface as an additional bullet).
        let primary = answer
            .find("`/opt/alpha/modules/connector/connector.conf`")
            .expect("package-owned config path present");
        if let Some(display) = answer.find("`/opt/alpha/display/display.ini`") {
            assert!(primary < display, "package-owned config path must precede the display path");
        }
        assert!(!answer.contains("`beta-widget`"));
    }

    #[test]
    fn module_configuration_setup_answer_reads_structured_table_rows() {
        let document_id = Uuid::now_v7();
        let chunks = vec![
            runtime_chunk(
                document_id,
                1,
                "Widget Gamma setup",
                r#"
Install the module:
sample-install gamma-connector

Configure the module:
sample-configure gamma-connector

Connector settings are stored in /opt/gamma/modules/connector/connector.conf under [Main].
"#,
            ),
            runtime_chunk(
                document_id,
                2,
                "Widget Gamma setup",
                "Sheet: Connector settings | Row 1 | Name: endpointUrl | Type: string | Description: Service endpoint",
            ),
            runtime_chunk(
                document_id,
                3,
                "Widget Gamma setup",
                "Sheet: Connector settings | Row 2 | Name: partnerId | Type: string | Description: Partner identifier",
            ),
            runtime_chunk(
                document_id,
                4,
                "Widget Gamma setup",
                "Sheet: Connector settings | Row 3 | Name: secretKey | Type: string | Description: Shared secret",
            ),
        ];
        let mut query_ir = configuration_setup_ir();
        query_ir.literal_constraints = vec![literal_constraint("secret"), literal_constraint("id")];

        let answer = build_module_configuration_setup_answer(
            "Configure Widget Gamma",
            &query_ir,
            &empty_evidence(),
            &chunks,
        )
        .expect("setup answer");
        let partner_pos = answer.find("`partnerId`").expect("partnerId row");
        let secret_pos = answer.find("`secretKey`").expect("secretKey row");
        let endpoint_pos = answer.find("`endpointUrl`").expect("endpointUrl row");

        assert!(answer.contains("`/opt/gamma/modules/connector/connector.conf`"));
        assert!(partner_pos < endpoint_pos);
        assert!(secret_pos < endpoint_pos);
    }

    #[test]
    fn module_configuration_setup_answer_prefers_parameter_rich_setup_document() {
        let release_document_id = Uuid::now_v7();
        let setup_document_id = Uuid::now_v7();
        let chunks = vec![
            runtime_chunk(
                release_document_id,
                0,
                "Widget Alpha release note",
                r#"
Release note:
sample-install alpha-connector
Configuration file: /opt/alpha/modules/connector/connector.conf
"#,
            ),
            runtime_chunk(
                setup_document_id,
                1,
                "Widget Alpha administrator guide",
                r#"
Install the module:
sample-install alpha-connector

Connector settings are stored in /opt/alpha/modules/connector/connector.conf under [Main].
"#,
            ),
            runtime_chunk(
                setup_document_id,
                2,
                "Widget Alpha administrator guide",
                "Sheet: Connector settings | Row 1 | Name: partnerId | Type: string | Description: Partner identifier",
            ),
            runtime_chunk(
                setup_document_id,
                3,
                "Widget Alpha administrator guide",
                "Sheet: Connector settings | Row 2 | Name: secretKey | Type: string | Description: Shared secret",
            ),
        ];

        let answer = build_module_configuration_setup_answer(
            "Configure Widget Alpha",
            &configuration_setup_ir(),
            &empty_evidence(),
            &chunks,
        )
        .expect("setup answer");

        assert!(answer.contains("`Widget Alpha administrator guide`"));
        assert!(answer.contains("`partnerId`"));
        assert!(answer.contains("`secretKey`"));
        assert!(!answer.contains("`Widget Alpha release note`"));
    }

    #[test]
    fn module_configuration_setup_answer_uses_evidence_when_compiler_keeps_generic_target_type() {
        let document_id = Uuid::now_v7();
        let chunks = vec![
            runtime_chunk(document_id, 0, "Provider Alpha setup", "Overview of Provider Alpha."),
            runtime_chunk(
                document_id,
                1,
                "Provider Alpha setup",
                r#"
To use the module, install it with sample-install alpha-connector and run sample-configure alpha-connector.

The module configuration file is /opt/alpha/modules/connector/connector.conf.
| endpointUrl | string | Service endpoint |
| partnerId | string | Partner identifier |
"#,
            ),
        ];
        let mut query_ir = configuration_setup_ir();
        query_ir.target_types = vec!["procedure".to_string()];
        query_ir.document_focus = Some(DocumentHint { hint: "Provider Alpha".to_string() });

        let answer = build_module_configuration_setup_answer(
            "How do I configure Provider Alpha?",
            &query_ir,
            &empty_evidence(),
            &chunks,
        )
        .expect("setup answer");

        assert!(answer.contains("`alpha-connector`"));
        assert!(answer.contains("`/opt/alpha/modules/connector/connector.conf`"));
        assert!(answer.contains("`endpointUrl`"));
        assert!(answer.contains("`partnerId`"));
    }

    #[test]
    fn module_configuration_setup_answer_uses_structural_evidence_for_untyped_low_confidence_ir() {
        let document_id = Uuid::now_v7();
        let chunks = vec![
            runtime_chunk(
                document_id,
                1,
                "Provider Alpha setup",
                r#"
Install the module:
sample-install alpha-connector

The module configuration file is /opt/alpha/modules/connector/connector.conf.
[Main]
endpointUrl = https://alpha.example/api
partnerId = alpha-partner
"#,
            ),
            runtime_chunk(
                document_id,
                2,
                "Provider Alpha setup",
                r#"
| endpointUrl | string | Service endpoint |
| partnerId | string | Partner identifier |
| visible | boolean | true false | Display the code |
"#,
            ),
        ];
        let mut query_ir = configuration_setup_ir();
        query_ir.act = QueryAct::Describe;
        query_ir.target_types.clear();
        query_ir.target_entities.clear();
        query_ir.literal_constraints.clear();
        query_ir.temporal_constraints.clear();
        query_ir.document_focus = None;
        query_ir.source_slice = None;
        query_ir.conversation_refs.clear();
        query_ir.confidence = 0.25;

        let answer = build_module_configuration_setup_answer(
            "Provider Alpha setup `/opt/alpha/modules/connector/connector.conf` `endpointUrl`",
            &query_ir,
            &empty_evidence(),
            &chunks,
        )
        .expect("setup answer from structural evidence");

        assert!(answer.contains("`alpha-connector`"));
        assert!(answer.contains("`/opt/alpha/modules/connector/connector.conf`"));
        assert!(answer.contains("endpointUrl"));
        assert!(answer.contains("partnerId"));
        assert!(answer.contains("visible"));
    }

    #[test]
    fn low_confidence_untyped_ir_requires_query_anchor_before_setup_answer() {
        let document_id = Uuid::now_v7();
        let chunks = vec![runtime_chunk(
            document_id,
            1,
            "Provider Alpha setup",
            r#"
Install the module:
sample-install alpha-connector

The module configuration file is /opt/alpha/modules/connector/connector.conf.
[Main]
endpointUrl = https://alpha.example/api
partnerId = alpha-partner
"#,
        )];
        let mut query_ir = configuration_setup_ir();
        query_ir.act = QueryAct::Describe;
        query_ir.target_types.clear();
        query_ir.target_entities.clear();
        query_ir.literal_constraints.clear();
        query_ir.temporal_constraints.clear();
        query_ir.document_focus = None;
        query_ir.source_slice = None;
        query_ir.conversation_refs.clear();
        query_ir.confidence = 0.25;

        let answer = build_module_configuration_setup_answer(
            "Provider Alpha terminal loses payment confirmation",
            &query_ir,
            &empty_evidence(),
            &chunks,
        );

        assert!(answer.is_none(), "{answer:?}");
    }

    #[test]
    fn low_confidence_untyped_ir_does_not_turn_unmatched_config_evidence_into_setup_answer() {
        let document_id = Uuid::now_v7();
        let chunks = vec![runtime_chunk(
            document_id,
            1,
            "Provider Beta setup",
            r#"
Install the module:
sample-install beta-connector

The module configuration file is /opt/beta/modules/connector/connector.conf.
[Main]
endpointUrl = https://beta.example/api
partnerId = beta-partner
visible = true
"#,
        )];
        let mut query_ir = configuration_setup_ir();
        query_ir.act = QueryAct::Describe;
        query_ir.target_types.clear();
        query_ir.target_entities.clear();
        query_ir.literal_constraints.clear();
        query_ir.temporal_constraints.clear();
        query_ir.document_focus = None;
        query_ir.source_slice = None;
        query_ir.conversation_refs.clear();
        query_ir.confidence = 0.25;

        let answer = build_module_configuration_setup_answer(
            "Provider Alpha operational troubleshooting",
            &query_ir,
            &empty_evidence(),
            &chunks,
        );

        assert!(answer.is_none(), "{answer:?}");
    }

    #[test]
    fn low_confidence_untyped_ir_requires_shared_code_for_weak_label_overlap() {
        let document_id = Uuid::now_v7();
        let chunks = vec![runtime_chunk(
            document_id,
            1,
            "Cash link setup",
            r#"
Install the module:
sample-install cash-link

The module configuration file is /opt/cash/link/link.conf.
[Main]
endpointUrl = https://cash.example/api
partnerId = cash-partner
visible = true
"#,
        )];
        let mut query_ir = configuration_setup_ir();
        query_ir.act = QueryAct::Describe;
        query_ir.target_types.clear();
        query_ir.target_entities.clear();
        query_ir.literal_constraints.clear();
        query_ir.temporal_constraints.clear();
        query_ir.document_focus = None;
        query_ir.source_slice = None;
        query_ir.conversation_refs.clear();
        query_ir.confidence = 0.25;

        let answer = build_module_configuration_setup_answer(
            "PAY cash link troubleshooting",
            &query_ir,
            &empty_evidence(),
            &chunks,
        );

        assert!(answer.is_none(), "{answer:?}");
    }

    #[test]
    fn low_confidence_untyped_ir_accepts_shared_code_for_config_evidence() {
        let document_id = Uuid::now_v7();
        let chunks = vec![runtime_chunk(
            document_id,
            1,
            "PAY cash link setup",
            r#"
Install the module:
sample-install cash-link

The module configuration file is /opt/cash/link/link.conf.
[Main]
endpointUrl = https://cash.example/api
partnerId = cash-partner
visible = true
"#,
        )];
        let mut query_ir = configuration_setup_ir();
        query_ir.act = QueryAct::Describe;
        query_ir.target_types.clear();
        query_ir.target_entities.clear();
        query_ir.literal_constraints.clear();
        query_ir.temporal_constraints.clear();
        query_ir.document_focus = None;
        query_ir.source_slice = None;
        query_ir.conversation_refs.clear();
        query_ir.confidence = 0.25;

        let answer = build_module_configuration_setup_answer(
            "PAY cash link setup `/opt/cash/link/link.conf` `visible`",
            &query_ir,
            &empty_evidence(),
            &chunks,
        )
        .expect("setup answer");

        assert!(answer.contains("`cash-link`"));
        assert!(answer.contains("`/opt/cash/link/link.conf`"));
        assert!(answer.contains("visible"));
    }

    #[test]
    fn low_confidence_structural_ir_uses_matching_config_evidence() {
        let document_id = Uuid::now_v7();
        let chunks = vec![runtime_chunk(
            document_id,
            1,
            "Provider Alpha setup",
            r#"
Install the module:
sample-install alpha-connector

The module configuration file is /opt/alpha/modules/connector/connector.conf.
[Main]
endpointUrl = https://alpha.example/api
partnerId = alpha-partner
visible = true
"#,
        )];
        let mut query_ir = configuration_setup_ir();
        query_ir.act = QueryAct::Describe;
        query_ir.scope = QueryScope::MultiDocument;
        query_ir.target_types.clear();
        query_ir.target_entities =
            vec![EntityMention { label: "Provider Alpha".to_string(), role: EntityRole::Subject }];
        query_ir.literal_constraints.clear();
        query_ir.temporal_constraints.clear();
        query_ir.document_focus = None;
        query_ir.source_slice = None;
        query_ir.conversation_refs.clear();
        query_ir.confidence = 0.25;

        let answer = build_module_configuration_setup_answer(
            "Provider Alpha settings",
            &query_ir,
            &empty_evidence(),
            &chunks,
        )
        .expect("setup answer");

        assert!(answer.contains("`alpha-connector`"));
        assert!(answer.contains("`/opt/alpha/modules/connector/connector.conf`"));
        assert!(answer.contains("`visible = true`"));
    }

    #[test]
    fn low_confidence_structural_ir_rejects_unmatched_config_evidence() {
        let document_id = Uuid::now_v7();
        let chunks = vec![runtime_chunk(
            document_id,
            1,
            "Provider Beta setup",
            r#"
Install the module:
sample-install beta-connector

The module configuration file is /opt/beta/modules/connector/connector.conf.
[Main]
endpointUrl = https://beta.example/api
partnerId = beta-partner
visible = true
"#,
        )];
        let mut query_ir = configuration_setup_ir();
        query_ir.act = QueryAct::Describe;
        query_ir.scope = QueryScope::MultiDocument;
        query_ir.target_types.clear();
        query_ir.target_entities =
            vec![EntityMention { label: "Provider Alpha".to_string(), role: EntityRole::Subject }];
        query_ir.literal_constraints.clear();
        query_ir.temporal_constraints.clear();
        query_ir.document_focus = None;
        query_ir.source_slice = None;
        query_ir.conversation_refs.clear();
        query_ir.confidence = 0.25;

        let answer = build_module_configuration_setup_answer(
            "Provider Alpha settings",
            &query_ir,
            &empty_evidence(),
            &chunks,
        );

        assert!(answer.is_none(), "{answer:?}");
    }

    #[test]
    fn module_configuration_setup_answer_adds_structured_rows_for_focused_document() {
        let setup_document_id = Uuid::now_v7();
        let other_document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let chunks = vec![
            runtime_chunk(
                setup_document_id,
                0,
                "Provider Delta setup",
                r#"
Install the module:
sample-install delta-connector

The module configuration file is /opt/delta/modules/connector/connector.conf.
| endpointUrl | string | Service endpoint |
"#,
            ),
            runtime_chunk(
                setup_document_id,
                1,
                "Provider Delta setup",
                "Table Summary | Sheet: Connector settings | Row Count: 12",
            ),
        ];
        let mut block = crate::services::query::execution::types::sample_structured_block_row(
            Uuid::now_v7(),
            setup_document_id,
            revision_id,
        );
        block.ordinal = 12;
        block.text =
            "Sheet: Connector settings | Row 12 | Name: fillDetails | Type: boolean | Description: Send detailed payload"
                .to_string();
        block.normalized_text = block.text.clone();
        let mut unrelated_block =
            crate::services::query::execution::types::sample_structured_block_row(
                Uuid::now_v7(),
                other_document_id,
                Uuid::now_v7(),
            );
        unrelated_block.text =
            "Sheet: Other settings | Row 1 | Name: unrelatedSecret | Type: string".to_string();
        unrelated_block.normalized_text = unrelated_block.text.clone();
        let evidence = evidence_with_blocks(vec![block, unrelated_block]);

        let answer = build_module_configuration_setup_answer(
            "Configure Provider Delta parameters",
            &configuration_setup_ir(),
            &evidence,
            &chunks,
        )
        .expect("setup answer");

        assert!(answer.contains("`endpointUrl`"));
        assert!(answer.contains("`fillDetails`"));
        assert!(!answer.contains("- ``"));
        assert!(!answer.contains("`unrelatedSecret`"));
    }

    #[test]
    fn module_configuration_setup_answer_reads_structured_paths_and_packages() {
        let setup_document_id = Uuid::now_v7();
        let other_document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let chunks = vec![runtime_chunk(
            setup_document_id,
            0,
            "Provider Epsilon setup",
            "Overview for Provider Epsilon connector settings.",
        )];
        let mut install_block =
            crate::services::query::execution::types::sample_structured_block_row(
                Uuid::now_v7(),
                setup_document_id,
                revision_id,
            );
        install_block.ordinal = 1;
        install_block.text =
            "Install the module:\nsample-install epsilon-connector\n\nConfigure it:\nsample-configure epsilon-connector"
                .to_string();
        install_block.normalized_text = install_block.text.clone();
        let mut path_block = crate::services::query::execution::types::sample_structured_block_row(
            Uuid::now_v7(),
            setup_document_id,
            revision_id,
        );
        path_block.ordinal = 2;
        path_block.text =
            "The module configuration file is /opt/epsilon/modules/connector/connector.conf. Receipt display uses /opt/epsilon/receipt/receipt.ini."
                .to_string();
        path_block.normalized_text = path_block.text.clone();
        let mut parameter_block =
            crate::services::query::execution::types::sample_structured_block_row(
                Uuid::now_v7(),
                setup_document_id,
                revision_id,
            );
        parameter_block.ordinal = 3;
        parameter_block.text =
            "Sheet: Connector settings | Row 1 | Name: endpointUrl | Type: string | Description: Service endpoint"
                .to_string();
        parameter_block.normalized_text = parameter_block.text.clone();
        let mut unrelated_block =
            crate::services::query::execution::types::sample_structured_block_row(
                Uuid::now_v7(),
                other_document_id,
                Uuid::now_v7(),
            );
        unrelated_block.text =
            "Install unrelated module with `sample-install omega-connector`; file /opt/omega/omega.conf"
                .to_string();
        unrelated_block.normalized_text = unrelated_block.text.clone();
        let evidence =
            evidence_with_blocks(vec![install_block, path_block, parameter_block, unrelated_block]);

        let answer = build_module_configuration_setup_answer(
            "Configure Provider Epsilon connector",
            &configuration_setup_ir(),
            &evidence,
            &chunks,
        )
        .expect("setup answer");

        assert!(answer.contains("`epsilon-connector`"));
        assert!(answer.contains("`/opt/epsilon/modules/connector/connector.conf`"));
        assert!(answer.contains("`/opt/epsilon/receipt/receipt.ini`"));
        assert!(answer.contains("`endpointUrl`"));
        assert!(!answer.contains("- ``"));
        assert!(!answer.contains("omega-connector"));
        assert!(!answer.contains("/opt/omega/omega.conf"));
    }

    #[test]
    fn exact_technical_literal_answer_abstains_for_untyped_entity_only_fallback_ir() {
        let document_id = Uuid::now_v7();
        let chunks = vec![runtime_chunk(
            document_id,
            0,
            "Provider Alpha setup",
            r#"
Install the module:
sample-install alpha-connector

Settings are stored in /opt/alpha/modules/connector/connector.conf under [Main].
endpointUrl = https://alpha.example/api
partnerId = alpha-partner
"#,
        )];
        let evidence = empty_evidence();
        let mut low_confidence_ir = configuration_setup_ir();
        low_confidence_ir.act = QueryAct::Describe;
        low_confidence_ir.target_types.clear();
        low_confidence_ir.target_entities =
            vec![EntityMention { label: "Provider Alpha".to_string(), role: EntityRole::Subject }];
        low_confidence_ir.confidence = 0.25;

        assert!(
            build_exact_technical_literal_answer(
                "What operational limits apply to Provider Alpha?",
                &low_confidence_ir,
                &evidence,
                &chunks,
            )
            .is_none(),
            "entity-only provider-free fallback IR must not turn setup literals into a final operational answer"
        );

        let typed_ir = configuration_setup_ir();
        let answer = build_exact_technical_literal_answer(
            "How do I configure Provider Alpha?",
            &typed_ir,
            &evidence,
            &chunks,
        )
        .expect("typed configuration IR should still allow deterministic literal answer");
        assert!(answer.contains("`alpha-connector`"), "{answer}");
    }

    #[test]
    fn module_configuration_setup_answer_abstains_for_port_inventory_question() {
        let document_id = Uuid::now_v7();
        let chunks = vec![runtime_chunk(
            document_id,
            0,
            "sample-manifest.yaml",
            r#"
services:
  api:
    environment:
      PORT: 8001
      apiPort = 8001
    ports:
      - "8001:8001"
  postgres:
    postgresPort = 5432
    ports:
      - "5432:5432"
"#,
        )];
        let mut query_ir = configuration_setup_ir();
        query_ir.act = QueryAct::Describe;
        query_ir.target_types =
            vec!["configuration_file".to_string(), "service".to_string(), "port".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Manifest".to_string(), role: EntityRole::Subject }];

        let answer = build_module_configuration_setup_answer(
            "What ports do the Sample Manifest services expose?",
            &query_ir,
            &empty_evidence(),
            &chunks,
        );

        assert!(
            answer.is_none(),
            "service/port inventory questions should use synthesis over source coverage, not setup field rendering: {answer:?}"
        );
        let exact_answer = build_exact_technical_literal_answer(
            "What ports do the Sample Manifest services expose?",
            &query_ir,
            &empty_evidence(),
            &chunks,
        );
        assert!(
            exact_answer.is_none(),
            "service/port inventory questions should not use exact assignment rendering: {exact_answer:?}"
        );
    }

    #[test]
    fn transport_config_assignment_answer_requires_assignment_evidence() {
        let document_id = Uuid::now_v7();
        let chunks = vec![runtime_chunk(
            document_id,
            0,
            "checkout_service_notes.md",
            "The checkout service accepts HTTPS traffic and calls the inventory service on port 9443.",
        )];
        let mut query_ir = configuration_setup_ir();
        query_ir.act = QueryAct::RetrieveValue;
        query_ir.target_types = vec!["port".to_string(), "connection".to_string()];

        let answer = build_transport_config_assignment_answer(
            "Which ports and connections does the checkout service require?",
            &query_ir,
            &chunks,
        );

        assert!(
            answer.is_none(),
            "transport assignment rendering requires concrete config assignments: {answer:?}"
        );
    }

    #[test]
    fn transport_config_assignment_answer_abstains_for_compound_port_inventory() {
        let document_id = Uuid::now_v7();
        let chunks = vec![runtime_chunk(
            document_id,
            0,
            "alpha_records.txt",
            r#"
entity.alpha = alpha
entity.beta = beta
entity.updated_at = now()
"#,
        )];
        let mut query_ir = configuration_setup_ir();
        query_ir.act = QueryAct::Describe;
        query_ir.target_types =
            vec!["configuration_file".to_string(), "port".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Alpha Records".to_string(), role: EntityRole::Subject }];

        let answer = build_transport_config_assignment_answer(
            "Which port values does Alpha Records expose?",
            &query_ir,
            &chunks,
        );

        assert!(
            answer.is_none(),
            "compound port inventory should not be answered by assignment-shaped rows: {answer:?}"
        );
    }

    #[test]
    fn transport_config_assignment_answer_abstains_for_broad_protocol_explanation() {
        let document_id = Uuid::now_v7();
        let chunks = vec![runtime_chunk(
            document_id,
            0,
            "neighboring_config.txt",
            r#"
service.endpoint = https://example.invalid:9443
service.timeout = 30
service.enabled = true
"#,
        )];
        let mut query_ir = configuration_setup_ir();
        query_ir.act = QueryAct::Describe;
        query_ir.target_types = vec!["protocol".to_string(), "concept".to_string()];

        let answer = build_transport_config_assignment_answer(
            "What are the main improvements of Protocol X version 2 over version 1?",
            &query_ir,
            &chunks,
        );

        assert!(
            answer.is_none(),
            "broad protocol/concept questions should be synthesized from evidence, not rendered as config assignments: {answer:?}"
        );
    }

    #[test]
    fn transport_config_assignment_answer_requires_connection_or_configuration_target() {
        let document_id = Uuid::now_v7();
        let chunks = vec![runtime_chunk(
            document_id,
            0,
            "neighboring_config.txt",
            r#"
listener.protocol = alpha
listener.timeout = 30
listener.enabled = true
"#,
        )];
        let mut query_ir = configuration_setup_ir();
        query_ir.act = QueryAct::RetrieveValue;
        query_ir.target_types = vec!["protocol".to_string()];

        let answer = build_transport_config_assignment_answer(
            "Which protocol is described?",
            &query_ir,
            &chunks,
        );

        assert!(
            answer.is_none(),
            "a protocol-only value lookup must not infer a config-assignment answer without a connection/config target: {answer:?}"
        );
    }

    fn configuration_setup_ir() -> QueryIR {
        QueryIR {
            act: QueryAct::ConfigureHow,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::En,
            target_types: vec![
                "package".to_string(),
                "configuration_file".to_string(),
                "parameter".to_string(),
            ],
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::<UnresolvedRef>::new(),
            needs_clarification: None,
            source_slice: Option::<SourceSliceSpec>::None,
            retrieval_query: None,
            confidence: 1.0,
        }
    }

    fn literal_constraint(text: &str) -> crate::domains::query_ir::LiteralSpan {
        crate::domains::query_ir::LiteralSpan {
            kind: crate::domains::query_ir::LiteralKind::Identifier,
            text: text.to_string(),
        }
    }

    fn empty_evidence() -> CanonicalAnswerEvidence {
        evidence_with_blocks(Vec::new())
    }

    fn evidence_with_blocks(blocks: Vec<KnowledgeStructuredBlockRow>) -> CanonicalAnswerEvidence {
        CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: blocks,
            technical_facts: Vec::new(),
        }
    }

    fn runtime_chunk(
        document_id: Uuid,
        index: i32,
        label: &str,
        text: &str,
    ) -> RuntimeMatchedChunk {
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id,
            revision_id: Uuid::now_v7(),
            chunk_index: index,
            chunk_kind: None,
            document_label: label.to_string(),
            excerpt: text.to_string(),
            score_kind: RuntimeChunkScoreKind::Relevance,
            score: Some(1.0),
            source_text: text.to_string(),
        }
    }
}
