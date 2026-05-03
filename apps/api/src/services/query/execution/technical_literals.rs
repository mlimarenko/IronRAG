use crate::domains::query_ir::{LiteralKind, QueryIR};

pub(super) use super::technical_literal_extractors::{
    extract_explicit_path_literals, extract_http_methods, extract_parameter_literals,
    extract_prefix_literals, extract_url_literals, push_unique_limited,
};
pub(super) use super::technical_literal_focus::{
    document_local_focus_keywords, question_mentions_pagination, select_document_balanced_chunks,
    technical_chunk_selection_score, technical_keyword_weight,
    technical_literal_focus_keyword_segments, technical_literal_focus_keywords,
};

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TechnicalLiteralIntent {
    pub(crate) wants_urls: bool,
    pub(crate) wants_prefixes: bool,
    pub(crate) wants_paths: bool,
    pub(crate) wants_methods: bool,
    pub(crate) wants_parameters: bool,
}

impl TechnicalLiteralIntent {
    pub(super) fn any(self) -> bool {
        self.wants_urls
            || self.wants_prefixes
            || self.wants_paths
            || self.wants_methods
            || self.wants_parameters
    }
}

pub(super) fn technical_literal_candidate_limit(
    intent: TechnicalLiteralIntent,
    top_k: usize,
) -> usize {
    if !intent.any() {
        return top_k;
    }

    let multiplier =
        if intent.wants_paths || intent.wants_urls || intent.wants_methods { 4 } else { 3 };
    top_k.saturating_mul(multiplier).clamp(top_k, 64)
}

#[cfg(test)]
pub(super) fn detect_technical_literal_intent(question: &str) -> TechnicalLiteralIntent {
    TechnicalLiteralIntent {
        wants_urls: !extract_url_literals(question, 1).is_empty(),
        wants_prefixes: !extract_prefix_literals(question, 1).is_empty(),
        wants_paths: !extract_explicit_path_literals(question, 1).is_empty(),
        wants_methods: !extract_http_methods(question, 1).is_empty(),
        wants_parameters: !extract_parameter_literals(question, 1).is_empty(),
    }
}

pub(super) fn detect_technical_literal_intent_from_query_ir(
    _question: &str,
    query_ir: &QueryIR,
) -> TechnicalLiteralIntent {
    let mut intent = TechnicalLiteralIntent::default();
    for tag in query_ir.target_types.iter().map(|value| value.trim().to_ascii_lowercase()) {
        match tag.as_str() {
            "endpoint" | "api_endpoint" | "route" | "api_route" | "path" | "url" | "wsdl" => {
                intent.wants_urls = true;
                intent.wants_paths = true;
                intent.wants_methods = true;
            }
            "base_url" | "base_path" | "prefix" => {
                intent.wants_urls = true;
                intent.wants_prefixes = true;
            }
            "parameter" | "query_parameter" | "request_parameter" | "config_key" | "setting" => {
                intent.wants_parameters = true;
            }
            "http_method" | "method" => intent.wants_methods = true,
            _ => {}
        }
    }
    for literal in &query_ir.literal_constraints {
        match literal.kind {
            LiteralKind::Url => intent.wants_urls = true,
            LiteralKind::Path => intent.wants_paths = true,
            LiteralKind::Identifier => intent.wants_parameters = true,
            LiteralKind::Version | LiteralKind::NumericCode | LiteralKind::Other => {}
        }
    }
    intent
}

pub(super) fn trim_literal_token(token: &str) -> &str {
    token.trim_matches(|ch: char| {
        ch.is_whitespace()
            || matches!(ch, ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | '`')
    })
}
