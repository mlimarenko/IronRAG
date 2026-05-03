//! Typed question intent classification.
//!
//! Runtime intent classification is QueryIR-driven. Raw natural-language
//! keyword tables are intentionally not used here: the compiler/provider
//! owns language understanding, and this module only translates typed IR
//! tags into local answer-builder intents.

use crate::domains::query_ir::{LiteralKind, QueryIR};

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
    /// "What version / which release?"
    Version,
    /// "What is the error code / what does E1234 mean?"
    ErrorCode,
    /// "What environment variable / $DATABASE_URL?"
    EnvVar,
    /// "What is the config key / setting / default value?"
    ConfigKey,
    /// "What protocol — REST, SOAP, GraphQL?"
    Protocol,
    /// "What is the base URL / prefix?"
    BasePrefix,
    /// "What port does X use?"
    Port,
    /// "Which formats are listed under test in this document?"
    FocusedFormatsUnderTest,
    /// "What report name / validating heading does this document contain?"
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
        let tag = target_type.trim().to_ascii_lowercase().replace('-', "_");
        let intent = match tag.as_str() {
            "endpoint" | "api_endpoint" | "route" | "api_route" | "path" | "url" | "wsdl" => {
                Some(QuestionIntent::Endpoint)
            }
            "base_url" | "base_path" | "prefix" => Some(QuestionIntent::BasePrefix),
            "parameter" | "query_parameter" | "request_parameter" => {
                Some(QuestionIntent::Parameter)
            }
            "http_method" | "method" => Some(QuestionIntent::HttpMethod),
            "version" | "release" | "changelog" | "change_log" => Some(QuestionIntent::Version),
            "error_code" | "status_code" => Some(QuestionIntent::ErrorCode),
            "env_var" | "environment_variable" => Some(QuestionIntent::EnvVar),
            "config_key" | "setting" => Some(QuestionIntent::ConfigKey),
            "protocol" | "transport" => Some(QuestionIntent::Protocol),
            "port" => Some(QuestionIntent::Port),
            "formats_under_test" => Some(QuestionIntent::FocusedFormatsUnderTest),
            "secondary_heading" | "report_name" => Some(QuestionIntent::FocusedSecondaryHeading),
            "primary_heading" | "title" => Some(QuestionIntent::FocusedPrimaryHeading),
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
            LiteralKind::Identifier => Some(QuestionIntent::Parameter),
            LiteralKind::NumericCode | LiteralKind::Other => None,
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
        .map(|value| value.trim().to_ascii_lowercase().replace('-', "_"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::query_ir::{QueryAct, QueryLanguage, QueryScope};

    #[test]
    fn classifies_endpoint_query_ir() {
        let ir = test_ir(["endpoint"]);
        let intents = classify_query_ir_intents(&ir);
        assert!(intents.contains(&QuestionIntent::Endpoint));
    }

    #[test]
    fn classifies_parameter_query_ir() {
        let ir = test_ir(["query_parameter", "endpoint"]);
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
    fn blocks_endpoint_lookup_for_transport_ir() {
        assert!(query_ir_blocks_endpoint_lookup(&test_ir(["transport"])));
    }

    #[test]
    fn classifies_port_query_ir_without_report_false_positive() {
        let report_intents = classify_query_ir_intents(&test_ir(["report_name"]));
        assert!(!report_intents.contains(&QuestionIntent::Port));
        assert!(report_intents.contains(&QuestionIntent::FocusedSecondaryHeading));

        let port_intents = classify_query_ir_intents(&test_ir(["port"]));
        assert!(port_intents.contains(&QuestionIntent::Port));
    }

    #[test]
    fn classifies_focused_secondary_heading_request() {
        let intents = classify_query_ir_intents(&test_ir(["report_name"]));
        assert!(intents.contains(&QuestionIntent::FocusedSecondaryHeading));
    }

    #[test]
    fn classifies_focused_formats_under_test_request() {
        let intents = classify_query_ir_intents(&test_ir(["formats_under_test"]));
        assert!(intents.contains(&QuestionIntent::FocusedFormatsUnderTest));
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
            confidence: 1.0,
        }
    }
}
