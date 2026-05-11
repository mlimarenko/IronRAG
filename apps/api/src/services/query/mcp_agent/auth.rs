use std::collections::BTreeSet;

use uuid::Uuid;

use crate::{
    domains::iam::PrincipalKind,
    interfaces::http::{
        auth::{AuthContext, AuthGrant, AuthTokenKind},
        authorization::{
            POLICY_DOCUMENTS_WRITE, POLICY_LIBRARY_READ, POLICY_LIBRARY_WRITE,
            POLICY_MCP_MEMORY_READ, POLICY_QUERY_RUN, POLICY_RUNTIME_READ,
        },
    },
};

/// Derives a synthetic library-scoped [`AuthContext`] that mirrors what an
/// external API token with full rights on `library_id` would carry.
///
/// The returned context has no session membership, no system-admin flag, and
/// grants only the permissions that belong to the target library — preventing
/// cross-library access and blocking admin-only tools.
pub fn derive_library_scoped_auth(
    principal_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
) -> AuthContext {
    let grants = build_library_grants(workspace_id, library_id);

    let mut visible_workspace_ids = BTreeSet::new();
    visible_workspace_ids.insert(workspace_id);

    AuthContext {
        token_id: Uuid::new_v4(),
        principal_id,
        parent_principal_id: None,
        workspace_id: Some(workspace_id),
        token_kind: AuthTokenKind::Principal(PrincipalKind::ApiToken),
        scopes: Vec::new(),
        grants,
        workspace_memberships: Vec::new(),
        visible_workspace_ids,
        is_system_admin: false,
    }
}

fn build_library_grants(workspace_id: Uuid, library_id: Uuid) -> Vec<AuthGrant> {
    // One grant per policy that a full-rights library token carries.
    // POLICY_*[0] is the minimal permission that satisfies that policy check.
    let permissions = [
        POLICY_QUERY_RUN[0],
        POLICY_MCP_MEMORY_READ[0],
        POLICY_LIBRARY_READ[0],
        POLICY_LIBRARY_WRITE[0],
        POLICY_DOCUMENTS_WRITE[0],
        POLICY_RUNTIME_READ[0],
    ];

    permissions
        .iter()
        .enumerate()
        .map(|(i, &permission_kind)| AuthGrant {
            // Deterministic ids: low 32 bits encode the index so they are stable
            // across calls for the same library.
            id: Uuid::from_u128(
                (library_id.as_u128() & 0xffff_ffff_ffff_ffff_ffff_ffff_0000_0000u128)
                    | (i as u128 + 1),
            ),
            resource_kind: "library".to_string(),
            resource_id: library_id,
            permission_kind: permission_kind.to_string(),
            workspace_id: Some(workspace_id),
            library_id: Some(library_id),
            document_id: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use crate::interfaces::http::{
        authorization::POLICY_QUERY_RUN,
        mcp::{McpToolSurface, tools::visible_tool_names},
    };

    use super::derive_library_scoped_auth;

    fn library_a_auth() -> (
        crate::interfaces::http::auth::AuthContext,
        Uuid, // workspace_a
        Uuid, // library_a
    ) {
        let principal = Uuid::from_u128(0xaaaa);
        let workspace_a = Uuid::from_u128(0x1111);
        let library_a = Uuid::from_u128(0x2222);
        (derive_library_scoped_auth(principal, workspace_a, library_a), workspace_a, library_a)
    }

    #[test]
    fn derived_auth_exposes_full_library_diagnostics_toolset() {
        let (auth, _, _) = library_a_auth();
        let tools = visible_tool_names(&auth, McpToolSurface::Diagnostics);

        let expected = [
            "grounded_answer",
            "search_documents",
            "read_document",
            "list_documents",
            "search_entities",
            "get_graph_topology",
            "list_relations",
            "get_communities",
            "submit_web_ingest_run",
            "get_web_ingest_run",
            "upload_documents",
            "update_document",
            "delete_document",
            "get_mutation_status",
            "get_runtime_execution",
            "get_runtime_execution_trace",
            "list_workspaces",
            "list_libraries",
        ];

        for tool in expected {
            assert!(
                tools.iter().any(|t| t == tool),
                "expected tool '{tool}' missing from diagnostics surface; got: {tools:?}"
            );
        }
    }

    #[test]
    fn derived_auth_excludes_admin_create_tools() {
        let (auth, _, _) = library_a_auth();
        let tools = visible_tool_names(&auth, McpToolSurface::Diagnostics);

        assert!(
            !tools.iter().any(|t| t == "create_workspace"),
            "create_workspace must not appear for non-admin derived auth; got: {tools:?}"
        );
    }

    #[test]
    fn derived_auth_does_not_grant_cross_library_access() {
        let (auth, _, _) = library_a_auth();

        let workspace_b = Uuid::from_u128(0x9999);
        let library_b = Uuid::from_u128(0x8888);

        assert!(
            !auth.has_library_permission(workspace_b, library_b, POLICY_QUERY_RUN),
            "derived auth for library A must not grant access to unrelated library B"
        );
    }
}
