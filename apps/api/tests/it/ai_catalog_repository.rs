//! PostgreSQL integration coverage for AI catalog and binding repositories.
//!
//! Run with:
//! `cargo test -p ironrag-backend --test it -- --include-ignored ai_catalog_repository::`

use crate::temp_postgres_database;

use anyhow::{Context as _, Result};
use ironrag_backend::{
    app::config::Settings,
    infra::repositories::{ai_repository, catalog_repository},
};
use sqlx::{PgPool, postgres::PgPoolOptions};
use temp_postgres_database::TempPostgresDatabase;
use uuid::Uuid;

const TEST_DATABASE_MAX_CONNECTIONS: u32 = 2;

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn account_update_rejects_a_stale_editor_timestamp() -> Result<()> {
    let settings = Settings::from_env()?;
    let temp_database =
        TempPostgresDatabase::create(&settings.database_url, "ai_account_update_fence").await?;
    let pool = PgPoolOptions::new()
        .max_connections(TEST_DATABASE_MAX_CONNECTIONS)
        .connect(&temp_database.database_url)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;

    let result = async {
        let suffix = Uuid::now_v7().simple();
        let provider = ai_repository::create_provider_catalog(
            &pool,
            ai_repository::NewProviderCatalog {
                provider_kind: &format!("account-fence-provider-{suffix}"),
                display_name: "Account Fence Provider",
                api_style: "openai_compatible",
                lifecycle_state: "active",
                default_base_url: None,
                capability_flags_json: serde_json::json!({}),
            },
        )
        .await?;
        let account_id = Uuid::now_v7();
        let original = ai_repository::create_account(
            &pool,
            ai_repository::NewAiAccount {
                account_id,
                scope_kind: "instance",
                workspace_id: None,
                library_id: None,
                provider_catalog_id: provider.id,
                label: "Original account",
                base_url: None,
                created_by_principal_id: None,
            },
            None,
        )
        .await?;

        let changed = sqlx::query(
            "update ai_account
             set credential_state = 'revoked',
                 updated_at = updated_at + interval '1 second'
             where id = $1",
        )
        .bind(account_id)
        .execute(&pool)
        .await?;
        assert_eq!(changed.rows_affected(), 1, "fixture must revoke exactly one account");

        let stale_update = ai_repository::update_account(
            &pool,
            ai_repository::AiAccountUpdate {
                account_id,
                label: "Stale editor label",
                base_url: None,
                credential_state: "active",
                expected_updated_at: original.updated_at,
            },
            None,
        )
        .await?;
        assert!(stale_update.is_none(), "stale editor must lose the optimistic update race");

        let current = ai_repository::get_account_by_id(&pool, account_id)
            .await?
            .context("account must still exist after rejected stale update")?;
        assert_eq!(current.label, "Original account");
        assert_eq!(current.credential_state, "revoked");
        Ok(())
    }
    .await;

    pool.close().await;
    temp_database.drop().await?;
    result
}

fn zero_pricing_binds() -> ai_repository::ModelPricingBinds {
    ai_repository::ModelPricingBinds {
        currency_code: "USD".to_string(),
        input_price_per_1m: rust_decimal::Decimal::ZERO,
        cached_input_price_per_1m: rust_decimal::Decimal::ZERO,
        cache_write_input_price_per_1m: rust_decimal::Decimal::ZERO,
        output_price_per_1m: rust_decimal::Decimal::ZERO,
        long_context_threshold_tokens: None,
        long_context_input_price_per_1m: None,
        long_context_cached_input_price_per_1m: None,
        long_context_output_price_per_1m: None,
    }
}

async fn create_library_agent_binding(
    pool: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    provider_catalog_id: Uuid,
    model_catalog_id: Uuid,
) -> Result<()> {
    let account_id = Uuid::now_v7();
    sqlx::query(
        "insert into ai_account (
            id, scope_kind, workspace_id, library_id, provider_catalog_id,
            label, api_key, credential_state
         ) values ($1, 'library', $2, $3, $4, 'Vision fixture account', null, 'active')",
    )
    .bind(account_id)
    .bind(workspace_id)
    .bind(library_id)
    .bind(provider_catalog_id)
    .execute(pool)
    .await?;

    ai_repository::create_binding(
        pool,
        ai_repository::CreateAiBindingInput {
            scope_kind: "library",
            workspace_id: Some(workspace_id),
            library_id: Some(library_id),
            binding_purpose: "agent",
            account_id,
            provider_catalog_id,
            model_catalog_id,
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens_override: None,
            extra_parameters_json: serde_json::json!({}),
            binding_state: "active",
            updated_by_principal_id: None,
        },
    )
    .await?;
    Ok(())
}

async fn per_library_agent_binding_supports_vision(
    pool: &PgPool,
    library_ids: &[Uuid],
) -> Result<bool> {
    for library_id in library_ids {
        let Some(binding) =
            ai_repository::get_effective_runtime_binding(pool, *library_id, "agent").await?
        else {
            continue;
        };
        let model = ai_repository::get_model_catalog_by_id(pool, binding.model.model_catalog_id)
            .await?
            .context("effective Agent binding model must exist")?;
        if model.allowed_binding_purposes.iter().any(|purpose| purpose == "extract_text") {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn assert_vision_batch_matches_per_library(
    pool: &PgPool,
    library_ids: &[Uuid],
) -> Result<()> {
    let expected = per_library_agent_binding_supports_vision(pool, library_ids).await?;
    let actual = ai_repository::any_effective_binding_model_supports_purpose(
        pool,
        library_ids,
        "agent",
        "extract_text",
    )
    .await?;
    assert_eq!(actual, expected);
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn batched_agent_vision_query_matches_per_library_resolution() -> Result<()> {
    let settings = Settings::from_env()?;
    let temp_database =
        TempPostgresDatabase::create(&settings.database_url, "ai_vision_binding_batch").await?;
    let pool = PgPoolOptions::new()
        .max_connections(TEST_DATABASE_MAX_CONNECTIONS)
        .connect(&temp_database.database_url)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;

    let suffix = Uuid::now_v7().simple();
    let provider = ai_repository::create_provider_catalog(
        &pool,
        ai_repository::NewProviderCatalog {
            provider_kind: &format!("vision-provider-{suffix}"),
            display_name: "Vision Batch Provider",
            api_style: "openai_compatible",
            lifecycle_state: "active",
            default_base_url: None,
            capability_flags_json: serde_json::json!({}),
        },
    )
    .await?;
    let vision_model = ai_repository::create_model_catalog(
        &pool,
        ai_repository::NewModelCatalog {
            provider_catalog_id: provider.id,
            model_name: "vision-model",
            context_window: None,
            max_output_tokens: None,
            lifecycle_state: "active",
            metadata_json: serde_json::json!({}),
            pricing: &zero_pricing_binds(),
            allowed_binding_purposes: &["agent".to_string(), "extract_text".to_string()],
        },
    )
    .await?;
    let text_model = ai_repository::create_model_catalog(
        &pool,
        ai_repository::NewModelCatalog {
            provider_catalog_id: provider.id,
            model_name: "text-model",
            context_window: None,
            max_output_tokens: None,
            lifecycle_state: "active",
            metadata_json: serde_json::json!({}),
            pricing: &zero_pricing_binds(),
            allowed_binding_purposes: &["agent".to_string()],
        },
    )
    .await?;
    let workspace = catalog_repository::create_workspace(
        &pool,
        &format!("vision-workspace-{suffix}"),
        "Vision Batch Workspace",
        None,
    )
    .await?;
    let vision_library = catalog_repository::create_library(
        &pool,
        workspace.id,
        &format!("vision-library-{suffix}"),
        "Vision Library",
        None,
        None,
    )
    .await?;
    let text_library = catalog_repository::create_library(
        &pool,
        workspace.id,
        &format!("text-library-{suffix}"),
        "Text Library",
        None,
        None,
    )
    .await?;
    create_library_agent_binding(
        &pool,
        workspace.id,
        vision_library.id,
        provider.id,
        vision_model.id,
    )
    .await?;
    create_library_agent_binding(&pool, workspace.id, text_library.id, provider.id, text_model.id)
        .await?;

    assert_vision_batch_matches_per_library(&pool, &[]).await?;
    assert_vision_batch_matches_per_library(&pool, &[vision_library.id]).await?;
    assert_vision_batch_matches_per_library(&pool, &[text_library.id]).await?;
    assert_vision_batch_matches_per_library(&pool, &[text_library.id, vision_library.id]).await?;

    pool.close().await;
    temp_database.drop().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn catalog_and_binding_repositories_preserve_catalog_and_binding_invariants() -> Result<()> {
    let settings = Settings::from_env()?;
    let temp_database =
        TempPostgresDatabase::create(&settings.database_url, "ai_catalog_repository").await?;
    let pool = PgPoolOptions::new()
        .max_connections(TEST_DATABASE_MAX_CONNECTIONS)
        .connect(&temp_database.database_url)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;

    let suffix = Uuid::now_v7().simple();
    let provider = ai_repository::create_provider_catalog(
        &pool,
        ai_repository::NewProviderCatalog {
            provider_kind: &format!("provider-{suffix}"),
            display_name: "Repository Provider",
            api_style: "openai_compatible",
            lifecycle_state: "active",
            default_base_url: Some("https://provider.example/v1"),
            capability_flags_json: serde_json::json!({"capability": "synthetic"}),
        },
    )
    .await?;
    let disabled_provider = ai_repository::create_provider_catalog(
        &pool,
        ai_repository::NewProviderCatalog {
            provider_kind: &format!("disabled-provider-{suffix}"),
            display_name: "Disabled Repository Provider",
            api_style: "openai_compatible",
            lifecycle_state: "disabled",
            default_base_url: None,
            capability_flags_json: serde_json::json!({}),
        },
    )
    .await?;

    assert_eq!(
        ai_repository::get_provider_catalog_by_kind(&pool, &provider.provider_kind)
            .await?
            .context("active provider must be discoverable")?
            .id,
        provider.id
    );
    assert!(
        ai_repository::get_provider_catalog_by_kind(&pool, &disabled_provider.provider_kind)
            .await?
            .is_none(),
        "disabled providers must not be discovered by kind"
    );
    assert_eq!(ai_repository::list_provider_catalog(&pool).await?.len(), 2);

    let allowed_purposes = vec!["query_answer".to_string(), "query_answer".to_string()];
    let model = ai_repository::create_model_catalog(
        &pool,
        ai_repository::NewModelCatalog {
            provider_catalog_id: provider.id,
            model_name: "repository-model",
            context_window: Some(4_096),
            max_output_tokens: Some(512),
            lifecycle_state: "active",
            metadata_json: serde_json::json!({"synthetic": true}),
            pricing: &zero_pricing_binds(),
            allowed_binding_purposes: &allowed_purposes,
        },
    )
    .await?;
    assert_eq!(model.allowed_binding_purposes, ["query_answer"]);
    assert_eq!(
        ai_repository::get_model_catalog_by_provider_and_name(
            &pool,
            &provider.provider_kind,
            "repository-model",
        )
        .await?
        .context("active model must be discoverable by provider and name")?
        .id,
        model.id
    );

    let updated_purposes = vec!["agent".to_string(), "agent".to_string()];
    let updated_model = ai_repository::update_model_catalog(
        &pool,
        ai_repository::UpdateModelCatalog {
            model_id: model.id,
            context_window: None,
            max_output_tokens: Some(1_024),
            lifecycle_state: "disabled",
            metadata_json: serde_json::json!({"replaced": true}),
            pricing: &zero_pricing_binds(),
            allowed_binding_purposes: &updated_purposes,
        },
    )
    .await?
    .context("existing model must update")?;
    assert_eq!(updated_model.allowed_binding_purposes, ["agent"]);
    assert!(
        ai_repository::get_model_catalog_by_provider_and_name(
            &pool,
            &provider.provider_kind,
            "repository-model",
        )
        .await?
        .is_none(),
        "disabled models must not be discoverable"
    );
    assert_eq!(
        ai_repository::list_model_catalog(&pool, Some(provider.id)).await?.len(),
        1,
        "provider filter must not include models owned by other providers"
    );
    assert!(
        ai_repository::update_model_catalog(
            &pool,
            ai_repository::UpdateModelCatalog {
                model_id: Uuid::now_v7(),
                context_window: None,
                max_output_tokens: None,
                lifecycle_state: "active",
                metadata_json: serde_json::json!({}),
                pricing: &zero_pricing_binds(),
                allowed_binding_purposes: &updated_purposes,
            }
        )
        .await?
        .is_none(),
        "updating an unknown model must not create one"
    );

    let workspace = catalog_repository::create_workspace(
        &pool,
        &format!("ai-catalog-{suffix}"),
        "AI Catalog Repository Workspace",
        None,
    )
    .await?;
    let library = catalog_repository::create_library(
        &pool,
        workspace.id,
        &format!("ai-catalog-library-{suffix}"),
        "AI Catalog Repository Library",
        None,
        None,
    )
    .await?;
    let active_model = ai_repository::update_model_catalog(
        &pool,
        ai_repository::UpdateModelCatalog {
            model_id: model.id,
            context_window: None,
            max_output_tokens: Some(1_024),
            lifecycle_state: "active",
            metadata_json: serde_json::json!({"replaced": true}),
            pricing: &zero_pricing_binds(),
            allowed_binding_purposes: &allowed_purposes,
        },
    )
    .await?
    .context("model must reactivate for the binding fixture")?;
    let account_id = Uuid::now_v7();
    sqlx::query(
        "insert into ai_account (
            id, scope_kind, workspace_id, library_id, provider_catalog_id,
            label, api_key, credential_state
         ) values ($1, 'library', $2, $3, $4, 'Fixture account', 'opaque-test-value', 'active')",
    )
    .bind(account_id)
    .bind(workspace.id)
    .bind(library.id)
    .bind(provider.id)
    .execute(&pool)
    .await?;

    let binding = ai_repository::create_binding(
        &pool,
        ai_repository::CreateAiBindingInput {
            scope_kind: "library",
            workspace_id: Some(workspace.id),
            library_id: Some(library.id),
            binding_purpose: "query_answer",
            account_id,
            provider_catalog_id: provider.id,
            model_catalog_id: active_model.id,
            system_prompt: Some("Use supplied evidence."),
            temperature: Some(0.2),
            top_p: Some(0.9),
            max_output_tokens_override: Some(256),
            extra_parameters_json: serde_json::json!({"synthetic": true}),
            binding_state: "active",
            updated_by_principal_id: None,
        },
    )
    .await?;
    assert_eq!(
        ai_repository::get_effective_binding_by_purpose(&pool, library.id, "query_answer")
            .await?
            .context("library binding must resolve")?
            .id,
        binding.id
    );

    let updated_binding = ai_repository::update_binding(
        &pool,
        ai_repository::UpdateAiBindingInput {
            binding_id: binding.id,
            account_id,
            provider_catalog_id: provider.id,
            model_catalog_id: active_model.id,
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens_override: None,
            extra_parameters_json: serde_json::json!({"updated": true}),
            binding_state: "disabled",
            updated_by_principal_id: None,
        },
    )
    .await?
    .context("existing binding must update")?;
    assert_eq!(updated_binding.binding_state, "disabled");
    assert!(
        ai_repository::get_effective_binding_by_purpose(&pool, library.id, "query_answer")
            .await?
            .is_none(),
        "disabled bindings must not resolve"
    );
    assert!(
        ai_repository::update_binding(
            &pool,
            ai_repository::UpdateAiBindingInput {
                binding_id: Uuid::now_v7(),
                account_id,
                provider_catalog_id: provider.id,
                model_catalog_id: active_model.id,
                system_prompt: None,
                temperature: None,
                top_p: None,
                max_output_tokens_override: None,
                extra_parameters_json: serde_json::json!({}),
                binding_state: "active",
                updated_by_principal_id: None,
            },
        )
        .await?
        .is_none(),
        "updating an unknown binding must not create one"
    );

    let validation = ai_repository::create_binding_validation(
        &pool,
        binding.id,
        "failed",
        Some("synthetic"),
        None,
    )
    .await?;
    assert_eq!(
        ai_repository::get_binding_validation_by_id(&pool, validation.id)
            .await?
            .context("binding validation must be persisted")?
            .failure_code
            .as_deref(),
        Some("synthetic")
    );
    assert_eq!(ai_repository::list_binding_validations(&pool, binding.id).await?.len(), 1);
    assert!(ai_repository::delete_binding(&pool, binding.id).await?);
    assert!(!ai_repository::delete_binding(&pool, binding.id).await?);

    pool.close().await;
    temp_database.drop().await?;
    Ok(())
}
