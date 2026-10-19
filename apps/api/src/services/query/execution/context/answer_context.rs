use std::collections::HashMap;

use futures::future::join_all;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::{
        knowledge_rows::{KnowledgeDocumentRow, KnowledgeRevisionRow},
        repositories::catalog_repository,
    },
    services::content::document_hint::resolve_document_hint,
    shared::extraction::text_render::repair_technical_layout_noise,
};

use super::super::{
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

    let briefs: Vec<_> = previews.into_iter().flatten().collect();
    crate::services::query::turn_spans::record_span(
        "context.document_briefs",
        "db",
        span_started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        None,
        Some(briefs.len() as u64),
    );
    briefs
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
pub(super) fn focused_preview_from_bundle_chunks(
    chunks: &[&RuntimeMatchedChunk],
) -> Option<String> {
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
