use crate::{
    domains::graph_quality::{ExtractionOutcomeStatus, ExtractionRecoverySummary},
    infra::repositories::RuntimeGraphExtractionRecoveryAttemptRow,
};

const MIN_SECOND_PASS_WORD_COUNT: usize = 12;
const MAX_RECOVERY_SUMMARY_CHARS: usize = 240;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryDecisionSummary {
    pub reason_code: String,
    pub reason_summary_redacted: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecondPassTrigger {
    pub should_attempt: bool,
    pub decision: Option<RecoveryDecisionSummary>,
}

#[derive(Debug, Clone, Default)]
pub struct ExtractionRecoveryService;

impl ExtractionRecoveryService {
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
            return SecondPassTrigger { should_attempt: false, decision: None };
        }

        let word_count = chunk_text.split_whitespace().count();
        if word_count < MIN_SECOND_PASS_WORD_COUNT {
            return SecondPassTrigger { should_attempt: false, decision: None };
        }

        let total = entity_count.saturating_add(relationship_count);
        if entity_count == 0 && relationship_count > 0 {
            return SecondPassTrigger {
                should_attempt: true,
                decision: Some(self.recovery_decision(
                    "inconsistent_relations_without_entities",
                    "Relationships were extracted without enough entity support.",
                )),
            };
        }
        if total <= 1 {
            return SecondPassTrigger {
                should_attempt: true,
                decision: Some(self.recovery_decision(
                    "sparse_extraction",
                    "The extraction result looked too sparse for the chunk content.",
                )),
            };
        }
        if relationship_count > entity_count.saturating_add(1) {
            return SecondPassTrigger {
                should_attempt: true,
                decision: Some(self.recovery_decision(
                    "inconsistent_relation_density",
                    "The extraction result looked internally inconsistent.",
                )),
            };
        }

        SecondPassTrigger { should_attempt: false, decision: None }
    }

    #[must_use]
    pub fn redact_recovery_summary(&self, summary: &str) -> String {
        let normalized = summary.split_whitespace().collect::<Vec<_>>().join(" ");
        let trimmed = normalized.trim();
        if trimmed.chars().count() <= MAX_RECOVERY_SUMMARY_CHARS {
            return trimmed.to_string();
        }
        let truncated = trimmed.chars().take(MAX_RECOVERY_SUMMARY_CHARS).collect::<String>();
        format!("{truncated}...")
    }

    #[must_use]
    pub fn classify_outcome(
        &self,
        provider_attempt_count: usize,
        second_pass_applied: bool,
        partial: bool,
        failed: bool,
    ) -> ExtractionRecoverySummary {
        let status = if failed {
            ExtractionOutcomeStatus::Failed
        } else if partial {
            ExtractionOutcomeStatus::Partial
        } else if second_pass_applied || provider_attempt_count > 1 {
            ExtractionOutcomeStatus::Recovered
        } else {
            ExtractionOutcomeStatus::Clean
        };

        ExtractionRecoverySummary {
            warning: warning_for_status(&status),
            status,
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
            second_pass_applied,
        })
    }

    fn recovery_decision(
        &self,
        reason_code: &str,
        reason_summary: &str,
    ) -> RecoveryDecisionSummary {
        RecoveryDecisionSummary {
            reason_code: reason_code.to_string(),
            reason_summary_redacted: self.redact_recovery_summary(reason_summary),
        }
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

#[cfg(test)]
mod tests {
    use crate::domains::graph_quality::ExtractionOutcomeStatus;

    use super::*;

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
        assert_eq!(
            decision.decision.as_ref().map(|decision| decision.reason_code.as_str()),
            Some("sparse_extraction")
        );
    }

    #[test]
    fn classifies_partial_and_failed_outcomes() {
        let service = ExtractionRecoveryService;
        let partial = service.classify_outcome(2, true, true, false);
        let failed = service.classify_outcome(2, false, false, true);

        assert_eq!(partial.status, ExtractionOutcomeStatus::Partial);
        assert!(partial.second_pass_applied);
        assert_eq!(failed.status, ExtractionOutcomeStatus::Failed);
        assert!(failed.warning.is_some());
    }
}
