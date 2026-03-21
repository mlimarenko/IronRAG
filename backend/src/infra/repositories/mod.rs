pub mod ai_repository;
pub mod audit_repository;
pub mod billing_repository;
pub mod catalog_repository;
pub mod content_repository;
pub mod extract_repository;
pub mod graph_repository;
pub mod iam_repository;
pub mod ingest_repository;
pub mod ops_repository;
pub mod query_repository;
mod runtime_graph_repository;
mod runtime_graph_summary_repository;

pub use runtime_graph_repository::*;
pub use runtime_graph_summary_repository::*;

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::domains::{
    billing::{
        PricingBillingUnit, PricingCapability, RuntimeStageBillingPolicy,
        decorate_payload_with_stage_ownership, runtime_stage_billing_policy,
        stage_native_ownership,
    },
    runtime_ingestion::RuntimeQueueWaitingReason,
};

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct WorkspaceRow {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ProjectRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct UiUserRow {
    pub id: Uuid,
    pub login: String,
    pub email: String,
    pub display_name: String,
    pub role_label: String,
    pub password_hash: String,
    pub preferred_locale: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct UiSessionRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub active_workspace_id: Option<Uuid>,
    pub active_project_id: Option<Uuid>,
    pub locale: String,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct WorkspaceMemberRow {
    pub workspace_id: Uuid,
    pub user_id: Uuid,
    pub role_label: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ProjectAccessGrantRow {
    pub project_id: Uuid,
    pub user_id: Uuid,
    pub access_level: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct WorkspaceMemberDetailRow {
    pub workspace_id: Uuid,
    pub user_id: Uuid,
    pub role_label: String,
    pub created_at: DateTime<Utc>,
    pub email: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ProjectAccessGrantDetailRow {
    pub project_id: Uuid,
    pub user_id: Uuid,
    pub access_level: String,
    pub created_at: DateTime<Utc>,
    pub project_name: String,
    pub email: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ProviderAccountRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub provider_kind: String,
    pub label: String,
    pub api_base_url: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ModelProfileRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub provider_account_id: Uuid,
    pub profile_kind: String,
    pub model_name: String,
    pub temperature: Option<f64>,
    pub max_output_tokens: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Database repository helper: `list_workspaces`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn list_workspaces(pool: &PgPool) -> Result<Vec<WorkspaceRow>, sqlx::Error> {
    sqlx::query_as::<_, WorkspaceRow>(
        "select id, slug, name, status, created_at, updated_at from workspace order by created_at desc",
    )
    .fetch_all(pool)
    .await
}

/// Database repository helper: `create_workspace`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn create_workspace(
    pool: &PgPool,
    slug: &str,
    name: &str,
) -> Result<WorkspaceRow, sqlx::Error> {
    sqlx::query_as::<_, WorkspaceRow>(
        "insert into workspace (id, slug, name) values ($1, $2, $3)
         returning id, slug, name, status, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(slug)
    .bind(name)
    .fetch_one(pool)
    .await
}

/// Database repository helper: `find_or_create_default_workspace`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn find_or_create_default_workspace(pool: &PgPool) -> Result<WorkspaceRow, sqlx::Error> {
    if let Some(existing) = sqlx::query_as::<_, WorkspaceRow>(
        "select id, slug, name, status, created_at, updated_at
         from workspace
         order by created_at asc
         limit 1",
    )
    .fetch_optional(pool)
    .await?
    {
        return Ok(existing);
    }

    sqlx::query_as::<_, WorkspaceRow>(
        "insert into workspace (id, slug, name)
         values ($1, $2, $3)
         on conflict (slug) do update set name = workspace.name
         returning id, slug, name, status, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind("default")
    .bind("Default workspace")
    .fetch_one(pool)
    .await
}

/// Database repository helper: `list_projects`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn list_projects(
    pool: &PgPool,
    workspace_id: Option<Uuid>,
) -> Result<Vec<ProjectRow>, sqlx::Error> {
    match workspace_id {
        Some(workspace_id) => {
            sqlx::query_as::<_, ProjectRow>(
                "select id, workspace_id, slug, name, description, created_at, updated_at
                 from project where workspace_id = $1 order by created_at desc",
            )
            .bind(workspace_id)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, ProjectRow>(
                "select id, workspace_id, slug, name, description, created_at, updated_at
                 from project order by created_at desc",
            )
            .fetch_all(pool)
            .await
        }
    }
}

/// Database repository helper: `create_project`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn create_project(
    pool: &PgPool,
    workspace_id: Uuid,
    slug: &str,
    name: &str,
    description: Option<&str>,
) -> Result<ProjectRow, sqlx::Error> {
    sqlx::query_as::<_, ProjectRow>(
        "insert into project (id, workspace_id, slug, name, description) values ($1, $2, $3, $4, $5)
         returning id, workspace_id, slug, name, description, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind(slug)
    .bind(name)
    .bind(description)
    .fetch_one(pool)
    .await
}

/// Loads a UI user by login.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the `ui_user` row.
pub async fn get_ui_user_by_login(
    pool: &PgPool,
    login: &str,
) -> Result<Option<UiUserRow>, sqlx::Error> {
    sqlx::query_as::<_, UiUserRow>(
        "select id, login, email, display_name, role_label, password_hash, preferred_locale, created_at, updated_at
         from ui_user where lower(login) = lower($1)",
    )
    .bind(login)
    .fetch_optional(pool)
    .await
}

/// Loads a UI user by email.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the `ui_user` row.
pub async fn get_ui_user_by_email(
    pool: &PgPool,
    email: &str,
) -> Result<Option<UiUserRow>, sqlx::Error> {
    sqlx::query_as::<_, UiUserRow>(
        "select id, login, email, display_name, role_label, password_hash, preferred_locale, created_at, updated_at
         from ui_user where lower(email) = lower($1)",
    )
    .bind(email)
    .fetch_optional(pool)
    .await
}

/// Loads a UI user by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the `ui_user` row.
pub async fn get_ui_user_by_id(pool: &PgPool, id: Uuid) -> Result<Option<UiUserRow>, sqlx::Error> {
    sqlx::query_as::<_, UiUserRow>(
        "select id, login, email, display_name, role_label, password_hash, preferred_locale, created_at, updated_at
         from ui_user where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Counts persisted UI users.
///
/// # Errors
/// Returns any `SQLx` error raised while counting `ui_user` rows.
pub async fn count_ui_users(pool: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("select count(*) from ui_user").fetch_one(pool).await
}

/// Creates a new UI user row.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the `ui_user` row.
pub async fn create_ui_user(
    pool: &PgPool,
    login: &str,
    email: &str,
    display_name: &str,
    role_label: &str,
    password_hash: &str,
    preferred_locale: &str,
) -> Result<UiUserRow, sqlx::Error> {
    sqlx::query_as::<_, UiUserRow>(
        "insert into ui_user (id, login, email, display_name, role_label, password_hash, preferred_locale)
         values ($1, $2, $3, $4, $5, $6, $7)
         returning id, login, email, display_name, role_label, password_hash, preferred_locale, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(login)
    .bind(email)
    .bind(display_name)
    .bind(role_label)
    .bind(password_hash)
    .bind(preferred_locale)
    .fetch_one(pool)
    .await
}

/// Ensures a workspace membership exists for a UI user.
///
/// # Errors
/// Returns any `SQLx` error raised while upserting `workspace_member`.
pub async fn ensure_workspace_member(
    pool: &PgPool,
    workspace_id: Uuid,
    user_id: Uuid,
    role_label: &str,
) -> Result<WorkspaceMemberRow, sqlx::Error> {
    sqlx::query_as::<_, WorkspaceMemberRow>(
        "insert into workspace_member (workspace_id, user_id, role_label)
         values ($1, $2, $3)
         on conflict (workspace_id, user_id) do update
         set role_label = excluded.role_label
         returning workspace_id, user_id, role_label, created_at",
    )
    .bind(workspace_id)
    .bind(user_id)
    .bind(role_label)
    .fetch_one(pool)
    .await
}

/// Ensures a project access grant exists for a UI user.
///
/// # Errors
/// Returns any `SQLx` error raised while upserting `project_access_grant`.
pub async fn ensure_project_access_grant(
    pool: &PgPool,
    project_id: Uuid,
    user_id: Uuid,
    access_level: &str,
) -> Result<ProjectAccessGrantRow, sqlx::Error> {
    sqlx::query_as::<_, ProjectAccessGrantRow>(
        "insert into project_access_grant (project_id, user_id, access_level)
         values ($1, $2, $3)
         on conflict (project_id, user_id) do update
         set access_level = excluded.access_level
         returning project_id, user_id, access_level, created_at",
    )
    .bind(project_id)
    .bind(user_id)
    .bind(access_level)
    .fetch_one(pool)
    .await
}

/// Lists workspaces visible to a UI user.
///
/// # Errors
/// Returns any `SQLx` error raised while querying workspace memberships.
pub async fn list_workspaces_for_ui_user(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Vec<WorkspaceRow>, sqlx::Error> {
    sqlx::query_as::<_, WorkspaceRow>(
        "select workspace.id, workspace.slug, workspace.name, workspace.status, workspace.created_at, workspace.updated_at
         from workspace
         join workspace_member on workspace_member.workspace_id = workspace.id
         where workspace_member.user_id = $1
         order by workspace.created_at asc",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
}

/// Lists projects visible to a UI user for a workspace.
///
/// # Errors
/// Returns any `SQLx` error raised while querying project access grants.
pub async fn list_projects_for_ui_user(
    pool: &PgPool,
    user_id: Uuid,
    workspace_id: Uuid,
) -> Result<Vec<ProjectRow>, sqlx::Error> {
    sqlx::query_as::<_, ProjectRow>(
        "select project.id, project.workspace_id, project.slug, project.name, project.description, project.created_at, project.updated_at
         from project
         join project_access_grant on project_access_grant.project_id = project.id
         where project_access_grant.user_id = $1 and project.workspace_id = $2
         order by project.created_at asc",
    )
    .bind(user_id)
    .bind(workspace_id)
    .fetch_all(pool)
    .await
}

/// Lists workspace members with their user metadata.
///
/// # Errors
/// Returns any `SQLx` error raised while querying workspace memberships.
pub async fn list_workspace_members(
    pool: &PgPool,
    workspace_id: Uuid,
) -> Result<Vec<WorkspaceMemberDetailRow>, sqlx::Error> {
    sqlx::query_as::<_, WorkspaceMemberDetailRow>(
        "select workspace_member.workspace_id,
                workspace_member.user_id,
                workspace_member.role_label,
                workspace_member.created_at,
                ui_user.email,
                ui_user.display_name
         from workspace_member
         join ui_user on ui_user.id = workspace_member.user_id
         where workspace_member.workspace_id = $1
         order by workspace_member.created_at asc, ui_user.display_name asc",
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await
}

/// Lists project access grants with project and user metadata for a workspace.
///
/// # Errors
/// Returns any `SQLx` error raised while querying access grants.
pub async fn list_project_access_grants(
    pool: &PgPool,
    workspace_id: Uuid,
) -> Result<Vec<ProjectAccessGrantDetailRow>, sqlx::Error> {
    sqlx::query_as::<_, ProjectAccessGrantDetailRow>(
        "select project_access_grant.project_id,
                project_access_grant.user_id,
                project_access_grant.access_level,
                project_access_grant.created_at,
                project.name as project_name,
                ui_user.email,
                ui_user.display_name
         from project_access_grant
         join project on project.id = project_access_grant.project_id
         join ui_user on ui_user.id = project_access_grant.user_id
         where project.workspace_id = $1
         order by project.name asc, ui_user.display_name asc",
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await
}

/// Creates a new UI session row.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the `ui_session` row.
pub async fn create_ui_session(
    pool: &PgPool,
    user_id: Uuid,
    active_workspace_id: Option<Uuid>,
    active_project_id: Option<Uuid>,
    locale: &str,
    expires_at: DateTime<Utc>,
) -> Result<UiSessionRow, sqlx::Error> {
    sqlx::query_as::<_, UiSessionRow>(
        "insert into ui_session (id, user_id, active_workspace_id, active_project_id, locale, expires_at)
         values ($1, $2, $3, $4, $5, $6)
         returning id, user_id, active_workspace_id, active_project_id, locale, expires_at, created_at, updated_at, last_seen_at",
    )
    .bind(Uuid::now_v7())
    .bind(user_id)
    .bind(active_workspace_id)
    .bind(active_project_id)
    .bind(locale)
    .bind(expires_at)
    .fetch_one(pool)
    .await
}

/// Loads a UI session by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the `ui_session` row.
pub async fn get_ui_session_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<UiSessionRow>, sqlx::Error> {
    sqlx::query_as::<_, UiSessionRow>(
        "select id, user_id, active_workspace_id, active_project_id, locale, expires_at, created_at, updated_at, last_seen_at
         from ui_session where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Refreshes UI session activity and active context.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the `ui_session` row.
pub async fn touch_ui_session(
    pool: &PgPool,
    id: Uuid,
    active_workspace_id: Option<Uuid>,
    active_project_id: Option<Uuid>,
    locale: &str,
) -> Result<Option<UiSessionRow>, sqlx::Error> {
    sqlx::query_as::<_, UiSessionRow>(
        "update ui_session
         set active_workspace_id = $2,
             active_project_id = $3,
             locale = $4,
             updated_at = now(),
             last_seen_at = now()
         where id = $1
         returning id, user_id, active_workspace_id, active_project_id, locale, expires_at, created_at, updated_at, last_seen_at",
    )
    .bind(id)
    .bind(active_workspace_id)
    .bind(active_project_id)
    .bind(locale)
    .fetch_optional(pool)
    .await
}

/// Deletes a UI session by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while deleting the `ui_session` row.
pub async fn revoke_ui_session(pool: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("delete from ui_session where id = $1").bind(id).execute(pool).await?;
    Ok(result.rows_affected() > 0)
}

/// Database repository helper: `find_or_create_default_project`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn find_or_create_default_project(
    pool: &PgPool,
    workspace_id: Uuid,
) -> Result<ProjectRow, sqlx::Error> {
    if let Some(existing) = sqlx::query_as::<_, ProjectRow>(
        "select id, workspace_id, slug, name, description, created_at, updated_at
         from project
         where workspace_id = $1
         order by created_at asc
         limit 1",
    )
    .bind(workspace_id)
    .fetch_optional(pool)
    .await?
    {
        return Ok(existing);
    }

    sqlx::query_as::<_, ProjectRow>(
        "insert into project (id, workspace_id, slug, name, description)
         values ($1, $2, $3, $4, $5)
         on conflict (workspace_id, slug) do update set name = project.name
         returning id, workspace_id, slug, name, description, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind("default-library")
    .bind("Default library")
    .bind(Some("Backstage default library for the primary documents and ask flow"))
    .fetch_one(pool)
    .await
}

/// Database repository helper: `list_provider_accounts`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn list_provider_accounts(
    pool: &PgPool,
    workspace_id: Option<Uuid>,
) -> Result<Vec<ProviderAccountRow>, sqlx::Error> {
    match workspace_id {
        Some(workspace_id) => {
            sqlx::query_as::<_, ProviderAccountRow>(
                "select id, workspace_id, provider_kind, label, api_base_url, status, created_at, updated_at
                 from provider_account where workspace_id = $1 order by created_at desc",
            )
            .bind(workspace_id)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, ProviderAccountRow>(
                "select id, workspace_id, provider_kind, label, api_base_url, status, created_at, updated_at
                 from provider_account order by created_at desc",
            )
            .fetch_all(pool)
            .await
        }
    }
}

/// Database repository helper: `create_provider_account`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn create_provider_account(
    pool: &PgPool,
    workspace_id: Uuid,
    provider_kind: &str,
    label: &str,
    api_base_url: Option<&str>,
) -> Result<ProviderAccountRow, sqlx::Error> {
    sqlx::query_as::<_, ProviderAccountRow>(
        "insert into provider_account (id, workspace_id, provider_kind, label, api_base_url)
         values ($1, $2, $3, $4, $5)
         returning id, workspace_id, provider_kind, label, api_base_url, status, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind(provider_kind)
    .bind(label)
    .bind(api_base_url)
    .fetch_one(pool)
    .await
}

/// Database repository helper: `list_model_profiles`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn list_model_profiles(
    pool: &PgPool,
    workspace_id: Option<Uuid>,
) -> Result<Vec<ModelProfileRow>, sqlx::Error> {
    match workspace_id {
        Some(workspace_id) => {
            sqlx::query_as::<_, ModelProfileRow>(
                "select id, workspace_id, provider_account_id, profile_kind, model_name, temperature, max_output_tokens, created_at, updated_at
                 from model_profile where workspace_id = $1 order by created_at desc",
            )
            .bind(workspace_id)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, ModelProfileRow>(
                "select id, workspace_id, provider_account_id, profile_kind, model_name, temperature, max_output_tokens, created_at, updated_at
                 from model_profile order by created_at desc",
            )
            .fetch_all(pool)
            .await
        }
    }
}

/// Database repository helper: `create_model_profile`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn create_model_profile(
    pool: &PgPool,
    workspace_id: Uuid,
    provider_account_id: Uuid,
    profile_kind: &str,
    model_name: &str,
    temperature: Option<f64>,
    max_output_tokens: Option<i32>,
) -> Result<ModelProfileRow, sqlx::Error> {
    sqlx::query_as::<_, ModelProfileRow>(
        "insert into model_profile (id, workspace_id, provider_account_id, profile_kind, model_name, temperature, max_output_tokens)
         values ($1, $2, $3, $4, $5, $6, $7)
         returning id, workspace_id, provider_account_id, profile_kind, model_name, temperature, max_output_tokens, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind(provider_account_id)
    .bind(profile_kind)
    .bind(model_name)
    .bind(temperature)
    .bind(max_output_tokens)
    .fetch_one(pool)
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct SourceRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub source_kind: String,
    pub label: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct IngestionJobRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub source_id: Option<Uuid>,
    pub trigger_kind: String,
    pub status: String,
    pub stage: String,
    pub requested_by: Option<String>,
    pub error_message: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub idempotency_key: Option<String>,
    pub parent_job_id: Option<Uuid>,
    pub attempt_count: i32,
    pub worker_id: Option<String>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub heartbeat_at: Option<DateTime<Utc>>,
    pub payload_json: serde_json::Value,
    pub result_json: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseRenewalOutcome {
    Renewed,
    Busy,
    NotOwned,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RecoveredIngestionJobRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub source_id: Option<Uuid>,
    pub trigger_kind: String,
    pub status: String,
    pub stage: String,
    pub requested_by: Option<String>,
    pub error_message: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub idempotency_key: Option<String>,
    pub parent_job_id: Option<Uuid>,
    pub attempt_count: i32,
    pub worker_id: Option<String>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub heartbeat_at: Option<DateTime<Utc>>,
    pub payload_json: serde_json::Value,
    pub result_json: serde_json::Value,
    pub previous_status: String,
    pub previous_stage: String,
    pub previous_worker_id: Option<String>,
    pub previous_error_message: Option<String>,
    pub previous_lease_expires_at: Option<DateTime<Utc>>,
    pub previous_heartbeat_at: Option<DateTime<Utc>>,
}

impl RecoveredIngestionJobRow {
    #[must_use]
    pub fn current_job(&self) -> IngestionJobRow {
        IngestionJobRow {
            id: self.id,
            project_id: self.project_id,
            source_id: self.source_id,
            trigger_kind: self.trigger_kind.clone(),
            status: self.status.clone(),
            stage: self.stage.clone(),
            requested_by: self.requested_by.clone(),
            error_message: self.error_message.clone(),
            started_at: self.started_at,
            finished_at: self.finished_at,
            created_at: self.created_at,
            updated_at: self.updated_at,
            idempotency_key: self.idempotency_key.clone(),
            parent_job_id: self.parent_job_id,
            attempt_count: self.attempt_count,
            worker_id: self.worker_id.clone(),
            lease_expires_at: self.lease_expires_at,
            heartbeat_at: self.heartbeat_at,
            payload_json: self.payload_json.clone(),
            result_json: self.result_json.clone(),
        }
    }

    #[must_use]
    pub fn attempt_worker_id<'a>(&'a self, fallback_worker_id: &'a str) -> &'a str {
        self.previous_worker_id.as_deref().unwrap_or(fallback_worker_id)
    }
}

const INGESTION_JOB_COLUMNS: &str = "id, project_id, source_id, trigger_kind, status, stage, requested_by, error_message, started_at, finished_at, created_at, updated_at, idempotency_key, parent_job_id, attempt_count, worker_id, lease_expires_at, heartbeat_at, payload_json, result_json";

fn ingestion_job_columns_for_alias(alias: &str) -> String {
    format!(
        "{alias}.id as id, \
         {alias}.project_id as project_id, \
         {alias}.source_id as source_id, \
         {alias}.trigger_kind as trigger_kind, \
         {alias}.status as status, \
         {alias}.stage as stage, \
         {alias}.requested_by as requested_by, \
         {alias}.error_message as error_message, \
         {alias}.started_at as started_at, \
         {alias}.finished_at as finished_at, \
         {alias}.created_at as created_at, \
         {alias}.updated_at as updated_at, \
         {alias}.idempotency_key as idempotency_key, \
         {alias}.parent_job_id as parent_job_id, \
         {alias}.attempt_count as attempt_count, \
         {alias}.worker_id as worker_id, \
         {alias}.lease_expires_at as lease_expires_at, \
         {alias}.heartbeat_at as heartbeat_at, \
         {alias}.payload_json as payload_json, \
         {alias}.result_json as result_json"
    )
}

fn recovered_ingestion_job_columns(current_alias: &str, previous_alias: &str) -> String {
    format!(
        "{current_columns}, \
         {previous_alias}.status as previous_status, \
         {previous_alias}.stage as previous_stage, \
         {previous_alias}.worker_id as previous_worker_id, \
         {previous_alias}.error_message as previous_error_message, \
         {previous_alias}.lease_expires_at as previous_lease_expires_at, \
         {previous_alias}.heartbeat_at as previous_heartbeat_at",
        current_columns = ingestion_job_columns_for_alias(current_alias),
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestionExecutionPayload {
    pub project_id: Uuid,
    #[serde(default)]
    pub runtime_ingestion_run_id: Option<Uuid>,
    #[serde(default)]
    pub upload_batch_id: Option<Uuid>,
    #[serde(default)]
    pub logical_document_id: Option<Uuid>,
    #[serde(default)]
    pub target_revision_id: Option<Uuid>,
    #[serde(default)]
    pub document_mutation_workflow_id: Option<Uuid>,
    #[serde(default)]
    pub stale_guard_revision_no: Option<i32>,
    #[serde(default)]
    pub attempt_kind: Option<String>,
    #[serde(default)]
    pub mutation_kind: Option<String>,
    pub source_id: Option<Uuid>,
    pub external_key: String,
    pub title: Option<String>,
    pub mime_type: Option<String>,
    pub text: Option<String>,
    pub file_kind: Option<String>,
    pub file_size_bytes: Option<u64>,
    pub adapter_status: Option<String>,
    pub extraction_error: Option<String>,
    #[serde(default)]
    pub extraction_kind: Option<String>,
    #[serde(default)]
    pub page_count: Option<u32>,
    #[serde(default)]
    pub extraction_warnings: Vec<String>,
    #[serde(default = "default_json_object")]
    pub source_map: serde_json::Value,
    #[serde(default)]
    pub extraction_provider_kind: Option<String>,
    #[serde(default)]
    pub extraction_model_name: Option<String>,
    #[serde(default)]
    pub extraction_version: Option<String>,
    pub ingest_mode: String,
    pub extra_metadata: serde_json::Value,
}

fn default_json_object() -> serde_json::Value {
    serde_json::json!({})
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct IngestionJobAttemptRow {
    pub id: Uuid,
    pub job_id: Uuid,
    pub attempt_no: i32,
    pub worker_id: Option<String>,
    pub status: String,
    pub stage: String,
    pub error_message: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Database repository helper: `list_sources`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn list_sources(
    pool: &PgPool,
    project_id: Option<Uuid>,
) -> Result<Vec<SourceRow>, sqlx::Error> {
    match project_id {
        Some(project_id) => {
            sqlx::query_as::<_, SourceRow>(
                "select id, project_id, source_kind, label, status, created_at, updated_at
                 from source where project_id = $1 order by created_at desc",
            )
            .bind(project_id)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, SourceRow>(
                "select id, project_id, source_kind, label, status, created_at, updated_at
                 from source order by created_at desc",
            )
            .fetch_all(pool)
            .await
        }
    }
}

/// Database repository helper: `create_source`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn create_source(
    pool: &PgPool,
    project_id: Uuid,
    source_kind: &str,
    label: &str,
) -> Result<SourceRow, sqlx::Error> {
    sqlx::query_as::<_, SourceRow>(
        "insert into source (id, project_id, source_kind, label) values ($1, $2, $3, $4)
         returning id, project_id, source_kind, label, status, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(source_kind)
    .bind(label)
    .fetch_one(pool)
    .await
}

/// Database repository helper: `list_ingestion_jobs`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn list_ingestion_jobs(
    pool: &PgPool,
    project_id: Option<Uuid>,
) -> Result<Vec<IngestionJobRow>, sqlx::Error> {
    match project_id {
        Some(project_id) => {
            sqlx::query_as::<_, IngestionJobRow>(&format!(
                "select {INGESTION_JOB_COLUMNS}
                 from ingestion_job where project_id = $1 order by created_at desc",
            ))
            .bind(project_id)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, IngestionJobRow>(&format!(
                "select {INGESTION_JOB_COLUMNS}
                 from ingestion_job order by created_at desc",
            ))
            .fetch_all(pool)
            .await
        }
    }
}

/// Database repository helper: `create_ingestion_job`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn create_ingestion_job(
    pool: &PgPool,
    project_id: Uuid,
    source_id: Option<Uuid>,
    trigger_kind: &str,
    requested_by: Option<&str>,
    parent_job_id: Option<Uuid>,
    idempotency_key: Option<&str>,
    initial_attempt_count: Option<i32>,
    payload_json: serde_json::Value,
) -> Result<IngestionJobRow, sqlx::Error> {
    sqlx::query_as::<_, IngestionJobRow>(&format!(
        "insert into ingestion_job (id, project_id, source_id, trigger_kind, status, stage, requested_by, parent_job_id, idempotency_key, attempt_count, payload_json)
         values ($1, $2, $3, $4, 'queued', 'created', $5, $6, $7, coalesce($8, 0), $9)
         returning {INGESTION_JOB_COLUMNS}",
    ))
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(source_id)
    .bind(trigger_kind)
    .bind(requested_by)
    .bind(parent_job_id)
    .bind(idempotency_key)
    .bind(initial_attempt_count)
    .bind(payload_json)
    .fetch_one(pool)
    .await
}

pub async fn repair_queued_runtime_ingestion_job_attempt_counts(
    pool: &PgPool,
) -> Result<u64, sqlx::Error> {
    let repaired = sqlx::query(
        "update ingestion_job as job
         set attempt_count = greatest(job.attempt_count, run.current_attempt_no),
             updated_at = now()
         from runtime_ingestion_run as run
         where job.status = 'queued'
           and job.payload_json ? 'runtime_ingestion_run_id'
           and (job.payload_json ->> 'runtime_ingestion_run_id')::uuid = run.id
           and job.attempt_count < run.current_attempt_no",
    )
    .execute(pool)
    .await?;

    Ok(repaired.rows_affected())
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct DocumentRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub source_id: Option<Uuid>,
    pub external_key: String,
    pub title: Option<String>,
    pub mime_type: Option<String>,
    pub checksum: Option<String>,
    pub current_revision_id: Option<Uuid>,
    pub active_status: String,
    pub active_mutation_kind: Option<String>,
    pub active_mutation_status: Option<String>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct DocumentRevisionRow {
    pub id: Uuid,
    pub document_id: Uuid,
    pub revision_no: i32,
    pub revision_kind: String,
    pub parent_revision_id: Option<Uuid>,
    pub source_file_name: String,
    pub mime_type: Option<String>,
    pub file_size_bytes: Option<i64>,
    pub appended_text_excerpt: Option<String>,
    pub content_hash: Option<String>,
    pub status: String,
    pub accepted_at: DateTime<Utc>,
    pub activated_at: Option<DateTime<Utc>>,
    pub superseded_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct DocumentMutationWorkflowRow {
    pub id: Uuid,
    pub document_id: Uuid,
    pub target_revision_id: Option<Uuid>,
    pub mutation_kind: String,
    pub status: String,
    pub stale_guard_revision_no: Option<i32>,
    pub requested_by: Option<String>,
    pub accepted_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct LogicalDocumentProjectionRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub source_id: Option<Uuid>,
    pub external_key: String,
    pub title: Option<String>,
    pub mime_type: Option<String>,
    pub checksum: Option<String>,
    pub current_revision_id: Option<Uuid>,
    pub active_revision_no: Option<i32>,
    pub active_revision_kind: Option<String>,
    pub active_revision_status: Option<String>,
    pub active_status: String,
    pub active_mutation_kind: Option<String>,
    pub active_mutation_status: Option<String>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Database repository helper: `list_documents`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn list_documents(
    pool: &PgPool,
    project_id: Option<Uuid>,
) -> Result<Vec<DocumentRow>, sqlx::Error> {
    match project_id {
        Some(project_id) => {
            sqlx::query_as::<_, DocumentRow>(
                "select id, project_id, source_id, external_key, title, mime_type, checksum,
                    current_revision_id, active_status, active_mutation_kind, active_mutation_status,
                    deleted_at, created_at, updated_at
                 from document where project_id = $1 order by created_at desc",
            )
            .bind(project_id)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, DocumentRow>(
                "select id, project_id, source_id, external_key, title, mime_type, checksum,
                    current_revision_id, active_status, active_mutation_kind, active_mutation_status,
                    deleted_at, created_at, updated_at
                 from document order by created_at desc",
            )
            .fetch_all(pool)
            .await
        }
    }
}

/// Database repository helper: `next_document_revision_no`.
///
/// # Errors
/// Returns any `SQLx` error raised while computing the next revision number.
pub async fn next_document_revision_no(
    pool: &PgPool,
    document_id: Uuid,
) -> Result<i32, sqlx::Error> {
    let row: (i32,) = sqlx::query_as(
        "select coalesce(max(revision_no), 0)::integer + 1
         from document_revision
         where document_id = $1",
    )
    .bind(document_id)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

/// Database repository helper: `create_document_revision`.
///
/// # Errors
/// Returns any `SQLx` error raised while creating a document revision row.
pub async fn create_document_revision(
    pool: &PgPool,
    document_id: Uuid,
    revision_no: i32,
    revision_kind: &str,
    parent_revision_id: Option<Uuid>,
    source_file_name: &str,
    mime_type: Option<&str>,
    file_size_bytes: Option<i64>,
    appended_text_excerpt: Option<&str>,
    content_hash: Option<&str>,
) -> Result<DocumentRevisionRow, sqlx::Error> {
    sqlx::query_as::<_, DocumentRevisionRow>(
        "insert into document_revision (
            id, document_id, revision_no, revision_kind, parent_revision_id, source_file_name,
            mime_type, file_size_bytes, appended_text_excerpt, content_hash, status
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'pending')
         returning id, document_id, revision_no, revision_kind, parent_revision_id, source_file_name,
            mime_type, file_size_bytes, appended_text_excerpt, content_hash, status, accepted_at,
            activated_at, superseded_at, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(document_id)
    .bind(revision_no)
    .bind(revision_kind)
    .bind(parent_revision_id)
    .bind(source_file_name)
    .bind(mime_type)
    .bind(file_size_bytes)
    .bind(appended_text_excerpt)
    .bind(content_hash)
    .fetch_one(pool)
    .await
}

/// Database repository helper: `list_document_revisions_by_document_id`.
///
/// # Errors
/// Returns any `SQLx` error raised while listing document revisions.
pub async fn list_document_revisions_by_document_id(
    pool: &PgPool,
    document_id: Uuid,
) -> Result<Vec<DocumentRevisionRow>, sqlx::Error> {
    sqlx::query_as::<_, DocumentRevisionRow>(
        "select id, document_id, revision_no, revision_kind, parent_revision_id, source_file_name,
            mime_type, file_size_bytes, appended_text_excerpt, content_hash, status, accepted_at,
            activated_at, superseded_at, created_at, updated_at
         from document_revision
         where document_id = $1
         order by revision_no desc, accepted_at desc",
    )
    .bind(document_id)
    .fetch_all(pool)
    .await
}

/// Database repository helper: `get_document_revision_by_id`.
///
/// # Errors
/// Returns any `SQLx` error raised while loading one document revision.
pub async fn get_document_revision_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<DocumentRevisionRow>, sqlx::Error> {
    sqlx::query_as::<_, DocumentRevisionRow>(
        "select id, document_id, revision_no, revision_kind, parent_revision_id, source_file_name,
            mime_type, file_size_bytes, appended_text_excerpt, content_hash, status, accepted_at,
            activated_at, superseded_at, created_at, updated_at
         from document_revision
         where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Database repository helper: `get_active_document_revision_by_document_id`.
///
/// # Errors
/// Returns any `SQLx` error raised while loading the active document revision.
pub async fn get_active_document_revision_by_document_id(
    pool: &PgPool,
    document_id: Uuid,
) -> Result<Option<DocumentRevisionRow>, sqlx::Error> {
    sqlx::query_as::<_, DocumentRevisionRow>(
        "select id, document_id, revision_no, revision_kind, parent_revision_id, source_file_name,
            mime_type, file_size_bytes, appended_text_excerpt, content_hash, status, accepted_at,
            activated_at, superseded_at, created_at, updated_at
         from document_revision
         where document_id = $1 and status = 'active'
         order by revision_no desc
         limit 1",
    )
    .bind(document_id)
    .fetch_optional(pool)
    .await
}

/// Database repository helper: `supersede_document_revisions`.
///
/// # Errors
/// Returns any `SQLx` error raised while superseding previous revisions.
pub async fn supersede_document_revisions(
    pool: &PgPool,
    document_id: Uuid,
    keep_revision_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "update document_revision
         set status = 'superseded',
             superseded_at = now(),
             updated_at = now()
         where document_id = $1
           and id <> $2
           and status = 'active'",
    )
    .bind(document_id)
    .bind(keep_revision_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Database repository helper: `activate_document_revision`.
///
/// # Errors
/// Returns any `SQLx` error raised while activating a revision.
pub async fn activate_document_revision(
    pool: &PgPool,
    document_id: Uuid,
    revision_id: Uuid,
) -> Result<DocumentRevisionRow, sqlx::Error> {
    sqlx::query_as::<_, DocumentRevisionRow>(
        "update document_revision
         set status = 'active',
             activated_at = now(),
             updated_at = now()
         where id = $1
           and document_id = $2
         returning id, document_id, revision_no, revision_kind, parent_revision_id, source_file_name,
            mime_type, file_size_bytes, appended_text_excerpt, content_hash, status, accepted_at,
            activated_at, superseded_at, created_at, updated_at",
    )
    .bind(revision_id)
    .bind(document_id)
    .fetch_one(pool)
    .await
}

/// Database repository helper: `update_document_revision_status`.
///
/// # Errors
/// Returns any `SQLx` error raised while updating one revision status.
pub async fn update_document_revision_status(
    pool: &PgPool,
    revision_id: Uuid,
    status: &str,
) -> Result<DocumentRevisionRow, sqlx::Error> {
    sqlx::query_as::<_, DocumentRevisionRow>(
        "update document_revision
         set status = $2,
             superseded_at = case when $2 = 'superseded' then coalesce(superseded_at, now()) else superseded_at end,
             updated_at = now()
         where id = $1
         returning id, document_id, revision_no, revision_kind, parent_revision_id, source_file_name,
            mime_type, file_size_bytes, appended_text_excerpt, content_hash, status, accepted_at,
            activated_at, superseded_at, created_at, updated_at",
    )
    .bind(revision_id)
    .bind(status)
    .fetch_one(pool)
    .await
}

/// Database repository helper: `update_document_current_revision`.
///
/// # Errors
/// Returns any `SQLx` error raised while updating logical document lifecycle fields.
pub async fn update_document_current_revision(
    pool: &PgPool,
    document_id: Uuid,
    current_revision_id: Option<Uuid>,
    active_status: &str,
    active_mutation_kind: Option<&str>,
    active_mutation_status: Option<&str>,
) -> Result<DocumentRow, sqlx::Error> {
    sqlx::query_as::<_, DocumentRow>(
        "update document
         set current_revision_id = $2,
             active_status = $3,
             active_mutation_kind = $4,
             active_mutation_status = $5,
             updated_at = now()
         where id = $1
         returning id, project_id, source_id, external_key, title, mime_type, checksum,
            current_revision_id, active_status, active_mutation_kind, active_mutation_status,
            deleted_at, created_at, updated_at",
    )
    .bind(document_id)
    .bind(current_revision_id)
    .bind(active_status)
    .bind(active_mutation_kind)
    .bind(active_mutation_status)
    .fetch_one(pool)
    .await
}

/// Database repository helper: `create_document_mutation_workflow`.
///
/// # Errors
/// Returns any `SQLx` error raised while creating a mutation workflow row.
pub async fn create_document_mutation_workflow(
    pool: &PgPool,
    document_id: Uuid,
    target_revision_id: Option<Uuid>,
    mutation_kind: &str,
    stale_guard_revision_no: Option<i32>,
    requested_by: Option<&str>,
) -> Result<DocumentMutationWorkflowRow, sqlx::Error> {
    sqlx::query_as::<_, DocumentMutationWorkflowRow>(
        "insert into document_mutation_workflow (
            id, document_id, target_revision_id, mutation_kind, status, stale_guard_revision_no, requested_by
         ) values ($1, $2, $3, $4, 'accepted', $5, $6)
         returning id, document_id, target_revision_id, mutation_kind, status, stale_guard_revision_no,
            requested_by, accepted_at, finished_at, error_message, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(document_id)
    .bind(target_revision_id)
    .bind(mutation_kind)
    .bind(stale_guard_revision_no)
    .bind(requested_by)
    .fetch_one(pool)
    .await
}

/// Database repository helper: `get_active_document_mutation_workflow_by_document_id`.
///
/// # Errors
/// Returns any `SQLx` error raised while loading the active mutation workflow.
pub async fn get_active_document_mutation_workflow_by_document_id(
    pool: &PgPool,
    document_id: Uuid,
) -> Result<Option<DocumentMutationWorkflowRow>, sqlx::Error> {
    sqlx::query_as::<_, DocumentMutationWorkflowRow>(
        "select id, document_id, target_revision_id, mutation_kind, status, stale_guard_revision_no,
            requested_by, accepted_at, finished_at, error_message, created_at, updated_at
         from document_mutation_workflow
         where document_id = $1
           and status in ('accepted', 'reconciling')
         order by accepted_at desc
         limit 1",
    )
    .bind(document_id)
    .fetch_optional(pool)
    .await
}

/// Database repository helper: `update_document_mutation_workflow_status`.
///
/// # Errors
/// Returns any `SQLx` error raised while updating a mutation workflow status.
pub async fn update_document_mutation_workflow_status(
    pool: &PgPool,
    id: Uuid,
    status: &str,
    error_message: Option<&str>,
) -> Result<DocumentMutationWorkflowRow, sqlx::Error> {
    sqlx::query_as::<_, DocumentMutationWorkflowRow>(
        "update document_mutation_workflow
         set status = $2,
             error_message = $3,
             finished_at = case when $2 in ('completed', 'failed') then now() else finished_at end,
             updated_at = now()
         where id = $1
         returning id, document_id, target_revision_id, mutation_kind, status, stale_guard_revision_no,
            requested_by, accepted_at, finished_at, error_message, created_at, updated_at",
    )
    .bind(id)
    .bind(status)
    .bind(error_message)
    .fetch_one(pool)
    .await
}

/// Database repository helper: `create_document`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn create_document(
    pool: &PgPool,
    project_id: Uuid,
    source_id: Option<Uuid>,
    external_key: &str,
    title: Option<&str>,
    mime_type: Option<&str>,
    checksum: Option<&str>,
) -> Result<DocumentRow, sqlx::Error> {
    sqlx::query_as::<_, DocumentRow>(
        "insert into document (id, project_id, source_id, external_key, title, mime_type, checksum)
         values ($1, $2, $3, $4, $5, $6, $7)
         returning id, project_id, source_id, external_key, title, mime_type, checksum,
            current_revision_id, active_status, active_mutation_kind, active_mutation_status,
            deleted_at, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(source_id)
    .bind(external_key)
    .bind(title)
    .bind(mime_type)
    .bind(checksum)
    .fetch_one(pool)
    .await
}

/// Database repository helper: `get_document_by_id`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn get_document_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<DocumentRow>, sqlx::Error> {
    sqlx::query_as::<_, DocumentRow>(
        "select id, project_id, source_id, external_key, title, mime_type, checksum,
            current_revision_id, active_status, active_mutation_kind, active_mutation_status,
            deleted_at, created_at, updated_at
         from document where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Updates mutable document metadata while preserving logical identity.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the document row.
pub async fn update_document_metadata(
    pool: &PgPool,
    id: Uuid,
    external_key: &str,
    title: Option<&str>,
    mime_type: Option<&str>,
    checksum: Option<&str>,
) -> Result<DocumentRow, sqlx::Error> {
    sqlx::query_as::<_, DocumentRow>(
        "update document
         set external_key = $2,
             title = $3,
             mime_type = $4,
             checksum = $5,
             updated_at = now()
         where id = $1
         returning id, project_id, source_id, external_key, title, mime_type, checksum,
            current_revision_id, active_status, active_mutation_kind, active_mutation_status,
            deleted_at, created_at, updated_at",
    )
    .bind(id)
    .bind(external_key)
    .bind(title)
    .bind(mime_type)
    .bind(checksum)
    .fetch_one(pool)
    .await
}

/// Tombstones a logical document while preserving its history rows.
///
/// # Errors
/// Returns any `SQLx` error raised while marking the document as deleted.
pub async fn tombstone_document_by_id(
    pool: &PgPool,
    id: Uuid,
    active_status: &str,
    active_mutation_kind: Option<&str>,
    active_mutation_status: Option<&str>,
) -> Result<DocumentRow, sqlx::Error> {
    sqlx::query_as::<_, DocumentRow>(
        "update document
         set deleted_at = coalesce(deleted_at, now()),
             active_status = $2,
             active_mutation_kind = $3,
             active_mutation_status = $4,
             updated_at = now()
         where id = $1
         returning id, project_id, source_id, external_key, title, mime_type, checksum,
            current_revision_id, active_status, active_mutation_kind, active_mutation_status,
            deleted_at, created_at, updated_at",
    )
    .bind(id)
    .bind(active_status)
    .bind(active_mutation_kind)
    .bind(active_mutation_status)
    .fetch_one(pool)
    .await
}

/// Loads one logical document projection together with the currently active revision metadata.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the projection row.
pub async fn get_logical_document_projection_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<LogicalDocumentProjectionRow>, sqlx::Error> {
    sqlx::query_as::<_, LogicalDocumentProjectionRow>(
        "select document.id, document.project_id, document.source_id, document.external_key,
            document.title, document.mime_type, document.checksum, document.current_revision_id,
            revision.revision_no as active_revision_no,
            revision.revision_kind as active_revision_kind,
            revision.status as active_revision_status,
            document.active_status, document.active_mutation_kind, document.active_mutation_status,
            document.deleted_at, document.created_at, document.updated_at
         from document
         left join document_revision as revision
           on revision.id = document.current_revision_id
         where document.id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Lists logical document projections for one project.
///
/// # Errors
/// Returns any `SQLx` error raised while querying logical document projections.
pub async fn list_logical_document_projections_by_project(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Vec<LogicalDocumentProjectionRow>, sqlx::Error> {
    sqlx::query_as::<_, LogicalDocumentProjectionRow>(
        "select document.id, document.project_id, document.source_id, document.external_key,
            document.title, document.mime_type, document.checksum, document.current_revision_id,
            revision.revision_no as active_revision_no,
            revision.revision_kind as active_revision_kind,
            revision.status as active_revision_status,
            document.active_status, document.active_mutation_kind, document.active_mutation_status,
            document.deleted_at, document.created_at, document.updated_at
         from document
         left join document_revision as revision
           on revision.id = document.current_revision_id
         where document.project_id = $1
         order by document.created_at desc",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
}

/// Deletes a document by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while deleting the `document` row.
pub async fn delete_document_by_id(pool: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("delete from document where id = $1").bind(id).execute(pool).await?;
    Ok(result.rows_affected() > 0)
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ChunkRow {
    pub id: Uuid,
    pub document_id: Uuid,
    pub project_id: Uuid,
    pub ordinal: i32,
    pub content: String,
    pub token_count: Option<i32>,
    pub metadata_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

/// Database repository helper: `create_chunk`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn create_chunk(
    pool: &PgPool,
    document_id: Uuid,
    project_id: Uuid,
    ordinal: i32,
    content: &str,
    token_count: Option<i32>,
    metadata_json: serde_json::Value,
) -> Result<ChunkRow, sqlx::Error> {
    sqlx::query_as::<_, ChunkRow>(
        "insert into chunk (id, document_id, project_id, ordinal, content, token_count, metadata_json)
         values ($1, $2, $3, $4, $5, $6, $7)
         returning id, document_id, project_id, ordinal, content, token_count, metadata_json, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(document_id)
    .bind(project_id)
    .bind(ordinal)
    .bind(content)
    .bind(token_count)
    .bind(metadata_json)
    .fetch_one(pool)
    .await
}

/// Database repository helper: `list_chunks_by_document`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn list_chunks_by_document(
    pool: &PgPool,
    document_id: Uuid,
) -> Result<Vec<ChunkRow>, sqlx::Error> {
    sqlx::query_as::<_, ChunkRow>(
        "select id, document_id, project_id, ordinal, content, token_count, metadata_json, created_at
         from chunk where document_id = $1 order by ordinal asc",
    )
    .bind(document_id)
    .fetch_all(pool)
    .await
}

/// Deletes a set of chunk rows by id.
///
/// # Errors
/// Returns any `SQLx` error raised while deleting the chunk rows.
pub async fn delete_chunks_by_ids(pool: &PgPool, chunk_ids: &[Uuid]) -> Result<u64, sqlx::Error> {
    if chunk_ids.is_empty() {
        return Ok(0);
    }

    let result =
        sqlx::query("delete from chunk where id = any($1)").bind(chunk_ids).execute(pool).await?;
    Ok(result.rows_affected())
}

/// Deletes all chunk rows for one document.
///
/// # Errors
/// Returns any `SQLx` error raised while deleting the chunk rows.
pub async fn delete_chunks_by_document(
    pool: &PgPool,
    document_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("delete from chunk where document_id = $1")
        .bind(document_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// Loads one chunk by id.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the chunk.
pub async fn get_chunk_by_id(pool: &PgPool, id: Uuid) -> Result<Option<ChunkRow>, sqlx::Error> {
    sqlx::query_as::<_, ChunkRow>(
        "select id, document_id, project_id, ordinal, content, token_count, metadata_json, created_at
         from chunk where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Database repository helper: `search_chunks_by_project`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn search_chunks_by_project(
    pool: &PgPool,
    project_id: Uuid,
    query_text: &str,
    top_k: i32,
) -> Result<Vec<ChunkRow>, sqlx::Error> {
    let pattern = format!("%{query_text}%");
    sqlx::query_as::<_, ChunkRow>(
        "select id, document_id, project_id, ordinal, content, token_count, metadata_json, created_at
         from chunk
         where project_id = $1 and content ilike $2
         order by ordinal asc
         limit $3",
    )
    .bind(project_id)
    .bind(pattern)
    .bind(top_k)
    .fetch_all(pool)
    .await
}

/// Database repository helper: `list_chunks_by_project`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn list_chunks_by_project(
    pool: &PgPool,
    project_id: Uuid,
    limit: i64,
) -> Result<Vec<ChunkRow>, sqlx::Error> {
    sqlx::query_as::<_, ChunkRow>(
        "select id, document_id, project_id, ordinal, content, token_count, metadata_json, created_at
         from chunk where project_id = $1 order by created_at desc limit $2",
    )
    .bind(project_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ApiTokenRow {
    pub id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub token_kind: String,
    pub label: String,
    pub token_hash: String,
    pub token_preview: Option<String>,
    pub scope_json: serde_json::Value,
    pub status: String,
    pub last_used_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Finds an active API token row by its hashed token value.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the `api_token` row.
pub async fn find_api_token_by_hash(
    pool: &PgPool,
    token_hash: &str,
) -> Result<Option<ApiTokenRow>, sqlx::Error> {
    sqlx::query_as::<_, ApiTokenRow>(
        "select id, workspace_id, token_kind, label, token_hash, token_preview, scope_json, status, last_used_at, expires_at, created_at, updated_at
         from api_token
         where token_hash = $1
           and status = 'active'
           and (expires_at is null or expires_at > now())",
    )
    .bind(token_hash)
    .fetch_optional(pool)
    .await
}

/// Updates the last-used timestamp for an API token.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the `api_token` row.
pub async fn touch_api_token_last_used(
    pool: &PgPool,
    token_id: Uuid,
    min_interval_seconds: i64,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "update api_token
         set last_used_at = now(),
             updated_at = now()
         where id = $1
           and (
               last_used_at is null
               or last_used_at <= now() - ($2 * interval '1 second')
           )",
    )
    .bind(token_id)
    .bind(min_interval_seconds.max(1))
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Creates a new API token row.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the `api_token` row.
pub async fn create_api_token(
    pool: &PgPool,
    workspace_id: Option<Uuid>,
    token_kind: &str,
    label: &str,
    token_hash: &str,
    token_preview: Option<&str>,
    scope_json: serde_json::Value,
    expires_at: Option<DateTime<Utc>>,
) -> Result<ApiTokenRow, sqlx::Error> {
    sqlx::query_as::<_, ApiTokenRow>(
        "insert into api_token (id, workspace_id, token_kind, label, token_hash, token_preview, scope_json, expires_at)
         values ($1, $2, $3, $4, $5, $6, $7, $8)
         returning id, workspace_id, token_kind, label, token_hash, token_preview, scope_json, status, last_used_at, expires_at, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind(token_kind)
    .bind(label)
    .bind(token_hash)
    .bind(token_preview)
    .bind(scope_json)
    .bind(expires_at)
    .fetch_one(pool)
    .await
}

/// Loads an API token by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the `api_token` row.
pub async fn get_api_token_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<ApiTokenRow>, sqlx::Error> {
    sqlx::query_as::<_, ApiTokenRow>(
        "select id, workspace_id, token_kind, label, token_hash, token_preview, scope_json, status, last_used_at, expires_at, created_at, updated_at
         from api_token where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Lists API tokens, optionally filtered by workspace.
///
/// # Errors
/// Returns any `SQLx` error raised while querying `api_token` rows.
pub async fn list_api_tokens(
    pool: &PgPool,
    workspace_id: Option<Uuid>,
) -> Result<Vec<ApiTokenRow>, sqlx::Error> {
    match workspace_id {
        Some(workspace_id) => {
            sqlx::query_as::<_, ApiTokenRow>(
                "select id, workspace_id, token_kind, label, token_hash, token_preview, scope_json, status, last_used_at, expires_at, created_at, updated_at
                 from api_token where workspace_id = $1 order by created_at desc",
            )
            .bind(workspace_id)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, ApiTokenRow>(
                "select id, workspace_id, token_kind, label, token_hash, token_preview, scope_json, status, last_used_at, expires_at, created_at, updated_at
                 from api_token order by created_at desc",
            )
            .fetch_all(pool)
            .await
        }
    }
}

/// Revokes an API token and returns the updated row.
///
/// # Errors
/// Returns any `SQLx` error raised while updating or querying the `api_token` row.
pub async fn revoke_api_token(pool: &PgPool, id: Uuid) -> Result<Option<ApiTokenRow>, sqlx::Error> {
    sqlx::query_as::<_, ApiTokenRow>(
        "update api_token
         set status = 'revoked', updated_at = now()
         where id = $1
         returning id, workspace_id, token_kind, label, token_hash, token_preview, scope_json, status, last_used_at, expires_at, created_at, updated_at",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct McpAuditEventRow {
    pub id: Uuid,
    pub request_id: String,
    pub token_id: Uuid,
    pub token_kind: String,
    pub action_kind: String,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub document_id: Option<Uuid>,
    pub status: String,
    pub error_kind: Option<String>,
    pub metadata_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewMcpAuditEvent {
    pub request_id: String,
    pub token_id: Uuid,
    pub token_kind: String,
    pub action_kind: String,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub document_id: Option<Uuid>,
    pub status: String,
    pub error_kind: Option<String>,
    pub metadata_json: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct McpMutationReceiptRow {
    pub id: Uuid,
    pub token_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Option<Uuid>,
    pub operation_kind: String,
    pub idempotency_key: String,
    pub payload_identity: Option<String>,
    pub runtime_tracking_id: Option<String>,
    pub status: String,
    pub failure_kind: Option<String>,
    pub accepted_at: DateTime<Utc>,
    pub last_status_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewMcpMutationReceipt {
    pub token_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Option<Uuid>,
    pub operation_kind: String,
    pub idempotency_key: String,
    pub payload_identity: Option<String>,
    pub runtime_tracking_id: Option<String>,
    pub status: String,
    pub failure_kind: Option<String>,
}

/// Persists one MCP audit event row.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the `mcp_audit_event` row.
pub async fn create_mcp_audit_event(
    pool: &PgPool,
    new_event: &NewMcpAuditEvent,
) -> Result<McpAuditEventRow, sqlx::Error> {
    sqlx::query_as::<_, McpAuditEventRow>(
        "insert into mcp_audit_event (
            id, request_id, token_id, token_kind, action_kind, workspace_id, library_id,
            document_id, status, error_kind, metadata_json
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
         returning id, request_id, token_id, token_kind, action_kind, workspace_id, library_id,
            document_id, status, error_kind, metadata_json, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(&new_event.request_id)
    .bind(new_event.token_id)
    .bind(&new_event.token_kind)
    .bind(&new_event.action_kind)
    .bind(new_event.workspace_id)
    .bind(new_event.library_id)
    .bind(new_event.document_id)
    .bind(&new_event.status)
    .bind(new_event.error_kind.as_deref())
    .bind(new_event.metadata_json.clone())
    .fetch_one(pool)
    .await
}

/// Lists persisted MCP audit rows for operator review.
///
/// # Errors
/// Returns any `SQLx` error raised while querying `mcp_audit_event` rows.
pub async fn list_mcp_audit_events(
    pool: &PgPool,
    workspace_id: Option<Uuid>,
    token_id: Option<Uuid>,
    limit: i64,
) -> Result<Vec<McpAuditEventRow>, sqlx::Error> {
    let mut builder = QueryBuilder::<Postgres>::new(
        "select id, request_id, token_id, token_kind, action_kind, workspace_id, library_id,
            document_id, status, error_kind, metadata_json, created_at
         from mcp_audit_event
         where true",
    );

    if let Some(workspace_id) = workspace_id {
        builder.push(" and workspace_id = ");
        builder.push_bind(workspace_id);
    }
    if let Some(token_id) = token_id {
        builder.push(" and token_id = ");
        builder.push_bind(token_id);
    }

    builder.push(" order by created_at desc limit ");
    builder.push_bind(limit.max(1));

    builder.build_query_as::<McpAuditEventRow>().fetch_all(pool).await
}

/// Persists one MCP mutation receipt row.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the `mcp_mutation_receipt` row.
pub async fn create_mcp_mutation_receipt(
    pool: &PgPool,
    new_receipt: &NewMcpMutationReceipt,
) -> Result<McpMutationReceiptRow, sqlx::Error> {
    sqlx::query_as::<_, McpMutationReceiptRow>(
        "insert into mcp_mutation_receipt (
            id, token_id, workspace_id, library_id, document_id, operation_kind,
            idempotency_key, payload_identity, runtime_tracking_id, status, failure_kind
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
         returning id, token_id, workspace_id, library_id, document_id, operation_kind,
            idempotency_key, payload_identity, runtime_tracking_id, status, failure_kind,
            accepted_at, last_status_at, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(new_receipt.token_id)
    .bind(new_receipt.workspace_id)
    .bind(new_receipt.library_id)
    .bind(new_receipt.document_id)
    .bind(&new_receipt.operation_kind)
    .bind(&new_receipt.idempotency_key)
    .bind(new_receipt.payload_identity.as_deref())
    .bind(new_receipt.runtime_tracking_id.as_deref())
    .bind(&new_receipt.status)
    .bind(new_receipt.failure_kind.as_deref())
    .fetch_one(pool)
    .await
}

/// Loads one MCP mutation receipt by idempotency scope.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the `mcp_mutation_receipt` row.
pub async fn find_mcp_mutation_receipt_by_idempotency(
    pool: &PgPool,
    token_id: Uuid,
    operation_kind: &str,
    library_id: Uuid,
    document_id: Option<Uuid>,
    idempotency_key: &str,
) -> Result<Option<McpMutationReceiptRow>, sqlx::Error> {
    sqlx::query_as::<_, McpMutationReceiptRow>(
        "select id, token_id, workspace_id, library_id, document_id, operation_kind,
            idempotency_key, payload_identity, runtime_tracking_id, status, failure_kind,
            accepted_at, last_status_at, created_at, updated_at
         from mcp_mutation_receipt
         where token_id = $1
           and operation_kind = $2
           and library_id = $3
           and document_id is not distinct from $4
           and idempotency_key = $5",
    )
    .bind(token_id)
    .bind(operation_kind)
    .bind(library_id)
    .bind(document_id)
    .bind(idempotency_key)
    .fetch_optional(pool)
    .await
}

/// Loads one MCP mutation receipt by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the `mcp_mutation_receipt` row.
pub async fn get_mcp_mutation_receipt_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<McpMutationReceiptRow>, sqlx::Error> {
    sqlx::query_as::<_, McpMutationReceiptRow>(
        "select id, token_id, workspace_id, library_id, document_id, operation_kind,
            idempotency_key, payload_identity, runtime_tracking_id, status, failure_kind,
            accepted_at, last_status_at, created_at, updated_at
         from mcp_mutation_receipt
         where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Loads a model profile by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the `model_profile` row.
pub async fn get_model_profile_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<ModelProfileRow>, sqlx::Error> {
    sqlx::query_as::<_, ModelProfileRow>(
        "select id, workspace_id, provider_account_id, profile_kind, model_name, temperature, max_output_tokens, created_at, updated_at
         from model_profile where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Loads a provider account by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the `provider_account` row.
pub async fn get_provider_account_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<ProviderAccountRow>, sqlx::Error> {
    sqlx::query_as::<_, ProviderAccountRow>(
        "select id, workspace_id, provider_kind, label, api_base_url, status, created_at, updated_at
         from provider_account where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct UsageEventRow {
    pub id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub provider_account_id: Option<Uuid>,
    pub model_profile_id: Option<Uuid>,
    pub usage_kind: String,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub total_tokens: Option<i32>,
    pub raw_usage_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewUsageEvent {
    pub workspace_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub provider_account_id: Option<Uuid>,
    pub model_profile_id: Option<Uuid>,
    pub usage_kind: String,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub total_tokens: Option<i32>,
    pub raw_usage_json: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct CostLedgerRow {
    pub id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub usage_event_id: Uuid,
    pub provider_kind: String,
    pub model_name: String,
    pub currency: String,
    pub estimated_cost: rust_decimal::Decimal,
    pub pricing_snapshot_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

/// Creates a persisted usage event row for token/cost accounting.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the `usage_event` row.
pub async fn create_usage_event(
    pool: &PgPool,
    new_event: &NewUsageEvent,
) -> Result<UsageEventRow, sqlx::Error> {
    sqlx::query_as::<_, UsageEventRow>(
        "insert into usage_event (id, workspace_id, project_id, provider_account_id, model_profile_id, usage_kind, prompt_tokens, completion_tokens, total_tokens, raw_usage_json)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         returning id, workspace_id, project_id, provider_account_id, model_profile_id, usage_kind, prompt_tokens, completion_tokens, total_tokens, raw_usage_json, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(new_event.workspace_id)
    .bind(new_event.project_id)
    .bind(new_event.provider_account_id)
    .bind(new_event.model_profile_id)
    .bind(&new_event.usage_kind)
    .bind(new_event.prompt_tokens)
    .bind(new_event.completion_tokens)
    .bind(new_event.total_tokens)
    .bind(new_event.raw_usage_json.clone())
    .fetch_one(pool)
    .await
}

/// Creates a persisted cost ledger row linked to a usage event.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the `cost_ledger` row.
pub async fn create_cost_ledger(
    pool: &PgPool,
    workspace_id: Option<Uuid>,
    project_id: Option<Uuid>,
    usage_event_id: Uuid,
    provider_kind: &str,
    model_name: &str,
    estimated_cost: rust_decimal::Decimal,
    pricing_snapshot_json: serde_json::Value,
) -> Result<CostLedgerRow, sqlx::Error> {
    sqlx::query_as::<_, CostLedgerRow>(
        "insert into cost_ledger (id, workspace_id, project_id, usage_event_id, provider_kind, model_name, estimated_cost, pricing_snapshot_json)
         values ($1, $2, $3, $4, $5, $6, $7, $8)
         returning id, workspace_id, project_id, usage_event_id, provider_kind, model_name, currency, estimated_cost, pricing_snapshot_json, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind(project_id)
    .bind(usage_event_id)
    .bind(provider_kind)
    .bind(model_name)
    .bind(estimated_cost)
    .bind(pricing_snapshot_json)
    .fetch_one(pool)
    .await
}

/// Loads a project by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the `project` row.
pub async fn get_project_by_id(pool: &PgPool, id: Uuid) -> Result<Option<ProjectRow>, sqlx::Error> {
    sqlx::query_as::<_, ProjectRow>(
        "select id, workspace_id, slug, name, description, created_at, updated_at from project where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Advances one library's dedicated source-truth version and returns the new value.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the `catalog_library` row.
pub async fn touch_project_source_truth_version(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "update catalog_library
         set source_truth_version = greatest(
                coalesce(source_truth_version, 0) + 1,
                (extract(epoch from clock_timestamp()) * 1000000)::bigint
             )
         where id = $1
         returning source_truth_version",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await
    .map(|version| version.max(1))
}

/// Loads a workspace by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the `workspace` row.
pub async fn get_workspace_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<WorkspaceRow>, sqlx::Error> {
    sqlx::query_as::<_, WorkspaceRow>(
        "select id, slug, name, status, created_at, updated_at from workspace where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct VisibleLibraryWithCountsRow {
    pub library_id: Uuid,
    pub workspace_id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub document_count: i64,
    pub readable_document_count: i64,
    pub processing_document_count: i64,
    pub failed_document_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct DocumentMemorySearchHitRow {
    pub document_id: Uuid,
    pub library_id: Uuid,
    pub workspace_id: Uuid,
    pub document_title: Option<String>,
    pub external_key: String,
    pub latest_revision_id: Option<Uuid>,
    pub chunk_match_count: i64,
    pub excerpt: Option<String>,
    pub excerpt_start_offset: Option<i64>,
    pub excerpt_end_offset: Option<i64>,
    pub readability_state: String,
    pub status_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct LatestReadableRuntimeDocumentStateRow {
    pub document_id: Uuid,
    pub library_id: Uuid,
    pub workspace_id: Uuid,
    pub latest_revision_id: Option<Uuid>,
    pub ingestion_run_id: Option<Uuid>,
    pub runtime_status: Option<String>,
    pub readability_state: String,
    pub status_reason: Option<String>,
    pub content_text: Option<String>,
    pub content_char_count: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeDocumentReadSliceRow {
    pub document_id: Uuid,
    pub library_id: Uuid,
    pub workspace_id: Uuid,
    pub latest_revision_id: Option<Uuid>,
    pub ingestion_run_id: Uuid,
    pub content: String,
    pub slice_start_offset: i64,
    pub slice_end_offset: i64,
    pub total_content_length: i64,
}

/// Lists libraries in one workspace with document readiness counters.
///
/// # Errors
/// Returns any `SQLx` error raised while querying aggregated library counts.
pub async fn list_visible_libraries_with_counts(
    pool: &PgPool,
    workspace_id: Uuid,
) -> Result<Vec<VisibleLibraryWithCountsRow>, sqlx::Error> {
    sqlx::query_as::<_, VisibleLibraryWithCountsRow>(
        "select p.id as library_id,
                p.workspace_id,
                p.slug,
                p.name,
                p.description,
                count(d.id)::bigint as document_count,
                count(d.id) filter (
                    where latest_run.id is not null
                      and extracted.content_text is not null
                      and btrim(extracted.content_text) <> ''
                )::bigint as readable_document_count,
                count(d.id) filter (
                    where latest_run.id is not null
                      and latest_run.status <> 'failed'
                      and (
                            extracted.content_text is null
                            or btrim(extracted.content_text) = ''
                          )
                )::bigint as processing_document_count,
                count(d.id) filter (
                    where latest_run.status = 'failed'
                      and (
                            extracted.content_text is null
                            or btrim(extracted.content_text) = ''
                          )
                )::bigint as failed_document_count
         from project p
         left join document d
           on d.project_id = p.id
          and d.deleted_at is null
         left join lateral (
            select rir.id, rir.status
            from runtime_ingestion_run rir
            where rir.project_id = p.id
              and rir.document_id = d.id
            order by rir.created_at desc
            limit 1
         ) latest_run on true
         left join runtime_extracted_content extracted
           on extracted.ingestion_run_id = latest_run.id
         where p.workspace_id = $1
         group by p.id, p.workspace_id, p.slug, p.name, p.description, p.created_at
         order by p.created_at asc, p.name asc",
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await
}

/// Searches document memory across one or more library scopes and aggregates chunk matches.
///
/// # Errors
/// Returns any `SQLx` error raised while querying document-level memory hits.
pub async fn search_document_memory_by_library_scope(
    pool: &PgPool,
    library_ids: &[Uuid],
    query_text: &str,
    limit: i64,
) -> Result<Vec<DocumentMemorySearchHitRow>, sqlx::Error> {
    if library_ids.is_empty() {
        return Ok(Vec::new());
    }

    let pattern = format!("%{query_text}%");
    sqlx::query_as::<_, DocumentMemorySearchHitRow>(
        "with latest_run as (
            select distinct on (rir.document_id)
                   rir.document_id,
                   rir.id,
                   rir.revision_id,
                   rir.status,
                   rir.latest_error_message
            from runtime_ingestion_run rir
            where rir.project_id = any($1)
              and rir.document_id is not null
            order by rir.document_id, rir.created_at desc
         ),
         latest_state as (
            select d.id as document_id,
                   d.project_id as library_id,
                   p.workspace_id,
                   d.title as document_title,
                   d.external_key,
                   coalesce(lr.revision_id, d.current_revision_id) as latest_revision_id,
                   lr.id as ingestion_run_id,
                   lr.status,
                   lr.latest_error_message,
                   nullif(btrim(extracted.content_text), '') as content_text
            from document d
            join project p
              on p.id = d.project_id
            left join latest_run lr
              on lr.document_id = d.id
            left join runtime_extracted_content extracted
              on extracted.ingestion_run_id = lr.id
            where d.project_id = any($1)
              and d.deleted_at is null
         ),
         readable_matches as (
            select ls.document_id,
                   ls.library_id,
                   greatest(
                       (
                           char_length(lower(ls.content_text))
                           - char_length(replace(lower(ls.content_text), lower($3), ''))
                       ) / greatest(char_length($3), 1),
                       1
                   )::bigint as match_count,
                   nullif(strpos(lower(ls.content_text), lower($3)), 0) as match_pos
            from latest_state ls
            where ls.content_text is not null
              and ls.content_text ilike $2
         ),
         fallback_chunk_matches as (
            select c.document_id,
                   c.project_id as library_id,
                   count(*)::bigint as match_count,
                   (array_agg(c.content order by c.ordinal asc))[1] as first_chunk_excerpt
            from chunk c
            join latest_state ls
              on ls.document_id = c.document_id
             and ls.library_id = c.project_id
            where c.project_id = any($1)
              and c.content ilike $2
              and ls.content_text is null
            group by c.document_id, c.project_id
         ),
         matched_documents as (
            select rm.document_id,
                   rm.library_id,
                   rm.match_count,
                   rm.match_pos,
                   null::text as first_chunk_excerpt
            from readable_matches rm
            union all
            select cm.document_id,
                   cm.library_id,
                   cm.match_count,
                   null::integer as match_pos,
                   cm.first_chunk_excerpt
            from fallback_chunk_matches cm
         )
         select ls.document_id,
                ls.library_id,
                ls.workspace_id,
                ls.document_title,
                ls.external_key,
                ls.latest_revision_id,
                md.match_count as chunk_match_count,
                case
                    when ls.content_text is not null
                    then substring(
                        ls.content_text
                        from greatest(coalesce(md.match_pos, 1), 1)
                        for 320
                    )
                    else md.first_chunk_excerpt
                end as excerpt,
                case
                    when ls.content_text is not null
                     and md.match_pos is not null
                    then (md.match_pos - 1)::bigint
                    else null
                end as excerpt_start_offset,
                case
                    when ls.content_text is not null
                     and md.match_pos is not null
                    then (
                        md.match_pos
                        - 1
                        + char_length($3)
                    )::bigint
                    else null
                end as excerpt_end_offset,
                case
                    when ls.content_text is not null then 'readable'
                    when ls.status = 'failed' then 'failed'
                    when ls.ingestion_run_id is not null then 'processing'
                    else 'unavailable'
                end as readability_state,
                case
                    when ls.ingestion_run_id is null then 'document has no runtime ingestion state yet'
                    when ls.content_text is not null then null
                    when ls.status = 'failed' then coalesce(ls.latest_error_message, 'document ingestion failed')
                    when ls.status in ('ready', 'ready_no_graph')
                    then 'document finished without normalized extracted text'
                    else 'document is still being processed'
                end as status_reason
         from matched_documents md
         join latest_state ls
           on ls.document_id = md.document_id
          and ls.library_id = md.library_id
         order by md.match_count desc, ls.document_id desc
         limit $4",
    )
    .bind(library_ids)
    .bind(pattern)
    .bind(query_text)
    .bind(limit.max(1))
    .fetch_all(pool)
    .await
}

/// Resolves the latest readable state projection for one logical document.
///
/// # Errors
/// Returns any `SQLx` error raised while querying document runtime state.
pub async fn get_latest_readable_runtime_document_state(
    pool: &PgPool,
    document_id: Uuid,
) -> Result<Option<LatestReadableRuntimeDocumentStateRow>, sqlx::Error> {
    sqlx::query_as::<_, LatestReadableRuntimeDocumentStateRow>(
        "select d.id as document_id,
                d.project_id as library_id,
                p.workspace_id,
                coalesce(latest_run.revision_id, d.current_revision_id) as latest_revision_id,
                latest_run.id as ingestion_run_id,
                latest_run.status as runtime_status,
                case
                    when extracted.content_text is not null
                     and btrim(extracted.content_text) <> ''
                    then 'readable'
                    when latest_run.status = 'failed' then 'failed'
                    when latest_run.id is not null then 'processing'
                    else 'unavailable'
                end as readability_state,
                case
                    when latest_run.id is null then 'document has no runtime ingestion state yet'
                    when extracted.content_text is not null
                     and btrim(extracted.content_text) <> ''
                    then null
                    when latest_run.status = 'failed' then coalesce(latest_run.latest_error_message, 'document ingestion failed')
                    when latest_run.status in ('ready', 'ready_no_graph')
                     and (extracted.content_text is null or btrim(extracted.content_text) = '')
                    then 'document finished without normalized extracted text'
                    else 'document is still being processed'
                end as status_reason,
                nullif(btrim(extracted.content_text), '') as content_text,
                extracted.char_count as content_char_count
         from document d
         join project p
           on p.id = d.project_id
         left join lateral (
            select rir.id, rir.revision_id, rir.status, rir.latest_error_message
            from runtime_ingestion_run rir
            where rir.project_id = d.project_id
              and rir.document_id = d.id
            order by rir.created_at desc
            limit 1
         ) latest_run on true
         left join runtime_extracted_content extracted
           on extracted.ingestion_run_id = latest_run.id
         where d.id = $1
           and d.deleted_at is null",
    )
    .bind(document_id)
    .fetch_optional(pool)
    .await
}

/// Loads one normalized read window from the latest readable text for a document.
///
/// # Errors
/// Returns any `SQLx` error raised while querying document runtime state.
pub async fn load_runtime_document_read_slice(
    pool: &PgPool,
    document_id: Uuid,
    start_offset: usize,
    requested_length: usize,
) -> Result<Option<RuntimeDocumentReadSliceRow>, sqlx::Error> {
    let Some(state) = get_latest_readable_runtime_document_state(pool, document_id).await? else {
        return Ok(None);
    };
    if state.readability_state != "readable" {
        return Ok(None);
    }

    let Some(ingestion_run_id) = state.ingestion_run_id else {
        return Ok(None);
    };
    let Some(content_text) = state.content_text else {
        return Ok(None);
    };

    let total_content_length = content_text.chars().count();
    let bounded_start = start_offset.min(total_content_length);
    let bounded_length = requested_length.max(1);
    let slice_content = slice_text_by_chars(&content_text, bounded_start, bounded_length);
    let slice_end_offset = bounded_start.saturating_add(slice_content.chars().count());

    Ok(Some(RuntimeDocumentReadSliceRow {
        document_id: state.document_id,
        library_id: state.library_id,
        workspace_id: state.workspace_id,
        latest_revision_id: state.latest_revision_id,
        ingestion_run_id,
        content: slice_content,
        slice_start_offset: i64::try_from(bounded_start).unwrap_or(i64::MAX),
        slice_end_offset: i64::try_from(slice_end_offset).unwrap_or(i64::MAX),
        total_content_length: i64::try_from(total_content_length).unwrap_or(i64::MAX),
    }))
}

fn slice_text_by_chars(content: &str, start_offset: usize, requested_length: usize) -> String {
    content.chars().skip(start_offset).take(requested_length).collect()
}

/// Loads an ingestion job by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the `ingestion_job` row.
pub async fn get_ingestion_job_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<IngestionJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestionJobRow>(&format!(
        "select {INGESTION_JOB_COLUMNS}
         from ingestion_job where id = $1",
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Deletes an ingestion job by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while deleting the `ingestion_job` row.
pub async fn delete_ingestion_job_by_id(pool: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let result =
        sqlx::query("delete from ingestion_job where id = $1").bind(id).execute(pool).await?;
    Ok(result.rows_affected() > 0)
}

/// Deletes ingestion jobs linked to one runtime ingestion run through payload metadata.
///
/// # Errors
/// Returns any `SQLx` error raised while deleting matching ingestion jobs.
pub async fn delete_ingestion_jobs_by_runtime_ingestion_run_id(
    pool: &PgPool,
    runtime_ingestion_run_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "delete from ingestion_job
         where payload_json ->> 'runtime_ingestion_run_id' = $1::text",
    )
    .bind(runtime_ingestion_run_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Lists ingestion jobs linked to one runtime ingestion run through payload metadata.
///
/// # Errors
/// Returns any `SQLx` error raised while loading matching ingestion jobs.
pub async fn list_ingestion_jobs_by_runtime_ingestion_run_id(
    pool: &PgPool,
    runtime_ingestion_run_id: Uuid,
) -> Result<Vec<IngestionJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestionJobRow>(&format!(
        "select {INGESTION_JOB_COLUMNS}
         from ingestion_job
         where payload_json ->> 'runtime_ingestion_run_id' = $1::text
         order by created_at asc, id asc",
    ))
    .bind(runtime_ingestion_run_id)
    .fetch_all(pool)
    .await
}

/// Parses one persisted ingestion execution payload.
///
/// # Errors
/// Returns any `serde_json` error raised while decoding `payload_json`.
pub fn parse_ingestion_execution_payload(
    row: &IngestionJobRow,
) -> Result<IngestionExecutionPayload, serde_json::Error> {
    serde_json::from_value(row.payload_json.clone())
}

pub async fn record_ingestion_job_attempt_claim(
    pool: &PgPool,
    job_id: Uuid,
    attempt_no: i32,
    worker_id: &str,
    stage: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into ingestion_job_attempt (id, job_id, attempt_no, worker_id, status, stage)
         values ($1, $2, $3, $4, 'running', $5)
         on conflict (job_id, attempt_no) do update
         set worker_id = excluded.worker_id,
             status = excluded.status,
             stage = excluded.stage,
             error_message = null,
             finished_at = null",
    )
    .bind(Uuid::now_v7())
    .bind(job_id)
    .bind(attempt_no)
    .bind(worker_id)
    .bind(stage)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn mark_ingestion_job_attempt_stage(
    pool: &PgPool,
    job_id: Uuid,
    attempt_no: i32,
    worker_id: &str,
    status: &str,
    stage: &str,
    error_message: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update ingestion_job_attempt
         set worker_id = $4,
             status = $5,
             stage = $6,
             error_message = $7
         where job_id = $1 and attempt_no = $2 and (worker_id = $3 or worker_id is null)",
    )
    .bind(job_id)
    .bind(attempt_no)
    .bind(worker_id)
    .bind(worker_id)
    .bind(status)
    .bind(stage)
    .bind(error_message)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn complete_ingestion_job_attempt(
    pool: &PgPool,
    job_id: Uuid,
    attempt_no: i32,
    worker_id: &str,
    stage: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update ingestion_job_attempt
         set worker_id = $4,
             status = 'completed',
             stage = $5,
             error_message = null,
             finished_at = now()
         where job_id = $1 and attempt_no = $2 and (worker_id = $3 or worker_id is null)",
    )
    .bind(job_id)
    .bind(attempt_no)
    .bind(worker_id)
    .bind(worker_id)
    .bind(stage)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn fail_ingestion_job_attempt(
    pool: &PgPool,
    job_id: Uuid,
    attempt_no: i32,
    worker_id: &str,
    stage: &str,
    error_message: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update ingestion_job_attempt
         set worker_id = $4,
             status = 'retryable_failed',
             stage = $5,
             error_message = $6,
             finished_at = now()
         where job_id = $1 and attempt_no = $2 and (worker_id = $3 or worker_id is null)",
    )
    .bind(job_id)
    .bind(attempt_no)
    .bind(worker_id)
    .bind(worker_id)
    .bind(stage)
    .bind(error_message)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_ingestion_job_attempts(
    pool: &PgPool,
    job_id: Uuid,
) -> Result<Vec<IngestionJobAttemptRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestionJobAttemptRow>(
        "select id, job_id, attempt_no, worker_id, status, stage, error_message, started_at, finished_at, created_at
         from ingestion_job_attempt
         where job_id = $1
         order by attempt_no desc, created_at desc",
    )
    .bind(job_id)
    .fetch_all(pool)
    .await
}

pub async fn recover_expired_ingestion_job_leases(
    pool: &PgPool,
) -> Result<Vec<RecoveredIngestionJobRow>, sqlx::Error> {
    sqlx::query_as::<_, RecoveredIngestionJobRow>(&format!(
        "with recovered as (
            select {INGESTION_JOB_COLUMNS}
            from ingestion_job
            where status = 'running'
              and lease_expires_at is not null
              and lease_expires_at < now()
            for update
         ),
         updated as (
            update ingestion_job as job
            set status = 'queued',
                stage = 'requeued_after_lease_expiry',
                worker_id = null,
                lease_expires_at = null,
                error_message = null,
                updated_at = now()
            from recovered
            where job.id = recovered.id
            returning {job_columns}
         )
         select {recovered_columns}
         from updated
         join recovered on recovered.id = updated.id",
        job_columns = ingestion_job_columns_for_alias("job"),
        recovered_columns = recovered_ingestion_job_columns("updated", "recovered"),
    ))
    .fetch_all(pool)
    .await
}

pub async fn recover_stale_ingestion_job_heartbeats(
    pool: &PgPool,
    stale_before: DateTime<Utc>,
) -> Result<Vec<RecoveredIngestionJobRow>, sqlx::Error> {
    sqlx::query_as::<_, RecoveredIngestionJobRow>(&format!(
        "with recovered as (
            select {INGESTION_JOB_COLUMNS}
            from ingestion_job
            where status = 'running'
              and finished_at is null
              and (
                    (heartbeat_at is not null and heartbeat_at < $1)
                    or (heartbeat_at is null and updated_at < $1)
               )
            for update
         ),
         updated as (
            update ingestion_job as job
            set status = 'queued',
                stage = 'requeued_after_stale_heartbeat',
                worker_id = null,
                lease_expires_at = null,
                error_message = null,
                updated_at = now()
            from recovered
            where job.id = recovered.id
            returning {job_columns}
         )
         select {recovered_columns}
         from updated
         join recovered on recovered.id = updated.id",
        job_columns = ingestion_job_columns_for_alias("job"),
        recovered_columns = recovered_ingestion_job_columns("updated", "recovered"),
    ))
    .bind(stale_before)
    .fetch_all(pool)
    .await
}

pub async fn claim_next_ingestion_job(
    pool: &PgPool,
    worker_id: &str,
    lease_duration: chrono::Duration,
    total_worker_slots: usize,
    minimum_slice_capacity: usize,
) -> Result<Option<IngestionJobRow>, sqlx::Error> {
    let lease_expires_at = Utc::now() + lease_duration;
    let total_worker_slots = i64::try_from(total_worker_slots.max(1)).unwrap_or(i64::MAX);
    let minimum_slice_capacity = i64::try_from(minimum_slice_capacity.max(1)).unwrap_or(i64::MAX);
    // Keep the isolated slice for interactive MCP mutations or completely quiet libraries.
    // Retries from an already-running library must not consume the final slice, otherwise
    // fresh agent memory writes can be starved behind recovery/backfill pressure.
    let claimed = sqlx::query_as::<_, IngestionJobRow>(&format!(
        "with limits as (
            select
                greatest($3 - $4, 1::bigint) as general_capacity,
                greatest($3, 1::bigint) as total_capacity
         ),
         running_project_load as (
            select project_id,
                   count(*) filter (
                       where status = 'running'
                         and finished_at is null
                   ) as running_count,
                   max(coalesce(heartbeat_at, updated_at, created_at)) filter (
                       where status = 'running'
                         and finished_at is null
                   ) as last_running_activity_at
            from ingestion_job
            group by project_id
         ),
         running_global_load as (
            select count(*) filter (
                       where status = 'running'
                         and finished_at is null
                   ) as running_count
            from ingestion_job
         ),
         running_workspace_load as (
            select project.workspace_id,
                   count(*) filter (
                       where job.status = 'running'
                         and job.finished_at is null
                   ) as running_count,
                   max(coalesce(job.heartbeat_at, job.updated_at, job.created_at)) filter (
                       where job.status = 'running'
                         and job.finished_at is null
                   ) as last_running_activity_at
            from ingestion_job as job
            join project on project.id = job.project_id
            group by project.workspace_id
         ),
         candidate as (
            select job.id
            from ingestion_job as job
            join project on project.id = job.project_id
            cross join limits
            cross join running_global_load
            left join running_project_load as project_load
              on project_load.project_id = job.project_id
            left join running_workspace_load as workspace_load
              on workspace_load.workspace_id = project.workspace_id
            where job.status = 'queued'
              and (job.lease_expires_at is null or job.lease_expires_at < now())
              and (
                    coalesce(running_global_load.running_count, 0) < limits.general_capacity
                    or (
                        coalesce(running_global_load.running_count, 0) < limits.total_capacity
                        and (
                            job.trigger_kind in ('mcp_upload', 'mcp_append', 'mcp_replace')
                            or coalesce(project_load.running_count, 0) = 0
                        )
                    )
              )
            order by
                case
                    when job.trigger_kind in ('mcp_upload', 'mcp_append', 'mcp_replace') then 0
                    when job.attempt_count > 0 then 1
                    else 2
                end asc,
                job.attempt_count desc,
                case
                    when coalesce(running_global_load.running_count, 0) >= limits.general_capacity
                     and coalesce(project_load.running_count, 0) = 0
                        then 0
                    else 1
                end asc,
                coalesce(workspace_load.running_count, 0) asc,
                coalesce(project_load.running_count, 0) asc,
                coalesce(
                    workspace_load.last_running_activity_at,
                    '-infinity'::timestamptz
                ) asc,
                coalesce(
                    project_load.last_running_activity_at,
                    '-infinity'::timestamptz
                ) asc,
                job.created_at asc,
                job.id asc
            limit 1
            for update of job skip locked
         )
         update ingestion_job as job
         set status = 'running',
             stage = case
                 when job.attempt_count = 0 then 'claimed'
                 else 'reclaimed_after_lease_expiry'
             end,
             started_at = coalesce(job.started_at, now()),
             finished_at = null,
             updated_at = now(),
             attempt_count = job.attempt_count + 1,
             worker_id = $1,
             lease_expires_at = $2,
             heartbeat_at = now()
         from candidate
         where job.id = candidate.id
         returning {job_columns}",
        job_columns = ingestion_job_columns_for_alias("job"),
    ))
    .bind(worker_id)
    .bind(lease_expires_at)
    .bind(total_worker_slots)
    .bind(minimum_slice_capacity)
    .fetch_optional(pool)
    .await?;

    if let Some(job) = &claimed {
        record_ingestion_job_attempt_claim(pool, job.id, job.attempt_count, worker_id, &job.stage)
            .await?;
    }

    Ok(claimed)
}

pub async fn mark_ingestion_job_stage(
    pool: &PgPool,
    job_id: Uuid,
    worker_id: &str,
    status: &str,
    stage: &str,
    error_message: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update ingestion_job
         set status = $2,
             stage = $3,
             error_message = $4,
             worker_id = $5,
             heartbeat_at = now(),
             updated_at = now()
         where id = $1",
    )
    .bind(job_id)
    .bind(status)
    .bind(stage)
    .bind(error_message)
    .bind(worker_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn renew_ingestion_job_lease(
    pool: &PgPool,
    job_id: Uuid,
    worker_id: &str,
    lease_duration: chrono::Duration,
    min_write_interval_seconds: i64,
) -> Result<LeaseRenewalOutcome, sqlx::Error> {
    let lease_expires_at = Utc::now() + lease_duration;
    let updated = sqlx::query(
        "with candidate as (
            select id
            from ingestion_job
            where id = $1
              and worker_id = $2
              and status = 'running'
              and finished_at is null
              and (lease_expires_at is null or lease_expires_at >= now() - interval '5 minutes')
            for update skip locked
         )
         update ingestion_job as job
         set heartbeat_at = case
                when heartbeat_at is null
                  or heartbeat_at <= now() - ($4 * interval '1 second')
                    then now()
                else heartbeat_at
             end,
             lease_expires_at = $3
         from candidate
         where job.id = candidate.id",
    )
    .bind(job_id)
    .bind(worker_id)
    .bind(lease_expires_at)
    .bind(min_write_interval_seconds.max(1))
    .execute(pool)
    .await?;

    if updated.rows_affected() > 0 {
        return Ok(LeaseRenewalOutcome::Renewed);
    }

    let still_owned = sqlx::query_scalar::<_, bool>(
        "select exists(
            select 1
            from ingestion_job
            where id = $1
              and worker_id = $2
              and status = 'running'
              and finished_at is null
              and (lease_expires_at is null or lease_expires_at >= now() - interval '5 minutes')
         )",
    )
    .bind(job_id)
    .bind(worker_id)
    .fetch_one(pool)
    .await?;

    Ok(if still_owned { LeaseRenewalOutcome::Busy } else { LeaseRenewalOutcome::NotOwned })
}

pub async fn complete_ingestion_job(
    pool: &PgPool,
    job_id: Uuid,
    worker_id: &str,
    result_json: serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update ingestion_job
         set status = 'completed',
             stage = 'completed',
             worker_id = $2,
             error_message = null,
             finished_at = now(),
             heartbeat_at = now(),
             lease_expires_at = null,
             result_json = $3,
             updated_at = now()
         where id = $1",
    )
    .bind(job_id)
    .bind(worker_id)
    .bind(result_json)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn fail_ingestion_job(
    pool: &PgPool,
    job_id: Uuid,
    worker_id: &str,
    error_message: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update ingestion_job
         set status = 'retryable_failed',
             stage = 'failed',
             worker_id = $2,
             error_message = $3,
             finished_at = now(),
             heartbeat_at = now(),
             lease_expires_at = null,
             updated_at = now()
         where id = $1",
    )
    .bind(job_id)
    .bind(worker_id)
    .bind(error_message)
    .execute(pool)
    .await?;
    Ok(())
}

/// Lists usage events, optionally filtered by project.
///
/// # Errors
/// Returns any `SQLx` error raised while querying `usage_event` rows.
pub async fn list_usage_events(
    pool: &PgPool,
    project_id: Option<Uuid>,
) -> Result<Vec<UsageEventRow>, sqlx::Error> {
    match project_id {
        Some(project_id) => {
            sqlx::query_as::<_, UsageEventRow>(
                "select id, workspace_id, project_id, provider_account_id, model_profile_id, usage_kind, prompt_tokens, completion_tokens, total_tokens, raw_usage_json, created_at
                 from usage_event where project_id = $1 order by created_at desc",
            )
            .bind(project_id)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, UsageEventRow>(
                "select id, workspace_id, project_id, provider_account_id, model_profile_id, usage_kind, prompt_tokens, completion_tokens, total_tokens, raw_usage_json, created_at
                 from usage_event order by created_at desc",
            )
            .fetch_all(pool)
            .await
        }
    }
}

/// Lists cost ledger rows, optionally filtered by project.
///
/// # Errors
/// Returns any `SQLx` error raised while querying `cost_ledger` rows.
pub async fn list_cost_ledger(
    pool: &PgPool,
    project_id: Option<Uuid>,
) -> Result<Vec<CostLedgerRow>, sqlx::Error> {
    match project_id {
        Some(project_id) => {
            sqlx::query_as::<_, CostLedgerRow>(
                "select id, workspace_id, project_id, usage_event_id, provider_kind, model_name, currency, estimated_cost, pricing_snapshot_json, created_at
                 from cost_ledger where project_id = $1 order by created_at desc",
            )
            .bind(project_id)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, CostLedgerRow>(
                "select id, workspace_id, project_id, usage_event_id, provider_kind, model_name, currency, estimated_cost, pricing_snapshot_json, created_at
                 from cost_ledger order by created_at desc",
            )
            .fetch_all(pool)
            .await
        }
    }
}

/// Loads a usage event by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the `usage_event` row.
pub async fn get_usage_event_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<UsageEventRow>, sqlx::Error> {
    sqlx::query_as::<_, UsageEventRow>(
        "select id, workspace_id, project_id, provider_account_id, model_profile_id, usage_kind, prompt_tokens, completion_tokens, total_tokens, raw_usage_json, created_at
         from usage_event where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Loads a cost ledger row by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the `cost_ledger` row.
pub async fn get_cost_ledger_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<CostLedgerRow>, sqlx::Error> {
    sqlx::query_as::<_, CostLedgerRow>(
        "select id, workspace_id, project_id, usage_event_id, provider_kind, model_name, currency, estimated_cost, pricing_snapshot_json, created_at
         from cost_ledger where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct UsageCostTotalsRow {
    pub usage_events: i64,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub estimated_cost: rust_decimal::Decimal,
}

/// Aggregates usage and estimated cost totals, optionally for one project.
///
/// # Errors
/// Returns any `SQLx` error raised while aggregating usage and cost totals.
pub async fn get_usage_cost_totals(
    pool: &PgPool,
    project_id: Option<Uuid>,
) -> Result<UsageCostTotalsRow, sqlx::Error> {
    match project_id {
        Some(project_id) => {
            sqlx::query_as::<_, UsageCostTotalsRow>(
                "select
                    count(distinct ue.id) as usage_events,
                    sum(ue.prompt_tokens)::bigint as prompt_tokens,
                    sum(ue.completion_tokens)::bigint as completion_tokens,
                    sum(ue.total_tokens)::bigint as total_tokens,
                    coalesce(sum(cl.estimated_cost), 0) as estimated_cost
                 from usage_event ue
                 left join cost_ledger cl on cl.usage_event_id = ue.id
                 where ue.project_id = $1",
            )
            .bind(project_id)
            .fetch_one(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, UsageCostTotalsRow>(
                "select
                    count(distinct ue.id) as usage_events,
                    sum(ue.prompt_tokens)::bigint as prompt_tokens,
                    sum(ue.completion_tokens)::bigint as completion_tokens,
                    sum(ue.total_tokens)::bigint as total_tokens,
                    coalesce(sum(cl.estimated_cost), 0) as estimated_cost
                 from usage_event ue
                 left join cost_ledger cl on cl.usage_event_id = ue.id",
            )
            .fetch_one(pool)
            .await
        }
    }
}

/// Aggregates usage and estimated cost totals for one workspace.
///
/// # Errors
/// Returns any `SQLx` error raised while aggregating usage and cost totals.
pub async fn get_workspace_usage_cost_totals(
    pool: &PgPool,
    workspace_id: Uuid,
) -> Result<UsageCostTotalsRow, sqlx::Error> {
    sqlx::query_as::<_, UsageCostTotalsRow>(
        "select
            count(distinct ue.id) as usage_events,
            sum(ue.prompt_tokens)::bigint as prompt_tokens,
            sum(ue.completion_tokens)::bigint as completion_tokens,
            sum(ue.total_tokens)::bigint as total_tokens,
            coalesce(sum(cl.estimated_cost), 0) as estimated_cost
         from usage_event ue
         left join cost_ledger cl on cl.usage_event_id = ue.id
         where ue.workspace_id = $1",
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeIngestionRunRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub document_id: Option<Uuid>,
    pub revision_id: Option<Uuid>,
    pub upload_batch_id: Option<Uuid>,
    pub track_id: String,
    pub file_name: String,
    pub file_type: String,
    pub mime_type: Option<String>,
    pub file_size_bytes: Option<i64>,
    pub status: String,
    pub current_stage: String,
    pub progress_percent: Option<i32>,
    pub activity_status: String,
    pub last_activity_at: Option<DateTime<Utc>>,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    pub provider_profile_snapshot_json: serde_json::Value,
    pub latest_error_message: Option<String>,
    pub current_attempt_no: i32,
    pub attempt_kind: String,
    pub queue_started_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub queue_elapsed_ms: Option<i64>,
    pub total_elapsed_ms: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeIngestionStageEventRow {
    pub id: Uuid,
    pub ingestion_run_id: Uuid,
    pub attempt_no: i32,
    pub stage: String,
    pub status: String,
    pub message: Option<String>,
    pub metadata_json: serde_json::Value,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub elapsed_ms: Option<i64>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct AttemptStageAccountingRow {
    pub id: Uuid,
    pub ingestion_run_id: Uuid,
    pub stage_event_id: Uuid,
    pub stage: String,
    pub accounting_scope: String,
    pub call_sequence_no: i32,
    pub workspace_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub capability: String,
    pub billing_unit: String,
    pub usage_event_id: Option<Uuid>,
    pub cost_ledger_id: Option<Uuid>,
    pub pricing_catalog_entry_id: Option<Uuid>,
    pub pricing_status: String,
    pub estimated_cost: Option<Decimal>,
    pub currency: Option<String>,
    pub token_usage_json: serde_json::Value,
    pub pricing_snapshot_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct AttemptStageCostSummaryRow {
    pub ingestion_run_id: Uuid,
    pub total_estimated_cost: Option<Decimal>,
    pub settled_estimated_cost: Option<Decimal>,
    pub in_flight_estimated_cost: Option<Decimal>,
    pub currency: Option<String>,
    pub priced_stage_count: i32,
    pub unpriced_stage_count: i32,
    pub in_flight_stage_count: i32,
    pub missing_stage_count: i32,
    pub accounting_status: String,
    pub computed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeCollectionResolvedStageAccountingRow {
    pub ingestion_run_id: Uuid,
    pub file_type: String,
    pub stage: String,
    pub accounting_scope: String,
    pub pricing_status: String,
    pub estimated_cost: Option<Decimal>,
    pub currency: Option<String>,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeCollectionProgressRollupRow {
    pub accepted_count: i64,
    pub content_extracted_count: i64,
    pub chunked_count: i64,
    pub embedded_count: i64,
    pub extracting_graph_count: i64,
    pub graph_ready_count: i64,
    pub ready_count: i64,
    pub failed_count: i64,
    pub queue_backlog_count: i64,
    pub processing_backlog_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeCollectionStageRollupRow {
    pub stage: String,
    pub active_count: i64,
    pub completed_count: i64,
    pub failed_count: i64,
    pub avg_elapsed_ms: Option<i64>,
    pub max_elapsed_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeCollectionFormatRollupRow {
    pub file_type: String,
    pub document_count: i64,
    pub queued_count: i64,
    pub processing_count: i64,
    pub ready_count: i64,
    pub ready_no_graph_count: i64,
    pub failed_count: i64,
    pub content_extracted_count: i64,
    pub chunked_count: i64,
    pub embedded_count: i64,
    pub extracting_graph_count: i64,
    pub graph_ready_count: i64,
    pub avg_queue_elapsed_ms: Option<i64>,
    pub max_queue_elapsed_ms: Option<i64>,
    pub avg_total_elapsed_ms: Option<i64>,
    pub max_total_elapsed_ms: Option<i64>,
    pub bottleneck_stage: Option<String>,
    pub bottleneck_avg_elapsed_ms: Option<i64>,
    pub bottleneck_max_elapsed_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeProviderFailureSnapshotRow {
    pub ingestion_run_id: Uuid,
    pub attempt_no: i32,
    pub provider_failure_class: Option<String>,
    pub request_shape_key: Option<String>,
    pub request_size_bytes: Option<i64>,
    pub upstream_status: Option<String>,
    pub retry_outcome: Option<String>,
    pub computed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphProgressCheckpointRow {
    pub ingestion_run_id: Uuid,
    pub attempt_no: i32,
    pub processed_chunks: i64,
    pub total_chunks: i64,
    pub progress_percent: Option<i32>,
    pub provider_call_count: i64,
    pub avg_call_elapsed_ms: Option<i64>,
    pub avg_chunk_elapsed_ms: Option<i64>,
    pub avg_chars_per_second: Option<f64>,
    pub avg_tokens_per_second: Option<f64>,
    pub last_provider_call_at: Option<DateTime<Utc>>,
    pub next_checkpoint_eta_ms: Option<i64>,
    pub pressure_kind: Option<String>,
    pub provider_failure_class: Option<String>,
    pub request_shape_key: Option<String>,
    pub request_size_bytes: Option<i64>,
    pub upstream_status: Option<String>,
    pub retry_outcome: Option<String>,
    pub computed_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct RuntimeGraphProgressCheckpointInput {
    pub ingestion_run_id: Uuid,
    pub attempt_no: i32,
    pub processed_chunks: i64,
    pub total_chunks: i64,
    pub progress_percent: Option<i32>,
    pub provider_call_count: i64,
    pub avg_call_elapsed_ms: Option<i64>,
    pub avg_chunk_elapsed_ms: Option<i64>,
    pub avg_chars_per_second: Option<f64>,
    pub avg_tokens_per_second: Option<f64>,
    pub last_provider_call_at: Option<DateTime<Utc>>,
    pub next_checkpoint_eta_ms: Option<i64>,
    pub pressure_kind: Option<String>,
    pub computed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeExtractedContentRow {
    pub id: Uuid,
    pub ingestion_run_id: Uuid,
    pub document_id: Option<Uuid>,
    pub extraction_kind: String,
    pub content_text: Option<String>,
    pub page_count: Option<i32>,
    pub char_count: Option<i32>,
    pub extraction_warnings_json: serde_json::Value,
    pub source_map_json: serde_json::Value,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub extraction_version: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphExtractionRecordRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub document_id: Uuid,
    pub chunk_id: Uuid,
    pub provider_kind: String,
    pub model_name: String,
    pub extraction_version: String,
    pub prompt_hash: String,
    pub status: String,
    pub raw_output_json: serde_json::Value,
    pub normalized_output_json: serde_json::Value,
    pub glean_pass_count: i32,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphExtractionResumeStateRow {
    pub ingestion_run_id: Uuid,
    pub chunk_ordinal: i32,
    pub chunk_content_hash: String,
    pub status: String,
    pub last_attempt_no: i32,
    pub replay_count: i32,
    pub resume_hit_count: i32,
    pub downgrade_level: i32,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub prompt_hash: Option<String>,
    pub request_shape_key: Option<String>,
    pub request_size_bytes: Option<i64>,
    pub provider_failure_class: Option<String>,
    pub provider_failure_json: Option<serde_json::Value>,
    pub recovery_summary_json: serde_json::Value,
    pub raw_output_json: serde_json::Value,
    pub normalized_output_json: serde_json::Value,
    pub last_successful_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphExtractionResumeRollupRow {
    pub ingestion_run_id: Uuid,
    pub chunk_count: i64,
    pub ready_chunk_count: i64,
    pub failed_chunk_count: i64,
    pub replayed_chunk_count: i64,
    pub resume_hit_count: i64,
    pub resumed_chunk_count: i64,
    pub max_downgrade_level: i32,
}

#[derive(Debug, Clone)]
pub struct UpsertRuntimeGraphExtractionResumeStateInput {
    pub ingestion_run_id: Uuid,
    pub chunk_ordinal: i32,
    pub chunk_content_hash: String,
    pub status: String,
    pub last_attempt_no: i32,
    pub replay_count: i32,
    pub resume_hit_count: i32,
    pub downgrade_level: i32,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub prompt_hash: Option<String>,
    pub request_shape_key: Option<String>,
    pub request_size_bytes: Option<i64>,
    pub provider_failure_class: Option<String>,
    pub provider_failure_json: Option<serde_json::Value>,
    pub recovery_summary_json: serde_json::Value,
    pub raw_output_json: serde_json::Value,
    pub normalized_output_json: serde_json::Value,
    pub last_successful_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphExtractionRecoveryAttemptRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub project_id: Uuid,
    pub document_id: Uuid,
    pub revision_id: Option<Uuid>,
    pub ingestion_run_id: Option<Uuid>,
    pub attempt_no: i32,
    pub chunk_id: Option<Uuid>,
    pub recovery_kind: String,
    pub trigger_reason: String,
    pub status: String,
    pub raw_issue_summary: Option<String>,
    pub recovered_summary: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct CreateRuntimeGraphExtractionRecoveryAttemptInput {
    pub workspace_id: Uuid,
    pub project_id: Uuid,
    pub document_id: Uuid,
    pub revision_id: Option<Uuid>,
    pub ingestion_run_id: Option<Uuid>,
    pub attempt_no: i32,
    pub chunk_id: Option<Uuid>,
    pub recovery_kind: String,
    pub trigger_reason: String,
    pub status: String,
    pub raw_issue_summary: Option<String>,
    pub recovered_summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct DocumentMutationImpactScopeRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub project_id: Uuid,
    pub document_id: Uuid,
    pub mutation_workflow_id: Uuid,
    pub mutation_kind: String,
    pub source_revision_id: Option<Uuid>,
    pub target_revision_id: Option<Uuid>,
    pub scope_status: String,
    pub confidence_status: String,
    pub affected_node_ids_json: serde_json::Value,
    pub affected_relationship_ids_json: serde_json::Value,
    pub fallback_reason: Option<String>,
    pub detected_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct CreateDocumentMutationImpactScopeInput {
    pub workspace_id: Uuid,
    pub project_id: Uuid,
    pub document_id: Uuid,
    pub mutation_workflow_id: Uuid,
    pub mutation_kind: String,
    pub source_revision_id: Option<Uuid>,
    pub target_revision_id: Option<Uuid>,
    pub scope_status: String,
    pub confidence_status: String,
    pub affected_node_ids_json: serde_json::Value,
    pub affected_relationship_ids_json: serde_json::Value,
    pub fallback_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeDocumentContributionSummaryRow {
    pub document_id: Uuid,
    pub revision_id: Option<Uuid>,
    pub ingestion_run_id: Option<Uuid>,
    pub latest_attempt_no: i32,
    pub chunk_count: Option<i32>,
    pub admitted_graph_node_count: i32,
    pub admitted_graph_edge_count: i32,
    pub filtered_graph_edge_count: i32,
    pub filtered_artifact_count: i32,
    pub computed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphFilteredArtifactRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub ingestion_run_id: Option<Uuid>,
    pub revision_id: Option<Uuid>,
    pub target_kind: String,
    pub candidate_key: String,
    pub source_node_key: Option<String>,
    pub target_node_key: Option<String>,
    pub relation_type: Option<String>,
    pub filter_reason: String,
    pub summary: Option<String>,
    pub metadata_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphConvergenceCountersRow {
    pub queued_document_count: i64,
    pub processing_document_count: i64,
    pub ready_no_graph_count: i64,
    pub pending_update_count: i64,
    pub pending_delete_count: i64,
    pub filtered_artifact_count: i64,
    pub filtered_empty_relation_count: i64,
    pub filtered_degenerate_loop_count: i64,
    pub latest_failed_mutation_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeProviderProfileRow {
    pub project_id: Uuid,
    pub indexing_provider_kind: String,
    pub indexing_model_name: String,
    pub embedding_provider_kind: String,
    pub embedding_model_name: String,
    pub answer_provider_kind: String,
    pub answer_model_name: String,
    pub vision_provider_kind: String,
    pub vision_model_name: String,
    pub last_validated_at: Option<DateTime<Utc>>,
    pub last_validation_status: Option<String>,
    pub last_validation_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeProviderValidationLogRow {
    pub id: Uuid,
    pub project_id: Option<Uuid>,
    pub provider_kind: String,
    pub model_name: String,
    pub capability: String,
    pub status: String,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ModelPricingCatalogEntryRow {
    pub id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub provider_kind: String,
    pub model_name: String,
    pub capability: String,
    pub billing_unit: String,
    pub input_price: Option<Decimal>,
    pub output_price: Option<Decimal>,
    pub currency: String,
    pub status: String,
    pub source_kind: String,
    pub note: Option<String>,
    pub effective_from: DateTime<Utc>,
    pub effective_to: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ModelPricingResolutionRow {
    pub pricing_catalog_entry_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub provider_kind: String,
    pub model_name: String,
    pub capability: String,
    pub billing_unit: String,
    pub input_price: Option<Decimal>,
    pub output_price: Option<Decimal>,
    pub currency: String,
    pub status: String,
    pub source_kind: String,
    pub effective_from: DateTime<Utc>,
    pub effective_to: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewAttemptStageAccounting {
    pub ingestion_run_id: Uuid,
    pub stage_event_id: Uuid,
    pub stage: String,
    pub accounting_scope: String,
    pub call_sequence_no: i32,
    pub workspace_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub capability: String,
    pub billing_unit: String,
    pub usage_event_id: Option<Uuid>,
    pub cost_ledger_id: Option<Uuid>,
    pub pricing_catalog_entry_id: Option<Uuid>,
    pub pricing_status: String,
    pub estimated_cost: Option<Decimal>,
    pub currency: Option<String>,
    pub token_usage_json: serde_json::Value,
    pub pricing_snapshot_json: serde_json::Value,
}

fn sanitize_new_attempt_stage_accounting(
    new_row: &NewAttemptStageAccounting,
) -> Result<NewAttemptStageAccounting, sqlx::Error> {
    let ownership =
        stage_native_ownership(new_row.ingestion_run_id, new_row.stage_event_id, &new_row.stage);
    let mut normalized = new_row.clone();
    if normalized.accounting_scope.trim().is_empty() {
        normalized.accounting_scope = "stage_rollup".to_string();
    }
    match normalized.accounting_scope.as_str() {
        "stage_rollup" => normalized.call_sequence_no = 0,
        "provider_call" => {
            if normalized.call_sequence_no <= 0 {
                return Err(sqlx::Error::Protocol(format!(
                    "provider_call accounting for stage {} must use positive call_sequence_no",
                    normalized.stage
                )));
            }
        }
        other => {
            return Err(sqlx::Error::Protocol(format!(
                "unsupported accounting_scope {} for stage {}",
                other, normalized.stage
            )));
        }
    }
    normalized.token_usage_json =
        decorate_payload_with_stage_ownership(normalized.token_usage_json, &ownership);
    normalized.pricing_snapshot_json =
        decorate_payload_with_stage_ownership(normalized.pricing_snapshot_json, &ownership);

    match runtime_stage_billing_policy(&normalized.stage) {
        RuntimeStageBillingPolicy::Billable { capability, billing_unit } => {
            let expected_capability = pricing_capability_label(&capability);
            let expected_billing_unit = pricing_billing_unit_label(&billing_unit);
            if normalized.capability != expected_capability
                || normalized.billing_unit != expected_billing_unit
            {
                return Err(sqlx::Error::Protocol(format!(
                    "stage accounting ownership mismatch for {}: expected {} / {}, got {} / {}",
                    normalized.stage,
                    expected_capability,
                    expected_billing_unit,
                    normalized.capability,
                    normalized.billing_unit,
                )));
            }
        }
        RuntimeStageBillingPolicy::NonBillable => {
            if normalized.pricing_status.eq_ignore_ascii_case("priced")
                || normalized.estimated_cost.is_some()
                || normalized.cost_ledger_id.is_some()
                || normalized.pricing_catalog_entry_id.is_some()
            {
                return Err(sqlx::Error::Protocol(format!(
                    "non-billable stage {} cannot persist priced accounting artifacts",
                    normalized.stage
                )));
            }
        }
    }

    Ok(normalized)
}

fn pricing_capability_label(value: &PricingCapability) -> &'static str {
    match value {
        PricingCapability::Indexing => "indexing",
        PricingCapability::Embedding => "embedding",
        PricingCapability::Answer => "answer",
        PricingCapability::Vision => "vision",
        PricingCapability::GraphExtract => "graph_extract",
    }
}

fn pricing_billing_unit_label(value: &PricingBillingUnit) -> &'static str {
    match value {
        PricingBillingUnit::Per1MInputTokens => "per_1m_input_tokens",
        PricingBillingUnit::Per1MOutputTokens => "per_1m_output_tokens",
        PricingBillingUnit::Per1MTokens => "per_1m_tokens",
        PricingBillingUnit::FixedPerCall => "fixed_per_call",
    }
}

#[derive(Debug, Clone)]
pub struct NewModelPricingCatalogEntry {
    pub workspace_id: Option<Uuid>,
    pub provider_kind: String,
    pub model_name: String,
    pub capability: String,
    pub billing_unit: String,
    pub input_price: Option<Decimal>,
    pub output_price: Option<Decimal>,
    pub currency: String,
    pub source_kind: String,
    pub note: Option<String>,
    pub effective_from: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct UpdateModelPricingCatalogEntry {
    pub workspace_id: Option<Uuid>,
    pub provider_kind: String,
    pub model_name: String,
    pub capability: String,
    pub billing_unit: String,
    pub input_price: Option<Decimal>,
    pub output_price: Option<Decimal>,
    pub currency: String,
    pub note: Option<String>,
    pub effective_from: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct QueryIntentCacheEntryRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub project_id: Uuid,
    pub normalized_question_hash: String,
    pub explicit_mode: String,
    pub planned_mode: String,
    pub high_level_keywords_json: serde_json::Value,
    pub low_level_keywords_json: serde_json::Value,
    pub intent_summary: Option<String>,
    pub source_truth_version: i64,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeVectorTargetRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub target_kind: String,
    pub target_id: Uuid,
    pub provider_kind: String,
    pub model_name: String,
    pub dimensions: Option<i32>,
    pub embedding_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct RuntimeVectorTargetUpsertInput {
    pub project_id: Uuid,
    pub target_kind: String,
    pub target_id: Uuid,
    pub provider_kind: String,
    pub model_name: String,
    pub dimensions: Option<i32>,
    pub embedding_json: serde_json::Value,
}

#[must_use]
pub fn runtime_queue_waiting_reason_key(value: &RuntimeQueueWaitingReason) -> &'static str {
    match value {
        RuntimeQueueWaitingReason::OrdinaryBacklog => "ordinary_backlog",
        RuntimeQueueWaitingReason::IsolatedCapacityWait => "isolated_capacity_wait",
        RuntimeQueueWaitingReason::Blocked => "blocked",
        RuntimeQueueWaitingReason::Degraded => "degraded",
    }
}

#[must_use]
pub fn parse_runtime_queue_waiting_reason(
    value: Option<&str>,
) -> Option<RuntimeQueueWaitingReason> {
    match value {
        Some("ordinary_backlog") => Some(RuntimeQueueWaitingReason::OrdinaryBacklog),
        Some("isolated_capacity_wait") => Some(RuntimeQueueWaitingReason::IsolatedCapacityWait),
        Some("blocked") => Some(RuntimeQueueWaitingReason::Blocked),
        Some("degraded") => Some(RuntimeQueueWaitingReason::Degraded),
        _ => None,
    }
}

/// Creates a runtime ingestion run.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the runtime ingestion run.
pub async fn create_runtime_ingestion_run(
    pool: &PgPool,
    project_id: Uuid,
    document_id: Option<Uuid>,
    revision_id: Option<Uuid>,
    upload_batch_id: Option<Uuid>,
    track_id: &str,
    file_name: &str,
    file_type: &str,
    mime_type: Option<&str>,
    file_size_bytes: Option<i64>,
    status: &str,
    current_stage: &str,
    attempt_kind: &str,
    provider_profile_snapshot_json: serde_json::Value,
) -> Result<RuntimeIngestionRunRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "insert into runtime_ingestion_run (
            id, project_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, attempt_kind, provider_profile_snapshot_json
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
         returning id, project_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type,
            mime_type, file_size_bytes, status, current_stage, progress_percent,
            activity_status, last_activity_at, last_heartbeat_at,
            provider_profile_snapshot_json, latest_error_message, current_attempt_no, attempt_kind,
            queue_started_at, started_at, finished_at, queue_elapsed_ms, total_elapsed_ms, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(document_id)
    .bind(revision_id)
    .bind(upload_batch_id)
    .bind(track_id)
    .bind(file_name)
    .bind(file_type)
    .bind(mime_type)
    .bind(file_size_bytes)
    .bind(status)
    .bind(current_stage)
    .bind(attempt_kind)
    .bind(provider_profile_snapshot_json)
    .fetch_one(pool)
    .await
}

/// Lists runtime ingestion runs for one project.
///
/// # Errors
/// Returns any `SQLx` error raised while querying runtime ingestion runs.
pub async fn list_runtime_ingestion_runs_by_project(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Vec<RuntimeIngestionRunRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "select id, project_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status,
            last_activity_at, last_heartbeat_at, provider_profile_snapshot_json,
            latest_error_message, current_attempt_no, attempt_kind, queue_started_at, started_at, finished_at,
            queue_elapsed_ms, total_elapsed_ms, created_at, updated_at
         from runtime_ingestion_run
         where project_id = $1
           and (
                document_id is null
                or not exists (
                    select 1
                    from document
                    where document.id = runtime_ingestion_run.document_id
                      and document.deleted_at is not null
                )
           )
         order by created_at desc",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
}

/// Loads one runtime ingestion run by id.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the runtime ingestion run.
pub async fn get_runtime_ingestion_run_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<RuntimeIngestionRunRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "select id, project_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status,
            last_activity_at, last_heartbeat_at, provider_profile_snapshot_json,
            latest_error_message, current_attempt_no, attempt_kind, queue_started_at, started_at, finished_at,
            queue_elapsed_ms, total_elapsed_ms, created_at, updated_at
         from runtime_ingestion_run
         where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Loads one runtime ingestion run by track id.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the runtime ingestion run.
pub async fn get_runtime_ingestion_run_by_track_id(
    pool: &PgPool,
    track_id: &str,
) -> Result<Option<RuntimeIngestionRunRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "select id, project_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status,
            last_activity_at, last_heartbeat_at, provider_profile_snapshot_json,
            latest_error_message, current_attempt_no, attempt_kind, queue_started_at, started_at, finished_at,
            queue_elapsed_ms, total_elapsed_ms, created_at, updated_at
         from runtime_ingestion_run
         where track_id = $1",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
}

/// Deletes one runtime ingestion run by id.
///
/// # Errors
/// Returns any `SQLx` error raised while deleting the runtime ingestion run.
pub async fn delete_runtime_ingestion_run_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("delete from runtime_ingestion_run where id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Appends a runtime ingestion stage event.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the stage event.
pub async fn append_runtime_stage_event(
    pool: &PgPool,
    ingestion_run_id: Uuid,
    attempt_no: i32,
    stage: &str,
    status: &str,
    message: Option<&str>,
    metadata_json: serde_json::Value,
) -> Result<RuntimeIngestionStageEventRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionStageEventRow>(
        "insert into runtime_ingestion_stage_event (
            id, ingestion_run_id, attempt_no, stage, status, message, metadata_json,
            provider_kind, model_name, started_at, finished_at, elapsed_ms
         ) values (
            $1, $2, $3, $4, $5, $6, $7,
            nullif($7 ->> 'provider_kind', ''),
            nullif($7 ->> 'model_name', ''),
            coalesce(($7 ->> 'started_at')::timestamptz, now()),
            ($7 ->> 'finished_at')::timestamptz,
            coalesce(
                ($7 ->> 'elapsed_ms')::bigint,
                case
                    when ($7 ->> 'started_at')::timestamptz is not null
                     and ($7 ->> 'finished_at')::timestamptz is not null
                        then greatest(
                            0,
                            floor(
                                extract(
                                    epoch from (
                                        ($7 ->> 'finished_at')::timestamptz
                                        - ($7 ->> 'started_at')::timestamptz
                                    )
                                ) * 1000
                            )::bigint
                        )
                    else null
                end
            )
         )
         returning id, ingestion_run_id, attempt_no, stage, status, message, metadata_json,
            provider_kind, model_name, started_at, finished_at, elapsed_ms, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(ingestion_run_id)
    .bind(attempt_no)
    .bind(stage)
    .bind(status)
    .bind(message)
    .bind(metadata_json)
    .fetch_one(pool)
    .await
}

/// Lists runtime stage events for one ingestion run.
///
/// # Errors
/// Returns any `SQLx` error raised while querying stage events.
pub async fn list_runtime_stage_events_by_run(
    pool: &PgPool,
    ingestion_run_id: Uuid,
) -> Result<Vec<RuntimeIngestionStageEventRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionStageEventRow>(
        "select id, ingestion_run_id, attempt_no, stage, status, message, metadata_json,
            provider_kind, model_name, started_at, finished_at, elapsed_ms, created_at
         from runtime_ingestion_stage_event
         where ingestion_run_id = $1
         order by created_at asc, id asc",
    )
    .bind(ingestion_run_id)
    .fetch_all(pool)
    .await
}

/// Updates the status, stage, and progress for a runtime ingestion run.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the runtime ingestion run.
pub async fn update_runtime_ingestion_run_status(
    pool: &PgPool,
    id: Uuid,
    status: &str,
    current_stage: &str,
    progress_percent: Option<i32>,
    latest_error_message: Option<&str>,
) -> Result<RuntimeIngestionRunRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "update runtime_ingestion_run
         set status = $2,
             current_stage = $3,
             progress_percent = $4,
             latest_error_message = $5,
             activity_status = case
                 when $2 in ('ready', 'ready_no_graph') then 'ready'
                 when $2 = 'failed' then 'failed'
                 when $2 = 'processing' then 'active'
                 else 'queued'
             end,
             last_activity_at = now(),
             last_heartbeat_at = case when $2 = 'processing' then now() else last_heartbeat_at end,
             updated_at = now(),
             started_at = coalesce(started_at, now()),
             queue_elapsed_ms = case
                 when started_at is null then queue_elapsed_ms
                 else coalesce(queue_elapsed_ms, greatest(0, floor(extract(epoch from (started_at - queue_started_at)) * 1000)::bigint))
             end,
             finished_at = case when $2 in ('ready', 'ready_no_graph', 'failed') then now() else finished_at end,
             total_elapsed_ms = case
                 when $2 in ('ready', 'ready_no_graph', 'failed')
                     then greatest(0, floor(extract(epoch from (now() - queue_started_at)) * 1000)::bigint)
                 else total_elapsed_ms
             end
         where id = $1
         returning id, project_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status,
            last_activity_at, last_heartbeat_at, provider_profile_snapshot_json,
            latest_error_message, current_attempt_no, attempt_kind, queue_started_at, started_at, finished_at,
            queue_elapsed_ms, total_elapsed_ms, created_at, updated_at",
    )
    .bind(id)
    .bind(status)
    .bind(current_stage)
    .bind(progress_percent)
    .bind(latest_error_message)
    .fetch_one(pool)
    .await
}

/// Updates a processing-stage transition in one write so long-running workers do not churn the
/// runtime row with separate status and activity updates.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the runtime ingestion run.
pub async fn update_runtime_ingestion_run_processing_stage(
    pool: &PgPool,
    id: Uuid,
    current_stage: &str,
    progress_percent: Option<i32>,
    last_activity_at: DateTime<Utc>,
    latest_error_message: Option<&str>,
) -> Result<RuntimeIngestionRunRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "update runtime_ingestion_run
         set status = 'processing',
             current_stage = $2,
             progress_percent = $3,
             activity_status = 'active',
             last_activity_at = $4,
             last_heartbeat_at = coalesce(last_heartbeat_at, $4),
             latest_error_message = $5,
             updated_at = now(),
             started_at = coalesce(started_at, $4),
             queue_elapsed_ms = coalesce(
                 queue_elapsed_ms,
                 greatest(0, floor(extract(epoch from (coalesce(started_at, $4) - queue_started_at)) * 1000)::bigint)
             )
         where id = $1
         returning id, project_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status,
            last_activity_at, last_heartbeat_at, provider_profile_snapshot_json,
            latest_error_message, current_attempt_no, attempt_kind, queue_started_at, started_at, finished_at,
            queue_elapsed_ms, total_elapsed_ms, created_at, updated_at",
    )
    .bind(id)
    .bind(current_stage)
    .bind(progress_percent)
    .bind(last_activity_at)
    .bind(latest_error_message)
    .fetch_one(pool)
    .await
}

/// Advances a processing-stage progress checkpoint only when the visible progress marker or
/// activity heartbeat meaningfully changes.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the runtime ingestion run.
pub async fn update_runtime_ingestion_run_processing_stage_checkpoint(
    pool: &PgPool,
    id: Uuid,
    current_stage: &str,
    progress_percent: i32,
    last_activity_at: DateTime<Utc>,
) -> Result<Option<RuntimeIngestionRunRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "update runtime_ingestion_run
         set status = 'processing',
             current_stage = $2,
             progress_percent = greatest(coalesce(progress_percent, $3), $3),
             activity_status = 'active',
             last_activity_at = $4,
             last_heartbeat_at = coalesce(last_heartbeat_at, $4),
             updated_at = now()
         where id = $1
           and status = 'processing'
           and current_stage = $2
           and (
                coalesce(progress_percent, -1) < $3
                or last_activity_at is null
                or last_activity_at <= ($4 - interval '30 seconds')
           )
         returning id, project_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status,
            last_activity_at, last_heartbeat_at, provider_profile_snapshot_json,
            latest_error_message, current_attempt_no, attempt_kind, queue_started_at, started_at, finished_at,
            queue_elapsed_ms, total_elapsed_ms, created_at, updated_at",
    )
    .bind(id)
    .bind(current_stage)
    .bind(progress_percent)
    .bind(last_activity_at)
    .fetch_optional(pool)
    .await
}

/// Marks a runtime ingestion run as claimed by a worker without implying that stage execution has
/// already produced visible processing activity.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the runtime ingestion run.
pub async fn mark_runtime_ingestion_run_claimed(
    pool: &PgPool,
    id: Uuid,
    claimed_at: DateTime<Utc>,
) -> Result<RuntimeIngestionRunRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "update runtime_ingestion_run
         set started_at = coalesce(started_at, $2),
             queue_elapsed_ms = coalesce(
                 queue_elapsed_ms,
                 greatest(0, floor(extract(epoch from (coalesce(started_at, $2) - queue_started_at)) * 1000)::bigint)
             ),
             last_heartbeat_at = coalesce(last_heartbeat_at, $2),
             updated_at = now()
         where id = $1
         returning id, project_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status,
            last_activity_at, last_heartbeat_at, provider_profile_snapshot_json,
            latest_error_message, current_attempt_no, attempt_kind, queue_started_at, started_at, finished_at,
            queue_elapsed_ms, total_elapsed_ms, created_at, updated_at",
    )
    .bind(id)
    .bind(claimed_at)
    .fetch_one(pool)
    .await
}

pub async fn reconcile_processing_runtime_ingestion_runs_with_queued_jobs(
    pool: &PgPool,
) -> Result<Vec<RuntimeIngestionRunRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "with stalled_runs as (
            select
                run.id,
                greatest(
                    run.current_attempt_no,
                    coalesce(max(job.attempt_count), run.current_attempt_no)
                ) as synced_attempt_no,
                (
                    array_agg(job.stage order by job.updated_at desc)
                    filter (where job.status = 'queued')
                )[1] as latest_recovery_stage
            from runtime_ingestion_run as run
            join ingestion_job as job
              on job.payload_json ->> 'runtime_ingestion_run_id' = run.id::text
            where run.status = 'processing'
              and job.status = 'queued'
              and not exists (
                  select 1
                  from ingestion_job as active_job
                  where active_job.payload_json ->> 'runtime_ingestion_run_id' = run.id::text
                    and active_job.status = 'running'
              )
            group by run.id, run.current_attempt_no
         )
         update runtime_ingestion_run as run
         set status = 'queued',
             current_stage = 'accepted',
             progress_percent = null,
             activity_status = case
                 when stalled_runs.latest_recovery_stage in (
                     'requeued_after_lease_expiry',
                     'requeued_after_stale_heartbeat'
                 ) then 'retrying'
                 else 'queued'
             end,
             last_activity_at = now(),
             latest_error_message = case
                 when stalled_runs.latest_recovery_stage = 'requeued_after_lease_expiry'
                    then 'job lease expired before completion; requeued for retry'
                 when stalled_runs.latest_recovery_stage = 'requeued_after_stale_heartbeat'
                    then 'worker heartbeat stalled before completion; requeued for retry'
                 else null
             end,
             current_attempt_no = stalled_runs.synced_attempt_no,
             queue_started_at = now(),
             queue_elapsed_ms = null,
             total_elapsed_ms = null,
             started_at = null,
             finished_at = null,
             updated_at = now()
         from stalled_runs
         where run.id = stalled_runs.id
         returning run.id, run.project_id, run.document_id, run.revision_id, run.upload_batch_id, run.track_id, run.file_name, run.file_type, run.mime_type,
            run.file_size_bytes, run.status, run.current_stage, run.progress_percent,
            run.activity_status, run.last_activity_at, run.last_heartbeat_at, run.provider_profile_snapshot_json,
            run.latest_error_message, run.current_attempt_no, run.attempt_kind, run.queue_started_at, run.started_at, run.finished_at,
            run.queue_elapsed_ms, run.total_elapsed_ms, run.created_at, run.updated_at",
    )
    .fetch_all(pool)
    .await
}

pub async fn reconcile_processing_runtime_ingestion_runs_with_failed_jobs(
    pool: &PgPool,
) -> Result<Vec<RuntimeIngestionRunRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "with stalled_runs as (
            select
                run.id,
                greatest(
                    run.current_attempt_no,
                    coalesce(max(job.attempt_count), run.current_attempt_no)
                ) as synced_attempt_no,
                (
                    array_agg(job.error_message order by job.updated_at desc)
                    filter (where job.status = 'retryable_failed' and job.error_message is not null)
                )[1] as latest_error_message
            from runtime_ingestion_run as run
            join ingestion_job as job
              on job.payload_json ->> 'runtime_ingestion_run_id' = run.id::text
            where run.status = 'processing'
              and job.status = 'retryable_failed'
              and not exists (
                  select 1
                  from ingestion_job as active_job
                  where active_job.payload_json ->> 'runtime_ingestion_run_id' = run.id::text
                    and active_job.status in ('running', 'queued')
              )
            group by run.id, run.current_attempt_no
         )
         update runtime_ingestion_run as run
         set status = 'failed',
             current_stage = 'failed',
             progress_percent = null,
             activity_status = 'failed',
             last_activity_at = now(),
             latest_error_message = coalesce(
                 stalled_runs.latest_error_message,
                 run.latest_error_message,
                 'runtime ingestion attempt failed'
             ),
             current_attempt_no = stalled_runs.synced_attempt_no,
             finished_at = coalesce(run.finished_at, now()),
             total_elapsed_ms = coalesce(
                 run.total_elapsed_ms,
                 greatest(0, floor(extract(epoch from (coalesce(run.finished_at, now()) - run.queue_started_at)) * 1000)::bigint)
             ),
             updated_at = now()
         from stalled_runs
         where run.id = stalled_runs.id
         returning run.id, run.project_id, run.document_id, run.revision_id, run.upload_batch_id, run.track_id, run.file_name, run.file_type, run.mime_type,
            run.file_size_bytes, run.status, run.current_stage, run.progress_percent,
            run.activity_status, run.last_activity_at, run.last_heartbeat_at, run.provider_profile_snapshot_json,
            run.latest_error_message, run.current_attempt_no, run.attempt_kind, run.queue_started_at, run.started_at, run.finished_at,
            run.queue_elapsed_ms, run.total_elapsed_ms, run.created_at, run.updated_at",
    )
    .fetch_all(pool)
    .await
}

/// Resets an existing runtime ingestion run back to the accepted queue state for a new attempt.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the runtime ingestion run.
pub async fn requeue_runtime_ingestion_run(
    pool: &PgPool,
    id: Uuid,
    provider_profile_snapshot_json: serde_json::Value,
) -> Result<RuntimeIngestionRunRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "update runtime_ingestion_run
         set status = 'queued',
             current_stage = 'accepted',
             progress_percent = null,
             activity_status = 'queued',
             last_activity_at = now(),
             last_heartbeat_at = null,
             provider_profile_snapshot_json = $2,
             latest_error_message = null,
             current_attempt_no = current_attempt_no + 1,
             attempt_kind = 'reprocess',
             queue_started_at = now(),
             queue_elapsed_ms = null,
             total_elapsed_ms = null,
             started_at = null,
             finished_at = null,
             updated_at = now()
         where id = $1
         returning id, project_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status,
            last_activity_at, last_heartbeat_at, provider_profile_snapshot_json,
            latest_error_message, current_attempt_no, attempt_kind, queue_started_at, started_at, finished_at,
            queue_elapsed_ms, total_elapsed_ms, created_at, updated_at",
    )
    .bind(id)
    .bind(provider_profile_snapshot_json)
    .fetch_one(pool)
    .await
}

/// Resets a runtime ingestion run for a new revision-aware attempt.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the runtime ingestion run.
pub async fn prepare_runtime_ingestion_run_for_attempt(
    pool: &PgPool,
    id: Uuid,
    revision_id: Option<Uuid>,
    provider_profile_snapshot_json: serde_json::Value,
    attempt_kind: &str,
    file_name: &str,
    file_type: &str,
    mime_type: Option<&str>,
    file_size_bytes: Option<i64>,
) -> Result<RuntimeIngestionRunRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "update runtime_ingestion_run
         set revision_id = $2,
             status = 'queued',
             current_stage = 'accepted',
             progress_percent = null,
             activity_status = 'queued',
             last_activity_at = now(),
             last_heartbeat_at = null,
             provider_profile_snapshot_json = $3,
             latest_error_message = null,
             current_attempt_no = current_attempt_no + 1,
             attempt_kind = $4,
             file_name = $5,
             file_type = $6,
             mime_type = $7,
             file_size_bytes = $8,
             queue_started_at = now(),
             queue_elapsed_ms = null,
             total_elapsed_ms = null,
             started_at = null,
             finished_at = null,
             updated_at = now()
         where id = $1
         returning id, project_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status,
            last_activity_at, last_heartbeat_at, provider_profile_snapshot_json,
            latest_error_message, current_attempt_no, attempt_kind, queue_started_at, started_at, finished_at,
            queue_elapsed_ms, total_elapsed_ms, created_at, updated_at",
    )
    .bind(id)
    .bind(revision_id)
    .bind(provider_profile_snapshot_json)
    .bind(attempt_kind)
    .bind(file_name)
    .bind(file_type)
    .bind(mime_type)
    .bind(file_size_bytes)
    .fetch_one(pool)
    .await
}

/// Attaches the persisted document id to a runtime ingestion run.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the runtime ingestion run.
pub async fn attach_runtime_ingestion_run_document(
    pool: &PgPool,
    id: Uuid,
    document_id: Uuid,
    revision_id: Option<Uuid>,
) -> Result<RuntimeIngestionRunRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "update runtime_ingestion_run
         set document_id = $2,
             revision_id = $3,
             updated_at = now()
         where id = $1
         returning id, project_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status,
            last_activity_at, last_heartbeat_at, provider_profile_snapshot_json,
            latest_error_message, current_attempt_no, attempt_kind, queue_started_at, started_at, finished_at,
            queue_elapsed_ms, total_elapsed_ms, created_at, updated_at",
    )
    .bind(id)
    .bind(document_id)
    .bind(revision_id)
    .fetch_one(pool)
    .await
}

/// Updates activity timestamps and the visible activity state for the active runtime ingestion run.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the runtime ingestion run.
pub async fn update_runtime_ingestion_run_activity(
    pool: &PgPool,
    id: Uuid,
    activity_status: &str,
    last_activity_at: DateTime<Utc>,
    last_heartbeat_at: Option<DateTime<Utc>>,
) -> Result<RuntimeIngestionRunRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "update runtime_ingestion_run
         set activity_status = $2,
             last_activity_at = $3,
             last_heartbeat_at = coalesce($4, last_heartbeat_at),
             updated_at = now()
         where id = $1
         returning id, project_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status, last_activity_at,
            last_heartbeat_at, provider_profile_snapshot_json, latest_error_message, current_attempt_no,
            attempt_kind, queue_started_at, started_at, finished_at, queue_elapsed_ms, total_elapsed_ms,
            created_at, updated_at",
    )
    .bind(id)
    .bind(activity_status)
    .bind(last_activity_at)
    .bind(last_heartbeat_at)
    .fetch_one(pool)
    .await
}

/// Updates activity state alongside the visible stage transition for the active runtime ingestion run.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the runtime ingestion run.
pub async fn update_runtime_ingestion_run_stage_activity(
    pool: &PgPool,
    id: Uuid,
    current_stage: &str,
    progress_percent: Option<i32>,
    activity_status: &str,
    last_activity_at: DateTime<Utc>,
    latest_error_message: Option<&str>,
) -> Result<RuntimeIngestionRunRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "update runtime_ingestion_run
         set current_stage = $2,
             progress_percent = $3,
             activity_status = $4,
             last_activity_at = $5,
             latest_error_message = $6,
             updated_at = now()
         where id = $1
         returning id, project_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status, last_activity_at,
            last_heartbeat_at, provider_profile_snapshot_json, latest_error_message, current_attempt_no,
            attempt_kind, queue_started_at, started_at, finished_at, queue_elapsed_ms, total_elapsed_ms,
            created_at, updated_at",
    )
    .bind(id)
    .bind(current_stage)
    .bind(progress_percent)
    .bind(activity_status)
    .bind(last_activity_at)
    .bind(latest_error_message)
    .fetch_one(pool)
    .await
}

/// Updates a queued runtime ingestion run without stamping synthetic visible activity.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the runtime ingestion run.
pub async fn update_runtime_ingestion_run_queued_stage(
    pool: &PgPool,
    id: Uuid,
    current_stage: &str,
    progress_percent: Option<i32>,
    activity_status: &str,
    latest_error_message: Option<&str>,
) -> Result<RuntimeIngestionRunRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "update runtime_ingestion_run
         set current_stage = $2,
             progress_percent = $3,
             activity_status = $4,
             latest_error_message = $5,
             updated_at = now()
         where id = $1
         returning id, project_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status, last_activity_at,
            last_heartbeat_at, provider_profile_snapshot_json, latest_error_message, current_attempt_no,
            attempt_kind, queue_started_at, started_at, finished_at, queue_elapsed_ms, total_elapsed_ms,
            created_at, updated_at",
    )
    .bind(id)
    .bind(current_stage)
    .bind(progress_percent)
    .bind(activity_status)
    .bind(latest_error_message)
    .fetch_one(pool)
    .await
}

/// Updates the worker heartbeat snapshot for the active runtime ingestion run.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the runtime ingestion run.
pub async fn update_runtime_ingestion_run_heartbeat(
    pool: &PgPool,
    id: Uuid,
    last_heartbeat_at: DateTime<Utc>,
    activity_status: &str,
) -> Result<Option<RuntimeIngestionRunRow>, sqlx::Error> {
    update_runtime_ingestion_run_heartbeat_with_interval(
        pool,
        id,
        last_heartbeat_at,
        activity_status,
        1,
    )
    .await
}

/// Updates the worker heartbeat snapshot for the active runtime ingestion run behind a bounded
/// write interval.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the runtime ingestion run.
pub async fn update_runtime_ingestion_run_heartbeat_with_interval(
    pool: &PgPool,
    id: Uuid,
    last_heartbeat_at: DateTime<Utc>,
    activity_status: &str,
    min_write_interval_seconds: i64,
) -> Result<Option<RuntimeIngestionRunRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "with candidate as (
            select id
            from runtime_ingestion_run
            where id = $1
              and (
                    last_heartbeat_at is null
                    or last_heartbeat_at <= ($2 - ($4 * interval '1 second'))
                    or activity_status <> $3
               )
            for update skip locked
         )
         update runtime_ingestion_run as run
         set activity_status = $3,
             last_activity_at = greatest(coalesce(last_activity_at, $2), $2),
             last_heartbeat_at = $2
         from candidate
         where run.id = candidate.id
         returning run.id, run.project_id, run.document_id, run.revision_id, run.upload_batch_id, run.track_id, run.file_name, run.file_type, run.mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status, last_activity_at,
            last_heartbeat_at, provider_profile_snapshot_json, latest_error_message, current_attempt_no,
            attempt_kind, queue_started_at, started_at, finished_at, queue_elapsed_ms, total_elapsed_ms,
            created_at, updated_at",
    )
    .bind(id)
    .bind(last_heartbeat_at)
    .bind(activity_status)
    .bind(min_write_interval_seconds.max(1))
    .fetch_optional(pool)
    .await
}

/// Counts persisted chunks for one document.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the chunk count.
pub async fn count_chunks_by_document(
    pool: &PgPool,
    document_id: Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("select count(*) from chunk where document_id = $1")
        .bind(document_id)
        .fetch_one(pool)
        .await
}

/// Upserts the full contribution summary for the latest active document revision.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating the contribution summary row.
pub async fn upsert_runtime_document_contribution_summary(
    pool: &PgPool,
    document_id: Uuid,
    revision_id: Option<Uuid>,
    ingestion_run_id: Option<Uuid>,
    latest_attempt_no: i32,
    chunk_count: Option<i32>,
    admitted_graph_node_count: i32,
    admitted_graph_edge_count: i32,
    filtered_graph_edge_count: i32,
    filtered_artifact_count: i32,
) -> Result<RuntimeDocumentContributionSummaryRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeDocumentContributionSummaryRow>(
        "insert into runtime_document_contribution_summary (
            document_id, revision_id, ingestion_run_id, latest_attempt_no, chunk_count,
            admitted_graph_node_count, admitted_graph_edge_count, filtered_graph_edge_count,
            filtered_artifact_count, computed_at
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, now())
         on conflict (document_id) do update
         set revision_id = excluded.revision_id,
             ingestion_run_id = excluded.ingestion_run_id,
             latest_attempt_no = excluded.latest_attempt_no,
             chunk_count = excluded.chunk_count,
             admitted_graph_node_count = excluded.admitted_graph_node_count,
             admitted_graph_edge_count = excluded.admitted_graph_edge_count,
             filtered_graph_edge_count = excluded.filtered_graph_edge_count,
             filtered_artifact_count = excluded.filtered_artifact_count,
             computed_at = excluded.computed_at
         returning document_id, revision_id, ingestion_run_id, latest_attempt_no, chunk_count,
            admitted_graph_node_count, admitted_graph_edge_count, filtered_graph_edge_count,
            filtered_artifact_count, computed_at",
    )
    .bind(document_id)
    .bind(revision_id)
    .bind(ingestion_run_id)
    .bind(latest_attempt_no)
    .bind(chunk_count)
    .bind(admitted_graph_node_count)
    .bind(admitted_graph_edge_count)
    .bind(filtered_graph_edge_count)
    .bind(filtered_artifact_count)
    .fetch_one(pool)
    .await
}

/// Upserts just the persisted chunk count for the latest active revision summary.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating the contribution summary row.
pub async fn upsert_runtime_document_chunk_count(
    pool: &PgPool,
    document_id: Uuid,
    revision_id: Option<Uuid>,
    ingestion_run_id: Option<Uuid>,
    latest_attempt_no: i32,
    chunk_count: Option<i32>,
) -> Result<RuntimeDocumentContributionSummaryRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeDocumentContributionSummaryRow>(
        "insert into runtime_document_contribution_summary (
            document_id, revision_id, ingestion_run_id, latest_attempt_no, chunk_count, computed_at
         ) values ($1, $2, $3, $4, $5, now())
         on conflict (document_id) do update
         set revision_id = excluded.revision_id,
             ingestion_run_id = excluded.ingestion_run_id,
             latest_attempt_no = excluded.latest_attempt_no,
             chunk_count = excluded.chunk_count,
             computed_at = excluded.computed_at
         returning document_id, revision_id, ingestion_run_id, latest_attempt_no, chunk_count,
            admitted_graph_node_count, admitted_graph_edge_count, filtered_graph_edge_count,
            filtered_artifact_count, computed_at",
    )
    .bind(document_id)
    .bind(revision_id)
    .bind(ingestion_run_id)
    .bind(latest_attempt_no)
    .bind(chunk_count)
    .fetch_one(pool)
    .await
}

/// Upserts admitted and filtered graph contribution counts for the latest active revision summary.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating the contribution summary row.
pub async fn upsert_runtime_document_graph_contribution_counts(
    pool: &PgPool,
    document_id: Uuid,
    revision_id: Option<Uuid>,
    ingestion_run_id: Option<Uuid>,
    latest_attempt_no: i32,
    admitted_graph_node_count: i32,
    admitted_graph_edge_count: i32,
    filtered_graph_edge_count: i32,
    filtered_artifact_count: i32,
) -> Result<RuntimeDocumentContributionSummaryRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeDocumentContributionSummaryRow>(
        "insert into runtime_document_contribution_summary (
            document_id, revision_id, ingestion_run_id, latest_attempt_no,
            admitted_graph_node_count, admitted_graph_edge_count, filtered_graph_edge_count,
            filtered_artifact_count, computed_at
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, now())
         on conflict (document_id) do update
         set revision_id = excluded.revision_id,
             ingestion_run_id = excluded.ingestion_run_id,
             latest_attempt_no = excluded.latest_attempt_no,
             admitted_graph_node_count = excluded.admitted_graph_node_count,
             admitted_graph_edge_count = excluded.admitted_graph_edge_count,
             filtered_graph_edge_count = excluded.filtered_graph_edge_count,
             filtered_artifact_count = excluded.filtered_artifact_count,
             computed_at = excluded.computed_at
         returning document_id, revision_id, ingestion_run_id, latest_attempt_no, chunk_count,
            admitted_graph_node_count, admitted_graph_edge_count, filtered_graph_edge_count,
            filtered_artifact_count, computed_at",
    )
    .bind(document_id)
    .bind(revision_id)
    .bind(ingestion_run_id)
    .bind(latest_attempt_no)
    .bind(admitted_graph_node_count)
    .bind(admitted_graph_edge_count)
    .bind(filtered_graph_edge_count)
    .bind(filtered_artifact_count)
    .fetch_one(pool)
    .await
}

/// Loads the latest contribution summary for one logical document.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the contribution summary row.
pub async fn get_runtime_document_contribution_summary_by_document_id(
    pool: &PgPool,
    document_id: Uuid,
) -> Result<Option<RuntimeDocumentContributionSummaryRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeDocumentContributionSummaryRow>(
        "select document_id, revision_id, ingestion_run_id, latest_attempt_no, chunk_count,
            admitted_graph_node_count, admitted_graph_edge_count, filtered_graph_edge_count,
            filtered_artifact_count, computed_at
         from runtime_document_contribution_summary
         where document_id = $1",
    )
    .bind(document_id)
    .fetch_optional(pool)
    .await
}

/// Deletes the cached contribution summary for one logical document.
///
/// # Errors
/// Returns any `SQLx` error raised while deleting the contribution summary row.
pub async fn delete_runtime_document_contribution_summary_by_document_id(
    pool: &PgPool,
    document_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let result =
        sqlx::query("delete from runtime_document_contribution_summary where document_id = $1")
            .bind(document_id)
            .execute(pool)
            .await?;
    Ok(result.rows_affected())
}

/// Upserts extracted-content metadata for one runtime ingestion run.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating extracted content.
pub async fn upsert_runtime_extracted_content(
    pool: &PgPool,
    ingestion_run_id: Uuid,
    document_id: Option<Uuid>,
    extraction_kind: &str,
    content_text: Option<&str>,
    page_count: Option<i32>,
    char_count: Option<i32>,
    extraction_warnings_json: serde_json::Value,
    source_map_json: serde_json::Value,
    provider_kind: Option<&str>,
    model_name: Option<&str>,
    extraction_version: Option<&str>,
) -> Result<RuntimeExtractedContentRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeExtractedContentRow>(
        "insert into runtime_extracted_content (
            id, ingestion_run_id, document_id, extraction_kind, content_text, page_count, char_count,
            extraction_warnings_json, source_map_json, provider_kind, model_name, extraction_version
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
         on conflict (ingestion_run_id) do update
         set document_id = excluded.document_id,
             extraction_kind = excluded.extraction_kind,
             content_text = excluded.content_text,
             page_count = excluded.page_count,
             char_count = excluded.char_count,
             extraction_warnings_json = excluded.extraction_warnings_json,
             source_map_json = excluded.source_map_json,
             provider_kind = excluded.provider_kind,
             model_name = excluded.model_name,
             extraction_version = excluded.extraction_version,
             updated_at = now()
         returning id, ingestion_run_id, document_id, extraction_kind, content_text, page_count,
            char_count, extraction_warnings_json, source_map_json, provider_kind, model_name,
            extraction_version, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(ingestion_run_id)
    .bind(document_id)
    .bind(extraction_kind)
    .bind(content_text)
    .bind(page_count)
    .bind(char_count)
    .bind(extraction_warnings_json)
    .bind(source_map_json)
    .bind(provider_kind)
    .bind(model_name)
    .bind(extraction_version)
    .fetch_one(pool)
    .await
}

/// Loads extracted content for one runtime ingestion run.
///
/// # Errors
/// Returns any `SQLx` error raised while querying extracted-content metadata.
pub async fn get_runtime_extracted_content_by_run(
    pool: &PgPool,
    ingestion_run_id: Uuid,
) -> Result<Option<RuntimeExtractedContentRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeExtractedContentRow>(
        "select id, ingestion_run_id, document_id, extraction_kind, content_text, page_count,
            char_count, extraction_warnings_json, source_map_json, provider_kind, model_name,
            extraction_version, created_at, updated_at
         from runtime_extracted_content
         where ingestion_run_id = $1",
    )
    .bind(ingestion_run_id)
    .fetch_optional(pool)
    .await
}

/// Creates one stage-accounting row for a runtime ingestion attempt.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the stage-accounting row.
pub async fn create_attempt_stage_accounting(
    pool: &PgPool,
    new_row: &NewAttemptStageAccounting,
) -> Result<AttemptStageAccountingRow, sqlx::Error> {
    let normalized = sanitize_new_attempt_stage_accounting(new_row)?;
    sqlx::query_as::<_, AttemptStageAccountingRow>(
        "insert into runtime_attempt_stage_accounting (
            id, ingestion_run_id, stage_event_id, stage, workspace_id, project_id, provider_kind,
            model_name, capability, billing_unit, accounting_scope, call_sequence_no, usage_event_id, cost_ledger_id,
            pricing_catalog_entry_id, pricing_status, estimated_cost, currency, token_usage_json,
            pricing_snapshot_json
         ) values (
            $1, $2, $3, $4, $5, $6, $7,
            $8, $9, $10, $11, $12, $13,
            $14, $15, $16, $17, $18,
            $19, $20
         )
         on conflict (stage_event_id, accounting_scope, call_sequence_no) do update
         set ingestion_run_id = excluded.ingestion_run_id,
             stage = excluded.stage,
             workspace_id = excluded.workspace_id,
             project_id = excluded.project_id,
             provider_kind = excluded.provider_kind,
             model_name = excluded.model_name,
             capability = excluded.capability,
             billing_unit = excluded.billing_unit,
             accounting_scope = excluded.accounting_scope,
             call_sequence_no = excluded.call_sequence_no,
             usage_event_id = excluded.usage_event_id,
             cost_ledger_id = excluded.cost_ledger_id,
             pricing_catalog_entry_id = excluded.pricing_catalog_entry_id,
             pricing_status = excluded.pricing_status,
             estimated_cost = excluded.estimated_cost,
             currency = excluded.currency,
             token_usage_json = excluded.token_usage_json,
             pricing_snapshot_json = excluded.pricing_snapshot_json
         returning id, ingestion_run_id, stage_event_id, stage, accounting_scope, call_sequence_no, workspace_id, project_id,
            provider_kind, model_name, capability, billing_unit, usage_event_id, cost_ledger_id,
            pricing_catalog_entry_id, pricing_status, estimated_cost, currency, token_usage_json,
            pricing_snapshot_json, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(normalized.ingestion_run_id)
    .bind(normalized.stage_event_id)
    .bind(&normalized.stage)
    .bind(normalized.workspace_id)
    .bind(normalized.project_id)
    .bind(normalized.provider_kind.as_deref())
    .bind(normalized.model_name.as_deref())
    .bind(&normalized.capability)
    .bind(&normalized.billing_unit)
    .bind(&normalized.accounting_scope)
    .bind(normalized.call_sequence_no)
    .bind(normalized.usage_event_id)
    .bind(normalized.cost_ledger_id)
    .bind(normalized.pricing_catalog_entry_id)
    .bind(&normalized.pricing_status)
    .bind(normalized.estimated_cost)
    .bind(normalized.currency.as_deref())
    .bind(normalized.token_usage_json.clone())
    .bind(normalized.pricing_snapshot_json.clone())
    .fetch_one(pool)
    .await
}

/// Lists stage-accounting rows for one runtime ingestion run.
///
/// # Errors
/// Returns any `SQLx` error raised while loading stage-accounting rows.
pub async fn list_attempt_stage_accounting_by_run(
    pool: &PgPool,
    ingestion_run_id: Uuid,
) -> Result<Vec<AttemptStageAccountingRow>, sqlx::Error> {
    sqlx::query_as::<_, AttemptStageAccountingRow>(
        "select id, ingestion_run_id, stage_event_id, stage, accounting_scope, call_sequence_no, workspace_id, project_id, provider_kind,
            model_name, capability, billing_unit, usage_event_id, cost_ledger_id,
            pricing_catalog_entry_id, pricing_status, estimated_cost, currency, token_usage_json,
            pricing_snapshot_json, created_at
         from runtime_attempt_stage_accounting
         where ingestion_run_id = $1
         order by created_at asc, accounting_scope asc, call_sequence_no asc, id asc",
    )
    .bind(ingestion_run_id)
    .fetch_all(pool)
    .await
}

/// Loads one stage-accounting row by its canonical provider/stage key.
///
/// # Errors
/// Returns any `SQLx` error raised while loading the stage-accounting row.
pub async fn get_attempt_stage_accounting_by_scope(
    pool: &PgPool,
    stage_event_id: Uuid,
    accounting_scope: &str,
    call_sequence_no: i32,
) -> Result<Option<AttemptStageAccountingRow>, sqlx::Error> {
    sqlx::query_as::<_, AttemptStageAccountingRow>(
        "select id, ingestion_run_id, stage_event_id, stage, accounting_scope, call_sequence_no, workspace_id, project_id, provider_kind,
            model_name, capability, billing_unit, usage_event_id, cost_ledger_id,
            pricing_catalog_entry_id, pricing_status, estimated_cost, currency, token_usage_json,
            pricing_snapshot_json, created_at
         from runtime_attempt_stage_accounting
         where stage_event_id = $1
           and accounting_scope = $2
           and call_sequence_no = $3",
    )
    .bind(stage_event_id)
    .bind(accounting_scope)
    .bind(call_sequence_no)
    .fetch_optional(pool)
    .await
}

/// Recomputes and persists one attempt cost summary.
///
/// # Errors
/// Returns any `SQLx` error raised while refreshing the attempt cost summary.
pub async fn refresh_attempt_stage_cost_summary(
    pool: &PgPool,
    ingestion_run_id: Uuid,
) -> Result<AttemptStageCostSummaryRow, sqlx::Error> {
    sqlx::query_as::<_, AttemptStageCostSummaryRow>(
        "insert into runtime_attempt_cost_summary (
            ingestion_run_id, total_estimated_cost, settled_estimated_cost, in_flight_estimated_cost,
            currency, priced_stage_count, unpriced_stage_count, in_flight_stage_count,
            missing_stage_count, accounting_status, computed_at
         )
         with current_attempt as (
            select id as ingestion_run_id, current_attempt_no
            from runtime_ingestion_run
            where id = $1
         ),
         billable_stages as (
            select distinct stage_event.ingestion_run_id, stage_event.stage
            from runtime_ingestion_stage_event as stage_event
            join current_attempt
              on current_attempt.ingestion_run_id = stage_event.ingestion_run_id
             and current_attempt.current_attempt_no = stage_event.attempt_no
            where stage_event.stage in ('extracting_content', 'embedding_chunks', 'extracting_graph')
         ),
         stage_rollups as (
            select
                accounting.ingestion_run_id,
                accounting.stage,
                max(accounting.estimated_cost) as estimated_cost,
                max(accounting.currency) as currency,
                (array_agg(accounting.pricing_status order by accounting.created_at desc))[1] as pricing_status
            from runtime_attempt_stage_accounting as accounting
            join runtime_ingestion_stage_event as stage_event
              on stage_event.id = accounting.stage_event_id
            join current_attempt
              on current_attempt.ingestion_run_id = accounting.ingestion_run_id
             and current_attempt.current_attempt_no = stage_event.attempt_no
            where accounting.ingestion_run_id = $1
              and accounting.accounting_scope = 'stage_rollup'
              and accounting.stage in ('extracting_content', 'embedding_chunks', 'extracting_graph')
            group by accounting.ingestion_run_id, accounting.stage
         ),
         provider_calls as (
            select
                accounting.ingestion_run_id,
                accounting.stage,
                sum(accounting.estimated_cost) as estimated_cost,
                max(accounting.currency) as currency,
                count(*) filter (where accounting.pricing_status = 'priced')::integer as priced_call_count,
                count(*) filter (where accounting.pricing_status <> 'priced')::integer as unpriced_call_count
            from runtime_attempt_stage_accounting as accounting
            join runtime_ingestion_stage_event as stage_event
              on stage_event.id = accounting.stage_event_id
            join current_attempt
              on current_attempt.ingestion_run_id = accounting.ingestion_run_id
             and current_attempt.current_attempt_no = stage_event.attempt_no
            where accounting.ingestion_run_id = $1
              and accounting.accounting_scope = 'provider_call'
              and accounting.stage in ('extracting_content', 'embedding_chunks', 'extracting_graph')
            group by accounting.ingestion_run_id, accounting.stage
         ),
         resolved_stage_accounting as (
            select
                billable_stages.ingestion_run_id,
                billable_stages.stage,
                case
                    when stage_rollups.stage is not null then 'stage_rollup'
                    when provider_calls.stage is not null then 'provider_call'
                    else 'missing'
                end as accounting_scope,
                coalesce(stage_rollups.estimated_cost, provider_calls.estimated_cost) as estimated_cost,
                coalesce(stage_rollups.currency, provider_calls.currency) as currency,
                case
                    when stage_rollups.stage is not null then stage_rollups.pricing_status
                    when provider_calls.stage is not null
                     and provider_calls.priced_call_count > 0
                     and provider_calls.unpriced_call_count = 0 then 'priced'
                    when provider_calls.stage is not null
                     and provider_calls.priced_call_count > 0 then 'partial'
                    when provider_calls.stage is not null then 'unpriced'
                    else 'unpriced'
                end as pricing_status
            from billable_stages
            left join stage_rollups
              on stage_rollups.ingestion_run_id = billable_stages.ingestion_run_id
             and stage_rollups.stage = billable_stages.stage
            left join provider_calls
              on provider_calls.ingestion_run_id = billable_stages.ingestion_run_id
             and provider_calls.stage = billable_stages.stage
         )
         select
            $1,
            sum(resolved_stage_accounting.estimated_cost) as total_estimated_cost,
            sum(resolved_stage_accounting.estimated_cost) filter (where resolved_stage_accounting.accounting_scope = 'stage_rollup') as settled_estimated_cost,
            sum(resolved_stage_accounting.estimated_cost) filter (where resolved_stage_accounting.accounting_scope = 'provider_call') as in_flight_estimated_cost,
            max(resolved_stage_accounting.currency) as currency,
            count(*) filter (
                where resolved_stage_accounting.accounting_scope = 'stage_rollup'
                  and resolved_stage_accounting.pricing_status = 'priced'
            )::integer as priced_stage_count,
            count(*) filter (
                where resolved_stage_accounting.accounting_scope = 'stage_rollup'
                  and resolved_stage_accounting.pricing_status <> 'priced'
            )::integer as unpriced_stage_count,
            count(*) filter (where resolved_stage_accounting.accounting_scope = 'provider_call')::integer as in_flight_stage_count,
            count(*) filter (where resolved_stage_accounting.accounting_scope = 'missing')::integer as missing_stage_count,
            case
                when count(*) filter (where resolved_stage_accounting.accounting_scope = 'provider_call') > 0
                    then 'in_flight_unsettled'
                when count(*) filter (
                    where resolved_stage_accounting.accounting_scope = 'stage_rollup'
                      and resolved_stage_accounting.pricing_status = 'priced'
                ) > 0
                 and count(*) filter (
                    where resolved_stage_accounting.accounting_scope <> 'stage_rollup'
                       or resolved_stage_accounting.pricing_status <> 'priced'
                ) = 0 then 'priced'
                when count(*) filter (where resolved_stage_accounting.accounting_scope = 'stage_rollup') > 0
                    then 'partial'
                else 'unpriced'
            end as accounting_status,
            now()
         from resolved_stage_accounting
         on conflict (ingestion_run_id) do update
         set total_estimated_cost = excluded.total_estimated_cost,
             settled_estimated_cost = excluded.settled_estimated_cost,
             in_flight_estimated_cost = excluded.in_flight_estimated_cost,
             currency = excluded.currency,
             priced_stage_count = excluded.priced_stage_count,
             unpriced_stage_count = excluded.unpriced_stage_count,
             in_flight_stage_count = excluded.in_flight_stage_count,
             missing_stage_count = excluded.missing_stage_count,
             accounting_status = excluded.accounting_status,
             computed_at = excluded.computed_at
         returning ingestion_run_id, total_estimated_cost, settled_estimated_cost, in_flight_estimated_cost, currency, priced_stage_count,
            unpriced_stage_count, in_flight_stage_count, missing_stage_count, accounting_status, computed_at",
    )
    .bind(ingestion_run_id)
    .fetch_one(pool)
    .await
}

/// Loads the persisted latest-attempt cost summary for one runtime ingestion run.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the attempt summary row.
pub async fn get_attempt_stage_cost_summary_by_run(
    pool: &PgPool,
    ingestion_run_id: Uuid,
) -> Result<Option<AttemptStageCostSummaryRow>, sqlx::Error> {
    sqlx::query_as::<_, AttemptStageCostSummaryRow>(
        "select ingestion_run_id, total_estimated_cost, settled_estimated_cost, in_flight_estimated_cost, currency, priced_stage_count,
            unpriced_stage_count, in_flight_stage_count, missing_stage_count, accounting_status, computed_at
         from runtime_attempt_cost_summary
         where ingestion_run_id = $1",
    )
    .bind(ingestion_run_id)
    .fetch_optional(pool)
    .await
}

/// Lists resolved current-attempt billable accounting rows for one project.
///
/// This returns at most one logical row per ingestion run and billable stage, preferring a
/// settled `stage_rollup` when present and otherwise aggregating in-flight `provider_call` rows.
///
/// # Errors
/// Returns any `SQLx` error raised while loading the collection accounting rows.
pub async fn list_runtime_collection_resolved_stage_accounting(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Vec<RuntimeCollectionResolvedStageAccountingRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeCollectionResolvedStageAccountingRow>(
        "with current_runs as (
            select id as ingestion_run_id, file_type, current_attempt_no
            from runtime_ingestion_run
            where project_id = $1
         ),
         billable_stages as (
            select distinct current_runs.ingestion_run_id, current_runs.file_type, stage_event.stage
            from runtime_ingestion_stage_event as stage_event
            join current_runs
              on current_runs.ingestion_run_id = stage_event.ingestion_run_id
             and current_runs.current_attempt_no = stage_event.attempt_no
            where stage_event.stage in ('extracting_content', 'embedding_chunks', 'extracting_graph')
         ),
         stage_rollups as (
            select
                accounting.ingestion_run_id,
                current_runs.file_type,
                accounting.stage,
                max(accounting.estimated_cost) as estimated_cost,
                max(accounting.currency) as currency,
                (array_agg(accounting.pricing_status order by accounting.created_at desc))[1] as pricing_status,
                max(coalesce(usage_event.prompt_tokens, 0))::bigint as prompt_tokens,
                max(coalesce(usage_event.completion_tokens, 0))::bigint as completion_tokens,
                max(coalesce(usage_event.total_tokens, 0))::bigint as total_tokens
            from runtime_attempt_stage_accounting as accounting
            join runtime_ingestion_stage_event as stage_event
              on stage_event.id = accounting.stage_event_id
            join current_runs
              on current_runs.ingestion_run_id = accounting.ingestion_run_id
             and current_runs.current_attempt_no = stage_event.attempt_no
            left join usage_event
              on usage_event.id = accounting.usage_event_id
            where accounting.accounting_scope = 'stage_rollup'
              and accounting.stage in ('extracting_content', 'embedding_chunks', 'extracting_graph')
            group by accounting.ingestion_run_id, current_runs.file_type, accounting.stage
         ),
         provider_calls as (
            select
                accounting.ingestion_run_id,
                current_runs.file_type,
                accounting.stage,
                sum(accounting.estimated_cost) as estimated_cost,
                max(accounting.currency) as currency,
                count(*) filter (where accounting.pricing_status = 'priced')::integer as priced_call_count,
                count(*) filter (where accounting.pricing_status <> 'priced')::integer as unpriced_call_count,
                sum(coalesce(usage_event.prompt_tokens, 0))::bigint as prompt_tokens,
                sum(coalesce(usage_event.completion_tokens, 0))::bigint as completion_tokens,
                sum(coalesce(usage_event.total_tokens, 0))::bigint as total_tokens
            from runtime_attempt_stage_accounting as accounting
            join runtime_ingestion_stage_event as stage_event
              on stage_event.id = accounting.stage_event_id
            join current_runs
              on current_runs.ingestion_run_id = accounting.ingestion_run_id
             and current_runs.current_attempt_no = stage_event.attempt_no
            left join usage_event
              on usage_event.id = accounting.usage_event_id
            where accounting.accounting_scope = 'provider_call'
              and accounting.stage in ('extracting_content', 'embedding_chunks', 'extracting_graph')
            group by accounting.ingestion_run_id, current_runs.file_type, accounting.stage
         )
         select
            billable_stages.ingestion_run_id,
            billable_stages.file_type,
            billable_stages.stage,
            case
                when stage_rollups.stage is not null then 'stage_rollup'
                when provider_calls.stage is not null then 'provider_call'
                else 'missing'
            end as accounting_scope,
            case
                when stage_rollups.stage is not null then stage_rollups.pricing_status
                when provider_calls.stage is not null
                 and provider_calls.priced_call_count > 0
                 and provider_calls.unpriced_call_count = 0 then 'priced'
                when provider_calls.stage is not null
                 and provider_calls.priced_call_count > 0 then 'partial'
                when provider_calls.stage is not null then 'unpriced'
                else 'unpriced'
            end as pricing_status,
            coalesce(stage_rollups.estimated_cost, provider_calls.estimated_cost) as estimated_cost,
            coalesce(stage_rollups.currency, provider_calls.currency) as currency,
            coalesce(stage_rollups.prompt_tokens, provider_calls.prompt_tokens, 0)::bigint as prompt_tokens,
            coalesce(stage_rollups.completion_tokens, provider_calls.completion_tokens, 0)::bigint as completion_tokens,
            coalesce(stage_rollups.total_tokens, provider_calls.total_tokens, 0)::bigint as total_tokens
         from billable_stages
         left join stage_rollups
           on stage_rollups.ingestion_run_id = billable_stages.ingestion_run_id
          and stage_rollups.file_type = billable_stages.file_type
          and stage_rollups.stage = billable_stages.stage
         left join provider_calls
           on provider_calls.ingestion_run_id = billable_stages.ingestion_run_id
          and provider_calls.file_type = billable_stages.file_type
          and provider_calls.stage = billable_stages.stage
         order by billable_stages.file_type asc, billable_stages.ingestion_run_id asc, billable_stages.stage asc",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
}

/// Loads milestone and backlog counters for one project's current runtime collection state.
///
/// # Errors
/// Returns any `SQLx` error raised while loading the collection progress rollup.
pub async fn load_runtime_collection_progress_rollup(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<RuntimeCollectionProgressRollupRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeCollectionProgressRollupRow>(
        "with current_runs as (
            select
                run.id as ingestion_run_id,
                run.document_id,
                run.status,
                run.current_stage,
                run.current_attempt_no
            from runtime_ingestion_run as run
            where run.project_id = $1
         ),
         extracted as (
            select distinct extraction.ingestion_run_id
            from runtime_extracted_content as extraction
            join current_runs
              on current_runs.ingestion_run_id = extraction.ingestion_run_id
         ),
         latest_stage_status as (
            select ingestion_run_id, stage, status
            from (
                select
                    stage_event.ingestion_run_id,
                    stage_event.stage,
                    stage_event.status,
                    row_number() over (
                        partition by stage_event.ingestion_run_id, stage_event.stage
                        order by stage_event.created_at desc, stage_event.id desc
                    ) as status_rank
                from runtime_ingestion_stage_event as stage_event
                join current_runs
                  on current_runs.ingestion_run_id = stage_event.ingestion_run_id
                 and current_runs.current_attempt_no = stage_event.attempt_no
            ) as ranked_stage_status
            where status_rank = 1
         ),
         stage_flags as (
            select
                latest_stage_status.ingestion_run_id,
                bool_or(
                    latest_stage_status.stage = 'extracting_content'
                    and latest_stage_status.status = 'completed'
                ) as content_extracted_complete,
                bool_or(
                    latest_stage_status.stage = 'chunking'
                    and latest_stage_status.status = 'completed'
                ) as chunking_complete,
                bool_or(
                    latest_stage_status.stage = 'embedding_chunks'
                    and latest_stage_status.status = 'completed'
                ) as embedding_complete,
                bool_or(
                    latest_stage_status.stage = 'extracting_graph'
                    and latest_stage_status.status = 'completed'
                ) as graph_ready_complete
            from latest_stage_status
            group by latest_stage_status.ingestion_run_id
         ),
         contribution as (
            select
                current_runs.ingestion_run_id,
                summary.chunk_count
            from current_runs
            left join runtime_document_contribution_summary as summary
              on summary.document_id = current_runs.document_id
         )
         select
            count(*)::bigint as accepted_count,
            count(*) filter (
                where extracted.ingestion_run_id is not null
                   or coalesce(stage_flags.content_extracted_complete, false)
            )::bigint as content_extracted_count,
            count(*) filter (
                where coalesce(contribution.chunk_count, 0) > 0
                   or coalesce(stage_flags.chunking_complete, false)
            )::bigint as chunked_count,
            count(*) filter (
                where coalesce(stage_flags.embedding_complete, false)
                   or current_runs.current_stage in (
                        'extracting_graph',
                        'merging_graph',
                        'projecting_graph',
                        'finalizing'
                   )
                   or current_runs.status in ('ready', 'ready_no_graph')
            )::bigint as embedded_count,
            count(*) filter (
                where current_runs.status = 'processing'
                  and current_runs.current_stage = 'extracting_graph'
            )::bigint as extracting_graph_count,
            count(*) filter (
                where coalesce(stage_flags.graph_ready_complete, false)
                   or current_runs.current_stage in (
                        'merging_graph',
                        'projecting_graph',
                        'finalizing'
                   )
                   or current_runs.status in ('ready', 'ready_no_graph')
            )::bigint as graph_ready_count,
            count(*) filter (where current_runs.status = 'ready')::bigint as ready_count,
            count(*) filter (where current_runs.status = 'failed')::bigint as failed_count,
            count(*) filter (where current_runs.status = 'queued')::bigint as queue_backlog_count,
            count(*) filter (where current_runs.status = 'processing')::bigint as processing_backlog_count
         from current_runs
         left join extracted
           on extracted.ingestion_run_id = current_runs.ingestion_run_id
         left join stage_flags
           on stage_flags.ingestion_run_id = current_runs.ingestion_run_id
         left join contribution
           on contribution.ingestion_run_id = current_runs.ingestion_run_id",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await
}

/// Lists elapsed-time and status rollups for current-attempt stage events in one project.
///
/// # Errors
/// Returns any `SQLx` error raised while loading the collection stage rollups.
pub async fn list_runtime_collection_stage_rollups(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Vec<RuntimeCollectionStageRollupRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeCollectionStageRollupRow>(
        "with current_runs as (
            select id as ingestion_run_id, current_attempt_no
            from runtime_ingestion_run
            where project_id = $1
         ),
         latest_stage_status as (
            select stage, status, elapsed_ms
            from (
                select
                    stage_event.ingestion_run_id,
                    stage_event.stage,
                    stage_event.status,
                    stage_event.elapsed_ms,
                    row_number() over (
                        partition by stage_event.ingestion_run_id, stage_event.stage
                        order by stage_event.created_at desc, stage_event.id desc
                    ) as status_rank
                from runtime_ingestion_stage_event as stage_event
                join current_runs
                  on current_runs.ingestion_run_id = stage_event.ingestion_run_id
                 and current_runs.current_attempt_no = stage_event.attempt_no
            ) as ranked_stage_status
            where status_rank = 1
         )
         select
            latest_stage_status.stage,
            count(*) filter (where latest_stage_status.status = 'started')::bigint as active_count,
            count(*) filter (where latest_stage_status.status = 'completed')::bigint as completed_count,
            count(*) filter (where latest_stage_status.status = 'failed')::bigint as failed_count,
            (
                avg(latest_stage_status.elapsed_ms) filter (
                where latest_stage_status.status in ('completed', 'failed')
                  and latest_stage_status.elapsed_ms is not null
                )
            )::bigint as avg_elapsed_ms,
            max(latest_stage_status.elapsed_ms) filter (
                where latest_stage_status.status in ('completed', 'failed')
            ) as max_elapsed_ms
         from latest_stage_status
         group by latest_stage_status.stage
         order by latest_stage_status.stage asc",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
}

/// Lists per-format progress, backlog, and elapsed-time rollups for one project's current runs.
///
/// # Errors
/// Returns any `SQLx` error raised while loading the collection format diagnostics.
pub async fn list_runtime_collection_format_rollups(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Vec<RuntimeCollectionFormatRollupRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeCollectionFormatRollupRow>(
        "with current_runs as (
            select
                run.id as ingestion_run_id,
                run.document_id,
                run.file_type,
                run.status,
                run.current_stage,
                run.current_attempt_no,
                run.queue_elapsed_ms,
                run.total_elapsed_ms
            from runtime_ingestion_run as run
            where run.project_id = $1
         ),
         extracted as (
            select distinct extraction.ingestion_run_id
            from runtime_extracted_content as extraction
            join current_runs
              on current_runs.ingestion_run_id = extraction.ingestion_run_id
         ),
         latest_stage_status as (
            select ingestion_run_id, file_type, stage, status, elapsed_ms
            from (
                select
                    stage_event.ingestion_run_id,
                    current_runs.file_type,
                    stage_event.stage,
                    stage_event.status,
                    stage_event.elapsed_ms,
                    row_number() over (
                        partition by stage_event.ingestion_run_id, stage_event.stage
                        order by stage_event.created_at desc, stage_event.id desc
                    ) as status_rank
                from runtime_ingestion_stage_event as stage_event
                join current_runs
                  on current_runs.ingestion_run_id = stage_event.ingestion_run_id
                 and current_runs.current_attempt_no = stage_event.attempt_no
            ) as ranked_stage_status
            where status_rank = 1
         ),
         stage_flags as (
            select
                latest_stage_status.ingestion_run_id,
                bool_or(
                    latest_stage_status.stage = 'extracting_content'
                    and latest_stage_status.status = 'completed'
                ) as content_extracted_complete,
                bool_or(
                    latest_stage_status.stage = 'chunking'
                    and latest_stage_status.status = 'completed'
                ) as chunking_complete,
                bool_or(
                    latest_stage_status.stage = 'embedding_chunks'
                    and latest_stage_status.status = 'completed'
                ) as embedding_complete,
                bool_or(
                    latest_stage_status.stage = 'extracting_graph'
                    and latest_stage_status.status = 'completed'
                ) as graph_ready_complete
            from latest_stage_status
            group by latest_stage_status.ingestion_run_id
         ),
         contribution as (
            select
                current_runs.ingestion_run_id,
                summary.chunk_count
            from current_runs
            left join runtime_document_contribution_summary as summary
              on summary.document_id = current_runs.document_id
         ),
         format_stage_elapsed as (
            select
                latest_stage_status.file_type,
                latest_stage_status.stage,
                (
                    avg(latest_stage_status.elapsed_ms) filter (
                    where latest_stage_status.status in ('completed', 'failed')
                      and latest_stage_status.elapsed_ms is not null
                    )
                )::bigint as avg_elapsed_ms,
                max(latest_stage_status.elapsed_ms) filter (
                    where latest_stage_status.status in ('completed', 'failed')
                ) as max_elapsed_ms
            from latest_stage_status
            group by latest_stage_status.file_type, latest_stage_status.stage
         ),
         ranked_format_bottleneck as (
            select
                format_stage_elapsed.file_type,
                format_stage_elapsed.stage,
                format_stage_elapsed.avg_elapsed_ms,
                format_stage_elapsed.max_elapsed_ms,
                row_number() over (
                    partition by format_stage_elapsed.file_type
                    order by
                        format_stage_elapsed.avg_elapsed_ms desc nulls last,
                        format_stage_elapsed.max_elapsed_ms desc nulls last,
                        format_stage_elapsed.stage asc
                ) as bottleneck_rank
            from format_stage_elapsed
         )
         select
            current_runs.file_type,
            count(*)::bigint as document_count,
            count(*) filter (where current_runs.status = 'queued')::bigint as queued_count,
            count(*) filter (where current_runs.status = 'processing')::bigint as processing_count,
            count(*) filter (where current_runs.status = 'ready')::bigint as ready_count,
            count(*) filter (where current_runs.status = 'ready_no_graph')::bigint as ready_no_graph_count,
            count(*) filter (where current_runs.status = 'failed')::bigint as failed_count,
            count(*) filter (
                where extracted.ingestion_run_id is not null
                   or coalesce(stage_flags.content_extracted_complete, false)
            )::bigint as content_extracted_count,
            count(*) filter (
                where coalesce(contribution.chunk_count, 0) > 0
                   or coalesce(stage_flags.chunking_complete, false)
            )::bigint as chunked_count,
            count(*) filter (
                where coalesce(stage_flags.embedding_complete, false)
                   or current_runs.current_stage in (
                        'extracting_graph',
                        'merging_graph',
                        'projecting_graph',
                        'finalizing'
                   )
                   or current_runs.status in ('ready', 'ready_no_graph')
            )::bigint as embedded_count,
            count(*) filter (
                where current_runs.status = 'processing'
                  and current_runs.current_stage = 'extracting_graph'
            )::bigint as extracting_graph_count,
            count(*) filter (
                where coalesce(stage_flags.graph_ready_complete, false)
                   or current_runs.current_stage in (
                        'merging_graph',
                        'projecting_graph',
                        'finalizing'
                   )
                   or current_runs.status in ('ready', 'ready_no_graph')
            )::bigint as graph_ready_count,
            (
                avg(current_runs.queue_elapsed_ms) filter (
                    where current_runs.queue_elapsed_ms is not null
                )
            )::bigint as avg_queue_elapsed_ms,
            max(current_runs.queue_elapsed_ms) as max_queue_elapsed_ms,
            (
                avg(current_runs.total_elapsed_ms) filter (
                    where current_runs.total_elapsed_ms is not null
                )
            )::bigint as avg_total_elapsed_ms,
            max(current_runs.total_elapsed_ms) as max_total_elapsed_ms,
            ranked_format_bottleneck.stage as bottleneck_stage,
            ranked_format_bottleneck.avg_elapsed_ms as bottleneck_avg_elapsed_ms,
            ranked_format_bottleneck.max_elapsed_ms as bottleneck_max_elapsed_ms
         from current_runs
         left join extracted
           on extracted.ingestion_run_id = current_runs.ingestion_run_id
         left join stage_flags
           on stage_flags.ingestion_run_id = current_runs.ingestion_run_id
         left join contribution
           on contribution.ingestion_run_id = current_runs.ingestion_run_id
         left join ranked_format_bottleneck
           on ranked_format_bottleneck.file_type = current_runs.file_type
          and ranked_format_bottleneck.bottleneck_rank = 1
         group by
            current_runs.file_type,
            ranked_format_bottleneck.stage,
            ranked_format_bottleneck.avg_elapsed_ms,
            ranked_format_bottleneck.max_elapsed_ms
         order by current_runs.file_type asc",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
}

/// Refreshes the visible progress marker for one queue slice.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the progress marker.
pub async fn refresh_runtime_library_queue_slice_activity(
    pool: &PgPool,
    project_id: Uuid,
    workspace_id: Uuid,
    last_progress_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into runtime_library_queue_slice (
            project_id, workspace_id, last_progress_at
         ) values ($1, $2, $3)
         on conflict (project_id) do update
         set workspace_id = excluded.workspace_id,
             last_progress_at = excluded.last_progress_at,
             updated_at = now()",
    )
    .bind(project_id)
    .bind(workspace_id)
    .bind(last_progress_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Acquires a library-scoped PostgreSQL advisory lock for graph projection serialization.
///
/// The returned pooled connection keeps the session lock alive until
/// `release_runtime_library_projection_lock` is called.
///
/// # Errors
/// Returns any `SQLx` error raised while acquiring the connection or advisory lock.
pub async fn acquire_runtime_library_projection_lock(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<sqlx::pool::PoolConnection<Postgres>, sqlx::Error> {
    let mut connection = pool.acquire().await?;
    sqlx::query("select pg_advisory_lock(hashtextextended($1::text, 0))")
        .bind(project_id.to_string())
        .execute(&mut *connection)
        .await?;
    Ok(connection)
}

/// Releases a library-scoped PostgreSQL advisory lock for graph projection serialization.
///
/// # Errors
/// Returns any `SQLx` error raised while unlocking the advisory key.
pub async fn release_runtime_library_projection_lock(
    mut connection: sqlx::pool::PoolConnection<Postgres>,
    project_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("select pg_advisory_unlock(hashtextextended($1::text, 0))")
        .bind(project_id.to_string())
        .execute(&mut *connection)
        .await?;
    Ok(())
}

/// Loads provider failure classification metadata captured for one graph-extraction attempt.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the checkpoint row.
pub async fn load_runtime_provider_failure_snapshot(
    pool: &PgPool,
    ingestion_run_id: Uuid,
    attempt_no: i32,
) -> Result<Option<RuntimeProviderFailureSnapshotRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeProviderFailureSnapshotRow>(
        "select
            ingestion_run_id,
            attempt_no,
            provider_failure_class,
            request_shape_key,
            request_size_bytes,
            upstream_status,
            retry_outcome,
            computed_at
         from runtime_graph_progress_checkpoint
         where ingestion_run_id = $1
           and attempt_no = $2
           and provider_failure_class is not null",
    )
    .bind(ingestion_run_id)
    .bind(attempt_no)
    .fetch_optional(pool)
    .await
}

/// Persists provider failure classification metadata onto the active graph-progress checkpoint row.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the checkpoint row.
pub async fn record_runtime_graph_progress_failure_classification(
    pool: &PgPool,
    ingestion_run_id: Uuid,
    attempt_no: i32,
    provider_failure_class: Option<&str>,
    request_shape_key: Option<&str>,
    request_size_bytes: Option<i64>,
    upstream_status: Option<&str>,
    retry_outcome: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update runtime_graph_progress_checkpoint
         set provider_failure_class = $3,
             request_shape_key = $4,
             request_size_bytes = $5,
             upstream_status = $6,
             retry_outcome = $7,
             diagnostics_snapshot_at = now()
         where ingestion_run_id = $1
           and attempt_no = $2",
    )
    .bind(ingestion_run_id)
    .bind(attempt_no)
    .bind(provider_failure_class)
    .bind(request_shape_key)
    .bind(request_size_bytes)
    .bind(upstream_status)
    .bind(retry_outcome)
    .execute(pool)
    .await?;
    Ok(())
}

/// Lists model pricing catalog entries.
///
/// # Errors
/// Returns any `SQLx` error raised while loading pricing entries.
pub async fn list_model_pricing_catalog_entries(
    pool: &PgPool,
    workspace_id: Option<Uuid>,
) -> Result<Vec<ModelPricingCatalogEntryRow>, sqlx::Error> {
    match workspace_id {
        Some(workspace_id) => {
            sqlx::query_as::<_, ModelPricingCatalogEntryRow>(
                "select id, workspace_id, provider_kind, model_name, capability, billing_unit,
                    input_price, output_price, currency, status, source_kind, note, effective_from,
                    effective_to, created_at, updated_at
                 from model_pricing_catalog
                 where workspace_id = $1
                 order by effective_from desc, created_at desc",
            )
            .bind(workspace_id)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, ModelPricingCatalogEntryRow>(
                "select id, workspace_id, provider_kind, model_name, capability, billing_unit,
                    input_price, output_price, currency, status, source_kind, note, effective_from,
                    effective_to, created_at, updated_at
                 from model_pricing_catalog
                 order by effective_from desc, created_at desc",
            )
            .fetch_all(pool)
            .await
        }
    }
}

/// Loads one pricing catalog entry by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while querying one pricing row.
pub async fn get_model_pricing_catalog_entry_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<ModelPricingCatalogEntryRow>, sqlx::Error> {
    sqlx::query_as::<_, ModelPricingCatalogEntryRow>(
        "select id, workspace_id, provider_kind, model_name, capability, billing_unit,
            input_price, output_price, currency, status, source_kind, note, effective_from,
            effective_to, created_at, updated_at
         from model_pricing_catalog
         where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Creates a model pricing catalog entry.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting a pricing entry.
pub async fn create_model_pricing_catalog_entry(
    pool: &PgPool,
    new_row: &NewModelPricingCatalogEntry,
) -> Result<ModelPricingCatalogEntryRow, sqlx::Error> {
    sqlx::query_as::<_, ModelPricingCatalogEntryRow>(
        "insert into model_pricing_catalog (
            id, workspace_id, provider_kind, model_name, capability, billing_unit, input_price,
            output_price, currency, status, source_kind, note, effective_from
         ) values (
            $1, $2, $3, $4, $5, $6, $7,
            $8, $9, 'active', $10, $11, $12
         )
         returning id, workspace_id, provider_kind, model_name, capability, billing_unit,
            input_price, output_price, currency, status, source_kind, note, effective_from,
            effective_to, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(new_row.workspace_id)
    .bind(&new_row.provider_kind)
    .bind(&new_row.model_name)
    .bind(&new_row.capability)
    .bind(&new_row.billing_unit)
    .bind(new_row.input_price)
    .bind(new_row.output_price)
    .bind(&new_row.currency)
    .bind(&new_row.source_kind)
    .bind(new_row.note.as_deref())
    .bind(new_row.effective_from)
    .fetch_one(pool)
    .await
}

/// Supersedes active pricing rows that overlap a new effective pricing window.
///
/// # Errors
/// Returns any `SQLx` error raised while updating overlapping pricing rows.
pub async fn supersede_overlapping_model_pricing_catalog_entries(
    pool: &PgPool,
    workspace_id: Option<Uuid>,
    provider_kind: &str,
    model_name: &str,
    capability: &str,
    billing_unit: &str,
    effective_from: DateTime<Utc>,
) -> Result<Vec<ModelPricingCatalogEntryRow>, sqlx::Error> {
    sqlx::query_as::<_, ModelPricingCatalogEntryRow>(
        "update model_pricing_catalog
         set status = 'superseded',
             effective_to = $6,
             updated_at = now()
         where (($1::uuid is null and workspace_id is null) or workspace_id = $1)
           and provider_kind = $2
           and model_name = $3
           and capability = $4
           and billing_unit = $5
           and status = 'active'
           and effective_from < $6
           and (effective_to is null or effective_to > $6)
         returning id, workspace_id, provider_kind, model_name, capability, billing_unit,
            input_price, output_price, currency, status, source_kind, note, effective_from,
            effective_to, created_at, updated_at",
    )
    .bind(workspace_id)
    .bind(provider_kind)
    .bind(model_name)
    .bind(capability)
    .bind(billing_unit)
    .bind(effective_from)
    .fetch_all(pool)
    .await
}

/// Updates an existing pricing row in place.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the pricing row.
pub async fn update_model_pricing_catalog_entry(
    pool: &PgPool,
    id: Uuid,
    updated_row: &UpdateModelPricingCatalogEntry,
) -> Result<Option<ModelPricingCatalogEntryRow>, sqlx::Error> {
    sqlx::query_as::<_, ModelPricingCatalogEntryRow>(
        "update model_pricing_catalog
         set workspace_id = $2,
             provider_kind = $3,
             model_name = $4,
             capability = $5,
             billing_unit = $6,
             input_price = $7,
             output_price = $8,
             currency = $9,
             note = $10,
             effective_from = $11,
             updated_at = now()
         where id = $1
         returning id, workspace_id, provider_kind, model_name, capability, billing_unit,
            input_price, output_price, currency, status, source_kind, note, effective_from,
            effective_to, created_at, updated_at",
    )
    .bind(id)
    .bind(updated_row.workspace_id)
    .bind(&updated_row.provider_kind)
    .bind(&updated_row.model_name)
    .bind(&updated_row.capability)
    .bind(&updated_row.billing_unit)
    .bind(updated_row.input_price)
    .bind(updated_row.output_price)
    .bind(&updated_row.currency)
    .bind(updated_row.note.as_deref())
    .bind(updated_row.effective_from)
    .fetch_optional(pool)
    .await
}

/// Deactivates a model pricing catalog entry.
///
/// # Errors
/// Returns any `SQLx` error raised while deactivating a pricing entry.
pub async fn deactivate_model_pricing_catalog_entry(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<ModelPricingCatalogEntryRow>, sqlx::Error> {
    sqlx::query_as::<_, ModelPricingCatalogEntryRow>(
        "update model_pricing_catalog
         set status = 'inactive',
             effective_to = coalesce(effective_to, greatest(now(), effective_from)),
             updated_at = now()
         where id = $1
         returning id, workspace_id, provider_kind, model_name, capability, billing_unit,
            input_price, output_price, currency, status, source_kind, note, effective_from,
            effective_to, created_at, updated_at",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Loads the effective model pricing catalog entry at a given point in time.
///
/// # Errors
/// Returns any `SQLx` error raised while resolving an effective price.
pub async fn get_effective_model_pricing_catalog_entry(
    pool: &PgPool,
    workspace_id: Option<Uuid>,
    provider_kind: &str,
    model_name: &str,
    capability: &str,
    billing_unit: &str,
    at: DateTime<Utc>,
) -> Result<Option<ModelPricingCatalogEntryRow>, sqlx::Error> {
    sqlx::query_as::<_, ModelPricingCatalogEntryRow>(
        "select id, workspace_id, provider_kind, model_name, capability, billing_unit,
            input_price, output_price, currency, status, source_kind, note, effective_from,
            effective_to, created_at, updated_at
         from model_pricing_catalog
         where (($1::uuid is null and workspace_id is null) or workspace_id = $1)
           and provider_kind = $2
           and model_name = $3
           and capability = $4
           and billing_unit = $5
           and status = 'active'
           and effective_from <= $6
           and (effective_to is null or effective_to > $6)
         order by effective_from desc, created_at desc
         limit 1",
    )
    .bind(workspace_id)
    .bind(provider_kind)
    .bind(model_name)
    .bind(capability)
    .bind(billing_unit)
    .bind(at)
    .fetch_optional(pool)
    .await
}

/// Loads the effective model pricing catalog entry as a resolution projection row.
///
/// # Errors
/// Returns any `SQLx` error raised while resolving the effective price row.
pub async fn resolve_model_pricing_catalog_entry(
    pool: &PgPool,
    workspace_id: Option<Uuid>,
    provider_kind: &str,
    model_name: &str,
    capability: &str,
    billing_unit: &str,
    at: DateTime<Utc>,
) -> Result<Option<ModelPricingResolutionRow>, sqlx::Error> {
    sqlx::query_as::<_, ModelPricingResolutionRow>(
        "select id as pricing_catalog_entry_id, workspace_id, provider_kind, model_name, capability,
            billing_unit, input_price, output_price, currency, status, source_kind, effective_from,
            effective_to
         from model_pricing_catalog
         where (($1::uuid is null and workspace_id is null) or workspace_id = $1)
           and provider_kind = $2
           and model_name = $3
           and capability = $4
           and billing_unit = $5
           and status = 'active'
           and effective_from <= $6
           and (effective_to is null or effective_to > $6)
         order by effective_from desc, created_at desc
         limit 1",
    )
    .bind(workspace_id)
    .bind(provider_kind)
    .bind(model_name)
    .bind(capability)
    .bind(billing_unit)
    .bind(at)
    .fetch_optional(pool)
    .await
}

/// Returns the current dedicated source-truth version for one library.
///
/// # Errors
/// Returns any `SQLx` error raised while loading the `catalog_library` row.
pub async fn get_project_source_truth_version(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "select coalesce(source_truth_version, 1) from catalog_library where id = $1",
    )
    .bind(project_id)
    .fetch_optional(pool)
    .await
    .map(|version| version.map_or(1, |value| value.max(1)))
}

/// Counts distinct filtered graph artifacts written for one ingestion attempt.
///
/// # Errors
/// Returns any `SQLx` error raised while querying filtered artifact rows.
pub async fn count_runtime_graph_filtered_artifacts_by_ingestion_run(
    pool: &PgPool,
    project_id: Uuid,
    ingestion_run_id: Uuid,
    revision_id: Option<Uuid>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "select count(distinct concat_ws(
                ':',
                coalesce(revision_id::text, 'none'),
                coalesce(ingestion_run_id::text, 'none'),
                target_kind,
                candidate_key,
                filter_reason
            ))
         from runtime_graph_filtered_artifact
         where project_id = $1
           and ingestion_run_id = $2
           and ($3::uuid is null or revision_id = $3)",
    )
    .bind(project_id)
    .bind(ingestion_run_id)
    .bind(revision_id)
    .fetch_one(pool)
    .await
}

/// Deletes persisted query references that point at knowledge contributed by one document.
///
/// # Errors
/// Returns any `SQLx` error raised while deleting the persisted query references.
pub async fn delete_query_execution_references_by_document(
    pool: &PgPool,
    project_id: Uuid,
    document_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let chunk_result = sqlx::query(
        "delete from query_chunk_reference as reference
         using query_execution as execution
         where reference.execution_id = execution.id
           and execution.library_id = $1
           and exists (
               select 1
               from content_chunk
               where content_chunk.id = reference.chunk_id
                 and content_chunk.document_id = $2
           )",
    )
    .bind(project_id)
    .bind(document_id)
    .execute(pool)
    .await?;
    let node_result = sqlx::query(
        "delete from query_graph_node_reference as reference
         using query_execution as execution
         where reference.execution_id = execution.id
           and execution.library_id = $1
           and exists (
               select 1
               from graph_node_evidence as evidence
               join content_chunk on content_chunk.id = evidence.chunk_id
               where evidence.node_id = reference.node_id
                 and content_chunk.document_id = $2
           )",
    )
    .bind(project_id)
    .bind(document_id)
    .execute(pool)
    .await?;
    let edge_result = sqlx::query(
        "delete from query_graph_edge_reference as reference
         using query_execution as execution
         where reference.execution_id = execution.id
           and execution.library_id = $1
           and exists (
               select 1
               from graph_edge_evidence as evidence
               join content_chunk on content_chunk.id = evidence.chunk_id
               where evidence.edge_id = reference.edge_id
                 and content_chunk.document_id = $2
           )",
    )
    .bind(project_id)
    .bind(document_id)
    .execute(pool)
    .await?;
    Ok(chunk_result.rows_affected() + node_result.rows_affected() + edge_result.rows_affected())
}

/// Deletes persisted query references that point at knowledge contributed by one document revision.
///
/// # Errors
/// Returns any `SQLx` error raised while deleting revision-scoped query references.
pub async fn delete_query_execution_references_by_document_revision(
    pool: &PgPool,
    project_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let chunk_result = sqlx::query(
        "delete from query_chunk_reference as reference
         using query_execution as execution
         where reference.execution_id = execution.id
           and execution.library_id = $1
           and exists (
               select 1
               from content_chunk
               where content_chunk.id = reference.chunk_id
                 and content_chunk.document_id = $2
                 and content_chunk.revision_id = $3
           )",
    )
    .bind(project_id)
    .bind(document_id)
    .bind(revision_id)
    .execute(pool)
    .await?;
    let node_result = sqlx::query(
        "delete from query_graph_node_reference as reference
         using query_execution as execution
         where reference.execution_id = execution.id
           and execution.library_id = $1
           and exists (
               select 1
               from graph_node_evidence as evidence
               where evidence.node_id = reference.node_id
                 and evidence.revision_id = $3
                 and evidence.chunk_id in (
                     select id
                     from content_chunk
                     where document_id = $2
                       and revision_id = $3
                 )
           )",
    )
    .bind(project_id)
    .bind(document_id)
    .bind(revision_id)
    .execute(pool)
    .await?;
    let edge_result = sqlx::query(
        "delete from query_graph_edge_reference as reference
         using query_execution as execution
         where reference.execution_id = execution.id
           and execution.library_id = $1
           and exists (
               select 1
               from graph_edge_evidence as evidence
               where evidence.edge_id = reference.edge_id
                 and evidence.revision_id = $3
                 and evidence.chunk_id in (
                     select id
                     from content_chunk
                     where document_id = $2
                       and revision_id = $3
                 )
           )",
    )
    .bind(project_id)
    .bind(document_id)
    .bind(revision_id)
    .execute(pool)
    .await?;
    Ok(chunk_result.rows_affected() + node_result.rows_affected() + edge_result.rows_affected())
}

/// Persists one chunk-level graph extraction record.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the graph extraction record.
pub async fn create_runtime_graph_extraction_record(
    pool: &PgPool,
    project_id: Uuid,
    document_id: Uuid,
    chunk_id: Uuid,
    provider_kind: &str,
    model_name: &str,
    extraction_version: &str,
    prompt_hash: &str,
    status: &str,
    raw_output_json: serde_json::Value,
    normalized_output_json: serde_json::Value,
    glean_pass_count: i32,
    error_message: Option<&str>,
) -> Result<RuntimeGraphExtractionRecordRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphExtractionRecordRow>(
        "insert into runtime_graph_extraction (
            id, project_id, document_id, chunk_id, provider_kind, model_name, extraction_version,
            prompt_hash, status, raw_output_json, normalized_output_json, glean_pass_count, error_message
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
         returning id, project_id, document_id, chunk_id, provider_kind, model_name, extraction_version,
            prompt_hash, status, raw_output_json, normalized_output_json, glean_pass_count,
            error_message, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(document_id)
    .bind(chunk_id)
    .bind(provider_kind)
    .bind(model_name)
    .bind(extraction_version)
    .bind(prompt_hash)
    .bind(status)
    .bind(raw_output_json)
    .bind(normalized_output_json)
    .bind(glean_pass_count)
    .bind(error_message)
    .fetch_one(pool)
    .await
}

/// Upserts one bounded graph-progress checkpoint for the active extraction attempt.
///
/// # Errors
/// Returns any `SQLx` error raised while persisting the checkpoint row.
pub async fn upsert_runtime_graph_progress_checkpoint(
    pool: &PgPool,
    row: &RuntimeGraphProgressCheckpointInput,
) -> Result<RuntimeGraphProgressCheckpointRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphProgressCheckpointRow>(
        "insert into runtime_graph_progress_checkpoint (
            ingestion_run_id, attempt_no, processed_chunks, total_chunks, progress_percent,
            provider_call_count, avg_call_elapsed_ms, avg_chunk_elapsed_ms,
            avg_chars_per_second, avg_tokens_per_second, last_provider_call_at,
            next_checkpoint_eta_ms, pressure_kind, provider_failure_class,
            request_shape_key, request_size_bytes, upstream_status, retry_outcome, computed_at
         ) values (
            $1, $2, $3, $4, $5,
            $6, $7, $8,
            $9, $10, $11,
            $12, $13, $14,
            $15, $16, $17, $18, $19
         )
         on conflict (ingestion_run_id, attempt_no) do update
         set processed_chunks = excluded.processed_chunks,
             total_chunks = excluded.total_chunks,
             progress_percent = excluded.progress_percent,
             provider_call_count = excluded.provider_call_count,
             avg_call_elapsed_ms = excluded.avg_call_elapsed_ms,
             avg_chunk_elapsed_ms = excluded.avg_chunk_elapsed_ms,
             avg_chars_per_second = excluded.avg_chars_per_second,
             avg_tokens_per_second = excluded.avg_tokens_per_second,
             last_provider_call_at = excluded.last_provider_call_at,
             next_checkpoint_eta_ms = excluded.next_checkpoint_eta_ms,
             pressure_kind = excluded.pressure_kind,
             provider_failure_class = coalesce(
                 runtime_graph_progress_checkpoint.provider_failure_class,
                 excluded.provider_failure_class
             ),
             request_shape_key = coalesce(
                 runtime_graph_progress_checkpoint.request_shape_key,
                 excluded.request_shape_key
             ),
             request_size_bytes = coalesce(
                 runtime_graph_progress_checkpoint.request_size_bytes,
                 excluded.request_size_bytes
             ),
             upstream_status = coalesce(
                 runtime_graph_progress_checkpoint.upstream_status,
                 excluded.upstream_status
             ),
             retry_outcome = coalesce(
                 runtime_graph_progress_checkpoint.retry_outcome,
                 excluded.retry_outcome
             ),
             computed_at = excluded.computed_at
         returning ingestion_run_id, attempt_no, processed_chunks, total_chunks, progress_percent,
            provider_call_count, avg_call_elapsed_ms, avg_chunk_elapsed_ms,
            avg_chars_per_second, avg_tokens_per_second, last_provider_call_at,
            next_checkpoint_eta_ms, pressure_kind, provider_failure_class,
            request_shape_key, request_size_bytes, upstream_status, retry_outcome, computed_at",
    )
    .bind(row.ingestion_run_id)
    .bind(row.attempt_no)
    .bind(row.processed_chunks)
    .bind(row.total_chunks)
    .bind(row.progress_percent)
    .bind(row.provider_call_count)
    .bind(row.avg_call_elapsed_ms)
    .bind(row.avg_chunk_elapsed_ms)
    .bind(row.avg_chars_per_second)
    .bind(row.avg_tokens_per_second)
    .bind(row.last_provider_call_at)
    .bind(row.next_checkpoint_eta_ms)
    .bind(row.pressure_kind.as_deref())
    .bind(Option::<&str>::None)
    .bind(Option::<&str>::None)
    .bind(Option::<i64>::None)
    .bind(Option::<&str>::None)
    .bind(Option::<&str>::None)
    .bind(row.computed_at)
    .fetch_one(pool)
    .await
}

/// Loads the most recent graph-progress checkpoint for one runtime attempt.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the checkpoint row.
pub async fn get_runtime_graph_progress_checkpoint(
    pool: &PgPool,
    ingestion_run_id: Uuid,
    attempt_no: i32,
) -> Result<Option<RuntimeGraphProgressCheckpointRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphProgressCheckpointRow>(
        "select ingestion_run_id, attempt_no, processed_chunks, total_chunks, progress_percent,
            provider_call_count, avg_call_elapsed_ms, avg_chunk_elapsed_ms,
            avg_chars_per_second, avg_tokens_per_second, last_provider_call_at,
            next_checkpoint_eta_ms, pressure_kind, provider_failure_class,
            request_shape_key, request_size_bytes, upstream_status, retry_outcome, computed_at
         from runtime_graph_progress_checkpoint
         where ingestion_run_id = $1 and attempt_no = $2",
    )
    .bind(ingestion_run_id)
    .bind(attempt_no)
    .fetch_optional(pool)
    .await
}

/// Lists active graph-progress checkpoints for the current attempts in one library.
///
/// # Errors
/// Returns any `SQLx` error raised while querying checkpoint rows.
pub async fn list_active_runtime_graph_progress_checkpoints_by_project(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Vec<RuntimeGraphProgressCheckpointRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphProgressCheckpointRow>(
        "select checkpoint.ingestion_run_id, checkpoint.attempt_no, checkpoint.processed_chunks,
            checkpoint.total_chunks, checkpoint.progress_percent, checkpoint.provider_call_count,
            checkpoint.avg_call_elapsed_ms, checkpoint.avg_chunk_elapsed_ms,
            checkpoint.avg_chars_per_second, checkpoint.avg_tokens_per_second,
            checkpoint.last_provider_call_at, checkpoint.next_checkpoint_eta_ms,
            checkpoint.pressure_kind, checkpoint.provider_failure_class,
            checkpoint.request_shape_key, checkpoint.request_size_bytes,
            checkpoint.upstream_status, checkpoint.retry_outcome, checkpoint.computed_at
         from runtime_graph_progress_checkpoint as checkpoint
         join runtime_ingestion_run as run
           on run.id = checkpoint.ingestion_run_id
          and run.current_attempt_no = checkpoint.attempt_no
         where run.project_id = $1
           and run.status = 'processing'
           and run.current_stage = 'extracting_graph'
         order by checkpoint.avg_chunk_elapsed_ms desc nulls last,
            checkpoint.computed_at desc",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
}

/// Lists graph extraction records for one document.
///
/// # Errors
/// Returns any `SQLx` error raised while querying graph extraction records.
pub async fn list_runtime_graph_extraction_records_by_document(
    pool: &PgPool,
    document_id: Uuid,
) -> Result<Vec<RuntimeGraphExtractionRecordRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphExtractionRecordRow>(
        "select id, project_id, document_id, chunk_id, provider_kind, model_name, extraction_version,
            prompt_hash, status, raw_output_json, normalized_output_json, glean_pass_count,
            error_message, created_at
         from runtime_graph_extraction
         where document_id = $1
         order by created_at asc, id asc",
    )
    .bind(document_id)
    .fetch_all(pool)
    .await
}

/// Lists graph extraction records for one project.
///
/// # Errors
/// Returns any `SQLx` error raised while querying graph extraction records.
pub async fn list_runtime_graph_extraction_records_by_project(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Vec<RuntimeGraphExtractionRecordRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphExtractionRecordRow>(
        "select id, project_id, document_id, chunk_id, provider_kind, model_name, extraction_version,
            prompt_hash, status, raw_output_json, normalized_output_json, glean_pass_count,
            error_message, created_at
         from runtime_graph_extraction
         where project_id = $1
         order by created_at asc, id asc",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
}

/// Lists graph-extraction resume-state rows for one runtime ingestion run.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the resume-state rows.
pub async fn list_runtime_graph_extraction_resume_states_by_run(
    pool: &PgPool,
    ingestion_run_id: Uuid,
) -> Result<Vec<RuntimeGraphExtractionResumeStateRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphExtractionResumeStateRow>(
        "select ingestion_run_id, chunk_ordinal, chunk_content_hash, status, last_attempt_no,
            replay_count, resume_hit_count, downgrade_level, provider_kind, model_name,
            prompt_hash, request_shape_key, request_size_bytes, provider_failure_class,
            provider_failure_json, recovery_summary_json, raw_output_json, normalized_output_json,
            last_successful_at, created_at, updated_at
         from runtime_graph_extraction_resume_state
         where ingestion_run_id = $1
         order by chunk_ordinal asc",
    )
    .bind(ingestion_run_id)
    .fetch_all(pool)
    .await
}

/// Upserts one graph-extraction resume-state row keyed by `(ingestion_run_id, chunk_ordinal)`.
///
/// # Errors
/// Returns any `SQLx` error raised while persisting the row.
pub async fn upsert_runtime_graph_extraction_resume_state(
    pool: &PgPool,
    input: &UpsertRuntimeGraphExtractionResumeStateInput,
) -> Result<RuntimeGraphExtractionResumeStateRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphExtractionResumeStateRow>(
        "insert into runtime_graph_extraction_resume_state (
            ingestion_run_id, chunk_ordinal, chunk_content_hash, status, last_attempt_no,
            replay_count, resume_hit_count, downgrade_level, provider_kind, model_name,
            prompt_hash, request_shape_key, request_size_bytes, provider_failure_class,
            provider_failure_json, recovery_summary_json, raw_output_json, normalized_output_json,
            last_successful_at
         ) values (
            $1, $2, $3, $4, $5,
            $6, $7, $8, $9, $10,
            $11, $12, $13, $14,
            $15, $16, $17, $18,
            $19
         )
         on conflict (ingestion_run_id, chunk_ordinal) do update
         set chunk_content_hash = excluded.chunk_content_hash,
             status = excluded.status,
             last_attempt_no = excluded.last_attempt_no,
             replay_count = excluded.replay_count,
             resume_hit_count = excluded.resume_hit_count,
             downgrade_level = excluded.downgrade_level,
             provider_kind = excluded.provider_kind,
             model_name = excluded.model_name,
             prompt_hash = excluded.prompt_hash,
             request_shape_key = excluded.request_shape_key,
             request_size_bytes = excluded.request_size_bytes,
             provider_failure_class = excluded.provider_failure_class,
             provider_failure_json = excluded.provider_failure_json,
             recovery_summary_json = excluded.recovery_summary_json,
             raw_output_json = excluded.raw_output_json,
             normalized_output_json = excluded.normalized_output_json,
             last_successful_at = excluded.last_successful_at,
             updated_at = now()
         returning ingestion_run_id, chunk_ordinal, chunk_content_hash, status, last_attempt_no,
            replay_count, resume_hit_count, downgrade_level, provider_kind, model_name,
            prompt_hash, request_shape_key, request_size_bytes, provider_failure_class,
            provider_failure_json, recovery_summary_json, raw_output_json, normalized_output_json,
            last_successful_at, created_at, updated_at",
    )
    .bind(input.ingestion_run_id)
    .bind(input.chunk_ordinal)
    .bind(&input.chunk_content_hash)
    .bind(&input.status)
    .bind(input.last_attempt_no)
    .bind(input.replay_count)
    .bind(input.resume_hit_count)
    .bind(input.downgrade_level)
    .bind(input.provider_kind.as_deref())
    .bind(input.model_name.as_deref())
    .bind(input.prompt_hash.as_deref())
    .bind(input.request_shape_key.as_deref())
    .bind(input.request_size_bytes)
    .bind(input.provider_failure_class.as_deref())
    .bind(input.provider_failure_json.clone())
    .bind(input.recovery_summary_json.clone())
    .bind(input.raw_output_json.clone())
    .bind(input.normalized_output_json.clone())
    .bind(input.last_successful_at)
    .fetch_one(pool)
    .await
}

/// Increments the resume-hit counter for one persisted graph-extraction resume row.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the row.
pub async fn increment_runtime_graph_extraction_resume_hit(
    pool: &PgPool,
    ingestion_run_id: Uuid,
    chunk_ordinal: i32,
) -> Result<RuntimeGraphExtractionResumeStateRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphExtractionResumeStateRow>(
        "update runtime_graph_extraction_resume_state
         set resume_hit_count = resume_hit_count + 1,
             updated_at = now()
         where ingestion_run_id = $1
           and chunk_ordinal = $2
         returning ingestion_run_id, chunk_ordinal, chunk_content_hash, status, last_attempt_no,
            replay_count, resume_hit_count, downgrade_level, provider_kind, model_name,
            prompt_hash, request_shape_key, request_size_bytes, provider_failure_class,
            provider_failure_json, recovery_summary_json, raw_output_json, normalized_output_json,
            last_successful_at, created_at, updated_at",
    )
    .bind(ingestion_run_id)
    .bind(chunk_ordinal)
    .fetch_one(pool)
    .await
}

/// Loads one aggregated graph-extraction resume rollup for a single ingestion run.
///
/// # Errors
/// Returns any `SQLx` error raised while loading the aggregated resume rollup.
pub async fn load_runtime_graph_extraction_resume_rollup_by_run(
    pool: &PgPool,
    ingestion_run_id: Uuid,
) -> Result<Option<RuntimeGraphExtractionResumeRollupRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphExtractionResumeRollupRow>(
        "select ingestion_run_id,
            count(*)::bigint as chunk_count,
            count(*) filter (where status = 'ready')::bigint as ready_chunk_count,
            count(*) filter (where status = 'failed')::bigint as failed_chunk_count,
            coalesce(sum(greatest(replay_count, 0)), 0)::bigint as replayed_chunk_count,
            coalesce(sum(greatest(resume_hit_count, 0)), 0)::bigint as resume_hit_count,
            count(*) filter (where resume_hit_count > 0)::bigint as resumed_chunk_count,
            coalesce(max(greatest(downgrade_level, 0)), 0)::int as max_downgrade_level
         from runtime_graph_extraction_resume_state
         where ingestion_run_id = $1
         group by ingestion_run_id",
    )
    .bind(ingestion_run_id)
    .fetch_optional(pool)
    .await
}

/// Lists aggregated graph-extraction resume rollups for active runs in one project.
///
/// # Errors
/// Returns any `SQLx` error raised while loading the aggregated resume rollups.
pub async fn list_active_runtime_graph_extraction_resume_rollups_by_project(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Vec<RuntimeGraphExtractionResumeRollupRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphExtractionResumeRollupRow>(
        "select resume.ingestion_run_id,
            count(*)::bigint as chunk_count,
            count(*) filter (where resume.status = 'ready')::bigint as ready_chunk_count,
            count(*) filter (where resume.status = 'failed')::bigint as failed_chunk_count,
            coalesce(sum(greatest(resume.replay_count, 0)), 0)::bigint as replayed_chunk_count,
            coalesce(sum(greatest(resume.resume_hit_count, 0)), 0)::bigint as resume_hit_count,
            count(*) filter (where resume.resume_hit_count > 0)::bigint as resumed_chunk_count,
            coalesce(max(greatest(resume.downgrade_level, 0)), 0)::int as max_downgrade_level
         from runtime_graph_extraction_resume_state resume
         join runtime_ingestion_run run
           on run.id = resume.ingestion_run_id
         where run.project_id = $1
           and run.status = 'processing'
           and run.current_stage = 'extracting_graph'
         group by resume.ingestion_run_id",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
}

/// Creates one extraction-recovery attempt row.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the recovery attempt row.
pub async fn create_runtime_graph_extraction_recovery_attempt(
    pool: &PgPool,
    input: &CreateRuntimeGraphExtractionRecoveryAttemptInput,
) -> Result<RuntimeGraphExtractionRecoveryAttemptRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphExtractionRecoveryAttemptRow>(
        "insert into runtime_graph_extraction_recovery_attempt (
            id, workspace_id, project_id, document_id, revision_id, ingestion_run_id,
            attempt_no, chunk_id, recovery_kind, trigger_reason, status, raw_issue_summary,
            recovered_summary
         ) values (
            $1, $2, $3, $4, $5, $6,
            $7, $8, $9, $10, $11, $12,
            $13
         )
         returning id, workspace_id, project_id, document_id, revision_id, ingestion_run_id,
            attempt_no, chunk_id, recovery_kind, trigger_reason, status, raw_issue_summary,
            recovered_summary, started_at, finished_at, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(input.workspace_id)
    .bind(input.project_id)
    .bind(input.document_id)
    .bind(input.revision_id)
    .bind(input.ingestion_run_id)
    .bind(input.attempt_no)
    .bind(input.chunk_id)
    .bind(&input.recovery_kind)
    .bind(&input.trigger_reason)
    .bind(&input.status)
    .bind(input.raw_issue_summary.as_deref())
    .bind(input.recovered_summary.as_deref())
    .fetch_one(pool)
    .await
}

/// Updates the terminal status of one extraction-recovery attempt row.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the recovery attempt row.
pub async fn update_runtime_graph_extraction_recovery_attempt_status(
    pool: &PgPool,
    id: Uuid,
    status: &str,
    recovered_summary: Option<&str>,
) -> Result<Option<RuntimeGraphExtractionRecoveryAttemptRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphExtractionRecoveryAttemptRow>(
        "update runtime_graph_extraction_recovery_attempt
         set status = $2,
             recovered_summary = coalesce($3, recovered_summary),
             finished_at = case
                when $2 in ('recovered', 'partial', 'failed', 'skipped') then now()
                else finished_at
             end,
             updated_at = now()
         where id = $1
         returning id, workspace_id, project_id, document_id, revision_id, ingestion_run_id,
            attempt_no, chunk_id, recovery_kind, trigger_reason, status, raw_issue_summary,
            recovered_summary, started_at, finished_at, created_at, updated_at",
    )
    .bind(id)
    .bind(status)
    .bind(recovered_summary)
    .fetch_optional(pool)
    .await
}

/// Lists extraction-recovery attempts for one runtime ingestion run and attempt number.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the recovery attempt rows.
pub async fn list_runtime_graph_extraction_recovery_attempts_by_run(
    pool: &PgPool,
    ingestion_run_id: Uuid,
    attempt_no: i32,
) -> Result<Vec<RuntimeGraphExtractionRecoveryAttemptRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphExtractionRecoveryAttemptRow>(
        "select id, workspace_id, project_id, document_id, revision_id, ingestion_run_id,
            attempt_no, chunk_id, recovery_kind, trigger_reason, status, raw_issue_summary,
            recovered_summary, started_at, finished_at, created_at, updated_at
         from runtime_graph_extraction_recovery_attempt
         where ingestion_run_id = $1
           and attempt_no = $2
         order by started_at asc, created_at asc",
    )
    .bind(ingestion_run_id)
    .bind(attempt_no)
    .fetch_all(pool)
    .await
}

/// Lists extraction-recovery attempts for one document.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the recovery attempt rows.
pub async fn list_runtime_graph_extraction_recovery_attempts_by_document(
    pool: &PgPool,
    document_id: Uuid,
) -> Result<Vec<RuntimeGraphExtractionRecoveryAttemptRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphExtractionRecoveryAttemptRow>(
        "select id, workspace_id, project_id, document_id, revision_id, ingestion_run_id,
            attempt_no, chunk_id, recovery_kind, trigger_reason, status, raw_issue_summary,
            recovered_summary, started_at, finished_at, created_at, updated_at
         from runtime_graph_extraction_recovery_attempt
         where document_id = $1
         order by started_at desc, created_at desc",
    )
    .bind(document_id)
    .fetch_all(pool)
    .await
}

/// Creates one mutation impact-scope row for a document mutation workflow.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the impact-scope row.
pub async fn create_document_mutation_impact_scope(
    pool: &PgPool,
    input: &CreateDocumentMutationImpactScopeInput,
) -> Result<DocumentMutationImpactScopeRow, sqlx::Error> {
    sqlx::query_as::<_, DocumentMutationImpactScopeRow>(
        "insert into document_mutation_impact_scope (
            id, workspace_id, project_id, document_id, mutation_workflow_id, mutation_kind,
            source_revision_id, target_revision_id, scope_status, confidence_status,
            affected_node_ids_json, affected_relationship_ids_json, fallback_reason
         ) values (
            $1, $2, $3, $4, $5, $6,
            $7, $8, $9, $10,
            $11, $12, $13
         )
         returning id, workspace_id, project_id, document_id, mutation_workflow_id, mutation_kind,
            source_revision_id, target_revision_id, scope_status, confidence_status,
            affected_node_ids_json, affected_relationship_ids_json, fallback_reason, detected_at,
            completed_at, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(input.workspace_id)
    .bind(input.project_id)
    .bind(input.document_id)
    .bind(input.mutation_workflow_id)
    .bind(&input.mutation_kind)
    .bind(input.source_revision_id)
    .bind(input.target_revision_id)
    .bind(&input.scope_status)
    .bind(&input.confidence_status)
    .bind(input.affected_node_ids_json.clone())
    .bind(input.affected_relationship_ids_json.clone())
    .bind(input.fallback_reason.as_deref())
    .fetch_one(pool)
    .await
}

/// Updates an existing mutation impact-scope row while the workflow is still active.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the impact-scope row.
pub async fn update_document_mutation_impact_scope(
    pool: &PgPool,
    mutation_workflow_id: Uuid,
    scope_status: &str,
    confidence_status: &str,
    affected_node_ids_json: serde_json::Value,
    affected_relationship_ids_json: serde_json::Value,
    fallback_reason: Option<&str>,
) -> Result<Option<DocumentMutationImpactScopeRow>, sqlx::Error> {
    sqlx::query_as::<_, DocumentMutationImpactScopeRow>(
        "update document_mutation_impact_scope
         set scope_status = $2,
             confidence_status = $3,
             affected_node_ids_json = $4,
             affected_relationship_ids_json = $5,
             fallback_reason = $6,
             updated_at = now()
         where mutation_workflow_id = $1
         returning id, workspace_id, project_id, document_id, mutation_workflow_id, mutation_kind,
            source_revision_id, target_revision_id, scope_status, confidence_status,
            affected_node_ids_json, affected_relationship_ids_json, fallback_reason, detected_at,
            completed_at, created_at, updated_at",
    )
    .bind(mutation_workflow_id)
    .bind(scope_status)
    .bind(confidence_status)
    .bind(affected_node_ids_json)
    .bind(affected_relationship_ids_json)
    .bind(fallback_reason)
    .fetch_optional(pool)
    .await
}

/// Completes one mutation impact-scope row for a workflow.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the impact-scope row.
pub async fn complete_document_mutation_impact_scope(
    pool: &PgPool,
    mutation_workflow_id: Uuid,
    scope_status: &str,
    fallback_reason: Option<&str>,
) -> Result<Option<DocumentMutationImpactScopeRow>, sqlx::Error> {
    sqlx::query_as::<_, DocumentMutationImpactScopeRow>(
        "update document_mutation_impact_scope
         set scope_status = $2,
             fallback_reason = coalesce($3, fallback_reason),
             completed_at = now(),
             updated_at = now()
         where mutation_workflow_id = $1
         returning id, workspace_id, project_id, document_id, mutation_workflow_id, mutation_kind,
            source_revision_id, target_revision_id, scope_status, confidence_status,
            affected_node_ids_json, affected_relationship_ids_json, fallback_reason, detected_at,
            completed_at, created_at, updated_at",
    )
    .bind(mutation_workflow_id)
    .bind(scope_status)
    .bind(fallback_reason)
    .fetch_optional(pool)
    .await
}

/// Loads the impact-scope row for one mutation workflow.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the impact-scope row.
pub async fn get_document_mutation_impact_scope_by_workflow_id(
    pool: &PgPool,
    mutation_workflow_id: Uuid,
) -> Result<Option<DocumentMutationImpactScopeRow>, sqlx::Error> {
    sqlx::query_as::<_, DocumentMutationImpactScopeRow>(
        "select id, workspace_id, project_id, document_id, mutation_workflow_id, mutation_kind,
            source_revision_id, target_revision_id, scope_status, confidence_status,
            affected_node_ids_json, affected_relationship_ids_json, fallback_reason, detected_at,
            completed_at, created_at, updated_at
         from document_mutation_impact_scope
         where mutation_workflow_id = $1",
    )
    .bind(mutation_workflow_id)
    .fetch_optional(pool)
    .await
}

/// Loads the active impact-scope row for one logical document.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the impact-scope row.
pub async fn get_active_document_mutation_impact_scope_by_document_id(
    pool: &PgPool,
    document_id: Uuid,
) -> Result<Option<DocumentMutationImpactScopeRow>, sqlx::Error> {
    sqlx::query_as::<_, DocumentMutationImpactScopeRow>(
        "select id, workspace_id, project_id, document_id, mutation_workflow_id, mutation_kind,
            source_revision_id, target_revision_id, scope_status, confidence_status,
            affected_node_ids_json, affected_relationship_ids_json, fallback_reason, detected_at,
            completed_at, created_at, updated_at
         from document_mutation_impact_scope
         where document_id = $1
           and completed_at is null
         order by updated_at desc, detected_at desc, created_at desc
         limit 1",
    )
    .bind(document_id)
    .fetch_optional(pool)
    .await
}

/// Lists active mutation impact-scope rows for one project.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the impact-scope rows.
pub async fn list_active_document_mutation_impact_scopes_by_project(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Vec<DocumentMutationImpactScopeRow>, sqlx::Error> {
    sqlx::query_as::<_, DocumentMutationImpactScopeRow>(
        "select id, workspace_id, project_id, document_id, mutation_workflow_id, mutation_kind,
            source_revision_id, target_revision_id, scope_status, confidence_status,
            affected_node_ids_json, affected_relationship_ids_json, fallback_reason, detected_at,
            completed_at, created_at, updated_at
         from document_mutation_impact_scope
         where project_id = $1
           and completed_at is null
         order by updated_at desc, detected_at desc, created_at desc",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
}

/// Upserts an embedding target for a canonical graph node or relation.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating the embedding target.
pub async fn upsert_runtime_vector_target(
    pool: &PgPool,
    project_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
    provider_kind: &str,
    model_name: &str,
    dimensions: Option<i32>,
    embedding_json: serde_json::Value,
) -> Result<RuntimeVectorTargetRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeVectorTargetRow>(
        "insert into runtime_vector_target (
            id, project_id, target_kind, target_id, provider_kind, model_name, dimensions, embedding_json
         ) values ($1, $2, $3, $4, $5, $6, $7, $8)
         on conflict (project_id, target_kind, target_id, provider_kind, model_name) do update
         set dimensions = excluded.dimensions,
             embedding_json = excluded.embedding_json,
             updated_at = now()
         returning id, project_id, target_kind, target_id, provider_kind, model_name,
            dimensions, embedding_json, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(target_kind)
    .bind(target_id)
    .bind(provider_kind)
    .bind(model_name)
    .bind(dimensions)
    .bind(embedding_json)
    .fetch_one(pool)
    .await
}

fn coalesce_runtime_vector_target_upserts(
    rows: &[RuntimeVectorTargetUpsertInput],
) -> Vec<RuntimeVectorTargetUpsertInput> {
    let mut deduped = BTreeMap::new();
    for row in rows {
        deduped.insert(
            (
                row.project_id,
                row.target_kind.clone(),
                row.target_id,
                row.provider_kind.clone(),
                row.model_name.clone(),
            ),
            row.clone(),
        );
    }
    deduped.into_values().collect()
}

/// Upserts many embedding targets for canonical graph nodes or relations.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating the targets.
pub async fn upsert_runtime_vector_targets(
    pool: &PgPool,
    rows: &[RuntimeVectorTargetUpsertInput],
) -> Result<(), sqlx::Error> {
    let rows = coalesce_runtime_vector_target_upserts(rows);
    if rows.is_empty() {
        return Ok(());
    }

    let mut builder = QueryBuilder::<Postgres>::new(
        "insert into runtime_vector_target (
            id, project_id, target_kind, target_id, provider_kind, model_name, dimensions, embedding_json
         ) ",
    );
    builder.push_values(rows.iter(), |mut row_builder, row| {
        row_builder
            .push_bind(Uuid::now_v7())
            .push_bind(row.project_id)
            .push_bind(&row.target_kind)
            .push_bind(row.target_id)
            .push_bind(&row.provider_kind)
            .push_bind(&row.model_name)
            .push_bind(row.dimensions)
            .push_bind(&row.embedding_json);
    });
    builder.push(
        " on conflict (project_id, target_kind, target_id, provider_kind, model_name) do update
          set dimensions = excluded.dimensions,
              embedding_json = excluded.embedding_json,
              updated_at = now()
          where runtime_vector_target.dimensions is distinct from excluded.dimensions
             or runtime_vector_target.embedding_json is distinct from excluded.embedding_json",
    );
    builder.build().execute(pool).await?;
    Ok(())
}

/// Lists runtime vector targets for one project/kind/provider tuple.
///
/// # Errors
/// Returns any `SQLx` error raised while querying vector targets.
pub async fn list_runtime_vector_targets_by_project_and_kind(
    pool: &PgPool,
    project_id: Uuid,
    target_kind: &str,
    provider_kind: &str,
    model_name: &str,
) -> Result<Vec<RuntimeVectorTargetRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeVectorTargetRow>(
        "select id, project_id, target_kind, target_id, provider_kind, model_name,
            dimensions, embedding_json, created_at, updated_at
         from runtime_vector_target
         where project_id = $1
           and target_kind = $2
           and provider_kind = $3
           and model_name = $4
         order by updated_at desc",
    )
    .bind(project_id)
    .bind(target_kind)
    .bind(provider_kind)
    .bind(model_name)
    .fetch_all(pool)
    .await
}

/// Upserts the runtime provider profile for one project.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating the provider profile.
pub async fn upsert_runtime_provider_profile(
    pool: &PgPool,
    project_id: Uuid,
    indexing_provider_kind: &str,
    indexing_model_name: &str,
    embedding_provider_kind: &str,
    embedding_model_name: &str,
    answer_provider_kind: &str,
    answer_model_name: &str,
    vision_provider_kind: &str,
    vision_model_name: &str,
) -> Result<RuntimeProviderProfileRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeProviderProfileRow>(
        "insert into runtime_provider_profile (
            project_id, indexing_provider_kind, indexing_model_name, embedding_provider_kind,
            embedding_model_name, answer_provider_kind, answer_model_name, vision_provider_kind,
            vision_model_name
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         on conflict (project_id) do update
         set indexing_provider_kind = excluded.indexing_provider_kind,
             indexing_model_name = excluded.indexing_model_name,
             embedding_provider_kind = excluded.embedding_provider_kind,
             embedding_model_name = excluded.embedding_model_name,
             answer_provider_kind = excluded.answer_provider_kind,
             answer_model_name = excluded.answer_model_name,
             vision_provider_kind = excluded.vision_provider_kind,
             vision_model_name = excluded.vision_model_name,
             last_validated_at = case
                 when runtime_provider_profile.indexing_provider_kind is distinct from excluded.indexing_provider_kind
                   or runtime_provider_profile.indexing_model_name is distinct from excluded.indexing_model_name
                   or runtime_provider_profile.embedding_provider_kind is distinct from excluded.embedding_provider_kind
                   or runtime_provider_profile.embedding_model_name is distinct from excluded.embedding_model_name
                   or runtime_provider_profile.answer_provider_kind is distinct from excluded.answer_provider_kind
                   or runtime_provider_profile.answer_model_name is distinct from excluded.answer_model_name
                   or runtime_provider_profile.vision_provider_kind is distinct from excluded.vision_provider_kind
                   or runtime_provider_profile.vision_model_name is distinct from excluded.vision_model_name
                 then null
                 else runtime_provider_profile.last_validated_at
             end,
             last_validation_status = case
                 when runtime_provider_profile.indexing_provider_kind is distinct from excluded.indexing_provider_kind
                   or runtime_provider_profile.indexing_model_name is distinct from excluded.indexing_model_name
                   or runtime_provider_profile.embedding_provider_kind is distinct from excluded.embedding_provider_kind
                   or runtime_provider_profile.embedding_model_name is distinct from excluded.embedding_model_name
                   or runtime_provider_profile.answer_provider_kind is distinct from excluded.answer_provider_kind
                   or runtime_provider_profile.answer_model_name is distinct from excluded.answer_model_name
                   or runtime_provider_profile.vision_provider_kind is distinct from excluded.vision_provider_kind
                   or runtime_provider_profile.vision_model_name is distinct from excluded.vision_model_name
                 then null
                 else runtime_provider_profile.last_validation_status
             end,
             last_validation_error = case
                 when runtime_provider_profile.indexing_provider_kind is distinct from excluded.indexing_provider_kind
                   or runtime_provider_profile.indexing_model_name is distinct from excluded.indexing_model_name
                   or runtime_provider_profile.embedding_provider_kind is distinct from excluded.embedding_provider_kind
                   or runtime_provider_profile.embedding_model_name is distinct from excluded.embedding_model_name
                   or runtime_provider_profile.answer_provider_kind is distinct from excluded.answer_provider_kind
                   or runtime_provider_profile.answer_model_name is distinct from excluded.answer_model_name
                   or runtime_provider_profile.vision_provider_kind is distinct from excluded.vision_provider_kind
                   or runtime_provider_profile.vision_model_name is distinct from excluded.vision_model_name
                 then null
                 else runtime_provider_profile.last_validation_error
             end,
             updated_at = now()
         returning project_id, indexing_provider_kind, indexing_model_name, embedding_provider_kind,
            embedding_model_name, answer_provider_kind, answer_model_name, vision_provider_kind,
            vision_model_name, last_validated_at, last_validation_status, last_validation_error,
            created_at, updated_at",
    )
    .bind(project_id)
    .bind(indexing_provider_kind)
    .bind(indexing_model_name)
    .bind(embedding_provider_kind)
    .bind(embedding_model_name)
    .bind(answer_provider_kind)
    .bind(answer_model_name)
    .bind(vision_provider_kind)
    .bind(vision_model_name)
    .fetch_one(pool)
    .await
}

/// Loads the runtime provider profile for one project.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the provider profile.
pub async fn get_runtime_provider_profile(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Option<RuntimeProviderProfileRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeProviderProfileRow>(
        "select project_id, indexing_provider_kind, indexing_model_name, embedding_provider_kind,
            embedding_model_name, answer_provider_kind, answer_model_name, vision_provider_kind,
            vision_model_name, last_validated_at, last_validation_status, last_validation_error,
            created_at, updated_at
         from runtime_provider_profile
         where project_id = $1",
    )
    .bind(project_id)
    .fetch_optional(pool)
    .await
}

/// Updates the last validation outcome for one runtime provider profile.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the provider profile.
pub async fn update_runtime_provider_profile_validation(
    pool: &PgPool,
    project_id: Uuid,
    status: &str,
    error_message: Option<&str>,
) -> Result<RuntimeProviderProfileRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeProviderProfileRow>(
        "update runtime_provider_profile
         set last_validated_at = now(),
             last_validation_status = $2,
             last_validation_error = $3,
             updated_at = now()
         where project_id = $1
         returning project_id, indexing_provider_kind, indexing_model_name, embedding_provider_kind,
            embedding_model_name, answer_provider_kind, answer_model_name, vision_provider_kind,
            vision_model_name, last_validated_at, last_validation_status, last_validation_error,
            created_at, updated_at",
    )
    .bind(project_id)
    .bind(status)
    .bind(error_message)
    .fetch_one(pool)
    .await
}

/// Appends a provider validation log entry.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the validation log.
pub async fn append_runtime_provider_validation_log(
    pool: &PgPool,
    project_id: Option<Uuid>,
    provider_kind: &str,
    model_name: &str,
    capability: &str,
    status: &str,
    error_message: Option<&str>,
) -> Result<RuntimeProviderValidationLogRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeProviderValidationLogRow>(
        "insert into runtime_provider_validation_log (
            id, project_id, provider_kind, model_name, capability, status, error_message
         ) values ($1, $2, $3, $4, $5, $6, $7)
         returning id, project_id, provider_kind, model_name, capability, status, error_message, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(provider_kind)
    .bind(model_name)
    .bind(capability)
    .bind(status)
    .bind(error_message)
    .fetch_one(pool)
    .await
}

/// Loads one fresh query-intent cache entry eligible for reuse.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the cache entry.
pub async fn get_query_intent_cache_entry_for_reuse(
    pool: &PgPool,
    project_id: Uuid,
    normalized_question_hash: &str,
    explicit_mode: &str,
    source_truth_version: i64,
    at: DateTime<Utc>,
) -> Result<Option<QueryIntentCacheEntryRow>, sqlx::Error> {
    sqlx::query_as::<_, QueryIntentCacheEntryRow>(
        "select id, workspace_id, project_id, normalized_question_hash, explicit_mode, planned_mode,
            high_level_keywords_json, low_level_keywords_json, intent_summary, source_truth_version,
            status, created_at, last_used_at, expires_at
         from query_intent_cache_entry
         where project_id = $1
           and normalized_question_hash = $2
           and explicit_mode = $3
           and source_truth_version = $4
           and status = 'fresh'
           and expires_at > $5
         order by last_used_at desc
         limit 1",
    )
    .bind(project_id)
    .bind(normalized_question_hash)
    .bind(explicit_mode)
    .bind(source_truth_version)
    .bind(at)
    .fetch_optional(pool)
    .await
}

/// Loads the newest query-intent cache entry for one normalized question and mode.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the cache entry.
pub async fn find_latest_query_intent_cache_entry(
    pool: &PgPool,
    project_id: Uuid,
    normalized_question_hash: &str,
    explicit_mode: &str,
) -> Result<Option<QueryIntentCacheEntryRow>, sqlx::Error> {
    sqlx::query_as::<_, QueryIntentCacheEntryRow>(
        "select id, workspace_id, project_id, normalized_question_hash, explicit_mode, planned_mode,
            high_level_keywords_json, low_level_keywords_json, intent_summary, source_truth_version,
            status, created_at, last_used_at, expires_at
         from query_intent_cache_entry
         where project_id = $1
           and normalized_question_hash = $2
           and explicit_mode = $3
         order by source_truth_version desc, last_used_at desc
         limit 1",
    )
    .bind(project_id)
    .bind(normalized_question_hash)
    .bind(explicit_mode)
    .fetch_optional(pool)
    .await
}

/// Inserts or refreshes one query-intent cache entry.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating the cache entry.
pub async fn upsert_query_intent_cache_entry(
    pool: &PgPool,
    workspace_id: Uuid,
    project_id: Uuid,
    normalized_question_hash: &str,
    explicit_mode: &str,
    planned_mode: &str,
    high_level_keywords_json: serde_json::Value,
    low_level_keywords_json: serde_json::Value,
    intent_summary: Option<&str>,
    source_truth_version: i64,
    status: &str,
    expires_at: DateTime<Utc>,
) -> Result<QueryIntentCacheEntryRow, sqlx::Error> {
    sqlx::query_as::<_, QueryIntentCacheEntryRow>(
        "insert into query_intent_cache_entry (
            id, workspace_id, project_id, normalized_question_hash, explicit_mode, planned_mode,
            high_level_keywords_json, low_level_keywords_json, intent_summary, source_truth_version,
            status, last_used_at, expires_at
         ) values (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
            $11, now(), $12
         )
         on conflict (project_id, normalized_question_hash, explicit_mode, source_truth_version) do update
         set planned_mode = excluded.planned_mode,
             high_level_keywords_json = excluded.high_level_keywords_json,
             low_level_keywords_json = excluded.low_level_keywords_json,
             intent_summary = excluded.intent_summary,
             status = excluded.status,
             last_used_at = now(),
             expires_at = excluded.expires_at
         returning id, workspace_id, project_id, normalized_question_hash, explicit_mode, planned_mode,
            high_level_keywords_json, low_level_keywords_json, intent_summary, source_truth_version,
            status, created_at, last_used_at, expires_at",
    )
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind(project_id)
    .bind(normalized_question_hash)
    .bind(explicit_mode)
    .bind(planned_mode)
    .bind(high_level_keywords_json)
    .bind(low_level_keywords_json)
    .bind(intent_summary)
    .bind(source_truth_version)
    .bind(status)
    .bind(expires_at)
    .fetch_one(pool)
    .await
}

/// Touches a cache entry after successful reuse.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the cache entry.
pub async fn touch_query_intent_cache_entry(
    pool: &PgPool,
    id: Uuid,
    expires_at: DateTime<Utc>,
) -> Result<Option<QueryIntentCacheEntryRow>, sqlx::Error> {
    sqlx::query_as::<_, QueryIntentCacheEntryRow>(
        "update query_intent_cache_entry
         set status = 'fresh',
             last_used_at = now(),
             expires_at = $2
         where id = $1
         returning id, workspace_id, project_id, normalized_question_hash, explicit_mode, planned_mode,
            high_level_keywords_json, low_level_keywords_json, intent_summary, source_truth_version,
            status, created_at, last_used_at, expires_at",
    )
    .bind(id)
    .bind(expires_at)
    .fetch_optional(pool)
    .await
}

/// Marks cache entries stale when the library truth version changes.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the cache rows.
pub async fn mark_query_intent_cache_entries_stale_for_project(
    pool: &PgPool,
    project_id: Uuid,
    source_truth_version: i64,
) -> Result<u64, sqlx::Error> {
    sqlx::query(
        "update query_intent_cache_entry
         set status = 'stale'
         where project_id = $1
           and source_truth_version <> $2
           and status = 'fresh'",
    )
    .bind(project_id)
    .bind(source_truth_version)
    .execute(pool)
    .await
    .map(|result| result.rows_affected())
}

/// Prunes old cache entries while preserving the newest rows.
///
/// # Errors
/// Returns any `SQLx` error raised while deleting old cache rows.
pub async fn prune_query_intent_cache_entries_for_project(
    pool: &PgPool,
    project_id: Uuid,
    keep_limit: i64,
) -> Result<u64, sqlx::Error> {
    if keep_limit <= 0 {
        return Ok(0);
    }

    sqlx::query(
        "delete from query_intent_cache_entry
         where id in (
            select id
            from query_intent_cache_entry
            where project_id = $1
            order by
                case status
                    when 'fresh' then 0
                    when 'stale' then 1
                    else 2
                end asc,
                last_used_at desc,
                created_at desc
            offset $2
         )",
    )
    .bind(project_id)
    .bind(keep_limit)
    .execute(pool)
    .await
    .map(|result| result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn runtime_vector_target_batch_coalesces_duplicate_keys_last_write_wins() {
        let project_id = Uuid::now_v7();
        let target_id = Uuid::now_v7();
        let rows = coalesce_runtime_vector_target_upserts(&[
            RuntimeVectorTargetUpsertInput {
                project_id,
                target_kind: "entity".to_string(),
                target_id,
                provider_kind: "openai".to_string(),
                model_name: "text-embedding-3-small".to_string(),
                dimensions: Some(1536),
                embedding_json: json!([0.1, 0.2]),
            },
            RuntimeVectorTargetUpsertInput {
                project_id,
                target_kind: "entity".to_string(),
                target_id,
                provider_kind: "openai".to_string(),
                model_name: "text-embedding-3-small".to_string(),
                dimensions: Some(1536),
                embedding_json: json!([0.9, 1.0]),
            },
        ]);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].embedding_json, json!([0.9, 1.0]));
    }
}
