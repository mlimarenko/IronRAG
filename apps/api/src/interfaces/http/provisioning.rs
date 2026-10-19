use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{delete, get, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        iam::{GrantResourceKind, PrincipalKind},
        ops::OpsAsyncOperationStatus,
    },
    infra::repositories::{catalog_repository, iam_repository, ops_repository},
    interfaces::http::{
        auth::{AuthContext, AuthTokenKind},
        catalog::{
            CatalogDeletionAcceptedResponse, CatalogDeletionWorker, spawn_library_deletion_worker,
            spawn_workspace_deletion_worker,
        },
        router_support::{ApiError, RequestId, RestAuditEvent, record_rest_audit_event},
    },
    services::{
        catalog_service::{CatalogDeletionAdmission, CreateLibraryCommand, CreateWorkspaceCommand},
        iam::{
            audit::AppendAuditEventSubjectCommand,
            service::MintApiTokenGrantCommand,
            service_identity::{
                CreateServiceIdentityCommand, ServiceIdentityOperationCommand, ServiceIdentityOwner,
            },
        },
    },
};

const MAX_PROVISIONED_WORKSPACES_PER_IDENTITY: i64 = 1_000;
const MAX_ACTIVE_LIBRARIES_PER_PROVISIONED_WORKSPACE: i64 = 10_000;
const MAX_CHILD_KEYS_PER_PROVISIONED_WORKSPACE: i64 = 10_000;
const ALLOWED_CHILD_PERMISSIONS: &[&str] = &["library_read", "library_write", "query_run"];

#[derive(Clone)]
struct ProvisioningActor {
    principal_id: Uuid,
    owner: ServiceIdentityOwner,
}

/// One successful integration-provisioning audit event. Every provisioner
/// operation is recorded as `succeeded` with a fixed summary, so only the
/// actor, request and subject vary.
struct ProvisioningAuditEvent {
    request_id: Option<String>,
    action_kind: String,
    subject_kind: String,
    subject_id: Uuid,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
}

/// Appends one integration-provisioning audit event through the canonical REST
/// emitter, framing the caller's subject as the single audit subject.
async fn record_provisioning_audit_event(
    state: &AppState,
    actor_principal_id: Uuid,
    event: ProvisioningAuditEvent,
) {
    let ProvisioningAuditEvent {
        request_id,
        action_kind,
        subject_kind,
        subject_id,
        workspace_id,
        library_id,
    } = event;
    record_rest_audit_event(
        state,
        actor_principal_id,
        RestAuditEvent {
            request_id,
            internal_message: Some(format!(
                "principal {actor_principal_id} completed {action_kind} for {subject_kind} {subject_id}"
            )),
            action_kind,
            result_kind: "succeeded".to_string(),
            redacted_message: Some("integration provisioning operation completed".to_string()),
            subjects: vec![AppendAuditEventSubjectCommand {
                subject_kind,
                subject_id,
                workspace_id,
                library_id,
                document_id: None,
            }],
        },
    )
    .await;
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CreateWorkspaceRequest {
    slug: Option<String>,
    display_name: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CreateLibraryRequest {
    slug: Option<String>,
    display_name: String,
    description: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CreateKeyRequest {
    idempotency_key: String,
    workspace_id: Uuid,
    library_ids: Vec<Uuid>,
    permission_kinds: Vec<String>,
    label: String,
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct KeyOperationRequest {
    idempotency_key: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListKeysQuery {
    workspace_id: Option<Uuid>,
}

#[derive(Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProvisionedWorkspaceResponse {
    id: Uuid,
    slug: String,
    display_name: String,
    lifecycle_state: String,
    adopted: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProvisionedLibraryResponse {
    id: Uuid,
    workspace_id: Uuid,
    slug: String,
    display_name: String,
    description: Option<String>,
    lifecycle_state: String,
    adopted: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProvisionedKeyResponse {
    credential: String,
    service_identity: ProvisionedKeyIdentity,
    replayed: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProvisionedKeyIdentity {
    principal_id: Uuid,
    credential_prefix: String,
    label: String,
    status: String,
    workspace_id: Option<Uuid>,
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProvisioningDeletionOperationResponse {
    operation_id: Uuid,
    status: OpsAsyncOperationStatus,
    failure_code: Option<String>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/provisioning/workspaces", post(create_workspace))
        .route("/provisioning/workspaces/{workspace_id}", delete(delete_workspace))
        .route("/provisioning/workspaces/{workspace_id}/libraries", post(create_library))
        .route(
            "/provisioning/workspaces/{workspace_id}/libraries/{library_id}",
            delete(delete_library),
        )
        .route("/provisioning/keys", get(list_keys).post(create_key))
        .route("/provisioning/keys/{key_principal_id}/revoke", post(revoke_key))
        .route("/provisioning/operations/{operation_id}", get(get_deletion_operation))
}

async fn require_actor(
    state: &AppState,
    auth: &AuthContext,
) -> Result<ProvisioningActor, ApiError> {
    if !matches!(auth.token_kind, AuthTokenKind::Principal(PrincipalKind::Worker))
        || auth.system_role.is_some()
        || auth.is_system_admin
    {
        return Err(ApiError::forbidden("integration provisioning authority is required"));
    }
    let identity = iam_repository::get_provisioning_identity_by_principal_id(
        &state.persistence.postgres,
        auth.principal_id,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    .filter(|row| row.status == "active")
    .filter(|row| row.expires_at.is_none_or(|expiry| expiry > Utc::now()))
    .ok_or_else(|| ApiError::forbidden("integration provisioning authority is required"))?;
    Ok(ProvisioningActor {
        principal_id: auth.principal_id,
        owner: ServiceIdentityOwner {
            principal_id: auth.principal_id,
            auth_provider_kind: identity.owner_auth_provider_kind,
            external_subject: identity.owner_external_subject,
        },
    })
}

async fn lock_actor(
    transaction: &mut sqlx::PgConnection,
    actor: &ProvisioningActor,
) -> Result<Option<DateTime<Utc>>, ApiError> {
    let row = iam_repository::get_service_identity_for_owner_with_transaction(
        transaction,
        actor.principal_id,
        &actor.owner.auth_provider_kind,
        &actor.owner.external_subject,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    if let Some(row) = row.filter(|row| {
        row.provisioning_authority.as_deref() == Some("integration")
            && row.status == "active"
            && row.expires_at.is_none_or(|expiry| expiry > Utc::now())
    }) {
        Ok(row.expires_at)
    } else {
        Err(ApiError::forbidden("integration provisioning authority is required"))
    }
}

async fn lock_owned_workspace(
    transaction: &mut sqlx::PgConnection,
    actor: &ProvisioningActor,
    workspace_id: Uuid,
) -> Result<catalog_repository::CatalogWorkspaceRow, ApiError> {
    catalog_repository::lock_provisioned_workspace(transaction, workspace_id, actor.principal_id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("workspace", workspace_id))
}

async fn lock_owned_library(
    transaction: &mut sqlx::PgConnection,
    actor: &ProvisioningActor,
    workspace_id: Uuid,
    library_id: Uuid,
) -> Result<catalog_repository::CatalogLibraryRow, ApiError> {
    catalog_repository::lock_provisioned_library(
        transaction,
        library_id,
        workspace_id,
        actor.principal_id,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?
    .ok_or_else(|| ApiError::resource_not_found("library", library_id))
}

/// The target of a provisioning deletion operation, grouped so the admission
/// and replay lookups do not repeat the same five positional values.
#[derive(Clone, Copy)]
struct ProvisionedDeletionSubject<'a> {
    workspace_id: Uuid,
    library_id: Option<Uuid>,
    operation_kind: &'a str,
    subject_kind: &'a str,
    subject_id: Uuid,
}

async fn admit_provisioned_deletion(
    transaction: &mut sqlx::PgConnection,
    actor_principal_id: Uuid,
    subject: ProvisionedDeletionSubject<'_>,
    display_name: String,
) -> Result<crate::services::catalog_service::CatalogDeletionAdmission, ApiError> {
    let ProvisionedDeletionSubject {
        workspace_id,
        library_id,
        operation_kind,
        subject_kind,
        subject_id,
    } = subject;
    if let Some(operation) = ops_repository::get_latest_async_operation_by_subject_with_transaction(
        transaction,
        subject_kind,
        subject_id,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?
    .filter(|operation| operation.operation_kind == operation_kind)
    .filter(|operation| matches!(operation.status.as_str(), "accepted" | "processing"))
    {
        return Ok(crate::services::catalog_service::CatalogDeletionAdmission {
            operation_id: operation.id,
            workspace_id,
            library_id,
            display_name,
            should_start_worker: false,
        });
    }
    let operation = ops_repository::create_async_operation_with_transaction(
        transaction,
        &ops_repository::NewOpsAsyncOperation {
            workspace_id,
            library_id,
            operation_kind,
            surface_kind: "rest",
            requested_by_principal_id: Some(actor_principal_id),
            status: "processing",
            subject_kind,
            subject_id: Some(subject_id),
            parent_async_operation_id: None,
            completed_at: None,
            failure_code: None,
        },
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    Ok(crate::services::catalog_service::CatalogDeletionAdmission {
        operation_id: operation.id,
        workspace_id,
        library_id,
        display_name,
        should_start_worker: true,
    })
}

async fn find_provisioned_deletion_replay(
    transaction: &mut sqlx::PgConnection,
    actor_principal_id: Uuid,
    subject: ProvisionedDeletionSubject<'_>,
) -> Result<Option<crate::services::catalog_service::CatalogDeletionAdmission>, ApiError> {
    let ProvisionedDeletionSubject {
        workspace_id,
        library_id,
        operation_kind,
        subject_kind,
        subject_id,
    } = subject;
    let operation = ops_repository::get_provisioning_deletion_replay_with_transaction(
        transaction,
        ops_repository::ProvisioningDeletionReplayQuery {
            actor_principal_id,
            operation_kind,
            subject_kind,
            subject_id,
            workspace_id,
            library_id,
        },
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    Ok(operation.map(|operation| crate::services::catalog_service::CatalogDeletionAdmission {
        operation_id: operation.id,
        workspace_id,
        library_id,
        display_name: String::new(),
        should_start_worker: false,
    }))
}

/// The canonical `202 Accepted` body for an admitted provisioning deletion.
fn accepted_deletion_response(
    operation_id: Uuid,
    workspace_id: Uuid,
    library_id: Option<Uuid>,
) -> (StatusCode, Json<CatalogDeletionAcceptedResponse>) {
    (
        StatusCode::ACCEPTED,
        Json(CatalogDeletionAcceptedResponse { operation_id, workspace_id, library_id }),
    )
}

/// Commits the deletion-admission transaction and returns the canonical
/// accepted response for the replay short-circuit, where the operation already
/// exists and no worker is spawned.
async fn commit_and_accept(
    authorization: sqlx::Transaction<'static, sqlx::Postgres>,
    admission: &CatalogDeletionAdmission,
    library_id: Option<Uuid>,
) -> Result<(StatusCode, Json<CatalogDeletionAcceptedResponse>), ApiError> {
    authorization.commit().await.map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    Ok(accepted_deletion_response(admission.operation_id, admission.workspace_id, library_id))
}

#[utoipa::path(
    get,
    path = "/v1/provisioning/operations/{operation_id}",
    tag = "provisioning",
    operation_id = "getProvisioningDeletionOperation",
    description = "Returns a deletion operation only to the provisioning identity that requested it.",
    params(("operation_id" = Uuid, Path, description = "Deletion operation identifier")),
    responses(
        (status = 200, description = "Deletion operation status", body = ProvisioningDeletionOperationResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Integration provisioning authority required"),
        (status = 404, description = "Owned deletion operation not found"),
    ),
)]
pub(crate) async fn get_deletion_operation(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(operation_id): Path<Uuid>,
) -> Result<Json<ProvisioningDeletionOperationResponse>, ApiError> {
    let actor = require_actor(&state, &auth).await?;
    let operation = ops_repository::get_provisioning_deletion_operation(
        &state.persistence.postgres,
        operation_id,
        actor.principal_id,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?
    .ok_or_else(|| ApiError::resource_not_found("operation", operation_id))?;
    let status = OpsAsyncOperationStatus::from_db(&operation.status)
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    Ok(Json(ProvisioningDeletionOperationResponse {
        operation_id: operation.id,
        status,
        failure_code: operation.failure_code,
    }))
}

#[utoipa::path(
    post,
    path = "/v1/provisioning/workspaces",
    tag = "provisioning",
    operation_id = "createProvisionedWorkspace",
    description = "Creates a workspace owned by the calling provisioning identity. An existing slug is adopted only when its immutable provisioning owner and active lifecycle match.",
    request_body = CreateWorkspaceRequest,
    responses(
        (status = 201, description = "Workspace created", body = ProvisionedWorkspaceResponse),
        (status = 200, description = "Equivalent owned workspace adopted", body = ProvisionedWorkspaceResponse),
        (status = 400, description = "Invalid provisioning request"),
        (status = 403, description = "Integration provisioning authority required"),
        (status = 409, description = "Slug conflict or provisioning limit reached"),
    ),
)]
pub(crate) async fn create_workspace(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Json(request): Json<CreateWorkspaceRequest>,
) -> Result<(StatusCode, Json<ProvisionedWorkspaceResponse>), ApiError> {
    let actor = require_actor(&state, &auth).await?;
    let mut transaction = state
        .persistence
        .postgres
        .begin()
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    iam_repository::lock_service_identity_idempotency(
        transaction.as_mut(),
        "provisioning",
        &actor.principal_id.to_string(),
        "workspace",
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let actor_expires_at = lock_actor(transaction.as_mut(), &actor).await?;
    let created = state
        .application_services
        .catalog
        .create_workspace_for_provisioning(
            transaction.as_mut(),
            CreateWorkspaceCommand {
                slug: request.slug,
                display_name: request.display_name,
                created_by_principal_id: Some(actor.principal_id),
            },
        )
        .await?;
    let count = catalog_repository::count_provisioned_workspaces_with_transaction(
        transaction.as_mut(),
        actor.principal_id,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let (workspace, adopted) = match created.row {
        Some(row) => {
            if count > MAX_PROVISIONED_WORKSPACES_PER_IDENTITY {
                return Err(ApiError::Conflict("provisioned workspace limit reached".to_string()));
            }
            iam_repository::create_grant(
                transaction.as_mut(),
                &iam_repository::NewGrant {
                    principal_id: actor.principal_id,
                    resource_kind: "workspace",
                    resource_id: row.id,
                    permission_kind: "workspace_read",
                    granted_by_principal_id: Some(actor.principal_id),
                    expires_at: actor_expires_at,
                },
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
            (row, false)
        }
        None => {
            let row = catalog_repository::get_workspace_by_slug_with_transaction(
                transaction.as_mut(),
                &created.slug,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or(ApiError::Internal)?;
            let owned = catalog_repository::lock_provisioned_workspace(
                transaction.as_mut(),
                row.id,
                actor.principal_id,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
            if owned.is_none() || row.display_name != created.display_name {
                return Err(ApiError::Conflict(
                    "workspace slug conflicts with a different provisioning request".to_string(),
                ));
            }
            (row, true)
        }
    };
    transaction.commit().await.map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    record_provisioning_audit_event(
        &state,
        actor.principal_id,
        ProvisioningAuditEvent {
            request_id: request_id.map(|value| value.0.0),
            action_kind: "provisioning.workspace.create".to_string(),
            subject_kind: "workspace".to_string(),
            subject_id: workspace.id,
            workspace_id: Some(workspace.id),
            library_id: None,
        },
    )
    .await;
    let status = if adopted { StatusCode::OK } else { StatusCode::CREATED };
    Ok((
        status,
        Json(ProvisionedWorkspaceResponse {
            id: workspace.id,
            slug: workspace.slug,
            display_name: workspace.display_name,
            lifecycle_state: workspace.lifecycle_state,
            adopted,
        }),
    ))
}

#[utoipa::path(
    post,
    path = "/v1/provisioning/workspaces/{workspace_id}/libraries",
    tag = "provisioning",
    operation_id = "createProvisionedLibrary",
    description = "Creates a library under an active workspace owned by the calling provisioning identity. Adoption requires immutable ownership of both resources and no deletion in progress.",
    params(("workspace_id" = Uuid, Path, description = "Owned workspace identifier")),
    request_body = CreateLibraryRequest,
    responses(
        (status = 201, description = "Library created", body = ProvisionedLibraryResponse),
        (status = 200, description = "Equivalent library adopted", body = ProvisionedLibraryResponse),
        (status = 404, description = "Owned workspace not found"),
        (status = 409, description = "Slug conflict or provisioning limit reached"),
    ),
)]
pub(crate) async fn create_library(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(workspace_id): Path<Uuid>,
    Json(request): Json<CreateLibraryRequest>,
) -> Result<(StatusCode, Json<ProvisionedLibraryResponse>), ApiError> {
    let actor = require_actor(&state, &auth).await?;
    let mut transaction = state
        .persistence
        .postgres
        .begin()
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    iam_repository::lock_service_identity_idempotency(
        transaction.as_mut(),
        "provisioning",
        &actor.principal_id.to_string(),
        "catalog",
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let _ = lock_actor(transaction.as_mut(), &actor).await?;
    lock_owned_workspace(transaction.as_mut(), &actor, workspace_id).await?;
    let created = state
        .application_services
        .catalog
        .create_library_for_provisioning(
            &state,
            transaction.as_mut(),
            CreateLibraryCommand {
                workspace_id,
                slug: request.slug,
                display_name: request.display_name,
                description: request.description,
                created_by_principal_id: Some(actor.principal_id),
            },
        )
        .await?;
    let library_count = catalog_repository::count_active_libraries_with_transaction(
        transaction.as_mut(),
        workspace_id,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let (library, adopted) = match created.row {
        Some(row) => {
            if library_count > MAX_ACTIVE_LIBRARIES_PER_PROVISIONED_WORKSPACE {
                return Err(ApiError::Conflict("provisioned library limit reached".to_string()));
            }
            (row, false)
        }
        None => {
            let row = catalog_repository::get_library_by_workspace_and_slug_with_transaction(
                transaction.as_mut(),
                workspace_id,
                &created.slug,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or(ApiError::Internal)?;
            let owned = catalog_repository::lock_provisioned_library(
                transaction.as_mut(),
                row.id,
                workspace_id,
                actor.principal_id,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
            if owned.is_none()
                || row.display_name != created.display_name
                || row.description != created.description
            {
                return Err(ApiError::Conflict(
                    "library slug conflicts with a different provisioning request".to_string(),
                ));
            }
            (row, true)
        }
    };
    transaction.commit().await.map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    record_provisioning_audit_event(
        &state,
        actor.principal_id,
        ProvisioningAuditEvent {
            request_id: request_id.map(|value| value.0.0),
            action_kind: "provisioning.library.create".to_string(),
            subject_kind: "library".to_string(),
            subject_id: library.id,
            workspace_id: Some(workspace_id),
            library_id: Some(library.id),
        },
    )
    .await;
    let status = if adopted { StatusCode::OK } else { StatusCode::CREATED };
    Ok((
        status,
        Json(ProvisionedLibraryResponse {
            id: library.id,
            workspace_id,
            slug: library.slug,
            display_name: library.display_name,
            description: library.description,
            lifecycle_state: library.lifecycle_state,
            adopted,
        }),
    ))
}

#[utoipa::path(
    delete,
    path = "/v1/provisioning/workspaces/{workspace_id}",
    tag = "provisioning",
    operation_id = "deleteProvisionedWorkspace",
    description = "Admits deletion only for an active workspace and all of its library children whose immutable provisioning owner is the caller. Workspace grants do not establish ownership.",
    params(("workspace_id" = Uuid, Path, description = "Owned workspace identifier")),
    responses(
        (status = 202, description = "Workspace deletion admitted", body = CatalogDeletionAcceptedResponse),
        (status = 404, description = "Owned workspace not found"),
    ),
)]
pub(crate) async fn delete_workspace(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(workspace_id): Path<Uuid>,
) -> Result<(StatusCode, Json<CatalogDeletionAcceptedResponse>), ApiError> {
    let actor = require_actor(&state, &auth).await?;
    let mut authorization = state
        .persistence
        .postgres
        .begin()
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let _ = lock_actor(authorization.as_mut(), &actor).await?;
    let subject = ProvisionedDeletionSubject {
        workspace_id,
        library_id: None,
        operation_kind: "delete_workspace",
        subject_kind: "catalog_workspace",
        subject_id: workspace_id,
    };
    if let Some(admission) =
        find_provisioned_deletion_replay(authorization.as_mut(), actor.principal_id, subject)
            .await?
    {
        return commit_and_accept(authorization, &admission, None).await;
    }
    let workspace = catalog_repository::lock_provisioned_workspace_for_deletion(
        authorization.as_mut(),
        workspace_id,
        actor.principal_id,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?
    .ok_or_else(|| ApiError::resource_not_found("workspace", workspace_id))?;
    let owns_every_library = catalog_repository::lock_workspace_libraries_owned_by_provisioner(
        authorization.as_mut(),
        workspace_id,
        actor.principal_id,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    if !owns_every_library {
        return Err(ApiError::resource_not_found("workspace", workspace_id));
    }
    let admission = admit_provisioned_deletion(
        authorization.as_mut(),
        actor.principal_id,
        subject,
        workspace.display_name,
    )
    .await?;
    authorization.commit().await.map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let request_id = request_id.map(|value| value.0.0);
    record_provisioning_audit_event(
        &state,
        actor.principal_id,
        ProvisioningAuditEvent {
            request_id: request_id.clone(),
            action_kind: "provisioning.workspace.delete".to_string(),
            subject_kind: "workspace".to_string(),
            subject_id: workspace_id,
            workspace_id: Some(workspace_id),
            library_id: None,
        },
    )
    .await;
    if admission.should_start_worker {
        spawn_workspace_deletion_worker(
            state.clone(),
            CatalogDeletionWorker {
                operation_id: admission.operation_id,
                workspace_id,
                display_name: admission.display_name,
                principal_id: auth.principal_id,
                request_id,
            },
        );
    }
    Ok(accepted_deletion_response(admission.operation_id, workspace_id, None))
}

#[utoipa::path(
    delete,
    path = "/v1/provisioning/workspaces/{workspace_id}/libraries/{library_id}",
    tag = "provisioning",
    operation_id = "deleteProvisionedLibrary",
    description = "Admits deletion only when the active workspace and active library have the calling provisioning identity as their immutable owner.",
    params(
        ("workspace_id" = Uuid, Path, description = "Owned workspace identifier"),
        ("library_id" = Uuid, Path, description = "Library identifier"),
    ),
    responses(
        (status = 202, description = "Library deletion admitted", body = CatalogDeletionAcceptedResponse),
        (status = 404, description = "Owned library not found"),
    ),
)]
pub(crate) async fn delete_library(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path((workspace_id, library_id)): Path<(Uuid, Uuid)>,
) -> Result<(StatusCode, Json<CatalogDeletionAcceptedResponse>), ApiError> {
    let actor = require_actor(&state, &auth).await?;
    let mut authorization = state
        .persistence
        .postgres
        .begin()
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let _ = lock_actor(authorization.as_mut(), &actor).await?;
    let subject = ProvisionedDeletionSubject {
        workspace_id,
        library_id: Some(library_id),
        operation_kind: "delete_library",
        subject_kind: "catalog_library",
        subject_id: library_id,
    };
    if let Some(admission) =
        find_provisioned_deletion_replay(authorization.as_mut(), actor.principal_id, subject)
            .await?
    {
        return commit_and_accept(authorization, &admission, Some(library_id)).await;
    }
    let _ = lock_owned_workspace(authorization.as_mut(), &actor, workspace_id).await?;
    let library =
        lock_owned_library(authorization.as_mut(), &actor, workspace_id, library_id).await?;
    let admission = admit_provisioned_deletion(
        authorization.as_mut(),
        actor.principal_id,
        subject,
        library.display_name,
    )
    .await?;
    authorization.commit().await.map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let request_id = request_id.map(|value| value.0.0);
    record_provisioning_audit_event(
        &state,
        actor.principal_id,
        ProvisioningAuditEvent {
            request_id: request_id.clone(),
            action_kind: "provisioning.library.delete".to_string(),
            subject_kind: "library".to_string(),
            subject_id: library_id,
            workspace_id: Some(workspace_id),
            library_id: Some(library_id),
        },
    )
    .await;
    if admission.should_start_worker {
        spawn_library_deletion_worker(
            state.clone(),
            CatalogDeletionWorker {
                operation_id: admission.operation_id,
                workspace_id,
                display_name: admission.display_name,
                principal_id: auth.principal_id,
                request_id,
            },
            library_id,
        );
    }
    Ok(accepted_deletion_response(admission.operation_id, workspace_id, Some(library_id)))
}

#[utoipa::path(
    post,
    path = "/v1/provisioning/keys",
    tag = "provisioning",
    operation_id = "createProvisionedKey",
    description = "Mints a library-scoped child credential only for active resources immutably owned by the caller. The child expiry cannot exceed the active parent identity expiry.",
    request_body = CreateKeyRequest,
    responses(
        (status = 200, description = "Library-scoped child credential created or replayed", body = ProvisionedKeyResponse),
        (status = 400, description = "Invalid scope or expiry"),
        (status = 404, description = "Owned workspace or library not found"),
        (status = 409, description = "Idempotency conflict or provisioning limit reached"),
    ),
)]
pub(crate) async fn create_key(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Json(request): Json<CreateKeyRequest>,
) -> Result<Json<ProvisionedKeyResponse>, ApiError> {
    let actor = require_actor(&state, &auth).await?;
    if request.library_ids.is_empty()
        || request.permission_kinds.is_empty()
        || request.permission_kinds.iter().any(|p| !ALLOWED_CHILD_PERMISSIONS.contains(&p.as_str()))
    {
        return Err(ApiError::BadRequest("child keys require libraries and only library_read, library_write, or query_run permissions".to_string()));
    }
    let mut transaction = state
        .persistence
        .postgres
        .begin()
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    iam_repository::lock_service_identity_idempotency(
        transaction.as_mut(),
        "provisioning",
        &actor.principal_id.to_string(),
        "catalog",
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let actor_expires_at = lock_actor(transaction.as_mut(), &actor).await?;
    let expires_at = bounded_child_expiry(request.expires_at, actor_expires_at)?;
    lock_owned_workspace(transaction.as_mut(), &actor, request.workspace_id).await?;
    for library_id in &request.library_ids {
        lock_owned_library(transaction.as_mut(), &actor, request.workspace_id, *library_id).await?;
    }
    let existing_operation =
        iam_repository::get_service_identity_operation_by_key_with_transaction(
            transaction.as_mut(),
            &actor.owner.auth_provider_kind,
            &actor.owner.external_subject,
            &request.idempotency_key,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    if existing_operation.is_none() {
        let child_count = iam_repository::count_child_keys_with_transaction(
            transaction.as_mut(),
            actor.principal_id,
            request.workspace_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if child_count >= MAX_CHILD_KEYS_PER_PROVISIONED_WORKSPACE {
            return Err(ApiError::Conflict("provisioned key limit reached".to_string()));
        }
    }
    let grants = request
        .library_ids
        .iter()
        .flat_map(|library_id| {
            request.permission_kinds.iter().map(move |permission_kind| MintApiTokenGrantCommand {
                resource_kind: GrantResourceKind::Library,
                resource_id: *library_id,
                permission_kind: permission_kind.clone(),
                expires_at,
            })
        })
        .collect();
    let mut outcome = state
        .application_services
        .iam
        .create_service_identity_with_transaction(
            &state,
            transaction.as_mut(),
            CreateServiceIdentityCommand {
                owner: actor.owner.clone(),
                workspace_id: Some(request.workspace_id),
                label: request.label,
                expires_at,
                grants,
                idempotency_key: request.idempotency_key,
                actor_principal_id: actor.principal_id,
                parent_principal_id: Some(actor.principal_id),
                provisioning_authority: None,
            },
        )
        .await?;
    transaction.commit().await.map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    record_provisioning_audit_event(
        &state,
        actor.principal_id,
        ProvisioningAuditEvent {
            request_id: request_id.map(|value| value.0.0),
            action_kind: "provisioning.key.create".to_string(),
            subject_kind: "service_identity".to_string(),
            subject_id: outcome.service_identity.principal_id,
            workspace_id: outcome.service_identity.workspace_id,
            library_id: None,
        },
    )
    .await;
    Ok(Json(ProvisionedKeyResponse {
        credential: std::mem::take(&mut outcome.credential),
        service_identity: map_key(outcome.service_identity.clone()),
        replayed: outcome.replayed,
    }))
}

fn bounded_child_expiry(
    requested: Option<DateTime<Utc>>,
    parent: Option<DateTime<Utc>>,
) -> Result<Option<DateTime<Utc>>, ApiError> {
    if requested.is_some_and(|expiry| expiry <= Utc::now()) {
        return Err(ApiError::BadRequest("expiresAt must be in the future".to_string()));
    }
    if requested.zip(parent).is_some_and(|(child, parent)| child > parent) {
        return Err(ApiError::BadRequest(
            "expiresAt cannot exceed the provisioning identity expiry".to_string(),
        ));
    }
    Ok(requested.or(parent))
}

#[utoipa::path(
    get,
    path = "/v1/provisioning/keys",
    tag = "provisioning",
    operation_id = "listProvisionedKeys",
    description = "Lists direct child identities. A workspace filter is accepted only for an active workspace immutably owned by the caller.",
    params(("workspaceId" = Option<Uuid>, Query, description = "Owned workspace filter")),
    responses((status = 200, description = "Child service identities", body = [ProvisionedKeyIdentity])),
)]
pub(crate) async fn list_keys(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ListKeysQuery>,
) -> Result<Json<Vec<ProvisionedKeyIdentity>>, ApiError> {
    let actor = require_actor(&state, &auth).await?;
    let mut transaction = state
        .persistence
        .postgres
        .begin()
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let _ = lock_actor(transaction.as_mut(), &actor).await?;
    if let Some(workspace_id) = query.workspace_id {
        lock_owned_workspace(transaction.as_mut(), &actor, workspace_id).await?;
    }
    transaction.commit().await.map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let rows = iam_repository::list_child_service_identities(
        &state.persistence.postgres,
        actor.principal_id,
        &actor.owner.auth_provider_kind,
        &actor.owner.external_subject,
        query.workspace_id,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    Ok(Json(rows.into_iter().map(map_key).collect()))
}

#[utoipa::path(
    post,
    path = "/v1/provisioning/keys/{key_principal_id}/revoke",
    tag = "provisioning",
    operation_id = "revokeProvisionedKey",
    description = "Revokes a direct child identity while transactionally rechecking the active parent provisioning authority.",
    params(("key_principal_id" = Uuid, Path, description = "Child principal identifier")),
    request_body = KeyOperationRequest,
    responses(
        (status = 204, description = "Child credential revoked"),
        (status = 404, description = "Owned child credential not found"),
        (status = 409, description = "Idempotency conflict"),
    ),
)]
pub(crate) async fn revoke_key(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(key_principal_id): Path<Uuid>,
    Json(request): Json<KeyOperationRequest>,
) -> Result<StatusCode, ApiError> {
    let actor = require_actor(&state, &auth).await?;
    let mut transaction = state
        .persistence
        .postgres
        .begin()
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let _ = lock_actor(transaction.as_mut(), &actor).await?;
    state
        .application_services
        .iam
        .revoke_provisioned_child_with_transaction(
            transaction.as_mut(),
            actor.principal_id,
            ServiceIdentityOperationCommand {
                owner: actor.owner,
                principal_id: key_principal_id,
                idempotency_key: request.idempotency_key,
            },
        )
        .await?;
    transaction.commit().await.map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    record_provisioning_audit_event(
        &state,
        actor.principal_id,
        ProvisioningAuditEvent {
            request_id: request_id.map(|value| value.0.0),
            action_kind: "provisioning.key.revoke".to_string(),
            subject_kind: "service_identity".to_string(),
            subject_id: key_principal_id,
            workspace_id: None,
            library_id: None,
        },
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

fn map_key(row: iam_repository::IamServiceIdentityRow) -> ProvisionedKeyIdentity {
    ProvisionedKeyIdentity {
        principal_id: row.principal_id,
        credential_prefix: row.token_prefix,
        label: row.label,
        status: row.status,
        workspace_id: row.workspace_id,
        expires_at: row.expires_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_permission_vocabulary_excludes_administration() {
        assert!(ALLOWED_CHILD_PERMISSIONS.contains(&"library_read"));
        assert!(ALLOWED_CHILD_PERMISSIONS.contains(&"library_write"));
        assert!(ALLOWED_CHILD_PERMISSIONS.contains(&"query_run"));
        assert!(!ALLOWED_CHILD_PERMISSIONS.contains(&"workspace_admin"));
        assert!(!ALLOWED_CHILD_PERMISSIONS.contains(&"document_write"));
        assert!(!ALLOWED_CHILD_PERMISSIONS.contains(&"credential_admin"));
    }

    #[test]
    fn child_expiry_inherits_parent_bound() {
        let parent = Utc::now() + chrono::Duration::hours(2);

        assert_eq!(bounded_child_expiry(None, Some(parent)).expect("valid bound"), Some(parent));
    }

    #[test]
    fn child_expiry_rejects_value_after_parent() {
        let parent = Utc::now() + chrono::Duration::hours(2);
        let child = parent + chrono::Duration::seconds(1);

        assert!(matches!(
            bounded_child_expiry(Some(child), Some(parent)),
            Err(ApiError::BadRequest(_))
        ));
    }
}
