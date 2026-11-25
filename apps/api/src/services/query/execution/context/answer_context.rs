use std::collections::HashMap;

use futures::future::join_all;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::{
        knowledge_rows::{KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeRevisionRow},
        repositories::catalog_repository,
    },
    services::content::document_hint::resolve_document_hint,
};

use super::super::{
    chunk_support::{chunk_answer_source_text, document_has_exact_readable_revision},
    retrieve::{excerpt_for, score_value},
    types::{RuntimeMatchedChunk, RuntimeQueryLibrarySummary, RuntimeRetrievedDocumentBrief},
};

const RETRIEVED_DOCUMENT_BRIEF_PREVIEW_CHARS: usize = 520;
const RETRIEVED_DOCUMENT_BRIEF_SOURCE_CHUNKS: usize = 3;

pub(crate) fn assemble_answer_context(
    summary: &RuntimeQueryLibrarySummary,
    retrieved_documents: &[RuntimeRetrievedDocumentBrief],
    technical_literals_text: Option<&str>,
    retrieved_context: &str,
    prioritize_retrieved_context: bool,
) -> String {
    // Authoritative answer prompt is a deterministic function of
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
    let span_started = std::time::Instant::now();
    let brief_limit = top_k.clamp(16, 48);
    let candidates =
        select_brief_candidates(chunks, document_index, brief_limit, focused_document_id);
    let ids = candidates
        .iter()
        .flat_map(|(_, chunks)| chunks.iter().map(|chunk| chunk.chunk_id))
        .collect::<Vec<_>>();
    let rows = if ids.is_empty() {
        Vec::new()
    } else {
        match state.document_store.list_chunks_by_ids(&ids).await {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, "failed to load canonical document brief chunks");
                return Vec::new();
            }
        }
    };
    let briefs = join_all(candidates.into_iter().map(|(document, selected)| {
        let rows = &rows;
        async move {
            let current = state.document_store.get_document(document.document_id).await.ok()??;
            if Some(current.document_id) != focused_document_id
                && crate::domains::content::role_is_attached_context(&current.document_role)
            {
                return None;
            }
            let preview_excerpt = canonical_brief_preview(document, &current, &selected, rows)?;
            let document_hint = load_retrieved_document_hint(state, &current).await;
            let title = current
                .title
                .clone()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| current.external_key.clone());
            Some(RuntimeRetrievedDocumentBrief { title, preview_excerpt, document_hint })
        }
    }))
    .await
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();

    crate::services::query::turn_spans::record_span(
        "context.document_briefs",
        "db",
        span_started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        None,
        Some(briefs.len() as u64),
    );
    briefs
}

pub(super) fn select_brief_candidates<'a>(
    chunks: &'a [RuntimeMatchedChunk],
    document_index: &'a HashMap<Uuid, KnowledgeDocumentRow>,
    brief_limit: usize,
    focused_document_id: Option<Uuid>,
) -> Vec<(&'a KnowledgeDocumentRow, Vec<&'a RuntimeMatchedChunk>)> {
    let mut by_document = HashMap::<Uuid, Vec<&RuntimeMatchedChunk>>::new();
    let mut ordered_document_ids = Vec::new();
    for chunk in chunks {
        let Some(document) = document_index.get(&chunk.document_id) else { continue };
        if !document_has_exact_readable_revision(document, chunk.revision_id)
            || (Some(chunk.document_id) != focused_document_id
                && crate::domains::content::role_is_attached_context(&document.document_role))
        {
            continue;
        }
        let entries = by_document.entry(chunk.document_id).or_insert_with(|| {
            ordered_document_ids.push(chunk.document_id);
            Vec::new()
        });
        entries.push(chunk);
    }
    ordered_document_ids
        .into_iter()
        .take(brief_limit)
        .filter_map(|document_id| {
            let document = document_index.get(&document_id)?;
            let mut document_chunks = by_document.remove(&document_id)?;
            if Some(document_id) == focused_document_id {
                document_chunks.sort_by_key(|chunk| chunk.chunk_index);
                document_chunks.truncate(RETRIEVED_DOCUMENT_BRIEF_SOURCE_CHUNKS);
            } else {
                let best = document_chunks.into_iter().reduce(|best, chunk| {
                    if score_value(chunk.score) > score_value(best.score) { chunk } else { best }
                })?;
                document_chunks = vec![best];
            }
            Some((document, document_chunks))
        })
        .collect()
}

pub(super) fn canonical_brief_preview(
    indexed_document: &KnowledgeDocumentRow,
    current_document: &KnowledgeDocumentRow,
    selected: &[&RuntimeMatchedChunk],
    rows: &[KnowledgeChunkRow],
) -> Option<String> {
    if current_document.document_id != indexed_document.document_id
        || current_document.workspace_id != indexed_document.workspace_id
        || current_document.library_id != indexed_document.library_id
        || selected.is_empty()
    {
        return None;
    }
    let mut parts = Vec::new();
    for chunk in selected {
        if chunk.document_id != current_document.document_id
            || !document_has_exact_readable_revision(current_document, chunk.revision_id)
        {
            return None;
        }
        let mut matches = rows.iter().filter(|row| row.chunk_id == chunk.chunk_id);
        let row = matches.next()?;
        if matches.next().is_some()
            || row.document_id != chunk.document_id
            || row.revision_id != chunk.revision_id
            || row.chunk_index != chunk.chunk_index
            || row.workspace_id != current_document.workspace_id
            || row.library_id != current_document.library_id
            || row.chunk_state != "ready"
        {
            return None;
        }
        let text = chunk_answer_source_text(row);
        if !text.trim().is_empty() {
            parts.push(text);
        }
    }
    let joined = parts.join(" ");
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

async fn resolve_retrieved_document_hint(
    state: &AppState,
    document: &KnowledgeDocumentRow,
    knowledge_revision: Option<&KnowledgeRevisionRow>,
) -> Option<String> {
    let library_setting = match catalog_repository::get_library_by_id(
        &state.persistence.postgres,
        document.library_id,
    )
    .await
    {
        Ok(Some(library)) => library.include_document_hint_in_mcp_answers,
        Ok(None) => true,
        Err(error) => {
            tracing::warn!(
                ?error,
                library_id = %document.library_id,
                "failed to load library include_document_hint_in_mcp_answers setting — hiding document hint"
            );
            false
        }
    };

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
