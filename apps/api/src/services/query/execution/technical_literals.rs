use super::question_intent::{
    QuestionIntent, classify_question_intents, has_technical_surface_intent,
};
pub(super) use super::technical_literal_extractors::{
    extract_explicit_path_literals, extract_http_methods, extract_parameter_literals,
    extract_prefix_literals, extract_url_literals, push_unique_limited,
};
pub(super) use super::technical_literal_focus::{
    document_local_focus_keywords, question_mentions_pagination, question_mentions_protocol,
    select_document_balanced_chunks, technical_chunk_selection_score, technical_keyword_weight,
    technical_literal_focus_keyword_segments, technical_literal_focus_keywords,
    technical_literal_focus_segments_text,
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

pub(super) fn detect_technical_literal_intent(question: &str) -> TechnicalLiteralIntent {
    let lowered = question.to_lowercase();
    let intents = classify_question_intents(question);
    let has_surface_intent = has_technical_surface_intent(&intents);
    let wants_urls = intents.iter().any(|intent| {
        matches!(
            intent,
            QuestionIntent::Endpoint | QuestionIntent::BasePrefix | QuestionIntent::Protocol
        )
    });
    let wants_prefixes = intents.contains(&QuestionIntent::BasePrefix);
    let wants_paths = wants_urls
        || ["path", "путь", "маршрут", "endpoint", "эндпоинт"]
            .iter()
            .any(|needle| lowered.contains(needle));
    let wants_methods = intents.contains(&QuestionIntent::HttpMethod)
        || (has_surface_intent
            && ["get ", "post ", "put ", "patch ", "delete "]
                .iter()
                .any(|needle| lowered.contains(needle)));
    let wants_parameters = intents.contains(&QuestionIntent::Parameter);

    TechnicalLiteralIntent {
        wants_urls,
        wants_prefixes,
        wants_paths,
        wants_methods,
        wants_parameters,
    }
}

pub(super) fn trim_literal_token(token: &str) -> &str {
    token.trim_matches(|ch: char| {
        ch.is_whitespace()
            || matches!(ch, ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | '`')
    })
}
