//! `ironrag-maintenance` — canonical operator CLI for the maintenance
//! pipeline. Subcommands cover the same surface the background scheduler
//! exposes (audit / gc / repair / retention / migrate / rebuild / index /
//! lease management), so an operator can either dry-run, force-run, or
//! inspect lease state for any maintenance class without writing ad-hoc
//! SQL.
//!
//! Phase B (this revision) only ships the **read-only audit and lease
//! inspection** surface. Destructive subcommands land as the matching
//! sweeper modules (`gc`, `retention`, `repair`, …) come online in later
//! phases. The clap layout is finalised now so future subcommands plug
//! into the already-released `--help` tree without renames.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::repositories::{catalog_repository, webhook_repository},
    services::maintenance::{
        audit,
        credential_secrets::{self, CredentialSecretMigrationOptions},
        gc::{self, GcStaleChunksOptions, LibraryGcReport},
        knowledge_projection_metadata,
        lease::{self, MaintenanceClass, MaintenanceJobRun, Scope, StateCounts},
        migrate, orphan_knowledge_documents, rebuild, repair, retention, vector_profile_keys,
        webhook_outbox_ops::{
            self, MAX_WEBHOOK_LIFECYCLE_OUTBOX_AUDIT_LIMIT,
            MAX_WEBHOOK_LIFECYCLE_OUTBOX_RESOLUTION_REASON_CODE_BYTES,
            WebhookLifecycleOutboxAuditCursor, WebhookLifecycleOutboxAuditOptions,
            WebhookLifecycleOutboxDispatchState,
        },
    },
};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Parser)]
#[command(
    name = "ironrag-maintenance",
    about = "IronRAG maintenance operator CLI — audit, gc, repair, retention, lease control"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Read-only inspection commands. Safe to run any time.
    #[command(subcommand)]
    Audit(AuditCommand),
    /// Garbage-collect content that is no longer reachable from a
    /// canonical head.
    #[command(subcommand)]
    Gc(GcCommand),
    /// Operator-driven state recovery passes.
    #[command(subcommand)]
    Repair(RepairCommand),
    /// TTL-based retention sweepers for INSERT-only history tables.
    #[command(subcommand)]
    Retention(RetentionCommand),
    /// One-shot data migrations. Each command is idempotent — a second
    /// run after success is a no-op.
    #[command(subcommand)]
    Migrate(MigrateCommand),
    /// Heavy operator-only rebuild passes. Never recurring.
    #[command(subcommand)]
    Rebuild(RebuildCommand),
    /// Inspect and control the durable scheduler lease table.
    #[command(subcommand)]
    Lease(LeaseCommand),
}

#[derive(Subcommand)]
enum AuditCommand {
    /// Top Postgres tables + their size, live/dead tuple counts, last
    /// autovacuum. Stand-in for "what is eating my disk".
    StorageSummary {
        /// Maximum rows to return.
        #[arg(long, default_value_t = 30)]
        limit: i64,
        /// Emit JSON instead of a human-readable table.
        #[arg(long)]
        json: bool,
    },
    /// Per-index size + scan count for the largest tables. Used to spot
    /// dead indexes (0 scans) and bloat candidates.
    IndexBloat {
        /// Restrict to indexes on these tables (comma-separated). Default
        /// is the canonical short-list of write-heavy tables.
        #[arg(long, value_delimiter = ',')]
        tables: Vec<String>,
        /// Minimum index size in megabytes to report.
        #[arg(long, default_value_t = 10)]
        min_size_mb: i64,
        /// Emit JSON instead of a human-readable table.
        #[arg(long)]
        json: bool,
    },
    /// Documents whose `content_document_head` carries no readable or
    /// active revision. Includes recovery_attempts_count and
    /// dead_letter_at so the operator can see how the retry budget has
    /// been spent.
    NullHeadDocs {
        /// Optional library filter.
        #[arg(long)]
        library: Option<Uuid>,
        /// Maximum rows to return.
        #[arg(long, default_value_t = 50)]
        limit: i64,
        /// Emit JSON instead of a human-readable table.
        #[arg(long)]
        json: bool,
    },
    /// Scan every knowledge-plane table for rows whose `library_id` does not
    /// match a live PostgreSQL `catalog_library` row. Read-only — destructive
    /// purge is `gc orphan-libraries`.
    OrphanLibraries {
        /// Emit JSON instead of a human-readable summary.
        #[arg(long)]
        json: bool,
    },
    /// Inspect a bounded, redacted lifecycle-webhook outbox projection.
    /// Defaults to dead-letter rows. Payloads, URLs, credentials, headers,
    /// lease identities, and raw errors are never loaded or printed.
    WebhookOutbox {
        /// Restrict to one durable dispatch state.
        #[arg(
            long,
            default_value = "dead-letter",
            value_parser = parse_webhook_outbox_state
        )]
        state: WebhookLifecycleOutboxDispatchState,
        /// Restrict to one library.
        #[arg(long)]
        library: Option<Uuid>,
        /// Keyset cursor timestamp returned by the previous page. Must be
        /// supplied together with `--before-id`.
        #[arg(long)]
        before_created_at: Option<chrono::DateTime<chrono::Utc>>,
        /// Keyset cursor UUID returned by the previous page. Must be supplied
        /// together with `--before-created-at`.
        #[arg(long)]
        before_id: Option<Uuid>,
        /// Maximum rows to return (hard-capped at 500).
        #[arg(long, default_value_t = 100, value_parser = parse_webhook_outbox_limit)]
        limit: i64,
        /// Emit JSON instead of human-readable rows.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum GcCommand {
    /// Delete chunks (and their vectors across every dim shard) for
    /// revisions that are no longer the readable/active head of their
    /// document. Holds the per-library graph advisory lock and refuses
    /// to run if any ingest job for the library is in flight.
    StaleChunks {
        /// Restrict to one library. Default: every library.
        #[arg(long)]
        library: Option<Uuid>,
        /// Count what would be removed without issuing destructive deletes.
        #[arg(long)]
        dry_run: bool,
        /// Also sweep documents whose head is null (failed ingest).
        /// Off by default: such documents may still be recoverable, and
        /// an aggressive sweep would erase the only physical trace.
        #[arg(long)]
        include_null_head: bool,
        /// Emit JSON instead of a human-readable summary.
        #[arg(long)]
        json: bool,
    },
    /// Delete `runtime_graph_evidence` rows whose revision is no
    /// longer the readable/active head of the source document, plus
    /// rows whose `chunk_id` points at a chunk that has already been
    /// swept. Holds the per-library graph advisory lock and refuses
    /// to run while any ingest job for the library is in flight.
    StaleEvidence {
        /// Restrict to one library. Default: every library.
        #[arg(long)]
        library: Option<Uuid>,
        /// Emit JSON instead of a human-readable summary.
        #[arg(long)]
        json: bool,
    },
    /// Wipe every knowledge-plane row whose `library_id` no longer matches a
    /// live PostgreSQL `catalog_library` row. Destructive — requires `--yes` to
    /// confirm.
    OrphanLibraries {
        /// Confirm destructive run. Without `--yes` the command refuses
        /// to issue any deletes; pair with the `audit orphan-libraries`
        /// command for the read-only inventory.
        #[arg(long)]
        yes: bool,
        /// Emit JSON instead of a human-readable summary.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum RepairCommand {
    /// Reconcile drifted knowledge-projection metadata (document
    /// external_key/parent/role; revision number/kind/source_uri/
    /// document_hint/mime/checksum/title/byte_size) from the canonical
    /// content plane. Heals parity-gate metadata drift without touching
    /// chunk text. Idempotent.
    KnowledgeProjectionMetadata {
        /// Restrict to one library. Default: every library.
        #[arg(long)]
        library: Option<Uuid>,
        /// Count rows that would be updated without writing.
        #[arg(long)]
        dry_run: bool,
        /// Emit JSON instead of a human-readable summary.
        #[arg(long)]
        json: bool,
    },
    /// Purge knowledge-plane documents whose canonical `content_document`
    /// row no longer exists. One such orphan permanently blocks its whole
    /// library behind the readable-content parity gate
    /// (`query_content_projection_converging`). Sweeps chunk vectors and
    /// graph evidence in the same transaction, advances the library
    /// source-truth generation, and re-projects the runtime graph.
    OrphanKnowledgeDocuments {
        /// Restrict to one library. Default: every library.
        #[arg(long)]
        library: Option<Uuid>,
        /// Report what would be removed without writing.
        #[arg(long)]
        dry_run: bool,
        /// Emit JSON instead of a human-readable summary.
        #[arg(long)]
        json: bool,
    },
    /// Promote `content_document_head` for documents whose head is
    /// currently null on both readable and active pointers but who
    /// have at least one chunk-bearing revision in the canonical
    /// store. Uses the same `promote_document_head` path the ingest
    /// pipeline uses on success. Idempotent.
    NullHeads {
        /// Restrict to one library. Default: every library.
        #[arg(long)]
        library: Option<Uuid>,
        /// Emit JSON instead of a human-readable summary.
        #[arg(long)]
        json: bool,
    },
    /// Same recovery pass as `null-heads`, but with the rate-limit
    /// the auto-recovery scheduler enforces: every failure records
    /// `recovery_attempts_count` / `last_recovery_error_code` /
    /// `last_recovery_attempt_at` on `content_document_head`, three
    /// consecutive same-error failures stamp `dead_letter_at`, and
    /// any document touched in the last hour is skipped so a flaky
    /// provider does not burn the whole budget in seconds. Operator
    /// use: prefer this over `null-heads` when re-driving recovery
    /// against the same library repeatedly.
    NullHeadsAuto {
        /// Restrict to one library. Default: every library.
        #[arg(long)]
        library: Option<Uuid>,
        /// Emit JSON instead of a human-readable summary.
        #[arg(long)]
        json: bool,
    },
    /// Clear the `dead_letter_at` mark and recovery counters for one
    /// document. Use after diagnosing and fixing the underlying
    /// failure that drove the doc into dead-letter; the next
    /// auto-recovery pass picks it up again.
    ClearRecoveryDeadLetter {
        #[arg(long)]
        document: Uuid,
    },
    /// Requeue one exact lifecycle outbox UUID only if it is currently
    /// dead-lettered. This resets retry/lease/error state but does not send
    /// the webhook; the ordinary worker delivers it later.
    WebhookOutboxDeadLetter {
        #[arg(long)]
        outbox: Uuid,
        /// Emit JSON instead of a human-readable confirmation.
        #[arg(long)]
        json: bool,
    },
    /// Permanently resolve one exact lifecycle-outbox dead-letter without
    /// claiming delivery. The canonical reason code and timestamp are stored
    /// on the row and in the durable redacted audit log.
    WebhookOutboxDeadLetterResolve {
        #[arg(long)]
        outbox: Uuid,
        /// Bounded machine-readable reason (lowercase snake_case, 1..=64
        /// ASCII bytes). Free-form or secret-bearing text is rejected.
        #[arg(long, value_parser = parse_webhook_outbox_resolution_reason_code)]
        reason_code: String,
        /// Confirm that the event will be terminally discarded without being
        /// reported as delivered.
        #[arg(long)]
        acknowledge_not_delivered: bool,
        /// Emit JSON instead of a human-readable confirmation.
        #[arg(long)]
        json: bool,
    },
    /// Force-abandon delivery owners that keep a tombstoned subscription in
    /// draining state. The remote endpoint may already have accepted a POST;
    /// this requires an explicit duplicate-delivery risk acknowledgement.
    WebhookDeliveryAbandon {
        #[arg(long)]
        subscription: Uuid,
        #[arg(long)]
        acknowledge_duplicate_delivery_risk: bool,
        /// Emit JSON instead of a human-readable confirmation.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum MigrateCommand {
    /// Inventory or rewrap non-current AI account and webhook secret envelopes.
    /// Without `--apply` this command is read-only. A valid dedicated
    /// credential master key is required in both modes.
    CredentialSecrets {
        /// Apply optimistic compare-and-update writes. Omit for dry-run.
        #[arg(long)]
        apply: bool,
        /// Maximum rows loaded per table batch (1..=1000).
        #[arg(long, default_value_t = 100)]
        batch_size: usize,
        /// Emit JSON aggregate counts instead of human-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Backfill `occurred_at` / `occurred_until` on chunks whose
    /// `normalized_text` carries a canonical JSONL temporal header but
    /// whose columns are still NULL. Re-runs after partial completion
    /// pick up where the previous run left off.
    ChunkTemporalBounds {
        /// Restrict to one library. Default: every library.
        #[arg(long)]
        library: Option<Uuid>,
        /// Count rows that would be updated without writing.
        #[arg(long)]
        dry_run: bool,
        /// Emit JSON instead of a human-readable summary.
        #[arg(long)]
        json: bool,
    },
    /// Re-key pre-hardening vector rows (legacy bare `model_catalog_id`
    /// lanes) onto the canonical embedding-execution-profile fingerprint.
    /// Only libraries whose active EmbedChunk binding still targets the
    /// same model catalog entry are re-keyed; everything unprovable is
    /// skipped with a warning prescribing `rebuild vector-plane`.
    VectorProfileKeys {
        /// Restrict to one library. Default: every library.
        #[arg(long)]
        library: Option<Uuid>,
        /// Report what would be re-keyed without writing.
        #[arg(long)]
        dry_run: bool,
        /// Emit JSON instead of a human-readable summary.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum RebuildCommand {
    /// Reconcile per-dim vector relation dimensions with the source library's
    /// active vector binding and rebuild all library vector material that must
    /// share those indexes.
    VectorPlane {
        /// Library whose active vector binding determines the target
        /// vector dimensions.
        #[arg(long)]
        source_library: Uuid,
    },
    /// Re-run the canonical runtime-graph projection for one library
    /// or for every library (batch mode tolerates per-library state
    /// conflicts and surfaces a non-zero exit at the end).
    RuntimeGraph {
        /// Restrict to one library. Default: every library.
        #[arg(long)]
        library: Option<Uuid>,
    },
    /// Re-embed every entity node in the library into the per-dim
    /// `knowledge_entity_vector_d*` PostgreSQL relations.
    /// Fails loudly if no active EmbedChunk binding is configured.
    /// Idempotent — safe to re-run after partial completion.
    EntityEmbeddings {
        /// Library whose entity nodes will be re-embedded.
        #[arg(long)]
        library: Uuid,
    },
}

#[derive(Subcommand)]
enum RetentionCommand {
    /// Batched DELETE on `ingest_stage_event` older than `--older-than-days`.
    /// Backed by `idx_ingest_stage_event_recorded_at`; rows are
    /// removed in 10 000-row batches with a short pause between
    /// batches so concurrent ingest writers stay responsive.
    StageEvents {
        /// Retention window in days. Anything older is removed.
        #[arg(long, default_value_t = 90)]
        older_than_days: u64,
        /// Emit JSON instead of a human-readable summary.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum LeaseCommand {
    /// List the current lease row for every (class, scope) the scheduler
    /// is tracking. Filters narrow the result set.
    Show {
        /// Restrict to one class.
        #[arg(long, value_parser = parse_class)]
        class: Option<MaintenanceClass>,
        /// Restrict to one state, e.g. `dead_letter`, `leased`.
        #[arg(long)]
        state: Option<String>,
        /// Maximum rows to return.
        #[arg(long, default_value_t = 100)]
        limit: i64,
        /// Emit JSON instead of a human-readable table.
        #[arg(long)]
        json: bool,
    },
    /// Per-class summary: pending / leased / completed / failed /
    /// dead_letter counts.
    Summary {
        /// Emit JSON instead of a human-readable table.
        #[arg(long)]
        json: bool,
    },
    /// Reset a dead-letter row back to pending so the scheduler resumes
    /// processing. Use after fixing the root cause.
    ClearFailure {
        #[arg(long, value_parser = parse_class)]
        class: MaintenanceClass,
        /// Optional library uuid. Omit for instance-scope classes.
        #[arg(long)]
        library: Option<Uuid>,
    },
    /// Reap leased rows whose heartbeat is older than `stale_after_secs`.
    /// The scheduler also reaps on every tick; this is the manual lever
    /// for when an operator suspects something is stuck.
    ReapStale {
        #[arg(long, default_value_t = 300)]
        stale_after_secs: u64,
    },
}

fn parse_class(value: &str) -> Result<MaintenanceClass, String> {
    MaintenanceClass::parse_wire(value)
        .ok_or_else(|| format!("unknown maintenance class `{value}`"))
}

fn parse_webhook_outbox_state(value: &str) -> Result<WebhookLifecycleOutboxDispatchState, String> {
    value.parse()
}

fn parse_webhook_outbox_limit(value: &str) -> Result<i64, String> {
    let limit = value
        .parse::<i64>()
        .map_err(|_| format!("invalid webhook outbox audit limit `{value}`"))?;
    if !(1..=MAX_WEBHOOK_LIFECYCLE_OUTBOX_AUDIT_LIMIT).contains(&limit) {
        return Err(format!(
            "webhook outbox audit limit must be between 1 and {MAX_WEBHOOK_LIFECYCLE_OUTBOX_AUDIT_LIMIT}"
        ));
    }
    Ok(limit)
}

fn parse_webhook_outbox_resolution_reason_code(value: &str) -> Result<String, String> {
    if ironrag_backend::infra::repositories::webhook_outbox_repository::is_canonical_webhook_lifecycle_outbox_resolution_reason_code(value)
    {
        return Ok(value.to_string());
    }
    Err(format!(
        "webhook outbox resolution reason code must be 1..={MAX_WEBHOOK_LIFECYCLE_OUTBOX_RESOLUTION_REASON_CODE_BYTES} ASCII bytes and match ^[a-z][a-z0-9_]*$"
    ))
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let settings = Settings::from_env().context("load settings from env")?;
    let deployment_id =
        ironrag_backend::observability::resolve_deployment_id(&settings.database_url).await;
    ironrag_backend::observability::init_tracing(deployment_id).context("init tracing")?;
    let state = AppState::new(settings).await.context("init application state")?;
    let pool = &state.persistence.postgres;

    let result = match cli.command {
        Command::Audit(audit_cmd) => run_audit(&state, audit_cmd).await,
        Command::Gc(gc_cmd) => run_gc(&state, gc_cmd).await,
        Command::Repair(repair_cmd) => run_repair(&state, repair_cmd).await,
        Command::Retention(ret_cmd) => run_retention(pool, ret_cmd).await,
        Command::Migrate(migrate_cmd) => run_migrate(&state, migrate_cmd).await,
        Command::Rebuild(rebuild_cmd) => run_rebuild(&state, rebuild_cmd).await,
        Command::Lease(lease_cmd) => run_lease(pool, lease_cmd).await,
    };

    ironrag_backend::observability::shutdown_tracing().await;
    result
}

// ---------------------------------------------------------------------------
// audit
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct StorageRow {
    table: String,
    total_size: String,
    heap_size: String,
    index_toast_size: String,
    n_live_tup: Option<i64>,
    n_dead_tup: Option<i64>,
    dead_pct: Option<f64>,
    last_autovacuum: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Serialize)]
struct IndexBloatRow {
    table: String,
    index: String,
    size_bytes: i64,
    size_human: String,
    idx_scan: i64,
    idx_tup_read: i64,
}

#[derive(Debug, Serialize)]
struct NullHeadRow {
    library_id: Uuid,
    document_id: Uuid,
    recovery_attempts_count: i32,
    last_recovery_error_code: Option<String>,
    last_recovery_attempt_at: Option<chrono::DateTime<chrono::Utc>>,
    dead_letter_at: Option<chrono::DateTime<chrono::Utc>>,
}

async fn run_audit(state: &AppState, command: AuditCommand) -> Result<()> {
    let pool = &state.persistence.postgres;
    match command {
        AuditCommand::StorageSummary { limit, json } => {
            audit_commands::storage_summary(pool, limit, json).await
        }
        AuditCommand::IndexBloat { tables, min_size_mb, json } => {
            audit_commands::index_bloat(pool, tables, min_size_mb, json).await
        }
        AuditCommand::OrphanLibraries { json } => {
            audit_commands::orphan_libraries(state, json).await
        }
        AuditCommand::NullHeadDocs { library, limit, json } => {
            audit_commands::null_head_docs(pool, library, limit, json).await
        }
        AuditCommand::WebhookOutbox {
            state,
            library,
            before_created_at,
            before_id,
            limit,
            json,
        } => {
            audit_commands::webhook_outbox(
                pool,
                state,
                library,
                before_created_at,
                before_id,
                limit,
                json,
            )
            .await
        }
    }
}

mod audit_commands {
    use super::*;

    macro_rules! terminal_println {
        ($($argument:tt)*) => {{
            use std::io::Write as _;
            writeln!(std::io::stdout().lock(), $($argument)*)?;
        }};
    }

    pub(super) async fn storage_summary(pool: &PgPool, limit: i64, json: bool) -> Result<()> {
        let rows = audit_storage_summary(pool, limit).await?;
        if json {
            terminal_println!("{}", serde_json::to_string_pretty(&rows)?);
        } else {
            terminal_println!(
                "{:<46} {:>10} {:>10} {:>10} {:>12} {:>12} {:>8} last_autovac",
                "table",
                "total",
                "heap",
                "idx+toast",
                "live",
                "dead",
                "dead %"
            );
            for row in rows {
                terminal_println!(
                    "{:<46} {:>10} {:>10} {:>10} {:>12} {:>12} {:>8} {}",
                    row.table,
                    row.total_size,
                    row.heap_size,
                    row.index_toast_size,
                    row.n_live_tup.unwrap_or(0),
                    row.n_dead_tup.unwrap_or(0),
                    row.dead_pct.map_or_else(|| "-".to_string(), |v| format!("{v:.1}")),
                    row.last_autovacuum.map_or_else(|| "-".to_string(), |t| t.to_rfc3339()),
                );
            }
        }
        Ok(())
    }

    pub(super) async fn index_bloat(
        pool: &PgPool,
        tables: Vec<String>,
        min_size_mb: i64,
        json: bool,
    ) -> Result<()> {
        let table_filter = if tables.is_empty() { default_index_audit_tables() } else { tables };
        let rows = audit_index_bloat(pool, &table_filter, min_size_mb).await?;
        if json {
            terminal_println!("{}", serde_json::to_string_pretty(&rows)?);
        } else {
            terminal_println!(
                "{:<36} {:<60} {:>12} {:>12} {:>14}",
                "table",
                "index",
                "size",
                "idx_scan",
                "idx_tup_read"
            );
            for row in rows {
                terminal_println!(
                    "{:<36} {:<60} {:>12} {:>12} {:>14}",
                    row.table,
                    row.index,
                    row.size_human,
                    row.idx_scan,
                    row.idx_tup_read,
                );
            }
        }
        Ok(())
    }

    pub(super) async fn orphan_libraries(state: &AppState, json: bool) -> Result<()> {
        let report = audit::orphan_libraries(state).await?;
        if json {
            terminal_println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            terminal_println!(
                "orphan-libraries  live_libraries={}  orphan_libraries={}",
                report.live_library_count,
                report.orphan_libraries.len(),
            );
            if !report.totals.is_empty() {
                terminal_println!("totals per collection:");
                for (collection, count) in &report.totals {
                    terminal_println!("  {collection:<48} {count}");
                }
            }
        }
        Ok(())
    }

    pub(super) async fn null_head_docs(
        pool: &PgPool,
        library: Option<Uuid>,
        limit: i64,
        json: bool,
    ) -> Result<()> {
        let rows = audit_null_head_docs(pool, library, limit).await?;
        if json {
            terminal_println!("{}", serde_json::to_string_pretty(&rows)?);
        } else {
            terminal_println!(
                "{:<38} {:<38} {:>9} {:>22} {:>26} dead_letter_at",
                "library_id",
                "document_id",
                "attempts",
                "last_error",
                "last_attempt_at",
            );
            for row in rows {
                terminal_println!(
                    "{:<38} {:<38} {:>9} {:>22} {:>26} {}",
                    row.library_id,
                    row.document_id,
                    row.recovery_attempts_count,
                    row.last_recovery_error_code.unwrap_or_else(|| "-".to_string()),
                    row.last_recovery_attempt_at
                        .map_or_else(|| "-".to_string(), |t| t.to_rfc3339()),
                    row.dead_letter_at.map_or_else(|| "-".to_string(), |t| t.to_rfc3339()),
                );
            }
        }
        Ok(())
    }

    pub(super) async fn webhook_outbox(
        pool: &PgPool,
        state: WebhookLifecycleOutboxDispatchState,
        library: Option<Uuid>,
        before_created_at: Option<chrono::DateTime<chrono::Utc>>,
        before_id: Option<Uuid>,
        limit: i64,
        json: bool,
    ) -> Result<()> {
        let cursor = webhook_outbox_audit_cursor(before_created_at, before_id)?;
        let report = webhook_outbox_ops::audit_webhook_lifecycle_outbox(
            pool,
            WebhookLifecycleOutboxAuditOptions {
                dispatch_state: Some(state),
                library_id: library,
                cursor,
                limit,
            },
        )
        .await?;
        if json {
            terminal_println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            terminal_println!(
                "audit.webhook-outbox  state={}  library={}  returned={}  limit={}",
                state,
                library.map_or_else(|| "all".to_string(), |id| id.to_string()),
                report.returned,
                report.limit,
            );
            for entry in &report.entries {
                terminal_println!(
                    "{}  state={}  event={}  occurred_at={}  workspace={}  library={}  attempts={}  last_error_code={}  resolution_reason_code={}  available_at={}  lease_expires_at={}  dispatched_at={}  resolved_at={}  created_at={}  updated_at={}",
                    entry.id,
                    entry.dispatch_state,
                    entry.event_type,
                    entry.occurred_at.to_rfc3339(),
                    entry.workspace_id,
                    entry.library_id,
                    entry.dispatch_attempts,
                    entry.last_error_code.as_deref().unwrap_or("-"),
                    entry.resolution_reason_code.as_deref().unwrap_or("-"),
                    entry.available_at.to_rfc3339(),
                    entry
                        .lease_expires_at
                        .map_or_else(|| "-".to_string(), |value| value.to_rfc3339()),
                    entry.dispatched_at.map_or_else(|| "-".to_string(), |value| value.to_rfc3339()),
                    entry.resolved_at.map_or_else(|| "-".to_string(), |value| value.to_rfc3339()),
                    entry.created_at.to_rfc3339(),
                    entry.updated_at.to_rfc3339(),
                );
            }
            if let Some(next_cursor) = report.next_cursor {
                terminal_println!(
                    "next page: --before-created-at {} --before-id {}",
                    next_cursor.created_at.to_rfc3339(),
                    next_cursor.id,
                );
            }
        }
        Ok(())
    }

    pub(super) fn webhook_outbox_audit_cursor(
        before_created_at: Option<chrono::DateTime<chrono::Utc>>,
        before_id: Option<Uuid>,
    ) -> Result<Option<WebhookLifecycleOutboxAuditCursor>> {
        match (before_created_at, before_id) {
            (Some(created_at), Some(id)) => {
                Ok(Some(WebhookLifecycleOutboxAuditCursor { created_at, id }))
            }
            (None, None) => Ok(None),
            _ => bail!("--before-created-at and --before-id must be supplied together"),
        }
    }
}

fn default_index_audit_tables() -> Vec<String> {
    [
        "runtime_graph_evidence",
        "runtime_graph_node",
        "runtime_graph_edge",
        "runtime_graph_canonical_summary",
        "runtime_graph_extraction",
        "ingest_attempt",
        "ingest_job",
        "ingest_stage_event",
        "content_chunk",
        "content_document",
        "content_document_head",
        "content_revision",
        "billing_provider_call",
        "billing_execution_cost",
        "billing_execution_cost_rollup_state",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

async fn audit_storage_summary(pool: &PgPool, limit: i64) -> Result<Vec<StorageRow>> {
    let rows = sqlx::query_as::<_, (
        String,
        String,
        String,
        String,
        Option<i64>,
        Option<i64>,
        Option<chrono::DateTime<chrono::Utc>>,
    )>(
        "select \
            n.nspname || '.' || c.relname as table_name, \
            pg_size_pretty(pg_total_relation_size(c.oid)) as total_size, \
            pg_size_pretty(pg_relation_size(c.oid)) as heap_size, \
            pg_size_pretty(pg_total_relation_size(c.oid) - pg_relation_size(c.oid)) as idx_toast_size, \
            s.n_live_tup, \
            s.n_dead_tup, \
            s.last_autovacuum \
         from pg_class c \
         join pg_namespace n on n.oid = c.relnamespace \
         left join pg_stat_user_tables s on s.relid = c.oid \
         where c.relkind = 'r' \
           and n.nspname not in ('pg_catalog', 'information_schema') \
         order by pg_total_relation_size(c.oid) desc \
         limit $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(table, total, heap, idx, live, dead, autovac)| {
            let dead_pct = match (live, dead) {
                (Some(l), Some(d)) if l + d > 0 => Some((d as f64) * 100.0 / ((l + d) as f64)),
                _ => None,
            };
            StorageRow {
                table,
                total_size: total,
                heap_size: heap,
                index_toast_size: idx,
                n_live_tup: live,
                n_dead_tup: dead,
                dead_pct,
                last_autovacuum: autovac,
            }
        })
        .collect())
}

async fn audit_index_bloat(
    pool: &PgPool,
    tables: &[String],
    min_size_mb: i64,
) -> Result<Vec<IndexBloatRow>> {
    let min_bytes = min_size_mb.saturating_mul(1024 * 1024);
    let rows = sqlx::query_as::<_, (String, String, i64, String, i64, i64)>(
        "select \
            s.relname::text, \
            s.indexrelname::text, \
            pg_relation_size(s.indexrelid) as size_bytes, \
            pg_size_pretty(pg_relation_size(s.indexrelid)) as size_human, \
            s.idx_scan::bigint, \
            s.idx_tup_read::bigint \
         from pg_stat_user_indexes s \
         where s.relname = any($1::text[]) \
           and pg_relation_size(s.indexrelid) >= $2 \
         order by pg_relation_size(s.indexrelid) desc",
    )
    .bind(tables)
    .bind(min_bytes)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(table, index, size_bytes, size_human, idx_scan, idx_tup_read)| IndexBloatRow {
            table,
            index,
            size_bytes,
            size_human,
            idx_scan,
            idx_tup_read,
        })
        .collect())
}

async fn audit_null_head_docs(
    pool: &PgPool,
    library: Option<Uuid>,
    limit: i64,
) -> Result<Vec<NullHeadRow>> {
    let rows = sqlx::query_as::<
        _,
        (
            Uuid,
            Uuid,
            i32,
            Option<String>,
            Option<chrono::DateTime<chrono::Utc>>,
            Option<chrono::DateTime<chrono::Utc>>,
        ),
    >(
        "select \
            d.library_id, \
            h.document_id, \
            h.recovery_attempts_count, \
            h.last_recovery_error_code, \
            h.last_recovery_attempt_at, \
            h.dead_letter_at \
         from content_document_head h \
         join content_document d on d.id = h.document_id \
         where h.readable_revision_id is null \
           and h.active_revision_id is null \
           and ($1::uuid is null or d.library_id = $1) \
         order by h.dead_letter_at nulls first, h.last_recovery_attempt_at nulls first \
         limit $2",
    )
    .bind(library)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(library_id, document_id, attempts, error_code, last_attempt, dead_letter)| {
            NullHeadRow {
                library_id,
                document_id,
                recovery_attempts_count: attempts,
                last_recovery_error_code: error_code,
                last_recovery_attempt_at: last_attempt,
                dead_letter_at: dead_letter,
            }
        })
        .collect())
}

// ---------------------------------------------------------------------------
// gc
// ---------------------------------------------------------------------------

async fn run_gc(state: &AppState, command: GcCommand) -> Result<()> {
    match command {
        GcCommand::OrphanLibraries { yes, json } => {
            gc_commands::orphan_libraries(state, yes, json).await
        }
        GcCommand::StaleEvidence { library, json } => {
            gc_commands::stale_evidence(state, library, json).await
        }
        GcCommand::StaleChunks { library, dry_run, include_null_head, json } => {
            gc_commands::stale_chunks(state, library, dry_run, include_null_head, json).await
        }
    }
}

#[allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "operator maintenance commands intentionally stream reports to the terminal"
)]
mod gc_commands {
    use super::*;

    pub(super) async fn orphan_libraries(state: &AppState, yes: bool, json: bool) -> Result<()> {
        let audit_report = audit::orphan_libraries(state).await?;
        if !yes {
            bail!(
                "gc orphan-libraries refuses to delete without --yes (found {} orphan libraries; use `audit orphan-libraries` for the read-only inventory)",
                audit_report.orphan_libraries.len(),
            );
        }
        let purge = gc::purge_orphan_libraries(state, &audit_report).await?;
        if json {
            println!("{}", serde_json::to_string_pretty(&purge)?);
        } else {
            println!(
                "gc.orphan-libraries  total={}  purged={}  failed={}",
                purge.orphan_libraries_total, purge.purged, purge.failed,
            );
        }
        Ok(())
    }

    pub(super) async fn stale_evidence(
        state: &AppState,
        library: Option<Uuid>,
        json: bool,
    ) -> Result<()> {
        let libraries = libraries_for_gc(state, library).await?;
        let total_report = collect_stale_evidence(state, &libraries).await;
        if json {
            println!("{}", serde_json::to_string_pretty(&total_report)?);
        } else {
            println!(
                "gc.stale-evidence  stale_revision_rows={}  phantom_chunk_rows={}",
                total_report.stale_revision_rows, total_report.phantom_chunk_rows,
            );
        }
        Ok(())
    }

    pub(super) async fn stale_chunks(
        state: &AppState,
        library: Option<Uuid>,
        dry_run: bool,
        include_null_head: bool,
        json: bool,
    ) -> Result<()> {
        let options = GcStaleChunksOptions { dry_run, include_null_head };
        let report = match library {
            Some(library_id) => {
                let row = library_for_gc(state, library_id).await?;
                gc::run_for_library(state, row.workspace_id, row.id, options).await?
            }
            None => gc::run_for_all_libraries(state, options).await?,
        };
        if json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            print_gc_report(library, options, &report);
        }
        Ok(())
    }

    async fn libraries_for_gc(
        state: &AppState,
        library: Option<Uuid>,
    ) -> Result<Vec<catalog_repository::CatalogLibraryRow>> {
        match library {
            Some(library_id) => Ok(vec![library_for_gc(state, library_id).await?]),
            None => catalog_repository::list_libraries(&state.persistence.postgres, None)
                .await
                .map_err(Into::into),
        }
    }

    async fn library_for_gc(
        state: &AppState,
        library_id: Uuid,
    ) -> Result<catalog_repository::CatalogLibraryRow> {
        catalog_repository::list_libraries(&state.persistence.postgres, None)
            .await?
            .into_iter()
            .find(|library| library.id == library_id)
            .ok_or_else(|| anyhow::anyhow!("library {library_id} not found"))
    }

    async fn collect_stale_evidence(
        state: &AppState,
        libraries: &[catalog_repository::CatalogLibraryRow],
    ) -> gc::StaleEvidenceReport {
        let mut totals = (0_i64, 0_i64);
        for library_row in libraries {
            match gc::run_stale_evidence(state, library_row.id).await {
                Ok(report) => {
                    totals.0 += report.stale_revision_rows;
                    totals.1 += report.phantom_chunk_rows;
                }
                Err(error) => {
                    eprintln!("gc.stale-evidence failed for library {}: {error}", library_row.id)
                }
            }
        }
        gc::StaleEvidenceReport { stale_revision_rows: totals.0, phantom_chunk_rows: totals.1 }
    }
}

#[allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "operator maintenance commands intentionally stream reports to the terminal"
)]
fn print_gc_report(library: Option<Uuid>, options: GcStaleChunksOptions, report: &LibraryGcReport) {
    let scope = library.map_or_else(|| "all".to_string(), |id| id.to_string());
    println!(
        "gc.stale-chunks scope={scope} dry_run={} include_null_head={}",
        options.dry_run, options.include_null_head,
    );
    println!("  documents_visited           {}", report.documents_visited);
    println!("  documents_with_stale        {}", report.documents_with_stale);
    println!("  stale_chunks_removed        {}", report.stale_chunks_removed);
    println!("  stale_vectors_removed       {}", report.stale_vectors_removed);
    println!("  null_head_docs_total        {}", report.null_head_docs_total);
    println!("  null_head_docs_processed    {}", report.null_head_docs_processed);
    println!("  null_head_chunks_removed    {}", report.null_head_chunks_removed);
    println!("  null_head_vectors_removed   {}", report.null_head_vectors_removed);
}

// ---------------------------------------------------------------------------
// repair
// ---------------------------------------------------------------------------

#[allow(
    clippy::print_stdout,
    reason = "operator maintenance commands intentionally stream reports to the terminal"
)]
async fn run_repair(state: &AppState, command: RepairCommand) -> Result<()> {
    match command {
        RepairCommand::KnowledgeProjectionMetadata { library, dry_run, json } => {
            let report = knowledge_projection_metadata::knowledge_projection_metadata(
                state, library, dry_run,
            )
            .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "repair.knowledge-projection-metadata  libraries={}  document_rows={}  revision_rows={}  dry_run={}",
                    report.libraries_reconciled,
                    report.document_rows_updated,
                    report.revision_rows_updated,
                    dry_run,
                );
            }
            Ok(())
        }
        RepairCommand::OrphanKnowledgeDocuments { library, dry_run, json } => {
            let report =
                orphan_knowledge_documents::orphan_knowledge_documents(state, library, dry_run)
                    .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "repair.orphan-knowledge-documents  libraries={}  documents={}  chunk_vector_rows={}  graph_evidence_rows={}  graph_rebuilds={}  graph_rebuilds_skipped={}  dry_run={}",
                    report.libraries_repaired,
                    report.orphan_documents_removed,
                    report.chunk_vector_rows_removed,
                    report.graph_evidence_rows_removed,
                    report.graph_rebuilds_completed,
                    report.graph_rebuilds_skipped,
                    dry_run,
                );
            }
            anyhow::ensure!(
                report.graph_rebuilds_skipped == 0,
                "orphan purge completed but {} runtime-graph re-projection(s) failed; \
                 run `rebuild runtime-graph --library <id>` for the warned libraries",
                report.graph_rebuilds_skipped,
            );
            Ok(())
        }
        RepairCommand::NullHeads { library, json } => {
            repair_commands::null_heads(state, library, json).await
        }
        RepairCommand::NullHeadsAuto { library, json } => {
            repair_commands::null_heads_auto(state, library, json).await
        }
        RepairCommand::ClearRecoveryDeadLetter { document } => {
            repair_commands::clear_recovery_dead_letter(state, document).await
        }
        RepairCommand::WebhookOutboxDeadLetter { outbox, json } => {
            repair_commands::webhook_outbox_dead_letter(state, outbox, json).await
        }
        RepairCommand::WebhookOutboxDeadLetterResolve {
            outbox,
            reason_code,
            acknowledge_not_delivered,
            json,
        } => {
            repair_commands::webhook_outbox_dead_letter_resolve(
                state,
                outbox,
                reason_code,
                acknowledge_not_delivered,
                json,
            )
            .await
        }
        RepairCommand::WebhookDeliveryAbandon {
            subscription,
            acknowledge_duplicate_delivery_risk,
            json,
        } => {
            repair_commands::webhook_delivery_abandon(
                state,
                subscription,
                acknowledge_duplicate_delivery_risk,
                json,
            )
            .await
        }
    }
}

mod repair_commands {
    use super::*;

    macro_rules! terminal_println {
        ($($argument:tt)*) => {{
            use std::io::Write as _;
            writeln!(std::io::stdout().lock(), $($argument)*)?;
        }};
    }

    pub(super) async fn null_heads(
        state: &AppState,
        library: Option<Uuid>,
        json: bool,
    ) -> Result<()> {
        let report = repair::promote_null_heads(state, library).await?;
        if json {
            terminal_println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            terminal_println!(
                "repair.null-heads  libraries_scanned={}  promoted={}  skipped_no_chunks={}",
                report.libraries_scanned,
                report.promoted,
                report.skipped_no_chunks,
            );
        }
        Ok(())
    }

    pub(super) async fn null_heads_auto(
        state: &AppState,
        library: Option<Uuid>,
        json: bool,
    ) -> Result<()> {
        let report = repair::promote_null_heads_auto(state, library).await?;
        if json {
            terminal_println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            terminal_println!(
                "repair.null-heads-auto  libraries_scanned={}  candidates={}  promoted={}  failed={}  dead_lettered={}  cooldown_skipped={}",
                report.libraries_scanned,
                report.candidates_considered,
                report.promoted,
                report.failed,
                report.dead_lettered,
                report.cooldown_skipped,
            );
        }
        Ok(())
    }

    pub(super) async fn clear_recovery_dead_letter(state: &AppState, document: Uuid) -> Result<()> {
        let cleared =
            repair::clear_recovery_dead_letter(&state.persistence.postgres, document).await?;
        if cleared {
            terminal_println!("cleared dead_letter_at for document {document}");
            Ok(())
        } else {
            bail!("no dead_letter_at mark to clear for document {document}");
        }
    }

    pub(super) async fn webhook_outbox_dead_letter(
        state: &AppState,
        outbox: Uuid,
        json: bool,
    ) -> Result<()> {
        let report = webhook_outbox_ops::requeue_dead_letter_webhook_lifecycle_outbox(
            &state.persistence.postgres,
            outbox,
        )
        .await?;
        if json {
            terminal_println!("{}", serde_json::to_string_pretty(&report)?);
        }
        if !report.requeued {
            bail!(
                "webhook lifecycle outbox row {outbox} was not requeued: it does not exist or is no longer dead_letter"
            );
        }
        if !json {
            terminal_println!(
                "repair.webhook-outbox-dead-letter  outbox_id={}  transition=dead_letter->pending  delivery=not-triggered",
                report.outbox_id,
            );
        }
        Ok(())
    }

    pub(super) async fn webhook_outbox_dead_letter_resolve(
        state: &AppState,
        outbox: Uuid,
        reason_code: String,
        acknowledge_not_delivered: bool,
        json: bool,
    ) -> Result<()> {
        if !acknowledge_not_delivered {
            bail!("refusing to resolve an undelivered webhook without --acknowledge-not-delivered");
        }
        let report = webhook_outbox_ops::resolve_dead_letter_webhook_lifecycle_outbox(
            &state.persistence.postgres,
            outbox,
            &reason_code,
        )
        .await?;
        if json {
            terminal_println!("{}", serde_json::to_string_pretty(&report)?);
        }
        if !report.resolved {
            bail!(
                "webhook lifecycle outbox row {outbox} was not resolved: it does not exist or is no longer dead_letter"
            );
        }
        if !json {
            terminal_println!(
                "repair.webhook-outbox-dead-letter-resolve  outbox_id={}  transition=dead_letter->resolved  reason_code={}  delivered=false",
                report.outbox_id,
                report.reason_code,
            );
        }
        Ok(())
    }

    pub(super) async fn webhook_delivery_abandon(
        state: &AppState,
        subscription: Uuid,
        acknowledge_duplicate_delivery_risk: bool,
        json: bool,
    ) -> Result<()> {
        if !acknowledge_duplicate_delivery_risk {
            bail!(
                "refusing to abandon delivery ownership without --acknowledge-duplicate-delivery-risk"
            );
        }
        let abandoned = webhook_repository::force_abandon_draining_webhook_deliveries(
            &state.persistence.postgres,
            subscription,
        )
        .await?;
        if abandoned == 0 {
            bail!(
                "subscription {subscription} is not a tombstoned drain with an active delivery owner"
            );
        }
        let delete_outcome = webhook_repository::delete_webhook_subscription(
            &state.persistence.postgres,
            subscription,
        )
        .await?;
        if delete_outcome != webhook_repository::DeleteWebhookSubscriptionOutcome::Deleted {
            bail!("subscription {subscription} remained draining after explicit abandon");
        }
        if json {
            terminal_println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "subscription_id": subscription,
                    "abandoned_deliveries": abandoned,
                    "deleted": true,
                }))?
            );
        } else {
            terminal_println!(
                "repair.webhook-delivery-abandon  subscription_id={}  abandoned={}  deleted=true  duplicate_delivery_risk=acknowledged",
                subscription,
                abandoned,
            );
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// retention
// ---------------------------------------------------------------------------

#[allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "operator maintenance commands intentionally stream reports to the terminal"
)]
async fn run_retention(pool: &PgPool, command: RetentionCommand) -> Result<()> {
    match command {
        RetentionCommand::StageEvents { older_than_days, json } => {
            let older_than = Duration::from_secs(older_than_days.saturating_mul(86_400));
            let report = retention::stage_events(pool, older_than).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "retention.stage-events  rows_removed={}  batches={}  older_than_days={}",
                    report.rows_removed, report.batches, older_than_days,
                );
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// migrate
// ---------------------------------------------------------------------------

#[allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "operator maintenance commands intentionally stream reports to the terminal"
)]
async fn run_migrate(state: &AppState, command: MigrateCommand) -> Result<()> {
    match command {
        MigrateCommand::CredentialSecrets { apply, batch_size, json } => {
            if apply && !state.settings.credential_encryption_write_enabled {
                anyhow::bail!(
                    "credential rewrap is disabled until IRONRAG_CREDENTIAL_ENCRYPTION_WRITE_ENABLED=true is active on every API and worker replica"
                );
            }
            let report = credential_secrets::migrate_credential_secrets(
                &state.persistence.postgres,
                &state.credential_cipher,
                CredentialSecretMigrationOptions { apply, batch_size },
            )
            .await?;
            let invalid_values = report.invalid_values();
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "migrate.credential-secrets  apply={}  ai_scanned={}  ai_rewrap_candidates={}  ai_migrated={}  ai_concurrent={}  ai_invalid={}  webhook_scanned={}  webhook_rewrap_candidates={}  webhook_migrated={}  webhook_concurrent={}  webhook_invalid={}  webhook_headers_scanned={}  webhook_header_rewrap_candidates={}  webhook_headers_migrated={}  webhook_header_concurrent={}  webhook_header_invalid={}  invalid_samples={}",
                    report.apply,
                    report.ai_rows_scanned,
                    report.ai_legacy_found,
                    report.ai_migrated,
                    report.ai_concurrent_changes,
                    report.ai_invalid,
                    report.webhook_rows_scanned,
                    report.webhook_legacy_found,
                    report.webhook_migrated,
                    report.webhook_concurrent_changes,
                    report.webhook_invalid,
                    report.webhook_header_rows_scanned,
                    report.webhook_header_rewrap_candidates,
                    report.webhook_headers_migrated,
                    report.webhook_header_concurrent_changes,
                    report.webhook_header_invalid,
                    report.invalid_samples.len(),
                );
            }
            if invalid_values > 0 {
                anyhow::bail!(
                    "credential audit found {invalid_values} invalid stored value(s); inspect the redacted report and repair or rotate those credentials"
                );
            }
        }
        MigrateCommand::ChunkTemporalBounds { library, dry_run, json } => {
            let report = migrate::chunk_temporal_bounds(state, library, dry_run).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "migrate.chunk-temporal-bounds  scanned={}  parsed={}  updated_pg={}  skipped_no_header={}  dry_run={}",
                    report.scanned,
                    report.parsed,
                    report.updated_pg,
                    report.skipped_no_header,
                    dry_run,
                );
            }
        }
        MigrateCommand::VectorProfileKeys { library, dry_run, json } => {
            let report =
                vector_profile_keys::legacy_vector_profile_keys(state, library, dry_run).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "migrate.vector-profile-keys  rekeyed={}  no_legacy_lanes={}  skipped_unprovable={}  vector_rows={}  manifest_lanes={}  dry_run={}",
                    report.libraries_rekeyed,
                    report.libraries_without_legacy_lanes,
                    report.libraries_skipped_unprovable,
                    report.vector_rows_rekeyed,
                    report.manifest_lanes_rekeyed,
                    dry_run,
                );
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// rebuild
// ---------------------------------------------------------------------------

#[allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "operator maintenance commands intentionally stream reports to the terminal"
)]
async fn run_rebuild(state: &AppState, command: RebuildCommand) -> Result<()> {
    match command {
        RebuildCommand::VectorPlane { source_library } => {
            rebuild::vector_plane(state, source_library).await?;
            println!("rebuild.vector-plane completed for source library {source_library}");
        }
        RebuildCommand::RuntimeGraph { library } => {
            rebuild::runtime_graph(state, library).await?;
            println!(
                "rebuild.runtime-graph completed for {}",
                library.map_or_else(|| "all libraries".to_string(), |id| id.to_string()),
            );
        }
        RebuildCommand::EntityEmbeddings { library } => {
            let started = std::time::Instant::now();
            let vectors_upserted = rebuild::entity_embeddings(state, library).await?;
            println!(
                "rebuild.entity-embeddings completed entities_processed={vectors_upserted} vectors_upserted={vectors_upserted} elapsed_ms={} library_id={library}",
                started.elapsed().as_millis(),
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// lease
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct LeaseSummaryRow {
    class: String,
    pending: i64,
    leased: i64,
    completed: i64,
    failed: i64,
    dead_letter: i64,
}

async fn run_lease(pool: &PgPool, command: LeaseCommand) -> Result<()> {
    match command {
        LeaseCommand::Show { class, state, limit, json } => {
            lease_commands::show(pool, class, state.as_deref(), limit, json).await
        }
        LeaseCommand::Summary { json } => lease_commands::summary(pool, json).await,
        LeaseCommand::ClearFailure { class, library } => {
            lease_commands::clear_failure(pool, class, library).await
        }
        LeaseCommand::ReapStale { stale_after_secs } => {
            lease_commands::reap_stale(pool, stale_after_secs).await
        }
    }
}

#[allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "operator maintenance commands intentionally stream reports to the terminal"
)]
mod lease_commands {
    use super::*;

    pub(super) async fn show(
        pool: &PgPool,
        class: Option<MaintenanceClass>,
        state: Option<&str>,
        limit: i64,
        json: bool,
    ) -> Result<()> {
        let rows = lease_show(pool, class, state, limit).await?;
        if json {
            println!("{}", serde_json::to_string_pretty(&rows)?);
        } else {
            println!(
                "{:<32} {:<10} {:<38} {:<14} {:>8} {:<24} error_code",
                "class", "scope", "scope_id", "state", "attempts", "next_due_at",
            );
            for row in rows {
                println!(
                    "{:<32} {:<10} {:<38} {:<14} {:>8} {:<24} {}",
                    row.class,
                    row.scope_kind,
                    row.scope_id.map_or_else(|| "-".to_string(), |id| id.to_string()),
                    format!("{:?}", row.state),
                    row.attempts,
                    row.next_due_at.to_rfc3339(),
                    row.error_code.unwrap_or_else(|| "-".to_string()),
                );
            }
        }
        Ok(())
    }

    pub(super) async fn summary(pool: &PgPool, json: bool) -> Result<()> {
        let rows = lease_summary(pool).await?;
        if json {
            println!("{}", serde_json::to_string_pretty(&rows)?);
        } else {
            println!(
                "{:<32} {:>10} {:>10} {:>10} {:>10} {:>12}",
                "class", "pending", "leased", "completed", "failed", "dead_letter"
            );
            for row in rows {
                println!(
                    "{:<32} {:>10} {:>10} {:>10} {:>10} {:>12}",
                    row.class, row.pending, row.leased, row.completed, row.failed, row.dead_letter,
                );
            }
        }
        Ok(())
    }

    pub(super) async fn clear_failure(
        pool: &PgPool,
        class: MaintenanceClass,
        library: Option<Uuid>,
    ) -> Result<()> {
        let scope = scope_for_clear_failure(library);
        let cleared = lease::clear_dead_letter(pool, class, scope).await?;
        if cleared {
            println!("cleared dead-letter for {} {:?}", class.as_str(), scope);
            Ok(())
        } else {
            bail!("no dead-letter row found for {} {:?}", class.as_str(), scope);
        }
    }

    pub(super) fn scope_for_clear_failure(library: Option<Uuid>) -> Scope {
        library.map_or(Scope::Instance, Scope::Library)
    }

    pub(super) async fn reap_stale(pool: &PgPool, stale_after_secs: u64) -> Result<()> {
        let reaped = lease::reap_stale_leases(pool, Duration::from_secs(stale_after_secs)).await?;
        println!("reaped {reaped} stale leases");
        Ok(())
    }
}

async fn lease_show(
    pool: &PgPool,
    class: Option<MaintenanceClass>,
    state: Option<&str>,
    limit: i64,
) -> Result<Vec<MaintenanceJobRun>> {
    let rows = sqlx::query_as::<_, MaintenanceJobRun>(
        "select * from maintenance_job_run \
         where ($1::text is null or class = $1) \
           and ($2::text is null or state::text = $2) \
         order by next_due_at asc \
         limit $3",
    )
    .bind(class.map(MaintenanceClass::as_str))
    .bind(state)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

async fn lease_summary(pool: &PgPool) -> Result<Vec<LeaseSummaryRow>> {
    let mut out = Vec::new();
    for class in [
        MaintenanceClass::GcStaleChunks,
        MaintenanceClass::GcStaleEvidence,
        MaintenanceClass::GcArchivalEvidence,
        MaintenanceClass::GcPgGraphZombies,
        MaintenanceClass::GcOrphanLibrariesAudit,
        MaintenanceClass::AuditStorageSummary,
        MaintenanceClass::AuditIndexBloat,
        MaintenanceClass::AuditNullHeadDocs,
        MaintenanceClass::RetentionStageEvents,
        MaintenanceClass::RetentionAttempts,
        MaintenanceClass::RetentionPolicyDecisions,
        MaintenanceClass::RetentionAsyncOperations,
        MaintenanceClass::RetentionWebDiscoveredPages,
    ] {
        let StateCounts { pending, leased, completed, failed, dead_letter } =
            lease::count_by_state(pool, class).await?;
        if pending + leased + completed + failed + dead_letter > 0 {
            out.push(LeaseSummaryRow {
                class: class.as_str().to_string(),
                pending,
                leased,
                completed,
                failed,
                dead_letter,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_complete_webhook_outbox_audit_cursor() {
        let created_at = chrono::DateTime::UNIX_EPOCH;
        let id = Uuid::nil();

        let cursor = audit_commands::webhook_outbox_audit_cursor(Some(created_at), Some(id));

        assert_eq!(cursor.ok(), Some(Some(WebhookLifecycleOutboxAuditCursor { created_at, id })));
    }

    #[test]
    fn rejects_partial_webhook_outbox_audit_cursor() {
        let created_at = chrono::DateTime::UNIX_EPOCH;

        let error = audit_commands::webhook_outbox_audit_cursor(Some(created_at), None);

        assert!(error.is_err_and(|error| {
            error.to_string() == "--before-created-at and --before-id must be supplied together"
        }));
    }

    #[test]
    fn uses_instance_scope_when_clearing_a_failure_without_a_library() {
        assert_eq!(lease_commands::scope_for_clear_failure(None), Scope::Instance);
    }

    #[test]
    fn uses_library_scope_when_clearing_a_failure_for_a_library() {
        let library_id = Uuid::nil();

        assert_eq!(
            lease_commands::scope_for_clear_failure(Some(library_id)),
            Scope::Library(library_id)
        );
    }
}
