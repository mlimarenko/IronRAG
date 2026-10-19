use super::*;

fn context_reference_set_grounding_count(
    reference_set: &KnowledgeContextBundleReferenceSetRow,
) -> usize {
    reference_set.chunk_references.len()
        + reference_set.entity_references.len()
        + reference_set.relation_references.len()
        + reference_set.evidence_references.len()
        + reference_set.bundle.selected_fact_ids.len()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MaterializedAgentGrounding {
    pub(super) source_execution_id: Uuid,
    pub(super) primary_execution_id: Uuid,
}

#[derive(Default)]
struct CollectedAgentGrounding {
    primary_reference_set: Option<KnowledgeContextBundleReferenceSetRow>,
    primary_execution_id: Option<Uuid>,
    primary_reference_score: usize,
    grounding_sources: Vec<(Uuid, Uuid)>,
    chunk_references: HashMap<Uuid, KnowledgeBundleChunkReferenceRow>,
    entity_references: HashMap<Uuid, KnowledgeBundleEntityReferenceRow>,
    relation_references: HashMap<Uuid, KnowledgeBundleRelationReferenceRow>,
    evidence_references: HashMap<Uuid, KnowledgeBundleEvidenceReferenceRow>,
    selected_fact_ids: Vec<Uuid>,
}

struct MaterializedGroundingReferences {
    chunk_references: Vec<KnowledgeBundleChunkReferenceRow>,
    entity_references: Vec<KnowledgeBundleEntityReferenceRow>,
    relation_references: Vec<KnowledgeBundleRelationReferenceRow>,
    evidence_references: Vec<KnowledgeBundleEvidenceReferenceRow>,
}

impl CollectedAgentGrounding {
    fn merge(
        &mut self,
        child_execution: &query_repository::QueryExecutionScopeRow,
        reference_set: KnowledgeContextBundleReferenceSetRow,
    ) {
        let reference_score = context_reference_set_grounding_count(&reference_set);
        if self.primary_reference_set.is_none() || reference_score > self.primary_reference_score {
            self.primary_reference_score = reference_score;
            self.primary_execution_id = Some(child_execution.id);
            self.primary_reference_set = Some(reference_set.clone());
        }
        self.grounding_sources.push((child_execution.id, child_execution.runtime_execution_id));
        for fact_id in &reference_set.bundle.selected_fact_ids {
            if !self.selected_fact_ids.contains(fact_id) {
                self.selected_fact_ids.push(*fact_id);
            }
        }
        for reference in reference_set.chunk_references {
            merge_chunk_reference(&mut self.chunk_references, reference);
        }
        for reference in reference_set.entity_references {
            merge_entity_reference(&mut self.entity_references, reference);
        }
        for reference in reference_set.relation_references {
            merge_relation_reference(&mut self.relation_references, reference);
        }
        for reference in reference_set.evidence_references {
            merge_evidence_reference(&mut self.evidence_references, reference);
        }
    }
}

pub(super) async fn materialize_agent_grounding_from_child_execution(
    state: &AppState,
    parent_execution: &query_repository::QueryExecutionRow,
    parent_context_bundle_id: Uuid,
    child_query_execution_ids: &[Uuid],
) -> anyhow::Result<Option<MaterializedAgentGrounding>> {
    let mut collected = match collect_agent_grounding_from_children(
        state,
        parent_execution,
        child_query_execution_ids,
    )
    .await?
    {
        Ok(collected) => collected,
        Err(misses) => {
            tracing::warn!(
                execution_id = %parent_execution.id,
                child_execution_count = child_query_execution_ids.len(),
                misses = %misses.iter().map(|miss| miss.as_str()).collect::<Vec<_>>().join(","),
                "no child grounding could be attached to the UI agent execution; the answer will \
                 reach the client with an empty reference list"
            );
            return Ok(None);
        }
    };
    let primary_execution_id =
        collected.primary_execution_id.context("primary agent grounding execution is missing")?;
    let source_execution_id = collected
        .grounding_sources
        .first()
        .map(|(execution_id, _)| *execution_id)
        .context("agent grounding source execution is missing")?;
    let reference_set = collected
        .primary_reference_set
        .take()
        .context("primary agent grounding reference set is missing")?;
    let now = Utc::now();
    let mut bundle = reference_set.bundle;
    bundle.bundle_id = parent_context_bundle_id;
    bundle.workspace_id = parent_execution.workspace_id;
    bundle.library_id = parent_execution.library_id;
    bundle.query_execution_id = Some(parent_execution.id);
    bundle.selected_fact_ids = std::mem::take(&mut collected.selected_fact_ids);
    bundle.assembly_diagnostics = agent_grounding_assembly_diagnostics(
        &bundle.assembly_diagnostics,
        &collected.grounding_sources,
    );
    bundle.created_at = now;
    bundle.updated_at = now;

    let references = sorted_materialized_grounding_references(collected);
    persist_materialized_agent_grounding(
        state,
        parent_execution,
        parent_context_bundle_id,
        &bundle,
        &references,
        now,
    )
    .await?;
    Ok(Some(MaterializedAgentGrounding { source_execution_id, primary_execution_id }))
}

/// Why one child execution contributed no grounding. Carried out of the scan so
/// a turn that ends up with no evidence at all can say which check dropped it;
/// silently skipping a child is how an answer reaches the client with an empty
/// reference list and nothing in the log to explain it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChildGroundingMiss {
    SelfReference,
    ExecutionRowMissing,
    ScopeMismatch,
    ReferenceSetMissing,
    ReferenceSetWithoutGrounding,
}

impl ChildGroundingMiss {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::SelfReference => "self_reference",
            Self::ExecutionRowMissing => "execution_row_missing",
            Self::ScopeMismatch => "scope_mismatch",
            Self::ReferenceSetMissing => "reference_set_missing",
            Self::ReferenceSetWithoutGrounding => "reference_set_without_grounding",
        }
    }
}

async fn collect_agent_grounding_from_children(
    state: &AppState,
    parent_execution: &query_repository::QueryExecutionRow,
    child_query_execution_ids: &[Uuid],
) -> anyhow::Result<Result<CollectedAgentGrounding, Vec<ChildGroundingMiss>>> {
    let mut collected = CollectedAgentGrounding::default();
    let mut misses = Vec::new();
    for child_execution_id in child_query_execution_ids.iter().rev().copied() {
        match load_child_agent_grounding(state, parent_execution, child_execution_id).await? {
            Ok((child_execution, reference_set)) => {
                collected.merge(&child_execution, reference_set);
            }
            Err(miss) => misses.push(miss),
        }
    }
    if collected.primary_reference_set.is_some() {
        return Ok(Ok(collected));
    }
    Ok(Err(misses))
}

type ChildGrounding =
    (query_repository::QueryExecutionScopeRow, KnowledgeContextBundleReferenceSetRow);

async fn load_child_agent_grounding(
    state: &AppState,
    parent_execution: &query_repository::QueryExecutionRow,
    child_execution_id: Uuid,
) -> anyhow::Result<Result<ChildGrounding, ChildGroundingMiss>> {
    if child_execution_id == parent_execution.id {
        return Ok(Err(ChildGroundingMiss::SelfReference));
    }
    // Deliberately not `get_execution_by_id`: that read inner-joins
    // `runtime_execution`, so a child whose runtime companion row is not yet
    // visible reads as absent. Attaching grounding needs only identity and
    // scope, and must not fail for a reason it does not depend on.
    let Some(child_execution) = query_repository::get_execution_scope_by_id(
        &state.persistence.postgres,
        child_execution_id,
    )
    .await?
    else {
        return Ok(Err(ChildGroundingMiss::ExecutionRowMissing));
    };
    if child_execution.workspace_id != parent_execution.workspace_id
        || child_execution.library_id != parent_execution.library_id
    {
        return Ok(Err(ChildGroundingMiss::ScopeMismatch));
    }
    let Some(reference_set) =
        state.context_store.get_bundle_reference_set_by_query_execution(child_execution_id).await?
    else {
        return Ok(Err(ChildGroundingMiss::ReferenceSetMissing));
    };
    if !context_reference_set_has_grounding(&reference_set) {
        return Ok(Err(ChildGroundingMiss::ReferenceSetWithoutGrounding));
    }
    Ok(Ok((child_execution, reference_set)))
}

fn sorted_materialized_grounding_references(
    collected: CollectedAgentGrounding,
) -> MaterializedGroundingReferences {
    let mut references = MaterializedGroundingReferences {
        chunk_references: collected.chunk_references.into_values().collect(),
        entity_references: collected.entity_references.into_values().collect(),
        relation_references: collected.relation_references.into_values().collect(),
        evidence_references: collected.evidence_references.into_values().collect(),
    };
    sort_chunk_references(&mut references.chunk_references);
    sort_entity_references(&mut references.entity_references);
    sort_relation_references(&mut references.relation_references);
    sort_evidence_references(&mut references.evidence_references);
    references
}

async fn persist_materialized_agent_grounding(
    state: &AppState,
    parent_execution: &query_repository::QueryExecutionRow,
    parent_context_bundle_id: Uuid,
    bundle: &KnowledgeContextBundleRow,
    references: &MaterializedGroundingReferences,
    now: chrono::DateTime<Utc>,
) -> anyhow::Result<()> {
    state.context_store.upsert_bundle(bundle).await?;
    state
        .context_store
        .replace_bundle_chunk_edges(
            parent_context_bundle_id,
            parent_execution.library_id,
            &clone_chunk_reference_edges(
                parent_context_bundle_id,
                &references.chunk_references,
                now,
            ),
        )
        .await?;
    state
        .context_store
        .replace_bundle_entity_edges(
            parent_context_bundle_id,
            parent_execution.library_id,
            &clone_entity_reference_edges(
                parent_context_bundle_id,
                &references.entity_references,
                now,
            ),
        )
        .await?;
    state
        .context_store
        .replace_bundle_relation_edges(
            parent_context_bundle_id,
            parent_execution.library_id,
            &clone_relation_reference_edges(
                parent_context_bundle_id,
                &references.relation_references,
                now,
            ),
        )
        .await?;
    state
        .context_store
        .replace_bundle_evidence_edges(
            parent_context_bundle_id,
            parent_execution.library_id,
            &clone_evidence_reference_edges(
                parent_context_bundle_id,
                &references.evidence_references,
                now,
            ),
        )
        .await?;
    append_materialized_chunk_references(state, parent_execution.id, &references.chunk_references)
        .await
}

async fn append_materialized_chunk_references(
    state: &AppState,
    parent_execution_id: Uuid,
    references: &[KnowledgeBundleChunkReferenceRow],
) -> anyhow::Result<()> {
    let chunk_refs = references
        .iter()
        .map(|reference| query_repository::NewQueryChunkReference {
            chunk_id: reference.chunk_id,
            rank: reference.rank,
            score: reference.score,
        })
        .collect::<Vec<_>>();
    query_repository::append_chunk_references(
        &state.persistence.postgres,
        parent_execution_id,
        &chunk_refs,
    )
    .await?;
    Ok(())
}

/// The persisted answer kind of the first child `grounded_answer` execution
/// that ended as the product's deterministic system statement.
///
/// Read from the persisted bundle rather than a runtime field: the kind is
/// diagnostic metadata the child finalizer wrote, and a missing bundle is not
/// an error — it only means that child carries no typed kind.
pub(super) async fn system_statement_kind_from_child_executions(
    state: &AppState,
    child_query_execution_ids: &[Uuid],
) -> anyhow::Result<Option<AnswerKind>> {
    for child_query_execution_id in child_query_execution_ids.iter().copied() {
        let Some(bundle) =
            state.context_store.get_bundle_by_query_execution(child_query_execution_id).await?
        else {
            continue;
        };
        if persisted_answer_is_system_statement(&bundle.candidate_summary) {
            return Ok(Some(AnswerKind::MissingExplicitDocument));
        }
    }
    Ok(None)
}

/// Persist the parent agent execution as a verbatim passthrough of a child's
/// deterministic system statement.
///
/// The parent body is the child's statement about the library, not a
/// corpus-grounded claim, so parent verification neither applies nor may attach
/// warnings or retrieval sources. The bundle is created neutral first because
/// persistence reads it; `answerKind` is what makes read-side consumers hide
/// the parent's retrieval references.
pub(super) async fn finalize_agent_system_statement(
    state: &AppState,
    execution: &query_repository::QueryExecutionRow,
    context_bundle_id: Uuid,
    assistant_grounding: &AssistantGroundingEvidence,
    kind: AnswerKind,
) -> anyhow::Result<()> {
    ensure_agent_tool_context_bundle(
        state,
        execution,
        context_bundle_id,
        assistant_grounding,
        QueryVerificationState::NotRun,
        serde_json::json!([]),
    )
    .await?;
    let verification = RuntimeAnswerVerification {
        state: QueryVerificationState::NotRun,
        warnings: Vec::new(),
        unsupported_literals: Vec::new(),
    };
    persist_query_verification(QueryVerificationPersistence {
        state,
        execution_id: execution.id,
        verification: &verification,
        answer_disposition: QueryAnswerDisposition::SafeFallback,
        clarification: &QueryClarification::default(),
        authoritative_evidence: &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        assistant_grounding,
        answer_kind: Some(kind),
    })
    .await
}

/// Whether a parent agent answer is the verbatim passthrough of a child's
/// deterministic system statement.
pub(super) fn is_system_statement_passthrough(
    provenance: AgentAnswerProvenance,
    child_answer_kind: Option<AnswerKind>,
) -> bool {
    matches!(provenance, AgentAnswerProvenance::GroundedAnswerPassthrough)
        && child_answer_kind == Some(AnswerKind::MissingExplicitDocument)
}

/// Evidence inputs for verifying an agent answer against the tool evidence
/// gathered during the agent loop.
pub(super) struct AgentAnswerVerificationRequest<'a> {
    pub(super) state: &'a AppState,
    pub(super) execution: &'a query_repository::QueryExecutionRow,
    pub(super) context_bundle_id: Uuid,
    pub(super) answer_text: &'a str,
    pub(super) assistant_grounding: &'a AssistantGroundingEvidence,
    pub(super) answer_provenance: AgentAnswerProvenance,
    pub(super) grounded_answer_outcome: Option<&'a AgentGroundedAnswerOutcome>,
}

pub(super) async fn verify_agent_answer_against_tool_evidence(
    request: AgentAnswerVerificationRequest<'_>,
) -> anyhow::Result<RuntimeAnswerVerification> {
    let AgentAnswerVerificationRequest {
        state,
        execution,
        context_bundle_id,
        answer_text,
        assistant_grounding,
        answer_provenance,
        grounded_answer_outcome,
    } = request;
    ensure_agent_tool_context_bundle(
        state,
        execution,
        context_bundle_id,
        assistant_grounding,
        QueryVerificationState::NotRun,
        serde_json::json!([]),
    )
    .await?;
    let reference_context =
        load_execution_prepared_reference_context(state, execution.id).await.map_err(|error| {
            anyhow::anyhow!("failed to hydrate UI agent verifier evidence: {error}")
        })?;
    let query_ir = reference_context
        .bundle_refs
        .as_ref()
        .and_then(|refs| query_ir_from_bundle_diagnostics(&refs.bundle.assembly_diagnostics));
    let intent_profile = query_intent_profile_from_query_ir(query_ir.as_ref());
    let authoritative_evidence = AuthoritativeAnswerEvidence {
        bundle: reference_context.bundle_refs.as_ref().map(|refs| refs.bundle.clone()),
        chunk_rows: reference_context.chunk_rows,
        structured_blocks: reference_context.structured_block_rows,
        technical_facts: reference_context.technical_fact_rows,
    };
    let prompt_context = assistant_grounding.verification_corpus.join("\n\n");
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: &execution.query_text,
        answer: answer_text,
        intent_profile: &intent_profile,
        evidence: &authoritative_evidence,
        chunks: &[],
        prompt_context: &prompt_context,
        assistant_grounding,
    });
    let finalized = finalize_answer_visibility(
        VerificationLevel::Moderate,
        verification.state,
        &verification.warnings,
        QueryLanguage::Auto,
        answer_text,
        AnswerVisibilityKind::FactualCandidate,
    );
    let (answer_disposition, clarification) = finalized_agent_answer_outcome(
        answer_provenance,
        grounded_answer_outcome,
        finalized.disposition,
    )?;
    persist_query_verification(QueryVerificationPersistence {
        state,
        execution_id: execution.id,
        verification: &verification,
        answer_disposition,
        clarification: &clarification,
        authoritative_evidence: &authoritative_evidence,
        assistant_grounding,
        answer_kind: None,
    })
    .await?;
    Ok(verification)
}

pub(super) fn finalized_agent_answer_outcome(
    answer_provenance: AgentAnswerProvenance,
    grounded_answer_outcome: Option<&AgentGroundedAnswerOutcome>,
    parent_disposition: crate::domains::query::QueryAnswerDisposition,
) -> anyhow::Result<(crate::domains::query::QueryAnswerDisposition, QueryClarification)> {
    match (answer_provenance, grounded_answer_outcome) {
        (AgentAnswerProvenance::Composed, None) => {
            Ok((parent_disposition, QueryClarification::default()))
        }
        (AgentAnswerProvenance::Composed, Some(_)) => {
            anyhow::bail!("composed agent answer unexpectedly carries a child finalizer outcome")
        }
        (AgentAnswerProvenance::GroundedAnswerPassthrough, None) => {
            anyhow::bail!("grounded-answer passthrough is missing its typed outcome")
        }
        (AgentAnswerProvenance::GroundedAnswerPassthrough, Some(outcome)) => {
            anyhow::ensure!(
                outcome.disposition.is_terminal(),
                "grounded-answer passthrough carries a nonterminal outcome"
            );
            anyhow::ensure!(
                matches!(
                    outcome.disposition,
                    crate::domains::query::QueryAnswerDisposition::Clarification
                ) == outcome.clarification.required,
                "grounded-answer disposition and clarification disagree"
            );
            if !outcome.clarification.required {
                anyhow::ensure!(
                    outcome.clarification == QueryClarification::default(),
                    "non-clarification grounded-answer outcome carries clarification metadata"
                );
            }
            let disposition = if matches!(
                outcome.disposition,
                crate::domains::query::QueryAnswerDisposition::FactualReady
            ) {
                parent_disposition
            } else {
                outcome.disposition
            };
            Ok((disposition, outcome.clarification.clone()))
        }
    }
}

pub(super) async fn ensure_agent_tool_context_bundle(
    state: &AppState,
    execution: &query_repository::QueryExecutionRow,
    context_bundle_id: Uuid,
    assistant_grounding: &AssistantGroundingEvidence,
    verification_state: QueryVerificationState,
    verification_warnings: serde_json::Value,
) -> anyhow::Result<()> {
    if state.context_store.get_bundle_by_query_execution(execution.id).await?.is_some() {
        return Ok(());
    }

    let now = Utc::now();
    let bundle = KnowledgeContextBundleRow {
        bundle_id: context_bundle_id,
        workspace_id: execution.workspace_id,
        library_id: execution.library_id,
        query_execution_id: Some(execution.id),
        bundle_state: "ready".to_string(),
        bundle_strategy: "agent_tool_evidence".to_string(),
        requested_mode: super::runtime_mode_label(GROUNDED_ANSWER_QUERY_MODE).to_string(),
        resolved_mode: super::runtime_mode_label(GROUNDED_ANSWER_QUERY_MODE).to_string(),
        selected_fact_ids: Vec::new(),
        verification_state: verification_state_storage_label(verification_state).to_string(),
        verification_warnings,
        freshness_snapshot: serde_json::json!({}),
        candidate_summary: serde_json::json!({
            "finalAssistantDocumentReferences": assistant_grounding.document_references.len(),
            "finalToolEvidenceFragments": assistant_grounding.verification_corpus.len(),
        }),
        assembly_diagnostics: serde_json::json!({
            "queryExecutionId": execution.id,
            "bundleId": context_bundle_id,
            "toolEvidenceFragmentCount": assistant_grounding.verification_corpus.len(),
        }),
        created_at: now,
        updated_at: now,
    };
    state.context_store.upsert_bundle(&bundle).await?;
    Ok(())
}

fn agent_grounding_assembly_diagnostics(
    source: &serde_json::Value,
    child_executions: &[(Uuid, Uuid)],
) -> serde_json::Value {
    let mut diagnostics = source.clone();
    let marker = serde_json::json!(
        child_executions
            .iter()
            .map(|(execution_id, runtime_execution_id)| {
                serde_json::json!({
                    "sourceExecutionId": execution_id,
                    "sourceRuntimeExecutionId": runtime_execution_id
                })
            })
            .collect::<Vec<_>>()
    );
    match diagnostics.as_object_mut() {
        Some(object) => {
            object.insert("uiAgentGroundingSources".to_string(), marker);
            diagnostics
        }
        None => serde_json::json!({
            "source": diagnostics,
            "uiAgentGroundingSources": marker
        }),
    }
}

fn should_replace_reference(current_rank: i32, current_score: f64, rank: i32, score: f64) -> bool {
    rank < current_rank || (rank == current_rank && score > current_score)
}

fn merge_chunk_reference(
    references: &mut HashMap<Uuid, KnowledgeBundleChunkReferenceRow>,
    reference: KnowledgeBundleChunkReferenceRow,
) {
    let replace = references.get(&reference.chunk_id).is_none_or(|current| {
        should_replace_reference(current.rank, current.score, reference.rank, reference.score)
    });
    if replace {
        references.insert(reference.chunk_id, reference);
    }
}

fn merge_entity_reference(
    references: &mut HashMap<Uuid, KnowledgeBundleEntityReferenceRow>,
    reference: KnowledgeBundleEntityReferenceRow,
) {
    let replace = references.get(&reference.entity_id).is_none_or(|current| {
        should_replace_reference(current.rank, current.score, reference.rank, reference.score)
    });
    if replace {
        references.insert(reference.entity_id, reference);
    }
}

fn merge_relation_reference(
    references: &mut HashMap<Uuid, KnowledgeBundleRelationReferenceRow>,
    reference: KnowledgeBundleRelationReferenceRow,
) {
    let replace = references.get(&reference.relation_id).is_none_or(|current| {
        should_replace_reference(current.rank, current.score, reference.rank, reference.score)
    });
    if replace {
        references.insert(reference.relation_id, reference);
    }
}

fn merge_evidence_reference(
    references: &mut HashMap<Uuid, KnowledgeBundleEvidenceReferenceRow>,
    reference: KnowledgeBundleEvidenceReferenceRow,
) {
    let replace = references.get(&reference.evidence_id).is_none_or(|current| {
        should_replace_reference(current.rank, current.score, reference.rank, reference.score)
    });
    if replace {
        references.insert(reference.evidence_id, reference);
    }
}

fn sort_chunk_references(references: &mut [KnowledgeBundleChunkReferenceRow]) {
    references.sort_by(|left, right| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
}

fn sort_entity_references(references: &mut [KnowledgeBundleEntityReferenceRow]) {
    references.sort_by(|left, right| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left.entity_id.cmp(&right.entity_id))
    });
}

fn sort_relation_references(references: &mut [KnowledgeBundleRelationReferenceRow]) {
    references.sort_by(|left, right| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left.relation_id.cmp(&right.relation_id))
    });
}

fn sort_evidence_references(references: &mut [KnowledgeBundleEvidenceReferenceRow]) {
    references.sort_by(|left, right| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left.evidence_id.cmp(&right.evidence_id))
    });
}

fn clone_chunk_reference_edges(
    bundle_id: Uuid,
    references: &[KnowledgeBundleChunkReferenceRow],
    created_at: chrono::DateTime<Utc>,
) -> Vec<KnowledgeBundleChunkEdgeRow> {
    references
        .iter()
        .map(|reference| KnowledgeBundleChunkEdgeRow {
            bundle_id,
            chunk_id: reference.chunk_id,
            rank: reference.rank,
            score: reference.score,
            inclusion_reason: reference.inclusion_reason.clone(),
            created_at,
        })
        .collect()
}

fn clone_entity_reference_edges(
    bundle_id: Uuid,
    references: &[KnowledgeBundleEntityReferenceRow],
    created_at: chrono::DateTime<Utc>,
) -> Vec<KnowledgeBundleEntityEdgeRow> {
    references
        .iter()
        .map(|reference| KnowledgeBundleEntityEdgeRow {
            bundle_id,
            entity_id: reference.entity_id,
            rank: reference.rank,
            score: reference.score,
            inclusion_reason: reference.inclusion_reason.clone(),
            created_at,
        })
        .collect()
}

fn clone_relation_reference_edges(
    bundle_id: Uuid,
    references: &[KnowledgeBundleRelationReferenceRow],
    created_at: chrono::DateTime<Utc>,
) -> Vec<KnowledgeBundleRelationEdgeRow> {
    references
        .iter()
        .map(|reference| KnowledgeBundleRelationEdgeRow {
            bundle_id,
            relation_id: reference.relation_id,
            rank: reference.rank,
            score: reference.score,
            inclusion_reason: reference.inclusion_reason.clone(),
            created_at,
        })
        .collect()
}

fn clone_evidence_reference_edges(
    bundle_id: Uuid,
    references: &[KnowledgeBundleEvidenceReferenceRow],
    created_at: chrono::DateTime<Utc>,
) -> Vec<KnowledgeBundleEvidenceEdgeRow> {
    references
        .iter()
        .map(|reference| KnowledgeBundleEvidenceEdgeRow {
            bundle_id,
            evidence_id: reference.evidence_id,
            rank: reference.rank,
            score: reference.score,
            inclusion_reason: reference.inclusion_reason.clone(),
            created_at,
        })
        .collect()
}
