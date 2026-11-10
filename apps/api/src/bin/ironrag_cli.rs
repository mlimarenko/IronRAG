use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use ironrag_backend::app::config::Settings;
use sqlx::{FromRow, PgPool};
use std::time::Duration;
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "ironrag-cli", about = "IronRAG admin CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Print the immutable policy fingerprint of a financial admission policy.
    ///
    /// Reads one bounded JSON `FinancialAdmission` document from stdin, uses the
    /// canonical `immutable_fingerprint`, and prints only the `sha256:` string.
    /// No database, provider client or runtime settings are initialized first,
    /// so it works while the estate is paused.
    FinancialPolicyFingerprint,

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

    /// Check a local API liveness or readiness endpoint without loading settings
    Probe {
        #[arg(long, value_enum, default_value = "health")]
        endpoint: ProbeEndpoint,
        #[arg(long, default_value_t = 8080, value_parser = clap::value_parser!(u16).range(1..))]
        port: u16,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ProbeEndpoint {
    Health,
    Ready,
}

impl ProbeEndpoint {
    fn path(self) -> &'static str {
        match self {
            Self::Health => "/v1/health",
            Self::Ready => "/v1/ready",
        }
    }
}

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const PROBE_BODY_LIMIT: usize = 64 * 1024;

async fn cmd_probe(endpoint: ProbeEndpoint, port: u16) -> Result<()> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(PROBE_TIMEOUT)
        .build()
        .context("failed to create local probe client")?;
    let mut response = client
        .get(format!("http://127.0.0.1:{port}{}", endpoint.path()))
        .send()
        .await
        .context("local probe request failed")?;
    if !response.status().is_success() {
        bail!("local probe returned HTTP {}", response.status());
    }
    if response.content_length().is_some_and(|length| length > PROBE_BODY_LIMIT as u64) {
        bail!("local probe response body exceeds 64 KiB");
    }
    let mut remaining = PROBE_BODY_LIMIT;
    while let Some(chunk) = response.chunk().await.context("failed to read local probe body")? {
        if chunk.len() > remaining {
            bail!("local probe response body exceeds 64 KiB");
        }
        remaining -= chunk.len();
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

/// Upper bound on a policy document read from stdin.
const FINANCIAL_POLICY_MAX_BYTES: u64 = 1_048_576;

/// Compute the canonical immutable-policy fingerprint without echoing the input.
fn cmd_financial_policy_fingerprint() -> Result<String> {
    use ironrag_backend::infra::repositories::ai_repository::FinancialAdmission;
    use std::io::Read as _;

    let mut raw = Vec::new();
    std::io::stdin()
        .lock()
        .take(FINANCIAL_POLICY_MAX_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|_| anyhow::anyhow!("financial policy input could not be read"))?;
    if raw.len() as u64 > FINANCIAL_POLICY_MAX_BYTES {
        bail!("financial policy input exceeds the {FINANCIAL_POLICY_MAX_BYTES} byte limit");
    }
    let policy: FinancialAdmission = serde_json::from_slice(&raw)
        .map_err(|_| anyhow::anyhow!("financial policy input is not a valid admission document"))?;
    let fingerprint = policy
        .immutable_fingerprint()
        .map_err(|_| anyhow::anyhow!("financial policy fingerprint could not be computed"))?;
    Ok(fingerprint)
}

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
    if let Commands::Probe { endpoint, port } = cli.command {
        return cmd_probe(endpoint, port).await;
    }
    if let Commands::FinancialPolicyFingerprint = cli.command {
        println!("{}", cmd_financial_policy_fingerprint()?);
        return Ok(());
    }

    let mut settings = Settings::from_env().context("failed to load settings")?;
    settings.discard_credential_master_key();
    let pool = connect(&settings.database_url).await?;

    match cli.command {
        Commands::FinancialPolicyFingerprint => {
            // Handled before configuration load; reaching here means the
            // dispatch order broke.
            eprintln!("financial-policy-fingerprint must run before configuration load");
            std::process::exit(2);
        }
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

        Commands::Version | Commands::Probe { .. } => unreachable!("handled before pool init"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio::time::{Instant, timeout};

    const TEST_TIMEOUT: Duration = Duration::from_secs(10);

    async fn probe_response(
        endpoint: ProbeEndpoint,
        response: Vec<u8>,
        stall: bool,
    ) -> Result<(Result<()>, Vec<u8>)> {
        timeout(TEST_TIMEOUT, async move {
            let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
            let port = listener.local_addr()?.port();
            let (release, released) = oneshot::channel();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await?;
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    if request.len() >= 4096 {
                        bail!("test request headers exceed fixture bound");
                    }
                    request.push(socket.read_u8().await?);
                }
                socket.write_all(&response).await?;
                if stall {
                    released.await.context("fixture release dropped")?;
                }
                Ok::<_, anyhow::Error>(request)
            });
            let result = cmd_probe(endpoint, port).await;
            // Closing the sender also releases a stalled fixture on an early error.
            let released = release.send(());
            let request = server.await??;
            if stall && released.is_err() {
                bail!("stalled fixture exited before probe completion");
            }
            Ok((result, request))
        })
        .await
        .context("outer test deadline expired; native probe timeout did not complete")?
    }

    #[test]
    fn probe_defaults() -> Result<()> {
        let cli = Cli::try_parse_from(["ironrag-cli", "probe"])?;
        assert!(matches!(
            cli.command,
            Commands::Probe { endpoint: ProbeEndpoint::Health, port: 8080 }
        ));
        Ok(())
    }

    #[test]
    fn probe_rejects_invalid_arguments() {
        for port in ["0", "65536", "-1", "not-a-port"] {
            assert!(Cli::try_parse_from(["ironrag-cli", "probe", "--port", port]).is_err());
        }
        for endpoint in ["unknown", "/v1/health", "http://example.com"] {
            assert!(Cli::try_parse_from(["ironrag-cli", "probe", "--endpoint", endpoint]).is_err());
        }
        assert!(
            Cli::try_parse_from(["ironrag-cli", "probe", "--url", "http://example.com"]).is_err()
        );
    }

    #[test]
    fn probe_accepts_port_boundaries_and_ready() -> Result<()> {
        for port in ["1", "65535"] {
            let cli = Cli::try_parse_from([
                "ironrag-cli",
                "probe",
                "--endpoint",
                "ready",
                "--port",
                port,
            ])?;
            assert!(matches!(cli.command, Commands::Probe { endpoint: ProbeEndpoint::Ready, .. }));
        }
        Ok(())
    }

    #[tokio::test]
    async fn probe_health_complete_response() -> Result<()> {
        let (result, request) = probe_response(
            ProbeEndpoint::Health,
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
            false,
        )
        .await?;
        result?;
        assert!(request.starts_with(b"GET /v1/health HTTP/1.1\r\n"));
        Ok(())
    }

    #[tokio::test]
    async fn probe_ready_complete_chunked_response() -> Result<()> {
        let (result, request) = probe_response(
            ProbeEndpoint::Ready,
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n\r\n".to_vec(),
            false,
        )
        .await?;
        result?;
        assert!(request.starts_with(b"GET /v1/ready HTTP/1.1\r\n"));
        Ok(())
    }

    #[tokio::test]
    async fn probe_rejects_unsuccessful_statuses() -> Result<()> {
        for status in ["503 Service Unavailable", "404 Not Found"] {
            let (result, _) = probe_response(
                ProbeEndpoint::Ready,
                format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n").into_bytes(),
                false,
            )
            .await?;
            let error = result.err().context("unsuccessful status was accepted")?;
            assert!(error.to_string().contains(status));
        }
        Ok(())
    }

    #[tokio::test]
    async fn probe_refuses_redirects() -> Result<()> {
        let (result, _) = probe_response(
            ProbeEndpoint::Health,
            b"HTTP/1.1 302 Found\r\nLocation: /v1/ready\r\nContent-Length: 0\r\n\r\n".to_vec(),
            false,
        )
        .await?;
        let error = result.err().context("redirect was accepted")?;
        assert!(error.to_string().contains("HTTP 302"));
        Ok(())
    }

    #[tokio::test]
    async fn probe_rejects_refused_connection() -> Result<()> {
        // Reserve the port without listening, avoiding a free-port rebind race.
        let socket = tokio::net::TcpSocket::new_v4()?;
        socket.bind((std::net::Ipv4Addr::LOCALHOST, 0).into())?;
        let result =
            timeout(TEST_TIMEOUT, cmd_probe(ProbeEndpoint::Health, socket.local_addr()?.port()))
                .await?;
        let error = result.err().context("connection without a listener was accepted")?;
        assert!(error.downcast_ref::<reqwest::Error>().is_some_and(reqwest::Error::is_connect));
        Ok(())
    }

    #[tokio::test]
    async fn probe_rejects_malformed_and_truncated_bodies() -> Result<()> {
        for response in [
            &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\ninvalid\r\n"[..],
            &b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort"[..],
        ] {
            let (result, _) =
                probe_response(ProbeEndpoint::Health, response.to_vec(), false).await?;
            let error = result.err().context("invalid body was accepted")?;
            assert!(error.downcast_ref::<reqwest::Error>().is_some());
        }
        Ok(())
    }

    #[tokio::test]
    async fn probe_body_limit() -> Result<()> {
        let mut response =
            format!("HTTP/1.1 200 OK\r\nContent-Length: {PROBE_BODY_LIMIT}\r\n\r\n").into_bytes();
        response.extend(vec![b'x'; PROBE_BODY_LIMIT]);
        probe_response(ProbeEndpoint::Health, response, false).await?.0?;

        for response in [
            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", PROBE_BODY_LIMIT + 1)
                .into_bytes(),
            {
                let mut response = format!(
                    "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n",
                    PROBE_BODY_LIMIT + 1
                )
                .into_bytes();
                response.extend(vec![b'x'; PROBE_BODY_LIMIT + 1]);
                response.extend_from_slice(b"\r\n0\r\n\r\n");
                response
            },
        ] {
            let (result, _) = probe_response(ProbeEndpoint::Health, response, false).await?;
            let error = result.err().context("oversized body was accepted")?;
            assert!(error.to_string().contains("exceeds 64 KiB"));
        }
        Ok(())
    }

    async fn assert_native_timeout(response: Vec<u8>) -> Result<()> {
        let started = Instant::now();
        let (result, _) = probe_response(ProbeEndpoint::Health, response, true).await?;
        let error = result.err().context("stalled response was accepted")?;
        assert!(error.downcast_ref::<reqwest::Error>().is_some_and(reqwest::Error::is_timeout));
        assert!(started.elapsed() >= PROBE_TIMEOUT);
        Ok(())
    }

    #[tokio::test]
    async fn probe_stalled_headers_use_native_timeout() -> Result<()> {
        assert_native_timeout(Vec::new()).await
    }

    #[tokio::test]
    async fn probe_stalled_body_uses_native_timeout() -> Result<()> {
        assert_native_timeout(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\no".to_vec()).await
    }
}
