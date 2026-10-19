use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zeroize::{Zeroize as _, Zeroizing};

use super::service::{IamService, MintApiTokenGrantCommand};
use crate::{
    app::state::AppState,
    domains::iam::GrantResourceKind,
    infra::repositories::iam_repository,
    interfaces::http::router_support::ApiError,
    shared::{
        auth_tokens::{hash_api_token, mint_plaintext_api_token, preview_api_token},
        secret_encryption::SecretPurpose,
    },
};

#[derive(Debug, Clone)]
pub struct ServiceIdentityOwner {
    pub principal_id: Uuid,
    pub auth_provider_kind: String,
    pub external_subject: String,
}

#[derive(Debug, Clone)]
pub struct CreateServiceIdentityCommand {
    pub owner: ServiceIdentityOwner,
    pub workspace_id: Option<Uuid>,
    pub label: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub grants: Vec<MintApiTokenGrantCommand>,
    pub idempotency_key: String,
    pub actor_principal_id: Uuid,
    pub parent_principal_id: Option<Uuid>,
    pub provisioning_authority: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PatchServiceIdentityCommand {
    pub owner: ServiceIdentityOwner,
    pub principal_id: Uuid,
    pub label: Option<String>,
    pub expires_at: Option<Option<DateTime<Utc>>>,
    pub grants: Vec<MintApiTokenGrantCommand>,
}

#[derive(Debug, Clone)]
pub struct ServiceIdentityOperationCommand {
    pub owner: ServiceIdentityOwner,
    pub principal_id: Uuid,
    pub idempotency_key: String,
}

#[derive(Clone)]
pub struct ServiceIdentityCredentialOutcome {
    pub credential: String,
    pub service_identity: iam_repository::IamServiceIdentityRow,
    pub replayed: bool,
}

impl std::fmt::Debug for ServiceIdentityCredentialOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServiceIdentityCredentialOutcome")
            .field("credential", &"<redacted>")
            .field("service_identity", &self.service_identity)
            .field("replayed", &self.replayed)
            .finish()
    }
}

impl Drop for ServiceIdentityCredentialOutcome {
    fn drop(&mut self) {
        self.credential.zeroize();
    }
}

#[derive(Debug, Clone)]
pub struct ServiceIdentityMutationOutcome {
    pub service_identity: iam_repository::IamServiceIdentityRow,
    pub replayed: bool,
}

fn normalize_idempotency_key(value: &str) -> Result<String, ApiError> {
    if value.is_empty()
        || value.len() > 128
        || value.trim() != value
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(ApiError::BadRequest(
            "idempotencyKey must contain 1-128 visible ASCII characters without surrounding whitespace"
                .to_string(),
        ));
    }
    Ok(value.to_string())
}

fn canonicalize_library_grants(
    grants: Vec<MintApiTokenGrantCommand>,
    expires_at: Option<DateTime<Utc>>,
) -> Result<Vec<MintApiTokenGrantCommand>, ApiError> {
    let mut seen = BTreeSet::new();
    let mut canonical = Vec::with_capacity(grants.len());
    for mut grant in grants {
        if !matches!(&grant.resource_kind, GrantResourceKind::Library) {
            return Err(ApiError::BadRequest(
                "service identities accept only direct library grants".to_string(),
            ));
        }
        let permission_kind = grant.permission_kind.trim();
        if permission_kind.is_empty() {
            return Err(ApiError::BadRequest("permission kind must not be empty".to_string()));
        }
        let key = (grant.resource_id, permission_kind.to_string());
        if seen.insert(key.clone()) {
            grant.permission_kind = key.1;
            grant.expires_at = expires_at;
            canonical.push(grant);
        }
    }
    if canonical.is_empty() {
        return Err(ApiError::BadRequest(
            "service identities require at least one direct library grant".to_string(),
        ));
    }
    canonical.sort_by(|left, right| {
        left.resource_id
            .cmp(&right.resource_id)
            .then_with(|| left.permission_kind.cmp(&right.permission_kind))
    });
    Ok(canonical)
}

fn fingerprint(value: serde_json::Value) -> String {
    let encoded = serde_json::to_vec(&value).expect("JSON Value serialization is infallible");
    hex::encode(Sha256::digest(encoded))
}

fn create_fingerprint(command: &CreateServiceIdentityCommand) -> String {
    fingerprint(json!({
        "kind": "create",
        "workspaceId": command.workspace_id,
        "label": command.label,
        "expiresAt": command.expires_at,
        "grants": command.grants.iter().map(|grant| json!({
            "resourceKind": "library",
            "resourceId": grant.resource_id,
            "permissionKind": grant.permission_kind,
            "expiresAt": grant.expires_at,
        })).collect::<Vec<_>>(),
        "provisioningAuthority": command.provisioning_authority,
    }))
}

fn lifecycle_fingerprint(kind: &str, principal_id: Uuid) -> String {
    fingerprint(json!({"kind": kind, "principalId": principal_id}))
}

fn validate_replay(
    operation: &iam_repository::IamServiceIdentityOperationRow,
    expected_kind: &str,
    expected_principal_id: Option<Uuid>,
    expected_fingerprint: &str,
    caller_principal_id: Uuid,
) -> Result<(), ApiError> {
    if operation.operation_kind != expected_kind
        || expected_principal_id
            .is_some_and(|principal_id| operation.service_identity_principal_id != principal_id)
        || operation.request_fingerprint != expected_fingerprint
        || operation.actor_principal_id != Some(caller_principal_id)
    {
        return Err(ApiError::Conflict(
            "idempotencyKey was already used for a different service identity operation"
                .to_string(),
        ));
    }
    Ok(())
}

fn decrypt_replay_credential(
    state: &AppState,
    operation: &iam_repository::IamServiceIdentityOperationRow,
) -> Result<String, ApiError> {
    let encrypted = operation.encrypted_credential.as_deref().ok_or_else(|| {
        ApiError::Conflict(
            "the credential receipt for this idempotent operation has been superseded".to_string(),
        )
    })?;
    state
        .credential_cipher
        .decrypt(SecretPurpose::ServiceIdentityCredential, operation.id, encrypted)
        .map(|credential| credential.expose_secret().to_string())
        .map_err(ApiError::from_secret_encryption)
}

async fn load_owned_identity(
    state: &AppState,
    owner: &ServiceIdentityOwner,
    principal_id: Uuid,
) -> Result<iam_repository::IamServiceIdentityRow, ApiError> {
    iam_repository::get_service_identity_for_owner(
        &state.persistence.postgres,
        principal_id,
        &owner.auth_provider_kind,
        &owner.external_subject,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?
    .ok_or_else(|| ApiError::resource_not_found("service_identity", principal_id))
}

/// Revoke every active child of an integration-provisioned service identity and
/// drop the authority's workspace grants, so revoking the parent cannot leave
/// orphaned child credentials behind.
async fn revoke_integration_provisioned_children(
    transaction: &mut sqlx::PgConnection,
    principal_id: Uuid,
) -> Result<(), ApiError> {
    let children =
        iam_repository::list_active_child_principal_ids_with_transaction(transaction, principal_id)
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    for child_principal_id in children {
        iam_repository::supersede_service_identity_credentials_with_transaction(
            transaction,
            child_principal_id,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        iam_repository::revoke_service_identity_with_transaction(transaction, child_principal_id)
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    }
    iam_repository::delete_workspace_grants_with_transaction(transaction, principal_id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    Ok(())
}

async fn record_service_identity_revocation(
    transaction: &mut sqlx::PgConnection,
    command: &ServiceIdentityOperationCommand,
    request_fingerprint: &str,
) -> Result<(), ApiError> {
    iam_repository::create_service_identity_operation_with_transaction(
        transaction,
        &iam_repository::NewServiceIdentityOperation {
            operation_id: Uuid::now_v7(),
            service_identity_principal_id: command.principal_id,
            owner_auth_provider_kind: &command.owner.auth_provider_kind,
            owner_external_subject: &command.owner.external_subject,
            actor_principal_id: command.owner.principal_id,
            operation_kind: "revoke",
            idempotency_key: &command.idempotency_key,
            request_fingerprint,
            credential_secret_version: None,
            encrypted_credential: None,
        },
    )
    .await
    .map(|_| ())
    .map_err(|error| ApiError::internal_with_log(error, "internal"))
}

async fn begin_idempotent_operation<'a>(
    state: &'a AppState,
    owner: &ServiceIdentityOwner,
    idempotency_key: &str,
) -> Result<
    (sqlx::Transaction<'a, sqlx::Postgres>, Option<iam_repository::IamServiceIdentityOperationRow>),
    ApiError,
> {
    let mut transaction = state
        .persistence
        .postgres
        .begin()
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let operation = load_idempotent_operation(transaction.as_mut(), owner, idempotency_key).await?;
    Ok((transaction, operation))
}

async fn load_idempotent_operation(
    transaction: &mut sqlx::PgConnection,
    owner: &ServiceIdentityOwner,
    idempotency_key: &str,
) -> Result<Option<iam_repository::IamServiceIdentityOperationRow>, ApiError> {
    iam_repository::lock_service_identity_idempotency(
        transaction,
        &owner.auth_provider_kind,
        &owner.external_subject,
        idempotency_key,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    iam_repository::get_service_identity_operation_by_key_with_transaction(
        transaction,
        &owner.auth_provider_kind,
        &owner.external_subject,
        idempotency_key,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))
}

impl IamService {
    pub(crate) async fn revoke_provisioned_child_with_transaction(
        &self,
        transaction: &mut sqlx::PgConnection,
        parent_principal_id: Uuid,
        mut command: ServiceIdentityOperationCommand,
    ) -> Result<ServiceIdentityMutationOutcome, ApiError> {
        command.idempotency_key = normalize_idempotency_key(&command.idempotency_key)?;
        let request_fingerprint = lifecycle_fingerprint("revoke", command.principal_id);
        let operation =
            load_idempotent_operation(transaction, &command.owner, &command.idempotency_key)
                .await?;
        if let Some(operation) = operation {
            validate_replay(
                &operation,
                "revoke",
                Some(command.principal_id),
                &request_fingerprint,
                command.owner.principal_id,
            )?;
            let service_identity =
                iam_repository::get_child_service_identity_for_parent_with_transaction(
                    transaction,
                    command.principal_id,
                    parent_principal_id,
                    &command.owner.auth_provider_kind,
                    &command.owner.external_subject,
                )
                .await
                .map_err(|error| ApiError::internal_with_log(error, "internal"))?
                .ok_or_else(|| {
                    ApiError::resource_not_found("service_identity", command.principal_id)
                })?;
            return Ok(ServiceIdentityMutationOutcome { service_identity, replayed: true });
        }
        iam_repository::get_child_service_identity_for_parent_with_transaction(
            transaction,
            command.principal_id,
            parent_principal_id,
            &command.owner.auth_provider_kind,
            &command.owner.external_subject,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("service_identity", command.principal_id))?;
        iam_repository::supersede_service_identity_credentials_with_transaction(
            transaction,
            command.principal_id,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        iam_repository::revoke_service_identity_with_transaction(transaction, command.principal_id)
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        record_service_identity_revocation(transaction, &command, &request_fingerprint).await?;
        let service_identity = iam_repository::get_service_identity_for_owner_with_transaction(
            transaction,
            command.principal_id,
            &command.owner.auth_provider_kind,
            &command.owner.external_subject,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("service_identity", command.principal_id))?;
        Ok(ServiceIdentityMutationOutcome { service_identity, replayed: false })
    }

    pub async fn create_service_identity(
        &self,
        state: &AppState,
        command: CreateServiceIdentityCommand,
    ) -> Result<ServiceIdentityCredentialOutcome, ApiError> {
        let mut transaction = state
            .persistence
            .postgres
            .begin()
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        let outcome = self
            .create_service_identity_with_transaction(state, transaction.as_mut(), command)
            .await?;
        transaction
            .commit()
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        Ok(outcome)
    }

    pub(crate) async fn create_service_identity_with_transaction(
        &self,
        state: &AppState,
        transaction: &mut sqlx::PgConnection,
        mut command: CreateServiceIdentityCommand,
    ) -> Result<ServiceIdentityCredentialOutcome, ApiError> {
        command.label = command.label.trim().to_string();
        if command.label.is_empty() {
            return Err(ApiError::BadRequest("label must not be empty".to_string()));
        }
        if command.expires_at.is_some_and(|expires_at| expires_at <= Utc::now()) {
            return Err(ApiError::BadRequest("expiresAt must be in the future".to_string()));
        }
        command.idempotency_key = normalize_idempotency_key(&command.idempotency_key)?;
        if command.provisioning_authority.is_some() {
            if command.provisioning_authority.as_deref() != Some("integration") {
                return Err(ApiError::BadRequest("unsupported provisioning authority".to_string()));
            }
            if command.workspace_id.is_some() || !command.grants.is_empty() {
                return Err(ApiError::BadRequest(
                    "a provisioning service identity cannot have workspaceId or grants".to_string(),
                ));
            }
        } else {
            command.grants = canonicalize_library_grants(command.grants, command.expires_at)?;
        }
        let request_fingerprint = create_fingerprint(&command);

        let operation =
            load_idempotent_operation(transaction, &command.owner, &command.idempotency_key)
                .await?;

        if let Some(operation) = operation {
            validate_replay(
                &operation,
                "create",
                None,
                &request_fingerprint,
                command.actor_principal_id,
            )?;
            let credential = decrypt_replay_credential(state, &operation)?;
            let principal_id = operation.service_identity_principal_id;
            let service_identity = iam_repository::get_service_identity_for_owner_with_transaction(
                transaction,
                principal_id,
                &command.owner.auth_provider_kind,
                &command.owner.external_subject,
            )
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("service_identity", principal_id))?;
            return Ok(ServiceIdentityCredentialOutcome {
                credential,
                service_identity,
                replayed: true,
            });
        }

        let credential = Zeroizing::new(mint_plaintext_api_token());
        let operation_id = Uuid::now_v7();
        let encrypted = state
            .credential_cipher
            .encrypt(SecretPurpose::ServiceIdentityCredential, operation_id, &credential)
            .map_err(ApiError::from_secret_encryption)?;
        let token = iam_repository::create_credential_principal_with_transaction(
            transaction,
            &iam_repository::NewCredentialPrincipal {
                principal_kind: "worker",
                workspace_id: command.workspace_id,
                label: &command.label,
                token_prefix: &preview_api_token(&credential),
                parent_principal_id: command.parent_principal_id,
                issued_by_principal_id: Some(command.actor_principal_id),
                expires_at: command.expires_at,
            },
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        let secret = iam_repository::create_api_token_secret_with_transaction(
            transaction,
            token.principal_id,
            &hash_api_token(&credential),
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        iam_repository::create_service_identity_with_transaction(
            transaction,
            token.principal_id,
            &command.owner.auth_provider_kind,
            &command.owner.external_subject,
            command.owner.principal_id,
            command.provisioning_authority.as_deref(),
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        for grant in &command.grants {
            iam_repository::create_grant(
                &mut *transaction,
                &iam_repository::NewGrant {
                    principal_id: token.principal_id,
                    resource_kind: "library",
                    resource_id: grant.resource_id,
                    permission_kind: &grant.permission_kind,
                    granted_by_principal_id: Some(command.actor_principal_id),
                    expires_at: grant.expires_at,
                },
            )
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        }
        iam_repository::create_service_identity_operation_with_transaction(
            transaction,
            &iam_repository::NewServiceIdentityOperation {
                operation_id,
                service_identity_principal_id: token.principal_id,
                owner_auth_provider_kind: &command.owner.auth_provider_kind,
                owner_external_subject: &command.owner.external_subject,
                actor_principal_id: command.actor_principal_id,
                operation_kind: "create",
                idempotency_key: &command.idempotency_key,
                request_fingerprint: &request_fingerprint,
                credential_secret_version: Some(secret.secret_version),
                encrypted_credential: Some(encrypted.as_str()),
            },
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        let service_identity = iam_repository::get_service_identity_for_owner_with_transaction(
            transaction,
            token.principal_id,
            &command.owner.auth_provider_kind,
            &command.owner.external_subject,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("service_identity", token.principal_id))?;
        Ok(ServiceIdentityCredentialOutcome {
            credential: credential.to_string(),
            service_identity,
            replayed: false,
        })
    }

    pub async fn list_service_identities(
        &self,
        state: &AppState,
        owner: &ServiceIdentityOwner,
        workspace_id: Option<Uuid>,
    ) -> Result<Vec<iam_repository::IamServiceIdentityRow>, ApiError> {
        iam_repository::list_service_identities_for_owner(
            &state.persistence.postgres,
            &owner.auth_provider_kind,
            &owner.external_subject,
            workspace_id,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))
    }

    pub async fn get_service_identity(
        &self,
        state: &AppState,
        owner: &ServiceIdentityOwner,
        principal_id: Uuid,
    ) -> Result<iam_repository::IamServiceIdentityRow, ApiError> {
        load_owned_identity(state, owner, principal_id).await
    }

    pub async fn patch_service_identity(
        &self,
        state: &AppState,
        mut command: PatchServiceIdentityCommand,
    ) -> Result<iam_repository::IamServiceIdentityRow, ApiError> {
        command.label = match command.label {
            Some(label) => {
                let label = label.trim().to_string();
                if label.is_empty() {
                    return Err(ApiError::BadRequest("label must not be empty".to_string()));
                }
                Some(label)
            }
            None => None,
        };
        let mut transaction = state
            .persistence
            .postgres
            .begin()
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        let existing = iam_repository::get_service_identity_for_owner_with_transaction(
            transaction.as_mut(),
            command.principal_id,
            &command.owner.auth_provider_kind,
            &command.owner.external_subject,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("service_identity", command.principal_id))?;
        if existing.status != "active" {
            return Err(ApiError::Conflict(
                "only an active service identity can be updated".to_string(),
            ));
        }
        if existing.provisioning_authority.is_some() || existing.parent_principal_id.is_some() {
            return Err(ApiError::Conflict(
                "provisioned service identity resource scope is immutable".to_string(),
            ));
        }
        let effective_expiry = command.expires_at.unwrap_or(existing.expires_at);
        command.grants = canonicalize_library_grants(command.grants, effective_expiry)?;
        iam_repository::update_service_identity_metadata_with_transaction(
            transaction.as_mut(),
            command.principal_id,
            command.label.as_deref(),
            command.expires_at,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        iam_repository::delete_service_identity_grants_with_transaction(
            transaction.as_mut(),
            command.principal_id,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        for grant in &command.grants {
            iam_repository::create_grant(
                transaction.as_mut(),
                &iam_repository::NewGrant {
                    principal_id: command.principal_id,
                    resource_kind: "library",
                    resource_id: grant.resource_id,
                    permission_kind: &grant.permission_kind,
                    granted_by_principal_id: Some(command.owner.principal_id),
                    expires_at: grant.expires_at,
                },
            )
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        }
        transaction
            .commit()
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        load_owned_identity(state, &command.owner, command.principal_id).await
    }

    pub async fn rotate_service_identity(
        &self,
        state: &AppState,
        mut command: ServiceIdentityOperationCommand,
    ) -> Result<ServiceIdentityCredentialOutcome, ApiError> {
        command.idempotency_key = normalize_idempotency_key(&command.idempotency_key)?;
        let request_fingerprint = lifecycle_fingerprint("rotate", command.principal_id);
        let (mut transaction, operation) =
            begin_idempotent_operation(state, &command.owner, &command.idempotency_key).await?;
        if let Some(operation) = operation {
            validate_replay(
                &operation,
                "rotate",
                Some(command.principal_id),
                &request_fingerprint,
                command.owner.principal_id,
            )?;
            let credential = decrypt_replay_credential(state, &operation)?;
            transaction
                .commit()
                .await
                .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
            let service_identity =
                load_owned_identity(state, &command.owner, command.principal_id).await?;
            return Ok(ServiceIdentityCredentialOutcome {
                credential,
                service_identity,
                replayed: true,
            });
        }
        let existing = iam_repository::get_service_identity_for_owner_with_transaction(
            transaction.as_mut(),
            command.principal_id,
            &command.owner.auth_provider_kind,
            &command.owner.external_subject,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("service_identity", command.principal_id))?;
        if existing.status != "active"
            || existing.expires_at.is_some_and(|expires_at| expires_at <= Utc::now())
        {
            return Err(ApiError::Conflict(
                "only an active, unexpired service identity can rotate credentials".to_string(),
            ));
        }

        let credential = Zeroizing::new(mint_plaintext_api_token());
        let operation_id = Uuid::now_v7();
        let encrypted = state
            .credential_cipher
            .encrypt(SecretPurpose::ServiceIdentityCredential, operation_id, &credential)
            .map_err(ApiError::from_secret_encryption)?;
        iam_repository::revoke_active_api_token_secrets_with_transaction(
            transaction.as_mut(),
            command.principal_id,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        let secret = iam_repository::create_api_token_secret_with_transaction(
            transaction.as_mut(),
            command.principal_id,
            &hash_api_token(&credential),
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        iam_repository::supersede_service_identity_credentials_with_transaction(
            transaction.as_mut(),
            command.principal_id,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        iam_repository::update_service_identity_token_prefix_with_transaction(
            transaction.as_mut(),
            command.principal_id,
            &preview_api_token(&credential),
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        iam_repository::create_service_identity_operation_with_transaction(
            transaction.as_mut(),
            &iam_repository::NewServiceIdentityOperation {
                operation_id,
                service_identity_principal_id: command.principal_id,
                owner_auth_provider_kind: &command.owner.auth_provider_kind,
                owner_external_subject: &command.owner.external_subject,
                actor_principal_id: command.owner.principal_id,
                operation_kind: "rotate",
                idempotency_key: &command.idempotency_key,
                request_fingerprint: &request_fingerprint,
                credential_secret_version: Some(secret.secret_version),
                encrypted_credential: Some(encrypted.as_str()),
            },
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        transaction
            .commit()
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        let service_identity =
            load_owned_identity(state, &command.owner, command.principal_id).await?;
        Ok(ServiceIdentityCredentialOutcome {
            credential: credential.to_string(),
            service_identity,
            replayed: false,
        })
    }

    pub async fn revoke_service_identity(
        &self,
        state: &AppState,
        mut command: ServiceIdentityOperationCommand,
    ) -> Result<ServiceIdentityMutationOutcome, ApiError> {
        command.idempotency_key = normalize_idempotency_key(&command.idempotency_key)?;
        let request_fingerprint = lifecycle_fingerprint("revoke", command.principal_id);
        let (mut transaction, operation) =
            begin_idempotent_operation(state, &command.owner, &command.idempotency_key).await?;
        if let Some(operation) = operation {
            validate_replay(
                &operation,
                "revoke",
                Some(command.principal_id),
                &request_fingerprint,
                command.owner.principal_id,
            )?;
            transaction
                .commit()
                .await
                .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
            let service_identity =
                load_owned_identity(state, &command.owner, command.principal_id).await?;
            return Ok(ServiceIdentityMutationOutcome { service_identity, replayed: true });
        }
        let existing = iam_repository::get_service_identity_for_owner_with_transaction(
            transaction.as_mut(),
            command.principal_id,
            &command.owner.auth_provider_kind,
            &command.owner.external_subject,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("service_identity", command.principal_id))?;
        if existing.provisioning_authority.as_deref() == Some("integration") {
            revoke_integration_provisioned_children(transaction.as_mut(), command.principal_id)
                .await?;
        }
        iam_repository::supersede_service_identity_credentials_with_transaction(
            transaction.as_mut(),
            command.principal_id,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        iam_repository::revoke_service_identity_with_transaction(
            transaction.as_mut(),
            command.principal_id,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        record_service_identity_revocation(transaction.as_mut(), &command, &request_fingerprint)
            .await?;
        transaction
            .commit()
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        let service_identity =
            load_owned_identity(state, &command.owner, command.principal_id).await?;
        Ok(ServiceIdentityMutationOutcome { service_identity, replayed: false })
    }

    pub async fn delete_service_identity(
        &self,
        state: &AppState,
        owner: &ServiceIdentityOwner,
        principal_id: Uuid,
    ) -> Result<(), ApiError> {
        let mut transaction = state
            .persistence
            .postgres
            .begin()
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        let existing = iam_repository::get_service_identity_for_owner_with_transaction(
            transaction.as_mut(),
            principal_id,
            &owner.auth_provider_kind,
            &owner.external_subject,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("service_identity", principal_id))?;
        if existing.status != "revoked" {
            return Err(ApiError::Conflict(
                "service identity must be revoked before deletion".to_string(),
            ));
        }
        let deleted = iam_repository::tombstone_revoked_service_identity_with_transaction(
            transaction.as_mut(),
            principal_id,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        if !deleted {
            return Err(ApiError::resource_not_found("service_identity", principal_id));
        }
        transaction
            .commit()
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        Ok(())
    }
}
