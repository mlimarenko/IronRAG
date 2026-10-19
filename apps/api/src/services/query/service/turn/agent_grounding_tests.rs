use super::test_prelude::*;

use super::agent_grounding::is_system_statement_passthrough;
use crate::services::query::execution::AnswerKind;

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

    assert!(!agent_has_verifiable_tool_evidence(&[], &grounding));
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

    assert!(agent_has_verifiable_tool_evidence(&child_query_execution_ids, &grounding));
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
            disposition: QueryAnswerDisposition::FactualReady,
            clarification: QueryClarification::default(),
        },
        AgentGroundedAnswerOutcome {
            disposition: QueryAnswerDisposition::SafeFallback,
            clarification: QueryClarification::default(),
        },
        AgentGroundedAnswerOutcome {
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
fn parent_verification_downgrades_factual_grounded_answer_passthrough() {
    let selected = finalized_agent_answer_outcome(
        AgentAnswerProvenance::GroundedAnswerPassthrough,
        Some(&AgentGroundedAnswerOutcome {
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
        verification_corpus: vec![
            "[MCP tool result: read_document]\ncontent:\nThe supported claim.".to_string(),
        ],
        document_references: Vec::new(),
    };

    assert!(agent_has_verifiable_tool_evidence(&[], &grounding));
}

#[test]
fn ui_agent_title_only_search_result_is_not_verifier_grade_evidence() {
    let grounding = AssistantGroundingEvidence {
        verification_corpus: vec![
            "[MCP tool result: search_documents]\nstructuredContent:\n{\"hits\":[{\"documentTitle\":\"Alpha Guide\"}]}".to_string(),
        ],
        document_references: Vec::new(),
    };

    assert!(!agent_has_verifiable_tool_evidence(&[], &grounding));
}

#[test]
fn ui_agent_reference_only_search_result_is_not_verifier_grade_evidence() {
    let grounding = AssistantGroundingEvidence {
        verification_corpus: vec![
            "[MCP tool result: search_documents]\nstructuredContent:\n{\"hits\":[{\"documentTitle\":\"Alpha Guide\",\"chunkReferences\":[{\"chunkId\":\"019f0000-0000-7000-8000-000000000001\"}]}]}"
                .to_string(),
        ],
        document_references: Vec::new(),
    };

    assert!(!agent_has_verifiable_tool_evidence(&[], &grounding));
}

#[test]
fn ui_agent_search_result_with_excerpt_is_verifier_grade_evidence() {
    let grounding = AssistantGroundingEvidence {
        verification_corpus: vec![
            "[MCP tool result: search_documents]\nstructuredContent:\n{\"hits\":[{\"documentTitle\":\"Alpha Guide\",\"excerpt\":\"The supported source statement.\"}]}"
                .to_string(),
        ],
        document_references: Vec::new(),
    };

    assert!(agent_has_verifiable_tool_evidence(&[], &grounding));
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
