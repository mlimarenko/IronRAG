use super::{
    KnowledgeStructuredBlockRow, LazyLock, QueryAct, QueryIR, QueryTargetKind, RuntimeMatchedChunk,
    Uuid, extract_config_section_literals, extract_explicit_path_literals,
    extract_package_command_literals,
    transport_config::{CONFIG_ASSIGNMENT_REGEX, clean_config_assignment_value},
};
use crate::services::query::execution::document_target::focused_answer_document_id;
use crate::shared::extraction::table_markdown::{
    split_semantic_key_value, split_semantic_segments, unescape_semantic_segment,
};

static MARKDOWN_TABLE_FIRST_CELL_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used, reason = "the static regex literal is compile-time owned")]
    regex::RegexBuilder::new(r"(?m)^\s*\|\s*`?([A-Za-z][A-Za-z0-9_.-]{1,160})`?\s*\|")
        .build()
        .expect("markdown table first-cell regex must compile")
});

fn push_code_bullets(answer: &mut String, values: impl IntoIterator<Item = String>) {
    for value in values {
        answer.push_str(&format!("\n- `{value}`"));
    }
}

pub(super) fn render_module_configuration_answer(
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

pub(super) fn query_ir_requests_module_configuration_setup(query_ir: &QueryIR) -> bool {
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

pub(super) fn module_configuration_inventory_question(_question: &str, query_ir: &QueryIR) -> bool {
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

pub(super) fn module_configuration_scope_chunks(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Vec<RuntimeMatchedChunk> {
    if let Some(document_id) = module_configuration_setup_document_id(chunks) {
        return chunks.iter().filter(|chunk| chunk.document_id == document_id).cloned().collect();
    }
    let Some(document_id) = focused_answer_document_id(question, chunks) else {
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

pub(super) fn collect_module_packages_with_structured_blocks(
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

pub(super) fn select_module_configuration_path_from_paths(
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

pub(super) fn collect_configuration_paths_with_structured_blocks(
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

pub(super) fn collect_configuration_sections_with_structured_blocks(
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

pub(super) fn collect_visible_parameter_rows_with_structured_blocks(
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
    let cells = split_semantic_segments(line)
        .into_iter()
        .map(str::trim)
        .filter(|cell| !cell.is_empty())
        .collect::<Vec<_>>();
    let row_position = cells.iter().position(|cell| {
        cell.strip_prefix("Row ")
            .is_some_and(|suffix| suffix.chars().next().is_some_and(|ch| ch.is_ascii_digit()))
    })?;
    let first_data_cell = cells.get(row_position.saturating_add(1))?;
    // Locate structural markers before decoding cell text, so escaped values
    // cannot become row metadata or introduce another field separator.
    let parameter = unescape_semantic_segment(
        split_semantic_key_value(first_data_cell)
            .map_or(*first_data_cell, |(_, value)| value)
            .trim()
            .trim_matches('`'),
    );
    if parameter.is_empty() {
        return None;
    }
    let rest_cells = cells
        .iter()
        .skip(row_position.saturating_add(2))
        .map(|cell| {
            let cell = cell.trim_matches('`').trim();
            if let Some((key, value)) = split_semantic_key_value(cell) {
                format!("{}: {}", unescape_semantic_segment(key), unescape_semantic_segment(value))
            } else {
                unescape_semantic_segment(cell)
            }
        })
        .filter(|cell| !cell.is_empty())
        .collect::<Vec<_>>();
    let mut rendered_parts = Vec::new();
    let rest_cell_refs = rest_cells.iter().map(String::as_str).collect::<Vec<_>>();
    if let Some(value) = infer_boolean_assignment_value_from_cells(&rest_cell_refs) {
        rendered_parts.push(format!("{parameter} = {value}"));
    } else {
        rendered_parts.push(parameter.to_string());
    }
    rendered_parts.extend(rest_cells);
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

#[cfg(test)]
mod tests {
    use super::{parse_structured_table_parameter_row, render_parameter_bullet};
    use crate::shared::extraction::table_markdown::{
        SemanticTableContext, build_semantic_table_row_text,
    };

    #[test]
    fn native_table_consumer_configuration_decodes_parameter_and_default_once() {
        let headers = vec!["Parameter".to_string(), "Default".to_string()];
        for (parameter, default) in [
            ("ns:key", "enabled"),
            (r"ns\:key | mode", r"C:\cache | ns:key"),
            (r"root\\branch\|name", r"literal\:value\\tail | enabled"),
            ("ordinary_parameter", "enabled"),
        ] {
            let row = vec![parameter.to_string(), default.to_string()];
            let text =
                build_semantic_table_row_text(SemanticTableContext::default(), 0, &headers, &row);
            let parsed = parse_structured_table_parameter_row(&text)
                .unwrap_or_else(|| panic!("emitted configuration row must parse: {text}"));
            assert_eq!(parsed.parameter_key, parameter);
            assert_eq!(
                render_parameter_bullet(&parsed.rendered),
                format!("`{parameter}` — Default: {default}")
            );
        }
    }

    #[test]
    fn native_table_consumer_configuration_keeps_metadata_out_of_parameter_cells() {
        // Unknown header labels retain the existing first-data-cell rule.
        let headers = vec!["Custom: parameter | label".to_string(), "Default: value".to_string()];
        let row = vec!["ns:key".to_string(), r"C:\cache | enabled".to_string()];
        let emitted = build_semantic_table_row_text(
            SemanticTableContext {
                sheet_name: Some("Sheet: one | settings"),
                table_name: Some("Configuration | names"),
                table_ref: Some(7),
            },
            0,
            &headers,
            &row,
        );
        let text = emitted;
        let parsed = parse_structured_table_parameter_row(&text)
            .unwrap_or_else(|| panic!("emitted configuration row must parse: {text}"));
        assert_eq!(parsed.parameter_key, "ns:key");
        assert_eq!(
            render_parameter_bullet(&parsed.rendered),
            r"`ns:key` — Default: value: C:\cache | enabled"
        );
    }

    #[test]
    fn native_table_consumer_configuration_preserves_raw_parameter_rules() {
        let parsed = parse_structured_table_parameter_row("Row 1 | ordinary_parameter | enabled")
            .unwrap_or_else(|| panic!("raw parameter row must parse"));
        assert_eq!(parsed.parameter_key, "ordinary_parameter");
        assert_eq!(render_parameter_bullet(&parsed.rendered), "`ordinary_parameter` — enabled");
        assert!(
            parse_structured_table_parameter_row("Table Ref: 7 | Parameter: ns\\:key").is_none()
        );
    }
}
