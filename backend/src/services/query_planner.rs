use std::collections::BTreeSet;

use crate::domains::query::{QueryPlanningMetadata, RuntimeQueryMode};

const MAX_TOP_K: usize = 48;
const DEFAULT_TOP_K: usize = 8;
const DEFAULT_CONTEXT_BUDGET_CHARS: usize = 22_000;
const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "for", "from", "into", "that", "the", "this", "what", "which", "with",
    "your", "about", "there", "their", "have", "will", "would", "should", "could",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeQueryPlan {
    pub requested_mode: RuntimeQueryMode,
    pub planned_mode: RuntimeQueryMode,
    pub keywords: Vec<String>,
    pub high_level_keywords: Vec<String>,
    pub low_level_keywords: Vec<String>,
    pub top_k: usize,
    pub context_budget_chars: usize,
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
    if contains_any(&question, &["document", "file", "pdf", "docx", "image", "notes", "report"]) {
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
        ],
    ) {
        return RuntimeQueryMode::Global;
    }
    if contains_any(
        &question,
        &["who is", "what is", "tell me about", "entity", "topic", "person", "company"],
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
        return build_query_plan_from_metadata(metadata, top_k);
    }

    let requested_mode = explicit.unwrap_or_else(|| choose_mode(None, question));
    let planned_mode = choose_mode(explicit, question);
    let keywords = extract_keywords(question);
    let (high_level_keywords, low_level_keywords) = split_keywords(&keywords);

    RuntimeQueryPlan {
        requested_mode,
        planned_mode,
        keywords,
        high_level_keywords,
        low_level_keywords,
        top_k: top_k.unwrap_or(DEFAULT_TOP_K).clamp(1, MAX_TOP_K),
        context_budget_chars: DEFAULT_CONTEXT_BUDGET_CHARS,
    }
}

#[must_use]
pub fn build_query_plan_from_metadata(
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
        keywords,
        high_level_keywords: metadata.keywords.high_level.clone(),
        low_level_keywords: metadata.keywords.low_level.clone(),
        top_k: top_k.unwrap_or(DEFAULT_TOP_K).clamp(1, MAX_TOP_K),
        context_budget_chars: DEFAULT_CONTEXT_BUDGET_CHARS,
    }
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

        let plan = build_query_plan_from_metadata(&metadata, Some(6));

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
    }
}
