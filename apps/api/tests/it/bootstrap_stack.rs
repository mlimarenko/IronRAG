use crate::temp_postgres_database;

use anyhow::{Context, Result};
use sqlx::PgPool;
use uuid::Uuid;

use temp_postgres_database::TempPostgresDatabase;

use ironrag_backend::{
    app::{
        config::{
            Settings, UiBootstrapAiBindingDefault, UiBootstrapAiProviderSecret, UiBootstrapAiSetup,
        },
        seed_clean_v1_ai_catalog,
        state::AppState,
    },
    domains::ai::AiBindingPurpose,
    infra::{
        persistence::{
            required_ai_catalog_seeded, required_baseline_present, run_postgres_migrations,
            validate_required_bootstrap_state,
        },
        repositories::catalog_repository,
    },
};

// Twelve providers and seventy models since IRG-182 added the TypeSafe Jev
// decision provider with its single `jev-latest` model. The numbers are pinned
// rather than derived because a bootstrap that silently seeds fewer rows than
// the release intended is exactly the state this test exists to catch.
const SEEDED_PROVIDER_COUNT: i64 = 12;
const SEEDED_MODEL_COUNT: i64 = 76;

struct BootstrapStackFixture {
    state: AppState,
    temp_database: TempPostgresDatabase,
}

impl BootstrapStackFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for bootstrap-stack test")?;
        let temp_database =
            TempPostgresDatabase::create(&settings.database_url, "bootstrap_stack").await?;
        settings.database_url = temp_database.database_url.clone();
        settings.destructive_fresh_bootstrap_required = true;
        settings.credential_master_key =
            Some("KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=".to_string());

        let state = AppState::new(settings).await?;
        // `AppState` owns dependency construction; the startup role owns
        // schema/catalog authority. Exercise that same ordered authority path
        // explicitly without starting a network listener in this integration
        // test.
        run_postgres_migrations(&state.persistence.postgres).await?;
        seed_clean_v1_ai_catalog(&state.persistence.postgres).await?;
        validate_required_bootstrap_state(&state.persistence.postgres, &state.settings).await?;
        Ok(Self { state, temp_database })
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_database.drop().await
    }
}

async fn scalar_count(postgres: &PgPool, table_name: &str) -> Result<i64> {
    sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!("select count(*) from {table_name}")))
        .fetch_one(postgres)
        .await
        .with_context(|| format!("failed to count rows in {table_name}"))
}

async fn table_exists(postgres: &PgPool, table_name: &str) -> Result<bool> {
    sqlx::query_scalar::<_, Option<String>>("select to_regclass($1)::text")
        .bind(table_name)
        .fetch_one(postgres)
        .await
        .with_context(|| format!("failed to check table {table_name}"))
        .map(|table| table.is_some())
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn fresh_startup_bootstraps_postgres_catalog_and_knowledge_plane() -> Result<()> {
    let fixture = BootstrapStackFixture::create().await?;

    let result = async {
        assert!(required_baseline_present(&fixture.state.persistence.postgres).await?);
        assert!(required_ai_catalog_seeded(&fixture.state.persistence.postgres).await?);
        assert_eq!(
            scalar_count(&fixture.state.persistence.postgres, "ai_provider_catalog").await?,
            SEEDED_PROVIDER_COUNT
        );
        assert_eq!(
            scalar_count(&fixture.state.persistence.postgres, "ai_model_catalog").await?,
            SEEDED_MODEL_COUNT
        );
        // The seed retires its own rows that the manifest no longer lists by
        // marking them deprecated. A fresh database has none, so a deprecated
        // row here would mean the retirement step hit a row the manifest still
        // carries. Manifest rows keep their declared state (`jev-latest` ships
        // as `preview`).
        let deprecated_models = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from ai_model_catalog where lifecycle_state = 'deprecated'",
        )
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(deprecated_models, 0, "a fresh seed retires no manifest row");
        // The two entries added with the priced router sets: Qwen as a direct
        // hosted provider, LiteLLM as the self-hosted gateway. Both must exist
        // as rows, independent of what the model count happens to be.
        let extended_providers = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from ai_provider_catalog
             where provider_kind in ('qwen', 'litellm')",
        )
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(extended_providers, 2, "Qwen and LiteLLM provider rows must be seeded");

        // A rouble router row read as dollars multiplies every reported cost.
        // The row carries the listing's currency, not a hardcoded one.
        let router_currencies = sqlx::query_scalar::<_, String>(
            "select distinct model.currency_code
             from ai_model_catalog model
             join ai_provider_catalog provider on provider.id = model.provider_catalog_id
             where provider.provider_kind in ('openrouter', 'routerai')
             order by model.currency_code",
        )
        .fetch_all(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(
            router_currencies,
            vec!["RUB".to_string(), "USD".to_string()],
            "each router row must carry the currency its listing quotes"
        );
        // Pricing lives on the model row; the bootstrap default models must
        // carry real vendor rates so a fresh stack never reports paid usage
        // as costless.
        let priced_bootstrap_models = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from ai_model_catalog model
             join ai_provider_catalog provider on provider.id = model.provider_catalog_id
             where provider.provider_kind = 'openai'
               and model.model_name in ('gpt-5.4-nano', 'gpt-5.6-luna', 'text-embedding-3-large')
               and model.input_price_per_1m > 0",
        )
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(priced_bootstrap_models, 3);

        for table in [
            "knowledge_document",
            "knowledge_revision",
            "knowledge_chunk",
            "knowledge_vector_relation_manifest",
            "knowledge_entity",
            "knowledge_relation",
            "knowledge_context_bundle",
        ] {
            assert!(
                table_exists(&fixture.state.persistence.postgres, table).await?,
                "missing {table}"
            );
        }

        // Upgrade simulation: a row this build seeded in an earlier release,
        // and an operator's own row, both absent from the current manifest.
        // Re-seeding must retire the first and leave the second alone —
        // otherwise an upgraded instance keeps offering a stale model at a
        // stale price forever.
        let postgres = &fixture.state.persistence.postgres;
        sqlx::query(
            "insert into ai_model_catalog (id, provider_catalog_id, model_name, metadata_json)
             select gen_random_uuid(), id, 'claude-opus-4-8', '{\"seedSource\":\"clean_v1\"}'::jsonb
             from ai_provider_catalog
             where provider_kind = 'anthropic'",
        )
        .execute(postgres)
        .await?;
        sqlx::query(
            "insert into ai_model_catalog (id, provider_catalog_id, model_name, metadata_json)
             select gen_random_uuid(), id, 'operator-fine-tune', '{}'::jsonb
             from ai_provider_catalog
             where provider_kind = 'anthropic'",
        )
        .execute(postgres)
        .await?;
        seed_clean_v1_ai_catalog(postgres).await?;
        let retired = sqlx::query_scalar::<_, String>(
            "select model.lifecycle_state::text
             from ai_model_catalog model
             join ai_provider_catalog provider on provider.id = model.provider_catalog_id
             where provider.provider_kind = 'anthropic' and model.model_name = 'claude-opus-4-8'",
        )
        .fetch_one(postgres)
        .await?;
        assert_eq!(
            retired, "deprecated",
            "a seed-owned row missing from the manifest must be retired"
        );
        let operator_owned = sqlx::query_scalar::<_, String>(
            "select model.lifecycle_state::text
             from ai_model_catalog model
             join ai_provider_catalog provider on provider.id = model.provider_catalog_id
             where provider.provider_kind = 'anthropic' and model.model_name = 'operator-fine-tune'",
        )
        .fetch_one(postgres)
        .await?;
        assert_eq!(
            operator_owned, "active",
            "an operator's own row must survive re-seeding untouched"
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// A single-provider instance bootstrap AI setup: the openai secret plus the
/// six required purposes bound to that provider's seeded models. Drives
/// `apply_configured_bootstrap_ai_setup` exactly as the startup path does.
fn instance_bootstrap_ai_setup(api_key: &str) -> UiBootstrapAiSetup {
    UiBootstrapAiSetup {
        provider_secrets: vec![UiBootstrapAiProviderSecret {
            provider_kind: "openai".to_string(),
            api_key: api_key.to_string(),
        }],
        binding_defaults: [
            (AiBindingPurpose::ExtractText, "gpt-6-luna"),
            (AiBindingPurpose::ExtractGraph, "gpt-6-luna"),
            (AiBindingPurpose::EmbedChunk, "text-embedding-3-large"),
            (AiBindingPurpose::QueryCompile, "gpt-6-luna"),
            (AiBindingPurpose::QueryAnswer, "gpt-6-luna"),
            (AiBindingPurpose::Agent, "gpt-6-luna"),
        ]
        .into_iter()
        .map(|(binding_purpose, model_name)| UiBootstrapAiBindingDefault {
            binding_purpose,
            provider_kind: Some("openai".to_string()),
            model_name: Some(model_name.to_string()),
        })
        .collect(),
    }
}

/// Turn off outbound provider credential validation on the temp database, so
/// the startup bootstrap path creates accounts without a network call.
async fn disable_provider_credential_validation(postgres: &PgPool) -> Result<()> {
    sqlx::query(
        "update ai_provider_catalog
          set capability_flags_json = jsonb_set(
              capability_flags_json,
              '{credentials,validationMode}',
              '\"none\"'::jsonb,
              false
          )",
    )
    .execute(postgres)
    .await
    .context("failed to disable outbound provider validation in test fixture")?;
    Ok(())
}

async fn source_truth_version(postgres: &PgPool, library_id: Uuid) -> Result<i64> {
    Ok(catalog_repository::get_library_source_truth_version(postgres, library_id).await?)
}

async fn instance_account_updated_at(
    postgres: &PgPool,
    provider_kind: &str,
) -> Result<chrono::DateTime<chrono::Utc>> {
    sqlx::query_scalar::<_, chrono::DateTime<chrono::Utc>>(
        "select account.updated_at
         from ai_account account
         join ai_provider_catalog provider on provider.id = account.provider_catalog_id
         where account.scope_kind = 'instance' and provider.provider_kind = $1",
    )
    .bind(provider_kind)
    .fetch_one(postgres)
    .await
    .with_context(|| format!("failed to read instance {provider_kind} account updated_at"))
}

/// Replaying the instance bootstrap with an unchanged credential must not write
/// `ai_account`; a real credential change must. Before IRG-213 the account
/// UPDATE fired on every startup, which bumped `catalog_library.source_truth_version`
/// for every library and re-armed the GC queue the lease gate reads.
#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn bootstrap_ai_account_upsert_is_idempotent() -> Result<()> {
    let mut fixture = BootstrapStackFixture::create().await?;
    let result = async {
        let postgres = fixture.state.persistence.postgres.clone();
        disable_provider_credential_validation(&postgres).await?;
        let workspace = catalog_repository::create_workspace(
            &postgres,
            "bootstrap-idempotence",
            "Bootstrap Idempotence",
            None,
        )
        .await?;
        let library = catalog_repository::create_library(
            &postgres,
            workspace.id,
            "bootstrap-idempotence",
            "Bootstrap Idempotence",
            None,
            None,
        )
        .await?;

        fixture.state.ui_bootstrap_ai_setup =
            Some(instance_bootstrap_ai_setup("bootstrap-key-one"));
        fixture
            .state
            .application_services
            .ai_catalog
            .apply_configured_bootstrap_ai_setup(&fixture.state, workspace.id, library.id, None)
            .await
            .context("first bootstrap application")?;

        let updated_at_before = instance_account_updated_at(&postgres, "openai").await?;
        let version_before = source_truth_version(&postgres, library.id).await?;

        fixture
            .state
            .application_services
            .ai_catalog
            .apply_configured_bootstrap_ai_setup(&fixture.state, workspace.id, library.id, None)
            .await
            .context("second bootstrap application")?;
        assert_eq!(
            instance_account_updated_at(&postgres, "openai").await?,
            updated_at_before,
            "an unchanged bootstrap must not update ai_account"
        );
        assert_eq!(
            source_truth_version(&postgres, library.id).await?,
            version_before,
            "an unchanged bootstrap must not bump catalog_library.source_truth_version"
        );

        fixture.state.ui_bootstrap_ai_setup =
            Some(instance_bootstrap_ai_setup("bootstrap-key-two"));
        fixture
            .state
            .application_services
            .ai_catalog
            .apply_configured_bootstrap_ai_setup(&fixture.state, workspace.id, library.id, None)
            .await
            .context("credential-change bootstrap application")?;
        assert!(
            instance_account_updated_at(&postgres, "openai").await? > updated_at_before,
            "a changed credential must update ai_account"
        );
        assert!(
            source_truth_version(&postgres, library.id).await? > version_before,
            "a changed credential must bump catalog_library.source_truth_version"
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// Re-running the startup catalog seed with an unchanged manifest must not
/// UPDATE the seeded provider/model rows. Before IRG-213 the `on conflict do
/// update` wrote every row unconditionally, so the model update trigger bumped
/// `catalog_library.source_truth_version` for every library an active binding
/// reaches.
#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn catalog_seed_is_idempotent_for_unchanged_manifest() -> Result<()> {
    let fixture = BootstrapStackFixture::create().await?;
    let result = async {
        let postgres = &fixture.state.persistence.postgres;
        let workspace = catalog_repository::create_workspace(
            postgres,
            "seed-idempotence",
            "Seed Idempotence",
            None,
        )
        .await?;
        let library = catalog_repository::create_library(
            postgres,
            workspace.id,
            "seed-idempotence",
            "Seed Idempotence",
            None,
            None,
        )
        .await?;

        // An active instance binding to a seeded model makes every library
        // depend on that model row, so a seed UPDATE would bump this library.
        let (provider_id, model_id) = sqlx::query_as::<_, (Uuid, Uuid)>(
            "select provider.id, model.id
             from ai_model_catalog model
             join ai_provider_catalog provider on provider.id = model.provider_catalog_id
             where provider.provider_kind = 'openai' and model.model_name = 'gpt-6-luna'",
        )
        .fetch_one(postgres)
        .await
        .context("failed to locate a seeded openai model")?;
        let account_id = Uuid::now_v7();
        sqlx::query(
            "insert into ai_account (
                id, scope_kind, workspace_id, library_id, provider_catalog_id,
                label, api_key, base_url, credential_state
             ) values ($1, 'instance', null, null, $2, 'Seed Idempotence', null, null, 'active')",
        )
        .bind(account_id)
        .bind(provider_id)
        .execute(postgres)
        .await
        .context("failed to insert seed-idempotence instance account")?;
        sqlx::query(
            "insert into ai_binding (
                id, scope_kind, workspace_id, library_id, binding_purpose,
                account_id, provider_catalog_id, model_catalog_id, system_prompt,
                extra_parameters_json, binding_state
             ) values ($1, 'instance', null, null, 'query_answer', $2, $3, $4,
                       'prompt', '{}'::jsonb, 'active')",
        )
        .bind(Uuid::now_v7())
        .bind(account_id)
        .bind(provider_id)
        .bind(model_id)
        .execute(postgres)
        .await
        .context("failed to insert seed-idempotence instance binding")?;

        let version_before = source_truth_version(postgres, library.id).await?;
        seed_clean_v1_ai_catalog(postgres).await?;
        assert_eq!(
            source_truth_version(postgres, library.id).await?,
            version_before,
            "re-seeding an unchanged manifest must not bump catalog_library.source_truth_version"
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
