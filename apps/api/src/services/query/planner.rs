use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::domains::query::{
    DEFAULT_TOP_K, IntentKeywords, MAX_TOP_K, QueryPlanningMetadata, RuntimeQueryMode,
};
use crate::domains::query_ir::{
    EntityRole, QueryAct, QueryIR, literal_kind_has_exact_technical_shape,
};
const DEFAULT_CONTEXT_BUDGET_CHARS: usize = 22_000;
/// Minimum token length after stripping punctuation. Tokens shorter than
/// this mostly carry no retrieval signal; a length cutoff avoids a
/// language-specific lexicon.
const TOKEN_MIN_LEN: usize = 3;
pub(crate) const QUERY_IR_LEXICAL_LANE_MIN_CONFIDENCE: f32 = 0.6;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryIntentProfile {
    pub exact_literal_technical: bool,
    pub multi_document_technical: bool,
    #[serde(default)]
    pub act: Option<QueryAct>,
    #[serde(default)]
    pub broad_procedure_variant_coverage: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryPlanTaskInput {
    pub question: String,
    pub top_k: Option<usize>,
    pub explicit_mode: Option<RuntimeQueryMode>,
    pub metadata: Option<QueryPlanningMetadata>,
    #[serde(default)]
    pub query_ir: Option<QueryIR>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum QueryPlanFailureCode {
    InvalidTopK,
}

impl QueryPlanFailureCode {
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidTopK => "invalid_top_k",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryPlanFailure {
    pub code: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeQueryPlan {
    pub requested_mode: RuntimeQueryMode,
    pub planned_mode: RuntimeQueryMode,
    pub intent_profile: QueryIntentProfile,
    pub keywords: Vec<String>,
    pub high_level_keywords: Vec<String>,
    pub low_level_keywords: Vec<String>,
    pub entity_keywords: Vec<String>,
    pub concept_keywords: Vec<String>,
    pub top_k: usize,
    pub context_budget_chars: usize,
    pub hyde_recommended: bool,
}

pub(crate) fn build_task_query_plan(
    input: &QueryPlanTaskInput,
) -> Result<RuntimeQueryPlan, QueryPlanFailure> {
    if matches!(input.top_k, Some(0)) {
        return Err(QueryPlanFailure {
            code: QueryPlanFailureCode::InvalidTopK.as_str().to_string(),
            summary: "query plan topK must be greater than zero".to_string(),
        });
    }

    Ok(build_query_plan_with_query_ir(
        &input.question,
        input.explicit_mode,
        input.top_k,
        input.metadata.as_ref(),
        input.query_ir.as_ref(),
    ))
}

#[must_use]
pub(crate) fn extract_keywords(question: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    strip_leading_question_marker(question)
        .split_whitespace()
        .map(|token| token.trim_matches(|ch: char| !ch.is_alphanumeric()))
        .filter(|token| token.chars().count() >= TOKEN_MIN_LEN)
        .map(str::to_lowercase)
        .filter(|token| seen.insert(token.clone()))
        .collect()
}

#[must_use]
pub(crate) fn strip_leading_question_marker(question: &str) -> &str {
    let trimmed = question.trim_start();
    let Some((marker, rest)) = trimmed.split_once(char::is_whitespace) else {
        return trimmed;
    };
    let marker = marker.trim_matches(|ch: char| matches!(ch, '(' | '[' | '{'));
    if !marker.ends_with(['.', ')', ']', '}', ':', '-']) {
        return trimmed;
    }
    let marker = marker.trim_end_matches(['.', ')', ']', '}', ':', '-']);
    if is_leading_question_marker(marker) { rest.trim_start() } else { trimmed }
}

fn is_leading_question_marker(marker: &str) -> bool {
    let chars = marker.chars().collect::<Vec<_>>();
    if chars.is_empty() || chars.len() > 4 || !chars.iter().all(|ch| ch.is_ascii_alphanumeric()) {
        return false;
    }

    if chars.iter().all(|ch| ch.is_ascii_digit()) {
        return true;
    }

    if chars.first().is_some_and(|ch| ch.is_ascii_digit()) {
        let digit_len = chars.iter().take_while(|ch| ch.is_ascii_digit()).count();
        return digit_len + 1 == chars.len()
            && chars.last().is_some_and(|ch| ch.is_ascii_alphabetic());
    }

    false
}

#[must_use]
pub(crate) fn choose_mode(explicit: Option<RuntimeQueryMode>, question: &str) -> RuntimeQueryMode {
    if let Some(explicit) = explicit {
        return explicit;
    }
    let _ = question;
    RuntimeQueryMode::Hybrid
}

#[must_use]
#[cfg(test)]
pub(crate) fn build_query_plan(
    question: &str,
    explicit: Option<RuntimeQueryMode>,
    top_k: Option<usize>,
    metadata: Option<&QueryPlanningMetadata>,
) -> RuntimeQueryPlan {
    build_query_plan_with_query_ir(question, explicit, top_k, metadata, None)
}

fn build_query_plan_with_query_ir(
    question: &str,
    explicit: Option<RuntimeQueryMode>,
    top_k: Option<usize>,
    metadata: Option<&QueryPlanningMetadata>,
    query_ir: Option<&QueryIR>,
) -> RuntimeQueryPlan {
    if let Some(metadata) = metadata {
        return build_query_plan_from_metadata_with_query_ir(question, metadata, top_k, query_ir);
    }

    let requested_mode = explicit.unwrap_or_else(|| choose_mode(None, question));
    let planned_mode = choose_mode(explicit, question);
    let keywords = extract_keywords(question);
    let lane_keywords = derive_lexical_lane_keywords(&keywords, query_ir);
    let semantic_lanes = lane_keywords.clone();
    let intent_profile = query_intent_profile_from_query_ir(query_ir);
    let hyde_recommended =
        intent_profile.multi_document_technical && !intent_profile.exact_literal_technical;

    RuntimeQueryPlan {
        requested_mode,
        planned_mode,
        intent_profile,
        keywords,
        high_level_keywords: lane_keywords.high_level,
        low_level_keywords: lane_keywords.low_level,
        entity_keywords: semantic_lanes.high_level,
        concept_keywords: semantic_lanes.low_level,
        top_k: planned_top_k(question, top_k),
        context_budget_chars: DEFAULT_CONTEXT_BUDGET_CHARS,
        hyde_recommended,
    }
}

#[must_use]
#[cfg(test)]
pub(crate) fn build_query_plan_from_metadata(
    question: &str,
    metadata: &QueryPlanningMetadata,
    top_k: Option<usize>,
) -> RuntimeQueryPlan {
    build_query_plan_from_metadata_with_query_ir(question, metadata, top_k, None)
}

fn build_query_plan_from_metadata_with_query_ir(
    question: &str,
    metadata: &QueryPlanningMetadata,
    top_k: Option<usize>,
    query_ir: Option<&QueryIR>,
) -> RuntimeQueryPlan {
    let mut keywords = metadata.keywords.high_level.clone();
    for keyword in &metadata.keywords.low_level {
        if !keywords.contains(keyword) {
            keywords.push(keyword.clone());
        }
    }

    let semantic_lanes =
        typed_semantic_keyword_lanes(&keywords, Some(&metadata.keywords), query_ir);
    let intent_profile = query_intent_profile_from_query_ir(query_ir);
    let hyde_recommended =
        intent_profile.multi_document_technical && !intent_profile.exact_literal_technical;

    RuntimeQueryPlan {
        requested_mode: metadata.requested_mode,
        planned_mode: metadata.planned_mode,
        intent_profile,
        keywords,
        high_level_keywords: metadata.keywords.high_level.clone(),
        low_level_keywords: metadata.keywords.low_level.clone(),
        entity_keywords: semantic_lanes.high_level,
        concept_keywords: semantic_lanes.low_level,
        top_k: planned_top_k(question, top_k),
        context_budget_chars: DEFAULT_CONTEXT_BUDGET_CHARS,
        hyde_recommended,
    }
}

pub(crate) fn query_intent_profile_from_query_ir(query_ir: Option<&QueryIR>) -> QueryIntentProfile {
    QueryIntentProfile {
        exact_literal_technical: query_ir.is_some_and(QueryIR::is_exact_literal_technical),
        multi_document_technical: query_ir.is_some_and(QueryIR::is_multi_document),
        act: query_ir.map(|query_ir| query_ir.act),
        broad_procedure_variant_coverage: query_ir
            .is_some_and(QueryIR::requests_broad_procedure_variant_coverage),
    }
}

fn planned_top_k(question: &str, top_k: Option<usize>) -> usize {
    let _ = question;
    top_k.unwrap_or(DEFAULT_TOP_K).clamp(1, MAX_TOP_K)
}

/// Extracts keywords from a question preserving original case.
/// This is lexical extraction only; semantic routing belongs to canonical QueryIR.
#[must_use]
pub(crate) fn extract_keywords_preserving_case(question: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    strip_leading_question_marker(question)
        .split_whitespace()
        .map(|token| token.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '.'))
        .filter(|token| token.chars().count() >= TOKEN_MIN_LEN)
        .filter(|token| seen.insert(token.to_ascii_lowercase()))
        .map(|token| token.to_string())
        .collect()
}

#[must_use]
pub(crate) fn derive_lexical_lane_keywords(
    keywords: &[String],
    query_ir: Option<&QueryIR>,
) -> IntentKeywords {
    let fallback = full_keyword_lanes(keywords);
    let Some(query_ir) = query_ir else {
        return fallback;
    };
    if query_ir.confidence < QUERY_IR_LEXICAL_LANE_MIN_CONFIDENCE {
        return fallback;
    }

    let seeds = collect_query_ir_lexical_lane_seeds(query_ir);
    if seeds.high.is_empty() && seeds.low.is_empty() {
        return fallback;
    }

    let high_level = keywords_matching_seeds(keywords, &seeds.high);
    let low_level = keywords_matching_seeds(keywords, &seeds.low);
    if high_level.is_empty() && low_level.is_empty() {
        return fallback;
    }

    IntentKeywords {
        high_level: if high_level.is_empty() { keywords.to_vec() } else { high_level },
        low_level: if low_level.is_empty() { keywords.to_vec() } else { low_level },
    }
}

fn full_keyword_lanes(keywords: &[String]) -> IntentKeywords {
    IntentKeywords { high_level: keywords.to_vec(), low_level: keywords.to_vec() }
}

fn typed_semantic_keyword_lanes(
    keywords: &[String],
    metadata_lanes: Option<&IntentKeywords>,
    query_ir: Option<&QueryIR>,
) -> IntentKeywords {
    let Some(query_ir) = query_ir else {
        return full_keyword_lanes(keywords);
    };
    if query_ir.confidence < QUERY_IR_LEXICAL_LANE_MIN_CONFIDENCE {
        return full_keyword_lanes(keywords);
    }

    let Some(metadata_lanes) = metadata_lanes else {
        return derive_lexical_lane_keywords(keywords, Some(query_ir));
    };
    if metadata_lanes.high_level.is_empty() && metadata_lanes.low_level.is_empty() {
        return derive_lexical_lane_keywords(keywords, Some(query_ir));
    }

    IntentKeywords {
        high_level: if metadata_lanes.high_level.is_empty() {
            keywords.to_vec()
        } else {
            metadata_lanes.high_level.clone()
        },
        low_level: if metadata_lanes.low_level.is_empty() {
            keywords.to_vec()
        } else {
            metadata_lanes.low_level.clone()
        },
    }
}

#[derive(Default)]
struct LexicalLaneSeeds {
    high: Vec<String>,
    low: Vec<String>,
}

fn collect_query_ir_lexical_lane_seeds(query_ir: &QueryIR) -> LexicalLaneSeeds {
    let mut seeds = LexicalLaneSeeds::default();
    for entity in &query_ir.target_entities {
        match entity.role {
            EntityRole::Subject | EntityRole::Object => push_seed(&mut seeds.high, &entity.label),
            EntityRole::Modifier => push_seed(&mut seeds.low, &entity.label),
        }
    }
    if let Some(document_focus) = &query_ir.document_focus {
        push_seed(&mut seeds.high, &document_focus.hint);
    }
    if let Some(comparison) = &query_ir.comparison {
        if let Some(left) = &comparison.a {
            push_seed(&mut seeds.high, left);
        }
        if let Some(right) = &comparison.b {
            push_seed(&mut seeds.high, right);
        }
        push_seed(&mut seeds.low, &comparison.dimension);
    }
    for literal in &query_ir.literal_constraints {
        if literal_kind_has_exact_technical_shape(literal.kind, &literal.text) {
            push_seed(&mut seeds.high, &literal.text);
        } else {
            push_seed(&mut seeds.low, &literal.text);
        }
    }
    for temporal in &query_ir.temporal_constraints {
        push_seed(&mut seeds.low, &temporal.surface);
    }
    seeds
}

fn push_seed(seeds: &mut Vec<String>, value: &str) {
    let normalized = normalize_lane_text(value);
    if normalized.is_empty() || seeds.iter().any(|seed| seed == &normalized) {
        return;
    }
    seeds.push(normalized);
}

fn keywords_matching_seeds(keywords: &[String], seeds: &[String]) -> Vec<String> {
    let mut selected = Vec::new();
    for keyword in keywords {
        if seeds.iter().any(|seed| keyword_matches_seed(keyword, seed))
            && !selected.contains(keyword)
        {
            selected.push(keyword.clone());
        }
    }
    selected
}

fn keyword_matches_seed(keyword: &str, seed: &str) -> bool {
    let keyword = normalize_lane_text(keyword);
    if keyword.is_empty() || seed.is_empty() {
        return false;
    }
    keyword == seed || seed.split_whitespace().any(|seed_token| seed_token == keyword)
}

fn normalize_lane_text(value: &str) -> String {
    let mut normalized = String::new();
    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            normalized.push(ch);
        } else {
            normalized.push(' ');
        }
    }
    normalized.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::query_ir::{
        EntityMention, LiteralKind, LiteralSpan, QueryAct, QueryLanguage, QueryScope,
    };

    fn query_ir_with_subject(label: &str, confidence: f32) -> QueryIR {
        QueryIR {
            act: QueryAct::Describe,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: Vec::new(),
            target_entities: vec![EntityMention {
                label: label.to_string(),
                role: EntityRole::Subject,
            }],
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence,
        }
    }

    fn query_plan_task_input(question: &str, query_ir: Option<QueryIR>) -> QueryPlanTaskInput {
        QueryPlanTaskInput {
            question: question.to_string(),
            top_k: None,
            explicit_mode: None,
            metadata: None,
            query_ir,
        }
    }

    #[test]
    fn legacy_intent_profile_without_act_deserializes_conservatively() {
        let profile: QueryIntentProfile = serde_json::from_value(serde_json::json!({
            "exactLiteralTechnical": true,
            "multiDocumentTechnical": false
        }))
        .expect("legacy profile");

        assert!(profile.exact_literal_technical);
        assert_eq!(profile.act, None);
    }

    #[test]
    fn extract_keywords_deduplicates_and_filters_short_tokens() {
        // Keyword extraction is intentionally language-agnostic: the IR
        // compiler handles routing semantics, not raw keyword lists.
        let keywords = extract_keywords("What themes and themes connect the documents?");
        assert!(keywords.contains(&"themes".to_string()));
        assert!(keywords.contains(&"connect".to_string()));
        assert!(keywords.contains(&"documents".to_string()));
        // Duplicates still collapse.
        assert_eq!(keywords.iter().filter(|k| *k == "themes").count(), 1);
    }

    #[test]
    fn extract_keywords_uses_unicode_case_folding() {
        let keywords = extract_keywords("CAFÉ ΔELTA AlphaKey");
        assert!(keywords.contains(&"café".to_string()));
        assert!(keywords.contains(&"δelta".to_string()));
        assert!(keywords.contains(&"alphakey".to_string()));
    }

    #[test]
    fn extract_keywords_strips_only_formal_numeric_question_markers() {
        let keywords = extract_keywords("Q16. Which ports should a terminal use?");
        assert!(keywords.contains(&"q16".to_string()));
        assert!(keywords.contains(&"which".to_string()));
        assert!(keywords.contains(&"ports".to_string()));

        let numbered = extract_keywords("10b) Which ports should a terminal use?");
        assert!(!numbered.contains(&"10b".to_string()));
        assert!(numbered.contains(&"terminal".to_string()));
    }

    #[test]
    fn extract_keywords_keeps_embedded_identifier_tokens() {
        let keywords = extract_keywords("HTTP2 routing settings");
        assert!(keywords.contains(&"http2".to_string()));
        assert!(keywords.contains(&"routing".to_string()));
    }

    #[test]
    fn extract_keywords_keeps_leading_identifier_without_marker_separator() {
        let keywords = extract_keywords("H2O sampling routine");
        assert!(keywords.contains(&"h2o".to_string()));
        assert!(keywords.contains(&"sampling".to_string()));

        let robot = extract_keywords("R2D2 deployment notes");
        assert!(robot.contains(&"r2d2".to_string()));
        assert!(robot.contains(&"deployment".to_string()));
    }

    #[test]
    fn leading_question_marker_strip_preserves_non_numeric_prefixes() {
        assert_eq!(
            strip_leading_question_marker("Q16. Which ports should a terminal use?"),
            "Q16. Which ports should a terminal use?"
        );
        assert_eq!(
            strip_leading_question_marker("10b) Which ports should a terminal use?"),
            "Which ports should a terminal use?"
        );
        assert_eq!(strip_leading_question_marker("RFC. connection notes"), "RFC. connection notes");
        assert_eq!(strip_leading_question_marker("ISO. export profile"), "ISO. export profile");
        assert_eq!(strip_leading_question_marker("API: request shape"), "API: request shape");
        assert_eq!(strip_leading_question_marker("v1. migration notes"), "v1. migration notes");
        assert_eq!(strip_leading_question_marker("H2: sampling routine"), "H2: sampling routine");
        assert_eq!(strip_leading_question_marker("Q4: rollout plan"), "Q4: rollout plan");
    }

    #[test]
    fn choose_mode_defaults_to_hybrid_without_explicit_metadata() {
        assert_eq!(
            choose_mode(None, "Which document mentions Sarah Chen?"),
            RuntimeQueryMode::Hybrid
        );
    }

    #[test]
    fn choose_mode_does_not_route_from_raw_relationship_words() {
        assert_eq!(
            choose_mode(None, "What relationships are most connected in this library?"),
            RuntimeQueryMode::Hybrid
        );
    }

    #[test]
    fn build_query_plan_clamps_top_k_and_preserves_explicit_mode() {
        let plan =
            build_query_plan("Tell me about OpenAI", Some(RuntimeQueryMode::Mix), Some(99), None);

        assert_eq!(plan.requested_mode, RuntimeQueryMode::Mix);
        assert_eq!(plan.planned_mode, RuntimeQueryMode::Mix);
        assert_eq!(plan.top_k, MAX_TOP_K);
    }

    #[test]
    fn build_query_plan_keeps_top_k_ir_agnostic() {
        let plan = build_query_plan("What's new in the last 5 releases?", None, None, None);
        assert_eq!(plan.top_k, DEFAULT_TOP_K);

        let explicit_low = build_query_plan("latest 5 releases", None, Some(6), None);
        assert_eq!(explicit_low.top_k, 6);

        let capped = build_query_plan("latest 100 releases", None, None, None);
        assert_eq!(capped.top_k, DEFAULT_TOP_K);
    }

    #[test]
    fn provider_free_plan_does_not_infer_semantics_from_prose_case() {
        let lowercase = build_task_query_plan(&query_plan_task_input(
            "explain callbackurl and database_url",
            None,
        ))
        .expect("provider-free plan should build");
        let mixed_case = build_task_query_plan(&query_plan_task_input(
            "Explain callbackUrl and DATABASE_URL",
            None,
        ))
        .expect("provider-free plan should build");

        assert_eq!(lowercase.intent_profile, QueryIntentProfile::default());
        assert_eq!(mixed_case.intent_profile, QueryIntentProfile::default());
        assert_eq!(lowercase.entity_keywords, lowercase.keywords);
        assert_eq!(lowercase.concept_keywords, lowercase.keywords);
        assert_eq!(mixed_case.entity_keywords, mixed_case.keywords);
        assert_eq!(mixed_case.concept_keywords, mixed_case.keywords);
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
            "Compare endpoint orders and inventory",
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
        assert!(!plan.intent_profile.multi_document_technical);
    }

    #[test]
    fn lexical_lanes_follow_query_ir_instead_of_keyword_position() {
        let keywords = vec![
            "common".to_string(),
            "prefix".to_string(),
            "shared".to_string(),
            "alpha".to_string(),
            "gamma".to_string(),
        ];

        let alpha = derive_lexical_lane_keywords(
            &keywords,
            Some(&query_ir_with_subject("Alpha Node", 0.9)),
        );
        let gamma = derive_lexical_lane_keywords(
            &keywords,
            Some(&query_ir_with_subject("Gamma Node", 0.9)),
        );

        assert_eq!(alpha.high_level, vec!["alpha".to_string()]);
        assert_eq!(gamma.high_level, vec!["gamma".to_string()]);
        assert_eq!(alpha.low_level, keywords);
        assert_eq!(gamma.low_level, keywords);
    }

    #[test]
    fn lexical_lanes_fallback_to_full_keywords_for_low_confidence_ir() {
        let keywords = vec!["common".to_string(), "prefix".to_string(), "gamma".to_string()];
        let lanes = derive_lexical_lane_keywords(
            &keywords,
            Some(&query_ir_with_subject("Gamma Node", 0.59)),
        );

        assert_eq!(lanes.high_level, keywords);
        assert_eq!(lanes.low_level, keywords);
    }

    #[test]
    fn canonical_query_ir_controls_intent_and_keyword_lanes() {
        let query_ir = QueryIR {
            act: QueryAct::RetrieveValue,
            scope: QueryScope::MultiDocument,
            target_entities: vec![
                EntityMention { label: "Alpha".to_string(), role: EntityRole::Subject },
                EntityMention { label: "Gamma".to_string(), role: EntityRole::Modifier },
            ],
            literal_constraints: vec![LiteralSpan {
                text: "callbackUrl".to_string(),
                kind: LiteralKind::Identifier,
            }],
            ..query_ir_with_subject("unused", 0.9)
        };

        let plan = build_task_query_plan(&query_plan_task_input(
            "Common prefix Alpha Gamma callbackUrl",
            Some(query_ir),
        ))
        .expect("typed query plan should build");

        assert!(plan.intent_profile.exact_literal_technical);
        assert!(plan.intent_profile.multi_document_technical);
        assert!(plan.entity_keywords.contains(&"alpha".to_string()));
        assert!(plan.entity_keywords.contains(&"callbackurl".to_string()));
        assert_eq!(plan.concept_keywords, vec!["gamma".to_string()]);
    }

    #[test]
    fn typed_enum_labels_never_become_language_specific_lexical_seeds() {
        let query_ir = QueryIR {
            act: QueryAct::Enumerate,
            scope: QueryScope::MultiDocument,
            target_types: vec![crate::domains::query_ir::QueryTargetKind::Release],
            source_slice: Some(crate::domains::query_ir::SourceSliceSpec {
                direction: crate::domains::query_ir::SourceSliceDirection::Tail,
                count: Some(5),
                filter: crate::domains::query_ir::SourceSliceFilter::ReleaseMarker,
            }),
            confidence: 0.9,
            ..query_ir_with_subject("Actual Product", 0.9)
        };

        let plan = build_task_query_plan(&query_plan_task_input(
            "Actual Product release marker tail",
            Some(query_ir),
        ))
        .expect("typed query plan should build");

        assert_eq!(plan.entity_keywords, vec!["actual".to_string(), "product".to_string()]);
        assert_eq!(plan.concept_keywords, plan.keywords);
    }

    #[test]
    fn low_confidence_query_ir_keeps_entity_and_concept_lanes_neutral() {
        let query_ir = query_ir_with_subject("Alpha", QUERY_IR_LEXICAL_LANE_MIN_CONFIDENCE - 0.01);
        let plan = build_task_query_plan(&query_plan_task_input(
            "Common prefix Alpha Gamma",
            Some(query_ir),
        ))
        .expect("low-confidence typed plan should build");

        assert_eq!(plan.entity_keywords, plan.keywords);
        assert_eq!(plan.concept_keywords, plan.keywords);
    }

    #[test]
    fn invalid_typed_identifier_literal_does_not_enable_exact_profile() {
        let query_ir = QueryIR {
            act: QueryAct::RetrieveValue,
            literal_constraints: vec![LiteralSpan {
                text: "ordinary".to_string(),
                kind: LiteralKind::Identifier,
            }],
            ..query_ir_with_subject("Alpha", 0.9)
        };
        let plan = build_task_query_plan(&query_plan_task_input(
            "Explain ordinary Alpha settings",
            Some(query_ir),
        ))
        .expect("typed query plan should build");

        assert!(!plan.intent_profile.exact_literal_technical);
    }
}
