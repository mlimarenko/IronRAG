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
        // Re-run the canonical seed exactly as a startup authority does, so the
        // manifest reconciliation retires seed-owned rows it no longer carries.
        // This is the ordinary bootstrap path; it carries no financial setup.
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
async fn financial_admission_charges_every_attempt_and_never_refunds() -> Result<()> {
    use ironrag_backend::infra::repositories::ai_repository::{
        FinancialAdmission, FinancialAdmissionState, FinancialModelAllowance,
        FinancialRequestEnvelope, RequiredFinancialScope, UpdateProviderCatalog,
        consume_financial_attempt, get_provider_catalog_by_id, update_provider_catalog,
    };

    let fixture = BootstrapStackFixture::create().await?;
    let result = async {
        let postgres = &fixture.state.persistence.postgres;
        let provider_id: Uuid =
            sqlx::query_scalar("select id from ai_provider_catalog where provider_kind = 'openai'")
                .fetch_one(postgres)
                .await?;
        let scope_id = Uuid::new_v4();
        let envelope = FinancialRequestEnvelope::Chat {
            allowed_service_tiers: vec!["default".into(), "flex".into()],
            max_input_bytes: 65_536,
            max_output_tokens: 128_000,
            max_input_tokens: 922_000,
        };
        let mut admission = FinancialAdmission {
            version: 1,
            provider_id,
            scope_id,
            policy_fingerprint: String::new(),
            cutoff: chrono::Utc::now() + chrono::Duration::hours(1),
            state: FinancialAdmissionState::Active,
            grant_microusd: 10_000,
            global_limit: 10,
            global_consumed: 0,
            history: Vec::new(),
            models: std::collections::BTreeMap::from([(
                "gpt-6-luna".to_string(),
                FinancialModelAllowance {
                    endpoint: "https://api.openai.com/v1/chat/completions".to_string(),
                    envelope,
                    attempt_microusd: 1_000,
                    limit: 10,
                    consumed: 0,
                },
            )]),
        };
        admission.policy_fingerprint = admission.immutable_fingerprint()?;
        let mut predecessor = admission.clone();
        predecessor.scope_id = Uuid::new_v4();
        predecessor.state = FinancialAdmissionState::Closed;
        predecessor.global_consumed = 1;
        predecessor.models.get_mut("gpt-6-luna").unwrap().consumed = 1;
        predecessor.policy_fingerprint = predecessor.immutable_fingerprint()?;
        admission.history.push(predecessor.into());
        let scope = RequiredFinancialScope {
            provider_id,
            scope_id,
            policy_fingerprint: admission.policy_fingerprint.clone(),
        };
        write_financial_admission(postgres, provider_id, &admission).await?;
        let before_seed = get_provider_catalog_by_id(postgres, provider_id).await?.unwrap();
        update_provider_catalog(
            postgres,
            UpdateProviderCatalog {
                provider_id,
                provider_kind: &before_seed.provider_kind,
                display_name: "Seed conflict fixture",
                api_style: &before_seed.api_style,
                lifecycle_state: &before_seed.lifecycle_state,
                default_base_url: before_seed.default_base_url.as_deref(),
                capability_flags_json: None,
            },
        )
        .await?
        .unwrap();
        seed_clean_v1_ai_catalog(postgres).await?;
        let after_seed = get_provider_catalog_by_id(postgres, provider_id).await?.unwrap();
        let seeded: FinancialAdmission =
            serde_json::from_value(after_seed.capability_flags_json["financialAdmission"].clone())?;
        assert_eq!(seeded, admission);
        let body = serde_json::json!({
            "model": "gpt-6-luna",
            "messages": [{"role": "user", "content": "hello"}],
            "service_tier": "flex",
            "n": 1,
            "max_completion_tokens": 1024,
        });
        let endpoint = "https://api.openai.com/v1/chat/completions";

        // One admitted attempt, then an application retry: two units.
        consume_financial_attempt(postgres, &scope, "gpt-6-luna", endpoint, &body).await?;
        consume_financial_attempt(postgres, &scope, "gpt-6-luna", endpoint, &body).await?;

        let row = get_provider_catalog_by_id(postgres, provider_id).await?.expect("provider row");
        let stored = serde_json::from_value::<FinancialAdmission>(
            row.capability_flags_json["financialAdmission"].clone(),
        )?;
        assert_eq!(stored.global_consumed, 2, "each admitted attempt consumes one unit");
        assert_eq!(stored.models["gpt-6-luna"].consumed, 2);
        assert_eq!(stored.history, admission.history);

        // Re-reading the durable row after an attempt completes changes nothing:
        // there is no refund on success, failure, or an unknown outcome.
        let after_success =
            get_provider_catalog_by_id(postgres, provider_id).await?.expect("provider row");
        let stored_after = serde_json::from_value::<FinancialAdmission>(
            after_success.capability_flags_json["financialAdmission"].clone(),
        )?;
        assert_eq!(stored_after.global_consumed, 2);
        assert_eq!(stored_after.models["gpt-6-luna"].consumed, 2);

        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn financial_admission_denials_consume_no_attempt() -> Result<()> {
    use ironrag_backend::infra::repositories::ai_repository::{
        FinancialAdmission, FinancialAdmissionError, FinancialAdmissionState,
        FinancialModelAllowance, FinancialRequestEnvelope, RequiredFinancialScope,
        consume_financial_attempt, get_provider_catalog_by_id,
    };

    let fixture = BootstrapStackFixture::create().await?;
    let result = async {
        let postgres = &fixture.state.persistence.postgres;
        let provider_id: Uuid = sqlx::query_scalar(
            "select id from ai_provider_catalog where provider_kind = 'openai'",
        )
        .fetch_one(postgres)
        .await?;
        let scope_id = Uuid::new_v4();
        let envelope = FinancialRequestEnvelope::Chat {
            allowed_service_tiers: vec!["default".into(), "flex".into()],
            max_input_bytes: 65_536,
            max_output_tokens: 128_000,
            max_input_tokens: 922_000,
        };
        let base = FinancialAdmission {
            version: 1,
            provider_id,
            scope_id,
            policy_fingerprint: String::new(),
            cutoff: chrono::Utc::now() + chrono::Duration::hours(1),
            state: FinancialAdmissionState::Active,
            grant_microusd: 5_000,
            global_limit: 5,
            global_consumed: 0,
            history: Vec::new(),
            models: std::collections::BTreeMap::from([(
                "gpt-6-luna".to_string(),
                FinancialModelAllowance {
                    endpoint: "https://api.openai.com/v1/chat/completions".to_string(),
                    envelope,
                    attempt_microusd: 1_000,
                    limit: 5,
                    consumed: 0,
                },
            )]),
        };
        let mut base = base;
        base.policy_fingerprint = base.immutable_fingerprint()?;
        let scope = RequiredFinancialScope {
            provider_id,
            scope_id,
            policy_fingerprint: base.policy_fingerprint.clone(),
        };
        let body = serde_json::json!({
            "model": "gpt-6-luna",
            "messages": [{"role": "user", "content": "hello"}],
            "service_tier": "default",
            "n": 1,
            "max_completion_tokens": 1024,
        });
        let endpoint = "https://api.openai.com/v1/chat/completions";

        // Each denial must leave the persisted counters untouched.
        let deny = |admission: Option<&FinancialAdmission>, scope: &RequiredFinancialScope| {
            let admission = admission.cloned();
            let scope = scope.clone();
            let body = body.clone();
            async move {
                let expected_global = admission.as_ref().map_or(0, |value| value.global_consumed);
                let expected_model = admission
                    .as_ref()
                    .and_then(|value| value.models.get("gpt-6-luna"))
                    .map_or(0, |model| model.consumed);
                match admission {
                    Some(admission) => {
                        write_financial_admission(postgres, provider_id, &admission).await?;
                    }
                    None => {
                        sqlx::query(
                            "update ai_provider_catalog set capability_flags_json = capability_flags_json - 'financialAdmission' where id = $1",
                        )
                        .bind(provider_id)
                        .execute(postgres)
                        .await?;
                    }
                }
                let outcome =
                    consume_financial_attempt(postgres, &scope, "gpt-6-luna", endpoint, &body)
                        .await;
                assert!(outcome.is_err(), "the attempt must be refused");
                let row = get_provider_catalog_by_id(postgres, provider_id)
                    .await?
                    .expect("provider row");
                let persisted = row.capability_flags_json.get("financialAdmission");
                if let Some(value) = persisted {
                    let stored = serde_json::from_value::<FinancialAdmission>(value.clone())?;
                    assert_eq!(
                        stored.global_consumed, expected_global,
                        "a refusal must not consume"
                    );
                    assert_eq!(stored.models["gpt-6-luna"].consumed, expected_model);
                }
                Ok::<(), anyhow::Error>(())
            }
        };

        // Missing state.
        deny(None, &scope).await?;

        // Corrupt payload.
        let corrupt = serde_json::json!({"version": 1, "providerId": provider_id});
        sqlx::query(
            "update ai_provider_catalog set capability_flags_json = jsonb_set(capability_flags_json, '{financialAdmission}', $2::jsonb, true) where id = $1",
        )
        .bind(provider_id)
        .bind(corrupt)
        .execute(postgres)
        .await?;
        let outcome =
            consume_financial_attempt(postgres, &scope, "gpt-6-luna", endpoint, &body).await;
        assert!(outcome.is_err());

        // Mismatched fingerprint.
        let mut mismatched = base.clone();
        mismatched.policy_fingerprint = "sha256".to_string();
        deny(Some(&mismatched), &scope).await?;

        // Prepared and closed states.
        let mut prepared = base.clone();
        prepared.state = FinancialAdmissionState::Prepared;
        deny(Some(&prepared), &scope).await?;
        let mut closed = base.clone();
        closed.state = FinancialAdmissionState::Closed;
        deny(Some(&closed), &scope).await?;

        // Expired.
        let mut expired = base.clone();
        expired.cutoff = chrono::Utc::now() - chrono::Duration::seconds(1);
        expired.policy_fingerprint = expired.immutable_fingerprint()?;
        let expired_scope = RequiredFinancialScope {
            provider_id,
            scope_id,
            policy_fingerprint: expired.policy_fingerprint.clone(),
        };
        deny(Some(&expired), &expired_scope).await?;

        // Model exhaustion and global exhaustion. The documents stay internally
        // consistent (sum of model counters equals global_consumed), so the
        // refusal is exhaustion, not invalid state.
        let mut model_full = base.clone();
        model_full.models.get_mut("gpt-6-luna").expect("model").consumed = 5;
        model_full.global_consumed = 5;
        model_full.global_limit = 10;
        model_full.policy_fingerprint = model_full.immutable_fingerprint()?;
        let model_scope = RequiredFinancialScope {
            provider_id,
            scope_id,
            policy_fingerprint: model_full.policy_fingerprint.clone(),
        };
        deny(Some(&model_full), &model_scope).await?;
        let outcome =
            consume_financial_attempt(postgres, &model_scope, "gpt-6-luna", endpoint, &body).await;
        assert!(matches!(outcome, Err(FinancialAdmissionError::Exhausted)));
        let mut global_full = base.clone();
        global_full.models.get_mut("gpt-6-luna").expect("model").consumed = 5;
        global_full.global_consumed = 5;
        deny(Some(&global_full), &scope).await?;
        let outcome =
            consume_financial_attempt(postgres, &scope, "gpt-6-luna", endpoint, &body).await;
        assert!(matches!(outcome, Err(FinancialAdmissionError::Exhausted)));

        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn successor_admission_activates_through_catalog_write() -> Result<()> {
    use ironrag_backend::infra::repositories::ai_repository::{
        FinancialAdmission, FinancialAdmissionState, FinancialModelAllowance,
        FinancialRequestEnvelope, RequiredFinancialScope, UpdateProviderCatalog,
        get_provider_catalog_by_id, update_provider_catalog_with_scope,
    };

    let fixture = BootstrapStackFixture::create().await?;
    let result = async {
        let postgres = &fixture.state.persistence.postgres;
        let provider_id: Uuid =
            sqlx::query_scalar("select id from ai_provider_catalog where provider_kind = 'openai'")
                .fetch_one(postgres)
                .await?;
        let before =
            get_provider_catalog_by_id(postgres, provider_id).await?.expect("provider row");
        let now = chrono::Utc::now();
        let envelope = FinancialRequestEnvelope::Chat {
            allowed_service_tiers: vec!["default".into(), "flex".into()],
            max_input_bytes: 65_536,
            max_output_tokens: 128_000,
            max_input_tokens: 922_000,
        };
        let mut predecessor = FinancialAdmission {
            version: 1,
            provider_id,
            scope_id: Uuid::new_v4(),
            policy_fingerprint: String::new(),
            cutoff: now + chrono::Duration::hours(1),
            state: FinancialAdmissionState::Closed,
            grant_microusd: 10_000,
            global_limit: 10,
            global_consumed: 1,
            history: Vec::new(),
            models: std::collections::BTreeMap::from([(
                "gpt-6-luna".to_string(),
                FinancialModelAllowance {
                    endpoint: "https://api.openai.com/v1/chat/completions".to_string(),
                    envelope,
                    attempt_microusd: 1_000,
                    limit: 10,
                    consumed: 1,
                },
            )]),
        };
        predecessor.policy_fingerprint = predecessor.immutable_fingerprint()?;
        write_financial_admission(postgres, provider_id, &predecessor).await?;

        let mut successor = predecessor.clone();
        successor.scope_id = Uuid::new_v4();
        successor.state = FinancialAdmissionState::Prepared;
        successor.global_consumed = 0;
        successor.models.get_mut("gpt-6-luna").expect("model").consumed = 0;
        successor.cutoff = now + chrono::Duration::hours(2);
        successor.history.clear();
        successor.policy_fingerprint = successor.immutable_fingerprint()?;
        let scope = RequiredFinancialScope {
            provider_id,
            scope_id: successor.scope_id,
            policy_fingerprint: successor.policy_fingerprint.clone(),
        };

        let mut profile = before.capability_flags_json.clone();
        profile["financialAdmission"] = serde_json::to_value(&successor)?;
        let stored = update_provider_catalog_with_scope(
            postgres,
            UpdateProviderCatalog {
                provider_id,
                provider_kind: &before.provider_kind,
                display_name: &before.display_name,
                api_style: &before.api_style,
                lifecycle_state: &before.lifecycle_state,
                default_base_url: before.default_base_url.as_deref(),
                capability_flags_json: Some(profile),
            },
            Some(&scope),
        )
        .await?
        .expect("provider row");
        let prepared: FinancialAdmission =
            serde_json::from_value(stored.capability_flags_json["financialAdmission"].clone())?;
        assert_eq!(prepared.state, FinancialAdmissionState::Prepared);
        assert_eq!(prepared.scope_id, successor.scope_id);
        assert_eq!(prepared.history.len(), 1);
        assert_eq!(prepared.history[0].scope_id, predecessor.scope_id);
        assert_eq!(prepared.history[0].state, FinancialAdmissionState::Closed);
        assert_eq!(prepared.history[0].global_consumed, 1);

        // The API response does not expose financialAdmission, so activation
        // must work without echoing the server-owned history.
        let mut active = successor.clone();
        active.state = FinancialAdmissionState::Active;
        active.history.clear();
        let mut profile = before.capability_flags_json.clone();
        profile["financialAdmission"] = serde_json::to_value(&active)?;
        let stored = update_provider_catalog_with_scope(
            postgres,
            UpdateProviderCatalog {
                provider_id,
                provider_kind: &before.provider_kind,
                display_name: &before.display_name,
                api_style: &before.api_style,
                lifecycle_state: &before.lifecycle_state,
                default_base_url: before.default_base_url.as_deref(),
                capability_flags_json: Some(profile),
            },
            Some(&scope),
        )
        .await?
        .expect("provider row");
        let activated: FinancialAdmission =
            serde_json::from_value(stored.capability_flags_json["financialAdmission"].clone())?;
        assert_eq!(activated.state, FinancialAdmissionState::Active);
        assert_eq!(activated.history, prepared.history);
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

async fn write_financial_admission(
    postgres: &PgPool,
    provider_id: Uuid,
    admission: &ironrag_backend::infra::repositories::ai_repository::FinancialAdmission,
) -> Result<()> {
    let value = serde_json::to_value(admission)?;
    sqlx::query(
        "update ai_provider_catalog set capability_flags_json = jsonb_set(capability_flags_json, '{financialAdmission}', $2::jsonb, true) where id = $1",
    )
    .bind(provider_id)
    .bind(value)
    .execute(postgres)
    .await?;
    Ok(())
}

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
