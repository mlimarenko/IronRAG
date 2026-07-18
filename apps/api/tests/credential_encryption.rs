//! Integration coverage for explicit credential migration and key rotation.
//!
//! Requires the same local PostgreSQL/pgvector service as the other ignored
//! repository tests. Values are synthetic and never logged.

use anyhow::{Context as _, Result};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use chacha20poly1305::{
    Key, KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use ironrag_backend::{
    app::config::Settings,
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
async fn legacy_and_previous_key_rows_migrate_once_to_active_v3() -> Result<()> {
    let settings = Settings::from_env()?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new().max_connections(2).connect(&temp_database.database_url).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;

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
    let account_id = Uuid::now_v7();
    sqlx::query(
        "insert into ai_account (
            id, scope_kind, provider_catalog_id, label, api_key, credential_state
         ) values ($1, 'instance', $2, $3, $4, 'active')",
    )
    .bind(account_id)
    .bind(provider_id)
    .bind(format!("credential-migration-{}", Uuid::now_v7().simple()))
    .bind("legacy-ai-value")
    .execute(&pool)
    .await?;

    let previous_key_account_id = Uuid::now_v7();
    let previous_key_envelope = old_cipher.encrypt(
        SecretPurpose::AiAccountApiKey,
        previous_key_account_id,
        "previous-key-ai-value",
    )?;
    sqlx::query(
        "insert into ai_account (
            id, scope_kind, provider_catalog_id, label, api_key, credential_state
         ) values ($1, 'instance', $2, $3, $4, 'active')",
    )
    .bind(previous_key_account_id)
    .bind(provider_id)
    .bind(format!("credential-rotation-{}", Uuid::now_v7().simple()))
    .bind(previous_key_envelope.as_str())
    .execute(&pool)
    .await?;

    let workspace = catalog_repository::create_workspace(
        &pool,
        &format!("credential-migration-{}", Uuid::now_v7().simple()),
        "Credential Migration",
        None,
    )
    .await?;
    let webhook_id = Uuid::now_v7();
    let legacy_webhook_envelope = legacy_v1_envelope(
        [53_u8; 32],
        b"ironrag:secret:v1:webhook_subscription.secret",
        b"legacy-webhook-value",
    )?;
    sqlx::query(
        "insert into webhook_subscription (
            id, workspace_id, display_name, target_url, secret, event_types
         ) values ($1, $2, $3, 'https://example.com/hook', $4, $5)",
    )
    .bind(webhook_id)
    .bind(workspace.id)
    .bind("Credential Migration Hook")
    .bind(legacy_webhook_envelope)
    .bind(vec!["revision.ready".to_string()])
    .execute(&pool)
    .await?;

    let dry_run = migrate_credential_secrets(
        &pool,
        &cipher,
        CredentialSecretMigrationOptions { apply: false, batch_size: 1 },
    )
    .await?;
    assert_eq!(dry_run.ai_legacy_found, 2);
    assert_eq!(dry_run.webhook_legacy_found, 1);
    assert_eq!(dry_run.webhook_header_rewrap_candidates, 1);
    assert_eq!(dry_run.ai_migrated + dry_run.webhook_migrated, 0);
    assert_eq!(dry_run.webhook_headers_migrated, 0);

    let applied = migrate_credential_secrets(
        &pool,
        &cipher,
        CredentialSecretMigrationOptions { apply: true, batch_size: 1 },
    )
    .await?;
    assert_eq!(applied.ai_migrated, 2);
    assert_eq!(applied.webhook_migrated, 1);
    assert_eq!(applied.webhook_headers_migrated, 1);

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
    let stored_previous_key_ai =
        sqlx::query_scalar::<_, String>("select api_key from ai_account where id = $1")
            .bind(previous_key_account_id)
            .fetch_one(&pool)
            .await?;
    for stored_value in [&stored_ai, &stored_previous_key_ai, &stored_webhook] {
        assert!(stored_value.starts_with("ironrag:enc:v3:xchacha20poly1305:current-2026:"));
        assert!(!cipher.needs_rewrap(stored_value)?);
    }
    assert_eq!(
        cipher.decrypt(SecretPurpose::AiAccountApiKey, account_id, &stored_ai)?.expose_secret(),
        "legacy-ai-value"
    );
    assert_eq!(
        cipher
            .decrypt(
                SecretPurpose::AiAccountApiKey,
                previous_key_account_id,
                &stored_previous_key_ai,
            )?
            .expose_secret(),
        "previous-key-ai-value"
    );
    assert_eq!(
        cipher
            .decrypt(SecretPurpose::WebhookSigningSecret, webhook_id, &stored_webhook)?
            .expose_secret(),
        "legacy-webhook-value"
    );
    let stored_webhook_headers_envelope = stored_webhook_headers
        .as_str()
        .context("custom headers should be stored as encrypted JSON string")?;
    assert!(
        stored_webhook_headers_envelope
            .starts_with("ironrag:enc:v3:xchacha20poly1305:current-2026:")
    );
    assert!(!cipher.needs_rewrap(stored_webhook_headers_envelope)?);
    let decoded_headers =
        custom_headers::decrypt_and_validate_stored(&cipher, webhook_id, &stored_webhook_headers)?;
    assert!(decoded_headers.is_empty());

    let rerun = migrate_credential_secrets(
        &pool,
        &cipher,
        CredentialSecretMigrationOptions { apply: true, batch_size: 1 },
    )
    .await?;
    assert_eq!(rerun.ai_legacy_found + rerun.webhook_legacy_found, 0);
    assert_eq!(rerun.ai_migrated + rerun.webhook_migrated, 0);
    assert_eq!(rerun.webhook_header_rewrap_candidates, 0);
    assert_eq!(rerun.webhook_headers_migrated, 0);

    assert!(matches!(
        old_cipher.decrypt(SecretPurpose::AiAccountApiKey, account_id, &stored_ai),
        Err(ironrag_backend::shared::secret_encryption::SecretEncryptionError::UnknownKeyId)
    ));

    // A current-key envelope still needs AEAD authentication. One damaged row
    // must be reported without preventing the cursor from finding a later
    // valid legacy row, and invalid stored header policy must be audited too.
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
    let later_legacy_account_id = Uuid::now_v7();
    sqlx::query(
        "insert into ai_account (
            id, scope_kind, provider_catalog_id, label, api_key, credential_state
         ) values ($1, 'instance', $2, $3, $4, 'active')",
    )
    .bind(later_legacy_account_id)
    .bind(provider_id)
    .bind(format!("credential-later-{}", Uuid::now_v7().simple()))
    .bind("later-valid-legacy-value")
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
    assert_eq!(damaged_audit.ai_legacy_found, 1);
    assert_eq!(damaged_audit.webhook_header_invalid, 1);
    assert_eq!(damaged_audit.invalid_values(), 2);
    assert_eq!(damaged_audit.invalid_samples.len(), 2);
    assert!(damaged_audit.invalid_samples.iter().all(|sample| {
        !sample.error_code.contains("synthetic") && !sample.error_code.contains("ironrag:enc:")
    }));

    pool.close().await;
    temp_database.drop().await
}

fn legacy_v1_envelope(key: [u8; 32], aad: &[u8], plaintext: &[u8]) -> Result<String> {
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&key));
    let nonce = [71_u8; 24];
    let ciphertext = cipher
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: plaintext, aad })
        .map_err(|_| anyhow::anyhow!("failed to build synthetic v1 envelope"))?;
    Ok(format!(
        "ironrag:enc:v1:xchacha20poly1305:{}:{}",
        URL_SAFE_NO_PAD.encode(nonce),
        URL_SAFE_NO_PAD.encode(ciphertext),
    ))
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
