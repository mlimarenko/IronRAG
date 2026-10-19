use super::*;

#[utoipa::path(
    get,
    path = "/v1/iam/users",
    tag = "iam",
    operation_id = "listIamUsers",
    responses(
        (status = 200, description = "All user principals with their system roles", body = [UserResponse]),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a system administrator"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.list_users", skip_all)]
pub async fn list_users(
    auth: AuthContext,
    State(state): State<AppState>,
) -> Result<Json<Vec<UserResponse>>, ApiError> {
    require_system_admin(&state, &auth).await?;

    let rows = iam_repository::list_users(&state.persistence.postgres).await.map_err(|error| {
        error!(auth_principal_id = %auth.principal_id, ?error, "failed to list users");
        ApiError::Internal
    })?;

    Ok(Json(rows.into_iter().map(map_user_row).collect()))
}

#[utoipa::path(
    post,
    path = "/v1/iam/users",
    tag = "iam",
    operation_id = "createIamUser",
    request_body = crate::interfaces::http::iam::types::CreateUserRequest,
    responses(
        (status = 200, description = "Newly created user", body = UserResponse),
        (status = 400, description = "Invalid request payload"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a system administrator"),
        (status = 409, description = "Login or email already exists"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.create_user", skip_all)]
pub async fn create_user(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Json(payload): Json<CreateUserRequest>,
) -> Result<Json<UserResponse>, ApiError> {
    require_system_admin(&state, &auth).await?;
    let request_id = request_id.map(|value| value.0.0);

    let login = payload.login.trim().to_ascii_lowercase();
    if login.is_empty() {
        return Err(ApiError::BadRequest("login must not be empty".into()));
    }
    if login.bytes().any(|byte| !matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-')) {
        return Err(ApiError::BadRequest(
            "login must contain only lowercase letters, digits, '.', '_' or '-'".into(),
        ));
    }
    let email = payload.email.trim().to_string();
    if email.is_empty() {
        return Err(ApiError::BadRequest("email must not be empty".into()));
    }
    let display_name = {
        let trimmed = payload.display_name.trim();
        if trimmed.is_empty() { login.clone() } else { trimmed.to_string() }
    };
    let password = zeroize::Zeroizing::new(payload.password.trim().to_string());
    if password.len() < 8 {
        return Err(ApiError::BadRequest("password must be at least 8 characters long".into()));
    }

    if iam_repository::get_user_by_login(&state.persistence.postgres, &login)
        .await
        .map_err(|error| {
            error!(auth_principal_id = %auth.principal_id, ?error, "failed to check login uniqueness");
            ApiError::Internal
        })?
        .is_some()
    {
        return Err(ApiError::Conflict("a user with this login already exists".to_string()));
    }
    if iam_repository::get_user_by_email(&state.persistence.postgres, &email)
        .await
        .map_err(|error| {
            error!(auth_principal_id = %auth.principal_id, ?error, "failed to check email uniqueness");
            ApiError::Internal
        })?
        .is_some()
    {
        return Err(ApiError::Conflict("a user with this email already exists".to_string()));
    }

    let password_hash = crate::services::iam::service::hash_password(&password)?;
    let role = map_route_system_role_to_repo(payload.role);
    let row = iam_repository::create_user(
        &state.persistence.postgres,
        &login,
        &email,
        &display_name,
        &password_hash,
        role,
    )
    .await
    .map_err(|error| {
        error!(auth_principal_id = %auth.principal_id, ?error, "failed to create user");
        ApiError::Internal
    })?;

    record_rest_audit_event(
        &state,
        auth.principal_id,
        RestAuditEvent {
            request_id,
            action_kind: "iam.user.create".to_string(),
            result_kind: "succeeded".to_string(),
            redacted_message: Some(format!("user {} created", row.login)),
            internal_message: Some(format!(
                "principal {} created user {} with role {}",
                auth.principal_id, row.principal_id, row.role,
            )),
            subjects: vec![AppendAuditEventSubjectCommand {
                subject_kind: "principal".to_string(),
                subject_id: row.principal_id,
                workspace_id: None,
                library_id: None,
                document_id: None,
            }],
        },
    )
    .await;

    Ok(Json(map_user_row(row)))
}

#[tracing::instrument(
    level = "info",
    name = "http.delete_user",
    skip_all,
    fields(principal_id = %principal_id)
)]
#[utoipa::path(
    delete,
    path = "/v1/iam/users/{userId}",
    tag = "iam",
    operation_id = "deleteIamUser",
    params(("userId" = uuid::Uuid, Path, description = "User principal id to delete")),
    responses(
        (status = 204, description = "User deleted (sessions and grants revoked, principal disabled)"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a system administrator"),
        (status = 404, description = "User not found"),
        (status = 409, description = "Would delete the last remaining administrator"),
    ),
)]
pub async fn delete_user(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(principal_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    require_system_admin(&state, &auth).await?;
    let request_id = request_id.map(|value| value.0.0);

    let target =
        iam_repository::get_user_by_principal_id(&state.persistence.postgres, principal_id)
            .await
            .map_err(|error| {
                error!(auth_principal_id = %auth.principal_id, %principal_id, ?error, "failed to load user for delete");
                ApiError::Internal
            })?
            .ok_or_else(|| ApiError::resource_not_found("user", principal_id))?;

    match iam_repository::delete_user(&state.persistence.postgres, principal_id)
        .await
        .map_err(|error| {
            error!(auth_principal_id = %auth.principal_id, %principal_id, ?error, "failed to delete user");
            ApiError::Internal
        })? {
        iam_repository::DeleteUserOutcome::Deleted(_) => {}
        iam_repository::DeleteUserOutcome::NotFound => {
            return Err(ApiError::resource_not_found("user", principal_id));
        }
        iam_repository::DeleteUserOutcome::WouldRemoveLastAdmin => {
            return Err(ApiError::Conflict(
                "cannot delete the last remaining administrator".to_string(),
            ));
        }
    }

    record_rest_audit_event(
        &state,
        auth.principal_id,
        RestAuditEvent {
            request_id,
            action_kind: "iam.user.delete".to_string(),
            result_kind: "succeeded".to_string(),
            redacted_message: Some(format!("user {} deleted", target.login)),
            internal_message: Some(format!(
                "principal {} deleted user {}",
                auth.principal_id, principal_id
            )),
            subjects: vec![AppendAuditEventSubjectCommand {
                subject_kind: "principal".to_string(),
                subject_id: principal_id,
                workspace_id: None,
                library_id: None,
                document_id: None,
            }],
        },
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    patch,
    path = "/v1/iam/users/{userId}",
    tag = "iam",
    operation_id = "patchIamUser",
    params(("userId" = uuid::Uuid, Path, description = "User principal id whose role changes")),
    request_body = crate::interfaces::http::iam::types::SetUserRoleRequest,
    responses(
        (status = 200, description = "Updated user", body = UserResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a system administrator"),
        (status = 404, description = "User not found"),
        (status = 409, description = "Would demote the last remaining administrator"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.set_user_role",
    skip_all,
    fields(principal_id = %principal_id)
)]
pub async fn set_user_role(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(principal_id): Path<Uuid>,
    Json(payload): Json<SetUserRoleRequest>,
) -> Result<Json<UserResponse>, ApiError> {
    require_system_admin(&state, &auth).await?;
    let request_id = request_id.map(|value| value.0.0);
    let next_role = map_route_system_role_to_repo(payload.role);

    let row = match iam_repository::set_user_role(&state.persistence.postgres, principal_id, next_role)
        .await
        .map_err(|error| {
            error!(auth_principal_id = %auth.principal_id, %principal_id, ?error, "failed to set user role");
            ApiError::Internal
        })? {
        iam_repository::SetUserRoleOutcome::Updated(row) => row,
        iam_repository::SetUserRoleOutcome::NotFound => {
            return Err(ApiError::resource_not_found("user", principal_id));
        }
        iam_repository::SetUserRoleOutcome::WouldDemoteLastAdmin => {
            return Err(ApiError::Conflict(
                "cannot demote the last remaining administrator".to_string(),
            ));
        }
    };

    record_rest_audit_event(
        &state,
        auth.principal_id,
        RestAuditEvent {
            request_id,
            action_kind: "iam.user.set_role".to_string(),
            result_kind: "succeeded".to_string(),
            redacted_message: Some(format!("user {} role set to {}", row.login, row.role)),
            internal_message: Some(format!(
                "principal {} set user {} role to {}",
                auth.principal_id, principal_id, row.role
            )),
            subjects: vec![AppendAuditEventSubjectCommand {
                subject_kind: "principal".to_string(),
                subject_id: principal_id,
                workspace_id: None,
                library_id: None,
                document_id: None,
            }],
        },
    )
    .await;

    Ok(Json(map_user_row(row)))
}
