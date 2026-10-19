use super::context_probes::{split_identifier_subtokens, trim_structural_literal_token};
use super::literal_coverage::{
    EVIDENCE_LITERAL_RESTORATION_LIMIT, answer_needs_literal_revision,
    literal_revision_can_replace_answer, literal_revision_targets,
};
use super::output_boundary::{
    enforce_hard_output_boundary, literal_revision_context, merge_generation_usage,
    selected_runtime_grounding_evidence,
};
use super::preflight::structural_question_tokens;
use super::verification::verify_generated_answer;
use super::{
    AnswerVerificationStage, AppState, AssistantGroundingEvidence, ChatMessage, HashMap, HashSet,
    KnowledgeDocumentRow, PreparedAnswerQueryResult, QueryClarification, QueryIR,
    QueryVerificationPersistence, RuntimeMatchedChunk, Uuid,
};
use anyhow::Context as _;

/// Clarify-candidate `kind` for label-only evidence without a graph node id.
pub(super) const ANSWER_CANDIDATE_KIND_DOCUMENT: &str = "document";
pub(super) const CLARIFICATION_LABEL_MAX_CHARS: usize = 120;

pub(crate) fn declared_subject_literal_revision_targets(
    answer: &str,
    query_ir: &QueryIR,
    grounded_context: &str,
) -> Vec<String> {
    let subject_token_sequences = declared_subject_token_sequences(query_ir);
    if subject_token_sequences.is_empty() {
        return Vec::new();
    }
    let declared =
        crate::shared::structural_declarations::matching_structural_declaration_identifiers(
            grounded_context,
            |identifier| declared_identifier_matches_subject(identifier, &subject_token_sequences),
            usize::MAX,
        );
    if declared.iter().any(|identifier| answer.contains(identifier)) {
        return Vec::new();
    }
    declared.into_iter().take(EVIDENCE_LITERAL_RESTORATION_LIMIT).collect()
}

fn declared_subject_token_sequences(query_ir: &QueryIR) -> Vec<Vec<String>> {
    query_ir
        .target_entities
        .iter()
        .filter(|entity| matches!(entity.role, crate::domains::query_ir::EntityRole::Subject))
        .map(|entity| entity.label.as_str())
        .chain(query_ir.literal_constraints.iter().filter_map(|literal| {
            matches!(literal.kind, crate::domains::query_ir::LiteralKind::Identifier)
                .then_some(literal.text.as_str())
        }))
        .map(split_identifier_subtokens)
        .filter(|tokens| !tokens.is_empty())
        .collect()
}

fn declared_identifier_matches_subject(
    identifier: &str,
    subject_token_sequences: &[Vec<String>],
) -> bool {
    if !crate::domains::query_ir::literal_text_is_identifier_shaped(identifier) {
        return false;
    }
    let identifier_tokens = split_identifier_subtokens(identifier);
    !identifier_tokens.is_empty()
        && subject_token_sequences.iter().any(|subject_tokens| {
            subject_tokens
                .windows(identifier_tokens.len())
                .any(|window| window == identifier_tokens)
        })
}

pub(super) fn literal_fidelity_revision_request<'a>(
    library_id: Uuid,
    user_question: &'a str,
    conversation_history: &'a [ChatMessage],
    original_answer: &'a str,
    unsupported_literals: &'a [String],
    grounded_context: &'a str,
) -> crate::services::query::agent_loop::LiteralRevisionRequest<'a> {
    crate::services::query::agent_loop::LiteralRevisionRequest::Fidelity {
        library_id,
        user_question,
        conversation_history,
        original_answer,
        unsupported_literals,
        grounded_context,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LiteralRevisionPath {
    FastPath,
    AnswerPreflight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LiteralRevisionTelemetry {
    pub(super) start_stage: &'static str,
    pub(super) boundary_stage: &'static str,
    pub(super) rejected_stage: &'static str,
    pub(super) error_stage: &'static str,
    pub(super) restoration_start_stage: &'static str,
    pub(super) restoration_boundary_stage: &'static str,
    pub(super) restoration_rejected_stage: &'static str,
    pub(super) restoration_error_stage: &'static str,
}

impl LiteralRevisionPath {
    const fn telemetry(self) -> LiteralRevisionTelemetry {
        match self {
            Self::FastPath => LiteralRevisionTelemetry {
                start_stage: "answer.single_shot_literal_revision_start",
                boundary_stage: "answer.single_shot_literal_revision",
                rejected_stage: "answer.single_shot_literal_revision_rejected",
                error_stage: "answer.single_shot_literal_revision_error",
                restoration_start_stage: "answer.single_shot_compound_literal_revision_start",
                restoration_boundary_stage: "answer.single_shot_compound_literal_revision",
                restoration_rejected_stage: "answer.single_shot_compound_literal_revision_rejected",
                restoration_error_stage: "answer.single_shot_compound_literal_revision_error",
            },
            Self::AnswerPreflight => LiteralRevisionTelemetry {
                start_stage: "answer.preflight_single_shot_literal_revision_start",
                boundary_stage: "answer.preflight_single_shot_literal_revision",
                rejected_stage: "answer.preflight_single_shot_literal_revision_rejected",
                error_stage: "answer.preflight_single_shot_literal_revision_error",
                restoration_start_stage: "answer.preflight_single_shot_compound_literal_revision_start",
                restoration_boundary_stage: "answer.preflight_single_shot_compound_literal_revision",
                restoration_rejected_stage: "answer.preflight_single_shot_compound_literal_revision_rejected",
                restoration_error_stage: "answer.preflight_single_shot_compound_literal_revision_error",
            },
        }
    }

    pub(super) fn revised_grounding(
        self,
        prepared: &PreparedAnswerQueryResult,
        revision_grounding: AssistantGroundingEvidence,
    ) -> AssistantGroundingEvidence {
        match self {
            Self::FastPath => selected_runtime_grounding_evidence(prepared, revision_grounding),
            Self::AnswerPreflight => AssistantGroundingEvidence::default(),
        }
    }
}

pub(super) struct LiteralRevisionAttempt<'a> {
    pub(super) state: &'a AppState,
    pub(super) execution_context:
        crate::services::query::provider_billing::QueryProviderExecutionContext,
    pub(super) revision_question: &'a str,
    pub(super) verification_question: &'a str,
    pub(super) conversation_history: &'a [ChatMessage],
    pub(super) prepared: &'a PreparedAnswerQueryResult,
    pub(super) path: LiteralRevisionPath,
}

pub(super) async fn revise_answer_literals_if_needed(
    context: LiteralRevisionAttempt<'_>,
    verification_stage: &mut AnswerVerificationStage,
    provider_calls: &mut Vec<crate::agent_runtime::tasks::query_answer::QueryProviderCall>,
    debug_iterations: &mut Vec<crate::services::query::llm_context_debug::LlmIterationDebug>,
) -> anyhow::Result<()> {
    let telemetry = context.path.telemetry();
    let revision_context = literal_revision_context(
        &verification_stage.generation.prompt_context,
        &verification_stage.generation.assistant_grounding,
    );
    let mut outputs =
        LiteralRevisionOutputs { verification_stage, provider_calls, debug_iterations };
    apply_literal_fidelity_revision(&context, &mut outputs, telemetry, &revision_context).await?;
    apply_literal_restoration_revision(&context, &mut outputs, telemetry, &revision_context)
        .await?;
    Ok(())
}

/// The mutable verification outputs every literal-revision step rewrites.
struct LiteralRevisionOutputs<'a> {
    verification_stage: &'a mut AnswerVerificationStage,
    provider_calls: &'a mut Vec<crate::agent_runtime::tasks::query_answer::QueryProviderCall>,
    debug_iterations: &'a mut Vec<crate::services::query::llm_context_debug::LlmIterationDebug>,
}

/// Revises the answer over its grounded evidence when the verifier flagged
/// unsupported literals.
async fn apply_literal_fidelity_revision(
    context: &LiteralRevisionAttempt<'_>,
    outputs: &mut LiteralRevisionOutputs<'_>,
    telemetry: LiteralRevisionTelemetry,
    revision_context: &str,
) -> anyhow::Result<()> {
    let execution_id = context.execution_context.query_execution_id;
    if !answer_needs_literal_revision(outputs.verification_stage) {
        return Ok(());
    }
    log_literal_fidelity_start(
        telemetry.start_stage,
        execution_id,
        outputs.verification_stage.verification.unsupported_literals.len(),
    );
    let revision_targets = literal_revision_targets(
        &outputs.verification_stage.generation.answer,
        &outputs.verification_stage.verification.unsupported_literals,
    );
    let revision = crate::services::query::agent_loop::run_literal_revision_turn(
        context.state,
        context.execution_context,
        literal_fidelity_revision_request(
            context.execution_context.library_id,
            context.revision_question,
            context.conversation_history,
            &outputs.verification_stage.generation.answer,
            &revision_targets,
            revision_context,
        ),
    )
    .await;
    match revision {
        Ok(revision) => {
            let (accepted, revision_chars) = apply_literal_revision_result(
                context,
                outputs,
                telemetry.boundary_stage,
                revision,
                &[],
            )
            .await?;
            if !accepted {
                log_literal_fidelity_rejected(
                    telemetry.rejected_stage,
                    execution_id,
                    &outputs.verification_stage.generation.answer,
                    revision_chars,
                );
            }
        }
        Err(error) => {
            log_literal_fidelity_failed(telemetry.error_stage, execution_id, error);
        }
    }
    Ok(())
}

/// Restoration targets are the declared identifiers of the question's own
/// subject that the answer dropped entirely. Nothing else earns a second
/// full-answer generation: this stage rewrites the whole answer, so it
/// trades content it keeps for content it restores.
async fn apply_literal_restoration_revision(
    context: &LiteralRevisionAttempt<'_>,
    outputs: &mut LiteralRevisionOutputs<'_>,
    telemetry: LiteralRevisionTelemetry,
    revision_context: &str,
) -> anyhow::Result<()> {
    let execution_id = context.execution_context.query_execution_id;
    let restoration_targets = declared_subject_literal_revision_targets(
        &outputs.verification_stage.generation.answer,
        &outputs.verification_stage.generation.query_ir,
        revision_context,
    );
    if restoration_targets.is_empty() {
        return Ok(());
    }
    log_literal_restoration_start(
        telemetry.restoration_start_stage,
        execution_id,
        restoration_targets.len(),
    );
    let revision = crate::services::query::agent_loop::run_literal_revision_turn(
        context.state,
        context.execution_context,
        crate::services::query::agent_loop::LiteralRevisionRequest::InventoryCoverage {
            library_id: context.execution_context.library_id,
            user_question: context.revision_question,
            conversation_history: context.conversation_history,
            original_answer: &outputs.verification_stage.generation.answer,
            required_literals: &restoration_targets,
            revision_context,
        },
    )
    .await;
    match revision {
        Ok(revision) => {
            let (accepted, revision_chars) = apply_literal_revision_result(
                context,
                outputs,
                telemetry.restoration_boundary_stage,
                revision,
                &restoration_targets,
            )
            .await?;
            if !accepted {
                log_literal_restoration_rejected(
                    telemetry.restoration_rejected_stage,
                    execution_id,
                    &outputs.verification_stage.generation.answer,
                    revision_chars,
                    restoration_targets.len(),
                );
            }
        }
        Err(error) => {
            log_literal_restoration_failed(telemetry.restoration_error_stage, execution_id, error);
        }
    }
    Ok(())
}

async fn apply_literal_revision_result(
    context: &LiteralRevisionAttempt<'_>,
    outputs: &mut LiteralRevisionOutputs<'_>,
    boundary_stage: &'static str,
    revision: crate::services::query::agent_loop::AgentTurnResult,
    required_literals: &[String],
) -> anyhow::Result<(bool, usize)> {
    let crate::services::query::agent_loop::AgentTurnResult {
        answer,
        usage_json: revision_usage,
        provider_calls: revision_provider_calls,
        assistant_grounding: revision_grounding,
        debug_iterations: revision_debug_iterations,
        ..
    } = revision;
    let revision_chars = answer.chars().count();
    outputs.provider_calls.extend(revision_provider_calls);
    outputs.debug_iterations.extend(revision_debug_iterations);
    let usage_json = merge_generation_usage(
        outputs.verification_stage.generation.usage_json.clone(),
        &revision_usage,
    );
    let revision_answer = enforce_hard_output_boundary(
        context.execution_context.query_execution_id,
        boundary_stage,
        &outputs.verification_stage.generation.query_ir,
        answer,
    );
    let accepted = literal_revision_can_replace_answer(
        &outputs.verification_stage.generation.answer,
        &revision_answer,
    ) && required_literals.iter().all(|literal| revision_answer.contains(literal));
    if !accepted {
        return Ok((false, revision_chars));
    }

    let mut generation = outputs.verification_stage.generation.clone();
    generation.assistant_grounding =
        context.path.revised_grounding(context.prepared, revision_grounding);
    generation.answer = revision_answer;
    generation.usage_json = usage_json;
    *outputs.verification_stage = verify_generated_answer(
        context.execution_context.query_execution_id,
        context.verification_question,
        generation,
    )
    .await?;
    Ok((true, revision_chars))
}

pub(crate) fn contains_label_mention(haystack: &str, label: &str) -> bool {
    let label = label.trim();
    if haystack.is_empty() || label.is_empty() {
        return false;
    }
    let haystack_lower = haystack.to_lowercase();
    let label_lower = label.to_lowercase();
    haystack_lower.match_indices(&label_lower).any(|(start, matched)| {
        let end = start + matched.len();
        let before = haystack_lower[..start].chars().next_back();
        let after = haystack_lower[end..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

pub(super) async fn hydrate_runtime_document_index(
    state: &AppState,
    document_index: &mut HashMap<Uuid, KnowledgeDocumentRow>,
    chunks: &[RuntimeMatchedChunk],
) -> anyhow::Result<()> {
    let mut missing_document_ids = chunks
        .iter()
        .map(|chunk| chunk.document_id)
        .filter(|document_id| !document_index.contains_key(document_id))
        .collect::<Vec<_>>();
    missing_document_ids.sort_unstable();
    missing_document_ids.dedup();
    if missing_document_ids.is_empty() {
        return Ok(());
    }
    let documents = state
        .document_store
        .list_documents_by_ids(&missing_document_ids)
        .await
        .context("failed to hydrate runtime query companion documents")?;
    for document in documents {
        document_index.insert(document.document_id, document);
    }
    Ok(())
}

pub(super) async fn finalize_verified_answer(
    state: &AppState,
    execution_id: Uuid,
    mut verification_stage: AnswerVerificationStage,
) -> anyhow::Result<AnswerVerificationStage> {
    let verification_level = verification_stage.generation.query_ir.verification_level();
    let verification_state = verification_stage.verification.state;
    let finalized = super::super::verification_policy::finalize_answer_visibility(
        verification_level,
        verification_state,
        &verification_stage.verification.warnings,
        verification_stage.generation.query_ir.language,
        &verification_stage.generation.answer,
        super::super::verification_policy::AnswerVisibilityKind::FactualCandidate,
    );
    let disposition = finalized.disposition;
    let visible_answer = finalized.visible_answer.into_owned();
    if visible_answer != verification_stage.generation.answer {
        tracing::warn!(
            %execution_id,
            ?verification_level,
            ?verification_state,
            ?disposition,
            warnings = verification_stage.verification.warnings.len(),
            "strict verification suppressed a non-verified public answer"
        );
        verification_stage.generation.answer = visible_answer;
    }
    super::super::persist_query_verification(QueryVerificationPersistence {
        state,
        execution_id,
        verification: &verification_stage.verification,
        answer_disposition: disposition,
        clarification: &QueryClarification::default(),
        authoritative_evidence: &verification_stage.generation.authoritative_evidence,
        assistant_grounding: &verification_stage.generation.assistant_grounding,
        answer_kind: None,
    })
    .await?;

    Ok(verification_stage)
}

pub(super) fn structural_literal_focus_keyword_is_eligible(keyword: &str) -> bool {
    let char_count = keyword.chars().count();
    char_count >= 4
        || ((2..=3).contains(&char_count)
            && keyword.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
            && keyword.chars().any(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit()))
}

pub(super) fn extract_ranked_structural_literal_candidates(
    text: &str,
    focus_keywords: &[String],
    seen: &mut HashSet<String>,
) -> Vec<String> {
    let focus_compounds = focus_keyword_compounds(focus_keywords);
    let mut candidates = structural_question_tokens(text)
        .into_iter()
        .map(|token| trim_structural_literal_token(&token).to_string())
        .filter(|token| ranked_structural_literal_token_is_eligible(token))
        .filter_map(|token| {
            let score =
                ranked_structural_literal_focus_score(&token, focus_keywords, &focus_compounds);
            (score > 0).then_some((score, token))
        })
        .filter(|(_, token)| seen.insert(token.to_lowercase()))
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| {
                ranked_structural_literal_shape_rank(&right.1)
                    .cmp(&ranked_structural_literal_shape_rank(&left.1))
            })
            .then_with(|| left.1.chars().count().cmp(&right.1.chars().count()))
            .then_with(|| left.1.cmp(&right.1))
    });
    let Some(best_score) = candidates.first().map(|(score, _)| *score) else {
        return Vec::new();
    };
    candidates
        .into_iter()
        .filter(|(score, _)| *score + 10 >= best_score)
        .map(|(_, token)| token)
        .take(4)
        .collect()
}

pub(super) fn focus_keyword_compounds(focus_keywords: &[String]) -> HashSet<String> {
    let mut compounds = HashSet::new();
    for window in focus_keywords.windows(2) {
        let compound = window.iter().map(String::as_str).collect::<String>();
        if compound.chars().count() >= 6 {
            compounds.insert(compound);
        }
    }
    for window in focus_keywords.windows(3) {
        let compound = window.iter().map(String::as_str).collect::<String>();
        if compound.chars().count() >= 8 {
            compounds.insert(compound);
        }
    }
    compounds
}

pub(super) fn ranked_structural_literal_token_is_eligible(token: &str) -> bool {
    let token = token.trim();
    let char_count = token.chars().count();
    if !(2..=96).contains(&char_count) || !token.chars().any(char::is_alphanumeric) {
        return false;
    }
    let lowered = token.to_lowercase();
    if [".py", ".rs", ".go", ".md", ".txt", ".yaml", ".yml", ".json", ".tf"]
        .iter()
        .any(|suffix| lowered.ends_with(suffix))
    {
        return false;
    }
    ranked_structural_literal_has_formal_shape(token)
        || token.chars().any(char::is_numeric)
        || token.chars().all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
        || (token.chars().take_while(|ch| ch.is_ascii_uppercase()).count() >= 2
            && token.chars().any(|ch| ch.is_ascii_lowercase()))
        || token_has_internal_uppercase(token)
}

pub(super) fn ranked_structural_literal_has_formal_shape(token: &str) -> bool {
    if token.chars().any(|ch| matches!(ch, '_' | '/' | ':' | '@')) {
        return true;
    }
    if token.contains('-') {
        let alphabetic = token.chars().filter(|ch| ch.is_alphabetic()).collect::<Vec<_>>();
        return token.chars().any(char::is_numeric)
            || (!alphabetic.is_empty() && alphabetic.iter().all(|ch| ch.is_uppercase()));
    }
    if !token.contains('.') || token.starts_with('.') || token.ends_with('.') {
        return false;
    }
    token.split('.').filter(|part| part.chars().any(char::is_alphanumeric)).count() >= 2
}

pub(super) fn ranked_structural_literal_shape_rank(token: &str) -> usize {
    let has_alphanumeric = token.chars().any(char::is_alphanumeric);
    if !has_alphanumeric {
        return 0;
    }
    let has_separator = token.chars().any(|ch| matches!(ch, '_' | '-' | '/' | ':' | '@' | '.'));
    let has_lowercase = token.chars().any(char::is_lowercase);
    let has_uppercase = token.chars().any(char::is_uppercase);
    let all_caps_or_digits = has_uppercase
        && !has_lowercase
        && token.chars().all(|ch| !ch.is_alphabetic() || ch.is_uppercase());
    usize::from(has_separator) * 4
        + usize::from(all_caps_or_digits) * 3
        + usize::from(token.chars().any(char::is_numeric))
}

pub(super) fn token_has_internal_uppercase(token: &str) -> bool {
    if !token.chars().all(char::is_alphanumeric) {
        return false;
    }
    let mut saw_lowercase = false;
    for ch in token.chars() {
        if ch.is_lowercase() {
            saw_lowercase = true;
        } else if saw_lowercase && ch.is_uppercase() {
            return true;
        }
    }
    false
}

pub(super) fn ranked_structural_literal_focus_score(
    token: &str,
    focus_keywords: &[String],
    focus_compounds: &HashSet<String>,
) -> usize {
    let lowered = token.to_lowercase();
    let compact = lowered.chars().filter(|ch| ch.is_alphanumeric()).collect::<String>();
    let subtokens = split_identifier_subtokens(token);
    let mut best = 0usize;
    if focus_compounds.contains(&compact) {
        best = best.max(90);
    }
    for keyword in focus_keywords {
        let keyword = keyword.trim();
        if keyword.is_empty() {
            continue;
        }
        if keyword == lowered.as_str() || keyword == compact.as_str() {
            best = best.max(100);
        } else if structural_literal_focus_prefix_match(token, keyword, &lowered, &compact) {
            best = best.max(95);
        } else if keyword.chars().count() < 4
            && lowered.starts_with(keyword)
            && token.chars().any(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
        {
            best = best.max(80);
        } else if keyword.chars().count() >= 4 && lowered.contains(keyword) {
            best = best.max(50);
        } else if subtokens.iter().any(|part| part == keyword) {
            best = best.max(35);
        } else if subtokens.iter().any(|part| focus_keyword_matches_literal_part(keyword, part)) {
            best = best.max(32);
        }
    }
    best
}

pub(super) fn focus_keyword_matches_literal_part(keyword: &str, literal_part: &str) -> bool {
    let keyword = keyword.trim();
    let literal_part = literal_part.trim();
    if keyword.chars().count() < 4 || literal_part.chars().count() < 4 {
        return false;
    }
    crate::services::query::text_match::related_prefix_token_match(keyword, literal_part)
        || crate::services::query::text_match::related_prefix_token_match(literal_part, keyword)
}

pub(super) fn structural_literal_focus_prefix_match(
    token: &str,
    keyword: &str,
    lowered_token: &str,
    compact_token: &str,
) -> bool {
    let keyword = keyword.trim().to_lowercase();
    if keyword.chars().count() < 4 || !ranked_structural_literal_has_formal_shape(&keyword) {
        return false;
    }
    let keyword_compact = keyword.chars().filter(|ch| ch.is_alphanumeric()).collect::<String>();
    if keyword_compact.chars().count() < 6 {
        return false;
    }
    lowered_token.starts_with(&format!("{keyword}_"))
        || lowered_token.starts_with(&format!("{keyword}-"))
        || lowered_token.starts_with(&format!("{keyword}."))
        || lowered_token.starts_with(&format!("{keyword}/"))
        || compact_token.starts_with(&keyword_compact)
            && split_identifier_subtokens(token)
                .iter()
                .any(|part| keyword.split(|ch: char| !ch.is_alphanumeric()).any(|kw| kw == part))
}

fn log_literal_fidelity_start(
    stage: &'static str,
    execution_id: Uuid,
    unsupported_literals: usize,
) {
    tracing::info!(
        stage,
        %execution_id,
        unsupported_literals,
        "answer needs literal-fidelity revision over its grounded evidence"
    );
}

fn log_literal_fidelity_rejected(
    stage: &'static str,
    execution_id: Uuid,
    draft_answer: &str,
    revision_chars: usize,
) {
    tracing::warn!(
        stage,
        %execution_id,
        draft_chars = draft_answer.chars().count(),
        revision_chars,
        "literal-fidelity revision did not preserve the answer shape"
    );
}

fn log_literal_fidelity_failed<E: std::fmt::Debug>(
    stage: &'static str,
    execution_id: Uuid,
    error: E,
) {
    tracing::warn!(stage, %execution_id, ?error, "literal-fidelity revision failed");
}

fn log_literal_restoration_start(
    stage: &'static str,
    execution_id: Uuid,
    restoration_targets: usize,
) {
    tracing::info!(
        stage,
        %execution_id,
        restoration_targets,
        "answer needs evidence-backed literal restoration"
    );
}

fn log_literal_restoration_rejected(
    stage: &'static str,
    execution_id: Uuid,
    draft_answer: &str,
    revision_chars: usize,
    restoration_targets: usize,
) {
    tracing::warn!(
        stage,
        %execution_id,
        draft_chars = draft_answer.chars().count(),
        revision_chars,
        restoration_targets,
        "literal revision did not preserve every required evidence literal"
    );
}

fn log_literal_restoration_failed<E: std::fmt::Debug>(
    stage: &'static str,
    execution_id: Uuid,
    error: E,
) {
    tracing::warn!(stage, %execution_id, ?error, "evidence-backed literal restoration failed");
}
