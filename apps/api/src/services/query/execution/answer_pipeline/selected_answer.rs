use super::super::semantic_verification::{
    SemanticCandidateContext, bind_visible_answer, has_formal_blocker, is_bounded_abstention,
    verify_selected_candidate,
};
use super::{
    AnswerVerificationStage, AppState, ChatMessage, QueryAnswerDisposition, QueryClarification,
    QueryVerificationPersistence, QueryVerificationState, QueryVerificationWarning,
    RuntimeAnswerQueryResult,
};
use super::{
    answer_snapshot::AnswerSnapshotContext,
    literal_coverage::literal_revision_can_replace_answer,
    output_boundary::enforce_hard_output_boundary,
    output_boundary::merge_generation_usage,
    preflight::persist_llm_context_snapshot,
    verification::{EXACT_WORDING_OMITTED, verify_generated_answer},
};
use crate::agent_runtime::tasks::query_answer::QueryProviderCall;
use crate::services::query::execution::semantic_verification::{
    EVIDENCE_SELECTION_BOUNDED, SelectedEvidenceMeasurement, bounded_selection_warning,
};
use crate::services::query::execution::types::{
    RetrievalDegradation, RuntimeMatchedRelationship, applicable_preserved_source_spans,
};
use crate::services::query::provider_billing::QueryProviderExecutionContext;

const PRESERVED_SOURCE_SPAN_LIMIT: usize = 24;
const MAX_UNSUPPORTED_CLAIMS_FOR_REVISION: usize = 6;

pub(super) struct SelectedAnswerContext<'a> {
    pub(super) state: &'a AppState,
    pub(super) execution: QueryProviderExecutionContext,
    pub(super) question: &'a str,
    pub(super) conversation_history: &'a [ChatMessage],
    pub(super) snapshot: &'a AnswerSnapshotContext,
    /// Optional retrieval-channel degradations retained with the prepared query.
    /// They are surfaced as nonblocking verification warnings, never as failed
    /// evidence or an empty-success claim.
    pub(super) retrieval_degradations: &'a [RetrievalDegradation],
    /// Present only when the bounded evidence selection dropped authorized
    /// sources. The final warning owner reports the honest partial coverage.
    pub(super) bounded_selection: Option<SelectedEvidenceMeasurement>,
    /// Candidate graph-relation statements considered for verbatim preservation
    /// against the same authorized primary source excerpts.
    pub(super) candidate_relationships: &'a [RuntimeMatchedRelationship],
}

/// The sole factual selected-candidate boundary. Drafts never call a provider,
/// mutate to a visibility fallback, persist an outcome or replace a snapshot.
pub(super) fn finish_selected_answer<'a>(
    context: SelectedAnswerContext<'a>,
    stage: AnswerVerificationStage,
    calls: &'a mut Vec<QueryProviderCall>,
) -> impl std::future::Future<Output = anyhow::Result<RuntimeAnswerQueryResult>> + Send + 'a {
    // Keep one selected-candidate state machine off every enclosing draft-path
    // future, rather than boxing each caller in the escalation chain.
    Box::pin(finish_selected_answer_inner(context, stage, calls))
}

async fn finish_selected_answer_inner(
    context: SelectedAnswerContext<'_>,
    mut stage: AnswerVerificationStage,
    calls: &mut Vec<QueryProviderCall>,
) -> anyhow::Result<RuntimeAnswerQueryResult> {
    let SelectedAnswerContext {
        state,
        execution,
        question,
        conversation_history,
        snapshot,
        retrieval_degradations,
        bounded_selection,
        candidate_relationships,
    } = context;
    let mut iterations = snapshot.take_iterations()?;
    if let Err(error) = verify_selected_candidate(
        SemanticCandidateContext { state, execution, question, reuse: None },
        &mut stage,
        calls,
        &mut iterations,
    )
    .await
    {
        persist_semantic_failure_snapshot(
            state,
            execution.query_execution_id,
            snapshot,
            iterations,
        )
        .await;
        return Err(error);
    }
    if let Err(error) = revise_unsupported_semantic_claims(
        PreservedSpanEnforcementContext {
            state,
            execution,
            question,
            conversation_history,
            candidate_relationships,
        },
        &mut stage,
        calls,
        &mut iterations,
    )
    .await
    {
        persist_semantic_failure_snapshot(
            state,
            execution.query_execution_id,
            snapshot,
            iterations,
        )
        .await;
        return Err(error);
    }
    if let Err(error) = enforce_applicable_preserved_spans(
        PreservedSpanEnforcementContext {
            state,
            execution,
            question,
            conversation_history,
            candidate_relationships,
        },
        &mut stage,
        calls,
        &mut iterations,
    )
    .await
    {
        persist_semantic_failure_snapshot(
            state,
            execution.query_execution_id,
            snapshot,
            iterations,
        )
        .await;
        return Err(error);
    }
    append_retrieval_degradation_warnings(&mut stage.verification.warnings, retrieval_degradations);
    if let Some(measurement) = bounded_selection
        && !stage
            .verification
            .warnings
            .iter()
            .any(|warning| warning.code == EVIDENCE_SELECTION_BOUNDED)
    {
        stage.verification.warnings.push(bounded_selection_warning(measurement));
    }
    let nonfactual = !has_formal_blocker(&stage.verification)
        && stage.verification.semantic_verification.as_ref().is_some_and(is_bounded_abstention);
    if !nonfactual {
        super::super::verification_policy::append_evidence_reproduction_warning(
            &stage.generation.answer,
            [stage.generation.prompt_context.as_str()],
            &mut stage.verification,
        );
    }
    let finalized = super::super::verification_policy::finalize_answer_visibility(
        stage.generation.query_ir.verification_level(),
        stage.verification.state,
        &stage.verification.warnings,
        stage.generation.query_ir.language,
        &stage.generation.answer,
        if nonfactual {
            super::super::verification_policy::AnswerVisibilityKind::DeterministicStatement
        } else {
            super::super::verification_policy::AnswerVisibilityKind::FactualCandidate
        },
    );
    let disposition =
        if nonfactual { QueryAnswerDisposition::SafeFallback } else { finalized.disposition };
    let visible = finalized.visible_answer.into_owned();
    if let Some(report) = &mut stage.verification.semantic_verification {
        bind_visible_answer(report, &visible);
    }
    let total_iterations = iterations.len();
    persist_llm_context_snapshot(
        state,
        snapshot.snapshot_with_iterations(&visible, iterations, total_iterations),
    )
    .await?;
    super::super::persist_query_verification(QueryVerificationPersistence {
        state,
        execution_id: execution.query_execution_id,
        verification: &stage.verification,
        answer_disposition: disposition,
        clarification: &QueryClarification::default(),
        authoritative_evidence: &stage.generation.authoritative_evidence,
        primary_answer_chunks: &stage.generation.primary_answer_chunks,
        assistant_grounding: &stage.generation.assistant_grounding,
        answer_kind: None,
    })
    .await?;
    Ok(RuntimeAnswerQueryResult { answer: visible, provider_calls: std::mem::take(calls) })
}

struct PreservedSpanEnforcementContext<'a> {
    state: &'a AppState,
    execution: QueryProviderExecutionContext,
    question: &'a str,
    conversation_history: &'a [ChatMessage],
    candidate_relationships: &'a [RuntimeMatchedRelationship],
}

fn missing_preserved_spans_for_stage(
    question: &str,
    candidate_relationships: &[RuntimeMatchedRelationship],
    stage: &AnswerVerificationStage,
) -> Vec<String> {
    let required = applicable_preserved_source_spans(
        question,
        candidate_relationships.iter().filter_map(RuntimeMatchedRelationship::evidence_text),
        stage.verification.semantic_verification.as_ref(),
        PRESERVED_SOURCE_SPAN_LIMIT,
    );
    required.into_iter().filter(|span| !stage.generation.answer.contains(span.as_str())).collect()
}

/// Extracts unsupported claim texts when a completed semantic verification
/// report is mostly supported (at least one `Supported` unit) and has a small
/// bounded number of unsupported units suitable for one targeted pruning pass.
fn unsupported_semantic_claim_texts(
    report: &ironrag_contracts::assistant::semantic_verification::SemanticVerificationReport,
) -> Vec<String> {
    use ironrag_contracts::assistant::semantic_verification::{
        SemanticClaimSupport, SemanticVerificationOutcome,
    };
    let SemanticVerificationOutcome::Completed { assessment } = &report.outcome else {
        return Vec::new();
    };
    let has_supported =
        assessment.units.iter().any(|unit| matches!(unit.support, SemanticClaimSupport::Supported));
    if !has_supported {
        return Vec::new();
    }
    let unit_text: std::collections::HashMap<&str, &str> =
        report.answer_units.iter().map(|unit| (unit.unit_id.as_str(), unit.text.trim())).collect();
    let unsupported: Vec<String> = assessment
        .units
        .iter()
        .filter(|unit| {
            !matches!(
                unit.support,
                SemanticClaimSupport::Supported | SemanticClaimSupport::NoFactualClaims
            )
        })
        .filter_map(|unit| unit_text.get(unit.unit_id.as_str()).copied())
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    if unsupported.is_empty() || unsupported.len() > MAX_UNSUPPORTED_CLAIMS_FOR_REVISION {
        return Vec::new();
    }
    unsupported
}

/// When semantic verification marks a mostly-supported answer `PartiallySupported`
/// because a small number of units went beyond the authorized sources, runs one
/// bounded revision pass over those exact authorized sources to prune or ground
/// the unsupported units, then re-verifies fail-closed.
async fn revise_unsupported_semantic_claims(
    context: PreservedSpanEnforcementContext<'_>,
    stage: &mut AnswerVerificationStage,
    calls: &mut Vec<QueryProviderCall>,
    iterations: &mut Vec<crate::services::query::llm_context_debug::LlmIterationDebug>,
) -> anyhow::Result<()> {
    if has_formal_blocker(&stage.verification)
        || stage.verification.state != QueryVerificationState::PartiallySupported
    {
        return Ok(());
    }
    let Some(report) = stage.verification.semantic_verification.as_ref() else {
        return Ok(());
    };
    if is_bounded_abstention(report) {
        return Ok(());
    }
    let unsupported_claims = unsupported_semantic_claim_texts(report);
    if unsupported_claims.is_empty() {
        return Ok(());
    }
    let source_context = report
        .sources
        .iter()
        .map(|source| source.text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if source_context.is_empty() {
        return Ok(());
    }
    match crate::services::query::agent_loop::run_literal_revision_turn(
        context.state,
        context.execution,
        crate::services::query::agent_loop::LiteralRevisionRequest::UnsupportedClaims {
            library_id: context.execution.library_id,
            user_question: context.question,
            conversation_history: context.conversation_history,
            original_answer: &stage.generation.answer,
            unsupported_claims: &unsupported_claims,
            revision_context: &source_context,
        },
    )
    .await
    {
        Ok(revision) => {
            calls.extend(revision.provider_calls);
            iterations.extend(revision.debug_iterations);
            let revised_answer = enforce_hard_output_boundary(
                context.execution.query_execution_id,
                "answer.unsupported_claims_revision",
                &stage.generation.query_ir,
                revision.answer,
            );
            if literal_revision_can_replace_answer(&stage.generation.answer, &revised_answer) {
                let mut generation = stage.generation.clone();
                generation.answer = revised_answer;
                generation.usage_json =
                    merge_generation_usage(generation.usage_json, &revision.usage_json);
                let mut revised_stage = verify_generated_answer(
                    context.execution.query_execution_id,
                    context.question,
                    generation,
                )
                .await?;
                verify_selected_candidate(
                    SemanticCandidateContext {
                        state: context.state,
                        execution: context.execution,
                        question: context.question,
                        reuse: None,
                    },
                    &mut revised_stage,
                    calls,
                    iterations,
                )
                .await?;
                if revised_stage.verification.state == QueryVerificationState::Verified {
                    *stage = revised_stage;
                }
            }
        }
        Err(error) => {
            tracing::warn!(
                execution_id = %context.execution.query_execution_id,
                %error,
                "unsupported-claims revision failed; keeping initial semantic verification result"
            );
        }
    }
    Ok(())
}

/// Checks applicable verbatim source statements against the visible answer
/// before finalizing readiness. When a verified answer paraphrased a required
/// statement, runs one bounded revision pass over the same authorized sources
/// and re-verifies; any remaining omission blocks `FactualReady` fail-closed.
async fn enforce_applicable_preserved_spans(
    context: PreservedSpanEnforcementContext<'_>,
    stage: &mut AnswerVerificationStage,
    calls: &mut Vec<QueryProviderCall>,
    iterations: &mut Vec<crate::services::query::llm_context_debug::LlmIterationDebug>,
) -> anyhow::Result<()> {
    let nonfactual = !has_formal_blocker(&stage.verification)
        && stage.verification.semantic_verification.as_ref().is_some_and(is_bounded_abstention);
    if nonfactual {
        return Ok(());
    }
    let missing =
        missing_preserved_spans_for_stage(context.question, context.candidate_relationships, stage);
    if missing.is_empty() {
        return Ok(());
    }
    if stage.verification.state == QueryVerificationState::Verified
        && let Some(report) = stage.verification.semantic_verification.as_ref()
    {
        let source_context = report
            .sources
            .iter()
            .map(|source| source.text.trim())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        if !source_context.is_empty() {
            match crate::services::query::agent_loop::run_literal_revision_turn(
                context.state,
                context.execution,
                crate::services::query::agent_loop::LiteralRevisionRequest::PreservedStatements {
                    library_id: context.execution.library_id,
                    user_question: context.question,
                    conversation_history: context.conversation_history,
                    original_answer: &stage.generation.answer,
                    required_statements: &missing,
                    revision_context: &source_context,
                },
            )
            .await
            {
                Ok(revision) => {
                    calls.extend(revision.provider_calls);
                    iterations.extend(revision.debug_iterations);
                    let revised_answer = enforce_hard_output_boundary(
                        context.execution.query_execution_id,
                        "answer.preserved_statements_revision",
                        &stage.generation.query_ir,
                        revision.answer,
                    );
                    if literal_revision_can_replace_answer(
                        &stage.generation.answer,
                        &revised_answer,
                    ) && missing.iter().all(|span| revised_answer.contains(span.as_str()))
                    {
                        let mut generation = stage.generation.clone();
                        generation.answer = revised_answer;
                        generation.usage_json =
                            merge_generation_usage(generation.usage_json, &revision.usage_json);
                        let mut revised_stage = verify_generated_answer(
                            context.execution.query_execution_id,
                            context.question,
                            generation,
                        )
                        .await?;
                        verify_selected_candidate(
                            SemanticCandidateContext {
                                state: context.state,
                                execution: context.execution,
                                question: context.question,
                                reuse: None,
                            },
                            &mut revised_stage,
                            calls,
                            iterations,
                        )
                        .await?;
                        if revised_stage.verification.state == QueryVerificationState::Verified {
                            *stage = revised_stage;
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        execution_id = %context.execution.query_execution_id,
                        %error,
                        "preserved-statements revision failed; failing closed on missing spans"
                    );
                }
            }
        }
    }
    let remaining_missing =
        missing_preserved_spans_for_stage(context.question, context.candidate_relationships, stage);
    if !remaining_missing.is_empty()
        && !stage.verification.warnings.iter().any(|warning| warning.code == EXACT_WORDING_OMITTED)
    {
        stage.verification.warnings.push(QueryVerificationWarning {
            code: EXACT_WORDING_OMITTED.to_string(),
            message: format!(
                "Visible answer omitted {} applicable verbatim source statement(s).",
                remaining_missing.len()
            ),
            related_segment_id: None,
            related_fact_id: None,
        });
        if matches!(stage.verification.state, QueryVerificationState::Verified) {
            stage.verification.state = QueryVerificationState::PartiallySupported;
        }
    }
    Ok(())
}

/// Surface optional retrieval-channel degradation as distinct nonblocking
/// warnings. They are metadata about a completed retrieval over the remaining
/// authorized evidence: the supported facts and the answer disposition do not
/// change, and a zero-evidence run is still refused elsewhere.
fn append_retrieval_degradation_warnings(
    warnings: &mut Vec<QueryVerificationWarning>,
    degradations: &[RetrievalDegradation],
) {
    for degradation in degradations {
        let warning = degradation.warning();
        if warnings.iter().any(|existing| existing.code == warning.code) {
            continue;
        }
        warnings.push(warning);
    }
}

async fn persist_semantic_failure_snapshot(
    state: &AppState,
    execution_id: uuid::Uuid,
    snapshot: &AnswerSnapshotContext,
    iterations: Vec<crate::services::query::llm_context_debug::LlmIterationDebug>,
) {
    let count = iterations.len();
    let mut failed_snapshot = snapshot.snapshot_with_iterations("", iterations, count);
    failed_snapshot.final_answer = None;
    if let Err(error) = persist_llm_context_snapshot(state, failed_snapshot).await {
        tracing::warn!(%error, %execution_id, "failed to retain paid semantic failure snapshot");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retrieval_degradation_warnings_are_distinct_and_nonblocking() {
        let degradations = [
            RetrievalDegradation::VectorStore,
            RetrievalDegradation::VectorStore,
            RetrievalDegradation::GraphStore,
        ];
        let mut warnings = Vec::new();
        append_retrieval_degradation_warnings(&mut warnings, &degradations);
        assert_eq!(warnings.len(), 2);
        assert_eq!(warnings[0].code, "retrieval_vector_store_unavailable");
        assert_eq!(warnings[1].code, "retrieval_graph_store_unavailable");
        assert!(warnings.iter().all(|warning| {
            !crate::services::query::execution::verification_policy::verification_warning_blocks_factual_readiness(&warning.code)
        }));
    }

    #[test]
    fn bounded_selection_warning_reports_partial_coverage_without_blocking() {
        let warning = bounded_selection_warning(SelectedEvidenceMeasurement {
            source_entries: 12,
            evidence_utf8_bytes: 22_000,
        });
        assert_eq!(warning.code, "evidence_selection_bounded");
        assert!(warning.message.contains("12"));
        assert!(warning.message.contains("22000"));
        assert!(
            !crate::services::query::execution::verification_policy::verification_warning_blocks_factual_readiness(&warning.code)
        );
    }

    #[test]
    fn missing_preserved_span_warning_blocks_factual_readiness() {
        assert!(
            crate::services::query::execution::verification_policy::verification_warning_blocks_factual_readiness(
                EXACT_WORDING_OMITTED
            )
        );
    }

    #[test]
    fn unsupported_semantic_claim_texts_selects_only_partial_supported_answers() {
        use ironrag_contracts::assistant::semantic_verification::{
            SEMANTIC_VERIFICATION_PROTOCOL_VERSION, SemanticAnswerUnit, SemanticAssessment,
            SemanticByteSpan, SemanticClaimSupport, SemanticCompleteness,
            SemanticResponseClassification, SemanticUnitVerdict, SemanticVerificationOutcome,
            SemanticVerificationReport,
        };

        let build = |units: Vec<(&str, &str, SemanticClaimSupport)>| SemanticVerificationReport {
            protocol_version: SEMANTIC_VERIFICATION_PROTOCOL_VERSION,
            question_sha256: "a".repeat(64),
            checked_answer_sha256: "b".repeat(64),
            visible_answer_sha256: None,
            evidence_manifest_sha256: "c".repeat(64),
            answer_units: units
                .iter()
                .map(|(id, text, _)| SemanticAnswerUnit {
                    unit_id: (*id).to_string(),
                    span: SemanticByteSpan { start: 0, end: text.len() },
                    text: (*text).to_string(),
                })
                .collect(),
            sources: Vec::new(),
            elapsed_ms: 1,
            provider_call_id: None,
            preparation_limit: None,
            outcome: SemanticVerificationOutcome::Completed {
                assessment: SemanticAssessment {
                    claim_support: SemanticClaimSupport::InsufficientEvidence,
                    completeness: SemanticCompleteness::Complete,
                    missing_caveats: Vec::new(),
                    response_classification: SemanticResponseClassification::FactualAnswer,
                    units: units
                        .into_iter()
                        .map(|(id, _, support)| SemanticUnitVerdict {
                            unit_id: id.to_string(),
                            support,
                            asserts_document_absence: false,
                            evidence: Vec::new(),
                        })
                        .collect(),
                },
            },
        };

        let partial = build(vec![
            ("u1", "Supported fact.", SemanticClaimSupport::Supported),
            ("u2", "Unsupported extra claim.", SemanticClaimSupport::InsufficientEvidence),
        ]);
        assert_eq!(
            unsupported_semantic_claim_texts(&partial),
            vec!["Unsupported extra claim.".to_string()]
        );

        let all_unsupported =
            build(vec![("u1", "Nothing supported.", SemanticClaimSupport::InsufficientEvidence)]);
        assert!(unsupported_semantic_claim_texts(&all_unsupported).is_empty());
    }
}
