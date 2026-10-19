//! `ironrag-maintenance` — supported operator CLI for the maintenance
//! pipeline. Subcommands group by verb:
//!
//! * `audit` — read-only storage / index / null-head / webhook-outbox
//!   inspection.
//! * `gc` — delete derived data no longer reachable from a current document
//!   head (`superseded-vectors`, `stale-evidence`, `historical-projection-window`).
//! * `repair` — operator-driven state recovery (null-head promotion,
//!   knowledge-projection reconciliation, webhook-outbox remediation).
//! * `retention` — TTL sweepers for INSERT-only history tables.
//! * `migrate` — durable data migrations (credential-secret rewrap).
//! * `rebuild` — heavy operator-only rebuild passes.
//! * `lease` — inspect and control the durable scheduler lease table.
//!
//! Every subcommand shares the primitives the background scheduler uses,
//! so an operator can dry-run, force-run, or inspect lease state without
//! writing ad-hoc SQL.

mod audit;
mod gc;
mod lease;
mod migrate;
mod rebuild;
mod repair;
mod retention;

#[cfg(test)]
mod tests;

use std::time::Duration;

use audit::run_audit;
use gc::run_gc;
use lease::run_lease;
use migrate::run_migrate;
use rebuild::run_rebuild;
use repair::run_repair;
use retention::run_retention;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::repositories::{catalog_repository, webhook_repository},
    services::maintenance::{
        admission_visibility_policy_schema, ai_binding_purpose_schema,
        content_visibility_intent_schema,
        credential_secrets::{self, CredentialSecretMigrationOptions},
        decision_catalog_schema,
        gc::{self as maintenance_gc, GcSupersededVectorsOptions, SupersededVectorGcReport},
        knowledge_projection_metadata,
        lease::{
            self as maintenance_lease, MaintenanceClass, MaintenanceJobRun, Scope, StateCounts,
        },
        ops_failure_disposition_schema, orphan_knowledge_documents, rebuild as maintenance_rebuild,
        repair as maintenance_repair, retention as maintenance_retention, service_identity_schema,
        vector_orphan_reconcile,
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
    /// current document head.
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
        /// is the defined short-list of write-heavy tables.
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
    /// Delete vectors outside the readable or authoritatively leased active
    /// revision. Source history and knowledge chunks are preserved.
    SupersededVectors {
        /// Restrict to one library. Default: every library.
        #[arg(long)]
        library: Option<Uuid>,
        /// Count what would be removed without issuing destructive deletes.
        #[arg(long)]
        dry_run: bool,
        /// Also sweep vectors for documents whose head is null (failed ingest).
        /// Off by default because such documents may still be recoverable.
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
    /// Purge historical knowledge-plane revisions outside the shared
    /// retention window and sweep their chunk vectors and runtime-graph
    /// evidence. Takes the library lifecycle lock and refuses to run while
    /// any ingest job for the library is in flight.
    HistoricalProjectionWindow {
        /// Library whose historical projection is swept.
        #[arg(long)]
        library: Uuid,
        /// Report the retention plan without deleting.
        #[arg(long)]
        dry_run: bool,
        /// Emit JSON instead of a human-readable summary.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum RepairCommand {
    /// Reconcile drifted knowledge-projection metadata (document
    /// external_key/parent/role/document_state/deleted_at; revision number/kind/source_uri/
    /// document_hint/mime/checksum/title/byte_size) from the authoritative
    /// content plane for active, non-deleted content in matching scope.
    /// Restores projected lifecycle without changing source, heads, readiness
    /// or chunk text. Idempotent; does not recreate missing projections.
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
    /// Purge knowledge-plane documents whose stored `content_document` row no
    /// longer exists. Sweeps chunk vectors and graph evidence in the same
    /// transaction, advances the library source-truth generation, and
    /// re-projects the runtime graph.
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
    /// have at least one chunk-bearing revision in the current document head
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
    /// claiming delivery. The supported reason code and timestamp are stored
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
    /// Remove orphaned per-dim vector-shard rows whose library no longer
    /// exists and re-attach the canonical `on delete cascade` from each shard
    /// to `catalog_library`. The catalog delete path refuses while such rows
    /// exist and prints the exact lanes, so run this after reading that
    /// diagnostic. Without `--dry-run` the removal and the cascade attach
    /// share one transaction. Idempotent.
    OrphanVectorRows {
        /// Report the orphaned lanes without deleting rows or changing DDL.
        #[arg(long)]
        dry_run: bool,
        /// Emit JSON instead of a human-readable summary.
        #[arg(long)]
        json: bool,
    },
    /// Transition historical zero-contribution `processing` revisions (a
    /// successful `extract_graph` that contributed no graph target) to the
    /// canonical terminal graph state computed from that completed extraction.
    /// No provider call is made. Idempotent; repeated runs find nothing.
    TerminalGraphStates {
        /// Restrict to one library. Default: every library.
        #[arg(long)]
        library: Option<Uuid>,
        /// Report the cohort without writing.
        #[arg(long)]
        dry_run: bool,
        /// Emit JSON instead of a human-readable summary.
        #[arg(long)]
        json: bool,
    },
    /// Settle fan-out parents (`ops_async_operation` rows with children) that a
    /// release stranded in `accepted`/`processing` while every child already
    /// reached a terminal state. Uses the same terminal-state aggregate as the
    /// live batch worker; nothing is invented. Idempotent.
    StrandedFanOutOperations {
        /// Report the cohort without writing.
        #[arg(long)]
        dry_run: bool,
        /// Emit JSON instead of a human-readable summary.
        #[arg(long)]
        json: bool,
    },
    /// Re-derive fan-out parents a release wrongly marked `failed` with a
    /// background graph failure code (`graph_projection_failed` /
    /// `graph_cleanup_failed`) while every child is `ready`. The parent's
    /// outcome is its children's: the row settles `ready` and the old failure
    /// code is kept as a `graph_convergence` detail. Idempotent.
    ContradictedFanOutOutcomes {
        /// Report the cohort without writing.
        #[arg(long)]
        dry_run: bool,
        /// Emit JSON instead of a human-readable summary.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum MigrateCommand {
    /// Apply the additive live schema required by integration provisioning.
    /// Without `--apply`, reports whether the database still needs the upgrade.
    IntegrationProvisioningSchema {
        /// Apply the transactional schema and baseline-checksum update.
        #[arg(long)]
        apply: bool,
        /// Emit JSON instead of a human-readable report.
        #[arg(long)]
        json: bool,
    },
    /// Apply the additive live `ops_async_operation.failure_disposition`
    /// column. Without `--apply`, reports whether the upgrade is still needed.
    OpsFailureDispositionSchema {
        /// Apply the transactional DDL and baseline-checksum update.
        #[arg(long)]
        apply: bool,
        /// Emit JSON instead of a human-readable report.
        #[arg(long)]
        json: bool,
    },
    /// Apply the additive live enum value that exposes the `query_rerank`
    /// binding purpose. Without `--apply`, reports whether the upgrade is
    /// still needed.
    QueryRerankBindingPurpose {
        /// Apply the transactional DDL and baseline-checksum update.
        #[arg(long)]
        apply: bool,
        /// Emit JSON instead of a human-readable report.
        #[arg(long)]
        json: bool,
    },
    /// Apply the additive live enum values that expose the decision catalog: the
    /// `graph_entity_type` binding purpose and the `typesafe_system_one` provider
    /// API style. Without `--apply`, reports whether the upgrade is still needed.
    DecisionCatalogSchema {
        /// Apply the transactional DDL and baseline-checksum update.
        #[arg(long)]
        apply: bool,
        /// Emit JSON instead of a human-readable report.
        #[arg(long)]
        json: bool,
    },
    /// Admit reviewed readable revision identities and replace predecessor visibility policy.
    /// Dry-run validates the same certificate, row and source-byte bindings as apply.
    ContentVisibilityIntentSchema {
        /// Raw JSON certificate with explicit filenames, scope and complete descriptors.
        #[arg(long, requires_all = ["import_admission_sha256", "accepted_recovery_snapshot_id", "source_baseline_sha384", "admitted_by"])]
        import_admission_manifest: Option<std::path::PathBuf>,
        /// Independently reviewed SHA-256 of the raw certificate file, without a prefix.
        #[arg(long, requires = "import_admission_manifest")]
        import_admission_sha256: Option<String>,
        /// Full accepted recovery snapshot ID, supplied independently of the certificate.
        #[arg(long, requires = "import_admission_manifest")]
        accepted_recovery_snapshot_id: Option<String>,
        /// Full SHA-384 of the recovery snapshot's schema57 source baseline.
        #[arg(long, requires = "import_admission_manifest")]
        source_baseline_sha384: Option<String>,
        /// Reviewed admitting operator identity, not a cryptographic signature.
        #[arg(long, requires = "import_admission_manifest")]
        admitted_by: Option<String>,
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        json: bool,
    },
    AdmissionVisibilityPolicySchema {
        /// Apply the transactional DDL and baseline-checksum update.
        #[arg(long)]
        apply: bool,
        /// Emit JSON instead of a human-readable report.
        #[arg(long)]
        json: bool,
    },
    /// Assign existing catalog IDs to an already-approved integration authority.
    /// Run this once before switching Botron to the authority credential.
    IntegrationProvisioningOwnership {
        #[arg(long)]
        authority_principal: Uuid,
        #[arg(long)]
        workspace: Uuid,
        /// Repeat for every library in the workspace; omit only when it has none.
        #[arg(long)]
        library: Vec<Uuid>,
        /// Apply the locked transaction. Omit for validation-only output.
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        json: bool,
    },
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
    /// Re-run the active runtime-graph projection for one library
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
    /// Per-class summary: pending / leased / dead_letter counts.
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
    if ironrag_backend::infra::repositories::webhook_outbox_repository::is_supported_webhook_lifecycle_outbox_resolution_reason_code(value)
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
    let state = AppState::new(settings).await.context("init application state")?;
    let deployment_id =
        ironrag_backend::observability::resolve_deployment_id(&state.persistence.postgres).await;
    ironrag_backend::observability::init_tracing(deployment_id).context("init tracing")?;
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
