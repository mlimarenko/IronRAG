// ---------------------------------------------------------------------------
// audit
// ---------------------------------------------------------------------------

use super::*;

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
    /// Current independent visibility obligation, not the latest payload owner.
    current_visibility_intent_id: Option<Uuid>,
    visibility_intent_item_state: Option<String>,
}

pub(super) async fn run_audit(state: &AppState, command: AuditCommand) -> Result<()> {
    let pool = &state.persistence.postgres;
    match command {
        AuditCommand::StorageSummary { limit, json } => {
            audit_commands::storage_summary(pool, limit, json).await
        }
        AuditCommand::IndexBloat { tables, min_size_mb, json } => {
            audit_commands::index_bloat(pool, tables, min_size_mb, json).await
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
            let cursor = audit_commands::webhook_outbox_audit_cursor(before_created_at, before_id)?;
            audit_commands::webhook_outbox(pool, state, library, cursor, limit, json).await
        }
    }
}

pub(super) mod audit_commands {
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
                "{:<38} {:<38} {:>9} {:>22} {:>26} {:<38} {:<16} dead_letter_at",
                "library_id",
                "document_id",
                "attempts",
                "last_error",
                "last_attempt_at",
                "current_visibility_intent_id",
                "intent_item_state",
            );
            for row in rows {
                terminal_println!(
                    "{:<38} {:<38} {:>9} {:>22} {:>26} {:<38} {:<16} {}",
                    row.library_id,
                    row.document_id,
                    row.recovery_attempts_count,
                    row.last_recovery_error_code.unwrap_or_else(|| "-".to_string()),
                    row.last_recovery_attempt_at
                        .map_or_else(|| "-".to_string(), |t| t.to_rfc3339()),
                    row.current_visibility_intent_id
                        .map_or_else(|| "-".to_string(), |id| id.to_string()),
                    row.visibility_intent_item_state.unwrap_or_else(|| "-".to_string()),
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
        cursor: Option<WebhookLifecycleOutboxAuditCursor>,
        limit: i64,
        json: bool,
    ) -> Result<()> {
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

    pub(crate) fn webhook_outbox_audit_cursor(
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
        "runtime_graph_\u{63}anonical_summary",
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
            Option<Uuid>,
            Option<String>,
        ),
    >(
        "select \
            d.library_id, \
            h.document_id, \
            h.recovery_attempts_count, \
            h.last_recovery_error_code, \
            h.last_recovery_attempt_at, \
            h.dead_letter_at, \
            h.visibility_intent_id as current_visibility_intent_id, \
            intent_item.item_state::text as visibility_intent_item_state \
         from content_document_head h \
         join content_document d on d.id = h.document_id \
          left join content_mutation_item intent_item on intent_item.id = h.visibility_intent_id and intent_item.document_id = h.document_id \
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
        .map(
            |(
                library_id,
                document_id,
                attempts,
                error_code,
                last_attempt,
                dead_letter,
                current_visibility_intent_id,
                visibility_intent_item_state,
            )| {
                NullHeadRow {
                    library_id,
                    document_id,
                    recovery_attempts_count: attempts,
                    last_recovery_error_code: error_code,
                    last_recovery_attempt_at: last_attempt,
                    dead_letter_at: dead_letter,
                    current_visibility_intent_id,
                    visibility_intent_item_state,
                }
            },
        )
        .collect())
}
