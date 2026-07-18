use std::collections::{HashMap, HashSet};

use anyhow::Context;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::query::{
        QueryAnswerDisposition, QueryClarification, QueryVerificationState,
        QueryVerificationWarning,
    },
    services::query::{
        assistant_grounding::AssistantGroundingEvidence,
        completion_policy::AnswerCompletionContract, planner::QueryIntentProfile,
    },
    shared::text_tokens::literal_wildcard_prefixes,
};

use super::{
    types::{CanonicalAnswerEvidence, RuntimeAnswerVerification, RuntimeMatchedChunk},
    verification_claims::{
        extract_answer_literals, extract_formal_exact_claims, is_exact_numeric_literal,
        normalize_verification_literal,
    },
    verification_support::{
        answer_is_verbatim_supported_by_corpus, build_boundary_verification_corpus,
        build_exact_relationship_verification_corpus, build_verification_corpus,
        collect_conflicting_fact_groups, exact_numeric_literal_is_supported_by_corpus,
        formal_exact_claim_is_supported_by_corpus, has_canonical_grounding_evidence,
        literal_is_supported_by_canonical_corpus, literal_is_user_supplied_wildcard_scope,
    },
};

pub(crate) fn verify_answer_against_canonical_evidence(
    question: &str,
    answer: &str,
    intent_profile: &QueryIntentProfile,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
    prompt_context: &str,
    assistant_grounding: &AssistantGroundingEvidence,
) -> RuntimeAnswerVerification {
    if let Some(early_result) =
        early_verification_result(answer, evidence, chunks, assistant_grounding)
    {
        return early_result;
    }

    let corpora = VerificationCorpora::new(evidence, chunks, assistant_grounding, prompt_context);
    let question_wildcard_prefixes = literal_wildcard_prefixes(question, 2);
    let (inline_literals, fenced_line_literals) = extract_answer_literals(answer);
    let formal_exact_claims = if intent_profile.exact_literal_technical {
        extract_formal_exact_claims(answer)
    } else {
        Vec::new()
    };
    let mut result = VerificationResult::default();
    verify_formal_exact_claims(&formal_exact_claims, &corpora.boundary, &mut result);
    verify_formatted_literals(
        inline_literals.iter().chain(fenced_line_literals.iter()),
        &question_wildcard_prefixes,
        &corpora,
        &mut result,
    );

    let has_conflicting_evidence = append_conflict_warning(
        intent_profile.exact_literal_technical,
        evidence,
        &mut result.warnings,
    );
    let answer_is_verbatim_grounded =
        answer_is_verbatim_supported_by_corpus(answer, &corpora.grounding);
    let has_intent_mismatch = append_intent_warning(answer, intent_profile, &mut result.warnings);
    let has_variant_coverage_mismatch =
        append_variant_warning(answer, intent_profile, evidence, chunks, &mut result.warnings);
    let has_unsupported_literals =
        result.warnings.iter().any(|warning| warning.code == "unsupported_literal");
    let has_unsupported_canonical_claim =
        result.warnings.iter().any(|warning| warning.code == "unsupported_canonical_claim");
    append_semantic_verification_warning(
        has_unsupported_literals,
        has_unsupported_canonical_claim,
        has_conflicting_evidence,
        answer_is_verbatim_grounded,
        result.verified_literal_count,
        &mut result.warnings,
    );
    let state = final_verification_state(
        has_unsupported_literals,
        has_unsupported_canonical_claim,
        has_conflicting_evidence,
        answer_is_verbatim_grounded,
        has_intent_mismatch,
        has_variant_coverage_mismatch,
        result.verified_literal_count,
    );
    RuntimeAnswerVerification {
        state,
        warnings: result.warnings,
        unsupported_literals: result.unsupported_literals,
    }
}

struct VerificationCorpora {
    all: Vec<String>,
    grounding: Vec<String>,
    boundary: Vec<String>,
    relationships: Vec<String>,
}

impl VerificationCorpora {
    fn new(
        evidence: &CanonicalAnswerEvidence,
        chunks: &[RuntimeMatchedChunk],
        assistant_grounding: &AssistantGroundingEvidence,
        prompt_context: &str,
    ) -> Self {
        let mut all = build_verification_corpus(evidence, chunks, assistant_grounding);
        let grounding = all.clone();
        let normalized_prompt_context = normalize_verification_literal(prompt_context);
        if !normalized_prompt_context.is_empty() {
            all.push(normalized_prompt_context);
        }
        Self {
            all,
            grounding,
            boundary: build_boundary_verification_corpus(evidence, chunks, assistant_grounding),
            relationships: build_exact_relationship_verification_corpus(
                evidence,
                chunks,
                assistant_grounding,
            ),
        }
    }
}

#[derive(Default)]
struct VerificationResult {
    warnings: Vec<QueryVerificationWarning>,
    unsupported_literals: Vec<String>,
    seen_literals: HashSet<String>,
    verified_literal_count: usize,
}

fn verification_warning(code: &str, message: String) -> QueryVerificationWarning {
    QueryVerificationWarning {
        code: code.to_string(),
        message,
        related_segment_id: None,
        related_fact_id: None,
    }
}

fn early_verification_result(
    answer: &str,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
    assistant_grounding: &AssistantGroundingEvidence,
) -> Option<RuntimeAnswerVerification> {
    let (state, code, message) = if answer.trim().is_empty() {
        (QueryVerificationState::Failed, "empty_answer", "Answer generation returned empty output.")
    } else if !has_canonical_grounding_evidence(evidence, chunks, assistant_grounding) {
        (
            QueryVerificationState::InsufficientEvidence,
            "no_canonical_evidence",
            "Answer verification requires selected canonical evidence.",
        )
    } else {
        return None;
    };
    Some(RuntimeAnswerVerification {
        state,
        warnings: vec![verification_warning(code, message.to_string())],
        unsupported_literals: Vec::new(),
    })
}

fn verify_formal_exact_claims(
    claims: &[super::verification_claims::FormalExactClaim],
    boundary_corpus: &[String],
    result: &mut VerificationResult,
) {
    for claim in claims {
        let literal = claim.literal();
        let normalized_literal = normalize_verification_literal(literal);
        if normalized_literal.is_empty() || !result.seen_literals.insert(normalized_literal) {
            continue;
        }
        if formal_exact_claim_is_supported_by_corpus(claim, boundary_corpus) {
            result.verified_literal_count += 1;
        } else {
            result.unsupported_literals.push(literal.to_string());
            result.warnings.push(verification_warning(
                "unsupported_canonical_claim",
                format!("Formal exact claim `{literal}` is not grounded in selected evidence."),
            ));
        }
    }
}

fn verify_formatted_literals<'a>(
    literals: impl Iterator<Item = &'a String>,
    question_wildcard_prefixes: &[String],
    corpora: &VerificationCorpora,
    result: &mut VerificationResult,
) {
    for literal in literals {
        let normalized_literal = normalize_verification_literal(literal);
        if normalized_literal.is_empty()
            || !result.seen_literals.insert(normalized_literal.clone())
            || literal_is_user_supplied_wildcard_scope(literal, question_wildcard_prefixes)
        {
            continue;
        }
        let supported = if is_exact_numeric_literal(literal) {
            exact_numeric_literal_is_supported_by_corpus(literal, &corpora.boundary)
        } else {
            literal_is_supported_by_canonical_corpus(
                literal,
                &corpora.all,
                &corpora.grounding,
                &corpora.relationships,
            )
        };
        if supported {
            result.verified_literal_count += 1;
        } else {
            result.unsupported_literals.push(literal.clone());
            result.warnings.push(verification_warning(
                "unsupported_literal",
                format!("Literal `{literal}` is not grounded in selected evidence."),
            ));
        }
    }
}

fn append_conflict_warning(
    checks_exact_literals: bool,
    evidence: &CanonicalAnswerEvidence,
    warnings: &mut Vec<QueryVerificationWarning>,
) -> bool {
    let conflict_count = if checks_exact_literals {
        collect_conflicting_fact_groups(&evidence.technical_facts).len()
    } else {
        0
    };
    if conflict_count > 0 {
        warnings.push(verification_warning(
            "conflicting_evidence",
            format!(
                "Selected evidence contains {conflict_count} conflicting technical fact group(s)."
            ),
        ));
    }
    conflict_count > 0
}

fn append_intent_warning(
    answer: &str,
    intent_profile: &QueryIntentProfile,
    warnings: &mut Vec<QueryVerificationWarning>,
) -> bool {
    let has_mismatch =
        !AnswerCompletionContract::from_query_act(intent_profile.act).evaluate(answer).complete;
    if has_mismatch {
        warnings.push(verification_warning(
            "intent_mismatch",
            "Answer structure does not satisfy the compiled query act.".to_string(),
        ));
    }
    has_mismatch
}

fn append_variant_warning(
    answer: &str,
    intent_profile: &QueryIntentProfile,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
    warnings: &mut Vec<QueryVerificationWarning>,
) -> bool {
    let represented_document_count = chunks
        .iter()
        .map(|chunk| chunk.document_id)
        .chain(evidence.structured_blocks.iter().map(|block| block.document_id))
        .chain(evidence.technical_facts.iter().map(|fact| fact.document_id))
        .collect::<HashSet<_>>()
        .len();
    let has_mismatch = intent_profile.broad_procedure_variant_coverage
        && represented_document_count >= 2
        && !answer_mentions_multiple_document_variants(answer, chunks, evidence);
    if has_mismatch {
        warnings.push(verification_warning(
            "variant_coverage_incomplete",
            "Answer does not cover multiple grounded procedure variants.".to_string(),
        ));
    }
    has_mismatch
}

fn append_semantic_verification_warning(
    has_unsupported_literals: bool,
    has_unsupported_canonical_claim: bool,
    has_conflicting_evidence: bool,
    answer_is_verbatim_grounded: bool,
    verified_literal_count: usize,
    warnings: &mut Vec<QueryVerificationWarning>,
) {
    if has_unsupported_literals
        || has_unsupported_canonical_claim
        || has_conflicting_evidence
        || answer_is_verbatim_grounded
    {
        return;
    }
    let (code, message) = if verified_literal_count > 0 {
        (
            "semantic_verification_partial",
            "Only formal literals were verified; ordinary prose has no typed semantic verification result.",
        )
    } else {
        (
            "semantic_verification_not_run",
            "Ordinary prose has no typed semantic verification result.",
        )
    };
    warnings.push(verification_warning(code, message.to_string()));
}

fn final_verification_state(
    has_unsupported_literals: bool,
    has_unsupported_canonical_claim: bool,
    has_conflicting_evidence: bool,
    answer_is_verbatim_grounded: bool,
    has_intent_mismatch: bool,
    has_variant_coverage_mismatch: bool,
    verified_literal_count: usize,
) -> QueryVerificationState {
    if has_unsupported_literals || has_unsupported_canonical_claim {
        QueryVerificationState::InsufficientEvidence
    } else if has_conflicting_evidence {
        QueryVerificationState::Conflicting
    } else if answer_is_verbatim_grounded && !has_intent_mismatch && !has_variant_coverage_mismatch
    {
        QueryVerificationState::Verified
    } else if verified_literal_count > 0
        || answer_is_verbatim_grounded
        || has_variant_coverage_mismatch
    {
        QueryVerificationState::PartiallySupported
    } else {
        QueryVerificationState::NotRun
    }
}

fn answer_mentions_multiple_document_variants(
    answer: &str,
    chunks: &[RuntimeMatchedChunk],
    evidence: &CanonicalAnswerEvidence,
) -> bool {
    let answer_literals = extract_answer_literals(answer);
    let answer_literal_keys = answer_literals
        .0
        .iter()
        .chain(answer_literals.1.iter())
        .map(|literal| normalize_verification_literal(literal))
        .filter(|literal| !literal.is_empty())
        .collect::<HashSet<_>>();
    let prose_anchors = source_local_prose_anchors(answer);
    let anchors = answer_literal_keys.union(&prose_anchors).cloned().collect::<HashSet<_>>();
    let mut anchor_documents = HashMap::<String, HashSet<Uuid>>::new();
    for chunk in chunks {
        record_source_variant_anchors(
            chunk.document_id,
            &format!("{}\n{}", chunk.source_text, chunk.excerpt),
            &anchors,
            &mut anchor_documents,
        );
    }
    for block in &evidence.structured_blocks {
        record_source_variant_anchors(
            block.document_id,
            &format!("{}\n{}", block.text, block.normalized_text),
            &anchors,
            &mut anchor_documents,
        );
    }
    for fact in &evidence.technical_facts {
        record_source_variant_anchors(
            fact.document_id,
            &format!(
                "{}\n{}\n{}",
                fact.canonical_value_exact, fact.canonical_value_text, fact.display_value
            ),
            &anchors,
            &mut anchor_documents,
        );
    }
    anchor_documents
        .values()
        .filter(|document_ids| document_ids.len() == 1)
        .flat_map(HashSet::iter)
        .copied()
        .collect::<HashSet<_>>()
        .len()
        >= 2
}

fn record_source_variant_anchors(
    document_id: Uuid,
    source: &str,
    anchors: &HashSet<String>,
    anchor_documents: &mut HashMap<String, HashSet<Uuid>>,
) {
    let normalized = normalize_verification_literal(source);
    for anchor in anchors.iter().filter(|anchor| normalized.contains(*anchor)) {
        anchor_documents.entry(anchor.clone()).or_default().insert(document_id);
    }
}

fn source_local_prose_anchors(answer: &str) -> HashSet<String> {
    answer
        .split(['\n', '.', '!', '?', ';'])
        .map(strip_formal_list_prefix)
        .map(normalize_verification_literal)
        .filter(|anchor| anchor.chars().filter(|ch| ch.is_alphanumeric()).count() >= 24)
        .collect()
}

fn strip_formal_list_prefix(value: &str) -> &str {
    let trimmed = value.trim_start();
    let after_bullet = trimmed
        .strip_prefix('-')
        .or_else(|| trimmed.strip_prefix('*'))
        .map(str::trim_start)
        .unwrap_or(trimmed);
    let digit_count = after_bullet.chars().take_while(char::is_ascii_digit).count();
    if digit_count == 0 {
        return after_bullet;
    }
    after_bullet[digit_count..].trim_start_matches(['.', ')', ':', '-', ' ']).trim_start()
}

pub(crate) async fn persist_query_verification(
    state: &AppState,
    execution_id: Uuid,
    verification: &RuntimeAnswerVerification,
    answer_disposition: QueryAnswerDisposition,
    clarification: &QueryClarification,
    canonical_evidence: &CanonicalAnswerEvidence,
    assistant_grounding: &AssistantGroundingEvidence,
) -> anyhow::Result<()> {
    let bundle = state
        .context_store
        .get_bundle_by_query_execution(execution_id)
        .await
        .with_context(|| format!("failed to load context bundle for verification {execution_id}"))?
        .with_context(|| {
            format!("query context bundle is missing while finalizing execution {execution_id}")
        })?;
    let warnings_json = serde_json::to_value(&verification.warnings)
        .context("failed to serialize verification warnings")?;
    let candidate_summary = enrich_query_candidate_summary(
        bundle.candidate_summary.clone(),
        canonical_evidence,
        assistant_grounding,
    );
    let candidate_summary =
        attach_query_answer_outcome(candidate_summary, answer_disposition, clarification)?;
    let assembly_diagnostics = enrich_query_assembly_diagnostics(
        bundle.assembly_diagnostics.clone(),
        verification,
        answer_disposition,
        &candidate_summary,
        assistant_grounding,
    )?;
    let updated_bundle = state
        .context_store
        .update_bundle_state(
            bundle.bundle_id,
            &bundle.bundle_state,
            &bundle.selected_fact_ids,
            verification_state_label(verification.state),
            warnings_json,
            bundle.freshness_snapshot,
            candidate_summary,
            assembly_diagnostics,
        )
        .await
        .context("failed to persist query verification state")?;
    anyhow::ensure!(
        updated_bundle.is_some(),
        "query context bundle disappeared while persisting final answer outcome"
    );
    Ok(())
}

const ANSWER_DISPOSITION_SUMMARY_KEY: &str = "answerDisposition";
const ANSWER_CLARIFICATION_SUMMARY_KEY: &str = "answerClarification";

pub(crate) fn attach_query_answer_outcome(
    mut candidate_summary: serde_json::Value,
    answer_disposition: QueryAnswerDisposition,
    clarification: &QueryClarification,
) -> anyhow::Result<serde_json::Value> {
    anyhow::ensure!(
        matches!(answer_disposition, QueryAnswerDisposition::Clarification)
            == clarification.required,
        "answer disposition and typed clarification metadata disagree"
    );
    let Some(object) = candidate_summary.as_object_mut() else {
        anyhow::bail!("query candidate summary is not a JSON object");
    };
    object.insert(
        ANSWER_DISPOSITION_SUMMARY_KEY.to_string(),
        serde_json::Value::String(answer_disposition.storage_label().to_string()),
    );
    if clarification.required {
        object.insert(
            ANSWER_CLARIFICATION_SUMMARY_KEY.to_string(),
            serde_json::to_value(clarification)
                .context("failed to serialize typed query clarification")?,
        );
    } else {
        object.remove(ANSWER_CLARIFICATION_SUMMARY_KEY);
    }
    Ok(candidate_summary)
}

pub(crate) fn persisted_query_answer_outcome(
    candidate_summary: &serde_json::Value,
) -> anyhow::Result<(QueryAnswerDisposition, QueryClarification)> {
    let summary = candidate_summary
        .as_object()
        .context("persisted query candidate summary must be a JSON object")?;
    let disposition_value = summary
        .get(ANSWER_DISPOSITION_SUMMARY_KEY)
        .map(|value| value.as_str().context("persisted answer disposition must be a string"))
        .transpose()?;
    let disposition = match disposition_value {
        Some("factual_ready") => QueryAnswerDisposition::FactualReady,
        Some("safe_fallback") => QueryAnswerDisposition::SafeFallback,
        Some("clarification") => QueryAnswerDisposition::Clarification,
        Some("non_terminal") | None => QueryAnswerDisposition::NonTerminal,
        Some(value) => anyhow::bail!("unknown persisted answer disposition `{value}`"),
    };
    if !matches!(disposition, QueryAnswerDisposition::Clarification) {
        anyhow::ensure!(
            !summary.contains_key(ANSWER_CLARIFICATION_SUMMARY_KEY),
            "persisted non-clarification disposition carries contradictory typed metadata"
        );
        return Ok((disposition, QueryClarification::default()));
    }
    let clarification = summary
        .get(ANSWER_CLARIFICATION_SUMMARY_KEY)
        .cloned()
        .context("persisted clarification disposition is missing typed metadata")
        .and_then(|value| {
            serde_json::from_value::<QueryClarification>(value)
                .context("failed to decode persisted typed clarification")
        })?;
    anyhow::ensure!(
        clarification.required,
        "persisted clarification disposition has required=false"
    );
    Ok((disposition, clarification))
}

fn verification_state_label(state: QueryVerificationState) -> &'static str {
    match state {
        QueryVerificationState::Verified => "verified",
        QueryVerificationState::PartiallySupported => "partially_supported",
        QueryVerificationState::Conflicting => "conflicting_evidence",
        QueryVerificationState::InsufficientEvidence => "insufficient_evidence",
        QueryVerificationState::Failed => "failed",
        QueryVerificationState::NotRun => "not_run",
    }
}

pub(crate) fn enrich_query_candidate_summary(
    candidate_summary: serde_json::Value,
    canonical_evidence: &CanonicalAnswerEvidence,
    assistant_grounding: &AssistantGroundingEvidence,
) -> serde_json::Value {
    let mut summary = candidate_summary;
    let Some(object) = summary.as_object_mut() else {
        return summary;
    };
    object.insert(
        "finalPreparedSegmentReferences".to_string(),
        serde_json::json!(canonical_evidence.structured_blocks.len()),
    );
    object.insert(
        "finalTechnicalFactReferences".to_string(),
        serde_json::json!(canonical_evidence.technical_facts.len()),
    );
    object.insert(
        "finalChunkReferences".to_string(),
        serde_json::json!(canonical_evidence.chunk_rows.len()),
    );
    object.insert(
        "finalAssistantDocumentReferences".to_string(),
        serde_json::json!(assistant_grounding.document_references.len()),
    );
    summary
}

pub(crate) fn enrich_query_assembly_diagnostics(
    assembly_diagnostics: serde_json::Value,
    verification: &RuntimeAnswerVerification,
    answer_disposition: QueryAnswerDisposition,
    candidate_summary: &serde_json::Value,
    assistant_grounding: &AssistantGroundingEvidence,
) -> anyhow::Result<serde_json::Value> {
    let mut diagnostics = assembly_diagnostics;
    let warnings = serde_json::to_value(&verification.warnings)
        .context("failed to serialize verification warnings for assembly diagnostics")?;
    let Some(object) = diagnostics.as_object_mut() else {
        return Ok(diagnostics);
    };
    object.insert(
        "verificationState".to_string(),
        serde_json::Value::String(verification_state_label(verification.state).to_string()),
    );
    object.insert("verificationWarnings".to_string(), warnings);
    object.insert(
        ANSWER_DISPOSITION_SUMMARY_KEY.to_string(),
        serde_json::Value::String(answer_disposition.storage_label().to_string()),
    );
    object.insert(
        "graphParticipation".to_string(),
        serde_json::json!({
            "entityReferenceCount": json_count(candidate_summary, "finalEntityReferences"),
            "relationReferenceCount": json_count(candidate_summary, "finalRelationReferences"),
            "graphBacked": json_count(candidate_summary, "finalEntityReferences") > 0
                || json_count(candidate_summary, "finalRelationReferences") > 0,
        }),
    );
    object.insert(
        "structuredEvidence".to_string(),
        serde_json::json!({
            "preparedSegmentReferenceCount": json_count(candidate_summary, "finalPreparedSegmentReferences"),
            "technicalFactReferenceCount": json_count(candidate_summary, "finalTechnicalFactReferences"),
            "chunkReferenceCount": json_count(candidate_summary, "finalChunkReferences"),
            "assistantDocumentReferenceCount": json_count(candidate_summary, "finalAssistantDocumentReferences"),
        }),
    );
    if !assistant_grounding.document_references.is_empty() {
        object.insert(
            "assistantGrounding".to_string(),
            serde_json::json!({
                "documentReferenceCount": assistant_grounding.document_references.len(),
                "documentReferences": assistant_grounding.document_references,
            }),
        );
    }
    Ok(diagnostics)
}

fn json_count(value: &serde_json::Value, key: &str) -> usize {
    value
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or_default()
}
