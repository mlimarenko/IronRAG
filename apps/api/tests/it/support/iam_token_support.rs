use std::collections::BTreeSet;

use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use ironrag_backend::{
    infra::repositories::iam_repository, interfaces::http::auth::hash_token,
    shared::auth_tokens::mint_plaintext_api_token,
};

pub(crate) struct MintedApiToken {
    pub(crate) plaintext: String,
}

pub(crate) fn scoped_workspace_auth(
    principal_id: Uuid,
    workspace_id: Uuid,
    permissions: &[&str],
) -> ironrag_backend::interfaces::http::auth::AuthContext {
    use ironrag_backend::{
        domains::iam::PrincipalKind,
        interfaces::http::auth::{AuthContext, AuthGrant, AuthTokenKind},
    };
    AuthContext {
        token_id: Uuid::now_v7(),
        principal_id,
        parent_principal_id: None,
        workspace_id: Some(workspace_id),
        token_kind: AuthTokenKind::Principal(PrincipalKind::ApiToken),
        scopes: permissions.iter().map(|value| (*value).to_owned()).collect(),
        grants: permissions
            .iter()
            .map(|permission| AuthGrant {
                id: Uuid::now_v7(),
                resource_kind: "workspace".to_owned(),
                resource_id: workspace_id,
                permission_kind: (*permission).to_owned(),
                workspace_id: Some(workspace_id),
                library_id: None,
                document_id: None,
            })
            .collect(),
        workspace_memberships: vec![],
        visible_workspace_ids: [workspace_id].into_iter().collect(),
        is_system_admin: false,
        system_role: None,
    }
}

#[derive(Clone)]
pub(crate) struct GrantSpec {
    pub(crate) resource_kind: &'static str,
    pub(crate) resource_id: Uuid,
    pub(crate) permission_kind: String,
}

pub(crate) async fn create_principal_grants(
    postgres: &PgPool,
    principal_id: Uuid,
    label: &str,
    grants: &[GrantSpec],
) -> anyhow::Result<()> {
    for grant in grants {
        iam_repository::create_grant(
            postgres,
            &iam_repository::NewGrant {
                principal_id,
                resource_kind: grant.resource_kind,
                resource_id: grant.resource_id,
                permission_kind: &grant.permission_kind,
                granted_by_principal_id: None,
                expires_at: None,
            },
        )
        .await
        .with_context(|| {
            format!(
                "failed to create grant {}:{} for {label}",
                grant.resource_kind, grant.permission_kind
            )
        })?;
    }
    Ok(())
}

pub(crate) async fn mint_api_token(
    postgres: &PgPool,
    workspace_id: Option<Uuid>,
    token_kind: &str,
    label: &str,
    scopes: &[&str],
) -> anyhow::Result<MintedApiToken> {
    let plaintext = mint_plaintext_api_token();
    let token =
        iam_repository::create_api_token(postgres, workspace_id, label, "test-token", None, None)
            .await
            .with_context(|| format!("failed to create iam api token for {label}"))?;
    iam_repository::create_api_token_secret(postgres, token.principal_id, &hash_token(&plaintext))
        .await
        .with_context(|| format!("failed to create iam api token secret for {label}"))?;

    match workspace_id {
        Some(workspace_id) => {
            iam_repository::upsert_workspace_membership(
                postgres,
                workspace_id,
                token.principal_id,
                "active",
            )
            .await
            .with_context(|| format!("failed to create workspace membership for {label}"))?;

            let permission_kinds = scopes
                .iter()
                .map(|scope| normalize_permission_kind(scope, token_kind))
                .collect::<BTreeSet<_>>();
            for permission_kind in permission_kinds {
                iam_repository::create_grant(
                    postgres,
                    &iam_repository::NewGrant {
                        principal_id: token.principal_id,
                        resource_kind: "workspace",
                        resource_id: workspace_id,
                        permission_kind,
                        granted_by_principal_id: None,
                        expires_at: None,
                    },
                )
                .await
                .with_context(|| {
                    format!("failed to create workspace grant {permission_kind} for {label}")
                })?;
            }
        }
        None => {
            iam_repository::create_grant(
                postgres,
                &iam_repository::NewGrant {
                    principal_id: token.principal_id,
                    resource_kind: "system",
                    resource_id: Uuid::nil(),
                    permission_kind: "iam_admin",
                    granted_by_principal_id: None,
                    expires_at: None,
                },
            )
            .await
            .with_context(|| format!("failed to create system admin grant for {label}"))?;
        }
    }

    Ok(MintedApiToken { plaintext })
}

fn normalize_permission_kind(scope: &str, token_kind: &str) -> &'static str {
    match scope {
        "workspace:admin" | "workspace_admin" => "workspace_admin",
        "workspace:read" | "workspace_read" => "workspace_read",
        "projects:write" | "library_write" => "library_write",
        "projects:read" | "library_read" => "library_read",
        "documents:read" | "document_read" => "document_read",
        "documents:write" | "document_write" => "document_write",
        "query:run" | "query_run" => "query_run",
        "ops:read" | "ops_read" => "ops_read",
        "audit:read" | "audit_read" => "audit_read",
        "iam:admin" | "iam_admin" => "iam_admin",
        _ if token_kind == "instance_admin" => "iam_admin",
        _ => "document_read",
    }
}
