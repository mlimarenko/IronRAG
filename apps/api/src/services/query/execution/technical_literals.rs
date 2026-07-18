use crate::domains::query_ir::{
    LiteralKind, QueryIR, QueryTargetKind, literal_text_is_identifier_shaped,
};

use super::question_intent::query_ir_has_focused_document_answer_intent;
pub(super) use super::technical_literal_extractors::{
    extract_config_assignment_literals, extract_config_section_literals,
    extract_explicit_path_literals, extract_http_methods, extract_package_command_literals,
    extract_parameter_literals, extract_prefix_literals, extract_url_literals, push_unique_limited,
};
pub(super) use super::technical_literal_focus::{
    document_local_focus_keywords, select_document_balanced_chunks,
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
    detect_technical_literal_intent_from_query_ir_inner(query_ir, true)
}

pub(super) fn detect_explicit_technical_literal_intent_from_query_ir(
    _question: &str,
    query_ir: &QueryIR,
) -> TechnicalLiteralIntent {
    detect_technical_literal_intent_from_query_ir_inner(query_ir, false)
}

fn detect_technical_literal_intent_from_query_ir_inner(
    query_ir: &QueryIR,
    include_configure_setup: bool,
) -> TechnicalLiteralIntent {
    if query_ir_has_focused_document_answer_intent(query_ir) {
        return TechnicalLiteralIntent::default();
    }

    let mut intent = configure_setup_intent(query_ir, include_configure_setup);
    for target in &query_ir.target_types {
        apply_target_literal_intent(&mut intent, *target);
    }
    for literal in &query_ir.literal_constraints {
        apply_literal_constraint_intent(&mut intent, literal.kind, &literal.text);
    }
    if !intent.any() && query_ir.is_exact_literal_technical() {
        intent.wants_parameters = true;
    }
    intent
}

fn configure_setup_intent(
    query_ir: &QueryIR,
    include_configure_setup: bool,
) -> TechnicalLiteralIntent {
    let wants_setup = include_configure_setup
        && matches!(query_ir.act, crate::domains::query_ir::QueryAct::ConfigureHow)
        && (!query_ir.is_follow_up() || configure_follow_up_has_evidence_anchor(query_ir));
    TechnicalLiteralIntent {
        wants_paths: wants_setup,
        wants_parameters: wants_setup,
        ..TechnicalLiteralIntent::default()
    }
}

fn apply_target_literal_intent(intent: &mut TechnicalLiteralIntent, target: QueryTargetKind) {
    match target {
        QueryTargetKind::Endpoint
        | QueryTargetKind::Path
        | QueryTargetKind::Url
        | QueryTargetKind::Wsdl => {
            intent.wants_urls = true;
            intent.wants_paths = true;
            intent.wants_methods = true;
            intent.wants_parameters |= matches!(target, QueryTargetKind::Path);
        }
        QueryTargetKind::BaseUrl => {
            intent.wants_urls = true;
            intent.wants_prefixes = true;
        }
        QueryTargetKind::Parameter
        | QueryTargetKind::ConfigKey
        | QueryTargetKind::SoftwareModule
        | QueryTargetKind::Package => {
            intent.wants_parameters = true;
            intent.wants_paths |=
                matches!(target, QueryTargetKind::SoftwareModule | QueryTargetKind::Package);
        }
        QueryTargetKind::ConfigurationFile | QueryTargetKind::FilesystemPath => {
            intent.wants_paths = true;
            intent.wants_parameters = true;
        }
        QueryTargetKind::HttpMethod => intent.wants_methods = true,
        QueryTargetKind::Port | QueryTargetKind::Protocol | QueryTargetKind::Connection => {
            intent.wants_urls = true;
            intent.wants_parameters = true;
        }
        _ => {}
    }
}

fn apply_literal_constraint_intent(
    intent: &mut TechnicalLiteralIntent,
    kind: LiteralKind,
    text: &str,
) {
    match kind {
        LiteralKind::Url => intent.wants_urls = true,
        LiteralKind::Path => intent.wants_paths = true,
        LiteralKind::Identifier if literal_text_is_identifier_shaped(text) => {
            intent.wants_parameters = true;
        }
        LiteralKind::Identifier
        | LiteralKind::Version
        | LiteralKind::NumericCode
        | LiteralKind::Other => {}
    }
}

fn configure_follow_up_has_evidence_anchor(query_ir: &QueryIR) -> bool {
    matches!(query_ir.scope, crate::domains::query_ir::QueryScope::SingleDocument)
        || query_ir.document_focus.is_some()
        || !query_ir.target_entities.is_empty()
        || !query_ir.literal_constraints.is_empty()
        || !query_ir.conversation_refs.is_empty()
}

pub(super) fn trim_literal_token(token: &str) -> &str {
    token.trim_matches(|ch: char| {
        ch.is_whitespace()
            || matches!(ch, ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | '`')
    })
}
