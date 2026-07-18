//! PostgreSQL integration coverage for AI configuration cache-generation fencing.

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result};
use ironrag_backend::{app::config::Settings, infra::repositories::catalog_repository};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::sync::Barrier;
use uuid::Uuid;

struct Fixture {
    workspace_id: Uuid,
    library_id: Uuid,
    provider_id: Uuid,
    model_id: Uuid,
    account_id: Uuid,
    binding_id: Uuid,
}

impl Fixture {
    async fn create(pool: &PgPool) -> Result<Self> {
        let suffix = Uuid::now_v7().simple().to_string();
        let provider_id = Uuid::now_v7();
        let model_id = Uuid::now_v7();
        let account_id = Uuid::now_v7();
        let binding_id = Uuid::now_v7();
        let workspace = catalog_repository::create_workspace(
            pool,
            &format!("ai-generation-fence-{suffix}"),
            "AI Generation Fence",
            None,
        )
        .await?;
        let library = catalog_repository::create_library(
            pool,
            workspace.id,
            &format!("ai-generation-fence-{suffix}"),
            "AI Generation Fence",
            None,
            None,
        )
        .await?;

        sqlx::query(
            "insert into ai_provider_catalog (
                id, provider_kind, display_name, api_style, lifecycle_state,
                default_base_url, capability_flags_json
             ) values ($1, $2, 'AI Generation Fence', 'openai_compatible', 'active',
                       'https://provider.invalid/v1', '{}'::jsonb)",
        )
        .bind(provider_id)
        .bind(format!("ai-generation-fence-{suffix}"))
        .execute(pool)
        .await?;
        sqlx::query(
            "insert into ai_model_catalog (
                id, provider_catalog_id, model_name, capability_kind, modality_kind,
                lifecycle_state, metadata_json
             ) values ($1, $2, 'model-a', 'chat', 'text', 'active',
                       '{\"defaultRoles\":[\"query_answer\"]}'::jsonb)",
        )
        .bind(model_id)
        .bind(provider_id)
        .execute(pool)
        .await?;
        sqlx::query(
            "insert into ai_account (
                id, scope_kind, workspace_id, library_id, provider_catalog_id,
                label, api_key, base_url, credential_state
             ) values ($1, 'library', $2, $3, $4, 'AI Generation Fence', null,
                       'https://account.invalid/v1', 'active')",
        )
        .bind(account_id)
        .bind(workspace.id)
        .bind(library.id)
        .bind(provider_id)
        .execute(pool)
        .await?;
        sqlx::query(
            "insert into ai_binding (
                id, scope_kind, workspace_id, library_id, binding_purpose,
                account_id, model_catalog_id, system_prompt, extra_parameters_json,
                binding_state, created_at, updated_at
             ) values ($1, 'library', $2, $3, 'query_answer', $4, $5,
                       'prompt-a', '{}'::jsonb, 'active', now(), now())",
        )
        .bind(binding_id)
        .bind(workspace.id)
        .bind(library.id)
        .bind(account_id)
        .bind(model_id)
        .execute(pool)
        .await?;

        Ok(Self {
            workspace_id: workspace.id,
            library_id: library.id,
            provider_id,
            model_id,
            account_id,
            binding_id,
        })
    }

    async fn cleanup(&self, pool: &PgPool) -> Result<()> {
        sqlx::query("delete from catalog_workspace where id = $1")
            .bind(self.workspace_id)
            .execute(pool)
            .await?;
        sqlx::query("delete from ai_model_catalog where id = $1")
            .bind(self.model_id)
            .execute(pool)
            .await?;
        sqlx::query("delete from ai_provider_catalog where id = $1")
            .bind(self.provider_id)
            .execute(pool)
            .await?;
        Ok(())
    }
}

async fn connect_postgres() -> Result<PgPool> {
    let settings = Settings::from_env().context("load AI config generation fence settings")?;
    let pool = PgPoolOptions::new().max_connections(4).connect(&settings.database_url).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}

async fn source_generation(pool: &PgPool, library_id: Uuid) -> Result<i64> {
    Ok(catalog_repository::get_library_source_truth_version(pool, library_id).await?)
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn every_effective_ai_identity_layer_advances_library_generation() -> Result<()> {
    let pool = connect_postgres().await?;
    let fixture = Fixture::create(&pool).await?;

    let result = async {
        let before_binding = source_generation(&pool, fixture.library_id).await?;
        sqlx::query(
            "update ai_binding
             set system_prompt = 'prompt-b', updated_at = now()
             where id = $1",
        )
        .bind(fixture.binding_id)
        .execute(&pool)
        .await?;
        let after_binding = source_generation(&pool, fixture.library_id).await?;
        assert!(after_binding > before_binding);

        sqlx::query(
            "update ai_account
             set base_url = 'https://account-b.invalid/v1', updated_at = now()
             where id = $1",
        )
        .bind(fixture.account_id)
        .execute(&pool)
        .await?;
        let after_account = source_generation(&pool, fixture.library_id).await?;
        assert!(after_account > after_binding);

        sqlx::query("update ai_model_catalog set model_name = 'model-b' where id = $1")
            .bind(fixture.model_id)
            .execute(&pool)
            .await?;
        let after_model = source_generation(&pool, fixture.library_id).await?;
        assert!(after_model > after_account);

        sqlx::query(
            "update ai_provider_catalog
             set default_base_url = 'https://provider-b.invalid/v1'
             where id = $1",
        )
        .bind(fixture.provider_id)
        .execute(&pool)
        .await?;
        let after_provider = source_generation(&pool, fixture.library_id).await?;
        assert!(after_provider > after_model);
        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn config_commit_and_old_cache_generation_fence_are_atomic() -> Result<()> {
    let pool = connect_postgres().await?;
    let fixture = Fixture::create(&pool).await?;

    let result = async {
        let stale_generation = source_generation(&pool, fixture.library_id).await?;
        let mut mutation = pool.begin().await?;
        sqlx::query(
            "update ai_binding
             set system_prompt = 'racing-prompt', updated_at = now()
             where id = $1",
        )
        .bind(fixture.binding_id)
        .execute(&mut *mutation)
        .await?;

        let fence_pool = pool.clone();
        let library_id = fixture.library_id;
        let fence = tokio::spawn(async move {
            sqlx::query_scalar::<_, bool>(
                "select true
                 from catalog_library
                 where id = $1 and source_truth_version = $2
                 for share",
            )
            .bind(library_id)
            .bind(stale_generation)
            .fetch_optional(&fence_pool)
            .await
        });
        tokio::task::yield_now().await;
        mutation.commit().await?;

        assert_eq!(
            fence.await??,
            None,
            "a cache writer/replayer fenced by the old generation must lose after config commit"
        );
        assert!(source_generation(&pool, fixture.library_id).await? > stale_generation);
        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn opposite_scope_moves_lock_affected_libraries_in_one_ordered_pass() -> Result<()> {
    let pool = connect_postgres().await?;
    let fixture = Fixture::create(&pool).await?;

    let result = async {
        let suffix = Uuid::now_v7().simple().to_string();
        let second_library = catalog_repository::create_library(
            &pool,
            fixture.workspace_id,
            &format!("ai-generation-fence-second-{suffix}"),
            "AI Generation Fence Second",
            None,
            None,
        )
        .await?;
        let second_binding_id = Uuid::now_v7();
        sqlx::query(
            "insert into ai_binding (
                id, scope_kind, workspace_id, library_id, binding_purpose,
                account_id, model_catalog_id, system_prompt, extra_parameters_json,
                binding_state, created_at, updated_at
             ) values ($1, 'library', $2, $3, 'query_compile', $4, $5,
                       'prompt-second', '{}'::jsonb, 'active', now(), now())",
        )
        .bind(second_binding_id)
        .bind(fixture.workspace_id)
        .bind(second_library.id)
        .bind(fixture.account_id)
        .bind(fixture.model_id)
        .execute(&pool)
        .await?;

        let first_before = source_generation(&pool, fixture.library_id).await?;
        let second_before = source_generation(&pool, second_library.id).await?;
        let barrier = Arc::new(Barrier::new(3));
        let first_move = {
            let pool = pool.clone();
            let barrier = Arc::clone(&barrier);
            let workspace_id = fixture.workspace_id;
            let first_binding_id = fixture.binding_id;
            let second_library_id = second_library.id;
            tokio::spawn(async move {
                barrier.wait().await;
                sqlx::query(
                    "update ai_binding
                     set workspace_id = $2, library_id = $3, updated_at = now()
                     where id = $1",
                )
                .bind(first_binding_id)
                .bind(workspace_id)
                .bind(second_library_id)
                .execute(&pool)
                .await
            })
        };
        let second_move = {
            let pool = pool.clone();
            let barrier = Arc::clone(&barrier);
            let workspace_id = fixture.workspace_id;
            let first_library_id = fixture.library_id;
            tokio::spawn(async move {
                barrier.wait().await;
                sqlx::query(
                    "update ai_binding
                     set workspace_id = $2, library_id = $3, updated_at = now()
                     where id = $1",
                )
                .bind(second_binding_id)
                .bind(workspace_id)
                .bind(first_library_id)
                .execute(&pool)
                .await
            })
        };
        barrier.wait().await;

        tokio::time::timeout(Duration::from_secs(10), async {
            first_move.await??;
            second_move.await??;
            Ok::<(), anyhow::Error>(())
        })
        .await
        .context("opposite scope moves timed out while locking generation rows")??;

        assert!(source_generation(&pool, fixture.library_id).await? > first_before);
        assert!(source_generation(&pool, second_library.id).await? > second_before);
        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn binding_update_and_catalog_delete_share_parent_child_lock_order() -> Result<()> {
    let pool = connect_postgres().await?;
    let fixture = Fixture::create(&pool).await?;

    let barrier = Arc::new(Barrier::new(3));
    let update = {
        let pool = pool.clone();
        let barrier = Arc::clone(&barrier);
        let binding_id = fixture.binding_id;
        tokio::spawn(async move {
            barrier.wait().await;
            sqlx::query(
                "update ai_binding
                 set system_prompt = 'delete-race-prompt', updated_at = now()
                 where id = $1",
            )
            .bind(binding_id)
            .execute(&pool)
            .await
        })
    };
    let delete = {
        let pool = pool.clone();
        let barrier = Arc::clone(&barrier);
        let library_id = fixture.library_id;
        tokio::spawn(async move {
            barrier.wait().await;
            catalog_repository::delete_library(&pool, library_id).await
        })
    };
    barrier.wait().await;

    let delete_outcome = tokio::time::timeout(Duration::from_secs(10), async {
        update.await??;
        Ok::<_, anyhow::Error>(delete.await??)
    })
    .await
    .context("binding update and catalog delete timed out on parent/child locks")??;
    assert_eq!(delete_outcome, catalog_repository::CatalogLibraryDeleteOutcome::Deleted);

    fixture.cleanup(&pool).await?;
    Ok(())
}
