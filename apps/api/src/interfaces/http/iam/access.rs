use super::*;

#[utoipa::path(
    get,
    path = "/v1/iam/users/{userId}/access",
    tag = "iam",
    operation_id = "getIamUserAccess",
    params(("userId" = uuid::Uuid, Path, description = "User principal id whose access is read")),
    responses(
        (status = 200, description = "The user's workspace and library access grants. Carries an `ETag` header (a hash of the current grant set) for optimistic concurrency on the paired PUT.", body = UserAccessResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a system administrator"),
        (status = 404, description = "User not found"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.get_user_access",
    skip_all,
    fields(principal_id = %principal_id)
)]
pub async fn get_user_access(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(principal_id): Path<Uuid>,
) -> Result<axum::response::Response, ApiError> {
    require_system_admin(&state, &auth).await?;

    // Confirm the target principal is a real user so callers get a clean 404
    // rather than an empty access list for a typo'd id.
    iam_repository::get_user_by_principal_id(&state.persistence.postgres, principal_id)
        .await
        .map_err(|error| {
            error!(auth_principal_id = %auth.principal_id, %principal_id, ?error, "failed to load user for access read");
            ApiError::Internal
        })?
        .ok_or_else(|| ApiError::resource_not_found("user", principal_id))?;

    let access = load_user_access(&state, principal_id).await?;
    Ok(with_access_etag(access))
}

/// Verifies the optional optimistic-concurrency precondition for a user access
/// update. Invalid header bytes retain the endpoint's previous behavior and are
/// treated as if no `If-Match` value was supplied.
async fn verify_user_access_revision(
    state: &AppState,
    principal_id: Uuid,
    headers: &HeaderMap,
) -> Result<(), ApiError> {
    let Some(if_match) =
        headers.get(header::IF_MATCH).and_then(|value| value.to_str().ok()).map(str::trim)
    else {
        return Ok(());
    };
    if if_match == "*" {
        return Ok(());
    }

    let current_access = load_user_access(state, principal_id).await?;
    let current_etag = compute_access_etag(&current_access);
    if if_match == current_etag {
        return Ok(());
    }

    Err(ApiError::StaleRevision(format!(
        "user access ETag is stale for principal {principal_id}; GET the latest access and retry the PUT with the current If-Match value"
    )))
}

#[utoipa::path(
    put,
    path = "/v1/iam/users/{userId}/access",
    tag = "iam",
    operation_id = "setIamUserAccess",
    params(("userId" = uuid::Uuid, Path, description = "User principal id whose access is set")),
    request_body = crate::interfaces::http::iam::types::SetUserAccessRequest,
    responses(
        (status = 200, description = "The user's access after reconciliation. Carries a fresh `ETag` header.", body = UserAccessResponse),
        (status = 400, description = "Invalid permission kind for a resource"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a system administrator"),
        (status = 404, description = "User, workspace or library not found"),
        (status = 409, description = "The optional `If-Match` header did not match the current access ETag; re-fetch with GET and retry"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.set_user_access",
    skip_all,
    fields(principal_id = %principal_id)
)]
pub async fn set_user_access(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(principal_id): Path<Uuid>,
    headers: HeaderMap,
    Json(payload): Json<SetUserAccessRequest>,
) -> Result<axum::response::Response, ApiError> {
    require_system_admin(&state, &auth).await?;
    let request_id = request_id.map(|value| value.0.0);

    iam_repository::get_user_by_principal_id(&state.persistence.postgres, principal_id)
        .await
        .map_err(|error| {
            error!(auth_principal_id = %auth.principal_id, %principal_id, ?error, "failed to load user for access write");
            ApiError::Internal
        })?
        .ok_or_else(|| ApiError::resource_not_found("user", principal_id))?;

    // Optional optimistic-concurrency check: if the caller sent `If-Match`,
    // the access set must not have changed since they last read it via GET.
    // `*` always passes (the resource is known to exist by this point). This
    // is the single write model's race guard (plan §5.2) — no parallel
    // grants write path is needed to close it.
    verify_user_access_revision(&state, principal_id, &headers).await?;

    // Build the desired (resource_kind, resource_id, permission_kind) set,
    // validating each permission against its resource kind and that each
    // referenced workspace/library exists.
    let mut desired: BTreeSet<(&'static str, Uuid, String)> = BTreeSet::new();
    for entry in &payload.workspaces {
        validate_permission_kind_for_resource(
            IamGrantResourceKind::Workspace,
            entry.permission_kind.clone(),
        )?;
        catalog_repository::get_workspace_by_id(&state.persistence.postgres, entry.workspace_id)
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("workspace", entry.workspace_id))?;
        desired.insert((
            "workspace",
            entry.workspace_id,
            entry.permission_kind.as_str().to_string(),
        ));
    }
    for entry in &payload.libraries {
        validate_permission_kind_for_resource(
            IamGrantResourceKind::Library,
            entry.permission_kind.clone(),
        )?;
        catalog_repository::get_library_by_id(&state.persistence.postgres, entry.library_id)
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("library", entry.library_id))?;
        desired.insert(("library", entry.library_id, entry.permission_kind.as_str().to_string()));
    }

    // Reconcile against the user's existing workspace/library grants. Only these
    // two resource kinds are managed here; system/document/etc. grants are left
    // untouched so this endpoint cannot be used to escalate to admin.
    let existing = iam_repository::list_grants_by_principal(&state.persistence.postgres, principal_id)
        .await
        .map_err(|error| {
            error!(auth_principal_id = %auth.principal_id, %principal_id, ?error, "failed to list grants for access reconcile");
            ApiError::Internal
        })?;

    let mut keep: BTreeSet<(&'static str, Uuid, String)> = BTreeSet::new();
    for grant in &existing {
        let kind = match grant.resource_kind.as_str() {
            "workspace" => "workspace",
            "library" => "library",
            _ => continue,
        };
        let key = (kind, grant.resource_id, grant.permission_kind.clone());
        if desired.contains(&key) {
            keep.insert(key);
        } else {
            // No longer desired: revoke it.
            state.application_services.iam.revoke_grant(&state, grant.id).await.map_err(|error| {
                error!(auth_principal_id = %auth.principal_id, %principal_id, grant_id = %grant.id, ?error, "failed to revoke grant during access reconcile");
                error
            })?;
        }
    }

    for (kind, resource_id, permission_kind) in &desired {
        if keep.contains(&(*kind, *resource_id, permission_kind.clone())) {
            continue;
        }
        let resource_kind = match *kind {
            "workspace" => GrantResourceKind::Workspace,
            _ => GrantResourceKind::Library,
        };
        state
            .application_services
            .iam
            .create_grant(
                &state,
                CreateGrantCommand {
                    principal_id,
                    resource_kind,
                    resource_id: *resource_id,
                    permission_kind: permission_kind.clone(),
                    granted_by_principal_id: Some(auth.principal_id),
                    expires_at: None,
                },
            )
            .await
            .map_err(|error| {
                error!(auth_principal_id = %auth.principal_id, %principal_id, ?error, "failed to create grant during access reconcile");
                ApiError::Internal
            })?;
    }

    record_rest_audit_event(
        &state,
        auth.principal_id,
        RestAuditEvent {
            request_id,
            action_kind: "iam.user.set_access".to_string(),
            result_kind: "succeeded".to_string(),
            redacted_message: Some("user access updated".to_string()),
            internal_message: Some(format!(
                "principal {} set access for user {} ({} workspace, {} library grants)",
                auth.principal_id,
                principal_id,
                payload.workspaces.len(),
                payload.libraries.len(),
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

    let access = load_user_access(&state, principal_id).await?;
    Ok(with_access_etag(access))
}

/// Wraps a `UserAccessResponse` body with an `ETag` header computed from its
/// current grant set. Shared by the GET and PUT handlers so the header a
/// client reads always matches what `compute_access_etag` would recompute
/// from that exact body.
fn with_access_etag(access: UserAccessResponse) -> axum::response::Response {
    let etag = compute_access_etag(&access);
    let mut response = Json(access).into_response();
    if let Ok(value) = HeaderValue::from_str(&etag) {
        response.headers_mut().insert(header::ETAG, value);
    }
    response
}

/// Computes a stable ETag for a user's declarative access set (workspace and
/// library grants), independent of iteration order and of `grantId` (an
/// internal correlation id, not semantically part of "access"). Two calls
/// with the same effective grant set always produce the same ETag.
fn compute_access_etag(access: &UserAccessResponse) -> String {
    let mut entries: Vec<(&'static str, Uuid, &str)> = access
        .workspaces
        .iter()
        .map(|entry| ("workspace", entry.workspace_id, entry.permission_kind.as_str()))
        .chain(
            access
                .libraries
                .iter()
                .map(|entry| ("library", entry.library_id, entry.permission_kind.as_str())),
        )
        .collect();
    entries.sort();

    let mut hasher = Sha256::new();
    hasher.update(access.principal_id.as_bytes());
    for (kind, resource_id, permission_kind) in entries {
        hasher.update(kind.as_bytes());
        hasher.update(b"|");
        hasher.update(resource_id.as_bytes());
        hasher.update(b"|");
        hasher.update(permission_kind.as_bytes());
        hasher.update(b"\n");
    }
    format!("\"{}\"", hex::encode(hasher.finalize()))
}

/// Loads a user's workspace- and library-scoped grants, joined with display
/// names, for the per-user access editor.
async fn load_user_access(
    state: &AppState,
    principal_id: Uuid,
) -> Result<UserAccessResponse, ApiError> {
    let grants = iam_repository::list_resolved_grants_by_principal(
        &state.persistence.postgres,
        principal_id,
    )
    .await
    .map_err(|error| {
        error!(%principal_id, ?error, "failed to resolve grants for user access");
        ApiError::Internal
    })?;

    let workspace_rows = catalog_repository::list_workspaces(&state.persistence.postgres)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let workspace_names: BTreeMap<Uuid, String> =
        workspace_rows.into_iter().map(|row| (row.id, row.display_name)).collect();
    let library_rows = catalog_repository::list_libraries(&state.persistence.postgres, None)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let library_names: BTreeMap<Uuid, (Uuid, String)> = library_rows
        .into_iter()
        .map(|row| (row.id, (row.workspace_id, row.display_name)))
        .collect();

    let mut workspaces = Vec::new();
    let mut libraries = Vec::new();
    for grant in grants {
        match grant.resource_kind.as_str() {
            "workspace" => {
                let display_name = workspace_names
                    .get(&grant.resource_id)
                    .cloned()
                    .unwrap_or_else(|| grant.resource_id.to_string());
                workspaces.push(UserWorkspaceAccessResponse {
                    grant_id: grant.id,
                    workspace_id: grant.resource_id,
                    display_name,
                    permission_kind: map_permission_kind(&grant.permission_kind)?,
                });
            }
            "library" => {
                let (workspace_id, display_name) =
                    library_names.get(&grant.resource_id).cloned().unwrap_or_else(|| {
                        (grant.workspace_id.unwrap_or_default(), grant.resource_id.to_string())
                    });
                libraries.push(UserLibraryAccessResponse {
                    grant_id: grant.id,
                    library_id: grant.resource_id,
                    workspace_id,
                    display_name,
                    permission_kind: map_permission_kind(&grant.permission_kind)?,
                });
            }
            _ => {}
        }
    }

    Ok(UserAccessResponse { principal_id, workspaces, libraries })
}
