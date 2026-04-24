use std::collections::HashSet;

use crate::domains::query_ir::QueryIR;

#[cfg(test)]
use super::concise_document_subject_label;
use super::retrieve::focused_excerpt_for;
use super::technical_literals::{
    TechnicalLiteralIntent, detect_technical_literal_intent, extract_explicit_path_literals,
    extract_http_methods, extract_parameter_literals, extract_prefix_literals,
    extract_url_literals, push_unique_limited, question_mentions_pagination,
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
            || !self.parameters.is_empty()
    }
}

pub(super) fn collect_technical_literal_groups(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Vec<TechnicalLiteralDocumentGroup> {
    let intent: TechnicalLiteralIntent = detect_technical_literal_intent(question);
    if !intent.any() {
        return Vec::new();
    }

    let mut groups: Vec<TechnicalLiteralDocumentGroup> = Vec::new();
    let literal_focus_keywords = technical_literal_focus_keywords(question, Some(query_ir));
    let pagination_requested = question_mentions_pagination(question);

    for chunk in select_document_balanced_chunks(
        question,
        Some(query_ir),
        chunks,
        &literal_focus_keywords,
        pagination_requested,
        8,
        1,
    ) {
        let group_index = groups
            .iter()
            .position(|group| group.document_label == chunk.document_label)
            .unwrap_or_else(|| {
                groups.push(TechnicalLiteralDocumentGroup::new(chunk.document_label.clone()));
                groups.len() - 1
            });
        let group = &mut groups[group_index];
        if group.matched_excerpt.is_none() && !chunk.excerpt.trim().is_empty() {
            group.matched_excerpt = Some(chunk.excerpt.trim().to_string());
        }
        let focused_source_text =
            focused_excerpt_for(&chunk.source_text, &literal_focus_keywords, 900);
        let literal_source_text = if focused_source_text.trim().is_empty() {
            chunk.source_text.as_str()
        } else {
            focused_source_text.as_str()
        };

        if intent.wants_urls {
            for value in extract_url_literals(literal_source_text, 6) {
                push_unique_limited(&mut group.urls, &mut group.url_seen, value, 6);
            }
        }
        if intent.wants_prefixes {
            for value in extract_prefix_literals(literal_source_text, 6) {
                push_unique_limited(&mut group.prefixes, &mut group.prefix_seen, value, 6);
            }
        }
        if intent.wants_paths {
            for value in extract_explicit_path_literals(literal_source_text, 10) {
                push_unique_limited(&mut group.paths, &mut group.path_seen, value, 10);
            }
        }
        if intent.wants_methods {
            for value in extract_http_methods(literal_source_text, 5) {
                push_unique_limited(&mut group.methods, &mut group.method_seen, value, 5);
            }
        }
        if intent.wants_parameters {
            for value in extract_parameter_literals(literal_source_text, 8) {
                push_unique_limited(&mut group.parameters, &mut group.parameter_seen, value, 8);
            }
        }
    }

    groups.into_iter().filter(|group| group.has_any()).collect()
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
        if !group.urls.is_empty() {
            lines.push(format!(
                "  URLs: {}",
                group.urls.iter().map(|value| format!("`{value}`")).collect::<Vec<_>>().join(", ")
            ));
        }
        if !group.prefixes.is_empty() {
            lines.push(format!(
                "  Prefixes: {}",
                group
                    .prefixes
                    .iter()
                    .map(|value| format!("`{value}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !group.paths.is_empty() {
            lines.push(format!(
                "  Paths: {}",
                group.paths.iter().map(|value| format!("`{value}`")).collect::<Vec<_>>().join(", ")
            ));
        }
        if !group.methods.is_empty() {
            lines.push(format!(
                "  HTTP methods: {}",
                group
                    .methods
                    .iter()
                    .map(|value| format!("`{value}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !group.parameters.is_empty() {
            lines.push(format!(
                "  Parameters: {}",
                group
                    .parameters
                    .iter()
                    .map(|value| format!("`{value}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    if lines.is_empty() {
        return None;
    }

    Some(format!("Exact technical literals\n{}", lines.join("\n")))
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
