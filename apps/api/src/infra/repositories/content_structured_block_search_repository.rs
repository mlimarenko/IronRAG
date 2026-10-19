use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub(crate) struct ContentStructuredBlockSearchRow {
    pub document_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub external_key: String,
    pub readable_revision_id: Uuid,
    pub document_title: Option<String>,
    pub source_text: String,
    pub span_start: Option<i32>,
    pub text_score: f64,
}

const STRUCTURED_BLOCK_SEARCH_SQL: &str = r"with search_query as materialized (
        select plainto_tsquery('simple'::regconfig, ironrag_unaccent($2)) as value
    ), matching_blocks as materialized (
      select
        block.block_id,
        document.document_id,
        document.workspace_id,
        document.library_id,
        document.external_key,
        block.revision_id as readable_revision_id,
        document.title as document_title,
        block.ordinal,
        block.span_start,
        ts_rank_cd(block.search_tsv, search_query.value)::double precision as text_score
      from knowledge_structured_block as block
      join content_current_revision current_revision
        on current_revision.document_id = block.document_id
       and current_revision.revision_id = block.revision_id
      join knowledge_document as document
        on document.document_id = block.document_id
       and document.workspace_id = block.workspace_id
       and document.library_id = block.library_id
       and document.document_state = 'active'
       and document.deleted_at is null
      join knowledge_structured_revision as structured_revision
        on structured_revision.revision_id = block.revision_id
       and structured_revision.document_id = block.document_id
       and structured_revision.workspace_id = block.workspace_id
       and structured_revision.library_id = block.library_id
       and structured_revision.preparation_state = 'prepared'
      cross join search_query
      where block.library_id = $1
        and block.search_tsv @@ search_query.value
        and not exists (
            select 1
            from knowledge_chunk as chunk
            where chunk.revision_id = block.revision_id
              and chunk.document_id = block.document_id
              and chunk.workspace_id = block.workspace_id
              and chunk.library_id = block.library_id
              and chunk.chunk_state = 'ready'
        )
    ), best_per_document as materialized (
      select distinct on (document_id) *
      from matching_blocks
      order by document_id asc, text_score desc, ordinal asc, block_id asc
    ), top_documents as materialized (
      select *
      from best_per_document
      order by text_score desc, document_id asc
      limit $3
    )
    select
      top_documents.document_id,
      top_documents.workspace_id,
      top_documents.library_id,
      top_documents.external_key,
      top_documents.readable_revision_id,
      top_documents.document_title,
      block.text as source_text,
      top_documents.span_start,
      top_documents.text_score
    from top_documents
    join knowledge_structured_block as block
      on block.block_id = top_documents.block_id
    order by top_documents.text_score desc, top_documents.document_id asc";

/// Selects one best original-text block per exact readable document head that
/// has not reached the ready chunk lexical lane yet.
///
/// The persisted `search_tsv` GIN lane performs matching and document-level
/// top-k selection before the chosen blocks' source text is fetched. This
/// prevents one fragmented document from exhausting the candidate limit and
/// preserves source glyphs for the Unicode-aware preview builder. A ready
/// chunk hands the revision off to the ordinary lexical lane, while vector or
/// graph readiness does not affect text discoverability.
pub(crate) async fn search_readable_structured_blocks_without_ready_chunks(
    postgres: &PgPool,
    library_id: Uuid,
    query: &str,
    limit: u32,
) -> Result<Vec<ContentStructuredBlockSearchRow>, sqlx::Error> {
    if query.trim().is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, ContentStructuredBlockSearchRow>(STRUCTURED_BLOCK_SEARCH_SQL)
        .bind(library_id)
        .bind(query.trim())
        .bind(i64::from(limit))
        .fetch_all(postgres)
        .await
}

#[cfg(test)]
#[path = "content_structured_block_search_repository_tests.rs"]
mod tests;
