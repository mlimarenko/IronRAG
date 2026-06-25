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

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::repositories::catalog_repository,
    services::maintenance::{
        audit,
        gc::{self, GcStaleChunksOptions, LibraryGcReport},
        lease::{self, MaintenanceClass, MaintenanceJobRun, Scope, StateCounts},
        migrate, rebuild, repair, retention,
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
}

#[derive(Subcommand)]
enum MigrateCommand {
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
    MaintenanceClass::from_str(value).ok_or_else(|| format!("unknown maintenance class `{value}`"))
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
            let rows = audit_storage_summary(pool, limit).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                println!(
                    "{:<46} {:>10} {:>10} {:>10} {:>12} {:>12} {:>8} last_autovac",
                    "table", "total", "heap", "idx+toast", "live", "dead", "dead %"
                );
                for row in rows {
                    println!(
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
        }
        AuditCommand::IndexBloat { tables, min_size_mb, json } => {
            let table_filter =
                if tables.is_empty() { default_index_audit_tables() } else { tables };
            let rows = audit_index_bloat(pool, &table_filter, min_size_mb).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                println!(
                    "{:<36} {:<60} {:>12} {:>12} {:>14}",
                    "table", "index", "size", "idx_scan", "idx_tup_read"
                );
                for row in rows {
                    println!(
                        "{:<36} {:<60} {:>12} {:>12} {:>14}",
                        row.table, row.index, row.size_human, row.idx_scan, row.idx_tup_read,
                    );
                }
            }
        }
        AuditCommand::OrphanLibraries { json } => {
            let report = audit::orphan_libraries(state).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "orphan-libraries  live_libraries={}  orphan_libraries={}",
                    report.live_library_count,
                    report.orphan_libraries.len(),
                );
                if !report.totals.is_empty() {
                    println!("totals per collection:");
                    for (collection, count) in &report.totals {
                        println!("  {collection:<48} {count}");
                    }
                }
            }
        }
        AuditCommand::NullHeadDocs { library, limit, json } => {
            let rows = audit_null_head_docs(pool, library, limit).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                println!(
                    "{:<38} {:<38} {:>9} {:>22} {:>26} dead_letter_at",
                    "library_id", "document_id", "attempts", "last_error", "last_attempt_at",
                );
                for row in rows {
                    println!(
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
        }
    }
    Ok(())
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
        }
        GcCommand::StaleEvidence { library, json } => {
            let mut totals = (0_i64, 0_i64);
            let libraries = match library {
                Some(library_id) => {
                    let row = catalog_repository::list_libraries(&state.persistence.postgres, None)
                        .await?
                        .into_iter()
                        .find(|library| library.id == library_id)
                        .ok_or_else(|| anyhow::anyhow!("library {library_id} not found"))?;
                    vec![row]
                }
                None => {
                    catalog_repository::list_libraries(&state.persistence.postgres, None).await?
                }
            };
            for library_row in &libraries {
                match gc::run_stale_evidence(state, library_row.id).await {
                    Ok(report) => {
                        totals.0 += report.stale_revision_rows;
                        totals.1 += report.phantom_chunk_rows;
                    }
                    Err(error) => eprintln!(
                        "gc.stale-evidence failed for library {}: {error}",
                        library_row.id
                    ),
                }
            }
            let total_report = gc::StaleEvidenceReport {
                stale_revision_rows: totals.0,
                phantom_chunk_rows: totals.1,
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&total_report)?);
            } else {
                println!(
                    "gc.stale-evidence  stale_revision_rows={}  phantom_chunk_rows={}",
                    total_report.stale_revision_rows, total_report.phantom_chunk_rows,
                );
            }
        }
        GcCommand::StaleChunks { library, dry_run, include_null_head, json } => {
            let options = GcStaleChunksOptions { dry_run, include_null_head };
            let report = if let Some(library_id) = library {
                let row = catalog_repository::list_libraries(&state.persistence.postgres, None)
                    .await?
                    .into_iter()
                    .find(|library| library.id == library_id)
                    .ok_or_else(|| anyhow::anyhow!("library {library_id} not found"))?;
                gc::run_for_library(state, row.workspace_id, row.id, options).await?
            } else {
                gc::run_for_all_libraries(state, options).await?
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_gc_report(library, options, &report);
            }
        }
    }
    Ok(())
}

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

async fn run_repair(state: &AppState, command: RepairCommand) -> Result<()> {
    match command {
        RepairCommand::NullHeads { library, json } => {
            let report = repair::promote_null_heads(state, library).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "repair.null-heads  libraries_scanned={}  promoted={}  skipped_no_chunks={}",
                    report.libraries_scanned, report.promoted, report.skipped_no_chunks,
                );
            }
        }
        RepairCommand::NullHeadsAuto { library, json } => {
            let report = repair::promote_null_heads_auto(state, library).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "repair.null-heads-auto  libraries_scanned={}  candidates={}  promoted={}  failed={}  dead_lettered={}  cooldown_skipped={}",
                    report.libraries_scanned,
                    report.candidates_considered,
                    report.promoted,
                    report.failed,
                    report.dead_lettered,
                    report.cooldown_skipped,
                );
            }
        }
        RepairCommand::ClearRecoveryDeadLetter { document } => {
            let cleared =
                repair::clear_recovery_dead_letter(&state.persistence.postgres, document).await?;
            if cleared {
                println!("cleared dead_letter_at for document {document}");
            } else {
                bail!("no dead_letter_at mark to clear for document {document}");
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// retention
// ---------------------------------------------------------------------------

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

async fn run_migrate(state: &AppState, command: MigrateCommand) -> Result<()> {
    match command {
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
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// rebuild
// ---------------------------------------------------------------------------

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
            let rows = lease_show(pool, class, state.as_deref(), limit).await?;
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
        }
        LeaseCommand::Summary { json } => {
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
                        row.class,
                        row.pending,
                        row.leased,
                        row.completed,
                        row.failed,
                        row.dead_letter,
                    );
                }
            }
        }
        LeaseCommand::ClearFailure { class, library } => {
            let scope = match library {
                Some(id) => Scope::Library(id),
                None => Scope::Instance,
            };
            let cleared = lease::clear_dead_letter(pool, class, scope).await?;
            if cleared {
                println!("cleared dead-letter for {} {:?}", class.as_str(), scope);
            } else {
                bail!("no dead-letter row found for {} {:?}", class.as_str(), scope);
            }
        }
        LeaseCommand::ReapStale { stale_after_secs } => {
            let reaped =
                lease::reap_stale_leases(pool, Duration::from_secs(stale_after_secs)).await?;
            println!("reaped {reaped} stale leases");
        }
    }
    Ok(())
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
