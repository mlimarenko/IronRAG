use super::*;

impl PgContextStore {
    pub(super) async fn upsert_bundle_impl(
        &self,
        row: &KnowledgeContextBundleRow,
    ) -> anyhow::Result<KnowledgeContextBundleRow> {
        let row = sqlx::query_as::<_, PgBundleRow>(
            "insert into knowledge_context_bundle (
                bundle_id, workspace_id, library_id, query_execution_id, bundle_state,
                bundle_strategy, requested_mode, resolved_mode, selected_fact_ids,
                verification_state, verification_warnings, freshness_snapshot,
                candidate_summary, assembly_diagnostics, created_at, updated_at
             ) values (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16
             )
             on conflict (bundle_id) do update
             set workspace_id = excluded.workspace_id,
                 library_id = excluded.library_id,
                 query_execution_id = excluded.query_execution_id,
                 bundle_state = excluded.bundle_state,
                 bundle_strategy = excluded.bundle_strategy,
                 requested_mode = excluded.requested_mode,
                 resolved_mode = excluded.resolved_mode,
                 selected_fact_ids = excluded.selected_fact_ids,
                 verification_state = excluded.verification_state,
                 verification_warnings = excluded.verification_warnings,
                 freshness_snapshot = excluded.freshness_snapshot,
                 candidate_summary = excluded.candidate_summary,
                 assembly_diagnostics = excluded.assembly_diagnostics,
                 updated_at = excluded.updated_at
             returning bundle_id, workspace_id, library_id, query_execution_id,
                bundle_state, bundle_strategy, requested_mode, resolved_mode, selected_fact_ids,
                verification_state, verification_warnings, freshness_snapshot, candidate_summary,
                assembly_diagnostics, created_at, updated_at",
        )
        .bind(row.bundle_id)
        .bind(row.workspace_id)
        .bind(row.library_id)
        .bind(row.query_execution_id)
        .bind(&row.bundle_state)
        .bind(&row.bundle_strategy)
        .bind(&row.requested_mode)
        .bind(&row.resolved_mode)
        .bind(&row.selected_fact_ids)
        .bind(&row.verification_state)
        .bind(&row.verification_warnings)
        .bind(&row.freshness_snapshot)
        .bind(&row.candidate_summary)
        .bind(&row.assembly_diagnostics)
        .bind(row.created_at)
        .bind(row.updated_at)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.into())
    }

    pub(super) async fn get_bundle_impl(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeContextBundleRow>> {
        let row = sqlx::query_as::<_, PgBundleRow>(
            "select bundle_id, workspace_id, library_id, query_execution_id,
                bundle_state, bundle_strategy, requested_mode, resolved_mode, selected_fact_ids,
                verification_state, verification_warnings, freshness_snapshot, candidate_summary,
                assembly_diagnostics, created_at, updated_at
             from knowledge_context_bundle
             where bundle_id = $1
             limit 1",
        )
        .bind(bundle_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    pub(super) async fn get_bundle_by_query_execution_impl(
        &self,
        query_execution_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeContextBundleRow>> {
        let row = sqlx::query_as::<_, PgBundleRow>(
            "select b.bundle_id, b.workspace_id, b.library_id, b.query_execution_id,
                b.bundle_state, b.bundle_strategy, b.requested_mode, b.resolved_mode,
                b.selected_fact_ids, b.verification_state, b.verification_warnings,
                b.freshness_snapshot, b.candidate_summary, b.assembly_diagnostics,
                b.created_at, b.updated_at
             from query_execution execution
             join knowledge_context_bundle b
               on b.bundle_id = execution.context_bundle_id
              and b.query_execution_id = execution.id
              and b.workspace_id = execution.workspace_id
              and b.library_id = execution.library_id
             where execution.id = $1",
        )
        .bind(query_execution_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    pub(super) async fn list_bundles_by_library_impl(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeContextBundleRow>> {
        let rows = sqlx::query_as::<_, PgBundleRow>(
            "select bundle_id, workspace_id, library_id, query_execution_id,
                bundle_state, bundle_strategy, requested_mode, resolved_mode, selected_fact_ids,
                verification_state, verification_warnings, freshness_snapshot, candidate_summary,
                assembly_diagnostics, created_at, updated_at
             from knowledge_context_bundle
             where library_id = $1
             order by updated_at desc, bundle_id desc",
        )
        .bind(library_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(map_rows(rows))
    }

    pub(super) async fn update_bundle_state_impl(
        &self,
        update: BundleStateUpdate<'_>,
    ) -> anyhow::Result<Option<KnowledgeContextBundleRow>> {
        let BundleStateUpdate {
            bundle_id,
            bundle_state,
            selected_fact_ids,
            verification_state,
            verification_warnings,
            freshness_snapshot,
            candidate_summary,
            assembly_diagnostics,
        } = update;
        let row = sqlx::query_as::<_, PgBundleRow>(
            "update knowledge_context_bundle
             set bundle_state = $2,
                 selected_fact_ids = $3,
                 verification_state = $4,
                 verification_warnings = $5,
                 freshness_snapshot = $6,
                 candidate_summary = $7,
                 assembly_diagnostics = $8,
                 updated_at = now()
             where bundle_id = $1
             returning bundle_id, workspace_id, library_id, query_execution_id,
                bundle_state, bundle_strategy, requested_mode, resolved_mode, selected_fact_ids,
                verification_state, verification_warnings, freshness_snapshot, candidate_summary,
                assembly_diagnostics, created_at, updated_at",
        )
        .bind(bundle_id)
        .bind(bundle_state)
        .bind(selected_fact_ids)
        .bind(verification_state)
        .bind(&verification_warnings)
        .bind(&freshness_snapshot)
        .bind(&candidate_summary)
        .bind(&assembly_diagnostics)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }
}
