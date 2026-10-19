use super::answer_snapshot::AnswerSnapshotContext;
use super::literal_revision::finalize_verified_answer;
use super::output_boundary::{
    build_update_procedure_answer_with_source_context_fallback, selected_runtime_answer_chunks,
    selected_runtime_grounding_evidence,
};
use super::preflight::persist_llm_context_snapshot;
use super::single_shot::generate_answer_query_inner;
use super::verification::verify_generated_answer;
use super::{
    AnswerGenerationStage, AnswerKind, AppState, AssistantGroundingEvidence,
    PreparedAnswerQueryResult, QueryAct, RuntimeAnswerQueryFailure, RuntimeAnswerQueryResult,
    RuntimeMatchedChunk, Uuid,
};

/// The request identity and prepared context that every deterministic branch
/// below verifies, finalizes and snapshots against.
///
/// Each branch decides *which* deterministic answer applies; none of them
/// decides what happens afterwards. Keeping the shared borrows in one place
/// lets [`DeterministicAnswerContext::finish`] own that tail exactly once.
struct DeterministicAnswerContext<'a> {
    state: &'a AppState,
    effective_question: &'a str,
    prepared: &'a PreparedAnswerQueryResult,
    snapshot_context: &'a AnswerSnapshotContext,
}

impl DeterministicAnswerContext<'_> {
    /// Verify a selected deterministic answer, apply the literal-revision
    /// policy, persist the LLM context snapshot and build the runtime result.
    ///
    /// Deterministic answers carry no provider calls by construction: nothing
    /// on this path talks to a model. The snapshot is built from
    /// [`AnswerSnapshotContext`] so retrieval spans survive a later write of
    /// the same row.
    async fn finish(
        &self,
        answer: String,
        primary_answer_chunks: Vec<RuntimeMatchedChunk>,
        usage_json: serde_json::Value,
    ) -> anyhow::Result<RuntimeAnswerQueryResult> {
        let execution_id = self.snapshot_context.execution_id();
        let verification_stage = verify_generated_answer(
            execution_id,
            self.effective_question,
            AnswerGenerationStage {
                intent_profile: self.prepared.structured.intent_profile.clone(),
                primary_answer_chunks,
                authoritative_evidence: super::super::AuthoritativeAnswerEvidence {
                    bundle: None,
                    chunk_rows: Vec::new(),
                    structured_blocks: Vec::new(),
                    technical_facts: Vec::new(),
                },
                assistant_grounding: selected_runtime_grounding_evidence(
                    self.prepared,
                    AssistantGroundingEvidence::default(),
                ),
                answer,
                usage_json,
                prompt_context: self.prepared.answer_context.clone(),
                query_ir: self.prepared.query_ir.clone(),
            },
        )
        .await?;
        let verification_stage =
            finalize_verified_answer(self.state, execution_id, verification_stage).await?;
        let final_answer = verification_stage.generation.answer.clone();
        persist_llm_context_snapshot(self.state, self.snapshot_context.snapshot(&final_answer))
            .await?;
        Ok(RuntimeAnswerQueryResult { answer: final_answer, provider_calls: Vec::new() })
    }
}

pub(crate) async fn generate_answer_query(
    request: super::single_shot::AnswerQueryRequest<'_>,
    prepared: PreparedAnswerQueryResult,
) -> Result<RuntimeAnswerQueryResult, RuntimeAnswerQueryFailure> {
    let mut provider_calls = Vec::new();
    Box::pin(generate_answer_query_inner(request, prepared, &mut provider_calls))
        .await
        .map_err(|error| RuntimeAnswerQueryFailure { error, provider_calls })
}

pub(super) async fn try_deterministic_answer(
    state: &AppState,
    effective_question: &str,
    generation_question: &str,
    prepared: &PreparedAnswerQueryResult,
    snapshot_context: &AnswerSnapshotContext,
    requires_remediation_synthesis: bool,
) -> anyhow::Result<Option<RuntimeAnswerQueryResult>> {
    let context =
        DeterministicAnswerContext { state, effective_question, prepared, snapshot_context };
    if let Some(result) =
        try_summary_deterministic_answer(&context, requires_remediation_synthesis).await?
    {
        return Ok(Some(result));
    }
    try_procedural_deterministic_answer(
        &context,
        generation_question,
        requires_remediation_synthesis,
    )
    .await
}

/// Deterministic answers that replace the whole answer outright: the
/// exact-version change summary and the ordered source slice.
async fn try_summary_deterministic_answer(
    context: &DeterministicAnswerContext<'_>,
    requires_remediation_synthesis: bool,
) -> anyhow::Result<Option<RuntimeAnswerQueryResult>> {
    let prepared = context.prepared;
    let execution_id = context.snapshot_context.execution_id();
    let library_id = context.snapshot_context.library_id();

    if !requires_remediation_synthesis
        && let Some(exact_version_answer) = super::super::build_exact_version_change_summary_answer(
            &prepared.query_ir,
            &prepared.structured.context_chunks,
            &prepared.structured.graph_evidence_context_lines,
        )
    {
        log_exact_version_deterministic(execution_id, library_id);
        return context
            .finish(
                exact_version_answer,
                selected_runtime_answer_chunks(prepared),
                serde_json::json!({
                    "deterministic": true,
                    "answer_kind": AnswerKind::ExactVersionChangeSummary.as_str(),
                }),
            )
            .await
            .map(Some);
    }

    if !requires_remediation_synthesis
        && let Some(source_slice_answer) = super::super::build_ordered_source_slice_answer(
            &prepared.query_ir,
            &prepared.structured.ordered_source_units,
            &prepared.structured.context_chunks,
        )
    {
        log_source_slice_deterministic(execution_id, library_id, source_slice_answer.unit_count);

        let usage_json = serde_json::json!({
            "deterministic": true,
            "answer_kind": AnswerKind::OrderedSourceSlice.as_str(),
            "source_unit_count": source_slice_answer.unit_count,
        });
        return context
            .finish(
                source_slice_answer.answer,
                selected_runtime_answer_chunks(prepared),
                usage_json,
            )
            .await
            .map(Some);
    }

    Ok(None)
}

/// Deterministic answers built from the retrieved procedure/setup/structured
/// evidence: the update-procedure sequence, the setup-configuration anchor and
/// the structural identifier mapping.
async fn try_procedural_deterministic_answer(
    context: &DeterministicAnswerContext<'_>,
    generation_question: &str,
    requires_remediation_synthesis: bool,
) -> anyhow::Result<Option<RuntimeAnswerQueryResult>> {
    let prepared = context.prepared;
    let execution_id = context.snapshot_context.execution_id();
    let library_id = context.snapshot_context.library_id();
    let deterministic_setup_answer =
        build_deterministic_setup_answer(prepared, generation_question);
    let selected_update_answer_chunks = selected_runtime_answer_chunks(prepared);

    if deterministic_setup_answer.is_none()
        && let Some((update_answer, update_answer_chunks)) =
            build_update_procedure_answer_with_source_context_fallback(
                &prepared.query_ir,
                &selected_update_answer_chunks,
            )
    {
        let update_answer =
            super::super::answer::augment_deterministic_grounded_answer_with_evidence(
                update_answer,
                generation_question,
                &prepared.query_ir,
                &update_answer_chunks,
            );
        log_update_procedure_deterministic(execution_id, library_id);
        return context
            .finish(
                update_answer,
                update_answer_chunks,
                serde_json::json!({
                    "deterministic": true,
                    "answer_kind": AnswerKind::UpdateProcedureSequence.as_str(),
                }),
            )
            .await
            .map(Some);
    }

    if let Some(setup_answer) = deterministic_setup_answer {
        log_setup_configuration_deterministic(execution_id, library_id);
        return context
            .finish(
                setup_answer,
                selected_runtime_answer_chunks(prepared),
                serde_json::json!({
                    "deterministic": true,
                    "answer_kind": AnswerKind::SetupConfigurationAnchor.as_str(),
                }),
            )
            .await
            .map(Some);
    }

    if let Some(source_unit_answer) = build_structural_source_unit_answer(
        prepared,
        generation_question,
        requires_remediation_synthesis,
    ) {
        log_structured_deterministic(execution_id, library_id);
        return context
            .finish(
                source_unit_answer,
                selected_runtime_answer_chunks(prepared),
                serde_json::json!({
                    "deterministic": true,
                    "answer_kind": AnswerKind::DeterministicGroundedAnswer.as_str(),
                }),
            )
            .await
            .map(Some);
    }
    Ok(None)
}

/// The deterministic setup-configuration anchor candidate, kept only when it is
/// safe to answer directly from the prepared context.
fn build_deterministic_setup_answer(
    prepared: &PreparedAnswerQueryResult,
    generation_question: &str,
) -> Option<String> {
    if !matches!(prepared.query_ir.act, QueryAct::ConfigureHow) {
        return None;
    }
    super::super::answer::build_setup_configuration_anchor_candidate(
        generation_question,
        &prepared.query_ir,
        &prepared.structured.context_chunks,
    )
    .filter(|answer| {
        answer.should_use_as_direct_answer(&prepared.query_ir, &prepared.structured.context_chunks)
    })
    .map(super::super::answer::SetupConfigurationAnchorCandidate::into_answer)
}

/// The structured identifier-mapping or source-unit inventory answer, gated on
/// the act, an absent source slice and no remediation-synthesis requirement.
fn build_structural_source_unit_answer(
    prepared: &PreparedAnswerQueryResult,
    generation_question: &str,
    requires_remediation_synthesis: bool,
) -> Option<String> {
    if requires_remediation_synthesis
        || prepared.query_ir.source_slice.is_some()
        || !matches!(
            prepared.query_ir.act,
            QueryAct::Compare | QueryAct::Describe | QueryAct::Enumerate | QueryAct::RetrieveValue
        )
    {
        return None;
    }
    super::super::answer::build_structural_identifier_mapping_answer(
        generation_question,
        &prepared.query_ir,
        &prepared.structured.context_chunks,
    )
    .or_else(|| {
        super::super::answer::build_structured_source_unit_inventory_answer(
            generation_question,
            &prepared.query_ir,
            &prepared.structured.context_chunks,
        )
    })
}

fn log_exact_version_deterministic(execution_id: Uuid, library_id: Uuid) {
    tracing::info!(
        stage = "answer.exact_version_deterministic",
        %execution_id,
        %library_id,
        "deterministic exact-version change answer selected"
    );
}

fn log_source_slice_deterministic(execution_id: Uuid, library_id: Uuid, source_unit_count: usize) {
    tracing::info!(
        stage = "answer.source_slice_deterministic",
        %execution_id,
        %library_id,
        source_unit_count,
        "deterministic ordered source-slice answer selected"
    );
}

fn log_update_procedure_deterministic(execution_id: Uuid, library_id: Uuid) {
    tracing::info!(
        stage = "answer.update_procedure_deterministic",
        %execution_id,
        %library_id,
        "deterministic update-procedure answer selected before single-shot generation"
    );
}

fn log_setup_configuration_deterministic(execution_id: Uuid, library_id: Uuid) {
    tracing::info!(
        stage = "answer.setup_configuration_deterministic",
        %execution_id,
        %library_id,
        "deterministic setup-configuration answer selected"
    );
}

fn log_structured_deterministic(execution_id: Uuid, library_id: Uuid) {
    tracing::info!(
        stage = "answer.structured_deterministic",
        %execution_id,
        %library_id,
        "deterministic structured answer selected after context augmentation"
    );
}
