use std::{collections::BTreeSet, sync::LazyLock};

use uuid::Uuid;

use crate::domains::query_ir::{QueryAct, QueryIR, QueryTargetKind};
use crate::infra::knowledge_rows::KnowledgeStructuredBlockRow;
use crate::shared::extraction::technical_facts::TechnicalFactKind;

use super::question_intent::{
    QuestionIntent, classify_question_or_ir_intents, has_question_intent,
};
#[cfg(test)]
use super::technical_literals::technical_chunk_selection_score;
use super::technical_literals::{
    extract_config_section_literals, extract_explicit_path_literals,
    extract_package_command_literals,
};
use super::technical_parameter_answer::build_exact_parameter_answer;
use super::technical_url_answer::build_exact_url_answer;
use super::{CanonicalAnswerEvidence, RuntimeMatchedChunk};

static ERROR_CODE_ASSIGNMENT_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used, reason = "the static regex literal is compile-time owned")]
    regex::RegexBuilder::new(
        r"^\s*([A-Za-z][A-Za-z0-9_.-]{2,160})\s*=\s*((?:-?[0-9]+(?:[.][0-9]+)?\s*[,;]\s*)*-?[0-9]+(?:[.][0-9]+)?)\s*$",
    )
    .case_insensitive(true)
    .multi_line(true)
    .build()
    .expect("error-code assignment regex must compile")
});

static ERROR_CODE_MAPPING_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used, reason = "the static regex literal is compile-time owned")]
    regex::RegexBuilder::new(r"^\s*(-?[0-9]+(?:[.][0-9]+)?)\s*=\s*(\S[^\r\n]{0,160})$")
        .multi_line(true)
        .build()
        .expect("error-code mapping regex must compile")
});

static CONFIG_ASSIGNMENT_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used, reason = "the static regex literal is compile-time owned")]
    regex::RegexBuilder::new(
        r"(?:^|[;\r\n])\s*[#;]?\s*([A-Za-z][A-Za-z0-9_.-]{2,160})\s*=\s*([^;\r\n]{1,220})",
    )
    .build()
    .expect("config assignment regex must compile")
});

static MARKDOWN_TABLE_FIRST_CELL_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used, reason = "the static regex literal is compile-time owned")]
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
        .or_else(|| build_transport_config_assignment_answer(question, query_ir, evidence, chunks))
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
    if !query_ir_requests_module_configuration_setup(query_ir) {
        return None;
    }
    let scoped_chunks = module_configuration_scope_chunks(question, chunks);
    let candidate_chunks = if scoped_chunks.is_empty() { chunks } else { scoped_chunks.as_slice() };
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
    Some(render_module_configuration_answer(
        document_label,
        packages,
        &config_path,
        &config_paths,
        config_sections,
        parameters,
    ))
}

fn push_code_bullets(answer: &mut String, values: impl IntoIterator<Item = String>) {
    for value in values {
        answer.push_str(&format!("\n- `{value}`"));
    }
}

fn render_module_configuration_answer(
    document_label: &str,
    packages: Vec<String>,
    config_path: &str,
    config_paths: &[String],
    config_sections: Vec<String>,
    parameters: Vec<String>,
) -> String {
    let mut answer = format!("`{document_label}`\n");
    if !packages.is_empty() {
        push_code_bullets(&mut answer, packages);
        answer.push('\n');
    }
    push_code_bullets(
        &mut answer,
        std::iter::once(config_path.to_string()).chain(
            config_paths.iter().filter(|path| path.as_str() != config_path).take(6).cloned(),
        ),
    );
    answer.push('\n');
    if !config_sections.is_empty() {
        push_code_bullets(&mut answer, config_sections);
        answer.push('\n');
    }
    for parameter in parameters {
        answer.push_str("\n- ");
        answer.push_str(&render_parameter_bullet(&parameter));
    }
    answer.trim_end().to_string()
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
    let requests_configuration =
        query_ir.targets_any(&[QueryTargetKind::ConfigurationFile, QueryTargetKind::ConfigKey]);
    let requests_module_or_parameter =
        query_ir.targets_any(&[QueryTargetKind::Package, QueryTargetKind::Parameter]);
    requests_configuration && (requests_module_or_parameter || has_focus_signal)
}

fn module_configuration_inventory_question(_question: &str, query_ir: &QueryIR) -> bool {
    if matches!(query_ir.act, QueryAct::ConfigureHow) {
        return false;
    }
    query_ir.targets_any(&[
        QueryTargetKind::Port,
        QueryTargetKind::Service,
        QueryTargetKind::Endpoint,
        QueryTargetKind::HttpMethod,
        QueryTargetKind::ErrorCode,
        QueryTargetKind::Relationship,
        QueryTargetKind::Protocol,
    ])
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
    evidence: &CanonicalAnswerEvidence,
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
        .filter_map(|(rank, chunk)| {
            config_assignment_candidate_from_chunk(rank, query_ir, evidence, chunk)
        })
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
        match target_type {
            QueryTargetKind::Connection
            | QueryTargetKind::Endpoint
            | QueryTargetKind::Url
            | QueryTargetKind::BaseUrl
            | QueryTargetKind::Wsdl => {
                has_connection_target = true;
            }
            QueryTargetKind::ConfigurationFile
            | QueryTargetKind::ConfigKey
            | QueryTargetKind::EnvVar
            | QueryTargetKind::Parameter => {
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
    let has_non_port_target =
        query_ir.target_types.iter().any(|target| !matches!(target, QueryTargetKind::Port));
    let has_connection = has_question_intent(intents, QuestionIntent::Protocol)
        || query_ir.targets_any(&[
            QueryTargetKind::Connection,
            QueryTargetKind::Endpoint,
            QueryTargetKind::Url,
            QueryTargetKind::BaseUrl,
            QueryTargetKind::Wsdl,
            QueryTargetKind::Protocol,
        ]);
    has_port && has_non_port_target && !has_connection
}

fn config_assignment_candidate_from_chunk(
    rank: usize,
    query_ir: &QueryIR,
    evidence: &CanonicalAnswerEvidence,
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
    let score = (64usize.saturating_sub(rank.min(64)) * 100)
        + typed_config_assignment_fact_score(query_ir, evidence, chunk, &entries)
        + entries
            .iter()
            .map(|(_, value)| formal_config_assignment_value_score(value))
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

fn formal_config_assignment_value_score(value: &str) -> usize {
    let trimmed = value.trim();
    usize::from(trimmed.contains("://")) * 120
        + usize::from(url_value_contains_port(trimmed)) * 240
        + usize::from(trimmed.parse::<bool>().is_ok()) * 20
        + usize::from(trimmed.parse::<i64>().is_ok()) * 20
}

fn typed_config_assignment_fact_score(
    query_ir: &QueryIR,
    evidence: &CanonicalAnswerEvidence,
    chunk: &RuntimeMatchedChunk,
    entries: &[(String, String)],
) -> usize {
    let mut seen = BTreeSet::<(TechnicalFactKind, String)>::new();
    evidence
        .technical_facts
        .iter()
        .filter(|fact| {
            fact.document_id == chunk.document_id && fact.revision_id == chunk.revision_id
        })
        .filter_map(|fact| {
            let kind = fact.fact_kind.parse::<TechnicalFactKind>().ok()?;
            if !query_target_supports_technical_fact(query_ir, kind) {
                return None;
            }
            let matches_entry = entries
                .iter()
                .any(|(name, value)| technical_fact_matches_config_entry(kind, fact, name, value));
            if !matches_entry {
                return None;
            }
            let canonical_value = if fact.canonical_value_exact.trim().is_empty() {
                fact.display_value.trim()
            } else {
                fact.canonical_value_exact.trim()
            };
            if !seen.insert((kind, canonical_value.to_string())) {
                return None;
            }
            let support_score =
                usize::from(fact.support_chunk_ids.contains(&chunk.chunk_id)) * 1_000;
            Some(20_000usize.saturating_add(support_score))
        })
        .fold(0usize, usize::saturating_add)
}

fn query_target_supports_technical_fact(query_ir: &QueryIR, kind: TechnicalFactKind) -> bool {
    query_ir.target_types.iter().any(|target_type| match kind {
        TechnicalFactKind::Url => matches!(
            target_type,
            QueryTargetKind::Url
                | QueryTargetKind::BaseUrl
                | QueryTargetKind::Wsdl
                | QueryTargetKind::Endpoint
                | QueryTargetKind::Connection
        ),
        TechnicalFactKind::EndpointPath => {
            matches!(
                target_type,
                QueryTargetKind::Endpoint | QueryTargetKind::Path | QueryTargetKind::Connection
            )
        }
        TechnicalFactKind::Port => {
            matches!(
                target_type,
                QueryTargetKind::Port | QueryTargetKind::Endpoint | QueryTargetKind::Connection
            )
        }
        TechnicalFactKind::Protocol => {
            matches!(
                target_type,
                QueryTargetKind::Protocol | QueryTargetKind::Endpoint | QueryTargetKind::Connection
            )
        }
        TechnicalFactKind::ConfigurationKey => {
            matches!(target_type, QueryTargetKind::ConfigKey | QueryTargetKind::Parameter)
        }
        TechnicalFactKind::ParameterName => {
            matches!(target_type, QueryTargetKind::Parameter | QueryTargetKind::ConfigKey)
        }
        TechnicalFactKind::EnvironmentVariable => {
            matches!(target_type, QueryTargetKind::EnvVar)
        }
        _ => false,
    })
}

fn technical_fact_matches_config_entry(
    kind: TechnicalFactKind,
    fact: &crate::infra::knowledge_rows::KnowledgeTechnicalFactRow,
    name: &str,
    value: &str,
) -> bool {
    let fact_values = [
        fact.display_value.as_str(),
        fact.canonical_value_exact.as_str(),
        fact.canonical_value_text.as_str(),
    ];
    match kind {
        TechnicalFactKind::ConfigurationKey
        | TechnicalFactKind::ParameterName
        | TechnicalFactKind::EnvironmentVariable => fact_values
            .iter()
            .any(|candidate| !candidate.is_empty() && name.eq_ignore_ascii_case(candidate)),
        TechnicalFactKind::Url
        | TechnicalFactKind::EndpointPath
        | TechnicalFactKind::Port
        | TechnicalFactKind::Protocol => fact_values
            .iter()
            .any(|candidate| formal_assignment_value_contains_fact(value, candidate)),
        _ => false,
    }
}

fn formal_assignment_value_contains_fact(value: &str, fact_value: &str) -> bool {
    let value = value.trim().trim_matches(['`', '\'', '"']);
    let fact_value = fact_value.trim().trim_matches(['`', '\'', '"']);
    if fact_value.is_empty() {
        return false;
    }
    value.eq_ignore_ascii_case(fact_value)
        || value
            .split(|ch: char| {
                ch.is_whitespace() || matches!(ch, ',' | ';' | '[' | ']' | '(' | ')' | '{' | '}')
            })
            .any(|token| token.eq_ignore_ascii_case(fact_value))
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
#[path = "technical_answer_tests.rs"]
mod tests;
