use crate::{
    domains::graph_quality::{ExtractionOutcomeStatus, ExtractionRecoverySummary},
    infra::repositories::RuntimeGraphExtractionRecoveryAttemptRow,
};

const MIN_SECOND_PASS_WORD_COUNT: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParserRepairClassification {
    pub should_attempt: bool,
    pub trigger_reason: Option<String>,
    pub repair_candidate: Option<String>,
    pub issue_summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecondPassTrigger {
    pub should_attempt: bool,
    pub trigger_reason: Option<String>,
    pub issue_summary: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ExtractionRecoveryService;

impl ExtractionRecoveryService {
    #[must_use]
    pub fn classify_parser_repair(
        &self,
        raw_output: &str,
        enabled: bool,
    ) -> ParserRepairClassification {
        let trimmed = raw_output.trim();
        if !enabled || trimmed.is_empty() {
            return ParserRepairClassification {
                should_attempt: false,
                trigger_reason: None,
                repair_candidate: None,
                issue_summary: None,
            };
        }

        let repair_candidate = repair_graph_extraction_output(trimmed);
        let trigger_reason = repair_candidate.as_ref().map(|candidate| {
            if candidate.contains("\"entities\"") || candidate.contains("\"relations\"") {
                "malformed_json_sections".to_string()
            } else {
                "malformed_json_wrapper".to_string()
            }
        });

        ParserRepairClassification {
            should_attempt: repair_candidate.is_some(),
            trigger_reason,
            repair_candidate,
            issue_summary: Some(
                "The extraction output looked malformed but still salvageable.".to_string(),
            ),
        }
    }

    #[must_use]
    pub fn classify_second_pass(
        &self,
        chunk_text: &str,
        entity_count: usize,
        relationship_count: usize,
        enabled: bool,
        attempt_no: usize,
        max_attempts: usize,
    ) -> SecondPassTrigger {
        if !enabled || attempt_no >= max_attempts {
            return SecondPassTrigger {
                should_attempt: false,
                trigger_reason: None,
                issue_summary: None,
            };
        }

        let word_count = chunk_text.split_whitespace().count();
        if word_count < MIN_SECOND_PASS_WORD_COUNT {
            return SecondPassTrigger {
                should_attempt: false,
                trigger_reason: None,
                issue_summary: None,
            };
        }

        let total = entity_count.saturating_add(relationship_count);
        if entity_count == 0 && relationship_count > 0 {
            return SecondPassTrigger {
                should_attempt: true,
                trigger_reason: Some("inconsistent_relations_without_entities".to_string()),
                issue_summary: Some(
                    "Relationships were extracted without enough entity support.".to_string(),
                ),
            };
        }
        if total <= 1 {
            return SecondPassTrigger {
                should_attempt: true,
                trigger_reason: Some("sparse_extraction".to_string()),
                issue_summary: Some(
                    "The extraction result looked too sparse for the chunk content.".to_string(),
                ),
            };
        }
        if relationship_count > entity_count.saturating_add(1) {
            return SecondPassTrigger {
                should_attempt: true,
                trigger_reason: Some("inconsistent_relation_density".to_string()),
                issue_summary: Some(
                    "The extraction result looked internally inconsistent.".to_string(),
                ),
            };
        }

        SecondPassTrigger { should_attempt: false, trigger_reason: None, issue_summary: None }
    }

    #[must_use]
    pub fn classify_outcome(
        &self,
        provider_attempt_count: usize,
        parser_repair_applied: bool,
        second_pass_applied: bool,
        partial: bool,
        failed: bool,
    ) -> ExtractionRecoverySummary {
        let status = if failed {
            ExtractionOutcomeStatus::Failed
        } else if partial {
            ExtractionOutcomeStatus::Partial
        } else if parser_repair_applied || second_pass_applied || provider_attempt_count > 1 {
            ExtractionOutcomeStatus::Recovered
        } else {
            ExtractionOutcomeStatus::Clean
        };

        ExtractionRecoverySummary {
            warning: warning_for_status(&status),
            status,
            parser_repair_applied,
            second_pass_applied,
        }
    }

    #[must_use]
    pub fn summarize_attempt_rows(
        &self,
        attempts: &[RuntimeGraphExtractionRecoveryAttemptRow],
    ) -> Option<ExtractionRecoverySummary> {
        if attempts.is_empty() {
            return None;
        }

        let parser_repair_applied =
            attempts.iter().any(|attempt| attempt.recovery_kind == "parser_repair");
        let second_pass_applied =
            attempts.iter().any(|attempt| attempt.recovery_kind == "second_pass");
        let status = if attempts.iter().any(|attempt| attempt.status == "failed") {
            ExtractionOutcomeStatus::Failed
        } else if attempts.iter().any(|attempt| attempt.status == "partial") {
            ExtractionOutcomeStatus::Partial
        } else if attempts.iter().any(|attempt| attempt.status == "recovered") {
            ExtractionOutcomeStatus::Recovered
        } else {
            ExtractionOutcomeStatus::Clean
        };

        Some(ExtractionRecoverySummary {
            warning: warning_for_status(&status),
            status,
            parser_repair_applied,
            second_pass_applied,
        })
    }
}

fn warning_for_status(status: &ExtractionOutcomeStatus) -> Option<String> {
    match status {
        ExtractionOutcomeStatus::Clean => None,
        ExtractionOutcomeStatus::Recovered => Some(
            "Some visible support required extraction recovery before it could be admitted."
                .to_string(),
        ),
        ExtractionOutcomeStatus::Partial => Some(
            "Some visible support remains partial because graph extraction could only be recovered in part."
                .to_string(),
        ),
        ExtractionOutcomeStatus::Failed => Some(
            "Some graph support could not be recovered after extraction issues and may still be incomplete."
                .to_string(),
        ),
    }
}

fn repair_graph_extraction_output(output_text: &str) -> Option<String> {
    let normalized = normalize_jsonish_text(output_text);
    let mut candidates = Vec::new();
    if let Some(candidate) = synthesize_root_object_from_sections(&normalized) {
        candidates.push(candidate);
    }
    if normalized != output_text.trim() {
        candidates.push(normalized.clone());
    }

    candidates.into_iter().find(|candidate| !candidate.trim().is_empty())
}

fn normalize_jsonish_text(value: &str) -> String {
    value
        .replace(['\u{2018}', '\u{2019}'], "'")
        .replace(['\u{201C}', '\u{201D}'], "\"")
        .replace('\u{00A0}', " ")
        .replace('\u{200B}', "")
        .trim()
        .to_string()
}

fn synthesize_root_object_from_sections(value: &str) -> Option<String> {
    let entities =
        extract_named_array_fragment(value, "entities").unwrap_or_else(|| "[]".to_string());
    let relations =
        extract_named_array_fragment(value, "relations").unwrap_or_else(|| "[]".to_string());
    if entities == "[]" && relations == "[]" {
        return None;
    }
    Some(format!("{{\"entities\":{entities},\"relations\":{relations}}}"))
}

fn extract_named_array_fragment(value: &str, key: &str) -> Option<String> {
    let lower = value.to_ascii_lowercase();
    let needle = key.to_ascii_lowercase();
    let key_index = lower.find(&needle)?;
    let array_start = value[key_index..].find('[')? + key_index;
    let array_end = find_matching_bracket(value, array_start)?;
    Some(value[array_start..=array_end].trim().to_string())
}

fn find_matching_bracket(value: &str, start_index: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, ch) in value.char_indices().skip_while(|(index, _)| *index < start_index) {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && in_string {
            escaped = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match ch {
            '[' => depth += 1,
            ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use crate::domains::graph_quality::ExtractionOutcomeStatus;

    use super::*;

    #[test]
    fn classifies_parser_repair_for_named_sections() {
        let service = ExtractionRecoveryService;
        let decision =
            service.classify_parser_repair("entities:[{\"label\":\"OpenAI\"}] relations:[]", true);

        assert!(decision.should_attempt);
        assert_eq!(decision.trigger_reason.as_deref(), Some("malformed_json_sections"));
        assert!(decision.repair_candidate.is_some());
    }

    #[test]
    fn classifies_second_pass_for_sparse_output() {
        let service = ExtractionRecoveryService;
        let decision = service.classify_second_pass(
            "OpenAI signed a multiyear infrastructure agreement with Contoso in 2025 and expanded the knowledge graph platform rollout.",
            1,
            0,
            true,
            1,
            2,
        );

        assert!(decision.should_attempt);
        assert_eq!(decision.trigger_reason.as_deref(), Some("sparse_extraction"));
    }

    #[test]
    fn classifies_partial_and_failed_outcomes() {
        let service = ExtractionRecoveryService;
        let partial = service.classify_outcome(2, false, true, true, false);
        let failed = service.classify_outcome(2, false, false, false, true);

        assert_eq!(partial.status, ExtractionOutcomeStatus::Partial);
        assert!(partial.second_pass_applied);
        assert_eq!(failed.status, ExtractionOutcomeStatus::Failed);
        assert!(failed.warning.is_some());
    }
}
