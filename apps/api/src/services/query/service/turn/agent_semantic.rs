use super::*;
use crate::services::query::assistant_grounding::AssistantGroundingDocumentReference;
use crate::services::query::execution::semantic_verification::{
    EVIDENCE_SELECTION_BOUNDED, SemanticCandidateContext, bind_visible_answer,
    bounded_selection_warning, has_formal_blocker, is_bounded_abstention, load_semantic_sources,
    nonfactual_report, persisted_semantic_verification, prepare, receipt_matches,
    verify_selected_candidate,
};
use crate::services::query::execution::{
    AnswerVerificationStage, AuthoritativeAnswerEvidence, FinalizedAnswerVisibility,
    RuntimeChunkScoreKind, RuntimeMatchedChunk, append_evidence_reproduction_warning,
};
use ironrag_contracts::assistant::semantic_verification::SemanticSourceLocator;
use ironrag_contracts::assistant::semantic_verification::SemanticVerificationReport;

struct TrustedChildVerification {
    report: SemanticVerificationReport,
    state: QueryVerificationState,
    warnings: Vec<crate::domains::query::QueryVerificationWarning>,
}

impl TrustedChildVerification {
    fn reused_coverage_warning(
        &self,
        checked: Option<&SemanticVerificationReport>,
    ) -> Option<&crate::domains::query::QueryVerificationWarning> {
        if checked != Some(&self.report) {
            return None;
        }
        self.warnings.iter().find(|warning| warning.code == EVIDENCE_SELECTION_BOUNDED)
    }
}

/// Hydration can expand support blocks beyond the child's selected manifest.
/// Exact passthrough reconstructs the trusted child source set from the typed
/// report, then reloads it through the parent's authorized/current source owner
/// before reuse. It never widens to the full child bundle or parses prose.
fn select_child_manifest(
    stage: &mut crate::services::query::execution::AnswerVerificationStage,
    execution: &query_repository::QueryExecutionRow,
    child: &TrustedChildVerification,
    question: &str,
    raw_answer: &str,
) {
    use crate::services::query::execution::semantic_verification::answer_hash;
    if child.report.question_sha256 != answer_hash(question)
        || child.report.checked_answer_sha256 != answer_hash(raw_answer)
    {
        return;
    }
    let mut chunks = Vec::new();
    let mut blocks = Vec::new();
    let mut document_references = Vec::new();
    for source in &child.report.sources {
        match &source.locator {
            SemanticSourceLocator::Chunk { chunk_id } => chunks.push(RuntimeMatchedChunk {
                chunk_id: *chunk_id,
                document_id: source.document_id,
                revision_id: source.document_revision_id,
                chunk_index: 0,
                chunk_kind: None,
                document_label: String::new(),
                excerpt: String::new(),
                score_kind: RuntimeChunkScoreKind::Relevance,
                score: None,
                source_text: source.text.clone(),
            }),
            SemanticSourceLocator::Segment { segment_id } => {
                blocks.push(crate::infra::knowledge_rows::KnowledgeStructuredBlockRow {
                    block_id: *segment_id,
                    workspace_id: execution.workspace_id,
                    library_id: execution.library_id,
                    document_id: source.document_id,
                    revision_id: source.document_revision_id,
                    ordinal: 0,
                    block_kind: String::new(),
                    text: source.text.clone(),
                    normalized_text: source.text.clone(),
                    heading_trail: Vec::new(),
                    section_path: Vec::new(),
                    page_number: None,
                    span_start: None,
                    span_end: None,
                    parent_block_id: None,
                    table_coordinates_json: None,
                    code_language: None,
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                })
            }
            SemanticSourceLocator::DocumentSlice { start, end } => {
                document_references.push(AssistantGroundingDocumentReference {
                    document_id: source.document_id,
                    revision_id: Some(source.document_revision_id),
                    document_title: String::new(),
                    source_uri: None,
                    source_access: None,
                    slice_start_offset: *start,
                    slice_end_offset: *end,
                    excerpt: source.text.clone(),
                    rank: i32::try_from(document_references.len() + 1).unwrap_or(i32::MAX),
                });
            }
        }
    }
    stage.generation.primary_answer_chunks = chunks;
    stage.generation.authoritative_evidence = AuthoritativeAnswerEvidence {
        bundle: None,
        chunk_rows: Vec::new(),
        structured_blocks: blocks,
        technical_facts: Vec::new(),
    };
    stage.generation.assistant_grounding.document_references = document_references;
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

/// A bounded abstention reports a safe fallback; a verified body keeps the
/// disposition the visibility finalizer chose.
fn fallback_disposition(
    abstention: bool,
    finalized: QueryAnswerDisposition,
) -> (QueryAnswerDisposition, QueryClarification) {
    if abstention {
        (QueryAnswerDisposition::SafeFallback, QueryClarification::default())
    } else {
        (finalized, QueryClarification::default())
    }
}

/// A typed child clarification or safe fallback is not a new factual candidate.
fn is_typed_nonfactual_outcome(outcome: Option<&super::AgentGroundedAnswerOutcome>) -> bool {
    outcome.is_some_and(|outcome| {
        matches!(
            outcome.disposition,
            QueryAnswerDisposition::Clarification | QueryAnswerDisposition::SafeFallback
        )
    })
}

fn finalize_agent_visibility<'a>(
    stage: &'a mut AnswerVerificationStage,
    provenance: AgentAnswerProvenance,
    abstention: bool,
    manifest: &AgentEvidenceManifest,
) -> FinalizedAnswerVisibility<'a> {
    if provenance == AgentAnswerProvenance::Composed && !abstention {
        append_evidence_reproduction_warning(
            &stage.generation.answer,
            manifest.source_texts(&stage.generation.assistant_grounding),
            &mut stage.verification,
        );
    }
    finalize_answer_visibility(
        VerificationLevel::Moderate,
        stage.verification.state,
        &stage.verification.warnings,
        stage.generation.query_ir.language,
        &stage.generation.answer,
        if abstention {
            AnswerVisibilityKind::DeterministicStatement
        } else {
            AnswerVisibilityKind::FactualCandidate
        },
    )
}

pub(super) async fn finalize_selected_agent_answer(
    state: &AppState,
    execution: &query_repository::QueryExecutionRow,
    result: &mut AgentTurnResult,
) -> anyhow::Result<()> {
    let passthrough = result.answer_provenance == AgentAnswerProvenance::GroundedAnswerPassthrough;
    let child_outcome = result.grounded_answer_outcome.clone();
    let typed_nonfactual = passthrough && is_typed_nonfactual_outcome(child_outcome.as_ref());
    let selected_grounding = selected_parent_grounding(state, execution, result).await?;
    let mut stage = super::agent_grounding::prepare_agent_verification_stage_with_manifest(
        AgentAnswerVerificationRequest {
            state,
            execution,
            context_bundle_id: execution.context_bundle_id,
            answer_text: &result.answer,
            assistant_grounding: &selected_grounding,
        },
        &result.evidence_manifest,
        passthrough,
    )
    .await?;
    let receipt = trusted_child_report(state, execution, result).await?;
    let original_visible = result.answer.clone();
    select_passthrough_manifest(
        &mut stage,
        execution,
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
    let coverage_warning =
        result.evidence_selection_bounded.map(bounded_selection_warning).or_else(|| {
            receipt
                .as_ref()?
                .reused_coverage_warning(stage.verification.semantic_verification.as_ref())
                .cloned()
        });
    if let Some(warning) = coverage_warning
        && !stage.verification.warnings.iter().any(|existing| existing.code == warning.code)
    {
        stage.verification.warnings.push(warning);
    }
    let abstention = !has_formal_blocker(&stage.verification)
        && stage.verification.semantic_verification.as_ref().is_some_and(is_bounded_abstention);
    let finalized = finalize_agent_visibility(
        &mut stage,
        result.answer_provenance,
        abstention,
        &result.evidence_manifest,
    );
    let (disposition, clarification) = if typed_nonfactual {
        super::agent_grounding::finalized_agent_answer_outcome(
            result.answer_provenance,
            child_outcome.as_ref(),
            QueryAnswerDisposition::NonTerminal,
        )?
    } else {
        fallback_disposition(abstention, finalized.disposition)
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
        primary_answer_chunks: &stage.generation.primary_answer_chunks,
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
    execution: &query_repository::QueryExecutionRow,
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
    select_child_manifest(stage, execution, child, question, &raw);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::agent_runtime::RuntimeLifecycleState;
    use crate::domains::query_ir::QueryLanguage;
    use crate::services::query::assistant_grounding::AdmittedAgentSource;
    use crate::services::query::execution::semantic_verification::{
        AuthorizedSemanticSource, measure_selected_evidence,
    };
    use crate::services::query::execution::{AnswerGenerationStage, RuntimeAnswerVerification};
    use ironrag_contracts::assistant::semantic_verification::SemanticNotRunReason;

    #[test]
    fn composed_visibility_refuses_unverified_evidence_without_changing_provenance() {
        let feed = (0..12)
            .map(|n| {
                format!("Activity {n}: a participant recorded an ordinary update for the group.")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let manifest = AgentEvidenceManifest {
            sources: vec![AdmittedAgentSource {
                chunk_id: Uuid::from_u128(1),
                document_id: Uuid::from_u128(2),
                document_revision_id: Uuid::from_u128(3),
                text: feed.clone(),
            }],
            selection_bounded: false,
        };
        let mut stage = AnswerVerificationStage {
            generation: AnswerGenerationStage {
                intent_profile: Default::default(),
                primary_answer_chunks: super::super::agent_grounding::admitted_manifest_chunks(
                    &manifest,
                ),
                authoritative_evidence: AuthoritativeAnswerEvidence {
                    bundle: None,
                    chunk_rows: Vec::new(),
                    structured_blocks: Vec::new(),
                    technical_facts: Vec::new(),
                },
                assistant_grounding: AssistantGroundingEvidence::default(),
                answer: feed.clone(),
                usage_json: serde_json::Value::Null,
                prompt_context: String::new(),
                query_ir: crate::services::query::compiler::provider_free_fallback_query_ir(
                    "Summarize the activity",
                ),
            },
            verification: RuntimeAnswerVerification {
                semantic_verification: None,
                state: QueryVerificationState::Failed,
                warnings: Vec::new(),
                unsupported_literals: Vec::new(),
            },
        };
        let synthesis = (0..12)
            .map(|n| format!("The synthesis identifies theme {n} and explains its implications."))
            .collect::<Vec<_>>()
            .join(" ");
        stage.generation.assistant_grounding.verification_corpus.push(format!(
            "[MCP tool result: grounded_answer]\ncontent:\n{synthesis}\nstructuredContent:\n{{\"answerBody\": {}}}",
            serde_json::to_string(&synthesis).unwrap()
        ));
        stage.generation.answer =
            format!("{synthesis}\n\nThis answer is limited to selected sources.");
        let _ = finalize_agent_visibility(
            &mut stage,
            AgentAnswerProvenance::Composed,
            false,
            &manifest,
        );
        assert!(
            !stage.verification.warnings.iter().any(|w| w.code == "answer_reproduced_evidence")
        );
        assert_eq!(stage.verification.state, QueryVerificationState::Failed);
        stage.generation.answer = feed.clone();
        stage.generation.query_ir.language = QueryLanguage::Ru;
        let finalized = finalize_agent_visibility(
            &mut stage,
            AgentAnswerProvenance::Composed,
            false,
            &manifest,
        );
        assert_eq!(finalized.disposition, QueryAnswerDisposition::SafeFallback);
        assert_eq!(
            finalized.visible_answer,
            crate::services::query::i18n::deterministic_query_messages(QueryLanguage::Ru)
                .evidence_reproduction_refusal
        );
        assert_ne!(finalized.visible_answer, feed);
        assert!(finalized.visible_answer.len() < feed.len());
        assert_eq!(stage.generation.primary_answer_chunks[0].chunk_id, Uuid::from_u128(1));
        assert_eq!(stage.generation.primary_answer_chunks[0].source_text, feed);
        assert_eq!(stage.generation.answer, feed);
        assert!(stage.verification.warnings.iter().any(|w| w.code == "answer_reproduced_evidence"));
        assert_eq!(stage.verification.state, QueryVerificationState::Failed);

        stage.verification.warnings.clear();
        stage.verification.state = QueryVerificationState::Verified;
        let passed = finalize_agent_visibility(
            &mut stage,
            AgentAnswerProvenance::GroundedAnswerPassthrough,
            false,
            &manifest,
        );
        assert_eq!(passed.disposition, QueryAnswerDisposition::FactualReady);
        assert_eq!(passed.visible_answer, feed);
        stage.verification.state = QueryVerificationState::NotRun;
        let summary = "The group recorded several updates, including `Activity 5`.";
        stage.generation.answer = summary.to_owned();
        let synthesized = finalize_agent_visibility(
            &mut stage,
            AgentAnswerProvenance::Composed,
            false,
            &manifest,
        );
        assert_eq!(synthesized.disposition, QueryAnswerDisposition::FactualReady);
        assert_eq!(synthesized.visible_answer, summary);
    }

    fn child_report(base: u128, question: &str, answer: &str) -> TrustedChildVerification {
        let locators = [
            SemanticSourceLocator::Chunk { chunk_id: Uuid::from_u128(base + 1) },
            SemanticSourceLocator::Segment { segment_id: Uuid::from_u128(base + 2) },
            SemanticSourceLocator::DocumentSlice { start: 7, end: 20 },
        ];
        let sources = locators
            .into_iter()
            .map(|locator| AuthorizedSemanticSource {
                document_id: Uuid::from_u128(base + 3),
                document_revision_id: Uuid::from_u128(base + 4),
                locator,
                text: "source caveat".to_string(),
                complete_document: None,
            })
            .collect();
        let prepared = prepare(question, answer, sources).expect("bounded child fixture");
        TrustedChildVerification {
            report: prepared.not_run(SemanticNotRunReason::BindingUnavailable, 0),
            state: QueryVerificationState::NotRun,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn coverage_caveat_follows_only_the_reused_child_report() {
        let mut child = child_report(10, "Which condition applies?", "A condition applies.");
        let warning = bounded_selection_warning(
            crate::services::query::execution::semantic_verification::SelectedEvidenceMeasurement {
                source_entries: 2,
                evidence_utf8_bytes: 128,
            },
        );
        child.warnings.push(warning);
        let reused = child.reused_coverage_warning(Some(&child.report)).unwrap();
        assert_eq!(reused.message, child.warnings[0].message);
        let different = child_report(20, "Which condition applies?", "A condition applies.");
        assert!(child.reused_coverage_warning(Some(&different.report)).is_none());
        assert!(child.reused_coverage_warning(None).is_none());
    }

    #[test]
    fn direct_child_manifest_replaces_empty_or_disjoint_parent_sources() {
        let question = "Which caveat applies?";
        let answer = "The source caveat applies.";
        let now = Utc::now();
        let execution = query_repository::QueryExecutionRow {
            id: Uuid::from_u128(1),
            workspace_id: Uuid::from_u128(2),
            library_id: Uuid::from_u128(3),
            conversation_id: Uuid::from_u128(4),
            context_bundle_id: Uuid::from_u128(5),
            request_turn_id: None,
            response_turn_id: None,
            binding_id: None,
            runtime_execution_id: Uuid::from_u128(6),
            runtime_lifecycle_state: RuntimeLifecycleState::Completed,
            runtime_active_stage: None,
            turn_budget: 1,
            turn_count: 1,
            parallel_action_limit: 1,
            query_text: question.to_string(),
            failure_code: None,
            failure_summary_redacted: None,
            started_at: now,
            runtime_accepted_at: now,
            runtime_completed_at: Some(now),
        };
        let child = child_report(100, question, answer);
        let prior = child_report(200, question, answer);
        for has_prior in [false, true] {
            let mut stage = AnswerVerificationStage {
                generation: AnswerGenerationStage {
                    intent_profile: Default::default(),
                    primary_answer_chunks: Vec::new(),
                    authoritative_evidence: AuthoritativeAnswerEvidence {
                        bundle: None,
                        chunk_rows: Vec::new(),
                        structured_blocks: Vec::new(),
                        technical_facts: Vec::new(),
                    },
                    assistant_grounding: AssistantGroundingEvidence::default(),
                    answer: answer.to_string(),
                    usage_json: serde_json::Value::Null,
                    prompt_context: String::new(),
                    query_ir: crate::services::query::compiler::provider_free_fallback_query_ir(
                        question,
                    ),
                },
                verification: RuntimeAnswerVerification {
                    semantic_verification: None,
                    state: QueryVerificationState::NotRun,
                    warnings: Vec::new(),
                    unsupported_literals: Vec::new(),
                },
            };
            if has_prior {
                select_child_manifest(&mut stage, &execution, &prior, question, answer);
            }
            select_child_manifest(&mut stage, &execution, &child, question, answer);

            assert_child_source_selected(&stage, &execution, &child);
        }
    }

    /// The selected manifest carries exactly the child source, under every
    /// identity the evidence carries.
    fn assert_child_source_selected(
        stage: &AnswerVerificationStage,
        execution: &query_repository::QueryExecutionRow,
        child: &TrustedChildVerification,
    ) {
        let generation = &stage.generation;
        assert_child_chunk_identity(generation, execution);
        assert_child_block_and_reference_identity(generation, execution);
        assert_child_evidence_measurement(generation, child);
    }

    /// The selected chunk keeps the child's exact identities and text.
    fn assert_child_chunk_identity(
        generation: &AnswerGenerationStage,
        execution: &query_repository::QueryExecutionRow,
    ) {
        let chunks = &generation.primary_answer_chunks;
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_id, Uuid::from_u128(101));
        assert_eq!(chunks[0].document_id, Uuid::from_u128(103));
        assert_eq!(chunks[0].revision_id, Uuid::from_u128(104));
        assert_eq!(chunks[0].source_text, "source caveat");
        assert_eq!(execution.library_id, Uuid::from_u128(3));
    }

    /// The selected blocks and document references keep the same scope.
    fn assert_child_block_and_reference_identity(
        generation: &AnswerGenerationStage,
        execution: &query_repository::QueryExecutionRow,
    ) {
        let blocks = &generation.authoritative_evidence.structured_blocks;
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].block_id, Uuid::from_u128(102));
        assert_eq!(blocks[0].workspace_id, execution.workspace_id);
        assert_eq!(blocks[0].library_id, execution.library_id);
        assert_eq!(blocks[0].document_id, Uuid::from_u128(103));
        assert_eq!(blocks[0].revision_id, Uuid::from_u128(104));
        assert_eq!(blocks[0].text, "source caveat");
        let references = &generation.assistant_grounding.document_references;
        assert_eq!(references.len(), 1);
        assert_eq!(references[0].document_id, Uuid::from_u128(103));
        assert_eq!(references[0].revision_id, Some(Uuid::from_u128(104)));
        assert_eq!((references[0].slice_start_offset, references[0].slice_end_offset), (7, 20));
        assert_eq!(references[0].excerpt, "source caveat");
    }

    /// The measured selection matches the child report exactly.
    fn assert_child_evidence_measurement(
        generation: &AnswerGenerationStage,
        child: &TrustedChildVerification,
    ) {
        let selected = measure_selected_evidence(
            &generation.primary_answer_chunks,
            &generation.authoritative_evidence,
            &generation.assistant_grounding,
        )
        .expect("the exact child selection fits");
        assert_eq!(selected.measurement.source_entries, child.report.sources.len());
        assert_eq!(
            selected.measurement.evidence_utf8_bytes,
            child.report.sources.iter().map(|source| source.text.len()).sum::<usize>()
        );
    }
}
