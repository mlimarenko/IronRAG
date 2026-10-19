use super::{
    AuthoritativeAnswerEvidence, KnowledgeTechnicalFactRow, QueryIR, QuestionIntent,
    RuntimeMatchedChunk, SetupConfigurationAnchorCandidate, TechnicalFactKind,
    build_exact_technical_literal_answer, build_focused_document_answer,
    build_multi_document_endpoint_answer_from_facts, build_port_and_protocol_answer_from_facts,
    build_port_answer_from_facts, build_setup_configuration_anchor_candidate,
    build_single_endpoint_answer_from_facts, build_structured_list_grounded_answer,
    build_structured_source_unit_inventory_answer, build_table_row_grounded_answer,
    build_table_summary_grounded_answer, build_update_procedure_sequence_answer,
    classify_question_or_ir_intents, extract_explicit_path_literals, extract_http_methods,
    extract_parameter_literals, extract_prefix_literals, extract_url_literals,
    query_ir_requires_remediation_synthesis, repair_technical_layout_noise,
};

pub(crate) fn build_deterministic_technical_answer(
    question: &str,
    query_ir: &crate::domains::query_ir::QueryIR,
    evidence: &AuthoritativeAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    accept_deterministic_technical_candidate(
        build_port_and_protocol_answer_from_facts(question, query_ir, evidence, chunks),
        question,
        query_ir,
        evidence,
        chunks,
    )
    .or_else(|| {
        accept_deterministic_technical_candidate(
            build_port_answer_from_facts(question, query_ir, evidence, chunks),
            question,
            query_ir,
            evidence,
            chunks,
        )
    })
    .or_else(|| {
        accept_deterministic_technical_candidate(
            build_single_endpoint_answer_from_facts(question, query_ir, evidence, chunks),
            question,
            query_ir,
            evidence,
            chunks,
        )
    })
    .or_else(|| {
        accept_deterministic_technical_candidate(
            build_multi_document_endpoint_answer_from_facts(question, query_ir, evidence, chunks),
            question,
            query_ir,
            evidence,
            chunks,
        )
    })
    .or_else(|| {
        accept_deterministic_technical_candidate(
            build_exact_technical_literal_answer(question, query_ir, evidence, chunks),
            question,
            query_ir,
            evidence,
            chunks,
        )
    })
}

fn accept_deterministic_technical_candidate(
    candidate: Option<String>,
    question: &str,
    query_ir: &crate::domains::query_ir::QueryIR,
    evidence: &AuthoritativeAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    candidate.filter(|answer| {
        deterministic_answer_satisfies_required_technical_facets(
            answer, question, query_ir, evidence, chunks,
        )
    })
}

fn deterministic_answer_satisfies_required_technical_facets(
    answer: &str,
    question: &str,
    query_ir: &crate::domains::query_ir::QueryIR,
    evidence: &AuthoritativeAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> bool {
    classify_question_or_ir_intents(question, query_ir).into_iter().all(|intent| {
        !intent_has_grounded_literal_evidence(intent, evidence, chunks)
            || answer_covers_technical_intent(answer, intent, evidence)
    })
}

fn intent_has_grounded_literal_evidence(
    intent: QuestionIntent,
    evidence: &AuthoritativeAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> bool {
    let fact_match = evidence.technical_facts.iter().any(|fact| {
        fact.fact_kind
            .parse::<TechnicalFactKind>()
            .is_ok_and(|kind| technical_fact_kind_supports_intent(kind, intent))
    });
    if fact_match {
        return true;
    }
    chunks.iter().any(|chunk| text_supports_technical_intent(&chunk.source_text, intent))
}

fn technical_fact_kind_supports_intent(kind: TechnicalFactKind, intent: QuestionIntent) -> bool {
    matches!(
        (kind, intent),
        (TechnicalFactKind::EndpointPath, QuestionIntent::Endpoint)
            | (TechnicalFactKind::Url, QuestionIntent::Endpoint)
            | (TechnicalFactKind::Url, QuestionIntent::BasePrefix)
            | (TechnicalFactKind::HttpMethod, QuestionIntent::HttpMethod)
            | (TechnicalFactKind::Port, QuestionIntent::Port)
            | (TechnicalFactKind::ParameterName, QuestionIntent::Parameter)
            | (TechnicalFactKind::StatusCode, QuestionIntent::ErrorCode)
            | (TechnicalFactKind::ErrorCode, QuestionIntent::ErrorCode)
            | (TechnicalFactKind::Protocol, QuestionIntent::Protocol)
            | (TechnicalFactKind::EnvironmentVariable, QuestionIntent::EnvVar)
            | (TechnicalFactKind::VersionNumber, QuestionIntent::Version)
            | (TechnicalFactKind::ConfigurationKey, QuestionIntent::ConfigKey)
    )
}

fn text_supports_technical_intent(text: &str, intent: QuestionIntent) -> bool {
    match intent {
        QuestionIntent::Endpoint => {
            !extract_explicit_path_literals(text, 1).is_empty()
                || !extract_url_literals(text, 1).is_empty()
        }
        QuestionIntent::BasePrefix => {
            !extract_prefix_literals(text, 1).is_empty()
                || !extract_url_literals(text, 1).is_empty()
        }
        QuestionIntent::HttpMethod => !extract_http_methods(text, 1).is_empty(),
        QuestionIntent::Parameter | QuestionIntent::ConfigKey | QuestionIntent::EnvVar => {
            !extract_parameter_literals(text, 1).is_empty()
        }
        QuestionIntent::Port
        | QuestionIntent::Version
        | QuestionIntent::ErrorCode
        | QuestionIntent::Protocol
        | QuestionIntent::FocusedFormatsUnderTest
        | QuestionIntent::FocusedSecondaryHeading
        | QuestionIntent::FocusedPrimaryHeading => false,
    }
}

fn answer_covers_technical_intent(
    answer: &str,
    intent: QuestionIntent,
    evidence: &AuthoritativeAnswerEvidence,
) -> bool {
    text_supports_technical_intent(answer, intent)
        || evidence.technical_facts.iter().any(|fact| {
            fact.fact_kind
                .parse::<TechnicalFactKind>()
                .is_ok_and(|kind| technical_fact_kind_supports_intent(kind, intent))
                && answer_contains_fact_value(answer, fact)
        })
}

fn answer_contains_fact_value(answer: &str, fact: &KnowledgeTechnicalFactRow) -> bool {
    let answer = answer.to_lowercase();
    for value in [
        fact.display_value.as_str(),
        fact.normalized_value_exact.as_str(),
        fact.normalized_value_text.as_str(),
    ] {
        let normalized = repair_technical_layout_noise(value).trim().to_lowercase();
        if !normalized.is_empty() && answer.contains(&normalized) {
            return true;
        }
    }
    false
}

pub(crate) fn build_deterministic_grounded_answer(
    question: &str,
    query_ir: &QueryIR,
    evidence: &AuthoritativeAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if query_ir_requires_remediation_synthesis(query_ir) {
        return None;
    }
    let answer = build_table_summary_grounded_answer(question, Some(query_ir), chunks)
        .or_else(|| build_table_row_grounded_answer(question, Some(query_ir), chunks))
        .or_else(|| {
            build_setup_configuration_anchor_candidate(question, query_ir, chunks)
                .filter(|candidate| candidate.should_use_as_preflight_answer(query_ir, chunks))
                .map(SetupConfigurationAnchorCandidate::into_answer)
        })
        .or_else(|| build_structured_list_grounded_answer(question, query_ir, chunks))
        .or_else(|| build_update_procedure_sequence_answer(query_ir, chunks))
        .or_else(|| build_focused_document_answer(question, query_ir, chunks))
        .or_else(|| build_structured_source_unit_inventory_answer(question, query_ir, chunks))
        .or_else(|| build_deterministic_technical_answer(question, query_ir, evidence, chunks))?;
    Some(augment_deterministic_grounded_answer_with_evidence(answer, question, query_ir, chunks))
}

pub(in crate::services::query::execution) fn augment_deterministic_grounded_answer_with_evidence(
    answer: String,
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> String {
    let _ = (question, query_ir, chunks);
    answer
}
