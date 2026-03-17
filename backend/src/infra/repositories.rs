use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::domains::{
    pricing_catalog::{PricingBillingUnit, PricingCapability},
    usage_governance::{
        RuntimeStageBillingPolicy, decorate_payload_with_stage_ownership,
        runtime_stage_billing_policy, stage_native_ownership,
    },
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
            sqlx::query_as::<_, IngestionJobRow>(
                "select id, project_id, source_id, trigger_kind, status, stage, requested_by, error_message, started_at, finished_at, created_at, updated_at, idempotency_key, parent_job_id, attempt_count, worker_id, lease_expires_at, heartbeat_at, payload_json, result_json
                 from ingestion_job where project_id = $1 order by created_at desc",
            )
            .bind(project_id)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, IngestionJobRow>(
                "select id, project_id, source_id, trigger_kind, status, stage, requested_by, error_message, started_at, finished_at, created_at, updated_at, idempotency_key, parent_job_id, attempt_count, worker_id, lease_expires_at, heartbeat_at, payload_json, result_json
                 from ingestion_job order by created_at desc",
            )
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
    payload_json: serde_json::Value,
) -> Result<IngestionJobRow, sqlx::Error> {
    sqlx::query_as::<_, IngestionJobRow>(
        "insert into ingestion_job (id, project_id, source_id, trigger_kind, status, stage, requested_by, parent_job_id, idempotency_key, payload_json)
         values ($1, $2, $3, $4, 'queued', 'created', $5, $6, $7, $8)
         returning id, project_id, source_id, trigger_kind, status, stage, requested_by, error_message, started_at, finished_at, created_at, updated_at, idempotency_key, parent_job_id, attempt_count, worker_id, lease_expires_at, heartbeat_at, payload_json, result_json",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(source_id)
    .bind(trigger_kind)
    .bind(requested_by)
    .bind(parent_job_id)
    .bind(idempotency_key)
    .bind(payload_json)
    .fetch_one(pool)
    .await
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
pub struct ChatSessionRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub project_id: Uuid,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ChatSessionListRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub project_id: Uuid,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub message_count: i64,
    pub last_message_preview: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ChatMessageRow {
    pub id: Uuid,
    pub session_id: Uuid,
    pub project_id: Uuid,
    pub role: String,
    pub content: String,
    pub retrieval_run_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RetrievalRunRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub session_id: Option<Uuid>,
    pub query_text: String,
    pub model_profile_id: Option<Uuid>,
    pub top_k: i32,
    pub response_text: Option<String>,
    pub debug_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

/// Database repository helper: `list_retrieval_runs`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn list_retrieval_runs(
    pool: &PgPool,
    project_id: Option<Uuid>,
) -> Result<Vec<RetrievalRunRow>, sqlx::Error> {
    match project_id {
        Some(project_id) => {
            sqlx::query_as::<_, RetrievalRunRow>(
                "select id, project_id, session_id, query_text, model_profile_id, top_k, response_text, debug_json, created_at
                 from retrieval_run where project_id = $1 order by created_at desc",
            )
            .bind(project_id)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, RetrievalRunRow>(
                "select id, project_id, session_id, query_text, model_profile_id, top_k, response_text, debug_json, created_at
                 from retrieval_run order by created_at desc",
            )
            .fetch_all(pool)
            .await
        }
    }
}

/// Database repository helper: `create_retrieval_run`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn create_retrieval_run(
    pool: &PgPool,
    project_id: Uuid,
    session_id: Option<Uuid>,
    query_text: &str,
    model_profile_id: Option<Uuid>,
    top_k: i32,
    response_text: Option<&str>,
    debug_json: serde_json::Value,
) -> Result<RetrievalRunRow, sqlx::Error> {
    sqlx::query_as::<_, RetrievalRunRow>(
        "insert into retrieval_run (id, project_id, session_id, query_text, model_profile_id, top_k, response_text, debug_json)
         values ($1, $2, $3, $4, $5, $6, $7, $8)
         returning id, project_id, session_id, query_text, model_profile_id, top_k, response_text, debug_json, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(session_id)
    .bind(query_text)
    .bind(model_profile_id)
    .bind(top_k)
    .bind(response_text)
    .bind(debug_json)
    .fetch_one(pool)
    .await
}

pub async fn create_chat_session(
    pool: &PgPool,
    workspace_id: Uuid,
    project_id: Uuid,
    title: &str,
) -> Result<ChatSessionRow, sqlx::Error> {
    sqlx::query_as::<_, ChatSessionRow>(
        "insert into chat_session (id, workspace_id, project_id, title)
         values ($1, $2, $3, $4)
         returning id, workspace_id, project_id, title, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind(project_id)
    .bind(title)
    .fetch_one(pool)
    .await
}

pub async fn get_chat_session_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<ChatSessionRow>, sqlx::Error> {
    sqlx::query_as::<_, ChatSessionRow>(
        "select id, workspace_id, project_id, title, created_at, updated_at
         from chat_session where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

pub async fn list_chat_sessions_by_project(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Vec<ChatSessionListRow>, sqlx::Error> {
    sqlx::query_as::<_, ChatSessionListRow>(
        "select
            session.id,
            session.workspace_id,
            session.project_id,
            session.title,
            session.created_at,
            session.updated_at,
            count(message.id)::bigint as message_count,
            (
                array_agg(left(message.content, 180) order by message.created_at desc, message.id desc)
                filter (where message.id is not null)
            )[1] as last_message_preview
         from chat_session session
         left join chat_message message on message.session_id = session.id
         where session.project_id = $1
         group by session.id, session.workspace_id, session.project_id, session.title, session.created_at, session.updated_at
         order by session.updated_at desc, session.created_at desc",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
}

pub async fn create_chat_message(
    pool: &PgPool,
    session_id: Uuid,
    project_id: Uuid,
    role: &str,
    content: &str,
    retrieval_run_id: Option<Uuid>,
) -> Result<ChatMessageRow, sqlx::Error> {
    let message = sqlx::query_as::<_, ChatMessageRow>(
        "insert into chat_message (id, session_id, project_id, role, content, retrieval_run_id)
         values ($1, $2, $3, $4, $5, $6)
         returning id, session_id, project_id, role, content, retrieval_run_id, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(session_id)
    .bind(project_id)
    .bind(role)
    .bind(content)
    .bind(retrieval_run_id)
    .fetch_one(pool)
    .await?;

    sqlx::query("update chat_session set updated_at = now() where id = $1")
        .bind(session_id)
        .execute(pool)
        .await?;

    Ok(message)
}

pub async fn list_chat_messages_by_session(
    pool: &PgPool,
    session_id: Uuid,
) -> Result<Vec<ChatMessageRow>, sqlx::Error> {
    sqlx::query_as::<_, ChatMessageRow>(
        "select id, session_id, project_id, role, content, retrieval_run_id, created_at
         from chat_message where session_id = $1
         order by created_at asc, id asc",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await
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
pub struct ChunkEmbeddingRow {
    pub chunk_id: Uuid,
    pub project_id: Uuid,
    pub provider_kind: String,
    pub model_name: String,
    pub dimensions: i32,
    pub embedding_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Database repository helper: `upsert_chunk_embedding`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn upsert_chunk_embedding(
    pool: &PgPool,
    chunk_id: Uuid,
    project_id: Uuid,
    provider_kind: &str,
    model_name: &str,
    dimensions: i32,
    embedding_json: serde_json::Value,
) -> Result<ChunkEmbeddingRow, sqlx::Error> {
    sqlx::query_as::<_, ChunkEmbeddingRow>(
        "insert into chunk_embedding (chunk_id, project_id, provider_kind, model_name, dimensions, embedding_json)
         values ($1, $2, $3, $4, $5, $6)
         on conflict (chunk_id) do update set
           provider_kind = excluded.provider_kind,
           model_name = excluded.model_name,
           dimensions = excluded.dimensions,
           embedding_json = excluded.embedding_json,
           updated_at = now()
         returning chunk_id, project_id, provider_kind, model_name, dimensions, embedding_json, created_at, updated_at",
    )
    .bind(chunk_id)
    .bind(project_id)
    .bind(provider_kind)
    .bind(model_name)
    .bind(dimensions)
    .bind(embedding_json)
    .fetch_one(pool)
    .await
}

/// Database repository helper: `list_chunk_embeddings_by_project`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn list_chunk_embeddings_by_project(
    pool: &PgPool,
    project_id: Uuid,
    limit: i64,
) -> Result<Vec<ChunkEmbeddingRow>, sqlx::Error> {
    sqlx::query_as::<_, ChunkEmbeddingRow>(
        "select chunk_id, project_id, provider_kind, model_name, dimensions, embedding_json, created_at, updated_at
         from chunk_embedding where project_id = $1 order by updated_at desc limit $2",
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
pub async fn touch_api_token_last_used(pool: &PgPool, token_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("update api_token set last_used_at = now(), updated_at = now() where id = $1")
        .bind(token_id)
        .execute(pool)
        .await?;
    Ok(())
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

/// Loads an ingestion job by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the `ingestion_job` row.
pub async fn get_ingestion_job_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<IngestionJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestionJobRow>(
        "select id, project_id, source_id, trigger_kind, status, stage, requested_by, error_message, started_at, finished_at, created_at, updated_at, idempotency_key, parent_job_id, attempt_count, worker_id, lease_expires_at, heartbeat_at, payload_json, result_json
         from ingestion_job where id = $1",
    )
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
    sqlx::query_as::<_, IngestionJobRow>(
        "select id, project_id, source_id, trigger_kind, status, stage, requested_by, error_message,
            started_at, finished_at, created_at, updated_at, idempotency_key, parent_job_id,
            attempt_count, worker_id, lease_expires_at, heartbeat_at, payload_json, result_json
         from ingestion_job
         where payload_json ->> 'runtime_ingestion_run_id' = $1::text
         order by created_at asc, id asc",
    )
    .bind(runtime_ingestion_run_id)
    .fetch_all(pool)
    .await
}

/// Loads a retrieval run by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the `retrieval_run` row.
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
) -> Result<Vec<IngestionJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestionJobRow>(
        "update ingestion_job
         set status = 'queued',
             stage = 'requeued_after_lease_expiry',
             worker_id = null,
             lease_expires_at = null,
             error_message = null,
             updated_at = now()
         where status = 'running'
           and lease_expires_at is not null
           and lease_expires_at < now()
         returning id, project_id, source_id, trigger_kind, status, stage, requested_by, error_message, started_at, finished_at, created_at, updated_at, idempotency_key, parent_job_id, attempt_count, worker_id, lease_expires_at, heartbeat_at, payload_json, result_json",
    )
    .fetch_all(pool)
    .await
}

pub async fn recover_stale_ingestion_job_heartbeats(
    pool: &PgPool,
    stale_before: DateTime<Utc>,
) -> Result<Vec<IngestionJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestionJobRow>(
        "update ingestion_job
         set status = 'queued',
             stage = 'requeued_after_stale_heartbeat',
             worker_id = null,
             lease_expires_at = null,
             error_message = null,
             updated_at = now()
         where status = 'running'
           and finished_at is null
           and (
                (heartbeat_at is not null and heartbeat_at < $1)
                or (heartbeat_at is null and updated_at < $1)
           )
         returning id, project_id, source_id, trigger_kind, status, stage, requested_by, error_message, started_at, finished_at, created_at, updated_at, idempotency_key, parent_job_id, attempt_count, worker_id, lease_expires_at, heartbeat_at, payload_json, result_json",
    )
    .bind(stale_before)
    .fetch_all(pool)
    .await
}

pub async fn claim_next_ingestion_job(
    pool: &PgPool,
    worker_id: &str,
    lease_duration: chrono::Duration,
) -> Result<Option<IngestionJobRow>, sqlx::Error> {
    let lease_expires_at = Utc::now() + lease_duration;
    let claimed = sqlx::query_as::<_, IngestionJobRow>(
        "with running_project_load as (
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
            left join running_project_load as project_load
              on project_load.project_id = job.project_id
            left join running_workspace_load as workspace_load
              on workspace_load.workspace_id = project.workspace_id
            where job.status = 'queued'
              and (job.lease_expires_at is null or job.lease_expires_at < now())
            order by
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
         returning job.id, job.project_id, job.source_id, job.trigger_kind, job.status, job.stage, job.requested_by, job.error_message, job.started_at, job.finished_at, job.created_at, job.updated_at, job.idempotency_key, job.parent_job_id, job.attempt_count, job.worker_id, job.lease_expires_at, job.heartbeat_at, job.payload_json, job.result_json",
    )
    .bind(worker_id)
    .bind(lease_expires_at)
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
) -> Result<bool, sqlx::Error> {
    let lease_expires_at = Utc::now() + lease_duration;
    let updated = sqlx::query(
        "update ingestion_job
         set heartbeat_at = now(),
             lease_expires_at = $3
         where id = $1
           and worker_id = $2
           and status = 'running'
           and finished_at is null
           and (lease_expires_at is null or lease_expires_at >= now() - interval '5 minutes')",
    )
    .bind(job_id)
    .bind(worker_id)
    .bind(lease_expires_at)
    .execute(pool)
    .await?;

    Ok(updated.rows_affected() > 0)
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

pub async fn get_retrieval_run_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<RetrievalRunRow>, sqlx::Error> {
    sqlx::query_as::<_, RetrievalRunRow>(
        "select id, project_id, session_id, query_text, model_profile_id, top_k, response_text, debug_json, created_at
         from retrieval_run where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
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
pub struct RuntimeGraphSnapshotRow {
    pub project_id: Uuid,
    pub graph_status: String,
    pub projection_version: i64,
    pub node_count: i32,
    pub edge_count: i32,
    pub provenance_coverage_percent: Option<f64>,
    pub last_built_at: Option<DateTime<Utc>>,
    pub last_error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphNodeRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub canonical_key: String,
    pub label: String,
    pub node_type: String,
    pub aliases_json: serde_json::Value,
    pub summary: Option<String>,
    pub metadata_json: serde_json::Value,
    pub support_count: i32,
    pub projection_version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphEdgeRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub from_node_id: Uuid,
    pub to_node_id: Uuid,
    pub relation_type: String,
    pub canonical_key: String,
    pub summary: Option<String>,
    pub weight: Option<f64>,
    pub support_count: i32,
    pub metadata_json: serde_json::Value,
    pub projection_version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphEvidenceRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub target_kind: String,
    pub target_id: Uuid,
    pub document_id: Option<Uuid>,
    pub chunk_id: Option<Uuid>,
    pub source_file_name: Option<String>,
    pub page_ref: Option<String>,
    pub evidence_text: String,
    pub confidence_score: Option<f64>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphEvidenceLifecycleRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub target_kind: String,
    pub target_id: Uuid,
    pub document_id: Option<Uuid>,
    pub revision_id: Option<Uuid>,
    pub activated_by_attempt_id: Option<Uuid>,
    pub deactivated_by_mutation_id: Option<Uuid>,
    pub chunk_id: Option<Uuid>,
    pub source_file_name: Option<String>,
    pub page_ref: Option<String>,
    pub evidence_text: String,
    pub confidence_score: Option<f64>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphContributionCountsRow {
    pub node_count: i64,
    pub edge_count: i64,
    pub evidence_count: i64,
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
pub struct RuntimeQueryExecutionRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub mode: String,
    pub question: String,
    pub status: String,
    pub answer_text: Option<String>,
    pub grounding_status: String,
    pub provider_kind: String,
    pub model_name: String,
    pub debug_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeQueryReferenceRow {
    pub id: Uuid,
    pub query_execution_id: Uuid,
    pub reference_kind: String,
    pub reference_id: Uuid,
    pub excerpt: Option<String>,
    pub rank: i32,
    pub score: Option<f64>,
    pub metadata_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
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
) -> Result<RuntimeIngestionRunRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "update runtime_ingestion_run
         set activity_status = $3,
             last_activity_at = greatest(coalesce(last_activity_at, $2), $2),
             last_heartbeat_at = $2
         where id = $1
         returning id, project_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status, last_activity_at,
            last_heartbeat_at, provider_profile_snapshot_json, latest_error_message, current_attempt_no,
            attempt_kind, queue_started_at, started_at, finished_at, queue_elapsed_ms, total_elapsed_ms,
            created_at, updated_at",
    )
    .bind(id)
    .bind(last_heartbeat_at)
    .bind(activity_status)
    .fetch_one(pool)
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
            $19
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

/// Loads the active runtime graph snapshot for one project.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph snapshot.
pub async fn get_runtime_graph_snapshot(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Option<RuntimeGraphSnapshotRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphSnapshotRow>(
        "select project_id, graph_status, projection_version, node_count, edge_count,
            provenance_coverage_percent, last_built_at, last_error_message, created_at, updated_at
         from runtime_graph_snapshot
         where project_id = $1",
    )
    .bind(project_id)
    .fetch_optional(pool)
    .await
}

/// Returns the current source-truth version for one project.
///
/// # Errors
/// Returns any `SQLx` error raised while loading the graph snapshot row.
pub async fn get_project_source_truth_version(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<i64, sqlx::Error> {
    get_runtime_graph_snapshot(pool, project_id).await.map(|snapshot| {
        snapshot.map(|row| row.projection_version).filter(|value| *value > 0).unwrap_or(1)
    })
}

/// Upserts a runtime graph snapshot.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating the graph snapshot.
pub async fn upsert_runtime_graph_snapshot(
    pool: &PgPool,
    project_id: Uuid,
    graph_status: &str,
    projection_version: i64,
    node_count: i32,
    edge_count: i32,
    provenance_coverage_percent: Option<f64>,
    last_error_message: Option<&str>,
) -> Result<RuntimeGraphSnapshotRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphSnapshotRow>(
        "insert into runtime_graph_snapshot (
            project_id, graph_status, projection_version, node_count, edge_count,
            provenance_coverage_percent, last_built_at, last_error_message
         ) values ($1, $2, $3, $4, $5, $6, now(), $7)
         on conflict (project_id) do update
         set graph_status = excluded.graph_status,
             projection_version = excluded.projection_version,
             node_count = excluded.node_count,
             edge_count = excluded.edge_count,
             provenance_coverage_percent = excluded.provenance_coverage_percent,
             last_built_at = now(),
             last_error_message = excluded.last_error_message,
             updated_at = now()
         returning project_id, graph_status, projection_version, node_count, edge_count,
            provenance_coverage_percent, last_built_at, last_error_message, created_at, updated_at",
    )
    .bind(project_id)
    .bind(graph_status)
    .bind(projection_version)
    .bind(node_count)
    .bind(edge_count)
    .bind(provenance_coverage_percent)
    .bind(last_error_message)
    .fetch_one(pool)
    .await
}

/// Persists one filtered graph artifact for later diagnostics.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the filtered artifact row.
pub async fn create_runtime_graph_filtered_artifact(
    pool: &PgPool,
    project_id: Uuid,
    ingestion_run_id: Option<Uuid>,
    revision_id: Option<Uuid>,
    target_kind: &str,
    candidate_key: &str,
    source_node_key: Option<&str>,
    target_node_key: Option<&str>,
    relation_type: Option<&str>,
    filter_reason: &str,
    summary: Option<&str>,
    metadata_json: serde_json::Value,
) -> Result<RuntimeGraphFilteredArtifactRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphFilteredArtifactRow>(
        "insert into runtime_graph_filtered_artifact (
            id, project_id, ingestion_run_id, revision_id, target_kind, candidate_key,
            source_node_key, target_node_key, relation_type, filter_reason, summary, metadata_json
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
         returning id, project_id, ingestion_run_id, revision_id, target_kind, candidate_key,
            source_node_key, target_node_key, relation_type, filter_reason, summary, metadata_json, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(ingestion_run_id)
    .bind(revision_id)
    .bind(target_kind)
    .bind(candidate_key)
    .bind(source_node_key)
    .bind(target_node_key)
    .bind(relation_type)
    .bind(filter_reason)
    .bind(summary)
    .bind(metadata_json)
    .fetch_one(pool)
    .await
}

/// Lists filtered graph artifacts for one project.
///
/// # Errors
/// Returns any `SQLx` error raised while querying filtered artifact rows.
pub async fn list_runtime_graph_filtered_artifacts_by_project(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Vec<RuntimeGraphFilteredArtifactRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphFilteredArtifactRow>(
        "select id, project_id, ingestion_run_id, revision_id, target_kind, candidate_key,
            source_node_key, target_node_key, relation_type, filter_reason, summary, metadata_json, created_at
         from runtime_graph_filtered_artifact
         where project_id = $1
         order by created_at desc, id desc",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
}

/// Loads convergence and filtered-artifact counters for one project.
///
/// # Errors
/// Returns any `SQLx` error raised while querying convergence counters.
pub async fn load_runtime_graph_convergence_counters(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<RuntimeGraphConvergenceCountersRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphConvergenceCountersRow>(
        "with visible_runs as (
            select run.status
            from runtime_ingestion_run as run
            where run.project_id = $1
              and (
                    run.document_id is null
                    or not exists (
                        select 1
                        from document
                        where document.id = run.document_id
                          and document.deleted_at is not null
                    )
              )
         ),
         backlog as (
            select
                count(*) filter (where status = 'queued') as queued_document_count,
                count(*) filter (where status = 'processing') as processing_document_count,
                count(*) filter (where status = 'ready_no_graph') as ready_no_graph_count
            from visible_runs
         ),
         mutation_backlog as (
            select
                count(*) filter (
                    where active_mutation_kind in ('update_append', 'update_replace')
                      and active_mutation_status in ('accepted', 'reconciling')
                      and deleted_at is null
                ) as pending_update_count,
                count(*) filter (
                    where active_mutation_kind = 'delete'
                      and active_mutation_status in ('accepted', 'reconciling')
                ) as pending_delete_count
            from document
            where project_id = $1
         ),
         latest_failed_mutation as (
            select active_mutation_kind
            from document
            where project_id = $1
              and active_mutation_status = 'failed'
            order by updated_at desc, id desc
            limit 1
         ),
         filtered as (
            select
                count(distinct artifact_identity) as filtered_artifact_count,
                count(distinct case when filter_reason = 'empty_relation' then artifact_identity end) as filtered_empty_relation_count,
                count(distinct case when filter_reason = 'degenerate_self_loop' then artifact_identity end) as filtered_degenerate_loop_count
            from (
                select
                    concat_ws(
                        ':',
                        coalesce(artifact.revision_id::text, 'none'),
                        coalesce(artifact.ingestion_run_id::text, 'none'),
                        artifact.target_kind,
                        artifact.candidate_key,
                        artifact.filter_reason
                    ) as artifact_identity,
                    artifact.filter_reason
                from runtime_graph_filtered_artifact as artifact
                left join document_revision as revision
                    on revision.id = artifact.revision_id
                left join document as document
                    on document.id = revision.document_id
                where artifact.project_id = $1
                  and (
                        artifact.revision_id is null
                        or (
                            document.deleted_at is null
                            and (
                                document.current_revision_id = revision.id
                                or (
                                    document.current_revision_id is null
                                    and coalesce(revision.status, '') not in ('superseded', 'deleted', 'failed')
                                )
                            )
                        )
                  )
            ) as active_filtered
         )
         select
            coalesce(backlog.queued_document_count, 0) as queued_document_count,
            coalesce(backlog.processing_document_count, 0) as processing_document_count,
            coalesce(backlog.ready_no_graph_count, 0) as ready_no_graph_count,
            coalesce(mutation_backlog.pending_update_count, 0) as pending_update_count,
            coalesce(mutation_backlog.pending_delete_count, 0) as pending_delete_count,
            coalesce(filtered.filtered_artifact_count, 0) as filtered_artifact_count,
            coalesce(filtered.filtered_empty_relation_count, 0) as filtered_empty_relation_count,
            coalesce(filtered.filtered_degenerate_loop_count, 0) as filtered_degenerate_loop_count,
            latest_failed_mutation.active_mutation_kind as latest_failed_mutation_kind
         from backlog
         cross join mutation_backlog
         cross join filtered
         left join latest_failed_mutation on true",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await
}

/// Lists admitted runtime graph nodes for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying admitted graph nodes.
pub async fn list_admitted_runtime_graph_nodes_by_projection(
    pool: &PgPool,
    project_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "select node.id, node.project_id, node.canonical_key, node.label, node.node_type,
            node.aliases_json, node.summary, node.metadata_json, node.support_count,
            node.projection_version, node.created_at, node.updated_at
         from runtime_graph_node as node
         where node.project_id = $1
           and node.projection_version = $2
           and (
                node.node_type = 'document'
                or exists (
                    select 1
                    from runtime_graph_edge as edge
                    where edge.project_id = node.project_id
                      and edge.projection_version = node.projection_version
                      and (edge.from_node_id = node.id or edge.to_node_id = node.id)
                      and btrim(edge.relation_type) <> ''
                      and edge.from_node_id <> edge.to_node_id
                )
           )
         order by node.node_type asc, node.label asc, node.created_at asc",
    )
    .bind(project_id)
    .bind(projection_version)
    .fetch_all(pool)
    .await
}

/// Lists admitted runtime graph edges for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying admitted graph edges.
pub async fn list_admitted_runtime_graph_edges_by_projection(
    pool: &PgPool,
    project_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphEdgeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEdgeRow>(
        "select id, project_id, from_node_id, to_node_id, relation_type, canonical_key,
            summary, weight, support_count, metadata_json, projection_version, created_at, updated_at
         from runtime_graph_edge
         where project_id = $1
           and projection_version = $2
           and btrim(relation_type) <> ''
           and from_node_id <> to_node_id
         order by relation_type asc, created_at asc",
    )
    .bind(project_id)
    .bind(projection_version)
    .fetch_all(pool)
    .await
}

/// Upserts a canonical runtime graph node.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating the graph node.
pub async fn upsert_runtime_graph_node(
    pool: &PgPool,
    project_id: Uuid,
    canonical_key: &str,
    label: &str,
    node_type: &str,
    aliases_json: serde_json::Value,
    summary: Option<&str>,
    metadata_json: serde_json::Value,
    support_count: i32,
    projection_version: i64,
) -> Result<RuntimeGraphNodeRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "insert into runtime_graph_node (
            id, project_id, canonical_key, label, node_type, aliases_json, summary,
            metadata_json, support_count, projection_version
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         on conflict (project_id, canonical_key, projection_version) do update
         set label = excluded.label,
             node_type = excluded.node_type,
             aliases_json = excluded.aliases_json,
             summary = excluded.summary,
             metadata_json = excluded.metadata_json,
             support_count = excluded.support_count,
             updated_at = now()
         returning id, project_id, canonical_key, label, node_type, aliases_json, summary,
            metadata_json, support_count, projection_version, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(canonical_key)
    .bind(label)
    .bind(node_type)
    .bind(aliases_json)
    .bind(summary)
    .bind(metadata_json)
    .bind(support_count)
    .bind(projection_version)
    .fetch_one(pool)
    .await
}

/// Loads one canonical runtime graph node for a projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph node.
pub async fn get_runtime_graph_node_by_key(
    pool: &PgPool,
    project_id: Uuid,
    canonical_key: &str,
    projection_version: i64,
) -> Result<Option<RuntimeGraphNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "select id, project_id, canonical_key, label, node_type, aliases_json, summary,
            metadata_json, support_count, projection_version, created_at, updated_at
         from runtime_graph_node
         where project_id = $1 and canonical_key = $2 and projection_version = $3",
    )
    .bind(project_id)
    .bind(canonical_key)
    .bind(projection_version)
    .fetch_optional(pool)
    .await
}

/// Lists canonical runtime graph nodes for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph nodes.
pub async fn list_runtime_graph_nodes_by_projection(
    pool: &PgPool,
    project_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "select id, project_id, canonical_key, label, node_type, aliases_json, summary,
            metadata_json, support_count, projection_version, created_at, updated_at
         from runtime_graph_node
         where project_id = $1 and projection_version = $2
         order by node_type asc, label asc, created_at asc",
    )
    .bind(project_id)
    .bind(projection_version)
    .fetch_all(pool)
    .await
}

/// Upserts a canonical runtime graph edge.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating the graph edge.
pub async fn upsert_runtime_graph_edge(
    pool: &PgPool,
    project_id: Uuid,
    from_node_id: Uuid,
    to_node_id: Uuid,
    relation_type: &str,
    canonical_key: &str,
    summary: Option<&str>,
    weight: Option<f64>,
    support_count: i32,
    metadata_json: serde_json::Value,
    projection_version: i64,
) -> Result<RuntimeGraphEdgeRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEdgeRow>(
        "insert into runtime_graph_edge (
            id, project_id, from_node_id, to_node_id, relation_type, canonical_key, summary,
            weight, support_count, metadata_json, projection_version
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
         on conflict (project_id, canonical_key, projection_version) do update
         set from_node_id = excluded.from_node_id,
             to_node_id = excluded.to_node_id,
             relation_type = excluded.relation_type,
             summary = excluded.summary,
             weight = excluded.weight,
             support_count = excluded.support_count,
             metadata_json = excluded.metadata_json,
             updated_at = now()
         returning id, project_id, from_node_id, to_node_id, relation_type, canonical_key,
            summary, weight, support_count, metadata_json, projection_version, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(from_node_id)
    .bind(to_node_id)
    .bind(relation_type)
    .bind(canonical_key)
    .bind(summary)
    .bind(weight)
    .bind(support_count)
    .bind(metadata_json)
    .bind(projection_version)
    .fetch_one(pool)
    .await
}

/// Loads one canonical runtime graph edge for a projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph edge.
pub async fn get_runtime_graph_edge_by_key(
    pool: &PgPool,
    project_id: Uuid,
    canonical_key: &str,
    projection_version: i64,
) -> Result<Option<RuntimeGraphEdgeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEdgeRow>(
        "select id, project_id, from_node_id, to_node_id, relation_type, canonical_key,
            summary, weight, support_count, metadata_json, projection_version, created_at, updated_at
         from runtime_graph_edge
         where project_id = $1 and canonical_key = $2 and projection_version = $3",
    )
    .bind(project_id)
    .bind(canonical_key)
    .bind(projection_version)
    .fetch_optional(pool)
    .await
}

/// Lists canonical runtime graph edges for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph edges.
pub async fn list_runtime_graph_edges_by_projection(
    pool: &PgPool,
    project_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphEdgeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEdgeRow>(
        "select id, project_id, from_node_id, to_node_id, relation_type, canonical_key,
            summary, weight, support_count, metadata_json, projection_version, created_at, updated_at
         from runtime_graph_edge
         where project_id = $1 and projection_version = $2
         order by relation_type asc, created_at asc",
    )
    .bind(project_id)
    .bind(projection_version)
    .fetch_all(pool)
    .await
}

/// Resolves the latest canonical graph projection version available in SQL.
///
/// # Errors
/// Returns any `SQLx` error raised while aggregating node/edge projection versions.
pub async fn get_latest_runtime_graph_projection_version(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar::<_, Option<i64>>(
        "select max(projection_version) from (
             select projection_version
             from runtime_graph_node
             where project_id = $1
             union all
             select projection_version
             from runtime_graph_edge
             where project_id = $1
         ) as versions",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await
}

/// Creates a runtime graph evidence link.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the evidence record.
pub async fn create_runtime_graph_evidence(
    pool: &PgPool,
    project_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
    document_id: Option<Uuid>,
    revision_id: Option<Uuid>,
    activated_by_attempt_id: Option<Uuid>,
    chunk_id: Option<Uuid>,
    source_file_name: Option<&str>,
    page_ref: Option<&str>,
    evidence_text: &str,
    confidence_score: Option<f64>,
) -> Result<RuntimeGraphEvidenceRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEvidenceRow>(
        "insert into runtime_graph_evidence (
            id, project_id, target_kind, target_id, document_id, revision_id, activated_by_attempt_id,
            chunk_id, source_file_name, page_ref, evidence_text, confidence_score
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
         returning id, project_id, target_kind, target_id, document_id, chunk_id, source_file_name,
            page_ref, evidence_text, confidence_score, is_active, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(target_kind)
    .bind(target_id)
    .bind(document_id)
    .bind(revision_id)
    .bind(activated_by_attempt_id)
    .bind(chunk_id)
    .bind(source_file_name)
    .bind(page_ref)
    .bind(evidence_text)
    .bind(confidence_score)
    .fetch_one(pool)
    .await
}

/// Lists runtime graph evidence for one target.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the evidence rows.
pub async fn list_runtime_graph_evidence_by_target(
    pool: &PgPool,
    project_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
) -> Result<Vec<RuntimeGraphEvidenceRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEvidenceRow>(
        "select id, project_id, target_kind, target_id, document_id, chunk_id, source_file_name,
            page_ref, evidence_text, confidence_score, is_active, created_at
         from runtime_graph_evidence
         where project_id = $1 and target_kind = $2 and target_id = $3 and is_active = true
         order by created_at desc",
    )
    .bind(project_id)
    .bind(target_kind)
    .bind(target_id)
    .fetch_all(pool)
    .await
}

/// Marks all active graph evidence for one document as inactive.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the evidence rows.
pub async fn deactivate_runtime_graph_evidence_by_document(
    pool: &PgPool,
    project_id: Uuid,
    document_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "update runtime_graph_evidence
         set is_active = false
         where project_id = $1 and document_id = $2 and is_active = true",
    )
    .bind(project_id)
    .bind(document_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Lists active graph evidence rows for one logical document revision.
///
/// # Errors
/// Returns any `SQLx` error raised while querying revision-scoped evidence rows.
pub async fn list_active_runtime_graph_evidence_by_document_revision(
    pool: &PgPool,
    project_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
) -> Result<Vec<RuntimeGraphEvidenceLifecycleRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEvidenceLifecycleRow>(
        "select id, project_id, target_kind, target_id, document_id, revision_id,
            activated_by_attempt_id, deactivated_by_mutation_id, chunk_id, source_file_name,
            page_ref, evidence_text, confidence_score, is_active, created_at
         from runtime_graph_evidence
         where project_id = $1
           and document_id = $2
           and is_active = true
           and (revision_id = $3 or revision_id is null)
         order by created_at desc",
    )
    .bind(project_id)
    .bind(document_id)
    .bind(revision_id)
    .fetch_all(pool)
    .await
}

/// Marks active graph evidence for one logical document revision as inactive.
///
/// # Errors
/// Returns any `SQLx` error raised while updating revision-scoped evidence rows.
pub async fn deactivate_runtime_graph_evidence_by_document_revision(
    pool: &PgPool,
    project_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    mutation_id: Option<Uuid>,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "update runtime_graph_evidence
         set is_active = false,
             deactivated_by_mutation_id = coalesce($4, deactivated_by_mutation_id)
         where project_id = $1
           and document_id = $2
           and is_active = true
           and (revision_id = $3 or revision_id is null)",
    )
    .bind(project_id)
    .bind(document_id)
    .bind(revision_id)
    .bind(mutation_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Recalculates graph node/edge support counters from surviving active evidence.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the canonical graph rows.
pub async fn recalculate_runtime_graph_support_counts(
    pool: &PgPool,
    project_id: Uuid,
    projection_version: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update runtime_graph_node as node
         set support_count = coalesce((
             select count(*)::integer
             from runtime_graph_evidence as evidence
             where evidence.project_id = $1
               and evidence.target_kind = 'node'
               and evidence.target_id = node.id
               and evidence.is_active = true
         ), 0),
             updated_at = now()
         where node.project_id = $1 and node.projection_version = $2",
    )
    .bind(project_id)
    .bind(projection_version)
    .execute(pool)
    .await?;

    sqlx::query(
        "update runtime_graph_edge as edge
         set support_count = coalesce((
             select count(*)::integer
             from runtime_graph_evidence as evidence
             where evidence.project_id = $1
               and evidence.target_kind = 'edge'
               and evidence.target_id = edge.id
               and evidence.is_active = true
         ), 0),
             updated_at = now()
         where edge.project_id = $1 and edge.projection_version = $2",
    )
    .bind(project_id)
    .bind(projection_version)
    .execute(pool)
    .await?;

    Ok(())
}

/// Deletes canonical graph edges with zero surviving active evidence.
///
/// # Errors
/// Returns any `SQLx` error raised while pruning unsupported graph edges.
pub async fn delete_runtime_graph_edges_without_support(
    pool: &PgPool,
    project_id: Uuid,
    projection_version: i64,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "delete from runtime_graph_edge
         where project_id = $1
           and projection_version = $2
           and support_count <= 0",
    )
    .bind(project_id)
    .bind(projection_version)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Deletes canonical graph nodes with zero surviving active evidence.
///
/// # Errors
/// Returns any `SQLx` error raised while pruning unsupported graph nodes.
pub async fn delete_runtime_graph_nodes_without_support(
    pool: &PgPool,
    project_id: Uuid,
    projection_version: i64,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "delete from runtime_graph_node
         where project_id = $1
           and projection_version = $2
           and support_count <= 0",
    )
    .bind(project_id)
    .bind(projection_version)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Counts graph contributions that are still linked to one document.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the evidence rows.
pub async fn count_runtime_graph_contributions_by_document(
    pool: &PgPool,
    project_id: Uuid,
    document_id: Uuid,
) -> Result<RuntimeGraphContributionCountsRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphContributionCountsRow>(
        "select
            count(distinct case when target_kind = 'node' then target_id end) as node_count,
            count(distinct case when target_kind = 'edge' then target_id end) as edge_count,
            count(*) as evidence_count
         from runtime_graph_evidence
         where project_id = $1 and document_id = $2 and is_active = true",
    )
    .bind(project_id)
    .bind(document_id)
    .fetch_one(pool)
    .await
}

/// Counts active graph contributions linked to one logical document revision.
///
/// # Errors
/// Returns any `SQLx` error raised while querying revision-scoped evidence counts.
pub async fn count_runtime_graph_contributions_by_document_revision(
    pool: &PgPool,
    project_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
) -> Result<RuntimeGraphContributionCountsRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphContributionCountsRow>(
        "select
            count(distinct case when target_kind = 'node' then target_id end) as node_count,
            count(distinct case when target_kind = 'edge' then target_id end) as edge_count,
            count(*) as evidence_count
         from runtime_graph_evidence
         where project_id = $1
           and document_id = $2
           and is_active = true
           and (revision_id = $3 or revision_id is null)",
    )
    .bind(project_id)
    .bind(document_id)
    .bind(revision_id)
    .fetch_one(pool)
    .await
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
pub async fn delete_runtime_query_references_by_document(
    pool: &PgPool,
    project_id: Uuid,
    document_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "delete from runtime_query_reference as reference
         using runtime_query_execution as execution
         where reference.query_execution_id = execution.id
           and execution.project_id = $1
           and (
               (
                   reference.reference_kind = 'chunk'
                   and exists (
                       select 1
                       from chunk
                       where chunk.id = reference.reference_id
                         and chunk.document_id = $2
                   )
               )
               or (
                   reference.reference_kind = 'node'
                   and exists (
                       select 1
                       from runtime_graph_evidence as evidence
                       where evidence.project_id = $1
                         and evidence.document_id = $2
                         and evidence.target_kind = 'node'
                         and evidence.target_id = reference.reference_id
                   )
               )
               or (
                   reference.reference_kind = 'edge'
                   and exists (
                       select 1
                       from runtime_graph_evidence as evidence
                       where evidence.project_id = $1
                         and evidence.document_id = $2
                         and evidence.target_kind = 'edge'
                         and evidence.target_id = reference.reference_id
                   )
               )
           )",
    )
    .bind(project_id)
    .bind(document_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Deletes persisted query references that point at knowledge contributed by one document revision.
///
/// # Errors
/// Returns any `SQLx` error raised while deleting revision-scoped query references.
pub async fn delete_runtime_query_references_by_document_revision(
    pool: &PgPool,
    project_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "delete from runtime_query_reference as reference
         using runtime_query_execution as execution
         where reference.query_execution_id = execution.id
           and execution.project_id = $1
           and (
               (
                   reference.reference_kind = 'chunk'
                   and exists (
                       select 1
                       from chunk
                       where chunk.id = reference.reference_id
                         and chunk.document_id = $2
                   )
               )
               or (
                   reference.reference_kind = 'node'
                   and exists (
                       select 1
                       from runtime_graph_evidence as evidence
                       where evidence.project_id = $1
                         and evidence.document_id = $2
                         and evidence.target_kind = 'node'
                         and evidence.target_id = reference.reference_id
                         and (evidence.revision_id = $3 or evidence.revision_id is null)
                   )
               )
               or (
                   reference.reference_kind = 'edge'
                   and exists (
                       select 1
                       from runtime_graph_evidence as evidence
                       where evidence.project_id = $1
                         and evidence.document_id = $2
                         and evidence.target_kind = 'edge'
                         and evidence.target_id = reference.reference_id
                         and (evidence.revision_id = $3 or evidence.revision_id is null)
                   )
               )
           )",
    )
    .bind(project_id)
    .bind(document_id)
    .bind(revision_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
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

/// Creates a runtime query execution row.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the query execution.
pub async fn create_runtime_query_execution(
    pool: &PgPool,
    project_id: Uuid,
    mode: &str,
    question: &str,
    status: &str,
    answer_text: Option<&str>,
    grounding_status: &str,
    provider_kind: &str,
    model_name: &str,
    debug_json: serde_json::Value,
) -> Result<RuntimeQueryExecutionRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeQueryExecutionRow>(
        "insert into runtime_query_execution (
            id, project_id, mode, question, status, answer_text, grounding_status,
            provider_kind, model_name, debug_json, finished_at
         ) values (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
            case when $5 in ('completed', 'failed') then now() else null end
         )
         returning id, project_id, mode, question, status, answer_text, grounding_status,
            provider_kind, model_name, debug_json, created_at, finished_at",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(mode)
    .bind(question)
    .bind(status)
    .bind(answer_text)
    .bind(grounding_status)
    .bind(provider_kind)
    .bind(model_name)
    .bind(debug_json)
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

/// Invalidates every cache entry for one project.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the cache rows.
pub async fn invalidate_query_intent_cache_entries_for_project(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<u64, sqlx::Error> {
    sqlx::query(
        "update query_intent_cache_entry
         set status = 'invalidated'
         where project_id = $1
           and status <> 'invalidated'",
    )
    .bind(project_id)
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

/// Loads one runtime query execution row by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the execution row.
pub async fn get_runtime_query_execution_by_id(
    pool: &PgPool,
    query_execution_id: Uuid,
) -> Result<Option<RuntimeQueryExecutionRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeQueryExecutionRow>(
        "select id, project_id, mode, question, status, answer_text, grounding_status,
            provider_kind, model_name, debug_json, created_at, finished_at
         from runtime_query_execution
         where id = $1",
    )
    .bind(query_execution_id)
    .fetch_optional(pool)
    .await
}

/// Creates a runtime query reference row.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the query reference.
pub async fn create_runtime_query_reference(
    pool: &PgPool,
    query_execution_id: Uuid,
    reference_kind: &str,
    reference_id: Uuid,
    excerpt: Option<&str>,
    rank: i32,
    score: Option<f64>,
    metadata_json: serde_json::Value,
) -> Result<RuntimeQueryReferenceRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeQueryReferenceRow>(
        "insert into runtime_query_reference (
            id, query_execution_id, reference_kind, reference_id, excerpt, rank, score, metadata_json
         ) values ($1, $2, $3, $4, $5, $6, $7, $8)
         returning id, query_execution_id, reference_kind, reference_id, excerpt, rank, score, metadata_json, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(query_execution_id)
    .bind(reference_kind)
    .bind(reference_id)
    .bind(excerpt)
    .bind(rank)
    .bind(score)
    .bind(metadata_json)
    .fetch_one(pool)
    .await
}

/// Lists persisted runtime query references for one execution.
///
/// # Errors
/// Returns any `SQLx` error raised while querying persisted query references.
pub async fn list_runtime_query_references_by_execution(
    pool: &PgPool,
    query_execution_id: Uuid,
) -> Result<Vec<RuntimeQueryReferenceRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeQueryReferenceRow>(
        "select id, query_execution_id, reference_kind, reference_id, excerpt, rank, score, metadata_json, created_at
         from runtime_query_reference
         where query_execution_id = $1
         order by rank asc, created_at asc",
    )
    .bind(query_execution_id)
    .fetch_all(pool)
    .await
}
