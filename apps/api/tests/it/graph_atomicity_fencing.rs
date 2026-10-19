//! PostgreSQL integration coverage for ingest-authorized graph publication.
//!
//! Run with:
//! `cargo test -p ironrag-backend --test it -- --ignored graph_atomicity_fencing::`

use crate::{postgres_activity, temp_postgres_database};

use std::time::Duration as StdDuration;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use ironrag_backend::{
    app::{config::Settings, state::AppState},
    domains::agent_runtime::{RuntimeExecutionOwnerKind, RuntimeLifecycleState, RuntimeTaskKind},
    infra::{
        knowledge_plane::{IngestAttemptMutationOutcome, IngestRevisionWriteAuthority},
        persistence::Persistence,
        repositories::{
            catalog_repository, content_chunk_repository, content_document_repository,
            content_knowledge_projection_repository, content_revision_repository,
            ingest_repository, query_repository, runtime_graph_repository, runtime_repository,
        },
    },
    services::graph::{error::GraphServiceError, service::GraphService},
};
use postgres_activity::wait_for_backend_lock;
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool, postgres::PgPoolOptions};
use temp_postgres_database::TempPostgresDatabase;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const GRAPH_EXTRACTION_VERSION: &str = "graph_extract";
const FIXTURE_TEXT: &str = concat!(
    "A neutral record connects reusable entities through explicit structural evidence. ",
    "The record is long enough to exercise the graph reconciliation path deterministically."
);

struct PreparedRevision {
    revision_id: Uuid,
    chunk_id: Uuid,
}

struct GraphAttempt {
    job_id: Uuid,
    attempt_id: Uuid,
    authority: IngestRevisionWriteAuthority,
}

struct GraphAtomicityFixture {
    database: TempPostgresDatabase,
    state: AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    chunk_id: Uuid,
}

impl GraphAtomicityFixture {
    async fn create(label: &str) -> Result<Self> {
        let mut settings = Settings::from_env().context("load graph atomicity settings")?;
        let database =
            TempPostgresDatabase::create(&settings.database_url, "graph_atomicity").await?;
        settings.database_url = database.database_url.clone();
        settings.credential_master_key =
            Some("KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=".to_string());

        let postgres = PgPoolOptions::new()
            .max_connections(12)
            .connect(&settings.database_url)
            .await
            .context("connect graph atomicity postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("apply graph atomicity migrations")?;
        let redis = redis::Client::open(settings.redis_url.clone())
            .context("create graph atomicity redis client")?;
        let state = AppState::from_dependencies(settings, Persistence::for_tests(postgres, redis))?;

        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            &state.persistence.postgres,
            &format!("graph-atomicity-workspace-{suffix}"),
            "Graph Atomicity Workspace",
            None,
        )
        .await?;
        let library = catalog_repository::create_library(
            &state.persistence.postgres,
            workspace.id,
            &format!("graph-atomicity-library-{suffix}"),
            "Graph Atomicity Library",
            None,
            None,
        )
        .await?;
        let document = content_knowledge_projection_repository::create_document_with_projection(
            &state.persistence.postgres,
            &content_document_repository::NewContentDocument {
                workspace_id: workspace.id,
                library_id: library.id,
                external_key: &format!("graph-atomicity-document-{suffix}"),
                document_state: "active",
                created_by_principal_id: None,
                parent_external_key: None,
                parent_document_id: None,
                document_role: "primary",
            },
            Some("graph-atomicity.txt"),
        )
        .await?;
        let prepared = create_prepared_revision(
            &state.persistence.postgres,
            workspace.id,
            library.id,
            document.id,
            label,
        )
        .await?;
        publish_readable_head(&state.persistence.postgres, document.id, prepared.revision_id)
            .await?;

        Ok(Self {
            database,
            state,
            workspace_id: workspace.id,
            library_id: library.id,
            document_id: document.id,
            revision_id: prepared.revision_id,
            chunk_id: prepared.chunk_id,
        })
    }

    fn pool(&self) -> &PgPool {
        &self.state.persistence.postgres
    }

    async fn create_replacement_revision(&self, label: &str) -> Result<PreparedRevision> {
        let prepared = create_prepared_revision(
            self.pool(),
            self.workspace_id,
            self.library_id,
            self.document_id,
            label,
        )
        .await?;
        publish_readable_head(self.pool(), self.document_id, prepared.revision_id).await?;
        Ok(prepared)
    }

    async fn create_graph_attempt(
        &self,
        revision_id: Uuid,
        chunk_id: Uuid,
        label: &str,
        entities: &[&str],
    ) -> Result<GraphAttempt> {
        create_graph_attempt(GraphAttemptFixture {
            pool: self.pool(),
            workspace_id: self.workspace_id,
            library_id: self.library_id,
            document_id: self.document_id,
            revision_id,
            chunk_id,
            label,
            entities,
        })
        .await
    }

    async fn reconcile(
        &self,
        attempt: &GraphAttempt,
        cancellation_token: &CancellationToken,
    ) -> Result<ironrag_backend::services::graph::service::AuthorizedRevisionGraphReconcile> {
        match GraphService::new()
            .reconcile_revision_graph(
                &self.state,
                attempt.authority,
                self.document_id,
                cancellation_token,
            )
            .await?
        {
            IngestAttemptMutationOutcome::Applied(authorized) => Ok(authorized),
            IngestAttemptMutationOutcome::AuthorityLost => {
                bail!("graph reconcile unexpectedly lost exact attempt authority")
            }
        }
    }

    async fn reconcile_and_publish(&self, attempt: &GraphAttempt) -> Result<usize> {
        let authorized = self.reconcile(attempt, &CancellationToken::new()).await?;
        let outcome = authorized.outcome?;
        let contribution = outcome.graph_contribution_count;
        let completed_at = Utc::now();
        let publication = ingest_repository::publish_graph_refresh_success_with_graph_guard(
            authorized.publication_guard,
            &graph_publication_command(self, attempt, completed_at),
            &ingest_repository::GraphRefreshPublicationEffects {
                terminal_stage_events: vec![terminal_graph_event(
                    attempt.attempt_id,
                    contribution,
                    completed_at,
                )],
            },
        )
        .await?;
        assert!(matches!(
            publication,
            ingest_repository::PublishGraphRefreshSuccessOutcome::Applied { .. }
        ));
        Ok(contribution)
    }

    async fn query_reference_count(&self) -> Result<i64> {
        sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from query_chunk_reference as reference
             join content_chunk as chunk on chunk.id = reference.chunk_id
             where chunk.revision_id = $1",
        )
        .bind(self.revision_id)
        .fetch_one(self.pool())
        .await
        .map_err(Into::into)
    }

    async fn append_query_reference(&self, chunk_id: Uuid) -> Result<Uuid> {
        let conversation = query_repository::create_conversation(
            self.pool(),
            &query_repository::NewQueryConversation {
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                created_by_principal_id: None,
                title: Some("Graph atomicity reference"),
                conversation_state: "active",
                request_surface: "ui",
            },
            10,
        )
        .await?;
        let execution_id = Uuid::now_v7();
        let runtime_execution_id = Uuid::now_v7();
        runtime_repository::create_runtime_execution(
            self.pool(),
            &runtime_repository::NewRuntimeExecution {
                id: runtime_execution_id,
                owner_kind: RuntimeExecutionOwnerKind::QueryExecution.as_str(),
                owner_id: execution_id,
                task_kind: RuntimeTaskKind::QueryAnswer.as_str(),
                surface_kind: "ui",
                contract_name: "graph_atomicity_query",
                contract_version: "test",
                lifecycle_state: RuntimeLifecycleState::Running.as_str(),
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
        query_repository::create_execution(
            self.pool(),
            &query_repository::NewQueryExecution {
                execution_id,
                context_bundle_id: Uuid::now_v7(),
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                conversation_id: conversation.id,
                request_turn_id: None,
                response_turn_id: None,
                binding_id: None,
                runtime_execution_id,
                query_text: "neutral structural lookup",
                failure_code: None,
            },
        )
        .await?;
        let inserted = query_repository::append_chunk_references(
            self.pool(),
            execution_id,
            &[query_repository::NewQueryChunkReference { chunk_id, rank: 1, score: 1.0 }],
        )
        .await?;
        anyhow::ensure!(inserted == 1, "query reference fixture was not inserted");
        Ok(execution_id)
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.database.drop().await
    }
}

async fn create_prepared_revision(
    pool: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    label: &str,
) -> Result<PreparedRevision> {
    let checksum = format!("sha256:{0}{0}", Uuid::now_v7().simple());
    let revision = match content_revision_repository::create_revision_with_projection(
        pool,
        &content_revision_repository::NewContentRevisionProjection {
            admission_identity: Some(&crate::content_repository::synthetic_import_admission(
                ironrag_contracts::documents::ReplacementIdentity {
                    checksum: checksum.clone(),
                    mime_type: "text/plain".to_owned(),
                    byte_size: i64::try_from(FIXTURE_TEXT.len())?,
                    file_name: "graph-source.txt".to_owned(),
                    title: Some(label.to_owned()),
                    language_code: None,
                    source_uri: None,
                    document_hint: None,
                },
            )?),
            document_id,
            workspace_id,
            library_id,
            content_source_kind: "upload",
            checksum: &checksum,
            mime_type: "text/plain",
            byte_size: i64::try_from(FIXTURE_TEXT.len()).unwrap_or(i64::MAX),
            title: Some(label),
            language_code: None,
            source_uri: None,
            document_hint: None,
            storage_key: None,
            created_by_principal_id: None,
        },
    )
    .await?
    {
        content_revision_repository::CreateContentRevisionOutcome::Created(revision) => *revision,
        other => bail!("prepared revision fixture was not created: {other:?}"),
    };
    let text_checksum = format!("sha256:text:{label}:{}", revision.id);
    let chunks = content_chunk_repository::replace_chunks_with_projection(
        pool,
        revision.id,
        &[content_chunk_repository::NewContentChunkProjection {
            workspace_id,
            library_id,
            document_id,
            revision_id: revision.id,
            chunk_index: 0,
            start_offset: 0,
            end_offset: i32::try_from(FIXTURE_TEXT.len()).unwrap_or(i32::MAX),
            token_count: Some(24),
            chunk_kind: Some("paragraph".to_string()),
            content_text: FIXTURE_TEXT.to_string(),
            normalized_text: FIXTURE_TEXT.to_string(),
            text_checksum: text_checksum.clone(),
            support_block_ids: Vec::new(),
            section_path: Vec::new(),
            heading_trail: Vec::new(),
            literal_digest: None,
            chunk_state: "ready".to_string(),
            text_generation: Some(i64::from(revision.revision_number)),
            vector_generation: Some(i64::from(revision.revision_number)),
            quality_score: Some(1.0),
            window_text: None,
            occurred_at: None,
            occurred_until: None,
        }],
    )
    .await?;
    let chunk = chunks.first().context("prepared revision chunk missing")?;
    sqlx::query(
        "update knowledge_revision
         set revision_state = 'ready',
             normalized_text = $2,
             text_checksum = $3,
             text_state = 'text_readable',
             vector_state = 'ready',
             graph_state = 'graph_degraded',
             text_readable_at = now(),
             vector_ready_at = now()
         where revision_id = $1",
    )
    .bind(revision.id)
    .bind(FIXTURE_TEXT)
    .bind(&text_checksum)
    .execute(pool)
    .await?;
    sqlx::query(
        "insert into knowledge_structured_revision (
            revision_id, workspace_id, library_id, document_id,
            preparation_state, normalization_profile, source_format, language_code,
            block_count, chunk_count, typed_fact_count, outline_json,
            prepared_at, updated_at
         ) values (
            $1, $2, $3, $4,
            'prepared', 'graph-atomicity-v1', 'text', null,
            0, 1, 0, '[]'::jsonb,
            now(), now()
         )",
    )
    .bind(revision.id)
    .bind(workspace_id)
    .bind(library_id)
    .bind(document_id)
    .execute(pool)
    .await?;
    Ok(PreparedRevision { revision_id: revision.id, chunk_id: chunk.id })
}

async fn publish_readable_head(pool: &PgPool, document_id: Uuid, revision_id: Uuid) -> Result<()> {
    crate::graph_reconcile_support::publish_readable_head(pool, document_id, revision_id).await
}

// The fixture spells out every persisted scope key so a test cannot
// accidentally inherit authority from mutable ambient state.
struct GraphAttemptFixture<'a, 'b> {
    pool: &'a PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    chunk_id: Uuid,
    label: &'a str,
    entities: &'a [&'b str],
}

async fn create_graph_attempt(input: GraphAttemptFixture<'_, '_>) -> Result<GraphAttempt> {
    let GraphAttemptFixture {
        pool,
        workspace_id,
        library_id,
        document_id,
        revision_id,
        chunk_id,
        label,
        entities,
    } = input;
    let job = crate::graph_reconcile_support::create_leased_ingest_job(
        pool,
        crate::graph_reconcile_support::LeasedIngestJobSeed {
            workspace_id,
            library_id,
            document_id,
            revision_id,
            job_kind: "graph_refresh",
            dedupe_key_prefix: &format!("graph-atomicity:{label}"),
        },
    )
    .await?;
    let attempt =
        crate::graph_reconcile_support::create_leased_graph_attempt(pool, job.id, label).await?;
    let runtime_execution_id = Uuid::now_v7();
    runtime_repository::create_runtime_execution(
        pool,
        &runtime_repository::NewRuntimeExecution {
            id: runtime_execution_id,
            owner_kind: RuntimeExecutionOwnerKind::GraphExtractionAttempt.as_str(),
            owner_id: attempt.id,
            task_kind: RuntimeTaskKind::GraphExtract.as_str(),
            surface_kind: "worker",
            contract_name: "graph_atomicity_extract",
            contract_version: "test",
            lifecycle_state: RuntimeLifecycleState::Running.as_str(),
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
    let normalized_entities = entities
        .iter()
        .map(|entity| {
            json!({
                "label": entity,
                "node_type": "concept",
                "sub_type": null,
                "aliases": [],
                "summary": null,
            })
        })
        .collect::<Vec<_>>();
    runtime_graph_repository::create_runtime_graph_extraction_record(
        pool,
        &runtime_graph_repository::CreateRuntimeGraphExtractionRecordInput {
            id: Uuid::now_v7(),
            runtime_execution_id,
            library_id,
            document_id,
            chunk_id,
            provider_kind: "synthetic".to_string(),
            model_name: "fixture".to_string(),
            extraction_version: GRAPH_EXTRACTION_VERSION.to_string(),
            prompt_hash: format!("sha256:{label}"),
            status: "ready".to_string(),
            raw_output_json: json!({
                "lifecycle": {
                    "revision_id": revision_id,
                    "activated_by_attempt_id": attempt.id,
                }
            }),
            normalized_output_json: json!({
                "entities": normalized_entities,
                "relations": [],
            }),
            glean_pass_count: 0,
            error_message: None,
        },
    )
    .await?;
    Ok(GraphAttempt {
        job_id: job.id,
        attempt_id: attempt.id,
        authority: IngestRevisionWriteAuthority {
            workspace_id,
            library_id,
            attempt_id: attempt.id,
            revision_id,
        },
    })
}

fn graph_publication_command(
    fixture: &GraphAtomicityFixture,
    attempt: &GraphAttempt,
    completed_at: chrono::DateTime<Utc>,
) -> ingest_repository::PublishGraphRefreshSuccess {
    ingest_repository::PublishGraphRefreshSuccess {
        workspace_id: fixture.workspace_id,
        library_id: fixture.library_id,
        document_id: fixture.document_id,
        revision_id: attempt.authority.revision_id,
        attempt_id: attempt.attempt_id,
        graph_state: "ready".to_string(),
        graph_ready_at: Some(completed_at),
        completed_at,
    }
}

fn terminal_graph_event(
    attempt_id: Uuid,
    graph_contribution_count: usize,
    recorded_at: chrono::DateTime<Utc>,
) -> ingest_repository::NewIngestStageEvent {
    ingest_repository::NewIngestStageEvent {
        attempt_id,
        stage_name: "extract_graph".to_string(),
        stage_state: "completed".to_string(),
        ordinal: 999,
        message: None,
        details_json: json!({
            "extractionVersion": GRAPH_EXTRACTION_VERSION,
            "graphContributionCount": graph_contribution_count,
        }),
        recorded_at: Some(recorded_at),
        provider_kind: None,
        model_name: None,
        prompt_tokens: None,
        completion_tokens: None,
        total_tokens: None,
        cached_tokens: None,
        estimated_cost: None,
        currency_code: None,
        elapsed_ms: Some(1),
    }
}

fn ordinary_graph_event(attempt_id: Uuid) -> ingest_repository::NewIngestStageEvent {
    ingest_repository::NewIngestStageEvent {
        attempt_id,
        stage_name: "extract_graph".to_string(),
        stage_state: "started".to_string(),
        ordinal: 777,
        message: None,
        details_json: json!({"checkpoint": "ordinary-writer"}),
        recorded_at: Some(Utc::now()),
        provider_kind: None,
        model_name: None,
        prompt_tokens: None,
        completion_tokens: None,
        total_tokens: None,
        cached_tokens: None,
        estimated_cost: None,
        currency_code: None,
        elapsed_ms: Some(1),
    }
}

#[derive(Debug, PartialEq)]
struct GraphDigest {
    head: (Option<Uuid>, Option<Uuid>),
    nodes: Vec<GraphNodeDigest>,
    edges: Vec<GraphEdgeDigest>,
    evidence: Vec<GraphEvidenceDigest>,
    snapshot: Option<GraphSnapshotDigest>,
    source_truth_version: i64,
    graph_state: String,
    job_state: String,
    attempt_state: String,
    stage_events: Vec<(String, String, i32, Value)>,
    revision_query_references: i64,
}

type GraphNodeDigest = (Uuid, String, String, i32, i64);
type GraphEdgeDigest = (Uuid, String, i32, i64);
type GraphEvidenceDigest = (Uuid, String, Uuid, Option<Uuid>, Option<Uuid>, Option<Uuid>);
type GraphSnapshotDigest = (String, i64, i32, i32, Option<f64>, i64, Uuid);

async fn graph_digest(
    fixture: &GraphAtomicityFixture,
    job_id: Uuid,
    attempt_id: Uuid,
    revision_id: Uuid,
) -> Result<GraphDigest> {
    let head = sqlx::query_as::<_, (Option<Uuid>, Option<Uuid>)>(
        "select active_revision_id, readable_revision_id
         from content_document_head
         where document_id = $1",
    )
    .bind(fixture.document_id)
    .fetch_one(fixture.pool())
    .await?;
    let nodes = sqlx::query_as::<_, GraphNodeDigest>(
        "select id, label, node_type, support_count, projection_version
         from runtime_graph_node
         where library_id = $1
         order by id",
    )
    .bind(fixture.library_id)
    .fetch_all(fixture.pool())
    .await?;
    let edges = sqlx::query_as::<_, GraphEdgeDigest>(
        "select id, canonical_key, support_count, projection_version
         from runtime_graph_edge
         where library_id = $1
         order by id",
    )
    .bind(fixture.library_id)
    .fetch_all(fixture.pool())
    .await?;
    let evidence = sqlx::query_as::<_, GraphEvidenceDigest>(
        "select id, target_kind, target_id, revision_id, activated_by_attempt_id, chunk_id
         from runtime_graph_evidence
         where library_id = $1
         order by id",
    )
    .bind(fixture.library_id)
    .fetch_all(fixture.pool())
    .await?;
    let snapshot = sqlx::query_as::<_, GraphSnapshotDigest>(
        "select graph_status, projection_version, node_count, edge_count,
                provenance_coverage_percent, topology_generation, build_epoch
         from runtime_graph_snapshot
         where library_id = $1",
    )
    .bind(fixture.library_id)
    .fetch_optional(fixture.pool())
    .await?;
    let source_truth_version =
        catalog_repository::get_library_source_truth_version(fixture.pool(), fixture.library_id)
            .await?;
    let graph_state = sqlx::query_scalar::<_, String>(
        "select graph_state from knowledge_revision where revision_id = $1",
    )
    .bind(revision_id)
    .fetch_one(fixture.pool())
    .await?;
    let job_state =
        sqlx::query_scalar::<_, String>("select queue_state::text from ingest_job where id = $1")
            .bind(job_id)
            .fetch_one(fixture.pool())
            .await?;
    let attempt_state = sqlx::query_scalar::<_, String>(
        "select attempt_state::text from ingest_attempt where id = $1",
    )
    .bind(attempt_id)
    .fetch_one(fixture.pool())
    .await?;
    let stage_events = sqlx::query_as::<_, (String, String, i32, Value)>(
        "select stage_name, stage_state::text, ordinal, details_json
         from ingest_stage_event
         where attempt_id = $1
         order by ordinal, id",
    )
    .bind(attempt_id)
    .fetch_all(fixture.pool())
    .await?;
    let revision_query_references = sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from query_chunk_reference as reference
         join content_chunk as chunk on chunk.id = reference.chunk_id
         where chunk.revision_id = $1",
    )
    .bind(revision_id)
    .fetch_one(fixture.pool())
    .await?;
    Ok(GraphDigest {
        head,
        nodes,
        edges,
        evidence,
        snapshot,
        source_truth_version,
        graph_state,
        job_state,
        attempt_state,
        stage_events,
        revision_query_references,
    })
}

#[derive(Clone, Copy)]
enum ConcurrentJobTransition {
    Cancel,
    Pause,
}

impl ConcurrentJobTransition {
    async fn apply(self, pool: PgPool, job_id: Uuid) -> Result<()> {
        match self {
            Self::Cancel => {
                let canceled = tokio::time::timeout(StdDuration::from_secs(2), async move {
                    ingest_repository::cancel_ingest_job(&pool, job_id).await
                })
                .await
                .context("database cancellation blocked behind the graph publication guard")??;
                assert_eq!(canceled, 1);
            }
            Self::Pause => {
                let paused = tokio::time::timeout(StdDuration::from_secs(2), async move {
                    ingest_repository::pause_ingest_job(&pool, job_id).await
                })
                .await
                .context("database pause blocked behind the graph publication guard")??;
                assert_eq!(paused, Some(()));
            }
        }
        Ok(())
    }

    fn expected_states(self) -> (&'static str, &'static str) {
        match self {
            Self::Cancel => ("canceled", "canceled"),
            Self::Pause => ("paused", "leased"),
        }
    }
}

async fn assert_database_transition_beats_guarded_publication(
    fixture: &GraphAtomicityFixture,
    label: &str,
    entity: &str,
    transition: ConcurrentJobTransition,
) -> Result<()> {
    let attempt = fixture
        .create_graph_attempt(fixture.revision_id, fixture.chunk_id, label, &[entity])
        .await?;
    let before =
        graph_digest(fixture, attempt.job_id, attempt.attempt_id, fixture.revision_id).await?;
    let authorized = fixture.reconcile(&attempt, &CancellationToken::new()).await?;
    let reconcile = authorized.outcome?;
    assert!(reconcile.projection.has_materialized_graph());

    transition.apply(fixture.pool().clone(), attempt.job_id).await?;

    let completed_at = Utc::now();
    let publication = ingest_repository::publish_graph_refresh_success_with_graph_guard(
        authorized.publication_guard,
        &graph_publication_command(fixture, &attempt, completed_at),
        &ingest_repository::GraphRefreshPublicationEffects {
            terminal_stage_events: vec![terminal_graph_event(
                attempt.attempt_id,
                reconcile.graph_contribution_count,
                completed_at,
            )],
        },
    )
    .await?;
    assert!(matches!(
        publication,
        ingest_repository::PublishGraphRefreshSuccessOutcome::AuthorityLost { .. }
    ));

    let after =
        graph_digest(fixture, attempt.job_id, attempt.attempt_id, fixture.revision_id).await?;
    assert_graph_projection_unchanged(&before, &after);
    let (expected_job_state, expected_attempt_state) = transition.expected_states();
    assert_eq!(after.job_state, expected_job_state);
    assert_eq!(after.attempt_state, expected_attempt_state);
    Ok(())
}

fn assert_graph_projection_unchanged(before: &GraphDigest, after: &GraphDigest) {
    assert_eq!(after.nodes, before.nodes);
    assert_eq!(after.edges, before.edges);
    assert_eq!(after.evidence, before.evidence);
    assert_eq!(after.snapshot, before.snapshot);
    assert_eq!(after.source_truth_version, before.source_truth_version);
    assert_eq!(after.head, before.head);
}

#[derive(Debug, PartialEq, Eq)]
struct SemanticGraphProjection {
    nodes: Vec<(String, String, i32)>,
    edges: Vec<(String, String, String, i32)>,
    evidence: Vec<(String, String, i64)>,
}

async fn semantic_graph_projection(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<SemanticGraphProjection> {
    let nodes = sqlx::query_as::<_, (String, String, i32)>(
        "select label, node_type, support_count
         from runtime_graph_node
         where library_id = $1
         order by label, node_type",
    )
    .bind(library_id)
    .fetch_all(pool)
    .await?;
    let edges = sqlx::query_as::<_, (String, String, String, i32)>(
        "select source.label, edge.relation_type, target.label, edge.support_count
         from runtime_graph_edge as edge
         join runtime_graph_node as source on source.id = edge.from_node_id
         join runtime_graph_node as target on target.id = edge.to_node_id
         where edge.library_id = $1
         order by source.label, edge.relation_type, target.label",
    )
    .bind(library_id)
    .fetch_all(pool)
    .await?;
    let evidence = sqlx::query_as::<_, (String, String, i64)>(
        "with evidence_target as (
            select evidence.target_kind,
                   coalesce(
                       case evidence.target_kind
                           when 'node' then node.label
                           when 'edge' then edge.canonical_key
                       end,
                       '<missing-target>'
                   ) as target_label
            from runtime_graph_evidence as evidence
            left join runtime_graph_node as node
              on evidence.target_kind = 'node' and node.id = evidence.target_id
            left join runtime_graph_edge as edge
              on evidence.target_kind = 'edge' and edge.id = evidence.target_id
            where evidence.library_id = $1
         )
         select target_kind, target_label, count(*)::bigint
         from evidence_target
         group by target_kind, target_label
         order by target_kind, target_label",
    )
    .bind(library_id)
    .fetch_all(pool)
    .await?;
    Ok(SemanticGraphProjection { nodes, edges, evidence })
}

fn assert_attempt_entities(
    projection: &SemanticGraphProjection,
    absent_label: &str,
    shared_label: &str,
    current_label: &str,
) {
    assert!(
        projection.nodes.iter().all(|(label, _, _)| label != absent_label),
        "superseded attempt-only node survived: {projection:#?}"
    );
    assert!(
        projection
            .edges
            .iter()
            .all(|(source, _, target, _)| source != absent_label && target != absent_label),
        "superseded attempt-only edge survived: {projection:#?}"
    );
    for label in [shared_label, current_label] {
        let rows = projection
            .nodes
            .iter()
            .filter(|(candidate, kind, _)| candidate == label && kind == "concept")
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1, "expected one current entity node for {label}");
        assert_eq!(rows[0].2, 1, "entity support must be recomputed, not accumulated");
        let mention_edges = projection
            .edges
            .iter()
            .filter(|(_, relation, target, _)| relation == "mentions" && target == label)
            .collect::<Vec<_>>();
        assert_eq!(mention_edges.len(), 1, "expected one mention edge for {label}");
        assert_eq!(mention_edges[0].3, 1, "edge support must not double on retry");
    }
    assert!(
        projection.evidence.iter().all(|(_, target, _)| !target.contains(absent_label)),
        "superseded attempt evidence survived: {projection:#?}"
    );
    assert!(
        projection.evidence.iter().all(|(_, target, _)| target != "<missing-target>"),
        "graph evidence must never dangle after retry pruning: {projection:#?}"
    );
    for label in [shared_label, current_label] {
        assert!(
            projection
                .evidence
                .iter()
                .any(|(kind, target, count)| kind == "node" && target == label && *count == 1),
            "expected one current node-evidence row for {label}: {projection:#?}"
        );
    }
}

#[tokio::test]
#[ignore = "requires local postgres with the pgvector extensions"]
async fn same_revision_retry_replaces_evidence_prunes_orphans_and_is_idempotent() -> Result<()> {
    let fixture = GraphAtomicityFixture::create("same-revision").await?;
    let result = async {
        let _execution_id = fixture.append_query_reference(fixture.chunk_id).await?;
        let attempt_a = fixture
            .create_graph_attempt(
                fixture.revision_id,
                fixture.chunk_id,
                "attempt-a",
                &["Attempt Alpha Unique", "Shared Entity"],
            )
            .await?;
        assert!(fixture.reconcile_and_publish(&attempt_a).await? > 0);
        assert_eq!(fixture.query_reference_count().await?, 1);

        let attempt_b = fixture
            .create_graph_attempt(
                fixture.revision_id,
                fixture.chunk_id,
                "attempt-b",
                &["Attempt Beta Unique", "Shared Entity"],
            )
            .await?;
        assert!(fixture.reconcile_and_publish(&attempt_b).await? > 0);
        let after_b = semantic_graph_projection(fixture.pool(), fixture.library_id).await?;
        assert_attempt_entities(
            &after_b,
            "Attempt Alpha Unique",
            "Shared Entity",
            "Attempt Beta Unique",
        );
        assert_eq!(
            fixture.query_reference_count().await?,
            1,
            "same-revision retry must preserve pre-existing query chunk references"
        );

        let retry_b = fixture
            .create_graph_attempt(
                fixture.revision_id,
                fixture.chunk_id,
                "attempt-b-retry",
                &["Attempt Beta Unique", "Shared Entity"],
            )
            .await?;
        assert!(fixture.reconcile_and_publish(&retry_b).await? > 0);
        let after_retry_b = semantic_graph_projection(fixture.pool(), fixture.library_id).await?;
        assert_eq!(
            after_retry_b, after_b,
            "semantic B retry must be idempotent across nodes, edges, evidence, and support"
        );
        assert_eq!(fixture.query_reference_count().await?, 1);
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with the pgvector extensions"]
async fn publication_error_after_terminal_snapshot_rolls_back_graph_and_lifecycle() -> Result<()> {
    let fixture = GraphAtomicityFixture::create("publication-rollback").await?;
    let result = async {
        let attempt = fixture
            .create_graph_attempt(
                fixture.revision_id,
                fixture.chunk_id,
                "publication-rollback",
                &["Rollback Entity"],
            )
            .await?;
        let before = graph_digest(
            &fixture,
            attempt.job_id,
            attempt.attempt_id,
            fixture.revision_id,
        )
        .await?;
        let authorized = fixture.reconcile(&attempt, &CancellationToken::new()).await?;
        let reconcile = authorized.outcome?;
        assert!(
            reconcile.projection.has_materialized_graph(),
            "the test must reach a terminal ready snapshot write before injecting failure"
        );
        let mut publication_guard = authorized.publication_guard;
        let staged_snapshot = runtime_graph_repository::get_runtime_graph_snapshot_with_executor(
            publication_guard.connection(),
            fixture.library_id,
        )
        .await?
        .context("terminal graph snapshot must exist inside the guarded transaction")?;
        assert_eq!(staged_snapshot.graph_status, "ready");
        assert!(staged_snapshot.node_count > 0);

        let completed_at = Utc::now();
        let publication = ingest_repository::publish_graph_refresh_success_with_graph_guard(
            publication_guard,
            &graph_publication_command(&fixture, &attempt, completed_at),
            &ingest_repository::GraphRefreshPublicationEffects {
                terminal_stage_events: vec![terminal_graph_event(
                    Uuid::now_v7(),
                    reconcile.graph_contribution_count,
                    completed_at,
                )],
            },
        )
        .await;
        let error = match publication {
            Err(error) => error,
            Ok(_) => bail!("mismatched terminal event must abort guarded publication"),
        };
        assert!(error.to_string().contains("attempt mismatch"));

        let after = graph_digest(
            &fixture,
            attempt.job_id,
            attempt.attempt_id,
            fixture.revision_id,
        )
        .await?;
        assert_eq!(
            after, before,
            "nodes, edges, evidence, snapshot, source generation, lifecycle, and event must roll back together"
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with the pgvector extensions"]
async fn cancellation_after_reconcile_explicitly_rolls_back_before_worker_exit() -> Result<()> {
    let fixture = GraphAtomicityFixture::create("cancellation-rollback").await?;
    let result = async {
        let attempt = fixture
            .create_graph_attempt(
                fixture.revision_id,
                fixture.chunk_id,
                "cancellation-rollback",
                &["Cancellation Entity"],
            )
            .await?;
        let before =
            graph_digest(&fixture, attempt.job_id, attempt.attempt_id, fixture.revision_id).await?;
        let cancellation_token = CancellationToken::new();
        let authorized = fixture.reconcile(&attempt, &cancellation_token).await?;
        let reconcile = authorized.outcome?;
        assert!(reconcile.projection.has_materialized_graph());
        let mut publication_guard = authorized.publication_guard;
        let staged_snapshot = runtime_graph_repository::get_runtime_graph_snapshot_with_executor(
            publication_guard.connection(),
            fixture.library_id,
        )
        .await?
        .context("cancellation fixture must stage a terminal graph snapshot")?;
        assert_eq!(staged_snapshot.graph_status, "ready");

        cancellation_token.cancel();
        assert!(cancellation_token.is_cancelled(), "post-reconcile cancellation did not land");
        publication_guard
            .rollback()
            .await
            .context("explicitly roll back canceled graph publication guard")?;

        let after =
            graph_digest(&fixture, attempt.job_id, attempt.attempt_id, fixture.revision_id).await?;
        assert_eq!(after, before, "cancellation must be durable before the worker returns");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with the pgvector extensions"]
async fn database_cancel_beats_guarded_graph_publication_and_rolls_back_graph() -> Result<()> {
    let fixture = GraphAtomicityFixture::create("database-cancel").await?;
    let result = assert_database_transition_beats_guarded_publication(
        &fixture,
        "database-cancel",
        "Canceled Entity",
        ConcurrentJobTransition::Cancel,
    )
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with the pgvector extensions"]
async fn database_pause_beats_guarded_graph_publication_and_rolls_back_graph() -> Result<()> {
    let fixture = GraphAtomicityFixture::create("database-pause").await?;
    let result = assert_database_transition_beats_guarded_publication(
        &fixture,
        "database-pause",
        "Paused Entity",
        ConcurrentJobTransition::Pause,
    )
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with the pgvector extensions"]
async fn stale_graph_refresh_cannot_touch_current_readable_revision_projection() -> Result<()> {
    let fixture = GraphAtomicityFixture::create("stale-a").await?;
    let result = async {
        let stale_revision_id = fixture.revision_id;
        let stale_chunk_id = fixture.chunk_id;
        let current = fixture.create_replacement_revision("current-b").await?;
        let current_attempt = fixture
            .create_graph_attempt(
                current.revision_id,
                current.chunk_id,
                "current-b",
                &["Current Readable Entity"],
            )
            .await?;
        assert!(fixture.reconcile_and_publish(&current_attempt).await? > 0);
        let _execution_id = fixture.append_query_reference(current.chunk_id).await?;

        let stale_attempt = fixture
            .create_graph_attempt(
                stale_revision_id,
                stale_chunk_id,
                "stale-a",
                &["Stale Revision Entity"],
            )
            .await?;
        let before = graph_digest(
            &fixture,
            stale_attempt.job_id,
            stale_attempt.attempt_id,
            current.revision_id,
        )
        .await?;
        let outcome = GraphService::new()
            .reconcile_revision_graph(
                &fixture.state,
                stale_attempt.authority,
                fixture.document_id,
                &CancellationToken::new(),
            )
            .await;
        let error = match outcome {
            Err(error) => error,
            Ok(_) => bail!("stale graph refresh must be rejected before graph mutation"),
        };
        assert!(matches!(
            error,
            GraphServiceError::RevisionSuperseded { revision_id }
                if revision_id == stale_revision_id
        ));
        let after = graph_digest(
            &fixture,
            stale_attempt.job_id,
            stale_attempt.attempt_id,
            current.revision_id,
        )
        .await?;
        assert_eq!(
            after, before,
            "stale A must not alter B head, graph, evidence, query references, snapshot, source, or lifecycle"
        );
        assert!(after.nodes.iter().any(|(_, label, _, _, _)| label == "Current Readable Entity"));
        assert!(after.nodes.iter().all(|(_, label, _, _, _)| label != "Stale Revision Entity"));
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

async fn acquire_graph_guard(
    pool: &PgPool,
    attempt: &GraphAttempt,
    document_id: Uuid,
) -> Result<runtime_graph_repository::IngestRevisionGraphPublicationGuard> {
    match runtime_graph_repository::acquire_ingest_revision_graph_reconcile_guard(
        pool,
        attempt.authority,
        document_id,
    )
    .await?
    {
        runtime_graph_repository::IngestRevisionGraphGuardAcquireOutcome::Acquired(guard) => {
            Ok(guard)
        }
        runtime_graph_repository::IngestRevisionGraphGuardAcquireOutcome::AuthorityLost => {
            bail!("graph guard unexpectedly lost authority")
        }
        runtime_graph_repository::IngestRevisionGraphGuardAcquireOutcome::Superseded => {
            bail!("graph guard unexpectedly saw a superseded revision")
        }
        runtime_graph_repository::IngestRevisionGraphGuardAcquireOutcome::Busy => {
            bail!("graph guard unexpectedly encountered contention")
        }
    }
}

async fn backend_pid(connection: &mut PgConnection) -> Result<i32> {
    sqlx::query_scalar::<_, i32>("select pg_backend_pid()")
        .fetch_one(connection)
        .await
        .map_err(Into::into)
}

async fn lock_attempt_row(
    pool: &PgPool,
    attempt_id: Uuid,
) -> Result<sqlx::Transaction<'static, sqlx::Postgres>> {
    let mut transaction = pool.begin().await?;
    sqlx::query("select id from ingest_attempt where id = $1 for update")
        .bind(attempt_id)
        .execute(&mut *transaction)
        .await?;
    Ok(transaction)
}

async fn assert_terminal_stage_state(
    pool: &PgPool,
    attempt: &GraphAttempt,
    expected_ordinals: &[i32],
) -> Result<Vec<ingest_repository::IngestStageEventRow>> {
    let persisted_attempt = ingest_repository::get_ingest_attempt_by_id(pool, attempt.attempt_id)
        .await?
        .context("terminal graph attempt missing")?;
    let persisted_job = ingest_repository::get_ingest_job_by_id(pool, attempt.job_id)
        .await?
        .context("terminal graph job missing")?;
    assert_eq!(persisted_attempt.attempt_state, "succeeded");
    assert_eq!(persisted_attempt.current_stage.as_deref(), Some("finalizing"));
    assert_eq!(persisted_attempt.progress_percent, 100);
    assert_eq!(persisted_job.queue_state, "completed");
    let events =
        ingest_repository::list_ingest_stage_events_by_attempt(pool, attempt.attempt_id).await?;
    let ordinals = events.iter().map(|event| event.ordinal).collect::<Vec<_>>();
    assert_eq!(ordinals, expected_ordinals, "stage ordinals must be unique and repository-owned");
    assert_eq!(
        events.iter().filter(|event| event.stage_state == "completed").count(),
        1,
        "exactly one guarded terminal graph event must commit"
    );
    Ok(events)
}

/// Outcome of an ordinary stage-event append racing a guarded terminal publication.
type OrdinaryStageWriterOutcome = Result<
    Option<(ingest_repository::IngestStageEventRow, ingest_repository::IngestAttemptRow)>,
    sqlx::Error,
>;

struct StageWriterRace {
    ordinary_pool: PgPool,
    ordinary_pid: i32,
    ordinary_task: tokio::task::JoinHandle<OrdinaryStageWriterOutcome>,
}

async fn spawn_ordinary_stage_writer(
    fixture: &GraphAtomicityFixture,
    attempt_id: Uuid,
) -> Result<StageWriterRace> {
    let ordinary_pool =
        PgPoolOptions::new().max_connections(1).connect(&fixture.database.database_url).await?;
    let ordinary_pid =
        sqlx::query_scalar::<_, i32>("select pg_backend_pid()").fetch_one(&ordinary_pool).await?;
    let ordinary_event = ordinary_graph_event(attempt_id);
    let ordinary_task = tokio::spawn({
        let ordinary_pool = ordinary_pool.clone();
        async move {
            ingest_repository::append_ingest_stage_event_and_update_attempt(
                &ordinary_pool,
                &ordinary_event,
                "extract_graph",
                Utc::now(),
                None,
                95,
            )
            .await
        }
    });
    Ok(StageWriterRace { ordinary_pool, ordinary_pid, ordinary_task })
}

fn spawn_guarded_terminal_publication(
    guard: runtime_graph_repository::IngestRevisionGraphPublicationGuard,
    command: ingest_repository::PublishGraphRefreshSuccess,
    terminal: ingest_repository::NewIngestStageEvent,
) -> tokio::task::JoinHandle<Result<ingest_repository::PublishGraphRefreshSuccessOutcome>> {
    tokio::spawn(async move {
        ingest_repository::publish_graph_refresh_success_with_graph_guard(
            guard,
            &command,
            &ingest_repository::GraphRefreshPublicationEffects {
                terminal_stage_events: vec![terminal],
            },
        )
        .await
    })
}

#[tokio::test]
#[ignore = "requires local postgres with the pgvector extensions"]
async fn ordinary_stage_writer_first_does_not_deadlock_guarded_terminal_publication() -> Result<()>
{
    let fixture = GraphAtomicityFixture::create("stage-ordinary-first").await?;
    let result = async {
        let attempt = fixture
            .create_graph_attempt(
                fixture.revision_id,
                fixture.chunk_id,
                "stage-ordinary-first",
                &[],
            )
            .await?;
        let attempt_blocker = lock_attempt_row(fixture.pool(), attempt.attempt_id).await?;
        let mut guard = acquire_graph_guard(fixture.pool(), &attempt, fixture.document_id).await?;
        let publisher_pid = backend_pid(guard.connection()).await?;

        let ordinary = spawn_ordinary_stage_writer(&fixture, attempt.attempt_id).await?;
        // The blocker owns FOR UPDATE on the parent attempt row. The ordinary
        // writer already owns the stage-event advisory lock when its child-row
        // insert waits for the foreign-key KEY SHARE check, which establishes
        // the intended ordinary-writer-first lock order before publication.
        wait_for_backend_lock(
            fixture.pool(),
            ordinary.ordinary_pid,
            "insert into ingest_stage_event",
        )
        .await?;

        let completed_at = Utc::now();
        let command = graph_publication_command(&fixture, &attempt, completed_at);
        let terminal = terminal_graph_event(attempt.attempt_id, 0, completed_at);
        let publisher_task = spawn_guarded_terminal_publication(guard, command, terminal);
        wait_for_backend_lock(fixture.pool(), publisher_pid, "pg_advisory_xact_lock").await?;

        attempt_blocker.rollback().await?;
        let ordinary_outcome =
            tokio::time::timeout(StdDuration::from_secs(5), ordinary.ordinary_task)
                .await
                .context("ordinary-first writer did not finish")???;
        assert!(
            ordinary_outcome.is_some(),
            "ordinary writer must commit before terminal publication"
        );
        let publication = tokio::time::timeout(StdDuration::from_secs(5), publisher_task)
            .await
            .context("ordinary-first guarded publisher did not finish")???;
        assert!(matches!(
            publication,
            ingest_repository::PublishGraphRefreshSuccessOutcome::Applied { .. }
        ));
        let events = assert_terminal_stage_state(fixture.pool(), &attempt, &[1, 2]).await?;
        assert_eq!(events[0].stage_state, "started");
        assert_eq!(events[1].stage_state, "completed");
        ordinary.ordinary_pool.close().await;
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with the pgvector extensions"]
async fn guarded_terminal_publication_first_prevents_ordinary_attempt_resurrection() -> Result<()> {
    let fixture = GraphAtomicityFixture::create("stage-publisher-first").await?;
    let result = async {
        let attempt = fixture
            .create_graph_attempt(
                fixture.revision_id,
                fixture.chunk_id,
                "stage-publisher-first",
                &[],
            )
            .await?;
        let attempt_blocker = lock_attempt_row(fixture.pool(), attempt.attempt_id).await?;
        let mut guard = acquire_graph_guard(fixture.pool(), &attempt, fixture.document_id).await?;
        let publisher_pid = backend_pid(guard.connection()).await?;
        let completed_at = Utc::now();
        let command = graph_publication_command(&fixture, &attempt, completed_at);
        let terminal = terminal_graph_event(attempt.attempt_id, 0, completed_at);
        let publisher_task = spawn_guarded_terminal_publication(guard, command, terminal);
        wait_for_backend_lock(fixture.pool(), publisher_pid, "select attempt.id").await?;

        let ordinary = spawn_ordinary_stage_writer(&fixture, attempt.attempt_id).await?;
        wait_for_backend_lock(fixture.pool(), ordinary.ordinary_pid, "pg_advisory_xact_lock")
            .await?;

        attempt_blocker.rollback().await?;
        let publication = tokio::time::timeout(StdDuration::from_secs(5), publisher_task)
            .await
            .context("publisher-first guarded publication did not finish")???;
        assert!(matches!(
            publication,
            ingest_repository::PublishGraphRefreshSuccessOutcome::Applied { .. }
        ));
        let ordinary_outcome =
            tokio::time::timeout(StdDuration::from_secs(5), ordinary.ordinary_task)
                .await
                .context("publisher-first ordinary writer did not finish")???;
        assert!(
            ordinary_outcome.is_none(),
            "ordinary writer must observe terminal state and no-op"
        );
        let events = assert_terminal_stage_state(fixture.pool(), &attempt, &[1]).await?;
        assert_eq!(events[0].stage_state, "completed");
        ordinary.ordinary_pool.close().await;
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with the pgvector extensions"]
async fn maximum_heartbeat_guards_leave_main_and_control_reads_unstarved() -> Result<()> {
    const IN_FLIGHT_JOBS: usize = 3;

    let settings = Settings::from_env().context("load graph pool starvation settings")?;
    let database =
        TempPostgresDatabase::create(&settings.database_url, "graph_guard_starvation").await?;
    let setup_pool =
        PgPoolOptions::new().max_connections(4).connect(&database.database_url).await?;
    sqlx::migrate!("./migrations").run(&setup_pool).await?;
    let suffix = Uuid::now_v7().simple().to_string();
    let workspace = catalog_repository::create_workspace(
        &setup_pool,
        &format!("graph-guard-starvation-{suffix}"),
        "Graph Guard Starvation",
        None,
    )
    .await?;

    let mut scopes = Vec::new();
    for index in 0..IN_FLIGHT_JOBS {
        let library = catalog_repository::create_library(
            &setup_pool,
            workspace.id,
            &format!("graph-guard-starvation-{index}-{suffix}"),
            "Graph Guard Starvation",
            None,
            None,
        )
        .await?;
        let document = content_knowledge_projection_repository::create_document_with_projection(
            &setup_pool,
            &content_document_repository::NewContentDocument {
                workspace_id: workspace.id,
                library_id: library.id,
                external_key: &format!("graph-guard-starvation-document-{index}-{suffix}"),
                document_state: "active",
                created_by_principal_id: None,
                parent_external_key: None,
                parent_document_id: None,
                document_role: "primary",
            },
            Some("graph-guard-starvation.txt"),
        )
        .await?;
        let revision = create_prepared_revision(
            &setup_pool,
            workspace.id,
            library.id,
            document.id,
            &format!("guard-{index}"),
        )
        .await?;
        publish_readable_head(&setup_pool, document.id, revision.revision_id).await?;
        let attempt = create_graph_attempt(GraphAttemptFixture {
            pool: &setup_pool,
            workspace_id: workspace.id,
            library_id: library.id,
            document_id: document.id,
            revision_id: revision.revision_id,
            chunk_id: revision.chunk_id,
            label: &format!("guard-{index}"),
            entities: &[],
        })
        .await?;
        scopes.push((document.id, attempt));
    }

    let main_pool = PgPoolOptions::new().max_connections(1).connect(&database.database_url).await?;
    let heartbeat_pool = PgPoolOptions::new()
        .max_connections(u32::try_from(IN_FLIGHT_JOBS + 1).unwrap_or(u32::MAX))
        .connect(&database.database_url)
        .await?;
    let mut guards = Vec::new();
    for (document_id, attempt) in &scopes {
        guards.push(acquire_graph_guard(&heartbeat_pool, attempt, *document_id).await?);
    }
    assert_eq!(
        heartbeat_pool.num_idle(),
        0,
        "every job guard must occupy one heartbeat connection before the spare-lane probe"
    );

    let main_reads = async {
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..6 {
            let main_pool = main_pool.clone();
            tasks.spawn(async move {
                sqlx::query_scalar::<_, i64>("select count(*)::bigint from catalog_library")
                    .fetch_one(&main_pool)
                    .await
            });
        }
        while let Some(result) = tasks.join_next().await {
            assert_eq!(result??, i64::try_from(IN_FLIGHT_JOBS).unwrap_or(i64::MAX));
        }
        Ok::<_, anyhow::Error>(())
    };
    let control_read = sqlx::query_scalar::<_, i32>("select 1").fetch_one(&heartbeat_pool);
    let (main_result, control_result) = tokio::time::timeout(StdDuration::from_secs(5), async {
        tokio::join!(main_reads, control_read)
    })
    .await
    .context("guarded workers starved the main or reserved heartbeat lane")?;
    main_result?;
    assert_eq!(control_result?, 1);
    assert_eq!(
        guards.len(),
        IN_FLIGHT_JOBS,
        "the starvation probe must hold the configured maximum job guards"
    );

    for guard in guards {
        guard.rollback().await?;
    }
    heartbeat_pool.close().await;
    main_pool.close().await;
    setup_pool.close().await;
    database.drop().await
}
