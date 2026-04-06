use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::domains::query::{QueryPlanningMetadata, RuntimeQueryMode};

const MAX_TOP_K: usize = 48;
const DEFAULT_TOP_K: usize = 8;
const DEFAULT_CONTEXT_BUDGET_CHARS: usize = 22_000;
const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "for", "from", "into", "that", "the", "this", "what", "which", "with",
    "your", "about", "there", "their", "have", "will", "would", "should", "could",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedCapabilityIntent {
    GraphQlApi,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryIntentProfile {
    pub exact_literal_technical: bool,
    pub unsupported_capability: Option<UnsupportedCapabilityIntent>,
    pub multi_document_technical: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryPlanTaskInput {
    pub question: String,
    pub top_k: Option<usize>,
    pub explicit_mode: Option<RuntimeQueryMode>,
    pub metadata: Option<QueryPlanningMetadata>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryPlanFailureCode {
    InvalidTopK,
}

impl QueryPlanFailureCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidTopK => "invalid_top_k",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryPlanFailure {
    pub code: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeQueryPlan {
    pub requested_mode: RuntimeQueryMode,
    pub planned_mode: RuntimeQueryMode,
    pub intent_profile: QueryIntentProfile,
    pub keywords: Vec<String>,
    pub high_level_keywords: Vec<String>,
    pub low_level_keywords: Vec<String>,
    pub top_k: usize,
    pub context_budget_chars: usize,
}

pub fn build_task_query_plan(
    input: &QueryPlanTaskInput,
) -> Result<RuntimeQueryPlan, QueryPlanFailure> {
    if matches!(input.top_k, Some(0)) {
        return Err(QueryPlanFailure {
            code: QueryPlanFailureCode::InvalidTopK.as_str().to_string(),
            summary: "query plan topK must be greater than zero".to_string(),
        });
    }

    Ok(build_query_plan(&input.question, input.explicit_mode, input.top_k, input.metadata.as_ref()))
}

#[must_use]
pub fn extract_keywords(question: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    question
        .split_whitespace()
        .map(|token| token.trim_matches(|ch: char| !ch.is_alphanumeric()))
        .filter(|token| token.len() > 2)
        .map(str::to_ascii_lowercase)
        .filter(|token| !STOP_WORDS.contains(&token.as_str()))
        .filter(|token| seen.insert(token.clone()))
        .collect()
}

#[must_use]
pub fn choose_mode(explicit: Option<RuntimeQueryMode>, question: &str) -> RuntimeQueryMode {
    if let Some(explicit) = explicit {
        return explicit;
    }

    let question = question.to_ascii_lowercase();
    if contains_any(
        &question,
        &[
            "document",
            "file",
            "pdf",
            "docx",
            "image",
            "notes",
            "report",
            "документ",
            "файл",
            "изображен",
            "картинк",
            "отчёт",
            "отчет",
            "заметк",
        ],
    ) {
        return RuntimeQueryMode::Document;
    }
    if contains_any(
        &question,
        &[
            "relationship",
            "relationships",
            "connected",
            "connection",
            "network",
            "theme",
            "themes",
            "across",
            "most connected",
            "связ",
            "сеть",
            "темы",
            "между",
            "глобальн",
            "граф",
        ],
    ) {
        return RuntimeQueryMode::Global;
    }
    if contains_any(
        &question,
        &[
            "who is",
            "what is",
            "tell me about",
            "entity",
            "topic",
            "person",
            "company",
            "кто такой",
            "что такое",
            "расскажи",
            "сущност",
            "тема",
            "компани",
            "организац",
            "персон",
        ],
    ) {
        return RuntimeQueryMode::Local;
    }

    RuntimeQueryMode::Hybrid
}

#[must_use]
pub fn build_query_plan(
    question: &str,
    explicit: Option<RuntimeQueryMode>,
    top_k: Option<usize>,
    metadata: Option<&QueryPlanningMetadata>,
) -> RuntimeQueryPlan {
    if let Some(metadata) = metadata {
        return build_query_plan_from_metadata(question, metadata, top_k);
    }

    let requested_mode = explicit.unwrap_or_else(|| choose_mode(None, question));
    let planned_mode = choose_mode(explicit, question);
    let keywords = extract_keywords(question);
    let (high_level_keywords, low_level_keywords) = split_keywords(&keywords);

    RuntimeQueryPlan {
        requested_mode,
        planned_mode,
        intent_profile: classify_query_intent_profile(question, &keywords),
        keywords,
        high_level_keywords,
        low_level_keywords,
        top_k: top_k.unwrap_or(DEFAULT_TOP_K).clamp(1, MAX_TOP_K),
        context_budget_chars: DEFAULT_CONTEXT_BUDGET_CHARS,
    }
}

#[must_use]
pub fn build_query_plan_from_metadata(
    question: &str,
    metadata: &QueryPlanningMetadata,
    top_k: Option<usize>,
) -> RuntimeQueryPlan {
    let mut keywords = metadata.keywords.high_level.clone();
    for keyword in &metadata.keywords.low_level {
        if !keywords.contains(keyword) {
            keywords.push(keyword.clone());
        }
    }

    RuntimeQueryPlan {
        requested_mode: metadata.requested_mode,
        planned_mode: metadata.planned_mode,
        intent_profile: classify_query_intent_profile(question, &keywords),
        keywords,
        high_level_keywords: metadata.keywords.high_level.clone(),
        low_level_keywords: metadata.keywords.low_level.clone(),
        top_k: top_k.unwrap_or(DEFAULT_TOP_K).clamp(1, MAX_TOP_K),
        context_budget_chars: DEFAULT_CONTEXT_BUDGET_CHARS,
    }
}

fn classify_query_intent_profile(question: &str, keywords: &[String]) -> QueryIntentProfile {
    let lowered = question.trim().to_lowercase();
    let exact_literal_technical = is_exact_literal_technical_question(&lowered, keywords);
    QueryIntentProfile {
        exact_literal_technical,
        unsupported_capability: classify_unsupported_capability(&lowered),
        multi_document_technical: exact_literal_technical
            && is_multi_document_technical_question(&lowered, keywords),
    }
}

fn is_exact_literal_technical_question(question: &str, keywords: &[String]) -> bool {
    let markers = [
        "url",
        "wsdl",
        "endpoint",
        "эндпоинт",
        "path",
        "путь",
        "маршрут",
        "method",
        "метод",
        "parameter",
        "параметр",
        "graphql",
        "rest",
        "soap",
        "port",
        "порт",
        "status code",
        "код статуса",
        "prefix",
        "префикс",
    ];
    let has_marker = markers.iter().any(|marker| question.contains(marker));
    let has_literal_shape = question.contains("http://")
        || question.contains("https://")
        || question.contains('/')
        || keywords.iter().any(|keyword| {
            keyword.contains('_')
                || keyword.chars().any(|ch| ch.is_ascii_digit())
                || keyword.chars().any(|ch| ch.is_ascii_uppercase())
        });
    has_marker || has_literal_shape
}

fn classify_unsupported_capability(question: &str) -> Option<UnsupportedCapabilityIntent> {
    question.contains("graphql").then_some(UnsupportedCapabilityIntent::GraphQlApi)
}

fn is_multi_document_technical_question(question: &str, keywords: &[String]) -> bool {
    let markers = [
        "compare",
        "сравни",
        "оба",
        "обе",
        "both",
        "двух",
        "два",
        "нескольк",
        "cross-document",
        "multi-document",
        "разных документ",
        "нескольких документ",
        "отдельно",
        "separately",
    ];
    let _ = keywords;
    markers.iter().any(|marker| question.contains(marker))
}

fn split_keywords(keywords: &[String]) -> (Vec<String>, Vec<String>) {
    let high_level_keywords = keywords.iter().take(3).cloned().collect::<Vec<_>>();
    let low_level_keywords = keywords.iter().skip(3).cloned().collect::<Vec<_>>();
    (high_level_keywords, low_level_keywords)
}

fn contains_any(question: &str, fragments: &[&str]) -> bool {
    fragments.iter().any(|fragment| question.contains(fragment))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_keywords_deduplicates_and_skips_stop_words() {
        assert_eq!(
            extract_keywords("What themes and themes connect the documents?"),
            vec!["themes".to_string(), "connect".to_string(), "documents".to_string()]
        );
    }

    #[test]
    fn choose_mode_prefers_document_for_file_questions() {
        assert_eq!(
            choose_mode(None, "Which document mentions Sarah Chen?"),
            RuntimeQueryMode::Document
        );
    }

    #[test]
    fn choose_mode_prefers_global_for_relationship_language() {
        assert_eq!(
            choose_mode(None, "What relationships are most connected in this library?"),
            RuntimeQueryMode::Global
        );
    }

    #[test]
    fn build_query_plan_clamps_top_k_and_preserves_explicit_mode() {
        let plan =
            build_query_plan("Tell me about OpenAI", Some(RuntimeQueryMode::Mix), Some(99), None);

        assert_eq!(plan.requested_mode, RuntimeQueryMode::Mix);
        assert_eq!(plan.planned_mode, RuntimeQueryMode::Mix);
        assert_eq!(plan.top_k, 48);
    }

    #[test]
    fn build_query_plan_from_metadata_preserves_keyword_levels() {
        let metadata = QueryPlanningMetadata {
            requested_mode: RuntimeQueryMode::Hybrid,
            planned_mode: RuntimeQueryMode::Global,
            intent_cache_status: crate::domains::query::QueryIntentCacheStatus::Miss,
            keywords: crate::domains::query::IntentKeywords {
                high_level: vec!["budget".to_string(), "approval".to_string()],
                low_level: vec!["sarah".to_string(), "chen".to_string()],
            },
            warnings: Vec::new(),
        };

        let plan = build_query_plan_from_metadata(
            "Сравни endpoint orders и inventory",
            &metadata,
            Some(6),
        );

        assert_eq!(plan.requested_mode, RuntimeQueryMode::Hybrid);
        assert_eq!(plan.planned_mode, RuntimeQueryMode::Global);
        assert_eq!(plan.high_level_keywords, vec!["budget".to_string(), "approval".to_string()]);
        assert_eq!(plan.low_level_keywords, vec!["sarah".to_string(), "chen".to_string()]);
        assert_eq!(
            plan.keywords,
            vec![
                "budget".to_string(),
                "approval".to_string(),
                "sarah".to_string(),
                "chen".to_string()
            ]
        );
        assert!(plan.intent_profile.multi_document_technical);
    }

    #[test]
    fn build_query_plan_classifies_exact_literal_and_unsupported_capability() {
        let plan = build_query_plan(
            "Есть ли GraphQL endpoint и какой URL у GET /api/status?",
            None,
            None,
            None,
        );

        assert!(plan.intent_profile.exact_literal_technical);
        assert_eq!(
            plan.intent_profile.unsupported_capability,
            Some(UnsupportedCapabilityIntent::GraphQlApi)
        );
    }
}
