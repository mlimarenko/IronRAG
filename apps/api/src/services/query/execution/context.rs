use std::collections::{HashMap, HashSet};

use anyhow::Context;
use futures::future::join_all;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::query::{GroupedReferenceKind, RuntimeQueryMode},
    domains::query_ir::{QueryAct, QueryIR, SourceSliceDirection},
    infra::arangodb::document_store::KnowledgeDocumentRow,
    services::query::{
        support::{
            ContextAssemblyRequest, GroupedReferenceCandidate, assemble_context_metadata,
            group_visible_references,
        },
        text_match::{
            normalized_alnum_tokens, select_related_overlap_tokens,
            token_sequence_exact_or_contains,
        },
    },
    shared::extraction::text_render::repair_technical_layout_noise,
};

use super::retrieve::{
    excerpt_for, focused_excerpt_for, load_latest_library_generation, query_graph_status,
    score_value,
};
use super::technical_literals::{
    question_mentions_pagination, select_document_balanced_chunks, technical_literal_focus_keywords,
};
use super::types::*;

const BOUNDED_SOURCE_UNIT_CONTEXT_CHARS: usize = 4_000;
const BOUNDED_ORDINARY_CONTEXT_CHARS: usize = 1_200;
const ENTITY_MATCH_CONTEXT_LINE_LIMIT: usize = 8;

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
    )
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
) -> String {
    let mut graph_lines = entity_match_lines.to_vec();
    graph_lines.extend(graph_evidence_lines.iter().cloned());
    graph_lines.extend(
        entities
            .iter()
            .map(|entity| format!("[graph-node] {} ({})", entity.label, entity.node_type)),
    );
    graph_lines.extend(relationships.iter().map(RuntimeMatchedRelationship::context_line));
    let document_lines = chunks
        .iter()
        .map(|chunk| bounded_context_document_block(chunk, ordinary_keywords))
        .collect::<Vec<_>>();

    let mut sections = Vec::new();
    let mut used = 0usize;
    let mut graph_index = 0usize;
    let mut document_index = 0usize;
    if prefer_graph_first {
        while let Some(line) = graph_lines.get(graph_index) {
            let projected = used + "Context".len() + line.len() + 4;
            if projected > budget_chars {
                if sections.is_empty() {
                    let available = budget_chars.saturating_sub("Context\n".len() + 4);
                    if available > 0 {
                        sections.push(excerpt_for(line, available));
                    }
                }
                return if sections.is_empty() { String::new() } else { sections.join("\n") };
            }
            used = projected;
            sections.push(line.clone());
            graph_index += 1;
        }
    }

    let mut prefer_document = !document_lines.is_empty();

    while graph_index < graph_lines.len() || document_index < document_lines.len() {
        let mut consumed = false;
        for bucket in 0..2 {
            let take_document = if prefer_document { bucket == 0 } else { bucket == 1 };
            let next_line = if take_document {
                document_lines.get(document_index).cloned().map(|line| {
                    document_index += 1;
                    line
                })
            } else {
                graph_lines.get(graph_index).cloned().map(|line| {
                    graph_index += 1;
                    line
                })
            };

            let Some(line) = next_line else {
                continue;
            };
            let projected = used + "Context".len() + line.len() + 4;
            if projected > budget_chars {
                if sections.is_empty() {
                    let available = budget_chars.saturating_sub("Context\n".len() + 4);
                    if available > 0 {
                        sections.push(excerpt_for(&line, available));
                    }
                }
                return if sections.is_empty() { String::new() } else { sections.join("\n") };
            }
            used = projected;
            sections.push(line);
            consumed = true;
        }
        if !consumed {
            break;
        }
        prefer_document = !prefer_document;
    }

    if sections.is_empty() { String::new() } else { format!("Context\n{}", sections.join("\n")) }
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
    assemble_bounded_context_from_chunks(
        entities,
        relationships,
        &ordered_chunks,
        budget_chars,
        &keywords,
        &entity_match_lines,
        graph_evidence_lines,
        prefer_graph_first,
    )
}

fn entity_match_context_lines(
    query_ir: &QueryIR,
    entities: &[RuntimeMatchedEntity],
) -> Vec<String> {
    if query_ir.target_entities.is_empty() || entities.is_empty() {
        return Vec::new();
    }

    let target_sets = query_ir
        .target_entities
        .iter()
        .filter_map(|mention| {
            let label = mention.label.trim();
            if label.is_empty() {
                return None;
            }
            if normalized_alnum_tokens(label, 3).is_empty() {
                return None;
            }
            let related_tokens = select_related_overlap_tokens(
                label,
                entities.iter().map(|entity| entity.label.as_str()),
                3,
            );
            Some((label.to_string(), related_tokens))
        })
        .collect::<Vec<_>>();
    if target_sets.is_empty() {
        return Vec::new();
    }

    let mut seen_nodes = HashSet::<Uuid>::new();
    let mut lines = Vec::new();
    for entity in entities {
        if lines.len() >= ENTITY_MATCH_CONTEXT_LINE_LIMIT || !seen_nodes.insert(entity.node_id) {
            continue;
        }
        let label = entity.label.trim();
        if label.is_empty() {
            continue;
        }
        let label_tokens = normalized_alnum_tokens(label, 3);
        if label_tokens.is_empty() {
            continue;
        }
        let mut best_kind: Option<&'static str> = None;
        for (target_label, related_tokens) in &target_sets {
            if token_sequence_exact_or_contains(label, target_label, 3) {
                best_kind = Some("exact");
                break;
            }
            if !related_tokens.is_empty() && related_tokens.matches_tokens(&label_tokens) {
                best_kind.get_or_insert("token-overlap");
            }
        }
        let Some(kind) = best_kind else {
            continue;
        };
        lines.push(format!("[entity-match {kind}] {} ({})", entity.label, entity.node_type));
    }
    lines
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
    (has_graph_topology_support || has_graph_evidence_support)
        && !query_ir.target_entities.is_empty()
        && matches!(
            query_ir.act,
            QueryAct::RetrieveValue
                | QueryAct::Describe
                | QueryAct::Compare
                | QueryAct::Enumerate
                | QueryAct::Meta
        )
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
    let pagination_requested = question_mentions_pagination(question);
    let selected = select_document_balanced_chunks(
        question,
        Some(query_ir),
        chunks,
        keywords,
        pagination_requested,
        chunks.len(),
        super::MAX_CHUNKS_PER_DOCUMENT,
    );
    let mut ordered = Vec::<&RuntimeMatchedChunk>::with_capacity(chunks.len());
    let mut seen = HashSet::<uuid::Uuid>::with_capacity(chunks.len());

    for chunk in
        chunks.iter().filter(|chunk| super::source_profile::is_source_profile_runtime_chunk(chunk))
    {
        if seen.insert(chunk.chunk_id) {
            ordered.push(chunk);
        }
    }
    for chunk in selected {
        if seen.insert(chunk.chunk_id) {
            ordered.push(chunk);
        }
    }
    for chunk in chunks {
        if seen.insert(chunk.chunk_id) {
            ordered.push(chunk);
        }
    }
    ordered
}

fn bounded_context_document_block(
    chunk: &RuntimeMatchedChunk,
    ordinary_keywords: &[String],
) -> String {
    if chunk.score_kind == RuntimeChunkScoreKind::GraphEvidence {
        let source_text = chunk.source_text.trim();
        let text = if source_text.is_empty() { chunk.excerpt.trim() } else { source_text };
        return format!(
            "[document graph_evidence document=\"{}\" ordinal={}]\n{}",
            context_label(&chunk.document_label),
            chunk.chunk_index,
            excerpt_for(text, BOUNDED_SOURCE_UNIT_CONTEXT_CHARS)
        );
    }
    if is_structured_source_unit_context_chunk(chunk) {
        let source_text = chunk.source_text.trim();
        let text = if source_text.is_empty() { chunk.excerpt.trim() } else { source_text };
        return format!(
            "[document source_unit ordinal={} document=\"{}\"]\n{}",
            chunk.chunk_index,
            context_label(&chunk.document_label),
            excerpt_for(text, BOUNDED_SOURCE_UNIT_CONTEXT_CHARS)
        );
    }
    if chunk.score_kind == RuntimeChunkScoreKind::SourceContext
        && !super::source_profile::is_source_profile_runtime_chunk(chunk)
    {
        let source_text = chunk.source_text.trim();
        let text = if source_text.is_empty() { chunk.excerpt.trim() } else { source_text };
        return format!(
            "[document source_context ordinal={} document=\"{}\"]\n{}",
            chunk.chunk_index,
            context_label(&chunk.document_label),
            excerpt_for(text, BOUNDED_SOURCE_UNIT_CONTEXT_CHARS)
        );
    }
    let text = query_focused_chunk_context_text(chunk, ordinary_keywords);
    format!("[document] {}: {}", chunk.document_label, text.trim())
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
    if content_chunks.is_empty() {
        return None;
    }
    content_chunks.sort_by_key(|chunk| (chunk.document_label.clone(), chunk.chunk_index));
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
    content_blocks =
        select_source_slice_blocks_for_budget(content_blocks, remaining_budget, slice.direction);
    if content_blocks.is_empty() {
        return None;
    }
    let mut sections = Vec::new();
    sections.push(header);
    sections.append(&mut profile_blocks);
    sections.append(&mut content_blocks);
    Some(sections.join("\n\n"))
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
) -> Vec<GroupedReferenceCandidate> {
    let mut candidates = Vec::new();
    let mut rank = 1usize;

    for chunk in chunks.iter().take(top_k) {
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
    // The previous implementation enumerated every document + 6 Arango
    // prefetches per call, which on a 5k-doc library burned ~180 s per
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
            format!("- Documents still processing: {}", summary.processing_count),
            format!("- Documents failed in pipeline: {}", summary.failed_count),
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
                // Render source URL when the runtime has one (web
                // ingest, external link). The model is instructed
                // in the single-shot prompt to quote this URL when
                // citing the document so the end user can click
                // through. For uploads without a URL we just show
                // the document title.
                let mut line = format!("- {}", document.title);
                if let Some(source) = document.source_uri.as_deref() {
                    let trimmed = source.trim();
                    if !trimmed.is_empty() {
                        line.push_str(&format!(" (source: {trimmed})"));
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
            let (preview_excerpt, source_uri) = if is_focused {
                // For the winner we already have the anchor-window
                // chunks in the bundle; synthesize the preview from
                // them and skip the `list_chunks_by_revision` fetch
                // entirely. The separate `get_revision` call is kept
                // so the source_uri still reaches the prompt.
                let source_uri = load_retrieved_document_source_uri(state, &document).await;
                let preview = focused_preview_ref.cloned().or(fallback_excerpt).unwrap_or_default();
                (preview, source_uri)
            } else {
                let (preview, source_uri) =
                    load_retrieved_document_preview_and_source(state, &document)
                        .await
                        .unwrap_or((None, None));
                let preview = preview.or(fallback_excerpt).unwrap_or_default();
                (preview, source_uri)
            };
            if preview_excerpt.trim().is_empty() {
                return None;
            }
            let title = document
                .title
                .clone()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| document.external_key.clone());
            Some(RuntimeRetrievedDocumentBrief { title, preview_excerpt, source_uri })
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
        .take(3)
        .collect::<Vec<_>>()
        .join(" ");
    (!joined.is_empty()).then(|| excerpt_for(&joined, 240))
}

async fn load_retrieved_document_source_uri(
    state: &AppState,
    document: &KnowledgeDocumentRow,
) -> Option<String> {
    let revision_id = document.readable_revision_id.or(document.active_revision_id)?;
    let revision = state.arango_document_store.get_revision(revision_id).await.ok()??;
    revision.source_uri.map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

async fn load_retrieved_document_preview_and_source(
    state: &AppState,
    document: &KnowledgeDocumentRow,
) -> Option<(Option<String>, Option<String>)> {
    // `source_uri` is stored on the revision row, not on the
    // document root — a document can have many revisions over its
    // lifetime and each carries the provenance of *that* upload
    // (URL for web-ingested pages, storage reference for files).
    // We read the readable revision first (what the user would see
    // today); the active revision is the fallback while a newer
    // ingest run is still processing but has not landed yet.
    let revision_id = document.readable_revision_id.or(document.active_revision_id)?;

    let revision_future = state.arango_document_store.get_revision(revision_id);
    let chunks_future = state.arango_document_store.list_chunks_by_revision(revision_id);
    let (revision_result, chunks_result) =
        futures::future::join(revision_future, chunks_future).await;

    let source_uri = revision_result
        .ok()
        .flatten()
        .and_then(|revision| revision.source_uri)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

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
        .take(3)
        .collect::<Vec<_>>()
        .join(" ");

    let preview = (!combined.is_empty()).then(|| excerpt_for(&combined, 240));

    Some((preview, source_uri))
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
mod tests {
    use crate::domains::query_ir::{
        EntityMention, EntityRole, QueryAct, QueryLanguage, QueryScope, SourceSliceSpec,
    };

    use super::*;

    fn source_slice_ir(direction: SourceSliceDirection, count: u16) -> QueryIR {
        QueryIR {
            act: QueryAct::Enumerate,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec!["record".to_string()],
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: Some(SourceSliceSpec { direction, count: Some(count) }),
            confidence: 0.9,
        }
    }

    fn general_ir() -> QueryIR {
        QueryIR {
            act: QueryAct::Enumerate,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec!["record".to_string()],
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            confidence: 0.9,
        }
    }

    fn entity_ir() -> QueryIR {
        QueryIR {
            act: QueryAct::Describe,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec!["person".to_string()],
            target_entities: vec![EntityMention {
                label: "Project Omega".to_string(),
                role: EntityRole::Subject,
            }],
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            confidence: 0.9,
        }
    }

    fn source_slice_unit(ordinal: i32, source_text: &str) -> RuntimeMatchedChunk {
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: ordinal,
            chunk_kind: Some("metadata_block".to_string()),
            document_label: "records.jsonl".to_string(),
            excerpt: source_text.to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(3.0),
            source_text: source_text.to_string(),
        }
    }

    fn source_profile(source_text: &str) -> RuntimeMatchedChunk {
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: Some("source_profile".to_string()),
            document_label: "records.jsonl".to_string(),
            excerpt: "[source_profile source_format=record_jsonl unit_count=2]".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(4.0),
            source_text: source_text.to_string(),
        }
    }

    fn ordinary_chunk(excerpt: &str, source_text: &str) -> RuntimeMatchedChunk {
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 1,
            chunk_kind: Some("paragraph".to_string()),
            document_label: "guide.md".to_string(),
            excerpt: excerpt.to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(1.0),
            source_text: source_text.to_string(),
        }
    }

    #[test]
    fn source_slice_context_renders_ordered_units_not_chunks() {
        let query_ir = source_slice_ir(SourceSliceDirection::Tail, 2);
        let chunks = vec![
            source_slice_unit(2, "[unit_id=u-2] second"),
            source_slice_unit(3, "[unit_id=u-3] third"),
        ];

        let context = assemble_bounded_context_for_query(
            &query_ir,
            "show latest records",
            &[],
            &[],
            &chunks,
            &[],
            4096,
        );

        assert!(context.contains("SOURCE_SLICE_UNIT"));
        assert!(context.contains("returned_unit_count: 2"));
        assert!(!context.contains("SOURCE_SLICE_CHUNK"));
        assert!(context.find("u-2").unwrap() < context.find("u-3").unwrap());
    }

    #[test]
    fn source_slice_context_does_not_leak_profile_sample_units() {
        let query_ir = source_slice_ir(SourceSliceDirection::Tail, 1);
        let chunks = vec![
            source_profile(
                "[source_profile source_format=record_jsonl unit_count=2]\n[unit_id=old] old sample",
            ),
            source_slice_unit(2, "[unit_id=u-2] latest unit"),
        ];

        let context = assemble_bounded_context_for_query(
            &query_ir,
            "show latest record",
            &[],
            &[],
            &chunks,
            &[],
            4096,
        );

        assert!(context.contains("[source_profile source_format=record_jsonl unit_count=2]"));
        assert!(context.contains("[unit_id=u-2] latest unit"));
        assert!(!context.contains("[unit_id=old] old sample"));
    }

    #[test]
    fn bounded_context_ranks_source_units_by_question_focus_and_renders_source_text() {
        let query_ir = general_ir();
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let unrelated = RuntimeMatchedChunk {
            document_id,
            revision_id,
            ..source_slice_unit(
                194,
                "[unit_id=later]\n44. video outline\n45. lesson plan\n46. music prompt",
            )
        };
        let correct_body = format!(
            "[unit_id=scripts]\n{}\n10. status report generator for ArcadeEditor beginners",
            "1. ArcadeEditor calculator script for beginners. ".repeat(12)
        );
        let correct = RuntimeMatchedChunk {
            document_id,
            revision_id,
            excerpt: excerpt_for(&correct_body, 120),
            ..source_slice_unit(6, &correct_body)
        };
        let chunks = vec![unrelated, correct];

        let context = assemble_bounded_context_for_query(
            &query_ir,
            "simple ArcadeEditor scripts for beginners",
            &[],
            &[],
            &chunks,
            &[],
            8192,
        );

        assert!(context.find("unit_id=scripts").unwrap() < context.find("unit_id=later").unwrap());
        assert!(context.contains("10. status report generator"));
    }

    #[test]
    fn bounded_context_keeps_ordinary_chunks_on_excerpt_text() {
        let context = assemble_bounded_context(
            &[],
            &[],
            &[ordinary_chunk("short excerpt", "short excerpt plus hidden source body")],
            4096,
        );

        assert!(context.contains("short excerpt"));
        assert!(!context.contains("hidden source body"));
    }

    #[test]
    fn bounded_context_keeps_source_context_block_text() {
        let mut chunk = ordinary_chunk(
            "Alpha Devices: Device A",
            &format!(
                "{}\nAlpha Devices:\n- Device A\n- Device B\n- Device C\n- Device D",
                "preface ".repeat(160)
            ),
        );
        chunk.score_kind = RuntimeChunkScoreKind::SourceContext;

        let context = assemble_bounded_context_for_query(
            &general_ir(),
            "Which Alpha Devices are listed?",
            &[],
            &[],
            &[chunk],
            &[],
            8192,
        );

        assert!(context.contains("[document source_context"));
        assert!(context.contains("Device A"));
        assert!(context.contains("Device D"));
    }

    #[test]
    fn entity_target_context_prioritizes_graph_lines_before_documents() {
        let context = assemble_bounded_context_for_query(
            &entity_ir(),
            "Project Omega",
            &[
                RuntimeMatchedEntity {
                    node_id: Uuid::now_v7(),
                    label: "Project Omega".to_string(),
                    node_type: "person".to_string(),
                    score: Some(0.9),
                },
                RuntimeMatchedEntity {
                    node_id: Uuid::now_v7(),
                    label: "Project Omega Peer".to_string(),
                    node_type: "person".to_string(),
                    score: Some(0.8),
                },
            ],
            &[],
            &[ordinary_chunk(
                "Project Omega appears in a long planning note.",
                "Project Omega appears in a long planning note.",
            )],
            &[],
            4096,
        );

        let graph_index = context.find("[graph-node]").unwrap_or_default();
        let second_graph_index = context.find("Project Omega Peer").unwrap_or_default();
        let document_index = context.find("[document]").unwrap_or_default();
        assert!(graph_index < document_index);
        assert!(second_graph_index < document_index);
    }

    #[test]
    fn entity_target_context_keeps_unanchored_graph_evidence_before_documents() {
        let graph_evidence_lines = vec![
            "[graph-evidence target=\"Project Omega\"]\nProject Omega has a rare one-row fact."
                .to_string(),
        ];
        let context = assemble_bounded_context_for_query(
            &entity_ir(),
            "Project Omega",
            &[],
            &[],
            &[ordinary_chunk(
                "Project Omega appears in a long planning note.",
                "Project Omega appears in a long planning note.",
            )],
            &graph_evidence_lines,
            4096,
        );

        let evidence_index = context.find("[graph-evidence").unwrap();
        let document_index = context.find("[document]").unwrap();
        assert!(evidence_index < document_index);
        assert!(context.contains("rare one-row fact"));
    }

    #[test]
    fn entity_target_context_marks_exact_and_token_overlap_matches() {
        let context = assemble_bounded_context_for_query(
            &entity_ir(),
            "Project Omega",
            &[
                RuntimeMatchedEntity {
                    node_id: Uuid::now_v7(),
                    label: "Project Omega".to_string(),
                    node_type: "person".to_string(),
                    score: Some(0.9),
                },
                RuntimeMatchedEntity {
                    node_id: Uuid::now_v7(),
                    label: "Omega Delta".to_string(),
                    node_type: "person".to_string(),
                    score: Some(0.8),
                },
                RuntimeMatchedEntity {
                    node_id: Uuid::now_v7(),
                    label: "Project Alpha".to_string(),
                    node_type: "person".to_string(),
                    score: Some(0.7),
                },
                RuntimeMatchedEntity {
                    node_id: Uuid::now_v7(),
                    label: "Project Beta".to_string(),
                    node_type: "person".to_string(),
                    score: Some(0.6),
                },
                RuntimeMatchedEntity {
                    node_id: Uuid::now_v7(),
                    label: "Unrelated Sigma".to_string(),
                    node_type: "person".to_string(),
                    score: Some(0.1),
                },
            ],
            &[],
            &[ordinary_chunk(
                "Project Omega appears in a long planning note.",
                "Project Omega appears in a long planning note.",
            )],
            &[],
            4096,
        );

        let exact_index = context.find("[entity-match exact] Project Omega").unwrap();
        let related_index = context.find("[entity-match token-overlap] Omega Delta").unwrap();
        let graph_index = context.find("[graph-node]").unwrap();
        assert!(exact_index < graph_index);
        assert!(related_index < graph_index);
        assert!(!context.contains("[entity-match token-overlap] Project Alpha"));
        assert!(!context.contains("[entity-match token-overlap] Project Beta"));
        assert!(!context.contains("[entity-match token-overlap] Unrelated Sigma"));
    }

    #[test]
    fn entity_target_context_rejects_embedded_short_exact_match() {
        let mut ir = entity_ir();
        ir.target_entities[0].label = "Sasha Otoya".to_string();
        let context = assemble_bounded_context_for_query(
            &ir,
            "Sasha Otoya",
            &[
                RuntimeMatchedEntity {
                    node_id: Uuid::now_v7(),
                    label: "OTO".to_string(),
                    node_type: "organization".to_string(),
                    score: Some(0.9),
                },
                RuntimeMatchedEntity {
                    node_id: Uuid::now_v7(),
                    label: "Alex Otoya".to_string(),
                    node_type: "person".to_string(),
                    score: Some(0.8),
                },
            ],
            &[],
            &[ordinary_chunk("Sasha Otoya is mentioned once.", "Sasha Otoya is mentioned once.")],
            &[],
            4096,
        );

        assert!(!context.contains("[entity-match exact] OTO"));
        assert!(context.contains("[entity-match token-overlap] Alex Otoya"));
    }

    #[test]
    fn bounded_context_renders_query_focused_source_text_for_ordinary_chunks() {
        let hidden_rules = "retail_clock rules: register once at start and once at finish.";
        let source_text = format!(
            "{}\n{}",
            "introductory material without the requested rule. ".repeat(20),
            hidden_rules
        );
        let context = assemble_bounded_context_for_query(
            &general_ir(),
            "what are the retail_clock rules?",
            &[],
            &[],
            &[ordinary_chunk("introductory material without details", &source_text)],
            &[],
            4096,
        );

        assert!(context.contains(hidden_rules));
    }
}
