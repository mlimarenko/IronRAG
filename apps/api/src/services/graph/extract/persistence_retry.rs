//! Bounded retry for the fenced graph-extraction owner-record writes.
//!
//! Every fenced owner-record write in the parent module acquires the shared
//! ingest-revision graph write guard, and that guard opens its transaction with
//! `set local idle_in_transaction_session_timeout = '60s'` (see
//! `apps/api/src/infra/repositories/runtime_graph_repository/coordination.rs`).
//! PostgreSQL therefore closes a stranded guard session on its own instead of
//! stalling every graph-refresh worker for the library. A session killed that
//! way surfaces as `25P03`, and a transient connection or contention failure
//! surfaces as one of the other retryable states below. Such a failure only
//! means the write never happened; it must not terminally fail the ingest
//! stage, so the owner-record writes are retried a bounded number of times.

use std::time::Duration;

const GRAPH_PERSISTENCE_MAX_RETRIES: usize = 1;
const GRAPH_PERSISTENCE_BASE_BACKOFF_MS: u64 = 50;

const PG_SQLSTATE_IDLE_IN_TRANSACTION_SESSION_TIMEOUT: &str = "25P03";
const PG_SQLSTATE_SERIALIZATION_FAILURE: &str = "40001";
const PG_SQLSTATE_DEADLOCK_DETECTED: &str = "40P01";
const PG_SQLSTATE_LOCK_NOT_AVAILABLE: &str = "55P03";
const PG_SQLSTATE_ADMIN_SHUTDOWN: &str = "57P01";
const PG_SQLSTATE_CANNOT_CONNECT_NOW: &str = "57P03";
const PG_SQLSTATE_TOO_MANY_CONNECTIONS: &str = "53300";
const PG_SQLSTATE_CONNECTION_FAILURE: &str = "08006";

/// Returns whether a transient database failure should be retried.
///
/// The retryable set is deliberately narrow: session/connection loss and
/// contention states that mean the write did not commit. `23505`
/// (unique_violation) and `42501` (insufficient_privilege) are permanent for
/// this call site and stay non-retryable.
pub(super) fn is_retryable_graph_persistence_error(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed | sqlx::Error::Io(_) => true,
        sqlx::Error::Database(database_error) => database_error.code().is_some_and(|code| {
            matches!(
                code.as_ref(),
                PG_SQLSTATE_IDLE_IN_TRANSACTION_SESSION_TIMEOUT
                    | PG_SQLSTATE_SERIALIZATION_FAILURE
                    | PG_SQLSTATE_DEADLOCK_DETECTED
                    | PG_SQLSTATE_LOCK_NOT_AVAILABLE
                    | PG_SQLSTATE_ADMIN_SHUTDOWN
                    | PG_SQLSTATE_CANNOT_CONNECT_NOW
                    | PG_SQLSTATE_TOO_MANY_CONNECTIONS
                    | PG_SQLSTATE_CONNECTION_FAILURE
            )
        }),
        _ => false,
    }
}

/// Decides what a `23505` conflict means given the row the conflicting insert
/// may have committed before its acknowledgement was lost.
///
/// `Ok(Some(value))` adopts the stored row. `Ok(None)` keeps the original
/// conflict because the row really is absent. A read failure is surfaced as
/// itself: a transient failure of the adoption read must not be reported as a
/// permanent conflict, or the retry this module exists for is defeated.
pub(super) fn resolve_unique_conflict<T>(
    existing: Result<Option<T>, sqlx::Error>,
    conflict: sqlx::Error,
) -> Result<T, sqlx::Error> {
    match existing {
        Ok(Some(value)) => Ok(value),
        Ok(None) => Err(conflict),
        Err(read_error) => Err(read_error),
    }
}

const fn graph_persistence_backoff(retry_count: usize) -> Duration {
    Duration::from_millis(GRAPH_PERSISTENCE_BASE_BACKOFF_MS * (1_u64 << retry_count))
}

/// Runs one owner-record write, retrying transient database failures.
///
/// Attempts at most [`GRAPH_PERSISTENCE_MAX_RETRIES`] retries after the first
/// call with a 50 ms backoff. Non-retryable errors return
/// immediately, and the final error is returned once the retries are spent.
pub(super) async fn retry_graph_persistence<T, F, Fut>(mut operation: F) -> Result<T, sqlx::Error>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, sqlx::Error>>,
{
    let mut retry_count = 0;
    loop {
        match operation().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                if is_retryable_graph_persistence_error(&error)
                    && retry_count < GRAPH_PERSISTENCE_MAX_RETRIES
                {
                    let backoff = graph_persistence_backoff(retry_count);
                    retry_count += 1;
                    tracing::warn!(
                        attempt = retry_count,
                        %error,
                        "retrying graph extraction owner-record write after transient database failure"
                    );
                    tokio::time::sleep(backoff).await;
                    continue;
                }
                return Err(error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    mod native {
        use std::sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        };
        use std::time::Duration;

        use anyhow::Result;
        use serde_json::json;
        use sqlx::{PgPool, postgres::PgPoolOptions};
        use uuid::Uuid;

        use super::super::{is_retryable_graph_persistence_error, retry_graph_persistence};
        use crate::infra::{
            knowledge_plane::{IngestAttemptMutationOutcome, IngestRevisionWriteAuthority},
            repositories::{
                catalog_repository, ingest_repository, runtime_graph_repository, runtime_repository,
            },
        };

        async fn seed_owner(
            pool: &PgPool,
        ) -> Result<(
            IngestRevisionWriteAuthority,
            runtime_graph_repository::CreateRuntimeGraphExtractionRecordInput,
            Uuid,
        )> {
            let workspace =
                catalog_repository::create_workspace(pool, "native-retry", "Native retry", None)
                    .await?;
            let library = catalog_repository::create_library(
                pool,
                workspace.id,
                "native-retry",
                "Native retry",
                None,
                None,
            )
            .await?;
            let document_id = sqlx::query_scalar::<_, Uuid>(
                "insert into content_document (workspace_id, library_id, external_key) values ($1, $2, 'native-retry') returning id",
            ).bind(workspace.id).bind(library.id).fetch_one(pool).await?;
            let revision_id = sqlx::query_scalar::<_, Uuid>(
                "insert into content_revision (document_id, workspace_id, library_id, revision_number, content_source_kind, checksum, mime_type, byte_size)
                 values ($1, $2, $3, 1, 'upload', 'native-retry', 'text/plain', 1) returning id",
            ).bind(document_id).bind(workspace.id).bind(library.id).fetch_one(pool).await?;
            let chunk_id = sqlx::query_scalar::<_, Uuid>(
                "insert into content_chunk (revision_id, chunk_index, start_offset, end_offset, normalized_text, text_checksum)
                 values ($1, 0, 0, 1, 'x', 'native-retry') returning id",
            ).bind(revision_id).fetch_one(pool).await?;
            let job = ingest_repository::create_ingest_job(
                pool,
                &ingest_repository::NewIngestJob {
                    workspace_id: workspace.id,
                    library_id: library.id,
                    mutation_id: None,
                    mutation_item_id: None,
                    connector_id: None,
                    async_operation_id: None,
                    knowledge_document_id: Some(document_id),
                    knowledge_revision_id: Some(revision_id),
                    job_kind: "graph_refresh".into(),
                    queue_state: "leased".into(),
                    priority: 200,
                    dedupe_key: None,
                    queued_at: None,
                    available_at: None,
                    completed_at: None,
                },
            )
            .await?;
            let attempt = ingest_repository::create_ingest_attempt(
                pool,
                &ingest_repository::NewIngestAttempt {
                    job_id: job.id,
                    attempt_number: 1,
                    worker_principal_id: None,
                    lease_token: None,
                    knowledge_generation_id: None,
                    attempt_state: "leased".into(),
                    current_stage: Some("extract_graph".into()),
                    started_at: None,
                    heartbeat_at: None,
                    finished_at: None,
                    failure_class: None,
                    failure_code: None,
                    failure_message: None,
                    progress_percent: 90,
                    retryable: false,
                },
            )
            .await?;
            let runtime_execution_id = Uuid::now_v7();
            runtime_repository::create_runtime_execution(
                pool,
                &runtime_repository::NewRuntimeExecution {
                    id: runtime_execution_id,
                    owner_kind: "graph_extraction_attempt",
                    owner_id: attempt.id,
                    task_kind: "graph_extract",
                    surface_kind: "worker",
                    contract_name: "native-retry",
                    contract_version: "test",
                    lifecycle_state: "running",
                    active_stage: None,
                    turn_budget: 1,
                    turn_count: 0,
                    parallel_action_limit: 1,
                    failure_code: None,
                    failure_summary_redacted: None,
                    parent_execution_id: None,
                },
            )
            .await?;
            Ok((
                IngestRevisionWriteAuthority {
                    workspace_id: workspace.id,
                    library_id: library.id,
                    attempt_id: attempt.id,
                    revision_id,
                },
                runtime_graph_repository::CreateRuntimeGraphExtractionRecordInput {
                    id: Uuid::now_v7(),
                    runtime_execution_id,
                    library_id: library.id,
                    document_id,
                    chunk_id,
                    provider_kind: "native-fixture".into(),
                    model_name: "fixture".into(),
                    extraction_version: "test".into(),
                    prompt_hash: "native-retry".into(),
                    status: "ready".into(),
                    raw_output_json: json!({}),
                    normalized_output_json: json!({"entities": [], "relations": []}),
                    glean_pass_count: 0,
                    error_message: None,
                },
                job.id,
            ))
        }

        // sqlx creates a fresh database and applies the real migrations; DATABASE_URL
        // is supplied explicitly by the native Make target, never by application config.
        #[sqlx::test(migrations = "./migrations")]
        #[ignore = "requires owned PostgreSQL with pgvector"]
        async fn killed_guard_session_reconnects_once_and_commits_one_owner(
            pool: PgPool,
        ) -> Result<()> {
            let _subscriber = tracing::subscriber::set_default(
                tracing_subscriber::fmt().with_test_writer().finish(),
            );
            let (authority, input, job_id) = seed_owner(&pool).await?;
            let application_name = format!("owner-native-{}", Uuid::now_v7());
            let target = PgPoolOptions::new()
                .max_connections(1)
                .connect_with(
                    pool.connect_options().as_ref().clone().application_name(&application_name),
                )
                .await?;
            let first_pid: i32 =
                sqlx::query_scalar("select pg_backend_pid()").fetch_one(&target).await?;
            let mut blocker = pool.begin().await?;
            let blocker_pid: i32 =
                sqlx::query_scalar("select pg_backend_pid()").fetch_one(&mut *blocker).await?;
            sqlx::query("select id from catalog_library where id = $1 for update")
                .bind(authority.library_id)
                .execute(&mut *blocker)
                .await?;
            let calls = AtomicUsize::new(0);
            let pids = Mutex::new(Vec::new());
            let first_error = Mutex::new(None);
            let write = retry_graph_persistence(|| {
                calls.fetch_add(1, Ordering::SeqCst);
                async {
                    let pid: i32 =
                        sqlx::query_scalar("select pg_backend_pid()").fetch_one(&target).await?;
                    pids.lock().expect("pid mutex").push(pid);
                    let result =
                        runtime_graph_repository::create_runtime_graph_extraction_record_fenced(
                            &target, authority, &input,
                        )
                        .await;
                    if let Err(error) = &result {
                        assert!(is_retryable_graph_persistence_error(error));
                        let class = error
                            .as_database_error()
                            .and_then(|error| error.code())
                            .map(|code| code.into_owned())
                            .unwrap_or_else(|| match error {
                                sqlx::Error::Io(error) => format!("IO:{:?}", error.kind()),
                                _ => "unexpected".into(),
                            });
                        *first_error.lock().expect("error mutex") = Some(class);
                    }
                    result
                }
            });
            let kill = async {
                // The guard sets its 60s local idle timeout before this parent-row lock.
                // Observe only the pre-acquired session in this fresh database, blocked
                // by our exact library-row-lock connection; no backend discovery/kill scan.
                tokio::time::timeout(Duration::from_secs(10), async {
                    loop {
                        let waiting: bool = sqlx::query_scalar(
                            "select exists(select 1 from pg_stat_activity where pid = $1
                             and datname = current_database() and application_name = $2 and wait_event_type = 'Lock'
                             and $3 = any(pg_blocking_pids(pid)))",
                        ).bind(first_pid).bind(&application_name).bind(blocker_pid).fetch_one(&pool).await?;
                        if waiting { break Ok::<_, sqlx::Error>(()); }
                        tokio::task::yield_now().await;
                    }
                }).await??;
                let before: i64 =
                    sqlx::query_scalar("select count(*) from runtime_graph_extraction")
                        .fetch_one(&pool)
                        .await?;
                assert_eq!(before, 0, "no partial owner write before termination");
                let terminated: bool = sqlx::query_scalar(
                    "select pg_terminate_backend(pid) from pg_stat_activity where pid = $1
                     and datname = current_database() and application_name = $2 and wait_event_type = 'Lock'
                     and $3 = any(pg_blocking_pids(pid))",
                ).bind(first_pid).bind(&application_name).bind(blocker_pid).fetch_one(&pool).await?;
                assert!(terminated);
                blocker.rollback().await?;
                Ok::<_, anyhow::Error>(())
            };
            let (result, killed) =
                tokio::time::timeout(Duration::from_secs(20), async { tokio::join!(write, kill) })
                    .await?;
            killed?;
            let IngestAttemptMutationOutcome::Applied(row) = result? else {
                anyhow::bail!("live authority was lost")
            };
            assert_eq!(row.id, input.id);
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            let observed = pids.lock().expect("pid mutex").clone();
            assert_eq!(observed.len(), 2);
            assert_eq!(observed[0], first_pid);
            assert_ne!(observed[1], first_pid);
            let error = first_error
                .lock()
                .expect("error mutex")
                .clone()
                .expect("first call failed natively");
            let count: i64 = sqlx::query_scalar("select count(*) from runtime_graph_extraction")
                .fetch_one(&pool)
                .await?;
            assert_eq!(count, 1, "retry committed exactly one owner record");
            tracing::info!(%error, pids = ?observed, calls = 2, stored = 1, partial = 0, "native guard kill");
            sqlx::query("update ingest_job set queue_state = 'completed' where id = $1")
                .bind(job_id)
                .execute(&pool)
                .await?;
            assert!(matches!(
                runtime_graph_repository::create_runtime_graph_extraction_record_fenced(
                    &target, authority, &input
                )
                .await?,
                IngestAttemptMutationOutcome::AuthorityLost
            ));
            target.close().await;
            Ok(())
        }
    }

    use std::sync::Arc;

    use crate::shared::database::test_database_error;

    use super::{
        is_retryable_graph_persistence_error, resolve_unique_conflict, retry_graph_persistence,
    };

    fn database_error(code: &'static str) -> sqlx::Error {
        test_database_error(code, None)
    }

    fn sqlstate(error: &sqlx::Error) -> Option<String> {
        error.as_database_error().and_then(|error| error.code()).map(|code| code.into_owned())
    }

    #[test]
    fn classifier_retries_transient_connection_and_contention_states() {
        for code in ["25P03", "40P01", "40001", "55P03", "57P01", "57P03", "53300", "08006"] {
            assert!(
                is_retryable_graph_persistence_error(&database_error(code)),
                "SQLSTATE {code} must be retryable"
            );
        }
        assert!(is_retryable_graph_persistence_error(&sqlx::Error::PoolTimedOut));
        assert!(is_retryable_graph_persistence_error(&sqlx::Error::PoolClosed));
        assert!(is_retryable_graph_persistence_error(&sqlx::Error::Io(std::io::Error::other(
            "reset"
        ))));
    }

    #[test]
    fn classifier_does_not_retry_permanent_failures() {
        assert!(!is_retryable_graph_persistence_error(&database_error("23505")));
        assert!(!is_retryable_graph_persistence_error(&database_error("42501")));
        assert!(!is_retryable_graph_persistence_error(&sqlx::Error::RowNotFound));
    }

    #[test]
    fn resolve_unique_conflict_adopts_or_surfaces_the_right_error() {
        let adopted = resolve_unique_conflict::<u8>(Ok(Some(9)), database_error("23505"))
            .expect("a stored row must be adopted");
        assert_eq!(adopted, 9);

        let missing = resolve_unique_conflict::<u8>(Ok(None), database_error("23505"))
            .expect_err("a missing row must keep the original conflict");
        assert_eq!(sqlstate(&missing).as_deref(), Some("23505"));

        let read_failure =
            resolve_unique_conflict::<u8>(Err(database_error("25P03")), database_error("23505"))
                .expect_err("a read failure must be surfaced, not masked");
        assert_eq!(sqlstate(&read_failure).as_deref(), Some("25P03"));
    }

    #[tokio::test]
    async fn retries_transient_failure_until_success() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let result = retry_graph_persistence(|| {
            let counter = Arc::clone(&counter);
            async move {
                let attempt = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if attempt < 1 { Err(database_error("25P03")) } else { Ok(7_u8) }
            }
        })
        .await;

        assert_eq!(result.expect("transient failure must self-heal"), 7);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn non_retryable_error_returns_after_one_call() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let result = retry_graph_persistence(|| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err::<u8, _>(database_error("23505"))
            }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn exhausted_retries_return_last_error_after_two_calls() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let mut codes = ["40001", "40P01"].into_iter();
        let result = retry_graph_persistence(|| {
            let counter = Arc::clone(&counter);
            let code = codes.next().expect("exactly two attempts");
            async move {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err::<u8, _>(database_error(code))
            }
        })
        .await;

        let error = result.expect_err("persistent transient errors must exhaust the retries");
        assert_eq!(sqlstate(&error).as_deref(), Some("40P01"));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}
