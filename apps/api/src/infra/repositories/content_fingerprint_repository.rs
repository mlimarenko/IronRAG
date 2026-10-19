use sqlx::PgPool;
use uuid::Uuid;

/// Resolves the only revision ordinary readers may expose. Authorization is
/// deliberately performed by the caller, independently of publication.
pub async fn get_current_revision_id(
    postgres: &PgPool,
    document_id: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar("select revision_id from content_current_revision where document_id = $1")
        .bind(document_id)
        .fetch_optional(postgres)
        .await
}

/// Revalidates an exact revision immediately before returning stored content.
pub async fn get_current_source_file_name(
    postgres: &PgPool,
    document_id: Uuid,
    revision_id: Uuid,
) -> Result<Option<String>, sqlx::Error> {
    let filename: Option<Option<String>> = sqlx::query_scalar(
        "select source_file_name from content_current_revision where document_id=$1 and revision_id=$2",
    ).bind(document_id).bind(revision_id).fetch_optional(postgres).await?;
    Ok(filename.flatten())
}

pub async fn revision_is_current(
    postgres: &PgPool,
    document_id: Uuid,
    revision_id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "select exists (select 1 from content_current_revision
         where document_id = $1 and revision_id = $2)",
    )
    .bind(document_id)
    .bind(revision_id)
    .fetch_one(postgres)
    .await
}

/// Batch form of current revision membership for list and retrieval readers.
pub async fn list_current_revision_ids(
    postgres: &PgPool,
    document_ids: &[Uuid],
) -> Result<Vec<(Uuid, Uuid)>, sqlx::Error> {
    sqlx::query_as(
        "select document_id, revision_id from content_current_revision
         where document_id = any($1::uuid[])",
    )
    .bind(document_ids)
    .fetch_all(postgres)
    .await
}

/// Frozen source identity of a chunk hit retained for serialization.
#[derive(Debug, Clone, Copy)]
pub struct SelectedChunkSource {
    pub chunk_id: Uuid,
    pub revision_id: Uuid,
    pub library_id: Uuid,
}

/// Revalidates every retained chunk/revision pair in one database snapshot,
/// including sources outside a separately ranked document page. Missing chunks,
/// mismatched owners, and withdrawn revisions deny the entire retained batch.
pub async fn chunk_sources_are_current(
    postgres: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    sources: &[SelectedChunkSource],
) -> Result<bool, sqlx::Error> {
    let chunk_ids: Vec<Uuid> = sources.iter().map(|source| source.chunk_id).collect();
    let revision_ids: Vec<Uuid> = sources.iter().map(|source| source.revision_id).collect();
    let library_ids: Vec<Uuid> = sources.iter().map(|source| source.library_id).collect();
    sqlx::query_scalar(
        "select not exists (
             select 1
             from unnest($3::uuid[], $4::uuid[], $5::uuid[])
                  selected(chunk_id, revision_id, library_id)
             left join knowledge_chunk chunk
               on chunk.chunk_id = selected.chunk_id
              and chunk.revision_id = selected.revision_id
              and chunk.workspace_id = $1 and chunk.library_id = $2
              and selected.library_id = $2 and chunk.chunk_state = 'ready'
             left join content_current_revision current_revision
               on current_revision.document_id = chunk.document_id
              and current_revision.revision_id = chunk.revision_id
              and current_revision.workspace_id = chunk.workspace_id
              and current_revision.library_id = chunk.library_id
             where current_revision.revision_id is null
         )",
    )
    .bind(workspace_id)
    .bind(library_id)
    .bind(chunk_ids)
    .bind(revision_ids)
    .bind(library_ids)
    .fetch_one(postgres)
    .await
}

/// Exact graph selectors must also belong to the current published projection.
pub async fn graph_target_is_current(
    postgres: &PgPool,
    library_id: Uuid,
    target_id: Uuid,
    target_kind: &str,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "select exists (
            select 1 from runtime_graph_snapshot snapshot
            where snapshot.library_id = $1 and (
                ($3 = 'node' and exists (select 1 from runtime_graph_node node
                 where node.id = $2 and node.library_id = $1
                   and node.projection_version = snapshot.projection_version))
                or ($3 = 'edge' and exists (select 1 from runtime_graph_edge edge
                 where edge.id = $2 and edge.library_id = $1
                   and edge.projection_version = snapshot.projection_version))
            )
         ) and exists (
            select 1 from runtime_graph_evidence evidence
            join content_current_revision current_revision
              on current_revision.document_id = evidence.document_id
             and current_revision.revision_id = evidence.revision_id
             and current_revision.library_id = evidence.library_id
            where evidence.library_id = $1 and evidence.target_id = $2
              and evidence.target_kind = $3
         ) and not exists (
            select 1 from runtime_graph_evidence evidence
            left join content_current_revision current_revision
              on current_revision.document_id = evidence.document_id
             and current_revision.revision_id = evidence.revision_id
             and current_revision.library_id = evidence.library_id
            where evidence.library_id = $1 and evidence.target_id = $2
              and evidence.target_kind = $3 and current_revision.revision_id is null
         )",
    )
    .bind(library_id)
    .bind(target_id)
    .bind(target_kind)
    .fetch_one(postgres)
    .await
}

/// Atomically revalidates the selected projection identity and all its owning
/// sources. A clean successor cannot authorize bytes retained from an old graph.
pub async fn graph_projection_is_current(
    postgres: &PgPool,
    selected: super::RuntimeGraphProjectionIdentity,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "with selected_snapshot as materialized (
             select library_id, projection_version from runtime_graph_snapshot
             where library_id = $1 and projection_version = $2
               and topology_generation = $3 and graph_status in ('ready', 'empty')
         )
         select exists (select 1 from selected_snapshot) and not exists (
            select 1 from selected_snapshot snapshot
            join runtime_graph_node node on node.library_id = snapshot.library_id
             and node.projection_version = snapshot.projection_version
            left join content_current_revision current_revision
              on current_revision.document_id = node.document_id
             and current_revision.library_id = node.library_id
            where snapshot.library_id = $1 and node.node_type = 'document'
              and (current_revision.revision_id is null or not exists (
                  select 1 from runtime_graph_evidence evidence
                  where evidence.target_id = node.id and evidence.target_kind = 'node'
                    and evidence.library_id = node.library_id
                    and evidence.document_id = current_revision.document_id
                    and evidence.revision_id = current_revision.revision_id
              ))
         ) and not exists (
            select 1 from selected_snapshot snapshot
            cross join lateral (
                select node.id, 'node'::text as kind from runtime_graph_node node
                where node.library_id = snapshot.library_id
                  and node.projection_version = snapshot.projection_version
                union all
                select edge.id, 'edge'::text as kind from runtime_graph_edge edge
                where edge.library_id = snapshot.library_id
                  and edge.projection_version = snapshot.projection_version
            ) target
            where snapshot.library_id = $1 and not exists (
                select 1 from runtime_graph_evidence evidence
                join content_current_revision current_revision
                  on current_revision.document_id = evidence.document_id
                 and current_revision.revision_id = evidence.revision_id
                 and current_revision.library_id = evidence.library_id
                where evidence.target_id = target.id and evidence.target_kind = target.kind
                  and evidence.library_id = snapshot.library_id
            )
         ) and not exists (
            select 1 from runtime_graph_evidence evidence
            join selected_snapshot snapshot on snapshot.library_id = evidence.library_id
            left join content_current_revision current_revision
              on current_revision.document_id = evidence.document_id
             and current_revision.revision_id = evidence.revision_id
             and current_revision.library_id = evidence.library_id
            where evidence.library_id = $1 and current_revision.revision_id is null
              and (exists (select 1 from runtime_graph_node node
                   where node.id = evidence.target_id and evidence.target_kind = 'node'
                     and node.projection_version = snapshot.projection_version)
                or exists (select 1 from runtime_graph_edge edge
                   where edge.id = evidence.target_id and evidence.target_kind = 'edge'
                     and edge.projection_version = snapshot.projection_version))
         )",
    )
    .bind(selected.library_id)
    .bind(selected.projection_version)
    .bind(selected.topology_generation)
    .fetch_one(postgres)
    .await
}

const LIBRARY_READABLE_STATUS_SQL: &str = "select
    library.source_truth_version,
    library.readable_projection_version is distinct from library.source_truth_version
        as is_catching_up,
    exists (
        select 1
        from knowledge_document as document
        join content_current_revision current_revision
          on current_revision.document_id = document.document_id
        join knowledge_chunk as chunk
          on chunk.document_id = document.document_id
         and chunk.workspace_id = document.workspace_id
         and chunk.library_id = document.library_id
          and chunk.revision_id = current_revision.revision_id
        join knowledge_structured_revision as structured
          on structured.revision_id = chunk.revision_id
         and structured.workspace_id = chunk.workspace_id
         and structured.library_id = chunk.library_id
         and structured.document_id = chunk.document_id
         and structured.preparation_state = 'prepared'
        where document.library_id = library.id
          and document.document_state = 'active'
          and document.deleted_at is null
          and chunk.chunk_state = 'ready'
    ) as has_readable_content
 from catalog_library as library
 where library.id = $1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryReadableContentFingerprint {
    pub value: String,
    pub source_truth_version: i64,
    pub is_catching_up: bool,
    pub has_readable_content: bool,
}

pub async fn get_library_readable_content_fingerprint(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<LibraryReadableContentFingerprint, sqlx::Error> {
    let (source_truth_version, is_catching_up, has_readable_content) =
        sqlx::query_as::<_, (i64, bool, bool)>(LIBRARY_READABLE_STATUS_SQL)
            .bind(library_id)
            .fetch_one(postgres)
            .await?;
    let value = source_truth_version.to_string();
    Ok(LibraryReadableContentFingerprint {
        value,
        source_truth_version,
        is_catching_up,
        has_readable_content,
    })
}

#[cfg(test)]
#[path = "content_fingerprint_repository_tests.rs"]
mod tests;
