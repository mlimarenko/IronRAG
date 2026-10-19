//! PostgreSQL integration coverage for the set-based AI binding resolver.
//!
//! The test uses an isolated database because deterministic tie-break coverage
//! intentionally drops the fresh-baseline per-library uniqueness index and
//! seeds duplicate bindings. Production schema and databases are never modified.

use anyhow::{Context as _, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ironrag_backend::{
    app::config::Settings,
    infra::repositories::{ai_repository, catalog_repository},
    shared::secret_encryption::{CredentialCipher, SecretPurpose},
};
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

struct TempDatabase {
    name: String,
    admin_url: String,
    database_url: String,
}

impl TempDatabase {
    async fn create(base_url: &str) -> Result<Self> {
        let admin_url = replace_database_name(base_url, "postgres")?;
        let name = format!("provider_batch_resolution_{}", Uuid::now_v7().simple());
        let admin = PgPoolOptions::new().max_connections(1).connect(&admin_url).await?;
        terminate_connections(&admin, &name).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{name}\"")))
            .execute(&admin)
            .await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("create database \"{name}\"")))
            .execute(&admin)
            .await?;
        admin.close().await;
        Ok(Self { database_url: replace_database_name(base_url, &name)?, name, admin_url })
    }

    async fn drop(self) -> Result<()> {
        let admin = PgPoolOptions::new().max_connections(1).connect(&self.admin_url).await?;
        terminate_connections(&admin, &self.name).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{}\"", self.name)))
            .execute(&admin)
            .await?;
        admin.close().await;
        Ok(())
    }
}

struct Target<'value> {
    account_id: Uuid,
    model_id: Uuid,
    model_name: &'value str,
    secret_marker: &'value str,
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn batch_resolver_proves_precedence_ties_dedup_and_single_request_hydration() -> Result<()> {
    let mut settings = Settings::from_env()?;
    let database_url = settings.database_url.clone();
    settings.discard_credential_master_key();
    drop(settings);

    let temp_database = TempDatabase::create(&database_url).await?;
    let pool = PgPoolOptions::new().max_connections(2).connect(&temp_database.database_url).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;

    let suffix = Uuid::now_v7().simple().to_string();
    let provider_id = Uuid::now_v7();
    sqlx::query(
        "insert into ai_provider_catalog (
            id, provider_kind, display_name, api_style, lifecycle_state,
            default_base_url, capability_flags_json
         ) values ($1, $2, $3, 'openai_compatible', 'active', $4, '{}'::jsonb)",
    )
    .bind(provider_id)
    .bind(format!("batch-resolver-{suffix}"))
    .bind("Batch Resolver Fixture")
    .bind("https://provider.example/v1")
    .execute(&pool)
    .await?;

    let workspace = catalog_repository::create_workspace(
        &pool,
        &format!("batch-resolver-{suffix}"),
        "Batch Resolver Fixture",
        None,
    )
    .await?;
    let library = catalog_repository::create_library(
        &pool,
        workspace.id,
        &format!("batch-resolver-library-{suffix}"),
        "Batch Resolver Library",
        None,
        None,
    )
    .await?;
    let cipher = CredentialCipher::from_base64(&STANDARD.encode([83_u8; 32]))?;

    let instance = Target {
        account_id: Uuid::now_v7(),
        model_id: Uuid::now_v7(),
        model_name: "fixture-instance",
        secret_marker: "instance-secret",
    };
    let workspace_target = Target {
        account_id: Uuid::now_v7(),
        model_id: Uuid::now_v7(),
        model_name: "fixture-workspace",
        secret_marker: "workspace-secret",
    };
    let library_target = Target {
        account_id: Uuid::now_v7(),
        model_id: Uuid::now_v7(),
        model_name: "fixture-library",
        secret_marker: "library-secret",
    };
    let tie_old = Target {
        account_id: Uuid::now_v7(),
        model_id: Uuid::now_v7(),
        model_name: "fixture-tie-old",
        secret_marker: "tie-old-secret",
    };
    let tie_low = Target {
        account_id: Uuid::now_v7(),
        model_id: Uuid::now_v7(),
        model_name: "fixture-tie-low",
        secret_marker: "tie-low-secret",
    };
    let tie_high = Target {
        account_id: Uuid::now_v7(),
        model_id: Uuid::now_v7(),
        model_name: "fixture-tie-high",
        secret_marker: "tie-high-secret",
    };

    for target in [&instance, &workspace_target, &library_target, &tie_old, &tie_low, &tie_high] {
        insert_model(&pool, provider_id, target.model_id, target.model_name).await?;
    }
    insert_account(
        &pool,
        &cipher,
        provider_id,
        &instance,
        Scope { kind: "instance", workspace_id: None, library_id: None },
    )
    .await?;
    insert_account(
        &pool,
        &cipher,
        provider_id,
        &workspace_target,
        Scope { kind: "workspace", workspace_id: Some(workspace.id), library_id: None },
    )
    .await?;
    for target in [&library_target, &tie_old, &tie_low, &tie_high] {
        insert_account(
            &pool,
            &cipher,
            provider_id,
            target,
            Scope {
                kind: "library",
                workspace_id: Some(workspace.id),
                library_id: Some(library.id),
            },
        )
        .await?;
    }

    sqlx::query(
        "insert into ai_model_binding_purpose (model_catalog_id, binding_purpose)
         values ($1, 'embed_chunk')",
    )
    .bind(library_target.model_id)
    .execute(&pool)
    .await?;
    insert_binding(
        &pool,
        provider_id,
        BindingSeed {
            id: Uuid::now_v7(),
            scope: Scope {
                kind: "library",
                workspace_id: Some(workspace.id),
                library_id: Some(library.id),
            },
            purpose: "embed_chunk",
            updated_at: "2030-03-01T00:00:00Z",
        },
        &library_target,
    )
    .await?;

    let mut transaction = pool.begin().await?;
    let embedding_profile = ai_repository::get_effective_embedding_profile_with_transaction(
        &mut transaction,
        library.id,
    )
    .await?
    .context("single-request embedding profile should resolve inside the caller transaction")?;
    assert_eq!(embedding_profile.binding_purpose, "embed_chunk");
    assert_eq!(embedding_profile.model.model_name, library_target.model_name);
    transaction.rollback().await?;

    let answer_instance_id = Uuid::now_v7();
    let answer_workspace_id = Uuid::now_v7();
    let answer_library_id = Uuid::now_v7();
    insert_binding(
        &pool,
        provider_id,
        BindingSeed {
            id: answer_instance_id,
            scope: Scope { kind: "instance", workspace_id: None, library_id: None },
            purpose: "query_answer",
            updated_at: "2030-03-03T00:00:00Z",
        },
        &instance,
    )
    .await?;
    insert_binding(
        &pool,
        provider_id,
        BindingSeed {
            id: answer_workspace_id,
            scope: Scope { kind: "workspace", workspace_id: Some(workspace.id), library_id: None },
            purpose: "query_answer",
            updated_at: "2030-03-02T00:00:00Z",
        },
        &workspace_target,
    )
    .await?;
    insert_binding(
        &pool,
        provider_id,
        BindingSeed {
            id: answer_library_id,
            scope: Scope {
                kind: "library",
                workspace_id: Some(workspace.id),
                library_id: Some(library.id),
            },
            purpose: "query_answer",
            updated_at: "2030-03-01T00:00:00Z",
        },
        &library_target,
    )
    .await?;

    // This fixture deliberately removes the fresh-baseline uniqueness index to
    // prove deterministic resolution when externally introduced duplicate rows
    // violate that invariant.
    sqlx::query("drop index ai_binding_library_purpose_key").execute(&pool).await?;
    let tie_old_id = Uuid::from_u128(0x100);
    let tie_low_id = Uuid::from_u128(0x200);
    let tie_high_id = Uuid::from_u128(0x300);
    insert_binding(
        &pool,
        provider_id,
        BindingSeed {
            id: tie_old_id,
            scope: Scope {
                kind: "library",
                workspace_id: Some(workspace.id),
                library_id: Some(library.id),
            },
            purpose: "query_compile",
            updated_at: "2030-04-01T00:00:00Z",
        },
        &tie_old,
    )
    .await?;
    insert_binding(
        &pool,
        provider_id,
        BindingSeed {
            id: tie_low_id,
            scope: Scope {
                kind: "library",
                workspace_id: Some(workspace.id),
                library_id: Some(library.id),
            },
            purpose: "query_compile",
            updated_at: "2030-04-02T00:00:00Z",
        },
        &tie_low,
    )
    .await?;
    insert_binding(
        &pool,
        provider_id,
        BindingSeed {
            id: tie_high_id,
            scope: Scope {
                kind: "library",
                workspace_id: Some(workspace.id),
                library_id: Some(library.id),
            },
            purpose: "query_compile",
            updated_at: "2030-04-02T00:00:00Z",
        },
        &tie_high,
    )
    .await?;

    let requested = vec![
        "query_compile".to_string(),
        "query_answer".to_string(),
        "query_compile".to_string(),
        "query_answer".to_string(),
    ];
    let selected =
        ai_repository::list_effective_provider_selections(&pool, library.id, &requested).await?;
    assert_eq!(selected.len(), 2, "duplicate requested purposes must collapse to one row each");
    assert_eq!(selected[0].binding_purpose, "query_compile");
    assert_eq!(selected[0].model.model_name, tie_high.model_name, "higher id wins equal timestamp");
    assert_eq!(selected[1].binding_purpose, "query_answer");
    assert_eq!(
        selected[1].model.model_name, library_target.model_name,
        "library scope must beat newer workspace and instance rows"
    );

    let hydrated = ai_repository::get_effective_runtime_binding(&pool, library.id, "query_compile")
        .await?
        .context("query_compile binding should hydrate")?;
    assert_eq!(hydrated.binding_id, tie_high_id);
    assert_eq!(hydrated.binding_purpose, "query_compile");
    assert_eq!(hydrated.account_id, tie_high.account_id);
    let stored = hydrated
        .account_api_key
        .as_deref()
        .context("selected account should carry its encrypted credential")?;
    assert!(!stored.contains(tie_high.secret_marker));
    assert_eq!(
        cipher
            .decrypt(SecretPurpose::AiAccountApiKey, hydrated.account_id, stored)?
            .expose_secret(),
        tie_high.secret_marker,
        "one-purpose hydration must fetch the selected account only"
    );
    drop(hydrated);

    disable_binding(&pool, tie_high_id).await?;
    let selected = ai_repository::list_effective_provider_selections(
        &pool,
        library.id,
        &["query_compile".to_string()],
    )
    .await?;
    assert_eq!(
        selected[0].model.model_name, tie_low.model_name,
        "newer updated_at must beat a higher-id older row"
    );

    disable_binding(&pool, answer_library_id).await?;
    let selected = ai_repository::list_effective_provider_selections(
        &pool,
        library.id,
        &["query_answer".to_string()],
    )
    .await?;
    assert_eq!(selected[0].model.model_name, workspace_target.model_name);
    disable_binding(&pool, answer_workspace_id).await?;
    let selected = ai_repository::list_effective_provider_selections(
        &pool,
        library.id,
        &["query_answer".to_string()],
    )
    .await?;
    assert_eq!(selected[0].model.model_name, instance.model_name);

    pool.close().await;
    temp_database.drop().await?;
    Ok(())
}

async fn insert_model(
    pool: &PgPool,
    provider_id: Uuid,
    model_id: Uuid,
    model_name: &str,
) -> Result<()> {
    sqlx::query(
        "insert into ai_model_catalog (
            id, provider_catalog_id, model_name, lifecycle_state, metadata_json
         ) values ($1, $2, $3, 'active', $4)",
    )
    .bind(model_id)
    .bind(provider_id)
    .bind(model_name)
    .bind(serde_json::json!({}))
    .execute(pool)
    .await?;
    sqlx::query(
        "insert into ai_model_binding_purpose (model_catalog_id, binding_purpose)
         values ($1, 'query_compile'), ($1, 'query_answer')",
    )
    .bind(model_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// The scope an account or binding row is attached to: its kind plus the
/// optional workspace and library ids.
struct Scope<'a> {
    kind: &'a str,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
}

/// One seeded binding row: its id, scope, purpose and `updated_at` used by the
/// resolver's tie-break.
struct BindingSeed<'a> {
    id: Uuid,
    scope: Scope<'a>,
    purpose: &'a str,
    updated_at: &'a str,
}

async fn insert_account(
    pool: &PgPool,
    cipher: &CredentialCipher,
    provider_id: Uuid,
    target: &Target<'_>,
    scope: Scope<'_>,
) -> Result<()> {
    let encrypted =
        cipher.encrypt(SecretPurpose::AiAccountApiKey, target.account_id, target.secret_marker)?;
    sqlx::query(
        "insert into ai_account (
            id, scope_kind, workspace_id, library_id, provider_catalog_id,
            label, api_key, base_url, credential_state
         ) values ($1, $2::ai_scope_kind, $3, $4, $5, $6, $7, $8, 'active')",
    )
    .bind(target.account_id)
    .bind(scope.kind)
    .bind(scope.workspace_id)
    .bind(scope.library_id)
    .bind(provider_id)
    .bind(target.model_name)
    .bind(encrypted.as_str())
    .bind(format!("https://{}.example/v1", target.model_name))
    .execute(pool)
    .await?;
    Ok(())
}

async fn insert_binding(
    pool: &PgPool,
    provider_id: Uuid,
    seed: BindingSeed<'_>,
    target: &Target<'_>,
) -> Result<()> {
    sqlx::query(
        "insert into ai_binding (
            id, scope_kind, workspace_id, library_id, binding_purpose,
            account_id, provider_catalog_id, model_catalog_id, system_prompt,
            extra_parameters_json, binding_state, created_at, updated_at
         ) values (
            $1, $2::ai_scope_kind, $3, $4, $5::ai_binding_purpose,
            $6, $7, $8, $9, '{}'::jsonb, 'active', $10::timestamptz, $10::timestamptz
         )",
    )
    .bind(seed.id)
    .bind(seed.scope.kind)
    .bind(seed.scope.workspace_id)
    .bind(seed.scope.library_id)
    .bind(seed.purpose)
    .bind(target.account_id)
    .bind(provider_id)
    .bind(target.model_id)
    .bind(format!("prompt-for-{}", target.model_name))
    .bind(seed.updated_at)
    .execute(pool)
    .await?;
    Ok(())
}

async fn disable_binding(pool: &PgPool, binding_id: Uuid) -> Result<()> {
    sqlx::query("update ai_binding set binding_state = 'disabled' where id = $1")
        .bind(binding_id)
        .execute(pool)
        .await?;
    Ok(())
}

fn replace_database_name(url: &str, database_name: &str) -> Result<String> {
    let (base, query) =
        url.split_once('?').map_or((url, None), |(base, query)| (base, Some(query)));
    let slash = base.rfind('/').context("database URL must contain a database name")?;
    let mut replaced = format!("{}{database_name}", &base[..=slash]);
    if let Some(query) = query {
        replaced.push('?');
        replaced.push_str(query);
    }
    Ok(replaced)
}

async fn terminate_connections(admin: &PgPool, database_name: &str) -> Result<()> {
    sqlx::query(
        "select pg_terminate_backend(pid)
         from pg_stat_activity
         where datname = $1 and pid <> pg_backend_pid()",
    )
    .bind(database_name)
    .execute(admin)
    .await?;
    Ok(())
}
