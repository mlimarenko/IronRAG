use std::collections::HashMap;

use anyhow::Context;
use futures::future::join_all;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::query::{GroupedReferenceKind, RuntimeQueryMode},
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
                    (Some("failed"), _) | (_, Some("failed" | "conflicted" | "canceled")) => {
                        "failed"
                    }
                    (Some("leased"), _) => "processing",
                    (Some("queued"), _) | (_, Some("accepted" | "running")) => "queued",
                    _ if row.readable_revision_id.is_some() => "ready",
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
    // first N of them in reading order. Non-focused documents keep
    // the legacy "best-scored chunk excerpt" fallback.
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
