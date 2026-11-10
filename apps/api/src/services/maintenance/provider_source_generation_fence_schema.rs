//! Live baseline delta: financial counters are not vector source truth.
//!
//! `consume_financial_attempt` updates
//! `ai_provider_catalog.capability_flags_json -> 'financialAdmission'` for every
//! physical provider attempt. The baseline `AFTER UPDATE` trigger treated that
//! counter write as a provider configuration change and bumped every bound
//! library's `source_truth_version`. A query that had already fenced its exact
//! vector inventory then reserved the attempt and failed its own post-dispatch
//! version check with `query_state_conflict`.
//!
//! Financial counters do not change documents, chunks or embedding profiles.
//! The replacement trigger advances source truth only when a non-financial
//! provider configuration field changed, mirroring the clean-v1 catalog seed's
//! upsert guard. A second migration file is forbidden (`docs/ru/CLI.md`), so an
//! existing database reaches this revision through
//! `ironrag-maintenance migrate provider-source-generation-fence --apply`,
//! which shares the transition skeleton in
//! [`crate::services::maintenance::baseline_delta`].

use sqlx::PgPool;

use super::baseline_delta::{AdditiveBaselineDeltaSpec, apply_additive_baseline_delta};
use crate::infra::persistence::REQUIRED_BASELINE_OBJECT_GROUPS_BILLING_KNOWLEDGE_SEARCH;

/// The immutable provider source-generation fence baseline delta: the
/// billing-knowledge-search revision → the provider source-generation fence
/// revision.
pub(crate) const PROVIDER_SOURCE_GENERATION_FENCE_DELTA: AdditiveBaselineDeltaSpec =
    AdditiveBaselineDeltaSpec {
        operator_command: "ironrag-maintenance migrate provider-source-generation-fence",
        predecessor_sha384_hex:
            super::baseline_delta::BASELINE_REVISION_BILLING_KNOWLEDGE_SEARCH_SHA384,
        target_sha384_hex:
            super::baseline_delta::BASELINE_REVISION_PROVIDER_SOURCE_GENERATION_FENCE_SHA384,
        prerequisite_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_BILLING_KNOWLEDGE_SEARCH,
        required_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_BILLING_KNOWLEDGE_SEARCH,
        additive_sql: include_str!("../../../ops/apply-provider-source-generation-fence.sql"),
        precondition: None,
        delta_present: (
            "select exists (
                 select 1 from pg_proc
                 where proname = 'invalidate_ai_provider_update_source_generations'
                   and pronamespace = 'public'::regnamespace
                   and prosrc like '%financialAdmission%'
             )",
            "the provider source-generation fence is already present under its predecessor checksum; refusing to stamp partial or pre-applied state",
        ),
        predecessor_error: "baseline checksum is not the exact provider source-generation fence predecessor; refusing to apply or stamp",
    };

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSourceGenerationFenceMigrationReport {
    pub apply: bool,
    pub already_current: bool,
    pub schema_applied: bool,
}

pub async fn migrate_provider_source_generation_fence(
    postgres: &PgPool,
    apply: bool,
) -> anyhow::Result<ProviderSourceGenerationFenceMigrationReport> {
    let transition =
        apply_additive_baseline_delta(postgres, &PROVIDER_SOURCE_GENERATION_FENCE_DELTA, apply)
            .await?;
    Ok(ProviderSourceGenerationFenceMigrationReport {
        apply,
        already_current: transition.already_current,
        schema_applied: transition.schema_applied,
    })
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;
    use uuid::Uuid;

    /// A financial admission counter update is runtime accounting, not source
    /// truth; a real provider configuration change still advances the library
    /// generation. Before the fence fix the first update bumped the library and
    /// this assertion failed.
    #[sqlx::test(migrations = "./migrations")]
    #[ignore = "requires owned PostgreSQL with pgvector"]
    async fn financial_counter_update_does_not_advance_source_truth(
        pool: PgPool,
    ) -> anyhow::Result<()> {
        let workspace = Uuid::now_v7();
        let library = Uuid::now_v7();
        let provider = Uuid::now_v7();
        let account = Uuid::now_v7();
        let model = Uuid::now_v7();
        let binding = Uuid::now_v7();

        sqlx::query(
            "insert into catalog_workspace (id, slug, display_name) values ($1, $2, 'Workspace')",
        )
        .bind(workspace)
        .bind(format!("ws-{workspace}"))
        .execute(&pool)
        .await?;
        sqlx::query(
            "insert into catalog_library (id, workspace_id, slug, display_name)
             values ($1, $2, $3, 'Library')",
        )
        .bind(library)
        .bind(workspace)
        .bind(format!("lib-{library}"))
        .execute(&pool)
        .await?;
        sqlx::query(
            "insert into ai_provider_catalog
                 (id, provider_kind, display_name, api_style, default_base_url, capability_flags_json)
             values ($1, $2, 'Provider', 'openai_compatible', 'https://example.invalid/v1', $3)",
        )
        .bind(provider)
        .bind(format!("provider-{provider}"))
        .bind(serde_json::json!({"financialAdmission": {"state": "active", "globalConsumed": 0}}))
        .execute(&pool)
        .await?;
        sqlx::query(
            "insert into ai_account (id, provider_catalog_id, label, scope_kind)
             values ($1, $2, 'Account', 'instance')",
        )
        .bind(account)
        .bind(provider)
        .execute(&pool)
        .await?;
        sqlx::query(
            "insert into ai_model_catalog (id, provider_catalog_id, model_name, metadata_json)
             values ($1, $2, 'gpt-test', '{}'::jsonb)",
        )
        .bind(model)
        .bind(provider)
        .execute(&pool)
        .await?;
        sqlx::query(
            "insert into ai_model_binding_purpose (model_catalog_id, binding_purpose)
             values ($1, 'query_answer')",
        )
        .bind(model)
        .execute(&pool)
        .await?;
        sqlx::query(
            "insert into ai_binding
                 (id, binding_purpose, account_id, provider_catalog_id, model_catalog_id,
                  scope_kind, workspace_id, library_id)
             values ($1, 'query_answer', $2, $3, $4, 'library', $5, $6)",
        )
        .bind(binding)
        .bind(account)
        .bind(provider)
        .bind(model)
        .bind(workspace)
        .bind(library)
        .execute(&pool)
        .await?;

        let before: i64 =
            sqlx::query_scalar("select source_truth_version from catalog_library where id = $1")
                .bind(library)
                .fetch_one(&pool)
                .await?;

        sqlx::query(
            "update ai_provider_catalog
             set capability_flags_json = jsonb_set(
                 capability_flags_json, '{financialAdmission,globalConsumed}', '1'::jsonb, false)
             where id = $1",
        )
        .bind(provider)
        .execute(&pool)
        .await?;
        let after_financial: i64 =
            sqlx::query_scalar("select source_truth_version from catalog_library where id = $1")
                .bind(library)
                .fetch_one(&pool)
                .await?;
        assert_eq!(
            after_financial, before,
            "a financial admission counter update must not advance library source truth"
        );

        sqlx::query(
            "update ai_provider_catalog set display_name = display_name || ' v2' where id = $1",
        )
        .bind(provider)
        .execute(&pool)
        .await?;
        let after_config: i64 =
            sqlx::query_scalar("select source_truth_version from catalog_library where id = $1")
                .bind(library)
                .fetch_one(&pool)
                .await?;
        assert!(
            after_config > before,
            "a provider configuration change must still advance library source truth"
        );
        Ok(())
    }
}
