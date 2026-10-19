//! Owned-Postgres regression for the bounded global vector-lifecycle inventory
//! probe.
//!
//! These tests exercise the real `read_vector_inventory` and
//! `sample_global_inventory` read path against a private migrated database, so
//! the SQL primitives that a mock cannot see are actually executed: bind
//! parameter handling, the nullable library scope, the current-vector predicate
//! bindings, the `regclass` relation-size read, NULL/latest last-success
//! decoding and the real global backlog age.
//!
//! Run against the task-owned instance:
//!
//! ```text
//! IRONRAG_DATABASE_URL=postgresql://postgres@127.0.0.1:55468/postgres \
//!   CARGO_TARGET_DIR=/data/targets/irg68-continuation cargo test -p ironrag-backend \
//!   --all-features --lib -- --ignored vector_inventory::tests --test-threads=1
//! ```
//!
//! They are `#[ignore]`d like every other Postgres-backed library test and run
//! in the integration lane, which carries the pgvector service.

use anyhow::Result;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use super::*;
use crate::infra::repositories::{content_mutation_repository, content_revision_repository};
use crate::services::maintenance::{
    lease::MaintenanceClass, vector_lifecycle_inventory::sample_global_inventory,
    vector_lifecycle_metrics::InventorySnapshot,
};

const DIMENSIONS: i64 = 2;
const RELATION: &str = "knowledge_chunk_vector_d2";

mod database_url {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/support/database_url.rs"));
}
mod temp_postgres_database {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/support/temp_postgres_database.rs"));
}
use temp_postgres_database::TempPostgresDatabase;

/// `sample_global_inventory` returns a bounded [`GcErrorCode`] rather than an
/// `Error`; the test maps it onto `anyhow` so `?` works.
async fn sample(pool: &PgPool) -> Result<InventorySnapshot> {
    sample_global_inventory(pool)
        .await
        .map_err(|code| anyhow::anyhow!("inventory probe failed: {}", code.label()))
}

async fn connect() -> Result<(TempPostgresDatabase, PgPool)> {
    let url = std::env::var("IRONRAG_DATABASE_URL")
        .context("IRONRAG_DATABASE_URL must point at the owned test postgres")?;
    let database = TempPostgresDatabase::create(&url, "vector_inventory").await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&database.database_url)
        .await
        .context("connect owned vector-inventory test postgres")?;
    sqlx::migrate!("./migrations").run(&pool).await.context("apply migrations")?;
    sqlx::query("create extension if not exists vector").execute(&pool).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "create table {RELATION} (
             key text primary key,
             vector_id uuid not null,
             workspace_id uuid not null,
             library_id uuid not null,
             chunk_id uuid not null,
             revision_id uuid not null,
             embedding_model_key text not null,
             vector_kind text not null,
             dimensions integer not null,
             embedding vector({DIMENSIONS}) not null,
             freshness_generation bigint not null,
             created_at timestamp with time zone not null
         )"
    )))
    .execute(&pool)
    .await
    .context("create vector relation")?;
    Ok((database, pool))
}

async fn seed_library(pool: &PgPool) -> Result<(Uuid, Uuid)> {
    let workspace_id = Uuid::now_v7();
    let library_id = Uuid::now_v7();
    let suffix = Uuid::now_v7().simple().to_string();
    sqlx::query("insert into catalog_workspace (id, slug, display_name) values ($1, $2, $3)")
        .bind(workspace_id)
        .bind(format!("vec-inv-ws-{suffix}"))
        .bind("Vector inventory probe workspace")
        .execute(pool)
        .await?;
    sqlx::query(
        "insert into catalog_library (id, workspace_id, slug, display_name) values ($1, $2, $3, $4)",
    )
    .bind(library_id)
    .bind(workspace_id)
    .bind(format!("vec-inv-lib-{suffix}"))
    .bind("Vector inventory probe library")
    .execute(pool)
    .await?;
    Ok((workspace_id, library_id))
}

async fn seed_chunk(
    pool: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
) -> Result<Uuid> {
    let chunk_id = Uuid::now_v7();
    sqlx::query(
        "insert into knowledge_chunk (
             chunk_id, workspace_id, library_id, document_id, revision_id,
             chunk_index, content_text, normalized_text, chunk_state
         ) values ($1, $2, $3, $4, $5, 0, 'body', 'body', 'ready')",
    )
    .bind(chunk_id)
    .bind(workspace_id)
    .bind(library_id)
    .bind(document_id)
    .bind(revision_id)
    .execute(pool)
    .await?;
    Ok(chunk_id)
}

async fn seed_vector(
    pool: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    chunk_id: Uuid,
    revision_id: Uuid,
    created_at: &str,
) -> Result<()> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "insert into {RELATION} (
             key, vector_id, workspace_id, library_id, chunk_id, revision_id,
             embedding_model_key, vector_kind, dimensions, embedding,
             freshness_generation, created_at
         ) values ($1, $2, $3, $4, $5, $6, 'profile', 'chunk_embedding', $7,
                   '[0.1,0.2]', 1, {created_at})"
    )))
    .bind(format!("key-{}", Uuid::now_v7()))
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind(library_id)
    .bind(chunk_id)
    .bind(revision_id)
    .bind(i32::try_from(DIMENSIONS)?)
    .execute(pool)
    .await
    .map(|_| ())
    .context("seed vector row")
}

/// Seeds one document with a readable/active current revision and a superseded
/// historical revision behind it. Returns `(workspace_id, library_id)`.
async fn seed_current_and_history(pool: &PgPool, historical_rows: usize) -> Result<(Uuid, Uuid)> {
    let (workspace_id, library_id) = seed_library(pool).await?;
    let document_id = Uuid::now_v7();
    let current_revision_id = Uuid::now_v7();
    let historical_revision_id = Uuid::now_v7();
    let suffix = library_id.simple().to_string();

    sqlx::query(
        "insert into knowledge_document (
             document_id, workspace_id, library_id, external_key, document_state,
             active_revision_id, readable_revision_id, created_at, updated_at
         ) values ($1, $2, $3, $4, 'active', $5, $5, now(), now())",
    )
    .bind(document_id)
    .bind(workspace_id)
    .bind(library_id)
    .bind(format!("external-{suffix}"))
    .bind(current_revision_id)
    .execute(pool)
    .await?;

    sqlx::query(
        "insert into content_document (id, workspace_id, library_id, external_key)
         values ($1, $2, $3, $4)",
    )
    .bind(document_id)
    .bind(workspace_id)
    .bind(library_id)
    .bind(format!("source-{suffix}"))
    .execute(pool)
    .await?;
    for (revision_id, revision_number) in
        [(current_revision_id, 2_i64), (historical_revision_id, 1)]
    {
        seed_revision(pool, (workspace_id, library_id, document_id), revision_id, revision_number)
            .await?;
    }
    let owner = seed_revision_mutation(pool, current_revision_id, "applied", "applied").await?;
    sqlx::query(
        "insert into content_document_head (document_id, active_revision_id, readable_revision_id,
          latest_mutation_id) values ($1, $2, $2, $3)",
    )
    .bind(document_id)
    .bind(current_revision_id)
    .bind(owner.mutation_id)
    .execute(pool)
    .await?;
    let published: Option<Uuid> = sqlx::query_scalar(
        "select revision_id from content_current_revision where document_id = $1",
    )
    .bind(document_id)
    .fetch_optional(pool)
    .await?;
    assert_eq!(published, Some(current_revision_id), "inventory fixture must be published");

    let current_chunk =
        seed_chunk(pool, workspace_id, library_id, document_id, current_revision_id).await?;
    seed_vector(pool, workspace_id, library_id, current_chunk, current_revision_id, "now()")
        .await?;

    let historical_chunk =
        seed_chunk(pool, workspace_id, library_id, document_id, historical_revision_id).await?;
    for _ in 0..historical_rows {
        seed_vector(
            pool,
            workspace_id,
            library_id,
            historical_chunk,
            historical_revision_id,
            "now() - interval '2 days'",
        )
        .await?;
    }
    Ok((workspace_id, library_id))
}

async fn seed_revision(
    pool: &PgPool,
    scope: (Uuid, Uuid, Uuid),
    revision: Uuid,
    number: i64,
) -> Result<()> {
    let (workspace, library, document) = scope;
    sqlx::query(
        "insert into knowledge_revision (
          revision_id, workspace_id, library_id, document_id, revision_number,
          revision_state, revision_kind, mime_type, checksum, byte_size,
           text_state, vector_state, graph_state, text_readable_at, vector_ready_at, created_at)
         values ($1,$2,$3,$4,$5,'ready','upload','text/plain','sha256:' || repeat('a',64),4,
                 'text_readable','ready','ready',now(),now(),now())",
    )
    .bind(revision)
    .bind(workspace)
    .bind(library)
    .bind(document)
    .bind(number)
    .execute(pool)
    .await?;
    sqlx::query(
        "insert into content_revision (id,workspace_id,library_id,document_id,revision_number,
         content_source_kind,mime_type,checksum,byte_size,admission_identity)
         values ($1,$2,$3,$4,$5,'upload','text/plain','sha256:' || repeat('a',64),4,
             jsonb_build_object('origin','mutation','sourceIdentity',null,
                 'materializationToken',null,'visibilityIntentId',null,
                 'replacement',jsonb_build_object('checksum','sha256:' || repeat('a',64),
                     'byteSize',4,'mimeType','text/plain','fileName','inventory.txt',
                     'title',null,'languageCode',null,'sourceUri',null,'documentHint',null)))",
    )
    .bind(revision)
    .bind(workspace)
    .bind(library)
    .bind(document)
    .bind(number)
    .execute(pool)
    .await?;
    sqlx::query(
        "insert into knowledge_structured_revision (
          revision_id,workspace_id,library_id,document_id,preparation_state,
          normalization_profile,source_format,block_count,chunk_count,typed_fact_count,
          outline_json,prepared_at,updated_at)
         values ($1,$2,$3,$4,'prepared','test','text',0,1,0,'{}',now(),now())",
    )
    .bind(revision)
    .bind(workspace)
    .bind(library)
    .bind(document)
    .execute(pool)
    .await?;
    Ok(())
}

/// Uses the production registry and fingerprint path for both readable and leased owners.
async fn seed_revision_mutation(
    pool: &PgPool,
    revision_id: Uuid,
    mutation_state: &str,
    item_state: &str,
) -> Result<content_mutation_repository::ContentMutationItemRow> {
    let revision = content_revision_repository::get_revision_by_id(pool, revision_id)
        .await?
        .context("inventory fixture revision missing")?;
    let replacement = ironrag_contracts::documents::ReplacementIdentity {
        file_name: "inventory.txt".to_owned(),
        checksum: revision.checksum,
        mime_type: revision.mime_type,
        byte_size: revision.byte_size,
        title: revision.title,
        language_code: revision.language_code,
        source_uri: revision.source_uri,
        document_hint: revision.document_hint,
    };
    let request_identity = serde_json::json!({
        "workspaceId": revision.workspace_id,
        "libraryId": revision.library_id,
        "operationKind": "replace",
        "target": {"kind": "existing", "documentId": revision.document_id},
        "sourceIdentity": null,
        "revision": {
            "contentSourceKind": revision.content_source_kind,
            "baseRevisionId": revision.parent_revision_id,
            "visibilityIntentId": null,
        },
        "replacement": replacement,
    });
    let mutation = content_mutation_repository::create_mutation(
        pool,
        &content_mutation_repository::NewContentMutation {
            request_identity: &request_identity,
            workspace_id: revision.workspace_id,
            library_id: revision.library_id,
            operation_kind: "replace",
            requested_by_principal_id: None,
            request_surface: "internal",
            idempotency_key: None,
            source_identity: None,
            mutation_state,
            failure_code: None,
            conflict_code: None,
        },
    )
    .await?;
    Ok(content_mutation_repository::create_mutation_item(
        pool,
        &content_mutation_repository::NewContentMutationItem {
            mutation_id: mutation.id,
            document_id: revision.document_id,
            base_revision_id: revision.parent_revision_id,
            result_revision_id: Some(revision_id),
            item_state,
            message: None,
        },
    )
    .await?)
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn probe_splits_current_from_history_and_reports_real_backlog_age() -> Result<()> {
    let (database, pool) = connect().await?;
    let result: Result<()> = async {
        let (workspace_id, library_id) = seed_current_and_history(&pool, 2).await?;
        let (_, other_library) = seed_current_and_history(&pool, 3).await?;

        let inventory = read_vector_inventory(&pool, Some(library_id))
            .await
            .context("read bounded inventory")?;

        let current = inventory
            .lifecycle_rows
            .iter()
            .find(|row| row.lifecycle_category == LIFECYCLE_TAG_CURRENT_READABLE)
            .context("current category row")?;
        let history = inventory
            .lifecycle_rows
            .iter()
            .find(|row| row.lifecycle_category == LIFECYCLE_TAG_SUPERSEDED_READABLE_HISTORY)
            .context("history category row")?;
        assert_eq!(current.vector_rows, 1, "the readable/active revision stays");
        assert_eq!(history.vector_rows, 2, "the two historical rows are reclaimable");
        assert_eq!(inventory.backlog_vectors, 2);
        let age = inventory.backlog_oldest_age_seconds.context("backlog must carry an age")?;
        assert!(age > 86_400.0, "seeded two-day-old backlog must report a real age, got {age}");
        assert!(
            !inventory.physical_shards.is_empty(),
            "the regclass size read must return the seeded relation"
        );
        assert!(inventory.physical_shards.iter().all(|shard| shard.physical_shard_bytes > 0));
        seed_vector(&pool, workspace_id, library_id, Uuid::now_v7(), Uuid::now_v7(), "now()")
            .await?;
        let inventory = read_vector_inventory(&pool, Some(library_id)).await?;
        assert_eq!(
            inventory
                .lifecycle_rows
                .iter()
                .find(|row| row.lifecycle_category == LIFECYCLE_TAG_INCONSISTENT_OR_ORPHAN)
                .context("orphan category")?
                .vector_rows,
            1
        );
        assert_eq!(
            inventory
                .lifecycle_rows
                .iter()
                .find(|row| row.lifecycle_category == LIFECYCLE_TAG_CURRENT_READABLE)
                .context("current category")?
                .vector_rows,
            1
        );
        assert_eq!(
            inventory.backlog_vectors, 2,
            "an orphan is not proven reclaimable by the sweep"
        );
        assert_eq!(
            read_vector_inventory(&pool, Some(other_library)).await?.backlog_vectors,
            3,
            "library inventory must not leak another library's counts"
        );
        protect_historical_revision(&pool, library_id).await?;
        let inventory = read_vector_inventory(&pool, Some(library_id)).await?;
        assert_eq!(
            inventory
                .lifecycle_rows
                .iter()
                .find(|row| row.lifecycle_category == LIFECYCLE_TAG_PROTECTED_ACTIVE_INGEST)
                .context("protected category")?
                .vector_rows,
            2
        );
        assert_eq!(
            inventory
                .lifecycle_rows
                .iter()
                .find(|row| row.lifecycle_category == LIFECYCLE_TAG_CURRENT_READABLE)
                .context("current category")?
                .vector_rows,
            1
        );
        assert_eq!(
            inventory
                .lifecycle_rows
                .iter()
                .find(|row| row.lifecycle_category == LIFECYCLE_TAG_SUPERSEDED_READABLE_HISTORY)
                .context("superseded category")?
                .vector_rows,
            0
        );
        assert_eq!(inventory.backlog_vectors, 0);
        let global = read_vector_inventory(&pool, None).await?;
        assert_eq!(
            global.backlog_vectors, 3,
            "global backlog must exclude the protected library's active ingest"
        );
        assert_eq!(
            global
                .lifecycle_rows
                .iter()
                .find(|row| { row.lifecycle_category == LIFECYCLE_TAG_PROTECTED_ACTIVE_INGEST })
                .context("global protected category")?
                .vector_rows,
            2
        );
        Ok(())
    }
    .await;
    pool.close().await;
    database.drop().await?;
    result
}

async fn protect_historical_revision(pool: &PgPool, library: Uuid) -> Result<()> {
    let (document, revision): (Uuid, Uuid) = sqlx::query_as(
        "select document_id, revision_id from knowledge_revision
         where library_id = $1 and revision_number = 1",
    )
    .bind(library)
    .fetch_one(pool)
    .await?;
    let owner = seed_revision_mutation(pool, revision, "running", "pending").await?;
    sqlx::query("update knowledge_document set active_revision_id = $2 where document_id = $1")
        .bind(document)
        .bind(revision)
        .execute(pool)
        .await?;
    sqlx::query("update content_document_head set active_revision_id = $2, latest_mutation_id = $3 where document_id = $1")
        .bind(document)
        .bind(revision)
        .bind(owner.mutation_id)
        .execute(pool)
        .await?;
    sqlx::query(
        "with job as (
          insert into ingest_job (id,workspace_id,library_id,mutation_id,mutation_item_id,
            knowledge_document_id,knowledge_revision_id,job_kind,queue_state,queue_leased_at,
            queue_lease_token,queue_lease_owner)
          select uuidv7(),m.workspace_id,m.library_id,m.id,$2,
            $3,$4,'content_mutation','leased',now(),'inventory-test','inventory-test'
          from content_mutation m where m.id=$1 returning id
         ) insert into ingest_attempt (job_id,attempt_number,attempt_state,lease_token,heartbeat_at)
          select id,1,'leased','inventory-test',now() from job",
    )
    .bind(owner.mutation_id)
    .bind(owner.id)
    .bind(document)
    .bind(revision)
    .execute(pool)
    .await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn zero_backlog_reports_no_age_and_never_lease_still_samples() -> Result<()> {
    let (database, pool) = connect().await?;
    let result: Result<()> = async {
        // (a) A library with only a current vector: zero backlog, no fabricated age.
        let _ = seed_current_and_history(&pool, 0).await?;
        let inventory = read_vector_inventory(&pool, None).await?;
        assert_eq!(inventory.backlog_vectors, 0);
        assert_eq!(
            inventory.backlog_oldest_age_seconds, None,
            "zero backlog must not invent a positive age"
        );
        let snapshot = sample(&pool).await?;
        let backlog = snapshot.backlog.context("snapshot backlog")?;
        assert_eq!(backlog.vectors, 0);
        assert_eq!(backlog.oldest_age_seconds, None);

        // (b) A lease row with a NULL `last_completed_at` is ignored by the
        // global `max(last_completed_at)` and decodes as "never succeeded",
        // instead of erroring on the NULL.
        let (_, null_lease_library) = seed_library(&pool).await?;
        sqlx::query(
            "insert into maintenance_job_run (class, scope_kind, scope_id, last_completed_at)
         values ($1, 'library', $2, null)",
        )
        .bind(MaintenanceClass::GcSupersededVectors.as_str())
        .bind(null_lease_library)
        .execute(&pool)
        .await?;
        let snapshot = sample(&pool).await?;
        assert_eq!(snapshot.gc_last_success_unix_seconds, None);

        // (c) No lease row at all still yields a sample with no last success.
        let _ = seed_library(&pool).await?;
        let snapshot = sample(&pool).await?;
        assert_eq!(snapshot.gc_last_success_unix_seconds, None);
        Ok(())
    }
    .await;
    pool.close().await;
    database.drop().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn positive_last_success_decodes_as_unix_seconds() -> Result<()> {
    let (database, pool) = connect().await?;
    let result: Result<()> = async {
        let (_, library_id) = seed_library(&pool).await?;
        sqlx::query(
            "insert into maintenance_job_run (class, scope_kind, scope_id, last_completed_at)
         values ($1, 'library', $2, now() - interval '1 hour')",
        )
        .bind(MaintenanceClass::GcSupersededVectors.as_str())
        .bind(library_id)
        .execute(&pool)
        .await?;

        let snapshot = sample(&pool).await?;
        let timestamp = snapshot
            .gc_last_success_unix_seconds
            .context("a completed lease row must decode a positive timestamp")?;
        assert!(timestamp > 0.0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |elapsed| elapsed.as_secs_f64());
        assert!((now - timestamp - 3_600.0).abs() < 300.0, "≈1 hour ago");
        Ok(())
    }
    .await;
    pool.close().await;
    database.drop().await?;
    result
}

/// Seed one durable GC-completion row for a library scope, using a SQL time
/// expression (`now() - interval '…'` or `null`).
async fn seed_gc_completion(pool: &PgPool, library_id: Uuid, completed_at: &str) -> Result<()> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "insert into maintenance_job_run (class, scope_kind, scope_id, last_completed_at)
         values ($1, 'library', $2, {completed_at})"
    )))
    .bind(MaintenanceClass::GcSupersededVectors.as_str())
    .bind(library_id)
    .execute(pool)
    .await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn global_snapshot_sums_two_libraries_and_some_scope_stays_isolated() -> Result<()> {
    let (database, pool) = connect().await?;
    let result: Result<()> = async {
        let (_, first_library) = seed_current_and_history(&pool, 2).await?;
        let (_, second_library) = seed_current_and_history(&pool, 3).await?;

        // None scope: the counts are the sum across libraries and the oldest
        // backlog age is the real global maximum (two seeded days old).
        let global = read_vector_inventory(&pool, None).await?;
        let global_history = global
            .lifecycle_rows
            .iter()
            .find(|row| row.lifecycle_category == LIFECYCLE_TAG_SUPERSEDED_READABLE_HISTORY)
            .context("global history category")?;
        assert_eq!(
            global_history.vector_rows, 5,
            "global history rows must be the sum of both libraries"
        );
        let global_current = global
            .lifecycle_rows
            .iter()
            .find(|row| row.lifecycle_category == LIFECYCLE_TAG_CURRENT_READABLE)
            .context("global current category")?;
        assert_eq!(global_current.vector_rows, 2, "each library contributes its readable vector");
        assert_eq!(global.backlog_vectors, 5, "global backlog is the sum of both libraries");
        let age =
            global.backlog_oldest_age_seconds.context("global backlog must carry a real age")?;
        assert!(
            age > 86_400.0,
            "the seeded two-day-old rows are the real global oldest, got {age}"
        );

        // Some scope: the same query restricted to one library is exactly that
        // library, so isolation is preserved for operator diagnostics.
        assert_eq!(read_vector_inventory(&pool, Some(first_library)).await?.backlog_vectors, 2);
        assert_eq!(read_vector_inventory(&pool, Some(second_library)).await?.backlog_vectors, 3);

        // Latest durable GC completion of any library, never a drained claim:
        // seed A 3h ago, B 1h ago and a NULL row, then expect B's timestamp.
        seed_gc_completion(&pool, first_library, "now() - interval '3 hours'").await?;
        seed_gc_completion(&pool, second_library, "now() - interval '1 hour'").await?;
        let (_, null_scope_library) = seed_library(&pool).await?;
        seed_gc_completion(&pool, null_scope_library, "null").await?;
        let snapshot = sample(&pool).await?;
        let timestamp = snapshot
            .gc_last_success_unix_seconds
            .context("the latest completed lease must decode")?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |elapsed| elapsed.as_secs_f64());
        assert!(
            (now - timestamp - 3_600.0).abs() < 300.0,
            "global last-success must be the newest completed row, not the NULL or the 3-hour row"
        );
        Ok(())
    }
    .await;
    pool.close().await;
    database.drop().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn probe_transaction_is_read_only_with_a_bound_statement_timeout() -> Result<()> {
    let (database, pool) = connect().await?;
    let result: Result<()> = async {
        let mut transaction = pool.begin().await?;
        apply_inventory_transaction_settings(&mut transaction).await?;

        let read_only: String =
            sqlx::query_scalar("show transaction_read_only").fetch_one(&mut *transaction).await?;
        let timeout: String =
            sqlx::query_scalar("show statement_timeout").fetch_one(&mut *transaction).await?;
        assert_eq!(read_only, "on");
        let isolation: String =
            sqlx::query_scalar("show transaction_isolation").fetch_one(&mut *transaction).await?;
        assert_eq!(isolation, "repeatable read");
        // `set_config('statement_timeout', '30000', true)` means 30s.
        assert_eq!(timeout, "30s");
        transaction.rollback().await?;
        Ok(())
    }
    .await;
    pool.close().await;
    database.drop().await?;
    result
}
