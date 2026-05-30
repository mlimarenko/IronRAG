//! Typed question intent classification.
//!
//! Runtime intent classification is QueryIR-driven. Raw natural-language
//! keyword tables are intentionally not used here: the compiler/provider
//! owns language understanding, and this module only translates typed IR
//! tags into local answer-builder intents.

use crate::domains::query_ir::{LiteralKind, QueryAct, QueryIR, literal_text_is_identifier_shaped};

/// A recognized question intent. Downstream builders use these to
/// pick the right answer strategy (fact-store lookup, evidence scan,
/// LLM synthesis).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuestionIntent {
    /// "What is the URL/endpoint/WSDL for..."
    Endpoint,
    /// "What parameters does X accept?"
    Parameter,
    /// "What HTTP method / GET or POST?"
    HttpMethod,
    /// "What version?"
    Version,
    /// "What is the error code / what does E1234 mean?"
    ErrorCode,
    /// "What environment variable / $DATABASE_URL?"
    EnvVar,
    /// "What is the config key / default value?"
    ConfigKey,
    /// "What protocol — REST, SOAP, GraphQL?"
    Protocol,
    /// "What is the base URL?"
    BasePrefix,
    /// "What port does X use?"
    Port,
    /// "Which formats are listed under test in this document?"
    FocusedFormatsUnderTest,
    /// "What validating heading does this document contain?"
    FocusedSecondaryHeading,
    /// "What is the title / primary heading of this document?"
    FocusedPrimaryHeading,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExactUrlLookupKind {
    Url,
    Wsdl,
}

pub fn classify_query_ir_intents(ir: &QueryIR) -> Vec<QuestionIntent> {
    let mut intents = Vec::new();
    for target_type in &ir.target_types {
        let tag = canonical_target_type_tag(target_type);
        let intent = match tag.as_str() {
            tag if target_type_tag_is_endpoint_lookup(tag) => Some(QuestionIntent::Endpoint),
            "base_url" => Some(QuestionIntent::BasePrefix),
            "parameter" => Some(QuestionIntent::Parameter),
            "http_method" => Some(QuestionIntent::HttpMethod),
            "version" => Some(QuestionIntent::Version),
            "error_code" => Some(QuestionIntent::ErrorCode),
            "env_var" => Some(QuestionIntent::EnvVar),
            "config_key" => Some(QuestionIntent::ConfigKey),
            "protocol" => Some(QuestionIntent::Protocol),
            "port" => Some(QuestionIntent::Port),
            "formats_under_test" => Some(QuestionIntent::FocusedFormatsUnderTest),
            "secondary_heading" => Some(QuestionIntent::FocusedSecondaryHeading),
            "primary_heading" => Some(QuestionIntent::FocusedPrimaryHeading),
            _ => None,
        };
        if let Some(intent) = intent
            && !intents.contains(&intent)
        {
            intents.push(intent);
        }
    }
    for literal in &ir.literal_constraints {
        let intent = match literal.kind {
            LiteralKind::Url | LiteralKind::Path => Some(QuestionIntent::Endpoint),
            LiteralKind::Version => Some(QuestionIntent::Version),
            LiteralKind::Identifier if literal_text_is_identifier_shaped(&literal.text) => {
                Some(QuestionIntent::Parameter)
            }
            LiteralKind::Identifier | LiteralKind::NumericCode | LiteralKind::Other => None,
        };
        if let Some(intent) = intent
            && !intents.contains(&intent)
        {
            intents.push(intent);
        }
    }
    intents
}

pub fn classify_question_or_ir_intents(_question: &str, ir: &QueryIR) -> Vec<QuestionIntent> {
    classify_query_ir_intents(ir)
}

pub fn query_ir_targets_graph_entities_or_relationships(query_ir: &QueryIR) -> bool {
    query_ir.target_types.iter().map(|value| canonical_target_type_tag(value)).any(|tag| {
        matches!(
            tag.as_str(),
            "person"
                | "organization"
                | "location"
                | "event"
                | "artifact"
                | "natural"
                | "process"
                | "concept"
                | "attribute"
                | "entity"
                | "relationship"
        )
    })
}

pub fn query_ir_has_endpoint_request_signal(query_ir: &QueryIR) -> bool {
    query_ir_has_specific_endpoint_lookup_target(query_ir)
        || query_ir.literal_constraints.iter().any(|literal| {
            matches!(literal.kind, LiteralKind::Url | LiteralKind::Path)
                && !query_ir_disallows_graph_id_like_endpoint_candidate(query_ir, &literal.text)
        })
}

pub fn query_ir_allows_deterministic_endpoint_lookup(query_ir: &QueryIR) -> bool {
    if !has_question_intent(&classify_query_ir_intents(query_ir), QuestionIntent::Endpoint) {
        return false;
    }
    if query_ir_blocks_endpoint_lookup(query_ir) {
        return false;
    }
    if !matches!(query_ir.act, QueryAct::RetrieveValue | QueryAct::Compare) {
        return false;
    }
    if query_ir_targets_graph_entities_or_relationships(query_ir) {
        return query_ir_has_endpoint_request_signal(query_ir);
    }
    true
}

pub fn query_ir_disallows_graph_id_like_endpoint_path(query_ir: &QueryIR, path: &str) -> bool {
    query_ir_targets_graph_entities_or_relationships(query_ir) && is_graph_id_like_path(path)
}

pub fn query_ir_disallows_graph_id_like_endpoint_candidate(
    query_ir: &QueryIR,
    candidate: &str,
) -> bool {
    if query_ir_has_specific_endpoint_lookup_target(query_ir) {
        return false;
    }
    if query_ir_disallows_graph_id_like_endpoint_path(query_ir, candidate) {
        return true;
    }

    endpoint_candidate_url_path(candidate)
        .is_some_and(|path| query_ir_disallows_graph_id_like_endpoint_path(query_ir, path))
}

fn query_ir_has_specific_endpoint_lookup_target(query_ir: &QueryIR) -> bool {
    query_ir
        .target_types
        .iter()
        .any(|value| matches!(canonical_target_type_tag(value).as_str(), "url" | "wsdl"))
}

pub(crate) fn canonical_target_type_tag(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn target_type_tag_is_endpoint_lookup(tag: &str) -> bool {
    matches!(tag, "endpoint" | "path" | "url" | "wsdl")
}

fn endpoint_candidate_url_path(candidate: &str) -> Option<&str> {
    let candidate = candidate.trim();
    let scheme_index = candidate.find("://")?;
    let remainder = &candidate[(scheme_index + 3)..];
    let path_index = remainder.find('/')?;
    Some(&remainder[path_index..])
}

fn is_graph_id_like_path(path: &str) -> bool {
    let path = path.trim();
    if !path.starts_with('/') {
        return false;
    }

    let segments = path
        .trim_end_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.len() < 2 {
        return false;
    }

    matches!(segments[0], "wiki" | "entity")
}

pub fn has_question_intent(intents: &[QuestionIntent], intent: QuestionIntent) -> bool {
    intents.contains(&intent)
}

pub fn classify_exact_url_lookup(
    query_ir: &QueryIR,
    intents: &[QuestionIntent],
) -> Option<ExactUrlLookupKind> {
    if !has_question_intent(intents, QuestionIntent::Endpoint) {
        return None;
    }

    let target_tags = query_ir
        .target_types
        .iter()
        .map(|value| canonical_target_type_tag(value))
        .collect::<Vec<_>>();
    let asks_wsdl = target_tags.iter().any(|tag| tag == "wsdl");
    let asks_url_like =
        asks_wsdl || target_tags.iter().any(|tag| matches!(tag.as_str(), "url" | "base_url"));

    asks_url_like.then_some(if asks_wsdl {
        ExactUrlLookupKind::Wsdl
    } else {
        ExactUrlLookupKind::Url
    })
}

pub fn query_ir_blocks_endpoint_lookup(query_ir: &QueryIR) -> bool {
    classify_query_ir_intents(query_ir)
        .iter()
        .any(|intent| matches!(intent, QuestionIntent::Port | QuestionIntent::Protocol))
}

pub fn query_ir_has_focused_document_answer_intent(query_ir: &QueryIR) -> bool {
    classify_query_ir_intents(query_ir).iter().any(|intent| {
        matches!(
            intent,
            QuestionIntent::FocusedFormatsUnderTest
                | QuestionIntent::FocusedSecondaryHeading
                | QuestionIntent::FocusedPrimaryHeading
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::query_ir::{LiteralSpan, QueryAct, QueryLanguage, QueryScope};

    #[test]
    fn classifies_endpoint_query_ir() {
        let ir = test_ir(["endpoint"]);
        let intents = classify_query_ir_intents(&ir);
        assert!(intents.contains(&QuestionIntent::Endpoint));
    }

    #[test]
    fn classifies_parameter_query_ir() {
        let ir = test_ir(["parameter", "endpoint"]);
        let intents = classify_query_ir_intents(&ir);
        assert!(intents.contains(&QuestionIntent::Parameter));
        assert!(intents.contains(&QuestionIntent::Endpoint));
    }

    #[test]
    fn classifies_version_query_ir() {
        let ir = test_ir(["version"]);
        let intents = classify_query_ir_intents(&ir);
        assert!(intents.contains(&QuestionIntent::Version));
    }

    #[test]
    fn classifies_config_query_ir() {
        let ir = test_ir(["config_key"]);
        let intents = classify_query_ir_intents(&ir);
        assert!(intents.contains(&QuestionIntent::ConfigKey));
    }

    #[test]
    fn empty_on_unrelated_query_ir() {
        let intents = classify_query_ir_intents(&test_ir(["general_topic"]));
        assert!(intents.is_empty());
    }

    #[test]
    fn classifies_exact_wsdl_lookup() {
        let ir = test_ir(["wsdl"]);
        let intents = classify_query_ir_intents(&ir);
        assert_eq!(classify_exact_url_lookup(&ir, &intents), Some(ExactUrlLookupKind::Wsdl));
    }

    #[test]
    fn classifies_relationship_query_type_as_non_endpoint() {
        let intents = classify_query_ir_intents(&test_ir(["relationship"]));
        assert!(!intents.contains(&QuestionIntent::Endpoint));
    }

    #[test]
    fn blocks_endpoint_lookup_for_protocol_ir() {
        assert!(query_ir_blocks_endpoint_lookup(&test_ir(["protocol"])));
    }

    #[test]
    fn allows_endpoint_lookup_for_retrieve_value_endpoint_query() {
        assert!(query_ir_allows_deterministic_endpoint_lookup(&test_ir(["endpoint"])));
    }

    #[test]
    fn blocks_endpoint_lookup_for_graph_relationship_without_endpoint_signal() {
        let ir = test_ir_with_act(QueryAct::Describe, ["entity", "relationship"]);
        assert!(!query_ir_allows_deterministic_endpoint_lookup(&ir));
    }

    #[test]
    fn blocks_endpoint_lookup_for_graph_relationship_with_only_generic_endpoint_target() {
        let ir = test_ir_with_act(QueryAct::RetrieveValue, ["endpoint", "relationship"]);
        assert!(!query_ir_allows_deterministic_endpoint_lookup(&ir));
    }

    #[test]
    fn blocks_endpoint_lookup_for_graph_relationship_with_path_target() {
        let ir = test_ir_with_act(QueryAct::RetrieveValue, ["relationship", "path"]);
        assert!(!query_ir_allows_deterministic_endpoint_lookup(&ir));
    }

    #[test]
    fn treats_relationship_as_graph_target_not_endpoint_lookup() {
        let ir = test_ir_with_act(QueryAct::RetrieveValue, ["relationship"]);
        assert!(query_ir_targets_graph_entities_or_relationships(&ir));
        assert!(!query_ir_allows_deterministic_endpoint_lookup(&ir));
    }

    #[test]
    fn keeps_non_graph_path_target_on_endpoint_lookup_path() {
        let ir = test_ir_with_act(QueryAct::RetrieveValue, ["path"]);
        assert!(query_ir_allows_deterministic_endpoint_lookup(&ir));
    }

    #[test]
    fn blocks_endpoint_lookup_when_only_endpoint_signal_is_graph_id_path_literal() {
        let mut ir = test_ir_with_act(QueryAct::RetrieveValue, ["relationship"]);
        ir.literal_constraints.push(LiteralSpan {
            text: "/wiki/Knowledge_graph".to_string(),
            kind: LiteralKind::Path,
        });

        assert!(!query_ir_allows_deterministic_endpoint_lookup(&ir));
    }

    #[test]
    fn disallows_graph_id_like_paths_for_graph_intents() {
        let ir = test_ir_with_act(QueryAct::RetrieveValue, ["artifact"]);
        assert!(query_ir_disallows_graph_id_like_endpoint_path(&ir, "/wiki/Knowledge_graph"));
        assert!(query_ir_disallows_graph_id_like_endpoint_path(&ir, "/wiki/knowledge-graph"));
        assert!(query_ir_disallows_graph_id_like_endpoint_path(&ir, "/entity/Q1731"));
        assert!(!query_ir_disallows_graph_id_like_endpoint_path(&ir, "/system/info"));
    }

    #[test]
    fn disallows_graph_id_like_url_candidates_for_graph_intents() {
        let ir = test_ir_with_act(QueryAct::RetrieveValue, ["entity"]);
        assert!(query_ir_disallows_graph_id_like_endpoint_candidate(
            &ir,
            "https://example.org/wiki/Knowledge_graph"
        ));
        assert!(!query_ir_disallows_graph_id_like_endpoint_candidate(
            &ir,
            "https://example.org/system/info"
        ));
    }

    #[test]
    fn disallows_graph_namespace_candidate_when_only_generic_endpoint_target_names_graph_entity() {
        let ir = test_ir_with_act(QueryAct::RetrieveValue, ["endpoint", "entity"]);
        assert!(query_ir_disallows_graph_id_like_endpoint_candidate(&ir, "/wiki/Knowledge_graph"));
    }

    #[test]
    fn keeps_graph_namespace_candidate_when_url_target_is_explicit() {
        let ir = test_ir_with_act(QueryAct::RetrieveValue, ["url", "entity"]);
        assert!(!query_ir_disallows_graph_id_like_endpoint_candidate(&ir, "/wiki/Knowledge_graph"));
    }

    #[test]
    fn classifies_port_query_ir_without_report_false_positive() {
        let report_intents = classify_query_ir_intents(&test_ir(["secondary_heading"]));
        assert!(!report_intents.contains(&QuestionIntent::Port));
        assert!(report_intents.contains(&QuestionIntent::FocusedSecondaryHeading));

        let port_intents = classify_query_ir_intents(&test_ir(["port"]));
        assert!(port_intents.contains(&QuestionIntent::Port));
    }

    #[test]
    fn classifies_focused_secondary_heading_request() {
        let intents = classify_query_ir_intents(&test_ir(["secondary_heading"]));
        assert!(intents.contains(&QuestionIntent::FocusedSecondaryHeading));
    }

    #[test]
    fn classifies_focused_formats_under_test_request() {
        let intents = classify_query_ir_intents(&test_ir(["formats_under_test"]));
        assert!(intents.contains(&QuestionIntent::FocusedFormatsUnderTest));
    }

    #[test]
    fn detects_focused_document_answer_intent() {
        assert!(query_ir_has_focused_document_answer_intent(&test_ir(["secondary_heading"])));
        assert!(query_ir_has_focused_document_answer_intent(&test_ir(["primary_heading"])));
        assert!(!query_ir_has_focused_document_answer_intent(&test_ir(["endpoint"])));
    }

    fn test_ir<const N: usize>(target_types: [&str; N]) -> QueryIR {
        QueryIR {
            act: QueryAct::RetrieveValue,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: target_types.into_iter().map(str::to_string).collect(),
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

    fn test_ir_with_act<const N: usize>(act: QueryAct, target_types: [&str; N]) -> QueryIR {
        QueryIR { act, ..test_ir(target_types) }
    }
}
