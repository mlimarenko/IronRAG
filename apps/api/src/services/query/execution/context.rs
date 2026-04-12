#![allow(dead_code, unused_imports, unused_variables, unused_mut)]
use std::collections::HashMap;

use anyhow::Context;
use futures::future::join_all;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        content::ContentDocumentSummary,
        query::{GroupedReferenceKind, RuntimeQueryMode},
    },
    infra::arangodb::document_store::KnowledgeDocumentRow,
    services::query::support::{
        ContextAssemblyRequest, GroupedReferenceCandidate, assemble_context_metadata,
        group_visible_references,
    },
    shared::extraction::text_render::repair_technical_layout_noise,
};

use super::retrieve::{
    excerpt_for, load_latest_library_generation, query_graph_status, score_value,
};
use super::types::*;

pub(crate) fn assemble_bounded_context(
    entities: &[RuntimeMatchedEntity],
    relationships: &[RuntimeMatchedRelationship],
    chunks: &[RuntimeMatchedChunk],
    budget_chars: usize,
) -> String {
    let mut graph_lines = entities
        .iter()
        .map(|entity| format!("[graph-node] {} ({})", entity.label, entity.node_type))
        .collect::<Vec<_>>();
    graph_lines.extend(relationships.iter().map(|edge| {
        format!("[graph-edge] {} --{}--> {}", edge.from_label, edge.relation_type, edge.to_label)
    }));
    let document_lines = chunks
        .iter()
        .map(|chunk| format!("[document] {}: {}", chunk.document_label, chunk.excerpt))
        .collect::<Vec<_>>();

    let mut sections = Vec::new();
    let mut used = 0usize;
    let mut graph_index = 0usize;
    let mut document_index = 0usize;
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
            excerpt: Some(format!(
                "{} {} {}",
                relationship.from_label, relationship.relation_type, relationship.to_label
            )),
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
            title: format!(
                "{} {} {}",
                relationship.from_label, relationship.relation_type, relationship.to_label
            ),
            excerpt: Some(format!(
                "{} --{}--> {}",
                relationship.from_label, relationship.relation_type, relationship.to_label
            )),
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
            graph_node_count: graph_index.nodes.len(),
            graph_edge_count: graph_index.edges.len(),
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
    let documents = state
        .canonical_services
        .content
        .list_documents(state, library_id)
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))
        .context("failed to load canonical document summaries for query readiness")?;
    let backlog_count = runtime_query_backlog_count(&documents);
    let convergence_status = query_execution_convergence_status(graph_status, backlog_count);
    Ok(RuntimeQueryLibraryContext {
        summary: summarize_query_library(graph_status, &documents),
        recent_documents: summarize_recent_query_documents(state, &documents, 12).await,
        warning: query_execution_convergence_warning(state, convergence_status, backlog_count),
    })
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

fn summarize_query_library(
    graph_status: &'static str,
    documents: &[ContentDocumentSummary],
) -> RuntimeQueryLibrarySummary {
    let mut graph_ready_count = 0usize;
    let mut processing_count = 0usize;
    let mut failed_count = 0usize;

    for summary in documents {
        if document_has_query_failure(summary) {
            failed_count += 1;
            continue;
        }
        if document_requires_query_backlog(summary) {
            processing_count += 1;
        }
        if summary.readiness.as_ref().is_some_and(|readiness| readiness.graph_state == "ready") {
            graph_ready_count += 1;
        }
    }

    RuntimeQueryLibrarySummary {
        document_count: documents.len(),
        graph_ready_count,
        processing_count,
        failed_count,
        graph_status,
    }
}

async fn summarize_recent_query_documents(
    state: &AppState,
    documents: &[ContentDocumentSummary],
    limit: usize,
) -> Vec<RuntimeQueryRecentDocument> {
    let mut ranked_documents = documents.iter().collect::<Vec<_>>();
    ranked_documents.sort_by(|left, right| {
        query_prompt_document_uploaded_at(right)
            .cmp(&query_prompt_document_uploaded_at(left))
            .then_with(|| {
                query_prompt_document_title(left).cmp(&query_prompt_document_title(right))
            })
    });
    ranked_documents.truncate(limit);

    let previews = join_all(
        ranked_documents.iter().map(|summary| load_query_prompt_document_preview(state, summary)),
    )
    .await;

    ranked_documents
        .into_iter()
        .zip(previews)
        .map(|(summary, preview_excerpt)| RuntimeQueryRecentDocument {
            title: query_prompt_document_title(summary),
            uploaded_at: query_prompt_document_uploaded_at(summary).to_rfc3339(),
            mime_type: summary.active_revision.as_ref().map(|revision| revision.mime_type.clone()),
            pipeline_state: query_prompt_pipeline_state(summary),
            graph_state: query_prompt_graph_state(summary),
            preview_excerpt,
        })
        .collect()
}

pub(crate) fn assemble_answer_context(
    summary: &RuntimeQueryLibrarySummary,
    recent_documents: &[RuntimeQueryRecentDocument],
    retrieved_documents: &[RuntimeRetrievedDocumentBrief],
    technical_literals_text: Option<&str>,
    retrieved_context: &str,
) -> String {
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
    if !recent_documents.is_empty() {
        let recent_lines = recent_documents
            .iter()
            .map(|document| {
                let metadata = match document.mime_type.as_deref() {
                    Some(mime_type) => format!(
                        "{}; pipeline {}; graph {}",
                        mime_type, document.pipeline_state, document.graph_state
                    ),
                    None => format!(
                        "pipeline {}; graph {}",
                        document.pipeline_state, document.graph_state
                    ),
                };
                let mut line =
                    format!("- {} — {} ({metadata})", document.uploaded_at, document.title);
                if let Some(preview_excerpt) = document.preview_excerpt.as_deref() {
                    line.push_str(&format!("\n  Preview: {preview_excerpt}"));
                }
                line
            })
            .collect::<Vec<_>>();
        sections.push(format!("Recent documents\n{}", recent_lines.join("\n")));
    }
    if !retrieved_documents.is_empty() {
        let retrieved_lines = retrieved_documents
            .iter()
            .map(|document| format!("- {}: {}", document.title, document.preview_excerpt))
            .collect::<Vec<_>>();
        sections.push(format!("Retrieved document briefs\n{}", retrieved_lines.join("\n")));
    }
    if let Some(technical_literals_text) = technical_literals_text
        && !technical_literals_text.trim().is_empty()
    {
        sections.push(technical_literals_text.trim().to_string());
    }
    let trimmed_context = retrieved_context.trim();
    if trimmed_context.is_empty() {
        return sections.join("\n\n");
    }
    sections.push(trimmed_context.to_string());
    sections.join("\n\n")
}

fn query_graph_status_prompt_label(graph_status: &str) -> &'static str {
    match graph_status {
        "current" => "current (all documents processed)",
        "partial" => "partial (some documents still processing)",
        _ => "empty (no graph data yet)",
    }
}

pub(crate) fn runtime_query_backlog_count(documents: &[ContentDocumentSummary]) -> i64 {
    documents.iter().filter(|summary| document_requires_query_backlog(summary)).count() as i64
}

pub(crate) fn document_requires_query_backlog(summary: &ContentDocumentSummary) -> bool {
    let latest_mutation = summary.pipeline.latest_mutation.as_ref();
    let latest_job = summary.pipeline.latest_job.as_ref();

    let mutation_inflight = latest_mutation
        .is_some_and(|mutation| matches!(mutation.mutation_state.as_str(), "accepted" | "running"));
    let job_inflight =
        latest_job.is_some_and(|job| matches!(job.queue_state.as_str(), "queued" | "running"));
    let graph_pending =
        summary.readiness.as_ref().is_some_and(|readiness| readiness.graph_state != "ready")
            && !document_has_query_failure(summary);

    mutation_inflight || job_inflight || graph_pending
}

pub(crate) fn document_has_query_failure(summary: &ContentDocumentSummary) -> bool {
    let latest_mutation = summary.pipeline.latest_mutation.as_ref();
    let latest_job = summary.pipeline.latest_job.as_ref();

    latest_mutation.is_some_and(|mutation| mutation.mutation_state == "failed")
        || latest_job
            .is_some_and(|job| matches!(job.queue_state.as_str(), "failed" | "retryable_failed"))
}

fn query_prompt_document_title(summary: &ContentDocumentSummary) -> String {
    summary
        .active_revision
        .as_ref()
        .and_then(|revision| revision.title.as_deref())
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| summary.document.external_key.clone())
}

fn query_prompt_document_uploaded_at(
    summary: &ContentDocumentSummary,
) -> chrono::DateTime<chrono::Utc> {
    summary
        .active_revision
        .as_ref()
        .map(|revision| revision.created_at)
        .unwrap_or(summary.document.created_at)
}

fn query_prompt_pipeline_state(summary: &ContentDocumentSummary) -> &'static str {
    if document_has_query_failure(summary) {
        return "failed";
    }
    if document_requires_query_backlog(summary) {
        return "processing";
    }
    "ready"
}

fn query_prompt_graph_state(summary: &ContentDocumentSummary) -> &'static str {
    match summary.readiness.as_ref().map(|readiness| readiness.graph_state.as_str()) {
        Some("ready") => "ready",
        Some("failed") => "failed",
        Some("queued" | "running") => "processing",
        Some(_) => "partial",
        None => "unknown",
    }
}

pub(crate) async fn load_retrieved_document_briefs(
    state: &AppState,
    chunks: &[RuntimeMatchedChunk],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    top_k: usize,
) -> Vec<RuntimeRetrievedDocumentBrief> {
    let brief_limit = top_k.clamp(16, 48);
    let mut best_by_document = HashMap::<Uuid, RuntimeMatchedChunk>::new();
    let mut ordered_document_ids = Vec::<Uuid>::new();

    for chunk in chunks {
        let entry = best_by_document.entry(chunk.document_id).or_insert_with(|| {
            ordered_document_ids.push(chunk.document_id);
            chunk.clone()
        });
        if score_value(chunk.score) > score_value(entry.score) {
            *entry = chunk.clone();
        }
    }

    let ranked_documents = ordered_document_ids
        .into_iter()
        .take(brief_limit)
        .filter_map(|document_id| {
            let document = document_index.get(&document_id)?.clone();
            let fallback_excerpt =
                best_by_document.get(&document_id).map(|chunk| chunk.excerpt.clone());
            Some((document, fallback_excerpt))
        })
        .collect::<Vec<_>>();

    let previews =
        join_all(ranked_documents.into_iter().map(|(document, fallback_excerpt)| async move {
            let preview_excerpt = load_retrieved_document_preview(state, &document)
                .await
                .or(fallback_excerpt)
                .unwrap_or_default();
            if preview_excerpt.trim().is_empty() {
                return None;
            }
            let title = document
                .title
                .clone()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| document.external_key.clone());
            Some(RuntimeRetrievedDocumentBrief { title, preview_excerpt })
        }))
        .await;

    previews.into_iter().flatten().collect()
}

async fn load_query_prompt_document_preview(
    state: &AppState,
    summary: &ContentDocumentSummary,
) -> Option<String> {
    let revision_id = summary.active_revision.as_ref()?.id;
    let chunks = state.canonical_services.content.list_chunks(state, revision_id).await.ok()?;
    chunks.into_iter().find_map(|chunk| {
        let repaired = repair_technical_layout_noise(&chunk.normalized_text);
        let normalized = repaired.trim();
        if normalized.is_empty() {
            return None;
        }
        Some(excerpt_for(normalized, 180))
    })
}

async fn load_retrieved_document_preview(
    state: &AppState,
    document: &KnowledgeDocumentRow,
) -> Option<String> {
    let revision_id = document.readable_revision_id.or(document.active_revision_id)?;
    let chunks = state.arango_document_store.list_chunks_by_revision(revision_id).await.ok()?;
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
    if combined.is_empty() {
        return None;
    }
    Some(excerpt_for(&combined, 240))
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
