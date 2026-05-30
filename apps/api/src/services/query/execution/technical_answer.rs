use std::sync::LazyLock;

use uuid::Uuid;

use crate::domains::query_ir::{QueryAct, QueryIR};
use crate::infra::arangodb::document_store::KnowledgeStructuredBlockRow;

use super::question_intent::{
    QuestionIntent, canonical_target_type_tag, classify_question_or_ir_intents, has_question_intent,
};
#[cfg(test)]
use super::technical_literals::technical_chunk_selection_score;
use super::technical_literals::{extract_explicit_path_literals, extract_package_command_literals};
use super::technical_parameter_answer::build_exact_parameter_answer;
use super::technical_url_answer::build_exact_url_answer;
use super::{CanonicalAnswerEvidence, RuntimeMatchedChunk};

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
    build_module_configuration_setup_answer(question, query_ir, evidence, chunks)
        .or_else(|| build_transport_config_assignment_answer(question, query_ir, chunks))
        .or_else(|| build_error_code_mapping_answer(question, query_ir, chunks))
        .or_else(|| build_exact_parameter_answer(question, query_ir, evidence, chunks))
        .or_else(|| build_exact_url_answer(question, query_ir, evidence, chunks))
}

pub(super) fn build_module_configuration_setup_answer(
    question: &str,
    query_ir: &QueryIR,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let explicitly_requested = query_ir_requests_module_configuration_setup(query_ir);
    if !explicitly_requested
        && !query_ir_allows_evidence_driven_module_configuration_setup(query_ir)
    {
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
    if !explicitly_requested && packages.is_empty() {
        return None;
    }
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
    matches!(query_ir.act, QueryAct::ConfigureHow | QueryAct::Describe | QueryAct::RetrieveValue)
        && query_ir.target_types.iter().any(|target_type| {
            matches!(
                canonical_target_type_tag(target_type).as_str(),
                "configuration_file" | "config_key"
            )
        })
        && query_ir.target_types.iter().any(|target_type| {
            matches!(canonical_target_type_tag(target_type).as_str(), "package" | "parameter")
        })
}

fn query_ir_allows_evidence_driven_module_configuration_setup(query_ir: &QueryIR) -> bool {
    matches!(query_ir.act, QueryAct::ConfigureHow)
        && (matches!(query_ir.scope, crate::domains::query_ir::QueryScope::SingleDocument)
            || query_ir.document_focus.is_some()
            || !query_ir.target_entities.is_empty())
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
    parse_markdown_parameter_row(line).or_else(|| parse_structured_table_parameter_row(line))
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
    let rendered = std::iter::once(parameter.to_string())
        .chain(
            cells
                .iter()
                .skip(row_position.saturating_add(2))
                .map(|cell| cell.trim_matches('`').trim())
                .filter(|cell| !cell.is_empty())
                .map(str::to_string),
        )
        .collect::<Vec<_>>()
        .join(" — ");
    Some(VisibleParameterRow { parameter_key: parameter.to_string(), rendered })
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
    let mut rendered = cells[0].to_string();
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
    if !has_question_intent(&intents, QuestionIntent::Port)
        && !has_question_intent(&intents, QuestionIntent::Protocol)
        && !query_ir
            .target_types
            .iter()
            .any(|target_type| target_type.trim().eq_ignore_ascii_case("connection"))
    {
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
        DocumentHint, QueryLanguage, QueryScope, SourceSliceSpec, UnresolvedRef,
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
aptitude install beta-widget

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
aptitude install alpha-connector

Configure the module:
dpkg-reconfigure alpha-connector

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
aptitude install gamma-connector

Configure the module:
dpkg-reconfigure gamma-connector

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
aptitude install alpha-connector
Configuration file: /opt/alpha/modules/connector/connector.conf
"#,
            ),
            runtime_chunk(
                setup_document_id,
                1,
                "Widget Alpha administrator guide",
                r#"
Install the module:
aptitude install alpha-connector

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
To use the module, install it with aptitude install alpha-connector and run dpkg-reconfigure alpha-connector.

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
aptitude install delta-connector

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
            "Install the module:\naptitude install epsilon-connector\n\nConfigure it:\ndpkg-reconfigure epsilon-connector"
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
            "Install unrelated module with `aptitude install omega-connector`; file /opt/omega/omega.conf"
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
