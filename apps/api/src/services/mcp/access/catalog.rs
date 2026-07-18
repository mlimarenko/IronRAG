use std::collections::BTreeSet;

use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        ai::AiBindingPurpose,
        catalog::{CatalogLibraryIngestionReadiness, CatalogLifecycleState},
        recognition::LibraryRecognitionPolicy,
    },
    infra::repositories::catalog_repository::{self, CatalogLibraryRow, CatalogWorkspaceRow},
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_LIBRARY_WRITE, POLICY_MCP_MEMORY_READ, POLICY_QUERY_RUN, POLICY_WORKSPACE_ADMIN,
            authorize_library_discovery, authorize_library_permission,
            authorize_workspace_discovery, authorize_workspace_permission,
        },
        router_support::{ApiError, map_library_create_error, map_workspace_create_error},
    },
    mcp_types::{
        McpCreateLibraryRequest, McpCreateWorkspaceRequest, McpLibraryDescriptor,
        McpLibraryIngestionReadiness, McpUpdateLibraryRequest, McpUpdateWorkspaceRequest,
        McpWorkspaceDescriptor,
    },
};

use super::types::VisibleLibraryContext;

const LIBRARY_REF_SEPARATOR: char = '/';
const AMBIGUOUS_QUERY_LIBRARY_MESSAGE: &str =
    "library could not be inferred; provide an authorized library ref";

#[derive(Debug, Default, PartialEq, Eq)]
struct QueryAuthorizedLibraryScope {
    all_libraries: bool,
    workspace_ids: Vec<Uuid>,
    library_ids: Vec<Uuid>,
}

#[must_use]
pub(crate) fn workspace_catalog_ref(workspace_slug: &str) -> String {
    workspace_slug.to_string()
}

#[must_use]
pub(crate) fn library_catalog_ref(workspace_slug: &str, library_slug: &str) -> String {
    format!("{workspace_slug}{LIBRARY_REF_SEPARATOR}{library_slug}")
}

fn parse_workspace_catalog_ref(value: &str) -> Result<String, ApiError> {
    let normalized = value.trim();
    if normalized.is_empty() {
        return Err(ApiError::invalid_mcp_tool_call("workspace must not be empty"));
    }
    if normalized.contains(LIBRARY_REF_SEPARATOR) {
        return Err(ApiError::invalid_mcp_tool_call(format!(
            "workspace ref '{normalized}' must not contain '{LIBRARY_REF_SEPARATOR}'"
        )));
    }
    Ok(normalized.to_string())
}

fn parse_library_catalog_ref(value: &str) -> Result<(String, String), ApiError> {
    let normalized = value.trim();
    if normalized.is_empty() {
        return Err(ApiError::invalid_mcp_tool_call("library must not be empty"));
    }
    let Some((workspace_slug, library_slug)) = normalized.split_once(LIBRARY_REF_SEPARATOR) else {
        return Err(ApiError::invalid_mcp_tool_call(format!(
            "library ref '{normalized}' must use '<workspace>/<library>'"
        )));
    };
    let workspace_slug = parse_workspace_catalog_ref(workspace_slug)?;
    let library_slug = library_slug.trim();
    if library_slug.is_empty() || library_slug.contains(LIBRARY_REF_SEPARATOR) {
        return Err(ApiError::invalid_mcp_tool_call(format!(
            "library ref '{normalized}' must use exactly one '{LIBRARY_REF_SEPARATOR}' separator"
        )));
    }
    Ok((workspace_slug, library_slug.to_string()))
}

async fn load_workspace_row_by_catalog_ref(
    state: &AppState,
    workspace_ref: &str,
) -> Result<CatalogWorkspaceRow, ApiError> {
    let workspace_ref = parse_workspace_catalog_ref(workspace_ref)?;
    catalog_repository::get_workspace_by_slug(&state.persistence.postgres, &workspace_ref)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("workspace", workspace_ref))
}

pub(crate) async fn load_workspace_by_catalog_ref(
    auth: &AuthContext,
    state: &AppState,
    workspace_ref: &str,
    accepted_permissions: &[&str],
) -> Result<CatalogWorkspaceRow, ApiError> {
    let workspace = load_workspace_row_by_catalog_ref(state, workspace_ref).await?;
    authorize_workspace_permission(auth, workspace.id, accepted_permissions)?;
    Ok(workspace)
}

pub(crate) async fn load_workspace_by_catalog_ref_for_discovery(
    auth: &AuthContext,
    state: &AppState,
    workspace_ref: &str,
) -> Result<CatalogWorkspaceRow, ApiError> {
    let workspace = load_workspace_row_by_catalog_ref(state, workspace_ref).await?;
    authorize_workspace_discovery(auth, workspace.id)?;
    Ok(workspace)
}

pub(crate) async fn load_library_by_catalog_ref(
    auth: &AuthContext,
    state: &AppState,
    library_ref: &str,
    accepted_permissions: &[&str],
) -> Result<CatalogLibraryRow, ApiError> {
    let (workspace_ref, library_slug) = parse_library_catalog_ref(library_ref)?;
    let workspace = load_workspace_row_by_catalog_ref(state, &workspace_ref).await?;
    let library = catalog_repository::get_library_by_workspace_and_slug(
        &state.persistence.postgres,
        workspace.id,
        &library_slug,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?
    .ok_or_else(|| ApiError::resource_not_found("library", library_ref))?;
    authorize_library_permission(auth, library.workspace_id, library.id, accepted_permissions)?;
    Ok(library)
}

pub(crate) async fn load_sole_query_authorized_library(
    auth: &AuthContext,
    state: &AppState,
) -> Result<CatalogLibraryRow, ApiError> {
    let scope = query_authorized_library_scope(auth);
    let candidate_ids = catalog_repository::list_query_authorized_library_candidates(
        &state.persistence.postgres,
        scope.all_libraries,
        &scope.workspace_ids,
        &scope.library_ids,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let library_id = select_sole_query_authorized_library_id(candidate_ids)?;
    let library = catalog_repository::get_library_by_id(&state.persistence.postgres, library_id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .ok_or_else(ambiguous_query_library_error)?;

    // Defend against a lifecycle or authorization change between the bounded
    // candidate lookup and the targeted row load. Both cases intentionally
    // collapse to the same non-enumerating inference error.
    if library.lifecycle_state != "active"
        || !auth.has_library_permission(library.workspace_id, library.id, POLICY_QUERY_RUN)
    {
        return Err(ambiguous_query_library_error());
    }
    Ok(library)
}

fn query_authorized_library_scope(auth: &AuthContext) -> QueryAuthorizedLibraryScope {
    if auth.is_system_admin {
        return QueryAuthorizedLibraryScope { all_libraries: true, ..Default::default() };
    }

    let mut all_libraries = false;
    let mut workspace_ids = BTreeSet::new();
    let mut library_ids = BTreeSet::new();
    for grant in &auth.grants {
        if !POLICY_QUERY_RUN.contains(&grant.permission_kind.as_str()) {
            continue;
        }
        match grant.resource_kind.as_str() {
            "system" => all_libraries = true,
            "workspace" => {
                if let Some(workspace_id) = grant.workspace_id {
                    workspace_ids.insert(workspace_id);
                }
            }
            "library" => {
                if let Some(library_id) = grant.library_id {
                    library_ids.insert(library_id);
                }
            }
            _ => {}
        }
    }
    if all_libraries {
        return QueryAuthorizedLibraryScope { all_libraries: true, ..Default::default() };
    }
    QueryAuthorizedLibraryScope {
        all_libraries: false,
        workspace_ids: workspace_ids.into_iter().collect(),
        library_ids: library_ids.into_iter().collect(),
    }
}

fn select_sole_query_authorized_library_id(candidate_ids: Vec<Uuid>) -> Result<Uuid, ApiError> {
    let [library_id] = candidate_ids.as_slice() else {
        return Err(ambiguous_query_library_error());
    };
    Ok(*library_id)
}

fn ambiguous_query_library_error() -> ApiError {
    ApiError::invalid_mcp_tool_call(AMBIGUOUS_QUERY_LIBRARY_MESSAGE)
}

pub(crate) async fn visible_workspaces(
    auth: &AuthContext,
    state: &AppState,
) -> Result<Vec<McpWorkspaceDescriptor>, ApiError> {
    // Load every visible workspace row and every visible library row
    // in two concurrent queries instead of one workspace load followed
    // by N per-workspace library loads. The earlier loop issued
    // `load_visible_library_contexts(Some(ws_id))` once per workspace,
    // which turned the MCP capability read into an N+1 — every
    // capability probe and every `initialize` call paid for it.
    let (workspace_rows, libraries) = tokio::try_join!(
        load_visible_workspace_rows(auth, state),
        load_visible_library_contexts(auth, state, None),
    )?;

    // Group library descriptors by workspace once so per-workspace
    // counts and the `can_write_any_library` flag are derived in
    // memory instead of via another query.
    let mut libs_by_workspace: std::collections::HashMap<Uuid, Vec<&McpLibraryDescriptor>> =
        std::collections::HashMap::with_capacity(workspace_rows.len());
    for library in &libraries {
        libs_by_workspace
            .entry(library.descriptor.workspace_id)
            .or_default()
            .push(&library.descriptor);
    }

    let mut items = Vec::with_capacity(workspace_rows.len());
    for workspace in workspace_rows {
        let workspace_libraries = libs_by_workspace.remove(&workspace.id).unwrap_or_default();
        let can_write_any_library = workspace_libraries.iter().any(|item| item.supports_write);
        items.push(McpWorkspaceDescriptor {
            workspace_id: workspace.id,
            catalog_ref: workspace_catalog_ref(&workspace.slug),
            name: workspace.display_name,
            status: workspace.lifecycle_state,
            visible_library_count: workspace_libraries.len(),
            can_write_any_library,
        });
    }
    Ok(items)
}

pub(crate) async fn visible_libraries(
    auth: &AuthContext,
    state: &AppState,
    workspace_filter: Option<&str>,
) -> Result<Vec<McpLibraryDescriptor>, ApiError> {
    let libraries = load_visible_library_contexts(auth, state, workspace_filter).await?;
    Ok(libraries.into_iter().map(|item| item.descriptor).collect())
}

/// Concurrent (workspaces, libraries) load for MCP capability snapshots.
///
/// Used by the hot capability/initialize path to avoid issuing two
/// sequential round-trips and the old workspace-level N+1 library
/// fetch. Both lists are derived from the same underlying queries
/// `load_visible_workspace_rows` and `load_visible_library_contexts`,
/// which are run in parallel via `tokio::try_join!`.
pub(crate) async fn visible_catalog(
    auth: &AuthContext,
    state: &AppState,
) -> Result<(Vec<McpWorkspaceDescriptor>, Vec<McpLibraryDescriptor>), ApiError> {
    let (workspace_rows, libraries) = tokio::try_join!(
        load_visible_workspace_rows(auth, state),
        load_visible_library_contexts(auth, state, None),
    )?;

    // Group library descriptors by workspace so per-workspace counts
    // are derived in memory rather than via additional queries.
    let mut libs_by_workspace: std::collections::HashMap<Uuid, Vec<&McpLibraryDescriptor>> =
        std::collections::HashMap::with_capacity(workspace_rows.len());
    for library in &libraries {
        libs_by_workspace
            .entry(library.descriptor.workspace_id)
            .or_default()
            .push(&library.descriptor);
    }

    let mut workspaces = Vec::with_capacity(workspace_rows.len());
    for workspace in workspace_rows {
        let workspace_libs = libs_by_workspace.remove(&workspace.id).unwrap_or_default();
        let can_write_any_library = workspace_libs.iter().any(|item| item.supports_write);
        workspaces.push(McpWorkspaceDescriptor {
            workspace_id: workspace.id,
            catalog_ref: workspace_catalog_ref(&workspace.slug),
            name: workspace.display_name,
            status: workspace.lifecycle_state,
            visible_library_count: workspace_libs.len(),
            can_write_any_library,
        });
    }

    let library_descriptors: Vec<McpLibraryDescriptor> =
        libraries.into_iter().map(|item| item.descriptor).collect();
    Ok((workspaces, library_descriptors))
}

pub(crate) async fn create_workspace(
    auth: &AuthContext,
    state: &AppState,
    request: McpCreateWorkspaceRequest,
) -> Result<McpWorkspaceDescriptor, ApiError> {
    if !auth.is_system_admin {
        return Err(ApiError::Unauthorized);
    }
    let workspace_ref = parse_workspace_catalog_ref(&request.workspace)?;
    let display_name = request
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&workspace_ref)
        .to_string();

    let workspace = state
        .canonical_services
        .catalog
        .create_workspace(
            state,
            crate::services::catalog_service::CreateWorkspaceCommand {
                slug: Some(workspace_ref.clone()),
                display_name,
                created_by_principal_id: Some(auth.principal_id),
            },
        )
        .await
        .map_err(|error| match error {
            ApiError::Conflict(_) => error,
            _ => {
                map_workspace_create_error(sqlx::Error::Protocol(error.to_string()), &workspace_ref)
            }
        })?;

    Ok(McpWorkspaceDescriptor {
        workspace_id: workspace.id,
        catalog_ref: workspace_catalog_ref(&workspace.slug),
        name: workspace.display_name,
        status: "active".to_string(),
        visible_library_count: 0,
        can_write_any_library: auth.is_system_admin,
    })
}

pub(crate) async fn create_library(
    auth: &AuthContext,
    state: &AppState,
    request: McpCreateLibraryRequest,
) -> Result<McpLibraryDescriptor, ApiError> {
    let (workspace_ref, library_slug) = parse_library_catalog_ref(&request.library)?;
    let workspace =
        load_workspace_by_catalog_ref(auth, state, &workspace_ref, POLICY_WORKSPACE_ADMIN).await?;
    let display_name = request
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&library_slug)
        .to_string();

    let library = state
        .canonical_services
        .catalog
        .create_library(
            state,
            crate::services::catalog_service::CreateLibraryCommand {
                workspace_id: workspace.id,
                slug: Some(library_slug.clone()),
                display_name,
                description: request.description,
                created_by_principal_id: Some(auth.principal_id),
            },
        )
        .await
        .map_err(|error| match error {
            ApiError::Conflict(_) => error,
            _ => map_library_create_error(
                sqlx::Error::Protocol(error.to_string()),
                workspace.id,
                &library_slug,
            ),
        })?;

    let row = catalog_repository::get_library_by_id(&state.persistence.postgres, library.id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("library", library.id))?;
    let recognition_policy = parse_library_recognition_policy(&row)?;
    let readiness = state
        .canonical_services
        .catalog
        .get_library_ingestion_readiness(state, row.id, &recognition_policy)
        .await?;
    let context = describe_library(auth, state, row, &workspace.slug, readiness).await?;
    Ok(context.descriptor)
}

/// Parses a lifecycle-state value the tool caller is *requesting to set*.
/// `"disabled"` is intentionally rejected here even though it is a valid
/// stored value (see [`parse_current_lifecycle_state`]) — the write path
/// (`catalog_service::update_workspace`/`update_library`) rejects it the
/// same way the REST PATCH does, so failing fast here gives a clearer
/// tool-call error than surfacing the service-layer rejection.
fn parse_requested_lifecycle_state(value: &str) -> Result<CatalogLifecycleState, ApiError> {
    match value {
        "active" => Ok(CatalogLifecycleState::Active),
        "archived" => Ok(CatalogLifecycleState::Archived),
        other => Err(ApiError::invalid_mcp_tool_call(format!(
            "lifecycleState must be \"active\" or \"archived\", got \"{other}\""
        ))),
    }
}

/// Parses the *currently stored* lifecycle-state string so an update that
/// omits `lifecycleState` can round-trip it unchanged (load-current ->
/// merge -> write). Unlike [`parse_requested_lifecycle_state`], accepts
/// `"disabled"` — that is the one state a fresh write can never set
/// (`CatalogLifecycleError::DisabledVocabulary`), but a workspace/library
/// already carrying it must still be loadable and re-saveable without an
/// unrelated field change forcing an unintended lifecycle transition.
fn parse_current_lifecycle_state(value: &str) -> Result<CatalogLifecycleState, ApiError> {
    match value {
        "active" => Ok(CatalogLifecycleState::Active),
        "disabled" => Ok(CatalogLifecycleState::Disabled),
        "archived" => Ok(CatalogLifecycleState::Archived),
        other => Err(ApiError::internal_with_log(
            format!("unknown catalog lifecycle state: {other}"),
            "internal",
        )),
    }
}

/// Updates a workspace's display name and/or lifecycle state.
/// Load-current -> merge -> write: any field the caller omits keeps its
/// current value, so renaming a workspace never silently resets its
/// lifecycle (see [`McpUpdateWorkspaceRequest`]).
pub(crate) async fn update_workspace(
    auth: &AuthContext,
    state: &AppState,
    request: McpUpdateWorkspaceRequest,
) -> Result<McpWorkspaceDescriptor, ApiError> {
    if !auth.is_system_admin {
        return Err(ApiError::Unauthorized);
    }
    let current = load_workspace_row_by_catalog_ref(state, &request.workspace).await?;
    let display_name = request
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map_or_else(|| current.display_name.clone(), ToString::to_string);
    let lifecycle_state = match request.lifecycle_state.as_deref() {
        Some(value) => parse_requested_lifecycle_state(value)?,
        None => parse_current_lifecycle_state(&current.lifecycle_state)?,
    };

    state
        .canonical_services
        .catalog
        .update_workspace(
            state,
            crate::services::catalog_service::UpdateWorkspaceCommand {
                workspace_id: current.id,
                slug: Some(current.slug.clone()),
                display_name,
                lifecycle_state,
            },
        )
        .await?;

    let workspaces = visible_workspaces(auth, state).await?;
    workspaces
        .into_iter()
        .find(|workspace| workspace.workspace_id == current.id)
        .ok_or_else(|| ApiError::resource_not_found("workspace", current.id))
}

/// Updates a library's display name, description, and/or lifecycle state.
/// Load-current -> merge -> write, same as [`update_workspace`]. Knobs not
/// exposed on [`McpUpdateLibraryRequest`] (extraction prompt,
/// `includeDocumentHintInMcpAnswers`) are always carried forward unchanged.
pub(crate) async fn update_library(
    auth: &AuthContext,
    state: &AppState,
    request: McpUpdateLibraryRequest,
) -> Result<McpLibraryDescriptor, ApiError> {
    let current =
        load_library_by_catalog_ref(auth, state, &request.library, POLICY_WORKSPACE_ADMIN).await?;
    let workspace =
        catalog_repository::get_workspace_by_id(&state.persistence.postgres, current.workspace_id)
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("workspace", current.workspace_id))?;

    let display_name = request
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map_or_else(|| current.display_name.clone(), ToString::to_string);
    let description = request.description.clone().or_else(|| current.description.clone());
    let lifecycle_state = match request.lifecycle_state.as_deref() {
        Some(value) => parse_requested_lifecycle_state(value)?,
        None => parse_current_lifecycle_state(&current.lifecycle_state)?,
    };

    state
        .canonical_services
        .catalog
        .update_library(
            state,
            crate::services::catalog_service::UpdateLibraryCommand {
                library_id: current.id,
                slug: Some(current.slug.clone()),
                display_name,
                description,
                extraction_prompt: current.extraction_prompt.clone(),
                lifecycle_state,
                include_document_hint_in_mcp_answers: current.include_document_hint_in_mcp_answers,
            },
        )
        .await?;

    let row = catalog_repository::get_library_by_id(&state.persistence.postgres, current.id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("library", current.id))?;
    let recognition_policy = parse_library_recognition_policy(&row)?;
    let readiness = state
        .canonical_services
        .catalog
        .get_library_ingestion_readiness(state, row.id, &recognition_policy)
        .await?;
    let context = describe_library(auth, state, row, &workspace.slug, readiness).await?;
    Ok(context.descriptor)
}

pub(crate) async fn load_visible_workspace_rows(
    auth: &AuthContext,
    state: &AppState,
) -> Result<Vec<CatalogWorkspaceRow>, ApiError> {
    let rows = catalog_repository::list_workspaces(&state.persistence.postgres)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;

    Ok(rows.into_iter().filter(|row| authorize_workspace_discovery(auth, row.id).is_ok()).collect())
}

pub(crate) async fn load_visible_library_contexts(
    auth: &AuthContext,
    state: &AppState,
    workspace_filter: Option<&str>,
) -> Result<Vec<VisibleLibraryContext>, ApiError> {
    let workspace_ids = if let Some(workspace_id) = workspace_filter {
        vec![load_workspace_by_catalog_ref_for_discovery(auth, state, workspace_id).await?.id]
    } else {
        load_visible_workspace_rows(auth, state)
            .await?
            .into_iter()
            .map(|workspace| workspace.id)
            .collect::<Vec<_>>()
    };

    let mut libraries = Vec::new();
    for workspace_id in workspace_ids {
        let rows =
            catalog_repository::list_libraries(&state.persistence.postgres, Some(workspace_id))
                .await
                .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        for library in rows {
            if authorize_library_discovery(auth, workspace_id, library.id).is_ok() {
                libraries.push(library);
            }
        }
    }
    describe_libraries(auth, state, libraries).await
}

pub(crate) async fn describe_libraries(
    auth: &AuthContext,
    state: &AppState,
    libraries: Vec<CatalogLibraryRow>,
) -> Result<Vec<VisibleLibraryContext>, ApiError> {
    let workspace_slug_by_id = catalog_repository::list_workspaces(&state.persistence.postgres)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .into_iter()
        .map(|workspace| (workspace.id, workspace.slug))
        .collect::<std::collections::HashMap<_, _>>();
    let readiness_by_library = state
        .canonical_services
        .catalog
        .list_library_ingestion_readiness(state, &parse_library_recognition_policies(&libraries)?)
        .await?;

    let mut items = Vec::with_capacity(libraries.len());
    for library in libraries {
        let readiness = readiness_by_library.get(&library.id).cloned().unwrap_or_else(|| {
            CatalogLibraryIngestionReadiness {
                ready: false,
                missing_binding_purposes: vec![
                    AiBindingPurpose::ExtractGraph,
                    AiBindingPurpose::EmbedChunk,
                ],
            }
        });
        let workspace_slug = workspace_slug_by_id
            .get(&library.workspace_id)
            .cloned()
            .ok_or_else(|| ApiError::resource_not_found("workspace", library.workspace_id))?;
        items.push(describe_library(auth, state, library, &workspace_slug, readiness).await?);
    }
    Ok(items)
}

fn parse_library_recognition_policy(
    row: &CatalogLibraryRow,
) -> Result<LibraryRecognitionPolicy, ApiError> {
    LibraryRecognitionPolicy::from_json(row.recognition_policy.clone()).map_err(|error| {
        ApiError::internal_with_log(anyhow::anyhow!(error), "invalid persisted recognition policy")
    })
}

fn parse_library_recognition_policies(
    rows: &[CatalogLibraryRow],
) -> Result<Vec<(Uuid, LibraryRecognitionPolicy)>, ApiError> {
    rows.iter()
        .map(|row| parse_library_recognition_policy(row).map(|policy| (row.id, policy)))
        .collect()
}

pub(crate) async fn describe_library(
    auth: &AuthContext,
    state: &AppState,
    library: CatalogLibraryRow,
    workspace_slug: &str,
    ingestion_readiness: CatalogLibraryIngestionReadiness,
) -> Result<VisibleLibraryContext, ApiError> {
    let supports_search =
        auth.has_library_permission(library.workspace_id, library.id, POLICY_MCP_MEMORY_READ);
    let supports_write =
        auth.has_library_permission(library.workspace_id, library.id, POLICY_LIBRARY_WRITE);
    let coverage = state
        .canonical_services
        .knowledge
        .get_library_knowledge_coverage(state, library.id)
        .await?;
    let document_count =
        usize::try_from(coverage.document_counts_by_readiness.values().copied().sum::<i64>())
            .unwrap_or(usize::MAX);
    let readable_document_count = readiness_count(&coverage, "readable")
        .saturating_add(usize::try_from(coverage.graph_sparse_document_count).unwrap_or(usize::MAX))
        .saturating_add(usize::try_from(coverage.graph_ready_document_count).unwrap_or(usize::MAX));
    let processing_document_count = readiness_count(&coverage, "processing");
    let descriptor = McpLibraryDescriptor {
        library_id: library.id,
        workspace_id: library.workspace_id,
        catalog_ref: library_catalog_ref(workspace_slug, &library.slug),
        name: library.display_name.trim().to_string(),
        description: library.description.clone(),
        web_ingest_policy: serde_json::from_value(library.web_ingest_policy.clone())
            .map_err(|_| ApiError::Internal)?,
        recognition_policy: parse_library_recognition_policy(&library)?,
        ingestion_readiness: map_ingestion_readiness(ingestion_readiness),
        document_count,
        readable_document_count,
        processing_document_count,
        failed_document_count: readiness_count(&coverage, "failed"),
        document_counts_by_readiness: coverage
            .document_counts_by_readiness
            .iter()
            .map(|(kind, count)| (kind.clone(), usize::try_from(*count).unwrap_or(usize::MAX)))
            .collect(),
        graph_ready_document_count: usize::try_from(coverage.graph_ready_document_count)
            .unwrap_or(usize::MAX),
        graph_sparse_document_count: usize::try_from(coverage.graph_sparse_document_count)
            .unwrap_or(usize::MAX),
        typed_fact_document_count: usize::try_from(coverage.typed_fact_document_count)
            .unwrap_or(usize::MAX),
        supports_search,
        supports_read: auth.has_document_or_library_read_scope_for_library(
            library.workspace_id,
            library.id,
            POLICY_MCP_MEMORY_READ,
        ),
        supports_write,
    };
    Ok(VisibleLibraryContext { library, descriptor })
}

fn map_ingestion_readiness(
    readiness: CatalogLibraryIngestionReadiness,
) -> McpLibraryIngestionReadiness {
    McpLibraryIngestionReadiness {
        ready: readiness.ready,
        missing_binding_purposes: readiness.missing_binding_purposes,
    }
}

fn readiness_count(
    coverage: &crate::domains::content::LibraryKnowledgeCoverage,
    readiness_kind: &str,
) -> usize {
    coverage
        .document_counts_by_readiness
        .get(readiness_kind)
        .copied()
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::{
        domains::iam::PrincipalKind,
        interfaces::http::{
            auth::{AuthGrant, AuthTokenKind},
            authorization::{
                PERMISSION_IAM_ADMIN, PERMISSION_LIBRARY_READ, PERMISSION_LIBRARY_WRITE,
                PERMISSION_QUERY_RUN,
            },
        },
    };

    fn grant(
        id: u128,
        resource_kind: &str,
        workspace_id: Option<Uuid>,
        library_id: Option<Uuid>,
        permission: &str,
    ) -> AuthGrant {
        AuthGrant {
            id: Uuid::from_u128(id),
            resource_kind: resource_kind.to_string(),
            resource_id: library_id.or(workspace_id).unwrap_or_else(Uuid::nil),
            permission_kind: permission.to_string(),
            workspace_id,
            library_id,
            document_id: None,
        }
    }

    fn auth_with_grants(grants: Vec<AuthGrant>) -> AuthContext {
        AuthContext {
            token_id: Uuid::from_u128(1),
            principal_id: Uuid::from_u128(2),
            parent_principal_id: None,
            workspace_id: None,
            token_kind: AuthTokenKind::Principal(PrincipalKind::ApiToken),
            scopes: Vec::new(),
            grants,
            workspace_memberships: Vec::new(),
            visible_workspace_ids: BTreeSet::new(),
            is_system_admin: false,
            system_role: None,
        }
    }

    #[test]
    fn query_authorized_scope_matches_has_library_permission_resource_semantics() {
        let first_workspace = Uuid::from_u128(10);
        let second_workspace = Uuid::from_u128(20);
        let library_id = Uuid::from_u128(11);
        let ignored_library_id = Uuid::from_u128(12);
        let auth = auth_with_grants(vec![
            grant(100, "library", Some(first_workspace), Some(library_id), PERMISSION_QUERY_RUN),
            grant(101, "workspace", Some(second_workspace), None, PERMISSION_LIBRARY_WRITE),
            grant(
                102,
                "library",
                Some(first_workspace),
                Some(ignored_library_id),
                PERMISSION_LIBRARY_READ,
            ),
        ]);

        let scope = query_authorized_library_scope(&auth);

        assert!(!scope.all_libraries);
        assert_eq!(scope.workspace_ids, vec![second_workspace]);
        assert_eq!(scope.library_ids, vec![library_id]);
    }

    #[test]
    fn system_authority_collapses_candidate_scope_to_all_active_libraries() {
        let workspace_id = Uuid::from_u128(20);
        let library_id = Uuid::from_u128(21);
        let mut system_admin = auth_with_grants(vec![grant(
            110,
            "library",
            Some(workspace_id),
            Some(library_id),
            PERMISSION_QUERY_RUN,
        )]);
        system_admin.is_system_admin = true;
        assert_eq!(
            query_authorized_library_scope(&system_admin),
            QueryAuthorizedLibraryScope {
                all_libraries: true,
                workspace_ids: Vec::new(),
                library_ids: Vec::new(),
            }
        );

        let system_grant =
            auth_with_grants(vec![grant(111, "system", None, None, PERMISSION_IAM_ADMIN)]);
        assert!(query_authorized_library_scope(&system_grant).all_libraries);
    }

    #[test]
    fn query_authorized_scope_deduplicates_ids_and_ignores_malformed_grants() {
        let workspace_id = Uuid::from_u128(30);
        let library_id = Uuid::from_u128(31);
        let auth = auth_with_grants(vec![
            grant(120, "workspace", Some(workspace_id), None, PERMISSION_QUERY_RUN),
            grant(121, "workspace", Some(workspace_id), None, PERMISSION_QUERY_RUN),
            grant(122, "library", None, Some(library_id), PERMISSION_QUERY_RUN),
            grant(123, "library", None, None, PERMISSION_QUERY_RUN),
            grant(124, "workspace", None, None, PERMISSION_QUERY_RUN),
        ]);

        let scope = query_authorized_library_scope(&auth);

        assert_eq!(scope.workspace_ids, vec![workspace_id]);
        assert_eq!(scope.library_ids, vec![library_id]);
    }

    #[test]
    fn zero_and_multiple_query_authorized_candidates_fail_with_same_non_leaking_error() {
        let first_id = Uuid::from_u128(41);
        let second_id = Uuid::from_u128(42);

        let zero_error = select_sole_query_authorized_library_id(Vec::new())
            .expect_err("zero query-authorized libraries must fail")
            .to_string();
        let multiple_error = select_sole_query_authorized_library_id(vec![first_id, second_id])
            .expect_err("multiple query-authorized libraries must fail")
            .to_string();

        assert_eq!(zero_error, multiple_error);
        assert!(!zero_error.contains(&first_id.to_string()));
        assert!(!zero_error.contains(&second_id.to_string()));
    }

    #[test]
    fn one_query_authorized_candidate_is_selected() {
        let library_id = Uuid::from_u128(51);

        assert_eq!(
            select_sole_query_authorized_library_id(vec![library_id])
                .expect("select sole query-authorized library"),
            library_id
        );
    }
}
