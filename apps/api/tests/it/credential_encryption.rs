//! Integration coverage for explicit credential migration and key rotation.
//!
//! Requires the same local PostgreSQL/pgvector service as the other ignored
//! repository tests. Values are synthetic and never logged.

use anyhow::{Context as _, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ironrag_backend::{
    app::{config::Settings, seed_clean_v1_ai_catalog},
    infra::repositories::catalog_repository,
    services::maintenance::credential_secrets::{
        CredentialSecretMigrationOptions, migrate_credential_secrets,
    },
    services::webhook::custom_headers,
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
        let name = format!("credential_encryption_{}", Uuid::now_v7().simple());
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

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn previous_key_v3_rows_rewrap_once_to_active_v3() -> Result<()> {
    let settings = Settings::from_env()?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new().max_connections(2).connect(&temp_database.database_url).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    seed_clean_v1_ai_catalog(&pool).await?;

    let old_encoded_key = STANDARD.encode([53_u8; 32]);
    let active_encoded_key = STANDARD.encode([54_u8; 32]);
    let old_cipher =
        CredentialCipher::from_keyring_base64(Some("old-2026"), Some(&old_encoded_key), None)?;
    let previous_key_map = format!("old-2026={old_encoded_key}");
    let cipher = CredentialCipher::from_keyring_base64(
        Some("current-2026"),
        Some(&active_encoded_key),
        Some(&previous_key_map),
    )?;
    let provider_id = sqlx::query_scalar::<_, Uuid>(
        "select id from ai_provider_catalog order by provider_kind limit 1",
    )
    .fetch_one(&pool)
    .await?;

    // AI account credential wrapped under the previous key.
    let account_id = Uuid::now_v7();
    let previous_key_ai =
        old_cipher.encrypt(SecretPurpose::AiAccountApiKey, account_id, "previous-key-ai-value")?;
    sqlx::query(
        "insert into ai_account (
            id, scope_kind, provider_catalog_id, label, api_key, credential_state
         ) values ($1, 'instance', $2, $3, $4, 'active')",
    )
    .bind(account_id)
    .bind(provider_id)
    .bind(format!("credential-rotation-{}", Uuid::now_v7().simple()))
    .bind(previous_key_ai.as_str())
    .execute(&pool)
    .await?;

    // Webhook signing secret and custom headers wrapped under the previous key.
    let workspace = catalog_repository::create_workspace(
        &pool,
        &format!("credential-rotation-{}", Uuid::now_v7().simple()),
        "Credential Rotation",
        None,
    )
    .await?;
    let owner_principal_id = Uuid::now_v7();
    let service_principal_id = Uuid::now_v7();
    let service_operation_id = Uuid::now_v7();
    let previous_key_service_credential = old_cipher.encrypt(
        SecretPurpose::ServiceIdentityCredential,
        service_operation_id,
        "previous-key-service-credential",
    )?;
    sqlx::query(
        "insert into iam_principal (id, principal_kind, display_label)
         values ($1, 'user', 'Credential Rotation Owner'),
                ($2, 'worker', 'Credential Rotation Service')",
    )
    .bind(owner_principal_id)
    .bind(service_principal_id)
    .execute(&pool)
    .await?;
    sqlx::query(
        "insert into iam_api_token (
            principal_id, workspace_id, label, token_prefix, issued_by_principal_id
         ) values ($1, $2, 'Credential Rotation Service', 'irg_test', $3)",
    )
    .bind(service_principal_id)
    .bind(workspace.id)
    .bind(owner_principal_id)
    .execute(&pool)
    .await?;
    sqlx::query(
        "insert into iam_api_token_secret (
            token_principal_id, secret_version, secret_hash
         ) values ($1, 1, 'synthetic-test-hash')",
    )
    .bind(service_principal_id)
    .execute(&pool)
    .await?;
    sqlx::query(
        "insert into iam_service_identity (
            principal_id, owner_auth_provider_kind, owner_external_subject,
            created_by_principal_id
         ) values ($1, 'oidc:https://issuer.example', 'credential-rotation-owner', $2)",
    )
    .bind(service_principal_id)
    .bind(owner_principal_id)
    .execute(&pool)
    .await?;
    sqlx::query(
        "insert into iam_service_identity_operation (
            id, service_identity_principal_id, owner_auth_provider_kind,
            owner_external_subject, actor_principal_id, operation_kind,
            idempotency_key, request_fingerprint, credential_secret_version,
            encrypted_credential
         ) values (
            $1, $2, 'oidc:https://issuer.example', 'credential-rotation-owner', $3,
            'create', 'credential-rotation-create', $4, 1, $5
         )",
    )
    .bind(service_operation_id)
    .bind(service_principal_id)
    .bind(owner_principal_id)
    .bind("0".repeat(64))
    .bind(previous_key_service_credential.as_str())
    .execute(&pool)
    .await?;
    let webhook_id = Uuid::now_v7();
    let previous_key_secret = old_cipher.encrypt(
        SecretPurpose::WebhookSigningSecret,
        webhook_id,
        "previous-key-webhook-value",
    )?;
    let previous_key_headers = old_cipher.encrypt(
        SecretPurpose::WebhookCustomHeaders,
        webhook_id,
        r#"{"X-Env":"neutral-fixture"}"#,
    )?;
    sqlx::query(
        "insert into webhook_subscription (
            id, workspace_id, display_name, target_url, secret, event_types, custom_headers_json
         ) values ($1, $2, $3, 'https://example.com/hook', $4, $5, $6)",
    )
    .bind(webhook_id)
    .bind(workspace.id)
    .bind("Credential Rotation Hook")
    .bind(previous_key_secret.as_str())
    .bind(vec!["revision.ready".to_string()])
    .bind(serde_json::Value::String(previous_key_headers.as_str().to_owned()))
    .execute(&pool)
    .await?;

    let dry_run = migrate_credential_secrets(
        &pool,
        &cipher,
        CredentialSecretMigrationOptions { apply: false, batch_size: 1 },
    )
    .await?;
    assert_eq!(dry_run.ai_rewrap_candidates, 1);
    assert_eq!(dry_run.webhook_rewrap_candidates, 1);
    assert_eq!(dry_run.webhook_header_rewrap_candidates, 1);
    assert_eq!(dry_run.service_identity_rewrap_candidates, 1);
    assert_eq!(dry_run.ai_migrated + dry_run.webhook_migrated, 0);
    assert_eq!(dry_run.webhook_headers_migrated, 0);
    assert_eq!(dry_run.service_identity_migrated, 0);
    assert_eq!(dry_run.invalid_values(), 0);

    let applied = migrate_credential_secrets(
        &pool,
        &cipher,
        CredentialSecretMigrationOptions { apply: true, batch_size: 1 },
    )
    .await?;
    assert_eq!(applied.ai_migrated, 1);
    assert_eq!(applied.webhook_migrated, 1);
    assert_eq!(applied.webhook_headers_migrated, 1);
    assert_eq!(applied.service_identity_migrated, 1);

    let stored_ai = sqlx::query_scalar::<_, String>("select api_key from ai_account where id = $1")
        .bind(account_id)
        .fetch_one(&pool)
        .await?;
    let stored_webhook =
        sqlx::query_scalar::<_, String>("select secret from webhook_subscription where id = $1")
            .bind(webhook_id)
            .fetch_one(&pool)
            .await?;
    let stored_webhook_headers = sqlx::query_scalar::<_, serde_json::Value>(
        "select custom_headers_json from webhook_subscription where id = $1",
    )
    .bind(webhook_id)
    .fetch_one(&pool)
    .await?;
    let stored_service_credential = sqlx::query_scalar::<_, String>(
        "select encrypted_credential
         from iam_service_identity_operation
         where id = $1",
    )
    .bind(service_operation_id)
    .fetch_one(&pool)
    .await?;
    for stored_value in [&stored_ai, &stored_webhook] {
        assert!(stored_value.starts_with("ironrag:enc:v3:xchacha20poly1305:current-2026:"));
        assert!(!cipher.needs_rewrap(stored_value)?);
    }
    assert_eq!(
        cipher.decrypt(SecretPurpose::AiAccountApiKey, account_id, &stored_ai)?.expose_secret(),
        "previous-key-ai-value"
    );
    assert_eq!(
        cipher
            .decrypt(SecretPurpose::WebhookSigningSecret, webhook_id, &stored_webhook)?
            .expose_secret(),
        "previous-key-webhook-value"
    );
    let stored_webhook_headers_envelope = stored_webhook_headers
        .as_str()
        .context("custom headers should be stored as encrypted JSON string")?;
    assert!(
        stored_webhook_headers_envelope
            .starts_with("ironrag:enc:v3:xchacha20poly1305:current-2026:")
    );
    assert!(!cipher.needs_rewrap(stored_webhook_headers_envelope)?);
    assert!(
        stored_service_credential.starts_with("ironrag:enc:v3:xchacha20poly1305:current-2026:")
    );
    assert!(!cipher.needs_rewrap(&stored_service_credential)?);
    assert_eq!(
        cipher
            .decrypt(
                SecretPurpose::ServiceIdentityCredential,
                service_operation_id,
                &stored_service_credential,
            )?
            .expose_secret(),
        "previous-key-service-credential"
    );
    let decoded_headers =
        custom_headers::decrypt_and_validate_stored(&cipher, webhook_id, &stored_webhook_headers)?;
    assert_eq!(decoded_headers.iter().count(), 1);

    let rerun = migrate_credential_secrets(
        &pool,
        &cipher,
        CredentialSecretMigrationOptions { apply: true, batch_size: 1 },
    )
    .await?;
    assert_eq!(rerun.ai_rewrap_candidates + rerun.webhook_rewrap_candidates, 0);
    assert_eq!(rerun.ai_migrated + rerun.webhook_migrated, 0);
    assert_eq!(rerun.webhook_header_rewrap_candidates, 0);
    assert_eq!(rerun.webhook_headers_migrated, 0);
    assert_eq!(rerun.service_identity_rewrap_candidates, 0);
    assert_eq!(rerun.service_identity_migrated, 0);

    assert!(matches!(
        old_cipher.decrypt(SecretPurpose::AiAccountApiKey, account_id, &stored_ai),
        Err(ironrag_backend::shared::secret_encryption::SecretEncryptionError::UnknownKeyId)
    ));

    // A current-key envelope still needs AEAD authentication. One damaged row
    // must be reported without preventing the cursor from finding a later valid
    // previous-key row, and a non-envelope stored header must be audited too.
    let damaged_account_id = Uuid::now_v7();
    let current_envelope = cipher.encrypt(
        SecretPurpose::AiAccountApiKey,
        damaged_account_id,
        "synthetic-current-key-value",
    )?;
    let mut damaged_envelope = current_envelope.as_str().to_owned();
    let ciphertext_start =
        damaged_envelope.rfind(':').context("v3 envelope must contain a ciphertext separator")? + 1;
    let replacement = if damaged_envelope.as_bytes()[ciphertext_start] == b'A' { "B" } else { "A" };
    damaged_envelope.replace_range(ciphertext_start..ciphertext_start + 1, replacement);
    sqlx::query(
        "insert into ai_account (
            id, scope_kind, provider_catalog_id, label, api_key, credential_state
         ) values ($1, 'instance', $2, $3, $4, 'active')",
    )
    .bind(damaged_account_id)
    .bind(provider_id)
    .bind(format!("credential-damaged-{}", Uuid::now_v7().simple()))
    .bind(damaged_envelope)
    .execute(&pool)
    .await?;
    let later_previous_key_account_id = Uuid::now_v7();
    let later_previous_key_ai = old_cipher.encrypt(
        SecretPurpose::AiAccountApiKey,
        later_previous_key_account_id,
        "later-previous-key-value",
    )?;
    sqlx::query(
        "insert into ai_account (
            id, scope_kind, provider_catalog_id, label, api_key, credential_state
         ) values ($1, 'instance', $2, $3, $4, 'active')",
    )
    .bind(later_previous_key_account_id)
    .bind(provider_id)
    .bind(format!("credential-later-{}", Uuid::now_v7().simple()))
    .bind(later_previous_key_ai.as_str())
    .execute(&pool)
    .await?;
    sqlx::query(
        "update webhook_subscription
         set custom_headers_json = $2
         where id = $1",
    )
    .bind(webhook_id)
    .bind(serde_json::json!({"Content-Length": "4"}))
    .execute(&pool)
    .await?;

    let damaged_audit = migrate_credential_secrets(
        &pool,
        &cipher,
        CredentialSecretMigrationOptions { apply: false, batch_size: 1 },
    )
    .await?;
    assert_eq!(damaged_audit.ai_invalid, 1);
    assert_eq!(damaged_audit.ai_rewrap_candidates, 1);
    assert_eq!(damaged_audit.webhook_header_invalid, 1);
    assert_eq!(damaged_audit.invalid_values(), 2);
    assert_eq!(damaged_audit.invalid_samples.len(), 2);
    assert!(damaged_audit.invalid_samples.iter().all(|sample| {
        !sample.error_code.contains("synthetic") && !sample.error_code.contains("ironrag:enc:")
    }));

    pool.close().await;
    temp_database.drop().await
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
