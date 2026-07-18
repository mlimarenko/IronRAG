use std::collections::BTreeSet;

use anyhow::Context as _;
use ironrag_contracts::assistant::AssistantAnswerDisposition;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    domains::query_ir::{QueryAct, QueryIR, QueryTargetKind, SourceSliceFilter},
    services::query::latest_versions::{
        query_requests_latest_versions, requested_latest_version_count,
    },
};

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AnswerCompletionGapReason {
    #[serde(rename = "ordered_inventory_incomplete")]
    OrderedInventory,
    #[serde(rename = "procedure_incomplete")]
    Procedure,
    #[serde(rename = "troubleshooting_incomplete")]
    Troubleshooting,
    #[serde(rename = "answer_structure_incomplete")]
    AnswerStructure,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AnswerCompletionAssessment {
    pub complete: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<AnswerCompletionGapReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed: Option<usize>,
}

impl AnswerCompletionAssessment {
    #[must_use]
    pub(crate) const fn complete() -> Self {
        Self { complete: true, reason: None, expected: None, observed: None }
    }

    #[must_use]
    pub(crate) const fn incomplete(
        reason: AnswerCompletionGapReason,
        expected: usize,
        observed: usize,
    ) -> Self {
        Self {
            complete: false,
            reason: Some(reason),
            expected: Some(expected),
            observed: Some(observed),
        }
    }

    #[must_use]
    pub(crate) const fn is_consistent(&self) -> bool {
        if self.complete {
            self.reason.is_none() && self.expected.is_none() && self.observed.is_none()
        } else {
            self.reason.is_some() && self.expected.is_some() && self.observed.is_some()
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct OrderedInventoryRequirement {
    expected: usize,
    release_inventory: bool,
}

/// Internal requirements compiled from the canonical query IR.
///
/// This type is deliberately not serialized. It keeps answer-shape policy on
/// the compiler side of the boundary and prevents downstream stages from
/// reclassifying a raw user question independently.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct AnswerCompletionContract {
    ordered_inventory: Option<OrderedInventoryRequirement>,
    procedure_required: bool,
    troubleshooting_required: bool,
}

impl AnswerCompletionContract {
    #[must_use]
    pub(crate) fn from_query_act(act: Option<QueryAct>) -> Self {
        Self {
            ordered_inventory: None,
            procedure_required: matches!(act, Some(QueryAct::ConfigureHow)),
            troubleshooting_required: false,
        }
    }

    #[must_use]
    pub(crate) fn from_query_ir(query_ir: &QueryIR) -> Self {
        let latest_inventory = query_requests_latest_versions(query_ir);
        let release_inventory = latest_inventory
            && query_ir
                .source_slice
                .as_ref()
                .is_some_and(|slice| matches!(slice.filter, SourceSliceFilter::ReleaseMarker));
        let ordered_inventory = latest_inventory
            .then(|| query_ir.source_slice.as_ref().and_then(|slice| slice.count))
            .flatten()
            .map(|_| OrderedInventoryRequirement {
                expected: requested_latest_version_count(query_ir),
                release_inventory,
            });
        let procedure_required = matches!(query_ir.act, QueryAct::ConfigureHow);
        let troubleshooting_required =
            procedure_required && query_ir.targets(QueryTargetKind::Troubleshooting);
        Self { ordered_inventory, procedure_required, troubleshooting_required }
    }

    #[must_use]
    pub(crate) fn evaluate(&self, answer: &str) -> AnswerCompletionAssessment {
        if !answer_has_complete_structure(answer) {
            return AnswerCompletionAssessment::incomplete(
                AnswerCompletionGapReason::AnswerStructure,
                1,
                0,
            );
        }

        if let Some(requirement) = self.ordered_inventory {
            let observed = observed_latest_inventory_items(answer, requirement.release_inventory);
            if observed < requirement.expected {
                return AnswerCompletionAssessment::incomplete(
                    AnswerCompletionGapReason::OrderedInventory,
                    requirement.expected,
                    observed,
                );
            }
        }

        let structure = answer_structure_evidence(answer);
        if self.troubleshooting_required {
            let observed = usize::from(
                structure.top_level_step_count >= 2 && structure.anchored_step_count > 0,
            );
            if observed == 0 {
                return AnswerCompletionAssessment::incomplete(
                    AnswerCompletionGapReason::Troubleshooting,
                    1,
                    0,
                );
            }
            return AnswerCompletionAssessment::complete();
        }

        if self.procedure_required {
            let observed = structure.top_level_step_count;
            if observed < 2 {
                return AnswerCompletionAssessment::incomplete(
                    AnswerCompletionGapReason::Procedure,
                    2,
                    observed,
                );
            }
        }

        AnswerCompletionAssessment::complete()
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GroundedAnswerRepairReason {
    AnswerMissing,
    VerificationIncomplete,
    #[serde(rename = "ordered_inventory_incomplete")]
    OrderedInventory,
    #[serde(rename = "procedure_incomplete")]
    Procedure,
    #[serde(rename = "troubleshooting_incomplete")]
    Troubleshooting,
    #[serde(rename = "answer_structure_incomplete")]
    AnswerStructure,
}

impl From<AnswerCompletionGapReason> for GroundedAnswerRepairReason {
    fn from(reason: AnswerCompletionGapReason) -> Self {
        match reason {
            AnswerCompletionGapReason::OrderedInventory => Self::OrderedInventory,
            AnswerCompletionGapReason::Procedure => Self::Procedure,
            AnswerCompletionGapReason::Troubleshooting => Self::Troubleshooting,
            AnswerCompletionGapReason::AnswerStructure => Self::AnswerStructure,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GroundedAnswerReadiness {
    pub lifecycle_state: String,
    pub answer_disposition: AssistantAnswerDisposition,
    pub final_answer_ready: bool,
    pub finalizable: bool,
    pub clarification_required: bool,
    pub completion_required: bool,
    pub failure_code: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GroundedAnswerRepairPolicy {
    pub required: bool,
    pub reason: Option<GroundedAnswerRepairReason>,
    pub max_additional_grounded_answer_calls: u8,
}

/// Serialized completion/readiness contract shared by MCP and the in-process
/// UI agent. Unknown top-level response fields are ignored while every field
/// owned by this envelope remains required and cross-validated.
#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GroundedAnswerCompletionEnvelope {
    pub final_answer_ready: bool,
    pub finalizable: bool,
    pub completion: AnswerCompletionAssessment,
    pub repair_policy: GroundedAnswerRepairPolicy,
    pub readiness: GroundedAnswerReadiness,
}

impl GroundedAnswerCompletionEnvelope {
    #[must_use]
    pub(crate) fn new(
        answer_disposition: AssistantAnswerDisposition,
        answer_text: &str,
        completion: AnswerCompletionAssessment,
        lifecycle_state: &str,
        failure_code: Option<String>,
    ) -> Self {
        let answer_present = !answer_text.trim().is_empty();
        let answer_disposition = if answer_present {
            answer_disposition
        } else {
            AssistantAnswerDisposition::NonTerminal
        };
        let lifecycle_completed = lifecycle_state == "completed";
        let final_answer_ready =
            matches!(answer_disposition, AssistantAnswerDisposition::FactualReady)
                && completion.complete
                && lifecycle_completed;
        let terminal_nonfactual = lifecycle_completed
            && matches!(
                answer_disposition,
                AssistantAnswerDisposition::SafeFallback
                    | AssistantAnswerDisposition::Clarification
            );
        let repair_required = !final_answer_ready && !terminal_nonfactual;
        let repair_reason = if !repair_required {
            None
        } else if !answer_present {
            Some(GroundedAnswerRepairReason::AnswerMissing)
        } else if let Some(reason) = completion.reason {
            Some(reason.into())
        } else {
            Some(GroundedAnswerRepairReason::VerificationIncomplete)
        };
        let readiness = GroundedAnswerReadiness {
            lifecycle_state: lifecycle_state.to_string(),
            answer_disposition,
            final_answer_ready,
            finalizable: final_answer_ready,
            clarification_required: matches!(
                answer_disposition,
                AssistantAnswerDisposition::Clarification
            ),
            completion_required: repair_required && !completion.complete,
            failure_code,
        };
        Self {
            final_answer_ready,
            finalizable: final_answer_ready,
            completion,
            repair_policy: GroundedAnswerRepairPolicy {
                required: repair_required,
                reason: repair_reason,
                max_additional_grounded_answer_calls: u8::from(repair_required),
            },
            readiness,
        }
    }

    pub(crate) fn from_structured_content(value: &Value) -> anyhow::Result<Self> {
        let envelope: Self = serde_json::from_value(value.clone())
            .context("grounded-answer completion envelope is malformed")?;
        anyhow::ensure!(
            envelope.is_consistent(),
            "grounded-answer completion envelope is internally inconsistent"
        );
        Ok(envelope)
    }

    #[must_use]
    pub(crate) fn is_consistent(&self) -> bool {
        let lifecycle_completed = self.readiness.lifecycle_state == "completed";
        let factual_ready =
            matches!(self.readiness.answer_disposition, AssistantAnswerDisposition::FactualReady);
        let terminal_nonfactual = lifecycle_completed
            && matches!(
                self.readiness.answer_disposition,
                AssistantAnswerDisposition::SafeFallback
                    | AssistantAnswerDisposition::Clarification
            );
        let final_answer_ready = factual_ready && lifecycle_completed && self.completion.complete;
        let repair_required = !final_answer_ready && !terminal_nonfactual;
        let completion_required = repair_required && !self.completion.complete;
        if !self.completion.is_consistent()
            || self.final_answer_ready != self.finalizable
            || self.final_answer_ready != final_answer_ready
            || self.readiness.final_answer_ready != self.final_answer_ready
            || self.readiness.finalizable != self.finalizable
            || self.readiness.completion_required != completion_required
            || self.readiness.clarification_required
                != matches!(
                    self.readiness.answer_disposition,
                    AssistantAnswerDisposition::Clarification
                )
            || self.repair_policy.required != repair_required
            || self.repair_policy.max_additional_grounded_answer_calls != u8::from(repair_required)
            || self.readiness.lifecycle_state.trim().is_empty()
        {
            return false;
        }
        if self.final_answer_ready {
            self.completion.complete
                && self.readiness.lifecycle_state == "completed"
                && self.repair_policy.reason.is_none()
                && self.repair_policy.max_additional_grounded_answer_calls == 0
                && !self.readiness.clarification_required
        } else if terminal_nonfactual {
            self.repair_policy.reason.is_none()
                && self.repair_policy.max_additional_grounded_answer_calls == 0
        } else {
            self.repair_policy.reason.is_some()
        }
    }
}

fn answer_has_complete_structure(answer: &str) -> bool {
    let trimmed = answer.trim();
    if trimmed.is_empty() || trimmed.ends_with(':') {
        return false;
    }
    if !trimmed.matches("```").count().is_multiple_of(2)
        || !trimmed.matches('`').count().is_multiple_of(2)
    {
        return false;
    }
    if has_adjacent_duplicate_lines(trimmed) {
        return false;
    }
    has_balanced_delimiters(trimmed)
}

fn has_adjacent_duplicate_lines(answer: &str) -> bool {
    answer
        .lines()
        .map(normalized_substantive_line)
        .filter(|line| !line.is_empty())
        .try_fold(None::<String>, |previous, line| {
            if previous.as_deref() == Some(line.as_str()) { None } else { Some(Some(line)) }
        })
        .is_none()
}

fn normalized_substantive_line(line: &str) -> String {
    let trimmed = line.trim();
    let without_marker = ordered_line(trimmed).map_or(trimmed, |ordered| ordered.body);
    normalized_words(without_marker).join(" ")
}

fn has_balanced_delimiters(answer: &str) -> bool {
    let mut delimiters = Vec::new();
    let mut in_code = false;
    let mut escaped = false;
    for ch in answer.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '`' {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        match ch {
            '(' | '[' | '{' => delimiters.push(ch),
            ')' if delimiters.pop() != Some('(') => return false,
            ']' if delimiters.pop() != Some('[') => return false,
            '}' if delimiters.pop() != Some('{') => return false,
            ')' | ']' | '}' => {}
            _ => {}
        }
    }
    delimiters.is_empty() && !in_code
}

fn observed_latest_inventory_items(answer: &str, release_inventory: bool) -> usize {
    if release_inventory {
        let ordered = answer.lines().filter_map(ordered_line).collect::<Vec<_>>();
        if let Some(minimum_indent) = ordered.iter().map(|line| line.indent).min() {
            let rendered_source_units = ordered
                .iter()
                .filter(|line| {
                    line.indent == minimum_indent && line.body.trim_start().starts_with("source=`")
                })
                .count();
            if rendered_source_units > 0 {
                // The deterministic answer builder already deduplicates these
                // units by canonical document/revision/version provenance.
                // Count the rendered units themselves so independent source
                // records are not collapsed merely because they share a title
                // or a version literal.
                return rendered_source_units;
            }
        }

        let heading_keys = answer
            .lines()
            .filter(|line| line.trim_start().starts_with('#'))
            .filter_map(release_heading_identity_key)
            .collect::<BTreeSet<_>>();
        if !heading_keys.is_empty() {
            return heading_keys.len();
        }

        let Some(minimum_indent) = ordered.iter().map(|line| line.indent).min() else {
            return 0;
        };
        return ordered
            .into_iter()
            .filter(|line| line.indent == minimum_indent)
            .filter_map(|line| version_identity_token(line.body, false))
            .collect::<BTreeSet<_>>()
            .len();
    }

    let ordered = answer.lines().filter_map(ordered_line).collect::<Vec<_>>();
    let Some(minimum_indent) = ordered.iter().map(|line| line.indent).min() else {
        return 0;
    };
    ordered
        .into_iter()
        .filter(|line| line.indent == minimum_indent)
        .map(|line| normalized_words(line.body).join(" "))
        .filter(|key| !key.is_empty())
        .collect::<BTreeSet<_>>()
        .len()
}

fn release_heading_identity_key(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let body = trimmed.strip_prefix('#')?.trim_start_matches('#').trim();
    version_identity_token(body, false)
}

fn version_identity_token(value: &str, allow_hyphenated: bool) -> Option<String> {
    let tokens = value
        .split_whitespace()
        .map(normalized_version_token)
        .filter(|token| is_version_token(token, allow_hyphenated))
        .collect::<Vec<_>>();
    (!tokens.is_empty()).then(|| tokens.join("|"))
}

fn normalized_version_token(token: &str) -> String {
    token
        .trim_matches(|ch: char| !(ch.is_alphanumeric() || matches!(ch, '.' | '-' | '_')))
        .to_lowercase()
}

fn is_version_token(token: &str, allow_hyphenated: bool) -> bool {
    let has_digit = token.chars().any(|ch| ch.is_ascii_digit());
    let has_version_shape = token.contains('.')
        || allow_hyphenated && token.contains('-')
        || token.starts_with('v') && token.chars().nth(1).is_some_and(|ch| ch.is_ascii_digit());
    has_digit && has_version_shape
}

#[derive(Debug, Clone, Copy)]
struct OrderedLine<'a> {
    indent: usize,
    body: &'a str,
}

fn ordered_line(line: &str) -> Option<OrderedLine<'_>> {
    let indent = line.chars().take_while(|ch| ch.is_whitespace()).count();
    let trimmed = line.trim_start();
    if let Some(body) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("• "))
    {
        return body.chars().any(char::is_alphanumeric).then_some(OrderedLine { indent, body });
    }
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let body = trimmed[digits..]
        .strip_prefix('.')
        .or_else(|| trimmed[digits..].strip_prefix(')'))
        .or_else(|| trimmed[digits..].strip_prefix(':'))?
        .trim_start();
    body.chars().any(char::is_alphanumeric).then_some(OrderedLine { indent, body })
}

#[derive(Debug, Default)]
struct AnswerStructureEvidence {
    top_level_step_count: usize,
    anchored_step_count: usize,
}

fn answer_structure_evidence(answer: &str) -> AnswerStructureEvidence {
    let ordered = answer.lines().filter_map(ordered_line).collect::<Vec<_>>();
    let Some(minimum_indent) = ordered.iter().map(|line| line.indent).min() else {
        return AnswerStructureEvidence::default();
    };
    let mut seen = BTreeSet::new();
    let mut anchored_step_count = 0usize;
    for line in ordered.into_iter().filter(|line| line.indent == minimum_indent) {
        let identity = normalized_words(line.body).join(" ");
        if identity.is_empty() || !seen.insert(identity) {
            continue;
        }
        if line_has_formal_anchor(line.body) {
            anchored_step_count = anchored_step_count.saturating_add(1);
        }
    }
    AnswerStructureEvidence { top_level_step_count: seen.len(), anchored_step_count }
}

fn line_has_formal_anchor(line: &str) -> bool {
    let backtick_count = line.chars().filter(|ch| *ch == '`').count();
    if backtick_count >= 2 {
        return true;
    }
    line.split_whitespace().any(|token| {
        let token = token.trim_matches(|ch: char| ch.is_ascii_punctuation() && ch != '/');
        token.contains('/')
            || token.contains('=')
            || token.contains("::")
            || token.contains("->")
            || token.chars().any(|ch| ch.is_ascii_digit())
                && token.chars().any(|ch| matches!(ch, '.' | '-' | '_'))
    })
}

fn normalized_words(value: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            current.push(ch);
        } else if !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::{
        AnswerCompletionAssessment, AnswerCompletionContract, AnswerCompletionGapReason,
        GroundedAnswerCompletionEnvelope,
    };
    use crate::domains::query_ir::{
        QueryAct, QueryIR, QueryLanguage, QueryScope, QueryTargetKind, SourceSliceDirection,
        SourceSliceFilter, SourceSliceSpec,
    };
    use ironrag_contracts::assistant::AssistantAnswerDisposition;

    fn query_ir(act: QueryAct, target_types: &[&str]) -> QueryIR {
        QueryIR {
            act,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: target_types
                .iter()
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

    fn evaluate_answer_completion(query_ir: QueryIR, answer: &str) -> AnswerCompletionAssessment {
        AnswerCompletionContract::from_query_ir(&query_ir).evaluate(answer)
    }

    fn evaluate_release_inventory(count: u16, answer: &str) -> AnswerCompletionAssessment {
        let mut query_ir = query_ir(QueryAct::Enumerate, &["release"]);
        query_ir.source_slice = Some(SourceSliceSpec {
            direction: SourceSliceDirection::Tail,
            count: Some(count),
            filter: SourceSliceFilter::ReleaseMarker,
        });
        evaluate_answer_completion(query_ir, answer)
    }

    fn evaluate_procedure(answer: &str) -> AnswerCompletionAssessment {
        evaluate_answer_completion(query_ir(QueryAct::ConfigureHow, &[]), answer)
    }

    fn evaluate_troubleshooting(answer: &str) -> AnswerCompletionAssessment {
        evaluate_answer_completion(query_ir(QueryAct::ConfigureHow, &["troubleshooting"]), answer)
    }

    fn assert_gap(
        assessment: AnswerCompletionAssessment,
        reason: AnswerCompletionGapReason,
        expected: usize,
        observed: usize,
    ) {
        assert!(!assessment.complete);
        assert_eq!(assessment.reason, Some(reason));
        assert_eq!(assessment.expected, Some(expected));
        assert_eq!(assessment.observed, Some(observed));
    }

    #[test]
    fn latest_release_inventory_does_not_count_nested_change_bullets_as_releases() {
        let assessment = evaluate_release_inventory(
            4,
            "## Release 4.0\n- Improved startup.\n- Added metrics.\n- Fixed retries.\n- Updated examples.",
        );

        assert_gap(assessment, AnswerCompletionGapReason::OrderedInventory, 4, 1);
    }

    #[test]
    fn latest_release_inventory_does_not_count_nested_issue_codes_as_releases() {
        let assessment = evaluate_release_inventory(
            5,
            "## Release 5.0\n- Fixed E-17.\n- Fixed E-18.\n- Fixed E-19.\n- Fixed E-20.",
        );

        assert_gap(assessment, AnswerCompletionGapReason::OrderedInventory, 5, 1);
    }

    #[test]
    fn latest_release_inventory_accepts_unique_release_identities_in_requested_order() {
        let assessment = evaluate_release_inventory(
            4,
            "1. Δ 4.0 — item alpha.\n\
             2. Δ 3.0 — item beta.\n\
             3. Δ 2.0 — item gamma.\n\
             4. Δ 1.0 — item delta.",
        );

        assert_eq!(assessment, AnswerCompletionAssessment::complete());
    }

    #[test]
    fn latest_release_inventory_distinguishes_release_after_shared_product_version() {
        let assessment = evaluate_release_inventory(
            3,
            "1. source=`Sample Product Version 7.8 | 12.408`\n\
             2. source=`Sample Product Version 7.8 | 12.407`\n\
             3. source=`Sample Product Version 7.8 | 12.406`",
        );

        assert_eq!(assessment, AnswerCompletionAssessment::complete());
    }

    #[test]
    fn latest_release_inventory_counts_distinct_rendered_source_units_with_same_version() {
        let assessment = evaluate_release_inventory(
            3,
            "1. source=`Neutral record 9.0.5` - first evidence\n\
             2. source=`Neutral record 9.0.5` - second evidence\n\
             3. source=`Neutral record 9.0.5` - third evidence",
        );

        assert_eq!(assessment, AnswerCompletionAssessment::complete());
    }

    #[test]
    fn procedure_requires_multiple_action_bearing_steps() {
        let assessment =
            evaluate_procedure("The sample connector is a configurable integration component.");

        assert_gap(assessment, AnswerCompletionGapReason::Procedure, 2, 0);
    }

    #[test]
    fn procedure_accepts_two_top_level_steps_without_classifying_words() {
        let assessment = evaluate_procedure("1. Δelta phase.\n2. Ωmega phase.");

        assert_eq!(assessment, AnswerCompletionAssessment::complete());
    }

    #[test]
    fn procedure_does_not_treat_unordered_prose_as_a_typed_action_sequence() {
        let assessment =
            evaluate_procedure("Apply the first phase and then activate the second phase.");

        assert_gap(assessment, AnswerCompletionGapReason::Procedure, 2, 0);
    }

    #[test]
    fn russian_descriptive_how_question_is_not_misclassified_as_a_procedure() {
        let assessment = evaluate_answer_completion(
            query_ir(QueryAct::Describe, &[]),
            "Адаптивная маршрутизация выбирает путь на основе текущего состояния сети.",
        );

        assert_eq!(assessment, AnswerCompletionAssessment::complete());
    }

    #[test]
    fn procedure_rejects_adjacent_duplicate_steps() {
        let assessment = evaluate_procedure("1. Δelta phase.\n2. Δelta phase.");

        assert_gap(assessment, AnswerCompletionGapReason::AnswerStructure, 1, 0);
    }

    #[test]
    fn procedure_rejects_dangling_structural_suffix() {
        let assessment = evaluate_procedure("1. Δelta phase.\n2. Ωmega phase:");

        assert_gap(assessment, AnswerCompletionGapReason::AnswerStructure, 1, 0);
    }

    #[test]
    fn procedure_rejects_unbalanced_markdown_and_delimiters() {
        for answer in [
            "1. Run `alpha`.\n2. Run `beta.",
            "1. Run the first phase.\n2. Configure [the second phase.",
            "1. Run the first phase.\n2. Execute ```sample",
        ] {
            let assessment = evaluate_procedure(answer);
            assert_gap(assessment, AnswerCompletionGapReason::AnswerStructure, 1, 0);
        }
    }

    #[test]
    fn procedure_accepts_balanced_formal_structure() {
        let assessment = evaluate_procedure(
            "1. Configure `[alpha]` with `{mode=one}`.\n2. Run `sample --check`.",
        );

        assert_eq!(assessment, AnswerCompletionAssessment::complete());
    }

    #[test]
    fn troubleshooting_rejects_one_generic_advice_line() {
        let assessment = evaluate_troubleshooting(
            "`E-17` indicates a duplicate state. Check the documentation.",
        );

        assert_gap(assessment, AnswerCompletionGapReason::Troubleshooting, 1, 0);
    }

    #[test]
    fn troubleshooting_rejects_imperative_generic_documentation_advice() {
        let assessment = evaluate_troubleshooting("Check the documentation for `E-17`.");

        assert_gap(assessment, AnswerCompletionGapReason::Troubleshooting, 1, 0);
    }

    #[test]
    fn troubleshooting_fails_safe_for_untyped_action_prose() {
        let assessment = evaluate_troubleshooting(
            "For `E-17`, remove the duplicate item and retry the operation.",
        );

        assert_gap(assessment, AnswerCompletionGapReason::Troubleshooting, 1, 0);
    }

    #[test]
    fn troubleshooting_accepts_a_structured_sequence_with_a_formal_anchor() {
        let assessment = evaluate_troubleshooting("1. Δelta phase for `E-17`.\n2. Ωmega phase.");

        assert_eq!(assessment, AnswerCompletionAssessment::complete());
    }

    #[test]
    fn troubleshooting_rejects_a_structured_sequence_without_a_formal_anchor() {
        let assessment = evaluate_troubleshooting("1. Δelta phase.\n2. Ωmega phase.");

        assert_gap(assessment, AnswerCompletionGapReason::Troubleshooting, 1, 0);
    }

    #[test]
    fn completion_metadata_uses_stable_typed_reason_names() {
        let assessment = evaluate_release_inventory(3, "1. Release 2.0\n2. Release 1.0");
        let value = serde_json::to_value(assessment).expect("serialize completion assessment");

        assert_eq!(value["complete"], serde_json::json!(false));
        assert_eq!(value["reason"], serde_json::json!("ordered_inventory_incomplete"));
        assert_eq!(value["expected"], serde_json::json!(3));
        assert_eq!(value["observed"], serde_json::json!(2));
    }

    #[test]
    fn change_count_is_not_misread_as_latest_release_count() {
        let assessment = evaluate_answer_completion(
            query_ir(QueryAct::Describe, &[]),
            "Release 5.0 contains five documented changes.",
        );

        assert_eq!(assessment, AnswerCompletionAssessment::complete());
    }

    #[test]
    fn completion_envelope_rejects_missing_and_contradictory_readiness() {
        let missing_readiness = serde_json::json!({
            "finalAnswerReady": true,
            "finalizable": true,
            "completion": {"complete": true},
            "repairPolicy": {
                "required": false,
                "reason": null,
                "maxAdditionalGroundedAnswerCalls": 0
            }
        });
        assert!(
            GroundedAnswerCompletionEnvelope::from_structured_content(&missing_readiness).is_err()
        );

        let contradictory = serde_json::json!({
            "finalAnswerReady": true,
            "finalizable": false,
            "completion": {"complete": true},
            "repairPolicy": {
                "required": false,
                "reason": null,
                "maxAdditionalGroundedAnswerCalls": 0
            },
            "readiness": {
                "lifecycleState": "completed",
                "answerDisposition": "factual_ready",
                "finalAnswerReady": true,
                "finalizable": false,
                "clarificationRequired": false,
                "completionRequired": false,
                "failureCode": null
            }
        });
        assert!(GroundedAnswerCompletionEnvelope::from_structured_content(&contradictory).is_err());
    }

    #[test]
    fn completion_envelope_bounds_repair_calls_and_round_trips() {
        let envelope = GroundedAnswerCompletionEnvelope::new(
            AssistantAnswerDisposition::NonTerminal,
            "A descriptive answer.",
            AnswerCompletionAssessment::incomplete(AnswerCompletionGapReason::Procedure, 2, 0),
            "completed",
            None,
        );
        let serialized = serde_json::to_value(&envelope).expect("envelope should serialize");
        assert_eq!(envelope.repair_policy.max_additional_grounded_answer_calls, 1);
        assert!(GroundedAnswerCompletionEnvelope::from_structured_content(&serialized).is_ok());
    }

    #[test]
    fn typed_clarification_is_terminal_without_retrieval_repair() {
        let envelope = GroundedAnswerCompletionEnvelope::new(
            AssistantAnswerDisposition::Clarification,
            "Choose one documented variant.",
            AnswerCompletionAssessment::complete(),
            "completed",
            None,
        );

        assert!(!envelope.final_answer_ready);
        assert!(!envelope.finalizable);
        assert!(envelope.readiness.clarification_required);
        assert!(!envelope.readiness.completion_required);
        assert!(!envelope.repair_policy.required);
        assert_eq!(envelope.repair_policy.reason, None);
        assert_eq!(envelope.repair_policy.max_additional_grounded_answer_calls, 0);
        assert!(envelope.is_consistent());
        let serialized = serde_json::to_value(&envelope).expect("envelope should serialize");
        assert!(GroundedAnswerCompletionEnvelope::from_structured_content(&serialized).is_ok());
    }

    #[test]
    fn safe_fallback_is_terminal_but_never_factual_ready() {
        let envelope = GroundedAnswerCompletionEnvelope::new(
            AssistantAnswerDisposition::SafeFallback,
            "A deterministic safe fallback.",
            AnswerCompletionAssessment::complete(),
            "completed",
            None,
        );

        assert!(!envelope.final_answer_ready);
        assert!(!envelope.finalizable);
        assert!(!envelope.repair_policy.required);
        assert_eq!(envelope.readiness.answer_disposition, AssistantAnswerDisposition::SafeFallback);
    }

    #[test]
    fn factual_disposition_still_requires_complete_answer_shape() {
        let envelope = GroundedAnswerCompletionEnvelope::new(
            AssistantAnswerDisposition::FactualReady,
            "One grounded item.",
            AnswerCompletionAssessment::incomplete(
                AnswerCompletionGapReason::OrderedInventory,
                2,
                1,
            ),
            "completed",
            None,
        );

        assert!(!envelope.final_answer_ready);
        assert!(envelope.repair_policy.required);
        assert_eq!(envelope.readiness.answer_disposition, AssistantAnswerDisposition::FactualReady);
    }
}
