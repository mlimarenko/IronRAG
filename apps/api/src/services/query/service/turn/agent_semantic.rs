use super::*;
use crate::services::query::execution::semantic_verification::{
    SemanticCandidateContext, bind_visible_answer, has_formal_blocker, is_bounded_abstention,
    load_semantic_sources, nonfactual_report, persisted_semantic_verification, prepare,
    receipt_matches, verify_selected_candidate,
};
use ironrag_contracts::assistant::semantic_verification::SemanticVerificationReport;

struct TrustedChildVerification {
    report: SemanticVerificationReport,
    state: QueryVerificationState,
    warnings: Vec<crate::domains::query::QueryVerificationWarning>,
}

/// Hydration can expand support blocks beyond the child's selected manifest.
/// Exact passthrough selects the child's existing source subset, then reloads
/// it through the parent's authorized/current source owner before reuse.
fn select_child_manifest(
    stage: &mut crate::services::query::execution::AnswerVerificationStage,
    child: &TrustedChildVerification,
    question: &str,
    raw_answer: &str,
) {
    use crate::services::query::execution::semantic_verification::answer_hash;
    use ironrag_contracts::assistant::semantic_verification::SemanticSourceLocator;
    if child.report.question_sha256 != answer_hash(question)
        || child.report.checked_answer_sha256 != answer_hash(raw_answer)
    {
        return;
    }
    let chunks = child
        .report
        .sources
        .iter()
        .filter_map(|source| match source.locator {
            SemanticSourceLocator::Chunk { chunk_id } => Some(chunk_id),
            _ => None,
        })
        .collect::<HashSet<_>>();
    let blocks = child
        .report
        .sources
        .iter()
        .filter_map(|source| match source.locator {
            SemanticSourceLocator::Segment { segment_id } => Some(segment_id),
            _ => None,
        })
        .collect::<HashSet<_>>();
    stage.generation.primary_answer_chunks.retain(|row| chunks.contains(&row.chunk_id));
    stage.generation.authoritative_evidence.chunk_rows.retain(|row| chunks.contains(&row.chunk_id));
    stage
        .generation
        .authoritative_evidence
        .structured_blocks
        .retain(|row| blocks.contains(&row.block_id));
    stage.generation.assistant_grounding.document_references.retain(|reference| {
        child.report.sources.iter().any(|source| {
            source.document_id == reference.document_id
                && Some(source.document_revision_id) == reference.revision_id
                && matches!(source.locator, SemanticSourceLocator::DocumentSlice { .. })
                && source.text == reference.excerpt
        })
    });
}

async fn child_scope_is_eligible(
    state: &AppState,
    execution: &query_repository::QueryExecutionRow,
    child_id: Uuid,
) -> anyhow::Result<bool> {
    let Some(child) =
        query_repository::get_execution_scope_by_id(&state.persistence.postgres, child_id).await?
    else {
        return Ok(false);
    };
    if child.workspace_id != execution.workspace_id
        || child.library_id != execution.library_id
        || child.id == execution.id
    {
        return Ok(false);
    }
    // Completion was validated from the actual MCP envelope. The canonical
    // scope-only lookup deliberately avoids the runtime-companion join race.
    Ok(true)
}

/// Look up actual durable child rows; compact tool projections are never report
/// input. IDs come only from the actual MCP result ledger, never model prose.
/// A valid cache hit can name a prior completed execution rather than a new
/// descendant; scope and all checked-input identities still
/// have to match before reuse.
async fn trusted_child_report(
    state: &AppState,
    execution: &query_repository::QueryExecutionRow,
    result: &AgentTurnResult,
) -> anyhow::Result<Option<TrustedChildVerification>> {
    if result.answer_provenance != AgentAnswerProvenance::GroundedAnswerPassthrough {
        return Ok(None);
    }
    let Some(child_id) =
        result.grounded_answer_outcome.as_ref().and_then(|outcome| outcome.source_execution_id)
    else {
        return Ok(None);
    };
    anyhow::ensure!(
        result.child_query_execution_ids.contains(&child_id),
        "passthrough source was not observed in actual tool results"
    );
    if !child_scope_is_eligible(state, execution, child_id).await? {
        return Ok(None);
    }
    let Some(bundle) = state.context_store.get_bundle_by_query_execution(child_id).await? else {
        return Ok(None);
    };
    let (disposition, clarification) = persisted_query_answer_outcome(&bundle.candidate_summary)?;
    if result.grounded_answer_outcome.as_ref().is_none_or(|outcome| {
        outcome.disposition != disposition || outcome.clarification != clarification
    }) {
        return Ok(None);
    }
    let Some(report) = persisted_semantic_verification(&bundle.candidate_summary)? else {
        return Ok(None);
    };
    if report.visible_answer_sha256.as_deref()
        == Some(
            crate::services::query::execution::semantic_verification::answer_hash(&result.answer)
                .as_str(),
        )
    {
        let verification_state =
            serde_json::from_value(serde_json::Value::String(bundle.verification_state.clone()))
                .context("malformed child verifier state")?;
        let warnings = serde_json::from_value(bundle.verification_warnings.clone())
            .context("malformed child verifier warnings")?;
        return Ok(Some(TrustedChildVerification { report, state: verification_state, warnings }));
    }
    Ok(None)
}

async fn selected_parent_grounding(
    state: &AppState,
    execution: &query_repository::QueryExecutionRow,
    result: &AgentTurnResult,
) -> anyhow::Result<AssistantGroundingEvidence> {
    if result.answer_provenance != AgentAnswerProvenance::GroundedAnswerPassthrough {
        return Ok(result.assistant_grounding.clone());
    }
    // Materialization already selected the winning child by its actual ID.
    // Unused earlier tool wrappers must not widen its checked manifest.
    let references = state.context_store.get_bundle_by_query_execution(execution.id).await?;
    let documents = references
        .as_ref()
        .and_then(|bundle| {
            bundle.assembly_diagnostics.pointer("/assistantGrounding/documentReferences")
        })
        .cloned();
    let document_references =
        documents.map(serde_json::from_value).transpose()?.unwrap_or_default();
    Ok(AssistantGroundingEvidence { verification_corpus: Vec::new(), document_references })
}

async fn revalidate_child_receipt(
    state: &AppState,
    execution: &query_repository::QueryExecutionRow,
    stage: &crate::services::query::execution::AnswerVerificationStage,
    child: &TrustedChildVerification,
) -> bool {
    let raw_candidate =
        child.report.answer_units.iter().map(|unit| unit.text.as_str()).collect::<String>();
    let scoped = QueryProviderExecutionContext {
        workspace_id: execution.workspace_id,
        library_id: execution.library_id,
        query_execution_id: execution.id,
        runtime_execution_id: execution.runtime_execution_id,
    };
    let prepared = load_semantic_sources(
        state,
        scoped,
        &stage.generation.primary_answer_chunks,
        &stage.generation.authoritative_evidence,
        &stage.generation.assistant_grounding,
    )
    .await
    .and_then(|sources| prepare(&execution.query_text, &raw_candidate, sources));
    prepared.as_ref().is_ok_and(|prepared| receipt_matches(prepared, &child.report))
}

async fn apply_nonfactual_passthrough(
    state: &AppState,
    execution: &query_repository::QueryExecutionRow,
    stage: &mut crate::services::query::execution::AnswerVerificationStage,
    receipt: Option<&TrustedChildVerification>,
    visible: &str,
) {
    if let Some(child) = receipt
        && revalidate_child_receipt(state, execution, stage, child).await
    {
        stage.verification.state = child.state;
        stage.verification.warnings = child.warnings.clone();
        stage.verification.semantic_verification = Some(child.report.clone());
        return;
    }
    // The selected body is a typed product fallback/clarification, not the
    // child's rejected factual candidate. No semantic support is inherited.
    stage.verification.state = QueryVerificationState::NotRun;
    stage.verification.warnings.clear();
    stage.verification.unsupported_literals.clear();
    stage.verification.semantic_verification =
        Some(nonfactual_report(&execution.query_text, visible));
}

pub(super) async fn finalize_selected_agent_answer(
    state: &AppState,
    execution: &query_repository::QueryExecutionRow,
    result: &mut AgentTurnResult,
) -> anyhow::Result<()> {
    let passthrough = result.answer_provenance == AgentAnswerProvenance::GroundedAnswerPassthrough;
    let child_outcome = result.grounded_answer_outcome.clone();
    let typed_nonfactual = passthrough
        && child_outcome.as_ref().is_some_and(|outcome| {
            matches!(
                outcome.disposition,
                QueryAnswerDisposition::Clarification | QueryAnswerDisposition::SafeFallback
            )
        });
    let selected_grounding = selected_parent_grounding(state, execution, result).await?;
    let mut stage =
        super::agent_grounding::prepare_agent_verification_stage(AgentAnswerVerificationRequest {
            state,
            execution,
            context_bundle_id: execution.context_bundle_id,
            answer_text: &result.answer,
            assistant_grounding: &selected_grounding,
        })
        .await?;
    let receipt = trusted_child_report(state, execution, result).await?;
    let original_visible = result.answer.clone();
    select_passthrough_manifest(
        &mut stage,
        receipt.as_ref(),
        &execution.query_text,
        &original_visible,
        typed_nonfactual,
    );
    if typed_nonfactual {
        // A typed child clarification/fallback is not a new factual candidate.
        // A mismatching receipt is not relabelled as a check of this body.
        apply_nonfactual_passthrough(
            state,
            execution,
            &mut stage,
            receipt.as_ref(),
            &original_visible,
        )
        .await;
    } else {
        verify_selected_candidate(
            SemanticCandidateContext {
                state,
                execution: QueryProviderExecutionContext {
                    workspace_id: execution.workspace_id,
                    library_id: execution.library_id,
                    query_execution_id: execution.id,
                    runtime_execution_id: execution.runtime_execution_id,
                },
                question: &execution.query_text,
                reuse: receipt.as_ref().map(|child| &child.report),
            },
            &mut stage,
            &mut result.provider_calls,
            &mut result.debug_iterations,
        )
        .await?;
    }
    let abstention = !has_formal_blocker(&stage.verification)
        && stage.verification.semantic_verification.as_ref().is_some_and(is_bounded_abstention);
    let finalized = finalize_answer_visibility(
        VerificationLevel::Moderate,
        stage.verification.state,
        &stage.verification.warnings,
        QueryLanguage::Auto,
        &stage.generation.answer,
        if abstention {
            AnswerVisibilityKind::DeterministicStatement
        } else {
            AnswerVisibilityKind::FactualCandidate
        },
    );
    let (disposition, clarification) = if typed_nonfactual {
        super::agent_grounding::finalized_agent_answer_outcome(
            result.answer_provenance,
            child_outcome.as_ref(),
            QueryAnswerDisposition::NonTerminal,
        )?
    } else if abstention {
        (QueryAnswerDisposition::SafeFallback, QueryClarification::default())
    } else {
        (finalized.disposition, QueryClarification::default())
    };
    let visible =
        if typed_nonfactual { original_visible } else { finalized.visible_answer.into_owned() };
    if let Some(report) = &mut stage.verification.semantic_verification {
        bind_visible_answer(report, &visible);
    }
    let answer_kind = if typed_nonfactual {
        let selected_children = super::agent_grounding::selected_agent_child_execution_ids(
            result.answer_provenance,
            child_outcome.as_ref(),
            &result.child_query_execution_ids,
        )?;
        super::agent_grounding::system_statement_kind_from_child_executions(
            state,
            &selected_children,
        )
        .await?
    } else {
        None
    };
    persist_query_verification(QueryVerificationPersistence {
        state,
        execution_id: execution.id,
        verification: &stage.verification,
        answer_disposition: disposition,
        clarification: &clarification,
        authoritative_evidence: &stage.generation.authoritative_evidence,
        assistant_grounding: &stage.generation.assistant_grounding,
        answer_kind,
    })
    .await?;
    result.assistant_grounding = stage.generation.assistant_grounding;
    result.answer = visible;
    Ok(())
}

fn select_passthrough_manifest(
    stage: &mut crate::services::query::execution::AnswerVerificationStage,
    receipt: Option<&TrustedChildVerification>,
    question: &str,
    visible: &str,
    typed_nonfactual: bool,
) {
    let Some(child) = receipt else { return };
    let raw = if typed_nonfactual {
        child.report.answer_units.iter().map(|unit| unit.text.as_str()).collect::<String>()
    } else {
        visible.to_owned()
    };
    select_child_manifest(stage, child, question, &raw);
}
