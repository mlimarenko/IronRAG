//! Typed question intent classification.
//!
//! Runtime intent classification is QueryIR-driven. Raw natural-language
//! keyword tables are intentionally not used here: the compiler/provider
//! owns language understanding, and this module only translates typed IR
//! tags into local answer-builder intents.

use crate::domains::query_ir::{EntityRole, LiteralKind, QueryAct, QueryIR, QueryTargetKind};

/// A recognized question intent. Downstream builders use these to
/// pick the right answer strategy (fact-store lookup, evidence scan,
/// LLM synthesis).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum QuestionIntent {
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
pub(crate) enum ExactUrlLookupKind {
    Url,
    Wsdl,
}

pub(crate) fn classify_query_ir_intents(ir: &QueryIR) -> Vec<QuestionIntent> {
    let mut intents = Vec::new();
    for target_type in &ir.target_types {
        let intent = match *target_type {
            target if target_kind_is_endpoint_lookup(target) => Some(QuestionIntent::Endpoint),
            QueryTargetKind::BaseUrl => Some(QuestionIntent::BasePrefix),
            QueryTargetKind::Parameter => Some(QuestionIntent::Parameter),
            QueryTargetKind::HttpMethod => Some(QuestionIntent::HttpMethod),
            QueryTargetKind::Version => Some(QuestionIntent::Version),
            QueryTargetKind::ErrorCode => Some(QuestionIntent::ErrorCode),
            QueryTargetKind::EnvVar => Some(QuestionIntent::EnvVar),
            QueryTargetKind::ConfigKey => Some(QuestionIntent::ConfigKey),
            QueryTargetKind::Protocol => Some(QuestionIntent::Protocol),
            QueryTargetKind::Port => Some(QuestionIntent::Port),
            QueryTargetKind::FormatsUnderTest => Some(QuestionIntent::FocusedFormatsUnderTest),
            QueryTargetKind::SecondaryHeading => Some(QuestionIntent::FocusedSecondaryHeading),
            QueryTargetKind::PrimaryHeading => Some(QuestionIntent::FocusedPrimaryHeading),
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

pub(crate) fn classify_question_or_ir_intents(
    _question: &str,
    ir: &QueryIR,
) -> Vec<QuestionIntent> {
    classify_query_ir_intents(ir)
}

pub(crate) fn query_ir_targets_graph_entities_or_relationships(query_ir: &QueryIR) -> bool {
    query_ir.target_types.iter().any(|target| {
        matches!(
            target,
            QueryTargetKind::Person
                | QueryTargetKind::Organization
                | QueryTargetKind::Location
                | QueryTargetKind::Event
                | QueryTargetKind::Artifact
                | QueryTargetKind::Natural
                | QueryTargetKind::Process
                | QueryTargetKind::Concept
                | QueryTargetKind::Attribute
                | QueryTargetKind::Entity
                | QueryTargetKind::Relationship
        )
    })
}

pub(crate) fn query_ir_has_endpoint_request_signal(query_ir: &QueryIR) -> bool {
    query_ir_has_specific_endpoint_lookup_target(query_ir)
        || query_ir.literal_constraints.iter().any(|literal| {
            matches!(literal.kind, LiteralKind::Url | LiteralKind::Path)
                && !query_ir_disallows_graph_id_like_endpoint_candidate(query_ir, &literal.text)
        })
}

pub(crate) fn query_ir_allows_deterministic_endpoint_lookup(query_ir: &QueryIR) -> bool {
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

/// Conservatively rejects an absolute-path candidate when typed QueryIR names
/// graph targets but does not explicitly name an endpoint target. Path segment
/// spelling is intentionally ignored; semantic disambiguation belongs to the
/// QueryIR compiler rather than a handwritten namespace or word list here.
pub(crate) fn query_ir_disallows_graph_id_like_endpoint_path(
    query_ir: &QueryIR,
    path: &str,
) -> bool {
    query_ir_targets_graph_entities_or_relationships(query_ir)
        && !query_ir_has_explicit_endpoint_lookup_target(query_ir)
        && is_absolute_path_candidate(path)
}

/// Candidate-level form of [`query_ir_disallows_graph_id_like_endpoint_path`].
/// An explicit typed endpoint target wins; otherwise graph/path ambiguity
/// fails closed without guessing from identifier spelling.
pub(crate) fn query_ir_disallows_graph_id_like_endpoint_candidate(
    query_ir: &QueryIR,
    candidate: &str,
) -> bool {
    if query_ir_has_explicit_endpoint_lookup_target(query_ir) {
        return false;
    }
    if query_ir_disallows_graph_id_like_endpoint_path(query_ir, candidate) {
        return true;
    }

    endpoint_candidate_url_path(candidate)
        .is_some_and(|path| query_ir_disallows_graph_id_like_endpoint_path(query_ir, path))
}

fn query_ir_has_specific_endpoint_lookup_target(query_ir: &QueryIR) -> bool {
    query_ir.targets_any(&[QueryTargetKind::Url, QueryTargetKind::Wsdl])
}

fn query_ir_has_explicit_endpoint_lookup_target(query_ir: &QueryIR) -> bool {
    query_ir.target_types.iter().copied().any(target_kind_is_endpoint_lookup)
}

pub(crate) fn query_ir_has_setup_configuration_target(query_ir: &QueryIR) -> bool {
    query_ir.targets_any(&[
        QueryTargetKind::ConfigurationFile,
        QueryTargetKind::ConfigKey,
        QueryTargetKind::Parameter,
        QueryTargetKind::Package,
    ])
}

/// Returns true when deterministic fact/list renderers must yield to grounded
/// synthesis that can connect a specific issue to its remediation. The
/// predicate is intentionally QueryIR-driven so it stays domain- and
/// language-neutral. A plain inventory of error codes remains eligible for
/// deterministic rendering; an error target becomes remediation intent only
/// when the requested act is procedural.
pub(crate) fn query_ir_requires_remediation_synthesis(query_ir: &QueryIR) -> bool {
    let mut has_explicit_remediation_target = false;
    let mut has_error_target = false;
    let mut has_procedure_target = false;
    for target_type in &query_ir.target_types {
        match target_type {
            QueryTargetKind::Troubleshooting | QueryTargetKind::Remediation => {
                has_explicit_remediation_target = true;
            }
            QueryTargetKind::ErrorMessage | QueryTargetKind::ErrorCode => has_error_target = true,
            QueryTargetKind::Procedure => has_procedure_target = true,
            _ => {}
        }
    }
    let has_procedural_act = matches!(query_ir.act, QueryAct::ConfigureHow)
        || (matches!(query_ir.act, QueryAct::Describe) && has_procedure_target);
    has_explicit_remediation_target || (has_procedural_act && has_error_target)
}

pub(crate) fn query_ir_is_unambiguous_versioned_procedure(query_ir: &QueryIR) -> bool {
    let mut has_procedure = false;
    let mut has_concept = false;
    let mut has_version_or_release = false;
    for target_type in &query_ir.target_types {
        match target_type {
            QueryTargetKind::Procedure => has_procedure = true,
            QueryTargetKind::Concept => has_concept = true,
            QueryTargetKind::Version | QueryTargetKind::Release => has_version_or_release = true,
            _ => {}
        }
    }
    let has_procedure_action = matches!(query_ir.act, QueryAct::ConfigureHow);
    let has_procedure_signal = has_version_or_release && (has_procedure || has_procedure_action);
    let has_subject_signal =
        query_ir.target_entities.iter().any(|entity| matches!(entity.role, EntityRole::Subject))
            || (has_version_or_release && query_ir.document_focus.is_some());
    has_procedure_signal
        && has_subject_signal
        && !has_concept
        && (!query_ir_has_setup_configuration_target(query_ir) || has_procedure)
        && query_ir.needs_clarification.is_none()
}

pub(crate) fn query_ir_allows_procedure_runbook_target(query_ir: &QueryIR) -> bool {
    if query_ir.needs_clarification.is_some() || query_ir_has_setup_configuration_target(query_ir) {
        return false;
    }
    let mut has_procedure = false;
    let mut has_runbook_document_target = false;
    let mut has_generic_concept = false;
    for target_type in &query_ir.target_types {
        match target_type {
            QueryTargetKind::Procedure => has_procedure = true,
            QueryTargetKind::Document
            | QueryTargetKind::PrimaryHeading
            | QueryTargetKind::SecondaryHeading => {
                has_runbook_document_target = true;
            }
            QueryTargetKind::Concept => has_generic_concept = true,
            _ => {}
        }
    }
    has_procedure && has_runbook_document_target && !has_generic_concept
}

const fn target_kind_is_endpoint_lookup(kind: QueryTargetKind) -> bool {
    matches!(
        kind,
        QueryTargetKind::Endpoint
            | QueryTargetKind::Path
            | QueryTargetKind::Url
            | QueryTargetKind::Wsdl
    )
}

fn endpoint_candidate_url_path(candidate: &str) -> Option<&str> {
    let candidate = candidate.trim();
    let scheme_index = candidate.find("://")?;
    let remainder = &candidate[(scheme_index + 3)..];
    let path_index = remainder.find('/')?;
    Some(&remainder[path_index..])
}

fn is_absolute_path_candidate(path: &str) -> bool {
    path.trim().starts_with('/')
}

pub(crate) fn has_question_intent(intents: &[QuestionIntent], intent: QuestionIntent) -> bool {
    intents.contains(&intent)
}

pub(crate) fn classify_exact_url_lookup(
    query_ir: &QueryIR,
    intents: &[QuestionIntent],
) -> Option<ExactUrlLookupKind> {
    if !has_question_intent(intents, QuestionIntent::Endpoint) {
        return None;
    }

    let asks_wsdl = query_ir.targets(QueryTargetKind::Wsdl);
    let asks_url_like =
        asks_wsdl || query_ir.targets_any(&[QueryTargetKind::Url, QueryTargetKind::BaseUrl]);

    asks_url_like.then_some(if asks_wsdl {
        ExactUrlLookupKind::Wsdl
    } else {
        ExactUrlLookupKind::Url
    })
}

pub(crate) fn query_ir_blocks_endpoint_lookup(query_ir: &QueryIR) -> bool {
    classify_query_ir_intents(query_ir)
        .iter()
        .any(|intent| matches!(intent, QuestionIntent::Port | QuestionIntent::Protocol))
}

pub(crate) fn query_ir_has_focused_document_answer_intent(query_ir: &QueryIR) -> bool {
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
    fn identifier_literal_without_parameter_target_is_not_parameter_intent() {
        let mut ir = test_ir(["concept"]);
        ir.literal_constraints = vec![LiteralSpan {
            text: "mixedCaseIdentifier".to_string(),
            kind: LiteralKind::Identifier,
        }];

        let intents = classify_query_ir_intents(&ir);

        assert!(!intents.contains(&QuestionIntent::Parameter));
    }

    #[test]
    fn empty_on_unrelated_query_ir() {
        let intents = classify_query_ir_intents(&test_ir(["concept"]));
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
    fn typed_endpoint_path_is_not_reclassified_as_a_graph_identifier() {
        let ir = QueryIR {
            literal_constraints: vec![LiteralSpan {
                text: "/api/v1/order-items".to_string(),
                kind: LiteralKind::Path,
            }],
            ..test_ir_with_act(QueryAct::RetrieveValue, ["endpoint", "entity"])
        };

        assert!(query_ir_has_endpoint_request_signal(&ir));
        assert!(query_ir_allows_deterministic_endpoint_lookup(&ir));
        assert!(!query_ir_disallows_graph_id_like_endpoint_candidate(&ir, "/api/v1/order-items"));
    }

    #[test]
    fn graph_path_without_endpoint_target_fails_closed_for_unseen_namespace() {
        let ir = QueryIR {
            literal_constraints: vec![LiteralSpan {
                text: "/ns-9/Record_42".to_string(),
                kind: LiteralKind::Path,
            }],
            ..test_ir_with_act(QueryAct::RetrieveValue, ["artifact"])
        };

        assert!(query_ir_disallows_graph_id_like_endpoint_candidate(&ir, "/ns-9/Record_42"));
        assert!(!query_ir_allows_deterministic_endpoint_lookup(&ir));
    }

    #[test]
    fn graph_paths_without_explicit_endpoint_target_fail_closed() {
        let ir = test_ir_with_act(QueryAct::RetrieveValue, ["artifact"]);
        assert!(query_ir_disallows_graph_id_like_endpoint_path(&ir, "/wiki/Knowledge_graph"));
        assert!(query_ir_disallows_graph_id_like_endpoint_path(&ir, "/wiki/knowledge-graph"));
        assert!(query_ir_disallows_graph_id_like_endpoint_path(&ir, "/entity/Q1731"));
        assert!(query_ir_disallows_graph_id_like_endpoint_path(&ir, "/system/info"));
    }

    #[test]
    fn graph_path_filter_does_not_depend_on_namespace_or_terminal_spelling() {
        let ir = test_ir_with_act(QueryAct::RetrieveValue, ["artifact"]);

        assert!(query_ir_disallows_graph_id_like_endpoint_path(&ir, "/catalog/Record_42"));
        assert!(query_ir_disallows_graph_id_like_endpoint_path(&ir, "/пространство/Объект_42"));
        assert!(query_ir_disallows_graph_id_like_endpoint_path(&ir, "/catalog/record"));
    }

    #[test]
    fn graph_url_candidates_without_explicit_endpoint_target_fail_closed() {
        let ir = test_ir_with_act(QueryAct::RetrieveValue, ["entity"]);
        assert!(query_ir_disallows_graph_id_like_endpoint_candidate(
            &ir,
            "https://example.org/wiki/Knowledge_graph"
        ));
        assert!(query_ir_disallows_graph_id_like_endpoint_candidate(
            &ir,
            "https://example.org/system/info"
        ));
    }

    #[test]
    fn explicit_endpoint_target_prevents_path_spelling_reclassification() {
        let ir = test_ir_with_act(QueryAct::RetrieveValue, ["endpoint", "entity"]);
        assert!(!query_ir_disallows_graph_id_like_endpoint_candidate(&ir, "/wiki/Knowledge_graph"));
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

    #[test]
    fn detects_versioned_update_from_configure_version_target() {
        let configure =
            test_ir_with_document_focus(QueryAct::ConfigureHow, ["artifact", "version"]);
        let describe_procedure =
            test_ir_with_document_focus(QueryAct::Describe, ["procedure", "release"]);
        let describe_release_document =
            test_ir_with_document_focus(QueryAct::Describe, ["document", "release"]);
        let setup_version =
            test_ir_with_document_focus(QueryAct::ConfigureHow, ["package", "version"]);
        let setup_procedure_version = test_ir_with_document_focus(
            QueryAct::ConfigureHow,
            ["package", "procedure", "version"],
        );
        assert!(query_ir_is_unambiguous_versioned_procedure(&configure));
        assert!(query_ir_is_unambiguous_versioned_procedure(&describe_procedure));
        assert!(!query_ir_is_unambiguous_versioned_procedure(&describe_release_document));
        assert!(!query_ir_is_unambiguous_versioned_procedure(&setup_version));
        assert!(query_ir_is_unambiguous_versioned_procedure(&setup_procedure_version));
        assert!(!query_ir_is_unambiguous_versioned_procedure(&test_ir_with_act(
            QueryAct::RetrieveValue,
            ["artifact", "version"]
        )));
        assert!(!query_ir_is_unambiguous_versioned_procedure(&test_ir_with_act(
            QueryAct::ConfigureHow,
            ["artifact", "version"]
        )));
    }

    #[test]
    fn procedure_runbook_target_requires_procedure_document_without_setup_or_concept() {
        assert!(query_ir_allows_procedure_runbook_target(&test_ir_with_act(
            QueryAct::ConfigureHow,
            ["procedure", "document"]
        )));
        assert!(query_ir_allows_procedure_runbook_target(&test_ir_with_act(
            QueryAct::Describe,
            ["procedure", "primary_heading"]
        )));
        assert!(!query_ir_allows_procedure_runbook_target(&test_ir_with_act(
            QueryAct::ConfigureHow,
            ["procedure", "document", "concept"]
        )));
        assert!(!query_ir_allows_procedure_runbook_target(&test_ir_with_act(
            QueryAct::ConfigureHow,
            ["procedure", "document", "package"]
        )));
    }

    #[test]
    fn remediation_synthesis_is_act_aware_and_keeps_error_code_inventories_deterministic() {
        assert!(query_ir_requires_remediation_synthesis(&test_ir_with_act(
            QueryAct::Describe,
            ["troubleshooting"]
        )));
        assert!(query_ir_requires_remediation_synthesis(&test_ir_with_act(
            QueryAct::ConfigureHow,
            ["error_message"]
        )));
        assert!(query_ir_requires_remediation_synthesis(&test_ir_with_act(
            QueryAct::Describe,
            ["procedure", "error_message"]
        )));
        assert!(!query_ir_requires_remediation_synthesis(&test_ir_with_act(
            QueryAct::Enumerate,
            ["error_code"]
        )));
    }

    fn test_ir<const N: usize>(target_types: [&str; N]) -> QueryIR {
        QueryIR {
            act: QueryAct::RetrieveValue,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: target_types
                .into_iter()
                .map(|value| {
                    QueryTargetKind::from_wire(value).expect("test target type must be canonical")
                })
                .collect(),
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

    fn test_ir_with_document_focus<const N: usize>(
        act: QueryAct,
        target_types: [&str; N],
    ) -> QueryIR {
        QueryIR {
            document_focus: Some(crate::domains::query_ir::DocumentHint {
                hint: "Alpha Service".to_string(),
            }),
            ..test_ir_with_act(act, target_types)
        }
    }
}
