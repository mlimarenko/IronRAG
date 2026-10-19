use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::{
        knowledge_rows::KnowledgeChunkVectorRow,
        persistence::Persistence,
        repositories::{self, catalog_repository, content_knowledge_projection_repository},
    },
    services::{
        catalog_service::{CreateLibraryCommand, CreateWorkspaceCommand},
        knowledge::service::{
            CreateKnowledgeDocumentCommand, CreateKnowledgeRevisionCommand,
            PromoteKnowledgeDocumentCommand,
        },
        maintenance::{
            gc::{self as maintenance_gc, GcSupersededVectorsOptions},
            lease::{
                self as maintenance_lease, LeaseError, MaintenanceClass, MaintenanceRunState, Scope,
            },
            scheduler as maintenance_scheduler,
        },
    },
};

struct TempPostgresDatabase {
    name: String,
    admin_url: String,
    database_url: String,
}

fn assert_seeded_gc_report(
    report: &maintenance_gc::SupersededVectorGcReport,
    documents: i64,
    vectors: i64,
) -> Result<()> {
    anyhow::ensure!(documents > 0 && vectors > 0, "seeded GC expectations must be positive");
    for (field, actual, expected) in [
        ("documents_visited", report.documents_visited, documents),
        ("documents_with_superseded_vectors", report.documents_with_superseded_vectors, documents),
        ("superseded_vectors_removed", report.superseded_vectors_removed, vectors),
        ("null_head_vectors_removed", report.null_head_vectors_removed, 0),
    ] {
        anyhow::ensure!(
            actual == expected,
            "seeded GC report {field}: expected {expected}, got {actual}"
        );
    }
    Ok(())
}

async fn insert_runtime_graph_gc_zombie_node(
    pool: &PgPool,
    library_id: Uuid,
    node_id: Uuid,
) -> Result<()> {
    sqlx::query(
        "insert into runtime_graph_node (
             id, library_id, canonical_key, label, node_type, projection_version
         ) values ($1, $2, $3, $3, 'entity', 1)",
    )
    .bind(node_id)
    .bind(library_id)
    .bind(format!("runtime-graph-gc-node-{node_id}"))
    .execute(pool)
    .await?;
    Ok(())
}

async fn attach_runtime_graph_gc_bundle_entity(
    pool: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    node_id: Uuid,
    bundle_id: Uuid,
) -> Result<()> {
    sqlx::query(
        "insert into knowledge_context_bundle (
             bundle_id, workspace_id, library_id, bundle_state, bundle_strategy,
             requested_mode, resolved_mode, verification_state, created_at, updated_at
         ) values ($1, $2, $3, 'ready', 'test', 'auto', 'auto', 'verified', now(), now())",
    )
    .bind(bundle_id)
    .bind(workspace_id)
    .bind(library_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "insert into knowledge_bundle_entity (
             bundle_id, entity_id, library_id, rank, score, created_at
         ) values ($1, $2, $3, 0, 1.0, now())",
    )
    .bind(bundle_id)
    .bind(node_id)
    .bind(library_id)
    .execute(pool)
    .await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn runtime_graph_scheduler_enforces_physical_budget_and_replays_lost_cursor() -> Result<()> {
    let fixture = KnowledgeLifecycleFixture::create().await?;
    let result = async {
        let pool = &fixture.state.persistence.postgres;
        let node_phase = serde_json::json!({"version": 1, "phase": "zombie_nodes"});

        let current_document_id = Uuid::now_v7();
        let current_revision_id = Uuid::now_v7();
        fixture
            .state
            .application_services
            .knowledge
            .create_document_shell(
                &fixture.state,
                CreateKnowledgeDocumentCommand {
                    document_id: current_document_id,
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: format!("runtime-graph-gc-current-{current_document_id}"),
                    file_name: Some("runtime-graph-gc-current.txt".to_string()),
                    title: Some("Runtime graph GC current source".to_string()),
                    document_state: "active".to_string(),
                },
            )
            .await?;
        fixture
            .state
            .application_services
            .knowledge
            .write_revision(
                &fixture.state,
                knowledge_revision_command(
                    &fixture,
                    KnowledgeRevisionSeed {
                        document_id: current_document_id,
                        revision_id: current_revision_id,
                        revision_number: 1,
                        revision_kind: "upload",
                        title: "Runtime graph GC current revision",
                        normalized_text: "Current evidence supports a retained graph edge.",
                    },
                ),
            )
            .await?;
        let current_chunk_id = write_chunk(
            &fixture,
            current_document_id,
            current_revision_id,
            0,
            "Current evidence supports a retained graph edge.",
        )
        .await?;
        publish_authoritative_content_head(
            &fixture,
            current_document_id,
            "active",
            Some(current_revision_id),
            Some(current_revision_id),
        )
        .await?;
        sqlx::query(
            "insert into content_chunk (
                 id, revision_id, chunk_index, start_offset, end_offset,
                 token_count, normalized_text, text_checksum
             ) values ($1, $2, 0, 0, 48, 8, $3, $4)",
        )
        .bind(current_chunk_id)
        .bind(current_revision_id)
        .bind("Current evidence supports a retained graph edge.")
        .bind(format!("sha256:runtime-graph-gc:{current_chunk_id}"))
        .execute(pool)
        .await?;
        fixture
            .state
            .application_services
            .knowledge
            .promote_document(
                &fixture.state,
                PromoteKnowledgeDocumentCommand { document_id: current_document_id },
            )
            .await?;
        let embedding_profile_key = format!("runtime-graph-gc:{}", Uuid::now_v7().simple());
        write_vector(
            &fixture,
            current_chunk_id,
            current_revision_id,
            &embedding_profile_key,
            vec![0.25, 0.75],
        )
        .await?;

        let retained_from = repositories::upsert_runtime_graph_node(
            pool,
            repositories::RuntimeGraphNodeInput {
                library_id: fixture.library_id,
                identity_key: "runtime-graph-gc:retained-from",
                label: "Retained From",
                node_type: "entity",
                document_id: None,
                aliases_json: serde_json::json!([]),
                summary: None,
                metadata_json: serde_json::json!({}),
                support_count: 1,
                projection_version: 1,
            },
        )
        .await?;
        let retained_to = repositories::upsert_runtime_graph_node(
            pool,
            repositories::RuntimeGraphNodeInput {
                library_id: fixture.library_id,
                identity_key: "runtime-graph-gc:retained-to",
                label: "Retained To",
                node_type: "entity",
                document_id: None,
                aliases_json: serde_json::json!([]),
                summary: None,
                metadata_json: serde_json::json!({}),
                support_count: 1,
                projection_version: 1,
            },
        )
        .await?;
        let retained_edge = repositories::upsert_runtime_graph_edge(
            pool,
            repositories::RuntimeGraphEdgeInput {
                library_id: fixture.library_id,
                from_node_id: retained_from.id,
                to_node_id: retained_to.id,
                relation_type: "supports",
                identity_key: "runtime-graph-gc:retained-edge",
                summary: None,
                weight: Some(1.0),
                support_count: 1,
                metadata_json: serde_json::json!({}),
                projection_version: 1,
            },
        )
        .await?;
        for (target_kind, target_id, context_key) in [
            ("node", retained_from.id, "retained-from"),
            ("node", retained_to.id, "retained-to"),
            ("edge", retained_edge.id, "retained-edge"),
        ] {
            repositories::create_runtime_graph_evidence(
                pool,
                repositories::CreateRuntimeGraphEvidenceInput {
                    library_id: fixture.library_id,
                    target_kind,
                    target_id,
                    document_id: Some(current_document_id),
                    revision_id: Some(current_revision_id),
                    activated_by_attempt_id: None,
                    chunk_id: Some(current_chunk_id),
                    source_file_name: Some("runtime-graph-gc-current.txt"),
                    page_ref: None,
                    evidence_text: "Current readable evidence retained by runtime graph GC",
                    confidence_score: Some(1.0),
                    evidence_context_key: context_key,
                },
            )
            .await?;
        }

        let oversized_node = Uuid::from_u128(1_000);
        insert_runtime_graph_gc_zombie_node(pool, fixture.library_id, oversized_node).await?;
        for bundle in 10_000_u128..10_003 {
            attach_runtime_graph_gc_bundle_entity(
                pool,
                fixture.workspace_id,
                fixture.library_id,
                oversized_node,
                Uuid::from_u128(bundle),
            )
            .await?;
        }
        let oversized = maintenance_gc::run_runtime_graph_scheduler_batch_with_budget_for_test(
            &fixture.state,
            fixture.library_id,
            Some(&node_phase),
            3,
        )
        .await
        .expect_err("an indivisible four-row closure must exceed a three-row budget");
        assert_eq!(oversized.code(), "runtime_graph_oversized_closure");
        let untouched = sqlx::query_as::<_, (i64, i64)>(
            "select
                 (select count(*)::bigint from runtime_graph_node where id = $1),
                 (select count(*)::bigint from knowledge_bundle_entity where entity_id = $1)",
        )
        .bind(oversized_node)
        .fetch_one(pool)
        .await?;
        assert_eq!(untouched, (1, 3));
        sqlx::query("delete from knowledge_bundle_entity where entity_id = $1")
            .bind(oversized_node)
            .execute(pool)
            .await?;
        sqlx::query("delete from runtime_graph_node where id = $1")
            .bind(oversized_node)
            .execute(pool)
            .await?;

        let first = Uuid::from_u128(1_100);
        let second = Uuid::from_u128(1_101);
        let third = Uuid::from_u128(1_102);
        for node_id in [first, second, third] {
            insert_runtime_graph_gc_zombie_node(pool, fixture.library_id, node_id).await?;
        }
        attach_runtime_graph_gc_bundle_entity(
            pool,
            fixture.workspace_id,
            fixture.library_id,
            first,
            Uuid::from_u128(11_000),
        )
        .await?;
        for bundle_id in [Uuid::from_u128(11_001), Uuid::from_u128(11_002)] {
            attach_runtime_graph_gc_bundle_entity(
                pool,
                fixture.workspace_id,
                fixture.library_id,
                second,
                bundle_id,
            )
            .await?;
        }

        maintenance_lease::ensure_row(
            pool,
            MaintenanceClass::GcRuntimeGraph,
            Scope::Library(fixture.library_id),
            None,
        )
        .await?;
        let leased = maintenance_lease::acquire_next_due(
            pool,
            MaintenanceClass::GcRuntimeGraph,
            "runtime-graph-scheduler-a",
            maintenance_lease::AcquirePreference::InFlight,
            std::time::Duration::from_secs(3600),
        )
        .await?
        .context("runtime graph budget test could not acquire lease")?;
        let mut current_cursor = leased.cursor_json.clone();
        for expected_cursor in [
            serde_json::json!({"version": 1, "phase": "phantom_chunk_evidence"}),
            serde_json::json!({"version": 1, "phase": "orphan_evidence_before"}),
            serde_json::json!({"version": 1, "phase": "zombie_edges"}),
            node_phase.clone(),
        ] {
            let phase = maintenance_gc::run_runtime_graph_scheduler_batch_with_budget_for_test(
                &fixture.state,
                fixture.library_id,
                Some(&current_cursor),
                4,
            )
            .await?;
            assert_eq!(phase.physical_rows_removed, 0);
            current_cursor = phase.next_cursor_for_test().cloned().unwrap_or_default();
            assert_eq!(current_cursor, expected_cursor);
            maintenance_lease::update_cursor(
                pool,
                leased.id,
                "runtime-graph-scheduler-a",
                current_cursor.clone(),
            )
            .await?;
        }

        let first_batch = maintenance_gc::run_runtime_graph_scheduler_batch_with_budget_for_test(
            &fixture.state,
            fixture.library_id,
            Some(&node_phase),
            4,
        )
        .await?;
        assert_eq!(first_batch.candidate_roots, 3);
        assert_eq!(first_batch.nodes_removed, 1);
        assert_eq!(first_batch.dependent_rows_removed, 1);
        assert_eq!(first_batch.physical_rows_removed, 2);
        assert!(first_batch.physical_rows_removed <= 4);
        assert_eq!(first_batch.next_cursor_for_test(), Some(&node_phase));

        sqlx::query(
            "update maintenance_job_run
             set owner_node = 'runtime-graph-scheduler-b'
             where id = $1 and state = 'leased'",
        )
        .bind(leased.id)
        .execute(pool)
        .await?;
        let lost_cursor = maintenance_lease::update_cursor(
            pool,
            leased.id,
            "runtime-graph-scheduler-a",
            first_batch.next_cursor_for_test().cloned().unwrap_or_default(),
        )
        .await;
        assert!(matches!(lost_cursor, Err(LeaseError::LostOwnership { .. })));
        let current = maintenance_lease::load(
            pool,
            MaintenanceClass::GcRuntimeGraph,
            Scope::Library(fixture.library_id),
        )
        .await?
        .context("runtime graph lease disappeared after ownership transfer")?;
        assert_eq!(current.cursor_json, node_phase);

        let replay = maintenance_gc::run_runtime_graph_scheduler_batch_with_budget_for_test(
            &fixture.state,
            fixture.library_id,
            Some(&current.cursor_json),
            4,
        )
        .await?;
        assert_eq!(replay.nodes_removed, 2);
        assert_eq!(replay.dependent_rows_removed, 2);
        assert_eq!(replay.physical_rows_removed, 4);
        assert!(replay.physical_rows_removed <= 4);
        let orphan_after = serde_json::json!({"version": 1, "phase": "orphan_evidence_after"});
        assert_eq!(replay.next_cursor_for_test(), Some(&orphan_after));
        maintenance_lease::update_cursor(
            pool,
            leased.id,
            "runtime-graph-scheduler-b",
            orphan_after.clone(),
        )
        .await?;
        let tail = maintenance_gc::run_runtime_graph_scheduler_batch_with_budget_for_test(
            &fixture.state,
            fixture.library_id,
            Some(&orphan_after),
            4,
        )
        .await?;
        assert_eq!(tail.physical_rows_removed, 0);
        assert!(!tail.work_remaining);
        assert_eq!(tail.next_cursor_for_test(), Some(&serde_json::json!({})));
        maintenance_lease::update_cursor(
            pool,
            leased.id,
            "runtime-graph-scheduler-b",
            serde_json::json!({}),
        )
        .await?;
        maintenance_lease::complete(
            pool,
            leased.id,
            "runtime-graph-scheduler-b",
            first_batch.physical_rows_removed + replay.physical_rows_removed,
            0,
            std::time::Duration::from_secs(60),
        )
        .await?;
        let survivors = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from runtime_graph_node where library_id = $1",
        )
        .bind(fixture.library_id)
        .fetch_one(pool)
        .await?;
        assert_eq!(survivors, 2);
        let retained = sqlx::query_as::<_, (i64, i64, i64)>(
            "select
                 (select count(*)::bigint from runtime_graph_node where id = any($1)),
                 (select count(*)::bigint from runtime_graph_edge where id = $2),
                 (select count(*)::bigint
                  from runtime_graph_evidence
                  where target_id = any($3))",
        )
        .bind([retained_from.id, retained_to.id])
        .bind(retained_edge.id)
        .bind([retained_from.id, retained_to.id, retained_edge.id])
        .fetch_one(pool)
        .await?;
        assert_eq!(retained, (2, 1, 3));
        let current_vectors =
            fixture.state.search_store.list_chunk_vectors_by_chunk(current_chunk_id).await?;
        assert_eq!(current_vectors.len(), 1);
        assert_eq!(current_vectors[0].embedding_model_key, embedding_profile_key);

        sqlx::query(
            "update maintenance_job_run
             set state = 'pending', owner_node = null, heartbeat_at = null,
                 attempts = 0, error_code = null, error_text = null,
                 last_started_at = null,
                 next_due_at = now()
             where class = 'gc.runtime-graph'
               and scope_kind = 'library'
               and scope_id = $1",
        )
        .bind(fixture.library_id)
        .execute(pool)
        .await?;
        let before_busy_scheduler = maintenance_lease::load(
            pool,
            MaintenanceClass::GcRuntimeGraph,
            Scope::Library(fixture.library_id),
        )
        .await?
        .context("runtime graph lease disappeared before busy scheduler witness")?;

        let admission_blocked_node = Uuid::from_u128(1_200);
        insert_runtime_graph_gc_zombie_node(pool, fixture.library_id, admission_blocked_node)
            .await?;
        let mut admission = pool.begin().await?;
        let admitted_job_id = sqlx::query_scalar::<_, Uuid>(
            "insert into ingest_job (
                 workspace_id, library_id, job_kind, queue_state, priority
             ) values ($1, $2, 'graph_refresh', 'queued', 100)
             returning id",
        )
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .fetch_one(&mut *admission)
        .await?;
        let busy = maintenance_gc::run_runtime_graph_scheduler_batch_with_budget_for_test(
            &fixture.state,
            fixture.library_id,
            Some(&node_phase),
            4,
        )
        .await
        .expect_err("uncommitted ingest admission must make graph GC defer without blocking");
        assert_eq!(busy.code(), "library_busy");
        let blocked_node_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from runtime_graph_node where id = $1",
        )
        .bind(admission_blocked_node)
        .fetch_one(pool)
        .await?;
        assert_eq!(blocked_node_count, 1, "busy GC must not mutate graph rows");

        maintenance_scheduler::process_runtime_graph_class_once_for_test(
            &fixture.state,
            "runtime-graph-scheduler-busy",
        )
        .await;
        let after_busy_scheduler = maintenance_lease::load(
            pool,
            MaintenanceClass::GcRuntimeGraph,
            Scope::Library(fixture.library_id),
        )
        .await?
        .context("runtime graph lease disappeared after busy scheduler witness")?;
        assert_eq!(after_busy_scheduler.state, MaintenanceRunState::Pending);
        assert_eq!(after_busy_scheduler.attempts, before_busy_scheduler.attempts);
        assert!(after_busy_scheduler.owner_node.is_none());
        assert!(after_busy_scheduler.heartbeat_at.is_none());
        assert!(after_busy_scheduler.error_code.is_none());
        assert!(after_busy_scheduler.error_text.is_none());
        assert_eq!(
            after_busy_scheduler.rows_removed_total,
            before_busy_scheduler.rows_removed_total,
        );
        assert_eq!(
            after_busy_scheduler.bytes_reclaimed_total,
            before_busy_scheduler.bytes_reclaimed_total,
        );
        let busy_completed_at = after_busy_scheduler
            .last_completed_at
            .context("busy scheduler dispatch did not complete its lease")?;
        assert_eq!(
            after_busy_scheduler.next_due_at.signed_duration_since(busy_completed_at).num_seconds(),
            maintenance_scheduler::ACTIVE_INGEST_RETRY.as_secs() as i64,
        );
        let scheduler_blocked_node_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from runtime_graph_node where id = $1",
        )
        .bind(admission_blocked_node)
        .fetch_one(pool)
        .await?;
        assert_eq!(
            scheduler_blocked_node_count, 1,
            "busy scheduler dispatch must requeue without graph mutations",
        );

        admission.commit().await?;
        let committed_job = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from ingest_job where id = $1 and queue_state = 'queued'",
        )
        .bind(admitted_job_id)
        .fetch_one(pool)
        .await?;
        assert_eq!(committed_job, 1);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// One tick must drain more than a single row per class and requeue every
/// processed row into the future, so overdue rows shrink instead of staying
/// pinned behind a `library_count * tick_interval` walk.
///
/// `dispatch_gc_runtime_graph` short-circuits to a no-op `Ok` for a library
/// that is absent from `catalog_library`, so synthetic rows with arbitrary
/// scope ids exercise the full lease/dispatch/complete path without graph
/// fixtures.
#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn maintenance_tick_drains_a_batch_and_leaves_rows_pending_without_overdue_accumulation()
-> Result<()> {
    const SYNTHETIC_ROWS: i64 = 300;

    let fixture = KnowledgeLifecycleFixture::create().await?;
    let result = async {
        let pool = &fixture.state.persistence.postgres;

        let synthetic_scope_ids: Vec<Uuid> = sqlx::query_scalar::<_, Uuid>(
            "insert into maintenance_job_run (
                 class, scope_kind, scope_id, state, next_due_at, cursor_json
             )
             select 'gc.runtime-graph', 'library', gen_random_uuid(), 'pending',
                    now() - interval '3600 seconds', '{}'::jsonb
             from generate_series(1, $1)
             returning scope_id",
        )
        .bind(SYNTHETIC_ROWS)
        .fetch_all(pool)
        .await?;
        assert_eq!(
            synthetic_scope_ids.len(),
            usize::try_from(SYNTHETIC_ROWS).context("synthetic row count overflow")?,
        );

        let overdue_before = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from maintenance_job_run \
             where class = 'gc.runtime-graph' \
               and state = 'pending' \
               and next_due_at <= now()",
        )
        .fetch_one(pool)
        .await?;
        assert!(
            overdue_before >= SYNTHETIC_ROWS,
            "synthetic overdue rows must be present before the tick",
        );

        let tick_started_at = Utc::now();
        maintenance_scheduler::run_tick_for_test(&fixture.state, "maintenance-tick-batch").await;

        let processed = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from maintenance_job_run \
             where class = 'gc.runtime-graph' \
               and scope_id = any($1) \
               and state = 'pending' \
               and owner_node is null \
               and heartbeat_at is null \
               and error_code is null \
               and next_due_at > now() \
               and last_completed_at is not null \
               and last_completed_at >= $2",
        )
        .bind(&synthetic_scope_ids)
        .bind(tick_started_at)
        .fetch_one(pool)
        .await?;
        assert!(processed > 1, "a tick must drain more than one row; saw {processed}");

        let malformed = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from maintenance_job_run \
             where class = 'gc.runtime-graph' \
               and scope_id = any($1) \
               and last_completed_at is not null \
               and last_completed_at >= $2 \
               and (state <> 'pending' \
                    or owner_node is not null \
                    or heartbeat_at is not null \
                    or error_code is not null \
                    or next_due_at <= now())",
        )
        .bind(&synthetic_scope_ids)
        .bind(tick_started_at)
        .fetch_one(pool)
        .await?;
        assert_eq!(malformed, 0, "processed rows must be cleanly requeued as pending");

        let overdue_after = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from maintenance_job_run \
             where class = 'gc.runtime-graph' \
               and state = 'pending' \
               and next_due_at <= now()",
        )
        .fetch_one(pool)
        .await?;
        assert!(
            overdue_after < overdue_before,
            "overdue rows must strictly decrease: {overdue_before} -> {overdue_after}",
        );

        let terminal = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from maintenance_job_run \
             where class = 'gc.runtime-graph' \
               and state::text in ('completed', 'failed')",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(terminal, 0, "completed/failed are write-dead states and must never appear");

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// A never-started (`cursor_json = '{}'`) row must not be starved behind a
/// mid-sweep population larger than the per-tick batch. The unconditional
/// in-flight-first ordering spent every slot on the 300 mid-sweep rows and this
/// sentinel row was never selected.
#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn maintenance_tick_does_not_starve_fresh_rows_behind_a_mid_sweep_population() -> Result<()> {
    const IN_FLIGHT_ROWS: i64 = 300;

    let fixture = KnowledgeLifecycleFixture::create().await?;
    let result = async {
        let pool = &fixture.state.persistence.postgres;

        let in_flight_scope_ids: Vec<Uuid> = sqlx::query_scalar::<_, Uuid>(
            "insert into maintenance_job_run (
                 class, scope_kind, scope_id, state, next_due_at, cursor_json
             )
             select 'gc.runtime-graph', 'library', gen_random_uuid(), 'pending',
                    now() - interval '3600 seconds',
                    '{\"version\":1,\"phase\":\"orphan_evidence_after\"}'::jsonb
             from generate_series(1, $1)
             returning scope_id",
        )
        .bind(IN_FLIGHT_ROWS)
        .fetch_all(pool)
        .await?;
        assert_eq!(
            in_flight_scope_ids.len(),
            usize::try_from(IN_FLIGHT_ROWS).context("in-flight row count overflow")?,
        );

        // A single never-started row, newer than the mid-sweep backlog so the
        // old unconditional in-flight-first order can never reach it.
        let fresh_scope_id = Uuid::now_v7();
        sqlx::query(
            "insert into maintenance_job_run (
                 class, scope_kind, scope_id, state, next_due_at, cursor_json
             ) values ('gc.runtime-graph', 'library', $1, 'pending',
                       now() - interval '60 seconds', '{}'::jsonb)",
        )
        .bind(fresh_scope_id)
        .execute(pool)
        .await?;

        let tick_started_at = Utc::now();
        maintenance_scheduler::run_tick_for_test(&fixture.state, "maintenance-tick-fairness").await;

        let fresh = maintenance_lease::load(
            pool,
            MaintenanceClass::GcRuntimeGraph,
            Scope::Library(fresh_scope_id),
        )
        .await?
        .context("fresh sentinel row vanished during the tick")?;
        assert!(
            fresh.last_completed_at.is_some_and(|completed| completed >= tick_started_at),
            "a never-started row must be picked within one tick, not starved behind {} \
             mid-sweep rows; saw last_completed_at={:?}",
            IN_FLIGHT_ROWS,
            fresh.last_completed_at,
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// Move a library's source generation to `secs_ago` seconds before now, using
/// the same microsecond wall-clock units the ingest write paths maintain.
async fn set_library_source_truth_secs_ago(
    pool: &PgPool,
    library_id: Uuid,
    secs_ago: f64,
) -> Result<()> {
    sqlx::query(
        "update catalog_library \
         set source_truth_version = \
             (extract(epoch from now() - make_interval(secs => $2::double precision)) * 1000000)::bigint \
         where id = $1",
    )
    .bind(library_id)
    .bind(secs_ago)
    .execute(pool)
    .await?;
    Ok(())
}

/// Seed the fixture library's own `gc.runtime-graph` row: both watermarks
/// `secs_ago` seconds in the past and `next_due_at` `next_due_in_secs` seconds
/// from now (negative = already past). `last_started_at` matters as much as
/// `last_completed_at` because the changed-library fence is measured from the
/// start. Bootstrap's `on conflict do nothing` leaves this row alone during
/// the tick.
async fn seed_runtime_graph_row(
    pool: &PgPool,
    library_id: Uuid,
    cursor_json: &str,
    secs_ago: f64,
    next_due_in_secs: f64,
) -> Result<()> {
    sqlx::query(
        "insert into maintenance_job_run (
             class, scope_kind, scope_id, state, next_due_at,
             last_started_at, last_completed_at, cursor_json
         ) values ('gc.runtime-graph', 'library', $1, 'pending',
                   now() + make_interval(secs => $4::double precision),
                   now() - make_interval(secs => $2::double precision),
                   now() - make_interval(secs => $2::double precision),
                   $3::jsonb)",
    )
    .bind(library_id)
    .bind(secs_ago)
    .bind(cursor_json)
    .bind(next_due_in_secs)
    .execute(pool)
    .await?;
    Ok(())
}

/// The finished/idle shape, `next_due_at` already in the past.
async fn seed_due_idle_runtime_graph_row(
    pool: &PgPool,
    library_id: Uuid,
    secs_ago: f64,
) -> Result<()> {
    seed_runtime_graph_row(pool, library_id, "{}", secs_ago, -60.0).await
}

/// The mid-sweep shape: a non-empty cursor whose re-queue delay has elapsed.
async fn seed_due_mid_sweep_runtime_graph_row(
    pool: &PgPool,
    library_id: Uuid,
    secs_ago: f64,
) -> Result<()> {
    seed_runtime_graph_row(
        pool,
        library_id,
        r#"{"version":1,"phase":"phantom_chunk_evidence"}"#,
        secs_ago,
        -60.0,
    )
    .await
}

async fn load_library_runtime_graph_row(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<maintenance_lease::MaintenanceJobRun> {
    maintenance_lease::load(pool, MaintenanceClass::GcRuntimeGraph, Scope::Library(library_id))
        .await?
        .context("fixture gc.runtime-graph row missing")
}

/// Assert a row was leased, swept and cleanly re-queued within this tick.
fn assert_row_was_swept(
    row: &maintenance_lease::MaintenanceJobRun,
    tick_started_at: DateTime<Utc>,
) {
    assert!(
        row.last_completed_at.is_some_and(|completed| completed >= tick_started_at),
        "a dispatched row must record a completion at or after the tick start; saw {:?}",
        row.last_completed_at,
    );
    assert_eq!(row.state, MaintenanceRunState::Pending, "a swept row is re-queued pending");
    assert!(
        row.owner_node.is_none(),
        "a completed row releases its owner; saw {:?}",
        row.owner_node
    );
    assert!(row.next_due_at > Utc::now(), "a swept row is re-queued into the future");
}

/// An unchanged library whose `next_due_at` deadline is still ahead stays
/// pending and keeps its deadline: it is not overdue, so neither the lease nor
/// the metric may report it as work.
#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn maintenance_tick_skips_a_row_whose_next_due_at_is_still_ahead() -> Result<()> {
    let fixture = KnowledgeLifecycleFixture::create().await?;
    let result = async {
        let pool = &fixture.state.persistence.postgres;
        set_library_source_truth_secs_ago(pool, fixture.library_id, 2.0 * 3600.0).await?;
        let full_pass = fixture.state.settings.maintenance_full_pass_interval_seconds as f64;
        seed_runtime_graph_row(pool, fixture.library_id, "{}", 300.0, full_pass).await?;
        let before = load_library_runtime_graph_row(pool, fixture.library_id).await?;

        maintenance_scheduler::run_tick_for_test(&fixture.state, "maintenance-tick-idle").await;

        let after = load_library_runtime_graph_row(pool, fixture.library_id).await?;
        assert_eq!(
            after.state,
            MaintenanceRunState::Pending,
            "a row before its deadline must not be leased",
        );
        assert!(
            after.owner_node.is_none(),
            "a skipped row stamps no owner; saw {:?}",
            after.owner_node
        );
        assert_eq!(
            after.last_started_at, before.last_started_at,
            "a skipped row keeps its start watermark",
        );
        assert_eq!(
            after.last_completed_at, before.last_completed_at,
            "a skipped row keeps its completion record",
        );
        assert!(
            after.next_due_at > Utc::now(),
            "a non-overdue row keeps a future next_due_at; saw {}",
            after.next_due_at,
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// IRG-219: a row whose `next_due_at` deadline has passed is overdue, and the
/// lease must honour that deadline. Before the fix the changed-library fence
/// ignored `next_due_at` entirely, so an unchanged library's row sat "overdue"
/// for up to the full-pass interval without ever starting. The seed mirrors a
/// row written by the pre-fix `complete` (deadline a class interval out) that
/// time has since passed; the generation is older than the row's own start, so
/// only the honoured deadline can lease it.
#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn maintenance_tick_leases_a_row_whose_next_due_at_passed() -> Result<()> {
    let fixture = KnowledgeLifecycleFixture::create().await?;
    let result = async {
        let pool = &fixture.state.persistence.postgres;
        set_library_source_truth_secs_ago(pool, fixture.library_id, 2.0 * 3600.0).await?;
        seed_due_idle_runtime_graph_row(pool, fixture.library_id, 300.0).await?;

        let tick_started_at = Utc::now();
        maintenance_scheduler::run_tick_for_test(&fixture.state, "maintenance-tick-overdue").await;

        let after = load_library_runtime_graph_row(pool, fixture.library_id).await?;
        assert_row_was_swept(&after, tick_started_at);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// IRG-219: a finished unchanged sweep re-arms at the safety full-pass
/// deadline, so `next_due_at` states the real next run rather than a class
/// interval the fence would ignore. A scope whose library is absent from
/// `catalog_library` short-circuits to a clean `Ok` with no remaining work, so
/// the completion stamps the full pass; before the fix it stamped the class
/// interval and the row looked overdue an hour later while the fence withheld
/// it for a day.
#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn maintenance_completion_rearms_at_the_full_pass_deadline() -> Result<()> {
    let fixture = KnowledgeLifecycleFixture::create().await?;
    let result = async {
        let pool = &fixture.state.persistence.postgres;
        let scope_id = Uuid::now_v7();
        sqlx::query(
            "insert into maintenance_job_run (
                 class, scope_kind, scope_id, state, next_due_at, cursor_json
             ) values ('gc.runtime-graph', 'library', $1, 'pending',
                       now() - interval '60 seconds', '{}'::jsonb)",
        )
        .bind(scope_id)
        .execute(pool)
        .await?;

        let tick_started_at = Utc::now();
        maintenance_scheduler::run_tick_for_test(&fixture.state, "maintenance-tick-rearm").await;

        let after = maintenance_lease::load(
            pool,
            MaintenanceClass::GcRuntimeGraph,
            Scope::Library(scope_id),
        )
        .await?
        .context("synthetic gc.runtime-graph row vanished during the tick")?;
        assert_row_was_swept(&after, tick_started_at);
        let completed =
            after.last_completed_at.context("a swept row must record its completion")?;
        let full_pass = fixture.state.settings.maintenance_full_pass_interval_seconds as i64;
        assert_eq!(
            after.next_due_at.signed_duration_since(completed).num_seconds(),
            full_pass,
            "a finished sweep must re-arm at next_due_at = completion + full_pass_interval",
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// A due row whose library's source generation advanced after the row's own
/// start is leased on the changed-library arm even though its full-pass
/// deadline is still a day away.
#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn maintenance_tick_dispatches_library_whose_source_truth_advanced() -> Result<()> {
    let fixture = KnowledgeLifecycleFixture::create().await?;
    let result = async {
        let pool = &fixture.state.persistence.postgres;
        // Generation now; the row last ran two hours ago — past the one-hour
        // cooldown — and its full-pass deadline is still a day out, so only
        // the changed-library arm can re-arm it.
        set_library_source_truth_secs_ago(pool, fixture.library_id, 0.0).await?;
        let full_pass = fixture.state.settings.maintenance_full_pass_interval_seconds as f64;
        seed_runtime_graph_row(pool, fixture.library_id, "{}", 2.0 * 3600.0, full_pass).await?;

        let tick_started_at = Utc::now();
        maintenance_scheduler::run_tick_for_test(&fixture.state, "maintenance-tick-advanced").await;

        let after = load_library_runtime_graph_row(pool, fixture.library_id).await?;
        assert_row_was_swept(&after, tick_started_at);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// A mid-sweep row carries a non-empty cursor, which keeps it eligible even
/// when the library's generation is stale — the branch ~87 % of live rows ride.
#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn maintenance_tick_continues_a_mid_sweep_row_without_a_generation_advance() -> Result<()> {
    let fixture = KnowledgeLifecycleFixture::create().await?;
    let result = async {
        let pool = &fixture.state.persistence.postgres;
        set_library_source_truth_secs_ago(pool, fixture.library_id, 2.0 * 3600.0).await?;
        seed_due_mid_sweep_runtime_graph_row(pool, fixture.library_id, 300.0).await?;

        let tick_started_at = Utc::now();
        maintenance_scheduler::run_tick_for_test(&fixture.state, "maintenance-tick-mid-sweep")
            .await;

        let after = load_library_runtime_graph_row(pool, fixture.library_id).await?;
        assert_row_was_swept(&after, tick_started_at);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// Even when the library's source generation has not advanced, a row whose
/// full-pass deadline (`next_due_at`) has passed is swept, so every unchanged
/// library still gets its safety full pass.
#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn maintenance_tick_runs_a_full_pass_after_the_safety_interval() -> Result<()> {
    let fixture = KnowledgeLifecycleFixture::create().await?;
    let result = async {
        let pool = &fixture.state.persistence.postgres;
        // The library's generation is explicitly 48 h old, far older than the
        // 25 h-old start, so only the elapsed full-pass deadline can make the
        // row due.
        set_library_source_truth_secs_ago(pool, fixture.library_id, 48.0 * 3600.0).await?;
        seed_due_idle_runtime_graph_row(pool, fixture.library_id, 25.0 * 3600.0).await?;

        let tick_started_at = Utc::now();
        maintenance_scheduler::run_tick_for_test(&fixture.state, "maintenance-tick-full-pass")
            .await;

        let after = load_library_runtime_graph_row(pool, fixture.library_id).await?;
        assert_row_was_swept(&after, tick_started_at);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

impl TempPostgresDatabase {
    async fn create(base_database_url: &str) -> Result<Self> {
        let admin_url = replace_database_name(base_database_url, "postgres")?;
        let name = format!("knowledge_lifecycle_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("failed to connect to postgres admin database for knowledge_lifecycle")?;

        terminate_database_connections(&admin_pool, &name).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{name}\"")))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop stale test database {name}"))?;
        sqlx::query(sqlx::AssertSqlSafe(format!("create database \"{name}\"")))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to create test database {name}"))?;
        admin_pool.close().await;

        Ok(Self { database_url: replace_database_name(base_database_url, &name)?, admin_url, name })
    }

    async fn drop(self) -> Result<()> {
        let admin_pool =
            PgPoolOptions::new().max_connections(1).connect(&self.admin_url).await.context(
                "failed to reconnect postgres admin database for knowledge_lifecycle cleanup",
            )?;
        terminate_database_connections(&admin_pool, &self.name).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{}\"", self.name)))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop test database {}", self.name))?;
        admin_pool.close().await;
        Ok(())
    }
}

struct KnowledgeLifecycleFixture {
    state: AppState,
    postgres: TempPostgresDatabase,
    _content_storage_root: tempfile::TempDir,
    workspace_id: Uuid,
    library_id: Uuid,
}

impl KnowledgeLifecycleFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for knowledge_lifecycle")?;
        let postgres = TempPostgresDatabase::create(&settings.database_url).await?;
        settings.database_url = postgres.database_url.clone();
        let content_storage_root = tempfile::Builder::new()
            .prefix("knowledge-lifecycle-")
            .tempdir()
            .context("failed to create knowledge lifecycle content storage")?;
        settings.content_storage_root = content_storage_root.path().to_string_lossy().into_owned();

        let postgres_pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&postgres.database_url)
            .await
            .context("failed to connect knowledge_lifecycle postgres")?;
        sqlx::raw_sql(include_str!("../../migrations/0001_init.sql"))
            .execute(&postgres_pool)
            .await
            .context("failed to apply authoritative 0001_init.sql for knowledge_lifecycle")?;

        let redis = redis::Client::open(settings.redis_url.clone())
            .context("failed to create redis client for knowledge_lifecycle")?;
        settings.credential_master_key =
            Some("KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=".to_string());
        let state =
            AppState::from_dependencies(settings, Persistence::for_tests(postgres_pool, redis))?;

        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = state
            .application_services
            .catalog
            .create_workspace(
                &state,
                CreateWorkspaceCommand {
                    slug: Some(format!("knowledge-lifecycle-workspace-{suffix}")),
                    display_name: "Knowledge Lifecycle Workspace".to_string(),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create knowledge_lifecycle workspace")?;
        let library = state
            .application_services
            .catalog
            .create_library(
                &state,
                CreateLibraryCommand {
                    workspace_id: workspace.id,
                    slug: Some(format!("knowledge-lifecycle-library-{suffix}")),
                    display_name: "Knowledge Lifecycle Library".to_string(),
                    description: Some("knowledge lifecycle integration fixture".to_string()),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create knowledge_lifecycle library")?;

        Ok(Self {
            state,
            postgres,
            _content_storage_root: content_storage_root,
            workspace_id: workspace.id,
            library_id: library.id,
        })
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.postgres.drop().await
    }
}

fn replace_database_name(database_url: &str, new_database: &str) -> Result<String> {
    let (without_query, query_suffix) = database_url
        .split_once('?')
        .map_or((database_url, None), |(prefix, suffix)| (prefix, Some(suffix)));
    let slash_index = without_query
        .rfind('/')
        .with_context(|| format!("database url is missing database name: {database_url}"))?;
    let mut rebuilt = format!("{}{new_database}", &without_query[..=slash_index]);
    if let Some(query) = query_suffix {
        rebuilt.push('?');
        rebuilt.push_str(query);
    }
    Ok(rebuilt)
}

async fn terminate_database_connections(postgres: &PgPool, database_name: &str) -> Result<()> {
    sqlx::query(
        "select pg_terminate_backend(pid)
         from pg_stat_activity
         where datname = $1
           and pid <> pg_backend_pid()",
    )
    .bind(database_name)
    .execute(postgres)
    .await
    .with_context(|| format!("failed to terminate connections for {database_name}"))?;
    Ok(())
}

struct KnowledgeRevisionSeed<'a> {
    document_id: Uuid,
    revision_id: Uuid,
    revision_number: i64,
    revision_kind: &'a str,
    title: &'a str,
    normalized_text: &'a str,
}

fn knowledge_revision_command(
    fixture: &KnowledgeLifecycleFixture,
    seed: KnowledgeRevisionSeed<'_>,
) -> CreateKnowledgeRevisionCommand {
    let KnowledgeRevisionSeed {
        document_id,
        revision_id,
        revision_number,
        revision_kind,
        title,
        normalized_text,
    } = seed;
    CreateKnowledgeRevisionCommand {
        revision_id,
        workspace_id: fixture.workspace_id,
        library_id: fixture.library_id,
        document_id,
        revision_number,
        revision_state: "active".to_string(),
        revision_kind: revision_kind.to_string(),
        storage_ref: Some(format!("memory://{revision_id}")),
        source_uri: Some(format!("memory://knowledge-lifecycle/{revision_id}")),
        document_hint: None,
        mime_type: "text/plain".to_string(),
        checksum: format!("sha256:{0}{0}", revision_id.simple()),
        byte_size: i64::try_from(normalized_text.len()).unwrap_or(i64::MAX),
        title: Some(title.to_string()),
        normalized_text: Some(normalized_text.to_string()),
        text_checksum: Some(format!("sha256:text:{revision_id}")),
        text_state: "text_readable".to_string(),
        vector_state: "ready".to_string(),
        // `accepted` is the admitted-but-not-yet-processed state;
        // `knowledge_revision_graph_state_canonical_check` rejects `pending`.
        graph_state: "accepted".to_string(),
        text_readable_at: Some(Utc::now()),
        vector_ready_at: Some(Utc::now()),
        graph_ready_at: None,
        superseded_by_revision_id: None,
    }
}

async fn write_chunk(
    fixture: &KnowledgeLifecycleFixture,
    document_id: Uuid,
    revision_id: Uuid,
    chunk_index: i32,
    text: &str,
) -> Result<Uuid> {
    crate::knowledge_seed::write_ready_chunk(
        &fixture.state,
        crate::knowledge_seed::ReadyChunkSeed {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            document_id,
            revision_id,
            chunk_index,
            text,
        },
    )
    .await
}

async fn write_vector(
    fixture: &KnowledgeLifecycleFixture,
    chunk_id: Uuid,
    revision_id: Uuid,
    embedding_profile_key: &str,
    vector: Vec<f32>,
) -> Result<()> {
    let dimensions = i32::try_from(vector.len()).context("test vector dimension overflowed i32")?;
    fixture
        .state
        .search_store
        .upsert_chunk_vector(&KnowledgeChunkVectorRow {
            vector_id: Uuid::now_v7(),
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            chunk_id,
            revision_id,
            embedding_model_key: embedding_profile_key.to_string(),
            vector_kind: "chunk_embedding".to_string(),
            dimensions,
            vector,
            freshness_generation: 1,
            created_at: Utc::now(),
            occurred_at: None,
            occurred_until: None,
        })
        .await
        .context("failed to write knowledge lifecycle chunk vector")?;
    Ok(())
}

async fn chunk_vector_manifest_count(
    fixture: &KnowledgeLifecycleFixture,
    embedding_profile_key: &str,
    dimensions: i32,
) -> Result<i64> {
    sqlx::query_scalar::<_, i64>(
        "select row_count
         from knowledge_vector_relation_manifest
         where library_id = $1
           and dim = $2
           and embedding_model_key = $3
           and vector_kind = 'chunk_embedding'",
    )
    .bind(fixture.library_id)
    .bind(dimensions)
    .bind(embedding_profile_key)
    .fetch_one(&fixture.state.persistence.postgres)
    .await
    .context("failed to read chunk vector manifest row_count")
}

async fn publish_authoritative_content_head(
    fixture: &KnowledgeLifecycleFixture,
    document_id: Uuid,
    document_state: &str,
    active_revision_id: Option<Uuid>,
    readable_revision_id: Option<Uuid>,
) -> Result<()> {
    let deleted_at = (document_state == "deleted").then(Utc::now);
    sqlx::query(
        "insert into content_document (
            id, workspace_id, library_id, external_key, document_state,
            source_file_name, deleted_at
         )
         select
            document_id, workspace_id, library_id, external_key,
            $2::content_document_state, file_name, $3
         from knowledge_document
         where document_id = $1
         on conflict (id) do update
         set document_state = excluded.document_state,
             deleted_at = excluded.deleted_at",
    )
    .bind(document_id)
    .bind(document_state)
    .bind(deleted_at)
    .execute(&fixture.state.persistence.postgres)
    .await
    .context("failed to publish authoritative content document")?;
    let revision_ids: Vec<Uuid> =
        sqlx::query_scalar("select revision_id from knowledge_revision where document_id=$1")
            .bind(document_id)
            .fetch_all(&fixture.state.persistence.postgres)
            .await?;
    for revision_id in revision_ids {
        let admission = crate::content_repository::imported_knowledge_revision_admission(
            &fixture.state.persistence.postgres,
            revision_id,
            "knowledge-lifecycle-source.txt",
        )
        .await?;
        sqlx::query(
            "insert into content_revision (
            id, document_id, workspace_id, library_id, revision_number,
            content_source_kind, checksum, mime_type, byte_size, title,
            source_uri, storage_key, document_hint, admission_identity
         )
         select
            revision_id, document_id, workspace_id, library_id, revision_number,
            revision_kind::content_source_kind, checksum, mime_type, byte_size, title,
            source_uri, storage_ref, document_hint, $2
         from knowledge_revision
         where revision_id = $1
         on conflict (id) do nothing",
        )
        .bind(revision_id)
        .bind(admission)
        .execute(&fixture.state.persistence.postgres)
        .await
        .context("failed to publish authoritative content revisions")?;
    }
    sqlx::query("update content_document set revision_sequence = greatest(revision_sequence, (select coalesce(max(revision_number),0) from content_revision where document_id=$1)) where id=$1")
        .bind(document_id).execute(&fixture.state.persistence.postgres).await?;
    sqlx::query(
        "insert into content_document_head (
            document_id, active_revision_id, readable_revision_id
         ) values ($1, $2, $3)
         on conflict (document_id) do update
         set active_revision_id = excluded.active_revision_id,
              readable_revision_id = excluded.readable_revision_id,
             head_updated_at = now()",
    )
    .bind(document_id)
    .bind(active_revision_id)
    .bind(readable_revision_id)
    .execute(&fixture.state.persistence.postgres)
    .await
    .context("failed to publish authoritative content head")?;
    Ok(())
}

async fn create_sweep_candidate_mutation(
    fixture: &KnowledgeLifecycleFixture,
    document_id: Uuid,
    revision_id: Uuid,
) -> Result<Uuid> {
    use ironrag_backend::infra::repositories::{
        content_mutation_repository, content_revision_repository,
    };
    let revision = content_revision_repository::get_revision_by_id(
        &fixture.state.persistence.postgres,
        revision_id,
    )
    .await?
    .context("sweep candidate must have a content revision")?;
    let replacement = ironrag_contracts::documents::ReplacementIdentity {
        checksum: revision.checksum,
        byte_size: revision.byte_size,
        mime_type: revision.mime_type,
        file_name: "sweep-candidate.txt".to_owned(),
        title: revision.title,
        language_code: revision.language_code,
        source_uri: revision.source_uri,
        document_hint: revision.document_hint,
    };
    let identity = serde_json::json!({"operationKind":"replace", "documentId":document_id,
        "sourceIdentity":null, "replacement":replacement});
    Ok(content_mutation_repository::create_mutation(
        &fixture.state.persistence.postgres,
        &content_mutation_repository::NewContentMutation {
            request_identity: &identity,
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            operation_kind: "replace",
            request_surface: "internal",
            requested_by_principal_id: None,
            idempotency_key: None,
            source_identity: None,
            mutation_state: "running",
            failure_code: None,
            conflict_code: None,
        },
    )
    .await?
    .id)
}

async fn assert_candidate_blocks_library_rebuild(
    fixture: &KnowledgeLifecycleFixture,
    document_id: Uuid,
    a_id: Uuid,
    a_chunk: Uuid,
    c_chunk: Uuid,
    profile: &str,
) -> Result<String> {
    let source_version: i64 =
        sqlx::query_scalar("select source_truth_version from catalog_library where id=$1")
            .bind(fixture.library_id)
            .fetch_one(&fixture.state.persistence.postgres)
            .await?;
    assert_candidate_blocks_global_reset(fixture, c_chunk, source_version).await?;
    assert_candidate_blocks_empty_purge(fixture, document_id, a_id, c_chunk, source_version)
        .await?;
    assert_candidate_blocks_staged_promotion(
        fixture,
        a_id,
        a_chunk,
        c_chunk,
        profile,
        source_version,
    )
    .await
}

async fn assert_candidate_blocks_global_reset(
    fixture: &KnowledgeLifecycleFixture,
    c_chunk: Uuid,
    source_version: i64,
) -> Result<()> {
    let pg = &fixture.state.persistence.postgres;
    let reset = ironrag_backend::infra::repositories::knowledge_graph_projection_repository::reset_library_materialized_graph_and_vectors(
        pg, fixture.library_id, source_version,
    ).await;
    let reset_error =
        reset.expect_err("global entity reset must preserve authoritative candidates");
    assert!(format!("{reset_error:#}").contains("authoritative ingest candidate"));
    assert_eq!(fixture.state.search_store.list_chunk_vectors_by_chunk(c_chunk).await?.len(), 1);
    Ok(())
}

async fn assert_candidate_blocks_empty_purge(
    fixture: &KnowledgeLifecycleFixture,
    document_id: Uuid,
    a_id: Uuid,
    c_chunk: Uuid,
    source_version: i64,
) -> Result<()> {
    let pg = &fixture.state.persistence.postgres;
    sqlx::query("update content_document_head set active_revision_id=null,readable_revision_id=null where document_id=$1")
        .bind(document_id).execute(pg).await?;
    sqlx::query("update knowledge_document set active_revision_id=null,readable_revision_id=null where document_id=$1")
        .bind(document_id).execute(pg).await?;
    let purge = fixture
        .state
        .search_store
        .purge_empty_library_vector_plane(fixture.library_id, source_version)
        .await;
    let purge_error = purge.expect_err("zero readable chunks must not authorize deleting leased C");
    assert!(purge_error.to_string().contains("authoritative ingest candidate"), "{purge_error:#}");
    assert_eq!(fixture.state.search_store.list_chunk_vectors_by_chunk(c_chunk).await?.len(), 1);
    sqlx::query("update content_document_head set active_revision_id=$2,readable_revision_id=$2 where document_id=$1")
        .bind(document_id).bind(a_id).execute(pg).await?;
    sqlx::query("update knowledge_document set active_revision_id=$2,readable_revision_id=$2 where document_id=$1")
        .bind(document_id).bind(a_id).execute(pg).await?;
    Ok(())
}

async fn assert_candidate_blocks_staged_promotion(
    fixture: &KnowledgeLifecycleFixture,
    a_id: Uuid,
    a_chunk: Uuid,
    c_chunk: Uuid,
    profile: &str,
    source_version: i64,
) -> Result<String> {
    use ironrag_backend::infra::knowledge_plane::{
        StagedVectorRebuildPromotion, VECTOR_REBUILD_STAGING_PROFILE_PREFIX,
    };
    let digest = format!("{0}{0}", Uuid::now_v7().simple());
    let staging =
        format!("{VECTOR_REBUILD_STAGING_PROFILE_PREFIX}{source_version:016x}{}", &digest[..48]);
    write_vector(fixture, a_chunk, a_id, &staging, vec![0.1, 0.2]).await?;
    let promotion = fixture
        .state
        .search_store
        .promote_staged_vector_rebuild(StagedVectorRebuildPromotion {
            library_id: fixture.library_id,
            dimensions: 2,
            active_embedding_profile_key: profile,
            staging_embedding_model_key: &staging,
            expected_source_truth_version: source_version,
            chunk_count: Some(1),
            entity_count: None,
        })
        .await;
    let promotion_error =
        promotion.expect_err("staging readable A must not authorize deleting leased C");
    assert!(
        promotion_error.to_string().contains("authoritative ingest candidate"),
        "{promotion_error:#}"
    );
    assert_eq!(fixture.state.search_store.list_chunk_vectors_by_chunk(c_chunk).await?.len(), 1);
    assert_eq!(chunk_vector_manifest_count(fixture, profile, 2).await?, 2);
    assert_eq!(chunk_vector_manifest_count(fixture, &staging, 2).await?, 1);
    Ok(staging)
}

async fn mark_structured_revision_prepared(
    fixture: &KnowledgeLifecycleFixture,
    document_id: Uuid,
    revision_id: Uuid,
    chunk_count: i64,
) -> Result<()> {
    sqlx::query(
        "insert into knowledge_structured_revision (
            revision_id, workspace_id, library_id, document_id,
            preparation_state, normalization_profile, source_format,
            block_count, chunk_count, typed_fact_count, outline_json,
            prepared_at, updated_at
         ) values (
            $1, $2, $3, $4,
            'prepared', 'knowledge-lifecycle-test-v1', 'text',
            0, $5, 0, '{}'::jsonb,
            now(), now()
         )
         on conflict (revision_id) do update
         set preparation_state = excluded.preparation_state,
             chunk_count = excluded.chunk_count,
             updated_at = excluded.updated_at",
    )
    .bind(revision_id)
    .bind(fixture.workspace_id)
    .bind(fixture.library_id)
    .bind(document_id)
    .bind(chunk_count)
    .execute(&fixture.state.persistence.postgres)
    .await
    .context("failed to mark structured revision prepared")?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn current_document_collections_exclude_both_half_deleted_shapes() -> Result<()> {
    let fixture = KnowledgeLifecycleFixture::create().await?;

    let result = async {
        let healthy_id = Uuid::now_v7();
        let timestamp_deleted_id = Uuid::now_v7();
        let state_deleted_id = Uuid::now_v7();
        for (document_id, label) in [
            (healthy_id, "healthy"),
            (timestamp_deleted_id, "timestamp-deleted"),
            (state_deleted_id, "state-deleted"),
        ] {
            fixture
                .state
                .application_services
                .knowledge
                .create_document_shell(
                    &fixture.state,
                    CreateKnowledgeDocumentCommand {
                        document_id,
                        workspace_id: fixture.workspace_id,
                        library_id: fixture.library_id,
                        external_key: format!("half-deleted-{label}-{document_id}"),
                        file_name: None,
                        title: Some(label.to_string()),
                        document_state: "active".to_string(),
                    },
                )
                .await
                .with_context(|| format!("failed to create {label} knowledge document"))?;
        }

        sqlx::query("update knowledge_document set deleted_at = now() where document_id = $1")
            .bind(timestamp_deleted_id)
            .execute(&fixture.state.persistence.postgres)
            .await
            .context("failed to plant active document with a deletion timestamp")?;
        sqlx::query(
            "update knowledge_document
             set document_state = 'deleted', deleted_at = null
             where document_id = $1",
        )
        .bind(state_deleted_id)
        .execute(&fixture.state.persistence.postgres)
        .await
        .context("failed to plant deleted document without a deletion timestamp")?;

        let by_ids = fixture
            .state
            .document_store
            .list_documents_by_ids(&[healthy_id, timestamp_deleted_id, state_deleted_id])
            .await
            .context("failed to list current documents by ids")?;
        assert_eq!(by_ids.iter().map(|row| row.document_id).collect::<Vec<_>>(), vec![healthy_id]);

        let current = fixture
            .state
            .document_store
            .list_documents_by_library(fixture.workspace_id, fixture.library_id, false)
            .await
            .context("failed to list current documents by library")?;
        assert_eq!(current.iter().map(|row| row.document_id).collect::<Vec<_>>(), vec![healthy_id]);

        let mut lifecycle_visible = fixture
            .state
            .document_store
            .list_documents_by_library(fixture.workspace_id, fixture.library_id, true)
            .await
            .context("failed to list lifecycle-visible documents")?
            .into_iter()
            .map(|row| row.document_id)
            .collect::<Vec<_>>();
        lifecycle_visible.sort_unstable();
        let mut expected = vec![healthy_id, timestamp_deleted_id, state_deleted_id];
        expected.sort_unstable();
        assert_eq!(lifecycle_visible, expected);

        assert!(
            fixture
                .state
                .document_store
                .get_document(timestamp_deleted_id)
                .await
                .context("failed to load divergent lifecycle row")?
                .is_some(),
            "repair paths must still be able to observe divergent rows",
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn authoritative_knowledge_lifecycle_persists_document_shell_revisions_pointers_and_chunks()
-> Result<()> {
    let fixture = KnowledgeLifecycleFixture::create().await?;

    let result = async {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let external_key = format!("knowledge-doc-{}", document_id.simple());

        fixture
            .state
            .application_services
            .knowledge
            .create_document_shell(
                &fixture.state,
                CreateKnowledgeDocumentCommand {
                    document_id,
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: external_key.clone(),
                    file_name: None,
                    title: Some("Knowledge Lifecycle Document".to_string()),
                    document_state: "active".to_string(),
                },
            )
            .await
            .context("failed to create knowledge document shell")?;

        fixture
            .state
            .application_services
            .knowledge
            .write_revision(
                &fixture.state,
                knowledge_revision_command(
                    &fixture,
                    KnowledgeRevisionSeed {
                        document_id,
                        revision_id,
                        revision_number: 1,
                        revision_kind: "upload",
                        title: "Initial Revision",
                        normalized_text: "Ada Lovelace wrote the analytical note.",
                    },
                ),
            )
            .await
            .context("failed to write knowledge revision")?;

        let chunk_a = write_chunk(
            &fixture,
            document_id,
            revision_id,
            0,
            "Ada Lovelace wrote the analytical note.",
        )
        .await?;
        let chunk_b =
            write_chunk(&fixture, document_id, revision_id, 1, "Charles Babbage built the engine.")
                .await?;
        mark_structured_revision_prepared(&fixture, document_id, revision_id, 2).await?;

        publish_authoritative_content_head(
            &fixture,
            document_id,
            "active",
            Some(revision_id),
            Some(revision_id),
        )
        .await?;

        fixture
            .state
            .application_services
            .knowledge
            .promote_document(&fixture.state, PromoteKnowledgeDocumentCommand { document_id })
            .await
            .context("failed to promote knowledge document")?;

        let document = fixture
            .state
            .document_store
            .get_document(document_id)
            .await
            .context("failed to reload knowledge document")?
            .context("knowledge document missing from store")?;
        assert_eq!(document.external_key, external_key);
        assert_eq!(document.document_state, "active");
        assert_eq!(document.active_revision_id, Some(revision_id));
        assert_eq!(document.readable_revision_id, Some(revision_id));
        assert_eq!(document.latest_revision_no, Some(1));

        let revisions = fixture
            .state
            .document_store
            .list_revisions_by_document(document_id)
            .await
            .context("failed to list knowledge revisions")?;
        assert_eq!(revisions.len(), 1);
        assert_eq!(revisions[0].revision_id, revision_id);
        assert_eq!(revisions[0].text_state, "text_readable");

        let chunks = fixture
            .state
            .document_store
            .list_chunks_by_revision(revision_id)
            .await
            .context("failed to list knowledge chunks")?;
        assert_eq!(
            chunks.iter().map(|row| row.chunk_id).collect::<Vec<_>>(),
            vec![chunk_a, chunk_b]
        );
        assert_eq!(chunks[0].chunk_index, 0);
        assert_eq!(chunks[1].chunk_index, 1);
        assert_eq!(chunks[0].document_id, document_id);
        assert_eq!(chunks[0].library_id, fixture.library_id);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn authoritative_knowledge_lifecycle_materializes_append_replace_and_delete_content_truth()
-> Result<()> {
    let fixture = KnowledgeLifecycleFixture::create().await?;

    let result = async {
        let document_id = Uuid::now_v7();
        let revision_one_id = Uuid::now_v7();
        let revision_two_id = Uuid::now_v7();
        let revision_three_id = Uuid::now_v7();

        fixture
            .state
            .application_services
            .knowledge
            .create_document_shell(
                &fixture.state,
                CreateKnowledgeDocumentCommand {
                    document_id,
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: format!("knowledge-mutation-doc-{}", document_id.simple()),
                    file_name: None,
                    title: Some("Knowledge Mutation Document".to_string()),
                    document_state: "active".to_string(),
                },
            )
            .await
            .context("failed to create mutation document shell")?;

        fixture
            .state
            .application_services
            .knowledge
            .write_revision(
                &fixture.state,
                knowledge_revision_command(
                    &fixture,
                    KnowledgeRevisionSeed {
                        document_id,
                        revision_id: revision_one_id,
                        revision_number: 1,
                        revision_kind: "upload",
                        title: "Base Revision",
                        normalized_text: "Base memory about the engine.",
                    },
                ),
            )
            .await
            .context("failed to write revision one")?;
        let revision_one_chunk_id =
            write_chunk(&fixture, document_id, revision_one_id, 0, "Base memory about the engine.")
                .await?;
        let embedding_profile_key = format!("embedding-profile:v1:{0}{0}", Uuid::now_v7().simple());
        fixture
            .state
            .search_store
            .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                chunk_id: revision_one_chunk_id,
                revision_id: revision_one_id,
                embedding_model_key: embedding_profile_key.clone(),
                vector_kind: "chunk_embedding".to_string(),
                dimensions: 2,
                vector: vec![0.1, 0.2],
                freshness_generation: 1,
                created_at: Utc::now(),
                occurred_at: None,
                occurred_until: None,
            })
            .await
            .context("failed to write revision one chunk vector")?;

        publish_authoritative_content_head(
            &fixture,
            document_id,
            "active",
            Some(revision_one_id),
            Some(revision_one_id),
        )
        .await?;

        fixture
            .state
            .application_services
            .knowledge
            .promote_document(&fixture.state, PromoteKnowledgeDocumentCommand { document_id })
            .await
            .context("failed to promote revision one")?;

        fixture
            .state
            .application_services
            .knowledge
            .write_revision(
                &fixture.state,
                knowledge_revision_command(
                    &fixture,
                    KnowledgeRevisionSeed {
                        document_id,
                        revision_id: revision_two_id,
                        revision_number: 2,
                        revision_kind: "append",
                        title: "Appended Revision",
                        normalized_text: "Base memory about the engine. Additional note about Ada.",
                    },
                ),
            )
            .await
            .context("failed to write revision two")?;
        write_chunk(
            &fixture,
            document_id,
            revision_two_id,
            0,
            "Base memory about the engine. Additional note about Ada.",
        )
        .await?;

        publish_authoritative_content_head(
            &fixture,
            document_id,
            "active",
            Some(revision_two_id),
            Some(revision_one_id),
        )
        .await?;

        fixture
            .state
            .application_services
            .knowledge
            .promote_document(&fixture.state, PromoteKnowledgeDocumentCommand { document_id })
            .await
            .context("failed to promote revision two with split readable pointer")?;

        let split_pointer_document = fixture
            .state
            .document_store
            .get_document(document_id)
            .await
            .context("failed to reload split pointer document")?
            .context("split pointer document missing")?;
        assert_eq!(split_pointer_document.active_revision_id, Some(revision_two_id));
        assert_eq!(split_pointer_document.readable_revision_id, Some(revision_one_id));
        assert_eq!(split_pointer_document.latest_revision_no, Some(2));

        fixture
            .state
            .application_services
            .knowledge
            .write_revision(
                &fixture.state,
                knowledge_revision_command(
                    &fixture,
                    KnowledgeRevisionSeed {
                        document_id,
                        revision_id: revision_three_id,
                        revision_number: 3,
                        revision_kind: "replace",
                        title: "Replacement Revision",
                        normalized_text: "Replacement memory about the analytical engine only.",
                    },
                ),
            )
            .await
            .context("failed to write revision three")?;
        write_chunk(
            &fixture,
            document_id,
            revision_three_id,
            0,
            "Replacement memory about the analytical engine only.",
        )
        .await?;

        let deleted_chunks = fixture
            .state
            .application_services
            .knowledge
            .delete_revision_chunks(&fixture.state, revision_one_id)
            .await
            .context("failed to delete superseded revision one chunks")?;
        assert_eq!(deleted_chunks.len(), 1);
        assert!(
            fixture
                .state
                .document_store
                .list_chunks_by_revision(revision_one_id)
                .await
                .context("failed to re-list revision one chunks")?
                .is_empty()
        );
        let revision_one_vectors = fixture
            .state
            .search_store
            .count_chunk_vectors_by_revision(
                revision_one_id,
                &embedding_profile_key,
                "chunk_embedding",
                1,
            )
            .await
            .context("failed to count revision one chunk vectors after delete")?;
        assert_eq!(revision_one_vectors, 0);

        publish_authoritative_content_head(
            &fixture,
            document_id,
            "deleted",
            None,
            Some(revision_three_id),
        )
        .await?;

        fixture
            .state
            .application_services
            .knowledge
            .promote_document(&fixture.state, PromoteKnowledgeDocumentCommand { document_id })
            .await
            .context("failed to tombstone knowledge document")?;

        let tombstoned = fixture
            .state
            .document_store
            .get_document(document_id)
            .await
            .context("failed to reload tombstoned document")?
            .context("tombstoned document missing")?;
        assert_eq!(tombstoned.document_state, "deleted");
        assert_eq!(tombstoned.active_revision_id, None);
        assert_eq!(tombstoned.readable_revision_id, Some(revision_three_id));
        assert_eq!(tombstoned.latest_revision_no, Some(3));
        assert!(tombstoned.deleted_at.is_some());

        let revisions = fixture
            .state
            .document_store
            .list_revisions_by_document(document_id)
            .await
            .context("failed to list mutation revisions")?;
        assert_eq!(
            revisions.iter().map(|row| row.revision_number).collect::<Vec<_>>(),
            vec![3, 2, 1]
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn deleted_document_revision_cleanup_is_not_capped_at_one_hundred_revisions() -> Result<()> {
    let fixture = KnowledgeLifecycleFixture::create().await?;

    let result = async {
        let document_id = Uuid::now_v7();
        let external_key = format!("knowledge-revision-cap-doc-{}", document_id.simple());
        let embedding_profile_key = format!("embedding-profile:v1:{0}{0}", Uuid::now_v7().simple());
        let mut oldest_revision_id = None;
        let mut oldest_chunk_id = None;

        fixture
            .state
            .application_services
            .knowledge
            .create_document_shell(
                &fixture.state,
                CreateKnowledgeDocumentCommand {
                    document_id,
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key,
                    file_name: None,
                    title: Some("Knowledge Revision Cap Document".to_string()),
                    document_state: "active".to_string(),
                },
            )
            .await
            .context("failed to create revision cap document shell")?;

        for revision_number in 1..=101 {
            let revision_id = Uuid::now_v7();
            fixture
                .state
                .application_services
                .knowledge
                .write_revision(
                    &fixture.state,
                    knowledge_revision_command(
                        &fixture,
                        KnowledgeRevisionSeed {
                            document_id,
                            revision_id,
                            revision_number,
                            revision_kind: "replace",
                            title: &format!("Revision {revision_number}"),
                            normalized_text: &format!(
                                "Revision {revision_number} synthetic cleanup text."
                            ),
                        },
                    ),
                )
                .await
                .with_context(|| format!("failed to write revision {revision_number}"))?;
            let chunk_id = write_chunk(
                &fixture,
                document_id,
                revision_id,
                0,
                &format!("Revision {revision_number} synthetic cleanup text."),
            )
            .await?;
            if revision_number == 1 {
                oldest_revision_id = Some(revision_id);
                oldest_chunk_id = Some(chunk_id);
                fixture
                    .state
                    .search_store
                    .upsert_chunk_vector(&KnowledgeChunkVectorRow {
                        vector_id: Uuid::now_v7(),
                        workspace_id: fixture.workspace_id,
                        library_id: fixture.library_id,
                        chunk_id,
                        revision_id,
                        embedding_model_key: embedding_profile_key.clone(),
                        vector_kind: "chunk_embedding".to_string(),
                        dimensions: 2,
                        vector: vec![0.1, 0.2],
                        freshness_generation: 1,
                        created_at: Utc::now(),
                        occurred_at: None,
                        occurred_until: None,
                    })
                    .await
                    .context("failed to write oldest revision vector")?;
            }
        }

        let revisions = fixture
            .state
            .document_store
            .list_revisions_by_document(document_id)
            .await
            .context("failed to list all document revisions")?;
        assert_eq!(revisions.len(), 101);
        assert_eq!(revisions.first().map(|row| row.revision_number), Some(101));
        assert_eq!(revisions.last().map(|row| row.revision_number), Some(1));

        for revision in &revisions {
            fixture
                .state
                .application_services
                .knowledge
                .delete_revision_chunks(&fixture.state, revision.revision_id)
                .await
                .with_context(|| {
                    format!("failed to delete chunks for revision {}", revision.revision_number)
                })?;
        }

        let oldest_revision_id = oldest_revision_id.context("oldest revision id missing")?;
        let oldest_chunk_id = oldest_chunk_id.context("oldest chunk id missing")?;
        assert!(
            fixture
                .state
                .document_store
                .list_chunks_by_revision(oldest_revision_id)
                .await
                .context("failed to re-list oldest revision chunks")?
                .is_empty()
        );
        assert!(
            fixture
                .state
                .search_store
                .list_chunk_vectors_by_chunk(oldest_chunk_id)
                .await
                .context("failed to list oldest revision vectors")?
                .is_empty()
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn knowledge_projection_preserves_authoritative_split_readiness_pointers() -> Result<()> {
    let fixture = KnowledgeLifecycleFixture::create().await?;

    let result = async {
        let document_id = Uuid::now_v7();
        let readable_revision_id = Uuid::now_v7();
        let active_revision_id = Uuid::now_v7();

        fixture
            .state
            .application_services
            .knowledge
            .create_document_shell(
                &fixture.state,
                CreateKnowledgeDocumentCommand {
                    document_id,
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: format!("readiness-doc-{}", document_id.simple()),
                    file_name: None,
                    title: Some("Readiness Coherence Document".to_string()),
                    document_state: "active".to_string(),
                },
            )
            .await
            .context("failed to create readiness coherence document shell")?;

        fixture
            .state
            .application_services
            .knowledge
            .write_revision(
                &fixture.state,
                knowledge_revision_command(
                    &fixture,
                    KnowledgeRevisionSeed {
                        document_id,
                        revision_id: readable_revision_id,
                        revision_number: 1,
                        revision_kind: "upload",
                        title: "Readable Baseline",
                        normalized_text: "Readable baseline memory for coherence checks.",
                    },
                ),
            )
            .await
            .context("failed to write readable baseline revision")?;
        write_chunk(
            &fixture,
            document_id,
            readable_revision_id,
            0,
            "Readable baseline memory for coherence checks.",
        )
        .await?;
        mark_structured_revision_prepared(&fixture, document_id, readable_revision_id, 1).await?;

        let mut active_revision = knowledge_revision_command(
            &fixture,
            KnowledgeRevisionSeed {
                document_id,
                revision_id: active_revision_id,
                revision_number: 2,
                revision_kind: "append",
                title: "Active But Not Readable",
                normalized_text: "Active revision is newer but still processing downstream readiness.",
            },
        );
        active_revision.vector_state = "accepted".to_string();
        active_revision.graph_state = "accepted".to_string();
        active_revision.vector_ready_at = None;
        active_revision.graph_ready_at = None;

        fixture
            .state
            .application_services
            .knowledge
            .write_revision(&fixture.state, active_revision)
            .await
            .context("failed to write active non-ready revision")?;
        write_chunk(
            &fixture,
            document_id,
            active_revision_id,
            0,
            "Active revision is newer but still processing downstream readiness.",
        )
        .await?;
        mark_structured_revision_prepared(&fixture, document_id, active_revision_id, 1).await?;

        publish_authoritative_content_head(
            &fixture,
            document_id,
            "active",
            Some(active_revision_id),
            Some(readable_revision_id),
        )
        .await?;

        fixture
            .state
            .application_services
            .knowledge
            .promote_document(&fixture.state, PromoteKnowledgeDocumentCommand { document_id })
            .await
            .context("failed to promote split readiness pointers")?;

        let document = fixture
            .state
            .document_store
            .get_document(document_id)
            .await
            .context("failed to reload readiness coherence document")?
            .context("readiness coherence document missing")?;
        assert_eq!(document.active_revision_id, Some(active_revision_id));
        assert_eq!(document.readable_revision_id, Some(readable_revision_id));
        assert_ne!(document.active_revision_id, document.readable_revision_id);
        assert_eq!(document.latest_revision_no, Some(2));

        let readable_chunks = fixture
            .state
            .document_store
            .list_chunks_by_revision(readable_revision_id)
            .await
            .context("failed to list readable baseline chunks")?;
        let active_chunks = fixture
            .state
            .document_store
            .list_chunks_by_revision(active_revision_id)
            .await
            .context("failed to list active processing chunks")?;
        assert!(!readable_chunks.is_empty());
        assert!(!active_chunks.is_empty());
        assert!(readable_chunks.iter().all(|chunk| chunk.chunk_state == "ready"));
        assert!(active_chunks.iter().all(|chunk| chunk.chunk_state == "ready"));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn projection_purge_reconciles_chunk_vector_manifest_atomically() -> Result<()> {
    let fixture = KnowledgeLifecycleFixture::create().await?;

    let result = async {
        let embedding_profile_key = format!("embedding-profile:v1:{0}{0}", Uuid::now_v7().simple());
        let mut seeded = Vec::new();
        for ordinal in 1..=2 {
            let document_id = Uuid::now_v7();
            let revision_id = Uuid::now_v7();
            fixture
                .state
                .application_services
                .knowledge
                .create_document_shell(
                    &fixture.state,
                    CreateKnowledgeDocumentCommand {
                        document_id,
                        workspace_id: fixture.workspace_id,
                        library_id: fixture.library_id,
                        external_key: format!(
                            "projection-purge-manifest-{ordinal}-{}",
                            document_id.simple()
                        ),
                        file_name: None,
                        title: Some(format!("Projection purge manifest document {ordinal}")),
                        document_state: "active".to_string(),
                    },
                )
                .await?;
            fixture
                .state
                .application_services
                .knowledge
                .write_revision(
                    &fixture.state,
                    knowledge_revision_command(
                        &fixture,
                        KnowledgeRevisionSeed {
                            document_id,
                            revision_id,
                            revision_number: 1,
                            revision_kind: "upload",
                            title: "Projection purge manifest revision",
                            normalized_text: "Projection purge manifest evidence.",
                        },
                    ),
                )
                .await?;
            let chunk_id = write_chunk(
                &fixture,
                document_id,
                revision_id,
                0,
                "Projection purge manifest evidence.",
            )
            .await?;
            write_vector(&fixture, chunk_id, revision_id, &embedding_profile_key, vec![0.1, 0.2])
                .await?;
            seeded.push((document_id, chunk_id, revision_id));
        }

        let embedding_profile_key_d3 =
            format!("embedding-profile:v1:{0}{0}", Uuid::now_v7().simple());
        write_vector(
            &fixture,
            seeded[0].1,
            seeded[0].2,
            &embedding_profile_key_d3,
            vec![0.1, 0.2, 0.3],
        )
        .await?;
        write_vector(
            &fixture,
            seeded[1].1,
            seeded[1].2,
            &embedding_profile_key_d3,
            vec![0.3, 0.2, 0.1],
        )
        .await?;

        assert_eq!(chunk_vector_manifest_count(&fixture, &embedding_profile_key, 2).await?, 2);
        assert_eq!(chunk_vector_manifest_count(&fixture, &embedding_profile_key_d3, 3).await?, 2);
        let rebuild_source_truth_version = catalog_repository::get_library_source_truth_version(
            &fixture.state.persistence.postgres,
            fixture.library_id,
        )
        .await?;
        let staging_embedding_profile_key =
            format!("embedding-rebuild:v1:{rebuild_source_truth_version:016x}{}", "a".repeat(48));
        fixture
            .state
            .search_store
            .prepare_chunk_vector_rebuild_lane(
                fixture.library_id,
                2,
                &staging_embedding_profile_key,
            )
            .await?;
        fixture
            .state
            .search_store
            .upsert_chunk_vectors_bulk_deferred_manifest(&[KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                chunk_id: seeded[0].1,
                revision_id: seeded[0].2,
                embedding_model_key: staging_embedding_profile_key.clone(),
                vector_kind: "chunk_embedding".to_string(),
                dimensions: 2,
                vector: vec![0.2, 0.1],
                freshness_generation: 1,
                created_at: Utc::now(),
                occurred_at: None,
                occurred_until: None,
            }])
            .await?;
        let missing_manifest_profile_key =
            format!("embedding-profile:v1:{0}{0}", Uuid::now_v7().simple());
        let orphan_inserted = sqlx::query(
            "insert into knowledge_chunk_vector_d2 (
                 key, vector_id, workspace_id, library_id, chunk_id, revision_id,
                 embedding_model_key, vector_kind, dimensions, embedding,
                 freshness_generation, created_at
             ) values ($1, $2, $3, $4, $5, $6, $7, 'chunk_embedding', 2, '[0.2,0.1]', 1, now())",
        )
        .bind(format!("missing-manifest-{}", Uuid::now_v7()))
        .bind(Uuid::now_v7())
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind(seeded[0].1)
        .bind(seeded[0].2)
        .bind(&missing_manifest_profile_key)
        .execute(&fixture.state.persistence.postgres)
        .await?
        .rows_affected();
        assert_eq!(orphan_inserted, 1);

        let corrupted = sqlx::query(
            "update knowledge_vector_relation_manifest
             set row_count = 1
             where library_id = $1
               and relation_name = 'knowledge_chunk_vector_d3'
               and embedding_model_key = $2
               and vector_kind = 'chunk_embedding'",
        )
        .bind(fixture.library_id)
        .bind(&embedding_profile_key_d3)
        .execute(&fixture.state.persistence.postgres)
        .await?
        .rows_affected();
        assert_eq!(corrupted, 1, "the under-counted manifest test plant must land exactly once");
        let mut rejected_transaction = fixture.state.persistence.postgres.begin().await?;
        let locked = catalog_repository::lock_library_for_lifecycle_event_with_executor(
            &mut *rejected_transaction,
            fixture.workspace_id,
            fixture.library_id,
        )
        .await?;
        assert!(locked);
        let rejected_error =
            content_knowledge_projection_repository::purge_knowledge_projection_with_transaction(
                &mut rejected_transaction,
                fixture.library_id,
                &[seeded[0].0],
            )
            .await
            .expect_err("an under-counted manifest must reject projection purge");
        assert!(
            format!("{rejected_error:#}").contains("active vector manifest was not exact"),
            "the exact positive-undercount guard must be the rejecting failure: {rejected_error:#}"
        );
        rejected_transaction.rollback().await?;
        assert_eq!(
            fixture.state.search_store.list_chunk_vectors_by_chunk(seeded[0].1).await?.len(),
            4,
            "a later relation failure must roll back every earlier physical vector delete",
        );
        assert_eq!(
            fixture.state.search_store.list_chunk_vectors_by_chunk(seeded[1].1).await?.len(),
            2,
            "rejected purge must preserve unrelated vectors",
        );

        let restored = sqlx::query(
            "update knowledge_vector_relation_manifest
             set row_count = 2
             where library_id = $1
               and relation_name = 'knowledge_chunk_vector_d3'
               and embedding_model_key = $2
               and vector_kind = 'chunk_embedding'",
        )
        .bind(fixture.library_id)
        .bind(&embedding_profile_key_d3)
        .execute(&fixture.state.persistence.postgres)
        .await?
        .rows_affected();
        assert_eq!(restored, 1);

        let mut transaction = fixture.state.persistence.postgres.begin().await?;
        let locked = catalog_repository::lock_library_for_lifecycle_event_with_executor(
            &mut *transaction,
            fixture.workspace_id,
            fixture.library_id,
        )
        .await?;
        assert!(locked);
        let purged =
            content_knowledge_projection_repository::purge_knowledge_projection_with_transaction(
                &mut transaction,
                fixture.library_id,
                &[seeded[0].0],
            )
            .await?;
        transaction.commit().await?;

        assert_eq!(purged.documents, 1);
        assert_eq!(purged.chunk_vector_rows, 4);
        assert!(
            fixture.state.search_store.list_chunk_vectors_by_chunk(seeded[0].1).await?.is_empty(),
            "purged document vector must be removed",
        );
        assert_eq!(
            fixture.state.search_store.list_chunk_vectors_by_chunk(seeded[1].1).await?.len(),
            2,
            "sibling vector in the same physical relation must survive",
        );
        assert_eq!(chunk_vector_manifest_count(&fixture, &embedding_profile_key, 2).await?, 1);
        assert_eq!(chunk_vector_manifest_count(&fixture, &embedding_profile_key_d3, 3).await?, 1);
        assert_eq!(
            chunk_vector_manifest_count(&fixture, &staging_embedding_profile_key, 2).await?,
            0,
            "a provisional staging manifest may remain zero while its physical row is purged",
        );

        let stale_rebuild_write = fixture
            .state
            .search_store
            .upsert_chunk_vectors_bulk_deferred_manifest(&[KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                chunk_id: seeded[0].1,
                revision_id: seeded[0].2,
                embedding_model_key: staging_embedding_profile_key,
                vector_kind: "chunk_embedding".to_string(),
                dimensions: 2,
                vector: vec![0.2, 0.1],
                freshness_generation: 1,
                created_at: Utc::now(),
                occurred_at: None,
                occurred_until: None,
            }])
            .await;
        assert!(
            stale_rebuild_write.is_err(),
            "a rebuild batch must not resurrect a vector after its source chunk was purged",
        );
        assert!(
            fixture.state.search_store.list_chunk_vectors_by_chunk(seeded[0].1).await?.is_empty(),
            "rejected rebuild write must leave no physical orphan",
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn postgres_gc_reclaims_vectors_of_soft_deleted_documents() -> Result<()> {
    let fixture = KnowledgeLifecycleFixture::create().await?;

    let result = async {
        let profile = format!("embedding-profile:v1:{0}{0}", Uuid::now_v7().simple());
        // One live document whose readable-revision vectors must survive, and
        // one soft-deleted document whose vectors the sweep must reclaim even
        // though the delete kept its readable head.
        let mut seeded = Vec::new();
        for (label, deleted) in [("live", false), ("deleted", true)] {
            let document_id = Uuid::now_v7();
            let revision_id = Uuid::now_v7();
            let chunk_id = Uuid::now_v7();
            fixture
                .state
                .application_services
                .knowledge
                .create_document_shell(
                    &fixture.state,
                    CreateKnowledgeDocumentCommand {
                        document_id,
                        workspace_id: fixture.workspace_id,
                        library_id: fixture.library_id,
                        external_key: format!("vector-gc-{label}-{}", document_id.simple()),
                        file_name: None,
                        title: Some(format!("Vector GC {label} document")),
                        document_state: "active".to_string(),
                    },
                )
                .await?;
            fixture
                .state
                .application_services
                .knowledge
                .write_revision(
                    &fixture.state,
                    knowledge_revision_command(
                        &fixture,
                        KnowledgeRevisionSeed {
                            document_id,
                            revision_id,
                            revision_number: 1,
                            revision_kind: "upload",
                            title: label,
                            normalized_text: label,
                        },
                    ),
                )
                .await?;
            sqlx::query(
                "insert into knowledge_chunk (
                    chunk_id, workspace_id, library_id, document_id, revision_id,
                    chunk_index, content_text, normalized_text, chunk_state
                 ) values ($1, $2, $3, $4, $5, 0, $6, $6, 'ready')",
            )
            .bind(chunk_id)
            .bind(fixture.workspace_id)
            .bind(fixture.library_id)
            .bind(document_id)
            .bind(revision_id)
            .bind(label)
            .execute(&fixture.state.persistence.postgres)
            .await?;
            write_vector(&fixture, chunk_id, revision_id, &profile, vec![0.1, 0.2, 0.3]).await?;
            let document_state = if deleted { "deleted" } else { "active" };
            let active_revision_id = if deleted { None } else { Some(revision_id) };
            publish_authoritative_content_head(
                &fixture,
                document_id,
                document_state,
                active_revision_id,
                Some(revision_id),
            )
            .await?;
            sqlx::query(
                "update knowledge_document
                 set document_state = $2,
                     active_revision_id = $3,
                     readable_revision_id = $4,
                     deleted_at = case when $2 = 'deleted' then now() else null end,
                     updated_at = now()
                 where document_id = $1",
            )
            .bind(document_id)
            .bind(document_state)
            .bind(active_revision_id)
            .bind(revision_id)
            .execute(&fixture.state.persistence.postgres)
            .await?;
            seeded.push((label, chunk_id));
        }

        let report = maintenance_gc::run_superseded_vectors_for_library(
            &fixture.state,
            fixture.workspace_id,
            fixture.library_id,
            GcSupersededVectorsOptions {
                max_batches: None,
                ..GcSupersededVectorsOptions::default()
            },
        )
        .await?;
        assert_eq!(
            report.superseded_vectors_removed, 1,
            "only the deleted document's chunk vector is superseded"
        );

        for (label, chunk_id) in &seeded {
            let remaining =
                fixture.state.search_store.list_chunk_vectors_by_chunk(*chunk_id).await?;
            if *label == "live" {
                assert_eq!(remaining.len(), 1, "a live document's readable vectors must survive");
            } else {
                assert_eq!(remaining.len(), 0, "a deleted document's vectors must be swept");
            }
        }
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

/// IRG-217: a refused inline revision-chunk vector delete must remain a loud,
/// structured operator signal while the accepted document delete still
/// succeeds, and the periodic GC sweep must stay the durable backstop that
/// reclaims the vectors.
///
/// Drives the real product path (`ContentService::delete_document` ->
/// `refresh_deleted_document_graph_state` ->
/// `cleanup_revision_local_graph_artifacts` ->
/// `KnowledgeService::delete_revision_chunks` -> the fenced vector delete) and
/// refuses the vector delete at the store boundary through the IRG-217 test
/// seam. No production `DELETE`, source-truth touch, or unrelated mutation is
/// involved.
#[cfg(feature = "test-support")]
#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn refused_inline_revision_vector_delete_is_observable_and_gc_backstopped() -> Result<()> {
    use std::sync::{Arc, Mutex};

    use ironrag_backend::shared::cancellation::revision_chunk_vector_delete_fault;
    use tracing::field::{Field, Visit};
    use tracing::{Event, Level, Subscriber};
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::{Context, SubscriberExt as _};

    #[derive(Clone, Debug, Default)]
    struct CapturedEvent {
        level: Option<Level>,
        message: Option<String>,
        fields: Vec<(String, String)>,
    }

    #[derive(Default)]
    struct FieldVisitor {
        message: Option<String>,
        fields: Vec<(String, String)>,
    }

    impl FieldVisitor {
        fn record(&mut self, field: &Field, value: String) {
            if field.name() == "message" {
                self.message = Some(value);
            } else {
                self.fields.push((field.name().to_string(), value));
            }
        }
    }

    impl Visit for FieldVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.record(field, format!("{value:?}"));
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            self.record(field, value.to_string());
        }
    }

    struct EventCollector {
        events: Arc<Mutex<Vec<CapturedEvent>>>,
    }

    impl<S: Subscriber> Layer<S> for EventCollector {
        fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
            let mut visitor = FieldVisitor::default();
            event.record(&mut visitor);
            self.events.lock().expect("event capture mutex poisoned").push(CapturedEvent {
                level: Some(*event.metadata().level()),
                message: visitor.message,
                fields: visitor.fields,
            });
        }
    }

    let fixture = KnowledgeLifecycleFixture::create().await?;
    let result = async {
        let profile = format!("embedding-profile:v1:{0}{0}", Uuid::now_v7().simple());
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();

        fixture
            .state
            .application_services
            .knowledge
            .create_document_shell(
                &fixture.state,
                CreateKnowledgeDocumentCommand {
                    document_id,
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: format!("vector-refusal-doc-{}", document_id.simple()),
                    file_name: Some("vector-refusal.txt".to_string()),
                    title: Some("Inline vector delete refusal document".to_string()),
                    document_state: "active".to_string(),
                },
            )
            .await?;
        fixture
            .state
            .application_services
            .knowledge
            .write_revision(
                &fixture.state,
                knowledge_revision_command(
                    &fixture,
                    KnowledgeRevisionSeed {
                        document_id,
                        revision_id,
                        revision_number: 1,
                        revision_kind: "upload",
                        title: "Inline vector delete refusal revision",
                        normalized_text: "inline vector delete refusal revision",
                    },
                ),
            )
            .await?;
        let chunk_id = write_chunk(
            &fixture,
            document_id,
            revision_id,
            0,
            "inline vector delete refusal revision",
        )
        .await?;
        write_vector(&fixture, chunk_id, revision_id, &profile, vec![0.1, 0.2, 0.3]).await?;
        publish_authoritative_content_head(
            &fixture,
            document_id,
            "active",
            Some(revision_id),
            Some(revision_id),
        )
        .await?;
        sqlx::query(
            "update knowledge_document
             set document_state = 'active',
                 active_revision_id = $2,
                 readable_revision_id = $2,
                 updated_at = now()
             where document_id = $1",
        )
        .bind(document_id)
        .bind(revision_id)
        .execute(&fixture.state.persistence.postgres)
        .await?;

        let captured = Arc::new(Mutex::new(Vec::<CapturedEvent>::new()));
        let subscriber =
            tracing_subscriber::registry().with(EventCollector { events: Arc::clone(&captured) });
        let guard = tracing::subscriber::set_default(subscriber);

        revision_chunk_vector_delete_fault::install(revision_id);
        let deleted = fixture
            .state
            .application_services
            .content
            .delete_document(&fixture.state, document_id)
            .await
            .context("the accepted document delete must not fail on an inline cleanup refusal")?;
        drop(guard);

        assert_eq!(
            deleted.document_state, "deleted",
            "the accepted delete must still commit despite the refused inline cleanup",
        );

        let events = captured.lock().expect("event capture mutex poisoned").clone();
        let signal = events
            .iter()
            .find(|event| {
                event.message.as_deref()
                    == Some("failed to delete chunks and vectors for deleted document")
            })
            .context("the inline vector-delete refusal must be logged")?;
        assert_eq!(
            signal.level,
            Some(Level::ERROR),
            "an inline cleanup refusal must be an ERROR signal, not a routine warning",
        );
        let field = |name: &str| {
            signal.fields.iter().find(|(key, _)| key == name).map(|(_, value)| value.clone())
        };
        let document_id_text = document_id.to_string();
        let revision_id_text = revision_id.to_string();
        assert_eq!(
            field("document_id").as_deref(),
            Some(document_id_text.as_str()),
            "the signal must identify the deleted document",
        );
        assert_eq!(
            field("revision_id").as_deref(),
            Some(revision_id_text.as_str()),
            "the signal must identify the refused revision",
        );
        assert!(field("e").is_some(), "the signal must carry the refused error object");
        assert!(
            events
                .iter()
                .flat_map(|event| &event.fields)
                .any(|(_, value)| value
                    .contains("injected destructive revision vector delete refusal")),
            "the logged error chain must preserve the refusal reason, got: {events:?}",
        );

        let retained = fixture.state.search_store.list_chunk_vectors_by_chunk(chunk_id).await?;
        assert_eq!(
            retained.len(),
            1,
            "the refused inline delete must leave the revision's vectors for the backstop",
        );
        let report = maintenance_gc::run_superseded_vectors_for_library(
            &fixture.state,
            fixture.workspace_id,
            fixture.library_id,
            GcSupersededVectorsOptions {
                max_batches: None,
                ..GcSupersededVectorsOptions::default()
            },
        )
        .await
        .context("the GC backstop must still run after an inline cleanup refusal")?;
        assert_eq!(
            report.superseded_vectors_removed, 1,
            "the periodic sweep must reclaim the vectors the inline delete refused",
        );
        assert!(
            fixture.state.search_store.list_chunk_vectors_by_chunk(chunk_id).await?.is_empty(),
            "the backstop must leave no vector for the deleted document",
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

/// Seeds a document whose knowledge head diverges from its live content head,
/// with a ready chunk and one chunk vector on the readable revision, then marks
/// the knowledge row deleted. `document_state` selects the half: `"deleted"`
/// for the state tombstone, `"active"` for the timestamp-only half
/// (`deleted_at is not null`, `document_state <> 'deleted'`). Returns the chunk
/// whose vector the sweep must reclaim.
async fn seed_diverged_head_vector_document(
    fixture: &KnowledgeLifecycleFixture,
    external_prefix: &str,
    title: &str,
    document_state: &str,
) -> Result<Uuid> {
    let profile = format!("embedding-profile:v1:{0}{0}", Uuid::now_v7().simple());
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let chunk_id = Uuid::now_v7();
    fixture
        .state
        .application_services
        .knowledge
        .create_document_shell(
            &fixture.state,
            CreateKnowledgeDocumentCommand {
                document_id,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                external_key: format!("{external_prefix}-{}", document_id.simple()),
                file_name: None,
                title: Some(title.to_string()),
                document_state: "active".to_string(),
            },
        )
        .await?;
    fixture
        .state
        .application_services
        .knowledge
        .write_revision(
            &fixture.state,
            knowledge_revision_command(
                fixture,
                KnowledgeRevisionSeed {
                    document_id,
                    revision_id,
                    revision_number: 1,
                    revision_kind: "upload",
                    title: "diverged",
                    normalized_text: "diverged",
                },
            ),
        )
        .await?;
    sqlx::query(
        "insert into knowledge_chunk (
            chunk_id, workspace_id, library_id, document_id, revision_id,
            chunk_index, content_text, normalized_text, chunk_state
         ) values ($1, $2, $3, $4, $5, 0, $6, $6, 'ready')",
    )
    .bind(chunk_id)
    .bind(fixture.workspace_id)
    .bind(fixture.library_id)
    .bind(document_id)
    .bind(revision_id)
    .bind("diverged")
    .execute(&fixture.state.persistence.postgres)
    .await?;
    write_vector(fixture, chunk_id, revision_id, &profile, vec![0.1, 0.2, 0.3]).await?;
    // The content plane still owns a live head while the knowledge plane marks
    // the document deleted: the divergence the delete path legitimately leaves
    // and, for the timestamp-only half, what stalled the sweep before the page
    // classification and the head guard shared one deletion predicate.
    publish_authoritative_content_head(
        fixture,
        document_id,
        "active",
        Some(revision_id),
        Some(revision_id),
    )
    .await?;
    sqlx::query(
        "update knowledge_document
         set document_state = $3,
             active_revision_id = null,
             readable_revision_id = $2,
             deleted_at = now(),
             updated_at = now()
         where document_id = $1",
    )
    .bind(document_id)
    .bind(revision_id)
    .bind(document_state)
    .execute(&fixture.state.persistence.postgres)
    .await?;
    Ok(chunk_id)
}

/// Runs one scheduler-shaped sweep batch and asserts the diverged-head deleted
/// document's vectors were reclaimed instead of stalling the library.
async fn assert_diverged_head_vectors_swept(
    fixture: &KnowledgeLifecycleFixture,
    external_prefix: &str,
    title: &str,
    document_state: &str,
) -> Result<()> {
    let chunk_id =
        seed_diverged_head_vector_document(fixture, external_prefix, title, document_state).await?;
    let report = maintenance_gc::run_superseded_vectors_for_library(
        &fixture.state,
        fixture.workspace_id,
        fixture.library_id,
        GcSupersededVectorsOptions {
            max_batches: Some(1),
            ..GcSupersededVectorsOptions::default()
        },
    )
    .await
    .context("a deleted document with a diverged head must not stall the sweep")?;
    assert_eq!(report.superseded_vectors_removed, 1);
    assert!(
        fixture.state.search_store.list_chunk_vectors_by_chunk(chunk_id).await?.is_empty(),
        "the deleted document's vectors must be swept despite the head divergence",
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn postgres_gc_reclaims_deleted_vectors_when_heads_diverged() -> Result<()> {
    let fixture = KnowledgeLifecycleFixture::create().await?;
    let result = assert_diverged_head_vectors_swept(
        &fixture,
        "vector-gc-diverged",
        "Vector GC diverged-head document",
        "deleted",
    )
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn postgres_gc_reclaims_timestamp_deleted_vectors_when_heads_diverged() -> Result<()> {
    let fixture = KnowledgeLifecycleFixture::create().await?;
    let result = assert_diverged_head_vectors_swept(
        &fixture,
        "vector-gc-timestamp-diverged",
        "Vector GC timestamp-deleted diverged-head document",
        "active",
    )
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn vector_sweep_preserves_leased_replacement_after_preflight_with_retained_head() -> Result<()>
{
    use ironrag_backend::infra::repositories::{admission_repository, ingest_repository};
    use ironrag_contracts::documents::ReplacementIdentity;
    use sha2::{Digest, Sha256};
    let fixture = KnowledgeLifecycleFixture::create().await?;
    let result = Box::pin(async {
        let pg = &fixture.state.persistence.postgres;
        let document_id = Uuid::now_v7();
        let a_id = Uuid::now_v7();
        let profile = format!("embedding-profile:v1:{0}{0}", Uuid::now_v7().simple());
        fixture.state.application_services.knowledge.create_document_shell(
            &fixture.state, CreateKnowledgeDocumentCommand {
                document_id, workspace_id: fixture.workspace_id, library_id: fixture.library_id,
                external_key: format!("leased-sweep-{document_id}"), file_name: None,
                title: None, document_state: "active".to_owned(),
            },
        ).await?;
        fixture.state.application_services.knowledge.write_revision(
            &fixture.state, knowledge_revision_command(&fixture, KnowledgeRevisionSeed {
                document_id, revision_id: a_id, revision_number: 1, revision_kind: "upload",
                title: "Published A", normalized_text: "published A",
            }),
        ).await?;
        let a_chunk = write_chunk(&fixture, document_id, a_id, 0, "published A").await?;
        mark_structured_revision_prepared(&fixture, document_id, a_id, 1).await?;
        write_vector(&fixture, a_chunk, a_id, &profile, vec![0.1, 0.2]).await?;
        publish_authoritative_content_head(&fixture, document_id, "active", Some(a_id), Some(a_id)).await?;
        sqlx::query("update knowledge_document set active_revision_id=$2,readable_revision_id=$2 where document_id=$1")
            .bind(document_id).bind(a_id).execute(pg).await?;
        maintenance_gc::run_superseded_vectors_for_library(
            &fixture.state, fixture.workspace_id, fixture.library_id,
            GcSupersededVectorsOptions { dry_run: true, ..GcSupersededVectorsOptions::default() },
        ).await.context("GC preflight must pass before C is admitted")?;

        let bytes = b"pending C";
        let checksum = format!("sha256:{}", hex::encode(Sha256::digest(bytes)));
        let storage_key = fixture.state.content_storage.persist_revision_source(
            fixture.workspace_id, fixture.library_id, "pending-c.txt", &checksum, bytes,
        ).await?;
        let replacement = ReplacementIdentity {
            checksum: checksum.clone(), byte_size: i64::try_from(bytes.len())?,
            mime_type: "text/plain".to_owned(), file_name: "pending-c.txt".to_owned(),
            title: None, language_code: None, source_uri: None, document_hint: None,
        };
        let admitted = admission_repository::admit_content_with_failpoint(pg,
            &admission_repository::ContentAdmissionRequest {
                workspace_id: fixture.workspace_id, library_id: fixture.library_id,
                operation_kind: "replace".to_owned(), requested_by_principal_id: None,
                request_surface: "internal".to_owned(), idempotency_key: Some("pending-sweep-c".to_owned()),
                source_identity: Some("pending-sweep-c-source".to_owned()),
                target: admission_repository::ContentAdmissionTarget::Existing { document_id },
                parent_async_operation_id: None, priority: 100,
                revision: Some(admission_repository::RevisionAdmission {
                    content_source_kind: "replace".to_owned(), storage_key: Some(storage_key),
                    replacement, visibility_intent_id: None, base_revision_id: None,
                }),
            }, None,
        ).await?.into_bundle();
        let c = admitted.revision.as_ref().context("C admission revision")?;
        let job = admitted.job.as_ref().context("C admission job")?;
        let now = Utc::now();
        let lease = ingest_repository::create_ingest_attempt_for_inline_lease(pg,
            &ingest_repository::NewIngestAttempt {
                job_id: job.id, attempt_number: 1, worker_principal_id: None,
                lease_token: Some("pending-sweep-attempt".to_owned()), knowledge_generation_id: None,
                attempt_state: "leased".to_owned(), current_stage: Some("embed".to_owned()),
                started_at: Some(now), heartbeat_at: Some(now), finished_at: None,
                failure_class: None, failure_code: None, failure_message: None,
                progress_percent: 0, retryable: false,
            }, "pending-sweep-job", "pending-sweep-worker",
        ).await?.context("ordinary pending C job must lease")?;
        let mut knowledge_c = knowledge_revision_command(&fixture, KnowledgeRevisionSeed {
            document_id, revision_id: c.id, revision_number: i64::from(c.revision_number),
            revision_kind: "replace", title: "Pending C", normalized_text: "pending C",
        });
        knowledge_c.checksum = c.checksum.clone();
        knowledge_c.byte_size = c.byte_size;
        knowledge_c.storage_ref = c.storage_key.clone();
        knowledge_c.title = c.title.clone();
        knowledge_c.source_uri = c.source_uri.clone();
        fixture.state.application_services.knowledge.write_revision(&fixture.state, knowledge_c).await?;
        let c_chunk = write_chunk(&fixture, document_id, c.id, 0, "pending C").await?;
        mark_structured_revision_prepared(&fixture, document_id, c.id, 1).await?;
        write_vector(&fixture, c_chunk, c.id, &profile, vec![0.2, 0.1]).await?;
        sqlx::query("update knowledge_document set active_revision_id=$2,readable_revision_id=$2 where document_id=$1")
            .bind(document_id).bind(a_id).execute(pg).await?;
        let head: (Option<Uuid>, Option<Uuid>) = sqlx::query_as(
            "select active_revision_id,readable_revision_id from content_document_head where document_id=$1",
        ).bind(document_id).fetch_one(pg).await?;
        assert_eq!(head, (Some(a_id), Some(a_id)), "C is leased, not either published head pointer");
        let staging = assert_candidate_blocks_library_rebuild(&fixture, document_id, a_id, a_chunk, c_chunk, &profile).await?;
        let batch = maintenance_gc::run_superseded_vectors_without_preflight_for_test(
            &fixture.state, fixture.library_id, GcSupersededVectorsOptions::default(),
        ).await?;
        assert_eq!(batch.superseded_vectors_removed, 0, "a later sweep batch must protect candidate C's own leased item");
        assert_eq!(fixture.state.search_store.list_chunk_vectors_by_chunk(c_chunk).await?.len(), 1);
        assert_eq!(chunk_vector_manifest_count(&fixture, &profile, 2).await?, 2);
        sqlx::query("update ingest_attempt set attempt_state='failed',finished_at=now() where id=$1")
            .bind(lease.id).execute(pg).await?;
        sqlx::query("update ingest_job set queue_state='failed',completed_at=now() where id=$1")
            .bind(job.id).execute(pg).await?;
        let terminal = maintenance_gc::run_superseded_vectors_without_preflight_for_test(
            &fixture.state, fixture.library_id, GcSupersededVectorsOptions::default(),
        ).await?;
        assert_eq!(terminal.superseded_vectors_removed, 1, "terminal C no longer retains lease protection");
        assert!(fixture.state.search_store.list_chunk_vectors_by_chunk(c_chunk).await?.is_empty());
        let retained_a = fixture.state.search_store.list_chunk_vectors_by_chunk(a_chunk).await?;
        assert_eq!(retained_a.len(), 2, "both active and refused staging A vectors survive");
        assert_eq!(retained_a.iter().filter(|vector| vector.embedding_model_key == profile).count(), 1);
        assert_eq!(retained_a.iter().filter(|vector| vector.embedding_model_key == staging).count(), 1);
        assert_eq!(chunk_vector_manifest_count(&fixture, &profile, 2).await?, 1);
        assert_eq!(chunk_vector_manifest_count(&fixture, &staging, 2).await?, 1);
        Ok(())
    }).await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn postgres_gc_removes_only_superseded_vectors() -> Result<()> {
    let fixture = KnowledgeLifecycleFixture::create().await?;

    let result = async {
        let document_id = Uuid::now_v7();
        let superseded_revision_id = Uuid::now_v7();
        let readable_revision_id = Uuid::now_v7();
        let active_revision_id = Uuid::now_v7();
        let embedding_profile_key = format!("embedding-profile:v1:{0}{0}", Uuid::now_v7().simple());
        let embedding_profile_key_d3 =
            format!("embedding-profile:v1:{0}{0}", Uuid::now_v7().simple());

        fixture
            .state
            .application_services
            .knowledge
            .create_document_shell(
                &fixture.state,
                CreateKnowledgeDocumentCommand {
                    document_id,
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: format!("vector-gc-doc-{}", document_id.simple()),
                    file_name: None,
                    title: Some("Vector GC lifecycle document".to_string()),
                    document_state: "active".to_string(),
                },
            )
            .await?;

        let revisions = [
            (1, superseded_revision_id, "Superseded revision"),
            (2, readable_revision_id, "Readable revision"),
            (3, active_revision_id, "Active revision"),
        ];
        for (revision_number, revision_id, title) in revisions {
            fixture
                .state
                .application_services
                .knowledge
                .write_revision(
                    &fixture.state,
                    knowledge_revision_command(
                        &fixture,
                        KnowledgeRevisionSeed {
                            document_id,
                            revision_id,
                            revision_number,
                            revision_kind: "replace",
                            title,
                            normalized_text: title,
                        },
                    ),
                )
                .await?;
        }

        // Seed the persisted state from the reported incident directly. The
        // ordinary happy path may clean an older projection while creating a
        // later revision, which would make the regression impossible to
        // represent through API calls alone.
        let mut seeded_chunks = Vec::new();
        for (_, revision_id, title) in revisions {
            let chunk_id = Uuid::now_v7();
            sqlx::query(
                "insert into knowledge_chunk (
                    chunk_id, workspace_id, library_id, document_id, revision_id,
                    chunk_index, content_text, normalized_text, chunk_state
                 ) values ($1, $2, $3, $4, $5, 0, $6, $6, 'ready')",
            )
            .bind(chunk_id)
            .bind(fixture.workspace_id)
            .bind(fixture.library_id)
            .bind(document_id)
            .bind(revision_id)
            .bind(title)
            .execute(&fixture.state.persistence.postgres)
            .await?;
            mark_structured_revision_prepared(&fixture, document_id, revision_id, 1).await?;
            write_vector(
                &fixture,
                chunk_id,
                revision_id,
                &embedding_profile_key,
                vec![0.1, 0.2],
            )
            .await?;
            if revision_id == superseded_revision_id {
                write_vector(
                    &fixture,
                    chunk_id,
                    revision_id,
                    &embedding_profile_key_d3,
                    vec![0.1, 0.2, 0.3],
                )
                .await?;
            }
            seeded_chunks.push((revision_id, chunk_id));
        }

        publish_authoritative_content_head(
            &fixture,
            document_id,
            "active",
            Some(active_revision_id),
            Some(readable_revision_id),
        )
        .await?;
        let knowledge_head_update = sqlx::query(
            "update knowledge_document
             set active_revision_id = $2,
                 readable_revision_id = $3,
                 updated_at = now()
             where document_id = $1",
        )
        .bind(document_id)
        .bind(active_revision_id)
        .bind(readable_revision_id)
        .execute(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(knowledge_head_update.rows_affected(), 1);

        for (revision_id, _) in &seeded_chunks {
            assert_eq!(
                fixture
                    .state
                    .document_store
                    .list_chunks_by_revision(*revision_id)
                    .await?
                    .len(),
                1,
                "fixture must contain every historical knowledge chunk before vector GC: {revision_id}",
            );
        }

        let protected_mutation_id = create_sweep_candidate_mutation(&fixture, document_id, active_revision_id).await?;
        let protected_item_id = Uuid::now_v7();
        let protected_job_id = Uuid::now_v7();
        sqlx::query(
            "insert into content_mutation_item (
                 id, mutation_id, document_id, result_revision_id, item_state
             ) values ($1, $2, $3, $4, 'pending')",
        )
        .bind(protected_item_id)
        .bind(protected_mutation_id)
        .bind(document_id)
        .bind(active_revision_id)
        .execute(&fixture.state.persistence.postgres)
        .await?;
        sqlx::query(
            "insert into ingest_job (
                 id, workspace_id, library_id, mutation_id, mutation_item_id,
                 knowledge_document_id, knowledge_revision_id, job_kind,
                 queue_state, queue_leased_at, queue_lease_token, queue_lease_owner
             ) values (
                 $1, $2, $3, $4, $5, $6, $7, 'content_mutation',
                 'leased', now(), $8, 'gc-active-protection-test'
             )",
        )
        .bind(protected_job_id)
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind(protected_mutation_id)
        .bind(protected_item_id)
        .bind(document_id)
        .bind(active_revision_id)
        .bind(format!("gc-active-{protected_job_id}"))
        .execute(&fixture.state.persistence.postgres)
        .await?;
        sqlx::query(
            "insert into ingest_attempt (
                 job_id, attempt_number, attempt_state, lease_token, heartbeat_at
             ) values ($1, 1, 'leased', $2, now())",
        )
        .bind(protected_job_id)
        .bind(format!("gc-active-attempt-{protected_job_id}"))
        .execute(&fixture.state.persistence.postgres)
        .await?;
        let mismatched_mutation_id = create_sweep_candidate_mutation(&fixture, document_id, active_revision_id).await?;
        let mismatched_job_update = sqlx::query(
            "update ingest_job
             set mutation_id = $2
             where id = $1",
        )
        .bind(protected_job_id)
        .bind(mismatched_mutation_id)
        .execute(&fixture.state.persistence.postgres)
        .await;
        let ownership_error = mismatched_job_update.expect_err("a job cannot adopt another mutation's item");
        assert_eq!(ownership_error.as_database_error().and_then(|error| error.code()).as_deref(), Some("23514"));
        let mismatched_preview =
            maintenance_gc::run_superseded_vectors_without_preflight_for_test(
                &fixture.state,
                fixture.library_id,
                GcSupersededVectorsOptions {
                    dry_run: true,
                    ..GcSupersededVectorsOptions::default()
                },
            )
            .await?;
        assert_eq!(
            mismatched_preview.superseded_vectors_removed, 2,
            "rejected owner corruption must leave the exact leased candidate protected",
        );
        let protected_preview =
            maintenance_gc::run_superseded_vectors_without_preflight_for_test(
                &fixture.state,
                fixture.library_id,
                GcSupersededVectorsOptions {
                    dry_run: true,
                    ..GcSupersededVectorsOptions::default()
                },
            )
            .await?;
        assert_eq!(protected_preview.superseded_vectors_removed, 2);
        let protected_page = maintenance_gc::run_superseded_vectors_without_preflight_for_test(
            &fixture.state,
            fixture.library_id,
            GcSupersededVectorsOptions {
                max_batches: Some(1),
                ..GcSupersededVectorsOptions::default()
            },
        )
        .await?;
        assert_eq!(protected_page.superseded_vectors_removed, 1);
        assert!(protected_page.work_remaining);
        assert_seeded_gc_report(&protected_preview, 1, 2)?;
        assert_seeded_gc_report(&protected_page, 1, 1)?;
        assert_eq!(
            fixture
                .state
                .search_store
                .list_chunk_vectors_by_chunk(seeded_chunks[2].1)
                .await?
                .len(),
            1,
            "exact leased latest attempt must protect its active revision",
        );
        write_vector(
            &fixture,
            seeded_chunks[0].1,
            superseded_revision_id,
            &embedding_profile_key,
            vec![0.1, 0.2],
        )
        .await?;
        sqlx::query("delete from ingest_job where id = $1")
            .bind(protected_job_id)
            .execute(&fixture.state.persistence.postgres)
            .await?;
        sqlx::query("delete from content_mutation_reference where id = $1")
            .bind(protected_mutation_id)
            .execute(&fixture.state.persistence.postgres)
            .await?;
        sqlx::query("delete from content_mutation_reference where id = $1")
            .bind(mismatched_mutation_id)
            .execute(&fixture.state.persistence.postgres)
            .await?;

        let preview = maintenance_gc::run_superseded_vectors_for_library(
            &fixture.state,
            fixture.workspace_id,
            fixture.library_id,
            GcSupersededVectorsOptions {
                dry_run: true,
                include_null_head: false,
                max_batches: None,
            },
        )
        .await?;
        assert_eq!(preview.superseded_vectors_removed, 3);
        assert_eq!(
            fixture
                .state
                .search_store
                .list_chunk_vectors_by_chunk(seeded_chunks[0].1)
                .await?
                .len(),
            2,
            "dry-run must not delete vectors",
        );

        let report = maintenance_gc::run_superseded_vectors_for_library_from_cursor_for_test(
            &fixture.state,
            fixture.workspace_id,
            fixture.library_id,
            GcSupersededVectorsOptions {
                max_batches: Some(1),
                ..GcSupersededVectorsOptions::default()
            },
            None,
        )
        .await?;

        assert_eq!(report.superseded_vectors_removed, 2);
        assert!(report.work_remaining);
        assert_eq!(
            fixture
                .state
                .search_store
                .list_chunk_vectors_by_chunk(seeded_chunks[0].1)
                .await?
                .len(),
            1,
            "one scheduler run must commit at most one physical-shard batch",
        );
        let persisted_cursor = report
            .next_cursor_for_test()
            .cloned()
            .context("bounded scheduler pass must return a durable cursor")?;
        // Recreate the stale key in both physical relations. The canonical
        // write path advances source_truth_version. A resumed sweep that
        // trusted the old relation cursor could skip the relation behind it;
        // a safe sweep resets to relation zero and drains both in order.
        write_vector(
            &fixture,
            seeded_chunks[0].1,
            superseded_revision_id,
            &embedding_profile_key,
            vec![0.1, 0.2],
        )
        .await?;
        // The low-level test seeder intentionally bypasses the production
        // mutation fence. Model the fence advancement that accompanies a
        // canonical external mutation so the persisted cursor is stale.
        let source_truth_update = sqlx::query(
            "update catalog_library
             set source_truth_version = source_truth_version + 1,
                 updated_at = now()
             where id = $1",
        )
        .bind(fixture.library_id)
        .execute(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(source_truth_update.rows_affected(), 1);
        write_vector(
            &fixture,
            seeded_chunks[0].1,
            superseded_revision_id,
            &embedding_profile_key_d3,
            vec![0.1, 0.2, 0.3],
        )
        .await?;
        let reset = maintenance_gc::run_superseded_vectors_for_library_from_cursor_for_test(
            &fixture.state,
            fixture.workspace_id,
            fixture.library_id,
            GcSupersededVectorsOptions {
                max_batches: Some(1),
                ..GcSupersededVectorsOptions::default()
            },
            Some(&persisted_cursor),
        )
        .await?;
        assert_eq!(reset.superseded_vectors_removed, 1);
        assert!(
            reset.work_remaining,
            "source reset stopped early: persisted_cursor={persisted_cursor:?}, report={reset:?}"
        );
        let reset_cursor = reset
            .next_cursor_for_test()
            .cloned()
            .context("source-truth change must restart the durable cursor")?;
        let resumed = maintenance_gc::run_superseded_vectors_for_library_from_cursor_for_test(
            &fixture.state,
            fixture.workspace_id,
            fixture.library_id,
            GcSupersededVectorsOptions {
                max_batches: Some(1),
                ..GcSupersededVectorsOptions::default()
            },
            Some(&reset_cursor),
        )
        .await?;
        assert_eq!(resumed.superseded_vectors_removed, 1);
        assert!(!resumed.work_remaining);
        assert!(resumed.next_cursor_for_test().is_none());
        for (revision_id, chunk_id) in &seeded_chunks {
            let vectors = fixture.state.search_store.list_chunk_vectors_by_chunk(*chunk_id).await?;
            if *revision_id == readable_revision_id {
                assert_eq!(vectors.len(), 1, "the readable vector must survive");
            } else {
                assert!(
                    vectors.is_empty(),
                    "superseded and stale-active vectors must be removed",
                );
            }
            assert_eq!(
                fixture.state.document_store.list_chunks_by_revision(*revision_id).await?.len(),
                1,
                "vector GC must preserve historical knowledge chunks: {revision_id}",
            );
        }

        let repeated = maintenance_gc::run_superseded_vectors_for_library(
            &fixture.state,
            fixture.workspace_id,
            fixture.library_id,
            GcSupersededVectorsOptions::default(),
        )
        .await?;
        assert_eq!(
            repeated.superseded_vectors_removed,
            0,
            "vector GC must be idempotent"
        );

        let boundary_insert = sqlx::query(
            "insert into knowledge_chunk_vector_d2 (
                 key, vector_id, workspace_id, library_id, chunk_id, revision_id,
                 embedding_model_key, vector_kind, dimensions, embedding,
                 freshness_generation, created_at
             )
             select concat('gc-boundary-', $1::text, '-', generation::text),
                    gen_random_uuid(), $2, $3, $4, $1, $5,
                    'chunk_embedding', 2, '[0.1,0.2]', generation, now()
             from generate_series(1, 10001) generation",
        )
        .bind(superseded_revision_id)
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind(seeded_chunks[0].1)
        .bind(&embedding_profile_key)
        .execute(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(boundary_insert.rows_affected(), 10_001);
        let manifest_update = sqlx::query(
            "update knowledge_vector_relation_manifest
             set row_count = row_count + 10001
             where library_id = $1
               and relation_name = 'knowledge_chunk_vector_d2'
               and embedding_model_key = $2
               and vector_kind = 'chunk_embedding'",
        )
        .bind(fixture.library_id)
        .bind(&embedding_profile_key)
        .execute(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(manifest_update.rows_affected(), 1);
        let boundary_page =
            maintenance_gc::run_superseded_vectors_for_library_from_cursor_for_test(
                &fixture.state,
                fixture.workspace_id,
                fixture.library_id,
                GcSupersededVectorsOptions {
                    max_batches: Some(1),
                    ..GcSupersededVectorsOptions::default()
                },
                None,
            )
            .await?;
        assert_eq!(boundary_page.superseded_vectors_removed, 10_000);
        assert!(boundary_page.work_remaining);
        let boundary_cursor = boundary_page
            .next_cursor_for_test()
            .cloned()
            .context("full 10,000-row page must persist its last logical key")?;
        let boundary_tail =
            maintenance_gc::run_superseded_vectors_for_library_from_cursor_for_test(
                &fixture.state,
                fixture.workspace_id,
                fixture.library_id,
                GcSupersededVectorsOptions::default(),
                Some(&boundary_cursor),
            )
            .await?;
        assert_eq!(boundary_tail.superseded_vectors_removed, 1);
        assert!(!boundary_tail.work_remaining);
        assert!(boundary_tail.next_cursor_for_test().is_none());

        let live_insert = sqlx::query(
            "insert into knowledge_chunk_vector_d2 (
                 key, vector_id, workspace_id, library_id, chunk_id, revision_id,
                 embedding_model_key, vector_kind, dimensions, embedding,
                 freshness_generation, created_at
             )
             select concat('gc-live-boundary-', $1::text, '-', generation::text),
                    gen_random_uuid(), $2, $3, $4, $1, $5,
                    'chunk_embedding', 2, '[0.1,0.2]', generation, now()
             from generate_series(2, 10002) generation",
        )
        .bind(readable_revision_id)
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind(seeded_chunks[1].1)
        .bind(&embedding_profile_key)
        .execute(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(live_insert.rows_affected(), 10_001);
        let live_manifest_increment = sqlx::query(
            "update knowledge_vector_relation_manifest
             set row_count = row_count + 10001
             where library_id = $1
               and relation_name = 'knowledge_chunk_vector_d2'
               and embedding_model_key = $2
               and vector_kind = 'chunk_embedding'",
        )
        .bind(fixture.library_id)
        .bind(&embedding_profile_key)
        .execute(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(live_manifest_increment.rows_affected(), 1);
        let live_page = maintenance_gc::run_superseded_vectors_for_library_from_cursor_for_test(
            &fixture.state,
            fixture.workspace_id,
            fixture.library_id,
            GcSupersededVectorsOptions {
                max_batches: Some(1),
                ..GcSupersededVectorsOptions::default()
            },
            None,
        )
        .await?;
        assert_eq!(live_page.superseded_vectors_removed, 0);
        assert!(live_page.work_remaining, "a full live page must yield to the next scheduler tick");
        let live_cursor = live_page
            .next_cursor_for_test()
            .cloned()
            .context("a full live page must advance the durable scan cursor")?;
        let live_tail = maintenance_gc::run_superseded_vectors_for_library_from_cursor_for_test(
            &fixture.state,
            fixture.workspace_id,
            fixture.library_id,
            GcSupersededVectorsOptions::default(),
            Some(&live_cursor),
        )
        .await?;
        assert_eq!(live_tail.superseded_vectors_removed, 0);
        assert!(!live_tail.work_remaining);
        assert_eq!(
            fixture
                .state
                .search_store
                .list_chunk_vectors_by_chunk(seeded_chunks[1].1)
                .await?
                .len(),
            10_002,
            "bounded scanning must not trade response quality for cleanup speed",
        );
        let live_cleanup = sqlx::query(
            "delete from knowledge_chunk_vector_d2
             where library_id = $1
               and chunk_id = $2
               and freshness_generation > 1",
        )
        .bind(fixture.library_id)
        .bind(seeded_chunks[1].1)
        .execute(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(live_cleanup.rows_affected(), 10_001);
        let live_manifest_decrement = sqlx::query(
            "update knowledge_vector_relation_manifest
             set row_count = row_count - 10001
             where library_id = $1
               and relation_name = 'knowledge_chunk_vector_d2'
               and embedding_model_key = $2
               and vector_kind = 'chunk_embedding'",
        )
        .bind(fixture.library_id)
        .bind(&embedding_profile_key)
        .execute(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(live_manifest_decrement.rows_affected(), 1);

        let manifest_counts = sqlx::query_as::<_, (i32, i64)>(
            "select dim, row_count
             from knowledge_vector_relation_manifest
             where library_id = $1
             order by dim",
        )
        .bind(fixture.library_id)
        .fetch_all(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(manifest_counts, vec![(2, 1), (3, 0)]);

        let superseded_chunk_id = seeded_chunks[0].1;
        write_vector(
            &fixture,
            superseded_chunk_id,
            superseded_revision_id,
            &embedding_profile_key,
            vec![0.1, 0.2],
        )
        .await?;

        publish_authoritative_content_head(
            &fixture,
            document_id,
            "active",
            Some(active_revision_id),
            Some(superseded_revision_id),
        )
        .await?;
        let drift_error = maintenance_gc::run_superseded_vectors_for_library(
            &fixture.state,
            fixture.workspace_id,
            fixture.library_id,
            GcSupersededVectorsOptions::default(),
        )
        .await
        .expect_err("projection drift must fail closed");
        assert_eq!(drift_error.code(), "vector_plane");
        assert_eq!(
            fixture
                .state
                .search_store
                .list_chunk_vectors_by_chunk(superseded_chunk_id)
                .await?
                .len(),
            1,
            "projection drift must preserve the candidate vector",
        );

        publish_authoritative_content_head(
            &fixture,
            document_id,
            "active",
            Some(active_revision_id),
            Some(readable_revision_id),
        )
        .await?;
        sqlx::query(
            "update knowledge_vector_relation_manifest
             set row_count = 0
             where library_id = $1 and dim = 2",
        )
        .bind(fixture.library_id)
        .execute(&fixture.state.persistence.postgres)
        .await?;
        let manifest_error = maintenance_gc::run_superseded_vectors_for_library(
            &fixture.state,
            fixture.workspace_id,
            fixture.library_id,
            GcSupersededVectorsOptions::default(),
        )
        .await
        .expect_err("under-counted manifest must roll the physical delete back");
        assert_eq!(manifest_error.code(), "vector_plane");
        assert_eq!(
            fixture
                .state
                .search_store
                .list_chunk_vectors_by_chunk(superseded_chunk_id)
                .await?
                .len(),
            1,
            "manifest drift must roll the candidate delete back",
        );
        sqlx::query(
            "update knowledge_vector_relation_manifest
             set row_count = 2
             where library_id = $1 and dim = 2",
        )
        .bind(fixture.library_id)
        .execute(&fixture.state.persistence.postgres)
        .await?;

        let ingest_job_id = sqlx::query_scalar::<_, Uuid>(
            "insert into ingest_job (
                workspace_id, library_id, knowledge_document_id,
                job_kind, queue_state, priority, queued_at, available_at
             ) values ($1, $2, $3, 'content_mutation', 'queued', 100, now(), now())
             returning id",
        )
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind(document_id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        let active_ingest_error = maintenance_gc::run_superseded_vectors_for_library(
            &fixture.state,
            fixture.workspace_id,
            fixture.library_id,
            GcSupersededVectorsOptions::default(),
        )
        .await
        .expect_err("active ingest must defer vector GC");
        assert_eq!(active_ingest_error.code(), "active_ingest");
        assert_eq!(
            fixture
                .state
                .search_store
                .list_chunk_vectors_by_chunk(superseded_chunk_id)
                .await?
                .len(),
            1,
            "active ingest must preserve the candidate vector",
        );
        sqlx::query("delete from ingest_job where id = $1")
            .bind(ingest_job_id)
            .execute(&fixture.state.persistence.postgres)
            .await?;

        publish_authoritative_content_head(&fixture, document_id, "active", None, None).await?;
        sqlx::query(
            "update knowledge_document
             set active_revision_id = null,
                 readable_revision_id = null,
                 updated_at = now()
             where document_id = $1",
        )
        .bind(document_id)
        .execute(&fixture.state.persistence.postgres)
        .await?;

        let null_head_default = maintenance_gc::run_superseded_vectors_for_library(
            &fixture.state,
            fixture.workspace_id,
            fixture.library_id,
            GcSupersededVectorsOptions::default(),
        )
        .await?;
        assert_eq!(null_head_default.null_head_docs_total, 1);
        assert_eq!(null_head_default.null_head_docs_processed, 0);
        assert_eq!(null_head_default.null_head_vectors_removed, 0);

        let null_head_included = maintenance_gc::run_superseded_vectors_for_library(
            &fixture.state,
            fixture.workspace_id,
            fixture.library_id,
            GcSupersededVectorsOptions {
                dry_run: false,
                include_null_head: true,
                max_batches: None,
            },
        )
        .await?;
        assert_eq!(null_head_included.null_head_docs_total, 1);
        assert_eq!(null_head_included.null_head_docs_processed, 1);
        assert_eq!(null_head_included.null_head_vectors_removed, 2);
        let final_manifest_count = sqlx::query_scalar::<_, i64>(
            "select coalesce(sum(row_count), 0)::bigint
             from knowledge_vector_relation_manifest
             where library_id = $1",
        )
        .bind(fixture.library_id)
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(final_manifest_count, 0);
        for (revision_id, _) in &seeded_chunks {
            assert_eq!(
                fixture
                    .state
                    .document_store
                    .list_chunks_by_revision(*revision_id)
                    .await?
                    .len(),
                1,
                "null-head vector GC must preserve chunks: {revision_id}",
            );
        }

        sqlx::query(
            "insert into maintenance_job_run (
                 class, scope_kind, scope_id, next_due_at
             ) values ('gc.stale-chunks', 'library', $1, now())",
        )
        .bind(fixture.library_id)
        .execute(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(
            maintenance_lease::retire_obsolete_rows(&fixture.state.persistence.postgres).await?,
            1,
        );
        let obsolete_rows = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from maintenance_job_run
             where class = 'gc.stale-chunks'",
        )
        .fetch_one(&fixture.state.persistence.postgres)
        .await?;
        assert_eq!(obsolete_rows, 0);

        maintenance_lease::ensure_row(
            &fixture.state.persistence.postgres,
            MaintenanceClass::GcRuntimeGraph,
            Scope::Library(fixture.library_id),
            None,
        )
        .await?;
        let leased = maintenance_lease::acquire_next_due(
            &fixture.state.persistence.postgres,
            MaintenanceClass::GcRuntimeGraph,
            "scheduler-a",
            maintenance_lease::AcquirePreference::InFlight,
            std::time::Duration::from_secs(3600),
        )
        .await?
        .context("maintenance ownership test could not acquire lease")?;
        sqlx::query(
            "update maintenance_job_run
             set owner_node = 'scheduler-b'
             where id = $1 and state = 'leased'",
        )
        .bind(leased.id)
        .execute(&fixture.state.persistence.postgres)
        .await?;
        let stale_owner = maintenance_lease::update_cursor(
            &fixture.state.persistence.postgres,
            leased.id,
            "scheduler-a",
            serde_json::json!({"position": "must-not-land"}),
        )
        .await;
        assert!(matches!(stale_owner, Err(LeaseError::LostOwnership { .. })));
        let stale_completion = maintenance_lease::complete(
            &fixture.state.persistence.postgres,
            leased.id,
            "scheduler-a",
            1,
            0,
            std::time::Duration::from_secs(60),
        )
        .await;
        assert!(matches!(stale_completion, Err(LeaseError::LostOwnership { .. })));
        let current = maintenance_lease::load(
            &fixture.state.persistence.postgres,
            MaintenanceClass::GcRuntimeGraph,
            Scope::Library(fixture.library_id),
        )
        .await?
        .context("maintenance lease disappeared during ownership test")?;
        assert_eq!(current.owner_node.as_deref(), Some("scheduler-b"));
        assert_eq!(current.cursor_json, serde_json::json!({}));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
