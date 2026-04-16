use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{ai::AiBindingPurpose, catalog::CatalogLibraryIngestionReadiness},
    infra::repositories::catalog_repository::{self, CatalogLibraryRow, CatalogWorkspaceRow},
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_LIBRARY_WRITE, POLICY_MCP_MEMORY_READ, POLICY_WORKSPACE_ADMIN,
            authorize_library_discovery, authorize_workspace_discovery,
            authorize_workspace_permission,
        },
        router_support::{ApiError, map_library_create_error, map_workspace_create_error},
    },
    mcp_types::{
        McpCreateLibraryRequest, McpCreateWorkspaceRequest, McpLibraryDescriptor,
        McpLibraryIngestionReadiness, McpWorkspaceDescriptor,
    },
    shared::slugs::slugify,
};

use super::types::VisibleLibraryContext;

fn resolve_mcp_slug(requested_slug: Option<&str>, name: &str) -> String {
    requested_slug
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(slugify)
        .unwrap_or_else(|| slugify(name))
}

pub async fn visible_workspaces(
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
            slug: workspace.slug,
            name: workspace.display_name,
            status: workspace.lifecycle_state,
            visible_library_count: workspace_libraries.len(),
            can_write_any_library,
        });
    }
    Ok(items)
}

pub async fn visible_libraries(
    auth: &AuthContext,
    state: &AppState,
    workspace_filter: Option<Uuid>,
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
pub async fn visible_catalog(
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
            slug: workspace.slug,
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

pub async fn create_workspace(
    auth: &AuthContext,
    state: &AppState,
    request: McpCreateWorkspaceRequest,
) -> Result<McpWorkspaceDescriptor, ApiError> {
    if !auth.is_system_admin {
        return Err(ApiError::Unauthorized);
    }
    let name = request.name.trim();
    if name.is_empty() {
        return Err(ApiError::BadRequest("workspace name must not be empty".into()));
    }
    let slug = resolve_mcp_slug(request.slug.as_deref(), name);

    let workspace = state
        .canonical_services
        .catalog
        .create_workspace(
            state,
            crate::services::catalog_service::CreateWorkspaceCommand {
                slug: Some(slug.clone()),
                display_name: name.to_string(),
                created_by_principal_id: Some(auth.principal_id),
            },
        )
        .await
        .map_err(|error| match error {
            ApiError::Conflict(_) => error,
            _ => map_workspace_create_error(sqlx::Error::Protocol(error.to_string()), &slug),
        })?;

    Ok(McpWorkspaceDescriptor {
        workspace_id: workspace.id,
        slug: workspace.slug,
        name: workspace.display_name,
        status: "active".to_string(),
        visible_library_count: 0,
        can_write_any_library: auth.is_system_admin,
    })
}

pub async fn create_library(
    auth: &AuthContext,
    state: &AppState,
    request: McpCreateLibraryRequest,
) -> Result<McpLibraryDescriptor, ApiError> {
    authorize_workspace_permission(auth, request.workspace_id, POLICY_WORKSPACE_ADMIN)?;

    let name = request.name.trim();
    if name.is_empty() {
        return Err(ApiError::BadRequest("library name must not be empty".into()));
    }
    let slug = resolve_mcp_slug(request.slug.as_deref(), name);

    let library = state
        .canonical_services
        .catalog
        .create_library(
            state,
            crate::services::catalog_service::CreateLibraryCommand {
                workspace_id: request.workspace_id,
                slug: Some(slug.clone()),
                display_name: name.to_string(),
                description: request.description,
                created_by_principal_id: Some(auth.principal_id),
            },
        )
        .await
        .map_err(|error| match error {
            ApiError::Conflict(_) => error,
            _ => map_library_create_error(
                sqlx::Error::Protocol(error.to_string()),
                request.workspace_id,
                &slug,
            ),
        })?;

    let row = catalog_repository::get_library_by_id(&state.persistence.postgres, library.id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("library", library.id))?;
    let readiness =
        state.canonical_services.catalog.get_library_ingestion_readiness(state, row.id).await?;
    let context = describe_library(auth, state, row, readiness).await?;
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
    workspace_filter: Option<Uuid>,
) -> Result<Vec<VisibleLibraryContext>, ApiError> {
    let workspace_ids = if let Some(workspace_id) = workspace_filter {
        authorize_workspace_discovery(auth, workspace_id)?;
        vec![workspace_id]
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
    let readiness_by_library = state
        .canonical_services
        .catalog
        .list_library_ingestion_readiness(
            state,
            &libraries.iter().map(|library| library.id).collect::<Vec<_>>(),
        )
        .await?;

    let mut items = Vec::with_capacity(libraries.len());
    for library in libraries {
        let readiness = readiness_by_library.get(&library.id).cloned().unwrap_or(
            CatalogLibraryIngestionReadiness {
                ready: false,
                missing_binding_purposes: vec![AiBindingPurpose::ExtractGraph],
            },
        );
        items.push(describe_library(auth, state, library, readiness).await?);
    }
    Ok(items)
}

pub(crate) async fn describe_library(
    auth: &AuthContext,
    state: &AppState,
    library: CatalogLibraryRow,
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
        slug: library.slug.clone(),
        name: library.display_name.trim().to_string(),
        description: library.description.clone(),
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
