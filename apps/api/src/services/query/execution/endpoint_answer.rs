use std::collections::{BTreeSet, HashMap, HashSet};

use uuid::Uuid;

use crate::services::query::text_match::{near_token_overlap_count, normalized_alnum_tokens};
use crate::{
    domains::query_ir::{EntityRole, QueryIR},
    infra::knowledge_rows::{KnowledgeStructuredBlockRow, KnowledgeTechnicalFactRow},
    shared::extraction::technical_facts::{TechnicalFactKind, TechnicalFactQualifier},
    shared::extraction::text_render::repair_technical_layout_noise,
};

use super::{
    AuthoritativeAnswerEvidence, concise_document_subject_label,
    document_target::{focused_answer_document_id, question_requests_multi_document_scope},
    fact_lookup::{best_matching_fact, build_document_labels},
    question_intent::{
        query_ir_allows_deterministic_endpoint_lookup,
        query_ir_disallows_graph_id_like_endpoint_candidate,
    },
    source_profile::SOURCE_PROFILE_CHUNK_KIND,
    technical_answer::document_focus_preference,
    technical_literals::{
        technical_chunk_selection_score, technical_keyword_weight,
        technical_literal_focus_keyword_segments, technical_literal_focus_keywords,
    },
    types::RuntimeMatchedChunk,
};

pub(crate) fn build_multi_document_endpoint_answer_from_facts(
    question: &str,
    query_ir: &QueryIR,
    evidence: &AuthoritativeAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if !query_ir_allows_deterministic_endpoint_lookup(query_ir) {
        return None;
    }
    if !question_requests_multi_document_scope(question, Some(query_ir)) {
        return None;
    }

    let question_keywords = technical_literal_focus_keywords(question, Some(query_ir));
    if question_keywords.is_empty() {
        return None;
    }
    let focus_segments = endpoint_support_focus_segments(query_ir, &question_keywords);
    let question_focus = question_keywords.iter().cloned().collect::<BTreeSet<_>>();
    let subject_focus = endpoint_subject_focus_terms(query_ir);

    let per_document_chunks = chunks_by_document(chunks);
    let mut ordered_document_ids = Vec::<Uuid>::new();
    for chunk in chunks {
        if !ordered_document_ids.contains(&chunk.document_id) {
            ordered_document_ids.push(chunk.document_id);
        }
    }
    let document_labels = build_document_labels(chunks);
    let scoped_document_ids = select_multi_document_scope_ids(
        question,
        query_ir,
        &ordered_document_ids,
        &per_document_chunks,
    );

    let mut lines = Vec::new();
    for document_id in scoped_document_ids {
        let document_label =
            document_labels.get(&document_id).map(String::as_str).unwrap_or_default();
        let endpoint_match = best_matching_fact(
            evidence,
            &document_labels,
            TechnicalFactKind::EndpointPath,
            |fact| {
                fact.document_id == document_id
                    && !query_ir_disallows_graph_id_like_endpoint_candidate(
                        query_ir,
                        fact.display_value.as_str(),
                    )
                    && endpoint_fact_has_query_focused_support(
                        fact,
                        evidence,
                        chunks,
                        &focus_segments,
                    )
            },
            |fact, document_label| {
                endpoint_fact_score(EndpointFactScoreRequest {
                    fact,
                    evidence,
                    document_label: document_label.unwrap_or_default(),
                    candidate_document_id: fact.document_id,
                    focused_document_id: Some(document_id),
                    question_keywords: &question_keywords,
                    question_focus: &question_focus,
                    subject_focus: &subject_focus,
                })
            },
        );
        let Some(endpoint_match) = endpoint_match else {
            continue;
        };
        let subject = concise_document_subject_label(document_label);
        let method = endpoint_method_literal(endpoint_match.fact, evidence);
        let endpoint = endpoint_match.fact.display_value.as_str();
        let literal = method
            .map(|method| format!("`{method} {endpoint}`"))
            .unwrap_or_else(|| format!("`{endpoint}`"));
        lines.push(format!("- {subject}: {literal}"));
    }

    (lines.len() >= 2).then(|| format!("Required endpoints:\n\n{}", lines.join("\n")))
}

pub(crate) fn build_single_endpoint_answer_from_facts(
    question: &str,
    query_ir: &QueryIR,
    evidence: &AuthoritativeAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if !query_ir_allows_deterministic_endpoint_lookup(query_ir) {
        return None;
    }
    if question_requests_multi_document_scope(question, Some(query_ir)) {
        return None;
    }

    let focused_document_id = focused_answer_document_id(question, chunks);
    let question_keywords = technical_literal_focus_keywords(question, Some(query_ir));
    if question_keywords.is_empty() {
        return None;
    }
    let focus_segments = endpoint_support_focus_segments(query_ir, &question_keywords);
    let question_focus = question_keywords.iter().cloned().collect::<BTreeSet<_>>();
    let subject_focus = endpoint_subject_focus_terms(query_ir);

    let document_labels = build_document_labels(chunks);
    let endpoint_match = best_matching_fact(
        evidence,
        &document_labels,
        TechnicalFactKind::EndpointPath,
        |fact| {
            !query_ir_disallows_graph_id_like_endpoint_candidate(
                query_ir,
                fact.display_value.as_str(),
            ) && endpoint_fact_has_query_focused_support(fact, evidence, chunks, &focus_segments)
        },
        |fact, document_label| {
            endpoint_fact_score(EndpointFactScoreRequest {
                fact,
                evidence,
                document_label: document_label.unwrap_or_default(),
                candidate_document_id: fact.document_id,
                focused_document_id,
                question_keywords: &question_keywords,
                question_focus: &question_focus,
                subject_focus: &subject_focus,
            })
        },
    )?;

    let method = endpoint_method_literal(endpoint_match.fact, evidence);
    let endpoint = endpoint_match.fact.display_value.as_str();
    let literal = method
        .map(|method| format!("`{method} {endpoint}`"))
        .unwrap_or_else(|| format!("`{endpoint}`"));

    Some(format!("The endpoint is {literal}."))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct EndpointFactScore {
    document_focus: isize,
    question_coverage: usize,
    subject_coverage: usize,
    surface_coverage: usize,
}

/// Inputs of one endpoint-fact relevance score: the candidate fact, the
/// query-focused terms it is matched against and the document scope it belongs to.
pub(super) struct EndpointFactScoreRequest<'a> {
    pub(super) fact: &'a KnowledgeTechnicalFactRow,
    pub(super) evidence: &'a AuthoritativeAnswerEvidence,
    pub(super) document_label: &'a str,
    pub(super) candidate_document_id: Uuid,
    pub(super) focused_document_id: Option<Uuid>,
    pub(super) question_keywords: &'a [String],
    pub(super) question_focus: &'a BTreeSet<String>,
    pub(super) subject_focus: &'a BTreeSet<String>,
}

fn endpoint_fact_score(request: EndpointFactScoreRequest<'_>) -> EndpointFactScore {
    let EndpointFactScoreRequest {
        fact,
        evidence,
        document_label,
        candidate_document_id,
        focused_document_id,
        question_keywords,
        question_focus,
        subject_focus,
    } = request;
    let lowered_endpoint = fact.display_value.to_lowercase();
    let lowered_label = document_label.to_lowercase();
    let surface_coverage = question_keywords
        .iter()
        .map(|keyword| {
            usize::from(lowered_label.contains(keyword)) * 20
                + usize::from(lowered_endpoint.contains(keyword)) * 8
        })
        .sum::<usize>();
    EndpointFactScore {
        document_focus: document_focus_preference(candidate_document_id, focused_document_id),
        question_coverage: endpoint_fact_support_coverage(fact, evidence, question_focus),
        subject_coverage: endpoint_fact_support_coverage(fact, evidence, subject_focus),
        surface_coverage,
    }
}

fn endpoint_fact_has_query_focused_support(
    fact: &KnowledgeTechnicalFactRow,
    evidence: &AuthoritativeAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
    focus_segments: &[BTreeSet<String>],
) -> bool {
    let endpoint_literal = repair_technical_layout_noise(&fact.display_value).to_lowercase();
    if endpoint_literal.trim().is_empty() {
        return false;
    }

    let same_document_chunks = chunks
        .iter()
        .filter(|chunk| {
            chunk.document_id == fact.document_id
                && chunk.revision_id == fact.revision_id
                && chunk.chunk_kind.as_deref() != Some(SOURCE_PROFILE_CHUNK_KIND)
        })
        .collect::<Vec<_>>();
    if same_document_chunks.is_empty() {
        return fact.support_block_ids.is_empty() && fact.support_chunk_ids.is_empty();
    }

    let endpoint_chunks = same_document_chunks
        .into_iter()
        .filter(|chunk| endpoint_support_text(chunk).to_lowercase().contains(&endpoint_literal))
        .collect::<Vec<_>>();
    if endpoint_chunks.is_empty() {
        return false;
    }

    if focus_segments.is_empty() {
        return true;
    }

    let chunk_has_focus = endpoint_chunks.iter().any(|chunk| {
        let chunk_tokens = normalized_alnum_tokens(&endpoint_support_text(chunk), 3);
        focus_segments.iter().any(|segment| {
            let required_overlap = segment.len().clamp(1, 2);
            near_token_overlap_count(segment, &chunk_tokens) >= required_overlap
        })
    });
    if !chunk_has_focus {
        return false;
    }

    let support_blocks = endpoint_fact_support_blocks(fact, evidence);
    if support_blocks.is_empty() {
        return fact.support_block_ids.is_empty();
    }
    let own_coverage = support_blocks
        .iter()
        .map(|block| endpoint_block_focus_coverage(block, focus_segments))
        .max()
        .unwrap_or_default();
    if own_coverage == 0 {
        return false;
    }

    endpoint_fact_related_sibling_blocks(fact, evidence)
        .into_iter()
        .map(|block| endpoint_block_focus_coverage(block, focus_segments))
        .max()
        .is_none_or(|sibling_coverage| own_coverage >= sibling_coverage)
}

fn endpoint_fact_support_coverage(
    fact: &KnowledgeTechnicalFactRow,
    evidence: &AuthoritativeAnswerEvidence,
    focus_terms: &BTreeSet<String>,
) -> usize {
    if focus_terms.is_empty() {
        return 0;
    }
    endpoint_fact_support_blocks(fact, evidence)
        .into_iter()
        .map(|block| endpoint_block_term_coverage(block, focus_terms))
        .max()
        .unwrap_or_default()
}

fn endpoint_fact_support_blocks<'a>(
    fact: &KnowledgeTechnicalFactRow,
    evidence: &'a AuthoritativeAnswerEvidence,
) -> Vec<&'a KnowledgeStructuredBlockRow> {
    evidence
        .structured_blocks
        .iter()
        .filter(|block| {
            block.document_id == fact.document_id
                && block.revision_id == fact.revision_id
                && fact.support_block_ids.contains(&block.block_id)
        })
        .collect()
}

fn endpoint_fact_related_sibling_blocks<'a>(
    fact: &KnowledgeTechnicalFactRow,
    evidence: &'a AuthoritativeAnswerEvidence,
) -> Vec<&'a KnowledgeStructuredBlockRow> {
    let support_chunk_block_ids = evidence
        .chunk_rows
        .iter()
        .filter(|chunk| {
            chunk.document_id == fact.document_id
                && chunk.revision_id == fact.revision_id
                && (fact.support_chunk_ids.contains(&chunk.chunk_id)
                    || chunk
                        .support_block_ids
                        .iter()
                        .any(|block_id| fact.support_block_ids.contains(block_id)))
        })
        .flat_map(|chunk| chunk.support_block_ids.iter().copied())
        .collect::<HashSet<_>>();

    evidence
        .structured_blocks
        .iter()
        .filter(|block| {
            block.document_id == fact.document_id
                && block.revision_id == fact.revision_id
                && !fact.support_block_ids.contains(&block.block_id)
                && support_chunk_block_ids.contains(&block.block_id)
        })
        .collect()
}

fn endpoint_block_focus_coverage(
    block: &KnowledgeStructuredBlockRow,
    focus_segments: &[BTreeSet<String>],
) -> usize {
    let evidence_text = format!(
        "{} {} {}",
        block.heading_trail.join(" "),
        block.section_path.join(" "),
        block.text
    );
    let block_tokens = normalized_alnum_tokens(&repair_technical_layout_noise(&evidence_text), 3);
    focus_segments.iter().map(|segment| near_token_overlap_count(segment, &block_tokens)).sum()
}

fn endpoint_block_term_coverage(
    block: &KnowledgeStructuredBlockRow,
    focus_terms: &BTreeSet<String>,
) -> usize {
    let evidence_text = format!(
        "{} {} {}",
        block.heading_trail.join(" "),
        block.section_path.join(" "),
        block.text
    );
    let block_tokens = normalized_alnum_tokens(&repair_technical_layout_noise(&evidence_text), 3);
    near_token_overlap_count(focus_terms, &block_tokens)
}

fn endpoint_support_text(chunk: &RuntimeMatchedChunk) -> String {
    repair_technical_layout_noise(format!("{} {}", chunk.excerpt, chunk.source_text).as_str())
}

fn endpoint_support_focus_segments(
    query_ir: &QueryIR,
    question_keywords: &[String],
) -> Vec<BTreeSet<String>> {
    let mut segments = query_ir
        .literal_constraints
        .iter()
        .map(|literal| literal.text.as_str())
        .chain(query_ir.target_entities.iter().map(|entity| entity.label.as_str()))
        .map(|value| normalized_alnum_tokens(value, 3))
        .filter(|tokens| !tokens.is_empty())
        .collect::<Vec<_>>();
    let question_segment = question_keywords.iter().cloned().collect::<BTreeSet<_>>();
    if !question_segment.is_empty() && !segments.contains(&question_segment) {
        segments.push(question_segment);
    }
    segments
}

fn endpoint_subject_focus_terms(query_ir: &QueryIR) -> BTreeSet<String> {
    let mut terms = query_ir
        .target_entities
        .iter()
        .filter(|entity| entity.role == EntityRole::Subject)
        .flat_map(|entity| normalized_alnum_tokens(&entity.label, 3))
        .collect::<BTreeSet<_>>();
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        terms.extend(normalized_alnum_tokens(&document_focus.hint, 3));
    }
    terms
}

fn endpoint_method_literal(
    endpoint_fact: &crate::infra::knowledge_rows::KnowledgeTechnicalFactRow,
    evidence: &AuthoritativeAnswerEvidence,
) -> Option<String> {
    let qualifiers = serde_json::from_value::<Vec<TechnicalFactQualifier>>(
        endpoint_fact.qualifiers_json.clone(),
    )
    .unwrap_or_default();
    if let Some(method) = qualifiers
        .iter()
        .find(|qualifier| qualifier.key == "method")
        .map(|qualifier| qualifier.value.clone())
    {
        return Some(method);
    }

    let methods = evidence
        .technical_facts
        .iter()
        .filter(|fact| {
            fact.document_id == endpoint_fact.document_id
                && fact.fact_kind.parse::<TechnicalFactKind>().ok()
                    == Some(TechnicalFactKind::HttpMethod)
        })
        .map(|fact| repair_technical_layout_noise(&fact.display_value).to_ascii_uppercase())
        .collect::<HashSet<_>>();

    (methods.len() == 1).then(|| methods.into_iter().next()).flatten()
}

fn chunks_by_document(chunks: &[RuntimeMatchedChunk]) -> HashMap<Uuid, Vec<&RuntimeMatchedChunk>> {
    let mut per_document_chunks = HashMap::<Uuid, Vec<&RuntimeMatchedChunk>>::new();
    for chunk in chunks {
        per_document_chunks.entry(chunk.document_id).or_default().push(chunk);
    }
    per_document_chunks
}

pub(super) fn select_multi_document_scope_ids(
    question: &str,
    query_ir: &QueryIR,
    ordered_document_ids: &[Uuid],
    per_document_chunks: &HashMap<Uuid, Vec<&RuntimeMatchedChunk>>,
) -> Vec<Uuid> {
    let pagination_requested = false;
    let focus_segments = technical_literal_focus_keyword_segments(question, Some(query_ir));
    if focus_segments.is_empty() {
        return ordered_document_ids.to_vec();
    }

    let mut selected = Vec::new();
    let mut seen = HashSet::new();
    for segment_keywords in focus_segments {
        let best_document = ordered_document_ids
            .iter()
            .filter_map(|document_id| {
                let document_chunks = per_document_chunks.get(document_id)?;
                let best_chunk_score = document_chunks
                    .iter()
                    .map(|chunk| {
                        technical_chunk_selection_score(
                            &format!("{} {}", chunk.excerpt, chunk.source_text),
                            &segment_keywords,
                            pagination_requested,
                        )
                    })
                    .max()
                    .unwrap_or_default();
                let document_text = document_chunks
                    .iter()
                    .map(|chunk| format!("{} {}", chunk.excerpt, chunk.source_text))
                    .collect::<Vec<_>>()
                    .join("\n")
                    .to_lowercase();
                let document_keyword_score = segment_keywords
                    .iter()
                    .map(|keyword| technical_keyword_weight(&document_text, keyword) as isize)
                    .sum::<isize>();
                let score = best_chunk_score.max(document_keyword_score);
                (score > 0).then_some((score, *document_id))
            })
            .max_by(|left, right| {
                left.0.cmp(&right.0).then_with(|| {
                    let left_index = ordered_document_ids
                        .iter()
                        .position(|document_id| document_id == &left.1)
                        .unwrap_or(usize::MAX);
                    let right_index = ordered_document_ids
                        .iter()
                        .position(|document_id| document_id == &right.1)
                        .unwrap_or(usize::MAX);
                    right_index.cmp(&left_index)
                })
            });
        if let Some((_, document_id)) = best_document
            && seen.insert(document_id)
        {
            selected.push(document_id);
        }
    }

    if selected.is_empty() { ordered_document_ids.to_vec() } else { selected }
}
