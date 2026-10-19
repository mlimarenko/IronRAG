use crate::{
    domains::{
        graph_quality::ExtractionRecoverySummary, runtime_ingestion::RuntimeProviderFailureDetail,
    },
    services::graph::extract::parse::{
        graph_extraction_candidate_set_contains_encoding_damage,
        repair_graph_extraction_candidate_set,
    },
    services::graph::extract::types::{
        GraphExtractionCandidateSet, GraphExtractionRecoveryRecord, ParsedGraphExtractionCandidate,
        PendingRecoveryRecord, ResolvedGraphExtraction,
    },
};

use super::recovery_loop::GraphExtractionSessionState;
use super::usage_json::aggregate_provider_usage_json;

/// Provider provenance of one resolved graph extraction: the identity recorded
/// on the row and the provider failure (if any) the terminal record carries.
/// Every builder that produces a [`ResolvedGraphExtraction`] takes this one
/// struct instead of the individual fields.
pub(super) struct GraphExtractionProviderRecord<'a> {
    pub(super) provider_kind: &'a str,
    pub(super) model_name: &'a str,
    pub(super) provider_failure: Option<RuntimeProviderFailureDetail>,
}

pub(super) fn build_resolved_extraction_from_candidate(
    candidate: ParsedGraphExtractionCandidate,
    provider: GraphExtractionProviderRecord<'_>,
    session: &mut GraphExtractionSessionState,
    recovery_summary: ExtractionRecoverySummary,
    recovery_attempts: Vec<GraphExtractionRecoveryRecord>,
) -> ResolvedGraphExtraction {
    let normalized = repair_graph_extraction_candidate_set(candidate.normalized);
    if graph_extraction_candidate_set_contains_encoding_damage(&normalized) {
        tracing::error!(
            prompt_hash = %candidate.raw.prompt_hash,
            "graph extraction candidate retained encoding damage after repair"
        );
    }

    ResolvedGraphExtraction {
        provider_kind: provider.provider_kind.to_string(),
        model_name: provider.model_name.to_string(),
        prompt_hash: candidate.raw.prompt_hash.clone(),
        output_text: candidate.raw.output_text.clone(),
        usage_json: aggregate_provider_usage_json(
            provider.provider_kind,
            provider.model_name,
            &session.usage_samples,
        ),
        usage_calls: std::mem::take(&mut session.usage_calls),
        provider_failure: provider.provider_failure,
        normalized,
        lifecycle: candidate.raw.lifecycle,
        recovery: std::mem::take(&mut session.trace),
        recovery_summary,
        recovery_attempts,
    }
}

pub(crate) fn select_better_partial_candidate(
    existing: Option<ParsedGraphExtractionCandidate>,
    candidate: ParsedGraphExtractionCandidate,
) -> Option<ParsedGraphExtractionCandidate> {
    match existing {
        Some(current)
            if graph_candidate_score(&current.normalized)
                >= graph_candidate_score(&candidate.normalized) =>
        {
            Some(current)
        }
        _ => Some(candidate),
    }
}

const fn graph_candidate_score(candidate_set: &GraphExtractionCandidateSet) -> usize {
    candidate_set.entities.len().saturating_mul(2).saturating_add(candidate_set.relations.len())
}

pub(crate) fn finalize_recovery_attempt_records(
    pending_records: &[PendingRecoveryRecord],
    recovery_summary: &ExtractionRecoverySummary,
) -> Vec<GraphExtractionRecoveryRecord> {
    let status = match recovery_summary.status {
        crate::domains::graph_quality::ExtractionOutcomeStatus::Clean => "skipped",
        crate::domains::graph_quality::ExtractionOutcomeStatus::Recovered => "recovered",
        crate::domains::graph_quality::ExtractionOutcomeStatus::Partial => "partial",
        crate::domains::graph_quality::ExtractionOutcomeStatus::Failed => "failed",
    }
    .to_string();

    pending_records
        .iter()
        .map(|record| GraphExtractionRecoveryRecord {
            recovery_kind: record.recovery_kind.clone(),
            trigger_reason: record.trigger_reason.clone(),
            status: status.clone(),
            raw_issue_summary: record.raw_issue_summary.clone(),
            recovered_summary: record.recovered_summary.clone(),
        })
        .collect()
}
