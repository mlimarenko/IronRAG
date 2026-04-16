#![allow(clippy::print_stdout, clippy::print_stderr)]

use anyhow::{Context, Result, bail};
use argon2::{
    Argon2,
    password_hash::{PasswordHasher, SaltString, rand_core::OsRng},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use ironrag_backend::app::config::Settings;
use rand::Rng;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "ironrag-cli", about = "IronRAG admin CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Reset user password
    ResetPassword {
        /// User login
        login: String,
        /// New password (min 8 chars)
        password: String,
    },
    /// Create a new admin user
    CreateUser {
        /// User login
        login: String,
        /// Password (min 8 chars)
        password: String,
        /// Display name
        #[arg(short, long)]
        name: Option<String>,
        /// Permissions to grant (can specify multiple). Default: iam_admin.
        /// Available: iam_admin, workspace_admin, workspace_read, library_read,
        /// library_write, document_read, document_write, query_run, ops_read,
        /// audit_read, credential_admin, binding_admin
        #[arg(short = 'p', long)]
        permission: Vec<String>,
    },
    /// Delete a user
    DeleteUser {
        /// User login
        login: String,
    },
    /// List all users
    ListUsers,

    // ── API Token management ────────────────────────────────────────
    /// Create an API token for a user
    CreateToken {
        /// User login who owns the token
        login: String,
        /// Token label
        #[arg(short, long, default_value = "api-token")]
        label: String,
        /// Workspace scope (optional -- limits token to this workspace)
        #[arg(short, long)]
        workspace: Option<String>,
        /// Permissions to grant (can specify multiple). Default: iam_admin.
        /// Available: iam_admin, workspace_admin, workspace_read, library_read,
        /// library_write, document_read, document_write, query_run, ops_read,
        /// audit_read, credential_admin, binding_admin
        #[arg(short, long)]
        permission: Vec<String>,
        /// Resource scope for permission grants: 'system', 'workspace:<slug>', 'library:<slug>'
        /// Default: 'system' (global). Only used with non-iam_admin permissions.
        #[arg(long)]
        scope: Option<String>,
    },
    /// List all API tokens
    ListTokens,
    /// Revoke an API token
    RevokeToken {
        /// Token principal ID (UUID)
        token_id: String,
    },

    // ── Workspace management ────────────────────────────────────────
    /// List all workspaces
    ListWorkspaces,
    /// Create a new workspace
    CreateWorkspace {
        /// Workspace slug
        slug: String,
        /// Display name
        #[arg(short, long)]
        name: Option<String>,
    },

    // ── Library management ──────────────────────────────────────────
    /// List libraries in a workspace
    ListLibraries {
        /// Workspace slug or ID
        workspace: String,
    },
    /// Create a new library
    CreateLibrary {
        /// Workspace slug or ID
        workspace: String,
        /// Library slug
        slug: String,
        /// Display name
        #[arg(short, long)]
        name: Option<String>,
        /// Description
        #[arg(short, long)]
        description: Option<String>,
    },

    /// Print CLI build version
    Version,
}

fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("password hashing failed: {e}"))?;
    Ok(hash.to_string())
}

fn validate_password(password: &str) -> Result<()> {
    if password.len() < 8 {
        bail!("password must be at least 8 characters");
    }
    Ok(())
}

#[derive(Debug, FromRow)]
struct UserListRow {
    pub login: String,
    pub display_name: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, FromRow)]
struct TokenListRow {
    pub principal_id: Uuid,
    pub label: String,
    pub token_prefix: String,
    pub status: String,
    pub issued_at: DateTime<Utc>,
    pub owner: Option<String>,
    pub workspace_id: Option<Uuid>,
}

#[derive(Debug, FromRow)]
struct WorkspaceListRow {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_state: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, FromRow)]
struct LibraryListRow {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_state: String,
    pub created_at: DateTime<Utc>,
}

async fn connect(settings: &Settings) -> Result<PgPool> {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&settings.database_url)
        .await
        .context("failed to connect to database")
}

/// Resolve a workspace slug-or-UUID to a workspace ID.
async fn resolve_workspace_id(pool: &PgPool, slug_or_id: &str) -> Result<Uuid> {
    if let Ok(id) = slug_or_id.parse::<Uuid>() {
        let exists = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from catalog_workspace where id = $1",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .context("failed to look up workspace by ID")?;
        if exists == 0 {
            bail!("workspace with ID '{slug_or_id}' not found");
        }
        return Ok(id);
    }

    sqlx::query_scalar::<_, Uuid>("select id from catalog_workspace where slug = $1")
        .bind(slug_or_id)
        .fetch_optional(pool)
        .await
        .context("failed to look up workspace")?
        .ok_or_else(|| anyhow::anyhow!("workspace '{slug_or_id}' not found"))
}

/// Resolve a library slug to its UUID (looks in the default workspace).
async fn resolve_library_id(pool: &PgPool, slug: &str) -> Result<Uuid> {
    sqlx::query_scalar::<_, Uuid>("select id from catalog_library where slug = $1 limit 1")
        .bind(slug)
        .fetch_optional(pool)
        .await
        .context("failed to look up library")?
        .ok_or_else(|| anyhow::anyhow!("library '{slug}' not found"))
}

/// Parse a scope string into (resource_kind, resource_id).
async fn parse_scope(pool: &PgPool, scope: &str) -> Result<(&'static str, Uuid)> {
    if scope == "system" {
        return Ok(("system", Uuid::nil()));
    }
    if let Some(slug) = scope.strip_prefix("workspace:") {
        let id = resolve_workspace_id(pool, slug).await?;
        return Ok(("workspace", id));
    }
    if let Some(slug) = scope.strip_prefix("library:") {
        let id = resolve_library_id(pool, slug).await?;
        return Ok(("library", id));
    }
    bail!("invalid scope '{scope}': expected 'system', 'workspace:<slug>', or 'library:<slug>'");
}

const VALID_PERMISSIONS: &[&str] = &[
    "iam_admin",
    "workspace_admin",
    "workspace_read",
    "library_read",
    "library_write",
    "document_read",
    "document_write",
    "query_run",
    "ops_read",
    "audit_read",
    "credential_admin",
    "binding_admin",
];

/// Resolve resource_kind and resource_id for a permission, using explicit scope,
/// workspace flag, or the permission's natural default.
async fn resolve_permission_scope(
    pool: &PgPool,
    perm: &str,
    explicit_scope: Option<&str>,
    workspace: Option<&str>,
) -> Result<(&'static str, Uuid)> {
    if let Some(scope) = explicit_scope {
        return parse_scope(pool, scope).await;
    }
    match perm {
        "iam_admin" | "ops_read" | "audit_read" => Ok(("system", Uuid::nil())),
        p if p.starts_with("workspace_") || p == "credential_admin" || p == "binding_admin" => {
            match workspace {
                Some(ws) => {
                    let id = resolve_workspace_id(pool, ws).await?;
                    Ok(("workspace", id))
                }
                None => Ok(("system", Uuid::nil())),
            }
        }
        p if p.starts_with("library_") || p.starts_with("document_") || p == "query_run" => {
            match workspace {
                Some(ws) => {
                    let id = resolve_workspace_id(pool, ws).await?;
                    Ok(("workspace", id))
                }
                None => Ok(("system", Uuid::nil())),
            }
        }
        _ => Ok(("system", Uuid::nil())),
    }
}

/// Insert iam_grant rows for a list of permissions.
async fn insert_grants(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    principal_id: Uuid,
    granted_by: Option<Uuid>,
    permissions: &[String],
    explicit_scope: Option<&str>,
    workspace: Option<&str>,
    pool: &PgPool,
) -> Result<Vec<String>> {
    let perms: Vec<String> =
        if permissions.is_empty() { vec!["iam_admin".to_string()] } else { permissions.to_vec() };

    for p in &perms {
        if !VALID_PERMISSIONS.contains(&p.as_str()) {
            bail!("unknown permission '{p}'. Valid: {}", VALID_PERMISSIONS.join(", "));
        }
    }

    let mut granted: Vec<String> = Vec::new();
    for perm in &perms {
        let (resource_kind, resource_id) =
            resolve_permission_scope(pool, perm, explicit_scope, workspace).await?;

        sqlx::query(
            "insert into iam_grant (id, principal_id, resource_kind, resource_id, permission_kind, granted_by_principal_id, granted_at, expires_at)
             values ($1, $2, $3::iam_grant_resource_kind, $4, $5::iam_permission_kind, $6, now(), null)",
        )
        .bind(Uuid::now_v7())
        .bind(principal_id)
        .bind(resource_kind)
        .bind(resource_id)
        .bind(perm.as_str())
        .bind(granted_by)
        .execute(&mut **tx)
        .await
        .context(format!("failed to grant permission '{perm}'"))?;

        granted.push(format!("{perm} on {resource_kind}:{resource_id}"));
    }
    Ok(granted)
}

// ── User commands ───────────────────────────────────────────────────

async fn cmd_reset_password(pool: &PgPool, login: &str, password: &str) -> Result<()> {
    validate_password(password)?;
    let password_hash = hash_password(password)?;

    let result =
        sqlx::query("update iam_user set password_hash = $1 where lower(login) = lower($2)")
            .bind(&password_hash)
            .bind(login)
            .execute(pool)
            .await
            .context("failed to update password")?;

    if result.rows_affected() == 0 {
        bail!("user '{login}' not found");
    }

    // Invalidate all sessions for this user
    sqlx::query(
        "update iam_session set revoked_at = now()
         where principal_id = (select principal_id from iam_user where lower(login) = lower($1))
           and revoked_at is null",
    )
    .bind(login)
    .execute(pool)
    .await
    .context("failed to revoke sessions")?;

    println!("Password reset for '{login}'");
    Ok(())
}

async fn cmd_create_user(
    pool: &PgPool,
    login: &str,
    password: &str,
    name: Option<&str>,
    permissions: &[String],
) -> Result<()> {
    validate_password(password)?;

    let existing = sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint from iam_user where lower(login) = lower($1)",
    )
    .bind(login)
    .fetch_one(pool)
    .await
    .context("failed to check existing user")?;

    if existing > 0 {
        bail!("user '{login}' already exists");
    }

    let display_name = name.unwrap_or(login);
    let email = format!("{login}@ironrag.local");
    let password_hash = hash_password(password)?;
    let principal_id = Uuid::now_v7();

    let mut tx = pool.begin().await.context("failed to start transaction")?;

    sqlx::query(
        "insert into iam_principal (id, principal_kind, display_label, status, parent_principal_id, created_at, disabled_at)
         values ($1, 'user', $2, 'active', null, now(), null)",
    )
    .bind(principal_id)
    .bind(display_name)
    .execute(&mut *tx)
    .await
    .context("failed to create principal")?;

    sqlx::query(
        "insert into iam_user (principal_id, login, email, display_name, password_hash, auth_provider_kind, external_subject)
         values ($1, $2, $3, $4, $5, 'password', null)",
    )
    .bind(principal_id)
    .bind(login)
    .bind(&email)
    .bind(display_name)
    .bind(&password_hash)
    .execute(&mut *tx)
    .await
    .context("failed to create user")?;

    // Grant permissions (defaults to iam_admin if none specified)
    let granted = insert_grants(&mut tx, principal_id, None, permissions, None, None, pool).await?;

    // Add to default workspace if one exists
    let default_workspace_id = sqlx::query_scalar::<_, Uuid>(
        "select id from catalog_workspace where slug = 'default' limit 1",
    )
    .fetch_optional(&mut *tx)
    .await
    .context("failed to look up default workspace")?;

    if let Some(workspace_id) = default_workspace_id {
        sqlx::query(
            "insert into iam_workspace_membership (workspace_id, principal_id, membership_state, joined_at, ended_at)
             values ($1, $2, 'active', now(), null)
             on conflict (workspace_id, principal_id) do nothing",
        )
        .bind(workspace_id)
        .bind(principal_id)
        .execute(&mut *tx)
        .await
        .context("failed to add workspace membership")?;
    }

    tx.commit().await.context("failed to commit transaction")?;

    println!("Created user '{login}'");
    println!("  Permissions:");
    for g in &granted {
        println!("    - {g}");
    }
    Ok(())
}

async fn cmd_delete_user(pool: &PgPool, login: &str) -> Result<()> {
    let principal_id = sqlx::query_scalar::<_, Uuid>(
        "select principal_id from iam_user where lower(login) = lower($1)",
    )
    .bind(login)
    .fetch_optional(pool)
    .await
    .context("failed to look up user")?
    .ok_or_else(|| anyhow::anyhow!("user '{login}' not found"))?;

    let mut tx = pool.begin().await.context("failed to start transaction")?;

    sqlx::query("delete from iam_session where principal_id = $1")
        .bind(principal_id)
        .execute(&mut *tx)
        .await?;

    sqlx::query("delete from iam_grant where principal_id = $1")
        .bind(principal_id)
        .execute(&mut *tx)
        .await?;

    sqlx::query("delete from iam_workspace_membership where principal_id = $1")
        .bind(principal_id)
        .execute(&mut *tx)
        .await?;

    sqlx::query("delete from iam_user where principal_id = $1")
        .bind(principal_id)
        .execute(&mut *tx)
        .await?;

    sqlx::query("delete from iam_principal where id = $1")
        .bind(principal_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await.context("failed to commit transaction")?;

    println!("Deleted user '{login}'");
    Ok(())
}

async fn cmd_list_users(pool: &PgPool) -> Result<()> {
    let users = sqlx::query_as::<_, UserListRow>(
        "select u.login, u.display_name, p.status::text as status, p.created_at
         from iam_user u
         join iam_principal p on p.id = u.principal_id
         order by p.created_at asc",
    )
    .fetch_all(pool)
    .await
    .context("failed to list users")?;

    if users.is_empty() {
        println!("No users found.");
        return Ok(());
    }

    println!("{:<20} {:<24} {:<10} CREATED", "LOGIN", "DISPLAY NAME", "STATUS");
    println!("{}", "-".repeat(78));
    for user in &users {
        println!(
            "{:<20} {:<24} {:<10} {}",
            user.login,
            user.display_name,
            user.status,
            user.created_at.format("%Y-%m-%d %H:%M:%S"),
        );
    }
    println!("\n{} user(s) total", users.len());

    Ok(())
}

// ── Token commands ──────────────────────────────────────────────────

async fn cmd_create_token(
    pool: &PgPool,
    login: &str,
    label: &str,
    workspace: Option<&str>,
    permissions: &[String],
    scope: Option<&str>,
) -> Result<()> {
    // 1. Find user principal
    let user_principal_id = sqlx::query_scalar::<_, Uuid>(
        "select principal_id from iam_user where lower(login) = lower($1)",
    )
    .bind(login)
    .fetch_optional(pool)
    .await
    .context("failed to look up user")?
    .ok_or_else(|| anyhow::anyhow!("user '{login}' not found"))?;

    // 2. Resolve optional workspace
    let workspace_id = match workspace {
        Some(ws) => Some(resolve_workspace_id(pool, ws).await?),
        None => None,
    };

    // 3. Generate plaintext token
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);
    let token = format!("irt_{}", URL_SAFE_NO_PAD.encode(bytes));

    // 4. SHA-256 hash for storage
    let digest = Sha256::digest(token.as_bytes());
    let secret_hash = hex::encode(digest);

    // 5. Token prefix (first 12 chars)
    let token_prefix = &token[..12];

    let token_principal_id = Uuid::now_v7();
    let mut tx = pool.begin().await.context("failed to start transaction")?;

    // Create iam_principal for the token
    sqlx::query(
        "insert into iam_principal (id, principal_kind, display_label, status, parent_principal_id, created_at, disabled_at)
         values ($1, 'api_token', $2, 'active', $3, now(), null)",
    )
    .bind(token_principal_id)
    .bind(label)
    .bind(user_principal_id)
    .execute(&mut *tx)
    .await
    .context("failed to create token principal")?;

    // Create iam_api_token
    sqlx::query(
        "insert into iam_api_token (principal_id, workspace_id, label, token_prefix, status, issued_by_principal_id)
         values ($1, $2, $3, $4, 'active', $5)",
    )
    .bind(token_principal_id)
    .bind(workspace_id)
    .bind(label)
    .bind(token_prefix)
    .bind(user_principal_id)
    .execute(&mut *tx)
    .await
    .context("failed to create api token")?;

    // Create iam_api_token_secret
    sqlx::query(
        "insert into iam_api_token_secret (token_principal_id, secret_version, secret_hash, issued_at, revoked_at)
         values ($1, 1, $2, now(), null)",
    )
    .bind(token_principal_id)
    .bind(&secret_hash)
    .execute(&mut *tx)
    .await
    .context("failed to create token secret")?;

    // Grant permissions (defaults to iam_admin if none specified)
    let granted = insert_grants(
        &mut tx,
        token_principal_id,
        Some(user_principal_id),
        permissions,
        scope,
        workspace,
        pool,
    )
    .await?;

    tx.commit().await.context("failed to commit transaction")?;

    println!("Created API token for user '{login}'");
    println!("  Principal ID: {token_principal_id}");
    println!("  Label:        {label}");
    if let Some(ws) = workspace {
        println!("  Workspace:    {ws}");
    }
    println!("  Permissions:");
    for g in &granted {
        println!("    - {g}");
    }
    println!();
    println!("  Token (shown once): {token}");

    Ok(())
}

async fn cmd_list_tokens(pool: &PgPool) -> Result<()> {
    let tokens = sqlx::query_as::<_, TokenListRow>(
        "select t.principal_id, t.label, t.token_prefix, t.status::text as status,
                s.issued_at,
                u.login as owner, t.workspace_id
         from iam_api_token t
         join iam_principal tp on tp.id = t.principal_id
         left join iam_user u on u.principal_id = t.issued_by_principal_id
         left join lateral (
             select issued_at from iam_api_token_secret
             where token_principal_id = t.principal_id
             order by secret_version desc limit 1
         ) s on true
         order by s.issued_at",
    )
    .fetch_all(pool)
    .await
    .context("failed to list tokens")?;

    if tokens.is_empty() {
        println!("No API tokens found.");
        return Ok(());
    }

    println!(
        "{:<38} {:<16} {:<14} {:<10} {:<20} {:<14} WORKSPACE",
        "PRINCIPAL ID", "LABEL", "PREFIX", "STATUS", "ISSUED", "OWNER"
    );
    println!("{}", "-".repeat(125));
    for t in &tokens {
        let workspace =
            t.workspace_id.map(|id| id.to_string()).unwrap_or_else(|| "system".to_string());
        println!(
            "{:<38} {:<16} {:<14} {:<10} {:<20} {:<14} {}",
            t.principal_id,
            t.label,
            t.token_prefix,
            t.status,
            t.issued_at.format("%Y-%m-%d %H:%M:%S"),
            t.owner.as_deref().unwrap_or("-"),
            workspace,
        );
    }
    println!("\n{} token(s) total", tokens.len());

    Ok(())
}

async fn cmd_revoke_token(pool: &PgPool, token_id: &str) -> Result<()> {
    let principal_id: Uuid = token_id.parse().context("invalid UUID for token_id")?;

    let mut tx = pool.begin().await.context("failed to start transaction")?;

    let result = sqlx::query(
        "update iam_api_token set status = 'revoked', revoked_at = now() where principal_id = $1",
    )
    .bind(principal_id)
    .execute(&mut *tx)
    .await
    .context("failed to revoke token")?;

    if result.rows_affected() == 0 {
        bail!("token with principal_id '{token_id}' not found");
    }

    sqlx::query("update iam_principal set status = 'revoked', disabled_at = now() where id = $1")
        .bind(principal_id)
        .execute(&mut *tx)
        .await
        .context("failed to update principal status")?;

    tx.commit().await.context("failed to commit transaction")?;

    println!("Revoked token '{token_id}'");
    Ok(())
}

// ── Workspace commands ──────────────────────────────────────────────

async fn cmd_list_workspaces(pool: &PgPool) -> Result<()> {
    let rows = sqlx::query_as::<_, WorkspaceListRow>(
        "select id, slug, display_name, lifecycle_state::text as lifecycle_state, created_at
         from catalog_workspace
         order by created_at asc",
    )
    .fetch_all(pool)
    .await
    .context("failed to list workspaces")?;

    if rows.is_empty() {
        println!("No workspaces found.");
        return Ok(());
    }

    println!("{:<38} {:<20} {:<24} {:<10} CREATED", "ID", "SLUG", "DISPLAY NAME", "STATE");
    println!("{}", "-".repeat(110));
    for w in &rows {
        println!(
            "{:<38} {:<20} {:<24} {:<10} {}",
            w.id,
            w.slug,
            w.display_name,
            w.lifecycle_state,
            w.created_at.format("%Y-%m-%d %H:%M:%S"),
        );
    }
    println!("\n{} workspace(s) total", rows.len());

    Ok(())
}

async fn cmd_create_workspace(pool: &PgPool, slug: &str, name: Option<&str>) -> Result<()> {
    let display_name = name.unwrap_or(slug);
    let id = Uuid::now_v7();

    sqlx::query("insert into catalog_workspace (id, slug, display_name) values ($1, $2, $3)")
        .bind(id)
        .bind(slug)
        .bind(display_name)
        .execute(pool)
        .await
        .context("failed to create workspace")?;

    println!("Created workspace '{slug}' (id: {id})");
    Ok(())
}

// ── Library commands ────────────────────────────────────────────────

async fn cmd_list_libraries(pool: &PgPool, workspace: &str) -> Result<()> {
    let workspace_id = resolve_workspace_id(pool, workspace).await?;

    let rows = sqlx::query_as::<_, LibraryListRow>(
        "select id, slug, display_name, lifecycle_state::text as lifecycle_state, created_at
         from catalog_library
         where workspace_id = $1
         order by created_at asc",
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await
    .context("failed to list libraries")?;

    if rows.is_empty() {
        println!("No libraries found in workspace '{workspace}'.");
        return Ok(());
    }

    println!("{:<38} {:<20} {:<24} {:<10} CREATED", "ID", "SLUG", "DISPLAY NAME", "STATE");
    println!("{}", "-".repeat(110));
    for lib in &rows {
        println!(
            "{:<38} {:<20} {:<24} {:<10} {}",
            lib.id,
            lib.slug,
            lib.display_name,
            lib.lifecycle_state,
            lib.created_at.format("%Y-%m-%d %H:%M:%S"),
        );
    }
    println!("\n{} library(ies) total", rows.len());

    Ok(())
}

async fn cmd_create_library(
    pool: &PgPool,
    workspace: &str,
    slug: &str,
    name: Option<&str>,
    description: Option<&str>,
) -> Result<()> {
    let workspace_id = resolve_workspace_id(pool, workspace).await?;
    let display_name = name.unwrap_or(slug);
    let id = Uuid::now_v7();

    sqlx::query(
        "insert into catalog_library (id, workspace_id, slug, display_name, description) values ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(workspace_id)
    .bind(slug)
    .bind(display_name)
    .bind(description)
    .execute(pool)
    .await
    .context("failed to create library")?;

    println!("Created library '{slug}' in workspace '{workspace}' (id: {id})");
    Ok(())
}

// ── Main ────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Commands that don't need a database connection
    if let Commands::Version = cli.command {
        println!("ironrag-cli {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let settings = Settings::from_env().context("failed to load settings")?;
    let pool = connect(&settings).await?;

    match cli.command {
        Commands::ResetPassword { login, password } => {
            cmd_reset_password(&pool, &login, &password).await
        }
        Commands::CreateUser { login, password, name, permission } => {
            cmd_create_user(&pool, &login, &password, name.as_deref(), &permission).await
        }
        Commands::DeleteUser { login } => cmd_delete_user(&pool, &login).await,
        Commands::ListUsers => cmd_list_users(&pool).await,

        Commands::CreateToken { login, label, workspace, permission, scope } => {
            cmd_create_token(
                &pool,
                &login,
                &label,
                workspace.as_deref(),
                &permission,
                scope.as_deref(),
            )
            .await
        }
        Commands::ListTokens => cmd_list_tokens(&pool).await,
        Commands::RevokeToken { token_id } => cmd_revoke_token(&pool, &token_id).await,

        Commands::ListWorkspaces => cmd_list_workspaces(&pool).await,
        Commands::CreateWorkspace { slug, name } => {
            cmd_create_workspace(&pool, &slug, name.as_deref()).await
        }

        Commands::ListLibraries { workspace } => cmd_list_libraries(&pool, &workspace).await,
        Commands::CreateLibrary { workspace, slug, name, description } => {
            cmd_create_library(&pool, &workspace, &slug, name.as_deref(), description.as_deref())
                .await
        }
        Commands::Version => unreachable!("handled before pool init"),
    }
}
