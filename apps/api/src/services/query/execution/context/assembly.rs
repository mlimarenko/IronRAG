use std::collections::HashSet;

use uuid::Uuid;

use crate::domains::query_ir::{QueryAct, QueryIR};

use super::super::{
    retrieve::excerpt_for,
    source_excerpt::top_level_declaration_signatures,
    technical_literals::technical_literal_focus_keywords,
    types::{RuntimeMatchedChunk, RuntimeMatchedEntity, RuntimeMatchedRelationship},
};
use super::{
    chunk_ranking::order_bounded_context_chunks,
    document_rendering::{
        assemble_ordered_source_slice_context, bounded_context_document_block, context_label,
        preferred_chunk_text,
    },
    entity_context::{
        entity_match_context_lines, graph_node_context_line,
        should_prioritize_entity_nodes_before_evidence, should_prioritize_graph_context_for_query,
    },
};

// The character cap keeps the lane below one fifth of the default 22k answer
// budget. The separate entry cap also bounds documents with very short keys.
pub(super) const DECLARATION_INVENTORY_ENTRY_LIMIT: usize = 48;
pub(super) const DECLARATION_INVENTORY_CONTEXT_CHARS: usize = 4_096;

#[cfg(test)]
pub(crate) fn assemble_bounded_context(
    entities: &[RuntimeMatchedEntity],
    relationships: &[RuntimeMatchedRelationship],
    chunks: &[RuntimeMatchedChunk],
    budget_chars: usize,
) -> String {
    assemble_bounded_context_from_chunks(BoundedContextChunks {
        entities,
        relationships,
        chunks: &chunks.iter().collect::<Vec<_>>(),
        budget_chars,
        ordinary_keywords: &[],
        entity_match_lines: &[],
        graph_evidence_lines: &[],
        prefer_graph_first: false,
        prefer_entity_nodes_before_evidence: false,
        leading_lines: &[],
    })
}

struct BoundedContextSections {
    sections: Vec<String>,
    initial_excerpt: Option<String>,
    used_chars: usize,
    budget_chars: usize,
    exceeded_budget: bool,
}

impl BoundedContextSections {
    fn new(budget_chars: usize) -> Self {
        Self {
            sections: Vec::new(),
            initial_excerpt: None,
            used_chars: 0,
            budget_chars,
            exceeded_budget: false,
        }
    }

    fn push(&mut self, line: String) -> bool {
        let overhead = if self.sections.is_empty() { "Context\n".len() } else { 1 };
        let projected = self.used_chars + overhead + line.len();
        if projected > self.budget_chars {
            self.exceeded_budget = true;
            self.remember_initial_excerpt(&line);
            return false;
        }
        self.used_chars = projected;
        self.sections.push(line);
        self.initial_excerpt = None;
        true
    }

    fn remember_initial_excerpt(&mut self, line: &str) {
        if !self.sections.is_empty() || self.initial_excerpt.is_some() {
            return;
        }
        let available = self.budget_chars.saturating_sub("Context\n".len() + 4);
        if available > 0 {
            self.initial_excerpt = Some(excerpt_for(line, available));
        }
    }

    fn finish(self) -> String {
        if self.sections.is_empty() {
            self.initial_excerpt.unwrap_or_default()
        } else if self.exceeded_budget {
            self.sections.join("\n")
        } else {
            format!("Context\n{}", self.sections.join("\n"))
        }
    }
}

/// Inputs for the bounded-context chunk assembly helpers.
struct BoundedContextChunks<'a> {
    entities: &'a [RuntimeMatchedEntity],
    relationships: &'a [RuntimeMatchedRelationship],
    chunks: &'a [&'a RuntimeMatchedChunk],
    budget_chars: usize,
    ordinary_keywords: &'a [String],
    entity_match_lines: &'a [String],
    graph_evidence_lines: &'a [String],
    prefer_graph_first: bool,
    prefer_entity_nodes_before_evidence: bool,
    leading_lines: &'a [String],
}

fn assemble_bounded_context_from_chunks(input: BoundedContextChunks<'_>) -> String {
    assemble_bounded_context_from_chunks_with_leading_lines(input)
}

fn assemble_bounded_context_from_chunks_with_leading_lines(
    input: BoundedContextChunks<'_>,
) -> String {
    let BoundedContextChunks {
        entities,
        relationships,
        chunks,
        budget_chars,
        ordinary_keywords,
        entity_match_lines,
        graph_evidence_lines,
        prefer_graph_first,
        prefer_entity_nodes_before_evidence,
        leading_lines,
    } = input;
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
        leading_lines,
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

pub(super) fn interleave_bounded_context_lines(
    graph_lines: &[String],
    document_lines: &[String],
    budget_chars: usize,
    prefer_graph_first: bool,
    leading_lines: &[String],
) -> String {
    let mut output = BoundedContextSections::new(budget_chars);
    let mut leading_index = 0usize;
    push_bounded_line_slice(&mut output, leading_lines, &mut leading_index);
    let mut graph_index = 0usize;
    let mut document_index = 0usize;
    if prefer_graph_first {
        push_bounded_line_slice(&mut output, graph_lines, &mut graph_index);
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
) {
    while let Some(line) = lines.get(*index) {
        *index += 1;
        output.push(line.clone());
    }
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
        output.push(line);
    }
    consumed
}

/// Inputs of the query-level bounded-context assembly.
pub(crate) struct BoundedContextQuery<'a> {
    pub(crate) query_ir: &'a QueryIR,
    pub(crate) question: &'a str,
    pub(crate) entities: &'a [RuntimeMatchedEntity],
    pub(crate) relationships: &'a [RuntimeMatchedRelationship],
    pub(crate) chunks: &'a [RuntimeMatchedChunk],
    pub(crate) graph_evidence_lines: &'a [String],
    pub(crate) budget_chars: usize,
}

pub(crate) fn assemble_bounded_context_for_query(input: BoundedContextQuery<'_>) -> String {
    let BoundedContextQuery {
        query_ir,
        question,
        entities,
        relationships,
        chunks,
        graph_evidence_lines,
        budget_chars,
    } = input;
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
    assemble_bounded_context_from_chunks(BoundedContextChunks {
        entities,
        relationships,
        chunks: &ordered_chunks,
        budget_chars,
        ordinary_keywords: &keywords,
        entity_match_lines: &entity_match_lines,
        graph_evidence_lines,
        prefer_graph_first,
        prefer_entity_nodes_before_evidence,
        leading_lines: &[],
    })
}

/// Inputs of the declaration-inventory bounded-context assembly.
pub(crate) struct BoundedContextInventoryQuery<'a> {
    pub(crate) query_ir: &'a QueryIR,
    pub(crate) question: &'a str,
    pub(crate) entities: &'a [RuntimeMatchedEntity],
    pub(crate) relationships: &'a [RuntimeMatchedRelationship],
    pub(crate) chunks: &'a [RuntimeMatchedChunk],
    pub(crate) graph_evidence_lines: &'a [String],
    pub(crate) inventory_candidates: &'a [RuntimeMatchedChunk],
    pub(crate) focused_document_id: Option<Uuid>,
    pub(crate) fallback_declaration_chunks: &'a [RuntimeMatchedChunk],
    pub(crate) budget_chars: usize,
}

pub(crate) fn assemble_bounded_context_for_query_with_inventory_candidates(
    input: BoundedContextInventoryQuery<'_>,
) -> String {
    let BoundedContextInventoryQuery {
        query_ir,
        question,
        entities,
        relationships,
        chunks,
        graph_evidence_lines,
        inventory_candidates,
        focused_document_id,
        fallback_declaration_chunks,
        budget_chars,
    } = input;
    let Some(document_id) =
        declaration_inventory_document_id(query_ir, inventory_candidates, focused_document_id)
    else {
        return super::prepend_focused_declaration_context(
            query_ir,
            question,
            super::assemble_bounded_context_for_query(BoundedContextQuery {
                query_ir,
                question,
                entities,
                relationships,
                chunks,
                graph_evidence_lines,
                budget_chars,
            }),
            fallback_declaration_chunks,
        );
    };
    let mut leading_lines =
        declaration_inventory_context_block(inventory_candidates, document_id, budget_chars)
            .into_iter()
            .collect::<Vec<_>>();
    if leading_lines.is_empty() {
        let keywords = technical_literal_focus_keywords(question, Some(query_ir));
        leading_lines.extend(
            fallback_declaration_chunks
                .iter()
                .filter(|chunk| chunk.document_id == document_id)
                .map(|chunk| bounded_context_document_block(chunk, &keywords)),
        );
    }
    if let Some(context) = assemble_ordered_source_slice_context(query_ir, chunks, budget_chars) {
        return prepend_bounded_source_slice_sections(&context, &leading_lines, budget_chars);
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
    assemble_bounded_context_from_chunks_with_leading_lines(BoundedContextChunks {
        entities,
        relationships,
        chunks: &ordered_chunks,
        budget_chars,
        ordinary_keywords: &keywords,
        entity_match_lines: &entity_match_lines,
        graph_evidence_lines,
        prefer_graph_first,
        prefer_entity_nodes_before_evidence,
        leading_lines: &leading_lines,
    })
}

pub(crate) fn query_requests_declaration_inventory(query_ir: &QueryIR) -> bool {
    // Enumerate is the direct signal. Describe is included because the
    // compiler classifies "what Xs are used and what does each do" questions
    // as Describe (measured: the react_hooks golden question compiles to
    // act=describe, scope=single_document), and a description scoped to one
    // document is grounded by knowing what that document declares. The
    // inventory stays bounded and competes inside the existing budget, so
    // the cost of firing on a plain descriptive question is a capped block
    // of signature lines, not displaced evidence.
    matches!(query_ir.act, QueryAct::Enumerate | QueryAct::Describe)
        && matches!(query_ir.scope, crate::domains::query_ir::QueryScope::SingleDocument)
}

pub(super) fn declaration_inventory_context_block(
    candidates: &[RuntimeMatchedChunk],
    document_id: Uuid,
    budget_chars: usize,
) -> Option<String> {
    let mut document_chunks =
        candidates.iter().filter(|chunk| chunk.document_id == document_id).collect::<Vec<_>>();
    document_chunks.sort_by(|left, right| {
        left.chunk_index.cmp(&right.chunk_index).then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    let document_label = document_chunks.first()?.document_label.trim();
    let header = format!(
        "[document declaration_inventory document=\"{}\" order=document]",
        context_label(document_label)
    );
    let block_budget =
        DECLARATION_INVENTORY_CONTEXT_CHARS.min(budget_chars.saturating_sub("Context\n".len()));
    if header.len() > block_budget {
        return None;
    }

    let mut seen = HashSet::<String>::new();
    let mut lines = vec![header];
    let mut used_chars = lines[0].len();
    let mut declaration_count = 0usize;
    for signature in document_chunks
        .into_iter()
        .flat_map(|chunk| top_level_declaration_signatures(preferred_chunk_text(chunk)))
    {
        if !seen.insert(signature.clone()) {
            continue;
        }
        let line = format!("- {signature}");
        let projected = used_chars.saturating_add(1).saturating_add(line.len());
        if projected > block_budget || declaration_count >= DECLARATION_INVENTORY_ENTRY_LIMIT {
            break;
        }
        used_chars = projected;
        lines.push(line);
        declaration_count = declaration_count.saturating_add(1);
    }
    (lines.len() > 1).then(|| lines.join("\n"))
}

fn prepend_bounded_source_slice_sections(
    baseline_context: &str,
    leading_lines: &[String],
    budget_chars: usize,
) -> String {
    if leading_lines.is_empty() {
        return baseline_context.to_string();
    }
    let mut output = BoundedContextSections::new(budget_chars);
    let mut leading_index = 0usize;
    push_bounded_line_slice(&mut output, leading_lines, &mut leading_index);
    let baseline_body = baseline_context.strip_prefix("Context\n").unwrap_or(baseline_context);
    for section in baseline_body.split("\n\n").filter(|section| !section.trim().is_empty()) {
        output.push(section.to_string());
    }
    output.finish()
}

pub(crate) fn declaration_inventory_document_id(
    query_ir: &QueryIR,
    candidates: &[RuntimeMatchedChunk],
    focused_document_id: Option<Uuid>,
) -> Option<Uuid> {
    if !query_requests_declaration_inventory(query_ir) {
        return None;
    }
    if let Some(document_id) = focused_document_id
        && candidates.iter().any(|chunk| chunk.document_id == document_id)
    {
        return Some(document_id);
    }
    // Candidate order is the final retrieval order. Inventorying a document
    // is a focused answer-stage choice, so the first-ranked candidate wins;
    // repeated chunks from a runner-up must not overturn that ranking.
    candidates.first().map(|chunk| chunk.document_id)
}

pub(crate) fn prepend_focused_declaration_context(
    query_ir: &QueryIR,
    question: &str,
    baseline_context: String,
    declaration_chunks: &[RuntimeMatchedChunk],
) -> String {
    if declaration_chunks.is_empty() {
        baseline_context
    } else {
        let keywords = technical_literal_focus_keywords(question, Some(query_ir));
        let declaration_context = declaration_chunks
            .iter()
            .map(|chunk| bounded_context_document_block(chunk, &keywords))
            .collect::<Vec<_>>()
            .join("\n");
        if let Some(baseline_body) = baseline_context.strip_prefix("Context\n") {
            format!("Context\n{declaration_context}\n{baseline_body}")
        } else if baseline_context.is_empty() {
            format!("Context\n{declaration_context}")
        } else {
            format!("{declaration_context}\n{baseline_context}")
        }
    }
}
