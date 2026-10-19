use super::*;

#[utoipa::path(
    get,
    path = "/v1/iam/tokens",
    tag = "iam",
    operation_id = "listIamTokens",
    params(crate::interfaces::http::iam::types::ListTokensQuery),
    responses(
        (status = 200, description = "API tokens visible to the IAM administrator", body = [TokenResponse]),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not an IAM administrator"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.list_tokens",
    skip_all,
    fields(workspace_id = ?query.workspace_id, item_count)
)]
pub async fn list_tokens(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ListTokensQuery>,
) -> Result<Json<Vec<TokenResponse>>, ApiError> {
    let span = tracing::Span::current();
    require_system_admin(&state, &auth).await?;
    let workspace_filter = resolve_workspace_filter(&auth, query.workspace_id)?;

    let rows = iam_repository::list_api_tokens(&state.persistence.postgres, workspace_filter)
        .await
        .map_err(|error| {
            error!(
                auth_principal_id = %auth.principal_id,
                workspace_id = ?workspace_filter,
                ?error,
                "failed to list api tokens",
            );
            ApiError::Internal
        })?;

    info!(
        auth_principal_id = %auth.principal_id,
        requested_workspace_id = ?workspace_filter,
        token_count = rows.len(),
        "listed api tokens",
    );

    let principal_ids = rows.iter().map(|row| row.principal_id).collect::<Vec<_>>();
    let grant_rows = iam_repository::list_resolved_grants_by_principal_ids(
        &state.persistence.postgres,
        &principal_ids,
    )
    .await
    .map_err(|error| {
        error!(
            auth_principal_id = %auth.principal_id,
            requested_workspace_id = ?workspace_filter,
            ?error,
            "failed to resolve api token grants",
        );
        ApiError::Internal
    })?;
    let lookups = load_token_response_lookups(&state, &rows, &grant_rows).await?;
    let mut grants_by_principal =
        BTreeMap::<Uuid, Vec<iam_repository::ResolvedIamGrantScopeRow>>::new();
    for grant_row in grant_rows {
        grants_by_principal.entry(grant_row.principal_id).or_default().push(grant_row);
    }
    let items = rows
        .into_iter()
        .map(|row| {
            let principal_id = row.principal_id;
            build_token_response(
                row,
                grants_by_principal.remove(&principal_id).unwrap_or_default(),
                &lookups,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    span.record("item_count", items.len());
    Ok(Json(items))
}

#[utoipa::path(
    post,
    path = "/v1/iam/tokens",
    tag = "iam",
    operation_id = "mintIamToken",
    request_body = crate::interfaces::http::iam::types::MintTokenRequest,
    responses(
        (status = 200, description = "Newly minted API token (plaintext only returned once)", body = MintTokenResponse),
        (status = 400, description = "Invalid request payload"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not an IAM administrator"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.mint_token",
    skip_all,
    fields(workspace_id = ?payload.workspace_id)
)]
pub async fn mint_token(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Json(payload): Json<MintTokenRequest>,
) -> Result<Json<MintTokenResponse>, ApiError> {
    require_system_admin(&state, &auth).await?;
    let mut workspace_id = Some(payload.workspace_id.ok_or_else(|| {
        ApiError::BadRequest("workspaceId is required for resource-scoped API tokens".to_string())
    })?);
    let expires_at = payload.expires_at;

    if payload.label.trim().is_empty() {
        return Err(ApiError::BadRequest("label must not be empty".into()));
    }
    let library_ids = dedupe_uuids(payload.library_ids);
    let permission_kinds = dedupe_permissions(payload.permission_kinds);

    if !library_ids.is_empty() && permission_kinds.is_empty() {
        return Err(ApiError::BadRequest(
            "permissionKinds must be provided when libraryIds are present".into(),
        ));
    }

    if !library_ids.is_empty() {
        let mut library_workspace_ids = BTreeSet::new();
        for library_id in &library_ids {
            let library =
                catalog_repository::get_library_by_id(&state.persistence.postgres, *library_id)
                    .await
                    .map_err(|error| {
                        error!(
                            auth_principal_id = %auth.principal_id,
                            library_id = %library_id,
                            ?error,
                            "failed to load library for iam token mint",
                        );
                        ApiError::Internal
                    })?
                    .ok_or_else(|| ApiError::resource_not_found("library", library_id))?;

            if let Some(requested_workspace_id) = workspace_id
                && library.workspace_id != requested_workspace_id
            {
                return Err(ApiError::BadRequest(format!(
                    "library {} does not belong to workspace {}",
                    library.id, requested_workspace_id
                )));
            }
            library_workspace_ids.insert(library.workspace_id);
        }

        if workspace_id.is_none() && library_workspace_ids.len() == 1 {
            workspace_id = library_workspace_ids.iter().copied().next();
        }
    }
    let grants = if permission_kinds.is_empty() {
        Vec::new()
    } else if !library_ids.is_empty() {
        build_mint_grants(MintGrantScope::Libraries(library_ids), &permission_kinds, expires_at)?
    } else {
        let workspace_id = workspace_id.ok_or_else(|| {
            ApiError::BadRequest(
                "workspaceId is required when permissionKinds are provided without libraryIds"
                    .to_string(),
            )
        })?;
        build_mint_grants(MintGrantScope::Workspace(workspace_id), &permission_kinds, expires_at)?
    };

    let mut outcome = state
        .application_services
        .iam
        .mint_api_token(
            &state,
            crate::services::iam::service::MintApiTokenCommand {
                workspace_id,
                label: payload.label,
                expires_at,
                grants,
                issued_by_principal_id: Some(auth.principal_id),
            },
        )
        .await?;

    let row = iam_repository::get_regular_api_token_by_principal_id(
        &state.persistence.postgres,
        outcome.api_token.principal_id,
    )
    .await
    .map_err(|error| {
        error!(
            auth_principal_id = %auth.principal_id,
            api_token_principal_id = %outcome.api_token.principal_id,
            ?error,
            "failed to reload minted api token",
        );
        ApiError::Internal
    })?
    .ok_or_else(|| ApiError::resource_not_found("api_token", outcome.api_token.principal_id))?;
    record_rest_audit_event(
        &state,
        auth.principal_id,
        RestAuditEvent {
            request_id: request_id.map(|value| value.0.0),
            action_kind: "iam.api_token.mint".to_string(),
            result_kind: "succeeded".to_string(),
            redacted_message: Some(format!("api token {} minted", row.label)),
            internal_message: Some(format!(
                "principal {} minted api token {}",
                auth.principal_id, row.principal_id
            )),
            subjects: vec![AppendAuditEventSubjectCommand {
                subject_kind: "api_token".to_string(),
                subject_id: row.principal_id,
                workspace_id: row.workspace_id,
                library_id: None,
                document_id: None,
            }],
        },
    )
    .await;

    let grant_rows = iam_repository::list_resolved_grants_by_principal(
        &state.persistence.postgres,
        row.principal_id,
    )
    .await
    .map_err(|error| {
        error!(
            auth_principal_id = %auth.principal_id,
            api_token_principal_id = %row.principal_id,
            ?error,
            "failed to resolve minted api token grants",
        );
        ApiError::Internal
    })?;
    let lookups =
        load_token_response_lookups(&state, std::slice::from_ref(&row), &grant_rows).await?;
    let api_token = build_token_response(row, grant_rows, &lookups)?;

    Ok(Json(MintTokenResponse { token: std::mem::take(&mut outcome.token), api_token }))
}

#[tracing::instrument(
    level = "info",
    name = "http.get_token",
    skip_all,
    fields(token_principal_id = %token_principal_id)
)]
#[utoipa::path(
    get,
    path = "/v1/iam/tokens/{tokenId}",
    tag = "iam",
    operation_id = "getIamToken",
    params(("tokenId" = uuid::Uuid, Path, description = "API token principal id")),
    responses(
        (status = 200, description = "API token detail (the plaintext secret is never returned)", body = TokenResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not an IAM administrator"),
        (status = 404, description = "Token not found"),
    ),
)]
pub async fn get_token(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(token_principal_id): Path<Uuid>,
) -> Result<Json<TokenResponse>, ApiError> {
    require_system_admin(&state, &auth).await?;

    let row = iam_repository::get_regular_api_token_by_principal_id(
        &state.persistence.postgres,
        token_principal_id,
    )
    .await
    .map_err(|error| {
        error!(
            auth_principal_id = %auth.principal_id,
            token_principal_id = %token_principal_id,
            ?error,
            "failed to load api token",
        );
        ApiError::Internal
    })?
    .ok_or_else(|| ApiError::resource_not_found("api_token", token_principal_id))?;

    authorize_workspace_scope_for_row(&auth, row.workspace_id)?;

    let grant_rows = iam_repository::list_resolved_grants_by_principal(
        &state.persistence.postgres,
        token_principal_id,
    )
    .await
    .map_err(|error| {
        error!(
            auth_principal_id = %auth.principal_id,
            token_principal_id = %token_principal_id,
            ?error,
            "failed to resolve api token grants",
        );
        ApiError::Internal
    })?;
    let lookups =
        load_token_response_lookups(&state, std::slice::from_ref(&row), &grant_rows).await?;
    Ok(Json(build_token_response(row, grant_rows, &lookups)?))
}

#[tracing::instrument(
    level = "info",
    name = "http.patch_token",
    skip_all,
    fields(token_principal_id = %token_principal_id)
)]
#[utoipa::path(
    patch,
    path = "/v1/iam/tokens/{tokenId}",
    tag = "iam",
    operation_id = "patchIamToken",
    params(("tokenId" = uuid::Uuid, Path, description = "API token principal id")),
    request_body = crate::interfaces::http::iam::types::PatchTokenRequest,
    responses(
        (status = 200, description = "Updated API token detail", body = TokenResponse),
        (status = 400, description = "Invalid request payload"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not an IAM administrator"),
        (status = 404, description = "Token not found"),
    ),
)]
pub async fn patch_token(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(token_principal_id): Path<Uuid>,
    Json(payload): Json<PatchTokenRequest>,
) -> Result<Json<TokenResponse>, ApiError> {
    require_system_admin(&state, &auth).await?;
    let request_id = request_id.map(|value| value.0.0);

    let existing = iam_repository::get_regular_api_token_by_principal_id(
        &state.persistence.postgres,
        token_principal_id,
    )
    .await
    .map_err(|error| {
        error!(
            auth_principal_id = %auth.principal_id,
            token_principal_id = %token_principal_id,
            ?error,
            "failed to load api token for patch",
        );
        ApiError::Internal
    })?
    .ok_or_else(|| ApiError::resource_not_found("api_token", token_principal_id))?;
    authorize_workspace_scope_for_row(&auth, existing.workspace_id)?;

    let label = match payload.label {
        Some(label) => {
            let trimmed = label.trim();
            if trimmed.is_empty() {
                return Err(ApiError::BadRequest("label must not be empty".into()));
            }
            Some(trimmed.to_string())
        }
        None => None,
    };

    let row = iam_repository::update_api_token(
        &state.persistence.postgres,
        token_principal_id,
        label.as_deref(),
        payload.expires_at,
    )
    .await
    .map_err(|error| {
        error!(
            auth_principal_id = %auth.principal_id,
            token_principal_id = %token_principal_id,
            ?error,
            "failed to update api token",
        );
        ApiError::Internal
    })?
    .ok_or_else(|| ApiError::resource_not_found("api_token", token_principal_id))?;

    record_rest_audit_event(
        &state,
        auth.principal_id,
        RestAuditEvent {
            request_id,
            action_kind: "iam.api_token.patch".to_string(),
            result_kind: "succeeded".to_string(),
            redacted_message: Some(format!("api token {} updated", row.label)),
            internal_message: Some(format!(
                "principal {} updated api token {}",
                auth.principal_id, token_principal_id
            )),
            subjects: vec![AppendAuditEventSubjectCommand {
                subject_kind: "api_token".to_string(),
                subject_id: token_principal_id,
                workspace_id: row.workspace_id,
                library_id: None,
                document_id: None,
            }],
        },
    )
    .await;

    let grant_rows = iam_repository::list_resolved_grants_by_principal(
        &state.persistence.postgres,
        token_principal_id,
    )
    .await
    .map_err(|error| {
        error!(
            auth_principal_id = %auth.principal_id,
            token_principal_id = %token_principal_id,
            ?error,
            "failed to resolve api token grants after patch",
        );
        ApiError::Internal
    })?;
    let lookups =
        load_token_response_lookups(&state, std::slice::from_ref(&row), &grant_rows).await?;
    Ok(Json(build_token_response(row, grant_rows, &lookups)?))
}

#[tracing::instrument(
    level = "info",
    name = "http.delete_token",
    skip_all,
    fields(token_principal_id = %token_principal_id)
)]
#[utoipa::path(
    delete,
    path = "/v1/iam/tokens/{tokenId}",
    tag = "iam",
    operation_id = "deleteIamToken",
    params(("tokenId" = uuid::Uuid, Path, description = "API token principal id to revoke and delete")),
    responses(
        (status = 204, description = "Token revoked and deleted"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not an IAM administrator"),
        (status = 404, description = "Token principal not found"),
    ),
)]
pub async fn delete_token(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(token_principal_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    require_system_admin(&state, &auth).await?;
    let request_id = request_id.map(|value| value.0.0);

    let row = iam_repository::get_regular_api_token_by_principal_id(
        &state.persistence.postgres,
        token_principal_id,
    )
    .await
    .map_err(|error| {
        error!(
            auth_principal_id = %auth.principal_id,
            token_principal_id = %token_principal_id,
            ?error,
            "failed to load api token for delete",
        );
        ApiError::Internal
    })?
    .ok_or_else(|| ApiError::resource_not_found("api_token", token_principal_id))?;

    if let Err(error) = authorize_workspace_scope_for_row(&auth, row.workspace_id) {
        record_rest_audit_event(
            &state,
            auth.principal_id,
            RestAuditEvent {
                request_id: request_id.clone(),
                action_kind: "iam.api_token.delete".to_string(),
                result_kind: "rejected".to_string(),
                redacted_message: Some("api token delete denied".to_string()),
                internal_message: Some(format!(
                    "principal {} was denied api token delete for {}",
                    auth.principal_id, token_principal_id
                )),
                subjects: vec![AppendAuditEventSubjectCommand {
                    subject_kind: "api_token".to_string(),
                    subject_id: token_principal_id,
                    workspace_id: row.workspace_id,
                    library_id: None,
                    document_id: None,
                }],
            },
        )
        .await;
        return Err(error);
    }

    if row.status != "revoked" {
        state.application_services.iam.revoke_api_token(&state, token_principal_id).await?;
    }

    state.application_services.iam.delete_revoked_api_token(&state, token_principal_id).await?;
    record_rest_audit_event(
        &state,
        auth.principal_id,
        RestAuditEvent {
            request_id,
            action_kind: "iam.api_token.delete".to_string(),
            result_kind: "succeeded".to_string(),
            redacted_message: Some(format!("api token {} deleted", row.label)),
            internal_message: Some(format!(
                "principal {} deleted api token {}",
                auth.principal_id, token_principal_id
            )),
            subjects: vec![AppendAuditEventSubjectCommand {
                subject_kind: "api_token".to_string(),
                subject_id: token_principal_id,
                workspace_id: row.workspace_id,
                library_id: None,
                document_id: None,
            }],
        },
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

pub(super) enum MintGrantScope {
    Workspace(Uuid),
    Libraries(Vec<Uuid>),
}

pub(super) fn build_mint_grants(
    scope: MintGrantScope,
    permission_kinds: &[IamPermissionKind],
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<Vec<crate::services::iam::service::MintApiTokenGrantCommand>, ApiError> {
    match scope {
        MintGrantScope::Workspace(workspace_id) => permission_kinds
            .iter()
            .map(|permission_kind| {
                validate_permission_kind_for_resource(
                    IamGrantResourceKind::Workspace,
                    permission_kind.clone(),
                )?;
                Ok(crate::services::iam::service::MintApiTokenGrantCommand {
                    resource_kind: GrantResourceKind::Workspace,
                    resource_id: workspace_id,
                    permission_kind: permission_kind.as_str().to_string(),
                    expires_at,
                })
            })
            .collect(),
        MintGrantScope::Libraries(library_ids) => {
            let mut grants = Vec::with_capacity(library_ids.len() * permission_kinds.len());
            for library_id in library_ids {
                for permission_kind in permission_kinds {
                    validate_permission_kind_for_resource(
                        IamGrantResourceKind::Library,
                        permission_kind.clone(),
                    )?;
                    grants.push(crate::services::iam::service::MintApiTokenGrantCommand {
                        resource_kind: GrantResourceKind::Library,
                        resource_id: library_id,
                        permission_kind: permission_kind.as_str().to_string(),
                        expires_at,
                    });
                }
            }
            Ok(grants)
        }
    }
}

pub(super) fn dedupe_uuids(values: Vec<Uuid>) -> Vec<Uuid> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::with_capacity(values.len());
    for value in values {
        if seen.insert(value) {
            deduped.push(value);
        }
    }
    deduped
}

pub(super) fn dedupe_permissions(values: Vec<IamPermissionKind>) -> Vec<IamPermissionKind> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::with_capacity(values.len());
    for value in values {
        if seen.insert(value.as_str()) {
            deduped.push(value);
        }
    }
    deduped
}

#[derive(Debug, Default)]
pub(super) struct TokenResponseLookups {
    pub(super) workspaces: BTreeMap<Uuid, TokenWorkspaceSummaryResponse>,
    pub(super) libraries: BTreeMap<Uuid, TokenLibrarySummaryResponse>,
    pub(super) issuers: BTreeMap<Uuid, TokenIssuerResponse>,
}

pub(super) async fn load_token_response_lookups(
    state: &AppState,
    token_rows: &[iam_repository::IamApiTokenRow],
    grant_rows: &[iam_repository::ResolvedIamGrantScopeRow],
) -> Result<TokenResponseLookups, ApiError> {
    let workspace_ids = token_rows
        .iter()
        .filter_map(|row| row.workspace_id)
        .chain(grant_rows.iter().filter_map(|row| row.workspace_id))
        .collect::<BTreeSet<_>>();
    let library_ids = grant_rows.iter().filter_map(|row| row.library_id).collect::<BTreeSet<_>>();
    let issuer_ids =
        token_rows.iter().filter_map(|row| row.issued_by_principal_id).collect::<BTreeSet<_>>();

    let workspace_rows = catalog_repository::list_workspaces(&state.persistence.postgres)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let workspaces = workspace_rows
        .into_iter()
        .filter(|row| workspace_ids.contains(&row.id))
        .map(|row| {
            (row.id, TokenWorkspaceSummaryResponse { id: row.id, display_name: row.display_name })
        })
        .collect();

    let library_rows = catalog_repository::list_libraries(&state.persistence.postgres, None)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let libraries = library_rows
        .into_iter()
        .filter(|row| library_ids.contains(&row.id))
        .map(|row| {
            (
                row.id,
                TokenLibrarySummaryResponse {
                    id: row.id,
                    workspace_id: row.workspace_id,
                    display_name: row.display_name,
                },
            )
        })
        .collect();

    let mut issuers = BTreeMap::new();
    for principal_id in issuer_ids {
        if let Some(principal_row) =
            iam_repository::get_principal_by_id(&state.persistence.postgres, principal_id)
                .await
                .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        {
            issuers.insert(
                principal_id,
                TokenIssuerResponse { principal_id, display_label: principal_row.display_label },
            );
        }
    }

    Ok(TokenResponseLookups { workspaces, libraries, issuers })
}

fn workspace_summary(
    workspace_id: Uuid,
    lookups: &TokenResponseLookups,
) -> TokenWorkspaceSummaryResponse {
    lookups.workspaces.get(&workspace_id).cloned().unwrap_or_else(|| {
        TokenWorkspaceSummaryResponse { id: workspace_id, display_name: workspace_id.to_string() }
    })
}

fn library_summary(
    library_id: Uuid,
    workspace_id: Option<Uuid>,
    lookups: &TokenResponseLookups,
) -> TokenLibrarySummaryResponse {
    lookups.libraries.get(&library_id).cloned().unwrap_or_else(|| TokenLibrarySummaryResponse {
        id: library_id,
        workspace_id: workspace_id.unwrap_or(Uuid::nil()),
        display_name: library_id.to_string(),
    })
}

pub(super) fn build_token_response(
    row: iam_repository::IamApiTokenRow,
    grant_rows: Vec<iam_repository::ResolvedIamGrantScopeRow>,
    lookups: &TokenResponseLookups,
) -> Result<TokenResponse, ApiError> {
    let grants = grant_rows
        .into_iter()
        .map(|grant_row| {
            let workspace =
                grant_row.workspace_id.map(|workspace_id| workspace_summary(workspace_id, lookups));
            let library = grant_row
                .library_id
                .map(|library_id| library_summary(library_id, grant_row.workspace_id, lookups));
            Ok(TokenGrantSummaryResponse {
                resource_kind: map_grant_resource_kind(&grant_row.resource_kind)?,
                resource_id: grant_row.resource_id,
                permission_kind: map_permission_kind(&grant_row.permission_kind)?,
                workspace,
                library,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;

    let mut libraries = grants
        .iter()
        .filter_map(|grant| grant.library.clone())
        .fold(BTreeMap::<Uuid, TokenLibrarySummaryResponse>::new(), |mut acc, library| {
            acc.entry(library.id).or_insert(library);
            acc
        })
        .into_values()
        .collect::<Vec<_>>();
    libraries.sort_by(|left, right| {
        left.display_name
            .to_ascii_lowercase()
            .cmp(&right.display_name.to_ascii_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });

    let workspace = row
        .workspace_id
        .map(|workspace_id| workspace_summary(workspace_id, lookups))
        .or_else(|| grants.iter().find_map(|grant| grant.workspace.clone()));
    let issuer = row.issued_by_principal_id.map(|principal_id| {
        lookups.issuers.get(&principal_id).cloned().unwrap_or_else(|| TokenIssuerResponse {
            principal_id,
            display_label: principal_id.to_string(),
        })
    });
    let scope_kind =
        if !libraries.is_empty() { TokenScopeKind::Library } else { TokenScopeKind::Workspace };

    Ok(TokenResponse {
        principal_id: row.principal_id,
        label: row.label,
        token_prefix: row.token_prefix,
        status: row.status,
        expires_at: row.expires_at,
        revoked_at: row.revoked_at,
        last_used_at: row.last_used_at,
        issuer,
        scope: TokenScopeResponse { kind: scope_kind, workspace, libraries },
        grants,
    })
}
