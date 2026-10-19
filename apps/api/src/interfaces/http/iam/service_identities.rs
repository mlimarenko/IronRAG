use super::types::IamProvisioningAuthority;
use super::*;
use crate::services::iam::{
    service::MintApiTokenGrantCommand,
    service_identity::{
        CreateServiceIdentityCommand, PatchServiceIdentityCommand, ServiceIdentityOperationCommand,
        ServiceIdentityOwner,
    },
};
use chrono::{DateTime, Utc};

#[utoipa::path(
    get,
    path = "/v1/iam/service-identities",
    tag = "iam",
    operation_id = "listServiceIdentities",
    params(ListServiceIdentitiesQuery),
    responses(
        (status = 200, description = "Service identities owned by the authenticated OIDC administrator", body = [ServiceIdentityResponse]),
        (status = 401, description = "Caller is not authenticated or not a system administrator"),
        (status = 403, description = "System administrator is not backed by an OIDC subject"),
    ),
)]
pub async fn list_service_identities(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ListServiceIdentitiesQuery>,
) -> Result<Json<Vec<ServiceIdentityResponse>>, ApiError> {
    let owner = require_service_identity_owner(&state, &auth).await?;
    let rows = state
        .application_services
        .iam
        .list_service_identities(&state, &owner, query.workspace_id)
        .await?;
    Ok(Json(build_service_identity_responses(&state, rows).await?))
}

#[utoipa::path(
    post,
    path = "/v1/iam/service-identities",
    tag = "iam",
    operation_id = "createServiceIdentity",
    request_body = CreateServiceIdentityRequest,
    responses(
        (status = 200, description = "Created identity, or the identical idempotent replay", body = ServiceIdentityCredentialResponse),
        (status = 400, description = "Invalid request payload"),
        (status = 401, description = "Caller is not authenticated or not a system administrator"),
        (status = 403, description = "System administrator is not backed by an OIDC subject"),
        (status = 409, description = "Idempotency key conflicts with another request"),
    ),
)]
pub async fn create_service_identity(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Json(payload): Json<CreateServiceIdentityRequest>,
) -> Result<Json<ServiceIdentityCredentialResponse>, ApiError> {
    let owner = require_service_identity_owner(&state, &auth).await?;
    let provisioning_authority =
        payload.provisioning_authority.as_ref().map(IamProvisioningAuthority::as_str);
    let grants = if provisioning_authority.is_some() {
        if payload.workspace_id.is_some()
            || !payload.library_ids.is_empty()
            || !payload.permission_kinds.is_empty()
        {
            return Err(ApiError::BadRequest(
                "provisioningAuthority cannot be combined with workspaceId or grants".to_string(),
            ));
        }
        Vec::new()
    } else {
        validate_service_identity_grants(
            &state,
            payload
                .workspace_id
                .ok_or_else(|| ApiError::BadRequest("workspaceId is required".to_string()))?,
            payload.library_ids,
            payload.permission_kinds,
            payload.expires_at,
        )
        .await?
    };
    let mut outcome = state
        .application_services
        .iam
        .create_service_identity(
            &state,
            CreateServiceIdentityCommand {
                owner: owner.clone(),
                workspace_id: payload.workspace_id,
                label: payload.label,
                expires_at: payload.expires_at,
                grants,
                idempotency_key: payload.idempotency_key,
                actor_principal_id: owner.principal_id,
                parent_principal_id: None,
                provisioning_authority: provisioning_authority.map(str::to_string),
            },
        )
        .await?;
    let principal_id = outcome.service_identity.principal_id;
    let response = build_service_identity_responses(&state, vec![outcome.service_identity.clone()])
        .await?
        .pop()
        .ok_or(ApiError::Internal)?;
    record_service_identity_audit(
        &state,
        &auth,
        request_id,
        "iam.service_identity.create",
        principal_id,
        response.scope.workspace.as_ref().map(|workspace| workspace.id),
    )
    .await;
    Ok(Json(ServiceIdentityCredentialResponse {
        credential: std::mem::take(&mut outcome.credential),
        service_identity: response,
        replayed: outcome.replayed,
    }))
}

#[utoipa::path(
    get,
    path = "/v1/iam/service-identities/{serviceIdentityId}",
    tag = "iam",
    operation_id = "getServiceIdentity",
    params(("serviceIdentityId" = Uuid, Path, description = "Service identity principal id")),
    responses(
        (status = 200, description = "Owner-visible service identity", body = ServiceIdentityResponse),
        (status = 401, description = "Caller is not authenticated or not a system administrator"),
        (status = 403, description = "System administrator is not backed by an OIDC subject"),
        (status = 404, description = "Identity is absent or owned by another subject"),
    ),
)]
pub async fn get_service_identity(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(principal_id): Path<Uuid>,
) -> Result<Json<ServiceIdentityResponse>, ApiError> {
    let owner = require_service_identity_owner(&state, &auth).await?;
    let row =
        state.application_services.iam.get_service_identity(&state, &owner, principal_id).await?;
    Ok(Json(single_service_identity_response(&state, row).await?))
}

#[utoipa::path(
    patch,
    path = "/v1/iam/service-identities/{serviceIdentityId}",
    tag = "iam",
    operation_id = "patchServiceIdentity",
    params(("serviceIdentityId" = Uuid, Path, description = "Service identity principal id")),
    request_body = PatchServiceIdentityRequest,
    responses(
        (status = 200, description = "Identity metadata and exact direct library grants reconciled transactionally", body = ServiceIdentityResponse),
        (status = 400, description = "Invalid request payload"),
        (status = 401, description = "Caller is not authenticated or not a system administrator"),
        (status = 403, description = "System administrator is not backed by an OIDC subject"),
        (status = 404, description = "Identity is absent or owned by another subject"),
        (status = 409, description = "Identity is not active"),
    ),
)]
pub async fn patch_service_identity(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(principal_id): Path<Uuid>,
    Json(payload): Json<PatchServiceIdentityRequest>,
) -> Result<Json<ServiceIdentityResponse>, ApiError> {
    if payload.label.as_ref().is_some_and(|label| label.trim().is_empty()) {
        return Err(ApiError::BadRequest("label must not be empty".to_string()));
    }
    let owner = require_service_identity_owner(&state, &auth).await?;
    let existing =
        state.application_services.iam.get_service_identity(&state, &owner, principal_id).await?;
    let workspace_id = existing.workspace_id.ok_or_else(|| {
        ApiError::Conflict("provisioned service identity resource scope is immutable".to_string())
    })?;
    let effective_expiry = payload.expires_at.unwrap_or(existing.expires_at);
    let grants = validate_service_identity_grants(
        &state,
        workspace_id,
        payload.library_ids,
        payload.permission_kinds,
        effective_expiry,
    )
    .await?;
    let row = state
        .application_services
        .iam
        .patch_service_identity(
            &state,
            PatchServiceIdentityCommand {
                owner,
                principal_id,
                label: payload.label,
                expires_at: payload.expires_at,
                grants,
            },
        )
        .await?;
    let response = single_service_identity_response(&state, row).await?;
    record_service_identity_audit(
        &state,
        &auth,
        request_id,
        "iam.service_identity.patch",
        principal_id,
        Some(workspace_id),
    )
    .await;
    Ok(Json(response))
}

#[utoipa::path(
    post,
    path = "/v1/iam/service-identities/{serviceIdentityId}/rotate",
    tag = "iam",
    operation_id = "rotateServiceIdentity",
    params(("serviceIdentityId" = Uuid, Path, description = "Service identity principal id")),
    request_body = ServiceIdentityOperationRequest,
    responses(
        (status = 200, description = "Rotated credential, or the identical active idempotent receipt", body = ServiceIdentityCredentialResponse),
        (status = 401, description = "Caller is not authenticated or not a system administrator"),
        (status = 403, description = "System administrator is not backed by an OIDC subject"),
        (status = 404, description = "Identity is absent or owned by another subject"),
        (status = 409, description = "Identity is inactive or idempotency key conflicts"),
    ),
)]
pub async fn rotate_service_identity(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(principal_id): Path<Uuid>,
    Json(payload): Json<ServiceIdentityOperationRequest>,
) -> Result<Json<ServiceIdentityCredentialResponse>, ApiError> {
    let owner = require_service_identity_owner(&state, &auth).await?;
    let mut outcome = state
        .application_services
        .iam
        .rotate_service_identity(
            &state,
            ServiceIdentityOperationCommand {
                owner,
                principal_id,
                idempotency_key: payload.idempotency_key,
            },
        )
        .await?;
    let response =
        single_service_identity_response(&state, outcome.service_identity.clone()).await?;
    record_service_identity_audit(
        &state,
        &auth,
        request_id,
        "iam.service_identity.rotate",
        principal_id,
        response.scope.workspace.as_ref().map(|workspace| workspace.id),
    )
    .await;
    Ok(Json(ServiceIdentityCredentialResponse {
        credential: std::mem::take(&mut outcome.credential),
        service_identity: response,
        replayed: outcome.replayed,
    }))
}

#[utoipa::path(
    post,
    path = "/v1/iam/service-identities/{serviceIdentityId}/revoke",
    tag = "iam",
    operation_id = "revokeServiceIdentity",
    params(("serviceIdentityId" = Uuid, Path, description = "Service identity principal id")),
    request_body = ServiceIdentityOperationRequest,
    responses(
        (status = 200, description = "Revoked identity, or the identical idempotent replay", body = ServiceIdentityMutationResponse),
        (status = 401, description = "Caller is not authenticated or not a system administrator"),
        (status = 403, description = "System administrator is not backed by an OIDC subject"),
        (status = 404, description = "Identity is absent or owned by another subject"),
        (status = 409, description = "Idempotency key conflicts"),
    ),
)]
pub async fn revoke_service_identity(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(principal_id): Path<Uuid>,
    Json(payload): Json<ServiceIdentityOperationRequest>,
) -> Result<Json<ServiceIdentityMutationResponse>, ApiError> {
    let owner = require_service_identity_owner(&state, &auth).await?;
    let outcome = state
        .application_services
        .iam
        .revoke_service_identity(
            &state,
            ServiceIdentityOperationCommand {
                owner,
                principal_id,
                idempotency_key: payload.idempotency_key,
            },
        )
        .await?;
    let response = single_service_identity_response(&state, outcome.service_identity).await?;
    record_service_identity_audit(
        &state,
        &auth,
        request_id,
        "iam.service_identity.revoke",
        principal_id,
        response.scope.workspace.as_ref().map(|workspace| workspace.id),
    )
    .await;
    Ok(Json(ServiceIdentityMutationResponse {
        service_identity: response,
        replayed: outcome.replayed,
    }))
}

#[utoipa::path(
    delete,
    path = "/v1/iam/service-identities/{serviceIdentityId}",
    tag = "iam",
    operation_id = "deleteServiceIdentity",
    params(("serviceIdentityId" = Uuid, Path, description = "Revoked service identity principal id")),
    responses(
        (status = 204, description = "Revoked identity tombstoned"),
        (status = 401, description = "Caller is not authenticated or not a system administrator"),
        (status = 403, description = "System administrator is not backed by an OIDC subject"),
        (status = 404, description = "Identity is absent or owned by another subject"),
        (status = 409, description = "Identity must be revoked first"),
    ),
)]
pub async fn delete_service_identity(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(principal_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let owner = require_service_identity_owner(&state, &auth).await?;
    let existing =
        state.application_services.iam.get_service_identity(&state, &owner, principal_id).await?;
    state.application_services.iam.delete_service_identity(&state, &owner, principal_id).await?;
    record_service_identity_audit(
        &state,
        &auth,
        request_id,
        "iam.service_identity.delete",
        principal_id,
        existing.workspace_id,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn require_service_identity_owner(
    state: &AppState,
    auth: &AuthContext,
) -> Result<ServiceIdentityOwner, ApiError> {
    let user = require_system_admin(state, auth).await?;
    let issuer_is_oidc = user
        .auth_provider_kind
        .strip_prefix("oidc:")
        .is_some_and(|issuer_hash| !issuer_hash.is_empty());
    let Some(external_subject) =
        user.external_subject.as_deref().filter(|subject| !subject.trim().is_empty())
    else {
        return Err(ApiError::forbidden(
            "service identity ownership requires an OIDC issuer and subject",
        ));
    };
    if !issuer_is_oidc {
        return Err(ApiError::forbidden(
            "service identity ownership requires an OIDC issuer and subject",
        ));
    }
    Ok(ServiceIdentityOwner {
        principal_id: user.principal_id,
        auth_provider_kind: user.auth_provider_kind,
        external_subject: external_subject.to_string(),
    })
}

async fn validate_service_identity_grants(
    state: &AppState,
    workspace_id: Uuid,
    library_ids: Vec<Uuid>,
    permission_kinds: Vec<IamPermissionKind>,
    expires_at: Option<DateTime<Utc>>,
) -> Result<Vec<MintApiTokenGrantCommand>, ApiError> {
    let library_ids = dedupe_uuids(library_ids);
    let permission_kinds = dedupe_permissions(permission_kinds);
    if library_ids.is_empty() || permission_kinds.is_empty() {
        return Err(ApiError::BadRequest(
            "libraryIds and permissionKinds must both be non-empty".to_string(),
        ));
    }
    for permission_kind in &permission_kinds {
        validate_permission_kind_for_resource(
            IamGrantResourceKind::Library,
            permission_kind.clone(),
        )?;
    }
    for library_id in &library_ids {
        let library =
            catalog_repository::get_library_by_id(&state.persistence.postgres, *library_id)
                .await
                .map_err(|error| ApiError::internal_with_log(error, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("library", library_id))?;
        if library.workspace_id != workspace_id {
            return Err(ApiError::BadRequest(format!(
                "library {} does not belong to workspace {}",
                library.id, workspace_id
            )));
        }
    }
    let mut grants = Vec::with_capacity(library_ids.len() * permission_kinds.len());
    for library_id in library_ids {
        for permission_kind in &permission_kinds {
            grants.push(MintApiTokenGrantCommand {
                resource_kind: GrantResourceKind::Library,
                resource_id: library_id,
                permission_kind: permission_kind.as_str().to_string(),
                expires_at,
            });
        }
    }
    Ok(grants)
}

async fn single_service_identity_response(
    state: &AppState,
    row: iam_repository::IamServiceIdentityRow,
) -> Result<ServiceIdentityResponse, ApiError> {
    build_service_identity_responses(state, vec![row]).await?.pop().ok_or(ApiError::Internal)
}

async fn build_service_identity_responses(
    state: &AppState,
    rows: Vec<iam_repository::IamServiceIdentityRow>,
) -> Result<Vec<ServiceIdentityResponse>, ApiError> {
    let principal_ids = rows.iter().map(|row| row.principal_id).collect::<Vec<_>>();
    let grant_rows = iam_repository::list_resolved_grants_by_principal_ids(
        &state.persistence.postgres,
        &principal_ids,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let token_rows =
        rows.iter().map(iam_repository::IamServiceIdentityRow::api_token).collect::<Vec<_>>();
    let lookups =
        super::tokens::load_token_response_lookups(state, &token_rows, &grant_rows).await?;
    let mut grants_by_principal = BTreeMap::new();
    for grant in grant_rows {
        grants_by_principal.entry(grant.principal_id).or_insert_with(Vec::new).push(grant);
    }
    rows.into_iter()
        .map(|row| {
            let token = build_token_response(
                row.api_token(),
                grants_by_principal.remove(&row.principal_id).unwrap_or_default(),
                &lookups,
            )?;
            Ok(ServiceIdentityResponse {
                principal_id: row.principal_id,
                label: token.label,
                credential_prefix: token.token_prefix,
                status: token.status,
                expires_at: token.expires_at,
                revoked_at: token.revoked_at,
                last_used_at: token.last_used_at,
                owner: ServiceIdentityOwnerResponse {
                    issuer: row.owner_auth_provider_kind,
                    subject: row.owner_external_subject,
                },
                scope: token.scope,
                grants: token.grants,
            })
        })
        .collect()
}

async fn record_service_identity_audit(
    state: &AppState,
    auth: &AuthContext,
    request_id: Option<axum::Extension<RequestId>>,
    action_kind: &str,
    principal_id: Uuid,
    workspace_id: Option<Uuid>,
) {
    record_rest_audit_event(
        state,
        auth.principal_id,
        RestAuditEvent {
            request_id: request_id.map(|value| value.0.0),
            action_kind: action_kind.to_string(),
            result_kind: "succeeded".to_string(),
            redacted_message: Some("service identity lifecycle operation completed".to_string()),
            internal_message: Some(format!(
                "principal {} completed {} for service identity {}",
                auth.principal_id, action_kind, principal_id
            )),
            subjects: vec![AppendAuditEventSubjectCommand {
                subject_kind: "service_identity".to_string(),
                subject_id: principal_id,
                workspace_id,
                library_id: None,
                document_id: None,
            }],
        },
    )
    .await;
}
