use std::collections::{HashMap, HashSet};

use anyhow::Context;
use futures::future::join_all;
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::query::{GroupedReferenceKind, RuntimeQueryMode},
    domains::query_ir::{QueryAct, QueryIR, SourceSliceDirection},
    infra::{
        knowledge_rows::{KnowledgeDocumentRow, KnowledgeRevisionRow},
        repositories::catalog_repository,
    },
    services::content::document_hint::resolve_document_hint,
    services::query::{
        latest_versions::query_requests_latest_versions,
        support::{
            ContextAssemblyRequest, GroupedReferenceCandidate, assemble_context_metadata,
            group_visible_references,
        },
        text_match::{
            RelatedTokenSelection, build_related_token_candidates, normalized_alnum_tokens,
            select_related_overlap_tokens_from_candidates, token_sequence_exact_or_contains,
        },
    },
    shared::{
        extraction::text_render::repair_technical_layout_noise,
        json_coercion::from_value_or_default, text_tokens::literal_wildcard_prefixes,
    },
};

use super::command_shape::procedure_line_has_command_start;
use super::retrieve::{
    command_dense_excerpt_for, excerpt_for, focused_excerpt_for, load_latest_library_generation,
    query_graph_status, score_value,
};
use super::source_excerpt::structured_literal_excerpt_for;
use super::technical_literals::{
    select_document_balanced_chunks, technical_literal_focus_keywords,
};
#[cfg(test)]
use super::types::QueryExecutionReference;
use super::types::{
    QueryExecutionEnrichment, QueryGraphIndex, RetrievalBundle, RuntimeChunkScoreKind,
    RuntimeMatchedChunk, RuntimeMatchedEntity, RuntimeMatchedRelationship,
    RuntimeQueryLibraryContext, RuntimeQueryLibrarySummary, RuntimeQueryRecentDocument,
    RuntimeQueryWarning, RuntimeRetrievedDocumentBrief, RuntimeStructuredQueryDiagnostics,
    RuntimeStructuredQueryLibrarySummary, RuntimeStructuredQueryReferenceCounts,
};

const BOUNDED_SOURCE_UNIT_CONTEXT_CHARS: usize = 4_000;
const BOUNDED_ORDINARY_CONTEXT_CHARS: usize = 1_200;
const ENTITY_MATCH_CONTEXT_LINE_LIMIT: usize = 8;
const ENTITY_SUMMARY_CONTEXT_CHARS: usize = 320;
const TARGET_ENTITY_CONTEXT_LINE_LIMIT: usize = 64;
const TARGET_ENTITY_INVENTORY_CONTEXT_LINE_LIMIT: usize = 192;
const TARGET_ENTITY_SUMMARY_CONTEXT_CHARS: usize = 180;
const RETRIEVED_DOCUMENT_BRIEF_PREVIEW_CHARS: usize = 520;
const RETRIEVED_DOCUMENT_BRIEF_SOURCE_CHUNKS: usize = 3;
const CONTENT_ANCHOR_TOKEN_MIN_CHARS: usize = 4;
const CONTENT_ANCHOR_PRIORITY_LIMIT: usize = 8;
const CONTENT_ANCHOR_MIN_TOKEN_OVERLAP: usize = 2;

#[cfg(test)]
pub(crate) fn assemble_bounded_context(
    entities: &[RuntimeMatchedEntity],
    relationships: &[RuntimeMatchedRelationship],
    chunks: &[RuntimeMatchedChunk],
    budget_chars: usize,
) -> String {
    assemble_bounded_context_from_chunks(
        entities,
        relationships,
        &chunks.iter().collect::<Vec<_>>(),
        budget_chars,
        &[],
        &[],
        &[],
        false,
        false,
    )
}

struct BoundedContextSections {
    sections: Vec<String>,
    used_chars: usize,
    budget_chars: usize,
    exceeded_budget: bool,
}

impl BoundedContextSections {
    fn new(budget_chars: usize) -> Self {
        Self { sections: Vec::new(), used_chars: 0, budget_chars, exceeded_budget: false }
    }

    fn push(&mut self, line: String) -> bool {
        let projected = self.used_chars + "Context".len() + line.len() + 4;
        if projected > self.budget_chars {
            self.exceeded_budget = true;
            self.push_initial_excerpt(&line);
            return false;
        }
        self.used_chars = projected;
        self.sections.push(line);
        true
    }

    fn push_initial_excerpt(&mut self, line: &str) {
        if !self.sections.is_empty() {
            return;
        }
        let available = self.budget_chars.saturating_sub("Context\n".len() + 4);
        if available > 0 {
            self.sections.push(excerpt_for(line, available));
        }
    }

    fn finish(self) -> String {
        if self.sections.is_empty() {
            String::new()
        } else if self.exceeded_budget {
            self.sections.join("\n")
        } else {
            format!("Context\n{}", self.sections.join("\n"))
        }
    }
}

fn assemble_bounded_context_from_chunks(
    entities: &[RuntimeMatchedEntity],
    relationships: &[RuntimeMatchedRelationship],
    chunks: &[&RuntimeMatchedChunk],
    budget_chars: usize,
    ordinary_keywords: &[String],
    entity_match_lines: &[String],
    graph_evidence_lines: &[String],
    prefer_graph_first: bool,
    prefer_entity_nodes_before_evidence: bool,
) -> String {
    let graph_lines = bounded_context_graph_lines(
        entities,
        relationships,
        entity_match_lines,
        graph_evidence_lines,
        prefer_entity_nodes_before_evidence,
    );
    let document_lines = chunks
        .iter()
        .map(|chunk| bounded_context_document_block(chunk, ordinary_keywords))
        .collect::<Vec<_>>();
    interleave_bounded_context_lines(
        &graph_lines,
        &document_lines,
        budget_chars,
        prefer_graph_first,
    )
}

fn bounded_context_graph_lines(
    entities: &[RuntimeMatchedEntity],
    relationships: &[RuntimeMatchedRelationship],
    entity_match_lines: &[String],
    graph_evidence_lines: &[String],
    prefer_entity_nodes_before_evidence: bool,
) -> Vec<String> {
    let entity_lines = entities.iter().map(graph_node_context_line).collect::<Vec<_>>();
    let mut graph_lines = entity_match_lines.to_vec();
    if prefer_entity_nodes_before_evidence {
        graph_lines.extend(entity_lines);
        graph_lines.extend(graph_evidence_lines.iter().cloned());
    } else {
        graph_lines.extend(graph_evidence_lines.iter().cloned());
        graph_lines.extend(entity_lines);
    }
    graph_lines.extend(relationships.iter().map(RuntimeMatchedRelationship::context_line));
    graph_lines
}

fn interleave_bounded_context_lines(
    graph_lines: &[String],
    document_lines: &[String],
    budget_chars: usize,
    prefer_graph_first: bool,
) -> String {
    let mut output = BoundedContextSections::new(budget_chars);
    let mut graph_index = 0usize;
    let mut document_index = 0usize;
    if prefer_graph_first && !push_bounded_line_slice(&mut output, graph_lines, &mut graph_index) {
        return output.finish();
    }

    let mut prefer_document = !document_lines.is_empty();
    while graph_index < graph_lines.len() || document_index < document_lines.len() {
        let consumed = push_bounded_context_pair(
            &mut output,
            graph_lines,
            document_lines,
            &mut graph_index,
            &mut document_index,
            prefer_document,
        );
        if !consumed {
            break;
        }
        prefer_document = !prefer_document;
    }
    output.finish()
}

fn push_bounded_line_slice(
    output: &mut BoundedContextSections,
    lines: &[String],
    index: &mut usize,
) -> bool {
    while let Some(line) = lines.get(*index) {
        *index += 1;
        if !output.push(line.clone()) {
            return false;
        }
    }
    true
}

fn push_bounded_context_pair(
    output: &mut BoundedContextSections,
    graph_lines: &[String],
    document_lines: &[String],
    graph_index: &mut usize,
    document_index: &mut usize,
    prefer_document: bool,
) -> bool {
    let mut consumed = false;
    for take_document in [prefer_document, !prefer_document] {
        let next_line = if take_document {
            document_lines.get(*document_index).cloned().inspect(|_| *document_index += 1)
        } else {
            graph_lines.get(*graph_index).cloned().inspect(|_| *graph_index += 1)
        };
        let Some(line) = next_line else {
            continue;
        };
        consumed = true;
        if !output.push(line) {
            return false;
        }
    }
    consumed
}

pub(crate) fn assemble_bounded_context_for_query(
    query_ir: &QueryIR,
    question: &str,
    entities: &[RuntimeMatchedEntity],
    relationships: &[RuntimeMatchedRelationship],
    chunks: &[RuntimeMatchedChunk],
    graph_evidence_lines: &[String],
    budget_chars: usize,
) -> String {
    if let Some(context) = assemble_ordered_source_slice_context(query_ir, chunks, budget_chars) {
        return context;
    }
    let keywords = technical_literal_focus_keywords(question, Some(query_ir));
    let ordered_chunks = order_bounded_context_chunks(question, query_ir, chunks, &keywords);
    let entity_match_lines = entity_match_context_lines(query_ir, entities);
    let prefer_graph_first = should_prioritize_graph_context_for_query(
        query_ir,
        !entities.is_empty() || !relationships.is_empty(),
        !graph_evidence_lines.is_empty(),
    );
    let prefer_entity_nodes_before_evidence =
        should_prioritize_entity_nodes_before_evidence(query_ir, !entities.is_empty());
    assemble_bounded_context_from_chunks(
        entities,
        relationships,
        &ordered_chunks,
        budget_chars,
        &keywords,
        &entity_match_lines,
        graph_evidence_lines,
        prefer_graph_first,
        prefer_entity_nodes_before_evidence,
    )
}

pub(crate) fn target_entity_context_lines(
    query_ir: &QueryIR,
    graph_index: &QueryGraphIndex,
) -> Vec<String> {
    if query_ir.target_entities.is_empty() {
        return Vec::new();
    }

    let line_limit = target_entity_context_line_limit(query_ir);
    let mut seen_nodes = HashSet::<Uuid>::new();
    let mut lines = Vec::new();
    for mention in &query_ir.target_entities {
        if lines.len() >= line_limit {
            break;
        }
        let label = mention.label.trim();
        if label.is_empty() {
            continue;
        }
        let normalized_label = normalized_prefix_match_text(label);
        let wildcard_prefixes = literal_wildcard_prefixes(label, 2);
        if normalized_label.is_empty() && wildcard_prefixes.is_empty() {
            continue;
        }

        let mut matches = graph_index
            .nodes()
            .filter(|node| node.node_type != "document")
            .filter(|node| {
                graph_node_matches_target_label(node, &normalized_label, &wildcard_prefixes)
            })
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| {
            left.label
                .cmp(&right.label)
                .then_with(|| right.support_count.cmp(&left.support_count))
                .then_with(|| left.id.cmp(&right.id))
        });
        for node in matches {
            if lines.len() >= line_limit || !seen_nodes.insert(node.id) {
                continue;
            }
            lines.push(graph_node_target_context_line(node));
        }
    }
    lines
}

fn target_entity_context_line_limit(query_ir: &QueryIR) -> usize {
    if matches!(query_ir.act, QueryAct::Enumerate | QueryAct::Meta)
        && query_ir
            .target_entities
            .iter()
            .any(|mention| !literal_wildcard_prefixes(mention.label.trim(), 2).is_empty())
    {
        TARGET_ENTITY_INVENTORY_CONTEXT_LINE_LIMIT
    } else {
        TARGET_ENTITY_CONTEXT_LINE_LIMIT
    }
}

fn graph_node_matches_target_label(
    node: &crate::infra::repositories::RuntimeGraphQueryNodeRow,
    normalized_label: &str,
    wildcard_prefixes: &[String],
) -> bool {
    let label = normalized_prefix_match_text(&node.label);
    if !normalized_label.is_empty() && label == normalized_label {
        return true;
    }
    let aliases =
        from_value_or_default::<Vec<String>>("runtime_graph_node.aliases_json", &node.aliases_json);
    if !normalized_label.is_empty()
        && aliases.iter().any(|alias| normalized_prefix_match_text(alias) == normalized_label)
    {
        return true;
    }
    !wildcard_prefixes.is_empty()
        && wildcard_prefixes.iter().any(|prefix| {
            label.starts_with(prefix)
                || aliases
                    .iter()
                    .any(|alias| normalized_prefix_match_text(alias).starts_with(prefix))
        })
}

fn graph_node_target_context_line(
    node: &crate::infra::repositories::RuntimeGraphQueryNodeRow,
) -> String {
    let tail = format!("{} ({})", node.label, node.node_type);
    if let Some(summary) = node.summary.as_deref().map(str::trim).filter(|value| !value.is_empty())
    {
        return format!(
            "[graph-node] target_match=explicit evidence: {} | entity_hint: {}",
            excerpt_for(summary, TARGET_ENTITY_SUMMARY_CONTEXT_CHARS),
            tail
        );
    }
    format!("[graph-node] target_match=explicit entity_hint: {tail}")
}

struct EntityMatchTarget {
    label: String,
    wildcard_prefixes: Vec<String>,
    related_tokens: RelatedTokenSelection,
}

fn entity_match_context_lines(
    query_ir: &QueryIR,
    entities: &[RuntimeMatchedEntity],
) -> Vec<String> {
    if query_ir.target_entities.is_empty() || entities.is_empty() {
        return Vec::new();
    }
    let target_sets = entity_match_targets(query_ir, entities);
    if target_sets.is_empty() {
        return Vec::new();
    }

    let mut seen_nodes = HashSet::<Uuid>::new();
    entities
        .iter()
        .filter(|entity| seen_nodes.insert(entity.node_id))
        .filter_map(|entity| {
            let kind = entity_match_kind(entity, &target_sets)?;
            Some(format!("[entity-match {kind}] {}", graph_node_context_tail(entity)))
        })
        .take(ENTITY_MATCH_CONTEXT_LINE_LIMIT)
        .collect()
}

fn entity_match_targets(
    query_ir: &QueryIR,
    entities: &[RuntimeMatchedEntity],
) -> Vec<EntityMatchTarget> {
    let target_labels = query_ir
        .target_entities
        .iter()
        .filter_map(|mention| {
            let label = mention.label.trim();
            let is_matchable = !label.is_empty()
                && (!normalized_alnum_tokens(label, 3).is_empty()
                    || !literal_wildcard_prefixes(label, 2).is_empty());
            is_matchable.then(|| label.to_string())
        })
        .collect::<Vec<_>>();
    let related_candidates =
        build_related_token_candidates(entities.iter().map(|entity| entity.label.as_str()), 3);
    target_labels
        .into_iter()
        .map(|label| EntityMatchTarget {
            wildcard_prefixes: literal_wildcard_prefixes(&label, 2),
            related_tokens: select_related_overlap_tokens_from_candidates(
                &label,
                &related_candidates,
                3,
            ),
            label,
        })
        .collect()
}

fn entity_match_kind(
    entity: &RuntimeMatchedEntity,
    targets: &[EntityMatchTarget],
) -> Option<&'static str> {
    let label = entity.label.trim();
    if label.is_empty() {
        return None;
    }
    let label_tokens = normalized_alnum_tokens(label, 3);
    let label_prefix_text = normalized_prefix_match_text(label);
    let mut best_kind = None;
    for target in targets {
        if !target.wildcard_prefixes.is_empty() {
            if target.wildcard_prefixes.iter().any(|prefix| label_prefix_text.starts_with(prefix)) {
                return Some("prefix");
            }
            continue;
        }
        if token_sequence_exact_or_contains(label, &target.label, 3) {
            return Some("exact");
        }
        if !target.related_tokens.is_empty() && target.related_tokens.matches_tokens(&label_tokens)
        {
            best_kind = Some("token-overlap");
        }
    }
    best_kind
}

fn graph_node_context_line(entity: &RuntimeMatchedEntity) -> String {
    if let Some(summary) =
        entity.summary.as_deref().map(str::trim).filter(|value| !value.is_empty())
    {
        return format!(
            "[graph-node] evidence: {} | entity_hint: {}",
            excerpt_for(summary, ENTITY_SUMMARY_CONTEXT_CHARS),
            graph_node_context_tail(entity)
        );
    }
    format!("[graph-node] {}", graph_node_context_tail(entity))
}

fn graph_node_context_tail(entity: &RuntimeMatchedEntity) -> String {
    format!("{} ({})", entity.label, entity.node_type)
}

fn normalized_prefix_match_text(value: &str) -> String {
    value.nfkc().flat_map(char::to_lowercase).collect::<String>().trim().to_string()
}

pub(crate) fn should_prioritize_retrieved_context_for_query(
    query_ir: &QueryIR,
    retrieved_context: &str,
) -> bool {
    should_prioritize_graph_context_for_query(
        query_ir,
        retrieved_context.contains("[graph-node]") || retrieved_context.contains("[graph-edge"),
        retrieved_context.contains("[graph-evidence"),
    )
}

fn should_prioritize_graph_context_for_query(
    query_ir: &QueryIR,
    has_graph_topology_support: bool,
    has_graph_evidence_support: bool,
) -> bool {
    if !(has_graph_topology_support || has_graph_evidence_support) {
        return false;
    }
    if matches!(query_ir.act, QueryAct::Enumerate | QueryAct::Meta)
        && (query_ir.scope == crate::domains::query_ir::QueryScope::LibraryMeta
            || !query_ir.target_entities.is_empty())
    {
        return true;
    }
    !query_ir.target_entities.is_empty()
        && matches!(query_ir.act, QueryAct::RetrieveValue | QueryAct::Describe | QueryAct::Compare)
}

fn should_prioritize_entity_nodes_before_evidence(query_ir: &QueryIR, has_entities: bool) -> bool {
    has_entities && matches!(query_ir.act, QueryAct::Enumerate | QueryAct::Meta)
}

fn order_bounded_context_chunks<'a>(
    question: &str,
    query_ir: &QueryIR,
    chunks: &'a [RuntimeMatchedChunk],
    keywords: &[String],
) -> Vec<&'a RuntimeMatchedChunk> {
    if chunks.is_empty() {
        return Vec::new();
    }
    let selected = select_document_balanced_chunks(
        question,
        Some(query_ir),
        chunks,
        keywords,
        false,
        chunks.len(),
        super::MAX_CHUNKS_PER_DOCUMENT,
    );
    let mut ordered = Vec::<&RuntimeMatchedChunk>::with_capacity(chunks.len());
    let mut seen = HashSet::<uuid::Uuid>::with_capacity(chunks.len());

    extend_unseen_chunks(
        &mut ordered,
        &mut seen,
        chunks.iter().filter(|chunk| super::source_profile::is_source_profile_runtime_chunk(chunk)),
    );
    if super::retrieve::query_ir_requests_versioned_update_procedure_context(question, query_ir) {
        extend_unseen_chunks(
            &mut ordered,
            &mut seen,
            procedure_context_priority_chunks(question, query_ir, chunks),
        );
    }
    extend_unseen_chunks(
        &mut ordered,
        &mut seen,
        content_anchor_priority_chunks(question, query_ir, chunks),
    );
    extend_unseen_chunks(&mut ordered, &mut seen, sorted_identity_chunks(chunks));
    extend_unseen_chunks(&mut ordered, &mut seen, selected);
    extend_unseen_chunks(&mut ordered, &mut seen, chunks.iter());
    ordered
}

fn extend_unseen_chunks<'a>(
    ordered: &mut Vec<&'a RuntimeMatchedChunk>,
    seen: &mut HashSet<Uuid>,
    chunks: impl IntoIterator<Item = &'a RuntimeMatchedChunk>,
) {
    for chunk in chunks {
        if seen.insert(chunk.chunk_id) {
            ordered.push(chunk);
        }
    }
}

fn sorted_identity_chunks(chunks: &[RuntimeMatchedChunk]) -> Vec<&RuntimeMatchedChunk> {
    let mut identity_chunks = chunks
        .iter()
        .filter(|chunk| {
            matches!(
                chunk.score_kind,
                RuntimeChunkScoreKind::DocumentIdentity | RuntimeChunkScoreKind::LatestVersion
            )
        })
        .collect::<Vec<_>>();
    identity_chunks.sort_by(|left, right| {
        score_value(right.score)
            .total_cmp(&score_value(left.score))
            .then_with(|| left.document_id.cmp(&right.document_id))
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    identity_chunks
}

fn content_anchor_priority_chunks<'a>(
    question: &str,
    query_ir: &QueryIR,
    chunks: &'a [RuntimeMatchedChunk],
) -> Vec<&'a RuntimeMatchedChunk> {
    let model = ContentAnchorModel::new(question, query_ir);
    if model.is_empty() {
        return Vec::new();
    }
    let mut scored = chunks
        .iter()
        .filter(|chunk| content_anchor_candidate(chunk))
        .filter_map(|chunk| {
            let score = content_anchor_priority_score(chunk, &model);
            (score > 0).then_some((score, chunk))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| score_value(right.score).total_cmp(&score_value(left.score)))
            .then_with(|| left.document_id.cmp(&right.document_id))
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    scored.into_iter().take(CONTENT_ANCHOR_PRIORITY_LIMIT).map(|(_, chunk)| chunk).collect()
}

fn content_anchor_candidate(chunk: &RuntimeMatchedChunk) -> bool {
    !super::source_profile::is_source_profile_runtime_chunk(chunk)
        && !matches!(
            chunk.score_kind,
            RuntimeChunkScoreKind::DocumentIdentity | RuntimeChunkScoreKind::LatestVersion
        )
}

struct ContentAnchorModel {
    focus_tokens: HashSet<String>,
    quoted_phrases: Vec<String>,
}

impl ContentAnchorModel {
    fn new(question: &str, query_ir: &QueryIR) -> Self {
        let mut focus_tokens = HashSet::<String>::new();
        let mut quoted_phrases = Vec::<String>::new();
        let mut seen_phrases = HashSet::<String>::new();

        let mut add_source = |value: &str| {
            let current = crate::services::query::effective_query::current_question_segment(value);
            for token in normalized_alnum_tokens(current, CONTENT_ANCHOR_TOKEN_MIN_CHARS) {
                focus_tokens.insert(token);
            }
            for phrase in quoted_content_anchor_phrases(current) {
                if seen_phrases.insert(phrase.clone()) {
                    quoted_phrases.push(phrase);
                }
            }
        };

        add_source(question);
        if let Some(retrieval_query) = query_ir.retrieval_query.as_deref() {
            add_source(retrieval_query);
        }
        if let Some(document_focus) = query_ir.document_focus.as_ref() {
            add_source(&document_focus.hint);
        }
        for entity in &query_ir.target_entities {
            add_source(&entity.label);
        }
        for literal in &query_ir.literal_constraints {
            add_source(&literal.text);
        }

        Self { focus_tokens, quoted_phrases }
    }

    fn is_empty(&self) -> bool {
        self.focus_tokens.is_empty() && self.quoted_phrases.is_empty()
    }
}

fn quoted_content_anchor_phrases(value: &str) -> Vec<String> {
    let mut phrases = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    for (open, close) in [('«', '»'), ('“', '”'), ('"', '"'), ('`', '`'), ('\'', '\'')] {
        for phrase in quoted_spans(value, open, close) {
            if normalized_alnum_tokens(&phrase, 3).len() < 2 {
                continue;
            }
            if seen.insert(phrase.clone()) {
                phrases.push(phrase);
            }
        }
    }
    phrases
}

fn quoted_spans(value: &str, open: char, close: char) -> Vec<String> {
    let mut spans = Vec::<String>::new();
    let mut start: Option<usize> = None;
    for (index, ch) in value.char_indices() {
        if let Some(open_index) = start {
            if ch == close {
                let phrase = value[open_index..index].trim();
                if !phrase.is_empty() {
                    spans.push(phrase.to_string());
                }
                start = None;
            }
            continue;
        }
        if ch == open {
            start = Some(index + ch.len_utf8());
        }
    }
    spans
}

fn content_anchor_priority_score(chunk: &RuntimeMatchedChunk, model: &ContentAnchorModel) -> usize {
    let text = repair_technical_layout_noise(&format!("{}\n{}", chunk.excerpt, chunk.source_text));
    let phrase_hits = model
        .quoted_phrases
        .iter()
        .filter(|phrase| token_sequence_exact_or_contains(&text, phrase, 3))
        .count();
    let text_tokens =
        normalized_alnum_tokens(&text, CONTENT_ANCHOR_TOKEN_MIN_CHARS).into_iter().collect();
    let token_overlap = soft_context_overlap_count(&model.focus_tokens, &text_tokens);
    if phrase_hits == 0 && token_overlap < CONTENT_ANCHOR_MIN_TOKEN_OVERLAP {
        return 0;
    }

    phrase_hits
        .saturating_mul(512)
        .saturating_add(token_overlap.saturating_mul(64))
        .saturating_add((chunk.score_kind == RuntimeChunkScoreKind::FocusedDocument) as usize * 24)
        .saturating_add((chunk.score_kind == RuntimeChunkScoreKind::SourceContext) as usize * 12)
}

fn procedure_context_priority_chunks<'a>(
    question: &str,
    query_ir: &QueryIR,
    chunks: &'a [RuntimeMatchedChunk],
) -> Vec<&'a RuntimeMatchedChunk> {
    let model = ProcedureContextModel::new(question, query_ir);
    let mut scored = chunks
        .iter()
        .filter_map(|chunk| {
            let score = procedure_context_priority_score(chunk, &model);
            (score > 0).then_some((score, chunk))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| score_value(right.score).total_cmp(&score_value(left.score)))
            .then_with(|| left.document_id.cmp(&right.document_id))
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    scored.into_iter().map(|(_, chunk)| chunk).collect()
}

struct ProcedureContextModel {
    subject_terms: HashSet<String>,
    action_terms: HashSet<String>,
}

impl ProcedureContextModel {
    fn new(question: &str, query_ir: &QueryIR) -> Self {
        let current_question =
            crate::services::query::effective_query::current_question_segment(question);
        let mut subject_terms = HashSet::<String>::new();
        for entity in &query_ir.target_entities {
            subject_terms.extend(normalized_alnum_tokens(&entity.label, 2));
        }
        if let Some(document_focus) = query_ir.document_focus.as_ref() {
            subject_terms.extend(normalized_alnum_tokens(&document_focus.hint, 2));
        }

        let mut action_terms = normalized_alnum_tokens(current_question, 5)
            .into_iter()
            .filter(|term| {
                !subject_terms.iter().any(|subject| soft_context_terms_match(term, subject))
            })
            .collect::<HashSet<_>>();
        if let Some(retrieval_query) = query_ir.retrieval_query.as_deref() {
            let current_retrieval_query =
                crate::services::query::effective_query::current_question_segment(retrieval_query);
            action_terms.extend(
                normalized_alnum_tokens(current_retrieval_query, 5).into_iter().filter(|term| {
                    !subject_terms.iter().any(|subject| soft_context_terms_match(term, subject))
                }),
            );
        }

        Self { subject_terms, action_terms }
    }
}

fn procedure_context_priority_score(
    chunk: &RuntimeMatchedChunk,
    model: &ProcedureContextModel,
) -> usize {
    let text = repair_technical_layout_noise(&format!(
        "{}\n{}\n{}",
        chunk.document_label, chunk.excerpt, chunk.source_text
    ));
    let command_score = procedure_context_command_signal_score(&text);
    if command_score == 0 {
        return 0;
    }

    let tokens = normalized_alnum_tokens(&text, 2).into_iter().collect::<HashSet<_>>();
    let action_overlap = soft_context_overlap_count(&model.action_terms, &tokens);
    let subject_overlap = soft_context_overlap_count(&model.subject_terms, &tokens);
    if action_overlap == 0 && subject_overlap == 0 {
        return 0;
    }

    action_overlap
        .saturating_mul(96)
        .saturating_add(subject_overlap.saturating_mul(24))
        .saturating_add(command_score.saturating_mul(8))
        .saturating_add((chunk.score_kind == RuntimeChunkScoreKind::FocusedDocument) as usize * 16)
}

fn soft_context_overlap_count(expected: &HashSet<String>, available: &HashSet<String>) -> usize {
    expected
        .iter()
        .filter(|term| available.iter().any(|candidate| soft_context_terms_match(term, candidate)))
        .count()
}

fn soft_context_terms_match(left: &str, right: &str) -> bool {
    left == right || soft_context_common_prefix_len(left, right) >= 5
}

fn soft_context_common_prefix_len(left: &str, right: &str) -> usize {
    let mut count = 0usize;
    for (left_ch, right_ch) in left.chars().zip(right.chars()) {
        if left_ch != right_ch {
            break;
        }
        count += 1;
    }
    count
}

fn procedure_context_command_signal_score(text: &str) -> usize {
    let mut score = 0usize;
    for line in text.lines() {
        if procedure_line_has_command_start(line) {
            score = score.saturating_add(1);
        }
    }
    score.min(8)
}

fn bounded_context_document_block(
    chunk: &RuntimeMatchedChunk,
    ordinary_keywords: &[String],
) -> String {
    if chunk.score_kind == RuntimeChunkScoreKind::GraphEvidence {
        return graph_evidence_document_block(chunk, ordinary_keywords);
    }
    if is_structured_source_unit_context_chunk(chunk) {
        return structured_document_block(chunk, ordinary_keywords, "source_unit");
    }
    if chunk.score_kind == RuntimeChunkScoreKind::SourceContext
        && !super::source_profile::is_source_profile_runtime_chunk(chunk)
    {
        return structured_document_block(chunk, ordinary_keywords, "source_context");
    }
    if let Some(block_kind) = identity_document_block_kind(chunk) {
        return identity_document_block(chunk, block_kind);
    }
    let text = query_focused_chunk_context_text(chunk, ordinary_keywords);
    format!("[document] {}: {}", chunk.document_label, text.trim())
}

fn preferred_chunk_text(chunk: &RuntimeMatchedChunk) -> &str {
    let source_text = chunk.source_text.trim();
    if source_text.is_empty() { chunk.excerpt.trim() } else { source_text }
}

fn graph_evidence_document_block(
    chunk: &RuntimeMatchedChunk,
    ordinary_keywords: &[String],
) -> String {
    let text = preferred_chunk_text(chunk);
    let focused = focused_excerpt_for(text, ordinary_keywords, BOUNDED_SOURCE_UNIT_CONTEXT_CHARS);
    let excerpt = if focused.trim().is_empty() {
        excerpt_for(text, BOUNDED_SOURCE_UNIT_CONTEXT_CHARS)
    } else {
        focused
    };
    format!(
        "[document graph_evidence document=\"{}\" ordinal={}]\n{}",
        context_label(&chunk.document_label),
        chunk.chunk_index,
        excerpt
    )
}

fn structured_document_block(
    chunk: &RuntimeMatchedChunk,
    ordinary_keywords: &[String],
    block_kind: &str,
) -> String {
    let text = preferred_chunk_text(chunk);
    let excerpt =
        structured_literal_excerpt_for(text, ordinary_keywords, BOUNDED_SOURCE_UNIT_CONTEXT_CHARS)
            .unwrap_or_else(|| excerpt_for(text, BOUNDED_SOURCE_UNIT_CONTEXT_CHARS));
    format!(
        "[document {block_kind} ordinal={} document=\"{}\"]\n{}",
        chunk.chunk_index,
        context_label(&chunk.document_label),
        excerpt
    )
}

fn identity_document_block_kind(chunk: &RuntimeMatchedChunk) -> Option<&'static str> {
    match chunk.score_kind {
        RuntimeChunkScoreKind::LatestVersion => Some("latest_version"),
        RuntimeChunkScoreKind::DocumentIdentity => Some("document_identity"),
        _ => None,
    }
}

fn identity_document_block(chunk: &RuntimeMatchedChunk, block_kind: &str) -> String {
    format!(
        "[document {block_kind} ordinal={} document=\"{}\"]\n{}",
        chunk.chunk_index,
        context_label(&chunk.document_label),
        excerpt_for(preferred_chunk_text(chunk), BOUNDED_SOURCE_UNIT_CONTEXT_CHARS)
    )
}

fn query_focused_chunk_context_text(
    chunk: &RuntimeMatchedChunk,
    ordinary_keywords: &[String],
) -> String {
    if ordinary_keywords.is_empty() {
        return chunk.excerpt.trim().to_string();
    }
    let source_text = chunk.source_text.trim();
    if source_text.is_empty() {
        return chunk.excerpt.trim().to_string();
    }
    if let Some(excerpt) = command_dense_excerpt_for(source_text, BOUNDED_SOURCE_UNIT_CONTEXT_CHARS)
    {
        return excerpt;
    }
    focused_excerpt_for(source_text, ordinary_keywords, BOUNDED_ORDINARY_CONTEXT_CHARS)
}

fn is_structured_source_unit_context_chunk(chunk: &RuntimeMatchedChunk) -> bool {
    super::source_context::is_source_unit_runtime_chunk(chunk)
        || chunk.source_text.lines().map(str::trim_start).any(|line| line.starts_with("[unit_id="))
}

fn assemble_ordered_source_slice_context(
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
    budget_chars: usize,
) -> Option<String> {
    let slice = query_ir.source_slice.as_ref()?;
    let mut profile_blocks = chunks
        .iter()
        .filter(|chunk| super::source_profile::is_source_profile_runtime_chunk(chunk))
        .map(|chunk| {
            format!(
                "[SOURCE_PROFILE document=\"{}\"]\n{}",
                context_label(&chunk.document_label),
                source_profile_text_for_source_slice(chunk)
            )
        })
        .collect::<Vec<_>>();
    let mut content_chunks = chunks
        .iter()
        .filter(|chunk| !super::source_profile::is_source_profile_runtime_chunk(chunk))
        .collect::<Vec<_>>();
    if content_chunks.iter().any(|chunk| super::source_context::is_source_unit_runtime_chunk(chunk))
    {
        content_chunks.retain(|chunk| super::source_context::is_source_unit_runtime_chunk(chunk));
    }
    if content_chunks.is_empty() {
        return None;
    }
    let latest_version_context = query_requests_latest_versions(query_ir);
    if latest_version_context {
        let identity_chunks = content_chunks
            .iter()
            .copied()
            .filter(|chunk| {
                matches!(
                    chunk.score_kind,
                    RuntimeChunkScoreKind::DocumentIdentity | RuntimeChunkScoreKind::LatestVersion
                )
            })
            .collect::<Vec<_>>();
        if !identity_chunks.is_empty() {
            content_chunks = identity_chunks;
        }
        content_chunks.sort_by(latest_version_source_slice_chunk_order);
    } else {
        content_chunks.sort_by_key(|chunk| (chunk.document_label.clone(), chunk.chunk_index));
    }
    let mut content_blocks = content_chunks
        .iter()
        .map(|chunk| {
            format!(
                "[SOURCE_SLICE_UNIT direction={} requested_count={} document=\"{}\" ordinal={} coverage=ordered]\n{}",
                source_slice_direction_label(slice.direction),
                super::source_slice_requested_count(query_ir).unwrap_or_default(),
                context_label(&chunk.document_label),
                chunk.chunk_index,
                chunk_text_for_source_slice(chunk)
            )
        })
        .collect::<Vec<_>>();
    let header = format!(
        "Context\nSOURCE_SLICE blocks below are the canonical ordered source slice selected by the runtime for this question. Treat them as ordered source records, not sampled excerpts.\n- direction: {}\n- requested_count: {}\n- returned_unit_count: {}",
        source_slice_direction_label(slice.direction),
        super::source_slice_requested_count(query_ir).unwrap_or_default(),
        content_blocks.len()
    );
    let prefix_len =
        header.len() + profile_blocks.iter().map(|block| block.len() + 2).sum::<usize>() + 2;
    let remaining_budget = budget_chars.saturating_sub(prefix_len);
    content_blocks = if latest_version_context {
        select_blocks_for_budget_in_order(content_blocks, remaining_budget)
    } else {
        select_source_slice_blocks_for_budget(content_blocks, remaining_budget, slice.direction)
    };
    if content_blocks.is_empty() {
        return None;
    }
    let mut sections = Vec::new();
    sections.push(header);
    sections.append(&mut profile_blocks);
    sections.append(&mut content_blocks);
    Some(sections.join("\n\n"))
}

fn latest_version_source_slice_chunk_order(
    left: &&RuntimeMatchedChunk,
    right: &&RuntimeMatchedChunk,
) -> std::cmp::Ordering {
    score_value(right.score)
        .total_cmp(&score_value(left.score))
        .then_with(|| left.chunk_index.cmp(&right.chunk_index))
        .then_with(|| left.document_label.cmp(&right.document_label))
}

fn select_blocks_for_budget_in_order(blocks: Vec<String>, budget_chars: usize) -> Vec<String> {
    let mut selected = Vec::<String>::new();
    let mut used = 0usize;
    for block in blocks {
        let projected = used.saturating_add(block.len()).saturating_add(2);
        if projected > budget_chars && !selected.is_empty() {
            break;
        }
        used = projected;
        selected.push(block);
    }
    selected
}

fn select_source_slice_blocks_for_budget(
    blocks: Vec<String>,
    budget_chars: usize,
    direction: SourceSliceDirection,
) -> Vec<String> {
    let mut selected = Vec::<String>::new();
    let mut used = 0usize;
    let iter: Box<dyn Iterator<Item = String>> = match direction {
        SourceSliceDirection::Tail => Box::new(blocks.into_iter().rev()),
        SourceSliceDirection::Head | SourceSliceDirection::All => Box::new(blocks.into_iter()),
    };
    for block in iter {
        let projected = used.saturating_add(block.len()).saturating_add(2);
        if projected > budget_chars && !selected.is_empty() {
            break;
        }
        used = projected;
        selected.push(block);
    }
    if direction == SourceSliceDirection::Tail {
        selected.reverse();
    }
    selected
}

fn chunk_text_for_source_slice(chunk: &RuntimeMatchedChunk) -> String {
    let source = chunk.source_text.trim();
    if !source.is_empty() {
        return source.to_string();
    }
    chunk.excerpt.trim().to_string()
}

fn source_profile_text_for_source_slice(chunk: &RuntimeMatchedChunk) -> String {
    chunk
        .source_text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .filter(|line| !line.is_empty())
        .unwrap_or_else(|| chunk.excerpt.trim())
        .to_string()
}

fn source_slice_direction_label(direction: SourceSliceDirection) -> &'static str {
    match direction {
        SourceSliceDirection::Head => "head",
        SourceSliceDirection::Tail => "tail",
        SourceSliceDirection::All => "all",
    }
}

fn context_label(label: &str) -> String {
    label.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
pub(crate) fn build_references(
    entities: &[RuntimeMatchedEntity],
    relationships: &[RuntimeMatchedRelationship],
    chunks: &[RuntimeMatchedChunk],
    top_k: usize,
) -> Vec<QueryExecutionReference> {
    let mut references = Vec::new();
    let mut rank = 1usize;

    for chunk in chunks.iter().take(top_k) {
        references.push(QueryExecutionReference {
            kind: "chunk".to_string(),
            reference_id: chunk.chunk_id,
            excerpt: Some(chunk.excerpt.clone()),
            rank,
            score: chunk.score,
        });
        rank += 1;
    }
    for entity in entities.iter().take(top_k) {
        references.push(QueryExecutionReference {
            kind: "node".to_string(),
            reference_id: entity.node_id,
            excerpt: Some(entity.label.clone()),
            rank,
            score: entity.score,
        });
        rank += 1;
    }
    for relationship in relationships.iter().take(top_k) {
        references.push(QueryExecutionReference {
            kind: "edge".to_string(),
            reference_id: relationship.edge_id,
            excerpt: Some(relationship.reference_excerpt()),
            rank,
            score: relationship.score,
        });
        rank += 1;
    }

    references
}

pub(crate) fn build_grouped_reference_candidates(
    entities: &[RuntimeMatchedEntity],
    relationships: &[RuntimeMatchedRelationship],
    chunks: &[RuntimeMatchedChunk],
    top_k: usize,
    demoted_document_ids: &HashSet<Uuid>,
) -> Vec<GroupedReferenceCandidate> {
    let mut candidates = Vec::new();
    let mut rank = 1usize;

    for chunk in chunks.iter().take(top_k) {
        // Attached-context documents are subordinate context, never a standalone
        // subject to clarify between, so they do not produce a grouped reference
        // (and therefore never surface as a clarify variant).
        if demoted_document_ids.contains(&chunk.document_id) {
            continue;
        }
        candidates.push(GroupedReferenceCandidate {
            dedupe_key: format!("document:{}", chunk.document_id),
            kind: GroupedReferenceKind::Document,
            rank,
            title: chunk.document_label.clone(),
            excerpt: Some(chunk.excerpt.clone()),
            support_id: format!("chunk:{}", chunk.chunk_id),
        });
        rank += 1;
    }
    for entity in entities.iter().take(top_k) {
        candidates.push(GroupedReferenceCandidate {
            dedupe_key: format!("node:{}", entity.node_id),
            kind: GroupedReferenceKind::Entity,
            rank,
            title: entity.label.clone(),
            excerpt: Some(format!("{} ({})", entity.label, entity.node_type)),
            support_id: format!("node:{}", entity.node_id),
        });
        rank += 1;
    }
    for relationship in relationships.iter().take(top_k) {
        candidates.push(GroupedReferenceCandidate {
            dedupe_key: format!("edge:{}", relationship.edge_id),
            kind: GroupedReferenceKind::Relationship,
            rank,
            title: relationship.claim_text(),
            excerpt: Some(relationship.reference_excerpt()),
            support_id: format!("edge:{}", relationship.edge_id),
        });
        rank += 1;
    }

    candidates
}

pub(crate) fn build_structured_query_diagnostics(
    plan: &crate::services::query::planner::RuntimeQueryPlan,
    bundle: &RetrievalBundle,
    graph_index: &QueryGraphIndex,
    enrichment: &QueryExecutionEnrichment,
    include_debug: bool,
    context_text: &str,
) -> RuntimeStructuredQueryDiagnostics {
    RuntimeStructuredQueryDiagnostics {
        requested_mode: plan.requested_mode,
        planned_mode: plan.planned_mode,
        keywords: plan.keywords.clone(),
        high_level_keywords: plan.high_level_keywords.clone(),
        low_level_keywords: plan.low_level_keywords.clone(),
        top_k: plan.top_k,
        reference_counts: RuntimeStructuredQueryReferenceCounts {
            entity_count: bundle.entities.len(),
            relationship_count: bundle.relationships.len(),
            chunk_count: bundle.chunks.len(),
            graph_node_count: graph_index.node_count(),
            graph_edge_count: graph_index.edge_count(),
        },
        planning: enrichment.planning.clone(),
        rerank: enrichment.rerank.clone(),
        context_assembly: enrichment.context_assembly.clone(),
        grouped_references: enrichment.grouped_references.clone(),
        context_text: include_debug.then(|| context_text.to_string()),
        warning: None,
        warning_kind: None,
        library_summary: None,
    }
}

pub(crate) fn apply_query_execution_library_summary(
    diagnostics: &mut RuntimeStructuredQueryDiagnostics,
    context: Option<&RuntimeQueryLibraryContext>,
) {
    if let Some(context) = context {
        let summary = &context.summary;
        diagnostics.library_summary = Some(RuntimeStructuredQueryLibrarySummary {
            document_count: summary.document_count,
            graph_ready_count: summary.graph_ready_count,
            processing_count: summary.processing_count,
            failed_count: summary.failed_count,
            graph_status: summary.graph_status,
            recent_documents: context.recent_documents.clone(),
        });
        return;
    }

    diagnostics.library_summary = None;
}

pub(crate) fn apply_query_execution_warning(
    diagnostics: &mut RuntimeStructuredQueryDiagnostics,
    warning: Option<&RuntimeQueryWarning>,
) {
    if let Some(warning) = warning {
        diagnostics.warning = Some(warning.warning.clone());
        diagnostics.warning_kind = Some(warning.warning_kind);
        return;
    }

    diagnostics.warning = None;
    diagnostics.warning_kind = None;
}

pub(crate) async fn load_query_execution_library_context(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<RuntimeQueryLibraryContext> {
    let generation = load_latest_library_generation(state, library_id).await?;
    let graph_status = query_graph_status(generation.as_ref());

    // Canonical O(1) path — no more `list_documents` N+1 storm. Three
    // bounded queries: one Postgres aggregate for the summary counts,
    // one `runtime_graph_snapshot` point lookup, and one keyset page
    // (limit 12) for the recent-documents section fed into the prompt.
    // The previous implementation enumerated every document plus multiple
    // per-document prefetches per call, which on a 5k-doc library burned ~180 s per
    // query turn before the outer timeout cut it off.
    let metrics =
        crate::infra::repositories::content_repository::aggregate_library_document_metrics(
            &state.persistence.postgres,
            library_id,
        )
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))
        .context("failed to aggregate library metrics for query context")?;
    let recent_page = crate::infra::repositories::content_repository::list_document_page_rows(
        &state.persistence.postgres,
        library_id,
        false,
        None,
        12,
        None,
        crate::infra::repositories::content_repository::DocumentListSortColumn::CreatedAt,
        true,
        &[],
        &[],
    )
    .await
    .map_err(|error| anyhow::anyhow!(error.to_string()))
    .context("failed to load recent document rows for query context")?;

    let in_flight = metrics.processing + metrics.queued;
    // Backlog surfaced to the convergence-warning classifier covers
    // everything that is not yet readable — jobs still in flight
    // plus any queued / canceled retries the runtime will sweep
    // before the library reaches a fully-ready state. Derived from
    // the canonical metrics row so this number and the dashboard
    // `in-flight` card always agree.
    let backlog_count = in_flight;
    let convergence_status = query_execution_convergence_status(graph_status, in_flight);
    let summary = RuntimeQueryLibrarySummary {
        document_count: usize::try_from(metrics.total).unwrap_or(0),
        // Canonical `graph_ready` comes from the metrics row (already
        // clamped to `ready` so the published invariant holds).
        graph_ready_count: usize::try_from(metrics.graph_ready).unwrap_or(0),
        processing_count: usize::try_from(in_flight).unwrap_or(0),
        failed_count: usize::try_from(metrics.failed + metrics.canceled).unwrap_or(0),
        graph_status,
    };
    let recent_documents =
        summarize_recent_query_documents_from_rows(&recent_page.rows, graph_status);
    Ok(RuntimeQueryLibraryContext {
        summary,
        recent_documents,
        warning: query_execution_convergence_warning(state, convergence_status, backlog_count),
    })
}

fn summarize_recent_query_documents_from_rows(
    rows: &[crate::infra::repositories::content_repository::ContentDocumentListRow],
    graph_status: &'static str,
) -> Vec<RuntimeQueryRecentDocument> {
    rows.iter()
        .map(|row| {
            let title = row
                .revision_title
                .as_deref()
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| row.external_key.clone());
            let pipeline_state =
                match (row.job_queue_state.as_deref(), row.mutation_state.as_deref()) {
                    (Some("failed"), _) | (_, Some("failed" | "conflicted")) => "failed",
                    (Some("leased"), _) => "processing",
                    _ if row.readable_revision_id.is_some() => "ready",
                    (Some("canceled"), _) | (_, Some("canceled")) => "failed",
                    (Some("queued"), _) | (_, Some("accepted" | "running")) => "queued",
                    _ => "pending",
                };
            let graph_state = if pipeline_state == "ready" && graph_status == "current" {
                "ready"
            } else if pipeline_state == "failed" {
                "failed"
            } else {
                "pending"
            };
            RuntimeQueryRecentDocument {
                title,
                uploaded_at: row.created_at.to_rfc3339(),
                mime_type: row.revision_mime_type.clone(),
                pipeline_state,
                graph_state,
                preview_excerpt: None,
            }
        })
        .collect()
}

fn query_execution_convergence_status(graph_status: &str, backlog_count: i64) -> &'static str {
    if backlog_count > 0 || !matches!(graph_status, "current") {
        return "partial";
    }
    "current"
}

fn query_execution_convergence_warning(
    state: &AppState,
    convergence_status: &str,
    backlog_count: i64,
) -> Option<RuntimeQueryWarning> {
    if convergence_status != "partial" {
        return None;
    }

    let threshold =
        i64::try_from(state.bulk_ingest_hardening.graph_convergence_warning_backlog_threshold)
            .unwrap_or(1);
    if backlog_count < threshold {
        return None;
    }

    Some(RuntimeQueryWarning {
        warning: format!(
            "Graph coverage is still converging while {backlog_count} document or mutation task(s) remain in backlog."
        ),
        warning_kind: "partial_convergence",
    })
}

pub(crate) fn assemble_answer_context(
    summary: &RuntimeQueryLibrarySummary,
    retrieved_documents: &[RuntimeRetrievedDocumentBrief],
    technical_literals_text: Option<&str>,
    retrieved_context: &str,
    prioritize_retrieved_context: bool,
) -> String {
    // Canonical answer prompt is a deterministic function of
    // `(query, retrieved evidence, stable library summary)`. Live ingest
    // metadata (pipeline state, recent uploads, mutating preview excerpts)
    // is intentionally NOT included here — it would make the prompt
    // change between calls during active ingestion and break MCP–UI
    // parity (constitution §16). The same diagnostic signals are still
    // surfaced to the UI via `RuntimeStructuredQueryLibrarySummary` for
    // operator visibility, but they never reach the LLM answer step.
    let mut sections = vec![
        [
            "Library summary".to_string(),
            format!("- Documents in library: {}", summary.document_count),
            format!("- Graph-ready documents: {}", summary.graph_ready_count),
            format!(
                "- Graph coverage status: {}",
                query_graph_status_prompt_label(summary.graph_status)
            ),
        ]
        .join("\n"),
    ];
    let trimmed_context = retrieved_context.trim();
    if let Some(technical_literals_text) = technical_literals_text
        && !technical_literals_text.trim().is_empty()
    {
        sections.push(technical_literals_text.trim().to_string());
    }
    if prioritize_retrieved_context && !trimmed_context.is_empty() {
        sections.push(trimmed_context.to_string());
    }
    if !retrieved_documents.is_empty() {
        let retrieved_lines = retrieved_documents
            .iter()
            .map(|document| {
                // Render only the resolved document hint. Raw source_uri
                // stays out of the LLM-visible prompt surface.
                let mut line = format!("- {}", document.title);
                if let Some(hint) = document.document_hint.as_deref() {
                    let trimmed = hint.trim();
                    if !trimmed.is_empty() {
                        line.push_str(&format!(" (document_hint: {trimmed})"));
                    }
                }
                line.push_str(&format!(": {}", document.preview_excerpt));
                line
            })
            .collect::<Vec<_>>();
        sections.push(format!("Retrieved document briefs\n{}", retrieved_lines.join("\n")));
    }
    if trimmed_context.is_empty() {
        return sections.join("\n\n");
    }
    if !prioritize_retrieved_context {
        sections.push(trimmed_context.to_string());
    }
    sections.join("\n\n")
}

fn query_graph_status_prompt_label(graph_status: &str) -> &'static str {
    match graph_status {
        "current" => "current (all documents processed)",
        "partial" => "partial (some documents still processing)",
        _ => "empty (no graph data yet)",
    }
}

pub(crate) async fn load_retrieved_document_briefs(
    state: &AppState,
    chunks: &[RuntimeMatchedChunk],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    top_k: usize,
    focused_document_id: Option<Uuid>,
) -> Vec<RuntimeRetrievedDocumentBrief> {
    let brief_limit = top_k.clamp(16, 48);
    let mut best_by_document = HashMap::<Uuid, RuntimeMatchedChunk>::new();
    let mut ordered_document_ids = Vec::<Uuid>::new();
    // Collect the focused-document chunks once — consolidation has
    // already sorted them by chunk_index and biased their scores so
    // they sit at the top of the bundle; the brief preview joins the
    // first N of them in reading order. Non-focused documents fall
    // back to a single "best-scored chunk excerpt".
    let mut focused_chunks: Vec<&RuntimeMatchedChunk> = Vec::new();

    for chunk in chunks {
        // Attached-context documents (image attachments collapsed onto a parent
        // page) are subordinate context, not standalone retrieved documents the
        // user should be asked to clarify between. Exclude them from the brief
        // set (which feeds the clarify-vs-answer disposition) unless the query
        // explicitly focuses on that document. Role is the only signal — no
        // MIME/extension/filename inspection here.
        if Some(chunk.document_id) != focused_document_id
            && document_index.get(&chunk.document_id).is_some_and(|document| {
                crate::domains::content::role_is_attached_context(&document.document_role)
            })
        {
            continue;
        }
        if Some(chunk.document_id) == focused_document_id {
            focused_chunks.push(chunk);
        }
        let entry = best_by_document.entry(chunk.document_id).or_insert_with(|| {
            ordered_document_ids.push(chunk.document_id);
            chunk.clone()
        });
        if score_value(chunk.score) > score_value(entry.score) {
            *entry = chunk.clone();
        }
    }

    focused_chunks.sort_by_key(|chunk| chunk.chunk_index);
    let focused_preview = focused_preview_from_bundle_chunks(&focused_chunks);

    let ranked_documents = ordered_document_ids
        .into_iter()
        .take(brief_limit)
        .filter_map(|document_id| {
            let document = document_index.get(&document_id)?.clone();
            let fallback_excerpt =
                best_by_document.get(&document_id).map(|chunk| chunk.excerpt.clone());
            let is_focused = Some(document_id) == focused_document_id;
            Some((document, fallback_excerpt, is_focused))
        })
        .collect::<Vec<_>>();

    let focused_preview_ref = focused_preview.as_ref();
    let previews = join_all(ranked_documents.into_iter().map(
        |(document, fallback_excerpt, is_focused)| async move {
            let (preview_excerpt, document_hint) = if is_focused {
                // For the winner we already have the anchor-window
                // chunks in the bundle; synthesize the preview from
                // them and skip the `list_chunks_by_revision` fetch
                // entirely. The separate revision lookup is kept so
                // the resolved document_hint still reaches the prompt.
                let document_hint = load_retrieved_document_hint(state, &document).await;
                let preview = focused_preview_ref.cloned().or(fallback_excerpt).unwrap_or_default();
                (preview, document_hint)
            } else {
                let (preview, document_hint) =
                    load_retrieved_document_preview_and_hint(state, &document)
                        .await
                        .unwrap_or((None, None));
                let preview = preview.or(fallback_excerpt).unwrap_or_default();
                (preview, document_hint)
            };
            if preview_excerpt.trim().is_empty() {
                return None;
            }
            let title = document
                .title
                .clone()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| document.external_key.clone());
            Some(RuntimeRetrievedDocumentBrief { title, preview_excerpt, document_hint })
        },
    ))
    .await;

    previews.into_iter().flatten().collect()
}

/// Build the "Retrieved document briefs" preview for the winning
/// document out of the chunks consolidation has already packed into
/// the bundle. Joining the anchor-window `source_text` segments in
/// reading order produces a preview that actually reflects where the
/// answer will quote from, rather than the intro-chunk of the
/// revision (which is what `list_chunks_by_revision` surfaces).
///
/// `source_text` is already normalised in `apply_winner_chunks` via
/// `repair_technical_layout_noise`, so we just trim and join here.
fn focused_preview_from_bundle_chunks(chunks: &[&RuntimeMatchedChunk]) -> Option<String> {
    let joined = chunks
        .iter()
        .filter_map(|chunk| {
            let trimmed = chunk.source_text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
        .take(RETRIEVED_DOCUMENT_BRIEF_SOURCE_CHUNKS)
        .collect::<Vec<_>>()
        .join(" ");
    (!joined.is_empty()).then(|| excerpt_for(&joined, RETRIEVED_DOCUMENT_BRIEF_PREVIEW_CHARS))
}

async fn load_retrieved_document_hint(
    state: &AppState,
    document: &KnowledgeDocumentRow,
) -> Option<String> {
    let revision_id = document.readable_revision_id.or(document.active_revision_id)?;
    let revision = state.document_store.get_revision(revision_id).await.ok()??;
    resolve_retrieved_document_hint(state, document, Some(&revision)).await
}

async fn load_retrieved_document_preview_and_hint(
    state: &AppState,
    document: &KnowledgeDocumentRow,
) -> Option<(Option<String>, Option<String>)> {
    // Citation provenance is stored on the revision row, not on the
    // document root — a document can have many revisions over its
    // lifetime and each carries the provenance of *that* upload
    // (URL for web-ingested pages, storage reference for files).
    // We read the readable revision first (what the user would see
    // today); the active revision is the fallback while a newer
    // ingest run is still processing but has not landed yet.
    let revision_id = document.readable_revision_id.or(document.active_revision_id)?;

    let revision_future = state.document_store.get_revision(revision_id);
    // The preview only consumes the first few reading-order chunks, so
    // fetch a small head window instead of scanning the whole revision
    // (a large document can hold thousands of chunks). Extra headroom
    // covers empty/profile chunks dropped during normalization below.
    let chunks_future = state
        .document_store
        .list_head_chunks_by_revision(revision_id, RETRIEVED_DOCUMENT_BRIEF_SOURCE_CHUNKS * 4);
    let (revision_result, chunks_result) =
        futures::future::join(revision_future, chunks_future).await;

    let revision = revision_result.ok().flatten();
    let document_hint = resolve_retrieved_document_hint(state, document, revision.as_ref()).await;

    let chunks = chunks_result.ok().unwrap_or_default();
    let combined = chunks
        .into_iter()
        .filter_map(|chunk| {
            let normalized = repair_technical_layout_noise(&chunk.normalized_text);
            let normalized = normalized.trim().to_string();
            if normalized.is_empty() {
                return None;
            }
            Some(normalized)
        })
        .take(RETRIEVED_DOCUMENT_BRIEF_SOURCE_CHUNKS)
        .collect::<Vec<_>>()
        .join(" ");

    let preview = (!combined.is_empty())
        .then(|| excerpt_for(&combined, RETRIEVED_DOCUMENT_BRIEF_PREVIEW_CHARS));

    Some((preview, document_hint))
}

async fn resolve_retrieved_document_hint(
    state: &AppState,
    document: &KnowledgeDocumentRow,
    knowledge_revision: Option<&KnowledgeRevisionRow>,
) -> Option<String> {
    let library_setting =
        catalog_repository::get_library_by_id(&state.persistence.postgres, document.library_id)
            .await
            .ok()
            .flatten()
            .map(|library| library.include_document_hint_in_mcp_answers)
            .unwrap_or(true);

    let document_title = document
        .title
        .as_deref()
        .or_else(|| knowledge_revision.and_then(|revision| revision.title.as_deref()))
        .or(Some(document.external_key.as_str()));

    let resolved = knowledge_revision.and_then(|revision| {
        resolve_document_hint(
            &revision.revision_kind,
            revision.source_uri.as_deref(),
            revision.document_hint.as_deref(),
            document_title,
            library_setting,
        )
    });

    resolved.map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

pub(crate) fn assemble_context_metadata_for_query(
    planned_mode: RuntimeQueryMode,
    graph_support_count: usize,
    document_support_count: usize,
) -> crate::domains::query::ContextAssemblyMetadata {
    assemble_context_metadata(&ContextAssemblyRequest {
        requested_mode: planned_mode,
        graph_support_count,
        document_support_count,
    })
}

pub(crate) fn group_visible_references_for_query(
    candidates: &[GroupedReferenceCandidate],
    top_k: usize,
) -> Vec<crate::domains::query::GroupedReference> {
    group_visible_references(candidates, top_k)
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;
