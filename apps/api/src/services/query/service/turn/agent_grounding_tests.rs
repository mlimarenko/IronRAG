use super::test_prelude::*;

use super::agent_grounding::{
    admitted_manifest_chunks, composed_verification_evidence, is_system_statement_passthrough,
};
use crate::services::query::assistant_grounding::{
    AdmittedAgentSource, AgentEvidenceManifest, AssistantGroundingEvidence,
};
use crate::services::query::execution::{
    AnswerKind, AnswerVerificationRequest, AuthoritativeAnswerEvidence, RuntimeAnswerVerification,
    RuntimeChunkScoreKind, RuntimeMatchedChunk, enrich_query_assembly_diagnostics,
    enrich_query_candidate_summary, verify_answer_against_authoritative_evidence,
};
use crate::services::query::planner::QueryIntentProfile;

#[test]
fn admitted_manifest_chunks_preserve_every_source_identity() {
    let manifest = AgentEvidenceManifest {
        sources: vec![
            AdmittedAgentSource {
                chunk_id: Uuid::from_u128(1),
                document_id: Uuid::from_u128(2),
                document_revision_id: Uuid::from_u128(3),
                text: "alpha canonical text".to_string(),
            },
            AdmittedAgentSource {
                chunk_id: Uuid::from_u128(4),
                document_id: Uuid::from_u128(5),
                document_revision_id: Uuid::from_u128(6),
                text: "beta canonical text".to_string(),
            },
        ],
        selection_bounded: false,
    };

    let chunks = admitted_manifest_chunks(&manifest);

    assert_eq!(chunks.len(), manifest.sources.len());
    assert_eq!(
        chunks.iter().map(|chunk| chunk.chunk_id).collect::<Vec<_>>(),
        vec![Uuid::from_u128(1), Uuid::from_u128(4)]
    );
    assert_eq!(chunks[0].source_text, "alpha canonical text");
    assert_eq!(chunks[1].source_text, "beta canonical text");
    assert_eq!(
        (chunks[0].document_id, chunks[0].revision_id),
        (Uuid::from_u128(2), Uuid::from_u128(3))
    );
}

#[test]
fn system_statement_passthrough_requires_a_child_system_statement_kind() {
    assert!(is_system_statement_passthrough(
        AgentAnswerProvenance::GroundedAnswerPassthrough,
        Some(AnswerKind::MissingExplicitDocument),
    ));
    assert!(!is_system_statement_passthrough(
        AgentAnswerProvenance::GroundedAnswerPassthrough,
        None,
    ));
    assert!(!is_system_statement_passthrough(
        AgentAnswerProvenance::Composed,
        Some(AnswerKind::MissingExplicitDocument),
    ));
}

#[test]
fn ui_agent_no_tool_answer_has_no_verifiable_tool_evidence() {
    let grounding = AssistantGroundingEvidence::default();

    assert!(!agent_has_verifiable_tool_evidence(
        &[],
        &AgentEvidenceManifest::default(),
        &grounding
    ));
}

#[test]
fn ui_agent_no_tool_answer_warning_is_insufficient_evidence_signal() {
    let warnings = no_agent_tool_evidence_warnings();

    assert_eq!(warnings[0]["code"], "no_agent_tool_evidence");
}

#[test]
fn graph_entity_reference_backfill_only_runs_without_prepared_bundle_refs() {
    assert!(should_backfill_graph_entity_references(false, true));
    assert!(!should_backfill_graph_entity_references(true, true));
    assert!(!should_backfill_graph_entity_references(false, false));
}

#[test]
fn ui_agent_child_execution_is_verifiable_tool_evidence() {
    let grounding = AssistantGroundingEvidence::default();
    let child_query_execution_ids = [Uuid::nil()];

    assert!(agent_has_verifiable_tool_evidence(
        &child_query_execution_ids,
        &AgentEvidenceManifest::default(),
        &grounding
    ));
}

#[test]
fn ui_agent_tool_evidence_always_requires_parent_verification() {
    assert!(agent_answer_requires_parent_tool_evidence_verification(true));
    assert!(!agent_answer_requires_parent_tool_evidence_verification(false));
}

#[test]
fn grounded_answer_passthrough_skips_only_parent_model_revision() {
    assert!(!agent_answer_allows_parent_model_revision(
        AgentAnswerProvenance::GroundedAnswerPassthrough,
    ));
    assert!(agent_answer_allows_parent_model_revision(AgentAnswerProvenance::Composed,));
    assert!(agent_answer_requires_parent_tool_evidence_verification(true));
}

#[test]
fn grounded_answer_outcome_preserves_nonfactual_child_outcomes() {
    let clarification = QueryClarification {
        required: true,
        question: Some("Which neutral variant?".to_string()),
        answer_candidates: Vec::new(),
    };
    for outcome in [
        AgentGroundedAnswerOutcome {
            source_execution_id: None,
            disposition: QueryAnswerDisposition::FactualReady,
            clarification: QueryClarification::default(),
        },
        AgentGroundedAnswerOutcome {
            source_execution_id: None,
            disposition: QueryAnswerDisposition::SafeFallback,
            clarification: QueryClarification::default(),
        },
        AgentGroundedAnswerOutcome {
            source_execution_id: None,
            disposition: QueryAnswerDisposition::Clarification,
            clarification: clarification.clone(),
        },
    ] {
        let selected = finalized_agent_answer_outcome(
            AgentAnswerProvenance::GroundedAnswerPassthrough,
            Some(&outcome),
            QueryAnswerDisposition::FactualReady,
        )
        .expect("valid child outcome");

        assert_eq!(selected, (outcome.disposition, outcome.clarification));
    }
}

#[test]
fn exact_passthrough_selects_only_the_actual_winning_child() {
    let earlier = Uuid::from_u128(11);
    let selected = Uuid::from_u128(22);
    let mut outcome = AgentGroundedAnswerOutcome {
        source_execution_id: Some(selected),
        disposition: QueryAnswerDisposition::FactualReady,
        clarification: QueryClarification::default(),
    };
    assert_eq!(
        super::agent_grounding::selected_agent_child_execution_ids(
            AgentAnswerProvenance::GroundedAnswerPassthrough,
            Some(&outcome),
            &[earlier, selected]
        )
        .expect("selected observed child"),
        vec![selected]
    );
    assert_eq!(
        super::agent_grounding::selected_agent_child_execution_ids(
            AgentAnswerProvenance::Composed,
            None,
            &[earlier, selected]
        )
        .expect("composed evidence"),
        vec![earlier, selected]
    );
    outcome.source_execution_id = Some(Uuid::from_u128(99));
    assert!(
        super::agent_grounding::selected_agent_child_execution_ids(
            AgentAnswerProvenance::GroundedAnswerPassthrough,
            Some(&outcome),
            &[earlier, selected]
        )
        .is_err()
    );
}

#[test]
fn parent_verification_downgrades_factual_grounded_answer_passthrough() {
    let selected = finalized_agent_answer_outcome(
        AgentAnswerProvenance::GroundedAnswerPassthrough,
        Some(&AgentGroundedAnswerOutcome {
            source_execution_id: None,
            disposition: QueryAnswerDisposition::FactualReady,
            clarification: QueryClarification::default(),
        }),
        QueryAnswerDisposition::NonTerminal,
    )
    .expect("valid child outcome");

    assert_eq!(selected, (QueryAnswerDisposition::NonTerminal, QueryClarification::default()));
}

#[test]
fn composed_answer_is_parent_finalized_and_outcome_mismatches_fail_closed() {
    assert_eq!(
        finalized_agent_answer_outcome(
            AgentAnswerProvenance::Composed,
            None,
            QueryAnswerDisposition::SafeFallback,
        )
        .expect("composed answer"),
        (QueryAnswerDisposition::SafeFallback, QueryClarification::default()),
    );
    assert!(
        finalized_agent_answer_outcome(
            AgentAnswerProvenance::GroundedAnswerPassthrough,
            None,
            QueryAnswerDisposition::FactualReady,
        )
        .is_err()
    );
    assert!(
        finalized_agent_answer_outcome(
            AgentAnswerProvenance::Composed,
            Some(&AgentGroundedAnswerOutcome {
                source_execution_id: None,
                disposition: QueryAnswerDisposition::FactualReady,
                clarification: QueryClarification::default(),
            }),
            QueryAnswerDisposition::FactualReady,
        )
        .is_err()
    );
}

#[test]
fn ui_agent_verification_corpus_is_verifiable_tool_evidence() {
    let grounding = AssistantGroundingEvidence {
        verification_corpus: vec!["The supported claim.".to_string()],
        document_references: Vec::new(),
    };

    assert!(agent_has_verifiable_tool_evidence(&[], &AgentEvidenceManifest::default(), &grounding));
}

#[test]
fn ui_agent_tool_result_envelope_is_not_verifier_grade_evidence() {
    let grounding = AssistantGroundingEvidence {
        verification_corpus: vec![
            "[MCP tool result: read_document]\ncontent:\nThe supported claim.".to_string(),
        ],
        document_references: Vec::new(),
    };

    assert!(!agent_has_verifiable_tool_evidence(
        &[],
        &AgentEvidenceManifest::default(),
        &grounding
    ));
}

#[test]
fn ui_agent_can_verify_from_manifest_without_a_direct_tool_excerpt() {
    let grounding = AssistantGroundingEvidence {
        verification_corpus: Vec::new(),
        document_references: Vec::new(),
    };
    let manifest = AgentEvidenceManifest {
        sources: vec![AdmittedAgentSource {
            chunk_id: Uuid::from_u128(1),
            document_id: Uuid::from_u128(2),
            document_revision_id: Uuid::from_u128(3),
            text: "The supported source statement.".to_string(),
        }],
        selection_bounded: false,
    };

    assert!(agent_has_verifiable_tool_evidence(&[], &manifest, &grounding));
    assert!(!agent_has_verifiable_tool_evidence(
        &[],
        &AgentEvidenceManifest::default(),
        &grounding
    ));
}

#[test]
fn ui_agent_deadline_budget_covers_runtime_turns() {
    let iteration_cap = ui_agent_iteration_cap();

    assert!(iteration_cap >= ASSISTANT_AGENT_LOOP_MIN_ITERATIONS);
    assert!(
        ASSISTANT_AGENT_LOOP_DEADLINE_MS / iteration_cap as u64
            >= ASSISTANT_AGENT_LOOP_MIN_ITERATION_BUDGET_MS
    );
}

fn composed_evidence_with_unadmitted_bundle_reference() -> AuthoritativeAnswerEvidence {
    let now = chrono::Utc::now();
    AuthoritativeAnswerEvidence {
        bundle: None,
        chunk_rows: vec![crate::infra::knowledge_rows::KnowledgeChunkRow {
            chunk_id: Uuid::from_u128(20),
            workspace_id: Uuid::from_u128(1),
            library_id: Uuid::from_u128(2),
            document_id: Uuid::from_u128(3),
            revision_id: Uuid::from_u128(4),
            chunk_index: 0,
            chunk_kind: Some("text".to_string()),
            content_text: "The bundle-only selector is bundleOnly=12345.".to_string(),
            normalized_text: "The bundle-only selector is bundleOnly=12345.".to_string(),
            span_start: None,
            span_end: None,
            token_count: None,
            support_block_ids: Vec::new(),
            section_path: Vec::new(),
            heading_trail: Vec::new(),
            literal_digest: None,
            chunk_state: "ready".to_string(),
            text_generation: Some(1),
            vector_generation: Some(1),
            quality_score: Some(1.0),
            window_text: None,
            occurred_at: None,
            occurred_until: None,
        }],
        structured_blocks: Vec::new(),
        technical_facts: vec![crate::infra::knowledge_rows::KnowledgeTechnicalFactRow {
            fact_id: Uuid::from_u128(30),
            workspace_id: Uuid::from_u128(1),
            library_id: Uuid::from_u128(2),
            document_id: Uuid::from_u128(3),
            revision_id: Uuid::from_u128(4),
            fact_kind: "config_assignment".to_string(),
            normalized_value_text: "factOnly=67890".to_string(),
            normalized_value_exact: "factOnly=67890".to_string(),
            normalized_value_json: serde_json::json!("factOnly=67890"),
            display_value: "factOnly=67890".to_string(),
            qualifiers_json: serde_json::json!([]),
            support_block_ids: Vec::new(),
            support_chunk_ids: Vec::new(),
            confidence: Some(1.0),
            extraction_kind: "test".to_string(),
            conflict_group_id: None,
            created_at: now,
            updated_at: now,
        }],
    }
}

#[test]
fn composed_verifier_ignores_unadmitted_bundle_chunk_and_fact() {
    let admitted_chunk = RuntimeMatchedChunk {
        chunk_id: Uuid::from_u128(10),
        document_id: Uuid::from_u128(11),
        revision_id: Uuid::from_u128(12),
        chunk_index: 0,
        chunk_kind: None,
        document_label: "admitted".to_string(),
        excerpt: String::new(),
        score_kind: RuntimeChunkScoreKind::Relevance,
        score: None,
        source_text: "alpha-admitted".to_string(),
    };
    let full_evidence = composed_evidence_with_unadmitted_bundle_reference();
    let composed_evidence = composed_verification_evidence(full_evidence.clone());

    let intent_profile = QueryIntentProfile::default();
    let verify = |evidence: &AuthoritativeAnswerEvidence, answer: &str| {
        verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
            question: "Which selector is used?",
            answer,
            intent_profile: &intent_profile,
            evidence,
            chunks: std::slice::from_ref(&admitted_chunk),
            assistant_grounding: &AssistantGroundingEvidence::default(),
        })
    };

    // Distinct bundle-chunk and technical-fact literals prove each leak
    // independently, while the admitted chunk remains the positive control.
    for (answer, leaked_literal) in [
        ("Use `bundleOnly=12345`.", "bundleOnly=12345"),
        ("Use `factOnly=67890`.", "factOnly=67890"),
    ] {
        let with_bundle = verify(&full_evidence, answer);
        assert!(
            with_bundle
                .unsupported_literals
                .iter()
                .all(|literal| !literal.contains(leaked_literal)),
            "control: {leaked_literal} is supported before clearing"
        );
        let composed = verify(&composed_evidence, answer);
        assert!(
            composed.unsupported_literals.iter().any(|literal| literal.contains(leaked_literal)),
            "composed verification must not validate {leaked_literal} from an unadmitted bundle source"
        );
    }

    let admitted_answer = verify(&composed_evidence, "Use `alpha-admitted`.");
    assert!(
        admitted_answer
            .unsupported_literals
            .iter()
            .all(|literal| !literal.contains("alpha-admitted")),
        "the admitted chunk remains supported"
    );
}

#[test]
fn chunk_reference_count_counts_admitted_composed_chunks() {
    let manifest = AgentEvidenceManifest {
        sources: vec![
            AdmittedAgentSource {
                chunk_id: Uuid::from_u128(101),
                document_id: Uuid::from_u128(102),
                document_revision_id: Uuid::from_u128(103),
                text: "alpha admitted text".to_string(),
            },
            AdmittedAgentSource {
                chunk_id: Uuid::from_u128(104),
                document_id: Uuid::from_u128(105),
                document_revision_id: Uuid::from_u128(106),
                text: "beta admitted text".to_string(),
            },
        ],
        selection_bounded: false,
    };
    let composed_evidence =
        composed_verification_evidence(composed_evidence_with_unadmitted_bundle_reference());
    assert!(
        composed_evidence.chunk_rows.is_empty(),
        "composed evidence must not carry bundle chunk rows"
    );
    let primary_answer_chunks = admitted_manifest_chunks(&manifest);
    let unique_admitted = primary_answer_chunks
        .iter()
        .map(|chunk| chunk.chunk_id)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(unique_admitted.len(), primary_answer_chunks.len());

    let grounding = AssistantGroundingEvidence::default();
    let candidate_summary = enrich_query_candidate_summary(
        serde_json::json!({}),
        &composed_evidence,
        &primary_answer_chunks,
        &grounding,
    );
    let diagnostics = enrich_query_assembly_diagnostics(
        serde_json::json!({}),
        &RuntimeAnswerVerification {
            semantic_verification: None,
            state: QueryVerificationState::NotRun,
            warnings: Vec::new(),
            unsupported_literals: Vec::new(),
        },
        QueryAnswerDisposition::FactualReady,
        &candidate_summary,
        &grounding,
    )
    .expect("typed verification diagnostics should serialize");

    assert_eq!(
        diagnostics["structuredEvidence"]["chunkReferenceCount"],
        serde_json::json!(unique_admitted.len())
    );
    assert_ne!(diagnostics["structuredEvidence"]["chunkReferenceCount"], serde_json::json!(0));
}

#[test]
fn replay_load_error_preserves_omission_warning_from_actual_measurement() {
    let context = super::preparation::prior_replay_context(
        Uuid::from_u128(1),
        Err(crate::interfaces::http::router_support::ApiError::forbidden("replay unavailable")),
    );
    assert!(context.messages.is_empty());
    assert!(context.manifest.sources.is_empty());
    assert!(context.manifest.selection_bounded);

    let measurement =
        crate::services::query::execution::semantic_verification::EvidenceBudget::from_agent_sources(
            &context.manifest.sources,
        )
        .measurement();
    assert_eq!(measurement.source_entries, 0);
    assert_eq!(measurement.evidence_utf8_bytes, 0);
    let warning = context
        .manifest
        .selection_bounded
        .then(|| {
            crate::services::query::execution::semantic_verification::bounded_selection_warning(
                measurement,
            )
        })
        .expect("a replay load error must retain the omission warning");
    assert_eq!(warning.code, "evidence_selection_bounded");

    let normal =
        super::preparation::prior_replay_context(Uuid::from_u128(1), Ok(Default::default()));
    assert!(!normal.manifest.selection_bounded);
    assert!(normal.manifest.sources.is_empty());
    assert!(normal.messages.is_empty());
}
