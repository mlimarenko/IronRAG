use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use ironrag_backend::app::config::Settings;
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
    /// List all users
    ListUsers,

    // ── API Token management ────────────────────────────────────────
    /// List all API tokens
    ListTokens,
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

    /// Backfill corpus-gloss acronym aliases for a library's graph nodes.
    ///
    /// Re-reads each multi-word entity node's evidence chunk text, applies the
    /// structural acronym detectors against the node label, and unions any
    /// detected short forms into the node's aliases. Idempotent: re-running
    /// attaches nothing new.
    BackfillAcronymAliases {
        /// Library slug
        library: String,
    },

    /// Backfill structural document parentage and roles for a library.
    ///
    /// Re-resolves declared parent keys, recovers parentage structurally for
    /// legacy rows from the attachment page id in their structural source, and
    /// finalizes each child's typed role from its media class. Idempotent:
    /// re-running resolves and updates nothing. Reports scanned / resolved /
    /// role-set counts plus the orphan and unresolved-image-children gate
    /// metrics.
    BackfillDocumentParents {
        /// Library slug
        library: String,
    },

    /// Print CLI build version
    Version,
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

async fn connect(database_url: &str) -> Result<PgPool> {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(database_url)
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

#[allow(
    clippy::print_stdout,
    reason = "this command prints the requested user listing for the operator"
)]
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

#[allow(
    clippy::print_stdout,
    reason = "this command prints the requested token listing for the operator"
)]
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
         where tp.principal_kind = 'api_token'
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

#[allow(
    clippy::print_stdout,
    reason = "this command prints the requested workspace listing for the operator"
)]
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

#[allow(
    clippy::print_stdout,
    reason = "this command reports the created workspace identifier to the operator"
)]
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

#[allow(
    clippy::print_stdout,
    reason = "this command prints the requested library listing for the operator"
)]
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

#[allow(
    clippy::print_stdout,
    reason = "this command reports the created library identifier to the operator"
)]
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

// ── Graph maintenance commands ──────────────────────────────────────

#[allow(
    clippy::print_stdout,
    reason = "this maintenance command reports backfill progress to the operator"
)]
async fn cmd_backfill_acronym_aliases(pool: &PgPool, library: &str) -> Result<()> {
    let library_id = resolve_library_id(pool, library).await?;
    println!("Backfilling acronym aliases for library '{library}' (id: {library_id})...");

    let report =
        ironrag_backend::services::graph::acronym_alias_backfill::backfill_acronym_aliases(
            pool, library_id,
        )
        .await
        .context("acronym alias backfill failed")?;

    println!(
        "Acronym alias backfill complete:\n  forward (short alias on multi-word node): {} nodes scanned, {} nodes updated, {} aliases added\n  reverse (full-form alias on short node):  {} nodes scanned, {} nodes updated, {} aliases added",
        report.nodes_scanned,
        report.nodes_updated,
        report.aliases_added,
        report.reverse_nodes_scanned,
        report.reverse_nodes_updated,
        report.reverse_aliases_added,
    );
    Ok(())
}

#[allow(
    clippy::print_stderr,
    clippy::print_stdout,
    reason = "this maintenance command reports backfill progress and warnings to the operator"
)]
async fn cmd_backfill_document_parents(pool: &PgPool, library: &str) -> Result<()> {
    let library_id = resolve_library_id(pool, library).await?;
    println!("Backfilling document parentage for library '{library}' (id: {library_id})...");

    let report =
        ironrag_backend::services::content::document_parent_backfill::backfill_document_parents(
            pool, library_id,
        )
        .await
        .context("document-parentage backfill failed")?;

    println!(
        "Document-parentage backfill complete: {} scanned, {} resolved, {} roles set, {} orphan image children, {} unresolved image children",
        report.scanned,
        report.resolved,
        report.role_set,
        report.orphan_image_children,
        report.unresolved_image_children,
    );
    if report.unresolved_image_children > 0 {
        eprintln!(
            "WARNING: {} raster-image children have a resolvable parent that was not attached; this must be zero",
            report.unresolved_image_children,
        );
    }
    Ok(())
}

// ── Main ────────────────────────────────────────────────────────────

#[allow(
    clippy::print_stderr,
    clippy::print_stdout,
    reason = "the CLI entry point prints version and actionable operator errors"
)]
#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Commands that don't need a database connection
    if let Commands::Version = cli.command {
        println!("ironrag-cli {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let mut settings = Settings::from_env().context("failed to load settings")?;
    settings.discard_credential_master_key();
    let pool = connect(&settings.database_url).await?;

    match cli.command {
        Commands::ListUsers => cmd_list_users(&pool).await,

        Commands::ListTokens => cmd_list_tokens(&pool).await,
        Commands::ListWorkspaces => cmd_list_workspaces(&pool).await,
        Commands::CreateWorkspace { slug, name } => {
            cmd_create_workspace(&pool, &slug, name.as_deref()).await
        }

        Commands::ListLibraries { workspace } => cmd_list_libraries(&pool, &workspace).await,
        Commands::CreateLibrary { workspace, slug, name, description } => {
            cmd_create_library(&pool, &workspace, &slug, name.as_deref(), description.as_deref())
                .await
        }

        Commands::BackfillAcronymAliases { library } => {
            cmd_backfill_acronym_aliases(&pool, &library).await
        }

        Commands::BackfillDocumentParents { library } => {
            cmd_backfill_document_parents(&pool, &library).await
        }

        Commands::Version => unreachable!("handled before pool init"),
    }
}
