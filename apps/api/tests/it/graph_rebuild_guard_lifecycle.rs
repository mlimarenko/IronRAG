//! PostgreSQL integration coverage for whole-library graph rebuild fencing.
//!
//! Run with:
//! `cargo test -p ironrag-backend --test it -- --ignored graph_rebuild_guard_lifecycle::`

use crate::{postgres_activity, temp_postgres_database};

use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use ironrag_backend::{
    app::config::Settings,
    infra::repositories::{catalog_repository, runtime_graph_repository},
};
use postgres_activity::wait_for_backend_lock;
use sqlx::{PgConnection, PgPool, Postgres, Transaction, postgres::PgPoolOptions};
use temp_postgres_database::TempPostgresDatabase;
use tokio::sync::oneshot;
use uuid::Uuid;

const DATABASE_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const DATABASE_RETRY_DELAY: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RebuildLockAvailability {
    admission: bool,
    merge: bool,
    raw: bool,
}

impl RebuildLockAvailability {
    const ALL_AVAILABLE: Self = Self { admission: true, merge: true, raw: true };
    const NONE_AVAILABLE: Self = Self { admission: false, merge: false, raw: false };
}

struct RebuildGuardFixture {
    database: TempPostgresDatabase,
    observer_pool: PgPool,
    library_id: Uuid,
    commit_parent_id: Uuid,
}

impl RebuildGuardFixture {
    async fn create() -> Result<Self> {
        let settings = Settings::from_env().context("load graph rebuild guard settings")?;
        let database =
            TempPostgresDatabase::create(&settings.database_url, "graph_rebuild_guard").await?;
        let observer_pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&database.database_url)
            .await
            .context("connect graph rebuild guard observer pool")?;
        sqlx::migrate!("./migrations")
            .run(&observer_pool)
            .await
            .context("apply graph rebuild guard migrations")?;

        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            &observer_pool,
            &format!("graph-rebuild-guard-workspace-{suffix}"),
            "Graph Rebuild Guard Workspace",
            None,
        )
        .await?;
        let library = catalog_repository::create_library(
            &observer_pool,
            workspace.id,
            &format!("graph-rebuild-guard-library-{suffix}"),
            "Graph Rebuild Guard Library",
            None,
            None,
        )
        .await?;
        sqlx::query(
            "create table graph_rebuild_guard_probe (
                id uuid primary key,
                library_id uuid not null,
                outcome text not null
            )",
        )
        .execute(&observer_pool)
        .await?;
        sqlx::query(
            "create table graph_rebuild_guard_commit_parent (
                id uuid primary key
            )",
        )
        .execute(&observer_pool)
        .await?;
        sqlx::query(
            "create table graph_rebuild_guard_commit_child (
                id uuid primary key,
                parent_id uuid not null references graph_rebuild_guard_commit_parent (id)
                    deferrable initially deferred
            )",
        )
        .execute(&observer_pool)
        .await?;
        let commit_parent_id = Uuid::now_v7();
        sqlx::query("insert into graph_rebuild_guard_commit_parent (id) values ($1)")
            .bind(commit_parent_id)
            .execute(&observer_pool)
            .await?;

        Ok(Self { database, observer_pool, library_id: library.id, commit_parent_id })
    }

    async fn pool(&self) -> Result<PgPool> {
        PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.database.database_url)
            .await
            .context("connect graph rebuild guard test pool")
    }

    async fn cleanup(self) -> Result<()> {
        self.observer_pool.close().await;
        self.database.drop().await
    }
}

async fn acquire_rebuild_guard(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<runtime_graph_repository::RuntimeGraphRebuildGuard> {
    match runtime_graph_repository::acquire_runtime_library_graph_rebuild_guard(pool, library_id)
        .await?
    {
        runtime_graph_repository::RuntimeGraphRebuildGuardAcquireOutcome::Acquired(guard) => {
            Ok(guard)
        }
        runtime_graph_repository::RuntimeGraphRebuildGuardAcquireOutcome::LibraryNotFound => {
            bail!("graph rebuild guard unexpectedly missed the fixture library")
        }
        runtime_graph_repository::RuntimeGraphRebuildGuardAcquireOutcome::Busy => {
            bail!("graph rebuild guard unexpectedly encountered global admission contention")
        }
    }
}

async fn acquire_writer_lock_sequence(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
) -> Result<()> {
    sqlx::query(
        "select pg_advisory_xact_lock(
            hashtextextended('graph-merge:' || $1::text, 0)
         )",
    )
    .bind(library_id.to_string())
    .execute(&mut **transaction)
    .await?;
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind(library_id.to_string())
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

async fn probe_rebuild_locks(pool: &PgPool, library_id: Uuid) -> Result<RebuildLockAvailability> {
    let mut transaction = pool.begin().await?;
    let admission = sqlx::query_scalar::<_, bool>(
        "select pg_try_advisory_xact_lock(
            hashtextextended('graph-rebuild-admission', 0)
         )",
    )
    .fetch_one(&mut *transaction)
    .await?;
    let merge = sqlx::query_scalar::<_, bool>(
        "select pg_try_advisory_xact_lock(
            hashtextextended('graph-merge:' || $1::text, 0)
         )",
    )
    .bind(library_id.to_string())
    .fetch_one(&mut *transaction)
    .await?;
    let raw = sqlx::query_scalar::<_, bool>(
        "select pg_try_advisory_xact_lock(hashtextextended($1::text, 0))",
    )
    .bind(library_id.to_string())
    .fetch_one(&mut *transaction)
    .await?;
    transaction.rollback().await?;
    Ok(RebuildLockAvailability { admission, merge, raw })
}

async fn wait_for_rebuild_locks(
    pool: &PgPool,
    library_id: Uuid,
    expected: RebuildLockAvailability,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + DATABASE_WAIT_TIMEOUT;
    loop {
        let observed = probe_rebuild_locks(pool, library_id).await?;
        if observed == expected {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("rebuild locks did not reach {expected:?}; last observed {observed:?}");
        }
        tokio::time::sleep(DATABASE_RETRY_DELAY).await;
    }
}

async fn backend_pid(pool: &PgPool) -> Result<i32> {
    sqlx::query_scalar::<_, i32>("select pg_backend_pid()")
        .fetch_one(pool)
        .await
        .map_err(Into::into)
}

async fn insert_probe(
    connection: &mut PgConnection,
    id: Uuid,
    library_id: Uuid,
    outcome: &str,
) -> Result<()> {
    sqlx::query(
        "insert into graph_rebuild_guard_probe (id, library_id, outcome)
         values ($1, $2, $3)",
    )
    .bind(id)
    .bind(library_id)
    .bind(outcome)
    .execute(connection)
    .await?;
    Ok(())
}

async fn probe_outcome(pool: &PgPool, id: Uuid) -> Result<Option<String>> {
    sqlx::query_scalar::<_, String>("select outcome from graph_rebuild_guard_probe where id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(Into::into)
}

async fn probe_outcome_with_connection(
    connection: &mut PgConnection,
    id: Uuid,
) -> Result<Option<String>> {
    sqlx::query_scalar::<_, String>("select outcome from graph_rebuild_guard_probe where id = $1")
        .bind(id)
        .fetch_optional(connection)
        .await
        .map_err(Into::into)
}

async fn insert_deferred_commit_probe(
    connection: &mut PgConnection,
    id: Uuid,
    parent_id: Uuid,
) -> Result<()> {
    sqlx::query(
        "insert into graph_rebuild_guard_commit_child (id, parent_id)
         values ($1, $2)",
    )
    .bind(id)
    .bind(parent_id)
    .execute(connection)
    .await?;
    Ok(())
}

async fn lock_commit_parent(
    pool: &PgPool,
    parent_id: Uuid,
) -> Result<Transaction<'static, Postgres>> {
    let mut transaction = pool.begin().await?;
    sqlx::query("select id from graph_rebuild_guard_commit_parent where id = $1 for update")
        .bind(parent_id)
        .execute(&mut *transaction)
        .await?;
    Ok(transaction)
}

async fn commit_probe_exists(pool: &PgPool, id: Uuid) -> Result<bool> {
    sqlx::query_scalar::<_, bool>(
        "select exists (
            select 1 from graph_rebuild_guard_commit_child where id = $1
         )",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(Into::into)
}

async fn verify_writer_precedes_snapshot_and_commit_releases_locks(
    fixture: &RebuildGuardFixture,
) -> Result<()> {
    let writer_pool = fixture.pool().await?;
    let guard_pool = fixture.pool().await?;
    let probe_pool = fixture.pool().await?;
    let guard_pid = backend_pid(&guard_pool).await?;
    let writer_probe_id = Uuid::now_v7();
    let late_probe_id = Uuid::now_v7();
    let rebuild_probe_id = Uuid::now_v7();
    let commit_probe_id = Uuid::now_v7();

    let mut guard = acquire_guard_after_writer_commits(
        fixture,
        &writer_pool,
        &guard_pool,
        &probe_pool,
        guard_pid,
        writer_probe_id,
    )
    .await?;
    assert_rebuild_snapshot_is_stable(fixture, &mut guard, writer_probe_id, late_probe_id).await?;
    insert_probe(guard.connection(), rebuild_probe_id, fixture.library_id, "rebuild-committed")
        .await?;
    insert_deferred_commit_probe(guard.connection(), commit_probe_id, fixture.commit_parent_id)
        .await?;
    commit_rebuild_with_constraint_barrier(fixture, &writer_pool, &probe_pool, guard_pid, guard)
        .await?;
    assert_rebuild_commit_persisted(&fixture.observer_pool, rebuild_probe_id, commit_probe_id)
        .await?;
    wait_for_rebuild_locks(&probe_pool, fixture.library_id, RebuildLockAvailability::ALL_AVAILABLE)
        .await?;

    writer_pool.close().await;
    guard_pool.close().await;
    probe_pool.close().await;
    Ok(())
}

async fn acquire_guard_after_writer_commits(
    fixture: &RebuildGuardFixture,
    writer_pool: &PgPool,
    guard_pool: &PgPool,
    probe_pool: &PgPool,
    guard_pid: i32,
    writer_probe_id: Uuid,
) -> Result<runtime_graph_repository::RuntimeGraphRebuildGuard> {
    let mut writer = writer_pool.begin().await?;
    acquire_writer_lock_sequence(&mut writer, fixture.library_id).await?;
    insert_probe(
        &mut writer,
        writer_probe_id,
        fixture.library_id,
        "writer-committed-before-snapshot",
    )
    .await?;

    let acquire_task = tokio::spawn({
        let guard_pool = guard_pool.clone();
        let library_id = fixture.library_id;
        async move { acquire_rebuild_guard(&guard_pool, library_id).await }
    });
    wait_for_backend_lock(&fixture.observer_pool, guard_pid, "graph-merge:").await?;
    assert_eq!(
        probe_rebuild_locks(probe_pool, fixture.library_id).await?,
        RebuildLockAvailability { admission: false, merge: false, raw: false },
        "the waiting rebuild must own admission while the earlier writer owns merge and raw"
    );

    writer.commit().await?;
    let guard = tokio::time::timeout(DATABASE_WAIT_TIMEOUT, acquire_task)
        .await
        .context("rebuild guard did not acquire after the earlier writer committed")???;
    Ok(guard)
}

async fn assert_rebuild_snapshot_is_stable(
    fixture: &RebuildGuardFixture,
    guard: &mut runtime_graph_repository::RuntimeGraphRebuildGuard,
    writer_probe_id: Uuid,
    late_probe_id: Uuid,
) -> Result<()> {
    let writer_outcome = probe_outcome_with_connection(guard.connection(), writer_probe_id).await?;
    assert_eq!(
        writer_outcome.as_deref(),
        Some("writer-committed-before-snapshot"),
        "the first repeatable-read snapshot must include the earlier lock holder's commit"
    );
    sqlx::query(
        "insert into graph_rebuild_guard_probe (id, library_id, outcome)
         values ($1, $2, 'committed-after-snapshot')",
    )
    .bind(late_probe_id)
    .bind(fixture.library_id)
    .execute(&fixture.observer_pool)
    .await?;
    assert_eq!(
        probe_outcome(&fixture.observer_pool, late_probe_id).await?.as_deref(),
        Some("committed-after-snapshot"),
        "the observer must see the row committed after the rebuild snapshot"
    );
    assert_eq!(
        probe_outcome_with_connection(guard.connection(), late_probe_id).await?,
        None,
        "the established repeatable-read rebuild snapshot must remain stable"
    );
    Ok(())
}

async fn commit_rebuild_with_constraint_barrier(
    fixture: &RebuildGuardFixture,
    writer_pool: &PgPool,
    probe_pool: &PgPool,
    guard_pid: i32,
    guard: runtime_graph_repository::RuntimeGraphRebuildGuard,
) -> Result<()> {
    let commit_blocker = lock_commit_parent(writer_pool, fixture.commit_parent_id).await?;
    let commit_task = tokio::spawn(async move { guard.commit().await });
    wait_for_backend_lock(&fixture.observer_pool, guard_pid, "COMMIT").await?;
    assert_eq!(
        probe_rebuild_locks(probe_pool, fixture.library_id).await?,
        RebuildLockAvailability::NONE_AVAILABLE,
        "admission, merge, and raw locks must remain held until rebuild commit completes"
    );
    commit_blocker.commit().await?;
    tokio::time::timeout(DATABASE_WAIT_TIMEOUT, commit_task)
        .await
        .context("rebuild guard commit did not finish after its constraint barrier released")???;
    Ok(())
}

async fn assert_rebuild_commit_persisted(
    observer_pool: &PgPool,
    rebuild_probe_id: Uuid,
    commit_probe_id: Uuid,
) -> Result<()> {
    assert_eq!(
        probe_outcome(observer_pool, rebuild_probe_id).await?.as_deref(),
        Some("rebuild-committed"),
        "guard commit must persist rebuild data"
    );
    assert!(
        commit_probe_exists(observer_pool, commit_probe_id).await?,
        "the deferred commit probe must persist after the barrier releases"
    );
    Ok(())
}

async fn verify_cancelled_commit_keeps_locks_until_inner_outcome(
    fixture: &RebuildGuardFixture,
) -> Result<()> {
    let blocker_pool = fixture.pool().await?;
    let guard_pool = fixture.pool().await?;
    let probe_pool = fixture.pool().await?;
    let guard_pid = backend_pid(&guard_pool).await?;
    let commit_probe_id = Uuid::now_v7();

    let mut guard = acquire_rebuild_guard(&guard_pool, fixture.library_id).await?;
    insert_deferred_commit_probe(guard.connection(), commit_probe_id, fixture.commit_parent_id)
        .await?;
    let commit_blocker = lock_commit_parent(&blocker_pool, fixture.commit_parent_id).await?;
    let commit_task = tokio::spawn(async move { guard.commit().await });
    assert_commit_barrier_holds_locks(fixture, &probe_pool, guard_pid).await?;

    abort_commit_and_check_pending_locks(fixture, &probe_pool, guard_pid, commit_task).await?;
    commit_blocker.commit().await?;
    assert_cancelled_commit_pool_reusable(fixture, &guard_pool, &probe_pool, commit_probe_id)
        .await?;

    blocker_pool.close().await;
    guard_pool.close().await;
    probe_pool.close().await;
    Ok(())
}

async fn assert_commit_barrier_holds_locks(
    fixture: &RebuildGuardFixture,
    probe_pool: &PgPool,
    guard_pid: i32,
) -> Result<()> {
    wait_for_backend_lock(&fixture.observer_pool, guard_pid, "COMMIT").await?;
    assert_eq!(
        probe_rebuild_locks(probe_pool, fixture.library_id).await?,
        RebuildLockAvailability::NONE_AVAILABLE,
        "the session locks must be held while the deferred constraint blocks commit"
    );
    Ok(())
}

async fn abort_commit_and_check_pending_locks(
    fixture: &RebuildGuardFixture,
    probe_pool: &PgPool,
    guard_pid: i32,
    commit_task: tokio::task::JoinHandle<Result<(), sqlx::Error>>,
) -> Result<()> {
    commit_task.abort();
    let cancellation = match commit_task.await {
        Err(cancellation) => cancellation,
        Ok(outcome) => bail!("blocked commit caller unexpectedly completed: {outcome:?}"),
    };
    ensure!(cancellation.is_cancelled(), "blocked commit caller must end by cancellation");
    let backend_still_finishing = sqlx::query_scalar::<_, bool>(
        "select exists (
            select 1
            from pg_stat_activity
            where pid = $1
              and wait_event_type = 'Lock'
              and query like '%COMMIT%'
         )",
    )
    .bind(guard_pid)
    .fetch_one(&fixture.observer_pool)
    .await?;
    let observed_locks = probe_rebuild_locks(probe_pool, fixture.library_id).await?;
    ensure!(
        !backend_still_finishing || observed_locks == RebuildLockAvailability::NONE_AVAILABLE,
        "rebuild locks became available while the same backend still had an unresolved COMMIT"
    );
    Ok(())
}

async fn assert_cancelled_commit_pool_reusable(
    fixture: &RebuildGuardFixture,
    guard_pool: &PgPool,
    probe_pool: &PgPool,
    commit_probe_id: Uuid,
) -> Result<()> {
    wait_for_rebuild_locks(probe_pool, fixture.library_id, RebuildLockAvailability::ALL_AVAILABLE)
        .await?;
    let _commit_outcome_was_resolved =
        commit_probe_exists(&fixture.observer_pool, commit_probe_id).await?;

    let successor = acquire_rebuild_guard(guard_pool, fixture.library_id)
        .await
        .context("guard pools were not reusable after cancelled commit finalization")?;
    successor.rollback().await?;
    wait_for_rebuild_locks(probe_pool, fixture.library_id, RebuildLockAvailability::ALL_AVAILABLE)
        .await?;
    Ok(())
}

async fn verify_explicit_rollback_releases_locks(fixture: &RebuildGuardFixture) -> Result<()> {
    let guard_pool = fixture.pool().await?;
    let probe_pool = fixture.pool().await?;
    let rollback_probe_id = Uuid::now_v7();
    let mut guard = acquire_rebuild_guard(&guard_pool, fixture.library_id).await?;
    insert_probe(guard.connection(), rollback_probe_id, fixture.library_id, "rebuild-rolled-back")
        .await?;
    assert_eq!(
        probe_rebuild_locks(&probe_pool, fixture.library_id).await?,
        RebuildLockAvailability::NONE_AVAILABLE,
        "an acquired guard must own admission, merge, and raw locks"
    );

    guard.rollback().await?;
    assert_eq!(
        probe_outcome(&fixture.observer_pool, rollback_probe_id).await?,
        None,
        "explicit guard rollback must discard rebuild data"
    );
    wait_for_rebuild_locks(&probe_pool, fixture.library_id, RebuildLockAvailability::ALL_AVAILABLE)
        .await?;

    guard_pool.close().await;
    probe_pool.close().await;
    Ok(())
}

async fn verify_cancelled_guard_drop_releases_locks(fixture: &RebuildGuardFixture) -> Result<()> {
    let guard_pool = fixture.pool().await?;
    let probe_pool = fixture.pool().await?;
    let cancellation_probe_id = Uuid::now_v7();
    let (ready_tx, ready_rx) = oneshot::channel();
    let guard_task = tokio::spawn({
        let guard_pool = guard_pool.clone();
        let library_id = fixture.library_id;
        async move {
            let mut guard = acquire_rebuild_guard(&guard_pool, library_id).await?;
            insert_probe(
                guard.connection(),
                cancellation_probe_id,
                library_id,
                "cancelled-rebuild",
            )
            .await?;
            ensure!(ready_tx.send(()).is_ok(), "cancellation test receiver disappeared");
            std::future::pending::<()>().await;
            guard.rollback().await?;
            Ok::<_, anyhow::Error>(())
        }
    });
    tokio::time::timeout(DATABASE_WAIT_TIMEOUT, ready_rx)
        .await
        .context("cancelled guard task did not acquire its locks")??;
    assert_eq!(
        probe_rebuild_locks(&probe_pool, fixture.library_id).await?,
        RebuildLockAvailability::NONE_AVAILABLE,
        "the cancellable guard task must own all rebuild locks before abort"
    );

    guard_task.abort();
    let cancellation = match guard_task.await {
        Err(cancellation) => cancellation,
        Ok(outcome) => bail!("cancelled guard task unexpectedly completed: {outcome:?}"),
    };
    ensure!(cancellation.is_cancelled(), "guard task must end by cancellation");
    wait_for_rebuild_locks(&probe_pool, fixture.library_id, RebuildLockAvailability::ALL_AVAILABLE)
        .await?;
    assert_eq!(
        probe_outcome(&fixture.observer_pool, cancellation_probe_id).await?,
        None,
        "dropping a cancelled guard must roll back uncommitted rebuild data"
    );

    sqlx::query("select 1").execute(&guard_pool).await?;
    guard_pool.close().await;
    probe_pool.close().await;
    Ok(())
}

async fn verify_busy_outcome_leaves_no_locks(fixture: &RebuildGuardFixture) -> Result<()> {
    let owner_pool = fixture.pool().await?;
    let contender_pool = fixture.pool().await?;
    let probe_pool = fixture.pool().await?;
    let owner = acquire_rebuild_guard(&owner_pool, fixture.library_id).await?;

    assert_contender_busy_preserves_owner(fixture, &contender_pool, &probe_pool).await?;
    owner.rollback().await?;
    wait_for_rebuild_locks(&probe_pool, fixture.library_id, RebuildLockAvailability::ALL_AVAILABLE)
        .await?;
    assert_post_busy_pool_reusable(fixture, &contender_pool, &probe_pool).await?;

    owner_pool.close().await;
    contender_pool.close().await;
    probe_pool.close().await;
    Ok(())
}

async fn assert_contender_busy_preserves_owner(
    fixture: &RebuildGuardFixture,
    contender_pool: &PgPool,
    probe_pool: &PgPool,
) -> Result<()> {
    let contender_outcome = tokio::time::timeout(
        DATABASE_WAIT_TIMEOUT,
        runtime_graph_repository::acquire_runtime_library_graph_rebuild_guard(
            contender_pool,
            fixture.library_id,
        ),
    )
    .await
    .context("global admission contender waited instead of returning Busy")??;
    ensure!(
        matches!(
            contender_outcome,
            runtime_graph_repository::RuntimeGraphRebuildGuardAcquireOutcome::Busy
        ),
        "global admission contender must return Busy"
    );
    sqlx::query("select 1").execute(contender_pool).await?;
    assert_eq!(
        probe_rebuild_locks(probe_pool, fixture.library_id).await?,
        RebuildLockAvailability::NONE_AVAILABLE,
        "Busy must not disturb the active owner's admission, merge, or raw locks"
    );
    Ok(())
}

async fn assert_post_busy_pool_reusable(
    fixture: &RebuildGuardFixture,
    contender_pool: &PgPool,
    probe_pool: &PgPool,
) -> Result<()> {
    let successor = acquire_rebuild_guard(contender_pool, fixture.library_id)
        .await
        .context("the pool that returned Busy could not acquire after owner release")?;
    assert_eq!(
        probe_rebuild_locks(probe_pool, fixture.library_id).await?,
        RebuildLockAvailability::NONE_AVAILABLE,
        "the post-Busy successor must acquire admission, merge, and raw locks"
    );
    successor.rollback().await?;
    wait_for_rebuild_locks(probe_pool, fixture.library_id, RebuildLockAvailability::ALL_AVAILABLE)
        .await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres with the pgvector extensions"]
async fn rebuild_guard_orders_snapshot_and_releases_locks_on_requested_exit_paths() -> Result<()> {
    let fixture = RebuildGuardFixture::create().await?;
    let result = async {
        verify_writer_precedes_snapshot_and_commit_releases_locks(&fixture).await?;
        verify_cancelled_commit_keeps_locks_until_inner_outcome(&fixture).await?;
        verify_explicit_rollback_releases_locks(&fixture).await?;
        verify_cancelled_guard_drop_releases_locks(&fixture).await?;
        verify_busy_outcome_leaves_no_locks(&fixture).await?;
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}
