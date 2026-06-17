use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::domains::query::{
    DEFAULT_TOP_K, IntentKeywords, MAX_TOP_K, QueryPlanningMetadata, RuntimeQueryMode,
};
use crate::domains::query_ir::{
    EntityRole, QueryIR, SourceSliceDirection, SourceSliceFilter,
    literal_kind_has_exact_technical_shape, literal_text_is_identifier_shaped,
};
const DEFAULT_CONTEXT_BUDGET_CHARS: usize = 22_000;
/// Minimum token length after stripping punctuation. Tokens shorter than
/// this mostly carry no retrieval signal; a length cutoff avoids a
/// language-specific lexicon.
const TOKEN_MIN_LEN: usize = 3;
pub const QUERY_IR_LEXICAL_LANE_MIN_CONFIDENCE: f32 = 0.6;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryIntentProfile {
    pub exact_literal_technical: bool,
    pub multi_document_technical: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryPlanTaskInput {
    pub question: String,
    pub top_k: Option<usize>,
    pub explicit_mode: Option<RuntimeQueryMode>,
    pub metadata: Option<QueryPlanningMetadata>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
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
    strip_leading_question_marker(question)
        .split_whitespace()
        .map(|token| token.trim_matches(|ch: char| !ch.is_alphanumeric()))
        .filter(|token| token.chars().count() >= TOKEN_MIN_LEN)
        .map(str::to_lowercase)
        .filter(|token| seen.insert(token.clone()))
        .collect()
}

#[must_use]
pub fn strip_leading_question_marker(question: &str) -> &str {
    let trimmed = question.trim_start();
    let Some((marker, rest)) = trimmed.split_once(char::is_whitespace) else {
        return trimmed;
    };
    let marker = marker.trim_matches(|ch: char| matches!(ch, '(' | '[' | '{'));
    if !marker.ends_with(|ch: char| matches!(ch, '.' | ')' | ']' | '}' | ':' | '-')) {
        return trimmed;
    }
    let marker =
        marker.trim_end_matches(|ch: char| matches!(ch, '.' | ')' | ']' | '}' | ':' | '-'));
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

    if chars.first().is_some_and(|ch| ch.eq_ignore_ascii_case(&'q')) {
        let tail = &chars[1..];
        let digit_len = tail.iter().take_while(|ch| ch.is_ascii_digit()).count();
        return digit_len >= 2
            && (digit_len == tail.len()
                || (digit_len + 1 == tail.len()
                    && tail.last().is_some_and(|ch| ch.is_ascii_alphabetic())));
    }

    false
}

#[must_use]
pub fn choose_mode(explicit: Option<RuntimeQueryMode>, question: &str) -> RuntimeQueryMode {
    if let Some(explicit) = explicit {
        return explicit;
    }
    let _ = question;
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
    let lane_keywords = derive_lexical_lane_keywords(&keywords, None);
    let case_preserving = extract_keywords_preserving_case(question);
    let (entity_keywords, concept_keywords) = classify_keyword_levels(&case_preserving);

    let intent_profile = classify_query_intent_profile(question, &case_preserving);
    let hyde_recommended =
        intent_profile.multi_document_technical && !intent_profile.exact_literal_technical;

    RuntimeQueryPlan {
        requested_mode,
        planned_mode,
        intent_profile,
        keywords,
        high_level_keywords: lane_keywords.high_level,
        low_level_keywords: lane_keywords.low_level,
        entity_keywords,
        concept_keywords,
        top_k: planned_top_k(question, top_k),
        context_budget_chars: DEFAULT_CONTEXT_BUDGET_CHARS,
        hyde_recommended,
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

    let case_preserving = extract_keywords_preserving_case(question);
    let (entity_keywords, concept_keywords) = classify_keyword_levels(&case_preserving);

    let intent_profile = classify_query_intent_profile(question, &case_preserving);
    let hyde_recommended =
        intent_profile.multi_document_technical && !intent_profile.exact_literal_technical;

    RuntimeQueryPlan {
        requested_mode: metadata.requested_mode,
        planned_mode: metadata.planned_mode,
        intent_profile,
        keywords,
        high_level_keywords: metadata.keywords.high_level.clone(),
        low_level_keywords: metadata.keywords.low_level.clone(),
        entity_keywords,
        concept_keywords,
        top_k: planned_top_k(question, top_k),
        context_budget_chars: DEFAULT_CONTEXT_BUDGET_CHARS,
        hyde_recommended,
    }
}

fn classify_query_intent_profile(question: &str, keywords: &[String]) -> QueryIntentProfile {
    let lowered = question.trim().to_lowercase();
    let exact_literal_technical = is_exact_literal_technical_question(&lowered, keywords);
    QueryIntentProfile {
        exact_literal_technical,
        multi_document_technical: exact_literal_technical
            && is_multi_document_technical_question(&lowered),
    }
}

fn planned_top_k(question: &str, top_k: Option<usize>) -> usize {
    let _ = question;
    top_k.unwrap_or(DEFAULT_TOP_K).clamp(1, MAX_TOP_K)
}

fn is_exact_literal_technical_question(question: &str, keywords: &[String]) -> bool {
    question.contains("http://")
        || question.contains("https://")
        || question.contains('/')
        || keywords.iter().any(|keyword| literal_text_is_identifier_shaped(keyword))
}

fn is_multi_document_technical_question(question: &str) -> bool {
    let _ = question;
    false
}

/// Extracts keywords from a question preserving original case.
/// Used for entity-vs-concept classification where case matters.
#[must_use]
pub fn extract_keywords_preserving_case(question: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    strip_leading_question_marker(question)
        .split_whitespace()
        .map(|token| token.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '.'))
        .filter(|token| token.chars().count() >= TOKEN_MIN_LEN)
        .filter(|token| seen.insert(token.to_ascii_lowercase()))
        .map(|token| token.to_string())
        .collect()
}

/// Splits keywords into entity-level (specific names, technologies, functions)
/// and concept-level (abstract themes, topics, patterns).
#[must_use]
pub fn classify_keyword_levels(keywords: &[String]) -> (Vec<String>, Vec<String>) {
    let mut entity_keywords = Vec::new();
    let mut concept_keywords = Vec::new();

    for keyword in keywords {
        if is_entity_keyword(keyword) {
            entity_keywords.push(keyword.to_ascii_lowercase());
        } else {
            concept_keywords.push(keyword.to_ascii_lowercase());
        }
    }

    (entity_keywords, concept_keywords)
}

#[must_use]
pub fn derive_lexical_lane_keywords(
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
    for target_type in &query_ir.target_types {
        push_seed(&mut seeds.high, target_type);
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
    if let Some(source_slice) = &query_ir.source_slice {
        push_seed(&mut seeds.low, source_slice_direction_seed(source_slice.direction));
        if let Some(filter_seed) = source_slice_filter_seed(source_slice.filter) {
            push_seed(&mut seeds.low, filter_seed);
        }
    }
    seeds
}

const fn source_slice_direction_seed(direction: SourceSliceDirection) -> &'static str {
    match direction {
        SourceSliceDirection::Head => "head",
        SourceSliceDirection::Tail => "tail",
        SourceSliceDirection::All => "all",
    }
}

const fn source_slice_filter_seed(filter: SourceSliceFilter) -> Option<&'static str> {
    match filter {
        SourceSliceFilter::None => None,
        SourceSliceFilter::ReleaseMarker => Some("release_marker"),
    }
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

fn is_entity_keyword(keyword: &str) -> bool {
    // Entity keywords: proper nouns, technical names, specific identifiers
    // 1. Contains uppercase (likely proper noun): "PostgreSQL", "FastAPI", "OAuth"
    let has_upper = keyword.chars().any(|c| c.is_ascii_uppercase());
    // 2. Contains underscore/dot (technical identifier): "build_router", "app.config"
    let has_technical_chars = keyword.contains('_') || keyword.contains('.');
    // 3. Contains digits (version, port, ID): "v2.3", "8080", "HTTP2"
    let has_digits = keyword.chars().any(|c| c.is_ascii_digit());
    // 4. Starts with / (URL path): "/api/users"
    let is_path = keyword.starts_with('/');
    // 5. All caps with 2+ chars (acronym): "JWT", "API", "SQL"
    let is_acronym =
        keyword.len() >= 2 && keyword.chars().all(|c| c.is_ascii_uppercase() || c == '_');
    // 6. CamelCase: "ClassificationPipeline", "UserRole"
    let is_camel = keyword.len() > 2
        && keyword.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && keyword.chars().skip(1).any(|c| c.is_ascii_lowercase());

    has_upper || has_technical_chars || has_digits || is_path || is_acronym || is_camel
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::query_ir::{EntityMention, QueryAct, QueryLanguage, QueryScope};

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
    fn extract_keywords_strips_leading_question_markers() {
        let keywords = extract_keywords("Q16. Which ports should a terminal use?");
        assert!(!keywords.contains(&"q16".to_string()));
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
    fn leading_question_marker_strip_preserves_short_structural_prefixes() {
        assert_eq!(
            strip_leading_question_marker("Q16. Which ports should a terminal use?"),
            "Which ports should a terminal use?"
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
    fn exact_literal_profile_uses_structural_token_shape() {
        let plain = build_query_plan("Explain Alpha Suite settings", None, None, None);
        assert!(!plain.intent_profile.exact_literal_technical);

        let camel = build_query_plan("What does callbackUrl configure?", None, None, None);
        assert!(camel.intent_profile.exact_literal_technical);

        let acronym = build_query_plan("Where is DATABASE_URL documented?", None, None, None);
        assert!(acronym.intent_profile.exact_literal_technical);

        let separated = build_query_plan("Explain Настройка_2", None, None, None);
        assert!(separated.intent_profile.exact_literal_technical);
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
    fn metadata_query_plan_uses_question_shape_for_exact_literal_profile() {
        let metadata = QueryPlanningMetadata {
            requested_mode: RuntimeQueryMode::Hybrid,
            planned_mode: RuntimeQueryMode::Hybrid,
            intent_cache_status: crate::domains::query::QueryIntentCacheStatus::Miss,
            keywords: crate::domains::query::IntentKeywords {
                high_level: vec!["plain".to_string()],
                low_level: vec!["topic".to_string()],
            },
            warnings: Vec::new(),
        };

        let plain = build_query_plan_from_metadata("Explain Alpha Suite settings", &metadata, None);
        assert!(!plain.intent_profile.exact_literal_technical);

        let structural =
            build_query_plan_from_metadata("What does callbackUrl configure?", &metadata, None);
        assert!(structural.intent_profile.exact_literal_technical);
    }

    #[test]
    fn classifies_entity_vs_concept_keywords() {
        let (entities, concepts) = classify_keyword_levels(&[
            "PostgreSQL".to_string(),
            "authentication".to_string(),
            "JWT".to_string(),
            "security".to_string(),
            "build_router".to_string(),
            "performance".to_string(),
        ]);
        assert!(entities.contains(&"postgresql".to_string()));
        assert!(entities.contains(&"jwt".to_string()));
        assert!(entities.contains(&"build_router".to_string()));
        assert!(concepts.contains(&"authentication".to_string()));
        assert!(concepts.contains(&"security".to_string()));
        assert!(concepts.contains(&"performance".to_string()));
    }

    #[test]
    fn query_plan_populates_entity_and_concept_keywords() {
        let plan =
            build_query_plan("How does PostgreSQL handle JWT authentication?", None, None, None);

        assert!(plan.entity_keywords.contains(&"postgresql".to_string()));
        assert!(plan.entity_keywords.contains(&"jwt".to_string()));
        assert!(plan.concept_keywords.contains(&"authentication".to_string()));
        assert!(plan.concept_keywords.contains(&"handle".to_string()));
    }
}
