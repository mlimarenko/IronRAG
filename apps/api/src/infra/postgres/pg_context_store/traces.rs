use super::*;

impl PgContextStore {
    pub(super) async fn upsert_trace_impl(
        &self,
        row: &KnowledgeRetrievalTraceRow,
    ) -> anyhow::Result<KnowledgeRetrievalTraceRow> {
        let row = sqlx::query_as::<_, PgTraceRow>(
            "insert into knowledge_retrieval_trace (
                trace_id, workspace_id, library_id, query_execution_id, bundle_id, trace_state,
                retrieval_strategy, candidate_counts, dropped_reasons, timing_breakdown,
                diagnostics_json, created_at, updated_at
             ) values (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13
             )
             on conflict (trace_id) do update
             set workspace_id = excluded.workspace_id,
                 library_id = excluded.library_id,
                 query_execution_id = excluded.query_execution_id,
                 bundle_id = excluded.bundle_id,
                 trace_state = excluded.trace_state,
                 retrieval_strategy = excluded.retrieval_strategy,
                 candidate_counts = excluded.candidate_counts,
                 dropped_reasons = excluded.dropped_reasons,
                 timing_breakdown = excluded.timing_breakdown,
                 diagnostics_json = excluded.diagnostics_json,
                 updated_at = excluded.updated_at
             returning trace_id, workspace_id, library_id, query_execution_id, bundle_id,
                trace_state, retrieval_strategy, candidate_counts, dropped_reasons,
                timing_breakdown, diagnostics_json, created_at, updated_at",
        )
        .bind(row.trace_id)
        .bind(row.workspace_id)
        .bind(row.library_id)
        .bind(row.query_execution_id)
        .bind(row.bundle_id)
        .bind(&row.trace_state)
        .bind(&row.retrieval_strategy)
        .bind(&row.candidate_counts)
        .bind(&row.dropped_reasons)
        .bind(&row.timing_breakdown)
        .bind(&row.diagnostics_json)
        .bind(row.created_at)
        .bind(row.updated_at)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.into())
    }

    pub(super) async fn get_trace_impl(
        &self,
        trace_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeRetrievalTraceRow>> {
        let row = sqlx::query_as::<_, PgTraceRow>(
            "select trace_id, workspace_id, library_id, query_execution_id, bundle_id,
                trace_state, retrieval_strategy, candidate_counts, dropped_reasons,
                timing_breakdown, diagnostics_json, created_at, updated_at
             from knowledge_retrieval_trace
             where trace_id = $1
             limit 1",
        )
        .bind(trace_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    pub(super) async fn list_traces_by_bundle_impl(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRetrievalTraceRow>> {
        let rows = sqlx::query_as::<_, PgTraceRow>(
            "select trace_id, workspace_id, library_id, query_execution_id, bundle_id,
                trace_state, retrieval_strategy, candidate_counts, dropped_reasons,
                timing_breakdown, diagnostics_json, created_at, updated_at
             from knowledge_retrieval_trace
             where bundle_id = $1
             order by created_at desc, trace_id desc",
        )
        .bind(bundle_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(map_rows(rows))
    }

    pub(super) async fn list_traces_by_query_execution_impl(
        &self,
        query_execution_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRetrievalTraceRow>> {
        let rows = sqlx::query_as::<_, PgTraceRow>(
            "select trace_id, workspace_id, library_id, query_execution_id, bundle_id,
                trace_state, retrieval_strategy, candidate_counts, dropped_reasons,
                timing_breakdown, diagnostics_json, created_at, updated_at
             from knowledge_retrieval_trace
             where query_execution_id = $1
             order by created_at desc, trace_id desc",
        )
        .bind(query_execution_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(map_rows(rows))
    }

    pub(super) async fn update_trace_state_impl(
        &self,
        trace_id: Uuid,
        trace_state: &str,
        diagnostics_json: serde_json::Value,
    ) -> anyhow::Result<Option<KnowledgeRetrievalTraceRow>> {
        let row = sqlx::query_as::<_, PgTraceRow>(
            "update knowledge_retrieval_trace
             set trace_state = $2,
                 diagnostics_json = $3,
                 updated_at = now()
             where trace_id = $1
             returning trace_id, workspace_id, library_id, query_execution_id, bundle_id,
                trace_state, retrieval_strategy, candidate_counts, dropped_reasons,
                timing_breakdown, diagnostics_json, created_at, updated_at",
        )
        .bind(trace_id)
        .bind(trace_state)
        .bind(&diagnostics_json)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }
}
