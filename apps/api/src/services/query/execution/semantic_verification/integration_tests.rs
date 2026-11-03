//! Stubbed judgments prove selection, provenance and result plumbing, not truth.
use super::*;
use crate::{
    domains::{
        query::{QueryVerificationState as State, QueryVerificationWarning},
        query_ir::{QueryLanguage, VerificationLevel},
    },
    services::query::execution::{
        types::RuntimeAnswerVerification,
        verification_policy::{AnswerVisibilityKind, finalize_answer_visibility},
    },
};
use ironrag_contracts::assistant::semantic_verification::*;
use serde_json::json;
use std::cell::Cell;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixture(
    question: &str,
    answer: &str,
    text: &str,
) -> Result<PreparedSemanticVerification, SemanticPreparationError> {
    prepare(
        question,
        answer,
        vec![AuthorizedSemanticSource {
            document_id: Uuid::from_u128(1),
            document_revision_id: Uuid::from_u128(2),
            locator: SemanticSourceLocator::DocumentSlice { start: 0, end: text.len() },
            text: text.into(),
            complete_document: Some(CompleteDocumentProof {
                canonical_text_sha256: answer_hash(text),
                canonical_utf8_bytes: text.len(),
            }),
        }],
    )
}

fn report(
    prepared: &PreparedSemanticVerification,
    support: &str,
    completeness: &str,
    response: &str,
) -> Result<SemanticVerificationReport, SemanticValidationError> {
    let quote =
        json!({"sourceId":prepared.sources()[0].source_id,"quote":prepared.sources()[0].text});
    let links = if support == "no_factual_claims" { json!([]) } else { json!([quote.clone()]) };
    let units = prepared.units().iter().map(|unit| json!({"unitId":unit.unit_id,"support":support,"assertsDocumentAbsence":response == "correct_source_bounded_abstention","evidence":links})).collect::<Vec<_>>();
    let caveats = if completeness == "missing_essential_caveats" {
        json!([{"description":"An essential qualification was omitted.","evidence":[quote]}])
    } else {
        json!([])
    };
    let raw = json!({"protocolVersion":SEMANTIC_VERIFICATION_PROTOCOL_VERSION,"questionSha256":prepared.question_sha256(),"checkedAnswerSha256":prepared.answer_sha256(),"evidenceManifestSha256":prepared.manifest_sha256(),"units":units,"completeness":completeness,"missingCaveats":caveats,"responseClassification":response}).to_string();
    prepared.parse_response(
        StructuredSemanticOutput {
            json: &raw,
            status: SemanticOutputStatus::Complete,
            output_tokens: Some(100),
        },
        3,
        Some(Uuid::from_u128(8)),
    )
}

fn formal() -> RuntimeAnswerVerification {
    RuntimeAnswerVerification {
        semantic_verification: None,
        state: State::NotRun,
        warnings: vec![warning("semantic_verification_not_run")],
        unsupported_literals: Vec::new(),
    }
}

fn warning(code: &str) -> QueryVerificationWarning {
    QueryVerificationWarning {
        code: code.into(),
        message: "Typed test warning.".into(),
        related_segment_id: None,
        related_fact_id: None,
    }
}

#[test]
fn completed_assessments_map_without_conflating_caveats_refusals_or_operations() -> TestResult {
    let prepared = fixture("Q", "A raw candidate.", "A neutral selected source.")?;
    for (support, completeness, response, expected) in [
        ("supported", "complete", "factual_answer", State::Verified),
        ("mixed", "complete", "factual_answer", State::PartiallySupported),
        ("contradicted", "complete", "factual_answer", State::Conflicting),
        ("insufficient_evidence", "undetermined", "factual_answer", State::InsufficientEvidence),
        ("supported", "missing_essential_caveats", "factual_answer", State::PartiallySupported),
        ("supported", "undetermined", "factual_answer", State::PartiallySupported),
        ("no_factual_claims", "undetermined", "false_refusal", State::NotRun),
        ("supported", "complete", "correct_source_bounded_abstention", State::NotRun),
    ] {
        let mut verification = formal();
        apply_semantic_report(
            &mut verification,
            report(&prepared, support, completeness, response)?,
        );
        assert_eq!(verification.state, expected);
        assert!(
            !verification
                .warnings
                .iter()
                .any(|warning| warning.code == "semantic_verification_not_run")
        );
    }
    let mut failed = formal();
    apply_semantic_report(&mut failed, prepared.failed(SemanticFailureCode::Timeout, 12, None));
    assert_eq!(failed.state, State::Failed);
    assert!(matches!(
        failed.semantic_verification.as_ref().map(|report| &report.outcome),
        Some(SemanticVerificationOutcome::Failed { code: SemanticFailureCode::Timeout })
    ));
    let mut not_run = formal();
    not_run.state = State::Verified;
    apply_semantic_report(
        &mut not_run,
        prepared.not_run(SemanticNotRunReason::UnsupportedStructuredTransport, 0),
    );
    assert_eq!(not_run.state, State::NotRun);
    assert!(not_run.warnings.iter().any(|warning| warning.code == "semantic_not_run"));
    let mut blocked_not_run = formal();
    blocked_not_run.state = State::Verified;
    blocked_not_run.warnings.push(warning("partial_coverage"));
    apply_semantic_report(
        &mut blocked_not_run,
        prepared.not_run(SemanticNotRunReason::BindingUnavailable, 0),
    );
    assert_eq!(blocked_not_run.state, State::PartiallySupported);
    Ok(())
}

#[test]
fn supported_semantics_never_remove_formal_or_coverage_blockers() -> TestResult {
    let prepared = fixture("Q", "A raw candidate.", "A neutral source.")?;
    let completed = report(&prepared, "supported", "complete", "factual_answer")?;
    for code in [
        "unsupported_literal",
        "conflicting_evidence",
        "partial_coverage",
        "enumeration_coverage_partial",
        "variant_coverage_incomplete",
        "no_canonical_evidence",
    ] {
        let mut verification = formal();
        verification.state = State::Verified;
        verification.warnings.push(warning(code));
        apply_semantic_report(&mut verification, completed.clone());
        assert_ne!(verification.state, State::Verified);
        assert!(verification.warnings.iter().any(|warning| warning.code == code));
    }
    let mut verification = formal();
    verification.state = State::Conflicting;
    apply_semantic_report(&mut verification, completed);
    assert_eq!(verification.state, State::Conflicting);
    Ok(())
}

#[test]
fn raw_checked_candidate_is_not_relabelled_as_the_visible_strict_fallback() -> TestResult {
    let raw = "A selected raw candidate.";
    let prepared = fixture("Q", raw, "A selected neutral source.")?;
    let mut verification = formal();
    apply_semantic_report(
        &mut verification,
        report(&prepared, "contradicted", "complete", "factual_answer")?,
    );
    let finalized = finalize_answer_visibility(
        VerificationLevel::Strict,
        verification.state,
        &verification.warnings,
        QueryLanguage::Auto,
        raw,
        AnswerVisibilityKind::FactualCandidate,
    );
    assert_ne!(finalized.visible_answer, raw);
    let mut completed = verification.semantic_verification.take().ok_or("missing report")?;
    bind_visible_answer(&mut completed, &finalized.visible_answer);
    assert_eq!(completed.checked_answer_sha256, answer_hash(raw));
    assert_eq!(
        completed.answer_units.iter().map(|unit| unit.text.as_str()).collect::<String>(),
        raw
    );
    assert_ne!(
        completed.visible_answer_sha256.as_deref(),
        Some(completed.checked_answer_sha256.as_str())
    );
    let stored = attach_semantic_verification(
        json!({"answerDisposition":"safe_fallback"}),
        Some(&completed),
    )?;
    assert_eq!(persisted_semantic_verification(&stored)?, Some(completed));
    Ok(())
}

#[test]
fn historical_absence_is_none_but_corrupt_or_projected_metadata_fails() -> TestResult {
    assert_eq!(persisted_semantic_verification(&json!({}))?, None);
    for malformed in [
        json!(null),
        json!({"projection":"summary","outcome":"completed"}),
        json!({"protocolVersion":1}),
    ] {
        assert!(
            persisted_semantic_verification(&json!({"semanticVerification":malformed})).is_err()
        );
    }
    let prepared = fixture("Q", "A candidate.", "A source.")?;
    let completed = report(&prepared, "supported", "complete", "factual_answer")?;
    let mut stored = attach_semantic_verification(json!({}), Some(&completed))?;
    stored["semanticVerification"]["outcome"]["assessment"]["units"][0]["evidence"][0]["span"]["start"] =
        json!(1);
    assert!(persisted_semantic_verification(&stored).is_err());
    Ok(())
}

#[tokio::test]
async fn selected_dispatch_occurs_once_and_exact_receipt_reuse_does_not_dispatch() -> TestResult {
    let prepared = fixture("Q", "A selected candidate.", "A source.")?;
    let completed = report(&prepared, "supported", "complete", "factual_answer")?;
    let invoked = Cell::new(0);
    let mut calls = Vec::new();
    let mut iterations = Vec::new();
    let fresh = super::orchestration::dispatch_selected(
        &prepared,
        None,
        &mut calls,
        &mut iterations,
        || async {
            invoked.set(invoked.get() + 1);
            Ok(SemanticProviderAttempt {
                report: completed.clone(),
                provider_call: None,
                iteration: None,
                completion_error: None,
            })
        },
    )
    .await?;
    assert_eq!(invoked.get(), 1);
    let reused = super::orchestration::dispatch_selected(
        &prepared,
        Some(&fresh),
        &mut calls,
        &mut iterations,
        || async {
            invoked.set(invoked.get() + 1);
            Ok(SemanticProviderAttempt {
                report: completed.clone(),
                provider_call: None,
                iteration: None,
                completion_error: None,
            })
        },
    )
    .await?;
    assert_eq!(invoked.get(), 1);
    assert_eq!(fresh, reused);
    for changed in [
        fixture("Changed question", "A selected candidate.", "A source.")?,
        fixture("Q", "Changed candidate.", "A source.")?,
        fixture("Q", "A selected candidate.", "Changed source.")?,
    ] {
        assert!(!receipt_matches(&changed, &fresh));
        let changed_report = report(&changed, "supported", "complete", "factual_answer")?;
        super::orchestration::dispatch_selected(
            &changed,
            Some(&fresh),
            &mut calls,
            &mut iterations,
            || async {
                invoked.set(invoked.get() + 1);
                Ok(SemanticProviderAttempt {
                    report: changed_report,
                    provider_call: None,
                    iteration: None,
                    completion_error: None,
                })
            },
        )
        .await?;
    }
    assert_eq!(invoked.get(), 4);
    Ok(())
}

#[tokio::test]
async fn paid_attempt_is_retained_before_completion_failure_is_propagated() -> TestResult {
    use crate::{
        agent_runtime::tasks::query_answer::{QueryProviderCallAttribution, QueryProviderCallKind},
        domains::{ai::AiBindingPurpose, provider_profiles::ProviderModelSelection},
        interfaces::http::router_support::ApiError,
        services::query::llm_context_debug::LlmIterationDebug,
    };
    let prepared = fixture("Q", "A selected candidate.", "A source.")?;
    let usage = json!({"prompt_tokens":10,"completion_tokens":11});
    let call = QueryProviderCallAttribution::try_new(
        Uuid::from_u128(7),
        AiBindingPurpose::QueryAnswer,
        ProviderModelSelection {
            provider_kind: "neutral".into(),
            model_name: "neutral-model".into(),
        },
        QueryProviderCallKind::QueryAnswerSemanticVerification,
    )?
    .record(Uuid::from_u128(8), usage.clone());
    let debug = LlmIterationDebug {
        iteration: 0,
        provider_kind: "neutral".into(),
        model_name: "neutral-model".into(),
        request_messages: Vec::new(),
        response_text: Some("A paid malformed response.".into()),
        response_tool_calls: Vec::new(),
        usage: usage.clone(),
        duration_ms: Some(1),
        child_runtime_execution_ids: Vec::new(),
        child_query_execution_ids: Vec::new(),
    };
    let mut calls = Vec::new();
    let mut iterations = Vec::new();
    let outcome = super::orchestration::dispatch_selected(
        &prepared,
        None,
        &mut calls,
        &mut iterations,
        || async {
            Ok(SemanticProviderAttempt {
                report: prepared.failed(
                    SemanticFailureCode::ProviderError,
                    1,
                    Some(Uuid::from_u128(8)),
                ),
                provider_call: Some(call.clone()),
                iteration: Some(debug),
                completion_error: Some(ApiError::Conflict("completion failed".into())),
            })
        },
    )
    .await;
    assert!(outcome.is_err());
    assert_eq!(calls, vec![call]);
    assert_eq!(iterations[0].usage, usage);
    assert_eq!(iterations[0].response_text.as_deref(), Some("A paid malformed response."));
    assert_eq!(iterations[0].iteration, 1);
    Ok(())
}

#[test]
fn preparation_refusal_remains_empty_unchecked_metadata_after_reload() -> TestResult {
    let error = SemanticPreparationError::AnswerTooLarge;
    let mut refused =
        preparation_failure_report("Q", &"x".repeat(MAX_ANSWER_UTF8_BYTES + 1), error, 1);
    bind_visible_answer(&mut refused, "A system fallback.");
    assert!(refused.answer_units.is_empty() && refused.sources.is_empty());
    let stored = attach_semantic_verification(json!({}), Some(&refused))?;
    assert_eq!(persisted_semantic_verification(&stored)?, Some(refused));
    Ok(())
}

#[test]
fn source_admission_rejects_stale_heads_deleted_documents_and_cross_library_rows() -> TestResult {
    use crate::{
        infra::knowledge_rows::{KnowledgeDocumentRow, KnowledgeRevisionRow},
        services::query::provider_billing::QueryProviderExecutionContext,
    };
    let execution = QueryProviderExecutionContext {
        workspace_id: Uuid::from_u128(3),
        library_id: Uuid::from_u128(4),
        query_execution_id: Uuid::from_u128(5),
        runtime_execution_id: Uuid::from_u128(6),
    };
    let document: KnowledgeDocumentRow = serde_json::from_value(json!({
        "document_id":Uuid::from_u128(1),"workspace_id":execution.workspace_id,"library_id":execution.library_id,"external_key":"neutral", "document_state":"active", "readable_revision_id":Uuid::from_u128(2),"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"
    }))?;
    let revision: KnowledgeRevisionRow = serde_json::from_value(json!({
        "revision_id":Uuid::from_u128(2),"document_id":document.document_id,"workspace_id":execution.workspace_id,"library_id":execution.library_id,"revision_number":1,"revision_state":"ready","revision_kind":"upload","mime_type":"text/plain","checksum":"neutral","byte_size":1,"text_state":"ready","vector_state":"ready","graph_state":"ready","text_readable_at":"2026-01-01T00:00:00Z","created_at":"2026-01-01T00:00:00Z"
    }))?;
    assert!(super::evidence::source_scope_matches(execution, &document, &revision));
    let mut stale = document.clone();
    stale.readable_revision_id = Some(Uuid::from_u128(99));
    assert!(!super::evidence::source_scope_matches(execution, &stale, &revision));
    let mut other_library = revision.clone();
    other_library.library_id = Uuid::from_u128(99);
    assert!(!super::evidence::source_scope_matches(execution, &document, &other_library));
    let mut other_workspace = document.clone();
    other_workspace.workspace_id = Uuid::from_u128(99);
    assert!(!super::evidence::source_scope_matches(execution, &other_workspace, &revision));
    let mut deleted = document;
    deleted.deleted_at = Some(chrono::Utc::now());
    assert!(!super::evidence::source_scope_matches(execution, &deleted, &revision));
    Ok(())
}

#[test]
fn document_character_offsets_are_converted_without_inventing_complete_scope() -> TestResult {
    let prefix = "До.\n";
    let quote = "ёж.";
    let text = format!("{prefix}{quote}\nПосле.");
    let start_chars = prefix.chars().count();
    let end_chars = start_chars + quote.chars().count();
    let (start, end, selected) =
        super::evidence::canonical_document_slice(&text, start_chars, end_chars)?;
    assert_eq!(selected, quote);
    assert_eq!(start, prefix.len());
    assert_eq!(end, prefix.len() + quote.len());
    assert_ne!(end, text.len());
    let (start, end, selected) =
        super::evidence::canonical_document_slice(&text, 0, text.chars().count())?;
    assert_eq!((start, end, selected), (0, text.len(), text.as_str()));
    assert!(super::evidence::canonical_document_slice(&text, 0, usize::MAX).is_err());
    assert!(super::evidence::canonical_document_slice(&text, end_chars, start_chars).is_err());
    Ok(())
}

#[test]
fn selected_semantic_chunk_retains_window_conditions_and_table_column_identity() -> TestResult {
    use crate::infra::knowledge_rows::KnowledgeChunkRow;
    use ironrag_contracts::assistant::semantic_verification::SemanticEvidenceScope;
    let mut row: KnowledgeChunkRow = serde_json::from_value(json!({
        "chunk_id":Uuid::from_u128(1), "workspace_id":Uuid::from_u128(2),
        "library_id":Uuid::from_u128(3), "document_id":Uuid::from_u128(4),
        "revision_id":Uuid::from_u128(5), "chunk_index":0, "chunk_kind":"paragraph",
        "content_text":"An operator may restart the service.",
        "normalized_text":"An operator may restart the service.",
        "window_text":"An operator may restart the service. If confirmation is missing, restart is forbidden.",
        "section_path":["Procedure"], "heading_trail":["Procedure"], "chunk_state":"ready"
    }))?;
    let source = super::evidence::selected_chunk_source(&row);
    let prepared =
        prepare("When is restart allowed?", "Restart requires confirmation.", vec![source])?;
    let sent = prepared.request_json();
    assert!(
        sent["sources"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("restart is forbidden"))
    );
    assert_eq!(prepared.sources()[0].scope, SemanticEvidenceScope::SelectedExcerpt);
    row.chunk_kind = Some("table_row".into());
    row.content_text = "LP-080 | 20 GB".into();
    row.normalized_text = "- Plan code: LP-080\n- Export size: 20 GB".into();
    row.window_text = Some("Adjacent unrelated table prose.".into());
    let table = super::evidence::selected_chunk_source(&row);
    let prepared = prepare("What is the export limit?", "The export limit is 20 GB.", vec![table])?;
    assert!(prepared.sources()[0].text.contains("Export size: 20 GB"));
    assert!(!prepared.sources()[0].text.contains("unrelated"));
    assert_eq!(prepared.sources()[0].scope, SemanticEvidenceScope::SelectedExcerpt);
    Ok(())
}
