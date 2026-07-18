//! Explicit, idempotent rewrapping of legacy or non-current database
//! credentials into active-key row-bound v3 envelopes.
//!
//! This module is intentionally not wired into the recurring scheduler or
//! application startup. Operators first run the command without `--apply` to
//! inventory aggregate counts, then opt in to bounded optimistic updates.

use serde::Serialize;
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroize as _;

use crate::{
    services::webhook::custom_headers::{self, CustomHeadersError},
    shared::secret_encryption::{
        CredentialCipher, DecryptedSecret, SecretEncryptionError, SecretPurpose,
    },
};

const DEFAULT_BATCH_SIZE: usize = 100;
const MAX_BATCH_SIZE: usize = 1_000;
const MAX_INVALID_SAMPLES: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CredentialSecretMigrationOptions {
    /// `false` is an inventory-only dry run; writes require explicit opt-in.
    pub apply: bool,
    pub batch_size: usize,
}

impl Default for CredentialSecretMigrationOptions {
    fn default() -> Self {
        Self { apply: false, batch_size: DEFAULT_BATCH_SIZE }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CredentialSecretMigrationReport {
    pub apply: bool,
    pub ai_rows_scanned: usize,
    pub ai_legacy_found: usize,
    pub ai_migrated: usize,
    pub ai_concurrent_changes: usize,
    pub webhook_rows_scanned: usize,
    pub webhook_legacy_found: usize,
    pub webhook_migrated: usize,
    pub webhook_concurrent_changes: usize,
    pub webhook_header_rows_scanned: usize,
    pub webhook_header_rewrap_candidates: usize,
    pub webhook_headers_migrated: usize,
    pub webhook_header_concurrent_changes: usize,
    pub ai_invalid: usize,
    pub webhook_invalid: usize,
    pub webhook_header_invalid: usize,
    /// Bounded, non-secret operator diagnostics. Values and envelope details
    /// are intentionally never included.
    pub invalid_samples: Vec<CredentialSecretInvalidSample>,
}

impl CredentialSecretMigrationReport {
    #[must_use]
    pub const fn invalid_values(&self) -> usize {
        self.ai_invalid + self.webhook_invalid + self.webhook_header_invalid
    }

    fn record_invalid(
        &mut self,
        storage: CredentialSecretStorage,
        id: Uuid,
        error_code: &'static str,
    ) {
        match storage {
            CredentialSecretStorage::AiAccountApiKey => self.ai_invalid += 1,
            CredentialSecretStorage::WebhookSigningSecret => self.webhook_invalid += 1,
            CredentialSecretStorage::WebhookCustomHeaders => self.webhook_header_invalid += 1,
        }
        if self.invalid_samples.len() < MAX_INVALID_SAMPLES {
            self.invalid_samples.push(CredentialSecretInvalidSample {
                storage,
                id,
                error_code: error_code.to_owned(),
            });
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSecretStorage {
    AiAccountApiKey,
    WebhookSigningSecret,
    WebhookCustomHeaders,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CredentialSecretInvalidSample {
    pub storage: CredentialSecretStorage,
    pub id: Uuid,
    pub error_code: String,
}

#[derive(Debug, Error)]
pub enum CredentialSecretMigrationError {
    #[error("credential migration batch size must be between 1 and 1000")]
    InvalidBatchSize,
    #[error("credential protection is unavailable: {0}")]
    CredentialProtection(#[from] SecretEncryptionError),
    #[error("webhook custom-header migration failure: {0}")]
    WebhookCustomHeaders(#[from] CustomHeadersError),
    #[error("credential migration repository failure: {0}")]
    Repository(#[from] sqlx::Error),
}

const fn secret_encryption_error_code(error: &SecretEncryptionError) -> &'static str {
    match error {
        SecretEncryptionError::MasterKeyNotConfigured => "master_key_not_configured",
        SecretEncryptionError::InvalidMasterKey => "invalid_master_key",
        SecretEncryptionError::InvalidKeyId => "invalid_key_id",
        SecretEncryptionError::InvalidPreviousKeyMap => "invalid_previous_key_map",
        SecretEncryptionError::UnknownKeyId => "unknown_key_id",
        SecretEncryptionError::InvalidPlaintext => "invalid_plaintext",
        SecretEncryptionError::InvalidEnvelope => "invalid_envelope",
        SecretEncryptionError::UnsupportedEnvelope => "unsupported_envelope",
        SecretEncryptionError::EncryptionFailed => "encryption_failed",
        SecretEncryptionError::DecryptionFailed => "authentication_failed",
    }
}

fn custom_headers_error_code(error: &CustomHeadersError) -> &'static str {
    match error {
        CustomHeadersError::InvalidShape => "invalid_header_shape",
        CustomHeadersError::TooManyHeaders => "too_many_headers",
        CustomHeadersError::InvalidHeaderName => "invalid_header_name",
        CustomHeadersError::DuplicateHeaderName => "duplicate_header_name",
        CustomHeadersError::ReservedHeaderName => "reserved_header_name",
        CustomHeadersError::InvalidHeaderValue => "invalid_header_value",
        CustomHeadersError::SerializedSizeExceeded => "headers_size_exceeded",
        CustomHeadersError::InvalidStoredValue => "invalid_stored_headers",
        CustomHeadersError::CredentialProtection(error) => secret_encryption_error_code(error),
    }
}

fn inspect_secret(
    cipher: &CredentialCipher,
    purpose: SecretPurpose,
    id: Uuid,
    stored_value: &str,
) -> Result<(bool, DecryptedSecret), SecretEncryptionError> {
    let needs_rewrap = cipher.needs_rewrap(stored_value)?;
    // `needs_rewrap` intentionally only classifies the envelope. Always
    // decrypt as well so a current-key v3 authentication failure is visible to
    // the operator instead of being incorrectly reported as healthy.
    let plaintext = cipher.decrypt(purpose, id, stored_value)?;
    Ok((needs_rewrap, plaintext))
}

#[derive(FromRow)]
struct StoredCredentialRow {
    id: Uuid,
    stored_value: String,
}

impl std::fmt::Debug for StoredCredentialRow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StoredCredentialRow")
            .field("id", &self.id)
            .field("stored_value", &"<redacted>")
            .finish()
    }
}

impl Drop for StoredCredentialRow {
    fn drop(&mut self) {
        self.stored_value.zeroize();
    }
}

#[derive(FromRow)]
struct StoredWebhookHeadersRow {
    id: Uuid,
    stored_value: Value,
}

impl std::fmt::Debug for StoredWebhookHeadersRow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StoredWebhookHeadersRow")
            .field("id", &self.id)
            .field("stored_value", &"<redacted>")
            .finish()
    }
}

impl Drop for StoredWebhookHeadersRow {
    fn drop(&mut self) {
        custom_headers::scrub_json_strings(&mut self.stored_value);
    }
}

/// Inventories or re-encrypts every legacy or non-current AI/webhook secret.
///
/// The key preflight happens before the first query. Each batch holds at most
/// `batch_size` credential rows in memory, and every update compares the old
/// value so concurrent credential rotation wins instead of being overwritten.
/// Re-running after success is a no-op for v3 envelopes using the active key ID.
///
/// # Errors
/// Returns an error before database access when the key or batch size is
/// invalid, or when a repository/system operation fails. Invalid individual
/// rows are counted and sampled with redacted error codes while scanning
/// continues, so one damaged credential cannot hide later findings.
pub async fn migrate_credential_secrets(
    postgres: &PgPool,
    cipher: &CredentialCipher,
    options: CredentialSecretMigrationOptions,
) -> Result<CredentialSecretMigrationReport, CredentialSecretMigrationError> {
    cipher.require_configured()?;
    if !(1..=MAX_BATCH_SIZE).contains(&options.batch_size) {
        return Err(CredentialSecretMigrationError::InvalidBatchSize);
    }
    let database_batch_size = i64::try_from(options.batch_size)
        .map_err(|_| CredentialSecretMigrationError::InvalidBatchSize)?;

    let mut report = CredentialSecretMigrationReport { apply: options.apply, ..Default::default() };
    migrate_ai_account_secrets(postgres, cipher, options, database_batch_size, &mut report).await?;
    migrate_webhook_secrets(postgres, cipher, options, database_batch_size, &mut report).await?;
    migrate_webhook_custom_headers(postgres, cipher, options, database_batch_size, &mut report)
        .await?;
    Ok(report)
}

async fn migrate_ai_account_secrets(
    postgres: &PgPool,
    cipher: &CredentialCipher,
    options: CredentialSecretMigrationOptions,
    database_batch_size: i64,
    report: &mut CredentialSecretMigrationReport,
) -> Result<(), CredentialSecretMigrationError> {
    let mut cursor = None;
    loop {
        let rows = sqlx::query_as::<_, StoredCredentialRow>(
            "select id, api_key as stored_value
             from ai_account
             where api_key is not null
               and api_key <> ''
               and ($1::uuid is null or id > $1)
             order by id
             limit $2",
        )
        .bind(cursor)
        .bind(database_batch_size)
        .fetch_all(postgres)
        .await?;
        if rows.is_empty() {
            break;
        }
        let row_count = rows.len();
        report.ai_rows_scanned += row_count;
        let next_cursor = rows.last().map(|row| row.id);
        for row in rows {
            migrate_ai_account_row(postgres, cipher, options.apply, row, report).await?;
        }
        cursor = next_cursor;
        if row_count < options.batch_size {
            break;
        }
    }
    Ok(())
}

async fn migrate_ai_account_row(
    postgres: &PgPool,
    cipher: &CredentialCipher,
    apply: bool,
    row: StoredCredentialRow,
    report: &mut CredentialSecretMigrationReport,
) -> Result<(), CredentialSecretMigrationError> {
    let (needs_rewrap, plaintext) =
        match inspect_secret(cipher, SecretPurpose::AiAccountApiKey, row.id, &row.stored_value) {
            Ok(inspection) => inspection,
            Err(error) => {
                report.record_invalid(
                    CredentialSecretStorage::AiAccountApiKey,
                    row.id,
                    secret_encryption_error_code(&error),
                );
                return Ok(());
            }
        };
    if !needs_rewrap {
        drop(plaintext);
        return Ok(());
    }
    report.ai_legacy_found += 1;
    if !apply {
        drop(plaintext);
        return Ok(());
    }
    let encrypted =
        cipher.encrypt(SecretPurpose::AiAccountApiKey, row.id, plaintext.expose_secret())?;
    drop(plaintext);
    let result = sqlx::query(
        "update ai_account
         set api_key = $2, updated_at = now()
         where id = $1 and api_key = $3",
    )
    .bind(row.id)
    .bind(encrypted.as_str())
    .bind(&row.stored_value)
    .execute(postgres)
    .await?;
    if result.rows_affected() == 1 {
        report.ai_migrated += 1;
    } else {
        report.ai_concurrent_changes += 1;
    }
    Ok(())
}

async fn migrate_webhook_secrets(
    postgres: &PgPool,
    cipher: &CredentialCipher,
    options: CredentialSecretMigrationOptions,
    database_batch_size: i64,
    report: &mut CredentialSecretMigrationReport,
) -> Result<(), CredentialSecretMigrationError> {
    let mut cursor = None;
    loop {
        let rows = sqlx::query_as::<_, StoredCredentialRow>(
            "select id, secret as stored_value
             from webhook_subscription
             where secret <> ''
               and ($1::uuid is null or id > $1)
             order by id
             limit $2",
        )
        .bind(cursor)
        .bind(database_batch_size)
        .fetch_all(postgres)
        .await?;
        if rows.is_empty() {
            break;
        }
        let row_count = rows.len();
        report.webhook_rows_scanned += row_count;
        let next_cursor = rows.last().map(|row| row.id);
        for row in rows {
            migrate_webhook_secret_row(postgres, cipher, options.apply, row, report).await?;
        }
        cursor = next_cursor;
        if row_count < options.batch_size {
            break;
        }
    }
    Ok(())
}

async fn migrate_webhook_secret_row(
    postgres: &PgPool,
    cipher: &CredentialCipher,
    apply: bool,
    row: StoredCredentialRow,
    report: &mut CredentialSecretMigrationReport,
) -> Result<(), CredentialSecretMigrationError> {
    let (needs_rewrap, plaintext) = match inspect_secret(
        cipher,
        SecretPurpose::WebhookSigningSecret,
        row.id,
        &row.stored_value,
    ) {
        Ok(inspection) => inspection,
        Err(error) => {
            report.record_invalid(
                CredentialSecretStorage::WebhookSigningSecret,
                row.id,
                secret_encryption_error_code(&error),
            );
            return Ok(());
        }
    };
    if !needs_rewrap {
        drop(plaintext);
        return Ok(());
    }
    report.webhook_legacy_found += 1;
    if !apply {
        drop(plaintext);
        return Ok(());
    }
    let encrypted =
        cipher.encrypt(SecretPurpose::WebhookSigningSecret, row.id, plaintext.expose_secret())?;
    drop(plaintext);
    let result = sqlx::query(
        "update webhook_subscription
         set secret = $2, updated_at = now()
         where id = $1 and secret = $3",
    )
    .bind(row.id)
    .bind(encrypted.as_str())
    .bind(&row.stored_value)
    .execute(postgres)
    .await?;
    if result.rows_affected() == 1 {
        report.webhook_migrated += 1;
    } else {
        report.webhook_concurrent_changes += 1;
    }
    Ok(())
}

async fn migrate_webhook_custom_headers(
    postgres: &PgPool,
    cipher: &CredentialCipher,
    options: CredentialSecretMigrationOptions,
    database_batch_size: i64,
    report: &mut CredentialSecretMigrationReport,
) -> Result<(), CredentialSecretMigrationError> {
    let mut cursor = None;
    loop {
        let rows = sqlx::query_as::<_, StoredWebhookHeadersRow>(
            "select id, custom_headers_json as stored_value
             from webhook_subscription
             where ($1::uuid is null or id > $1)
             order by id
             limit $2",
        )
        .bind(cursor)
        .bind(database_batch_size)
        .fetch_all(postgres)
        .await?;
        if rows.is_empty() {
            break;
        }
        let row_count = rows.len();
        report.webhook_header_rows_scanned += row_count;
        let next_cursor = rows.last().map(|row| row.id);
        for row in rows {
            migrate_webhook_custom_headers_row(postgres, cipher, options.apply, row, report)
                .await?;
        }
        cursor = next_cursor;
        if row_count < options.batch_size {
            break;
        }
    }
    Ok(())
}

async fn migrate_webhook_custom_headers_row(
    postgres: &PgPool,
    cipher: &CredentialCipher,
    apply: bool,
    row: StoredWebhookHeadersRow,
    report: &mut CredentialSecretMigrationReport,
) -> Result<(), CredentialSecretMigrationError> {
    let needs_rewrap = match &row.stored_value {
        Value::String(stored_value) => match cipher.needs_rewrap(stored_value) {
            Ok(needs_rewrap) => needs_rewrap,
            Err(error) => {
                report.record_invalid(
                    CredentialSecretStorage::WebhookCustomHeaders,
                    row.id,
                    secret_encryption_error_code(&error),
                );
                return Ok(());
            }
        },
        Value::Object(_) | Value::Null => true,
        _ => true,
    };
    let headers =
        match custom_headers::decrypt_and_validate_stored(cipher, row.id, &row.stored_value) {
            Ok(headers) => headers,
            Err(error) => {
                report.record_invalid(
                    CredentialSecretStorage::WebhookCustomHeaders,
                    row.id,
                    custom_headers_error_code(&error),
                );
                return Ok(());
            }
        };
    if !needs_rewrap {
        drop(headers);
        return Ok(());
    }
    report.webhook_header_rewrap_candidates += 1;
    if !apply {
        drop(headers);
        return Ok(());
    }
    let plaintext = headers.serialized()?;
    let encrypted =
        cipher.encrypt(SecretPurpose::WebhookCustomHeaders, row.id, plaintext.as_str())?;
    drop(plaintext);
    drop(headers);
    let result = sqlx::query(
        "update webhook_subscription
         set custom_headers_json = $2, updated_at = now()
         where id = $1 and custom_headers_json = $3",
    )
    .bind(row.id)
    .bind(Value::String(encrypted.as_str().to_owned()))
    .bind(&row.stored_value)
    .execute(postgres)
    .await?;
    if result.rows_affected() == 1 {
        report.webhook_headers_migrated += 1;
    } else {
        report.webhook_header_concurrent_changes += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use sqlx::postgres::PgPoolOptions;
    use uuid::Uuid;

    use super::{
        CredentialSecretMigrationError, CredentialSecretMigrationOptions,
        CredentialSecretMigrationReport, CredentialSecretStorage, MAX_INVALID_SAMPLES,
        migrate_credential_secrets,
    };
    use crate::shared::secret_encryption::CredentialCipher;

    #[test]
    fn migration_defaults_to_dry_run_and_a_bounded_batch() {
        let options = CredentialSecretMigrationOptions::default();

        assert!(!options.apply);
        assert!((1..=1_000).contains(&options.batch_size));
    }

    #[test]
    fn invalid_findings_are_counted_but_samples_are_bounded_and_redacted() {
        let mut report = CredentialSecretMigrationReport::default();
        for _ in 0..(MAX_INVALID_SAMPLES + 7) {
            report.record_invalid(
                CredentialSecretStorage::WebhookSigningSecret,
                Uuid::now_v7(),
                "authentication_failed",
            );
        }

        assert_eq!(report.invalid_values(), MAX_INVALID_SAMPLES + 7);
        assert_eq!(report.invalid_samples.len(), MAX_INVALID_SAMPLES);
        let serialized = serde_json::to_string(&report).expect("report must serialize");
        assert!(!serialized.contains("ironrag:enc:"));
        assert!(!serialized.contains("ciphertext"));
    }

    #[tokio::test]
    async fn migration_requires_key_before_first_database_query() {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://127.0.0.1:1/unreachable")
            .expect("syntactically valid lazy database URL");
        let cipher = CredentialCipher::from_optional_base64(None)
            .expect("missing key is a supported runtime state");

        let error =
            migrate_credential_secrets(&pool, &cipher, CredentialSecretMigrationOptions::default())
                .await
                .expect_err("maintenance must fail before trying the unreachable database");

        assert!(matches!(error, CredentialSecretMigrationError::CredentialProtection(_)));
    }

    #[tokio::test]
    async fn invalid_batch_is_rejected_before_database_access() {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://127.0.0.1:1/unreachable")
            .expect("syntactically valid lazy database URL");
        let encoded_key = STANDARD.encode([23_u8; 32]);
        let cipher =
            CredentialCipher::from_optional_base64(Some(&encoded_key)).expect("valid master key");

        let error = migrate_credential_secrets(
            &pool,
            &cipher,
            CredentialSecretMigrationOptions { apply: false, batch_size: 0 },
        )
        .await
        .expect_err("zero batch must fail before trying the unreachable database");

        assert!(matches!(error, CredentialSecretMigrationError::InvalidBatchSize));
    }
}
