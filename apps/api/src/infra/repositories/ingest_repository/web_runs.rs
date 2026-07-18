use anyhow::Context as _;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct WebIngestRunRow {
    pub id: Uuid,
    pub mutation_id: Uuid,
    pub async_operation_id: Option<Uuid>,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub mode: String,
    pub seed_url: String,
    pub normalized_seed_url: String,
    pub boundary_policy: String,
    pub max_depth: i32,
    pub max_pages: i32,
    pub crawl_allow_patterns: Value,
    pub crawl_block_patterns: Value,
    pub materialization_allow_patterns: Value,
    pub materialization_block_patterns: Value,
    pub run_state: String,
    pub requested_by_principal_id: Option<Uuid>,
    pub requested_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub failure_code: Option<String>,
    pub cancel_requested_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewWebIngestRun<'a> {
    pub id: Uuid,
    pub mutation_id: Uuid,
    pub async_operation_id: Option<Uuid>,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub mode: &'a str,
    pub seed_url: &'a str,
    pub normalized_seed_url: &'a str,
    pub boundary_policy: &'a str,
    pub max_depth: i32,
    pub max_pages: i32,
    pub crawl_allow_patterns: Value,
    pub crawl_block_patterns: Value,
    pub materialization_allow_patterns: Value,
    pub materialization_block_patterns: Value,
    pub run_state: &'a str,
    pub requested_by_principal_id: Option<Uuid>,
    pub requested_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub failure_code: Option<&'a str>,
    pub cancel_requested_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct UpdateWebIngestRun<'a> {
    pub run_state: &'a str,
    pub completed_at: Option<DateTime<Utc>>,
    pub failure_code: Option<&'a str>,
    pub cancel_requested_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct WebRunCountsRow {
    pub discovered: i64,
    pub eligible: i64,
    pub processed: i64,
    pub queued: i64,
    pub processing: i64,
    pub duplicates: i64,
    pub excluded: i64,
    pub blocked: i64,
    pub failed: i64,
    pub canceled: i64,
    pub last_activity_at: Option<DateTime<Utc>>,
}

/// Result of reconciling one materialized web page against the exact content
/// mutation item created for that page. The repository never infers success
/// from another item in the same web-run mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettleWebIngestMutationItemOutcome {
    /// The content job settled before the page row persisted its immutable
    /// `mutation_item_id` link. The page-linking path must call settlement
    /// again after writing the link.
    AwaitingLink { mutation_item_id: Uuid },
    /// The page is linked, but its child content job has not reached a terminal
    /// item state yet.
    AwaitingItem { page_id: Uuid, run_id: Uuid },
    /// The exact page now reflects the terminal child outcome. `run_completed`
    /// is true only when this same transaction also completed the web run, its
    /// aggregate mutation, and its run-level async operation.
    Settled {
        page_id: Uuid,
        run_id: Uuid,
        candidate_state: String,
        run_state: String,
        run_completed: bool,
    },
}

#[derive(Debug, Clone, FromRow)]
struct WebSettlementRunRow {
    id: Uuid,
    mutation_id: Uuid,
    async_operation_id: Option<Uuid>,
    workspace_id: Uuid,
    library_id: Uuid,
    mode: String,
    run_state: String,
    cancel_requested_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
struct WebSettlementPageRow {
    id: Uuid,
    run_id: Uuid,
    document_id: Option<Uuid>,
    result_revision_id: Option<Uuid>,
    candidate_state: String,
}

pub async fn create_web_ingest_run(
    postgres: &PgPool,
    input: &NewWebIngestRun<'_>,
) -> Result<WebIngestRunRow, sqlx::Error> {
    sqlx::query_as::<_, WebIngestRunRow>(
        "insert into content_web_ingest_run (
            id,
            mutation_id,
            async_operation_id,
            workspace_id,
            library_id,
            mode,
            seed_url,
            normalized_seed_url,
            boundary_policy,
            max_depth,
            max_pages,
            crawl_allow_patterns,
            crawl_block_patterns,
            materialization_allow_patterns,
            materialization_block_patterns,
            run_state,
            requested_by_principal_id,
            requested_at,
            completed_at,
            failure_code,
            cancel_requested_at
        )
        values (
            $1,
            $2,
            $3,
            $4,
            $5,
            $6::web_ingest_mode,
            $7,
            $8,
            $9::web_boundary_policy,
            $10,
            $11,
            $12,
            $13,
            $14,
            $15,
            $16::web_run_state,
            $17,
            coalesce($18, now()),
            $19,
            $20,
            $21
        )
        returning
            id,
            mutation_id,
            async_operation_id,
            workspace_id,
            library_id,
            mode::text as mode,
            seed_url,
            normalized_seed_url,
            boundary_policy::text as boundary_policy,
            max_depth,
            max_pages,
            crawl_allow_patterns,
            crawl_block_patterns,
            materialization_allow_patterns,
            materialization_block_patterns,
            run_state::text as run_state,
            requested_by_principal_id,
            requested_at,
            completed_at,
            failure_code,
            cancel_requested_at",
    )
    .bind(input.id)
    .bind(input.mutation_id)
    .bind(input.async_operation_id)
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.mode)
    .bind(input.seed_url)
    .bind(input.normalized_seed_url)
    .bind(input.boundary_policy)
    .bind(input.max_depth)
    .bind(input.max_pages)
    .bind(&input.crawl_allow_patterns)
    .bind(&input.crawl_block_patterns)
    .bind(&input.materialization_allow_patterns)
    .bind(&input.materialization_block_patterns)
    .bind(input.run_state)
    .bind(input.requested_by_principal_id)
    .bind(input.requested_at)
    .bind(input.completed_at)
    .bind(input.failure_code)
    .bind(input.cancel_requested_at)
    .fetch_one(postgres)
    .await
}

pub async fn get_web_ingest_run_by_id(
    postgres: &PgPool,
    run_id: Uuid,
) -> Result<Option<WebIngestRunRow>, sqlx::Error> {
    sqlx::query_as::<_, WebIngestRunRow>(
        "select
            id,
            mutation_id,
            async_operation_id,
            workspace_id,
            library_id,
            mode::text as mode,
            seed_url,
            normalized_seed_url,
            boundary_policy::text as boundary_policy,
            max_depth,
            max_pages,
            crawl_allow_patterns,
            crawl_block_patterns,
            materialization_allow_patterns,
            materialization_block_patterns,
            run_state::text as run_state,
            requested_by_principal_id,
            requested_at,
            completed_at,
            failure_code,
            cancel_requested_at
         from content_web_ingest_run
         where id = $1",
    )
    .bind(run_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_web_ingest_run_by_mutation_id(
    postgres: &PgPool,
    mutation_id: Uuid,
) -> Result<Option<WebIngestRunRow>, sqlx::Error> {
    sqlx::query_as::<_, WebIngestRunRow>(
        "select
            id,
            mutation_id,
            async_operation_id,
            workspace_id,
            library_id,
            mode::text as mode,
            seed_url,
            normalized_seed_url,
            boundary_policy::text as boundary_policy,
            max_depth,
            max_pages,
            crawl_allow_patterns,
            crawl_block_patterns,
            materialization_allow_patterns,
            materialization_block_patterns,
            run_state::text as run_state,
            requested_by_principal_id,
            requested_at,
            completed_at,
            failure_code,
            cancel_requested_at
         from content_web_ingest_run
         where mutation_id = $1",
    )
    .bind(mutation_id)
    .fetch_optional(postgres)
    .await
}

pub async fn list_web_ingest_runs(
    postgres: &PgPool,
    library_id: Uuid,
    limit: i64,
) -> Result<Vec<WebIngestRunRow>, sqlx::Error> {
    sqlx::query_as::<_, WebIngestRunRow>(
        "select
            id,
            mutation_id,
            async_operation_id,
            workspace_id,
            library_id,
            mode::text as mode,
            seed_url,
            normalized_seed_url,
            boundary_policy::text as boundary_policy,
            max_depth,
            max_pages,
            crawl_allow_patterns,
            crawl_block_patterns,
            materialization_allow_patterns,
            materialization_block_patterns,
            run_state::text as run_state,
            requested_by_principal_id,
            requested_at,
            completed_at,
            failure_code,
            cancel_requested_at
         from content_web_ingest_run
         where library_id = $1
         order by requested_at desc, id desc
         limit $2",
    )
    .bind(library_id)
    .bind(limit)
    .fetch_all(postgres)
    .await
}

#[derive(Debug, Clone, FromRow)]
pub struct WebRunCountsByRunRow {
    pub run_id: Uuid,
    #[sqlx(flatten)]
    pub counts: WebRunCountsRow,
}

/// Batched version of [`get_web_run_counts`] — returns one row per
/// requested `run_id` in a single indexed aggregation. Callers that
/// render a list of runs MUST use this helper; the per-id variant in a
/// loop is an N+1 hazard that on reference-sized libraries pushes the
/// web-runs endpoint past the browser timeout.
pub async fn list_web_run_counts_by_run_ids(
    postgres: &PgPool,
    run_ids: &[Uuid],
) -> Result<Vec<WebRunCountsByRunRow>, sqlx::Error> {
    if run_ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, WebRunCountsByRunRow>(
        "select
            run_id,
            count(*)::bigint as discovered,
            count(*) filter (
                where candidate_state in (
                    'eligible', 'queued', 'processing', 'materialized', 'processed', 'failed', 'canceled'
                )
            )::bigint as eligible,
            count(*) filter (where candidate_state = 'processed')::bigint as processed,
            count(*) filter (where candidate_state = 'queued')::bigint as queued,
            count(*) filter (where candidate_state in ('processing', 'materialized'))::bigint as processing,
            count(*) filter (where candidate_state = 'duplicate')::bigint as duplicates,
            count(*) filter (where candidate_state = 'excluded')::bigint as excluded,
            count(*) filter (where candidate_state = 'blocked')::bigint as blocked,
            count(*) filter (where candidate_state = 'failed')::bigint as failed,
            count(*) filter (where candidate_state = 'canceled')::bigint as canceled,
            max(updated_at) as last_activity_at
         from content_web_discovered_page
         where run_id = any($1)
         group by run_id",
    )
    .bind(run_ids)
    .fetch_all(postgres)
    .await
}

pub async fn update_web_ingest_run(
    postgres: &PgPool,
    run_id: Uuid,
    input: &UpdateWebIngestRun<'_>,
) -> Result<Option<WebIngestRunRow>, sqlx::Error> {
    sqlx::query_as::<_, WebIngestRunRow>(
        "update content_web_ingest_run
         set run_state = $2::web_run_state,
             completed_at = $3,
             failure_code = $4,
             cancel_requested_at = $5
         where id = $1
         returning
            id,
            mutation_id,
            async_operation_id,
            workspace_id,
            library_id,
            mode::text as mode,
            seed_url,
            normalized_seed_url,
            boundary_policy::text as boundary_policy,
            max_depth,
            max_pages,
            crawl_allow_patterns,
            crawl_block_patterns,
            materialization_allow_patterns,
            materialization_block_patterns,
            run_state::text as run_state,
            requested_by_principal_id,
            requested_at,
            completed_at,
            failure_code,
            cancel_requested_at",
    )
    .bind(run_id)
    .bind(input.run_state)
    .bind(input.completed_at)
    .bind(input.failure_code)
    .bind(input.cancel_requested_at)
    .fetch_optional(postgres)
    .await
}

pub async fn get_web_run_counts(
    postgres: &PgPool,
    run_id: Uuid,
) -> Result<WebRunCountsRow, sqlx::Error> {
    sqlx::query_as::<_, WebRunCountsRow>(
        "select
            count(*)::bigint as discovered,
            count(*) filter (
                where candidate_state in (
                    'eligible', 'queued', 'processing', 'materialized', 'processed', 'failed', 'canceled'
                )
            )::bigint as eligible,
            count(*) filter (where candidate_state = 'processed')::bigint as processed,
            count(*) filter (where candidate_state = 'queued')::bigint as queued,
            count(*) filter (where candidate_state in ('processing', 'materialized'))::bigint as processing,
            count(*) filter (where candidate_state = 'duplicate')::bigint as duplicates,
            count(*) filter (where candidate_state = 'excluded')::bigint as excluded,
            count(*) filter (where candidate_state = 'blocked')::bigint as blocked,
            count(*) filter (where candidate_state = 'failed')::bigint as failed,
            count(*) filter (where candidate_state = 'canceled')::bigint as canceled,
            max(updated_at) as last_activity_at
         from content_web_discovered_page
         where run_id = $1",
    )
    .bind(run_id)
    .fetch_one(postgres)
    .await
}

fn terminal_mutation_state(run_state: &str) -> anyhow::Result<Option<&'static str>> {
    match run_state {
        "completed" | "completed_partial" => Ok(Some("applied")),
        "failed" => Ok(Some("failed")),
        "canceled" => Ok(Some("canceled")),
        "processing" | "accepted" => Ok(None),
        other => anyhow::bail!("unsupported web run state {other}"),
    }
}

fn terminal_run_metadata(
    terminal_state: crate::shared::web::ingest::WebRunState,
    mode: &str,
) -> anyhow::Result<(&'static str, &'static str, Option<&'static str>)> {
    use crate::shared::web::ingest::{WebRunFailureCode, WebRunState};
    match terminal_state {
        WebRunState::Completed | WebRunState::CompletedPartial => Ok(("applied", "ready", None)),
        WebRunState::Canceled => Ok(("canceled", "canceled", None)),
        WebRunState::Failed => {
            let failure_code = match mode {
                "single_page" => WebRunFailureCode::WebCaptureMaterializationFailed.as_str(),
                "recursive_crawl" => WebRunFailureCode::RecursiveCrawlFailed.as_str(),
                other => anyhow::bail!("unsupported web ingest mode {other}"),
            };
            Ok(("failed", "failed", Some(failure_code)))
        }
        _ => anyhow::bail!(
            "web settlement derived non-terminal run state {}",
            terminal_state.as_str()
        ),
    }
}

/// Settles one web page from the terminal state of its exact content-mutation
/// item and, when it is the final in-flight page, atomically completes the
/// web-run aggregate.
///
/// Lock order starts with `content_mutation` and `content_mutation_item`, which
/// matches canonical content publication. The aggregate mutation lock also
/// serializes sibling page settlement, so two last-page callbacks cannot both
/// observe a stale in-flight count and leave the run stranded.
pub async fn settle_web_ingest_mutation_item(
    postgres: &PgPool,
    mutation_item_id: Uuid,
    settled_at: DateTime<Utc>,
) -> anyhow::Result<SettleWebIngestMutationItemOutcome> {
    let initial_page_links = sqlx::query_scalar::<_, Uuid>(
        "select id
         from content_web_discovered_page
         where mutation_item_id = $1",
    )
    .bind(mutation_item_id)
    .fetch_all(postgres)
    .await
    .context("locate web-page mutation-item link")?;
    if initial_page_links.is_empty() {
        return Ok(SettleWebIngestMutationItemOutcome::AwaitingLink { mutation_item_id });
    }
    anyhow::ensure!(
        initial_page_links.len() == 1,
        "web mutation item is linked to {} pages instead of exactly one",
        initial_page_links.len()
    );

    let mutation_id = sqlx::query_scalar::<_, Uuid>(
        "select mutation_id
         from content_mutation_item
         where id = $1",
    )
    .bind(mutation_item_id)
    .fetch_optional(postgres)
    .await
    .context("load web settlement mutation-item identity")?
    .context("web settlement mutation item disappeared")?;

    let mut transaction = postgres.begin().await.context("begin web-page settlement")?;
    let mutation = sqlx::query_as::<_, (Uuid, Uuid, String)>(
        "select workspace_id, library_id, mutation_state::text
         from content_mutation
         where id = $1
         for update",
    )
    .bind(mutation_id)
    .fetch_optional(&mut *transaction)
    .await
    .context("lock web-run aggregate mutation")?
    .context("web-run aggregate mutation disappeared")?;
    let item = sqlx::query_as::<_, (Uuid, Option<Uuid>, Option<Uuid>, String)>(
        "select mutation_id, document_id, result_revision_id, item_state::text
         from content_mutation_item
         where id = $1
         for update",
    )
    .bind(mutation_item_id)
    .fetch_optional(&mut *transaction)
    .await
    .context("lock exact web-page mutation item")?
    .context("web settlement mutation item disappeared while locking")?;
    anyhow::ensure!(item.0 == mutation_id, "web settlement mutation-item identity changed");

    let page_links = sqlx::query_as::<_, (Uuid, Uuid)>(
        "select id, run_id
         from content_web_discovered_page
         where mutation_item_id = $1",
    )
    .bind(mutation_item_id)
    .fetch_all(&mut *transaction)
    .await
    .context("locate exact web page for mutation item")?;
    if page_links.is_empty() {
        transaction.commit().await.context("finish unlinked web-page settlement")?;
        return Ok(SettleWebIngestMutationItemOutcome::AwaitingLink { mutation_item_id });
    }
    anyhow::ensure!(
        page_links.len() == 1,
        "web mutation item is linked to {} pages instead of exactly one",
        page_links.len()
    );
    let (page_id, run_id) = page_links[0];

    let run = sqlx::query_as::<_, WebSettlementRunRow>(
        "select
            id,
            mutation_id,
            async_operation_id,
            workspace_id,
            library_id,
            mode::text as mode,
            run_state::text as run_state,
            cancel_requested_at
         from content_web_ingest_run
         where id = $1
         for update",
    )
    .bind(run_id)
    .fetch_optional(&mut *transaction)
    .await
    .context("lock exact web ingest run for page settlement")?
    .context("web ingest run disappeared during page settlement")?;
    anyhow::ensure!(run.id == run_id, "web settlement run identity changed");
    anyhow::ensure!(run.mutation_id == mutation_id, "web page belongs to another mutation");
    anyhow::ensure!(run.workspace_id == mutation.0, "web settlement workspace mismatch");
    anyhow::ensure!(run.library_id == mutation.1, "web settlement library mismatch");

    let page = sqlx::query_as::<_, WebSettlementPageRow>(
        "select
            id,
            run_id,
            document_id,
            result_revision_id,
            candidate_state::text as candidate_state
         from content_web_discovered_page
         where id = $1
         for update",
    )
    .bind(page_id)
    .fetch_optional(&mut *transaction)
    .await
    .context("lock exact materialized web page")?
    .context("materialized web page disappeared during settlement")?;
    anyhow::ensure!(page.id == page_id, "web settlement page identity changed");
    anyhow::ensure!(page.run_id == run.id, "web settlement page moved to another run");
    anyhow::ensure!(page.document_id == item.1, "web settlement item document mismatch");
    anyhow::ensure!(
        page.result_revision_id == item.2,
        "web settlement item result revision mismatch"
    );

    if item.3 == "pending" {
        transaction.commit().await.context("finish pending web-page settlement")?;
        return Ok(SettleWebIngestMutationItemOutcome::AwaitingItem { page_id, run_id });
    }
    let candidate_state = match item.3.as_str() {
        "applied" => "processed",
        "skipped" => "duplicate",
        "failed" | "conflicted" => "failed",
        other => anyhow::bail!("unsupported terminal web mutation-item state {other}"),
    };
    anyhow::ensure!(
        page.candidate_state == "materialized" || page.candidate_state == candidate_state,
        "web page has incompatible state {} for terminal item state {}",
        page.candidate_state,
        item.3
    );
    if page.candidate_state != candidate_state {
        let page_update = sqlx::query(
            "update content_web_discovered_page
             set candidate_state = $2::web_candidate_state,
                 updated_at = $3
             where id = $1
               and run_id = $4
               and mutation_item_id = $5
               and candidate_state = 'materialized'",
        )
        .bind(page.id)
        .bind(candidate_state)
        .bind(settled_at)
        .bind(run.id)
        .bind(mutation_item_id)
        .execute(&mut *transaction)
        .await
        .context("settle exact materialized web page")?;
        anyhow::ensure!(page_update.rows_affected() == 1, "web page settlement authority changed");
    }

    if let Some(expected_mutation_state) = terminal_mutation_state(&run.run_state)? {
        anyhow::ensure!(
            mutation.2 == expected_mutation_state,
            "terminal web run and aggregate mutation disagree"
        );
        transaction.commit().await.context("finish idempotent web-page settlement")?;
        return Ok(SettleWebIngestMutationItemOutcome::Settled {
            page_id,
            run_id,
            candidate_state: candidate_state.to_string(),
            run_state: run.run_state,
            run_completed: true,
        });
    }

    let active_pages = sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from content_web_discovered_page
         where run_id = $1
           and candidate_state in ('discovered', 'eligible', 'queued', 'processing', 'materialized')",
    )
    .bind(run.id)
    .fetch_one(&mut *transaction)
    .await
    .context("count unsettled web pages")?;
    if active_pages > 0 || run.run_state != "processing" {
        let current_run_state = run.run_state;
        transaction.commit().await.context("finish partial web-page settlement")?;
        return Ok(SettleWebIngestMutationItemOutcome::Settled {
            page_id,
            run_id,
            candidate_state: candidate_state.to_string(),
            run_state: current_run_state,
            run_completed: false,
        });
    }

    let counts = sqlx::query_as::<_, WebRunCountsRow>(
        "select
            count(*)::bigint as discovered,
            count(*) filter (
                where candidate_state in (
                    'eligible', 'queued', 'processing', 'materialized', 'processed', 'failed', 'canceled'
                )
            )::bigint as eligible,
            count(*) filter (where candidate_state = 'processed')::bigint as processed,
            count(*) filter (where candidate_state = 'queued')::bigint as queued,
            count(*) filter (where candidate_state in ('processing', 'materialized'))::bigint as processing,
            count(*) filter (where candidate_state = 'duplicate')::bigint as duplicates,
            count(*) filter (where candidate_state = 'excluded')::bigint as excluded,
            count(*) filter (where candidate_state = 'blocked')::bigint as blocked,
            count(*) filter (where candidate_state = 'failed')::bigint as failed,
            count(*) filter (where candidate_state = 'canceled')::bigint as canceled,
            max(updated_at) as last_activity_at
         from content_web_discovered_page
         where run_id = $1",
    )
    .bind(run.id)
    .fetch_one(&mut *transaction)
    .await
    .context("derive terminal web-run counts")?;
    let mut terminal_state = crate::shared::web::ingest::derive_terminal_run_state(
        &crate::shared::web::ingest::WebRunCounts {
            discovered: counts.discovered,
            eligible: counts.eligible,
            processed: counts.processed,
            queued: counts.queued,
            processing: counts.processing,
            duplicates: counts.duplicates,
            excluded: counts.excluded,
            blocked: counts.blocked,
            failed: counts.failed,
            canceled: counts.canceled,
        },
    );
    if run.cancel_requested_at.is_some() {
        terminal_state = crate::shared::web::ingest::WebRunState::Canceled;
    }
    let terminal_run_state = terminal_state.as_str();
    let (mutation_state, async_status, failure_code) =
        terminal_run_metadata(terminal_state, &run.mode)?;

    let run_update = sqlx::query(
        "update content_web_ingest_run
         set run_state = $2::web_run_state,
             completed_at = $3,
             failure_code = $4
         where id = $1
           and mutation_id = $5
           and workspace_id = $6
           and library_id = $7
           and run_state = 'processing'",
    )
    .bind(run.id)
    .bind(terminal_run_state)
    .bind(settled_at)
    .bind(failure_code)
    .bind(mutation_id)
    .bind(run.workspace_id)
    .bind(run.library_id)
    .execute(&mut *transaction)
    .await
    .context("complete exact web ingest run")?;
    anyhow::ensure!(run_update.rows_affected() == 1, "web-run settlement authority changed");

    let mutation_update = sqlx::query(
        "update content_mutation
         set mutation_state = $2::content_mutation_state,
             completed_at = $3,
             failure_code = $4,
             conflict_code = null
         where id = $1
           and workspace_id = $5
           and library_id = $6
           and mutation_state in ('accepted', 'running')",
    )
    .bind(mutation_id)
    .bind(mutation_state)
    .bind(settled_at)
    .bind(failure_code)
    .bind(run.workspace_id)
    .bind(run.library_id)
    .execute(&mut *transaction)
    .await
    .context("complete web-run aggregate mutation")?;
    anyhow::ensure!(
        mutation_update.rows_affected() == 1,
        "web-run aggregate mutation settlement authority changed"
    );

    if let Some(async_operation_id) = run.async_operation_id {
        let operation_update = sqlx::query(
            "update ops_async_operation
             set status = $2::ops_async_operation_status,
                 completed_at = $3,
                 failure_code = $4
             where id = $1
               and workspace_id = $5
               and library_id = $6
               and subject_kind = 'content_web_ingest_run'
               and subject_id = $7
               and status in ('accepted', 'processing')",
        )
        .bind(async_operation_id)
        .bind(async_status)
        .bind(settled_at)
        .bind(failure_code)
        .bind(run.workspace_id)
        .bind(run.library_id)
        .bind(run.id)
        .execute(&mut *transaction)
        .await
        .context("complete web-run async operation")?;
        anyhow::ensure!(
            operation_update.rows_affected() == 1,
            "web-run async operation settlement authority changed"
        );
    }

    transaction.commit().await.context("commit web-page settlement")?;
    Ok(SettleWebIngestMutationItemOutcome::Settled {
        page_id,
        run_id,
        candidate_state: candidate_state.to_string(),
        run_state: terminal_run_state.to_string(),
        run_completed: true,
    })
}
