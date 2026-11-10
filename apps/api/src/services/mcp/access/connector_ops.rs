//! Authorization and mapping for the connector catalog MCP tool.
//!
//! The bundled connector catalog is static code, not database state, and has
//! no REST CRUD surface. The only thing to authorize is the read, and it uses
//! the same `POLICY_CONNECTOR_READ` the ingest diagnostics already apply, so a
//! connector operator sees the catalog exactly when it may read ingest state.

use crate::{
    domains::connector::{CONNECTOR_SOURCES, ConnectorSourceSpec},
    interfaces::http::{
        auth::AuthContext, authorization::POLICY_CONNECTOR_READ, router_support::ApiError,
    },
    mcp_types::{McpConnectorSource, McpConnectorSourceList},
};

pub(crate) fn list_connector_sources(
    auth: &AuthContext,
) -> Result<McpConnectorSourceList, ApiError> {
    if !auth.can_read_any_library_memory(POLICY_CONNECTOR_READ) {
        return Err(ApiError::Unauthorized);
    }
    Ok(McpConnectorSourceList {
        items: CONNECTOR_SOURCES.iter().map(map_connector_source).collect(),
    })
}

fn map_connector_source(source: &ConnectorSourceSpec) -> McpConnectorSource {
    McpConnectorSource {
        kind: source.kind.to_string(),
        display_name: source.display_name.to_string(),
        run_modes: source.run_modes.iter().map(|run_mode| (*run_mode).to_string()).collect(),
        image: source.image.to_string(),
        deploy_directory: source.deploy_directory.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{list_connector_sources, map_connector_source};
    use crate::{
        domains::connector::CONNECTOR_SOURCES,
        interfaces::http::{
            auth::{AuthContext, AuthGrant, AuthTokenKind},
            authorization::{
                PERMISSION_CONNECTOR_ADMIN, PERMISSION_LIBRARY_READ, PERMISSION_LIBRARY_WRITE,
                PERMISSION_QUERY_RUN,
            },
            router_support::ApiError,
        },
    };
    use uuid::Uuid;

    fn auth(grants: Vec<AuthGrant>, is_system_admin: bool) -> AuthContext {
        AuthContext {
            token_id: Uuid::from_u128(1),
            principal_id: Uuid::from_u128(2),
            parent_principal_id: None,
            workspace_id: None,
            token_kind: AuthTokenKind::Principal(crate::domains::iam::PrincipalKind::ApiToken),
            scopes: Vec::new(),
            grants,
            workspace_memberships: Vec::new(),
            visible_workspace_ids: BTreeSet::new(),
            is_system_admin,
            system_role: None,
        }
    }

    fn library_grant(id: u128, permission_kind: &str) -> AuthGrant {
        AuthGrant {
            id: Uuid::from_u128(id),
            resource_kind: "library".to_string(),
            resource_id: Uuid::from_u128(11),
            permission_kind: permission_kind.to_string(),
            workspace_id: Some(Uuid::from_u128(101)),
            library_id: Some(Uuid::from_u128(11)),
            document_id: None,
        }
    }

    #[test]
    fn connector_read_grant_is_required() {
        let without_grants = auth(Vec::new(), false);
        assert!(matches!(list_connector_sources(&without_grants), Err(ApiError::Unauthorized)));

        // A grant on some other permission does not open the catalog.
        let unrelated_grant = auth(vec![library_grant(1, PERMISSION_QUERY_RUN)], false);
        assert!(matches!(list_connector_sources(&unrelated_grant), Err(ApiError::Unauthorized)));

        for permission_kind in [PERMISSION_LIBRARY_READ, PERMISSION_LIBRARY_WRITE] {
            let permitted = auth(vec![library_grant(2, permission_kind)], false);
            assert!(
                list_connector_sources(&permitted).is_ok(),
                "`{permission_kind}` is part of POLICY_CONNECTOR_READ and must open the catalog"
            );
        }

        let connector_admin = auth(vec![library_grant(3, PERMISSION_CONNECTOR_ADMIN)], false);
        assert!(list_connector_sources(&connector_admin).is_ok());

        let system_admin = auth(Vec::new(), true);
        assert!(list_connector_sources(&system_admin).is_ok());
    }

    #[test]
    fn catalog_is_mapped_in_order_with_every_field() {
        let payload = list_connector_sources(&auth(Vec::new(), true)).expect("system admin read");
        assert_eq!(payload.items.len(), CONNECTOR_SOURCES.len());
        assert_eq!(
            payload.items.iter().map(|item| item.kind.as_str()).collect::<Vec<_>>(),
            CONNECTOR_SOURCES.iter().map(|source| source.kind).collect::<Vec<_>>(),
            "the tool must preserve catalog order"
        );

        for (item, source) in payload.items.iter().zip(CONNECTOR_SOURCES) {
            assert_eq!(item.display_name, source.display_name);
            assert_eq!(item.image, source.image);
            assert_eq!(item.deploy_directory, source.deploy_directory);
            assert_eq!(
                item.run_modes.iter().map(String::as_str).collect::<Vec<_>>(),
                source.run_modes.to_vec()
            );
        }
    }

    #[test]
    fn connector_source_maps_to_camel_case_wire_shape() {
        let source = CONNECTOR_SOURCES.first().expect("the catalog is never empty");
        assert_eq!(
            serde_json::to_value(map_connector_source(source)).expect("serializable source"),
            serde_json::json!({
                "kind": source.kind,
                "displayName": source.display_name,
                "runModes": source.run_modes,
                "image": source.image,
                "deployDirectory": source.deploy_directory,
            })
        );
    }
}
