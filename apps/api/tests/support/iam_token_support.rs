#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::collections::BTreeSet;

use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use ironrag_backend::{infra::repositories::iam_repository, interfaces::http::auth::hash_token};

pub struct MintedApiToken {
    pub principal_id: Uuid,
    pub plaintext: String,
}

pub async fn mint_api_token(
    postgres: &PgPool,
    workspace_id: Option<Uuid>,
    token_kind: &str,
    label: &str,
    scopes: &[&str],
) -> anyhow::Result<MintedApiToken> {
    let plaintext = format!("test-{}-{}", label, Uuid::now_v7());
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
                    token.principal_id,
                    "workspace",
                    workspace_id,
                    permission_kind,
                    None,
                    None,
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
                token.principal_id,
                "system",
                Uuid::nil(),
                "iam_admin",
                None,
                None,
            )
            .await
            .with_context(|| format!("failed to create system admin grant for {label}"))?;
        }
    }

    Ok(MintedApiToken { principal_id: token.principal_id, plaintext })
}

pub async fn find_active_api_token(
    postgres: &PgPool,
    plaintext: &str,
) -> anyhow::Result<iam_repository::AuthenticatedApiTokenRow> {
    iam_repository::find_active_api_token_by_secret_hash(postgres, &hash_token(plaintext))
        .await
        .context("failed to resolve api token by secret hash")?
        .context("api token missing")
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
