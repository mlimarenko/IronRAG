use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use anyhow::Context as _;
use uuid::Uuid;

use crate::services::query::latest_versions::latest_version_scope_terms;
use crate::{
    app::state::AppState,
    domains::{
        query::{QueryVerificationState, QueryVerificationWarning},
        query_ir::{
            EntityMention, EntityRole, LiteralKind, LiteralSpan, QueryAct, QueryIR, QueryLanguage,
            QueryScope, SourceSliceDirection, SourceSliceFilter,
        },
    },
    infra::knowledge_rows::KnowledgeDocumentRow,
    integrations::llm::ChatMessage,
    interfaces::http::router_support::ApiError,
    services::query::{
        assistant_grounding::AssistantGroundingEvidence,
        compiler::{CompileHistoryTurn, CompileQueryCommand, QueryCompilerService},
        latest_versions::query_requests_latest_versions,
    },
};

use super::answer_kind::AnswerKind;
use super::question_intent::query_ir_has_focused_document_answer_intent;
use super::technical_literals::{
    TechnicalLiteralIntent, detect_explicit_technical_literal_intent_from_query_ir,
    detect_technical_literal_intent_from_query_ir, extract_config_assignment_literals,
    extract_config_section_literals, extract_explicit_path_literals, extract_http_methods,
    extract_package_command_literals, extract_parameter_literals, extract_prefix_literals,
    extract_url_literals, technical_literal_focus_keywords,
};
use super::tuning::{
    CLARIFY_DOMINANCE_RATIO, CLARIFY_MAX_VARIANTS, CLARIFY_MIN_DISTINCT_DOCUMENTS,
    RELEASE_CLARIFY_ENTITY_MIN_DOC_SPAN, RELEASE_CLARIFY_MIN_ENTITIES,
    SINGLE_SHOT_CONFIDENT_ANSWER_CHARS, SINGLE_SHOT_MIN_ANSWER_CHARS,
    SINGLE_SHOT_RETRIEVAL_ESCALATION_MIN_DOCUMENTS,
};
use super::types::{
    RuntimeAnswerCandidate, RuntimeAnswerCandidateProvenance, RuntimeClarification,
};
use super::{
    AnswerGenerationStage, AnswerVerificationStage, FocusReason, PreparedAnswerQueryResult,
    QueryChunkReferenceSnapshot, QueryCompileUsage, RuntimeAnswerQueryResult, RuntimeMatchedChunk,
    RuntimeMatchedEntity, apply_query_execution_library_summary, apply_query_execution_warning,
    assemble_answer_context, load_query_execution_library_context,
    render_targeted_evidence_chunk_section, should_prioritize_retrieved_context_for_query,
    verify_answer_against_canonical_evidence,
};

/// `kind` for a clarify candidate whose only evidence is a label string
/// (a document title or grouped-reference label) with no graph node id.
const ANSWER_CANDIDATE_KIND_DOCUMENT: &str = "document";

fn append_missing_grounded_requested_labels(
    answer: String,
    query_ir: &QueryIR,
    question: &str,
    answer_context: &str,
    graph_entity_references: &[RuntimeMatchedEntity],
) -> String {
    let target_labels = query_ir
        .target_entities
        .iter()
        .map(|entity| entity.label.trim())
        .filter(|label| !label.is_empty() && contains_label_mention(answer_context, label));
    let explicit_grounded_graph_labels =
        graph_entity_references.iter().map(|entity| entity.label.trim()).filter(|label| {
            !label.is_empty()
                && contains_label_mention(question, label)
                && contains_label_mention(answer_context, label)
        });
    let mut seen = HashSet::new();
    let missing = target_labels
        .chain(explicit_grounded_graph_labels)
        .filter(|label| {
            !contains_label_mention(&answer, label) && seen.insert(label.to_lowercase())
        })
        .map(str::to_string)
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return answer;
    }
    let suffix = format!("{}.", missing.join("; "));
    if answer.trim().is_empty() { suffix } else { format!("{}\n\n{}", answer.trim_end(), suffix) }
}

fn contains_label_mention(haystack: &str, label: &str) -> bool {
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

async fn hydrate_runtime_document_index(
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

fn append_missing_grounded_requested_labels_for_prepared(
    answer: String,
    prepared: &PreparedAnswerQueryResult,
    question: &str,
    answer_context: &str,
) -> String {
    let grounded_context = prepared_postprocessor_grounding_context(prepared, answer_context);
    let answer = append_missing_grounded_requested_labels(
        answer,
        &prepared.query_ir,
        question,
        &grounded_context,
        &prepared.structured.graph_entity_references,
    );
    append_missing_focus_aligned_exact_literals(
        answer,
        question,
        &prepared.query_ir,
        &grounded_context,
    )
}

async fn finalize_verified_answer_for_prepared(
    state: &AppState,
    execution_id: Uuid,
    effective_question: &str,
    verification_stage: AnswerVerificationStage,
    prepared: &PreparedAnswerQueryResult,
    question: &str,
    answer_context: &str,
) -> anyhow::Result<AnswerVerificationStage> {
    let finalized_answer = append_missing_grounded_requested_labels_for_prepared(
        verification_stage.generation.answer.clone(),
        prepared,
        question,
        answer_context,
    );
    if finalized_answer == verification_stage.generation.answer {
        return Ok(verification_stage);
    }
    let mut generation = verification_stage.generation;
    generation.answer = finalized_answer;
    verify_generated_answer(state, execution_id, effective_question, generation).await
}

fn prepared_postprocessor_grounding_context(
    prepared: &PreparedAnswerQueryResult,
    answer_context: &str,
) -> String {
    let mut context = String::new();
    let mut seen = HashSet::new();
    push_postprocessor_context_fragment(&mut context, &mut seen, answer_context);
    if let Some(text) = prepared.structured.technical_literals_text.as_deref() {
        push_postprocessor_context_fragment(&mut context, &mut seen, text);
    }
    for line in &prepared.structured.graph_evidence_context_lines {
        push_postprocessor_context_fragment(&mut context, &mut seen, line);
    }
    for chunk in selected_runtime_answer_chunks(prepared) {
        push_postprocessor_context_fragment(&mut context, &mut seen, &chunk.source_text);
        push_postprocessor_context_fragment(&mut context, &mut seen, &chunk.excerpt);
    }
    context
}

fn push_postprocessor_context_fragment(
    context: &mut String,
    seen: &mut HashSet<String>,
    fragment: &str,
) {
    let trimmed = fragment.trim();
    if trimmed.is_empty() || !seen.insert(trimmed.to_string()) {
        return;
    }
    if !context.is_empty() {
        context.push_str("\n\n");
    }
    context.push_str(trimmed);
}

fn append_missing_focus_aligned_exact_literals(
    answer: String,
    question: &str,
    query_ir: &QueryIR,
    answer_context: &str,
) -> String {
    let mut focus_keywords = exact_literal_postprocessor_focus_keywords(question, query_ir);
    if focus_keywords.is_empty() || answer_context.trim().is_empty() {
        return answer;
    }
    extend_focus_keywords_from_answer(&mut focus_keywords, &answer);
    let allow_path_like_literals = question.contains('/')
        || answer.contains('/')
        || query_ir.literal_constraints.iter().any(|literal| literal.text.contains('/'));
    let mut seen = HashSet::new();
    let missing = extract_focus_aligned_answer_suffix_literals(
        answer_context,
        &focus_keywords.iter().map(|keyword| keyword.trim().to_lowercase()).collect::<Vec<_>>(),
        &mut seen,
    )
    .into_iter()
    .filter(|literal| allow_path_like_literals || !answer_suffix_literal_has_path_shape(literal))
    .filter(|literal| !contains_label_mention(&answer, literal))
    .take(4)
    .collect::<Vec<_>>();
    if missing.is_empty() {
        return answer;
    }
    let suffix = format!(
        "Grounded exact terms: {}.",
        missing.into_iter().map(|literal| format!("`{literal}`")).collect::<Vec<_>>().join(", ")
    );
    if answer.trim().is_empty() { suffix } else { format!("{}\n\n{}", answer.trim_end(), suffix) }
}

fn exact_literal_postprocessor_focus_keywords(question: &str, query_ir: &QueryIR) -> Vec<String> {
    let mut seen = HashSet::new();
    let text_keywords = [question.trim(), query_ir.effective_retrieval_query(question).trim()]
        .into_iter()
        .filter(|text| !text.is_empty())
        .flat_map(|text| technical_literal_focus_keywords(text, Some(query_ir)));
    let probe_keywords = [question.trim(), query_ir.effective_retrieval_query(question).trim()]
        .into_iter()
        .filter(|text| !text.is_empty())
        .flat_map(structural_question_tokens);
    text_keywords
        .chain(probe_keywords)
        .filter(|keyword| exact_literal_postprocessor_focus_keyword_is_eligible(keyword))
        .filter(|keyword| seen.insert(keyword.to_lowercase()))
        .collect()
}

fn exact_literal_postprocessor_focus_keyword_is_eligible(keyword: &str) -> bool {
    let char_count = keyword.chars().count();
    char_count >= 4
        || ((2..=3).contains(&char_count)
            && keyword.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
            && keyword.chars().any(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit()))
}

fn extract_focus_aligned_answer_suffix_literals(
    text: &str,
    focus_keywords: &[String],
    seen: &mut HashSet<String>,
) -> Vec<String> {
    let focus_compounds = focus_keyword_compounds(focus_keywords);
    let mut candidates = structural_question_tokens(text)
        .into_iter()
        .map(|token| trim_structural_literal_token(&token).to_string())
        .filter(|token| answer_suffix_literal_token_is_eligible(token))
        .filter_map(|token| {
            let score = answer_suffix_literal_focus_score(&token, focus_keywords, &focus_compounds);
            (score > 0).then_some((score, token))
        })
        .filter(|(_, token)| seen.insert(token.to_lowercase()))
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| {
                answer_suffix_literal_shape_rank(&right.1)
                    .cmp(&answer_suffix_literal_shape_rank(&left.1))
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

fn focus_keyword_compounds(focus_keywords: &[String]) -> HashSet<String> {
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

fn answer_suffix_literal_token_is_eligible(token: &str) -> bool {
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
    answer_suffix_literal_has_structural_shape(token)
        || token.chars().any(char::is_numeric)
        || token.chars().all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
        || token_has_internal_uppercase(token)
}

fn answer_suffix_literal_has_structural_shape(token: &str) -> bool {
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

fn answer_suffix_literal_has_path_shape(token: &str) -> bool {
    token.contains('/')
}

fn answer_suffix_literal_shape_rank(token: &str) -> usize {
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

fn token_has_internal_uppercase(token: &str) -> bool {
    token.chars().all(char::is_alphanumeric) && token.chars().skip(1).any(char::is_uppercase)
}

fn extend_focus_keywords_from_answer(focus_keywords: &mut Vec<String>, answer: &str) {
    let mut seen =
        focus_keywords.iter().map(|keyword| keyword.to_lowercase()).collect::<HashSet<_>>();
    for token in structural_question_tokens(answer)
        .into_iter()
        .map(|token| trim_answer_focus_literal_token(&token))
        .filter(|token| answer_suffix_literal_token_is_eligible(token))
    {
        let lowered = token.to_lowercase();
        if seen.insert(lowered.clone()) {
            focus_keywords.push(lowered);
        }
        let compact =
            token.chars().filter(|ch| ch.is_alphanumeric()).collect::<String>().to_lowercase();
        if compact.chars().count() >= 4 && seen.insert(compact.clone()) {
            focus_keywords.push(compact);
        }
    }
}

fn trim_answer_focus_literal_token(token: &str) -> String {
    let trimmed = trim_structural_literal_token(token);
    if let Some(stripped) = trimmed.strip_suffix('.')
        && token_has_internal_uppercase(stripped)
    {
        return stripped.to_string();
    }
    trimmed.to_string()
}

fn answer_suffix_literal_focus_score(
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
        } else if answer_suffix_structural_focus_prefix_match(token, keyword, &lowered, &compact) {
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

fn focus_keyword_matches_literal_part(keyword: &str, literal_part: &str) -> bool {
    let keyword = keyword.trim();
    let literal_part = literal_part.trim();
    if keyword.chars().count() < 4 || literal_part.chars().count() < 4 {
        return false;
    }
    crate::services::query::text_match::related_prefix_token_match(keyword, literal_part)
        || crate::services::query::text_match::related_prefix_token_match(literal_part, keyword)
}

fn answer_suffix_structural_focus_prefix_match(
    token: &str,
    keyword: &str,
    lowered_token: &str,
    compact_token: &str,
) -> bool {
    let keyword = keyword.trim().to_lowercase();
    if keyword.chars().count() < 4 || !answer_suffix_literal_has_structural_shape(&keyword) {
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

/// Build a [`RuntimeClarification`] for the disposition-router clarify
/// branches. Those branches only have human-readable label strings (document
/// titles / graph node labels / grouped-reference labels), so each candidate
/// is `kind = "document"` with no provenance id and no confidence. This is a
/// serialization of the `variants` the branch already computed — no new
/// retrieval.
fn disposition_clarification(question: &str, variants: &[String]) -> RuntimeClarification {
    RuntimeClarification {
        required: true,
        question: Some(question.to_string()),
        answer_candidates: variants
            .iter()
            .map(|label| RuntimeAnswerCandidate {
                label: label.clone(),
                kind: ANSWER_CANDIDATE_KIND_DOCUMENT.to_string(),
                confidence: None,
                provenance: RuntimeAnswerCandidateProvenance::default(),
            })
            .collect(),
    }
}

fn structural_direct_answer_candidates(
    ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> RuntimeClarification {
    if !matches!(ir.act, QueryAct::ConfigureHow | QueryAct::Describe | QueryAct::RetrieveValue) {
        return RuntimeClarification::default();
    }
    let mut by_document = BTreeMap::<Uuid, RuntimeAnswerCandidate>::new();
    for chunk in chunks {
        if !super::retrieve::chunk_is_setup_focus_command_path_anchor(chunk) {
            continue;
        }
        by_document.entry(chunk.document_id).or_insert_with(|| RuntimeAnswerCandidate {
            label: chunk.document_label.clone(),
            kind: ANSWER_CANDIDATE_KIND_DOCUMENT.to_string(),
            confidence: chunk.score.map(f64::from),
            provenance: RuntimeAnswerCandidateProvenance {
                entity_id: None,
                document_id: Some(chunk.document_id),
                chunk_id: Some(chunk.chunk_id),
            },
        });
    }
    if by_document.len() < 2 {
        return RuntimeClarification::default();
    }
    let mut answer_candidates = by_document.into_values().collect::<Vec<_>>();
    answer_candidates.sort_by(|left, right| left.label.cmp(&right.label));
    answer_candidates.truncate(CLARIFY_MAX_VARIANTS);
    RuntimeClarification { required: false, question: None, answer_candidates }
}

fn structural_direct_answer_candidates_for_prepared(
    prepared: &PreparedAnswerQueryResult,
) -> RuntimeClarification {
    structural_direct_answer_candidates(&prepared.query_ir, &prepared.structured.context_chunks)
}

/// Build a [`RuntimeClarification`] for the release-inventory clarify branch.
/// Each candidate carries the graph `node_type` as its typed `kind` and the
/// `node_id` as its entity provenance handle, so an agent caller can route a
/// follow-up tool call against the exact subject without parsing prose.
fn release_clarification(
    question: &str,
    entities: &[ReleaseEvidenceEntity],
) -> RuntimeClarification {
    RuntimeClarification {
        required: true,
        question: Some(question.to_string()),
        answer_candidates: entities
            .iter()
            .map(|entity| RuntimeAnswerCandidate {
                label: entity.label.clone(),
                kind: entity.node_type.clone(),
                confidence: None,
                provenance: RuntimeAnswerCandidateProvenance {
                    entity_id: Some(entity.node_id),
                    document_id: None,
                    chunk_id: None,
                },
            })
            .collect(),
    }
}

const COMPARE_OPERAND_PROBE_LIMIT: usize = 8;
const COMPARE_OPERAND_PROBE_MAX_CHUNKS: usize = 6;
const COMPARE_OPERAND_PROBE_MAX_CHUNKS_PER_OPERAND: usize = 2;
const TECHNICAL_FOCUS_PROBE_TERM_LIMIT: usize = 8;
const TECHNICAL_FOCUS_PROBE_HIT_LIMIT: usize = 10;
const TECHNICAL_FOCUS_PROBE_MAX_CHUNKS: usize = 8;
const TECHNICAL_FOCUS_PROBE_MAX_CHUNKS_PER_TERM: usize = 2;
const STRUCTURAL_COVERAGE_MIN_CONTEXT_ANCHORS: usize = 4;
const STRUCTURAL_COVERAGE_MIN_CONTEXT_ANCHOR_LINES: usize = 2;
const STRUCTURAL_COVERAGE_MIN_ANSWER_ANCHORS: usize = 2;
const STRUCTURAL_COVERAGE_MAX_ANCHORS: usize = 64;
const STRUCTURAL_COVERAGE_MAX_ANCHOR_CHARS: usize = 80;
const STRUCTURAL_COVERAGE_FOCUS_MIN_TOKEN_CHARS: usize = 3;
const PROVIDER_FREE_FALLBACK_ENTITY_LIMIT: usize = 8;
const PROVIDER_FREE_FALLBACK_LITERAL_LIMIT: usize = 8;
const PROVIDER_FREE_FALLBACK_TOKEN_MAX_CHARS: usize = 80;

struct CanonicalAnswerCandidate {
    verification_stage: AnswerVerificationStage,
    debug_iterations: Vec<crate::services::query::llm_context_debug::LlmIterationDebug>,
    total_iterations: usize,
}

async fn persist_llm_context_snapshot(
    state: &AppState,
    snapshot: crate::services::query::llm_context_debug::LlmContextSnapshot,
) -> anyhow::Result<()> {
    crate::services::query::llm_context_debug::upsert_snapshot(
        &state.persistence.postgres,
        &snapshot,
    )
    .await
    .with_context(|| format!("failed to persist LLM context snapshot {}", snapshot.execution_id))
}

pub(crate) async fn prepare_answer_query(
    state: &AppState,
    library_id: Uuid,
    question: String,
    conversation_history: Option<&str>,
    mode: crate::domains::query::RuntimeQueryMode,
    top_k: usize,
    include_debug: bool,
) -> anyhow::Result<PreparedAnswerQueryResult> {
    // Capture fine-grained timed spans (DB queries, retrieval lanes) recorded
    // during preparation so the debug inspector can show where time went. The
    // sink propagates across the same-task parallelism used below.
    let (result, spans) =
        crate::services::query::turn_spans::capture_turn_spans(prepare_answer_query_inner(
            state,
            library_id,
            question,
            conversation_history,
            mode,
            top_k,
            include_debug,
        ))
        .await;
    let mut prepared = result?;
    prepared.retrieval_spans = spans;
    Ok(prepared)
}

async fn prepare_answer_query_inner(
    state: &AppState,
    library_id: Uuid,
    question: String,
    conversation_history: Option<&str>,
    mode: crate::domains::query::RuntimeQueryMode,
    top_k: usize,
    include_debug: bool,
) -> anyhow::Result<PreparedAnswerQueryResult> {
    // Stage 1: compile + planning run in parallel, then retrieval waits
    // for the compiled IR. This keeps the expensive planning/embedding
    // work overlapped while still letting retrieval consume
    // `document_focus`, scope, and subject entities on the first pass.
    let stage_1_started = std::time::Instant::now();
    let compile_future = compile_query_ir(state, library_id, &question, conversation_history);
    let plan_started = std::time::Instant::now();
    let planning_future = crate::agent_runtime::pipeline::try_op::run_async_try_op((), |_| {
        super::plan_structured_query(state, library_id, &question, mode, top_k)
    });
    let (compile_result, planning_result) = tokio::join!(compile_future, planning_future);
    let plan_elapsed_ms = plan_started.elapsed().as_millis();
    let (mut query_ir, query_compile_usage) = compile_result?;
    let mut planning_stage = planning_result?;
    tracing::info!(
        stage = "answer.plan_done",
        library_id = %library_id,
        elapsed_ms = plan_elapsed_ms,
        planned_mode = ?planning_stage.plan.planned_mode,
        "query compile+plan parallel step done"
    );
    // For elliptic follow-ups the compiler folds the recovered subject/scope
    // into `retrieval_query`; when that resolved string differs from the raw
    // question we re-derive the question-dependent plan/embeddings so the
    // retrieval lanes search the standalone query instead of the bare
    // fragment. The expensive question-independent graph/document indexes are
    // reused, and the common path (resolved == raw) is byte-identical.
    let retrieval_question = guarded_followup_retrieval_question(
        query_ir.effective_retrieval_query(&question),
        &question,
    )
    .to_string();
    if retrieval_question != question {
        tracing::info!(
            stage = "answer.resolved_retrieval_query",
            library_id = %library_id,
            "compiler emitted a resolved standalone retrieval query for an elliptic follow-up"
        );
        super::replan_for_resolved_retrieval_query(
            state,
            library_id,
            &mut planning_stage,
            &retrieval_question,
            mode,
            top_k,
            &query_ir,
        )
        .await?;
    } else {
        super::refresh_query_plan_for_compiled_ir(
            library_id,
            &mut planning_stage,
            &retrieval_question,
            mode,
            top_k,
            &query_ir,
            state.retrieval_intelligence.rerank_enabled,
            state.retrieval_intelligence.rerank_candidate_limit,
        )?;
    }
    let query_ir_for_retrieval = query_ir.clone();
    let retrieve_started = std::time::Instant::now();
    let retrieval_stage = crate::agent_runtime::pipeline::try_op::run_async_try_op(
        planning_stage,
        |planning_stage| {
            let query_ir = query_ir_for_retrieval.clone();
            let question = retrieval_question.clone();
            async move {
                super::retrieve_structured_query(
                    state,
                    library_id,
                    &question,
                    planning_stage,
                    Some(&query_ir),
                )
                .await
            }
        },
    )
    .await?;
    tracing::info!(
        stage = "answer.retrieve_done",
        library_id = %library_id,
        elapsed_ms = retrieve_started.elapsed().as_millis(),
        "structured retrieval done"
    );
    let rerank_question = question.clone();
    let rerank_started = std::time::Instant::now();
    let mut rerank_stage = crate::agent_runtime::pipeline::try_op::run_async_try_op(
        retrieval_stage,
        |retrieval_stage| {
            let question = rerank_question.clone();
            async move { super::rerank_structured_query(state, &question, retrieval_stage).await }
        },
    )
    .await?;
    tracing::info!(
        stage = "answer.rerank_done",
        library_id = %library_id,
        elapsed_ms = rerank_started.elapsed().as_millis(),
        "rerank done"
    );
    let stage_1_elapsed_ms = stage_1_started.elapsed().as_millis();
    let mut document_index = rerank_stage.retrieval.planning.document_index.clone();
    if let Some(inferred_query_ir) = super::infer_latest_version_query_ir_from_retrieved_evidence(
        &query_ir,
        &question,
        &rerank_stage.retrieval.bundle.chunks,
        &document_index,
    ) {
        tracing::info!(
            stage = "answer.query_ir_evidence_repaired",
            library_id = %library_id,
            inferred_act = ?inferred_query_ir.act,
            "repaired low-confidence fallback QueryIR from retrieved source evidence"
        );
        query_ir = inferred_query_ir;
    }
    // IR-aware consolidation: if the compiler pinned the question to
    // one document (explicit hint / single-doc subject) or the
    // retrieval itself shows one document dominating the evidence,
    // reallocate the top_k slot budget to pack contiguous neighbours
    // of that winner instead of keeping 7 tangentials + 1 winning intro.
    let consolidation_started = std::time::Instant::now();
    let consolidation = super::focused_document_consolidation(
        state,
        &mut rerank_stage.retrieval.bundle,
        &query_ir,
        &question,
        top_k,
    )
    .await;
    let consolidation_elapsed_ms = consolidation_started.elapsed().as_millis();
    let plan_keywords = rerank_stage.retrieval.planning.plan.keywords.clone();
    let stale_after_consolidation = super::retain_canonical_document_head_chunks(
        &mut rerank_stage.retrieval.bundle.chunks,
        &document_index,
    );
    if stale_after_consolidation > 0 {
        tracing::info!(
            stage = "retrieval.canonical_head_filter",
            library_id = %library_id,
            stale_chunk_count = stale_after_consolidation,
            "removed non-head revision chunks after focused-document consolidation"
        );
    }
    let source_context_started = std::time::Instant::now();
    let source_context = super::augment_structured_source_context(
        state,
        library_id,
        &question,
        Some(&query_ir),
        &document_index,
        &plan_keywords,
        &rerank_stage.retrieval.graph_evidence_source_document_ids,
        &mut rerank_stage.retrieval.bundle.chunks,
    )
    .await?;
    tracing::info!(
        stage = "answer.source_context_done",
        library_id = %library_id,
        elapsed_ms = source_context_started.elapsed().as_millis(),
        "augment_structured_source_context complete"
    );
    hydrate_runtime_document_index(
        state,
        &mut document_index,
        &rerank_stage.retrieval.bundle.chunks,
    )
    .await?;
    // Temporal hard-filter on the bundle AFTER source-context augmentation.
    // The companion paths (focused-match, source profile, neighbor expansion,
    // library source profile) bypass the retrieval temporal filter and pull
    // chunks regardless of `occurred_at`. When the user explicitly scoped
    // the question to a date range, drop any chunk whose underlying
    // `KnowledgeChunkRow.occurred_at` is null OR falls outside the bounds.
    // Verified necessary on stage 2026-05-03: image-OCR chunks (no
    // occurred_at) were leaking into "messages in March 2026" answers via
    // the prepared-segment / source-context path. Single knowledge-store read
    // via `list_chunks_by_ids`; no per-chunk lookup.
    let (bundle_temporal_start, bundle_temporal_end) = query_ir.resolved_temporal_bounds();
    if bundle_temporal_start.is_some()
        && bundle_temporal_end.is_some()
        && !rerank_stage.retrieval.bundle.chunks.is_empty()
    {
        let chunk_ids: Vec<uuid::Uuid> =
            rerank_stage.retrieval.bundle.chunks.iter().map(|c| c.chunk_id).collect();
        let rows = state.document_store.list_chunks_by_ids(&chunk_ids).await.map_err(|error| {
            anyhow::anyhow!("failed to look up chunks for bundle-temporal post-filter: {error}")
        })?;
        let allowed: std::collections::HashSet<uuid::Uuid> = rows
            .into_iter()
            .filter(|row| {
                let Some(at) = row.occurred_at else {
                    return false;
                };
                if let Some(start) = bundle_temporal_start
                    && row.occurred_until.unwrap_or(at) < start
                {
                    return false;
                }
                if let Some(end) = bundle_temporal_end
                    && at >= end
                {
                    return false;
                }
                true
            })
            .map(|row| row.chunk_id)
            .collect();
        let before = rerank_stage.retrieval.bundle.chunks.len();
        rerank_stage.retrieval.bundle.chunks.retain(|c| allowed.contains(&c.chunk_id));
        tracing::info!(
            stage = "answer.bundle_temporal_post_filter",
            library_id = %library_id,
            before,
            after = rerank_stage.retrieval.bundle.chunks.len(),
            "applied temporal hard-filter to bundle (post source-context)"
        );
    }
    if source_context.source_profile_count > 0
        || source_context.neighbor_count > 0
        || source_context.focused_match_count > 0
        || source_context.procedural_structured_sibling_count > 0
        || source_context.source_slice_count > 0
    {
        tracing::info!(
            stage = "retrieval.structured_source_context",
            library_id = %library_id,
            eligible_document_count = source_context.eligible_document_count,
            source_profile_count = source_context.source_profile_count,
            neighbor_count = source_context.neighbor_count,
            focused_match_count = source_context.focused_match_count,
            procedural_structured_sibling_count = source_context.procedural_structured_sibling_count,
            library_profile_count = source_context.library_profile_count,
            source_slice_count = source_context.source_slice_count,
            "structured source context companions added after consolidation"
        );
    }
    let stale_after_source_context = super::retain_canonical_document_head_chunks(
        &mut rerank_stage.retrieval.bundle.chunks,
        &document_index,
    );
    if stale_after_source_context > 0 {
        tracing::info!(
            stage = "retrieval.canonical_head_filter",
            library_id = %library_id,
            stale_chunk_count = stale_after_source_context,
            "removed non-head revision chunks after structured source context"
        );
    }
    let topical_prune = super::prune_non_topical_document_tail(
        &mut rerank_stage.retrieval.bundle,
        &question,
        Some(&query_ir),
        query_requests_latest_versions(&query_ir),
    );
    if topical_prune.removed_chunk_count > 0 {
        tracing::info!(
            stage = "answer.topical_prune",
            library_id = %library_id,
            removed_chunk_count = topical_prune.removed_chunk_count,
            kept_chunk_count = topical_prune.kept_chunk_count,
            topical_token_count = topical_prune.topical_token_count,
            "pruned non-topical retrieval tail before answer context assembly"
        );
    }

    // Context assembly runs AFTER consolidation so the assembled
    // `context_text` reflects the reshuffled bundle. The winner
    // document_id is threaded in so `load_retrieved_document_briefs`
    // can build the winner preview out of the anchor-window chunks
    // already in the bundle (rather than re-fetching intro chunks
    // that consolidation deliberately demoted).
    let mut structured = super::finalize_structured_query(
        state,
        &question,
        &query_ir,
        rerank_stage,
        include_debug,
        consolidation.focused_document_id,
    )
    .await?;

    // Stage 2: library summary is answer evidence; graph community
    // summaries are intentionally excluded from the final answer prompt
    // because they are broad topology hints, not cited evidence.
    let stage_2_started = std::time::Instant::now();
    let library_context = match load_query_execution_library_context(state, library_id).await {
        Ok(context) => Some(context),
        Err(error) => {
            tracing::warn!(
                error = %error,
                library_id = %library_id,
                "skipping non-critical query library context enrichment"
            );
            None
        }
    };
    let stage_2_elapsed_ms = stage_2_started.elapsed().as_millis();

    apply_query_execution_warning(
        &mut structured.diagnostics,
        library_context.as_ref().and_then(|context| context.warning.as_ref()),
    );
    apply_query_execution_library_summary(&mut structured.diagnostics, library_context.as_ref());
    let mut answer_context = library_context.as_ref().map_or_else(
        || structured.context_text.clone(),
        |context| {
            assemble_answer_context(
                &context.summary,
                &structured.retrieved_documents,
                structured.technical_literals_text.as_deref(),
                &structured.context_text,
                should_prioritize_retrieved_context_for_query(&query_ir, &structured.context_text),
            )
        },
    );
    let compare_probe = augment_partial_compare_context(
        state,
        library_id,
        &query_ir,
        &document_index,
        &plan_keywords,
        &mut answer_context,
        &mut structured,
    )
    .await?;
    if compare_probe.attempted {
        tracing::info!(
            stage = "answer.compare_context_probe",
            library_id = %library_id,
            missing_operand_count = compare_probe.missing_operand_count,
            added_chunk_count = compare_probe.added_chunk_count,
            unresolved_operand_count = compare_probe.unresolved_operand_count,
            "partial compare evidence probe completed"
        );
    }
    let technical_probe = augment_technical_focus_context(
        state,
        library_id,
        &query_ir,
        &question,
        &document_index,
        &plan_keywords,
        &mut answer_context,
        &mut structured,
    )
    .await?;
    if technical_probe.attempted {
        tracing::info!(
            stage = "answer.technical_focus_probe",
            library_id = %library_id,
            probe_term_count = technical_probe.probe_term_count,
            missing_term_count = technical_probe.missing_term_count,
            added_chunk_count = technical_probe.added_chunk_count,
            "technical focus evidence probe completed"
        );
    }

    tracing::info!(
        stage = "answer.prepare",
        library_id = %library_id,
        stage_1_compile_retrieval_ms = stage_1_elapsed_ms,
        stage_2_library_ms = stage_2_elapsed_ms,
        consolidation_ms = consolidation_elapsed_ms,
        consolidation_reason = consolidation.focus_reason.as_str(),
        consolidation_winner_chunks = consolidation.winner_chunk_count,
        consolidation_tangential_chunks = consolidation.tangential_chunk_count,
        topical_pruned_chunks = topical_prune.removed_chunk_count,
        retrieved_document_count = structured.retrieved_documents.len(),
        answer_context_chars = answer_context.chars().count(),
        query_ir_confidence = query_ir.confidence,
        query_ir_act = ?query_ir.act,
        "prepare_answer_query stages"
    );

    let embedding_usage = structured.embedding_usage.clone();
    Ok(PreparedAnswerQueryResult {
        structured,
        answer_context,
        embedding_usage,
        consolidation,
        query_ir,
        query_compile_usage,
        // Filled in by the `prepare_answer_query` wrapper after draining the
        // span sink scoped around this call.
        retrieval_spans: Vec::new(),
    })
}

/// Runs the NL->IR compiler for the current question + conversation history.
/// Provider failures are logged as operator-visible errors, then downgraded to
/// a neutral provider-free IR so the canonical grounded-answer tool can still
/// retrieve and verify evidence instead of forcing parent agents onto weaker
/// ad hoc search/read fallbacks.
pub(crate) async fn compile_query_ir(
    state: &AppState,
    library_id: Uuid,
    question: &str,
    conversation_history: Option<&str>,
) -> Result<(QueryIR, Option<QueryCompileUsage>), ApiError> {
    let started_at = std::time::Instant::now();
    let history = history_turns_from_serialized(conversation_history);
    match QueryCompilerService
        .compile(state, CompileQueryCommand { library_id, question: question.to_string(), history })
        .await
    {
        Ok(outcome) => {
            // Single structured line per compile so operators can
            // filter the log on `query.compile.ir` and see cache hit
            // rate + per-call LLM latency at a glance. `served_from_cache`
            // short-circuits LLM entirely, so elapsed_ms < 10 ms on hits
            // and typically 500–3 000 ms on cache-miss LLM calls.
            tracing::info!(
                %library_id,
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                served_from_cache = outcome.served_from_cache,
                provider_kind = %outcome.provider_kind,
                model_name = %outcome.model_name,
                "query.compile.ir"
            );
            // Capture usage only when the LLM actually ran. Cache hits
            // reuse the `usage_json` of the original call, so billing
            // them here would double-charge repeat questions.
            let billable_usage = (!outcome.served_from_cache).then(|| QueryCompileUsage {
                provider_kind: outcome.provider_kind.clone(),
                model_name: outcome.model_name.clone(),
                usage_json: outcome.usage_json.clone(),
            });
            Ok((guard_self_contained_question_ir(question, outcome.ir), billable_usage))
        }
        Err(error) => {
            tracing::error!(
                %library_id,
                ?error,
                "query compile failed"
            );
            if matches!(error, ApiError::ProviderFailure(_)) {
                let fallback_ir = provider_free_fallback_query_ir(question);
                tracing::error!(
                    %library_id,
                    fallback_act = ?fallback_ir.act,
                    fallback_scope = ?fallback_ir.scope,
                    "query compile provider failed; continuing with provider-free fallback IR"
                );
                Ok((fallback_ir, None))
            } else {
                Err(error)
            }
        }
    }
}

fn guard_self_contained_question_ir(question: &str, ir: QueryIR) -> QueryIR {
    let effective_question = question.trim();
    let current_question =
        crate::services::query::effective_query::current_question_segment(effective_question)
            .trim();
    let ir = guard_ordinary_question_source_slice(current_question, ir);
    let ir = guard_subjectless_source_slice_scope(current_question, ir);
    let ir = guard_configure_how_procedure_document_targets(current_question, ir);
    let ir = guard_current_question_constraint_focus(current_question, ir);
    let Some(retrieval_query) = ir.retrieval_query.as_deref().map(str::trim).map(str::to_string)
    else {
        return ir;
    };
    if retrieval_query.is_empty()
        || retrieval_query == current_question
        || !question_is_self_contained_for_ir_guard(current_question)
        || source_slice_scope_overlaps_current_question(current_question, &ir)
    {
        return ir;
    }

    if retrieval_query_preserves_current_question_focus(current_question, &retrieval_query) {
        if retrieval_query_has_history_only_excess(current_question, &retrieval_query) {
            tracing::warn!(
                stage = "answer.compile_ir_retrieval_query_excess_guard",
                question_len = current_question.chars().count(),
                retrieval_query_len = retrieval_query.chars().count(),
                "compiler preserved current focus but appended stale history-only retrieval terms; using current question retrieval focus"
            );
            return trim_ir_to_current_question_focus(ir, current_question);
        }
        return ir;
    }

    tracing::warn!(
        stage = "answer.compile_ir_current_question_guard",
        question_len = current_question.chars().count(),
        retrieval_query_len = retrieval_query.chars().count(),
        "compiler emitted a history-shaped retrieval query for a self-contained question"
    );
    if ir_has_current_question_supported_structured_focus(current_question, &ir) {
        tracing::warn!(
            stage = "answer.compile_ir_current_question_guard",
            question_len = current_question.chars().count(),
            retrieval_query_len = retrieval_query.chars().count(),
            target_type_count = ir.target_types.len(),
            target_entity_count = ir.target_entities.len(),
            literal_count = ir.literal_constraints.len(),
            "preserving typed current-question IR while trimming stale retrieval focus"
        );
        return trim_ir_to_current_question_focus(ir, current_question);
    }
    provider_free_fallback_query_ir(current_question)
}

fn guard_configure_how_procedure_document_targets(question: &str, mut ir: QueryIR) -> QueryIR {
    if !matches!(ir.act, QueryAct::ConfigureHow)
        || ir.source_slice.is_some()
        || ir.target_types.is_empty()
        || !question_is_self_contained_for_ir_guard(question)
    {
        return ir;
    }

    let mut has_procedure = false;
    let mut has_document_context = false;
    let mut has_artifact_context = false;
    let mut has_setup_configuration_target = false;
    let mut has_concept = false;
    for target_type in &ir.target_types {
        match super::question_intent::canonical_target_type_tag(target_type).as_str() {
            "procedure" => has_procedure = true,
            "document" | "primary_heading" | "secondary_heading" => has_document_context = true,
            "artifact" => has_artifact_context = true,
            "release" | "version" | "changelog" => {}
            "configuration_file" | "config_key" | "parameter" | "package" => {
                has_setup_configuration_target = true;
            }
            "concept" => has_concept = true,
            _ => {}
        }
    }
    if has_concept
        || has_setup_configuration_target
        || (!has_procedure && !has_document_context && !has_artifact_context)
        || (has_procedure && has_document_context)
    {
        return ir;
    }

    tracing::warn!(
        stage = "answer.compile_ir_configure_procedure_document_guard",
        question_len = question.chars().count(),
        target_type_count = ir.target_types.len(),
        "compiler emitted configure/how-to target types without a procedure-document retrieval focus"
    );
    if !has_procedure {
        ir.target_types.push("procedure".to_string());
    }
    if !has_document_context {
        ir.target_types.push("document".to_string());
    }
    if ir.retrieval_query.as_deref().is_none_or(|query| query.trim().is_empty()) {
        ir.retrieval_query = Some(question.trim().to_string());
    }
    ir.conversation_refs.clear();
    ir
}

fn ir_has_current_question_supported_structured_focus(question: &str, ir: &QueryIR) -> bool {
    if ir.confidence < 0.6 || ir.target_types.is_empty() {
        return false;
    }
    let focus_tokens = current_question_focus_token_set_for_ir_guard(question);
    ir.target_entities.iter().any(|entity| {
        surface_is_supported_by_current_question(&entity.label, question, &focus_tokens)
    }) || ir.literal_constraints.iter().any(|literal| {
        surface_is_supported_by_current_question(&literal.text, question, &focus_tokens)
    }) || ir.document_focus.as_ref().is_some_and(|focus| {
        surface_is_supported_by_current_question(&focus.hint, question, &focus_tokens)
    })
}

fn trim_ir_to_current_question_focus(mut ir: QueryIR, current_question: &str) -> QueryIR {
    let focus_tokens = current_question_focus_token_set_for_ir_guard(current_question);
    ir.retrieval_query = Some(current_question.trim().to_string());
    ir.conversation_refs.clear();
    ir.target_entities.retain(|entity| {
        surface_is_supported_by_current_question(&entity.label, current_question, &focus_tokens)
    });
    ir.literal_constraints.retain(|literal| {
        surface_is_supported_by_current_question(&literal.text, current_question, &focus_tokens)
    });
    if ir.document_focus.as_ref().is_some_and(|focus| {
        !surface_is_supported_by_current_question(&focus.hint, current_question, &focus_tokens)
    }) {
        ir.document_focus = None;
    }
    ir
}

fn guard_current_question_constraint_focus(question: &str, mut ir: QueryIR) -> QueryIR {
    let focus_tokens = current_question_focus_token_set_for_ir_guard(question);
    if focus_tokens.is_empty() {
        return ir;
    }

    let selected_entities =
        selected_target_entity_indices_for_current_question(&ir, question, &focus_tokens);
    let has_selected_entities = !selected_entities.is_empty();
    let supported_literal_count = ir
        .literal_constraints
        .iter()
        .filter(|literal| {
            surface_is_supported_by_current_question(&literal.text, question, &focus_tokens)
        })
        .count();
    if !has_selected_entities && supported_literal_count == 0 {
        if question_is_self_contained_for_ir_guard(question)
            && (!ir.target_entities.is_empty()
                || !ir.literal_constraints.is_empty()
                || ir.document_focus.is_some())
        {
            if ir.retrieval_query.as_deref().is_some_and(|retrieval_query| {
                let retrieval_query = retrieval_query.trim();
                !retrieval_query.is_empty()
                    && retrieval_query != question
                    && !retrieval_query_preserves_current_question_focus(question, retrieval_query)
            }) {
                tracing::warn!(
                    stage = "answer.compile_ir_current_constraint_guard",
                    question_len = question.chars().count(),
                    "compiler carried history-shaped constraints and stale retrieval terms into a self-contained current question; rebuilding fresh retrieval IR"
                );
                return provider_free_fallback_query_ir(question);
            }
            let removed_entity_count = ir.target_entities.len();
            let removed_literal_count = ir.literal_constraints.len();
            ir.target_entities.clear();
            ir.literal_constraints.clear();
            ir.document_focus = None;
            ir.retrieval_query = Some(question.trim().to_string());
            ir.conversation_refs.clear();
            tracing::warn!(
                stage = "answer.compile_ir_current_constraint_guard",
                question_len = question.chars().count(),
                removed_entity_count,
                removed_literal_count,
                "compiler carried only history-shaped constraints into a self-contained current question; clearing them before retrieval"
            );
        }
        return ir;
    }

    let original_entity_count = ir.target_entities.len();
    let original_literal_count = ir.literal_constraints.len();
    let original_document_focus = ir.document_focus.clone();

    if has_selected_entities {
        let mut index = 0usize;
        ir.target_entities.retain(|_| {
            let keep = selected_entities.contains(&index);
            index += 1;
            keep
        });
    }
    ir.literal_constraints.retain(|literal| {
        surface_is_supported_by_current_question(&literal.text, question, &focus_tokens)
    });
    if ir.document_focus.as_ref().is_some_and(|focus| {
        !surface_is_supported_by_current_question(&focus.hint, question, &focus_tokens)
    }) {
        ir.document_focus = None;
    }

    let changed = original_entity_count != ir.target_entities.len()
        || original_literal_count != ir.literal_constraints.len()
        || original_document_focus != ir.document_focus;
    if has_selected_entities
        && current_question_is_short_focus_for_ir_guard(question)
        && let Some(retrieval_query) = ir.retrieval_query.as_deref()
        && retrieval_query.trim() != question
        && retrieval_query_has_short_focus_excess(question, retrieval_query)
    {
        ir.retrieval_query = Some(question.trim().to_string());
        ir.conversation_refs.clear();
        tracing::warn!(
            stage = "answer.compile_ir_short_focus_excess_guard",
            question_len = question.chars().count(),
            "compiler preserved a short current-question entity but appended history-only retrieval terms; using current question retrieval focus"
        );
        return ir;
    }
    if let Some(retrieval_query) = ir.retrieval_query.as_deref()
        && retrieval_query.trim() != question
        && retrieval_query_has_unsupported_technical_excess_for_current_focus(
            question,
            &ir,
            retrieval_query,
        )
    {
        ir.retrieval_query = Some(question.trim().to_string());
        ir.conversation_refs.clear();
        tracing::warn!(
            stage = "answer.compile_ir_retrieval_query_technical_excess_guard",
            question_len = question.chars().count(),
            "compiler preserved current entity but appended stale technical retrieval terms; using current question retrieval focus"
        );
        return ir;
    }
    if !changed {
        return ir;
    }

    if question_is_self_contained_for_ir_guard(question) {
        ir.retrieval_query = Some(question.trim().to_string());
        ir.conversation_refs.clear();
    }
    tracing::warn!(
        stage = "answer.compile_ir_current_constraint_guard",
        question_len = question.chars().count(),
        removed_entity_count = original_entity_count.saturating_sub(ir.target_entities.len()),
        removed_literal_count = original_literal_count.saturating_sub(ir.literal_constraints.len()),
        "compiler carried stale entity or literal constraints into the current question focus; pruning them before retrieval"
    );
    ir
}

fn guard_ordinary_question_source_slice(question: &str, mut ir: QueryIR) -> QueryIR {
    if ir.source_slice.is_none() || question_allows_source_slice(question, &ir) {
        return ir;
    }
    tracing::warn!(
        stage = "answer.compile_ir_source_slice_guard",
        question_len = question.chars().count(),
        "compiler emitted source_slice for an ordinary question; clearing ordered source-slice mode"
    );
    ir.source_slice = None;
    ir
}

fn question_allows_source_slice(_question: &str, ir: &QueryIR) -> bool {
    let Some(slice) = ir.source_slice.as_ref() else {
        return false;
    };
    let has_release_marker = matches!(slice.filter, SourceSliceFilter::ReleaseMarker)
        && ir.target_types.iter().any(|target_type| {
            matches!(
                super::question_intent::canonical_target_type_tag(target_type).as_str(),
                "release" | "version" | "changelog"
            )
        });
    has_release_marker
        && (matches!(slice.direction, SourceSliceDirection::Tail)
            || slice.count.is_some()
            || query_requests_latest_versions(ir))
}

fn guard_subjectless_source_slice_scope(question: &str, ir: QueryIR) -> QueryIR {
    if ir.source_slice.is_none()
        || !question_is_self_contained_for_ir_guard(question)
        || !query_requests_latest_versions(&ir)
    {
        return ir;
    }
    let scoped_terms = latest_version_scope_terms(&ir)
        .into_iter()
        .filter(|term| !term.trim().is_empty())
        .collect::<Vec<_>>();
    if scoped_terms.is_empty() {
        if source_slice_scope_surfaces_overlap_question(question, &ir) {
            return ir;
        }
        let has_scope_surface = !ir.target_entities.is_empty()
            || ir.document_focus.is_some()
            || ir.literal_constraints.iter().any(|literal| {
                !matches!(literal.kind, LiteralKind::Version | LiteralKind::NumericCode)
            });
        if has_scope_surface {
            return clear_stale_source_slice_scope(question, ir);
        }
        return ir;
    }
    if source_slice_scope_terms_overlap_question(question, &scoped_terms) {
        return ir;
    }
    clear_stale_source_slice_scope(question, ir)
}

fn clear_stale_source_slice_scope(question: &str, mut ir: QueryIR) -> QueryIR {
    tracing::warn!(
        stage = "answer.compile_ir_source_slice_scope_guard",
        question_len = question.chars().count(),
        "compiler scoped a self-contained source-slice inventory to history-only terms; clearing stale scope"
    );
    ir.target_entities.clear();
    ir.literal_constraints.clear();
    ir.document_focus = None;
    ir.conversation_refs.clear();
    ir.retrieval_query = Some(question.trim().to_string());
    ir
}

fn source_slice_scope_overlaps_current_question(question: &str, ir: &QueryIR) -> bool {
    if ir.source_slice.is_none() || !query_requests_latest_versions(ir) {
        return false;
    }
    let scoped_terms = latest_version_scope_terms(ir)
        .into_iter()
        .filter(|term| !term.trim().is_empty())
        .collect::<Vec<_>>();
    !scoped_terms.is_empty() && source_slice_scope_terms_overlap_question(question, &scoped_terms)
}

fn source_slice_scope_surfaces_overlap_question(question: &str, ir: &QueryIR) -> bool {
    let focus_tokens = current_question_focus_token_set_for_ir_guard(question);
    ir.target_entities.iter().any(|entity| {
        surface_is_supported_by_current_question(&entity.label, question, &focus_tokens)
    }) || ir.document_focus.as_ref().is_some_and(|focus| {
        surface_is_supported_by_current_question(&focus.hint, question, &focus_tokens)
    }) || ir.literal_constraints.iter().any(|literal| {
        !matches!(literal.kind, LiteralKind::Version | LiteralKind::NumericCode)
            && surface_is_supported_by_current_question(&literal.text, question, &focus_tokens)
    })
}

fn source_slice_scope_terms_overlap_question(question: &str, scoped_terms: &[String]) -> bool {
    let current_tokens = crate::shared::text_tokens::normalized_alnum_tokens(question, 3);
    scoped_terms.iter().any(|term| {
        let term_tokens = crate::shared::text_tokens::normalized_alnum_tokens(term, 3);
        !term_tokens.is_empty()
            && term_tokens.iter().any(|token| current_tokens.iter().any(|current| current == token))
    })
}

fn question_is_self_contained_for_ir_guard(question: &str) -> bool {
    let focus_tokens = current_question_focus_tokens_for_ir_guard(question);
    let high_signal_count = focus_tokens
        .iter()
        .filter(|token| {
            token.chars().any(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
                || token.chars().any(|ch| matches!(ch, '_' | '-' | '.' | '/' | ':'))
        })
        .count();
    high_signal_count > 0 || focus_tokens.len() >= 2
}

fn retrieval_query_preserves_current_question_focus(question: &str, retrieval_query: &str) -> bool {
    if looks_like_internal_effective_query_block(retrieval_query) {
        return false;
    }
    let focus_tokens = current_question_focus_tokens_for_ir_guard(question);
    if focus_tokens.is_empty() {
        return true;
    }
    let retrieval_tokens = crate::shared::text_tokens::normalized_alnum_tokens(retrieval_query, 3);
    let high_signal_tokens =
        focus_tokens.iter().filter(|token| token.chars().count() >= 7).collect::<Vec<_>>();
    if high_signal_tokens.len() >= 2
        && high_signal_tokens
            .iter()
            .filter(|token| retrieval_tokens.iter().any(|candidate| candidate == **token))
            .count()
            < high_signal_tokens.len().saturating_sub(1)
    {
        return false;
    }
    let preserved = focus_tokens
        .iter()
        .filter(|token| retrieval_tokens.iter().any(|candidate| candidate == *token))
        .count();
    preserved >= focus_tokens.len().min(5)
}

fn retrieval_query_has_history_only_excess(question: &str, retrieval_query: &str) -> bool {
    if looks_like_internal_effective_query_block(retrieval_query) {
        return true;
    }
    let focus_tokens = current_question_focus_tokens_for_ir_guard(question);
    if focus_tokens.len() < 2 {
        return false;
    }
    let focus_token_set = focus_tokens.iter().cloned().collect::<BTreeSet<_>>();
    let retrieval_tokens = crate::shared::text_tokens::normalized_alnum_tokens(retrieval_query, 3)
        .into_iter()
        .filter(|token| !ir_guard_stopword(token))
        .collect::<Vec<_>>();
    let preserved = focus_token_set
        .iter()
        .filter(|token| retrieval_tokens.iter().any(|candidate| candidate == *token))
        .count();
    if preserved < focus_tokens.len().min(5) {
        return false;
    }
    let extra_tokens = retrieval_tokens
        .iter()
        .filter(|token| !focus_token_set.contains(*token))
        .cloned()
        .collect::<BTreeSet<_>>();
    let minimum_extra = if focus_tokens.len() >= 4 { 2 } else { 3 };
    let minimum_length = focus_tokens.len().saturating_add(minimum_extra + 1);
    extra_tokens.len() >= minimum_extra && retrieval_tokens.len() >= minimum_length
}

fn retrieval_query_has_short_focus_excess(question: &str, retrieval_query: &str) -> bool {
    let focus_tokens = current_question_focus_tokens_for_ir_guard(question);
    if focus_tokens.is_empty() || focus_tokens.len() > 2 {
        return false;
    }
    let focus_token_set = focus_tokens.iter().cloned().collect::<BTreeSet<_>>();
    let retrieval_tokens = crate::shared::text_tokens::normalized_alnum_tokens(retrieval_query, 3)
        .into_iter()
        .filter(|token| !ir_guard_stopword(token))
        .collect::<Vec<_>>();
    let preserved = focus_token_set
        .iter()
        .filter(|token| retrieval_tokens.iter().any(|candidate| candidate == *token))
        .count();
    if preserved == 0 {
        return false;
    }
    let extra_tokens = retrieval_tokens
        .iter()
        .filter(|token| !focus_token_set.contains(*token))
        .cloned()
        .collect::<BTreeSet<_>>();
    extra_tokens.len() >= 2 && retrieval_tokens.len() >= focus_tokens.len().saturating_add(2)
}

fn retrieval_query_has_unsupported_technical_excess_for_current_focus(
    question: &str,
    ir: &QueryIR,
    retrieval_query: &str,
) -> bool {
    let mut supported_tokens = current_question_focus_token_set_for_ir_guard(question);
    for literal in &ir.literal_constraints {
        supported_tokens.extend(
            crate::shared::text_tokens::normalized_alnum_tokens(&literal.text, 2)
                .into_iter()
                .filter(|token| !ir_guard_stopword(token)),
        );
    }
    if supported_tokens.is_empty() {
        return false;
    }

    retrieval_query.split_whitespace().any(|raw_token| {
        let token = clean_retrieval_query_surface_token(raw_token);
        if token.is_empty() || !retrieval_query_surface_token_is_technical(&token) {
            return false;
        }
        let token_parts = crate::shared::text_tokens::normalized_alnum_tokens(&token, 2);
        !token_parts.is_empty()
            && !token_parts.iter().any(|part| {
                supported_tokens
                    .iter()
                    .any(|supported| near_token_match_for_ir_guard(part, supported))
            })
    })
}

fn clean_retrieval_query_surface_token(token: &str) -> String {
    token
        .trim_matches(|ch: char| {
            ch.is_ascii_punctuation() && !matches!(ch, '/' | '\\' | '.' | '-' | '_' | ':' | '=')
        })
        .to_string()
}

fn retrieval_query_surface_token_is_technical(token: &str) -> bool {
    let has_alnum = token.chars().any(|ch| ch.is_alphanumeric());
    if !has_alnum {
        return false;
    }
    token.contains('/')
        || token.contains('\\')
        || token.contains("://")
        || token.contains('=')
        || token.contains('_')
        || token.contains('-')
        || (token.contains('.') && token.chars().any(|ch| ch.is_ascii_digit()))
        || (token.chars().any(|ch| ch.is_ascii_digit())
            && token.chars().any(|ch| ch.is_alphabetic())
            && token.chars().count() >= 5)
}

fn current_question_focus_token_set_for_ir_guard(question: &str) -> BTreeSet<String> {
    current_question_focus_tokens_for_ir_guard(question).into_iter().collect()
}

fn current_question_is_short_focus_for_ir_guard(question: &str) -> bool {
    let tokens = current_question_focus_tokens_for_ir_guard(question);
    !tokens.is_empty() && tokens.len() <= 2
}

fn selected_target_entity_indices_for_current_question(
    ir: &QueryIR,
    current_question: &str,
    focus_tokens: &BTreeSet<String>,
) -> BTreeSet<usize> {
    let strict = ir
        .target_entities
        .iter()
        .enumerate()
        .filter(|(_, entity)| {
            surface_is_supported_by_current_question(&entity.label, current_question, focus_tokens)
        })
        .map(|(index, _)| index)
        .collect::<BTreeSet<_>>();
    if !strict.is_empty() {
        return strict;
    }

    let loose = ir
        .target_entities
        .iter()
        .enumerate()
        .filter(|(_, entity)| label_has_any_current_question_overlap(&entity.label, focus_tokens))
        .map(|(index, _)| index)
        .collect::<BTreeSet<_>>();
    if loose.len() == 1 { loose } else { BTreeSet::new() }
}

fn surface_is_supported_by_current_question(
    surface: &str,
    current_question: &str,
    focus_tokens: &BTreeSet<String>,
) -> bool {
    let surface = surface.trim();
    if surface.is_empty() {
        return false;
    }
    if current_question.to_lowercase().contains(&surface.to_lowercase()) {
        return true;
    }
    label_is_supported_by_current_question(surface, focus_tokens)
}

fn label_has_any_current_question_overlap(label: &str, focus_tokens: &BTreeSet<String>) -> bool {
    let label_tokens = crate::shared::text_tokens::normalized_alnum_tokens(label, 2)
        .into_iter()
        .filter(|token| !ir_guard_stopword(token))
        .collect::<Vec<_>>();
    !label_tokens.is_empty()
        && label_tokens.iter().any(|token| {
            focus_tokens.iter().any(|focus| near_token_match_for_ir_guard(token, focus))
        })
}

fn label_is_supported_by_current_question(label: &str, focus_tokens: &BTreeSet<String>) -> bool {
    if focus_tokens.is_empty() {
        return true;
    }
    let label_tokens = crate::shared::text_tokens::normalized_alnum_tokens(label, 2)
        .into_iter()
        .filter(|token| !ir_guard_stopword(token))
        .collect::<BTreeSet<_>>();
    if label_tokens.is_empty() {
        return false;
    }
    let overlap = label_tokens
        .iter()
        .filter(|token| {
            focus_tokens.iter().any(|focus| near_token_match_for_ir_guard(token, focus))
        })
        .count();
    if label_tokens.len() <= 1 {
        overlap > 0
    } else {
        overlap >= 2 || overlap == label_tokens.len()
    }
}

fn near_token_match_for_ir_guard(left: &str, right: &str) -> bool {
    left == right
        || (left.chars().count() >= 5
            && right.chars().count() >= 5
            && common_prefix_len_for_ir_guard(left, right) >= 4)
}

fn common_prefix_len_for_ir_guard(left: &str, right: &str) -> usize {
    left.chars().zip(right.chars()).take_while(|(left, right)| left == right).count()
}

fn current_question_focus_tokens_for_ir_guard(question: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    crate::shared::text_tokens::normalized_alnum_tokens(question, 3)
        .into_iter()
        .filter(|token| !ir_guard_stopword(token))
        .filter(|token| seen.insert(token.clone()))
        .collect()
}

fn ir_guard_stopword(token: &str) -> bool {
    matches!(
        token,
        "what"
            | "and"
            | "the"
            | "are"
            | "for"
            | "have"
            | "which"
            | "where"
            | "when"
            | "who"
            | "why"
            | "how"
            | "about"
            | "does"
            | "with"
            | "from"
            | "that"
            | "this"
            | "they"
            | "their"
            | "them"
            | "each"
            | "uses"
            | "used"
            | "into"
            | "across"
            | "between"
            | "compare"
            | "implement"
            | "implements"
            | "require"
            | "requires"
            | "required"
            | "valid"
            | "defined"
            | "directly"
    )
}

fn provider_free_fallback_query_ir(question: &str) -> QueryIR {
    let retrieval_query = question.trim().to_string();
    QueryIR {
        act: QueryAct::Describe,
        scope: QueryScope::MultiDocument,
        language: QueryLanguage::Auto,
        target_types: Vec::new(),
        target_entities: provider_free_fallback_entity_mentions(question),
        literal_constraints: provider_free_fallback_literal_constraints(question),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: (!retrieval_query.is_empty()).then_some(retrieval_query),
        confidence: 0.25,
    }
}

fn provider_free_fallback_entity_mentions(question: &str) -> Vec<EntityMention> {
    let mut mentions = Vec::new();
    let mut seen = HashSet::new();
    let tokens = structural_question_tokens(question);
    let mut current = Vec::new();
    for (index, token) in tokens.into_iter().enumerate() {
        if token_has_fallback_entity_signal(&token, index) {
            current.push(token);
        } else {
            push_provider_free_fallback_entity(&mut mentions, &mut seen, &mut current);
        }
        if mentions.len() >= PROVIDER_FREE_FALLBACK_ENTITY_LIMIT {
            break;
        }
    }
    push_provider_free_fallback_entity(&mut mentions, &mut seen, &mut current);
    mentions
}

fn push_provider_free_fallback_entity(
    mentions: &mut Vec<EntityMention>,
    seen: &mut HashSet<String>,
    current: &mut Vec<String>,
) {
    if mentions.len() >= PROVIDER_FREE_FALLBACK_ENTITY_LIMIT || current.is_empty() {
        current.clear();
        return;
    }
    let label =
        current.join(" ").chars().take(PROVIDER_FREE_FALLBACK_TOKEN_MAX_CHARS).collect::<String>();
    current.clear();
    let label = label.trim();
    if label.chars().count() < 2 || !label.chars().any(char::is_alphanumeric) {
        return;
    }
    let key = label.to_lowercase();
    if seen.insert(key) {
        mentions.push(EntityMention { label: label.to_string(), role: EntityRole::Subject });
    }
}

fn provider_free_fallback_literal_constraints(question: &str) -> Vec<LiteralSpan> {
    let mut literals = Vec::new();
    let mut seen = HashSet::new();
    for token in structural_question_tokens(question) {
        if literals.len() >= PROVIDER_FREE_FALLBACK_LITERAL_LIMIT {
            break;
        }
        let kind = LiteralKind::infer(&token);
        if !provider_free_literal_kind_is_structural(kind, &token) {
            continue;
        }
        let key = token.to_lowercase();
        if seen.insert(key) {
            literals.push(LiteralSpan { text: token, kind });
        }
    }
    literals
}

fn provider_free_literal_kind_is_structural(kind: LiteralKind, text: &str) -> bool {
    matches!(kind, LiteralKind::Url | LiteralKind::Path | LiteralKind::Version)
        || (matches!(kind, LiteralKind::Identifier | LiteralKind::NumericCode)
            && text.chars().any(|ch| matches!(ch, '_' | '-' | '.' | '/' | ':') || ch.is_numeric()))
}

fn structural_question_tokens(question: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in question.chars() {
        if ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':') {
            current.push(ch);
        } else if !current.is_empty() {
            tokens.push(take_provider_free_fallback_token(&current));
            current.clear();
        }
    }
    if !current.is_empty() {
        tokens.push(take_provider_free_fallback_token(&current));
    }
    tokens
}

fn take_provider_free_fallback_token(value: &str) -> String {
    value.chars().take(PROVIDER_FREE_FALLBACK_TOKEN_MAX_CHARS).collect()
}

fn token_has_fallback_entity_signal(token: &str, index: usize) -> bool {
    if token.chars().count() < 2 {
        return false;
    }
    if index == 0 && token_is_plain_titlecase(token) {
        return false;
    }
    token.chars().any(char::is_uppercase)
        || token.chars().any(char::is_numeric)
        || token.chars().any(|ch| matches!(ch, '_' | '-' | '.' | '/' | ':'))
}

fn token_is_plain_titlecase(token: &str) -> bool {
    let mut alphabetic = token.chars().filter(|ch| ch.is_alphabetic());
    let Some(first) = alphabetic.next() else {
        return false;
    };
    first.is_uppercase()
        && alphabetic.all(|ch| ch.is_lowercase())
        && token.chars().all(char::is_alphabetic)
}

/// `conversation_history` arrives pre-serialized as a plain multi-line string
/// (`"role: content\nrole: content"`). Split it back into per-turn entries
/// so the compiler can reason about each turn individually; bad lines are
/// passed through as user content so the compiler still has context.
fn history_turns_from_serialized(history: Option<&str>) -> Vec<CompileHistoryTurn> {
    let Some(raw) = history else {
        return Vec::new();
    };
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            if let Some((role, content)) = line.split_once(':') {
                CompileHistoryTurn {
                    role: role.trim().to_string(),
                    content: content.trim().to_string(),
                }
            } else {
                CompileHistoryTurn { role: "user".to_string(), content: line.trim().to_string() }
            }
        })
        .collect()
}

pub(crate) async fn generate_answer_query(
    state: &AppState,
    library_id: Uuid,
    execution_id: Uuid,
    effective_question: &str,
    user_question: &str,
    conversation_history: Option<&str>,
    conversation_history_messages: &[ChatMessage],
    prepared: PreparedAnswerQueryResult,
) -> anyhow::Result<RuntimeAnswerQueryResult> {
    // Resolves just the QueryAnswer binding (one Postgres lookup)
    // instead of the full `resolve_effective_provider_profile` which
    // sequentially loaded ExtractGraph + EmbedChunk + QueryCompile
    // + QueryAnswer + Vision — five serial round-trips for something
    // the answer path only needs one of. The selection is still
    // threaded into the deterministic-preflight override branch below
    // (`provider: _answer_provider`), so behaviour is identical.
    let _answer_provider = {
        let binding = state
            .canonical_services
            .ai_catalog
            .resolve_active_runtime_binding(
                state,
                library_id,
                crate::domains::ai::AiBindingPurpose::QueryAnswer,
            )
            .await
            .map_err(|e| anyhow::anyhow!("failed to resolve query_answer binding: {e}"))?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "no active query_answer binding configured for library {library_id}"
                )
            })?;
        crate::domains::provider_profiles::ProviderModelSelection {
            provider_kind: binding.provider_kind.clone(),
            model_name: binding.model_name.clone(),
        }
    };

    let answer_question = effective_question.trim();
    let answer_question = if answer_question.is_empty() { user_question } else { answer_question };
    let generation_question = answer_generation_question(effective_question, user_question);

    if let Some(exact_version_answer) = super::build_exact_version_change_summary_answer(
        &prepared.query_ir,
        &prepared.structured.context_chunks,
        &prepared.structured.graph_evidence_context_lines,
    ) {
        tracing::info!(
            stage = "answer.exact_version_deterministic",
            %execution_id,
            %library_id,
            "deterministic exact-version change answer selected"
        );
        let verification_stage = verify_generated_answer(
            state,
            execution_id,
            effective_question,
            AnswerGenerationStage {
                intent_profile: prepared.structured.intent_profile.clone(),
                canonical_answer_chunks: selected_runtime_answer_chunks(&prepared),
                canonical_evidence: super::CanonicalAnswerEvidence {
                    bundle: None,
                    chunk_rows: Vec::new(),
                    structured_blocks: Vec::new(),
                    technical_facts: Vec::new(),
                },
                assistant_grounding: selected_runtime_grounding_evidence(
                    &prepared,
                    AssistantGroundingEvidence::default(),
                ),
                answer: exact_version_answer,
                provider: _answer_provider.clone(),
                usage_json: serde_json::json!({
                    "deterministic": true,
                    "answer_kind": AnswerKind::ExactVersionChangeSummary.as_str(),
                }),
                prompt_context: prepared.answer_context.clone(),
                query_ir: prepared.query_ir.clone(),
            },
        )
        .await?;
        let verification_stage = finalize_verified_answer_for_prepared(
            state,
            execution_id,
            effective_question,
            verification_stage,
            &prepared,
            generation_question,
            &prepared.answer_context,
        )
        .await?;
        let final_answer = verification_stage.generation.answer.clone();
        persist_llm_context_snapshot(
            state,
            crate::services::query::llm_context_debug::LlmContextSnapshot {
                execution_id,
                library_id,
                question: user_question.to_string(),
                total_iterations: 0,
                iterations: Vec::new(),
                final_answer: Some(final_answer.clone()),
                captured_at: chrono::Utc::now(),
                query_ir: Some(
                    serde_json::to_value(&prepared.query_ir).unwrap_or(serde_json::Value::Null),
                ),
                agent_loop: None,
                spans: Vec::new(),
            },
        )
        .await?;
        return Ok(RuntimeAnswerQueryResult {
            answer: final_answer,
            provider: verification_stage.generation.provider,
            usage_json: verification_stage.generation.usage_json,
            clarification: RuntimeClarification::default(),
        });
    }

    if let Some(source_slice_answer) = super::build_ordered_source_slice_answer(
        &prepared.query_ir,
        &prepared.structured.ordered_source_units,
        &prepared.structured.context_chunks,
    ) {
        tracing::info!(
            stage = "answer.source_slice_deterministic",
            %execution_id,
            %library_id,
            source_unit_count = source_slice_answer.unit_count,
            used_context_fallback = source_slice_answer.used_context_fallback,
            "deterministic ordered source-slice answer selected"
        );

        // When the latest-version inventory lane produced this answer for a
        // query that names NO explicit subject (no target_entities, no
        // document_focus), probe the gathered evidence for distinct named
        // entities that each span at least RELEASE_CLARIFY_ENTITY_MIN_DOC_SPAN
        // of the release documents.  Two or more such entities means the
        // release notes cover multiple distinct subjects — offer the user a
        // clarifying question with those subjects as choices, grounded on the
        // already-gathered evidence.  The gate reuses the lane's own
        // predicates (no parallel re-derivation), and no product names or NL
        // keywords are hardcoded here; the entity labels come entirely from
        // the graph built during ingest.
        if subjectless_release_inventory(
            &prepared.query_ir,
            super::answer::context_supports_latest_version_inventory(
                &prepared.query_ir,
                &prepared.structured.context_chunks,
            ),
        ) {
            // Scope the probe to the exact units the deterministic answer
            // was built from (post dominant-family retention + truncation),
            // not the wider retrieval context — otherwise the clarify could
            // offer subjects that the listed releases never mention.
            let chunk_ids: &[Uuid] = &source_slice_answer.unit_chunk_ids;

            if !chunk_ids.is_empty() {
                let release_entities =
                    query_release_evidence_entities(state, library_id, chunk_ids).await;
                match release_entities {
                    Ok(entities) if entities.len() >= RELEASE_CLARIFY_MIN_ENTITIES => {
                        tracing::info!(
                            stage = "answer.release_clarify_start",
                            %execution_id,
                            %library_id,
                            entity_count = entities.len(),
                            "release-lane entity probe found multiple subjects — routing to clarify"
                        );
                        // Bound the entity list before deriving labels for
                        // run_clarify_turn (prose) and candidates (typed).
                        let entities: Vec<ReleaseEvidenceEntity> =
                            entities.into_iter().take(CLARIFY_MAX_VARIANTS).collect();
                        let variants: Vec<String> =
                            entities.iter().map(|e| e.label.clone()).collect();
                        // Ground the clarify turn on the deterministic inventory
                        // itself, and PREPEND that inventory verbatim to the
                        // returned clarification. The full flat list must stay
                        // in the answer: tool callers are often agents (UI agent
                        // loop, external MCP clients) that relay the tool answer
                        // — a menu-only reply makes them re-query with a guessed
                        // subject and degrades the final turn, while the full
                        // list plus a trailing clarification serves both a human
                        // and an agent in one round trip.
                        let clarify_result = crate::services::query::agent_loop::run_clarify_turn(
                            state,
                            library_id,
                            generation_question,
                            conversation_history_messages,
                            &variants,
                            &source_slice_answer.answer,
                        )
                        .await;
                        match clarify_result {
                            Ok(clarify) if !clarify.answer.trim().is_empty() => {
                                let combined_answer = format!(
                                    "{}\n\n{}",
                                    source_slice_answer.answer.trim_end(),
                                    clarify.answer.trim_start()
                                );
                                tracing::info!(
                                    stage = "answer.release_clarify_done",
                                    %execution_id,
                                    answer_len = combined_answer.len(),
                                    "release clarify appended to the deterministic inventory"
                                );
                                let clarification =
                                    release_clarification(&clarify.answer, &entities);
                                let clarify_debug = clarify.debug_iterations.clone();
                                persist_llm_context_snapshot(
                                    state,
                                    crate::services::query::llm_context_debug::LlmContextSnapshot {
                                        execution_id,
                                        library_id,
                                        question: user_question.to_string(),
                                        total_iterations: clarify.iterations,
                                        iterations: clarify_debug,
                                        final_answer: Some(combined_answer.clone()),
                                        captured_at: chrono::Utc::now(),
                                        query_ir: Some(
                                            serde_json::to_value(&prepared.query_ir)
                                                .unwrap_or(serde_json::Value::Null),
                                        ),
                                        agent_loop: None,
                                        spans: Vec::new(),
                                    },
                                )
                                .await?;
                                return Ok(RuntimeAnswerQueryResult {
                                    answer: combined_answer,
                                    provider: clarify.provider,
                                    usage_json: clarify.usage_json,
                                    clarification,
                                });
                            }
                            Ok(_) => {
                                tracing::info!(
                                    stage = "answer.release_clarify_empty",
                                    %execution_id,
                                    "release clarify path returned empty — falling back to source-slice answer"
                                );
                            }
                            Err(error) => {
                                tracing::warn!(
                                    stage = "answer.release_clarify_error",
                                    %execution_id,
                                    ?error,
                                    "release clarify path failed — falling back to source-slice answer"
                                );
                            }
                        }
                    }
                    Ok(entities) => {
                        tracing::debug!(
                            stage = "answer.release_clarify_skip",
                            %execution_id,
                            entity_count = entities.len(),
                            min_required = RELEASE_CLARIFY_MIN_ENTITIES,
                            "release-lane entity probe found too few subjects — skipping clarify"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            stage = "answer.release_clarify_probe_error",
                            %execution_id,
                            ?error,
                            "release-lane entity probe failed — falling back to source-slice answer"
                        );
                    }
                }
            }
        }

        let usage_json = serde_json::json!({
            "deterministic": true,
            "answer_kind": if source_slice_answer.used_context_fallback {
                AnswerKind::OrderedSourceSliceIdentityFallback.as_str()
            } else {
                AnswerKind::OrderedSourceSlice.as_str()
            },
            "source_unit_count": source_slice_answer.unit_count,
            "used_context_fallback": source_slice_answer.used_context_fallback,
        });
        let verification_stage = verify_generated_answer(
            state,
            execution_id,
            effective_question,
            AnswerGenerationStage {
                intent_profile: prepared.structured.intent_profile.clone(),
                canonical_answer_chunks: selected_runtime_answer_chunks(&prepared),
                canonical_evidence: super::CanonicalAnswerEvidence {
                    bundle: None,
                    chunk_rows: Vec::new(),
                    structured_blocks: Vec::new(),
                    technical_facts: Vec::new(),
                },
                assistant_grounding: selected_runtime_grounding_evidence(
                    &prepared,
                    AssistantGroundingEvidence::default(),
                ),
                answer: source_slice_answer.answer,
                provider: _answer_provider.clone(),
                usage_json,
                prompt_context: prepared.answer_context.clone(),
                query_ir: prepared.query_ir.clone(),
            },
        )
        .await?;
        let verification_stage = finalize_verified_answer_for_prepared(
            state,
            execution_id,
            effective_question,
            verification_stage,
            &prepared,
            generation_question,
            &prepared.answer_context,
        )
        .await?;
        let final_answer = verification_stage.generation.answer.clone();
        persist_llm_context_snapshot(
            state,
            crate::services::query::llm_context_debug::LlmContextSnapshot {
                execution_id,
                library_id,
                question: user_question.to_string(),
                total_iterations: 0,
                iterations: Vec::new(),
                final_answer: Some(final_answer.clone()),
                captured_at: chrono::Utc::now(),
                query_ir: Some(
                    serde_json::to_value(&prepared.query_ir).unwrap_or(serde_json::Value::Null),
                ),
                agent_loop: None,
                spans: Vec::new(),
            },
        )
        .await?;
        return Ok(RuntimeAnswerQueryResult {
            answer: final_answer,
            provider: verification_stage.generation.provider,
            usage_json: verification_stage.generation.usage_json,
            clarification: RuntimeClarification::default(),
        });
    }

    let deterministic_setup_answer = if matches!(prepared.query_ir.act, QueryAct::ConfigureHow) {
        super::answer::build_setup_configuration_anchor_candidate(
            generation_question,
            &prepared.query_ir,
            &prepared.structured.context_chunks,
        )
        .filter(|answer| {
            answer.should_use_as_direct_answer(
                &prepared.query_ir,
                &prepared.structured.context_chunks,
            )
        })
        .map(super::answer::SetupConfigurationAnchorCandidate::into_answer)
    } else {
        None
    };

    let update_answer_chunks = selected_runtime_answer_chunks(&prepared);
    if deterministic_setup_answer.is_none()
        && let Some(update_answer) = super::build_update_procedure_sequence_answer(
            generation_question,
            &prepared.query_ir,
            &update_answer_chunks,
        )
    {
        let update_answer = super::augment_deterministic_grounded_answer_with_evidence(
            update_answer,
            generation_question,
            &prepared.query_ir,
            &update_answer_chunks,
        );
        tracing::info!(
            stage = "answer.update_procedure_deterministic",
            %execution_id,
            %library_id,
            "deterministic update-procedure answer selected before single-shot generation"
        );
        let verification_stage = verify_generated_answer(
            state,
            execution_id,
            effective_question,
            AnswerGenerationStage {
                intent_profile: prepared.structured.intent_profile.clone(),
                canonical_answer_chunks: update_answer_chunks,
                canonical_evidence: super::CanonicalAnswerEvidence {
                    bundle: None,
                    chunk_rows: Vec::new(),
                    structured_blocks: Vec::new(),
                    technical_facts: Vec::new(),
                },
                assistant_grounding: selected_runtime_grounding_evidence(
                    &prepared,
                    AssistantGroundingEvidence::default(),
                ),
                answer: update_answer,
                provider: _answer_provider.clone(),
                usage_json: serde_json::json!({
                    "deterministic": true,
                    "answer_kind": AnswerKind::UpdateProcedureSequence.as_str(),
                }),
                prompt_context: prepared.answer_context.clone(),
                query_ir: prepared.query_ir.clone(),
            },
        )
        .await?;
        let verification_stage = finalize_verified_answer_for_prepared(
            state,
            execution_id,
            effective_question,
            verification_stage,
            &prepared,
            generation_question,
            &prepared.answer_context,
        )
        .await?;
        let final_answer = verification_stage.generation.answer.clone();
        persist_llm_context_snapshot(
            state,
            crate::services::query::llm_context_debug::LlmContextSnapshot {
                execution_id,
                library_id,
                question: user_question.to_string(),
                total_iterations: 0,
                iterations: Vec::new(),
                final_answer: Some(final_answer.clone()),
                captured_at: chrono::Utc::now(),
                query_ir: Some(
                    serde_json::to_value(&prepared.query_ir).unwrap_or(serde_json::Value::Null),
                ),
                agent_loop: None,
                spans: Vec::new(),
            },
        )
        .await?;
        return Ok(RuntimeAnswerQueryResult {
            answer: final_answer,
            provider: verification_stage.generation.provider,
            usage_json: verification_stage.generation.usage_json,
            clarification: RuntimeClarification::default(),
        });
    }

    if let Some(setup_answer) = deterministic_setup_answer {
        tracing::info!(
            stage = "answer.setup_configuration_deterministic",
            %execution_id,
            %library_id,
            "deterministic setup-configuration answer selected"
        );
        let answer = append_missing_grounded_requested_labels_for_prepared(
            setup_answer,
            &prepared,
            generation_question,
            &prepared.answer_context,
        );
        let verification_stage = verify_generated_answer(
            state,
            execution_id,
            effective_question,
            AnswerGenerationStage {
                intent_profile: prepared.structured.intent_profile.clone(),
                canonical_answer_chunks: selected_runtime_answer_chunks(&prepared),
                canonical_evidence: super::CanonicalAnswerEvidence {
                    bundle: None,
                    chunk_rows: Vec::new(),
                    structured_blocks: Vec::new(),
                    technical_facts: Vec::new(),
                },
                assistant_grounding: selected_runtime_grounding_evidence(
                    &prepared,
                    AssistantGroundingEvidence::default(),
                ),
                answer,
                provider: _answer_provider.clone(),
                usage_json: serde_json::json!({
                    "deterministic": true,
                    "answer_kind": AnswerKind::SetupConfigurationAnchor.as_str(),
                }),
                prompt_context: prepared.answer_context.clone(),
                query_ir: prepared.query_ir.clone(),
            },
        )
        .await?;
        let verification_stage = finalize_verified_answer_for_prepared(
            state,
            execution_id,
            effective_question,
            verification_stage,
            &prepared,
            generation_question,
            &prepared.answer_context,
        )
        .await?;
        let final_answer = verification_stage.generation.answer.clone();
        persist_llm_context_snapshot(
            state,
            crate::services::query::llm_context_debug::LlmContextSnapshot {
                execution_id,
                library_id,
                question: user_question.to_string(),
                total_iterations: 0,
                iterations: Vec::new(),
                final_answer: Some(final_answer.clone()),
                captured_at: chrono::Utc::now(),
                query_ir: Some(
                    serde_json::to_value(&prepared.query_ir).unwrap_or(serde_json::Value::Null),
                ),
                agent_loop: None,
                spans: Vec::new(),
            },
        )
        .await?;
        return Ok(RuntimeAnswerQueryResult {
            answer: final_answer,
            provider: verification_stage.generation.provider,
            usage_json: verification_stage.generation.usage_json,
            clarification: RuntimeClarification::default(),
        });
    }

    if prepared.query_ir.source_slice.is_none()
        && matches!(
            prepared.query_ir.act,
            QueryAct::Compare | QueryAct::Describe | QueryAct::Enumerate | QueryAct::RetrieveValue
        )
        && let Some(source_unit_answer) =
            super::answer::build_structured_source_unit_inventory_answer(
                generation_question,
                &prepared.query_ir,
                &prepared.structured.context_chunks,
            )
    {
        tracing::info!(
            stage = "answer.structured_source_unit_deterministic",
            %execution_id,
            %library_id,
            "deterministic structured source-unit answer selected after context augmentation"
        );
        let verification_stage = verify_generated_answer(
            state,
            execution_id,
            effective_question,
            AnswerGenerationStage {
                intent_profile: prepared.structured.intent_profile.clone(),
                canonical_answer_chunks: selected_runtime_answer_chunks(&prepared),
                canonical_evidence: super::CanonicalAnswerEvidence {
                    bundle: None,
                    chunk_rows: Vec::new(),
                    structured_blocks: Vec::new(),
                    technical_facts: Vec::new(),
                },
                assistant_grounding: selected_runtime_grounding_evidence(
                    &prepared,
                    AssistantGroundingEvidence::default(),
                ),
                answer: source_unit_answer,
                provider: _answer_provider.clone(),
                usage_json: serde_json::json!({
                    "deterministic": true,
                    "answer_kind": AnswerKind::DeterministicGroundedAnswer.as_str(),
                }),
                prompt_context: prepared.answer_context.clone(),
                query_ir: prepared.query_ir.clone(),
            },
        )
        .await?;
        let verification_stage = finalize_verified_answer_for_prepared(
            state,
            execution_id,
            effective_question,
            verification_stage,
            &prepared,
            generation_question,
            &prepared.answer_context,
        )
        .await?;
        let final_answer = verification_stage.generation.answer.clone();
        persist_llm_context_snapshot(
            state,
            crate::services::query::llm_context_debug::LlmContextSnapshot {
                execution_id,
                library_id,
                question: user_question.to_string(),
                total_iterations: 0,
                iterations: Vec::new(),
                final_answer: Some(final_answer.clone()),
                captured_at: chrono::Utc::now(),
                query_ir: Some(
                    serde_json::to_value(&prepared.query_ir).unwrap_or(serde_json::Value::Null),
                ),
                agent_loop: None,
                spans: Vec::new(),
            },
        )
        .await?;
        return Ok(RuntimeAnswerQueryResult {
            answer: final_answer,
            provider: verification_stage.generation.provider,
            usage_json: verification_stage.generation.usage_json,
            clarification: RuntimeClarification::default(),
        });
    }

    // Single-shot fast path tried FIRST — we no longer pay the
    // ~2–3 s `prepare_canonical_answer_preflight` tax before every
    // question. Preflight loads document_index, canonical evidence,
    // and answer chunks. None of that is needed for the initial
    // grounded-answer LLM call: `prepared.answer_context` already
    // carries the retrieved chunks, technical literals, library
    // summary, and selected graph context. Preflight is now deferred
    // to the escalation path, where the verifier and deterministic
    // `answer_override` logic still use it.
    let should_try_single_shot =
        should_use_single_shot_answer(effective_question, &prepared, conversation_history);
    let mut canonical_candidate: Option<CanonicalAnswerCandidate> = None;
    let mut attempted_answer_generation = false;

    // Post-retrieval disposition router: before answer generation,
    // check whether retrieval returned a *dominant* cluster of
    // evidence or a *multi-modal* spread across several distinct
    // subsystems / variants. In the latter case, returning ONE short
    // clarifying question listing those variants is strictly more
    // useful than a "scattered mentions" summary. See
    // `classify_answer_disposition` for the structural signals — no
    // hardcoded domain vocabulary is involved.
    if let AnswerDisposition::Clarify { variants } =
        classify_answer_disposition(&prepared, answer_question)
    {
        let clarify_start = std::time::Instant::now();
        tracing::info!(
            stage = "answer.clarify_start",
            %execution_id,
            %library_id,
            variant_count = variants.len(),
            query_ir_act = ?prepared.query_ir.act,
            query_ir_confidence = prepared.query_ir.confidence,
            "post-retrieval router chose clarify path"
        );
        let clarify_result = crate::services::query::agent_loop::run_clarify_turn(
            state,
            library_id,
            generation_question,
            conversation_history_messages,
            &variants,
            &prepared.answer_context,
        )
        .await;
        match clarify_result {
            Ok(clarify) => {
                if !clarify.answer.trim().is_empty() {
                    tracing::info!(
                        stage = "answer.clarify_done",
                        %execution_id,
                        answer_len = clarify.answer.len(),
                        elapsed_ms = clarify_start.elapsed().as_millis(),
                        "clarify path returned a question to the user"
                    );
                    let clarify_debug = clarify.debug_iterations.clone();
                    persist_llm_context_snapshot(
                        state,
                        crate::services::query::llm_context_debug::LlmContextSnapshot {
                            execution_id,
                            library_id,
                            question: user_question.to_string(),
                            total_iterations: clarify.iterations,
                            iterations: clarify_debug,
                            final_answer: Some(clarify.answer.clone()),
                            captured_at: chrono::Utc::now(),
                            query_ir: Some(
                                serde_json::to_value(&prepared.query_ir)
                                    .unwrap_or(serde_json::Value::Null),
                            ),
                            agent_loop: None,
                            spans: Vec::new(),
                        },
                    )
                    .await?;
                    let clarification = disposition_clarification(&clarify.answer, &variants);
                    return Ok(RuntimeAnswerQueryResult {
                        answer: clarify.answer,
                        provider: clarify.provider,
                        usage_json: clarify.usage_json,
                        clarification,
                    });
                }
                tracing::info!(
                    stage = "answer.clarify_empty",
                    %execution_id,
                    "clarify path returned empty text — falling back to answer generation"
                );
            }
            Err(error) => {
                tracing::warn!(
                    stage = "answer.clarify_error",
                    %execution_id,
                    ?error,
                    "clarify path failed — falling back to answer generation"
                );
            }
        }
    }

    if should_try_single_shot {
        let single_shot_start = std::time::Instant::now();
        attempted_answer_generation = true;
        tracing::info!(
            stage = "answer.single_shot_start",
            %execution_id,
            %library_id,
            question_len = user_question.len(),
            query_ir_act = ?prepared.query_ir.act,
            query_ir_confidence = prepared.query_ir.confidence,
            retrieved_document_count = prepared.structured.retrieved_documents.len(),
            answer_context_chars = prepared.answer_context.chars().count(),
            "single-shot grounded-answer fast path start"
        );
        let single_shot_result = crate::services::query::agent_loop::run_single_shot_turn(
            state,
            library_id,
            generation_question,
            conversation_history_messages,
            &prepared.answer_context,
        )
        .await;
        match single_shot_result {
            Ok(single) => {
                let single_shot_elapsed_ms = single_shot_start.elapsed().as_millis();
                let single_answer = enforce_hard_output_boundary(
                    execution_id,
                    "answer.single_shot",
                    &prepared.query_ir,
                    single.answer.clone(),
                );
                let single_answer = append_missing_grounded_requested_labels_for_prepared(
                    single_answer,
                    &prepared,
                    generation_question,
                    &prepared.answer_context,
                );
                tracing::info!(
                    stage = "answer.single_shot_done",
                    %execution_id,
                    answer_len = single_answer.len(),
                    elapsed_ms = single_shot_elapsed_ms,
                    "single-shot grounded-answer fast path done"
                );
                let mut single_debug = single.debug_iterations.clone();
                persist_llm_context_snapshot(
                    state,
                    crate::services::query::llm_context_debug::LlmContextSnapshot {
                        execution_id,
                        library_id,
                        question: user_question.to_string(),
                        total_iterations: single.iterations,
                        iterations: single_debug.clone(),
                        final_answer: (!single_answer.is_empty()).then(|| single_answer.clone()),
                        captured_at: chrono::Utc::now(),
                        query_ir: Some(
                            serde_json::to_value(&prepared.query_ir)
                                .unwrap_or(serde_json::Value::Null),
                        ),
                        agent_loop: None,
                        spans: Vec::new(),
                    },
                )
                .await?;
                // Lightweight verify: no canonical evidence is
                // required on the fast path because we have not
                // loaded it. The verifier degrades to the
                // "no canonical chunks, no bundle" case and applies
                // only the QueryIR-driven strictness level, which
                // still suppresses hallucinated literals on strict
                // paths. Non-strict paths pass through as they did
                // before. When the fast path fails this check we
                // retry through canonical preflight over the same
                // retrieved evidence, which pays the full preflight
                // cost and runs the complete verifier.
                let verify_started = std::time::Instant::now();
                let fast_path_chunks = selected_runtime_answer_chunks(&prepared);
                let fast_path_grounding =
                    selected_runtime_grounding_evidence(&prepared, single.assistant_grounding);
                let mut verification_stage = verify_generated_answer(
                    state,
                    execution_id,
                    effective_question,
                    AnswerGenerationStage {
                        intent_profile: prepared.structured.intent_profile.clone(),
                        canonical_answer_chunks: fast_path_chunks.clone(),
                        canonical_evidence: super::CanonicalAnswerEvidence {
                            bundle: None,
                            chunk_rows: Vec::new(),
                            structured_blocks: Vec::new(),
                            technical_facts: Vec::new(),
                        },
                        assistant_grounding: fast_path_grounding.clone(),
                        answer: single_answer,
                        provider: single.provider.clone(),
                        usage_json: single.usage_json.clone(),
                        prompt_context: prepared.answer_context.clone(),
                        query_ir: prepared.query_ir.clone(),
                    },
                )
                .await?;
                if answer_needs_literal_revision(&verification_stage) {
                    tracing::info!(
                        stage = "answer.single_shot_literal_revision_start",
                        %execution_id,
                        unsupported_literals =
                            verification_stage.verification.unsupported_literals.len(),
                        "single-shot answer needs literal-fidelity revision over the same retrieved evidence"
                    );
                    let revision_context =
                        literal_revision_context(&prepared.answer_context, &fast_path_grounding);
                    let revision_targets = literal_revision_targets(
                        &verification_stage.generation.answer,
                        &verification_stage.verification.unsupported_literals,
                    );
                    match crate::services::query::agent_loop::run_literal_fidelity_revision_turn(
                        state,
                        library_id,
                        generation_question,
                        conversation_history_messages,
                        &verification_stage.generation.answer,
                        &revision_targets,
                        &revision_context,
                    )
                    .await
                    {
                        Ok(revision) => {
                            let usage_json = merge_generation_usage(
                                verification_stage.generation.usage_json.clone(),
                                &revision.usage_json,
                            );
                            single_debug.extend(revision.debug_iterations);
                            let revision_answer = enforce_hard_output_boundary(
                                execution_id,
                                "answer.single_shot_literal_revision",
                                &prepared.query_ir,
                                revision.answer.clone(),
                            );
                            if literal_revision_can_replace_answer(
                                &verification_stage.generation.answer,
                                &revision_answer,
                            ) {
                                let revision_answer =
                                    append_missing_grounded_requested_labels_for_prepared(
                                        revision_answer,
                                        &prepared,
                                        generation_question,
                                        &revision_context,
                                    );
                                let revised_stage = verify_generated_answer(
                                    state,
                                    execution_id,
                                    effective_question,
                                    AnswerGenerationStage {
                                        intent_profile: prepared.structured.intent_profile.clone(),
                                        canonical_answer_chunks: fast_path_chunks.clone(),
                                        canonical_evidence: super::CanonicalAnswerEvidence {
                                            bundle: None,
                                            chunk_rows: Vec::new(),
                                            structured_blocks: Vec::new(),
                                            technical_facts: Vec::new(),
                                        },
                                        assistant_grounding: selected_runtime_grounding_evidence(
                                            &prepared,
                                            revision.assistant_grounding,
                                        ),
                                        answer: revision_answer,
                                        provider: revision.provider.clone(),
                                        usage_json,
                                        prompt_context: prepared.answer_context.clone(),
                                        query_ir: prepared.query_ir.clone(),
                                    },
                                )
                                .await?;
                                verification_stage = revised_stage;
                            } else {
                                tracing::warn!(
                                    stage = "answer.single_shot_literal_revision_rejected",
                                    %execution_id,
                                    draft_chars = verification_stage.generation.answer.chars().count(),
                                    revision_chars = revision.answer.chars().count(),
                                    "literal-fidelity revision did not preserve the answer shape"
                                );
                            }
                        }
                        Err(error) => {
                            tracing::warn!(
                                stage = "answer.single_shot_literal_revision_error",
                                %execution_id,
                                ?error,
                                "literal-fidelity revision failed for single-shot answer"
                            );
                        }
                    }
                }
                verification_stage = finalize_verified_answer_for_prepared(
                    state,
                    execution_id,
                    effective_question,
                    verification_stage,
                    &prepared,
                    generation_question,
                    &prepared.answer_context,
                )
                .await?;
                let verify_elapsed_ms = verify_started.elapsed().as_millis();

                if single_shot_answer_is_acceptable(
                    &verification_stage.generation.answer,
                    &verification_stage,
                    prepared.structured.retrieved_documents.len(),
                    &prepared.query_ir,
                    &prepared.answer_context,
                ) {
                    tracing::info!(
                        stage = "answer.single_shot_accepted",
                        %execution_id,
                        verify_elapsed_ms,
                        total_elapsed_ms = single_shot_start.elapsed().as_millis(),
                        "single-shot grounded-answer accepted"
                    );
                    persist_llm_context_snapshot(
                        state,
                        crate::services::query::llm_context_debug::LlmContextSnapshot {
                            execution_id,
                            library_id,
                            question: user_question.to_string(),
                            total_iterations: single.iterations,
                            iterations: single_debug,
                            final_answer: Some(verification_stage.generation.answer.clone()),
                            captured_at: chrono::Utc::now(),
                            query_ir: Some(
                                serde_json::to_value(&prepared.query_ir)
                                    .unwrap_or(serde_json::Value::Null),
                            ),
                            agent_loop: None,
                            spans: Vec::new(),
                        },
                    )
                    .await?;
                    let clarification = structural_direct_answer_candidates_for_prepared(&prepared);
                    return Ok(RuntimeAnswerQueryResult {
                        answer: verification_stage.generation.answer,
                        provider: verification_stage.generation.provider,
                        usage_json: verification_stage.generation.usage_json,
                        clarification,
                    });
                }
                canonical_candidate = Some(CanonicalAnswerCandidate {
                    verification_stage,
                    debug_iterations: single_debug,
                    total_iterations: single.iterations,
                });
                tracing::info!(
                    stage = "answer.single_shot_rejected",
                    %execution_id,
                    "single-shot answer unacceptable — escalating to canonical preflight over the same retrieved evidence"
                );
            }
            Err(error) => {
                tracing::warn!(
                    stage = "answer.single_shot_error",
                    %execution_id,
                    ?error,
                    "single-shot grounded-answer fast path failed — escalating"
                );
            }
        }
    }

    // Canonical preflight path. Pay the preflight cost now: we need
    // `canonical_evidence` and `canonical_answer_chunks` both for the
    // strict verifier and for the deterministic `answer_override`
    // short-circuit (missing-document / unsupported-capability /
    // exact-literal-grounded answer).
    let preflight_started = std::time::Instant::now();
    let preflight = super::prepare_canonical_answer_preflight(
        state,
        library_id,
        execution_id,
        effective_question,
        &prepared,
    )
    .await?;
    let preflight_elapsed_ms = preflight_started.elapsed().as_millis();
    tracing::info!(
        stage = "answer.preflight_done",
        %execution_id,
        preflight_elapsed_ms,
        canonical_chunks = preflight.canonical_answer_chunks.len(),
        has_override = preflight.answer_override.is_some(),
        "canonical-answer preflight loaded (escalation)"
    );
    if let Some(answer_override) = preflight.answer_override.clone() {
        let answer = append_missing_grounded_requested_labels_for_prepared(
            answer_override.answer,
            &prepared,
            generation_question,
            &preflight.prompt_context,
        );
        persist_llm_context_snapshot(
            state,
            crate::services::query::llm_context_debug::LlmContextSnapshot {
                execution_id,
                library_id,
                question: user_question.to_string(),
                total_iterations: 0,
                iterations: Vec::new(),
                final_answer: Some(answer.clone()),
                captured_at: chrono::Utc::now(),
                query_ir: Some(
                    serde_json::to_value(&prepared.query_ir).unwrap_or(serde_json::Value::Null),
                ),
                agent_loop: None,
                spans: Vec::new(),
            },
        )
        .await?;
        let verification_stage = verify_generated_answer(
            state,
            execution_id,
            effective_question,
            AnswerGenerationStage {
                intent_profile: prepared.structured.intent_profile.clone(),
                canonical_answer_chunks: preflight.canonical_answer_chunks,
                canonical_evidence: preflight.canonical_evidence,
                assistant_grounding:
                    crate::services::query::assistant_grounding::AssistantGroundingEvidence::default(),
                answer,
                provider: _answer_provider,
                usage_json: serde_json::json!({
                    "deterministic": true,
                    "reason": "canonical_preflight_answer",
                    "answer_kind": answer_override.answer_kind.as_str(),
                }),
                prompt_context: preflight.prompt_context.clone(),
                query_ir: prepared.query_ir.clone(),
            },
        )
        .await?;
        let verification_stage = finalize_verified_answer_for_prepared(
            state,
            execution_id,
            effective_question,
            verification_stage,
            &prepared,
            generation_question,
            &preflight.prompt_context,
        )
        .await?;
        persist_llm_context_snapshot(
            state,
            crate::services::query::llm_context_debug::LlmContextSnapshot {
                execution_id,
                library_id,
                question: user_question.to_string(),
                total_iterations: 0,
                iterations: Vec::new(),
                final_answer: Some(verification_stage.generation.answer.clone()),
                captured_at: chrono::Utc::now(),
                query_ir: Some(
                    serde_json::to_value(&prepared.query_ir).unwrap_or(serde_json::Value::Null),
                ),
                agent_loop: None,
                spans: Vec::new(),
            },
        )
        .await?;
        let clarification = structural_direct_answer_candidates_for_prepared(&prepared);
        return Ok(RuntimeAnswerQueryResult {
            answer: verification_stage.generation.answer,
            provider: verification_stage.generation.provider,
            usage_json: verification_stage.generation.usage_json,
            clarification,
        });
    }

    let preflight_prepared =
        prepared_with_preflight_context_titles(&prepared, &preflight.canonical_answer_chunks);
    if let AnswerDisposition::Clarify { variants } =
        classify_answer_disposition(&preflight_prepared, answer_question)
    {
        let clarify_start = std::time::Instant::now();
        tracing::info!(
            stage = "answer.preflight_clarify_start",
            %execution_id,
            %library_id,
            variant_count = variants.len(),
            query_ir_act = ?prepared.query_ir.act,
            query_ir_confidence = prepared.query_ir.confidence,
            "canonical preflight evidence triggered clarify path"
        );
        let clarify_result = crate::services::query::agent_loop::run_clarify_turn(
            state,
            library_id,
            generation_question,
            conversation_history_messages,
            &variants,
            // Ground the clarify lead on the SAME canonical evidence the preflight
            // single-shot answers from (`preflight.prompt_context`), not the
            // title-only expansion — `prepared_with_preflight_context_titles` adds
            // titles to the variant menu but does not put the canonical chunks into
            // `answer_context`, so a lead grounded on the latter could cite variants
            // it cannot actually support.
            &preflight.prompt_context,
        )
        .await;
        match clarify_result {
            Ok(clarify) => {
                if !clarify.answer.trim().is_empty() {
                    tracing::info!(
                        stage = "answer.preflight_clarify_done",
                        %execution_id,
                        answer_len = clarify.answer.len(),
                        elapsed_ms = clarify_start.elapsed().as_millis(),
                        "canonical preflight clarify path returned a question to the user"
                    );
                    let clarify_debug = clarify.debug_iterations.clone();
                    persist_llm_context_snapshot(
                        state,
                        crate::services::query::llm_context_debug::LlmContextSnapshot {
                            execution_id,
                            library_id,
                            question: user_question.to_string(),
                            total_iterations: clarify.iterations,
                            iterations: clarify_debug,
                            final_answer: Some(clarify.answer.clone()),
                            captured_at: chrono::Utc::now(),
                            query_ir: Some(
                                serde_json::to_value(&prepared.query_ir)
                                    .unwrap_or(serde_json::Value::Null),
                            ),
                            agent_loop: None,
                            spans: Vec::new(),
                        },
                    )
                    .await?;
                    let clarification = disposition_clarification(&clarify.answer, &variants);
                    return Ok(RuntimeAnswerQueryResult {
                        answer: clarify.answer,
                        provider: clarify.provider,
                        usage_json: clarify.usage_json,
                        clarification,
                    });
                }
                tracing::info!(
                    stage = "answer.preflight_clarify_empty",
                    %execution_id,
                    "canonical preflight clarify path returned empty text — falling back to answer generation"
                );
            }
            Err(error) => {
                tracing::warn!(
                    stage = "answer.preflight_clarify_error",
                    %execution_id,
                    ?error,
                    "canonical preflight clarify path failed — falling back to answer generation"
                );
            }
        }
    }

    let has_conversation_history =
        conversation_history.map(str::trim).is_some_and(|value| !value.is_empty());
    let preflight_single_shot_coverage = evaluate_single_shot_evidence_coverage_for_context(
        &prepared.query_ir,
        &preflight.prompt_context,
        has_conversation_history,
    );
    if !preflight.prompt_context.trim().is_empty() {
        if !single_shot_coverage_allows_attempt(&preflight_single_shot_coverage) {
            tracing::info!(
                stage = "answer.preflight_single_shot_canonical_attempt",
                %execution_id,
                coverage = ?preflight_single_shot_coverage,
                "canonical preflight answer will stay on fixed retrieved evidence despite incomplete structural coverage"
            );
        }
        let preflight_single_started = std::time::Instant::now();
        attempted_answer_generation = true;
        tracing::info!(
            stage = "answer.preflight_single_shot_start",
            %execution_id,
            %library_id,
            canonical_chunks = preflight.canonical_answer_chunks.len(),
            prompt_context_chars = preflight.prompt_context.chars().count(),
            "canonical preflight single-shot answer start"
        );
        match crate::services::query::agent_loop::run_single_shot_turn(
            state,
            library_id,
            generation_question,
            conversation_history_messages,
            &preflight.prompt_context,
        )
        .await
        {
            Ok(preflight_single) => {
                tracing::info!(
                    stage = "answer.preflight_single_shot_done",
                    %execution_id,
                    answer_len = preflight_single.answer.len(),
                    elapsed_ms = preflight_single_started.elapsed().as_millis(),
                    "canonical preflight single-shot answer done"
                );
                let preflight_answer = enforce_hard_output_boundary(
                    execution_id,
                    "answer.preflight_single_shot",
                    &prepared.query_ir,
                    preflight_single.answer.clone(),
                );
                let preflight_answer = append_missing_grounded_requested_labels_for_prepared(
                    preflight_answer,
                    &prepared,
                    generation_question,
                    &preflight.prompt_context,
                );
                let mut preflight_debug = preflight_single.debug_iterations.clone();
                let mut verification_stage = verify_generated_answer(
                    state,
                    execution_id,
                    effective_question,
                    AnswerGenerationStage {
                        intent_profile: prepared.structured.intent_profile.clone(),
                        canonical_answer_chunks: preflight.canonical_answer_chunks.clone(),
                        canonical_evidence: preflight.canonical_evidence.clone(),
                        assistant_grounding:
                            crate::services::query::assistant_grounding::AssistantGroundingEvidence::default(),
                        answer: preflight_answer,
                        provider: preflight_single.provider.clone(),
                        usage_json: preflight_single.usage_json.clone(),
                        prompt_context: preflight.prompt_context.clone(),
                        query_ir: prepared.query_ir.clone(),
                    },
                )
                .await?;
                if answer_needs_literal_revision(&verification_stage) {
                    tracing::info!(
                        stage = "answer.preflight_single_shot_literal_revision_start",
                        %execution_id,
                        unsupported_literals =
                            verification_stage.verification.unsupported_literals.len(),
                        "canonical preflight single-shot answer needs literal-fidelity revision"
                    );
                    let revision_context = literal_revision_context(
                        &preflight.prompt_context,
                        &crate::services::query::assistant_grounding::AssistantGroundingEvidence::default(),
                    );
                    let revision_targets = literal_revision_targets(
                        &verification_stage.generation.answer,
                        &verification_stage.verification.unsupported_literals,
                    );
                    match crate::services::query::agent_loop::run_literal_fidelity_revision_turn(
                        state,
                        library_id,
                        generation_question,
                        conversation_history_messages,
                        &verification_stage.generation.answer,
                        &revision_targets,
                        &revision_context,
                    )
                    .await
                    {
                        Ok(revision) => {
                            let usage_json = merge_generation_usage(
                                verification_stage.generation.usage_json.clone(),
                                &revision.usage_json,
                            );
                            preflight_debug.extend(revision.debug_iterations);
                            let revision_answer = enforce_hard_output_boundary(
                                execution_id,
                                "answer.preflight_single_shot_literal_revision",
                                &prepared.query_ir,
                                revision.answer.clone(),
                            );
                            if literal_revision_can_replace_answer(
                                &verification_stage.generation.answer,
                                &revision_answer,
                            ) {
                                let revision_answer =
                                    append_missing_grounded_requested_labels_for_prepared(
                                        revision_answer,
                                        &prepared,
                                        generation_question,
                                        &revision_context,
                                    );
                                let revised_stage = verify_generated_answer(
                                    state,
                                    execution_id,
                                    effective_question,
                                    AnswerGenerationStage {
                                        intent_profile: prepared.structured.intent_profile.clone(),
                                        canonical_answer_chunks:
                                            preflight.canonical_answer_chunks.clone(),
                                        canonical_evidence: preflight.canonical_evidence.clone(),
                                        assistant_grounding:
                                            crate::services::query::assistant_grounding::AssistantGroundingEvidence::default(),
                                        answer: revision_answer,
                                        provider: revision.provider.clone(),
                                        usage_json,
                                        prompt_context: preflight.prompt_context.clone(),
                                        query_ir: prepared.query_ir.clone(),
                                    },
                                )
                                .await?;
                                verification_stage = revised_stage;
                            } else {
                                tracing::warn!(
                                    stage = "answer.preflight_single_shot_literal_revision_rejected",
                                    %execution_id,
                                    draft_chars = verification_stage.generation.answer.chars().count(),
                                    revision_chars = revision.answer.chars().count(),
                                    "literal-fidelity revision did not preserve the answer shape"
                                );
                            }
                        }
                        Err(error) => {
                            tracing::warn!(
                                stage = "answer.preflight_single_shot_literal_revision_error",
                                %execution_id,
                                ?error,
                                "literal-fidelity revision failed for canonical preflight answer"
                            );
                        }
                    }
                }
                verification_stage = finalize_verified_answer_for_prepared(
                    state,
                    execution_id,
                    effective_question,
                    verification_stage,
                    &prepared,
                    generation_question,
                    &preflight.prompt_context,
                )
                .await?;
                if single_shot_answer_is_acceptable(
                    &verification_stage.generation.answer,
                    &verification_stage,
                    prepared.structured.retrieved_documents.len(),
                    &prepared.query_ir,
                    &preflight.prompt_context,
                ) {
                    tracing::info!(
                        stage = "answer.preflight_single_shot_accepted",
                        %execution_id,
                        verify_state = ?verification_stage.verification.state,
                        total_elapsed_ms = preflight_single_started.elapsed().as_millis(),
                        "canonical preflight single-shot answer accepted"
                    );
                    persist_llm_context_snapshot(
                        state,
                        crate::services::query::llm_context_debug::LlmContextSnapshot {
                            execution_id,
                            library_id,
                            question: user_question.to_string(),
                            total_iterations: preflight_debug.len(),
                            iterations: preflight_debug,
                            final_answer: Some(verification_stage.generation.answer.clone()),
                            captured_at: chrono::Utc::now(),
                            query_ir: Some(
                                serde_json::to_value(&prepared.query_ir)
                                    .unwrap_or(serde_json::Value::Null),
                            ),
                            agent_loop: None,
                            spans: Vec::new(),
                        },
                    )
                    .await?;
                    return Ok(RuntimeAnswerQueryResult {
                        answer: verification_stage.generation.answer,
                        provider: verification_stage.generation.provider,
                        usage_json: verification_stage.generation.usage_json,
                        clarification: RuntimeClarification::default(),
                    });
                }
                let verify_state = verification_stage.verification.state;
                let warning_count = verification_stage.verification.warnings.len();
                tracing::info!(
                    stage = "answer.preflight_single_shot_rejected",
                    %execution_id,
                    verify_state = ?verify_state,
                    warning_count,
                    "canonical preflight single-shot answer has verifier warnings — returning fixed-evidence result without re-retrieval"
                );
                canonical_candidate = Some(CanonicalAnswerCandidate {
                    total_iterations: preflight_debug.len(),
                    debug_iterations: preflight_debug,
                    verification_stage,
                });
            }
            Err(error) => {
                tracing::warn!(
                    stage = "answer.preflight_single_shot_error",
                    %execution_id,
                    ?error,
                    "canonical preflight single-shot answer failed"
                );
            }
        }
    }

    if canonical_candidate.is_none() && !attempted_answer_generation {
        let answer = "No grounded evidence was retrieved for this question.".to_string();
        tracing::info!(
            stage = "answer.no_evidence_finalized",
            %execution_id,
            "finalizing deterministic insufficient-evidence answer because retrieval produced no answer context"
        );
        let verification_stage = verify_generated_answer(
            state,
            execution_id,
            effective_question,
            AnswerGenerationStage {
                intent_profile: prepared.structured.intent_profile.clone(),
                canonical_answer_chunks: Vec::new(),
                canonical_evidence: super::CanonicalAnswerEvidence {
                    bundle: None,
                    chunk_rows: Vec::new(),
                    structured_blocks: Vec::new(),
                    technical_facts: Vec::new(),
                },
                assistant_grounding:
                    crate::services::query::assistant_grounding::AssistantGroundingEvidence::default(
                    ),
                answer,
                provider: _answer_provider,
                usage_json: serde_json::json!({
                    "deterministic": true,
                    "reason": "no_grounded_evidence",
                }),
                prompt_context: String::new(),
                query_ir: prepared.query_ir.clone(),
            },
        )
        .await?;
        canonical_candidate = Some(CanonicalAnswerCandidate {
            verification_stage,
            debug_iterations: Vec::new(),
            total_iterations: 0,
        });
    }

    let Some(candidate) = canonical_candidate else {
        anyhow::bail!(
            "canonical grounded-answer generation produced no answer candidate for execution {execution_id}"
        );
    };
    let answer_context = candidate.verification_stage.generation.prompt_context.clone();
    let candidate = CanonicalAnswerCandidate {
        verification_stage: finalize_verified_answer_for_prepared(
            state,
            execution_id,
            effective_question,
            candidate.verification_stage,
            &prepared,
            generation_question,
            &answer_context,
        )
        .await?,
        debug_iterations: candidate.debug_iterations,
        total_iterations: candidate.total_iterations,
    };
    tracing::info!(
        stage = "answer.fixed_evidence_finalized",
        %execution_id,
        verify_state = ?candidate.verification_stage.verification.state,
        warning_count = candidate.verification_stage.verification.warnings.len(),
        "finalizing grounded answer from fixed retrieved evidence without a second retrieval pass"
    );
    persist_llm_context_snapshot(
        state,
        crate::services::query::llm_context_debug::LlmContextSnapshot {
            execution_id,
            library_id,
            question: user_question.to_string(),
            total_iterations: candidate.total_iterations,
            iterations: candidate.debug_iterations,
            final_answer: Some(candidate.verification_stage.generation.answer.clone()),
            captured_at: chrono::Utc::now(),
            query_ir: Some(
                serde_json::to_value(&prepared.query_ir).unwrap_or(serde_json::Value::Null),
            ),
            agent_loop: None,
            spans: Vec::new(),
        },
    )
    .await?;
    Ok(RuntimeAnswerQueryResult {
        answer: candidate.verification_stage.generation.answer,
        provider: candidate.verification_stage.generation.provider,
        usage_json: candidate.verification_stage.generation.usage_json,
        clarification: RuntimeClarification::default(),
    })
}

/// Post-retrieval routing decision: should the runtime answer the
/// question from the evidence it has, or should it ask the user a
/// short clarifying question first?
///
/// This is a *corpus-conditioned* signal — QueryCompiler sees only
/// the raw NL question, but the retrieval bundle reveals whether
/// the library has one dominant procedure for the asked topic or
/// several competing variants / subsystems that a single-shot
/// answer will inevitably hedge across (the observed "scattered
/// mentions but no full guide" failure mode on short
/// `ConfigureHow` queries). Driven purely by structural signals on
/// the retrieved context — no hardcoded domain words, no library-
/// specific lists.
#[derive(Debug, Clone)]
enum AnswerDisposition {
    /// Proceed with single-shot grounded answering; the evidence
    /// has a dominant cluster or the question is specific enough.
    Answer,
    /// Ask a short clarifying question that enumerates the distinct
    /// variants the retrieval bundle found. `variants` are human-
    /// readable labels pulled from retrieved document titles, graph
    /// node labels, or grouped references — whichever are most
    /// naming on the fetched context.
    Clarify { variants: Vec<String> },
}

fn prepared_with_preflight_context_titles(
    prepared: &PreparedAnswerQueryResult,
    canonical_answer_chunks: &[RuntimeMatchedChunk],
) -> PreparedAnswerQueryResult {
    let mut expanded = prepared.clone();
    let mut seen = expanded
        .structured
        .retrieved_context_document_titles
        .iter()
        .map(|title| title.to_lowercase())
        .collect::<HashSet<_>>();
    for chunk in canonical_answer_chunks {
        let title = chunk.document_label.trim();
        if title.is_empty() {
            continue;
        }
        if seen.insert(title.to_lowercase()) {
            expanded.structured.retrieved_context_document_titles.push(title.to_string());
        }
    }
    expanded
}

/// Classify whether the runtime should answer from the retrieved
/// evidence or clarify with the user.
///
/// `Clarify` fires when the retrieved evidence itself shows the user's
/// topic is split across multiple variants. The compiler's explicit
/// clarification flag lowers the evidence threshold, but it is not the
/// sole authority: the compiler cannot see the retrieved bundle, so a
/// no-clarify IR must not force a weak single-shot answer when retrieval
/// has already found several query-aligned variants.
///
/// Required structural signals:
///   1. IR is underspecified enough that a clarifying question could help —
///      `ConfigureHow` / `Describe` / `RetrieveValue` without
///      `literal_constraints` or `document_focus`. Multiple target entities
///      normally mean the query is specific, except when the compiler already
///      requested clarification or the user sent a terse topic-selector
///      follow-up such as `<product> <topic>`.
///   2. Retrieval is multi-modal — at least
///      `CLARIFY_MIN_DISTINCT_DOCUMENTS` distinct documents hit the
///      bundle and no single document dominates by score.
///   3. The retrieved context names variants — we can pull at
///      least two human-readable labels (document titles, graph
///      node labels) to offer the user.
///   4. Without an explicit compiler flag, the query must be a configure
///      intent or a terse topic-selection follow-up; definition and
///      enumerate intents stay on the answer path.
///
/// Any one failing → `Answer`. `Compare` / `FollowUp` / `Meta`
/// queries never clarify here; they stay on the answer path because
/// retrieval coverage decides whether the fixed context is sufficient.
fn classify_answer_disposition(
    prepared: &PreparedAnswerQueryResult,
    user_question: &str,
) -> AnswerDisposition {
    if consolidation_commits_to_focused_answer(&prepared.consolidation) {
        return AnswerDisposition::Answer;
    }
    if super::consolidation::query_has_multi_document_setup_anchors(
        &prepared.query_ir,
        &prepared.structured.context_chunks,
    ) {
        return AnswerDisposition::Answer;
    }

    classify_answer_disposition_from_evidence(
        user_question,
        &prepared.query_ir,
        &prepared.structured.retrieved_documents,
        &prepared.structured.retrieved_context_document_titles,
        &prepared.structured.diagnostics.grouped_references,
    )
}

fn consolidation_commits_to_focused_answer(
    consolidation: &super::ConsolidationDiagnostics,
) -> bool {
    consolidation.focused_document_id.is_some()
        && !matches!(consolidation.focus_reason, FocusReason::None)
        && consolidation.winner_chunk_count > 0
}

#[cfg(test)]
fn classify_answer_disposition_from_groups(
    user_question: &str,
    ir: &QueryIR,
    retrieved_documents: &[crate::services::query::execution::types::RuntimeRetrievedDocumentBrief],
    groups: &[crate::domains::query::GroupedReference],
) -> AnswerDisposition {
    classify_answer_disposition_from_evidence(user_question, ir, retrieved_documents, &[], groups)
}

fn classify_answer_disposition_from_evidence(
    user_question: &str,
    ir: &QueryIR,
    retrieved_documents: &[crate::services::query::execution::types::RuntimeRetrievedDocumentBrief],
    context_document_titles: &[String],
    groups: &[crate::domains::query::GroupedReference],
) -> AnswerDisposition {
    use crate::domains::query_ir::QueryAct;

    let compiler_requested_clarification = ir.should_request_clarification();

    // 1. IR-level: is the question underspecified enough that a
    //    clarifying question could plausibly help?
    let act_can_clarify =
        matches!(ir.act, QueryAct::ConfigureHow | QueryAct::Describe | QueryAct::RetrieveValue);
    if query_ir_carries_answerable_focus(ir, user_question) {
        return AnswerDisposition::Answer;
    }
    let target_entities_allow_clarify = ir.target_entities.len() <= 1
        || compiler_requested_clarification
        || (matches!(ir.act, QueryAct::RetrieveValue)
            && question_is_terse_variant_selector(user_question));
    let is_underspecified = ir.literal_constraints.is_empty()
        && ir.document_focus.is_none()
        && target_entities_allow_clarify;
    if !(act_can_clarify && is_underspecified) {
        return AnswerDisposition::Answer;
    }
    // Temporal hard-filter already scoped retrieval. If the IR carries
    // resolved RFC3339 bounds the user has narrowed the question by
    // window — the retrieval filter drops every off-window chunk before
    // ranking, so the retrieved cluster is by construction a single
    // window. Routing into the multi-variant clarify prompt then asks
    // the user to disambiguate between off-window topics that
    // retrieval has already excluded, which produces the off-topic
    // "could be one of: X, Y, Z" replies the date-anchored benchmarks
    // surfaced. Stay on the answer path so the grounded prompt can
    // describe what the in-window evidence actually says (or refuse
    // cleanly when it says nothing).
    let (temporal_start, temporal_end) = ir.resolved_temporal_bounds();
    if temporal_start.is_some() || temporal_end.is_some() {
        return AnswerDisposition::Answer;
    }
    if !compiler_requested_clarification
        && !structural_clarify_allowed_without_compiler(ir, user_question)
    {
        return AnswerDisposition::Answer;
    }

    // 2. Retrieval-level: use the already-ranked `grouped_references`
    //    from the structured-query diagnostics. Each entry has a
    //    `title`, a `rank` (already sorted by the runtime) and an
    //    `evidence_count` — the number of distinct chunks /
    //    structured blocks / graph edges that support this group.
    //    A dominant cluster looks like one high evidence count
    //    followed by a sharp drop; a multi-modal spread looks like
    //    several groups with comparable evidence counts.
    let evidence_document_count =
        groups.len().max(context_document_titles.len()).max(retrieved_documents.len());
    if evidence_document_count < CLARIFY_MIN_DISTINCT_DOCUMENTS {
        return AnswerDisposition::Answer;
    }

    let mut ranked: Vec<(usize, String)> = groups
        .iter()
        .map(|reference| (reference.evidence_count, reference.title.clone()))
        .collect();
    ranked.sort_by_key(|entry| std::cmp::Reverse(entry.0));

    // 3. Variant extraction: keep only titles that match the user's
    //    topic tokens. Falling back to unrelated ranked tail labels
    //    creates a worse UX than answering from the retrieved context:
    //    the user asked about one thing and the router manufactures a
    //    menu about another. If too few query-aligned labels survive
    //    deduplication we cannot form a useful clarify menu.
    let variants = extract_query_specific_variants(
        user_question,
        retrieved_documents,
        context_document_titles,
        &ranked,
    );
    // The image-menu pathology (several same-page attachments offered as a
    // "pick one" menu) is fixed upstream by `extract_query_specific_variants`,
    // which now collapses same-parent attachments and excludes bare file
    // artefacts BEFORE they reach this count. With that collapse in place the
    // distinct-variant floor keeps its original shape: a genuine two-way
    // ambiguity surfaced by the compiler (`compiler_requested_clarification`)
    // is still a valid 2-variant clarification, while ordinary turns need the
    // full `CLARIFY_MIN_DISTINCT_DOCUMENTS` floor.
    let required_variant_count =
        if compiler_requested_clarification { 2 } else { CLARIFY_MIN_DISTINCT_DOCUMENTS };
    if variants.len() < required_variant_count {
        return AnswerDisposition::Answer;
    }

    // Dominance check is applied only to query-aligned groups. A large
    // off-topic evidence cluster should not suppress clarification for
    // the smaller set of documents that actually share the user's topic.
    let variant_labels = variants.iter().map(|label| label.to_lowercase()).collect::<Vec<_>>();
    let topic_ranked = ranked
        .iter()
        .filter(|(_, label)| {
            let lowered = label.to_lowercase();
            variant_labels.iter().any(|variant| variant == &lowered)
        })
        .cloned()
        .collect::<Vec<_>>();

    // If the top query-aligned group has strictly more evidence than
    // `CLARIFY_DOMINANCE_RATIO × second`, it's the main cluster — the
    // single-shot prompt can answer from it.
    if topic_ranked.len() >= variants.len() {
        if let (Some(top), Some(second)) = (topic_ranked.first(), topic_ranked.get(1)) {
            let (top_n, _) = top;
            let (second_n, _) = second;
            let materially_more_evidence = top_n.saturating_sub(*second_n) >= 2;
            if *top_n > 0
                && *second_n > 0
                && materially_more_evidence
                && (*top_n as f32) >= (*second_n as f32) * CLARIFY_DOMINANCE_RATIO
            {
                return AnswerDisposition::Answer;
            }
        }
    }

    // If evidence counts are noisy but one query-aligned variant is
    // clearly closer to the user wording than the runner-up, answer
    // directly from that dominant topic path.
    if has_query_dominant_topic_match(user_question, &topic_ranked) {
        return AnswerDisposition::Answer;
    }

    AnswerDisposition::Clarify { variants }
}

fn has_query_dominant_topic_match(user_question: &str, topic_ranked: &[(usize, String)]) -> bool {
    if topic_ranked.len() < 2 {
        return false;
    }

    let question_tokens = clarification_topic_tokens(user_question);
    if question_tokens.is_empty() {
        return false;
    }

    let overlap_with_question = |label: &str| -> usize {
        let label_tokens = crate::services::query::text_match::normalized_alnum_tokens(label, 3);
        crate::services::query::text_match::near_token_overlap_count(
            &question_tokens,
            &label_tokens,
        )
    };

    let top_overlap = overlap_with_question(&topic_ranked[0].1);
    let second_overlap = overlap_with_question(&topic_ranked[1].1);
    if top_overlap <= second_overlap || top_overlap == 0 {
        return false;
    }

    top_overlap >= 2 || second_overlap == 0
}

fn query_ir_carries_answerable_focus(ir: &QueryIR, user_question: &str) -> bool {
    if query_ir_has_focused_document_answer_intent(ir) {
        return true;
    }
    if detect_explicit_technical_literal_intent_from_query_ir(user_question, ir).any() {
        return true;
    }
    matches!(ir.act, QueryAct::Describe | QueryAct::RetrieveValue)
        && !ir.target_entities.is_empty()
        && !question_is_terse_variant_selector(user_question)
}

fn structural_clarify_allowed_without_compiler(ir: &QueryIR, user_question: &str) -> bool {
    use crate::domains::query_ir::QueryAct;

    match ir.act {
        QueryAct::Describe => {
            ir.confidence < 0.5 && question_is_terse_variant_selector(user_question)
        }
        QueryAct::ConfigureHow => question_is_terse_variant_selector(user_question),
        QueryAct::RetrieveValue => question_is_terse_variant_selector(user_question),
        _ => false,
    }
}

fn question_is_terse_variant_selector(user_question: &str) -> bool {
    let topic_tokens = clarification_topic_tokens(user_question);
    !topic_tokens.is_empty() && topic_tokens.len() <= 3
}

fn extract_query_specific_variants(
    user_question: &str,
    retrieved_documents: &[crate::services::query::execution::types::RuntimeRetrievedDocumentBrief],
    context_document_titles: &[String],
    ranked_labels: &[(usize, String)],
) -> Vec<String> {
    use std::collections::HashSet;

    let candidate_labels = context_document_titles
        .iter()
        .map(String::as_str)
        .chain(retrieved_documents.iter().map(|document| document.title.as_str()))
        .chain(ranked_labels.iter().map(|(_, label)| label.as_str()))
        .collect::<Vec<_>>();
    let topic_tokens = clarification_focus_tokens(user_question, candidate_labels.iter().copied());
    let mut seen: HashSet<String> = HashSet::new();
    let mut topical: Vec<String> = Vec::new();
    for title in context_document_titles {
        let trimmed = title.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }
        let Some(dedup_key) = clarify_variant_dedup_key(&trimmed) else {
            continue;
        };
        if label_matches_topic_tokens(&topic_tokens, &trimmed) && seen.insert(dedup_key) {
            topical.push(trimmed);
        }
        if topical.len() >= CLARIFY_MAX_VARIANTS {
            return topical;
        }
    }
    for document in retrieved_documents {
        let trimmed = document.title.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }
        let Some(dedup_key) = clarify_variant_dedup_key(&trimmed) else {
            continue;
        };
        if label_matches_topic_tokens(&topic_tokens, &trimmed) && seen.insert(dedup_key) {
            topical.push(trimmed);
        }
        if topical.len() >= CLARIFY_MAX_VARIANTS {
            return topical;
        }
    }
    if !topical.is_empty() {
        return topical;
    }
    for (_, label) in ranked_labels {
        let trimmed = label.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }
        let Some(dedup_key) = clarify_variant_dedup_key(&trimmed) else {
            continue;
        };
        if label_matches_topic_tokens(&topic_tokens, &trimmed) && seen.insert(dedup_key) {
            topical.push(trimmed);
        }
        if topical.len() >= CLARIFY_MAX_VARIANTS {
            break;
        }
    }
    if !topical.is_empty() {
        return topical;
    }

    Vec::new()
}

/// Deduplication key for a candidate clarify-variant label.
///
/// Returns `None` when the label is an attachment artefact rather than a
/// distinct logical document — image/file attachments are surfaced as
/// retrieved documents titled like `"<page>: <file>.<ext>"` (or as a bare
/// `"<file>.<ext>"`), and a menu of several same-page attachments is a worse
/// UX than answering. `None` excludes the label from the variant menu.
///
/// Otherwise returns the collapse key: a trailing `": <filename>.<ext>"`
/// qualifier is stripped so several attachments hanging off the same parent
/// page collapse to one variant. Detection is purely structural — a
/// `:` separator followed by a filename-shaped trailing token
/// (`<non-space>.<2-5 alnum>`) — so it carries no natural-language keyword
/// list and survives provider / language changes.
fn clarify_variant_dedup_key(label: &str) -> Option<String> {
    let trimmed = label.trim();
    match trailing_filename_qualifier_prefix(trimmed) {
        // `"X: file.png"` → collapse every same-prefix attachment onto `"x"`.
        Some(prefix) if !prefix.is_empty() => Some(prefix.to_lowercase()),
        // `"file.png"` with no logical prefix, or a bare filename token, is a
        // pure attachment artefact — exclude it from the variant menu.
        Some(_) => None,
        None if token_is_filename_shaped(trimmed) => None,
        None => Some(trimmed.to_lowercase()),
    }
}

/// If `label` ends in a `": <filename>.<ext>"` qualifier, return the logical
/// prefix that precedes the separating `:` (trimmed). `Some("")` means the
/// qualifier consumed the whole label (no logical prefix). `None` means there
/// is no such trailing qualifier.
///
/// "Filename-shaped" is structural: the trailing token after the `:` must
/// match `<one-or-more-non-space>.<2..=5 ascii-alnum>` and contain no path
/// separators — no extension allowlist, no language-specific tokens.
fn trailing_filename_qualifier_prefix(label: &str) -> Option<&str> {
    let (prefix, qualifier) = label.rsplit_once(':')?;
    let qualifier = qualifier.trim();
    if !token_is_filename_shaped(qualifier) {
        return None;
    }
    Some(prefix.trim_end())
}

/// `true` when `token` looks like `name.ext` — a non-empty stem, a single
/// trailing `.ext` of 2..=5 ascii-alphanumeric chars, no path separators, and
/// no internal whitespace. Structural shape check only.
fn token_is_filename_shaped(token: &str) -> bool {
    if token.is_empty()
        || token.contains('/')
        || token.contains('\\')
        || token.chars().any(char::is_whitespace)
    {
        return false;
    }
    let Some((stem, ext)) = token.rsplit_once('.') else {
        return false;
    };
    if stem.is_empty() {
        return false;
    }
    let ext_len = ext.chars().count();
    (2..=5).contains(&ext_len) && ext.chars().all(|ch| ch.is_ascii_alphanumeric())
}

fn clarification_focus_tokens<'a, I>(
    user_question: &str,
    candidate_labels: I,
) -> std::collections::BTreeSet<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let topic_tokens = clarification_topic_tokens(user_question);
    if topic_tokens.len() <= 1 {
        return topic_tokens;
    }

    let label_token_sets = candidate_labels
        .into_iter()
        .map(|label| crate::services::query::text_match::normalized_alnum_tokens(label, 3))
        .filter(|tokens| !tokens.is_empty())
        .collect::<Vec<_>>();
    if label_token_sets.is_empty() {
        return topic_tokens;
    }

    let mut repeated = std::collections::BTreeSet::new();
    let mut discriminating = std::collections::BTreeSet::new();
    for token in &topic_tokens {
        let hit_count = label_token_sets
            .iter()
            .filter(|label_tokens| {
                label_tokens.iter().any(|label_token| {
                    crate::services::query::text_match::near_token_match(token, label_token)
                })
            })
            .count();
        if hit_count >= 2 {
            repeated.insert(token.clone());
        }
        if hit_count >= 2 && hit_count < label_token_sets.len() {
            discriminating.insert(token.clone());
        }
    }

    if !discriminating.is_empty() {
        return discriminating;
    }
    if !repeated.is_empty() {
        return repeated;
    }
    topic_tokens
}

fn label_matches_topic_tokens(
    topic_tokens: &std::collections::BTreeSet<String>,
    label: &str,
) -> bool {
    if topic_tokens.is_empty() {
        return false;
    }
    let label_tokens = crate::services::query::text_match::normalized_alnum_tokens(label, 3);
    crate::services::query::text_match::near_token_overlap_count(topic_tokens, &label_tokens) > 0
}

fn clarification_topic_tokens(user_question: &str) -> std::collections::BTreeSet<String> {
    crate::services::query::text_match::normalized_alnum_tokens(user_question, 3)
        .into_iter()
        .collect()
}

#[derive(Debug, Clone, Default)]
struct CompareContextProbeOutcome {
    attempted: bool,
    missing_operand_count: usize,
    added_chunk_count: usize,
    unresolved_operand_count: usize,
}

#[derive(Debug, Clone, Default)]
struct TechnicalFocusProbeOutcome {
    attempted: bool,
    probe_term_count: usize,
    missing_term_count: usize,
    added_chunk_count: usize,
}

async fn augment_technical_focus_context(
    state: &AppState,
    library_id: Uuid,
    ir: &QueryIR,
    question: &str,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    answer_context: &mut String,
    structured: &mut super::RuntimeStructuredQueryResult,
) -> anyhow::Result<TechnicalFocusProbeOutcome> {
    let probe_terms = collect_technical_focus_probe_terms(question, ir);
    if probe_terms.is_empty() {
        return Ok(TechnicalFocusProbeOutcome::default());
    }
    let missing_terms = probe_terms
        .iter()
        .filter(|term| !contains_label_mention(answer_context, term))
        .cloned()
        .collect::<Vec<_>>();
    let focus_keywords = technical_literal_focus_keywords(question, Some(ir));
    let existing_chunk_ids = structured
        .chunk_references
        .iter()
        .map(|reference| reference.chunk_id)
        .chain(structured.context_chunks.iter().map(|chunk| chunk.chunk_id))
        .collect::<HashSet<_>>();
    let probe_chunks = probe_missing_technical_focus_terms(
        state,
        library_id,
        &missing_terms,
        &focus_keywords,
        document_index,
        plan_keywords,
        &existing_chunk_ids,
    )
    .await?;
    if !probe_chunks.is_empty() {
        let probe_question = missing_terms.join(" ");
        let literal_inventory =
            render_technical_focus_literal_inventory(&probe_chunks, &focus_keywords);
        append_answer_context_section(answer_context, &literal_inventory);
        let probe_context = render_targeted_evidence_chunk_section(&probe_question, &probe_chunks);
        append_answer_context_section(answer_context, &probe_context);
        append_probe_chunk_references(structured, &probe_chunks);
        append_probe_document_titles(structured, &probe_chunks);
        append_probe_context_chunks(structured, probe_chunks.clone());
    }
    Ok(TechnicalFocusProbeOutcome {
        attempted: true,
        probe_term_count: probe_terms.len(),
        missing_term_count: missing_terms.len(),
        added_chunk_count: probe_chunks.len(),
    })
}

fn collect_technical_focus_probe_terms(question: &str, ir: &QueryIR) -> Vec<String> {
    let mut ranked_terms = Vec::<(usize, String)>::new();
    for literal in &ir.literal_constraints {
        push_technical_focus_probe_term(&mut ranked_terms, &literal.text, 0);
    }
    for entity in &ir.target_entities {
        push_technical_focus_probe_term(&mut ranked_terms, &entity.label, 1);
    }
    if let Some(focus) = &ir.document_focus {
        push_technical_focus_probe_term(&mut ranked_terms, &focus.hint, 2);
    }
    for (index, token) in structural_question_tokens(question).into_iter().enumerate() {
        let rank = if structural_token_is_high_signal_probe_term(&token, index) { 3 } else { 6 };
        push_technical_focus_probe_term(&mut ranked_terms, &token, rank);
    }
    for token in technical_literal_focus_keywords(question, Some(ir)) {
        push_technical_focus_probe_term(&mut ranked_terms, &token, 7);
    }

    let mut best_rank_by_key = HashMap::<String, (usize, String)>::new();
    for (rank, term) in ranked_terms {
        let trimmed = term.trim();
        if !technical_focus_probe_term_is_eligible(trimmed) {
            continue;
        }
        let key = trimmed.to_lowercase();
        best_rank_by_key
            .entry(key)
            .and_modify(|existing| {
                if rank < existing.0 {
                    *existing = (rank, trimmed.to_string());
                }
            })
            .or_insert_with(|| (rank, trimmed.to_string()));
    }
    let mut terms = best_rank_by_key.into_values().collect::<Vec<_>>();
    terms.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| right.1.chars().count().cmp(&left.1.chars().count()))
            .then_with(|| left.1.cmp(&right.1))
    });
    terms.into_iter().map(|(_, term)| term).take(TECHNICAL_FOCUS_PROBE_TERM_LIMIT).collect()
}

fn push_technical_focus_probe_term(
    ranked_terms: &mut Vec<(usize, String)>,
    value: &str,
    rank: usize,
) {
    for token in structural_question_tokens(value) {
        ranked_terms.push((rank, token));
    }
}

fn structural_token_is_high_signal_probe_term(token: &str, index: usize) -> bool {
    token_has_fallback_entity_signal(token, index)
        || token.chars().all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
        || token.chars().any(|ch| matches!(ch, '_' | '-' | '.' | '/' | ':' | '@'))
}

fn technical_focus_probe_term_is_eligible(term: &str) -> bool {
    let char_count = term.chars().count();
    if !(2..=80).contains(&char_count) || !term.chars().any(char::is_alphanumeric) {
        return false;
    }
    structural_token_is_high_signal_probe_term(term, usize::MAX) || char_count >= 5
}

async fn probe_missing_technical_focus_terms(
    state: &AppState,
    library_id: Uuid,
    missing_terms: &[String],
    focus_keywords: &[String],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    existing_chunk_ids: &HashSet<Uuid>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let mut score_by_chunk = HashMap::<Uuid, f32>::new();
    for term in missing_terms {
        let rows = state
            .search_store
            .search_chunks(library_id, term, TECHNICAL_FOCUS_PROBE_HIT_LIMIT, None, None)
            .await?;
        let mut accepted_for_term = 0usize;
        for row in rows {
            if existing_chunk_ids.contains(&row.chunk_id) || !search_row_covers_operand(term, &row)
            {
                continue;
            }
            let score = row.score as f32 + (term.chars().count().min(24) as f32 / 24.0);
            score_by_chunk
                .entry(row.chunk_id)
                .and_modify(|existing| {
                    if score > *existing {
                        *existing = score;
                    }
                })
                .or_insert(score);
            accepted_for_term += 1;
            if accepted_for_term >= TECHNICAL_FOCUS_PROBE_MAX_CHUNKS_PER_TERM {
                break;
            }
        }
    }
    if focus_keywords.len() >= 3 {
        let focus_query = focus_keywords
            .iter()
            .take(TECHNICAL_FOCUS_PROBE_TERM_LIMIT)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        let rows = state
            .search_store
            .search_chunks(library_id, &focus_query, TECHNICAL_FOCUS_PROBE_HIT_LIMIT, None, None)
            .await?;
        let mut accepted_for_query = 0usize;
        for row in rows {
            if existing_chunk_ids.contains(&row.chunk_id)
                || score_by_chunk.contains_key(&row.chunk_id)
                || !search_row_covers_technical_focus(&row, focus_keywords)
            {
                continue;
            }
            score_by_chunk.insert(row.chunk_id, row.score as f32);
            accepted_for_query += 1;
            if accepted_for_query >= TECHNICAL_FOCUS_PROBE_MAX_CHUNKS_PER_TERM {
                break;
            }
        }
    }
    if score_by_chunk.is_empty() {
        return Ok(Vec::new());
    }
    let chunk_ids = score_by_chunk.keys().copied().collect::<Vec<_>>();
    let rows = state.document_store.list_chunks_by_ids(&chunk_ids).await?;
    let mut chunks = Vec::<RuntimeMatchedChunk>::new();
    for row in rows {
        let Some(score) = score_by_chunk.get(&row.chunk_id).copied() else {
            continue;
        };
        let Some(chunk) = super::retrieve::map_chunk_hit(row, score, document_index, plan_keywords)
        else {
            continue;
        };
        chunks.push(chunk);
    }
    chunks.sort_by(|left, right| {
        right.score.partial_cmp(&left.score).unwrap_or(std::cmp::Ordering::Equal)
    });
    chunks.truncate(TECHNICAL_FOCUS_PROBE_MAX_CHUNKS);
    Ok(chunks)
}

fn render_technical_focus_literal_inventory(
    chunks: &[RuntimeMatchedChunk],
    focus_keywords: &[String],
) -> String {
    let focus_keywords = focus_keywords
        .iter()
        .map(|keyword| keyword.trim().to_lowercase())
        .filter(|keyword| keyword.chars().count() >= 2)
        .collect::<Vec<_>>();
    if focus_keywords.is_empty() {
        return String::new();
    }
    let mut seen = HashSet::<String>::new();
    let mut lines = vec!["Exact technical literals".to_string()];
    for chunk in chunks {
        let text = format!("{}\n{}", chunk.excerpt, chunk.source_text);
        let literals = extract_focus_aligned_structural_literals(&text, &focus_keywords, &mut seen);
        if literals.is_empty() {
            continue;
        }
        let rendered = literals
            .into_iter()
            .take(8)
            .map(|literal| format!("`{literal}`"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("- {}: {}", chunk.document_label, rendered));
        if seen.len() >= 32 {
            break;
        }
    }
    if lines.len() <= 1 { String::new() } else { lines.join("\n") }
}

fn extract_focus_aligned_structural_literals(
    text: &str,
    focus_keywords: &[String],
    seen: &mut HashSet<String>,
) -> Vec<String> {
    structural_question_tokens(text)
        .into_iter()
        .map(|token| trim_structural_literal_token(&token).to_string())
        .filter(|token| technical_focus_literal_token_is_eligible(token))
        .filter(|token| technical_focus_literal_token_matches_focus(token, focus_keywords))
        .filter(|token| seen.insert(token.to_lowercase()))
        .take(16)
        .collect()
}

fn trim_structural_literal_token(token: &str) -> &str {
    token.trim_matches(|ch: char| {
        ch.is_whitespace()
            || matches!(ch, ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | '`' | ':')
    })
}

fn technical_focus_literal_token_is_eligible(token: &str) -> bool {
    let char_count = token.chars().count();
    (2..=96).contains(&char_count)
        && (structural_token_is_high_signal_probe_term(token, usize::MAX)
            || token.chars().any(char::is_uppercase))
}

fn technical_focus_literal_token_matches_focus(token: &str, focus_keywords: &[String]) -> bool {
    let lowered = token.to_lowercase();
    focus_keywords.iter().any(|keyword| {
        keyword == &lowered
            || (keyword.chars().count() >= 4 && lowered.contains(keyword))
            || (lowered.chars().count() >= 4 && keyword.contains(&lowered))
            || (keyword.chars().count() < 4
                && split_identifier_subtokens(token).iter().any(|part| part == keyword))
            || (keyword.chars().count() < 4
                && lowered.starts_with(keyword)
                && token.chars().any(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit()))
            || split_identifier_subtokens(token)
                .iter()
                .any(|part| part.chars().count() >= 4 && part == keyword)
    })
}

fn split_identifier_subtokens(token: &str) -> Vec<String> {
    let mut parts = Vec::<String>::new();
    let mut current = String::new();
    let mut previous_lowercase = false;
    for ch in token.chars() {
        if !ch.is_alphanumeric() {
            if !current.is_empty() {
                parts.push(current.to_lowercase());
                current.clear();
            }
            previous_lowercase = false;
            continue;
        }
        if previous_lowercase && ch.is_uppercase() && !current.is_empty() {
            parts.push(current.to_lowercase());
            current.clear();
        }
        previous_lowercase = ch.is_lowercase();
        current.push(ch);
    }
    if !current.is_empty() {
        parts.push(current.to_lowercase());
    }
    parts
}

fn search_row_covers_technical_focus(
    row: &crate::infra::knowledge_rows::KnowledgeChunkSearchRow,
    focus_keywords: &[String],
) -> bool {
    let section = row.section_path.join(" ");
    let heading = row.heading_trail.join(" ");
    let evidence =
        format!("{}\n{}\n{}\n{}", row.content_text, row.normalized_text, section, heading)
            .to_lowercase();
    let evidence_tokens = crate::services::query::text_match::normalized_alnum_tokens(&evidence, 2);
    let overlap = focus_keywords
        .iter()
        .filter(|keyword| {
            let keyword = keyword.trim().to_lowercase();
            !keyword.is_empty()
                && evidence_tokens.iter().any(|token| {
                    crate::services::query::text_match::near_token_match(&keyword, token)
                })
        })
        .count();
    overlap >= focus_keywords.len().min(3).clamp(2, 3)
}

async fn augment_partial_compare_context(
    state: &AppState,
    library_id: Uuid,
    ir: &QueryIR,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    answer_context: &mut String,
    structured: &mut super::RuntimeStructuredQueryResult,
) -> anyhow::Result<CompareContextProbeOutcome> {
    if !matches!(ir.act, QueryAct::Compare) {
        return Ok(CompareContextProbeOutcome::default());
    }
    let EvidenceCoverage::Partial { covered_operands, missing_operands } =
        compare_operands_covered_by_context(ir, answer_context)
    else {
        return Ok(CompareContextProbeOutcome::default());
    };
    let mut outcome = CompareContextProbeOutcome {
        attempted: true,
        missing_operand_count: missing_operands.len(),
        added_chunk_count: 0,
        unresolved_operand_count: missing_operands.len(),
    };
    let existing_chunk_ids = HashSet::<Uuid>::new();
    let probe_chunks = probe_missing_compare_operands(
        state,
        library_id,
        &missing_operands,
        document_index,
        plan_keywords,
        &existing_chunk_ids,
    )
    .await?;
    if !probe_chunks.is_empty() {
        let probe_question = missing_operands.join(" ");
        let probe_context = render_targeted_evidence_chunk_section(&probe_question, &probe_chunks);
        append_answer_context_section(answer_context, &probe_context);
        append_probe_chunk_references(structured, &probe_chunks);
        append_probe_document_titles(structured, &probe_chunks);
        append_probe_context_chunks(structured, probe_chunks.clone());
        outcome.added_chunk_count = probe_chunks.len();
    }

    match compare_operands_covered_by_context(ir, answer_context) {
        EvidenceCoverage::Sufficient => {
            outcome.unresolved_operand_count = 0;
        }
        EvidenceCoverage::Partial { covered_operands, missing_operands } => {
            outcome.unresolved_operand_count = missing_operands.len();
            append_answer_context_section(
                answer_context,
                &render_partial_comparison_coverage(&covered_operands, &missing_operands),
            );
        }
        EvidenceCoverage::Insufficient(_) => {
            append_answer_context_section(
                answer_context,
                &render_partial_comparison_coverage(&covered_operands, &missing_operands),
            );
        }
    }
    Ok(outcome)
}

async fn probe_missing_compare_operands(
    state: &AppState,
    library_id: Uuid,
    missing_operands: &[String],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    existing_chunk_ids: &HashSet<Uuid>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let mut score_by_chunk = HashMap::<Uuid, f32>::new();
    for operand in missing_operands {
        let query_variants = compare_operand_probe_queries(operand, plan_keywords);
        for query in query_variants {
            let rows = state
                .search_store
                .search_chunks(library_id, &query, COMPARE_OPERAND_PROBE_LIMIT, None, None)
                .await?;
            let mut accepted_for_operand = 0usize;
            for row in rows {
                if existing_chunk_ids.contains(&row.chunk_id)
                    || !search_row_covers_operand(operand, &row)
                {
                    continue;
                }
                let score =
                    row.score as f32 + compare_probe_query_specificity_bonus(&query, operand);
                score_by_chunk
                    .entry(row.chunk_id)
                    .and_modify(|existing| {
                        if score > *existing {
                            *existing = score;
                        }
                    })
                    .or_insert(score);
                accepted_for_operand += 1;
                if accepted_for_operand >= COMPARE_OPERAND_PROBE_MAX_CHUNKS_PER_OPERAND {
                    break;
                }
            }
        }
    }
    if score_by_chunk.is_empty() {
        return Ok(Vec::new());
    }
    let chunk_ids = score_by_chunk.keys().copied().collect::<Vec<_>>();
    let rows = state.document_store.list_chunks_by_ids(&chunk_ids).await?;
    let mut chunks = Vec::<RuntimeMatchedChunk>::new();
    for row in rows {
        let Some(score) = score_by_chunk.get(&row.chunk_id).copied() else {
            continue;
        };
        let Some(chunk) = super::retrieve::map_chunk_hit(row, score, document_index, plan_keywords)
        else {
            continue;
        };
        chunks.push(chunk);
    }
    chunks.sort_by(|left, right| {
        right.score.partial_cmp(&left.score).unwrap_or(std::cmp::Ordering::Equal)
    });
    chunks.truncate(COMPARE_OPERAND_PROBE_MAX_CHUNKS);
    Ok(chunks)
}

fn compare_operand_probe_queries(operand: &str, plan_keywords: &[String]) -> Vec<String> {
    let mut queries = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    let mut push = |value: String| {
        let normalized = value.trim();
        if normalized.is_empty() {
            return;
        }
        if seen.insert(normalized.to_lowercase()) {
            queries.push(normalized.to_string());
        }
    };
    push(operand.to_string());
    let focus = plan_keywords
        .iter()
        .map(|keyword| keyword.trim())
        .filter(|keyword| keyword.chars().count() >= 4)
        .take(8)
        .collect::<Vec<_>>()
        .join(" ");
    if !focus.is_empty() {
        push(format!("{operand} {focus}"));
    }
    queries
}

fn compare_probe_query_specificity_bonus(query: &str, operand: &str) -> f32 {
    if query.trim().eq_ignore_ascii_case(operand.trim()) { 0.0 } else { 1.0 }
}

fn search_row_covers_operand(
    operand: &str,
    row: &crate::infra::knowledge_rows::KnowledgeChunkSearchRow,
) -> bool {
    let section = row.section_path.join(" ");
    let heading = row.heading_trail.join(" ");
    let evidence = [
        row.content_text.as_str(),
        row.normalized_text.as_str(),
        section.as_str(),
        heading.as_str(),
    ];
    operand_covered_by_evidence(operand, &evidence)
}

fn append_probe_chunk_references(
    structured: &mut super::RuntimeStructuredQueryResult,
    chunks: &[RuntimeMatchedChunk],
) {
    let mut seen = structured
        .chunk_references
        .iter()
        .map(|reference| reference.chunk_id)
        .collect::<HashSet<_>>();
    let mut next_rank =
        structured.chunk_references.iter().map(|reference| reference.rank).max().unwrap_or(0) + 1;
    for chunk in chunks {
        if !seen.insert(chunk.chunk_id) {
            continue;
        }
        structured.chunk_references.push(QueryChunkReferenceSnapshot {
            chunk_id: chunk.chunk_id,
            rank: next_rank,
            score: chunk.score.unwrap_or(0.0) as f64,
        });
        next_rank += 1;
    }
}

fn append_probe_document_titles(
    structured: &mut super::RuntimeStructuredQueryResult,
    chunks: &[RuntimeMatchedChunk],
) {
    let mut seen = structured
        .retrieved_context_document_titles
        .iter()
        .map(|title| title.to_lowercase())
        .collect::<HashSet<_>>();
    for chunk in chunks {
        let title = chunk.document_label.trim();
        if title.is_empty() {
            continue;
        }
        if seen.insert(title.to_lowercase()) {
            structured.retrieved_context_document_titles.push(title.to_string());
        }
    }
}

fn append_probe_context_chunks(
    structured: &mut super::RuntimeStructuredQueryResult,
    chunks: Vec<RuntimeMatchedChunk>,
) {
    let mut seen =
        structured.context_chunks.iter().map(|chunk| chunk.chunk_id).collect::<HashSet<_>>();
    for chunk in chunks {
        if seen.insert(chunk.chunk_id) {
            structured.context_chunks.push(chunk);
        }
    }
}

fn append_answer_context_section(answer_context: &mut String, section: &str) {
    let section = section.trim();
    if section.is_empty() {
        return;
    }
    if !answer_context.trim().is_empty() {
        answer_context.push_str("\n\n");
    }
    answer_context.push_str(section);
}

fn render_partial_comparison_coverage(
    covered_operands: &[String],
    missing_operands: &[String],
) -> String {
    let mut lines = vec!["COMPARISON_COVERAGE status=partial".to_string()];
    for operand in covered_operands {
        lines.push(format!("- covered_operand: {}", operand.trim()));
    }
    for operand in missing_operands {
        lines.push(format!("- uncovered_operand: {}", operand.trim()));
    }
    lines.join("\n")
}

fn should_use_single_shot_answer(
    question: &str,
    prepared: &PreparedAnswerQueryResult,
    conversation_history: Option<&str>,
) -> bool {
    let _ = question;
    if query_ir_has_focused_document_answer_intent(&prepared.query_ir) {
        return false;
    }
    if prepared.query_ir.requests_source_coverage_context() {
        return false;
    }
    // Only hard requirement: the prepared context must carry *something*
    // the model can ground an answer in. Even when structured retrieval
    // returned zero chunks, `answer_context` still packs the library
    // summary, recent documents, and selected graph context. That alone
    // is enough for the model to produce a grounded insufficiency answer
    // without spending another pass rediscovering the same empty result.
    if prepared.answer_context.trim().is_empty() {
        return false;
    }
    if focused_configuration_inventory_waits_for_preflight(
        &prepared.query_ir,
        &prepared.answer_context,
    ) {
        tracing::info!(
            stage = "answer.single_shot_coverage",
            query_ir_act = ?prepared.query_ir.act,
            "focused configuration inventory waits for canonical preflight"
        );
        return false;
    }
    if structural_literal_comparison_waits_for_preflight(
        &prepared.query_ir,
        &prepared.answer_context,
    ) {
        tracing::info!(
            stage = "answer.single_shot_coverage",
            query_ir_act = ?prepared.query_ir.act,
            "comparison with exact structural literals waits for canonical preflight"
        );
        return false;
    }
    if super::build_update_procedure_sequence_answer(
        question,
        &prepared.query_ir,
        &prepared.structured.context_chunks,
    )
    .is_some()
    {
        tracing::info!(
            stage = "answer.single_shot_coverage",
            query_ir_act = ?prepared.query_ir.act,
            "deterministic update procedure is available; single-shot must not preempt it"
        );
        return false;
    }
    // Single-shot is evidence-gated, not act-blacklisted. The
    // prepared retrieval context is authoritative when it structurally
    // covers the operands required by the IR. Questions that still
    // depend on unresolved conversation anchors or library operations
    // remain off the initial fast path until those requirements are
    // represented in the same coverage model.
    // Retrieval injects version-sorted release chunks directly into
    // `answer_context`; a second retrieval pass would only repeat
    // document reads without adding canonical evidence.
    let has_conversation_history =
        conversation_history.map(str::trim).is_some_and(|v| !v.is_empty());
    match evaluate_single_shot_evidence_coverage(prepared, has_conversation_history) {
        EvidenceCoverage::Sufficient => true,
        EvidenceCoverage::Partial { missing_operands, .. } => {
            tracing::info!(
                stage = "answer.single_shot_coverage",
                query_ir_act = ?prepared.query_ir.act,
                missing_operand_count = missing_operands.len(),
                "prepared answer context partially covers comparison operands; single-shot must answer with explicit insufficiency for uncovered operands"
            );
            true
        }
        EvidenceCoverage::Insufficient(reason) => {
            tracing::info!(
                stage = "answer.single_shot_coverage",
                query_ir_act = ?prepared.query_ir.act,
                reason,
                "prepared answer context does not structurally cover the single-shot requirements"
            );
            false
        }
    }
}

fn structural_literal_comparison_waits_for_preflight(
    query_ir: &QueryIR,
    answer_context: &str,
) -> bool {
    if !matches!(query_ir.act, QueryAct::Compare) || query_ir.comparison.is_none() {
        return false;
    }
    let literal_context = answer_context_without_evidence_metadata(answer_context);
    let exact_literals = extract_focus_aligned_answer_suffix_literals(
        &literal_context,
        &comparison_focus_keywords(query_ir),
        &mut HashSet::new(),
    );
    if exact_literals.is_empty() {
        return false;
    }
    !matches!(
        compare_operands_covered_by_context(query_ir, answer_context),
        EvidenceCoverage::Insufficient(_)
    )
}

fn comparison_focus_keywords(query_ir: &QueryIR) -> Vec<String> {
    let mut seen = HashSet::new();
    [
        query_ir.retrieval_query.as_deref(),
        query_ir.comparison.as_ref().map(|comparison| comparison.dimension.as_str()),
    ]
    .into_iter()
    .flatten()
    .flat_map(structural_question_tokens)
    .map(|token| token.to_lowercase())
    .filter(|token| exact_literal_postprocessor_focus_keyword_is_eligible(token))
    .filter(|token| seen.insert(token.clone()))
    .collect()
}

fn focused_configuration_inventory_waits_for_preflight(
    query_ir: &QueryIR,
    answer_context: &str,
) -> bool {
    let literal_context = answer_context_without_evidence_metadata(answer_context);
    let package_count = extract_package_command_literals(&literal_context, 4).len();
    let configuration_path_count = count_configuration_file_paths(&literal_context, 16);
    let section_count = extract_config_section_literals(&literal_context, 8).len();
    let parameter_count = extract_parameter_literals(&literal_context, 32).len();

    let carries_configuration_inventory = (package_count > 0 && configuration_path_count > 0)
        || (configuration_path_count > 0 && section_count > 0 && parameter_count >= 2)
        || (configuration_path_count > 0 && parameter_count >= 4)
        || (section_count > 0 && parameter_count >= 6);
    if !carries_configuration_inventory {
        return false;
    }

    if low_confidence_unfocused_configuration_ir(query_ir)
        || low_confidence_structural_configuration_ir(query_ir)
    {
        return true;
    }

    if !matches!(query_ir.act, QueryAct::ConfigureHow) {
        return false;
    }
    let requests_configuration = query_ir.target_types.iter().any(|target_type| {
        matches!(
            super::question_intent::canonical_target_type_tag(target_type).as_str(),
            "configuration_file" | "config_key"
        )
    });
    if !requests_configuration {
        return false;
    }
    query_ir.document_focus.is_some()
        || !query_ir.target_entities.is_empty()
        || !query_ir.literal_constraints.is_empty()
        || !query_ir.conversation_refs.is_empty()
}

fn low_confidence_unfocused_configuration_ir(query_ir: &QueryIR) -> bool {
    query_ir_is_low_confidence_unfocused_answer(query_ir)
}

fn low_confidence_structural_configuration_ir(query_ir: &QueryIR) -> bool {
    query_ir.confidence <= 0.35
        && matches!(query_ir.scope, QueryScope::SingleDocument | QueryScope::MultiDocument)
        && matches!(
            query_ir.act,
            QueryAct::Describe | QueryAct::ConfigureHow | QueryAct::RetrieveValue
        )
        && query_ir.source_slice.is_none()
        && query_ir.document_focus.is_none()
        && query_ir.target_types.is_empty()
        && query_ir.comparison.is_none()
        && query_ir.temporal_constraints.is_empty()
        && query_ir.conversation_refs.is_empty()
        && (!query_ir.target_entities.is_empty() || !query_ir.literal_constraints.is_empty())
}

fn answer_context_without_evidence_metadata(answer_context: &str) -> String {
    let mut stripped = String::with_capacity(answer_context.len());
    for line in answer_context.lines() {
        stripped.push_str(strip_evidence_chunk_prefix(line));
        stripped.push('\n');
    }
    stripped
}

fn strip_evidence_chunk_prefix(line: &str) -> &str {
    let trimmed = line.trim_start();
    let candidate = trimmed.strip_prefix("- ").unwrap_or(trimmed);
    if !candidate.starts_with("[EVIDENCE_CHUNK ") {
        return line;
    }
    candidate.find("] ").map(|close_index| &candidate[(close_index + 2)..]).unwrap_or(line)
}

fn count_configuration_file_paths(text: &str, limit: usize) -> usize {
    let mut seen = HashSet::new();
    for path in extract_explicit_path_literals(text, limit) {
        if is_configuration_file_path(&path) {
            seen.insert(path);
        }
    }
    for token in text.split_whitespace() {
        if seen.len() >= limit {
            break;
        }
        let cleaned = clean_configuration_path_candidate(token);
        if cleaned.starts_with('/') && is_configuration_file_path(cleaned) {
            seen.insert(cleaned.to_string());
        }
    }
    seen.len()
}

fn clean_configuration_path_candidate(token: &str) -> &str {
    let mut value = token.trim();
    loop {
        let trimmed = value
            .trim_start_matches(|ch: char| {
                ch.is_whitespace()
                    || matches!(ch, '`' | '"' | '\'' | '(' | '[' | '{' | ',' | ';' | ':')
            })
            .trim_end_matches(|ch: char| {
                ch.is_whitespace()
                    || matches!(ch, '`' | '"' | '\'' | ')' | ']' | '}' | ',' | ';' | ':' | '.')
            });
        if trimmed.len() == value.len() {
            return trimmed;
        }
        value = trimmed;
    }
}

fn is_configuration_file_path(path: &str) -> bool {
    let lowered = path.to_ascii_lowercase();
    [".conf", ".ini", ".cfg", ".properties", ".yaml", ".yml", ".toml", ".json"]
        .iter()
        .any(|extension| lowered.ends_with(extension))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EvidenceCoverage {
    Sufficient,
    Partial { covered_operands: Vec<String>, missing_operands: Vec<String> },
    Insufficient(&'static str),
}

fn evaluate_single_shot_evidence_coverage(
    prepared: &PreparedAnswerQueryResult,
    has_conversation_history: bool,
) -> EvidenceCoverage {
    evaluate_single_shot_evidence_coverage_for_context(
        &prepared.query_ir,
        &prepared.answer_context,
        has_conversation_history,
    )
}

fn evaluate_single_shot_evidence_coverage_for_context(
    ir: &QueryIR,
    answer_context: &str,
    has_conversation_history: bool,
) -> EvidenceCoverage {
    if ir.is_follow_up() && has_conversation_history {
        return EvidenceCoverage::Insufficient("follow_up_context_anchor_unresolved");
    }
    if matches!(ir.act, QueryAct::Meta) {
        return EvidenceCoverage::Insufficient("library_meta_requires_catalog_evidence");
    }
    if matches!(ir.act, QueryAct::Compare) {
        return compare_operands_covered_by_context(ir, answer_context);
    }
    if single_shot_context_lacks_query_focus_support(ir, answer_context) {
        return EvidenceCoverage::Insufficient("query_focus_uncovered");
    }
    EvidenceCoverage::Sufficient
}

fn single_shot_coverage_allows_attempt(coverage: &EvidenceCoverage) -> bool {
    matches!(coverage, EvidenceCoverage::Sufficient | EvidenceCoverage::Partial { .. })
}

fn compare_operands_covered_by_context(ir: &QueryIR, answer_context: &str) -> EvidenceCoverage {
    let operands = comparison_operands(ir);
    if operands.len() < 2 {
        return EvidenceCoverage::Insufficient("compare_operands_missing");
    }
    let evidence_lines =
        answer_context.lines().map(str::trim).filter(is_context_evidence_line).collect::<Vec<_>>();
    if evidence_lines.is_empty() {
        return EvidenceCoverage::Insufficient("compare_evidence_empty");
    }
    let mut covered_operands = Vec::<String>::new();
    let mut missing_operands = Vec::<String>::new();
    for operand in operands {
        if operand_covered_by_evidence(&operand, &evidence_lines) {
            covered_operands.push(operand);
        } else {
            missing_operands.push(operand);
        }
    }
    if missing_operands.is_empty() {
        EvidenceCoverage::Sufficient
    } else if covered_operands.is_empty() {
        EvidenceCoverage::Insufficient("compare_operands_uncovered")
    } else {
        EvidenceCoverage::Partial { covered_operands, missing_operands }
    }
}

fn is_context_evidence_line(line: &&str) -> bool {
    let line = line.trim();
    !line.is_empty()
        && !line.starts_with("COMPARISON_COVERAGE ")
        && !line.starts_with("- covered_operand:")
        && !line.starts_with("- uncovered_operand:")
}

fn comparison_operands(ir: &QueryIR) -> Vec<String> {
    let mut operands = Vec::<String>::new();
    if let Some(comparison) = &ir.comparison {
        if let Some(value) = comparison.a.as_deref() {
            push_operand(&mut operands, value);
        }
        if let Some(value) = comparison.b.as_deref() {
            push_operand(&mut operands, value);
        }
    }
    if operands.len() >= 2 {
        return operands;
    }
    for entity in &ir.target_entities {
        push_operand(&mut operands, &entity.label);
    }
    operands
}

fn push_operand(operands: &mut Vec<String>, value: &str) {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return;
    }
    if operands.iter().any(|existing| existing.eq_ignore_ascii_case(trimmed)) {
        return;
    }
    operands.push(trimmed.to_string());
}

fn operand_covered_by_evidence(operand: &str, evidence_lines: &[&str]) -> bool {
    let operand_tokens = crate::services::query::text_match::normalized_alnum_tokens(operand, 2);
    if operand_tokens.is_empty() {
        return false;
    }
    let required_overlap = operand_tokens.len().clamp(1, 2);
    evidence_lines.iter().any(|line| {
        let line_tokens = crate::services::query::text_match::normalized_alnum_tokens(line, 2);
        crate::services::query::text_match::near_token_overlap_count(&operand_tokens, &line_tokens)
            >= required_overlap
    })
}

/// Treat a single-shot answer as acceptable when it carries enough
/// text to be useful, the verifier did not rewrite it, AND the
/// model did not obviously capitulate in front of a non-empty
/// retrieval bundle.
///
/// Structural signals:
///   * Absolute length floor — below `SINGLE_SHOT_MIN_ANSWER_CHARS`
///     is always treated as a decline.
///   * Verifier rewrite — `verify_generated_answer` only rewrites
///     the answer under strict-mode suppression of a hallucinated
///     literal; a matching trimmed raw vs. verified string means
///     the verifier let the answer through.
///   * Retrieval-vs-length heuristic — when retrieval surfaced
///     `>= SINGLE_SHOT_RETRIEVAL_ESCALATION_MIN_DOCUMENTS` and the
///     answer is still `< SINGLE_SHOT_CONFIDENT_ANSWER_CHARS`, the
///     single-shot path almost certainly refused on partial
///     evidence (see the one-word vs. "who is X" observation above).
///     Escalate instead of returning the stub.
///
/// No decline-phrase matching, no language-specific strings: the
/// verifier owns grounding, length owns "did the model produce
/// something", and the retrieval footprint owns "did the model
/// refuse in the face of real evidence".
fn single_shot_answer_is_acceptable(
    raw_answer: &str,
    verification: &AnswerVerificationStage,
    retrieved_document_count: usize,
    query_ir: &QueryIR,
    grounding_context: &str,
) -> bool {
    let trimmed = raw_answer.trim();
    let answer_chars = trimmed.chars().count();
    if answer_chars < SINGLE_SHOT_MIN_ANSWER_CHARS {
        return false;
    }
    if answer_contains_internal_history_marker(trimmed) {
        return false;
    }
    if answer_needs_literal_revision(verification) {
        return false;
    }
    if answer_has_partial_coverage_warning(verification) {
        return false;
    }
    let verified = verification.generation.answer.trim();
    if verified.is_empty() {
        return false;
    }
    if trimmed != verified {
        return false;
    }
    if answer_chars < SINGLE_SHOT_CONFIDENT_ANSWER_CHARS
        && retrieved_document_count >= SINGLE_SHOT_RETRIEVAL_ESCALATION_MIN_DOCUMENTS
    {
        return false;
    }
    if let Some(min_chars) = source_slice_single_shot_min_chars(query_ir)
        && answer_chars < min_chars
    {
        return false;
    }
    if answer_omits_expected_technical_literals(trimmed, query_ir, grounding_context) {
        return false;
    }
    if single_shot_lacks_query_focus_support(trimmed, query_ir, grounding_context) {
        return false;
    }
    true
}

fn answer_contains_internal_history_marker(answer: &str) -> bool {
    answer.contains("Prior assistant compact literal memory.")
        || answer.contains("Prior assistant pinned literal anchors.")
}

fn single_shot_context_lacks_query_focus_support(
    query_ir: &QueryIR,
    grounding_context: &str,
) -> bool {
    if !query_requires_single_shot_focus_support(query_ir) {
        return false;
    }
    let focus_segments = query_focus_support_segments(query_ir);
    if focus_segments.is_empty() {
        return false;
    }
    let context_tokens =
        crate::services::query::text_match::normalized_alnum_tokens(grounding_context, 4);
    !focus_segments
        .iter()
        .any(|segment| focus_segment_supported_by_tokens(segment, &context_tokens))
}

fn single_shot_lacks_query_focus_support(
    answer: &str,
    query_ir: &QueryIR,
    grounding_context: &str,
) -> bool {
    if !query_requires_single_shot_focus_support(query_ir) {
        return false;
    }
    let focus_segments = query_focus_support_segments(query_ir);
    if focus_segments.is_empty() {
        return false;
    }
    let context_tokens =
        crate::services::query::text_match::normalized_alnum_tokens(grounding_context, 4);
    let answer_tokens = crate::services::query::text_match::normalized_alnum_tokens(answer, 4);
    let supported_segments = focus_segments
        .iter()
        .filter(|segment| focus_segment_supported_by_tokens(segment, &context_tokens))
        .collect::<Vec<_>>();
    if supported_segments.is_empty() {
        return true;
    }
    !supported_segments
        .iter()
        .any(|segment| focus_segment_supported_by_tokens(segment, &answer_tokens))
}

fn query_requires_single_shot_focus_support(query_ir: &QueryIR) -> bool {
    if !matches!(query_ir.act, QueryAct::ConfigureHow | QueryAct::RetrieveValue) {
        return false;
    }
    let intent = detect_explicit_technical_literal_intent_from_query_ir("", query_ir);
    intent.any() || query_ir.document_focus.is_some() || query_ir.has_exact_technical_literal()
}

fn query_focus_support_segments(query_ir: &QueryIR) -> Vec<BTreeSet<String>> {
    let mut segments = query_ir
        .target_entities
        .iter()
        .filter(|entity| matches!(entity.role, EntityRole::Subject | EntityRole::Object))
        .filter_map(|entity| focus_support_tokens(&entity.label))
        .collect::<Vec<_>>();
    if segments.is_empty() {
        if let Some(document_focus) = &query_ir.document_focus
            && let Some(tokens) = focus_support_tokens(&document_focus.hint)
        {
            segments.push(tokens);
        }
    }
    for literal in &query_ir.literal_constraints {
        if let Some(tokens) = focus_support_tokens(&literal.text) {
            segments.push(tokens);
        }
    }
    segments
}

fn focus_support_tokens(value: &str) -> Option<BTreeSet<String>> {
    let tokens = crate::services::query::text_match::normalized_alnum_tokens(value, 4);
    (!tokens.is_empty()).then_some(tokens)
}

fn focus_segment_supported_by_tokens(
    segment_tokens: &BTreeSet<String>,
    available_tokens: &BTreeSet<String>,
) -> bool {
    if segment_tokens.is_empty() {
        return false;
    }
    crate::services::query::text_match::near_token_overlap_count(segment_tokens, available_tokens)
        >= focus_segment_required_overlap(segment_tokens)
}

fn focus_segment_required_overlap(segment_tokens: &BTreeSet<String>) -> usize {
    if segment_tokens.len() <= 2 { 1 } else { 2 }
}

fn answer_omits_expected_technical_literals(
    answer: &str,
    query_ir: &QueryIR,
    grounding_context: &str,
) -> bool {
    let intent = detect_technical_literal_intent_from_query_ir("", query_ir);
    let omits_assignment_examples =
        answer_omits_focused_assignment_examples(answer, query_ir, grounding_context);
    if !intent.any() {
        return omits_assignment_examples;
    }
    let context_literals = collect_intended_technical_literals(grounding_context, intent, 8);
    if context_literals.is_empty() && !omits_assignment_examples {
        return false;
    }
    if answer_omits_focused_configuration_paths(answer, query_ir, grounding_context) {
        return true;
    }
    if answer_omits_focused_section_listing(answer, query_ir, grounding_context) {
        return true;
    }
    if answer_omits_focused_parameter_listing(answer, query_ir, grounding_context) {
        return true;
    }
    if omits_assignment_examples {
        return true;
    }
    collect_intended_technical_literals(answer, intent, 2).is_empty()
}

fn answer_omits_focused_assignment_examples(
    answer: &str,
    query_ir: &QueryIR,
    grounding_context: &str,
) -> bool {
    if !matches!(query_ir.scope, QueryScope::SingleDocument)
        || !matches!(query_ir.act, QueryAct::ConfigureHow | QueryAct::Describe)
    {
        return false;
    }
    let expected = collect_context_config_assignment_literals(grounding_context, 24);
    if expected.len() < 2 {
        return false;
    }
    let actual = extract_config_assignment_literals(answer, expected.len().max(24))
        .into_iter()
        .collect::<HashSet<_>>();
    expected.iter().any(|literal| !actual.contains(literal))
}

fn answer_omits_focused_configuration_paths(
    answer: &str,
    query_ir: &QueryIR,
    grounding_context: &str,
) -> bool {
    if !matches!(query_ir.act, QueryAct::ConfigureHow) {
        return false;
    }
    let expected = collect_focused_context_path_literals(grounding_context, query_ir, 8);
    if expected.is_empty() {
        return false;
    }
    let actual = collect_intended_technical_literals(
        answer,
        TechnicalLiteralIntent { wants_paths: true, ..TechnicalLiteralIntent::default() },
        expected.len().max(8),
    );
    expected.iter().any(|literal| !actual.contains(literal))
}

fn answer_omits_focused_section_listing(
    answer: &str,
    query_ir: &QueryIR,
    grounding_context: &str,
) -> bool {
    if !matches!(query_ir.act, QueryAct::ConfigureHow) {
        return false;
    }
    let expected = collect_focused_context_section_literals(grounding_context, query_ir, 16);
    if expected.is_empty() {
        return false;
    }
    let actual = collect_intended_technical_literals(
        answer,
        TechnicalLiteralIntent { wants_parameters: true, ..TechnicalLiteralIntent::default() },
        expected.len().max(16),
    );
    expected.iter().any(|literal| !actual.contains(literal))
}

fn answer_omits_focused_parameter_listing(
    answer: &str,
    query_ir: &QueryIR,
    grounding_context: &str,
) -> bool {
    if !matches!(query_ir.act, QueryAct::ConfigureHow) {
        return false;
    }
    let expected = collect_focused_context_parameter_literals(grounding_context, query_ir, 32);
    if expected.len() < 4 {
        return false;
    }
    let actual = collect_intended_technical_literals(
        answer,
        TechnicalLiteralIntent { wants_parameters: true, ..TechnicalLiteralIntent::default() },
        expected.len().max(32),
    );
    expected.iter().any(|literal| !actual.contains(literal))
}

fn collect_focused_context_section_literals(
    grounding_context: &str,
    query_ir: &QueryIR,
    limit: usize,
) -> HashSet<String> {
    collect_focused_context_literals(
        grounding_context,
        query_ir,
        limit,
        "Sections:",
        extract_config_section_literals,
    )
}

fn collect_focused_context_parameter_literals(
    grounding_context: &str,
    query_ir: &QueryIR,
    limit: usize,
) -> HashSet<String> {
    collect_focused_context_literals(
        grounding_context,
        query_ir,
        limit,
        "Parameters:",
        extract_parameter_literals,
    )
}

fn collect_focused_context_path_literals(
    grounding_context: &str,
    query_ir: &QueryIR,
    limit: usize,
) -> HashSet<String> {
    collect_focused_context_literals(
        grounding_context,
        query_ir,
        limit,
        "Paths:",
        extract_explicit_path_literals,
    )
}

fn collect_context_config_assignment_literals(
    grounding_context: &str,
    limit: usize,
) -> HashSet<String> {
    extract_config_assignment_literals(grounding_context, limit).into_iter().collect()
}

fn collect_focused_context_literals(
    grounding_context: &str,
    query_ir: &QueryIR,
    limit: usize,
    label: &str,
    extractor: fn(&str, usize) -> Vec<String>,
) -> HashSet<String> {
    let focus_segments = query_focus_support_segments(query_ir);
    if focus_segments.is_empty() {
        return HashSet::new();
    }
    let mut literals = HashSet::<String>::new();
    let mut current_label: Option<String> = None;
    let mut current_body = String::new();
    for line in grounding_context.lines() {
        if let Some(document_label) = exact_technical_literal_document_label(line) {
            push_focused_context_literals(
                current_label.as_deref(),
                &current_body,
                &focus_segments,
                limit,
                label,
                extractor,
                &mut literals,
            );
            current_label = Some(document_label);
            current_body.clear();
            continue;
        }
        if current_label.is_some() {
            current_body.push_str(line);
            current_body.push('\n');
        }
    }
    push_focused_context_literals(
        current_label.as_deref(),
        &current_body,
        &focus_segments,
        limit,
        label,
        extractor,
        &mut literals,
    );
    literals
}

fn push_focused_context_literals(
    document_label: Option<&str>,
    body: &str,
    focus_segments: &[BTreeSet<String>],
    limit: usize,
    label: &str,
    extractor: fn(&str, usize) -> Vec<String>,
    out: &mut HashSet<String>,
) {
    let Some(document_label) = document_label else {
        return;
    };
    if out.len() >= limit || !document_label_matches_focus(document_label, focus_segments) {
        return;
    }
    let literal_text = collect_focused_literal_candidate_text(body, label);
    push_focused_literals_from_text(&literal_text, limit, extractor, out);
}

fn push_focused_literals_from_text(
    text: &str,
    limit: usize,
    extractor: fn(&str, usize) -> Vec<String>,
    out: &mut HashSet<String>,
) {
    let remaining = limit.saturating_sub(out.len());
    if remaining == 0 {
        return;
    }
    for literal in extractor(text, remaining) {
        out.insert(literal);
        if out.len() >= limit {
            return;
        }
    }
}

fn collect_labelled_literal_inventory(body: &str, label: &str) -> String {
    let mut lines = Vec::new();
    let mut in_label = false;
    for line in body.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix(label) {
            in_label = true;
            lines.push(line.to_string());
            let suffix = rest.trim();
            if !suffix.is_empty() {
                lines.push(suffix.to_string());
            }
            continue;
        }
        if !in_label {
            continue;
        }
        if line.starts_with("- `") {
            lines.push(line.to_string());
            continue;
        }
        if line.ends_with(':') && !line.starts_with('-') {
            break;
        }
        if !line.is_empty() && !line.starts_with('-') {
            break;
        }
    }
    lines.join("\n")
}

fn collect_focused_literal_candidate_text(body: &str, label: &str) -> String {
    let mut lines = Vec::new();
    let labelled = collect_labelled_literal_inventory(body, label);
    if !labelled.is_empty() {
        lines.push(labelled);
    }
    if label == "Parameters:" {
        for line in body.lines().map(str::trim) {
            if line.is_empty() {
                continue;
            }
            if let Some(table_row) = line.strip_prefix("- table_row ") {
                push_structured_parameter_table_row(table_row, &mut lines);
                continue;
            }
        }
    }
    lines.join("\n")
}

fn push_structured_parameter_table_row(row: &str, out: &mut Vec<String>) {
    let Some((_, cells)) = row.split_once(": ") else {
        return;
    };
    let candidate_cells =
        cells.split('|').map(str::trim).filter(|cell| !cell.is_empty()).collect::<Vec<_>>();
    if candidate_cells.len() < 2 {
        return;
    }
    let first = candidate_cells[0];
    if first.chars().any(char::is_whitespace) {
        return;
    }
    if crate::domains::query_ir::literal_text_is_identifier_shaped(first) {
        out.push(first.to_string());
    }
}

fn exact_technical_literal_document_label(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix("- Document: `")?;
    let (label, _) = rest.split_once('`')?;
    let label = label.trim();
    (!label.is_empty()).then(|| label.to_string())
}

fn document_label_matches_focus(label: &str, focus_segments: &[BTreeSet<String>]) -> bool {
    let label_tokens = crate::services::query::text_match::normalized_alnum_tokens(label, 4);
    focus_segments.iter().any(|segment| {
        let overlap =
            crate::services::query::text_match::near_token_overlap_count(segment, &label_tokens);
        overlap >= segment.len().min(2)
    })
}

fn collect_intended_technical_literals(
    text: &str,
    intent: TechnicalLiteralIntent,
    limit: usize,
) -> HashSet<String> {
    let mut literals = HashSet::<String>::new();
    if intent.wants_urls {
        literals.extend(extract_url_literals(text, limit));
    }
    if intent.wants_prefixes {
        literals.extend(extract_prefix_literals(text, limit));
    }
    if intent.wants_paths {
        literals.extend(extract_explicit_path_literals(text, limit));
    }
    if intent.wants_methods {
        literals.extend(extract_http_methods(text, limit));
    }
    if intent.wants_parameters {
        literals.extend(extract_config_section_literals(text, limit));
        literals.extend(extract_parameter_literals(text, limit));
    }
    literals
}

fn answer_needs_literal_revision(verification: &AnswerVerificationStage) -> bool {
    verification.verification.warnings.iter().any(|warning| {
        warning.code == "unsupported_literal" || warning.code == "unsupported_canonical_claim"
    })
}

fn answer_has_partial_coverage_warning(verification: &AnswerVerificationStage) -> bool {
    verification.verification.warnings.iter().any(|warning| warning.code == "partial_coverage")
}

pub(crate) fn literal_revision_targets(
    answer: &str,
    unsupported_literals: &[String],
) -> Vec<String> {
    if unsupported_literals.is_empty() {
        return Vec::new();
    }
    let mut targets = unsupported_literals.to_vec();
    let mut seen = targets.iter().cloned().collect::<HashSet<_>>();
    for block in fenced_code_blocks(answer) {
        if unsupported_literals.iter().any(|literal| fenced_block_contains_literal(&block, literal))
            && seen.insert(block.clone())
        {
            targets.push(block);
        }
    }
    targets
}

fn fenced_code_blocks(answer: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let bytes = answer.as_bytes();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if !bytes[cursor..].starts_with(b"```") {
            cursor += utf8_char_len(bytes[cursor]);
            continue;
        }
        let body_start = cursor + 3;
        let Some(relative_end) = find_subslice(&bytes[body_start..], b"```") else {
            break;
        };
        let body_end = body_start + relative_end;
        let body = answer[body_start..body_end]
            .strip_prefix('\n')
            .or_else(|| answer[body_start..body_end].strip_prefix("\r\n"))
            .unwrap_or(&answer[body_start..body_end]);
        let mut lines = body.lines().collect::<Vec<_>>();
        if lines.first().is_some_and(|line| is_fenced_language_hint(line.trim())) {
            lines.remove(0);
        }
        let normalized = lines
            .into_iter()
            .map(|line| line.trim_end_matches('\r'))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();
        if !normalized.is_empty() {
            blocks.push(normalized);
        }
        cursor = body_end + 3;
    }
    blocks
}

fn fenced_block_contains_literal(block: &str, literal: &str) -> bool {
    let literal = literal.trim();
    !literal.is_empty()
        && block.lines().map(str::trim).any(|line| line == literal || line.contains(literal))
}

fn is_fenced_language_hint(candidate: &str) -> bool {
    !candidate.is_empty()
        && candidate.chars().count() <= 20
        && candidate
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '+' | '_' | '.'))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|window| window == needle)
}

fn utf8_char_len(byte: u8) -> usize {
    if byte < 0x80 {
        1
    } else if byte < 0xE0 {
        2
    } else if byte < 0xF0 {
        3
    } else {
        4
    }
}

/// Deterministic safety net for the compiler's resolved `retrieval_query` on
/// scoped follow-ups. The effective question for a context-dependent follow-up
/// carries the recovered history subject in its `scope:` segment; the compiler
/// is prompted to fold that subject into `retrieval_query` ({S, R}), but an
/// LLM miss can emit the bare refinement R alone — retrieval then searches a
/// subject-less fragment and drifts off-topic. When the question is scoped and
/// the resolved query shares ZERO normalized tokens with the scope segment,
/// distrust the rewrite and search the full scoped question instead (it holds
/// both S and R verbatim). Plain questions and subject-preserving rewrites are
/// untouched. Structural token overlap only — no language assumptions.
fn guarded_followup_retrieval_question<'a>(resolved: &'a str, question: &'a str) -> &'a str {
    if resolved.trim() == question.trim() {
        return resolved;
    }
    let Some(scope) = crate::services::query::effective_query::scope_segment(question) else {
        return resolved;
    };
    let scope_tokens = crate::shared::text_tokens::normalized_alnum_tokens(scope, 3);
    if scope_tokens.is_empty() {
        return resolved;
    }
    let resolved_tokens = crate::shared::text_tokens::normalized_alnum_tokens(resolved, 3);
    if resolved_tokens.iter().any(|token| scope_tokens.contains(token)) {
        return resolved;
    }
    tracing::warn!(
        stage = "answer.retrieval_query_subject_guard",
        "compiler retrieval_query shares no tokens with the follow-up scope — searching the scoped question instead"
    );
    question
}

fn answer_generation_question<'a>(effective_question: &'a str, user_question: &'a str) -> &'a str {
    let user_question = user_question.trim();
    if !user_question.is_empty() {
        return user_question;
    }
    effective_question.trim()
}

fn literal_revision_can_replace_answer(_draft_answer: &str, revision_answer: &str) -> bool {
    let revision = revision_answer.trim();
    if revision.is_empty() {
        return false;
    }
    if looks_like_internal_effective_query_block(revision) {
        return false;
    }
    true
}

fn enforce_hard_output_boundary(
    execution_id: Uuid,
    source_stage: &'static str,
    query_ir: &QueryIR,
    answer: String,
) -> String {
    let expected_inventory_count =
        query_ir.source_slice.as_ref().and_then(|source_slice| source_slice.count).map(usize::from);
    let Some(trimmed) = strip_trailing_inventory_meta_paragraph(&answer, expected_inventory_count)
    else {
        return answer;
    };
    tracing::info!(
        stage = "answer.hard_boundary_trim",
        %execution_id,
        source_stage,
        trimmed_chars = answer.chars().count().saturating_sub(trimmed.chars().count()),
        "trimmed trailing inventory meta paragraph from generated answer"
    );
    trimmed
}

fn strip_trailing_inventory_meta_paragraph(
    answer: &str,
    expected_inventory_count: Option<usize>,
) -> Option<String> {
    let trimmed = answer.trim_end();
    if trimmed.is_empty() {
        return None;
    }
    let lines = trimmed.lines().collect::<Vec<_>>();
    let mut trailing_end = lines.len();
    while trailing_end > 0 && lines[trailing_end - 1].trim().is_empty() {
        trailing_end -= 1;
    }
    if trailing_end == 0 {
        return None;
    }
    let mut trailing_start = trailing_end - 1;
    while trailing_start > 0 && !lines[trailing_start - 1].trim().is_empty() {
        trailing_start -= 1;
    }
    if trailing_start == 0 || !lines[trailing_start - 1].trim().is_empty() {
        return None;
    }
    let trailing_paragraph = lines[trailing_start..trailing_end]
        .iter()
        .map(|line| line.trim())
        .collect::<Vec<_>>()
        .join(" ");
    if trailing_paragraph.is_empty()
        || trailing_paragraph.chars().count() > 300
        || trailing_paragraph.contains('`')
        || trailing_paragraph.contains("://")
        || trailing_paragraph.contains("```")
        || sentence_terminal_count(&trailing_paragraph) > 2
        || lines[trailing_start..trailing_end]
            .iter()
            .any(|line| is_markdown_inventory_item(line.trim_start()))
    {
        return None;
    }
    let mut previous_end = trailing_start;
    while previous_end > 0 && lines[previous_end - 1].trim().is_empty() {
        previous_end -= 1;
    }
    if previous_end == 0 {
        return None;
    }
    let mut previous_start = previous_end - 1;
    while previous_start > 0 && !lines[previous_start - 1].trim().is_empty() {
        previous_start -= 1;
    }
    if !lines[previous_start..previous_end]
        .iter()
        .any(|line| is_markdown_inventory_item(line.trim_start()))
    {
        return None;
    }
    let list_item_count = markdown_inventory_item_count(&lines[..previous_end]);
    let expected_count_satisfied = expected_inventory_count
        .filter(|expected| *expected > 0)
        .is_some_and(|expected| list_item_count >= expected);
    if !ends_with_question_mark(&trailing_paragraph) && !expected_count_satisfied {
        return None;
    }
    Some(lines[..previous_end].join("\n").trim_end().to_string())
}

fn markdown_inventory_item_count(lines: &[&str]) -> usize {
    lines.iter().filter(|line| is_markdown_inventory_item(line.trim_start())).count()
}

fn is_markdown_inventory_item(line: &str) -> bool {
    let mut chars = line.chars();
    match chars.next() {
        Some('-' | '*' | '•') => chars.next().is_some_and(char::is_whitespace),
        Some(first) if first.is_ascii_digit() => {
            let mut saw_separator = false;
            for ch in chars {
                if ch.is_ascii_digit() {
                    continue;
                }
                saw_separator = matches!(ch, '.' | ')');
                if !saw_separator {
                    return false;
                }
                break;
            }
            saw_separator
                && line
                    .chars()
                    .skip_while(|ch| ch.is_ascii_digit())
                    .nth(1)
                    .is_some_and(char::is_whitespace)
        }
        _ => false,
    }
}

fn ends_with_question_mark(value: &str) -> bool {
    value.trim_end().chars().next_back().is_some_and(|ch| matches!(ch, '?' | '？' | '؟'))
}

fn sentence_terminal_count(value: &str) -> usize {
    value
        .chars()
        .filter(|ch| matches!(ch, '.' | '!' | '?' | '…' | '。' | '！' | '？' | '؟'))
        .count()
}

fn looks_like_internal_effective_query_block(value: &str) -> bool {
    let mut has_scope = false;
    let mut has_question = false;
    let mut has_entities = false;
    let mut line_count = 0usize;
    for line in value.lines().map(str::trim).filter(|line| !line.is_empty()) {
        line_count += 1;
        has_scope |= line.starts_with("scope:");
        has_question |= line.starts_with("question:");
        has_entities |= line.starts_with("entities:");
    }
    has_scope && has_question && (has_entities || line_count <= 4)
}

fn selected_runtime_answer_chunks(
    prepared: &PreparedAnswerQueryResult,
) -> Vec<RuntimeMatchedChunk> {
    let mut seen = HashSet::<Uuid>::new();
    let mut chunks = Vec::<RuntimeMatchedChunk>::new();
    for chunk in prepared
        .structured
        .ordered_source_units
        .iter()
        .chain(prepared.structured.technical_literal_chunks.iter())
        .chain(prepared.structured.context_chunks.iter())
    {
        if seen.insert(chunk.chunk_id) {
            chunks.push(chunk.clone());
        }
    }
    chunks
}

fn selected_runtime_grounding_evidence(
    prepared: &PreparedAnswerQueryResult,
    mut grounding: AssistantGroundingEvidence,
) -> AssistantGroundingEvidence {
    let mut seen = grounding
        .verification_corpus
        .iter()
        .map(|fragment| fragment.trim().to_string())
        .collect::<HashSet<_>>();

    push_grounding_fragment(
        &mut grounding.verification_corpus,
        &mut seen,
        &prepared.structured.context_text,
    );
    if let Some(text) = &prepared.structured.technical_literals_text {
        push_grounding_fragment(&mut grounding.verification_corpus, &mut seen, text);
    }
    for line in &prepared.structured.graph_evidence_context_lines {
        push_grounding_fragment(&mut grounding.verification_corpus, &mut seen, line);
    }
    for chunk in prepared
        .structured
        .ordered_source_units
        .iter()
        .chain(prepared.structured.technical_literal_chunks.iter())
        .chain(prepared.structured.context_chunks.iter())
    {
        push_grounding_fragment(&mut grounding.verification_corpus, &mut seen, &chunk.source_text);
        push_grounding_fragment(&mut grounding.verification_corpus, &mut seen, &chunk.excerpt);
    }
    for document in &prepared.structured.retrieved_documents {
        push_grounding_fragment(
            &mut grounding.verification_corpus,
            &mut seen,
            &document.preview_excerpt,
        );
        push_grounding_fragment(&mut grounding.verification_corpus, &mut seen, &document.title);
        if let Some(document_hint) = &document.document_hint {
            push_grounding_fragment(&mut grounding.verification_corpus, &mut seen, document_hint);
        }
    }
    push_grounding_fragment(
        &mut grounding.verification_corpus,
        &mut seen,
        &prepared.answer_context,
    );
    grounding
}

fn push_grounding_fragment(corpus: &mut Vec<String>, seen: &mut HashSet<String>, fragment: &str) {
    let trimmed = fragment.trim();
    if trimmed.is_empty() {
        return;
    }
    if seen.insert(trimmed.to_string()) {
        corpus.push(trimmed.to_string());
    }
}

fn literal_revision_context(
    prompt_context: &str,
    assistant_grounding: &AssistantGroundingEvidence,
) -> String {
    let mut context = prompt_context.trim().to_string();
    if assistant_grounding.verification_corpus.is_empty() {
        return context;
    }
    if !context.is_empty() {
        context.push_str("\n\n");
    }
    context.push_str("Additional tool evidence observed by the answer generator:\n");
    for (index, fragment) in assistant_grounding.verification_corpus.iter().enumerate() {
        let trimmed = fragment.trim();
        if trimmed.is_empty() {
            continue;
        }
        context.push_str(&format!("\n[TOOL_EVIDENCE {}]\n{}\n", index + 1, trimmed));
    }
    context
}

fn merge_generation_usage(
    mut primary: serde_json::Value,
    additional: &serde_json::Value,
) -> serde_json::Value {
    crate::services::query::agent_loop::merge_usage_into(&mut primary, additional);
    primary
}

fn source_slice_single_shot_min_chars(query_ir: &QueryIR) -> Option<usize> {
    let requested = super::source_slice_requested_count(query_ir)?;
    Some((requested.saturating_mul(48)).max(SINGLE_SHOT_CONFIDENT_ANSWER_CHARS))
}

pub(crate) async fn verify_generated_answer(
    state: &AppState,
    execution_id: Uuid,
    question: &str,
    generation: AnswerGenerationStage,
) -> anyhow::Result<AnswerVerificationStage> {
    let mut verification = verify_answer_against_canonical_evidence(
        question,
        &generation.answer,
        &generation.intent_profile,
        &generation.canonical_evidence,
        &generation.canonical_answer_chunks,
        &generation.prompt_context,
        &generation.assistant_grounding,
    );
    apply_structural_coverage_warning(
        question,
        &generation.answer,
        &generation.query_ir,
        &generation.prompt_context,
        &generation.usage_json,
        &mut verification,
    );
    super::persist_query_verification(
        state,
        execution_id,
        &verification,
        &generation.canonical_evidence,
        &generation.assistant_grounding,
    )
    .await?;

    let has_hallucinated_literal =
        verification.warnings.iter().any(|warning| warning.code == "unsupported_literal");
    let has_unsupported_canonical_claim =
        verification.warnings.iter().any(|warning| warning.code == "unsupported_canonical_claim");
    let verifier_tripped = has_hallucinated_literal || has_unsupported_canonical_claim;

    // Verifier warnings are surfaced as metadata; the grounded answer body
    // is not replaced by a static fallback.
    let verification_level = generation.query_ir.verification_level();
    if verifier_tripped {
        tracing::info!(
            %execution_id,
            ?verification_level,
            warnings = verification.warnings.len(),
            confidence = generation.query_ir.confidence,
            "answer kept despite verification warnings; surfacing via state + warnings only"
        );
    } else if matches!(verification.state, QueryVerificationState::Conflicting) {
        tracing::info!(
            %execution_id,
            "answer kept despite conflicting evidence (verification flag only)"
        );
    }

    Ok(AnswerVerificationStage { generation, verification })
}

fn apply_structural_coverage_warning(
    question: &str,
    answer: &str,
    query_ir: &QueryIR,
    prompt_context: &str,
    usage_json: &serde_json::Value,
    verification: &mut super::RuntimeAnswerVerification,
) {
    if deterministic_generation_skips_structural_coverage_warning(usage_json) {
        return;
    }
    if !answer_omits_structural_context_coverage(question, answer, query_ir, prompt_context) {
        return;
    }
    if verification.warnings.iter().any(|warning| warning.code == "partial_coverage") {
        return;
    }
    verification.warnings.push(QueryVerificationWarning {
        code: "partial_coverage".to_string(),
        message: "Answer omitted structural anchors that were present in retrieved evidence."
            .to_string(),
        related_segment_id: None,
        related_fact_id: None,
    });
    if matches!(verification.state, QueryVerificationState::Verified) {
        verification.state = QueryVerificationState::PartiallySupported;
    }
}

fn deterministic_generation_skips_structural_coverage_warning(
    usage_json: &serde_json::Value,
) -> bool {
    if usage_json.get("deterministic").and_then(serde_json::Value::as_bool) != Some(true) {
        return false;
    }
    matches!(
        AnswerKind::from_usage_json(usage_json),
        Some(
            AnswerKind::SetupConfigurationAnchor
                | AnswerKind::UpdateProcedureSequence
                | AnswerKind::DeterministicGroundedAnswer
        )
    )
}

fn answer_omits_structural_context_coverage(
    question: &str,
    answer: &str,
    query_ir: &QueryIR,
    prompt_context: &str,
) -> bool {
    if !query_ir_needs_structural_coverage_guard(query_ir) {
        return false;
    }
    let anchors = collect_structural_coverage_anchors(question, query_ir, prompt_context);
    if anchors.items.len() < STRUCTURAL_COVERAGE_MIN_CONTEXT_ANCHORS
        || anchors.line_count < STRUCTURAL_COVERAGE_MIN_CONTEXT_ANCHOR_LINES
    {
        return false;
    }
    let answer_lower = answer.to_lowercase();
    let answer_tokens =
        crate::services::query::text_match::normalized_alnum_tokens(&answer_lower, 1);
    let covered = anchors
        .items
        .iter()
        .filter(|anchor| structural_answer_contains_anchor(&answer_lower, &answer_tokens, anchor))
        .take(STRUCTURAL_COVERAGE_MIN_ANSWER_ANCHORS)
        .count();
    covered < STRUCTURAL_COVERAGE_MIN_ANSWER_ANCHORS
}

fn query_ir_needs_structural_coverage_guard(query_ir: &QueryIR) -> bool {
    if matches!(query_ir.act, QueryAct::Compare | QueryAct::Enumerate | QueryAct::Meta) {
        return false;
    }
    let explicit_structural_target = query_ir.target_types.iter().any(|target_type| {
        matches!(
            super::question_intent::canonical_target_type_tag(target_type).as_str(),
            "procedure" | "configuration_file" | "config_key" | "parameter"
        )
    });
    explicit_structural_target || query_ir_is_low_confidence_unfocused_answer(query_ir)
}

fn query_ir_is_low_confidence_unfocused_answer(query_ir: &QueryIR) -> bool {
    query_ir.confidence <= 0.35
        && matches!(query_ir.act, QueryAct::Describe | QueryAct::RetrieveValue)
        && query_ir.source_slice.is_none()
        && query_ir.document_focus.is_none()
        && query_ir.target_types.is_empty()
        && query_ir.target_entities.is_empty()
        && query_ir.literal_constraints.is_empty()
        && query_ir.temporal_constraints.is_empty()
        && query_ir.conversation_refs.is_empty()
}

#[derive(Debug, Default)]
struct StructuralCoverageAnchors {
    items: Vec<String>,
    line_count: usize,
}

fn collect_structural_coverage_anchors(
    question: &str,
    query_ir: &QueryIR,
    prompt_context: &str,
) -> StructuralCoverageAnchors {
    let mut seen = HashSet::<String>::new();
    let mut items = Vec::<String>::new();
    let mut line_count = 0usize;
    let focus_tokens = structural_coverage_focus_tokens(question, query_ir);
    let mut focused_graph_block = false;
    for line in prompt_context.lines() {
        if line.trim_start().starts_with("[graph-evidence") {
            focused_graph_block = structural_coverage_line_matches_focus(&focus_tokens, line);
        }
        let line_matches_focus =
            focused_graph_block || structural_coverage_line_matches_focus(&focus_tokens, line);
        if !line_matches_focus {
            continue;
        }
        let before = items.len();
        push_structural_anchor_literals(strip_evidence_chunk_prefix(line), &mut seen, &mut items);
        if items.len() > before {
            line_count += 1;
        }
        if items.len() >= STRUCTURAL_COVERAGE_MAX_ANCHORS {
            break;
        }
    }
    StructuralCoverageAnchors { items, line_count }
}

fn structural_coverage_focus_tokens(question: &str, query_ir: &QueryIR) -> BTreeSet<String> {
    let mut seen = HashSet::<String>::new();
    let mut tokens = BTreeSet::<String>::new();
    for text in [question, query_ir.effective_retrieval_query(question)] {
        for token in crate::services::query::text_match::normalized_alnum_tokens(
            text,
            STRUCTURAL_COVERAGE_FOCUS_MIN_TOKEN_CHARS,
        ) {
            if seen.insert(token.clone()) {
                tokens.insert(token);
            }
        }
    }
    tokens
}

fn structural_coverage_line_matches_focus(focus_tokens: &BTreeSet<String>, line: &str) -> bool {
    if focus_tokens.is_empty() {
        return true;
    }
    let line_tokens = crate::services::query::text_match::normalized_alnum_tokens(
        line,
        STRUCTURAL_COVERAGE_FOCUS_MIN_TOKEN_CHARS,
    );
    if line_tokens.is_empty() {
        return false;
    }
    crate::services::query::text_match::near_token_overlap_count(focus_tokens, &line_tokens) >= 1
}

fn push_structural_anchor_literals(text: &str, seen: &mut HashSet<String>, out: &mut Vec<String>) {
    for literal in structural_anchor_literal_candidates(text) {
        if out.len() >= STRUCTURAL_COVERAGE_MAX_ANCHORS {
            return;
        }
        let Some(anchor) = normalize_structural_coverage_anchor(&literal) else {
            continue;
        };
        if seen.insert(anchor.clone()) {
            out.push(anchor);
        }
    }
}

fn structural_anchor_literal_candidates(text: &str) -> Vec<String> {
    let mut candidates = Vec::<String>::new();
    candidates.extend(extract_delimited_spans(text, '`', '`'));
    candidates.extend(extract_delimited_spans(text, '"', '"'));
    candidates.extend(extract_delimited_spans(text, '\'', '\''));
    candidates.extend(extract_delimited_spans(text, '«', '»'));
    candidates.extend(extract_delimited_spans(text, '“', '”'));
    candidates.extend(extract_explicit_path_literals(text, 16));
    candidates.extend(extract_parameter_literals(text, 16));
    candidates.extend(extract_config_section_literals(text, 16));
    candidates
}

fn extract_delimited_spans(text: &str, open: char, close: char) -> Vec<String> {
    let mut spans = Vec::new();
    let mut start = None;
    for (index, ch) in text.char_indices() {
        if start.is_none() {
            if ch == open {
                start = Some(index + ch.len_utf8());
            }
            continue;
        }
        if ch == close
            && let Some(open_index) = start.take()
            && open_index <= index
        {
            let value = text[open_index..index].trim();
            if !value.is_empty() {
                spans.push(value.to_string());
            }
        }
    }
    spans
}

fn normalize_structural_coverage_anchor(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_matches(|ch: char| {
        ch.is_whitespace() || matches!(ch, '`' | '"' | '\'' | ',' | ';' | ':' | '.')
    });
    if trimmed.chars().count() > STRUCTURAL_COVERAGE_MAX_ANCHOR_CHARS
        || !trimmed.chars().any(char::is_alphanumeric)
        || structural_coverage_anchor_is_metadata(trimmed)
    {
        return None;
    }
    let token_count = crate::services::query::text_match::normalized_alnum_tokens(trimmed, 1).len();
    if !(1..=8).contains(&token_count) {
        return None;
    }
    Some(trimmed.to_lowercase())
}

fn structural_coverage_anchor_is_metadata(value: &str) -> bool {
    let lowered = value.trim().to_ascii_lowercase();
    if lowered.is_empty() {
        return true;
    }
    if lowered.starts_with("http://")
        || lowered.starts_with("https://")
        || lowered.starts_with("//")
        || lowered.starts_with("confluence:")
        || lowered.contains("://")
    {
        return true;
    }
    matches!(
        lowered.as_str(),
        "answer"
            | "api"
            | "attachment"
            | "bundle"
            | "canonical"
            | "chunk"
            | "chunk_index"
            | "confidence"
            | "coverage"
            | "document"
            | "evidence"
            | "evidence_chunk"
            | "execution"
            | "graph"
            | "graph-evidence"
            | "id"
            | "metadata_block"
            | "mode"
            | "page"
            | "query"
            | "reference_lexical"
            | "request"
            | "resolved"
            | "retrieved"
            | "scope"
            | "source"
            | "strategy"
            | "target"
            | "tool"
    )
}

fn structural_answer_contains_anchor(
    answer_lower: &str,
    answer_tokens: &BTreeSet<String>,
    anchor: &str,
) -> bool {
    let anchor_tokens = crate::services::query::text_match::normalized_alnum_tokens(anchor, 1);
    if anchor_tokens.len() == 1
        && let Some(token) = anchor_tokens.iter().next()
    {
        return answer_tokens.contains(token);
    }
    answer_lower.contains(anchor)
}

/// Structural gate for the release-lane clarify probe: the deterministic
/// latest-version inventory lane fired (either the explicit IR predicate or
/// the context-shape fallback — the same two predicates the lane itself
/// uses), and the compiled query carries no scoping subject.  "No subject"
/// is judged by the lane's own scope extractor (`latest_version_scope_terms`
/// covers target entities, document focus AND non-version literals), so a
/// subject compiled as a literal constraint also suppresses the clarify —
/// the inventory was already scoped to it.
fn subjectless_release_inventory(ir: &QueryIR, context_supports_inventory: bool) -> bool {
    (query_requests_latest_versions(ir) || context_supports_inventory)
        && crate::services::query::latest_versions::latest_version_scope_terms(ir).is_empty()
}

/// A distinct release-evidence subject the clarify probe found: the graph
/// `node_id`, its `label`, and its `node_type` (a member of the closed
/// `RuntimeNodeType` vocabulary). `node_type` becomes the typed candidate
/// `kind` and `node_id` becomes the candidate provenance handle.
pub(crate) struct ReleaseEvidenceEntity {
    pub(crate) node_id: Uuid,
    pub(crate) label: String,
    pub(crate) node_type: String,
}

/// Query `runtime_graph_evidence` for entities that appear across multiple
/// distinct source documents within the given chunk set.  Returns each
/// distinct subject (node id, label, node type) ranked by distinct-document
/// span (descending), filtered to those meeting
/// `RELEASE_CLARIFY_ENTITY_MIN_DOC_SPAN`.  The only semantic filter is on the
/// closed `RuntimeNodeType` vocabulary: `document` nodes are structurally
/// 1:1 with a source document (schema CHECK constraint) and `attribute`
/// nodes are value-properties, so neither can be a standalone subject
/// choice.  No NL keywords, entity names, or language-specific filtering —
/// the cross-document span is the ranking signal.
async fn query_release_evidence_entities(
    state: &AppState,
    library_id: Uuid,
    chunk_ids: &[Uuid],
) -> Result<Vec<ReleaseEvidenceEntity>, sqlx::Error> {
    if chunk_ids.is_empty() {
        return Ok(Vec::new());
    }
    // Build a parameterised ANY($1) query over the chunk_ids slice.
    // sqlx does not support passing Vec<Uuid> as an array to ANY() with
    // the plain `query!` macro when the element count is dynamic, so we
    // use the runtime `query_as` builder with explicit type annotation.
    #[derive(sqlx::FromRow)]
    struct EntityRow {
        node_id: Uuid,
        label: String,
        node_type: String,
        #[allow(dead_code)]
        doc_count: i64,
    }
    let rows = sqlx::query_as::<_, EntityRow>(
        "SELECT n.id AS node_id, n.label, n.node_type, COUNT(DISTINCT e.document_id) AS doc_count
         FROM runtime_graph_node n
         JOIN runtime_graph_evidence e
           ON e.target_id = n.id AND e.target_kind = 'node'
         WHERE e.chunk_id = ANY($1)
           AND e.library_id = $2
           AND n.library_id = $2
           AND n.node_type NOT IN ('document', 'attribute')
         GROUP BY n.id, n.label, n.node_type
         HAVING COUNT(DISTINCT e.document_id) >= $3
         ORDER BY COUNT(DISTINCT e.document_id) DESC, n.support_count DESC
         LIMIT $4",
    )
    .bind(chunk_ids)
    .bind(library_id)
    .bind(RELEASE_CLARIFY_ENTITY_MIN_DOC_SPAN as i64)
    .bind(CLARIFY_MAX_VARIANTS as i64 * 2) // fetch extra for dedup headroom
    .fetch_all(&state.persistence.postgres)
    .await?;

    // Dedup by normalised label (trim + lowercase) to suppress near-duplicates
    // that differ only in case or surrounding whitespace.
    let mut seen: HashSet<String> = HashSet::new();
    let entities = rows
        .into_iter()
        .filter_map(|row| {
            let key = row.label.trim().to_lowercase();
            if seen.insert(key) {
                Some(ReleaseEvidenceEntity {
                    node_id: row.node_id,
                    label: row.label,
                    node_type: row.node_type,
                })
            } else {
                None
            }
        })
        .collect();
    Ok(entities)
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{
        AnswerDisposition, answer_generation_question, append_missing_grounded_requested_labels,
        clarify_variant_dedup_key, classify_answer_disposition,
        classify_answer_disposition_from_evidence, classify_answer_disposition_from_groups,
        extract_query_specific_variants, literal_revision_can_replace_answer,
        literal_revision_targets, provider_free_fallback_query_ir, selected_runtime_answer_chunks,
        selected_runtime_grounding_evidence, strip_trailing_inventory_meta_paragraph,
        structural_direct_answer_candidates, verify_answer_against_canonical_evidence,
    };
    use crate::domains::query::{GroupedReference, GroupedReferenceKind, QueryVerificationState};
    use crate::domains::query_ir::{
        ClarificationReason, ClarificationSpec, ComparisonSpec, ConversationRefKind, DocumentHint,
        EntityMention, EntityRole, LiteralKind, LiteralSpan, QueryAct, QueryIR, QueryLanguage,
        QueryScope, UnresolvedRef,
    };
    use crate::services::query::assistant_grounding::AssistantGroundingEvidence;
    use crate::services::query::execution::RuntimeAnswerVerification;
    use crate::services::query::execution::{
        ConsolidationDiagnostics, FocusReason, RuntimeChunkScoreKind, RuntimeMatchedChunk,
        RuntimeMatchedEntity, RuntimeRetrievedDocumentBrief,
    };

    fn sample_ir(confidence: f32, needs_clarification: Option<ClarificationReason>) -> QueryIR {
        sample_ir_with_act(QueryAct::ConfigureHow, confidence, needs_clarification)
    }

    fn sample_ir_with_act(
        act: QueryAct,
        confidence: f32,
        needs_clarification: Option<ClarificationReason>,
    ) -> QueryIR {
        QueryIR {
            act,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Ru,
            target_types: vec!["procedure".to_string()],
            target_entities: vec![EntityMention {
                label: "workflow module".to_string(),
                role: EntityRole::Subject,
            }],
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: needs_clarification
                .map(|reason| ClarificationSpec { reason, suggestion: String::new() }),
            source_slice: None,
            retrieval_query: None,
            confidence,
        }
    }

    fn setup_anchor_chunk(
        document_id: Uuid,
        label: &str,
        package: &str,
        path: &str,
    ) -> RuntimeMatchedChunk {
        RuntimeMatchedChunk {
            chunk_id: Uuid::new_v4(),
            document_id,
            revision_id: Uuid::new_v4(),
            chunk_index: 0,
            chunk_kind: Some("paragraph".to_string()),
            document_label: label.to_string(),
            excerpt: format!("sample-install {package}\n{path}"),
            score_kind: RuntimeChunkScoreKind::DocumentIdentity,
            score: Some(42.0),
            source_text: format!(
                "sample-install {package}\n{path}\n[Main]\nurl = http://localhost"
            ),
        }
    }

    fn procedure_chunk(label: &str, text: &str) -> RuntimeMatchedChunk {
        RuntimeMatchedChunk {
            chunk_id: Uuid::new_v4(),
            document_id: Uuid::new_v4(),
            revision_id: Uuid::new_v4(),
            chunk_index: 1,
            chunk_kind: Some("paragraph".to_string()),
            document_label: label.to_string(),
            excerpt: text.to_string(),
            score_kind: RuntimeChunkScoreKind::FocusedDocument,
            score: Some(42.0),
            source_text: text.to_string(),
        }
    }

    #[test]
    fn technical_focus_probe_terms_prefer_structural_identifiers() {
        let mut ir = sample_ir_with_act(QueryAct::Describe, 0.8, None);
        ir.target_entities = vec![
            EntityMention { label: "OrderStateMachine".to_string(), role: EntityRole::Subject },
            EntityMention { label: "TransitionHooks".to_string(), role: EntityRole::Object },
        ];
        ir.literal_constraints = vec![LiteralSpan {
            text: "APP_DATABASE_URL".to_string(),
            kind: LiteralKind::Identifier,
        }];

        let terms = super::collect_technical_focus_probe_terms(
            "Which OrderStateMachine methods use TransitionHooks and SAMPLE_LIMIT?",
            &ir,
        );

        assert!(terms.iter().any(|term| term == "APP_DATABASE_URL"));
        assert!(terms.iter().any(|term| term == "OrderStateMachine"));
        assert!(terms.iter().any(|term| term == "TransitionHooks"));
        assert!(terms.iter().any(|term| term == "SAMPLE_LIMIT"));
        assert!(terms.len() <= super::TECHNICAL_FOCUS_PROBE_TERM_LIMIT);
    }

    #[test]
    fn technical_focus_probe_terms_keep_uppercase_numeric_anchors() {
        let ir = sample_ir_with_act(QueryAct::Describe, 0.8, None);

        let terms = super::collect_technical_focus_probe_terms(
            "Terraform CloudWatch alarms CPU 85 RDS 5xx threshold",
            &ir,
        );

        assert!(terms.iter().any(|term| term == "CPU"));
        assert!(terms.iter().any(|term| term == "85"));
        assert!(terms.iter().any(|term| term == "RDS"));
        assert!(terms.iter().any(|term| term == "5xx"));
    }

    #[test]
    fn technical_focus_literal_inventory_preserves_exact_identifiers() {
        let mut seen = std::collections::HashSet::new();
        let focus = vec![
            "error".to_string(),
            "sample".to_string(),
            "cpu".to_string(),
            "circuit".to_string(),
        ];

        let literals = super::extract_focus_aligned_structural_literals(
            "pub enum OrderError { InvalidTransition } class CircuitBreaker: pass SAMPLE_LIMIT_REQUESTS aws_cloudwatch_metric_alarm.cpu",
            &focus,
            &mut seen,
        );

        assert!(literals.iter().any(|literal| literal == "OrderError"));
        assert!(literals.iter().any(|literal| literal == "CircuitBreaker"));
        assert!(literals.iter().any(|literal| literal == "SAMPLE_LIMIT_REQUESTS"));
        assert!(literals.iter().any(|literal| literal == "aws_cloudwatch_metric_alarm.cpu"));
    }

    #[test]
    fn current_question_ir_guard_replaces_stale_history_query() {
        let mut ir = sample_ir_with_act(QueryAct::Compare, 0.97, None);
        ir.retrieval_query = Some(
            "Compare the error handling patterns between the Rust state machine and the Python data pipeline. What error types does each define?"
                .to_string(),
        );
        ir.literal_constraints = vec![LiteralSpan {
            text: "APP_DATABASE_URL".to_string(),
            kind: LiteralKind::Identifier,
        }];

        let guarded = super::guard_self_contained_question_ir(
            "What traits does the Rust state machine implement and what methods do they require?",
            ir,
        );

        assert_eq!(guarded.act, QueryAct::Describe);
        assert_eq!(
            guarded.retrieval_query.as_deref(),
            Some(
                "What traits does the Rust state machine implement and what methods do they require?"
            )
        );
        assert!(
            guarded
                .literal_constraints
                .iter()
                .all(|literal| literal.text != "APP_DATABASE_URL" && literal.text != "AppConfig")
        );
    }

    #[test]
    fn current_question_ir_guard_preserves_short_elliptic_followup() {
        let mut ir = sample_ir_with_act(QueryAct::Describe, 0.91, None);
        ir.retrieval_query = Some("Rust state machine timeout behavior".to_string());

        let guarded = super::guard_self_contained_question_ir("What about timeout?", ir);

        assert_eq!(guarded.retrieval_query.as_deref(), Some("Rust state machine timeout behavior"));
    }

    #[test]
    fn current_question_ir_guard_rejects_previous_turn_with_generic_overlap() {
        let mut ir = sample_ir_with_act(QueryAct::Describe, 0.95, None);
        ir.retrieval_query = Some(
            "What are the valid task status transitions defined in the TypeScript GraphQL schema? Can a task go directly from BACKLOG to DONE?"
                .to_string(),
        );

        let guarded = super::guard_self_contained_question_ir(
            "How does the Python data pipeline implement the circuit breaker pattern? What states does it have and when does it open?",
            ir,
        );

        assert_eq!(
            guarded.retrieval_query.as_deref(),
            Some(
                "How does the Python data pipeline implement the circuit breaker pattern? What states does it have and when does it open?"
            )
        );
    }

    #[test]
    fn current_question_ir_guard_trims_history_tail_even_when_focus_is_preserved() {
        let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.92, None);
        ir.target_types =
            vec!["artifact".to_string(), "version".to_string(), "procedure".to_string()];
        ir.target_entities = vec![
            EntityMention { label: "Stale Subject".to_string(), role: EntityRole::Subject },
            EntityMention {
                label: "Stale Adjacent Subject".to_string(),
                role: EntityRole::Subject,
            },
            EntityMention { label: "Sample Target".to_string(), role: EntityRole::Object },
        ];
        ir.literal_constraints = vec![LiteralSpan {
            text: "/var/log/alpha/alpha-node/migrate.log".to_string(),
            kind: LiteralKind::Path,
        }];
        ir.retrieval_query = Some(
            "how to update Stale Subject Stale Adjacent Subject how to update Sample Target version?"
                .to_string(),
        );

        let guarded =
            super::guard_self_contained_question_ir("how to update Sample Target version?", ir);

        assert_eq!(
            guarded.retrieval_query.as_deref(),
            Some("how to update Sample Target version?")
        );
        assert!(guarded.target_entities.iter().any(|entity| entity.label == "Sample Target"));
        assert!(
            guarded.target_entities.iter().all(|entity| entity.label != "Stale Subject"
                && entity.label != "Stale Adjacent Subject")
        );
        assert!(guarded.literal_constraints.is_empty());
    }

    #[test]
    fn current_question_ir_guard_prunes_stale_constraints_for_short_entity_followup() {
        let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.92, None);
        ir.target_types =
            vec!["procedure".to_string(), "artifact".to_string(), "config_key".to_string()];
        ir.target_entities = vec![
            EntityMention { label: "Subject Alpha".to_string(), role: EntityRole::Subject },
            EntityMention { label: "Subject Gamma".to_string(), role: EntityRole::Subject },
        ];
        ir.literal_constraints = vec![
            LiteralSpan {
                text: "subject-alpha-artifact".to_string(),
                kind: LiteralKind::Identifier,
            },
            LiteralSpan { text: "/etc/subject-alpha.ini".to_string(), kind: LiteralKind::Path },
        ];
        ir.retrieval_query = Some("how to configure workflow adapter Gamma".to_string());

        let guarded = super::guard_self_contained_question_ir("Gamma", ir);

        assert_eq!(guarded.retrieval_query.as_deref(), Some("Gamma"));
        assert_eq!(guarded.target_entities.len(), 1);
        assert_eq!(guarded.target_entities[0].label, "Subject Gamma");
        assert!(guarded.literal_constraints.is_empty());
    }

    #[test]
    fn current_question_ir_guard_trims_technical_history_tail_for_short_entity_followup() {
        let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.92, None);
        ir.target_types =
            vec!["procedure".to_string(), "artifact".to_string(), "config_key".to_string()];
        ir.target_entities =
            vec![EntityMention { label: "Subject Gamma".to_string(), role: EntityRole::Subject }];
        ir.retrieval_query = Some(
            "how to configure workflow adapter Subject Alpha subject-alpha-artifact /etc/subject-alpha.ini Gamma"
                .to_string(),
        );

        let guarded = super::guard_self_contained_question_ir("Gamma", ir);

        assert_eq!(guarded.retrieval_query.as_deref(), Some("Gamma"));
        assert_eq!(guarded.target_entities.len(), 1);
        assert_eq!(guarded.target_entities[0].label, "Subject Gamma");
    }

    #[test]
    fn current_question_ir_guard_clears_history_only_constraints_for_self_contained_question() {
        let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.94, None);
        ir.target_types = vec!["concept".to_string()];
        ir.target_entities = vec![
            EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject },
            EntityMention { label: "Alpha Worker".to_string(), role: EntityRole::Subject },
        ];
        ir.literal_constraints = vec![
            LiteralSpan { text: "alpha-console".to_string(), kind: LiteralKind::Identifier },
            LiteralSpan { text: "/etc/alpha/console.toml".to_string(), kind: LiteralKind::Path },
        ];
        ir.retrieval_query = Some("How to configure ABC?".to_string());

        let guarded = super::guard_self_contained_question_ir("How to configure ABC?", ir);

        assert_eq!(guarded.retrieval_query.as_deref(), Some("How to configure ABC?"));
        assert!(guarded.target_entities.is_empty());
        assert!(guarded.literal_constraints.is_empty());
    }

    #[test]
    fn current_question_ir_guard_rejects_internal_history_query_block() {
        let mut ir = sample_ir_with_act(QueryAct::Describe, 0.95, None);
        ir.retrieval_query = Some(
            "scope: Which systems in this corpus use PostgreSQL and how do they configure the connection?\n\
entities: These, PostgreSQL, APP_DATABASE_URL, AppConfig\n\
literal anchors: `APP_DATABASE_URL`, `AppConfig`\n\
question: Compare the error handling patterns between the Rust state machine and the Python data pipeline. What error types does each define?"
                .to_string(),
        );

        let guarded = super::guard_self_contained_question_ir(
            "Compare the error handling patterns between the Rust state machine and the Python data pipeline. What error types does each define?",
            ir,
        );

        assert_eq!(
            guarded.retrieval_query.as_deref(),
            Some(
                "Compare the error handling patterns between the Rust state machine and the Python data pipeline. What error types does each define?"
            )
        );
        assert!(
            guarded
                .literal_constraints
                .iter()
                .all(|literal| literal.text != "APP_DATABASE_URL" && literal.text != "AppConfig")
        );
    }

    #[test]
    fn current_question_ir_guard_keeps_typed_focus_when_retrieval_query_is_history_block() {
        let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.93, None);
        ir.target_types =
            vec!["artifact".to_string(), "version".to_string(), "procedure".to_string()];
        ir.target_entities = vec![EntityMention {
            label: "Alpha Control Console".to_string(),
            role: EntityRole::Subject,
        }];
        ir.literal_constraints = vec![LiteralSpan {
            text: "/etc/alpha/worker.toml".to_string(),
            kind: LiteralKind::Path,
        }];
        ir.retrieval_query = Some(
            "scope: How does the previous workflow adapter store callbacks?\n\
entities: Legacy Adapter, callbackToken, /etc/alpha/worker.toml\n\
question: How do I update Alpha Control Console?"
                .to_string(),
        );

        let guarded =
            super::guard_self_contained_question_ir("How do I update Alpha Control Console?", ir);

        assert_eq!(guarded.act, QueryAct::ConfigureHow);
        assert_eq!(
            guarded.retrieval_query.as_deref(),
            Some("How do I update Alpha Control Console?")
        );
        assert!(guarded.target_types.iter().any(|target_type| target_type == "procedure"));
        assert!(
            guarded.target_entities.iter().any(|entity| entity.label == "Alpha Control Console")
        );
        assert!(guarded.literal_constraints.is_empty());
        assert!(guarded.confidence > 0.6);
    }

    #[test]
    fn current_question_ir_guard_extracts_current_question_from_effective_query_block() {
        let mut ir = sample_ir_with_act(QueryAct::Describe, 0.95, None);
        ir.retrieval_query = Some(
            "scope: What are the valid task status transitions defined in the TypeScript GraphQL schema?\n\
entities: BACKLOG, TODO, DONE\n\
question: How does the Python data pipeline implement the circuit breaker pattern? What states does it have and when does it open?"
                .to_string(),
        );

        let guarded = super::guard_self_contained_question_ir(
            "scope: What are the valid task status transitions defined in the TypeScript GraphQL schema?\n\
entities: BACKLOG, TODO, DONE\n\
question: How does the Python data pipeline implement the circuit breaker pattern? What states does it have and when does it open?",
            ir,
        );

        assert_eq!(
            guarded.retrieval_query.as_deref(),
            Some(
                "How does the Python data pipeline implement the circuit breaker pattern? What states does it have and when does it open?"
            )
        );
    }

    #[test]
    fn configure_how_ir_guard_keeps_metadata_only_targets_structural() {
        let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.93, None);
        ir.target_types = vec!["release".to_string(), "version".to_string()];
        ir.retrieval_query = Some("How do I update Subject Alpha?".to_string());

        let guarded = super::guard_self_contained_question_ir("How do I update Subject Alpha?", ir);

        assert_eq!(guarded.act, QueryAct::ConfigureHow);
        assert_eq!(guarded.retrieval_query.as_deref(), Some("How do I update Subject Alpha?"));
        assert_eq!(guarded.target_types, vec!["release".to_string(), "version".to_string()]);
        assert!(guarded.source_slice.is_none());
    }

    #[test]
    fn configure_how_ir_guard_adds_document_to_procedure_revision_targets() {
        let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.93, None);
        ir.target_types = vec!["procedure".to_string(), "release".to_string()];
        ir.retrieval_query = Some("How do I update Subject Alpha?".to_string());

        let guarded = super::guard_self_contained_question_ir("How do I update Subject Alpha?", ir);

        assert_eq!(
            guarded.target_types,
            vec!["procedure".to_string(), "release".to_string(), "document".to_string()]
        );
        assert!(guarded.source_slice.is_none());
    }

    #[test]
    fn configure_how_ir_guard_adds_procedure_document_to_artifact_version_targets() {
        let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.93, None);
        ir.target_types = vec!["artifact".to_string(), "version".to_string()];
        ir.retrieval_query = Some("How do I update Subject Alpha?".to_string());

        let guarded = super::guard_self_contained_question_ir("How do I update Subject Alpha?", ir);

        assert_eq!(
            guarded.target_types,
            vec![
                "artifact".to_string(),
                "version".to_string(),
                "procedure".to_string(),
                "document".to_string()
            ]
        );
        assert!(guarded.source_slice.is_none());
    }

    #[test]
    fn configure_how_ir_guard_keeps_setup_configuration_targets() {
        let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.93, None);
        ir.target_types = vec!["configuration_file".to_string(), "config_key".to_string()];
        ir.retrieval_query = Some("How do I configure Subject Alpha?".to_string());

        let guarded =
            super::guard_self_contained_question_ir("How do I configure Subject Alpha?", ir);

        assert_eq!(guarded.target_types, vec!["configuration_file", "config_key"]);
        assert!(guarded.source_slice.is_none());
    }

    #[test]
    fn source_slice_scope_guard_clears_history_subject_from_subjectless_inventory() {
        let mut ir = sample_ir_with_act(QueryAct::Enumerate, 0.92, None);
        ir.target_types = vec!["release".to_string(), "version".to_string()];
        ir.target_entities = vec![EntityMention {
            label: "Alpha Receipt Upload".to_string(),
            role: EntityRole::Subject,
        }];
        ir.literal_constraints = vec![LiteralSpan {
            text: "alphaReceiptUploadToken".to_string(),
            kind: LiteralKind::Identifier,
        }];
        ir.retrieval_query = Some("Alpha Receipt Upload latest 10 releases".to_string());
        ir.source_slice = Some(crate::domains::query_ir::SourceSliceSpec {
            count: Some(10),
            filter: crate::domains::query_ir::SourceSliceFilter::ReleaseMarker,
            direction: crate::domains::query_ir::SourceSliceDirection::Tail,
        });

        let guarded =
            super::guard_self_contained_question_ir("What is new in the latest 10 releases?", ir);

        assert!(guarded.source_slice.is_some());
        assert!(guarded.target_entities.is_empty());
        assert!(guarded.literal_constraints.is_empty());
        assert!(guarded.document_focus.is_none());
        assert_eq!(
            guarded.retrieval_query.as_deref(),
            Some("What is new in the latest 10 releases?")
        );
    }

    #[test]
    fn source_slice_scope_guard_clears_short_history_subject_from_subjectless_inventory() {
        let mut ir = sample_ir_with_act(QueryAct::Enumerate, 0.92, None);
        ir.target_types = vec!["release".to_string(), "version".to_string()];
        ir.target_entities =
            vec![EntityMention { label: "AX".to_string(), role: EntityRole::Subject }];
        ir.retrieval_query = Some("AX latest 10 releases".to_string());
        ir.source_slice = Some(crate::domains::query_ir::SourceSliceSpec {
            count: Some(10),
            filter: crate::domains::query_ir::SourceSliceFilter::ReleaseMarker,
            direction: crate::domains::query_ir::SourceSliceDirection::Tail,
        });

        let guarded =
            super::guard_self_contained_question_ir("What is new in the latest 10 releases?", ir);

        assert!(guarded.source_slice.is_some());
        assert!(guarded.target_entities.is_empty());
        assert_eq!(
            guarded.retrieval_query.as_deref(),
            Some("What is new in the latest 10 releases?")
        );
    }

    #[test]
    fn source_slice_scope_guard_keeps_explicit_short_inventory_subject() {
        let mut ir = sample_ir_with_act(QueryAct::Enumerate, 0.92, None);
        ir.target_types = vec!["release".to_string(), "version".to_string()];
        ir.target_entities =
            vec![EntityMention { label: "AX".to_string(), role: EntityRole::Subject }];
        ir.retrieval_query = Some("AX latest 10 releases".to_string());
        ir.source_slice = Some(crate::domains::query_ir::SourceSliceSpec {
            count: Some(10),
            filter: crate::domains::query_ir::SourceSliceFilter::ReleaseMarker,
            direction: crate::domains::query_ir::SourceSliceDirection::Tail,
        });

        let guarded = super::guard_self_contained_question_ir(
            "What is new in the latest 10 AX releases?",
            ir,
        );

        assert_eq!(guarded.target_entities.len(), 1);
        assert_eq!(guarded.target_entities[0].label, "AX");
    }

    #[test]
    fn source_slice_scope_guard_keeps_explicit_inventory_subject() {
        let mut ir = sample_ir_with_act(QueryAct::Enumerate, 0.92, None);
        ir.target_types = vec!["release".to_string(), "version".to_string()];
        ir.target_entities =
            vec![EntityMention { label: "Alpha Gateway".to_string(), role: EntityRole::Subject }];
        ir.retrieval_query = Some("Alpha Gateway latest 10 releases".to_string());
        ir.source_slice = Some(crate::domains::query_ir::SourceSliceSpec {
            count: Some(10),
            filter: crate::domains::query_ir::SourceSliceFilter::ReleaseMarker,
            direction: crate::domains::query_ir::SourceSliceDirection::Tail,
        });

        let guarded = super::guard_self_contained_question_ir(
            "What is new in the latest 10 Alpha Gateway releases?",
            ir,
        );

        assert_eq!(guarded.target_entities.len(), 1);
        assert_eq!(guarded.target_entities[0].label, "Alpha Gateway");
    }

    #[test]
    fn source_slice_guard_clears_ordinary_version_inventory_question() {
        let mut ir = sample_ir_with_act(QueryAct::Enumerate, 0.95, None);
        ir.target_types = vec!["document".to_string()];
        ir.source_slice = Some(crate::domains::query_ir::SourceSliceSpec {
            count: Some(10),
            filter: crate::domains::query_ir::SourceSliceFilter::ReleaseMarker,
            direction: crate::domains::query_ir::SourceSliceDirection::Tail,
        });
        ir.retrieval_query = Some("Which subjects mention version-shaped identifiers?".to_string());

        let guarded = super::guard_self_contained_question_ir(
            "Which subjects mention version-shaped identifiers?",
            ir,
        );

        assert!(guarded.source_slice.is_none());
    }

    #[test]
    fn exact_literal_postprocessor_adds_only_focus_aligned_missing_identifiers() {
        let ir = sample_ir_with_act(QueryAct::Describe, 0.8, None);
        let answer = super::append_missing_focus_aligned_exact_literals(
            "The module defines concrete error types.".to_string(),
            "What error types does the module define?",
            &ir,
            "pub enum OrderError { InvalidTransition } DATABASE_URL SAMPLE_LIMIT_REQUESTS",
        );

        assert!(answer.contains("OrderError"));
        assert!(!answer.contains("DATABASE_URL"));
        assert!(!answer.contains("SAMPLE_LIMIT_REQUESTS"));
    }

    #[test]
    fn exact_literal_postprocessor_rejects_unrelated_context_identifiers() {
        let ir = sample_ir_with_act(QueryAct::Compare, 0.8, None);
        let answer = super::append_missing_focus_aligned_exact_literals(
            "The service uses bearer authentication.".to_string(),
            "Compare the authentication mechanisms and token format.",
            &ir,
            "Authorization: Bearer <token>\nOPERATOR_NAME=alpha\nPAYMENT_TIMEOUT_SECONDS=30",
        );

        assert!(!answer.contains("OPERATOR_NAME"));
        assert!(!answer.contains("PAYMENT_TIMEOUT_SECONDS"));
    }

    #[test]
    fn exact_literal_postprocessor_keeps_short_uppercase_focus_terms() {
        let ir = sample_ir_with_act(QueryAct::Describe, 0.8, None);
        let answer = super::append_missing_focus_aligned_exact_literals(
            "The alarms include memory thresholds.".to_string(),
            "What CloudWatch alarms cover CPU and memory thresholds?",
            &ir,
            "resource \"aws_cloudwatch_metric_alarm\" \"ecs_cpu_high\" { metric_name = \"CPUUtilization\" }",
        );

        assert!(answer.contains("CPUUtilization"));
    }

    #[test]
    fn exact_literal_postprocessor_adds_focus_aligned_uppercase_config_literals() {
        let ir = sample_ir_with_act(QueryAct::Describe, 0.8, None);
        let answer = super::append_missing_focus_aligned_exact_literals(
            "The limiter uses request and window fields.".to_string(),
            "How is retry limiting configured with RETRY_LIMIT?",
            &ir,
            "The setting is controlled by RETRY_LIMIT_REQUESTS and RETRY_LIMIT_WINDOW_SECONDS. how... limiting. configuration.",
        );

        assert!(answer.contains("RETRY_LIMIT_REQUESTS"), "{answer}");
        assert!(answer.contains("RETRY_LIMIT_WINDOW_SECONDS"), "{answer}");
        assert!(!answer.contains("how..."), "{answer}");
        assert!(!answer.contains("limiting."), "{answer}");
        assert!(!answer.contains("configuration."), "{answer}");
    }

    #[test]
    fn exact_literal_postprocessor_prefers_structural_focus_expansions() {
        let ir = sample_ir_with_act(QueryAct::Describe, 0.8, None);
        let answer = super::append_missing_focus_aligned_exact_literals(
            "The setting uses request and window fields.".to_string(),
            "Which retry limit configuration controls throttling?",
            &ir,
            "retry_limit_requests=100\nretry_limit_window_seconds=60\nApiError\ncomponent_id",
        );

        assert!(answer.contains("retry_limit_requests"), "{answer}");
        assert!(answer.contains("retry_limit_window_seconds"), "{answer}");
        assert!(!answer.contains("ApiError"), "{answer}");
        assert!(!answer.contains("component_id"), "{answer}");
    }

    #[test]
    fn exact_literal_postprocessor_recovers_exact_form_from_answer_identifier() {
        let ir = sample_ir_with_act(QueryAct::Describe, 0.8, None);
        let answer = super::append_missing_focus_aligned_exact_literals(
            "The component uses SampleLimitRequests and SampleLimitWindowSeconds.".to_string(),
            "Which controls are used by the component?",
            &ir,
            "SAMPLE_LIMIT_REQUESTS=100\nSAMPLE_LIMIT_WINDOW_SECONDS=60\nAPI.",
        );

        assert!(answer.contains("SAMPLE_LIMIT_REQUESTS"), "{answer}");
        assert!(answer.contains("SAMPLE_LIMIT_WINDOW_SECONDS"), "{answer}");
        assert!(!answer.contains("`API.`"), "{answer}");
    }

    #[test]
    fn exact_literal_postprocessor_recovers_exact_form_from_field_comment_context() {
        let ir = sample_ir_with_act(QueryAct::Compare, 0.8, None);
        let answer = super::append_missing_focus_aligned_exact_literals(
            "The component uses SampleLimitRequests and SampleLimitWindowSeconds.".to_string(),
            "How do Alpha and Beta implement sample limiting? What configuration controls the limits?",
            &ir,
            concat!(
                "// SAMPLE_LIMIT_REQUESTS - Max requests per window. Default: 100\n",
                "SampleLimitRequests int\n",
                "SampleLimitRequests: envInt(\"SAMPLE_LIMIT_REQUESTS\", 100)\n",
                "// SAMPLE_LIMIT_WINDOW_SECONDS - Window duration in seconds. Default: 60\n",
                "SampleLimitWindowSeconds int\n",
                "SampleLimitWindowSeconds: envInt(\"SAMPLE_LIMIT_WINDOW_SECONDS\", 60)"
            ),
        );

        assert!(answer.contains("SAMPLE_LIMIT_REQUESTS"), "{answer}");
        assert!(answer.contains("SAMPLE_LIMIT_WINDOW_SECONDS"), "{answer}");
    }

    #[test]
    fn exact_literal_postprocessor_recovers_exact_form_from_inflected_focus_terms() {
        let ir = sample_ir_with_act(QueryAct::Compare, 0.8, None);
        let answer = super::append_missing_focus_aligned_exact_literals(
            "The component uses request and window fields.".to_string(),
            "How do Alpha and Beta implement sample limiting? What controls the limits?",
            &ir,
            concat!(
                "SampleLimitRequests int\n",
                "SAMPLE_LIMIT_REQUESTS=100\n",
                "SampleLimitWindowSeconds int\n",
                "SAMPLE_LIMIT_WINDOW_SECONDS=60\n",
                "UnrelatedController\n",
                "UNRELATED_TIMEOUT_SECONDS=30"
            ),
        );

        assert!(answer.contains("SAMPLE_LIMIT_REQUESTS"), "{answer}");
        assert!(answer.contains("SAMPLE_LIMIT_WINDOW_SECONDS"), "{answer}");
        assert!(!answer.contains("UNRELATED_TIMEOUT_SECONDS"), "{answer}");
    }

    #[test]
    fn exact_literal_postprocessor_skips_path_literals_without_path_focus() {
        let ir = sample_ir_with_act(QueryAct::Describe, 0.8, None);
        let answer = super::append_missing_focus_aligned_exact_literals(
            "`accounts` columns:\n- `email`: type `TEXT`".to_string(),
            "What columns does the accounts table have?",
            &ir,
            "Endpoint /accounts lists account records. Table: accounts. Column: email.",
        );

        assert!(!answer.contains("`/accounts`"), "{answer}");
    }

    #[test]
    fn exact_literal_postprocessor_skips_title_case_hyphen_labels() {
        let ir = sample_ir_with_act(QueryAct::Describe, 0.8, None);
        let answer = super::append_missing_focus_aligned_exact_literals(
            "The document describes account columns.".to_string(),
            "What columns does the account table have in the Example-Commerce schema?",
            &ir,
            "Example-Commerce Schema\nACCOUNT_ID\naccount table columns",
        );

        assert!(answer.contains("ACCOUNT_ID"), "{answer}");
        assert!(!answer.contains("`Example-Commerce`"), "{answer}");
    }

    #[test]
    fn appends_only_grounded_missing_target_entities() {
        let mut ir = sample_ir_with_act(QueryAct::RetrieveValue, 0.9, None);
        ir.target_entities = vec![
            EntityMention { label: "Beacon Node".to_string(), role: EntityRole::Subject },
            EntityMention { label: "Harbor Delta".to_string(), role: EntityRole::Object },
            EntityMention { label: "Unseen Node".to_string(), role: EntityRole::Object },
        ];

        let answer = append_missing_grounded_requested_labels(
            "The transition is Beacon Harbor Transition.".to_string(),
            &ir,
            "Which transition joins Beacon Node to Harbor Delta?",
            "Beacon Node emits Beacon Harbor Transition. Harbor Delta receives it.",
            &[],
        );

        assert!(answer.contains("Beacon Node"));
        assert!(answer.contains("Harbor Delta"));
        assert!(!answer.contains("Unseen Node"));
    }

    #[test]
    fn appends_grounded_resolved_target_entities_not_literal_in_question() {
        let mut ir = sample_ir_with_act(QueryAct::RetrieveValue, 0.9, None);
        ir.target_entities = vec![
            EntityMention { label: "Beacon Node".to_string(), role: EntityRole::Subject },
            EntityMention { label: "Harbor Delta".to_string(), role: EntityRole::Object },
        ];

        let answer = append_missing_grounded_requested_labels(
            "The transition is Beacon Harbor Transition.".to_string(),
            &ir,
            "Which transition joins these endpoints?",
            "Beacon Node emits Beacon Harbor Transition. Harbor Delta receives it.",
            &[],
        );

        assert!(answer.contains("Beacon Node"));
        assert!(answer.contains("Harbor Delta"));
    }

    #[test]
    fn appends_grounded_graph_entities_without_ir_targets() {
        let mut ir = sample_ir_with_act(QueryAct::RetrieveValue, 0.9, None);
        ir.target_entities.clear();
        let graph_entities = vec![
            RuntimeMatchedEntity {
                node_id: Uuid::nil(),
                label: "Beacon Node".to_string(),
                node_type: "entity".to_string(),
                summary: None,
                score: Some(1.0),
            },
            RuntimeMatchedEntity {
                node_id: Uuid::nil(),
                label: "Harbor Delta".to_string(),
                node_type: "entity".to_string(),
                summary: None,
                score: Some(1.0),
            },
            RuntimeMatchedEntity {
                node_id: Uuid::nil(),
                label: "Delta Archive".to_string(),
                node_type: "entity".to_string(),
                summary: None,
                score: Some(1.0),
            },
        ];

        let answer = append_missing_grounded_requested_labels(
            "The transition is Beacon Harbor Transition.".to_string(),
            &ir,
            "Which transition joins Beacon Node to Harbor Delta, and which archive records it?",
            "Beacon Node emits Beacon Harbor Transition. Harbor Delta receives it.",
            &graph_entities,
        );

        assert!(answer.contains("Beacon Node"));
        assert!(answer.contains("Harbor Delta"));
        assert!(!answer.contains("Delta Archive"));
    }

    fn sample_ir_with_two_target_entities(
        act: QueryAct,
        confidence: f32,
        needs_clarification: Option<ClarificationReason>,
    ) -> QueryIR {
        let mut ir = sample_ir_with_act(act, confidence, needs_clarification);
        ir.target_entities = vec![
            EntityMention { label: "Sample Subject".to_string(), role: EntityRole::Subject },
            EntityMention { label: "workflows".to_string(), role: EntityRole::Object },
        ];
        ir
    }

    fn sample_groups() -> Vec<GroupedReference> {
        vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "Provider A configuration".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec![
                    "chunk:1".to_string(),
                    "chunk:2".to_string(),
                    "chunk:3".to_string(),
                ],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "Provider B configuration".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec![
                    "chunk:4".to_string(),
                    "chunk:5".to_string(),
                    "chunk:6".to_string(),
                ],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "Provider C configuration".to_string(),
                excerpt: None,
                evidence_count: 2,
                support_ids: vec!["chunk:7".to_string(), "chunk:8".to_string()],
            },
        ]
    }

    #[test]
    fn answer_generation_question_prefers_original_user_text() {
        assert_eq!(
            answer_generation_question(
                "scope: prior answer\nentities: alpha-one, alpha-two\nquestion: describe each",
                "describe each",
            ),
            "describe each"
        );
        assert_eq!(answer_generation_question("compiled focus", "  "), "compiled focus");
    }

    #[test]
    fn literal_revision_rejects_internal_query_block_replacement() {
        let draft = "- `alpha-one` — supported item.\n- `alpha-two` — supported item.";
        let revision =
            "scope: prior answer\nentities: alpha-one, alpha-two\nquestion: describe each";

        assert!(!literal_revision_can_replace_answer(draft, revision));
        assert!(literal_revision_can_replace_answer(
            draft,
            "- `alpha-one` — supported item.\n- `alpha-two` — supported item."
        ));
    }

    #[test]
    fn literal_revision_allows_removing_unsupported_synthetic_code_blocks() {
        let draft = format!(
            "{}\n\n```ini\n[Generated]\nalpha = <value>\nbeta = <value>\ngamma = <value>\n```\n",
            "This setup uses the Alpha connector. ".repeat(24)
        );
        let revision = "This setup uses the Alpha connector. The evidence provides the parameter names but not a complete ready configuration file.";

        assert!(literal_revision_can_replace_answer(&draft, revision));
    }

    #[test]
    fn strips_trailing_interrogative_meta_paragraph_after_numbered_inventory() {
        let answer = "1. Alpha item\n2. Beta item\n\nMore detail?";

        assert_eq!(
            strip_trailing_inventory_meta_paragraph(answer, None).as_deref(),
            Some("1. Alpha item\n2. Beta item")
        );
    }

    #[test]
    fn strips_meta_paragraph_after_multiline_inventory_item() {
        let answer =
            "1. Alpha item\n   Source: alpha.md\n2. Beta item\n   Source: beta.md\n\nMore detail?";

        assert_eq!(
            strip_trailing_inventory_meta_paragraph(answer, None).as_deref(),
            Some("1. Alpha item\n   Source: alpha.md\n2. Beta item\n   Source: beta.md")
        );
    }

    #[test]
    fn strips_short_meta_paragraph_when_requested_inventory_count_is_satisfied() {
        let answer = "1. Alpha item\n2. Beta item\n\nOverall theme: Alpha and Beta changes.";

        assert_eq!(
            strip_trailing_inventory_meta_paragraph(answer, Some(2)).as_deref(),
            Some("1. Alpha item\n2. Beta item")
        );
    }

    #[test]
    fn strips_trailing_interrogative_meta_paragraph_after_bulleted_inventory() {
        let answer = "- Alpha item\n- Beta item\n\nContinue？";

        assert_eq!(
            strip_trailing_inventory_meta_paragraph(answer, None).as_deref(),
            Some("- Alpha item\n- Beta item")
        );
    }

    #[test]
    fn preserves_trailing_coverage_limit_statement() {
        let answer =
            "1. Alpha item\n2. Beta item\n\nOnly the first two matching items were grounded.";

        assert!(strip_trailing_inventory_meta_paragraph(answer, None).is_none());
    }

    #[test]
    fn preserves_non_inventory_question_answer() {
        let answer = "The selected evidence contains two matching items. Which one is relevant?";

        assert!(strip_trailing_inventory_meta_paragraph(answer, None).is_none());
    }

    #[test]
    fn preserves_trailing_technical_paragraph() {
        let answer = "1. Alpha item\n2. Beta item\n\nCheck `alpha --status`?";

        assert!(strip_trailing_inventory_meta_paragraph(answer, Some(2)).is_none());
    }

    #[test]
    fn literal_revision_targets_include_fenced_block_for_unsupported_fenced_line() {
        let answer = "Supported facts:\n\n```ini\n[Alpha]\nendpoint = https://alpha.example\nsecretKey = <secret>\n```\n\nDone.";
        let targets = literal_revision_targets(answer, &["secretKey = <secret>".to_string()]);

        assert_eq!(targets[0], "secretKey = <secret>");
        assert!(targets.iter().any(|target| {
            target == "[Alpha]\nendpoint = https://alpha.example\nsecretKey = <secret>"
        }));
    }

    #[test]
    fn literal_revision_targets_do_not_duplicate_existing_targets() {
        let block = "[Alpha]\nsecretKey = <secret>";
        let answer = format!("```ini\n{block}\n```");
        let targets = literal_revision_targets(&answer, &[block.to_string()]);

        assert_eq!(targets, vec![block.to_string()]);
    }

    fn retrieved_doc(title: &str, document_hint: &str) -> RuntimeRetrievedDocumentBrief {
        RuntimeRetrievedDocumentBrief {
            title: title.to_string(),
            preview_excerpt: String::new(),
            document_hint: Some(document_hint.to_string()),
        }
    }

    fn prepared_for_single_shot(query_ir: QueryIR) -> super::PreparedAnswerQueryResult {
        use crate::domains::query::{
            ContextAssemblyMetadata, ContextAssemblyStatus, IntentKeywords, QueryIntentCacheStatus,
            QueryPlanningMetadata, RerankMetadata, RerankStatus, RuntimeQueryMode,
        };

        super::PreparedAnswerQueryResult {
            structured: super::super::types::RuntimeStructuredQueryResult {
                planned_mode: RuntimeQueryMode::Hybrid,
                embedding_usage: None,
                intent_profile: Default::default(),
                context_text: "context-fragment-a".to_string(),
                technical_literals_text: None,
                technical_literal_chunks: Vec::new(),
                diagnostics: super::super::types::RuntimeStructuredQueryDiagnostics {
                    requested_mode: RuntimeQueryMode::Hybrid,
                    planned_mode: RuntimeQueryMode::Hybrid,
                    keywords: Vec::new(),
                    high_level_keywords: Vec::new(),
                    low_level_keywords: Vec::new(),
                    top_k: 8,
                    reference_counts: super::super::types::RuntimeStructuredQueryReferenceCounts {
                        entity_count: 0,
                        relationship_count: 0,
                        chunk_count: 1,
                        graph_node_count: 0,
                        graph_edge_count: 0,
                    },
                    planning: QueryPlanningMetadata {
                        requested_mode: RuntimeQueryMode::Hybrid,
                        planned_mode: RuntimeQueryMode::Hybrid,
                        intent_cache_status: QueryIntentCacheStatus::Miss,
                        keywords: IntentKeywords::default(),
                        warnings: Vec::new(),
                    },
                    rerank: RerankMetadata {
                        status: RerankStatus::NotApplicable,
                        candidate_count: 1,
                        reordered_count: None,
                    },
                    context_assembly: ContextAssemblyMetadata {
                        status: ContextAssemblyStatus::DocumentOnly,
                        warning: None,
                    },
                    grouped_references: Vec::new(),
                    context_text: None,
                    warning: None,
                    warning_kind: None,
                    library_summary: None,
                },
                retrieved_documents: vec![RuntimeRetrievedDocumentBrief {
                    title: "document-a".to_string(),
                    preview_excerpt: "context-fragment-a".to_string(),
                    document_hint: None,
                }],
                retrieved_context_document_titles: vec!["document-a".to_string()],
                chunk_references: Vec::new(),
                context_chunks: Vec::new(),
                ordered_source_units: Vec::new(),
                graph_evidence_context_lines: Vec::new(),
                graph_entity_references: Vec::new(),
                graph_relation_references: Vec::new(),
            },
            answer_context: "context-fragment-a".to_string(),
            embedding_usage: None,
            consolidation: ConsolidationDiagnostics::noop(),
            query_ir,
            query_compile_usage: None,
            retrieval_spans: Vec::new(),
        }
    }

    #[test]
    fn fast_path_verifier_uses_selected_runtime_grounding() {
        let prepared = prepared_for_single_shot(sample_ir(0.8, None));

        let chunks = selected_runtime_answer_chunks(&prepared);
        let grounding =
            selected_runtime_grounding_evidence(&prepared, AssistantGroundingEvidence::default());
        let verification = verify_answer_against_canonical_evidence(
            "Which fragment is present?",
            "The selected fragment is context-fragment-a.",
            &prepared.structured.intent_profile,
            &super::super::CanonicalAnswerEvidence {
                bundle: None,
                chunk_rows: Vec::new(),
                structured_blocks: Vec::new(),
                technical_facts: Vec::new(),
            },
            &chunks,
            &prepared.answer_context,
            &grounding,
        );

        assert!(chunks.is_empty());
        assert!(
            grounding
                .verification_corpus
                .iter()
                .any(|fragment| fragment.contains("context-fragment-a"))
        );
        assert_eq!(verification.state, QueryVerificationState::Verified);
    }

    #[test]
    fn setup_configuration_builder_output_verifies_without_label_literals() {
        let mut prepared = prepared_for_single_shot(sample_ir(0.8, None));
        prepared.query_ir.target_types =
            vec!["configuration_file".to_string(), "parameter".to_string()];
        prepared.structured.context_chunks = vec![
            setup_anchor_chunk(
                Uuid::now_v7(),
                "workflow module alpha guide",
                "alpha-agent",
                "/etc/alpha-agent.conf",
            ),
            setup_anchor_chunk(
                Uuid::now_v7(),
                "workflow module beta guide",
                "beta-agent",
                "/etc/beta-agent.conf",
            ),
        ];

        let chunks = selected_runtime_answer_chunks(&prepared);
        let grounding =
            selected_runtime_grounding_evidence(&prepared, AssistantGroundingEvidence::default());
        let answer = super::super::answer::build_setup_configuration_anchor_answer(
            "How do I configure the workflow module?",
            &prepared.query_ir,
            &prepared.structured.context_chunks,
        )
        .expect("setup configuration answer");

        let verification = verify_answer_against_canonical_evidence(
            "How do I configure the workflow module?",
            &answer,
            &prepared.structured.intent_profile,
            &super::super::CanonicalAnswerEvidence {
                bundle: None,
                chunk_rows: Vec::new(),
                structured_blocks: Vec::new(),
                technical_facts: Vec::new(),
            },
            &chunks,
            &prepared.answer_context,
            &grounding,
        );

        let ru_labels = crate::services::query::i18n::RU_DETERMINISTIC_ANSWER_LABELS;
        assert!(answer.contains(&format!("**{}:**", ru_labels.variants)), "{answer}");
        assert!(!answer.contains("`setup_variants`:"));
        assert!(!answer.contains("`source`:"));
        assert_eq!(verification.state, QueryVerificationState::Verified);
        assert!(
            verification.warnings.iter().all(|warning| warning.code != "unsupported_literal"),
            "{:?}",
            verification.warnings
        );
    }

    #[test]
    fn latest_version_enumeration_uses_single_shot_when_context_is_prepared() {
        let mut ir = sample_ir_with_act(QueryAct::Enumerate, 0.8, None);
        ir.target_types = vec!["version".to_string()];
        ir.literal_constraints.push(crate::domains::query_ir::LiteralSpan {
            text: "5".to_string(),
            kind: crate::domains::query_ir::LiteralKind::NumericCode,
        });
        let prepared = prepared_for_single_shot(ir);

        assert!(
            super::should_use_single_shot_answer("q", &prepared, None),
            "latest-version retrieval already prepares grounded context and must not force a second retrieval pass"
        );
    }

    #[test]
    fn focused_configuration_inventory_waits_for_canonical_preflight() {
        let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.8, None);
        ir.target_types =
            vec!["configuration_file".to_string(), "config_key".to_string(), "concept".to_string()];
        ir.document_focus = Some(DocumentHint { hint: "Subject Alpha setup".to_string() });
        ir.target_entities =
            vec![EntityMention { label: "Subject Alpha".to_string(), role: EntityRole::Subject }];
        let mut prepared = prepared_for_single_shot(ir);
        prepared.answer_context = r#"
[EVIDENCE_CHUNK document="target"] Subject Alpha setup installs `alpha-connector`.
Use `/opt/alpha/modules/connector/connector.conf`.
[Main]
endpointUrl = https://alpha.example/api
partnerId = alpha-partner
[UI.ScanPanel.qrCode]
visible = true
"#
        .to_string();

        assert!(
            !super::should_use_single_shot_answer("q", &prepared, None),
            "focused configuration inventories need canonical preflight so the answer can keep package, file, section, and parameter coverage together"
        );
    }

    #[test]
    fn versioned_update_procedure_waits_for_deterministic_answer_before_single_shot() {
        let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.95, None);
        ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        ir.retrieval_query = Some("how to update Sample Target?".to_string());
        let mut prepared = prepared_for_single_shot(ir);
        prepared.structured.context_chunks = vec![procedure_chunk(
            "Sample Target runbook",
            "Sample Target update:\n\
             1. Stop Alpha subject workers.\n\
             2. Install alpha-subject-2.0.0.\n\
             3. Start Alpha subject workers.",
        )];

        assert!(
            !super::should_use_single_shot_answer("how to update Sample Target?", &prepared, None,),
            "command-bearing versioned procedures should be answered by the deterministic path instead of the initial single-shot fast path"
        );
    }

    #[test]
    fn update_procedure_builder_uses_selected_runtime_chunks() {
        let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.95, None);
        ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        ir.retrieval_query = Some("how to update Sample Target?".to_string());
        let mut prepared = prepared_for_single_shot(ir);
        prepared.structured.context_chunks =
            vec![procedure_chunk("Sample Target overview", "Sample Target overview only.")];
        let mut selected_unit = procedure_chunk(
            "Sample Target update",
            "Sample Target update:\n\
             1. stop sample workers\n\
             2. run sample updater\n\
             3. start sample workers",
        );
        selected_unit.chunk_kind = Some(super::super::SOURCE_UNIT_CHUNK_KIND.to_string());
        prepared.structured.ordered_source_units = vec![selected_unit];

        assert!(
            super::super::answer::build_update_procedure_sequence_answer(
                "how to update Sample Target?",
                &prepared.query_ir,
                &prepared.structured.context_chunks,
            )
            .is_none()
        );
        let selected_chunks = selected_runtime_answer_chunks(&prepared);
        let answer = super::super::answer::build_update_procedure_sequence_answer(
            "how to update Sample Target?",
            &prepared.query_ir,
            &selected_chunks,
        )
        .expect("update procedure answer from selected chunks");

        assert!(answer.contains("run sample updater"), "{answer}");
        assert!(answer.contains("start sample workers"), "{answer}");
        assert!(!answer.contains("overview only"), "{answer}");
    }

    #[test]
    fn low_confidence_structural_configuration_inventory_waits_for_canonical_preflight() {
        let mut ir = sample_ir_with_act(QueryAct::Describe, 0.25, None);
        ir.target_types.clear();
        ir.target_entities =
            vec![EntityMention { label: "Subject Alpha".to_string(), role: EntityRole::Subject }];
        ir.literal_constraints =
            vec![LiteralSpan { text: "alphaMode".to_string(), kind: LiteralKind::Identifier }];
        ir.temporal_constraints.clear();
        ir.document_focus = None;
        ir.source_slice = None;
        ir.conversation_refs.clear();
        let mut prepared = prepared_for_single_shot(ir);
        prepared.answer_context = r#"
[EVIDENCE_CHUNK document="target"] Subject Alpha setup installs `alpha-connector`.
Use `/opt/alpha/modules/connector/connector.conf`.
[Main]
endpointUrl = https://alpha.example/api
partnerId = alpha-partner
[UI.ScanPanel.qrCode]
visible = true
"#
        .to_string();

        assert!(
            !super::should_use_single_shot_answer("q", &prepared, None),
            "low-confidence untyped compiler output must not bypass canonical configuration coverage when the evidence itself carries a setup inventory"
        );
    }

    #[test]
    fn low_confidence_section_parameter_inventory_waits_for_canonical_preflight_without_path() {
        let mut ir = sample_ir_with_act(QueryAct::Describe, 0.25, None);
        ir.target_types.clear();
        ir.target_entities =
            vec![EntityMention { label: "Subject Alpha".to_string(), role: EntityRole::Subject }];
        ir.literal_constraints.clear();
        ir.temporal_constraints.clear();
        ir.document_focus = None;
        ir.source_slice = None;
        ir.conversation_refs.clear();
        let mut prepared = prepared_for_single_shot(ir);
        prepared.answer_context = r#"
[EVIDENCE_CHUNK document="target"] Subject Alpha configuration fragment.
[Main]
endpointUrl
partnerId
secretKey
currency
timeout
visible
"#
        .to_string();

        assert!(
            !super::should_use_single_shot_answer("q", &prepared, None),
            "section-scoped parameter inventories need canonical preflight even when the file path did not fit in the packed context"
        );
    }

    #[test]
    fn focused_configuration_single_setting_can_use_initial_fast_path() {
        let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.8, None);
        ir.target_types =
            vec!["configuration_file".to_string(), "config_key".to_string(), "concept".to_string()];
        ir.document_focus = Some(DocumentHint { hint: "Subject Alpha setup".to_string() });
        ir.target_entities =
            vec![EntityMention { label: "Subject Alpha".to_string(), role: EntityRole::Subject }];
        let mut prepared = prepared_for_single_shot(ir);
        prepared.answer_context = r#"
[EVIDENCE_CHUNK document="target"] Subject Alpha setup uses `/opt/alpha/display/display.ini`.
[UI.ScanPanel.qrCode]
visible = true
"#
        .to_string();

        assert!(
            !super::focused_configuration_inventory_waits_for_preflight(
                &prepared.query_ir,
                &prepared.answer_context,
            ),
            "a focused single-setting fragment is not a broader configuration inventory"
        );
    }

    #[test]
    fn structural_literal_comparison_with_exact_literals_waits_for_preflight() {
        let mut ir = sample_ir_with_act(QueryAct::Compare, 0.93, None);
        ir.retrieval_query =
            Some("Alpha unit and Beta unit SAMPLE_LIMIT threshold controls".to_string());
        ir.comparison = Some(crate::domains::query_ir::ComparisonSpec {
            a: Some("Alpha unit".to_string()),
            b: Some("Beta unit".to_string()),
            dimension: "SAMPLE_LIMIT threshold controls".to_string(),
        });
        let mut prepared = prepared_for_single_shot(ir);
        prepared.answer_context = r#"
[EVIDENCE_CHUNK document="alpha.txt"] Alpha unit applies SAMPLE_LIMIT_REQUESTS before admitting a record.

[EVIDENCE_CHUNK document="beta.txt"] Beta unit applies SAMPLE_LIMIT_WINDOW_SECONDS before expiring a record.
"#
        .to_string();

        assert!(
            !super::should_use_single_shot_answer("q", &prepared, None),
            "cross-surface comparisons with exact structural literals need canonical preflight"
        );
    }

    #[test]
    fn disposition_answers_when_consolidation_committed_focused_document() {
        let mut ir = sample_ir(
            0.74,
            Some(crate::domains::query_ir::ClarificationReason::MultipleInterpretations),
        );
        ir.target_entities =
            vec![EntityMention { label: "return process".to_string(), role: EntityRole::Subject }];
        let mut prepared = prepared_for_single_shot(ir);
        prepared.structured.retrieved_context_document_titles = vec![
            "Return process".to_string(),
            "Return process: attachment.png".to_string(),
            "Adjacent return workflow".to_string(),
        ];
        prepared.structured.retrieved_documents = vec![
            retrieved_doc("Return process", "source://return-process"),
            retrieved_doc("Return process: attachment.png", "source://return-attachment"),
            retrieved_doc("Adjacent return workflow", "source://adjacent-return"),
        ];
        prepared.structured.diagnostics.grouped_references = sample_groups();
        prepared.consolidation = ConsolidationDiagnostics {
            focused_document_id: Some(Uuid::now_v7()),
            focus_reason: FocusReason::ScoreDominance,
            winner_chunk_count: 1,
            tangential_chunk_count: 5,
        };

        let disposition = classify_answer_disposition(&prepared, "How do I complete return?");

        assert!(
            matches!(disposition, AnswerDisposition::Answer),
            "focused-document consolidation means retained tangentials must not force clarify"
        );
    }

    #[test]
    fn stateless_conversation_refs_can_use_initial_fast_path() {
        let mut ir = sample_ir_with_act(QueryAct::RetrieveValue, 0.55, None);
        ir.conversation_refs.push(UnresolvedRef {
            surface: "source-local anchor".to_string(),
            kind: ConversationRefKind::Deictic,
        });
        let prepared = prepared_for_single_shot(ir);

        assert!(
            super::should_use_single_shot_answer("q", &prepared, None),
            "without prior conversation, unresolved refs cannot be resolved by another retrieval pass and prepared context should answer or refuse directly"
        );
    }

    #[test]
    fn literal_free_retrieve_value_waits_for_source_coverage_preflight() {
        let mut ir = sample_ir_with_act(QueryAct::RetrieveValue, 0.8, None);
        ir.target_entities = vec![EntityMention {
            label: "sample service policy".to_string(),
            role: EntityRole::Subject,
        }];
        let prepared = prepared_for_single_shot(ir);

        assert!(
            !super::should_use_single_shot_answer(
                "What renewal policy applies to the sample service?",
                &prepared,
                None,
            ),
            "literal-free value lookups need source coverage before the model decides facts are missing"
        );
    }

    #[test]
    fn conversation_refs_with_history_wait_for_canonical_preflight() {
        let mut ir = sample_ir_with_act(QueryAct::RetrieveValue, 0.55, None);
        ir.conversation_refs.push(UnresolvedRef {
            surface: "that topic".to_string(),
            kind: ConversationRefKind::Deictic,
        });
        let prepared = prepared_for_single_shot(ir);

        assert!(
            !super::should_use_single_shot_answer("q", &prepared, Some("user: earlier topic")),
            "real prior conversation should skip only the initial fast path; canonical preflight still answers from fixed evidence"
        );
    }

    #[test]
    fn focused_document_answer_intent_waits_for_canonical_preflight() {
        let mut ir = sample_ir_with_act(QueryAct::RetrieveValue, 0.8, None);
        ir.target_types = vec!["secondary_heading".to_string()];
        let prepared = prepared_for_single_shot(ir);

        assert!(
            !super::should_use_single_shot_answer(
                "What report name appears in the runtime PDF upload check?",
                &prepared,
                None,
            ),
            "focused document literals need canonical chunks before final answer selection"
        );
    }

    #[test]
    fn literal_free_answer_is_not_acceptable_when_context_has_requested_technical_literals() {
        let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.8, None);
        ir.target_types = vec!["path".to_string(), "config_key".to_string()];
        let context = "Use `/srv/scans`, then set scan_path = /srv/scans in the share block.";

        assert!(super::answer_omits_expected_technical_literals(
            "The provided context does not include setup details.",
            &ir,
            context,
        ));
        assert!(super::answer_omits_expected_technical_literals(
            "The provided context does not include setup details.",
            &ir,
            "Use `/srv/scans` for the scan directory.",
        ));
        assert!(!super::answer_omits_expected_technical_literals(
            "Use `/srv/scans` for the scan directory.",
            &ir,
            context,
        ));
    }

    #[test]
    fn internal_history_markers_are_never_acceptable_answer_text() {
        assert!(super::answer_contains_internal_history_marker(
            "Prior assistant compact literal memory. Use anchors only.\nliterals: `alpha`"
        ));
        assert!(super::answer_contains_internal_history_marker(
            "Prior assistant pinned literal anchors. `alpha`"
        ));
        assert!(!super::answer_contains_internal_history_marker(
            "Use `alpha` as the source-backed value."
        ));
    }

    #[test]
    fn low_confidence_unfocused_answer_requires_structural_anchor_coverage() {
        let mut ir = sample_ir_with_act(QueryAct::Describe, 0.25, None);
        ir.target_types.clear();
        ir.target_entities.clear();
        ir.literal_constraints.clear();
        ir.temporal_constraints.clear();
        ir.document_focus = None;
        ir.source_slice = None;
        ir.conversation_refs.clear();
        let context = r#"
Retrieved graph evidence
[graph-evidence target="FocusTokenA P0"]
1. Choose "A0", then press "B1".
[graph-evidence target="FocusTokenA P1"]
2. If state is "C2", use "D3".
EVIDENCE_CHUNK blocks
- [EVIDENCE_CHUNK document="FocusTokenA D0" chunk_index=0] Dialog "E4" offers "F5".
"#;

        assert!(super::answer_omits_structural_context_coverage(
            "FocusTokenA requested state coverage",
            "Use the available fallback from the selected source.",
            &ir,
            context,
        ));
        assert!(!super::answer_omits_structural_context_coverage(
            "FocusTokenA requested state coverage",
            "Choose `A0`, then use `D3` for the alternate state.",
            &ir,
            context,
        ));
    }

    #[test]
    fn structural_anchor_coverage_uses_query_aligned_context() {
        let mut ir = sample_ir_with_act(QueryAct::Describe, 0.25, None);
        ir.target_types.clear();
        ir.target_entities.clear();
        ir.literal_constraints.clear();
        ir.temporal_constraints.clear();
        ir.document_focus = None;
        ir.source_slice = None;
        ir.conversation_refs.clear();
        let question = "FocusTokenA requires structural coverage";
        let context = r#"
Retrieved graph evidence
[graph-evidence target="NoiseTokenZ"]
1. Choose "A0", then press "B1".
[graph-evidence target="FocusTokenA"]
2. If state is "C2", use "D3".
EVIDENCE_CHUNK blocks
- [EVIDENCE_CHUNK document="FocusTokenA runbook" chunk_index=0] Dialog "E4" offers "F5".
"#;

        assert!(super::answer_omits_structural_context_coverage(
            question,
            "Choose `A0`, then press `B1`.",
            &ir,
            context,
        ));
        assert!(!super::answer_omits_structural_context_coverage(
            question,
            "Use `C2` with `D3`, then handle `E4` and `F5`.",
            &ir,
            context,
        ));
    }

    #[test]
    fn structural_anchor_coverage_ignores_evidence_chunk_metadata_literals() {
        let mut ir = sample_ir_with_act(QueryAct::Describe, 0.25, None);
        ir.target_types.clear();
        ir.target_entities.clear();
        ir.literal_constraints.clear();
        ir.temporal_constraints.clear();
        ir.document_focus = None;
        ir.source_slice = None;
        ir.conversation_refs.clear();
        let context = r#"
EVIDENCE_CHUNK blocks
- [EVIDENCE_CHUNK document="FocusTokenA D0" chunk_index=0] This paragraph has no quoted value.
- [EVIDENCE_CHUNK document="FocusTokenA D1" chunk_index=1] This paragraph also has no config value.
- [EVIDENCE_CHUNK document="FocusTokenA D2" chunk_index=2] This paragraph is descriptive only.
- [EVIDENCE_CHUNK document="FocusTokenA D3" chunk_index=3] This paragraph is still descriptive.
"#;

        assert!(!super::answer_omits_structural_context_coverage(
            "FocusTokenA requested state coverage",
            "Use the available fallback from the selected source.",
            &ir,
            context,
        ));
    }

    #[test]
    fn structural_anchor_coverage_uses_token_boundaries_for_single_token_anchors() {
        let answer_tokens =
            crate::services::query::text_match::normalized_alnum_tokens("A10x B20x", 1);

        assert!(!super::structural_answer_contains_anchor("a10x b20x", &answer_tokens, "A10",));
        assert!(super::structural_answer_contains_anchor("a10x b20x", &answer_tokens, "A10x",));
    }

    #[test]
    fn structural_anchor_coverage_warning_marks_verified_answer_partial() {
        let mut ir = sample_ir_with_act(QueryAct::Describe, 0.25, None);
        ir.target_types.clear();
        ir.target_entities.clear();
        ir.literal_constraints.clear();
        ir.temporal_constraints.clear();
        ir.document_focus = None;
        ir.source_slice = None;
        ir.conversation_refs.clear();
        let mut verification = RuntimeAnswerVerification {
            state: QueryVerificationState::Verified,
            warnings: Vec::new(),
            unsupported_literals: Vec::new(),
        };

        super::apply_structural_coverage_warning(
            "FocusTokenA status coverage",
            "Use the available fallback from the selected source.",
            &ir,
            r#"
[graph-evidence target="FocusTokenA P0"] Choose "A0", then press "B1".
[graph-evidence target="FocusTokenA P1"] If state is "C2", use "D3".
"#,
            &serde_json::json!({}),
            &mut verification,
        );

        assert_eq!(verification.state, QueryVerificationState::PartiallySupported);
        assert!(verification.warnings.iter().any(|warning| warning.code == "partial_coverage"));
    }

    #[test]
    fn structural_anchor_coverage_preserves_deterministic_anchor_inventory_answers() {
        let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.8, None);
        ir.target_types =
            vec!["package".to_string(), "configuration_file".to_string(), "config_key".to_string()];
        let mut verification = RuntimeAnswerVerification {
            state: QueryVerificationState::Verified,
            warnings: Vec::new(),
            unsupported_literals: Vec::new(),
        };
        let ru_labels = crate::services::query::i18n::RU_DETERMINISTIC_ANSWER_LABELS;
        let answer = format!(
            "**{}:**\n\n**{}:** **Subject Alpha setup**\n- **{}:** `/opt/alpha/connector.conf`\n- **{}:** `endpointUrl = https://example.invalid`",
            ru_labels.variants, ru_labels.source, ru_labels.path, ru_labels.parameter
        );

        super::apply_structural_coverage_warning(
            "Subject Alpha configuration coverage",
            &answer,
            &ir,
            r#"
[graph-evidence target="Subject Alpha P0"] Choose "A0", then press "B1".
[graph-evidence target="Subject Alpha P1"] If state is "C2", use "D3".
"#,
            &serde_json::json!({
                "deterministic": true,
                "answer_kind": super::AnswerKind::SetupConfigurationAnchor.as_str(),
            }),
            &mut verification,
        );

        assert_eq!(verification.state, QueryVerificationState::Verified);
        assert!(verification.warnings.is_empty());
    }

    #[test]
    fn configure_setup_answer_requires_focused_parameter_coverage() {
        let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.8, None);
        ir.target_types =
            vec!["package".to_string(), "configuration_file".to_string(), "config_key".to_string()];
        ir.target_entities =
            vec![EntityMention { label: "Subject Alpha".to_string(), role: EntityRole::Subject }];
        let context = r#"
Exact technical literals
- Document: `Subject Alpha setup`
  Paths: `/opt/alpha/modules/connector/connector.conf`
  Parameters: `endpointUrl`, `partnerId`, `secretKey`, `retryTimeout`, `sendDetails`
- Document: `Subject Beta setup`
  Parameters: `otherEndpoint`, `otherSecret`, `otherTimeout`, `otherFlag`
"#;

        assert!(super::answer_omits_expected_technical_literals(
            "Install `alpha-connector`, edit `/opt/alpha/modules/connector/connector.conf`, and set `endpointUrl`, `partnerId`, `secretKey`.",
            &ir,
            context,
        ));
        assert!(super::answer_omits_expected_technical_literals(
            "Install `alpha-connector`, edit `/opt/alpha/display/display.ini`, and set `endpointUrl`, `partnerId`, `secretKey`, `retryTimeout`, `sendDetails`.",
            &ir,
            context,
        ));
        let expected = super::collect_focused_context_parameter_literals(context, &ir, 32);
        let actual = super::collect_intended_technical_literals(
            "Install `alpha-connector`, edit `/opt/alpha/modules/connector/connector.conf`, and set `endpointUrl`, `partnerId`, `secretKey`, `retryTimeout`, `sendDetails`.",
            super::TechnicalLiteralIntent {
                wants_parameters: true,
                ..super::TechnicalLiteralIntent::default()
            },
            32,
        );
        assert!(
            expected.iter().all(|literal| actual.contains(literal)),
            "expected={expected:?} actual={actual:?}"
        );
        assert!(!super::answer_omits_expected_technical_literals(
            "Install `alpha-connector`, edit `/opt/alpha/modules/connector/connector.conf`, and set `endpointUrl`, `partnerId`, `secretKey`, `retryTimeout`, `sendDetails`.",
            &ir,
            context,
        ));
    }

    #[test]
    fn configure_setup_answer_requires_focused_section_coverage() {
        let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.8, None);
        ir.target_types =
            vec!["package".to_string(), "configuration_file".to_string(), "config_key".to_string()];
        ir.target_entities =
            vec![EntityMention { label: "Subject Alpha".to_string(), role: EntityRole::Subject }];
        let context = r#"
Exact technical literals
- Document: `Subject Alpha setup`
  Paths: `/opt/alpha/modules/connector/connector.conf`
  Sections:
    - `[CFG]`
    - `[UI.ScanPanel.qrCode]`
  Parameters:
    - `endpointUrl`
    - `partnerId`
"#;

        assert!(super::answer_omits_expected_technical_literals(
            "Install `alpha-connector`, edit `/opt/alpha/modules/connector/connector.conf`, and set `endpointUrl`, `partnerId`.",
            &ir,
            context,
        ));
        assert!(!super::answer_omits_expected_technical_literals(
            "Install `alpha-connector`, edit `/opt/alpha/modules/connector/connector.conf`, use `[CFG]` and `[UI.ScanPanel.qrCode]`, and set `endpointUrl`, `partnerId`.",
            &ir,
            context,
        ));
    }

    #[test]
    fn configure_setup_parameter_coverage_ignores_aggregate_metadata_values() {
        let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.8, None);
        ir.target_types =
            vec!["package".to_string(), "configuration_file".to_string(), "config_key".to_string()];
        ir.target_entities =
            vec![EntityMention { label: "Subject Alpha".to_string(), role: EntityRole::Subject }];
        let context = r#"
Exact technical literals
- Document: `Subject Alpha setup`
  Paths:
    - `/opt/alpha/modules/connector/connector.conf`
  Parameters:
    - `endpointUrl`
    - `partnerId`
    - `secretKey`
    - `retryInterval`

Prepared segments
- metadata_block > Subject Alpha setup: Table Summary | Sheet: Module setup | Column: Name | Value Kind: categorical | Most Frequent Values: retryTimeout; sendDetails; auditMode
- paragraph > Subject Alpha setup: The `sendDetails` flag controls whether workflow details are forwarded.
"#;

        let expected = super::collect_focused_context_parameter_literals(context, &ir, 32);

        assert!(!expected.contains("retryTimeout"));
        assert!(!expected.contains("sendDetails"));
        assert!(!expected.contains("auditMode"));

        assert!(super::answer_omits_expected_technical_literals(
            "Install `alpha-connector`, edit `/opt/alpha/modules/connector/connector.conf`, and set `endpointUrl`, `partnerId`.",
            &ir,
            context,
        ));
        assert!(!super::answer_omits_expected_technical_literals(
            "Install `alpha-connector`, edit `/opt/alpha/modules/connector/connector.conf`, and set `endpointUrl`, `partnerId`, `secretKey`, `retryInterval`.",
            &ir,
            context,
        ));
    }

    #[test]
    fn configure_setup_parameter_coverage_ignores_prose_only_literals() {
        let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.8, None);
        ir.target_types =
            vec!["package".to_string(), "configuration_file".to_string(), "config_key".to_string()];
        ir.target_entities =
            vec![EntityMention { label: "Subject Alpha".to_string(), role: EntityRole::Subject }];
        let context = r#"
Exact technical literals
- Document: `Subject Alpha setup`
  Parameters:
    - `endpointUrl`
    - `partnerId`
    - `secretKey`
    - `retryTimeout`

Prepared segments
- paragraph > Subject Alpha setup: Operators may inspect `LOG_LEVEL` while diagnosing a setup issue.
- list_item > Subject Alpha setup: Keep `backupBeforeChange` enabled for maintenance notes.
"#;

        let expected = super::collect_focused_context_parameter_literals(context, &ir, 32);

        assert!(!expected.contains("LOG_LEVEL"));
        assert!(!expected.contains("backupBeforeChange"));
        assert!(!super::answer_omits_expected_technical_literals(
            "Set `endpointUrl`, `partnerId`, `secretKey`, and `retryTimeout`.",
            &ir,
            context,
        ));
    }

    #[test]
    fn configure_setup_answer_requires_assignment_example_coverage() {
        let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.8, None);
        ir.target_types = vec!["configuration_file".to_string(), "config_key".to_string()];
        let context = r#"
EVIDENCE_CHUNK blocks
- [EVIDENCE_CHUNK document="Subject Alpha setup" chunk_index=2] Example:
[Main]
alphaFlag = true
[UI.Component]
visible = true
"#;

        assert!(super::answer_omits_expected_technical_literals(
            "Set `alphaFlag = true`.",
            &ir,
            context,
        ));
        assert!(!super::answer_omits_expected_technical_literals(
            "Set `alphaFlag = true` and `visible = true`.",
            &ir,
            context,
        ));
    }

    #[test]
    fn low_confidence_describe_answer_requires_assignment_example_coverage() {
        let mut ir = sample_ir_with_act(QueryAct::Describe, 0.25, None);
        ir.target_types.clear();
        ir.target_entities.clear();
        let context = r#"
EVIDENCE_CHUNK blocks
- [EVIDENCE_CHUNK document="Subject Alpha setup" chunk_index=2] Example:
[Main]
alphaFlag = true
[Check]
printSlip = false
"#;

        assert!(super::answer_omits_expected_technical_literals(
            "The context contains `alphaFlag = true`.",
            &ir,
            context,
        ));
        assert!(!super::answer_omits_expected_technical_literals(
            "The context contains `alphaFlag = true` and `printSlip = false`.",
            &ir,
            context,
        ));
    }

    #[test]
    fn targeted_technical_query_without_focus_context_skips_initial_fast_path() {
        let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.8, None);
        ir.target_types = vec!["path".to_string(), "config_key".to_string()];
        ir.target_entities = vec![
            EntityMention {
                label: "RareProtocol scan share".to_string(),
                role: EntityRole::Subject,
            },
            EntityMention { label: "RareProtocol daemon".to_string(), role: EntityRole::Object },
        ];
        let mut prepared = prepared_for_single_shot(ir);
        prepared.answer_context =
            "[EVIDENCE_CHUNK document=\"nearby\"] Sample Subject setup uses `/srv/scans`."
                .to_string();

        assert!(
            !super::should_use_single_shot_answer(
                "How do I configure the RareProtocol scan share?",
                &prepared,
                None,
            ),
            "technical initial single-shot needs focus evidence before it can finalize without preflight"
        );
    }

    #[test]
    fn targeted_technical_single_shot_answer_needs_context_backed_focus() {
        let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.8, None);
        ir.target_types = vec!["path".to_string(), "config_key".to_string()];
        ir.target_entities = vec![
            EntityMention {
                label: "RareProtocol scan share".to_string(),
                role: EntityRole::Subject,
            },
            EntityMention { label: "RareProtocol daemon".to_string(), role: EntityRole::Object },
        ];
        let unrelated_context =
            "[EVIDENCE_CHUNK document=\"nearby\"] Sample Subject setup uses `/srv/scans`.";
        let focused_context =
            "[EVIDENCE_CHUNK document=\"target\"] RareProtocol daemon setup uses `/srv/scans`.";

        assert!(super::single_shot_lacks_query_focus_support(
            "The context has no RareProtocol details, but mentions `/srv/scans`.",
            &ir,
            unrelated_context,
        ));
        assert!(super::single_shot_lacks_query_focus_support(
            "Use `/srv/scans` for the scan directory.",
            &ir,
            focused_context,
        ));
        assert!(!super::single_shot_lacks_query_focus_support(
            "RareProtocol daemon uses `/srv/scans` for the scan directory.",
            &ir,
            focused_context,
        ));
    }

    #[test]
    fn plain_literal_does_not_require_single_shot_focus_support() {
        let mut ir = sample_ir_with_act(QueryAct::RetrieveValue, 0.8, None);
        ir.target_types = vec!["concept".to_string()];
        ir.target_entities = Vec::new();
        ir.literal_constraints = vec![crate::domains::query_ir::LiteralSpan {
            text: "alpha".to_string(),
            kind: crate::domains::query_ir::LiteralKind::Identifier,
        }];

        assert!(!super::query_requires_single_shot_focus_support(&ir));

        ir.literal_constraints = vec![crate::domains::query_ir::LiteralSpan {
            text: "callbackUrl".to_string(),
            kind: crate::domains::query_ir::LiteralKind::Identifier,
        }];

        assert!(super::query_requires_single_shot_focus_support(&ir));
    }

    #[test]
    fn configure_how_exact_literal_requires_single_shot_focus_support() {
        let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.8, None);
        ir.target_types = vec!["concept".to_string()];
        ir.target_entities = Vec::new();
        ir.literal_constraints = vec![crate::domains::query_ir::LiteralSpan {
            text: "alpha".to_string(),
            kind: crate::domains::query_ir::LiteralKind::Other,
        }];

        assert!(!super::query_requires_single_shot_focus_support(&ir));

        ir.literal_constraints = vec![crate::domains::query_ir::LiteralSpan {
            text: "2.4.1".to_string(),
            kind: crate::domains::query_ir::LiteralKind::Version,
        }];

        assert!(super::query_requires_single_shot_focus_support(&ir));
    }

    #[test]
    fn compare_without_structural_operands_skips_initial_fast_path() {
        let prepared = prepared_for_single_shot(sample_ir_with_act(QueryAct::Compare, 0.8, None));

        assert!(
            !super::should_use_single_shot_answer("q", &prepared, None),
            "compare questions without covered operands should wait for canonical preflight evidence"
        );
    }

    #[test]
    fn compare_uses_single_shot_when_prepared_context_covers_operands() {
        let mut ir = sample_ir_with_act(QueryAct::Compare, 0.8, None);
        ir.comparison = Some(ComparisonSpec {
            a: Some("Sample Subject".to_string()),
            b: Some("Beta Suite".to_string()),
            dimension: "capability".to_string(),
        });
        let mut prepared = prepared_for_single_shot(ir);
        prepared.answer_context = "[EVIDENCE_CHUNK scope=excerpt coverage=sampled document=\"alpha\"] Sample Subject stores audit events.\n\
[EVIDENCE_CHUNK scope=excerpt coverage=sampled document=\"beta\"] Beta Suite stores billing events."
            .to_string();

        assert!(
            super::should_use_single_shot_answer("q", &prepared, None),
            "compare can use the prepared single-shot context when every IR operand is covered"
        );
    }

    #[test]
    fn compare_uses_single_shot_when_prepared_context_partially_covers_operands() {
        let mut ir = sample_ir_with_act(QueryAct::Compare, 0.8, None);
        ir.comparison = Some(ComparisonSpec {
            a: Some("Sample Subject".to_string()),
            b: Some("Beta Suite".to_string()),
            dimension: "capability".to_string(),
        });
        let mut prepared = prepared_for_single_shot(ir);
        prepared.answer_context =
            "[EVIDENCE_CHUNK scope=excerpt coverage=sampled document=\"alpha\"] Sample Subject stores audit events."
                .to_string();

        assert!(
            super::should_use_single_shot_answer("q", &prepared, None),
            "one-sided compare evidence should produce a fast grounded partial answer instead of a second retrieval pass"
        );
    }

    #[test]
    fn compare_skips_initial_fast_path_when_no_operand_is_covered() {
        let mut ir = sample_ir_with_act(QueryAct::Compare, 0.8, None);
        ir.comparison = Some(ComparisonSpec {
            a: Some("Sample Subject".to_string()),
            b: Some("Beta Suite".to_string()),
            dimension: "capability".to_string(),
        });
        let mut prepared = prepared_for_single_shot(ir);
        prepared.answer_context =
            "[EVIDENCE_CHUNK scope=excerpt coverage=sampled document=\"gamma\"] Gamma Console stores audit events."
                .to_string();

        assert!(
            !super::should_use_single_shot_answer("q", &prepared, None),
            "compare with zero covered operands should not use the initial fast path"
        );
    }

    #[test]
    fn comparison_coverage_metadata_does_not_count_as_grounding_evidence() {
        let mut ir = sample_ir_with_act(QueryAct::Compare, 0.8, None);
        ir.comparison = Some(ComparisonSpec {
            a: Some("Sample Subject".to_string()),
            b: Some("Beta Suite".to_string()),
            dimension: "capability".to_string(),
        });
        let coverage = super::compare_operands_covered_by_context(
            &ir,
            "COMPARISON_COVERAGE status=partial\n\
- covered_operand: Sample Subject\n\
- uncovered_operand: Beta Suite",
        );

        assert!(
            matches!(coverage, super::EvidenceCoverage::Insufficient("compare_evidence_empty")),
            "internal coverage markers must not become synthetic evidence"
        );
    }

    #[test]
    fn disposition_keeps_confident_ir_on_answer_path() {
        let disposition = classify_answer_disposition_from_groups(
            "how do i configure target workflows?",
            &sample_ir(0.9, None),
            &[],
            &sample_groups(),
        );

        assert!(matches!(disposition, AnswerDisposition::Answer));
    }

    #[test]
    fn disposition_keeps_low_confidence_ir_on_answer_path_without_explicit_reason() {
        let disposition = classify_answer_disposition_from_groups(
            "how do i configure target workflows?",
            &sample_ir(0.4, None),
            &[],
            &sample_groups(),
        );

        assert!(matches!(disposition, AnswerDisposition::Answer));
    }

    #[test]
    fn provider_free_fallback_ir_preserves_retrieval_question_without_clarifying() {
        let ir = provider_free_fallback_query_ir("  Describe ABC ConnectorX with `/v2/pay`  ");

        assert_eq!(ir.act, QueryAct::Describe);
        assert_eq!(ir.scope, QueryScope::MultiDocument);
        assert_eq!(ir.language, QueryLanguage::Auto);
        assert_eq!(
            ir.effective_retrieval_query("unused"),
            "Describe ABC ConnectorX with `/v2/pay`"
        );
        assert!(ir.target_entities.iter().any(|entity| entity.label == "ABC ConnectorX"));
        assert!(ir.literal_constraints.iter().any(|literal| literal.text == "/v2/pay"));
        assert!(ir.needs_clarification.is_none());
        assert!(ir.confidence < 0.5);
    }

    #[test]
    fn provider_free_fallback_ir_stays_on_answer_path_with_retrieved_evidence() {
        let ir = provider_free_fallback_query_ir("describe S");
        let disposition =
            classify_answer_disposition_from_groups("describe S", &ir, &[], &sample_groups());

        assert!(matches!(disposition, AnswerDisposition::Answer));
    }

    #[test]
    fn disposition_clarifies_low_confidence_terse_describe_with_multi_document_variants() {
        let disposition = classify_answer_disposition_from_groups(
            "provider configure",
            &sample_ir_with_act(QueryAct::Describe, 0.25, None),
            &[],
            &sample_groups(),
        );

        match disposition {
            AnswerDisposition::Clarify { variants } => {
                assert_eq!(variants.len(), 3);
                assert_eq!(variants[0], "Provider A configuration");
            }
            AnswerDisposition::Answer => {
                panic!("expected clarify disposition for terse describe with balanced variants")
            }
        }
    }

    #[test]
    fn disposition_can_clarify_when_compiler_explicitly_requests_it() {
        let disposition = classify_answer_disposition_from_groups(
            "how do i configure provider workflows?",
            &sample_ir(0.4, Some(ClarificationReason::MultipleInterpretations)),
            &[],
            &sample_groups(),
        );

        match disposition {
            AnswerDisposition::Clarify { variants } => {
                assert_eq!(variants.len(), 3);
                assert_eq!(variants[0], "Provider A configuration");
            }
            AnswerDisposition::Answer => {
                panic!("expected clarify disposition for explicit compiler clarification")
            }
        }
    }

    #[test]
    fn disposition_answers_technical_ir_when_compiler_requested_clarification() {
        let mut ir = sample_ir_with_act(
            QueryAct::RetrieveValue,
            0.4,
            Some(ClarificationReason::MultipleInterpretations),
        );
        ir.target_types = vec!["endpoint".to_string()];

        let disposition = classify_answer_disposition_from_groups(
            "which provider endpoint handles workflow module status?",
            &ir,
            &[],
            &sample_groups(),
        );

        assert!(
            matches!(disposition, AnswerDisposition::Answer),
            "exact technical IR must proceed to answer/preflight instead of asking for variants"
        );
    }

    #[test]
    fn disposition_answers_non_terse_retrieve_value_with_target_entity() {
        let mut ir = sample_ir_with_act(
            QueryAct::RetrieveValue,
            0.4,
            Some(ClarificationReason::MultipleInterpretations),
        );
        ir.target_types = vec!["attribute".to_string()];

        let disposition = classify_answer_disposition_from_groups(
            "which provider configuration owns the workflow module state?",
            &ir,
            &[],
            &sample_groups(),
        );

        assert!(
            matches!(disposition, AnswerDisposition::Answer),
            "a non-terse retrieve-value question with a structured target entity is already anchored"
        );
    }

    #[test]
    fn clarify_variant_dedup_key_collapses_same_page_attachments() {
        // Several image attachments hanging off the same parent page share a
        // logical prefix and differ only in the trailing filename qualifier.
        // They must collapse to one dedup key so they cannot pad a clarify
        // menu with near-duplicate "variants".
        let a = clarify_variant_dedup_key("Workflow setup: screen-a.png");
        let b = clarify_variant_dedup_key("Workflow setup: screen-b.png");
        assert_eq!(a, b);
        assert_eq!(a.as_deref(), Some("workflow setup"));
    }

    #[test]
    fn clarify_variant_dedup_key_excludes_bare_attachment_titles() {
        // A title that is only a filename is an attachment artefact with no
        // logical document behind it — exclude it from the menu entirely.
        assert_eq!(clarify_variant_dedup_key("diagram.png"), None);
        assert_eq!(clarify_variant_dedup_key("v2-config.jpeg"), None);
    }

    #[test]
    fn clarify_variant_dedup_key_keeps_ordinary_titles() {
        // A normal prose title (no trailing filename qualifier) is a distinct
        // logical document and keeps its own dedup key. A title that merely
        // ends with a filename-shaped word but has no `:` separator is prose,
        // not an attachment qualifier, and is preserved verbatim.
        assert_eq!(
            clarify_variant_dedup_key("Subject Alpha configuration"),
            Some("subject alpha configuration".to_string())
        );
        assert_eq!(
            clarify_variant_dedup_key("How to edit config.ini"),
            Some("how to edit config.ini".to_string())
        );
    }

    #[test]
    fn query_specific_variants_keep_all_matching_provider_titles() {
        let documents = vec![
            retrieved_doc("Subject Subject Alpha", "alpha"),
            retrieved_doc("Subject Subject Beta", "beta"),
            retrieved_doc("Subject Subject Gamma", "gamma"),
            retrieved_doc("Subject Provider Delta", "delta"),
        ];

        let variants =
            extract_query_specific_variants("how to configure Subject", &documents, &[], &[]);

        assert_eq!(
            variants,
            vec![
                "Subject Subject Alpha",
                "Subject Subject Beta",
                "Subject Subject Gamma",
                "Subject Provider Delta"
            ]
        );
    }

    #[test]
    fn extract_variants_collapses_same_page_image_attachments() {
        // Five same-page image attachments share a title prefix; after the
        // structural collapse only one logical variant survives.
        let documents = vec![
            retrieved_doc("Workflow setup: a.png", "page#1"),
            retrieved_doc("Workflow setup: b.png", "page#1"),
            retrieved_doc("Workflow setup: c.png", "page#1"),
            retrieved_doc("Workflow setup: d.png", "page#1"),
            retrieved_doc("Workflow setup: e.png", "page#1"),
        ];

        let variants = extract_query_specific_variants("workflow setup", &documents, &[], &[]);

        assert!(
            variants.len() <= 1,
            "same-page image attachments must collapse to at most one variant, got {variants:?}"
        );
    }

    #[test]
    fn disposition_answers_for_same_page_image_attachments() {
        // (a) N same-page image-attachment documents sharing a title prefix
        // must NOT manufacture a clarify menu of near-duplicates; they
        // collapse below the distinct-document floor and fall through to
        // Answer.
        let groups = (0..5)
            .map(|index| GroupedReference {
                id: format!("document:{index}"),
                kind: GroupedReferenceKind::Document,
                rank: index + 1,
                title: format!("Workflow setup: attachment-{index}.png"),
                excerpt: None,
                evidence_count: 2,
                support_ids: vec![format!("chunk:{index}")],
            })
            .collect::<Vec<_>>();

        let disposition = classify_answer_disposition_from_groups(
            "workflow setup",
            &sample_ir_with_act(QueryAct::ConfigureHow, 0.3, None),
            &[],
            &groups,
        );

        assert!(
            matches!(disposition, AnswerDisposition::Answer),
            "near-duplicate image attachments must answer, not clarify"
        );
    }

    #[test]
    fn disposition_answers_with_two_variants_when_top_variant_dominates() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "TargetName Subject Alpha Manual".to_string(),
                excerpt: None,
                evidence_count: 9,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "TargetName Subject Beta Manual".to_string(),
                excerpt: None,
                evidence_count: 2,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "Ancillary Reference Guide".to_string(),
                excerpt: None,
                evidence_count: 4,
                support_ids: vec!["chunk:3".to_string()],
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "targetname configure",
            &sample_ir(0.4, Some(ClarificationReason::MultipleInterpretations)),
            &[],
            &groups,
        );

        assert!(
            matches!(disposition, AnswerDisposition::Answer),
            "dominant top query variant in compiler clarification mode should use direct answer"
        );
    }

    #[test]
    fn disposition_answers_with_dominant_topic_match_for_rollout_query() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "rollout_runtime_contract.md".to_string(),
                excerpt: None,
                evidence_count: 2,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "sample_rust_http_server.rs".to_string(),
                excerpt: None,
                evidence_count: 4,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "websocket_protocol.md".to_string(),
                excerpt: None,
                evidence_count: 4,
                support_ids: vec!["chunk:3".to_string()],
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "Which endpoint in rollout runtime returns current server info?",
            &sample_ir(0.4, Some(ClarificationReason::AmbiguousTooShort)),
            &[],
            &groups,
        );

        assert!(
            matches!(disposition, AnswerDisposition::Answer),
            "query-word-dominant rollout intent should answer directly over broad evidence counts"
        );
    }

    #[test]
    fn disposition_answers_with_dominant_topic_match_for_inventory_wsdl() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "inventory_soap_api_contract.md".to_string(),
                excerpt: None,
                evidence_count: 2,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "rewards_accounts_api_contract.md".to_string(),
                excerpt: None,
                evidence_count: 4,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "api_design_guidelines.docx".to_string(),
                excerpt: None,
                evidence_count: 4,
                support_ids: vec!["chunk:3".to_string()],
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "Which WSDL does the inventory API use?",
            &sample_ir_with_act(
                QueryAct::RetrieveValue,
                0.4,
                Some(ClarificationReason::MultipleInterpretations),
            ),
            &[],
            &groups,
        );

        assert!(
            matches!(disposition, AnswerDisposition::Answer),
            "single strongly matching inventory variant should answer directly"
        );
    }

    #[test]
    fn disposition_answers_with_dominant_topic_match_for_react_hooks() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "react_dashboard.txt".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "rust_state_machine.rs".to_string(),
                excerpt: None,
                evidence_count: 5,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "monitoring_dashboard.pdf".to_string(),
                excerpt: None,
                evidence_count: 5,
                support_ids: vec!["chunk:3".to_string()],
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "What React hooks are used in the dashboard component and what state do they manage?",
            &sample_ir_with_act(
                QueryAct::ConfigureHow,
                0.4,
                Some(ClarificationReason::MultipleInterpretations),
            ),
            &[],
            &groups,
        );

        assert!(
            matches!(disposition, AnswerDisposition::Answer),
            "dominant topic overlap should override noisy title-level evidence split"
        );
    }

    #[test]
    fn disposition_clarifies_with_two_variants_when_top_two_are_balanced() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "TargetName Subject Alpha Manual".to_string(),
                excerpt: None,
                evidence_count: 8,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "TargetName Subject Beta Manual".to_string(),
                excerpt: None,
                evidence_count: 7,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "Ancillary Reference Guide".to_string(),
                excerpt: None,
                evidence_count: 4,
                support_ids: vec!["chunk:3".to_string()],
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "targetname configure",
            &sample_ir(0.4, Some(ClarificationReason::MultipleInterpretations)),
            &[],
            &groups,
        );

        match disposition {
            AnswerDisposition::Clarify { variants } => {
                assert_eq!(
                    variants,
                    vec![
                        "TargetName Subject Alpha Manual".to_string(),
                        "TargetName Subject Beta Manual".to_string()
                    ]
                );
            }
            AnswerDisposition::Answer => {
                panic!("expected clarification when two variants are competitively balanced")
            }
        }
    }

    #[test]
    fn disposition_clarifies_two_variants_without_absolute_evidence_gap() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "TargetName Subject Alpha Manual".to_string(),
                excerpt: None,
                evidence_count: 2,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "TargetName Subject Beta Manual".to_string(),
                excerpt: None,
                evidence_count: 1,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "Ancillary Reference Guide".to_string(),
                excerpt: None,
                evidence_count: 1,
                support_ids: vec!["chunk:3".to_string()],
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "targetname configure",
            &sample_ir(0.4, Some(ClarificationReason::MultipleInterpretations)),
            &[],
            &groups,
        );

        match disposition {
            AnswerDisposition::Clarify { variants } => {
                assert_eq!(
                    variants,
                    vec![
                        "TargetName Subject Alpha Manual".to_string(),
                        "TargetName Subject Beta Manual".to_string()
                    ]
                );
            }
            AnswerDisposition::Answer => {
                panic!("compiler clarification should not answer on a weak 2:1 evidence split")
            }
        }
    }

    #[test]
    fn disposition_allows_explicit_compiler_clarify_with_multiple_target_entities() {
        let ir = sample_ir_with_two_target_entities(
            QueryAct::RetrieveValue,
            0.4,
            Some(ClarificationReason::AmbiguousTooShort),
        );

        let disposition = classify_answer_disposition_from_groups(
            "provider configuration",
            &ir,
            &[],
            &sample_groups(),
        );

        match disposition {
            AnswerDisposition::Clarify { variants } => {
                assert_eq!(variants.len(), 3);
                assert_eq!(variants[0], "Provider A configuration");
            }
            AnswerDisposition::Answer => {
                panic!("explicit compiler clarification must bypass multi-entity specificity")
            }
        }
    }

    #[test]
    fn disposition_can_structurally_clarify_configure_without_compiler_reason() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "WorkflowLink Subject Alpha Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "WorkflowLink Subject Beta Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "WorkflowLink Subject Gamma Manual".to_string(),
                excerpt: None,
                evidence_count: 2,
                support_ids: vec!["chunk:3".to_string()],
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "how configure workflowlink?",
            &sample_ir(0.6, None),
            &[],
            &groups,
        );

        match disposition {
            AnswerDisposition::Clarify { variants } => {
                assert_eq!(
                    variants,
                    vec![
                        "WorkflowLink Subject Alpha Manual".to_string(),
                        "WorkflowLink Subject Beta Manual".to_string(),
                        "WorkflowLink Subject Gamma Manual".to_string(),
                    ]
                );
            }
            AnswerDisposition::Answer => {
                panic!("expected structural clarify without compiler clarification")
            }
        }
    }

    #[test]
    fn disposition_answers_non_terse_configure_without_compiler_reason() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "WorkflowLink Subject Alpha Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "WorkflowLink Subject Beta Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "WorkflowLink Subject Gamma Manual".to_string(),
                excerpt: None,
                evidence_count: 2,
                support_ids: vec!["chunk:3".to_string()],
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "how should operators configure workflowlink provider routing before rollout?",
            &sample_ir(0.6, None),
            &[],
            &groups,
        );

        assert!(
            matches!(disposition, AnswerDisposition::Answer),
            "non-terse configure questions should answer from retrieved evidence unless the compiler requested clarification"
        );
    }

    #[test]
    fn disposition_can_structurally_clarify_terse_followup_without_compiler_reason() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "WorkflowLink Subject Alpha Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "WorkflowLink Subject Beta Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "WorkflowLink Subject Gamma Manual".to_string(),
                excerpt: None,
                evidence_count: 2,
                support_ids: vec!["chunk:3".to_string()],
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "workflowlink platform",
            &sample_ir_with_two_target_entities(QueryAct::RetrieveValue, 0.6, None),
            &[],
            &groups,
        );

        match disposition {
            AnswerDisposition::Clarify { variants } => {
                assert_eq!(variants.len(), 3);
            }
            AnswerDisposition::Answer => {
                panic!("expected terse query-aligned follow-up to clarify")
            }
        }
    }

    #[test]
    fn disposition_does_not_structurally_clarify_describe_without_compiler_reason() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "API Gateway Alpha".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "API Gateway Beta".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "API Gateway Gamma".to_string(),
                excerpt: None,
                evidence_count: 2,
                support_ids: vec!["chunk:3".to_string()],
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "what is api?",
            &sample_ir_with_act(QueryAct::Describe, 0.6, None),
            &[],
            &groups,
        );

        assert!(matches!(disposition, AnswerDisposition::Answer));
    }

    #[test]
    fn disposition_prefers_query_specific_variant_titles_over_noise() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "Notification Console Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "WorkflowLink Subject Alpha Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "Embedded Browser Manual".to_string(),
                excerpt: None,
                evidence_count: 2,
                support_ids: vec!["chunk:3".to_string()],
            },
            GroupedReference {
                id: "document:4".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 4,
                title: "WorkflowLink Subject Beta Manual".to_string(),
                excerpt: None,
                evidence_count: 2,
                support_ids: vec!["chunk:4".to_string()],
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "how configure workflowlink?",
            &sample_ir(0.4, Some(ClarificationReason::MultipleInterpretations)),
            &[],
            &groups,
        );

        match disposition {
            AnswerDisposition::Clarify { variants } => {
                assert_eq!(
                    variants,
                    vec![
                        "WorkflowLink Subject Alpha Manual".to_string(),
                        "WorkflowLink Subject Beta Manual".to_string(),
                    ]
                );
            }
            AnswerDisposition::Answer => {
                panic!("expected clarify disposition with query-aligned variants")
            }
        }
    }

    #[test]
    fn disposition_uses_discriminating_topic_token_over_shared_product_tokens() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "Platform Pay Subject Alpha Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "Platform Inventory Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "Platform Pay Subject Beta Manual".to_string(),
                excerpt: None,
                evidence_count: 2,
                support_ids: vec!["chunk:3".to_string()],
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "platform pay",
            &sample_ir_with_act(
                QueryAct::RetrieveValue,
                0.4,
                Some(ClarificationReason::AmbiguousTooShort),
            ),
            &[],
            &groups,
        );

        match disposition {
            AnswerDisposition::Clarify { variants } => {
                assert_eq!(
                    variants,
                    vec![
                        "Platform Pay Subject Alpha Manual".to_string(),
                        "Platform Pay Subject Beta Manual".to_string(),
                    ]
                );
            }
            AnswerDisposition::Answer => {
                panic!("expected discriminating topic token to filter shared product labels")
            }
        }
    }

    #[test]
    fn disposition_uses_query_specific_retrieved_documents_when_group_titles_are_noisy() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "Notification Console Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "Embedded Browser Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "Inventory Exchange Manual".to_string(),
                excerpt: None,
                evidence_count: 2,
                support_ids: vec!["chunk:3".to_string()],
            },
        ];
        let retrieved_documents = vec![
            crate::services::query::execution::types::RuntimeRetrievedDocumentBrief {
                title: "WorkflowLink Subject Alpha Manual".to_string(),
                preview_excerpt: String::new(),
                document_hint: None,
            },
            crate::services::query::execution::types::RuntimeRetrievedDocumentBrief {
                title: "WorkflowLink Subject Beta Manual".to_string(),
                preview_excerpt: String::new(),
                document_hint: None,
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "how configure workflowlink?",
            &sample_ir(0.4, Some(ClarificationReason::MultipleInterpretations)),
            &retrieved_documents,
            &groups,
        );

        match disposition {
            AnswerDisposition::Clarify { variants } => {
                assert_eq!(
                    variants,
                    vec![
                        "WorkflowLink Subject Alpha Manual".to_string(),
                        "WorkflowLink Subject Beta Manual".to_string(),
                    ]
                );
            }
            AnswerDisposition::Answer => {
                panic!("expected clarify disposition with query-aligned retrieved documents")
            }
        }
    }

    #[test]
    fn disposition_uses_final_context_titles_when_briefs_and_groups_are_truncated() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "WorkflowLink Subject Alpha Manual".to_string(),
                excerpt: None,
                evidence_count: 6,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "WorkflowLink Subject Beta Manual".to_string(),
                excerpt: None,
                evidence_count: 2,
                support_ids: vec!["chunk:2".to_string()],
            },
        ];
        let context_titles = vec![
            "WorkflowLink Subject Alpha Manual".to_string(),
            "WorkflowLink Subject Beta Manual".to_string(),
            "WorkflowLink Subject Gamma Manual".to_string(),
            "WorkflowLink Provider Delta Manual".to_string(),
        ];

        let disposition = classify_answer_disposition_from_evidence(
            "how configure workflowlink?",
            &sample_ir(0.4, Some(ClarificationReason::MultipleInterpretations)),
            &[],
            &context_titles,
            &groups,
        );

        match disposition {
            AnswerDisposition::Clarify { variants } => {
                assert_eq!(variants, context_titles);
            }
            AnswerDisposition::Answer => {
                panic!("expected final context titles to preserve the variant menu")
            }
        }
    }

    #[test]
    fn disposition_answers_when_final_context_has_one_focused_document() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "Container Return Procedure".to_string(),
                excerpt: None,
                evidence_count: 4,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "node:1".to_string(),
                kind: GroupedReferenceKind::Entity,
                rank: 2,
                title: "return document".to_string(),
                excerpt: None,
                evidence_count: 2,
                support_ids: vec!["node:1".to_string()],
            },
        ];
        let retrieved_documents =
            vec![crate::services::query::execution::types::RuntimeRetrievedDocumentBrief {
                title: "Container Return Procedure".to_string(),
                preview_excerpt: String::new(),
                document_hint: None,
            }];
        let context_titles = vec!["Container Return Procedure".to_string()];

        let disposition = classify_answer_disposition_from_evidence(
            "how do i process container return?",
            &sample_ir(0.4, Some(ClarificationReason::MultipleInterpretations)),
            &retrieved_documents,
            &context_titles,
            &groups,
        );

        assert!(
            matches!(disposition, AnswerDisposition::Answer),
            "single focused document evidence should answer instead of clarifying on graph labels"
        );
    }

    #[test]
    fn disposition_answers_when_only_one_query_specific_variant_survives() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "TargetName Workflow Connector Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "Inventory Exchange Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "Embedded Browser Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:3".to_string()],
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "targetnme how",
            &sample_ir(0.4, Some(ClarificationReason::AmbiguousTooShort)),
            &[],
            &groups,
        );

        assert!(
            matches!(disposition, AnswerDisposition::Answer),
            "a single fuzzy topic match must answer from that document instead of clarifying on noise"
        );
    }

    #[test]
    fn disposition_does_not_clarify_from_unmatched_ranked_tail() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "Inventory Exchange Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "Embedded Browser Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "Notification Console Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:3".to_string()],
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "targetname how",
            &sample_ir(0.4, Some(ClarificationReason::AmbiguousTooShort)),
            &[],
            &groups,
        );

        assert!(
            matches!(disposition, AnswerDisposition::Answer),
            "unmatched tail labels must not be turned into a misleading clarify menu"
        );
    }

    #[test]
    fn disposition_ignores_question_word_substrings_in_variant_labels() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "Branch Director".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "Fruit Notes".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "Operations Handbook".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:3".to_string()],
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "who is TargetName?",
            &sample_ir(0.4, Some(ClarificationReason::AmbiguousTooShort)),
            &[],
            &groups,
        );

        assert!(
            matches!(disposition, AnswerDisposition::Answer),
            "question words must not match as substrings inside unrelated labels"
        );
    }

    #[test]
    fn disposition_keeps_short_acronym_variants_on_exact_token_match() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "API Gateway Alpha".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "Notification Console Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "API Gateway Beta".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:3".to_string()],
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "how configure api",
            &sample_ir(0.4, Some(ClarificationReason::AmbiguousTooShort)),
            &[],
            &groups,
        );

        match disposition {
            AnswerDisposition::Clarify { variants } => {
                assert_eq!(
                    variants,
                    vec!["API Gateway Alpha".to_string(), "API Gateway Beta".to_string()]
                );
            }
            AnswerDisposition::Answer => {
                panic!("expected exact short acronym matches to remain valid variants")
            }
        }
    }

    #[test]
    fn disposition_clarifies_with_multiple_fuzzy_query_specific_variants() {
        let groups = vec![
            GroupedReference {
                id: "document:1".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 1,
                title: "TargetName Subject Alpha Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:1".to_string()],
            },
            GroupedReference {
                id: "document:2".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 2,
                title: "Notification Console Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:2".to_string()],
            },
            GroupedReference {
                id: "document:3".to_string(),
                kind: GroupedReferenceKind::Document,
                rank: 3,
                title: "TargetName Subject Beta Manual".to_string(),
                excerpt: None,
                evidence_count: 3,
                support_ids: vec!["chunk:3".to_string()],
            },
        ];

        let disposition = classify_answer_disposition_from_groups(
            "targetnme how",
            &sample_ir(0.4, Some(ClarificationReason::AmbiguousTooShort)),
            &[],
            &groups,
        );

        match disposition {
            AnswerDisposition::Clarify { variants } => {
                assert_eq!(
                    variants,
                    vec![
                        "TargetName Subject Alpha Manual".to_string(),
                        "TargetName Subject Beta Manual".to_string(),
                    ]
                );
            }
            AnswerDisposition::Answer => {
                panic!("expected clarify disposition with multiple fuzzy topic matches")
            }
        }
    }

    // ── follow-up retrieval-query subject guard tests ───────────────────────

    use super::guarded_followup_retrieval_question;

    #[test]
    fn subject_guard_falls_back_when_rewrite_drops_the_scope_subject() {
        let question =
            "scope: Sample Subject update procedure\nquestion: give the detailed instruction";
        // Compiler rewrite kept only the refinement — zero scope tokens.
        let resolved = "detailed instruction steps";
        assert_eq!(guarded_followup_retrieval_question(resolved, question), question);
    }

    #[test]
    fn subject_guard_trusts_rewrite_that_retains_a_subject_token() {
        let question =
            "scope: Sample Subject update procedure\nquestion: give the detailed instruction";
        let resolved = "Sample Subject detailed update instruction";
        assert_eq!(guarded_followup_retrieval_question(resolved, question), resolved);
    }

    #[test]
    fn subject_guard_ignores_plain_unscoped_questions() {
        let question = "how to configure Sample Subject exports?";
        let resolved = "Sample Subject export configuration";
        assert_eq!(guarded_followup_retrieval_question(resolved, question), resolved);
    }

    #[test]
    fn subject_guard_passes_through_identity_resolution() {
        let question =
            "scope: Sample Subject update procedure\nquestion: give the detailed instruction";
        assert_eq!(guarded_followup_retrieval_question(question, question), question);
    }

    // ── release-lane entity clarify gate tests ──────────────────────────────

    use super::subjectless_release_inventory;

    /// Build a QueryIR that looks like a compiled "what's new in recent
    /// releases?" query: Enumerate act over release/version target types
    /// with no explicit subject named.
    fn release_inventory_ir() -> QueryIR {
        let mut ir = sample_ir_with_act(QueryAct::Enumerate, 0.94, None);
        ir.target_types = vec!["release".to_string(), "version".to_string()];
        ir.target_entities = Vec::new();
        ir.source_slice = None;
        ir
    }

    #[test]
    fn release_clarify_gate_fires_for_subjectless_release_inventory() {
        let ir = release_inventory_ir();
        assert!(subjectless_release_inventory(&ir, false));
    }

    #[test]
    fn release_clarify_gate_skips_when_query_names_a_subject() {
        let mut ir = release_inventory_ir();
        ir.target_entities =
            vec![EntityMention { label: "Sample Subject".to_string(), role: EntityRole::Subject }];
        assert!(!subjectless_release_inventory(&ir, false));
    }

    #[test]
    fn release_clarify_gate_skips_when_query_has_document_focus() {
        let mut ir = release_inventory_ir();
        ir.document_focus = Some(crate::domains::query_ir::DocumentHint {
            hint: "release notes archive".to_string(),
        });
        assert!(!subjectless_release_inventory(&ir, false));
    }

    #[test]
    fn release_clarify_gate_skips_when_subject_is_a_literal_constraint() {
        // A subject compiled as a literal (identifier) also scopes the
        // inventory lane via latest_version_scope_terms, so the gate must
        // treat the query as subject-bearing and skip the clarify probe.
        let mut ir = release_inventory_ir();
        ir.literal_constraints =
            vec![LiteralSpan { text: "alpha_suite".to_string(), kind: LiteralKind::Identifier }];
        assert!(!subjectless_release_inventory(&ir, false));
    }

    #[test]
    fn release_clarify_gate_ignores_version_and_numeric_literals() {
        // Version/numeric literals parameterize the slice (e.g. "last 3
        // releases") — they are not subjects and must not suppress clarify.
        let mut ir = release_inventory_ir();
        ir.literal_constraints =
            vec![LiteralSpan { text: "3".to_string(), kind: LiteralKind::NumericCode }];
        assert!(subjectless_release_inventory(&ir, false));
    }

    #[test]
    fn release_clarify_gate_skips_for_non_inventory_query() {
        // A plain procedure query (no release/version target types, no
        // context inventory shape) must never reach the clarify probe.
        let ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.94, None);
        assert!(!subjectless_release_inventory(&ir, false));
    }

    #[test]
    fn release_clarify_gate_fires_via_context_inventory_fallback() {
        // The inferred context-shape fallback (low-confidence subjectless
        // query whose context looks like a version family) also qualifies.
        let mut ir = sample_ir_with_act(QueryAct::Describe, 0.2, None);
        ir.target_entities = Vec::new();
        assert!(subjectless_release_inventory(&ir, true));
    }

    #[test]
    fn release_clarify_gate_requires_inventory_lane_even_when_subjectless() {
        let mut ir = sample_ir_with_act(QueryAct::Describe, 0.2, None);
        ir.target_entities = Vec::new();
        assert!(!subjectless_release_inventory(&ir, false));
    }

    // ---------------------------------------------------------------------------
    // RuntimeClarification builder unit tests
    // ---------------------------------------------------------------------------

    use super::{
        ReleaseEvidenceEntity, RuntimeClarification, disposition_clarification,
        release_clarification,
    };

    #[test]
    fn disposition_clarification_emits_candidates_for_each_variant() {
        let question = "Which component do you mean?";
        let variants = vec!["Sample Subject".to_string(), "Subject Beta".to_string()];
        let clar = disposition_clarification(question, &variants);

        assert!(clar.required);
        assert_eq!(clar.question.as_deref(), Some(question));
        assert_eq!(clar.answer_candidates.len(), 2);

        let first = &clar.answer_candidates[0];
        assert_eq!(first.label, "Sample Subject");
        assert_eq!(first.kind, "document");
        assert!(first.confidence.is_none());
        assert!(first.provenance.entity_id.is_none());
        assert!(first.provenance.document_id.is_none());
        assert!(first.provenance.chunk_id.is_none());
    }

    #[test]
    fn disposition_clarification_empty_variants_gives_no_candidates() {
        let clar = disposition_clarification("Which one?", &[]);
        assert!(clar.required);
        assert!(clar.answer_candidates.is_empty());
    }

    #[test]
    fn release_clarification_carries_entity_provenance() {
        let entity_id_a = Uuid::new_v4();
        let entity_id_b = Uuid::new_v4();
        let entities = vec![
            ReleaseEvidenceEntity {
                node_id: entity_id_a,
                label: "Adapter Alpha".to_string(),
                node_type: "service".to_string(),
            },
            ReleaseEvidenceEntity {
                node_id: entity_id_b,
                label: "Connector Beta".to_string(),
                node_type: "component".to_string(),
            },
        ];
        let question = "Which release are you asking about?";
        let clar = release_clarification(question, &entities);

        assert!(clar.required);
        assert_eq!(clar.question.as_deref(), Some(question));
        assert_eq!(clar.answer_candidates.len(), 2);

        let first = &clar.answer_candidates[0];
        assert_eq!(first.label, "Adapter Alpha");
        assert_eq!(first.kind, "service");
        assert!(first.confidence.is_none());
        assert_eq!(first.provenance.entity_id, Some(entity_id_a));
        assert!(first.provenance.document_id.is_none());

        let second = &clar.answer_candidates[1];
        assert_eq!(second.kind, "component");
        assert_eq!(second.provenance.entity_id, Some(entity_id_b));
    }

    #[test]
    fn structural_direct_answer_candidates_are_emitted_without_clarification() {
        let document_id_a = Uuid::new_v4();
        let document_id_b = Uuid::new_v4();
        let chunks = vec![
            setup_anchor_chunk(
                document_id_a,
                "Subject Subject Alpha",
                "alpha-subject",
                "/opt/subject/alpha.ini",
            ),
            setup_anchor_chunk(
                document_id_b,
                "Subject Subject Beta",
                "beta-subject",
                "/opt/subject/beta.ini",
            ),
        ];
        let clar = structural_direct_answer_candidates(
            &sample_ir_with_act(QueryAct::ConfigureHow, 0.9, None),
            &chunks,
        );

        assert!(!clar.required);
        assert!(clar.question.is_none());
        assert_eq!(clar.answer_candidates.len(), 2);
        assert_eq!(clar.answer_candidates[0].kind, "document");
        assert_eq!(clar.answer_candidates[0].provenance.document_id, Some(document_id_a));
        assert!(clar.answer_candidates[0].provenance.chunk_id.is_some());
        assert_eq!(clar.answer_candidates[1].provenance.document_id, Some(document_id_b));
    }

    #[test]
    fn unambiguous_path_clarification_is_not_required() {
        // Represent the non-clarify path: a caller that constructs default
        // RuntimeClarification (as answer_pipeline does for all non-clarify
        // outcomes).
        let clar = RuntimeClarification::default();
        assert!(!clar.required);
        assert!(clar.question.is_none());
        assert!(clar.answer_candidates.is_empty());
    }

    // Serde round-trips for the contract types.
    #[test]
    fn assistant_clarification_serde_roundtrip_empty() {
        use ironrag_contracts::assistant::AssistantClarification;
        let value = AssistantClarification::default();
        let json = serde_json::to_string(&value).expect("serialize");
        let back: AssistantClarification = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(value, back);
        // required=false, no question, empty candidates — the field must be
        // present in JSON but compact (skip_serializing_if on inner options).
        assert!(json.contains("\"required\":false"));
        assert!(!json.contains("\"question\""));
    }

    #[test]
    fn assistant_clarification_serde_roundtrip_with_candidates() {
        use ironrag_contracts::assistant::{
            AssistantAnswerCandidate, AssistantAnswerCandidateProvenance, AssistantClarification,
        };
        let entity_id = Uuid::new_v4();
        let value = AssistantClarification {
            required: true,
            question: Some("Which module?".to_string()),
            answer_candidates: vec![AssistantAnswerCandidate {
                label: "Synthetic Alpha".to_string(),
                kind: "service".to_string(),
                confidence: Some(0.9),
                provenance: AssistantAnswerCandidateProvenance {
                    entity_id: Some(entity_id),
                    document_id: None,
                    chunk_id: None,
                },
            }],
        };
        let json = serde_json::to_string(&value).expect("serialize");
        let back: AssistantClarification = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(value, back);
        assert!(json.contains("\"required\":true"));
        assert!(json.contains("\"question\""));
        assert!(json.contains("\"answerCandidates\""));
        assert!(json.contains("\"confidence\":0.9"));
    }
}
