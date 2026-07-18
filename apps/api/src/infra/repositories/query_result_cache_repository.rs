use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

use super::query_repository;

#[derive(Debug, Clone, FromRow)]
pub struct QueryResultCacheRow {
    pub cache_key: String,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub source_execution_id: Uuid,
    pub readable_content_fingerprint: String,
    pub graph_projection_version: i64,
    pub graph_topology_generation: i64,
    pub binding_fingerprint: String,
    pub hit_count: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Remaining absolute lifetime computed by `PostgreSQL`'s clock.
    pub remaining_ttl_seconds: i64,
}

#[derive(Debug, Clone)]
pub struct UpsertQueryResultCacheInput<'a> {
    pub cache_key: &'a str,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub source_execution_id: Uuid,
    /// Source generation observed before the answer pipeline started. The
    /// winner is persisted only while the library still has this generation.
    pub expected_source_truth_version: i64,
    pub readable_content_fingerprint: &'a str,
    pub graph_projection_version: i64,
    pub graph_topology_generation: i64,
    pub binding_fingerprint: &'a str,
    /// Absolute cache lifetime. `PostgreSQL` computes freshness and replacement
    /// from its own clock so replica clock skew cannot extend a winner.
    pub ttl_seconds: u64,
}

#[derive(Debug, Clone, FromRow)]
pub struct QueryExecutionReplayRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub conversation_id: Uuid,
    pub request_turn_id: Uuid,
    pub response_turn_id: Uuid,
    pub source_execution_id: Uuid,
    pub cache_key: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct NewQueryExecutionReplay<'a> {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub conversation_id: Uuid,
    pub request_turn_id: Uuid,
    pub response_turn_id: Uuid,
    pub source_execution_id: Uuid,
    pub cache_key: &'a str,
}

#[derive(Debug, Clone)]
pub struct CreateQueryExecutionReplayInput<'a> {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub conversation_id: Uuid,
    pub request_turn_id: Uuid,
    pub source_execution_id: Uuid,
    /// Source generation encoded in `cache_key`. Replay materialization takes
    /// a row lock and verifies this value in the same transaction as the turn.
    pub expected_source_truth_version: i64,
    pub cache_key: &'a str,
    /// Absolute cache lifetime, evaluated again with `PostgreSQL`'s clock while
    /// the exact winner row is locked for the replay transaction.
    pub ttl_seconds: u64,
}

pub async fn get_query_result_cache(
    postgres: &PgPool,
    cache_key: &str,
    ttl_seconds: u64,
) -> Result<Option<QueryResultCacheRow>, sqlx::Error> {
    let ttl_seconds = i64::try_from(ttl_seconds).unwrap_or(i64::MAX);
    sqlx::query_as::<_, QueryResultCacheRow>(
        "select
            cache_key,
            workspace_id,
            library_id,
            source_execution_id,
            readable_content_fingerprint,
            graph_projection_version,
            graph_topology_generation,
            binding_fingerprint,
            hit_count,
            created_at,
            updated_at,
            greatest(
                0,
                ceil(extract(epoch from (
                    updated_at + make_interval(secs => $2::double precision) - now()
                )))::bigint
            ) as remaining_ttl_seconds
         from query_result_cache
         where cache_key = $1
           and updated_at >= now() - make_interval(secs => $2::double precision)",
    )
    .bind(cache_key)
    .bind(ttl_seconds)
    .fetch_optional(postgres)
    .await
}

pub async fn delete_query_result_cache(
    postgres: &PgPool,
    cache_key: &str,
    expected_source_execution_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "delete from query_result_cache
         where cache_key = $1
           and source_execution_id = $2",
    )
    .bind(cache_key)
    .bind(expected_source_execution_id)
    .execute(postgres)
    .await?;
    Ok(result.rows_affected())
}

pub async fn upsert_query_result_cache_winner(
    postgres: &PgPool,
    input: &UpsertQueryResultCacheInput<'_>,
) -> Result<Option<QueryResultCacheRow>, sqlx::Error> {
    let ttl_seconds = i64::try_from(input.ttl_seconds).unwrap_or(i64::MAX);
    sqlx::query_as::<_, QueryResultCacheRow>(
        "with current_library as materialized (
            select id
            from catalog_library
            where id = $3
              and workspace_id = $2
              and coalesce(source_truth_version, 1) = $9
            for share
         )
         insert into query_result_cache (
            cache_key,
            workspace_id,
            library_id,
            source_execution_id,
            readable_content_fingerprint,
            graph_projection_version,
            graph_topology_generation,
            binding_fingerprint,
            hit_count,
            created_at,
            updated_at
        )
        select $1, $2, $3, $4, $5, $6, $7, $8, 0, now(), now()
        from current_library
        on conflict (cache_key) do update
            set workspace_id = case
                    when query_result_cache.updated_at
                         < now() - make_interval(secs => $10::double precision)
                    then excluded.workspace_id
                    else query_result_cache.workspace_id
                end,
                library_id = case
                    when query_result_cache.updated_at
                         < now() - make_interval(secs => $10::double precision)
                    then excluded.library_id
                    else query_result_cache.library_id
                end,
                source_execution_id = case
                    when query_result_cache.updated_at
                         < now() - make_interval(secs => $10::double precision)
                    then excluded.source_execution_id
                    else query_result_cache.source_execution_id
                end,
                readable_content_fingerprint = case
                    when query_result_cache.updated_at
                         < now() - make_interval(secs => $10::double precision)
                    then excluded.readable_content_fingerprint
                    else query_result_cache.readable_content_fingerprint
                end,
                graph_projection_version = case
                    when query_result_cache.updated_at
                         < now() - make_interval(secs => $10::double precision)
                    then excluded.graph_projection_version
                    else query_result_cache.graph_projection_version
                end,
                graph_topology_generation = case
                    when query_result_cache.updated_at
                         < now() - make_interval(secs => $10::double precision)
                    then excluded.graph_topology_generation
                    else query_result_cache.graph_topology_generation
                end,
                binding_fingerprint = case
                    when query_result_cache.updated_at
                         < now() - make_interval(secs => $10::double precision)
                    then excluded.binding_fingerprint
                    else query_result_cache.binding_fingerprint
                end,
                hit_count = case
                    when query_result_cache.updated_at
                         < now() - make_interval(secs => $10::double precision)
                    then 0
                    else query_result_cache.hit_count + 1
                end,
                created_at = case
                    when query_result_cache.updated_at
                         < now() - make_interval(secs => $10::double precision)
                    then now()
                    else query_result_cache.created_at
                end,
                updated_at = case
                    when query_result_cache.updated_at
                         < now() - make_interval(secs => $10::double precision)
                    then now()
                    else query_result_cache.updated_at
                end
        returning
            cache_key,
            workspace_id,
            library_id,
            source_execution_id,
            readable_content_fingerprint,
            graph_projection_version,
            graph_topology_generation,
            binding_fingerprint,
            hit_count,
            created_at,
            updated_at,
            greatest(
                0,
                ceil(extract(epoch from (
                    updated_at + make_interval(secs => $10::double precision) - now()
                )))::bigint
            ) as remaining_ttl_seconds",
    )
    .bind(input.cache_key)
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.source_execution_id)
    .bind(input.readable_content_fingerprint)
    .bind(input.graph_projection_version)
    .bind(input.graph_topology_generation)
    .bind(input.binding_fingerprint)
    .bind(input.expected_source_truth_version)
    .bind(ttl_seconds)
    .fetch_optional(postgres)
    .await
}

/// Atomically materializes the assistant replay turn and its cache audit row.
///
/// A uniqueness/FK failure in the audit insert rolls the turn back as well, so
/// clients never observe an assistant answer without replay provenance.
pub async fn create_query_execution_replay(
    postgres: &PgPool,
    input: &CreateQueryExecutionReplayInput<'_>,
) -> Result<Option<(query_repository::QueryTurnRow, QueryExecutionReplayRow)>, sqlx::Error> {
    let ttl_seconds = i64::try_from(input.ttl_seconds).unwrap_or(i64::MAX);
    let mut transaction = postgres.begin().await?;
    // Keep one deterministic lock order across source mutations, winner
    // replacement, conversation deletion, and replay materialization:
    // library -> target conversation/request -> source execution -> cache.
    // The library lock conflicts with the `FOR NO KEY UPDATE` lock taken by
    // every committed source-generation change.
    let library_is_current = sqlx::query_scalar::<_, bool>(
        "select true
         from catalog_library
         where id = $1
           and workspace_id = $2
           and coalesce(source_truth_version, 1) = $3
         for share",
    )
    .bind(input.library_id)
    .bind(input.workspace_id)
    .bind(input.expected_source_truth_version)
    .fetch_optional(&mut *transaction)
    .await?
    .unwrap_or(false);
    if !library_is_current {
        transaction.rollback().await?;
        return Ok(None);
    }

    let target_request_is_current = sqlx::query_scalar::<_, bool>(
        "select true
         from query_conversation as conversation
         join query_turn as request_turn
           on request_turn.id = $4
          and request_turn.conversation_id = conversation.id
          and request_turn.turn_kind = 'user'
         where conversation.id = $3
           and conversation.workspace_id = $1
           and conversation.library_id = $2
           and conversation.conversation_state = 'active'
         -- `create_turn_in_connection` updates the conversation to allocate a
         -- monotonic turn index. Lock it in the final mode immediately so two
         -- parallel cache hits serialize instead of deadlocking on SHARE ->
         -- UPDATE lock upgrades. The request FK makes a concurrent direct
         -- request deletion fail or roll this transaction back safely.
         for update of conversation",
    )
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.conversation_id)
    .bind(input.request_turn_id)
    .fetch_optional(&mut *transaction)
    .await?
    .unwrap_or(false);
    if !target_request_is_current {
        transaction.rollback().await?;
        return Ok(None);
    }

    // Re-read the canonical response and its terminal verification evidence
    // under row locks. The caller's previously hydrated detail is useful for
    // the response payload, but it is not trusted to authorize a replay.
    let source_identity = sqlx::query_as::<_, (String, Uuid)>(
        "select response_turn.content_text, bundle.bundle_id
         from query_execution as source_execution
         join runtime_execution as source_runtime
           on source_runtime.id = source_execution.runtime_execution_id
         join query_turn as response_turn
           on response_turn.id = source_execution.response_turn_id
          and response_turn.conversation_id = source_execution.conversation_id
          and response_turn.execution_id = source_execution.id
          and response_turn.turn_kind = 'assistant'
         join knowledge_context_bundle as bundle
           on bundle.bundle_id = source_execution.context_bundle_id
          and bundle.query_execution_id = source_execution.id
         where source_execution.id = $1
           and source_execution.workspace_id = $2
           and source_execution.library_id = $3
           and source_execution.failure_code is null
           and source_execution.completed_at is not null
           and source_runtime.owner_kind = 'query_execution'
           and source_runtime.owner_id = source_execution.id
           and source_runtime.lifecycle_state = 'completed'
           and source_runtime.failure_code is null
           and bundle.workspace_id = $2
           and bundle.library_id = $3
           and bundle.bundle_state = 'ready'
           and bundle.verification_state = 'verified'
           and nullif(btrim(response_turn.content_text), '') is not null
         for share of source_execution, source_runtime, response_turn, bundle",
    )
    .bind(input.source_execution_id)
    .bind(input.workspace_id)
    .bind(input.library_id)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some((source_response_content, source_bundle_id)) = source_identity else {
        transaction.rollback().await?;
        return Ok(None);
    };

    // A verified label alone is insufficient: lock at least one canonical
    // grounding reference that the answer was verified against. Each CTE is
    // bounded to one row; keeping the qualifying child locked prevents a
    // concurrent replace/delete from erasing all grounding between this check
    // and replay commit.
    let grounding_is_current = sqlx::query_scalar::<_, bool>(
        "with direct_chunk as materialized (
            select 1
            from query_chunk_reference as reference
            where reference.execution_id = $1
            limit 1
            for share of reference
         ), bundle_chunk as materialized (
            select 1
            from knowledge_bundle_chunk as reference
            where reference.bundle_id = $2
              and reference.library_id = $3
            limit 1
            for share of reference
         ), bundle_entity as materialized (
            select 1
            from knowledge_bundle_entity as reference
            where reference.bundle_id = $2
              and reference.library_id = $3
            limit 1
            for share of reference
         ), bundle_relation as materialized (
            select 1
            from knowledge_bundle_relation as reference
            where reference.bundle_id = $2
              and reference.library_id = $3
            limit 1
            for share of reference
         ), bundle_evidence as materialized (
            select 1
            from knowledge_bundle_evidence as reference
            where reference.bundle_id = $2
              and reference.library_id = $3
            limit 1
            for share of reference
         ), selected_fact as materialized (
            select 1
            from knowledge_context_bundle as bundle
            join knowledge_technical_fact as fact
              on fact.fact_id = any(bundle.selected_fact_ids)
             and fact.workspace_id = $4
             and fact.library_id = $3
            where bundle.bundle_id = $2
            limit 1
            for share of fact
         )
         select exists(select 1 from direct_chunk)
             or exists(select 1 from bundle_chunk)
             or exists(select 1 from bundle_entity)
             or exists(select 1 from bundle_relation)
             or exists(select 1 from bundle_evidence)
             or exists(select 1 from selected_fact)",
    )
    .bind(input.source_execution_id)
    .bind(source_bundle_id)
    .bind(input.library_id)
    .bind(input.workspace_id)
    .fetch_one(&mut *transaction)
    .await?;
    if !grounding_is_current {
        transaction.rollback().await?;
        return Ok(None);
    }

    // The cache row is checked last, after potentially slower source
    // validation, so an expiry, GC delete, or A->B replacement that happened
    // after the outer read cannot authorize stale A. `FOR SHARE` keeps the
    // exact winner stable until the assistant turn and audit row commit.
    let exact_winner_is_fresh = sqlx::query_scalar::<_, bool>(
        "select true
         from query_result_cache as cache
         where cache.cache_key = $1
           and cache.workspace_id = $2
           and cache.library_id = $3
           and cache.source_execution_id = $4
           and cache.updated_at
               >= clock_timestamp() - make_interval(secs => $5::double precision)
         for share of cache",
    )
    .bind(input.cache_key)
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.source_execution_id)
    .bind(ttl_seconds)
    .fetch_optional(&mut *transaction)
    .await?
    .unwrap_or(false);
    if !exact_winner_is_fresh {
        transaction.rollback().await?;
        return Ok(None);
    }

    let response_turn = query_repository::create_turn_in_connection(
        &mut transaction,
        &query_repository::NewQueryTurn {
            conversation_id: input.conversation_id,
            turn_kind: "assistant",
            author_principal_id: None,
            content_text: &source_response_content,
            execution_id: Some(input.source_execution_id),
        },
    )
    .await?;
    let replay = record_query_execution_replay_in_connection(
        &mut transaction,
        &NewQueryExecutionReplay {
            workspace_id: input.workspace_id,
            library_id: input.library_id,
            conversation_id: input.conversation_id,
            request_turn_id: input.request_turn_id,
            response_turn_id: response_turn.id,
            source_execution_id: input.source_execution_id,
            cache_key: input.cache_key,
        },
    )
    .await?;
    transaction.commit().await?;
    Ok(Some((response_turn, replay)))
}

async fn record_query_execution_replay_in_connection(
    connection: &mut PgConnection,
    input: &NewQueryExecutionReplay<'_>,
) -> Result<QueryExecutionReplayRow, sqlx::Error> {
    sqlx::query_as::<_, QueryExecutionReplayRow>(
        "insert into query_execution_replay (
            id,
            workspace_id,
            library_id,
            conversation_id,
            request_turn_id,
            response_turn_id,
            source_execution_id,
            cache_key,
            created_at
        )
        values ($1, $2, $3, $4, $5, $6, $7, $8, now())
        returning
            id,
            workspace_id,
            library_id,
            conversation_id,
            request_turn_id,
            response_turn_id,
            source_execution_id,
            cache_key,
            created_at",
    )
    .bind(Uuid::now_v7())
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.conversation_id)
    .bind(input.request_turn_id)
    .bind(input.response_turn_id)
    .bind(input.source_execution_id)
    .bind(input.cache_key)
    .fetch_one(connection)
    .await
}

/// Hard ceiling for one `PostgreSQL` result-cache garbage-collection statement.
/// The scheduler may request less, but no caller can turn a maintenance tick
/// into an unbounded DELETE.
pub const MAX_QUERY_RESULT_CACHE_GC_BATCH_LIMIT: i64 = 500;

/// Hard ceiling for the post-sweep backlog sample. One row beyond the delete
/// batch size distinguishes a small tail from at least another full batch,
/// while the supporting `(updated_at, cache_key)` index keeps the probe's work
/// independent of the total cache-table cardinality.
pub const MAX_QUERY_RESULT_CACHE_GC_BACKLOG_PROBE_LIMIT: i64 =
    MAX_QUERY_RESULT_CACHE_GC_BATCH_LIMIT + 1;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct QueryResultCacheBacklogProbe {
    /// Expired rows visited by the bounded, oldest-first sample.
    pub sampled_expired_rows: u64,
    /// Effective repository-enforced sample ceiling.
    pub sample_limit: u64,
    /// How long the oldest sampled row has been expired, not its total age.
    pub oldest_expired_age_seconds: Option<f64>,
}

impl QueryResultCacheBacklogProbe {
    #[must_use]
    pub const fn sample_at_capacity(self) -> bool {
        self.sample_limit > 0 && self.sampled_expired_rows == self.sample_limit
    }
}

/// Deletes at most one bounded batch of expired result-cache winners.
///
/// Freshness is evaluated with `PostgreSQL`'s `now()` so node clock skew cannot
/// delete a winner early or retain it indefinitely. Candidate row locks use
/// `SKIP LOCKED`, allowing multiple maintenance workers to make progress
/// without waiting for cache writers or for each other. Replay audit rows in
/// `query_execution_replay` are conversation-owned provenance: they cascade
/// with the target conversation and deliberately are never selected by this
/// short-lived winner-cache operation. Conversation-cap enforcement also
/// retains a source conversation while an external target conversation still
/// references it; do not apply winner TTL as replay-provenance retention.
pub async fn delete_expired_query_result_cache_batch(
    postgres: &PgPool,
    ttl_seconds: u64,
    batch_limit: i64,
) -> Result<u64, sqlx::Error> {
    if batch_limit <= 0 {
        return Ok(0);
    }
    let batch_limit = batch_limit.min(MAX_QUERY_RESULT_CACHE_GC_BATCH_LIMIT);
    let ttl_seconds = i64::try_from(ttl_seconds).unwrap_or(i64::MAX);
    let deleted = sqlx::query_scalar::<_, i64>(
        "with expired as materialized (
            select candidate.cache_key
            from query_result_cache candidate
            where candidate.updated_at
                  < now() - make_interval(secs => $1::double precision)
            order by candidate.updated_at asc, candidate.cache_key asc
            for update skip locked
            limit $2
         ), deleted as (
            delete from query_result_cache cache
            using expired
            where cache.cache_key = expired.cache_key
              and cache.updated_at
                  < now() - make_interval(secs => $1::double precision)
            returning 1
         )
         select count(*)::bigint from deleted",
    )
    .bind(ttl_seconds)
    .bind(batch_limit)
    .fetch_one(postgres)
    .await?;
    Ok(u64::try_from(deleted).unwrap_or(0))
}

/// Samples the oldest expired result-cache winners without a full-table count.
///
/// `expired_sample` is materialized after an index-ordered `LIMIT`, so both the
/// count and age aggregate visit at most 501 rows even when the backlog is much
/// larger. `sample_at_capacity()` means the reported row count is a lower
/// bound; operators should combine it with the GC budget-exhaustion counter and
/// oldest-expired-age trend rather than interpreting it as exact cardinality.
pub async fn probe_expired_query_result_cache_backlog(
    postgres: &PgPool,
    ttl_seconds: u64,
    sample_limit: i64,
) -> Result<QueryResultCacheBacklogProbe, sqlx::Error> {
    if sample_limit <= 0 {
        return Ok(QueryResultCacheBacklogProbe::default());
    }
    let sample_limit = sample_limit.min(MAX_QUERY_RESULT_CACHE_GC_BACKLOG_PROBE_LIMIT);
    let ttl_seconds = i64::try_from(ttl_seconds).unwrap_or(i64::MAX);
    let (sampled_expired_rows, oldest_expired_age_seconds) =
        sqlx::query_as::<_, (i64, Option<f64>)>(
            "with expired_sample as materialized (
                select candidate.updated_at
                from query_result_cache candidate
                where candidate.updated_at
                      < now() - make_interval(secs => $1::double precision)
                order by candidate.updated_at asc, candidate.cache_key asc
                limit $2
             )
             select
                count(*)::bigint,
                case
                    when min(updated_at) is null then null
                    else greatest(
                        0::double precision,
                        extract(epoch from (now() - min(updated_at)))::double precision
                            - $1::double precision
                    )
                end
             from expired_sample",
        )
        .bind(ttl_seconds)
        .bind(sample_limit)
        .fetch_one(postgres)
        .await?;

    Ok(QueryResultCacheBacklogProbe {
        sampled_expired_rows: u64::try_from(sampled_expired_rows).unwrap_or(0),
        sample_limit: u64::try_from(sample_limit).unwrap_or(0),
        oldest_expired_age_seconds,
    })
}
