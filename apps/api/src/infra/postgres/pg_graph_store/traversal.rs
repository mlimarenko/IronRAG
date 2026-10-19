use super::*;

impl PgGraphStore {
    pub(super) async fn active_projection_version(&self, library_id: Uuid) -> anyhow::Result<i64> {
        sqlx::query_scalar::<_, i64>(
            "select coalesce(
                (
                    select projection_version
                    from runtime_graph_snapshot
                    where library_id = $1
                      and projection_version > 0
                      and graph_status <> 'empty'
                    limit 1
                ),
                (
                    select max(projection_version)
                    from runtime_graph_node
                    where library_id = $1
                ),
                1
            )",
        )
        .bind(library_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to load active runtime graph projection version")
    }

    pub(super) async fn traverse_runtime_graph(
        &self,
        start_kind: &str,
        start_id: Uuid,
        library_id: Uuid,
        max_depth: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeGraphTraversalRow>> {
        let rows = sqlx::query_scalar::<_, serde_json::Value>(
            "with recursive active_projection(projection_version) as (
                select coalesce(
                    (
                        select projection_version
                        from runtime_graph_snapshot
                        where library_id = $1
                          and projection_version > 0
                          and graph_status <> 'empty'
                        limit 1
                    ),
                    (
                        select max(projection_version)
                        from runtime_graph_node
                        where library_id = $1
                    ),
                    1
                )
            ),
            walk(vertex_kind, vertex_id, depth, via_edge_id, path) as (
                select $2::text, $3::uuid, 0::int, null::uuid, array[$3::uuid]
                union all
                select next.vertex_kind, next.vertex_id, walk.depth + 1, next.via_edge_id,
                    walk.path || next.vertex_id
                from walk
                join lateral (
                    select 'runtime_node'::text as vertex_kind,
                        edge.to_node_id as vertex_id,
                        edge.id as via_edge_id,
                        edge.support_count
                    from runtime_graph_edge edge
                    where walk.vertex_kind = 'runtime_node'
                      and edge.library_id = $1
                      and edge.projection_version = (select projection_version from active_projection)
                      and btrim(edge.relation_type) <> ''
                      and edge.from_node_id <> edge.to_node_id
                      and edge.from_node_id = walk.vertex_id
                    union all
                    select 'runtime_node'::text as vertex_kind,
                        edge.from_node_id as vertex_id,
                        edge.id as via_edge_id,
                        edge.support_count
                    from runtime_graph_edge edge
                    where walk.vertex_kind = 'runtime_node'
                      and edge.library_id = $1
                      and edge.projection_version = (select projection_version from active_projection)
                      and btrim(edge.relation_type) <> ''
                      and edge.from_node_id <> edge.to_node_id
                      and edge.to_node_id = walk.vertex_id
                    union all
                    select 'runtime_edge'::text, edge.id, edge.id, edge.support_count
                    from runtime_graph_edge edge
                    where walk.vertex_kind = 'runtime_node'
                      and edge.library_id = $1
                      and edge.projection_version = (select projection_version from active_projection)
                      and btrim(edge.relation_type) <> ''
                      and edge.from_node_id <> edge.to_node_id
                      and edge.from_node_id = walk.vertex_id
                    union all
                    select 'runtime_edge'::text, edge.id, edge.id, edge.support_count
                    from runtime_graph_edge edge
                    where walk.vertex_kind = 'runtime_node'
                      and edge.library_id = $1
                      and edge.projection_version = (select projection_version from active_projection)
                      and btrim(edge.relation_type) <> ''
                      and edge.from_node_id <> edge.to_node_id
                      and edge.to_node_id = walk.vertex_id
                    union all
                    select 'runtime_node'::text, edge.from_node_id, edge.id, edge.support_count
                    from runtime_graph_edge edge
                    where walk.vertex_kind = 'runtime_edge'
                      and edge.id = walk.vertex_id
                      and edge.library_id = $1
                      and edge.projection_version = (select projection_version from active_projection)
                    union all
                    select 'runtime_node'::text, edge.to_node_id, edge.id, edge.support_count
                    from runtime_graph_edge edge
                    where walk.vertex_kind = 'runtime_edge'
                      and edge.id = walk.vertex_id
                      and edge.library_id = $1
                      and edge.projection_version = (select projection_version from active_projection)
                    union all
                    select 'runtime_evidence'::text, evidence.id, null::uuid, 0::int
                    from runtime_graph_evidence evidence
                    where evidence.library_id = $1
                      and (
                        (walk.vertex_kind = 'runtime_node'
                         and evidence.target_kind = 'node'
                         and evidence.target_id = walk.vertex_id)
                        or
                        (walk.vertex_kind = 'runtime_edge'
                         and evidence.target_kind = 'edge'
                         and evidence.target_id = walk.vertex_id)
                      )
                    order by support_count desc, vertex_id asc
                ) next on true
                where walk.depth < $4
                  and not next.vertex_id = any(walk.path)
            ),
            ranked as (
                select vertex_kind, vertex_id, min(depth)::bigint as path_length,
                    (array_agg(via_edge_id order by depth asc))[1] as via_edge_id
                from walk
                group by vertex_kind, vertex_id
            ),
            limited as (
                select *
                from ranked
                order by path_length asc, vertex_kind asc, vertex_id asc
                limit $5
            ),
            projected as (
                select limited.path_length,
                    case
                        when limited.vertex_kind = 'runtime_node' and node.node_type = 'document'
                            then $10
                        when limited.vertex_kind = 'runtime_node' then $6
                        when limited.vertex_kind = 'runtime_edge' then $7
                        when limited.vertex_kind = 'runtime_evidence' then $8
                        else $9
                    end as vertex_kind,
                    limited.vertex_id,
                    case when edge.id is null then null else 'runtime_graph_edge' end as edge_kind,
                    edge.\u{63}anonical_key as edge_key,
                    edge.rank as edge_rank,
                    coalesce(edge.edge_score, edge.weight) as edge_score,
                    edge.inclusion_reason as edge_inclusion_reason,
                    case
                        when limited.vertex_kind = 'runtime_node' then to_jsonb(node)
                        when limited.vertex_kind = 'runtime_edge' then to_jsonb(relation_edge)
                        when limited.vertex_kind = 'runtime_evidence' then to_jsonb(evidence)
                        else '{}'::jsonb
                    end as vertex
                from limited
                left join runtime_graph_node node
                  on limited.vertex_kind = 'runtime_node'
                 and node.id = limited.vertex_id
                 and node.library_id = $1
                 and node.projection_version = (select projection_version from active_projection)
                left join runtime_graph_edge relation_edge
                  on limited.vertex_kind = 'runtime_edge'
                 and relation_edge.id = limited.vertex_id
                 and relation_edge.library_id = $1
                 and relation_edge.projection_version = (select projection_version from active_projection)
                left join runtime_graph_evidence evidence
                  on limited.vertex_kind = 'runtime_evidence'
                 and evidence.id = limited.vertex_id
                 and evidence.library_id = $1
                left join runtime_graph_edge edge
                  on edge.id = limited.via_edge_id
                 and edge.library_id = $1
                 and edge.projection_version = (select projection_version from active_projection)
                where (limited.vertex_kind = 'runtime_node' and node.id is not null)
                   or (limited.vertex_kind = 'runtime_edge' and relation_edge.id is not null)
                   or (limited.vertex_kind = 'runtime_evidence' and evidence.id is not null)
            )
            select jsonb_build_object(
                'path_length', projected.path_length,
                'vertex_kind', projected.vertex_kind,
                'vertex_id', projected.vertex_id,
                'edge_kind', projected.edge_kind,
                'edge_key', projected.edge_key,
                'edge_rank', projected.edge_rank,
                'edge_score', projected.edge_score,
                'edge_inclusion_reason', projected.edge_inclusion_reason,
                'vertex', projected.vertex
            )
            from projected
            order by projected.path_length asc, projected.vertex_kind asc, projected.vertex_id asc",
        )
        .bind(library_id)
        .bind(start_kind)
        .bind(start_id)
        .bind(clamp_depth(max_depth))
        .bind(clamp_limit(limit))
        .bind(KNOWLEDGE_ENTITY_COLLECTION)
        .bind(KNOWLEDGE_RELATION_COLLECTION)
        .bind(KNOWLEDGE_EVIDENCE_COLLECTION)
        .bind(KNOWLEDGE_CHUNK_COLLECTION)
        .bind(KNOWLEDGE_DOCUMENT_COLLECTION)
        .fetch_all(&self.pool)
        .await
        .context("failed to traverse runtime graph")?;
        decode_json_rows(rows, "failed to decode runtime graph traversal rows")
    }
}

impl PgGraphStore {
    pub(super) async fn list_relation_topology_by_library_impl(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationTopologyRow>> {
        let projection_version = self.active_projection_version(library_id).await?;
        let rows = sqlx::query_scalar::<_, serde_json::Value>(
            "select jsonb_build_object(
                'relation_id', edge.id,
                'workspace_id', library.workspace_id,
                'library_id', edge.library_id,
                'predicate', edge.relation_type,
                'normalized_assertion', edge.\u{63}anonical_key,
                'confidence', edge.weight,
                'support_count', edge.support_count::bigint,
                'contradiction_state', coalesce(edge.metadata_json->>'contradiction_state', ''),
                'freshness_generation', edge.projection_version,
                'relation_state', coalesce(edge.metadata_json->>'relation_state', 'active'),
                'created_at', edge.created_at,
                'updated_at', edge.updated_at,
                'subject_entity_id', edge.from_node_id,
                'object_entity_id', edge.to_node_id
            )
            from runtime_graph_edge edge
            join catalog_library library on library.id = edge.library_id
            where edge.library_id = $1
              and edge.projection_version = $2
              and btrim(edge.relation_type) <> ''
              and edge.from_node_id <> edge.to_node_id
            order by edge.support_count desc, edge.updated_at desc, edge.id desc
            limit 10000",
        )
        .bind(library_id)
        .bind(projection_version)
        .fetch_all(&self.pool)
        .await
        .context("failed to list runtime relation topology by library")?;
        decode_json_rows(rows, "failed to decode relation topology rows")
    }

    pub(super) async fn get_relation_topology_by_id_impl(
        &self,
        relation_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeRelationTopologyRow>> {
        let row = sqlx::query_scalar::<_, serde_json::Value>(
            "select jsonb_build_object(
                'relation_id', edge.id,
                'workspace_id', library.workspace_id,
                'library_id', edge.library_id,
                'predicate', edge.relation_type,
                'normalized_assertion', edge.\u{63}anonical_key,
                'confidence', edge.weight,
                'support_count', edge.support_count::bigint,
                'contradiction_state', coalesce(edge.metadata_json->>'contradiction_state', ''),
                'freshness_generation', edge.projection_version,
                'relation_state', coalesce(edge.metadata_json->>'relation_state', 'active'),
                'created_at', edge.created_at,
                'updated_at', edge.updated_at,
                'subject_entity_id', edge.from_node_id,
                'object_entity_id', edge.to_node_id
            )
            from runtime_graph_edge edge
            join catalog_library library on library.id = edge.library_id
            where edge.id = $1
              and btrim(edge.relation_type) <> ''
              and edge.from_node_id <> edge.to_node_id
            limit 1",
        )
        .bind(relation_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get runtime relation topology by id")?;
        row.map(|value| decode_json(value, "failed to decode relation topology row")).transpose()
    }

    pub(super) async fn list_entity_neighborhood_impl(
        &self,
        entity_id: Uuid,
        library_id: Uuid,
        max_depth: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeGraphTraversalRow>> {
        self.traverse_runtime_graph("runtime_node", entity_id, library_id, max_depth, limit).await
    }

    pub(super) async fn expand_relation_centric_impl(
        &self,
        relation_id: Uuid,
        library_id: Uuid,
        max_depth: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeGraphTraversalRow>> {
        self.traverse_runtime_graph("runtime_edge", relation_id, library_id, max_depth, limit).await
    }

    pub(super) async fn list_relation_evidence_lookup_impl(
        &self,
        relation_id: Uuid,
        library_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeRelationEvidenceLookupRow>> {
        // runtime_graph_evidence does not persist the source edge's created_at.
        // Rank, evidence created_at, and id are the closest deterministic
        // ordering available in the PG runtime model.
        let rows = sqlx::query_scalar::<_, serde_json::Value>(
            "select jsonb_build_object(
                'relation', jsonb_build_object(
                    'relation_id', edge.id,
                    'workspace_id', library.workspace_id,
                    'library_id', edge.library_id,
                    'predicate', edge.relation_type,
                    'normalized_assertion', edge.\u{63}anonical_key,
                    'confidence', edge.weight,
                    'support_count', edge.support_count::bigint,
                    'contradiction_state', coalesce(edge.metadata_json->>'contradiction_state', ''),
                    'freshness_generation', edge.projection_version,
                    'relation_state', coalesce(edge.metadata_json->>'relation_state', 'active'),
                    'created_at', edge.created_at,
                    'updated_at', edge.updated_at
                ),
                'evidence', jsonb_build_object(
                    'evidence_id', evidence.id,
                    'workspace_id', library.workspace_id,
                    'library_id', evidence.library_id,
                    'document_id', evidence.document_id,
                    'revision_id', evidence.revision_id,
                    'chunk_id', evidence.chunk_id,
                    'block_id', null,
                    'fact_id', null,
                    'span_start', null,
                    'span_end', null,
                    'quote_text', evidence.evidence_text,
                    'literal_spans_json', '{}'::jsonb,
                    'evidence_kind', evidence.target_kind,
                    'extraction_method', 'runtime_graph',
                    'confidence', evidence.confidence_score,
                    'evidence_state', 'active',
                    'freshness_generation', 0,
                    'created_at', evidence.created_at,
                    'updated_at', evidence.created_at
                ),
                'support_edge_rank', evidence.rank,
                'support_edge_score', coalesce(evidence.score, evidence.support_edge_score),
                'support_edge_inclusion_reason',
                    coalesce(evidence.inclusion_reason, evidence.support_edge_inclusion_reason),
                'source_document',
                    case when document.id is null then null else to_jsonb(doc_row) end,
                'source_revision',
                    case when revision.id is null then null else to_jsonb(rev_row) end,
                'source_chunk',
                    case when chunk.id is null then null else to_jsonb(chunk_row) end
            )
            from runtime_graph_edge edge
            join catalog_library library on library.id = edge.library_id
            join runtime_graph_evidence evidence
              on evidence.library_id = edge.library_id
             and evidence.target_kind = 'edge'
             and evidence.target_id = edge.id
            left join content_document document
              on document.id = evidence.document_id
             and document.library_id = evidence.library_id
            left join content_document_head document_head
              on document_head.document_id = document.id
            left join lateral (
                select max(revision_number)::bigint as latest_revision_no
                from content_revision
                where document_id = document.id
            ) latest_revision on document.id is not null
            left join lateral (
                select document.id as document_id,
                    document.workspace_id, document.library_id, document.external_key,
                    null::text as file_name, null::text as title,
                    document.document_state::text as document_state,
                    document_head.active_revision_id, document_head.readable_revision_id,
                    latest_revision.latest_revision_no, document.created_at,
                    coalesce(document_head.head_updated_at, document.created_at) as updated_at,
                    document.deleted_at
            ) doc_row on document.id is not null
            left join content_revision revision
              on revision.id = evidence.revision_id
             and revision.library_id = evidence.library_id
            left join lateral (
                select revision.id as revision_id,
                    revision.workspace_id, revision.library_id, revision.document_id,
                    revision.revision_number::bigint as revision_number,
                    'active'::text as revision_state,
                    revision.content_source_kind::text as revision_kind,
                    revision.storage_key as storage_ref,
                    revision.source_uri, revision.document_hint,
                    revision.mime_type, revision.checksum, revision.title, revision.byte_size,
                    null::text as normalized_text,
                    revision.checksum as text_checksum,
                    null::text as image_checksum,
                    'text_readable'::text as text_state,
                    'ready'::text as vector_state,
                    'ready'::text as graph_state,
                    null::timestamptz as text_readable_at,
                    null::timestamptz as vector_ready_at,
                    null::timestamptz as graph_ready_at,
                    null::uuid as superseded_by_revision_id,
                    revision.created_at
            ) rev_row on revision.id is not null
            left join content_chunk chunk
              on chunk.id = evidence.chunk_id
            left join content_revision chunk_revision
              on chunk_revision.id = chunk.revision_id
            left join lateral (
                select chunk.id as chunk_id,
                    chunk_revision.workspace_id, chunk_revision.library_id,
                    chunk_revision.document_id, chunk.revision_id, chunk.chunk_index,
                    null::text as chunk_kind,
                    chunk.normalized_text as content_text,
                    chunk.normalized_text,
                    chunk.start_offset as span_start,
                    chunk.end_offset as span_end,
                    chunk.token_count,
                    '{}'::uuid[] as support_block_ids,
                    '{}'::text[] as section_path,
                    '{}'::text[] as heading_trail,
                    chunk.text_checksum as literal_digest,
                    'active'::text as chunk_state,
                    null::bigint as text_generation,
                    null::bigint as vector_generation,
                    null::real as quality_score,
                    chunk.window_text,
                    chunk.occurred_at, chunk.occurred_until
            ) chunk_row on chunk.id is not null and chunk_revision.id is not null
            where edge.id = $1
              and edge.library_id = $2
            order by evidence.rank asc nulls last, evidence.created_at asc, evidence.id asc
            limit $3",
        )
        .bind(relation_id)
        .bind(library_id)
        .bind(clamp_limit(limit))
        .fetch_all(&self.pool)
        .await
        .context("failed to lookup runtime relation evidence")?;
        decode_json_rows(rows, "failed to decode relation evidence lookup rows")
    }
}
