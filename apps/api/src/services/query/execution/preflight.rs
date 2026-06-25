use std::collections::{HashMap, HashSet};

use anyhow::Context;
use uuid::Uuid;

use super::answer_kind::AnswerKind;
use crate::{
    app::state::AppState,
    domains::query_ir::{QueryAct, QueryIR, QueryScope, literal_text_is_identifier_shaped},
    infra::knowledge_rows::{KnowledgeDocumentRow, KnowledgeStructuredBlockRow},
    services::query::{
        effective_query::{current_question_segment, structured_current_question_segment},
        planner::QueryIntentProfile,
        text_match::label_terms,
    },
};

use super::{
    CanonicalAnswerEvidence, PreparedAnswerQueryResult, RuntimeChunkScoreKind, RuntimeMatchedChunk,
    augment_deterministic_grounded_answer_with_evidence, build_canonical_answer_context,
    build_deterministic_grounded_answer, build_missing_explicit_document_answer,
    build_setup_configuration_anchor_candidate, build_update_procedure_sequence_answer,
    load_canonical_answer_chunks, load_canonical_answer_evidence,
    load_direct_targeted_table_answer, load_document_index,
    question_intent::{QuestionIntent, classify_query_ir_intents, has_question_intent},
    question_intent::{
        canonical_target_type_tag, query_ir_has_focused_document_answer_intent,
        query_ir_targets_graph_entities_or_relationships,
    },
    question_requests_multi_document_scope,
    retrieve::{canonical_document_revision_id, merge_chunks, score_value},
    technical_literals::{
        TechnicalLiteralIntent, document_local_focus_keywords, extract_explicit_path_literals,
        extract_package_command_literals, extract_parameter_literals,
        select_document_balanced_chunks, technical_chunk_selection_score, technical_keyword_weight,
        technical_literal_candidate_limit, technical_literal_focus_keywords,
    },
};

const SETUP_PREFLIGHT_STRUCTURED_BLOCK_LIMIT: usize = 96;

#[derive(Debug, Clone)]
pub(super) struct CanonicalAnswerPreflight {
    pub(super) canonical_answer_chunks: Vec<RuntimeMatchedChunk>,
    pub(super) canonical_evidence: CanonicalAnswerEvidence,
    pub(super) prompt_context: String,
    pub(super) answer_override: Option<CanonicalAnswerOverride>,
}

#[derive(Debug, Clone)]
pub(super) struct CanonicalAnswerOverride {
    pub(super) answer: String,
    pub(super) answer_kind: AnswerKind,
}

pub(super) async fn prepare_canonical_answer_preflight(
    state: &AppState,
    library_id: Uuid,
    execution_id: Uuid,
    question: &str,
    prepared: &PreparedAnswerQueryResult,
) -> anyhow::Result<CanonicalAnswerPreflight> {
    let document_index = load_document_index(state, library_id).await?;
    let direct_targeted_table_answer = load_direct_targeted_table_answer(
        state,
        question,
        Some(&prepared.query_ir),
        &document_index,
    )
    .await?;
    let canonical_answer_chunks = load_canonical_answer_chunks(
        state,
        execution_id,
        question,
        &prepared.query_ir,
        &prepared.structured.context_chunks,
        &document_index,
    )
    .await?;
    let canonical_evidence = load_canonical_answer_evidence(state, execution_id).await?;
    let scoped_document_ids = preflight_exact_literal_document_scope(
        question,
        &prepared.query_ir,
        &prepared.structured.intent_profile,
        &prepared.structured.technical_literal_chunks,
    );
    let allow_empty_scope_fallback =
        preflight_allows_empty_scope_fallback(question, &prepared.query_ir);
    let mut preflight_answer_chunks = build_preflight_answer_chunks_for_scope(
        &canonical_answer_chunks,
        &prepared.structured.technical_literal_chunks,
        scoped_document_ids.as_ref(),
        allow_empty_scope_fallback,
    );
    if query_ir_requests_setup_literal_context(&prepared.query_ir) {
        extend_setup_preflight_chunks_from_structured_context(
            &mut preflight_answer_chunks,
            &prepared.structured.context_chunks,
            scoped_document_ids.as_ref(),
        );
    } else if query_ir_requests_low_confidence_setup_preflight(
        question,
        &prepared.query_ir,
        &prepared.structured.context_chunks,
    ) || query_ir_requests_structured_inventory_preflight(
        question,
        &prepared.query_ir,
        &prepared.structured.context_chunks,
    ) {
        extend_setup_preflight_chunks_from_structured_context(
            &mut preflight_answer_chunks,
            &prepared.structured.context_chunks,
            None,
        );
    }
    let mut preflight_evidence = build_preflight_canonical_evidence_for_scope(
        &canonical_evidence,
        scoped_document_ids.as_ref(),
        allow_empty_scope_fallback,
    );
    augment_setup_preflight_structured_blocks(
        state,
        question,
        &prepared.query_ir,
        &document_index,
        &preflight_answer_chunks,
        scoped_document_ids.as_ref(),
        &mut preflight_evidence,
    )
    .await?;
    let graph_evidence_context_lines = build_preflight_graph_evidence_context_lines(
        &prepared.structured.graph_evidence_context_lines,
    );
    let prompt_context = build_canonical_answer_context(
        question,
        &prepared.query_ir,
        prepared.structured.technical_literals_text.as_deref(),
        &preflight_evidence,
        &preflight_answer_chunks,
        &graph_evidence_context_lines,
    );
    let prompt_context = prepend_preflight_source_title_inventory(
        &document_index,
        &preflight_evidence,
        &preflight_answer_chunks,
        prompt_context,
    );
    let primary_answer_override = build_primary_preflight_answer_override(
        question,
        &document_index,
        direct_targeted_table_answer.as_deref(),
    );
    let answer_override = primary_answer_override
        .or_else(|| {
            build_update_procedure_sequence_answer(
                question,
                &prepared.query_ir,
                &preflight_answer_chunks,
            )
            .map(|answer| {
                let answer = augment_deterministic_grounded_answer_with_evidence(
                    answer,
                    question,
                    &prepared.query_ir,
                    &preflight_answer_chunks,
                );
                CanonicalAnswerOverride { answer, answer_kind: AnswerKind::UpdateProcedureSequence }
            })
        })
        .or_else(|| {
            build_setup_configuration_anchor_answer_override(
                question,
                &prepared.query_ir,
                &preflight_answer_chunks,
            )
            .map(|answer| CanonicalAnswerOverride {
                answer,
                answer_kind: AnswerKind::SetupConfigurationAnchor,
            })
        })
        .or_else(|| {
            build_canonical_preflight_answer(
                question,
                &prepared.query_ir,
                &prepared.structured.intent_profile,
                &document_index,
                direct_targeted_table_answer,
                &preflight_evidence,
                &preflight_answer_chunks,
            )
        });
    Ok(CanonicalAnswerPreflight {
        canonical_answer_chunks: preflight_answer_chunks,
        canonical_evidence: preflight_evidence,
        prompt_context,
        answer_override,
    })
}

fn build_setup_configuration_anchor_answer_override(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if !matches!(query_ir.act, QueryAct::ConfigureHow) {
        return None;
    }
    let answer = build_setup_configuration_anchor_candidate(question, query_ir, chunks)?;
    if answer.should_use_as_preflight_answer(query_ir, chunks) {
        Some(answer.into_answer())
    } else {
        None
    }
}

fn build_primary_preflight_answer_override(
    question: &str,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    direct_targeted_table_answer: Option<&str>,
) -> Option<CanonicalAnswerOverride> {
    build_missing_explicit_document_answer(question, document_index)
        .map(|answer| CanonicalAnswerOverride {
            answer,
            answer_kind: AnswerKind::MissingExplicitDocument,
        })
        .or_else(|| {
            direct_targeted_table_answer.map(|answer| CanonicalAnswerOverride {
                answer: answer.to_string(),
                answer_kind: AnswerKind::TargetedTableAnswer,
            })
        })
}

fn prepend_preflight_source_title_inventory(
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
    prompt_context: String,
) -> String {
    let mut seen = HashSet::<String>::new();
    let mut titles = Vec::<String>::new();
    for chunk in chunks {
        push_preflight_source_title(&mut titles, &mut seen, chunk.document_label.trim());
    }
    for block in &evidence.structured_blocks {
        if let Some(document) = document_index.get(&block.document_id) {
            push_preflight_source_title(
                &mut titles,
                &mut seen,
                preflight_document_title(document).as_deref().unwrap_or_default(),
            );
        }
    }
    for fact in &evidence.technical_facts {
        if let Some(document) = document_index.get(&fact.document_id) {
            push_preflight_source_title(
                &mut titles,
                &mut seen,
                preflight_document_title(document).as_deref().unwrap_or_default(),
            );
        }
    }
    if titles.len() < 2 {
        return prompt_context;
    }
    titles.truncate(24);
    let inventory = format!("Source title inventory\n- {}", titles.join("\n- "));
    if prompt_context.trim().is_empty() {
        inventory
    } else {
        format!("{inventory}\n\n{prompt_context}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::query_ir::{
        ConversationRefKind, DocumentHint, EntityMention, EntityRole, QueryLanguage, UnresolvedRef,
    };
    use crate::services::query::execution::consolidation::query_has_multi_document_setup_anchors;

    #[test]
    fn setup_configuration_anchor_override_skips_single_value_query() {
        let query_ir = query_ir(QueryAct::RetrieveValue, QueryScope::SingleDocument, &["port"]);
        let chunk = chunk(
            "Subject Alpha setup",
            "Component configuration\napply artifact-alpha\n\
             Settings are defined in /opt/subject/alpha/alpha.ini.\n\
             port = 443",
        );

        assert!(
            build_setup_configuration_anchor_answer_override(
                "which port does Subject Alpha use?",
                &query_ir,
                &[chunk],
            )
            .is_none()
        );
    }

    #[test]
    fn setup_configuration_anchor_override_skips_retrieve_value_multi_document_query() {
        let query_ir = query_ir(QueryAct::RetrieveValue, QueryScope::MultiDocument, &["parameter"]);
        let chunks = vec![
            chunk(
                "Subject Alpha setup",
                "Component configuration\napply artifact-alpha\n\
                 Settings are defined in /opt/subject/alpha/alpha.ini.\n\
                 primaryKey = \"\"",
            ),
            chunk(
                "Subject Beta setup",
                "Component configuration\napply artifact-beta\n\
                 Settings are defined in /opt/subject/beta/beta.conf.\n\
                 secondaryKey = \"\"",
            ),
        ];

        assert!(
            build_setup_configuration_anchor_answer_override(
                "which parameter configures Subject?",
                &query_ir,
                &chunks,
            )
            .is_none()
        );
    }

    #[test]
    fn setup_configuration_anchor_override_allows_multi_document_setup() {
        let mut query_ir =
            query_ir(QueryAct::ConfigureHow, QueryScope::MultiDocument, &["package"]);
        query_ir
            .target_entities
            .push(EntityMention { label: "Subject".to_string(), role: EntityRole::Subject });
        let chunks = vec![
            chunk(
                "Subject Alpha setup",
                "Component configuration\napply artifact-alpha\n\
                 Settings are defined in /opt/subject/alpha/alpha.ini.\n\
                 primaryKey = \"\"",
            ),
            chunk(
                "Subject Beta setup",
                "Component configuration\napply artifact-beta\n\
                 Settings are defined in /opt/subject/beta/beta.conf.\n\
                 secondaryKey = \"\"",
            ),
        ];

        let answer = build_setup_configuration_anchor_answer_override(
            "how to configure Subject?",
            &query_ir,
            &chunks,
        )
        .expect("multi-document setup override");

        assert!(answer.contains("Subject Alpha setup"));
        assert!(answer.contains("Subject Beta setup"));
    }

    #[test]
    fn setup_configuration_anchor_override_allows_soft_multi_variant_setup() {
        let mut query_ir =
            query_ir(QueryAct::ConfigureHow, QueryScope::MultiDocument, &["parameter"]);
        query_ir
            .target_entities
            .push(EntityMention { label: "Subject".to_string(), role: EntityRole::Subject });
        let chunks = vec![
            chunk(
                "Subject Alpha setup",
                "Settings are defined in /opt/subject/alpha/alpha.ini.\n\
                 [AlphaSubject]\n\
                 primaryKey = \"\"",
            ),
            chunk(
                "Subject Beta setup",
                "Settings are defined in /opt/subject/beta/beta.conf.\n\
                 [BetaSubject]\n\
                 secondaryKey = \"\"",
            ),
        ];

        assert!(!query_has_multi_document_setup_anchors(&query_ir, &chunks));
        let answer = build_setup_configuration_anchor_answer_override(
            "how to configure Subject?",
            &query_ir,
            &chunks,
        )
        .expect("soft multi-variant setup override");

        assert!(answer.contains("**Setup variants:**"));
        assert!(answer.contains("Subject Alpha setup"));
        assert!(answer.contains("Subject Beta setup"));
    }

    #[test]
    fn setup_configuration_anchor_override_skips_soft_multi_variant_document_focus() {
        let mut query_ir =
            query_ir(QueryAct::ConfigureHow, QueryScope::SingleDocument, &["parameter"]);
        query_ir.document_focus = Some(DocumentHint { hint: "Subject Alpha setup".to_string() });
        let chunks = vec![
            chunk(
                "Subject Alpha setup",
                "Settings are defined in /opt/subject/alpha/alpha.ini.\n\
                 [AlphaSubject]\n\
                 primaryKey = \"\"",
            ),
            chunk(
                "Subject Beta setup",
                "Settings are defined in /opt/subject/beta/beta.conf.\n\
                 [BetaSubject]\n\
                 secondaryKey = \"\"",
            ),
        ];

        assert!(
            build_setup_configuration_anchor_answer_override(
                "how to configure Subject Alpha?",
                &query_ir,
                &chunks,
            )
            .is_none()
        );
    }

    #[test]
    fn setup_configuration_anchor_override_allows_single_variant_with_parameter_details() {
        let mut query_ir =
            query_ir(QueryAct::ConfigureHow, QueryScope::SingleDocument, &["parameter"]);
        query_ir.document_focus = Some(DocumentHint { hint: "Subject Alpha setup".to_string() });
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mut anchor = chunk(
            "Subject Alpha setup",
            "Settings are defined in /opt/subject/alpha/alpha.ini.\n\
             [AlphaSubject]",
        );
        anchor.document_id = document_id;
        anchor.revision_id = revision_id;
        let mut parameter_row = chunk(
            "Subject Alpha setup",
            "Sheet: Component settings | Row 12 | Name: primaryKey | Type: string | \
             Description: Primary identifier | Notes: Required",
        );
        parameter_row.document_id = document_id;
        parameter_row.revision_id = revision_id;

        let answer = build_setup_configuration_anchor_answer_override(
            "how to configure Subject Alpha?",
            &query_ir,
            &[anchor, parameter_row],
        )
        .expect("single-variant parameter table override");

        assert!(answer.contains("**Parameter details:**"));
        assert!(answer.contains("primaryKey"));
        assert!(answer.contains("Primary identifier"));
    }

    #[test]
    fn setup_configuration_anchor_override_skips_describe_parameter_inventory() {
        let mut query_ir = query_ir(QueryAct::Describe, QueryScope::MultiDocument, &["parameter"]);
        query_ir
            .target_entities
            .push(EntityMention { label: "S1".to_string(), role: EntityRole::Subject });
        let chunks = vec![
            chunk(
                "S1 Alpha reference",
                "Settings are defined in /x/s1-alpha.conf.\n\
                 [Alpha]\n\
                 firstValue = \"\"",
            ),
            chunk(
                "S1 Beta reference",
                "Sheet: Parameters | Row 4 | Name: secondValue | Type: string | \
                 Description: Secondary value | Notes: Optional",
            ),
        ];

        assert!(
            build_setup_configuration_anchor_answer_override(
                "describe S1 values",
                &query_ir,
                &chunks
            )
            .is_none()
        );
    }

    #[test]
    fn canonical_preflight_does_not_skip_deterministic_candidates_only_for_follow_up_state() {
        let mut query_ir =
            query_ir(QueryAct::ConfigureHow, QueryScope::SingleDocument, &["procedure"]);
        query_ir.conversation_refs.push(UnresolvedRef {
            surface: "previous turn".to_string(),
            kind: ConversationRefKind::Deictic,
        });

        assert!(query_ir.is_follow_up());
        assert!(!canonical_preflight_requires_synthesis("how to update Alpha Service?", &query_ir));
    }

    fn query_ir(act: QueryAct, scope: QueryScope, target_types: &[&str]) -> QueryIR {
        QueryIR {
            act,
            scope,
            language: QueryLanguage::Auto,
            target_types: target_types.iter().map(|value| (*value).to_string()).collect(),
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 0.95,
        }
    }

    fn chunk(label: &str, text: &str) -> RuntimeMatchedChunk {
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 1,
            chunk_kind: Some("paragraph".to_string()),
            document_label: label.to_string(),
            excerpt: text.to_string(),
            score_kind: RuntimeChunkScoreKind::Relevance,
            score: Some(3.0),
            source_text: text.to_string(),
        }
    }
}

fn preflight_document_title(document: &KnowledgeDocumentRow) -> Option<String> {
    document
        .title
        .as_deref()
        .or(document.file_name.as_deref())
        .or(document.document_hint.as_deref())
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(ToString::to_string)
}

fn push_preflight_source_title(titles: &mut Vec<String>, seen: &mut HashSet<String>, title: &str) {
    let title = title.trim();
    if title.is_empty()
        || title.chars().count() > 240
        || !title.chars().any(|ch| ch.is_alphanumeric())
    {
        return;
    }
    if seen.insert(title.to_lowercase()) {
        titles.push(title.to_string());
    }
}

pub(super) fn build_canonical_preflight_answer(
    question: &str,
    query_ir: &crate::domains::query_ir::QueryIR,
    intent_profile: &QueryIntentProfile,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    direct_targeted_table_answer: Option<String>,
    canonical_evidence: &CanonicalAnswerEvidence,
    canonical_answer_chunks: &[RuntimeMatchedChunk],
) -> Option<CanonicalAnswerOverride> {
    let missing_explicit_document_answer =
        build_missing_explicit_document_answer(question, document_index);
    let requires_synthesis = canonical_preflight_requires_synthesis(question, query_ir);
    let deterministic_grounded_answer = if requires_synthesis {
        None
    } else {
        build_deterministic_grounded_answer(
            question,
            query_ir,
            canonical_evidence,
            canonical_answer_chunks,
        )
    };

    if intent_profile.exact_literal_technical {
        // Telemetry stays content-free: emit shapes and counts only, never the
        // verbatim question text or document/chunk previews. Diagnostics export
        // (traces especially, which are on by default) must not carry user
        // content; the persisted query turn in Postgres holds it for operator
        // debugging.
        tracing::info!(
            question_len = question.chars().count(),
            chunk_count = canonical_answer_chunks.len(),
            chunk_document_count = canonical_answer_chunks
                .iter()
                .map(|chunk| chunk.document_id)
                .collect::<HashSet<_>>()
                .len(),
            technical_fact_count = canonical_evidence.technical_facts.len(),
            structured_block_count = canonical_evidence.structured_blocks.len(),
            has_missing_explicit_document_answer = missing_explicit_document_answer.is_some(),
            has_direct_targeted_table_answer = direct_targeted_table_answer.is_some(),
            has_deterministic_grounded_answer = deterministic_grounded_answer.is_some(),
            requires_synthesis,
            "exact technical preflight decision"
        );
    }

    if let Some(answer) = missing_explicit_document_answer {
        return Some(CanonicalAnswerOverride {
            answer,
            answer_kind: AnswerKind::MissingExplicitDocument,
        });
    }
    if let Some(answer) = direct_targeted_table_answer {
        return Some(CanonicalAnswerOverride {
            answer,
            answer_kind: AnswerKind::TargetedTableAnswer,
        });
    }
    deterministic_grounded_answer.map(|answer| CanonicalAnswerOverride {
        answer,
        answer_kind: AnswerKind::DeterministicGroundedAnswer,
    })
}

fn canonical_preflight_requires_synthesis(question: &str, query_ir: &QueryIR) -> bool {
    scoped_setup_literal_inventory_requires_synthesis(question, query_ir)
}

fn scoped_setup_literal_inventory_requires_synthesis(question: &str, query_ir: &QueryIR) -> bool {
    query_ir_requests_setup_literal_context(query_ir)
        && !current_question_has_exact_technical_surface(question)
        && (structured_current_question_segment(question).is_some()
            || !query_ir.conversation_refs.is_empty())
}

pub(super) fn build_preflight_graph_evidence_context_lines(
    graph_evidence_context_lines: &[String],
) -> Vec<String> {
    graph_evidence_context_lines.to_vec()
}

#[cfg(test)]
pub(super) fn build_preflight_answer_chunks(
    question: &str,
    query_ir: &QueryIR,
    intent_profile: &QueryIntentProfile,
    canonical_answer_chunks: &[RuntimeMatchedChunk],
    technical_literal_chunks: &[RuntimeMatchedChunk],
) -> Vec<RuntimeMatchedChunk> {
    let scoped_document_ids = preflight_exact_literal_document_scope(
        question,
        query_ir,
        intent_profile,
        technical_literal_chunks,
    );
    build_preflight_answer_chunks_for_scope(
        canonical_answer_chunks,
        technical_literal_chunks,
        scoped_document_ids.as_ref(),
        preflight_allows_empty_scope_fallback(question, query_ir),
    )
}

pub(super) fn select_technical_literal_chunks(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
    technical_literal_intent: TechnicalLiteralIntent,
    top_k: usize,
    literal_focus_keywords: &[String],
    preferred_document_ids: &[Uuid],
    pagination_requested: bool,
) -> Vec<RuntimeMatchedChunk> {
    let setup_literal_context = query_ir_requests_setup_literal_context(query_ir);
    let max_total_chunks = if setup_literal_context {
        top_k.saturating_mul(4).clamp(24, 64)
    } else if technical_literal_intent.any() {
        technical_literal_candidate_limit(technical_literal_intent, top_k)
    } else {
        12
    };
    let max_chunks_per_document = if setup_literal_context {
        24
    } else if technical_literal_intent.any() {
        4
    } else {
        3
    };
    let focused_chunks = if technical_literal_intent.any()
        && question_prefers_single_exact_literal_scope(question, query_ir)
    {
        let focused_document_id = if setup_literal_context {
            select_setup_literal_document_id(question, query_ir, chunks)
                .or_else(|| select_preflight_literal_document_id(question, query_ir, chunks))
                .or_else(|| {
                    select_preflight_literal_document_id_from_preferred(
                        question,
                        query_ir,
                        chunks,
                        preferred_document_ids,
                    )
                })
        } else {
            select_preflight_literal_document_id_from_preferred(
                question,
                query_ir,
                chunks,
                preferred_document_ids,
            )
            .or_else(|| select_preflight_literal_document_id(question, query_ir, chunks))
        };
        focused_document_id.map(|document_id| {
            chunks
                .iter()
                .filter(|chunk| chunk.document_id == document_id)
                .cloned()
                .collect::<Vec<_>>()
        })
    } else {
        None
    };
    let candidate_chunks = focused_chunks.as_deref().unwrap_or(chunks);
    let mut selected = select_document_balanced_chunks(
        question,
        Some(query_ir),
        candidate_chunks,
        literal_focus_keywords,
        pagination_requested,
        max_total_chunks,
        max_chunks_per_document,
    )
    .into_iter()
    .cloned()
    .collect::<Vec<_>>();
    if setup_literal_context {
        append_setup_literal_chunks(&mut selected, candidate_chunks, max_total_chunks);
    }
    selected
}

pub(super) fn query_ir_requests_setup_literal_context(query_ir: &QueryIR) -> bool {
    if !matches!(
        query_ir.act,
        QueryAct::ConfigureHow | QueryAct::Describe | QueryAct::RetrieveValue
    ) {
        return false;
    }
    let has_focus_signal = query_ir.document_focus.is_some()
        || !query_ir.target_entities.is_empty()
        || !query_ir.literal_constraints.is_empty()
        || !query_ir.conversation_refs.is_empty();
    let requests_configuration = query_ir.target_types.iter().any(|target_type| {
        matches!(
            canonical_target_type_tag(target_type).as_str(),
            "configuration_file" | "config_key"
        )
    });
    let requests_module_or_parameter = query_ir.target_types.iter().any(|target_type| {
        matches!(canonical_target_type_tag(target_type).as_str(), "package" | "parameter")
    });
    if requests_configuration && requests_module_or_parameter {
        return true;
    }
    if requests_configuration && has_focus_signal {
        return true;
    }
    matches!(query_ir.act, QueryAct::ConfigureHow)
        && (requests_configuration || requests_module_or_parameter)
        && has_focus_signal
}

pub(super) fn query_ir_requests_low_confidence_setup_preflight(
    question: &str,
    query_ir: &QueryIR,
    context_chunks: &[RuntimeMatchedChunk],
) -> bool {
    (query_ir_low_confidence_unfocused_descriptive_setup(query_ir)
        || query_ir_low_confidence_structural_descriptive_setup(query_ir))
        && context_chunks.iter().any(|chunk| {
            low_confidence_context_chunk_requests_setup_bridge(question, query_ir, chunk)
        })
}

fn query_ir_requests_structured_inventory_preflight(
    question: &str,
    query_ir: &QueryIR,
    context_chunks: &[RuntimeMatchedChunk],
) -> bool {
    if query_ir.source_slice.is_some()
        || !matches!(
            query_ir.act,
            QueryAct::Compare | QueryAct::Describe | QueryAct::Enumerate | QueryAct::RetrieveValue
        )
        || !context_chunks.iter().any(|chunk| {
            chunk.score_kind == RuntimeChunkScoreKind::SourceContext
                || chunk.chunk_kind.as_deref() == Some(super::SOURCE_UNIT_CHUNK_KIND)
        })
    {
        return false;
    }
    let intents = classify_query_ir_intents(query_ir);
    if has_question_intent(&intents, QuestionIntent::Port)
        || has_question_intent(&intents, QuestionIntent::Protocol)
        || has_question_intent(&intents, QuestionIntent::ConfigKey)
        || has_question_intent(&intents, QuestionIntent::Parameter)
        || has_question_intent(&intents, QuestionIntent::ErrorCode)
        || !query_ir.target_types.is_empty()
        || !query_ir.target_entities.is_empty()
        || !query_ir.literal_constraints.is_empty()
    {
        return true;
    }

    let focus_terms = structured_inventory_preflight_focus_terms(question, query_ir);
    !focus_terms.is_empty()
        && context_chunks
            .iter()
            .filter(|chunk| {
                chunk.score_kind == RuntimeChunkScoreKind::SourceContext
                    || chunk.chunk_kind.as_deref() == Some(super::SOURCE_UNIT_CHUNK_KIND)
            })
            .any(|chunk| structured_inventory_preflight_overlap_score(chunk, &focus_terms) >= 2)
}

fn structured_inventory_preflight_focus_terms(
    question: &str,
    query_ir: &QueryIR,
) -> HashSet<String> {
    let mut terms =
        label_terms(&current_question_segment(question), 3).into_iter().collect::<HashSet<_>>();
    for target_type in &query_ir.target_types {
        terms.extend(label_terms(target_type, 2));
    }
    for entity in &query_ir.target_entities {
        terms.extend(label_terms(&entity.label, 2));
    }
    for literal in &query_ir.literal_constraints {
        terms.extend(label_terms(&literal.text, 2));
    }
    terms
}

fn structured_inventory_preflight_overlap_score(
    chunk: &RuntimeMatchedChunk,
    focus_terms: &HashSet<String>,
) -> usize {
    label_terms(&format!("{}\n{}\n{}", chunk.document_label, chunk.excerpt, chunk.source_text), 2)
        .into_iter()
        .filter(|term| focus_terms.contains(term))
        .count()
}

fn query_ir_low_confidence_unfocused_descriptive_setup(query_ir: &QueryIR) -> bool {
    query_ir.confidence <= 0.3
        && matches!(query_ir.scope, QueryScope::SingleDocument)
        && matches!(query_ir.act, QueryAct::Describe | QueryAct::ConfigureHow)
        && query_ir.source_slice.is_none()
        && query_ir.document_focus.is_none()
        && query_ir.target_types.is_empty()
        && query_ir.target_entities.is_empty()
        && query_ir.literal_constraints.is_empty()
}

fn query_ir_low_confidence_structural_descriptive_setup(query_ir: &QueryIR) -> bool {
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

fn low_confidence_context_chunk_requests_setup_bridge(
    question: &str,
    query_ir: &QueryIR,
    chunk: &RuntimeMatchedChunk,
) -> bool {
    let text = format!("{}\n{}", chunk.excerpt, chunk.source_text);
    let setup_score = setup_literal_chunk_score(&text);
    setup_score.anchor_score > 0
        || (setup_score.total_score >= 3
            && chunk.score_kind == RuntimeChunkScoreKind::SourceContext)
        || (chunk.score_kind == RuntimeChunkScoreKind::SourceContext
            && technical_literal_focus_keywords(question, Some(query_ir))
                .iter()
                .any(|keyword| keyword.chars().count() < 4)
            && !extract_parameter_literals(&text, 2).is_empty())
}

fn select_setup_literal_document_id(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<Uuid> {
    if chunks.is_empty() {
        return None;
    }

    #[derive(Debug)]
    struct SetupLiteralDocumentCandidate {
        document_id: Uuid,
        label_score: usize,
        setup_anchor_score: usize,
        setup_score: usize,
        best_chunk_signal: isize,
        retrieval_score_sum: f32,
        first_rank: usize,
    }

    let label_keywords = preflight_target_label_keywords(query_ir);
    let question_keywords = technical_literal_focus_keywords(question, Some(query_ir));
    let pagination_requested = false;
    let mut ordered_document_ids = Vec::<Uuid>::new();
    let mut per_document_chunks = HashMap::<Uuid, Vec<&RuntimeMatchedChunk>>::new();
    for chunk in chunks {
        if !per_document_chunks.contains_key(&chunk.document_id) {
            ordered_document_ids.push(chunk.document_id);
        }
        per_document_chunks.entry(chunk.document_id).or_default().push(chunk);
    }

    let mut candidates = ordered_document_ids
        .iter()
        .enumerate()
        .filter_map(|(first_rank, document_id)| {
            let document_chunks = per_document_chunks.get(document_id)?;
            let document_label = document_chunks.first()?.document_label.to_lowercase();
            let label_score = label_keywords
                .iter()
                .map(|keyword| technical_keyword_weight(&document_label, keyword))
                .sum::<usize>();
            let local_keywords = document_local_focus_keywords(
                question,
                Some(query_ir),
                document_chunks,
                &question_keywords,
            );
            let mut setup_anchor_score = 0usize;
            let mut setup_score = 0usize;
            let mut best_chunk_signal = isize::MIN;
            let mut retrieval_score_sum = 0.0f32;
            for chunk in document_chunks {
                let text = format!("{} {}", chunk.excerpt, chunk.source_text);
                let chunk_setup_score = setup_literal_chunk_score(&text);
                setup_anchor_score =
                    setup_anchor_score.saturating_add(chunk_setup_score.anchor_score);
                setup_score = setup_score.saturating_add(chunk_setup_score.total_score);
                best_chunk_signal = best_chunk_signal.max(technical_chunk_selection_score(
                    &text,
                    &local_keywords,
                    pagination_requested,
                ));
                retrieval_score_sum += score_value(chunk.score);
            }
            (label_score > 0 || setup_score > 0).then_some(SetupLiteralDocumentCandidate {
                document_id: *document_id,
                label_score,
                setup_anchor_score,
                setup_score,
                best_chunk_signal,
                retrieval_score_sum,
                first_rank,
            })
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }
    if candidates.iter().any(|candidate| candidate.setup_anchor_score > 0) {
        candidates.retain(|candidate| candidate.setup_anchor_score > 0);
    } else if candidates.iter().any(|candidate| candidate.setup_score > 0) {
        candidates.retain(|candidate| candidate.setup_score > 0);
    }

    candidates.sort_by(|left, right| {
        right
            .label_score
            .cmp(&left.label_score)
            .then_with(|| right.setup_anchor_score.cmp(&left.setup_anchor_score))
            .then_with(|| right.setup_score.cmp(&left.setup_score))
            .then_with(|| right.best_chunk_signal.cmp(&left.best_chunk_signal))
            .then_with(|| right.retrieval_score_sum.total_cmp(&left.retrieval_score_sum))
            .then_with(|| left.first_rank.cmp(&right.first_rank))
            .then_with(|| left.document_id.cmp(&right.document_id))
    });

    Some(candidates[0].document_id)
}

#[derive(Debug, Clone, Copy, Default)]
struct SetupLiteralChunkScore {
    anchor_score: usize,
    total_score: usize,
}

fn setup_literal_chunk_score(text: &str) -> SetupLiteralChunkScore {
    let package_score = extract_package_command_literals(text, 4).len().saturating_mul(16);
    let path_score = setup_literal_configuration_path_count(text).saturating_mul(24);
    let assignment_score = setup_literal_assignment_count(text).saturating_mul(10);
    let section_score = setup_literal_section_count(text).saturating_mul(8);
    let parameter_score = extract_parameter_literals(text, 32).len().saturating_mul(3);
    let anchor_score = package_score
        .saturating_add(path_score)
        .saturating_add(assignment_score)
        .saturating_add(section_score);
    SetupLiteralChunkScore {
        anchor_score,
        total_score: anchor_score.saturating_add(parameter_score),
    }
}

fn setup_literal_configuration_path_count(text: &str) -> usize {
    extract_explicit_path_literals(text, 16)
        .into_iter()
        .filter(|path| setup_literal_path_has_configuration_extension(path))
        .count()
}

fn setup_literal_path_has_configuration_extension(path: &str) -> bool {
    let lowered = path.to_ascii_lowercase();
    [".conf", ".ini", ".cfg", ".properties", ".yaml", ".yml", ".toml", ".json"]
        .iter()
        .any(|extension| lowered.ends_with(extension))
}

fn setup_literal_assignment_count(text: &str) -> usize {
    text.split_whitespace()
        .filter(|token| {
            let Some((name, _)) = token.split_once('=') else {
                return false;
            };
            let name = name.trim_matches(|ch: char| {
                matches!(ch, '`' | '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}')
            });
            let Some(first) = name.chars().next() else {
                return false;
            };
            first.is_ascii_alphabetic()
                && name.chars().any(|ch| ch.is_ascii_alphabetic())
                && name
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        })
        .take(16)
        .count()
}

fn setup_literal_section_count(text: &str) -> usize {
    text.split_whitespace()
        .filter(|token| {
            let cleaned = token.trim_matches(|ch: char| {
                matches!(ch, '`' | '"' | '\'' | ',' | ';' | ':' | '.' | '(' | ')' | '{' | '}')
            });
            cleaned.len() > 2 && cleaned.starts_with('[') && cleaned.ends_with(']')
        })
        .take(16)
        .count()
}

fn append_setup_literal_chunks(
    selected: &mut Vec<RuntimeMatchedChunk>,
    candidate_chunks: &[RuntimeMatchedChunk],
    max_total_chunks: usize,
) {
    if selected.len() >= max_total_chunks {
        return;
    }
    let selected_ids = selected.iter().map(|chunk| chunk.chunk_id).collect::<HashSet<_>>();
    let mut candidates = candidate_chunks
        .iter()
        .filter(|chunk| !selected_ids.contains(&chunk.chunk_id))
        .filter_map(|chunk| {
            let package_count = extract_package_command_literals(&chunk.source_text, 2).len();
            let config_path_count = extract_explicit_path_literals(&chunk.source_text, 4)
                .into_iter()
                .filter(|path| {
                    let lowered = path.to_ascii_lowercase();
                    lowered.ends_with(".conf") || lowered.ends_with(".ini")
                })
                .count();
            (package_count > 0 && config_path_count > 0).then_some((
                package_count,
                config_path_count,
                chunk,
            ))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(
        |(left_packages, left_paths, left), (right_packages, right_paths, right)| {
            right_packages
                .cmp(left_packages)
                .then_with(|| right_paths.cmp(left_paths))
                .then_with(|| score_value(right.score).total_cmp(&score_value(left.score)))
                .then_with(|| left.document_id.cmp(&right.document_id))
                .then_with(|| left.chunk_index.cmp(&right.chunk_index))
                .then_with(|| left.chunk_id.cmp(&right.chunk_id))
        },
    );
    for (_, _, chunk) in candidates {
        if selected.len() >= max_total_chunks {
            break;
        }
        selected.push(chunk.clone());
    }
    if selected.len() >= max_total_chunks {
        return;
    }

    let selected_ids = selected.iter().map(|chunk| chunk.chunk_id).collect::<HashSet<_>>();
    let selected_documents = selected.iter().map(|chunk| chunk.document_id).collect::<HashSet<_>>();
    let mut parameter_candidates = candidate_chunks
        .iter()
        .filter(|chunk| {
            selected_documents.is_empty() || selected_documents.contains(&chunk.document_id)
        })
        .filter(|chunk| !selected_ids.contains(&chunk.chunk_id))
        .filter_map(|chunk| {
            let parameter_count = extract_parameter_literals(&chunk.source_text, 16).len();
            (parameter_count > 0).then_some((parameter_count, chunk))
        })
        .collect::<Vec<_>>();
    parameter_candidates.sort_by(|(left_count, left), (right_count, right)| {
        right_count
            .cmp(left_count)
            .then_with(|| left.document_id.cmp(&right.document_id))
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    for (_, chunk) in parameter_candidates {
        if selected.len() >= max_total_chunks {
            break;
        }
        selected.push(chunk.clone());
    }
}

#[cfg(test)]
pub(super) fn build_preflight_canonical_evidence(
    question: &str,
    query_ir: &QueryIR,
    intent_profile: &QueryIntentProfile,
    canonical_evidence: &CanonicalAnswerEvidence,
    technical_literal_chunks: &[RuntimeMatchedChunk],
) -> CanonicalAnswerEvidence {
    let scoped_document_ids = preflight_exact_literal_document_scope(
        question,
        query_ir,
        intent_profile,
        technical_literal_chunks,
    );
    build_preflight_canonical_evidence_for_scope(
        canonical_evidence,
        scoped_document_ids.as_ref(),
        preflight_allows_empty_scope_fallback(question, query_ir),
    )
}

fn preflight_allows_empty_scope_fallback(_question: &str, query_ir: &QueryIR) -> bool {
    query_ir.is_follow_up()
}

pub(super) fn preflight_exact_literal_document_scope(
    question: &str,
    query_ir: &QueryIR,
    intent_profile: &QueryIntentProfile,
    technical_literal_chunks: &[RuntimeMatchedChunk],
) -> Option<HashSet<Uuid>> {
    if query_ir_has_focused_document_answer_intent(query_ir) {
        return None;
    }
    if has_question_intent(&classify_query_ir_intents(query_ir), QuestionIntent::ErrorCode) {
        return None;
    }
    if query_ir_requests_open_descriptive_context(query_ir) {
        return None;
    }
    if query_ir_requests_transport_inventory_scope(query_ir) {
        return None;
    }
    if query_ir_low_confidence_unfocused_descriptive_setup(query_ir)
        && !current_question_has_exact_preflight_scope_surface(question)
    {
        return None;
    }
    if !intent_profile.exact_literal_technical || technical_literal_chunks.is_empty() {
        return None;
    }

    if !question_prefers_single_exact_literal_scope(question, query_ir) {
        return Some(
            technical_literal_chunks.iter().map(|chunk| chunk.document_id).collect::<HashSet<_>>(),
        );
    }

    select_preflight_literal_document_id(question, query_ir, technical_literal_chunks)
        .map(|document_id| HashSet::from([document_id]))
        .or_else(|| {
            Some(
                technical_literal_chunks
                    .iter()
                    .map(|chunk| chunk.document_id)
                    .collect::<HashSet<_>>(),
            )
        })
}

fn query_ir_requests_transport_inventory_scope(query_ir: &QueryIR) -> bool {
    if !query_ir.literal_constraints.is_empty() || query_ir.source_slice.is_some() {
        return false;
    }
    let intents = classify_query_ir_intents(query_ir);
    (has_question_intent(&intents, QuestionIntent::Port)
        && has_question_intent(&intents, QuestionIntent::Protocol))
        || query_ir
            .target_types
            .iter()
            .any(|target_type| target_type.trim().eq_ignore_ascii_case("connection"))
}

fn query_ir_requests_open_descriptive_context(query_ir: &QueryIR) -> bool {
    if !query_ir.literal_constraints.is_empty()
        || query_ir.source_slice.is_some()
        || query_ir.is_follow_up()
        || query_ir_has_focused_document_answer_intent(query_ir)
        || query_ir_requests_setup_literal_context(query_ir)
    {
        return false;
    }
    if !matches!(query_ir.act, QueryAct::Compare | QueryAct::Describe | QueryAct::RetrieveValue) {
        return false;
    }
    let intents = classify_query_ir_intents(query_ir);
    if !intents.is_empty() {
        return false;
    }
    !query_ir.target_types.is_empty() && query_ir_targets_graph_entities_or_relationships(query_ir)
}

pub(super) fn question_prefers_single_exact_literal_scope(
    question: &str,
    query_ir: &QueryIR,
) -> bool {
    if question_requests_multi_document_scope(question, Some(query_ir)) {
        return false;
    }
    if query_ir.is_follow_up() && !current_question_has_exact_technical_surface(question) {
        return false;
    }
    if query_ir_requests_setup_literal_context(query_ir) {
        return true;
    }
    if query_ir_targets_multiple_technical_literal_families(query_ir) {
        return false;
    }
    !matches!(query_ir.act, crate::domains::query_ir::QueryAct::Enumerate)
}

fn current_question_has_exact_technical_surface(question: &str) -> bool {
    let current = current_question_segment(question);
    current.contains("http://")
        || current.contains("https://")
        || current.contains('/')
        || current
            .split_whitespace()
            .map(|token| {
                token.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '.')
            })
            .any(literal_text_is_identifier_shaped)
}

fn current_question_has_exact_preflight_scope_surface(question: &str) -> bool {
    let current = current_question_segment(question);
    current.contains("http://")
        || current.contains("https://")
        || current.contains('/')
        || current
            .split_whitespace()
            .map(|token| {
                token.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '.')
            })
            .any(token_has_exact_preflight_scope_surface)
}

fn token_has_exact_preflight_scope_surface(token: &str) -> bool {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return false;
    }
    let mut has_numeric = false;
    let mut has_separator = false;
    let mut seen_lowercase_before = false;
    let mut has_uppercase_after_lowercase = false;

    for ch in trimmed.chars() {
        if ch.is_alphabetic() {
            if ch.is_uppercase() {
                has_uppercase_after_lowercase |= seen_lowercase_before;
            }
            if ch.is_lowercase() {
                seen_lowercase_before = true;
            }
        } else if ch.is_numeric() {
            has_numeric = true;
        } else if matches!(ch, '_' | '-' | '.') {
            has_separator = true;
        } else {
            return false;
        }
    }

    has_separator || has_numeric || has_uppercase_after_lowercase
}

fn query_ir_targets_multiple_technical_literal_families(query_ir: &QueryIR) -> bool {
    let mut families = HashSet::new();
    for target_type in query_ir.target_types.iter().map(|value| value.trim().to_ascii_lowercase()) {
        let Some(family) = technical_literal_target_family(&target_type) else {
            continue;
        };
        families.insert(family);
        if families.len() > 1 {
            return true;
        }
    }
    false
}

fn technical_literal_target_family(target_type: &str) -> Option<&'static str> {
    match target_type {
        "endpoint" | "url" | "path" | "wsdl" | "base_url" | "http_method" | "protocol" => {
            Some("interface")
        }
        "configuration_file" | "filesystem_path" | "config_key" | "parameter" | "env_var" => {
            Some("configuration")
        }
        "software_module" | "package" => Some("module"),
        _ => None,
    }
}

pub(super) fn select_preflight_literal_document_id(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<Uuid> {
    if chunks.is_empty() {
        return None;
    }

    #[derive(Debug)]
    struct ExactLiteralDocumentCandidate<'a> {
        document_id: Uuid,
        document_label: &'a str,
        focus_label_score: usize,
        target_label_score: usize,
        label_score: usize,
        best_chunk_signal: isize,
        chunk_signal_sum: isize,
        retrieval_score_sum: f32,
        first_rank: usize,
    }

    let question_keywords = technical_literal_focus_keywords(question, Some(query_ir));
    let focus_label_keywords = preflight_document_focus_label_keywords(query_ir);
    let target_label_keywords = preflight_target_label_keywords(query_ir);
    let pagination_requested = false;
    let mut ordered_document_ids = Vec::<Uuid>::new();
    let mut per_document_chunks = HashMap::<Uuid, Vec<&RuntimeMatchedChunk>>::new();
    for chunk in chunks {
        if !per_document_chunks.contains_key(&chunk.document_id) {
            ordered_document_ids.push(chunk.document_id);
        }
        per_document_chunks.entry(chunk.document_id).or_default().push(chunk);
    }

    let mut candidates = ordered_document_ids
        .iter()
        .enumerate()
        .filter_map(|(first_rank, document_id)| {
            let document_chunks = per_document_chunks.get(document_id)?;
            let local_keywords = document_local_focus_keywords(
                question,
                Some(query_ir),
                document_chunks,
                &question_keywords,
            );
            let document_label = document_chunks.first()?.document_label.as_str();
            let lowered_label = document_label.to_lowercase();
            let label_score = question_keywords
                .iter()
                .map(|keyword| technical_keyword_weight(&lowered_label, keyword))
                .sum::<usize>();
            let focus_label_score = focus_label_keywords
                .iter()
                .map(|keyword| technical_keyword_weight(&lowered_label, keyword))
                .sum::<usize>();
            let target_label_score = target_label_keywords
                .iter()
                .map(|keyword| technical_keyword_weight(&lowered_label, keyword))
                .sum::<usize>();
            let (best_chunk_signal, chunk_signal_sum, retrieval_score_sum) =
                document_chunks.iter().fold(
                    (isize::MIN, 0isize, 0.0f32),
                    |(best_chunk_signal, chunk_signal_sum, retrieval_score_sum), chunk| {
                        let chunk_signal = technical_chunk_selection_score(
                            &format!("{} {}", chunk.excerpt, chunk.source_text),
                            &local_keywords,
                            pagination_requested,
                        );
                        (
                            best_chunk_signal.max(chunk_signal),
                            chunk_signal_sum + chunk_signal,
                            retrieval_score_sum + score_value(chunk.score),
                        )
                    },
                );
            Some(ExactLiteralDocumentCandidate {
                document_id: *document_id,
                document_label,
                focus_label_score,
                target_label_score,
                label_score,
                best_chunk_signal,
                chunk_signal_sum,
                retrieval_score_sum,
                first_rank,
            })
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }

    candidates.sort_by(|left, right| {
        right
            .focus_label_score
            .cmp(&left.focus_label_score)
            .then_with(|| right.best_chunk_signal.cmp(&left.best_chunk_signal))
            .then_with(|| right.chunk_signal_sum.cmp(&left.chunk_signal_sum))
            .then_with(|| right.target_label_score.cmp(&left.target_label_score))
            .then_with(|| right.label_score.cmp(&left.label_score))
            .then_with(|| right.retrieval_score_sum.total_cmp(&left.retrieval_score_sum))
            .then_with(|| left.first_rank.cmp(&right.first_rank))
            .then_with(|| left.document_label.cmp(right.document_label))
    });

    Some(candidates[0].document_id)
}

fn select_preflight_literal_document_id_from_preferred(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
    preferred_document_ids: &[Uuid],
) -> Option<Uuid> {
    if chunks.is_empty() || preferred_document_ids.is_empty() {
        return None;
    }
    let preferred = preferred_document_ids.iter().copied().collect::<HashSet<_>>();
    let preferred_chunks = chunks
        .iter()
        .filter(|chunk| preferred.contains(&chunk.document_id))
        .cloned()
        .collect::<Vec<_>>();
    select_preflight_literal_document_id(question, query_ir, &preferred_chunks)
}

fn preflight_document_focus_label_keywords(query_ir: &QueryIR) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut keywords = Vec::new();
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        push_preflight_label_keywords(&document_focus.hint, &mut seen, &mut keywords);
    }
    keywords
}

fn preflight_target_label_keywords(query_ir: &QueryIR) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut keywords = Vec::new();
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        push_preflight_label_keywords(&document_focus.hint, &mut seen, &mut keywords);
    }
    for entity in &query_ir.target_entities {
        push_preflight_label_keywords(&entity.label, &mut seen, &mut keywords);
    }
    keywords
}

fn push_preflight_label_keywords(value: &str, seen: &mut HashSet<String>, out: &mut Vec<String>) {
    for token in value
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '/')
        .map(str::trim)
        .filter(|token| token.chars().count() >= 4)
        .map(str::to_lowercase)
    {
        if seen.insert(token.clone()) {
            out.push(token);
        }
    }
}

fn filter_runtime_chunks_to_documents(
    chunks: &[RuntimeMatchedChunk],
    document_ids: &HashSet<Uuid>,
) -> Vec<RuntimeMatchedChunk> {
    chunks.iter().filter(|chunk| document_ids.contains(&chunk.document_id)).cloned().collect()
}

fn build_preflight_answer_chunks_for_scope(
    canonical_answer_chunks: &[RuntimeMatchedChunk],
    technical_literal_chunks: &[RuntimeMatchedChunk],
    scoped_document_ids: Option<&HashSet<Uuid>>,
    allow_empty_scope_fallback: bool,
) -> Vec<RuntimeMatchedChunk> {
    let merged = if technical_literal_chunks.is_empty() {
        canonical_answer_chunks.to_vec()
    } else if canonical_answer_chunks.is_empty() {
        technical_literal_chunks.to_vec()
    } else {
        merge_chunks(
            technical_literal_chunks.to_vec(),
            canonical_answer_chunks.to_vec(),
            canonical_answer_chunks.len().max(technical_literal_chunks.len()).max(12),
        )
    };

    match scoped_document_ids {
        Some(document_ids) => {
            let filtered = filter_runtime_chunks_to_documents(&merged, document_ids);
            if filtered.is_empty()
                && allow_empty_scope_fallback
                && !canonical_answer_chunks.is_empty()
            {
                canonical_answer_chunks.to_vec()
            } else {
                filtered
            }
        }
        None => merged,
    }
}

pub(super) fn extend_setup_preflight_chunks_from_structured_context(
    preflight_answer_chunks: &mut Vec<RuntimeMatchedChunk>,
    structured_context_chunks: &[RuntimeMatchedChunk],
    scoped_document_ids: Option<&HashSet<Uuid>>,
) {
    if structured_context_chunks.is_empty() {
        return;
    }
    let mut seen_chunk_ids =
        preflight_answer_chunks.iter().map(|chunk| chunk.chunk_id).collect::<HashSet<_>>();
    let mut context_chunks = structured_context_chunks
        .iter()
        .filter(|chunk| {
            scoped_document_ids
                .map(|document_ids| document_ids.contains(&chunk.document_id))
                .unwrap_or(true)
        })
        .filter(|chunk| seen_chunk_ids.insert(chunk.chunk_id))
        .cloned()
        .collect::<Vec<_>>();
    context_chunks.sort_by(|left, right| {
        left.document_label
            .cmp(&right.document_label)
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    preflight_answer_chunks.extend(context_chunks);
}

async fn augment_setup_preflight_structured_blocks(
    state: &AppState,
    question: &str,
    query_ir: &QueryIR,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    preflight_answer_chunks: &[RuntimeMatchedChunk],
    scoped_document_ids: Option<&HashSet<Uuid>>,
    preflight_evidence: &mut CanonicalAnswerEvidence,
) -> anyhow::Result<()> {
    if !query_ir_requests_setup_literal_context(query_ir)
        && !query_ir_requests_low_confidence_setup_preflight(
            question,
            query_ir,
            preflight_answer_chunks,
        )
    {
        return Ok(());
    }
    let Some(document_id) = setup_preflight_focused_document_id(
        question,
        query_ir,
        preflight_answer_chunks,
        scoped_document_ids,
    ) else {
        return Ok(());
    };
    let Some(revision_id) =
        setup_preflight_revision_id(document_id, preflight_answer_chunks, document_index)
    else {
        return Ok(());
    };

    let revision_blocks = state
        .document_store
        .list_structured_blocks_by_revision(revision_id)
        .await
        .context("failed to load focused setup structured blocks for canonical preflight")?;
    let loaded_block_count = revision_blocks.len();
    let added_block_count = merge_setup_preflight_structured_blocks(
        preflight_evidence,
        document_id,
        revision_blocks,
        SETUP_PREFLIGHT_STRUCTURED_BLOCK_LIMIT,
    );
    if added_block_count > 0 {
        tracing::info!(
            stage = "answer.preflight.setup_structured_blocks",
            %document_id,
            %revision_id,
            loaded_block_count,
            added_block_count,
            structured_block_count = preflight_evidence.structured_blocks.len(),
            "focused setup structured blocks added to canonical preflight evidence"
        );
    }
    Ok(())
}

fn setup_preflight_focused_document_id(
    question: &str,
    query_ir: &QueryIR,
    preflight_answer_chunks: &[RuntimeMatchedChunk],
    scoped_document_ids: Option<&HashSet<Uuid>>,
) -> Option<Uuid> {
    if let Some(document_ids) = scoped_document_ids
        && document_ids.len() == 1
    {
        return document_ids.iter().next().copied();
    }
    select_setup_literal_document_id(question, query_ir, preflight_answer_chunks)
}

fn setup_preflight_revision_id(
    document_id: Uuid,
    preflight_answer_chunks: &[RuntimeMatchedChunk],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> Option<Uuid> {
    preflight_answer_chunks
        .iter()
        .find(|chunk| chunk.document_id == document_id)
        .map(|chunk| chunk.revision_id)
        .or_else(|| document_index.get(&document_id).and_then(canonical_document_revision_id))
}

pub(super) fn merge_setup_preflight_structured_blocks(
    preflight_evidence: &mut CanonicalAnswerEvidence,
    document_id: Uuid,
    revision_blocks: Vec<KnowledgeStructuredBlockRow>,
    limit: usize,
) -> usize {
    if limit == 0 {
        return 0;
    }
    let mut selected = revision_blocks
        .into_iter()
        .filter(|block| block.document_id == document_id)
        .filter_map(|block| {
            let score = setup_preflight_structured_block_score(&block);
            (score > 0).then_some((score, block.ordinal, block.block_id, block))
        })
        .collect::<Vec<_>>();
    selected.sort_by(|left, right| {
        right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)).then_with(|| left.2.cmp(&right.2))
    });
    selected.truncate(limit);
    selected.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.2.cmp(&right.2)));

    let mut seen_block_ids = preflight_evidence
        .structured_blocks
        .iter()
        .map(|block| block.block_id)
        .collect::<HashSet<_>>();
    let before = preflight_evidence.structured_blocks.len();
    preflight_evidence.structured_blocks.extend(
        selected
            .into_iter()
            .map(|(_, _, _, block)| block)
            .filter(|block| seen_block_ids.insert(block.block_id)),
    );
    preflight_evidence.structured_blocks.len().saturating_sub(before)
}

fn setup_preflight_structured_block_score(block: &KnowledgeStructuredBlockRow) -> usize {
    let text = if block.normalized_text == block.text {
        block.text.clone()
    } else {
        format!("{}\n{}", block.text, block.normalized_text)
    };
    let package_count = extract_package_command_literals(&text, 4).len();
    let path_count = setup_literal_configuration_path_count(&text);
    let assignment_count = setup_literal_assignment_count(&text);
    let section_count = setup_literal_section_count(&text);
    let parameter_count = extract_parameter_literals(&text, 32).len();
    let block_kind = block.block_kind.as_str();
    let kind_score: usize = if block_kind.contains("table_row") {
        32
    } else if block_kind.contains("table") {
        18
    } else if block_kind.contains("code") {
        24
    } else {
        0
    };
    let has_structured_parameter = parameter_count > 0 && kind_score > 0;
    let has_setup_signal =
        package_count > 0 || path_count > 0 || assignment_count > 0 || section_count > 0;
    if !has_setup_signal && !has_structured_parameter {
        return 0;
    }
    kind_score
        .saturating_add(package_count.saturating_mul(16))
        .saturating_add(path_count.saturating_mul(24))
        .saturating_add(assignment_count.saturating_mul(10))
        .saturating_add(section_count.saturating_mul(8))
        .saturating_add(parameter_count.saturating_mul(3))
}

fn build_preflight_canonical_evidence_for_scope(
    canonical_evidence: &CanonicalAnswerEvidence,
    scoped_document_ids: Option<&HashSet<Uuid>>,
    allow_empty_scope_fallback: bool,
) -> CanonicalAnswerEvidence {
    match scoped_document_ids {
        Some(document_ids) => {
            let filtered = filter_canonical_evidence_to_documents(canonical_evidence, document_ids);
            if allow_empty_scope_fallback
                && canonical_evidence_has_rows(canonical_evidence)
                && !canonical_evidence_has_rows(&filtered)
            {
                canonical_evidence.clone()
            } else {
                filtered
            }
        }
        None => canonical_evidence.clone(),
    }
}

fn canonical_evidence_has_rows(canonical_evidence: &CanonicalAnswerEvidence) -> bool {
    !canonical_evidence.chunk_rows.is_empty()
        || !canonical_evidence.structured_blocks.is_empty()
        || !canonical_evidence.technical_facts.is_empty()
}

fn filter_canonical_evidence_to_documents(
    canonical_evidence: &CanonicalAnswerEvidence,
    document_ids: &HashSet<Uuid>,
) -> CanonicalAnswerEvidence {
    CanonicalAnswerEvidence {
        bundle: canonical_evidence.bundle.clone(),
        chunk_rows: canonical_evidence
            .chunk_rows
            .iter()
            .filter(|row| document_ids.contains(&row.document_id))
            .cloned()
            .collect(),
        structured_blocks: canonical_evidence
            .structured_blocks
            .iter()
            .filter(|block| document_ids.contains(&block.document_id))
            .cloned()
            .collect(),
        technical_facts: canonical_evidence
            .technical_facts
            .iter()
            .filter(|fact| document_ids.contains(&fact.document_id))
            .cloned()
            .collect(),
    }
}
