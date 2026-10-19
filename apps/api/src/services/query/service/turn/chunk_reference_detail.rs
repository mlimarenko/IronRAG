use super::*;
use crate::domains::query::{QueryChunkReference, VISIBLE_REFERENCE_LIMIT};
use std::collections::BTreeSet;

/// How much of a chunk's text the sources panel carries. Enough to recognise
/// the passage, short enough that a hundred references do not become a page of
/// prose in the response.
const EXCERPT_MAX_CHARS: usize = 320;

fn excerpt_of(content_text: &str) -> Option<String> {
    let trimmed = content_text.split_whitespace().collect::<Vec<_>>().join(" ");
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.chars().count() <= EXCERPT_MAX_CHARS {
        return Some(trimmed);
    }
    let cut = trimmed.char_indices().nth(EXCERPT_MAX_CHARS).map_or(trimmed.len(), |(at, _)| at);
    Some(format!("{}…", trimmed[..cut].trim_end()))
}

/// Fills in what a chunk reference needs to read as a source: its document, the
/// headings above it, and the opening of its own text.
///
/// The stored reference is a chunk id, a rank and a score, which is all the
/// answer pipeline needs and nothing a reader can use — the sources panel
/// listed rows saying "#1" with a six-figure internal score next to them. The
/// chunk rows are still in the library, so this reads them back at display time
/// rather than widening what every answer persists.
///
/// A chunk whose document has since been deleted keeps its rank and simply
/// carries no detail.
pub(crate) async fn enrich_chunk_references(
    state: &AppState,
    references: &mut [QueryChunkReference],
) {
    if references.is_empty() {
        return;
    }
    // Only what a caller will actually be shown. An execution can carry
    // hundreds of references while the response projects `VISIBLE_REFERENCE_LIMIT`
    // of them, and reading chunk bodies for the rest would put that cost on
    // every answer and every history page for text nobody sees.
    let mut wanted = references.iter().collect::<Vec<_>>();
    wanted.sort_by_key(|reference| reference.rank);
    let chunk_ids = wanted
        .into_iter()
        .take(VISIBLE_REFERENCE_LIMIT)
        .map(|reference| reference.chunk_id)
        .collect::<Vec<_>>();
    let Ok(chunks) = state.document_store.list_chunks_by_ids(&chunk_ids).await else {
        return;
    };
    let by_chunk =
        chunks.into_iter().map(|chunk| (chunk.chunk_id, chunk)).collect::<HashMap<_, _>>();
    let document_ids = by_chunk
        .values()
        .map(|chunk| chunk.document_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let titles = state
        .document_store
        .list_documents_by_ids(&document_ids)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|document| (document.document_id, document.title))
        .collect::<HashMap<_, _>>();
    for reference in references.iter_mut() {
        let Some(chunk) = by_chunk.get(&reference.chunk_id) else {
            continue;
        };
        reference.document_id = Some(chunk.document_id);
        reference.document_title = titles.get(&chunk.document_id).cloned().flatten();
        // The heading trail is the reading path a person recognises; the
        // section path is the structural one. Prefer the first where both exist.
        reference.section_path = if chunk.heading_trail.is_empty() {
            chunk.section_path.clone()
        } else {
            chunk.heading_trail.clone()
        };
        reference.excerpt = excerpt_of(&chunk.content_text);
    }
}
