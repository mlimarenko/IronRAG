use super::*;

impl PgContextStore {
    pub(super) async fn get_bundle_reference_set_impl(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeContextBundleReferenceSetRow>> {
        let row = sqlx::query_as::<_, PgReferenceSetRow>(
            "select
                b.bundle_id, b.workspace_id, b.library_id, b.query_execution_id,
                b.bundle_state, b.bundle_strategy, b.requested_mode, b.resolved_mode,
                b.selected_fact_ids, b.verification_state, b.verification_warnings,
                b.freshness_snapshot, b.candidate_summary, b.assembly_diagnostics,
                b.created_at, b.updated_at,
                coalesce(chunk_refs.items, '[]'::jsonb) as chunk_references,
                coalesce(entity_refs.items, '[]'::jsonb) as entity_references,
                coalesce(relation_refs.items, '[]'::jsonb) as relation_references,
                coalesce(evidence_refs.items, '[]'::jsonb) as evidence_references
             from knowledge_context_bundle b
             left join lateral (
                select jsonb_agg(
                    jsonb_build_object(
                        'bundle_id', edge.bundle_id,
                        'chunk_id', edge.chunk_id,
                        'rank', edge.rank,
                        'score', edge.score,
                        'inclusion_reason', edge.inclusion_reason,
                        'created_at', edge.created_at
                    )
                    order by edge.rank asc, edge.score desc, edge.chunk_id asc
                ) as items
                from knowledge_bundle_chunk edge
                where edge.bundle_id = b.bundle_id
             ) chunk_refs on true
             left join lateral (
                select jsonb_agg(
                    jsonb_build_object(
                        'bundle_id', edge.bundle_id,
                        'entity_id', edge.entity_id,
                        'rank', edge.rank,
                        'score', edge.score,
                        'inclusion_reason', edge.inclusion_reason,
                        'created_at', edge.created_at
                    )
                    order by edge.rank asc, edge.score desc, edge.entity_id asc
                ) as items
                from knowledge_bundle_entity edge
                where edge.bundle_id = b.bundle_id
             ) entity_refs on true
             left join lateral (
                select jsonb_agg(
                    jsonb_build_object(
                        'bundle_id', edge.bundle_id,
                        'relation_id', edge.relation_id,
                        'rank', edge.rank,
                        'score', edge.score,
                        'inclusion_reason', edge.inclusion_reason,
                        'created_at', edge.created_at
                    )
                    order by edge.rank asc, edge.score desc, edge.relation_id asc
                ) as items
                from knowledge_bundle_relation edge
                where edge.bundle_id = b.bundle_id
             ) relation_refs on true
             left join lateral (
                select jsonb_agg(
                    jsonb_build_object(
                        'bundle_id', edge.bundle_id,
                        'evidence_id', edge.evidence_id,
                        'rank', edge.rank,
                        'score', edge.score,
                        'inclusion_reason', edge.inclusion_reason,
                        'created_at', edge.created_at
                    )
                    order by edge.rank asc, edge.score desc, edge.evidence_id asc
                ) as items
                from knowledge_bundle_evidence edge
                where edge.bundle_id = b.bundle_id
             ) evidence_refs on true
             where b.bundle_id = $1
             limit 1",
        )
        .bind(bundle_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(TryInto::try_into).transpose()
    }

    pub(super) async fn get_bundle_reference_set_by_query_execution_impl(
        &self,
        query_execution_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeContextBundleReferenceSetRow>> {
        let row = sqlx::query_as::<_, PgReferenceSetRow>(
            "select
                b.bundle_id, b.workspace_id, b.library_id, b.query_execution_id,
                b.bundle_state, b.bundle_strategy, b.requested_mode, b.resolved_mode,
                b.selected_fact_ids, b.verification_state, b.verification_warnings,
                b.freshness_snapshot, b.candidate_summary, b.assembly_diagnostics,
                b.created_at, b.updated_at,
                coalesce(chunk_refs.items, '[]'::jsonb) as chunk_references,
                coalesce(entity_refs.items, '[]'::jsonb) as entity_references,
                coalesce(relation_refs.items, '[]'::jsonb) as relation_references,
                coalesce(evidence_refs.items, '[]'::jsonb) as evidence_references
             from query_execution execution
             join knowledge_context_bundle b
               on b.bundle_id = execution.context_bundle_id
              and b.query_execution_id = execution.id
              and b.workspace_id = execution.workspace_id
              and b.library_id = execution.library_id
             left join lateral (
                select jsonb_agg(
                    jsonb_build_object(
                        'bundle_id', edge.bundle_id,
                        'chunk_id', edge.chunk_id,
                        'rank', edge.rank,
                        'score', edge.score,
                        'inclusion_reason', edge.inclusion_reason,
                        'created_at', edge.created_at
                    )
                    order by edge.rank asc, edge.score desc, edge.chunk_id asc
                ) as items
                from knowledge_bundle_chunk edge
                where edge.bundle_id = b.bundle_id
             ) chunk_refs on true
             left join lateral (
                select jsonb_agg(
                    jsonb_build_object(
                        'bundle_id', edge.bundle_id,
                        'entity_id', edge.entity_id,
                        'rank', edge.rank,
                        'score', edge.score,
                        'inclusion_reason', edge.inclusion_reason,
                        'created_at', edge.created_at
                    )
                    order by edge.rank asc, edge.score desc, edge.entity_id asc
                ) as items
                from knowledge_bundle_entity edge
                where edge.bundle_id = b.bundle_id
             ) entity_refs on true
             left join lateral (
                select jsonb_agg(
                    jsonb_build_object(
                        'bundle_id', edge.bundle_id,
                        'relation_id', edge.relation_id,
                        'rank', edge.rank,
                        'score', edge.score,
                        'inclusion_reason', edge.inclusion_reason,
                        'created_at', edge.created_at
                    )
                    order by edge.rank asc, edge.score desc, edge.relation_id asc
                ) as items
                from knowledge_bundle_relation edge
                where edge.bundle_id = b.bundle_id
             ) relation_refs on true
             left join lateral (
                select jsonb_agg(
                    jsonb_build_object(
                        'bundle_id', edge.bundle_id,
                        'evidence_id', edge.evidence_id,
                        'rank', edge.rank,
                        'score', edge.score,
                        'inclusion_reason', edge.inclusion_reason,
                        'created_at', edge.created_at
                    )
                    order by edge.rank asc, edge.score desc, edge.evidence_id asc
                ) as items
                from knowledge_bundle_evidence edge
                where edge.bundle_id = b.bundle_id
             ) evidence_refs on true
             where execution.id = $1",
        )
        .bind(query_execution_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(TryInto::try_into).transpose()
    }

    pub(super) async fn list_bundle_reference_sets_by_library_impl(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeContextBundleReferenceSetRow>> {
        let rows = sqlx::query_as::<_, PgReferenceSetRow>(
            "select
                b.bundle_id, b.workspace_id, b.library_id, b.query_execution_id,
                b.bundle_state, b.bundle_strategy, b.requested_mode, b.resolved_mode,
                b.selected_fact_ids, b.verification_state, b.verification_warnings,
                b.freshness_snapshot, b.candidate_summary, b.assembly_diagnostics,
                b.created_at, b.updated_at,
                coalesce(chunk_refs.items, '[]'::jsonb) as chunk_references,
                coalesce(entity_refs.items, '[]'::jsonb) as entity_references,
                coalesce(relation_refs.items, '[]'::jsonb) as relation_references,
                coalesce(evidence_refs.items, '[]'::jsonb) as evidence_references
             from knowledge_context_bundle b
             left join lateral (
                select jsonb_agg(
                    jsonb_build_object(
                        'bundle_id', edge.bundle_id,
                        'chunk_id', edge.chunk_id,
                        'rank', edge.rank,
                        'score', edge.score,
                        'inclusion_reason', edge.inclusion_reason,
                        'created_at', edge.created_at
                    )
                    order by edge.rank asc, edge.score desc, edge.chunk_id asc
                ) as items
                from knowledge_bundle_chunk edge
                where edge.bundle_id = b.bundle_id
             ) chunk_refs on true
             left join lateral (
                select jsonb_agg(
                    jsonb_build_object(
                        'bundle_id', edge.bundle_id,
                        'entity_id', edge.entity_id,
                        'rank', edge.rank,
                        'score', edge.score,
                        'inclusion_reason', edge.inclusion_reason,
                        'created_at', edge.created_at
                    )
                    order by edge.rank asc, edge.score desc, edge.entity_id asc
                ) as items
                from knowledge_bundle_entity edge
                where edge.bundle_id = b.bundle_id
             ) entity_refs on true
             left join lateral (
                select jsonb_agg(
                    jsonb_build_object(
                        'bundle_id', edge.bundle_id,
                        'relation_id', edge.relation_id,
                        'rank', edge.rank,
                        'score', edge.score,
                        'inclusion_reason', edge.inclusion_reason,
                        'created_at', edge.created_at
                    )
                    order by edge.rank asc, edge.score desc, edge.relation_id asc
                ) as items
                from knowledge_bundle_relation edge
                where edge.bundle_id = b.bundle_id
             ) relation_refs on true
             left join lateral (
                select jsonb_agg(
                    jsonb_build_object(
                        'bundle_id', edge.bundle_id,
                        'evidence_id', edge.evidence_id,
                        'rank', edge.rank,
                        'score', edge.score,
                        'inclusion_reason', edge.inclusion_reason,
                        'created_at', edge.created_at
                    )
                    order by edge.rank asc, edge.score desc, edge.evidence_id asc
                ) as items
                from knowledge_bundle_evidence edge
                where edge.bundle_id = b.bundle_id
             ) evidence_refs on true
             where b.library_id = $1
             order by b.updated_at desc, b.bundle_id desc",
        )
        .bind(library_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }
}
