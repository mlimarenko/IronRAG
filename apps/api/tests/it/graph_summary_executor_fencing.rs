//! PostgreSQL integration coverage for graph-summary executor fencing.

use crate::{postgres_activity::wait_for_backend_lock, temp_postgres_database};

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::{
        persistence::Persistence,
        repositories::{self, catalog_repository},
    },
    services::graph::{
        PendingGraphSummaryRefresh, PublishedGraphSummaryRefreshOutcome, error::GraphServiceError,
    },
};
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use temp_postgres_database::TempPostgresDatabase;
use tokio::{task::JoinHandle, time::Instant};
use uuid::Uuid;

type RefreshTask = JoinHandle<Result<PublishedGraphSummaryRefreshOutcome, GraphServiceError>>;

struct SummaryExecutorFixture {
    database: TempPostgresDatabase,
    state: Arc<AppState>,
    observer: PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    node_ids: Vec<Uuid>,
    edge_id: Uuid,
    generation: i64,
    backend_pid: i32,
}

impl SummaryExecutorFixture {
    async fn create() -> Result<Self> {
        let mut settings = Settings::from_env().context("load graph summary executor settings")?;
        let database =
            TempPostgresDatabase::create(&settings.database_url, "graph_summary_executor").await?;
        settings.database_url = database.database_url.clone();
        settings.credential_master_key =
            Some("KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=".to_string());
        // The generation guard occupies this sole connection. Its refresh must
        // use the transaction executor rather than acquire another main slot.
        let postgres = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(30))
            .connect(&settings.database_url)
            .await?;
        sqlx::migrate!("./migrations").run(&postgres).await?;
        let observer =
            PgPoolOptions::new().max_connections(3).connect(&settings.database_url).await?;
        let backend_pid =
            sqlx::query_scalar("select pg_backend_pid()").fetch_one(&postgres).await?;
        let redis = redis::Client::open(settings.redis_url.clone())?;
        let state = Arc::new(AppState::from_dependencies(
            settings,
            Persistence::for_tests(postgres, redis),
        )?);
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            &state.persistence.postgres,
            &format!("graph-summary-workspace-{suffix}"),
            "Graph Summary Workspace",
            None,
        )
        .await?;
        let library = catalog_repository::create_library(
            &state.persistence.postgres,
            workspace.id,
            &format!("graph-summary-library-{suffix}"),
            "Graph Summary Library",
            None,
            None,
        )
        .await?;
        let document_id = sqlx::query_scalar(
            "insert into content_document (workspace_id, library_id, external_key, document_state)
             values ($1, $2, $3, 'active') returning id",
        )
        .bind(workspace.id)
        .bind(library.id)
        .bind(format!("graph-summary:{suffix}"))
        .fetch_one(&state.persistence.postgres)
        .await?;
        let revision_id = crate::content_lifecycle_support::seed_graph_document_revision(
            &state.persistence.postgres,
            document_id,
            "summary.txt",
            "Summary source",
        )
        .await?;
        let nodes = repositories::bulk_upsert_runtime_graph_nodes(
            &state.persistence.postgres,
            library.id,
            1,
            &["source", "target"].map(|name| repositories::RuntimeGraphNodeUpsertInput {
                identity_key: format!("entity:{name}:{suffix}"),
                label: format!("Neutral {name}"),
                node_type: "entity".to_string(),
                aliases_json: json!([]),
                summary: Some(format!("A reusable {name} record")),
                metadata_json: json!({}),
                support_count: 1,
            }),
        )
        .await?;
        let source = nodes
            .iter()
            .find(|node| node.label == "Neutral source")
            .context("missing source node")?;
        let target = nodes
            .iter()
            .find(|node| node.label == "Neutral target")
            .context("missing target node")?;
        let node_ids = vec![source.id, target.id];
        let edge = repositories::upsert_runtime_graph_edge(
            &state.persistence.postgres,
            repositories::RuntimeGraphEdgeInput {
                library_id: library.id,
                from_node_id: source.id,
                to_node_id: target.id,
                relation_type: "relates_to",
                identity_key: &format!("edge:{suffix}"),
                summary: Some("The source relates to the target"),
                weight: Some(1.0),
                support_count: 1,
                metadata_json: json!({}),
                projection_version: 1,
            },
        )
        .await?;
        for (kind, target_id) in [("node", source.id), ("node", target.id), ("edge", edge.id)] {
            repositories::create_runtime_graph_evidence(
                &state.persistence.postgres,
                repositories::CreateRuntimeGraphEvidenceInput {
                    library_id: library.id,
                    target_kind: kind,
                    target_id,
                    document_id: Some(document_id),
                    revision_id: Some(revision_id),
                    activated_by_attempt_id: None,
                    chunk_id: None,
                    source_file_name: Some("summary.txt"),
                    page_ref: None,
                    evidence_text: "The source relates to the target",
                    confidence_score: Some(1.0),
                    evidence_context_key: &format!("summary:{kind}:{target_id}"),
                },
            )
            .await?;
        }
        let generation = sqlx::query_scalar::<_, i64>(
            "update catalog_library
             set source_truth_version = greatest(coalesce(source_truth_version, 1) + 1, 2)
             where id = $1 returning source_truth_version",
        )
        .bind(library.id)
        .fetch_one(&state.persistence.postgres)
        .await?;
        Ok(Self {
            database,
            state,
            observer,
            workspace_id: workspace.id,
            library_id: library.id,
            document_id,
            revision_id,
            node_ids,
            edge_id: edge.id,
            generation,
            backend_pid,
        })
    }

    fn pool(&self) -> &PgPool {
        &self.state.persistence.postgres
    }

    fn spawn_refresh(&self, generation: i64, pending: PendingGraphSummaryRefresh) -> RefreshTask {
        let state = self.state.clone();
        let library_id = self.library_id;
        tokio::spawn(async move {
            state
                .application_services
                .graph
                .apply_published_summary_refresh(&state, library_id, generation, &pending)
                .await
        })
    }

    async fn seed_previous_summaries(&self) -> Result<()> {
        for (kind, target_id) in
            [("node", self.node_ids[0]), ("node", self.node_ids[1]), ("edge", self.edge_id)]
        {
            sqlx::query(
                "insert into runtime_graph_canonical_summary
                 (workspace_id, library_id, target_kind, target_id, summary_text,
                  confidence_status, support_count, source_truth_version)
                 values ($1, $2, $3, $4, 'Previously committed summary', 'supported', 1, $5)",
            )
            .bind(self.workspace_id)
            .bind(self.library_id)
            .bind(kind)
            .bind(target_id)
            .bind(self.generation - 1)
            .execute(self.pool())
            .await?;
        }
        Ok(())
    }

    async fn summaries(&self) -> Result<Vec<Value>> {
        sqlx::query_scalar(
            "select to_jsonb(summary) from runtime_graph_canonical_summary summary
             where library_id = $1 order by target_kind, target_id, source_truth_version",
        )
        .bind(self.library_id)
        .fetch_all(&self.observer)
        .await
        .map_err(Into::into)
    }

    async fn stop_refresh(&self, task: &mut RefreshTask) -> Result<()> {
        // Call only after releasing the fixture lock. A failing old implementation
        // must not leave a detached task holding a connection during database drop.
        if tokio::time::timeout(Duration::from_secs(3), &mut *task).await.is_err() {
            sqlx::query("select pg_cancel_backend($1)")
                .bind(self.backend_pid)
                .execute(&self.observer)
                .await?;
            task.abort();
            let _ = task.await;
        }
        Ok(())
    }

    async fn assert_publication_lock_available(&self) -> Result<()> {
        let mut transaction = self.observer.begin().await?;
        sqlx::query("set local lock_timeout = '1s'").execute(&mut *transaction).await?;
        sqlx::query("select id from catalog_library where id = $1 for update")
            .bind(self.library_id)
            .execute(&mut *transaction)
            .await?;
        transaction.rollback().await?;
        Ok(())
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.observer.close().await;
        self.database.drop().await
    }
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn published_summary_refresh_completes_with_one_main_pool_connection() -> Result<()> {
    let fixture = SummaryExecutorFixture::create().await?;
    let result = async {
        let pending = PendingGraphSummaryRefresh::targeted(vec![fixture.node_ids[0]], Vec::new());
        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            fixture.state.application_services.graph.apply_published_summary_refresh(
                &fixture.state,
                fixture.library_id,
                fixture.generation,
                &pending,
            ),
        )
        .await
        .context("refresh acquired a second main-pool connection")??;
        assert_eq!(
            outcome,
            PublishedGraphSummaryRefreshOutcome::Applied { invalidated: 0, refreshed: 1 }
        );
        let summary = repositories::get_active_runtime_graph_summary_by_target(
            fixture.pool(),
            fixture.library_id,
            "node",
            fixture.node_ids[0],
        )
        .await?
        .context("target summary missing")?;
        assert_eq!(summary.workspace_id, fixture.workspace_id);
        assert_eq!(summary.source_truth_version, fixture.generation);
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn published_summary_streams_normalized_evidence_when_fragments_are_absent() -> Result<()> {
    let fixture = SummaryExecutorFixture::create().await?;
    let result = async {
        sqlx::query("update runtime_graph_node set summary = null, metadata_json = '{}' where id = $1")
            .bind(fixture.node_ids[0]).execute(fixture.pool()).await?;
        sqlx::query("update runtime_graph_edge set summary = null, metadata_json = '{}' where id = $1")
            .bind(fixture.edge_id).execute(fixture.pool()).await?;
        sqlx::query(
            "insert into runtime_graph_evidence
             (library_id, evidence_identity_key, target_kind, target_id, document_id, revision_id, evidence_text)
             select library_id, evidence_identity_key || ':stream:' || ordinal::text,
                    target_kind, target_id, document_id, revision_id, fragment
             from runtime_graph_evidence
             cross join unnest($4::text[]) with ordinality as fragments(fragment, ordinal)
             where library_id = $1 and ((target_kind = 'node' and target_id = $2)
                                    or (target_kind = 'edge' and target_id = $3))",
        ).bind(fixture.library_id).bind(fixture.node_ids[0]).bind(fixture.edge_id)
            .bind(["Zebra detail", "  Alpha   detail  ", "Alpha detail", "Beta detail"])
            .execute(fixture.pool()).await?;
        let pending = PendingGraphSummaryRefresh::targeted(
            vec![fixture.node_ids[0]], vec![fixture.edge_id],
        );
        let outcome = fixture.state.application_services.graph.apply_published_summary_refresh(
            &fixture.state, fixture.library_id, fixture.generation, &pending,
        ).await?;
        assert_eq!(outcome, PublishedGraphSummaryRefreshOutcome::Applied { invalidated: 0, refreshed: 2 });
        for (kind, id) in [("node", fixture.node_ids[0]), ("edge", fixture.edge_id)] {
            let summary = repositories::get_active_runtime_graph_summary_by_target(
                fixture.pool(), fixture.library_id, kind, id,
            ).await?.context("evidence-backed summary missing")?;
            assert_eq!(summary.summary_text, "Alpha detail.");
        }
        Ok(())
    }.await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn published_summary_second_insert_timeout_rolls_back_every_summary_write() -> Result<()> {
    let fixture = SummaryExecutorFixture::create().await?;
    let result = async {
        fixture.seed_previous_summaries().await?;
        let before = fixture.summaries().await?;
        sqlx::raw_sql(
            "create sequence summary_insert_progress;
             create function block_second_summary_insert() returns trigger language plpgsql as $$
             declare ordinal bigint;
             begin
                 ordinal := nextval('summary_insert_progress');
                 if ordinal = 2 then
                     if (select count(*) from runtime_graph_canonical_summary
                         where target_kind = 'node' and source_truth_version = new.source_truth_version) <> 1
                        or exists (select 1 from runtime_graph_canonical_summary
                                   where target_kind = 'node' and source_truth_version <> new.source_truth_version) then
                         raise exception 'second insert reached without invalidation and first write';
                     end if;
                     perform pg_advisory_xact_lock(298, 702);
                 end if;
                 return new;
             end $$;
             create trigger block_second_summary_insert before insert on runtime_graph_canonical_summary
             for each row execute function block_second_summary_insert();",
        ).execute(&fixture.observer).await?;
        let mut blocker = fixture.observer.begin().await?;
        sqlx::query("select pg_advisory_xact_lock(298, 702)").execute(&mut *blocker).await?;
        let deadline = Instant::now() + Duration::from_secs(9);
        let mut task = fixture.spawn_refresh(
            fixture.generation,
            PendingGraphSummaryRefresh::targeted(fixture.node_ids.clone(), Vec::new()),
        );
        let observed = wait_for_backend_lock(
            &fixture.observer, fixture.backend_pid, "insert into runtime_graph_",
        ).await;
        let completed = tokio::time::timeout_at(deadline, &mut task).await;
        // Release even on a pre-fix timeout before propagating any test failure.
        blocker.rollback().await?;
        if completed.is_err() {
            fixture.stop_refresh(&mut task).await?;
            bail!("production summary refresh did not return before the outer nine-second deadline");
        }
        observed?;
        let outcome = completed.context("summary task exceeded outer deadline")?
            .context("summary task panicked")?;
        assert!(outcome.is_err(), "production must return an error rather than commit partial work");
        let ordinal: i64 = sqlx::query_scalar("select last_value from summary_insert_progress")
            .fetch_one(&fixture.observer).await?;
        assert_eq!(ordinal, 2, "the failure must occur after the first write, on the second insert");
        assert_eq!(fixture.summaries().await?, before);
        fixture.assert_publication_lock_available().await?;
        Ok(())
    }.await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn published_summary_generation_race_preserves_pre_refresh_rows() -> Result<()> {
    let fixture = SummaryExecutorFixture::create().await?;
    let result = async {
        fixture.seed_previous_summaries().await?;
        let before = fixture.summaries().await?;
        let mut publisher = fixture.observer.begin().await?;
        sqlx::query("select id from catalog_library where id = $1 for update")
            .bind(fixture.library_id)
            .execute(&mut *publisher)
            .await?;
        let mut task =
            fixture.spawn_refresh(fixture.generation, PendingGraphSummaryRefresh::broad());
        let observed =
            wait_for_backend_lock(&fixture.observer, fixture.backend_pid, "for share").await;
        sqlx::query("update catalog_library set source_truth_version = $2 where id = $1")
            .bind(fixture.library_id)
            .bind(fixture.generation + 1)
            .execute(&mut *publisher)
            .await?;
        publisher.commit().await?;
        let completed = tokio::time::timeout(Duration::from_secs(9), &mut task).await;
        if completed.is_err() {
            fixture.stop_refresh(&mut task).await?;
            bail!("generation-race refresh did not finish");
        }
        observed?;
        let outcome = completed
            .context("generation-race outer deadline")?
            .context("generation-race task panicked")??;
        assert_eq!(
            outcome,
            PublishedGraphSummaryRefreshOutcome::GenerationMismatch {
                observed_source_truth_version: fixture.generation + 1,
            }
        );
        assert_eq!(fixture.summaries().await?, before);
        fixture.assert_publication_lock_available().await?;
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn published_summary_actual_candidate_cap_skips_all_broad_regeneration_but_allows_targeted()
-> Result<()> {
    let fixture = SummaryExecutorFixture::create().await?;
    let result = async {
        fixture.seed_previous_summaries().await?;
        sqlx::query(
            "insert into runtime_graph_snapshot
             (library_id, graph_status, projection_version, node_count, edge_count)
              values ($1, 'ready', 1, 1, 0)",
        ).bind(fixture.library_id).execute(fixture.pool()).await?;
        // 300 nodes plus their supported chain exceed 500 combined identities,
        // while neither side individually does. A positive but understated
        // snapshot must not admit an unrestricted regeneration of this graph.
        sqlx::query(
            "insert into runtime_graph_node
             (library_id, canonical_key, label, node_type, summary, projection_version)
             select $1, 'unrelated:' || n, 'Unrelated entity ' || n, 'entity',
                    'Reusable unrelated summary', 1 from generate_series(1, 300) n",
        ).bind(fixture.library_id).execute(fixture.pool()).await?;
        sqlx::query(
            "insert into runtime_graph_edge
             (library_id, canonical_key, from_node_id, to_node_id, relation_type, summary, projection_version)
             select $1, 'unrelated-edge:' || id::text, id, next_id, 'relates_to',
                    'Reusable unrelated relation', 1
             from (select id, lead(id) over (order by id) next_id
                   from runtime_graph_node where library_id = $1 and canonical_key like 'unrelated:%') nodes
             where next_id is not null",
        ).bind(fixture.library_id).execute(fixture.pool()).await?;
        sqlx::query(
            "insert into runtime_graph_evidence
             (library_id, evidence_identity_key, target_kind, target_id, document_id, revision_id, evidence_text)
             select $1, 'unrelated:' || kind || ':' || id::text, kind, id, $2, $3, 'Current source evidence'
             from (select 'node'::text kind, id from runtime_graph_node
                   where library_id = $1 and canonical_key like 'unrelated:%'
                   union all select 'edge'::text, id from runtime_graph_edge
                   where library_id = $1 and canonical_key like 'unrelated-edge:%') targets",
        ).bind(fixture.library_id).bind(fixture.document_id).bind(fixture.revision_id)
        .execute(fixture.pool()).await?;
        let admitted = repositories::count_admitted_runtime_graph_projection(
            fixture.pool(), fixture.library_id, 1,
        ).await?;
        assert!(admitted.node_count + admitted.edge_count > 500);
        assert!(admitted.node_count < 501 && admitted.edge_count < 501);
        let broad = PendingGraphSummaryRefresh::broad();
        let outcome = tokio::time::timeout(Duration::from_secs(9),
            fixture.state.application_services.graph.apply_published_summary_refresh(
                &fixture.state, fixture.library_id, fixture.generation, &broad,
            ),
        ).await.context("broad summary refresh exceeded outer deadline")??;
        assert_eq!(outcome, PublishedGraphSummaryRefreshOutcome::Applied { invalidated: 3, refreshed: 0 });
        assert!(fixture.summaries().await?.is_empty(), "over-cap broad refresh must not regenerate a prefix");
        sqlx::query("update runtime_graph_snapshot set node_count = 0, edge_count = 0 where library_id = $1")
            .bind(fixture.library_id).execute(fixture.pool()).await?;
        let zero_count_outcome = tokio::time::timeout(Duration::from_secs(9),
            fixture.state.application_services.graph.apply_published_summary_refresh(
                &fixture.state, fixture.library_id, fixture.generation, &broad,
            ),
        ).await.context("zero-counter broad refresh exceeded outer deadline")??;
        assert_eq!(zero_count_outcome, PublishedGraphSummaryRefreshOutcome::Applied { invalidated: 0, refreshed: 0 });
        assert!(fixture.summaries().await?.is_empty());
        let targeted = PendingGraphSummaryRefresh::targeted(
            vec![fixture.node_ids[0]], vec![fixture.edge_id],
        );
        let outcome = tokio::time::timeout(Duration::from_secs(9),
            fixture.state.application_services.graph.apply_published_summary_refresh(
                &fixture.state, fixture.library_id, fixture.generation, &targeted,
            ),
        ).await.context("small targeted refresh exceeded outer deadline")??;
        assert_eq!(outcome, PublishedGraphSummaryRefreshOutcome::Applied { invalidated: 0, refreshed: 2 });
        let rows = fixture.summaries().await?;
        assert_eq!(rows.len(), 2);
        let mut targets = rows.iter().map(|row| {
            (row["target_kind"].as_str().unwrap_or_default().to_string(), row["target_id"].as_str().unwrap_or_default().to_string())
        }).collect::<Vec<_>>();
        targets.sort_unstable();
        assert_eq!(targets, vec![
            ("edge".to_string(), fixture.edge_id.to_string()),
            ("node".to_string(), fixture.node_ids[0].to_string()),
        ]);
        fixture.assert_publication_lock_available().await?;
        Ok(())
    }.await;
    fixture.cleanup().await?;
    result
}
