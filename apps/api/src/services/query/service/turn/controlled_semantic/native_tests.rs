//! Offline native lifecycle and binding checks. No provider dispatch is permitted.

use super::*;
use crate::{
    app::seed_clean_v1_ai_catalog,
    infra::repositories::{
        catalog_repository,
        iam_repository::{self, SystemRole},
    },
    interfaces::http::auth::{
        build_session_cookie_value, hash_session_secret, hash_token, mint_plaintext_session_secret,
    },
};
use base64::{Engine, engine::general_purpose::STANDARD};
use sqlx::PgPool;

struct Fixture {
    state: AppState,
    guard: Arc<ProviderReadGuard>,
    manifest: Manifest,
    admission: Admission,
    _directory: tempfile::TempDir,
}

impl Fixture {
    async fn new(pool: PgPool) -> Result<Self> {
        seed_clean_v1_ai_catalog(&pool).await?;
        let mut settings = Settings::from_env()?;
        settings.discard_credential_master_key();
        settings.credential_master_key = Some(STANDARD.encode([41_u8; 32]));
        let redis = redis::Client::open(settings.redis_url.clone())?;
        let mut state = AppState::from_dependencies(settings, Persistence::for_tests(pool, redis))?;
        let guard = Arc::new(ProviderReadGuard::default());
        state.llm_gateway = guard.clone();
        let pool = &state.persistence.postgres;
        let suffix = Uuid::now_v7();
        let workspace =
            catalog_repository::create_workspace(pool, &format!("native-{suffix}"), "Native", None)
                .await?;
        let library = catalog_repository::create_library(
            pool,
            workspace.id,
            &format!("native-{suffix}"),
            "Native",
            None,
            None,
        )
        .await?;
        let user = iam_repository::create_user(
            pool,
            &format!("native-{suffix}"),
            &format!("native-{suffix}@example.invalid"),
            "Native",
            &hash_token("synthetic-password"),
            SystemRole::Admin,
        )
        .await?;
        let secret = mint_plaintext_session_secret();
        let session = iam_repository::create_session(
            pool,
            user.principal_id,
            &hash_session_secret(&secret),
            Utc::now() + chrono::Duration::hours(1),
        )
        .await?;
        let directory = tempfile::tempdir()?;
        let (mut manifest, _, mut admission) = offline_inputs(directory.path());
        manifest.workspace_id = workspace.id;
        manifest.library_id = library.id;
        let session_bytes = serde_json::to_vec(
            &json!({"cookie": build_session_cookie_value(session.id, &secret)}),
        )?;
        manifest.session_file = directory.path().join("session.json");
        std::fs::write(&manifest.session_file, &session_bytes)?;
        manifest.session_sha256 = hash(&session_bytes);
        admission.lifecycle_id = Uuid::now_v7();
        Ok(Self { state, guard, manifest, admission, _directory: directory })
    }

    async fn counts(&self) -> Result<(i64, i64, i64)> {
        let pool = &self.state.persistence.postgres;
        Ok((
            sqlx::query_scalar("select count(*) from query_execution").fetch_one(pool).await?,
            sqlx::query_scalar("select count(*) from query_conversation").fetch_one(pool).await?,
            sqlx::query_scalar("select count(*) from runtime_execution").fetch_one(pool).await?,
        ))
    }
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires owned PostgreSQL with pgvector and Redis"]
async fn lifecycle_persists_and_rejects_drift(pool: PgPool) -> Result<()> {
    let mut fixture = Fixture::new(pool).await?;
    anchor_lifecycle(&fixture.state, &fixture.manifest, &fixture.admission).await?;
    let expected = lifecycle_identity(&fixture.manifest, &fixture.admission)?;
    let row = query_repository::get_execution_by_id(
        &fixture.state.persistence.postgres,
        fixture.admission.lifecycle_id,
    )
    .await?
    .context("missing native lifecycle")?;
    assert_eq!(row.workspace_id, fixture.manifest.workspace_id);
    assert_eq!(row.library_id, fixture.manifest.library_id);
    assert_eq!(row.query_text, expected);
    let counts = fixture.counts().await?;
    assert_eq!(counts, (1, 1, 1));
    anchor_lifecycle(&fixture.state, &fixture.manifest, &fixture.admission).await?;
    assert_eq!(fixture.counts().await?, counts);
    let other_root = tempfile::tempdir()?;
    fixture.admission.ledger_root = other_root.path().canonicalize()?;
    assert!(anchor_lifecycle(&fixture.state, &fixture.manifest, &fixture.admission).await.is_err());
    fixture.admission.ledger_root = fixture._directory.path().to_path_buf();
    fixture.manifest.workspace_id = Uuid::now_v7();
    assert!(anchor_lifecycle(&fixture.state, &fixture.manifest, &fixture.admission).await.is_err());
    fixture.manifest.workspace_id = row.workspace_id;
    fixture.manifest.library_id = Uuid::now_v7();
    assert!(anchor_lifecycle(&fixture.state, &fixture.manifest, &fixture.admission).await.is_err());
    assert_eq!(fixture.counts().await?, counts);
    let unchanged = query_repository::get_execution_by_id(
        &fixture.state.persistence.postgres,
        fixture.admission.lifecycle_id,
    )
    .await?
    .context("missing native lifecycle after refusal")?;
    assert_eq!(unchanged.query_text, expected);
    assert_eq!(unchanged.workspace_id, row.workspace_id);
    assert_eq!(unchanged.library_id, row.library_id);
    assert_eq!(fixture.guard.0.load(Ordering::SeqCst), 0);
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires owned PostgreSQL with pgvector and Redis"]
async fn concurrent_roots_have_one_native_owner(pool: PgPool) -> Result<()> {
    let mut fixture = Fixture::new(pool).await?;
    for different_roots in [false, true] {
        fixture.admission.lifecycle_id = Uuid::now_v7();
        let other_dir = tempfile::tempdir()?;
        let (_, _, mut other) = offline_inputs(other_dir.path());
        other.lifecycle_id = fixture.admission.lifecycle_id;
        if !different_roots {
            other.ledger_root = fixture.admission.ledger_root.clone();
        }
        let pool = &fixture.state.persistence.postgres;
        let mut blocker = pool.begin().await?;
        let blocker_pid: i32 =
            sqlx::query_scalar("select pg_backend_pid()").fetch_one(&mut *blocker).await?;
        let key = i64::from_be_bytes(fixture.admission.lifecycle_id.as_bytes()[..8].try_into()?);
        sqlx::query("select pg_advisory_xact_lock($1)").bind(key).execute(&mut *blocker).await?;
        let first = anchor_lifecycle(&fixture.state, &fixture.manifest, &fixture.admission);
        let second = anchor_lifecycle(&fixture.state, &fixture.manifest, &other);
        let release = async {
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    let waiting: i64 = sqlx::query_scalar(
                        "select count(*) from pg_stat_activity where datname=current_database() and wait_event_type='Lock' and $1 = any(pg_blocking_pids(pid))"
                    ).bind(blocker_pid).fetch_one(pool).await?;
                    if waiting == 2 { break Ok::<_, sqlx::Error>(()); }
                    tokio::task::yield_now().await;
                }
            }).await??;
            blocker.commit().await?;
            Ok::<_, anyhow::Error>(())
        };
        let (a, b, released) =
            Box::pin(tokio::time::timeout(std::time::Duration::from_secs(20), async {
                tokio::join!(first, second, release)
            }))
            .await?;
        released?;
        if different_roots {
            assert_ne!(a.is_ok(), b.is_ok());
        } else {
            a?;
            b?;
        }
        let row = query_repository::get_execution_by_id(pool, fixture.admission.lifecycle_id)
            .await?
            .context("missing native owner")?;
        let expected_a = lifecycle_identity(&fixture.manifest, &fixture.admission)?;
        let expected_b = lifecycle_identity(&fixture.manifest, &other)?;
        assert!(row.query_text == expected_a || row.query_text == expected_b);
    }
    assert_eq!(fixture.counts().await?, (2, 2, 2));
    assert_eq!(fixture.guard.0.load(Ordering::SeqCst), 0);
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires owned PostgreSQL with pgvector and Redis"]
async fn resolved_binding_refuses_pinned_identity_drift(pool: PgPool) -> Result<()> {
    use crate::infra::repositories::ai_repository;
    use crate::shared::secret_encryption::SecretPurpose;

    let mut fixture = Fixture::new(pool).await?;
    let pool = &fixture.state.persistence.postgres;
    let provider = ai_repository::list_provider_catalog(pool)
        .await?
        .into_iter()
        .find(|row| row.provider_kind == "openai")
        .context("missing seeded provider")?;
    let model = ai_repository::list_model_catalog(pool, Some(provider.id))
        .await?
        .into_iter()
        .find(|row| row.model_name == "gpt-6-astra")
        .context("missing seeded model")?;
    // OpenAI's catalog profile has a fixed URL and rejects account overrides.
    // Replace its default only inside this disposable test database.
    sqlx::query("update ai_provider_catalog set default_base_url = $1 where id = $2")
        .bind("https://example.invalid/v1")
        .bind(provider.id)
        .execute(pool)
        .await?;
    let account_id = Uuid::now_v7();
    let encrypted = fixture.state.credential_cipher.encrypt(
        SecretPurpose::AiAccountApiKey,
        account_id,
        "synthetic-only",
    )?;
    let account = ai_repository::create_account(
        pool,
        ai_repository::NewAiAccount {
            account_id,
            scope_kind: "workspace",
            workspace_id: Some(fixture.manifest.workspace_id),
            library_id: None,
            provider_catalog_id: provider.id,
            label: "offline guard",
            base_url: None,
            created_by_principal_id: None,
        },
        Some(&encrypted),
    )
    .await?;
    let binding = ai_repository::create_binding(
        pool,
        ai_repository::CreateAiBindingInput {
            scope_kind: "library",
            workspace_id: Some(fixture.manifest.workspace_id),
            library_id: Some(fixture.manifest.library_id),
            binding_purpose: "query_answer",
            account_id: account.id,
            provider_catalog_id: provider.id,
            model_catalog_id: model.id,
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens_override: None,
            extra_parameters_json: json!({}),
            binding_state: "active",
            updated_by_principal_id: None,
        },
    )
    .await?;
    fixture.admission.binding_id = binding.id;
    fixture.admission.account_id = account.id;
    fixture.admission.provider_catalog_id = provider.id;
    fixture.admission.model_catalog_id = model.id;
    fixture.admission.provider_kind = provider.provider_kind;
    fixture.admission.model_name = model.model_name.clone();
    fixture.admission.gateway_base_url = "https://example.invalid/v1".into();
    check_binding(&fixture.state, &fixture.manifest, &fixture.admission).await?;

    let original = fixture.admission.binding_id;
    fixture.admission.binding_id = Uuid::now_v7();
    assert!(check_binding(&fixture.state, &fixture.manifest, &fixture.admission).await.is_err());
    fixture.admission.binding_id = original;
    fixture.admission.account_id = Uuid::now_v7();
    assert!(check_binding(&fixture.state, &fixture.manifest, &fixture.admission).await.is_err());
    fixture.admission.account_id = account.id;
    fixture.admission.model_catalog_id = Uuid::now_v7();
    assert!(check_binding(&fixture.state, &fixture.manifest, &fixture.admission).await.is_err());
    fixture.admission.model_catalog_id = model.id;
    fixture.admission.model_name = "different-model".into();
    assert!(check_binding(&fixture.state, &fixture.manifest, &fixture.admission).await.is_err());
    fixture.admission.model_name = model.model_name;
    fixture.admission.gateway_base_url = "https://different.invalid/v1".into();
    assert!(check_binding(&fixture.state, &fixture.manifest, &fixture.admission).await.is_err());
    assert_eq!(fixture.guard.0.load(Ordering::SeqCst), 0);
    Ok(())
}
