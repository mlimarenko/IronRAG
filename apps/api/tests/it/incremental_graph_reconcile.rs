//! PostgreSQL integration coverage for the IRG-197 incremental graph reconcile.
//!
//! Two properties are asserted against a real database:
//!
//! 1. An append revision reconciled incrementally (previous revision's
//!    unchanged chunks carried forward, only appended chunks merged) produces
//!    the same canonical graph as a single full ingest of the final document.
//!    The comparison is modulo ids: it uses canonical node/edge identity keys
//!    and the `(target, evidence context, evidence text)` evidence key.
//! 2. The append reconcile writes rows only for the appended chunks: the
//!    carried chunks' evidence rows keep their ids and `created_at` (updated in
//!    place) and no new evidence row points at a carried chunk.
//!
//! Run with:
//! `cargo test -p ironrag-backend --features test-support --test it -- --ignored incremental_graph_reconcile::`

use crate::temp_postgres_database;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use ironrag_backend::{
    app::{config::Settings, state::AppState},
    domains::agent_runtime::{RuntimeExecutionOwnerKind, RuntimeLifecycleState, RuntimeTaskKind},
    infra::{
        knowledge_plane::IngestRevisionWriteAuthority,
        persistence::Persistence,
        repositories::{
            catalog_repository, content_chunk_repository, content_document_repository,
            content_knowledge_projection_repository, content_revision_repository,
            runtime_graph_repository, runtime_repository,
        },
    },
    services::graph::reconcile_revision_graph_for_test,
};
use serde_json::json;
use sqlx::{PgPool, postgres::PgPoolOptions};
use temp_postgres_database::TempPostgresDatabase;
use uuid::Uuid;

const GRAPH_EXTRACTION_VERSION: &str = "graph_extract";

/// Chunk checksums are the cross-revision identity used by the delta path, so
/// the fixtures pin them rather than deriving them from the revision id.
struct FixtureChunk {
    chunk_index: i32,
    text_checksum: &'static str,
    text: &'static str,
    entities: &'static [&'static str],
}

const CHUNK_A: FixtureChunk = FixtureChunk {
    chunk_index: 0,
    text_checksum: "sha256:chunk-a",
    text: "Alpha subsystem describes the neutral first record with reusable entities and explicit structural evidence.",
    entities: &["Alpha Subsystem"],
};
const CHUNK_B: FixtureChunk = FixtureChunk {
    chunk_index: 1,
    text_checksum: "sha256:chunk-b",
    text: "Beta subsystem connects the second neutral record through reusable entities and explicit structural evidence.",
    entities: &["Beta Subsystem"],
};
const CHUNK_C: FixtureChunk = FixtureChunk {
    chunk_index: 2,
    text_checksum: "sha256:chunk-c",
    text: "Gamma subsystem appends the third neutral record with reusable entities and explicit structural evidence.",
    entities: &["Gamma Subsystem"],
};

struct Fixture {
    database: TempPostgresDatabase,
    state: AppState,
    workspace_id: Uuid,
}

struct Library {
    library_id: Uuid,
    document_id: Uuid,
}

impl Fixture {
    async fn create(label: &str) -> Result<Self> {
        let mut settings = Settings::from_env().context("load incremental graph settings")?;
        let database =
            TempPostgresDatabase::create(&settings.database_url, "incremental_graph").await?;
        settings.database_url = database.database_url.clone();
        settings.credential_master_key =
            Some("KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=".to_string());

        let postgres = PgPoolOptions::new()
            .max_connections(8)
            .connect(&settings.database_url)
            .await
            .context("connect incremental graph postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("apply incremental graph migrations")?;
        let redis = redis::Client::open(settings.redis_url.clone())
            .context("create incremental graph redis client")?;
        let state = AppState::from_dependencies(settings, Persistence::for_tests(postgres, redis))?;

        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            &state.persistence.postgres,
            &format!("incremental-graph-workspace-{suffix}"),
            &format!("Incremental Graph Workspace {label}"),
            None,
        )
        .await?;

        Ok(Self { database, state, workspace_id: workspace.id })
    }

    fn pool(&self) -> &PgPool {
        &self.state.persistence.postgres
    }

    async fn create_library(&self, label: &str) -> Result<Library> {
        let suffix = Uuid::now_v7().simple().to_string();
        let library = catalog_repository::create_library(
            self.pool(),
            self.workspace_id,
            &format!("incremental-graph-library-{label}-{suffix}"),
            &format!("Incremental Graph Library {label}"),
            None,
            None,
        )
        .await?;
        let document = content_knowledge_projection_repository::create_document_with_projection(
            self.pool(),
            &content_document_repository::NewContentDocument {
                workspace_id: self.workspace_id,
                library_id: library.id,
                external_key: &format!("incremental-graph-{label}-{suffix}.txt"),
                document_state: "active",
                created_by_principal_id: None,
                parent_external_key: None,
                parent_document_id: None,
                document_role: "primary",
            },
            Some(&format!("incremental-graph-{label}.txt")),
        )
        .await?;
        Ok(Library { library_id: library.id, document_id: document.id })
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.database.drop().await
    }
}

struct PreparedRevision {
    revision_id: Uuid,
    chunk_ids: Vec<Uuid>,
}

async fn create_revision_with_chunks(
    pool: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    label: &str,
    chunks: &[&FixtureChunk],
) -> Result<PreparedRevision> {
    let checksum = format!("sha256:{0}{0}", Uuid::now_v7().simple());
    let revision = match content_revision_repository::create_revision_with_projection(
        pool,
        &content_revision_repository::NewContentRevisionProjection {
            admission_identity: Some(&crate::content_repository::synthetic_import_admission(
                ironrag_contracts::documents::ReplacementIdentity {
                    checksum: checksum.clone(),
                    mime_type: "text/plain".to_owned(),
                    byte_size: i64::try_from(
                        chunks.iter().map(|chunk| chunk.text.len()).sum::<usize>(),
                    )?,
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
            byte_size: i64::try_from(chunks.iter().map(|chunk| chunk.text.len()).sum::<usize>())
                .unwrap_or(i64::MAX),
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
    let projection = chunks
        .iter()
        .map(|chunk| {
            crate::knowledge_seed::ready_paragraph_projection(
                crate::knowledge_seed::ReadyChunkSeed {
                    workspace_id,
                    library_id,
                    document_id,
                    revision_id: revision.id,
                    chunk_index: chunk.chunk_index,
                    text: chunk.text,
                },
                i32::try_from(chunk.text.len()).unwrap_or(i32::MAX),
                24,
                chunk.text_checksum,
                i64::from(revision.revision_number),
            )
        })
        .collect::<Vec<_>>();
    let created =
        content_chunk_repository::replace_chunks_with_projection(pool, revision.id, &projection)
            .await?;
    let mut chunk_ids = created.iter().map(|chunk| chunk.id).collect::<Vec<_>>();
    chunk_ids.sort_by_key(|chunk_id| {
        created.iter().find(|chunk| chunk.id == *chunk_id).map_or(0, |chunk| chunk.chunk_index)
    });
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
    .bind(chunks.iter().map(|chunk| chunk.text).collect::<Vec<_>>().join("\n\n"))
    .bind(&checksum)
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
            'prepared', 'incremental-graph-v1', 'text', null,
            $5, $5, 0, '[]'::jsonb,
            now(), now()
         )",
    )
    .bind(revision.id)
    .bind(workspace_id)
    .bind(library_id)
    .bind(document_id)
    .bind(i32::try_from(chunks.len()).unwrap_or(0))
    .execute(pool)
    .await?;
    Ok(PreparedRevision { revision_id: revision.id, chunk_ids })
}

async fn publish_readable_head(pool: &PgPool, document_id: Uuid, revision_id: Uuid) -> Result<()> {
    crate::graph_reconcile_support::set_active_revision_head(pool, document_id, revision_id)
        .await?;
    crate::content_lifecycle_support::assert_current_revision(pool, document_id, revision_id).await
}

/// One graph-extraction attempt seed over a document revision.
struct GraphAttemptSeed<'a> {
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    chunks: &'a [(&'a FixtureChunk, Uuid)],
    label: &'a str,
}

async fn seed_graph_attempt(pool: &PgPool, seed: GraphAttemptSeed<'_>) -> Result<Uuid> {
    let job = crate::graph_reconcile_support::create_leased_ingest_job(
        pool,
        crate::graph_reconcile_support::LeasedIngestJobSeed {
            workspace_id: seed.workspace_id,
            library_id: seed.library_id,
            document_id: seed.document_id,
            revision_id: seed.revision_id,
            job_kind: "content_mutation",
            dedupe_key_prefix: &format!("incremental-graph:{}", seed.label),
        },
    )
    .await?;
    let attempt =
        crate::graph_reconcile_support::create_leased_graph_attempt(pool, job.id, seed.label)
            .await?;
    let runtime_execution_id = Uuid::now_v7();
    runtime_repository::create_runtime_execution(
        pool,
        &runtime_repository::NewRuntimeExecution {
            id: runtime_execution_id,
            owner_kind: RuntimeExecutionOwnerKind::GraphExtractionAttempt.as_str(),
            owner_id: attempt.id,
            task_kind: RuntimeTaskKind::GraphExtract.as_str(),
            surface_kind: "worker",
            contract_name: "incremental_graph_extract",
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
    for (chunk, chunk_id) in seed.chunks {
        let normalized_entities = chunk
            .entities
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
                library_id: seed.library_id,
                document_id: seed.document_id,
                chunk_id: *chunk_id,
                provider_kind: "synthetic".to_string(),
                model_name: "fixture".to_string(),
                extraction_version: GRAPH_EXTRACTION_VERSION.to_string(),
                prompt_hash: format!("sha256:{}:{}", seed.label, chunk.text_checksum),
                status: "ready".to_string(),
                raw_output_json: json!({
                    "lifecycle": {
                        "revision_id": seed.revision_id,
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
    }
    Ok(attempt.id)
}

async fn reconcile(
    state: &AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    attempt_id: Uuid,
) -> Result<ironrag_backend::services::graph::RevisionGraphReconcileOutcome> {
    let authority =
        IngestRevisionWriteAuthority { workspace_id, library_id, attempt_id, revision_id };
    let mut transaction = state
        .persistence
        .postgres
        .begin()
        .await
        .context("begin incremental graph reconcile transaction")?;
    let outcome =
        reconcile_revision_graph_for_test(state, authority, document_id, &mut transaction)
            .await
            .context("incremental graph reconcile failed")?;
    transaction.commit().await.context("commit incremental graph reconcile")?;
    Ok(outcome)
}

async fn assert_private_candidate_projection(
    fixture: &Fixture,
    library: &Library,
    authority: IngestRevisionWriteAuthority,
) -> Result<()> {
    use ironrag_backend::infra::knowledge_plane::IngestAttemptMutationOutcome;
    let authorized = ironrag_backend::services::graph::service::GraphService::new()
        .reconcile_revision_graph(
            &fixture.state,
            authority,
            library.document_id,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await?;
    let IngestAttemptMutationOutcome::Applied(mut authorized) = authorized else {
        bail!("candidate must have live authority");
    };
    let projection = authorized.outcome?;
    anyhow::ensure!(
        projection.graph_ready && projection.projection.node_count > 0,
        "exact leased candidate must count its staged materialization"
    );
    let public = runtime_graph_repository::count_admitted_runtime_graph_projection_with_executor(
        authorized.publication_guard.connection(),
        library.library_id,
        projection.projection.projection_version,
    )
    .await?;
    anyhow::ensure!(
        public.node_count == 0 && public.edge_count == 0,
        "staged source must stay excluded from public admission on the same connection"
    );
    authorized.publication_guard.rollback().await?;
    Ok(())
}

async fn assert_candidate_authority_refused(
    fixture: &Fixture,
    library: &Library,
    authority: IngestRevisionWriteAuthority,
) -> Result<()> {
    use ironrag_backend::infra::knowledge_plane::IngestAttemptMutationOutcome;
    match ironrag_backend::services::graph::service::GraphService::new()
        .reconcile_revision_graph(
            &fixture.state,
            authority,
            library.document_id,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await?
    {
        IngestAttemptMutationOutcome::AuthorityLost => {}
        IngestAttemptMutationOutcome::Applied(authorized) => {
            anyhow::ensure!(
                authorized.outcome.is_err(),
                "invalid owner cannot obtain writer counts"
            );
            authorized.publication_guard.rollback().await?;
        }
    }
    let rows: i64 =
        sqlx::query_scalar("select count(*) from runtime_graph_node where library_id=$1")
            .bind(library.library_id)
            .fetch_one(fixture.pool())
            .await?;
    anyhow::ensure!(rows == 0, "refused authority must leave committed graph untouched");
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn prepublication_graph_accounting_requires_exact_leased_source_owner() -> Result<()> {
    let fixture = Fixture::create("private-accounting").await?;
    let result = async {
        let library = fixture.create_library("candidate").await?;
        let revision = create_revision_with_chunks(fixture.pool(), fixture.workspace_id,
            library.library_id, library.document_id, "candidate", &[&CHUNK_A]).await?;
        let chunks = [(&CHUNK_A, revision.chunk_ids[0])];
        let attempt_id = seed_graph_attempt(fixture.pool(), GraphAttemptSeed {
            workspace_id: fixture.workspace_id, library_id: library.library_id,
            document_id: library.document_id, revision_id: revision.revision_id,
            chunks: &chunks, label: "candidate",
        }).await?;
        let authority = IngestRevisionWriteAuthority { workspace_id: fixture.workspace_id,
            library_id: library.library_id, revision_id: revision.revision_id, attempt_id };
        let (job_id, item_id): (Uuid, Uuid) = sqlx::query_as(
            "select j.id,j.mutation_item_id from ingest_job j join ingest_attempt a on a.job_id=j.id where a.id=$1",
        ).bind(attempt_id).fetch_one(fixture.pool()).await?;
        assert_private_candidate_projection(&fixture, &library, authority).await?;
        sqlx::query("update ingest_job set mutation_item_id=null where id=$1")
            .bind(job_id).execute(fixture.pool()).await?;
        assert_candidate_authority_refused(&fixture, &library, authority).await?;
        sqlx::query("update ingest_job set mutation_item_id=$2 where id=$1")
            .bind(job_id).bind(item_id).execute(fixture.pool()).await?;
        assert_private_candidate_projection(&fixture, &library, authority).await?;
        let foreign = fixture.create_library("foreign-owner").await?;
        let foreign_revision = create_revision_with_chunks(fixture.pool(), fixture.workspace_id,
            foreign.library_id, foreign.document_id, "foreign", &[&CHUNK_B]).await?;
        let foreign_item = crate::content_lifecycle_support::seed_pending_revision_owner(
            fixture.pool(), foreign_revision.revision_id, "foreign.txt",
        ).await?;
        let rejected = sqlx::query("update ingest_job set mutation_item_id=$2 where id=$1")
            .bind(job_id).bind(foreign_item).execute(fixture.pool()).await
            .expect_err("foreign item must be rejected at the durable job lineage boundary");
        assert_eq!(rejected.as_database_error().and_then(|error| error.code()).as_deref(), Some("23514"));
        assert_private_candidate_projection(&fixture, &library, authority).await?;
        assert_candidate_authority_refused(&fixture, &library,
            IngestRevisionWriteAuthority { revision_id: foreign_revision.revision_id, ..authority }).await?;
        assert_private_candidate_projection(&fixture, &library, authority).await?;
        assert_candidate_authority_refused(&fixture, &library,
            IngestRevisionWriteAuthority { attempt_id: Uuid::now_v7(), ..authority }).await?;
        assert_private_candidate_projection(&fixture, &library, authority).await?;
        sqlx::query("update ingest_attempt set attempt_state='canceled' where id=$1")
            .bind(attempt_id).execute(fixture.pool()).await?;
        assert_candidate_authority_refused(&fixture, &library, authority).await?;
        Ok(())
    }.await;
    fixture.cleanup().await?;
    result
}

/// Canonical, id-free graph signature with multiplicities: entity node
/// canonical keys, entity-entity edge canonical keys, and evidence keyed by
/// target canonical key and evidence text. Document-scoped rows (the document
/// node and its mention edges) are excluded because their keys are per
/// document; everything compared here is content-derived.
async fn canonical_graph_signature(pool: &PgPool, library_id: Uuid) -> Result<Vec<(String, i64)>> {
    let rows = sqlx::query_as::<_, (String, i64)>(
        "with entity_nodes as (
             select node.id, node.canonical_key
             from runtime_graph_node as node
             where node.library_id = $1
               and node.node_type <> 'document'
         ),
         entity_edges as (
             select edge.id, edge.canonical_key
             from runtime_graph_edge as edge
             where edge.library_id = $1
               and edge.from_node_id in (select id from entity_nodes)
               and edge.to_node_id in (select id from entity_nodes)
         ),
         signatures as (
             select 'node:' || entity_nodes.canonical_key as signature
             from entity_nodes
             union all
             select 'edge:' || entity_edges.canonical_key
             from entity_edges
             union all
             select 'evidence:' || evidence.target_kind || ':' ||
                    coalesce(node.canonical_key, edge.canonical_key) || ':' ||
                    evidence.evidence_text
             from runtime_graph_evidence as evidence
             left join entity_nodes as node
               on evidence.target_kind = 'node' and node.id = evidence.target_id
             left join entity_edges as edge
               on evidence.target_kind = 'edge' and edge.id = evidence.target_id
             where evidence.library_id = $1
               and coalesce(node.canonical_key, edge.canonical_key) is not null
         )
         select signature, count(*)::bigint
         from signatures
         group by signature
         order by signature",
    )
    .bind(library_id)
    .fetch_all(pool)
    .await
    .context("failed to compute canonical graph signature")?;
    Ok(rows)
}

fn format_signature(signature: &[(String, i64)]) -> String {
    signature
        .iter()
        .map(|(value, count)| format!("{count} x {value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

async fn reconcile_and_signature(
    fixture: &Fixture,
    library: &Library,
    revision: &PreparedRevision,
    chunks: &[&FixtureChunk],
    label: &str,
) -> Result<Vec<(String, i64)>> {
    let pairs = chunks
        .iter()
        .zip(revision.chunk_ids.iter())
        .map(|(chunk, chunk_id)| (*chunk, *chunk_id))
        .collect::<Vec<_>>();
    let attempt_id = seed_graph_attempt(
        fixture.pool(),
        GraphAttemptSeed {
            workspace_id: fixture.workspace_id,
            library_id: library.library_id,
            document_id: library.document_id,
            revision_id: revision.revision_id,
            chunks: &pairs,
            label,
        },
    )
    .await?;
    reconcile(
        &fixture.state,
        fixture.workspace_id,
        library.library_id,
        library.document_id,
        revision.revision_id,
        attempt_id,
    )
    .await?;
    canonical_graph_signature(fixture.pool(), library.library_id).await
}

#[tokio::test]
#[ignore = "requires PostgreSQL and Redis"]
async fn incremental_append_matches_full_ingest_and_merges_only_added_chunks() -> Result<()> {
    let fixture = Fixture::create("append").await?;
    let incremental = fixture.create_library("incremental").await?;
    let full = fixture.create_library("full").await?;

    // Incremental document: parent [A, B] then append [A', B', C].
    let parent = create_revision_with_chunks(
        fixture.pool(),
        fixture.workspace_id,
        incremental.library_id,
        incremental.document_id,
        "parent",
        &[&CHUNK_A, &CHUNK_B],
    )
    .await?;
    publish_readable_head(fixture.pool(), incremental.document_id, parent.revision_id).await?;
    let parent_signature =
        reconcile_and_signature(&fixture, &incremental, &parent, &[&CHUNK_A, &CHUNK_B], "parent")
            .await?;

    let parent_evidence = sqlx::query_as::<_, (Uuid, Uuid, chrono::DateTime<Utc>)>(
        "select id, chunk_id, created_at
         from runtime_graph_evidence
         where library_id = $1
         order by id",
    )
    .bind(incremental.library_id)
    .fetch_all(fixture.pool())
    .await?;
    anyhow::ensure!(
        !parent_evidence.is_empty(),
        "parent reconcile must have written graph evidence"
    );
    let parent_evidence_ids = parent_evidence.iter().map(|(id, _, _)| *id).collect::<Vec<_>>();
    let parent_evidence_by_id = parent_evidence
        .iter()
        .map(|(id, chunk, created)| (*id, (*chunk, *created)))
        .collect::<std::collections::BTreeMap<_, _>>();

    // The head deliberately stays on the parent: the production pipeline
    // reconciles the new revision before content publication moves the head,
    // which is exactly the arrangement the delta path keys on.
    let append = create_revision_with_chunks(
        fixture.pool(),
        fixture.workspace_id,
        incremental.library_id,
        incremental.document_id,
        "append",
        &[&CHUNK_A, &CHUNK_B, &CHUNK_C],
    )
    .await?;
    anyhow::ensure!(
        append.chunk_ids.len() == 3 && parent.chunk_ids.len() == 2,
        "append fixture must carry the parent chunks plus one appended chunk"
    );

    // The carried chunks get decoy extraction output that must never be
    // merged: the delta path carries the parent evidence instead.
    const DECOY_A: FixtureChunk = FixtureChunk {
        chunk_index: 0,
        text_checksum: CHUNK_A.text_checksum,
        text: CHUNK_A.text,
        entities: &["Decoy Alpha Entity"],
    };
    const DECOY_B: FixtureChunk = FixtureChunk {
        chunk_index: 1,
        text_checksum: CHUNK_B.text_checksum,
        text: CHUNK_B.text,
        entities: &["Decoy Beta Entity"],
    };
    let append_attempt = seed_graph_attempt(
        fixture.pool(),
        GraphAttemptSeed {
            workspace_id: fixture.workspace_id,
            library_id: incremental.library_id,
            document_id: incremental.document_id,
            revision_id: append.revision_id,
            chunks: &[
                (&DECOY_A, append.chunk_ids[0]),
                (&DECOY_B, append.chunk_ids[1]),
                (&CHUNK_C, append.chunk_ids[2]),
            ],
            label: "append",
        },
    )
    .await?;
    let append_outcome = reconcile(
        &fixture.state,
        fixture.workspace_id,
        incremental.library_id,
        incremental.document_id,
        append.revision_id,
        append_attempt,
    )
    .await?;

    let incremental_signature =
        canonical_graph_signature(fixture.pool(), incremental.library_id).await?;

    // Write counter: carried evidence is updated in place, never deleted and
    // re-inserted, and no new row points at a carried chunk.
    let after = sqlx::query_as::<_, (Uuid, Uuid, Uuid, chrono::DateTime<Utc>)>(
        "select id, revision_id, chunk_id, created_at
         from runtime_graph_evidence
         where library_id = $1",
    )
    .bind(incremental.library_id)
    .fetch_all(fixture.pool())
    .await?;
    let carried_chunk_ids = [append.chunk_ids[0], append.chunk_ids[1]]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    let appended_chunk_id = append.chunk_ids[2];

    let mut carried_rows = 0usize;
    for (id, revision_id, chunk_id, created_at) in &after {
        if parent_evidence_by_id.contains_key(id) {
            carried_rows += 1;
            anyhow::ensure!(
                *revision_id == append.revision_id,
                "carried evidence {id} must move to the new revision"
            );
            anyhow::ensure!(
                carried_chunk_ids.contains(chunk_id),
                "carried evidence {id} must point at a carried revision chunk"
            );
            let (parent_chunk_id, parent_created_at) = parent_evidence_by_id[id];
            anyhow::ensure!(
                created_at == &parent_created_at,
                "carried evidence {id} must keep its created_at (in-place update, not re-insert)"
            );
            anyhow::ensure!(
                chunk_id != &parent_chunk_id,
                "carried evidence {id} must be retagged to the new revision chunk id"
            );
        } else {
            anyhow::ensure!(
                *chunk_id == appended_chunk_id,
                "new evidence {id} may only belong to the appended chunk, got chunk {chunk_id}"
            );
        }
    }
    anyhow::ensure!(
        carried_rows == parent_evidence.len(),
        "every parent evidence row must be carried forward, carried {carried_rows} of {}",
        parent_evidence.len()
    );
    anyhow::ensure!(
        after.len() > parent_evidence.len(),
        "the appended chunk must contribute new evidence"
    );
    anyhow::ensure!(
        parent_evidence_ids.len() == parent_evidence.len(),
        "fixture parent evidence ids must be unique"
    );

    // Full document: the same final content in a single revision.
    let full_revision = create_revision_with_chunks(
        fixture.pool(),
        fixture.workspace_id,
        full.library_id,
        full.document_id,
        "full",
        &[&CHUNK_A, &CHUNK_B, &CHUNK_C],
    )
    .await?;
    publish_readable_head(fixture.pool(), full.document_id, full_revision.revision_id).await?;
    let full_signature = reconcile_and_signature(
        &fixture,
        &full,
        &full_revision,
        &[&CHUNK_A, &CHUNK_B, &CHUNK_C],
        "full",
    )
    .await?;

    anyhow::ensure!(
        !incremental_signature.is_empty() && !full_signature.is_empty(),
        "both reconciles must publish graph artifacts"
    );
    anyhow::ensure!(
        incremental_signature == full_signature,
        "incremental append graph must equal the full ingest graph\nincremental:\n{}\nfull:\n{}",
        format_signature(&incremental_signature),
        format_signature(&full_signature)
    );
    anyhow::ensure!(
        !parent_signature.is_empty(),
        "parent reconcile must publish a non-empty graph"
    );
    anyhow::ensure!(
        append_outcome.graph_ready,
        "append reconcile must still report a ready graph from carried evidence"
    );

    let _ = fixture.cleanup().await;
    Ok(())
}
