use std::collections::HashSet;

use crate::domains::query_ir::{QueryIR, QueryScope};

#[cfg(test)]
use super::concise_document_subject_label;
use super::retrieve::focused_excerpt_for;
use super::technical_literals::{
    TechnicalLiteralIntent, detect_technical_literal_intent_from_query_ir,
    extract_config_section_literals, extract_explicit_path_literals, extract_http_methods,
    extract_parameter_literals, extract_prefix_literals, extract_url_literals, push_unique_limited,
    select_document_balanced_chunks, technical_literal_focus_keywords,
};
use super::types::RuntimeMatchedChunk;

#[derive(Debug, Clone, Default)]
pub(super) struct TechnicalLiteralDocumentGroup {
    pub(super) document_label: String,
    pub(super) matched_excerpt: Option<String>,
    pub(super) urls: Vec<String>,
    pub(super) url_seen: HashSet<String>,
    pub(super) prefixes: Vec<String>,
    pub(super) prefix_seen: HashSet<String>,
    pub(super) paths: Vec<String>,
    pub(super) path_seen: HashSet<String>,
    pub(super) methods: Vec<String>,
    pub(super) method_seen: HashSet<String>,
    pub(super) sections: Vec<String>,
    pub(super) section_seen: HashSet<String>,
    pub(super) parameters: Vec<String>,
    pub(super) parameter_seen: HashSet<String>,
}

impl TechnicalLiteralDocumentGroup {
    fn new(document_label: String) -> Self {
        Self { document_label, ..Self::default() }
    }

    pub(super) fn has_any(&self) -> bool {
        self.matched_excerpt.is_some()
            || !self.urls.is_empty()
            || !self.prefixes.is_empty()
            || !self.paths.is_empty()
            || !self.methods.is_empty()
            || !self.sections.is_empty()
            || !self.parameters.is_empty()
    }
}

pub(super) fn collect_technical_literal_groups(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Vec<TechnicalLiteralDocumentGroup> {
    let intent: TechnicalLiteralIntent =
        detect_technical_literal_intent_from_query_ir(question, query_ir);
    if !intent.any() && !query_ir.is_exact_literal_technical() {
        return Vec::new();
    }

    let mut groups: Vec<TechnicalLiteralDocumentGroup> = Vec::new();
    let literal_focus_keywords = technical_literal_focus_keywords(question, Some(query_ir));
    let pagination_requested = false;

    let max_chunks_per_document = technical_literal_group_chunks_per_document(query_ir, intent);

    let max_total_chunks = technical_literal_group_total_chunks(query_ir, intent);
    for chunk in select_document_balanced_chunks(
        question,
        Some(query_ir),
        chunks,
        &literal_focus_keywords,
        pagination_requested,
        max_total_chunks,
        max_chunks_per_document,
    ) {
        collect_technical_literals_from_chunk(&mut groups, chunk, &literal_focus_keywords, intent);
    }

    groups.into_iter().filter(|group| group.has_any()).collect()
}

fn collect_technical_literals_from_chunk(
    groups: &mut Vec<TechnicalLiteralDocumentGroup>,
    chunk: &RuntimeMatchedChunk,
    literal_focus_keywords: &[String],
    intent: TechnicalLiteralIntent,
) {
    let group_index = groups
        .iter()
        .position(|group| group.document_label == chunk.document_label)
        .unwrap_or_else(|| {
            groups.push(TechnicalLiteralDocumentGroup::new(chunk.document_label.clone()));
            groups.len() - 1
        });
    let group = &mut groups[group_index];
    let focused_source_text = focused_excerpt_for(&chunk.source_text, literal_focus_keywords, 900);
    let literal_source_text = if focused_source_text.trim().is_empty() {
        chunk.source_text.as_str()
    } else {
        focused_source_text.as_str()
    };
    populate_matched_literal_excerpt(group, chunk, &focused_source_text);
    if intent.wants_urls {
        collect_literal_values(
            extract_url_literals,
            literal_source_text,
            &chunk.source_text,
            &mut group.urls,
            &mut group.url_seen,
            6,
        );
    }
    if intent.wants_prefixes {
        collect_literal_values(
            extract_prefix_literals,
            literal_source_text,
            &chunk.source_text,
            &mut group.prefixes,
            &mut group.prefix_seen,
            6,
        );
    }
    if intent.wants_paths {
        collect_literal_values(
            extract_explicit_path_literals,
            literal_source_text,
            &chunk.source_text,
            &mut group.paths,
            &mut group.path_seen,
            10,
        );
    }
    if intent.wants_methods {
        collect_literal_values(
            extract_http_methods,
            literal_source_text,
            &chunk.source_text,
            &mut group.methods,
            &mut group.method_seen,
            5,
        );
    }
    if intent.wants_parameters {
        collect_parameter_literals(group, literal_source_text, &chunk.source_text);
    }
}

fn populate_matched_literal_excerpt(
    group: &mut TechnicalLiteralDocumentGroup,
    chunk: &RuntimeMatchedChunk,
    focused_source_text: &str,
) {
    if group.matched_excerpt.is_some() {
        return;
    }
    let excerpt = chunk.excerpt.trim();
    let focused = focused_source_text.trim();
    if excerpt.is_empty() {
        group.matched_excerpt = (!focused.is_empty()).then(|| focused.to_string());
        return;
    }
    let mut matched = excerpt.to_string();
    if !focused.is_empty() && focused != excerpt {
        matched.push_str(" Focused literal excerpt: ");
        matched.push_str(focused);
    }
    group.matched_excerpt = Some(matched);
}

fn collect_literal_values(
    extractor: fn(&str, usize) -> Vec<String>,
    primary_text: &str,
    fallback_text: &str,
    values: &mut Vec<String>,
    seen: &mut HashSet<String>,
    limit: usize,
) {
    for value in extractor(primary_text, limit) {
        push_unique_limited(values, seen, value, limit);
    }
    if values.len() >= limit {
        return;
    }
    for value in extractor(fallback_text, limit) {
        push_unique_limited(values, seen, value, limit);
    }
}

fn collect_parameter_literals(
    group: &mut TechnicalLiteralDocumentGroup,
    primary_text: &str,
    fallback_text: &str,
) {
    collect_literal_values(
        extract_config_section_literals,
        primary_text,
        fallback_text,
        &mut group.sections,
        &mut group.section_seen,
        12,
    );
    collect_literal_values(
        extract_parameter_literals,
        primary_text,
        fallback_text,
        &mut group.parameters,
        &mut group.parameter_seen,
        24,
    );
}

fn technical_literal_group_chunks_per_document(
    query_ir: &QueryIR,
    intent: TechnicalLiteralIntent,
) -> usize {
    if intent.wants_parameters
        && (matches!(query_ir.scope, QueryScope::SingleDocument)
            || matches!(query_ir.act, crate::domains::query_ir::QueryAct::ConfigureHow))
        && (intent.wants_paths
            || query_ir.is_follow_up()
            || matches!(query_ir.act, crate::domains::query_ir::QueryAct::ConfigureHow))
    {
        4
    } else {
        1
    }
}

fn technical_literal_group_total_chunks(
    query_ir: &QueryIR,
    intent: TechnicalLiteralIntent,
) -> usize {
    if intent.wants_parameters
        && (matches!(query_ir.scope, QueryScope::SingleDocument)
            || matches!(query_ir.act, crate::domains::query_ir::QueryAct::ConfigureHow)
            || query_ir.is_exact_literal_technical())
    {
        16
    } else {
        8
    }
}

pub(super) fn render_exact_technical_literals_section(
    groups: &[TechnicalLiteralDocumentGroup],
) -> Option<String> {
    if groups.is_empty() {
        return None;
    }

    let mut lines = Vec::new();
    for group in groups.iter().filter(|group| group.has_any()) {
        lines.push(format!("- Document: `{}`", group.document_label));
        if let Some(excerpt) = &group.matched_excerpt {
            lines.push(format!("  Matched excerpt: {excerpt}"));
        }
        push_literal_inventory_lines(&mut lines, "URLs", &group.urls);
        push_literal_inventory_lines(&mut lines, "Prefixes", &group.prefixes);
        push_literal_inventory_lines(&mut lines, "Paths", &group.paths);
        push_literal_inventory_lines(&mut lines, "HTTP methods", &group.methods);
        push_literal_inventory_lines(&mut lines, "Sections", &group.sections);
        push_literal_inventory_lines(&mut lines, "Parameters", &group.parameters);
    }

    if lines.is_empty() {
        return None;
    }

    Some(format!("Exact technical literals\n{}", lines.join("\n")))
}

fn push_literal_inventory_lines(lines: &mut Vec<String>, label: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    lines.push(format!("  {label}:"));
    lines.extend(values.iter().map(|value| format!("    - `{value}`")));
}

#[cfg(test)]
pub(super) fn build_exact_technical_literals_section(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let groups = collect_technical_literal_groups(question, query_ir, chunks);
    render_exact_technical_literals_section(&groups)
}

#[cfg(test)]
pub(super) fn infer_endpoint_subject_label(group: &TechnicalLiteralDocumentGroup) -> String {
    concise_document_subject_label(&group.document_label)
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::domains::query_ir::{QueryAct, QueryIR, QueryLanguage, QueryScope};
    use crate::services::query::execution::{RuntimeChunkScoreKind, RuntimeMatchedChunk};

    fn chunk(document_id: Uuid, label: &str, index: i32, source_text: &str) -> RuntimeMatchedChunk {
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id,
            revision_id: Uuid::now_v7(),
            chunk_index: index,
            chunk_kind: Some("text".to_string()),
            document_label: label.to_string(),
            excerpt: source_text.to_string(),
            score_kind: RuntimeChunkScoreKind::Relevance,
            score: Some(1.0),
            source_text: source_text.to_string(),
        }
    }

    fn test_query_ir() -> QueryIR {
        QueryIR {
            act: QueryAct::ConfigureHow,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: Vec::new(),
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 1.0,
        }
    }

    #[test]
    fn exact_technical_literals_keep_second_round_parameter_inventory_on_large_result_sets() {
        let target_document_id = Uuid::now_v7();
        let mut chunks = vec![chunk(
            target_document_id,
            "Alpha Connector setup guide",
            0,
            "General setup overview without assignment literals.",
        )];
        for index in 0..9 {
            chunks.push(chunk(
                Uuid::now_v7(),
                &format!("Reference document {index}"),
                0,
                "General reference text without configuration assignments.",
            ));
        }
        chunks.push(chunk(
            target_document_id,
            "Alpha Connector setup guide",
            1,
            "[Main]\nalphaMerchantId = 10\nsecretKey = value\npollInterval = 30",
        ));
        let mut query_ir = test_query_ir();
        query_ir.target_types = vec![
            crate::domains::query_ir::QueryTargetKind::ConfigKey,
            crate::domains::query_ir::QueryTargetKind::ConfigurationFile,
        ];

        let section = build_exact_technical_literals_section(
            "Alpha Connector configuration parameters",
            &query_ir,
            &chunks,
        )
        .expect("technical literal section");

        assert!(section.contains("alphaMerchantId"));
        assert!(section.contains("secretKey"));
        assert!(section.contains("pollInterval"));
    }
}
