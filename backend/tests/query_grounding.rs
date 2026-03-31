use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use reqwest::{Client, StatusCode};
use serde_json::json;
use uuid::Uuid;

use rustrag_backend::{
    app::config::Settings,
    domains::query::QueryExecution,
    domains::{audit::AuditEventSubject, ops::OpsAsyncOperation},
    infra::arangodb::{
        bootstrap::{ArangoBootstrapOptions, bootstrap_knowledge_plane},
        client::ArangoClient,
        context_store::{
            ArangoContextStore, KnowledgeBundleChunkEdgeRow, KnowledgeBundleChunkReferenceRow,
            KnowledgeBundleEntityEdgeRow, KnowledgeBundleEntityReferenceRow,
            KnowledgeBundleEvidenceEdgeRow, KnowledgeBundleEvidenceReferenceRow,
            KnowledgeBundleRelationEdgeRow, KnowledgeBundleRelationReferenceRow,
            KnowledgeContextBundleReferenceSetRow, KnowledgeContextBundleRow,
            KnowledgeRetrievalTraceRow,
        },
        document_store::{
            ArangoDocumentStore, KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeRevisionRow,
        },
        graph_store::{ArangoGraphStore, NewKnowledgeEntity},
    },
    services::query_service::QueryService,
};

struct TempArangoDatabase {
    base_url: String,
    username: String,
    password: String,
    name: String,
    http: Client,
}

impl TempArangoDatabase {
    async fn create(settings: &Settings) -> Result<Self> {
        let base_url = settings.arangodb_url.trim().trim_end_matches('/').to_string();
        let name = format!("query_grounding_{}", Uuid::now_v7().simple());
        let http = Client::builder()
            .timeout(Duration::from_secs(settings.arangodb_request_timeout_seconds.max(1)))
            .build()
            .context("failed to build ArangoDB admin http client")?;
        let response = http
            .post(format!("{base_url}/_api/database"))
            .basic_auth(&settings.arangodb_username, Some(&settings.arangodb_password))
            .json(&json!({ "name": name }))
            .send()
            .await
            .context("failed to create temp ArangoDB database")?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "failed to create temp ArangoDB database {}: status {}",
                name,
                response.status()
            ));
        }

        Ok(Self {
            base_url,
            username: settings.arangodb_username.clone(),
            password: settings.arangodb_password.clone(),
            name,
            http,
        })
    }

    async fn drop(self) -> Result<()> {
        let response = self
            .http
            .delete(format!("{}/_api/database/{}", self.base_url, self.name))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .context("failed to drop temp ArangoDB database")?;
        if response.status() != StatusCode::NOT_FOUND && !response.status().is_success() {
            return Err(anyhow!(
                "failed to drop temp ArangoDB database {}: status {}",
                self.name,
                response.status()
            ));
        }
        Ok(())
    }
}

struct QueryGroundingFixture {
    temp_database: TempArangoDatabase,
    document_store: ArangoDocumentStore,
    context_store: ArangoContextStore,
    graph_store: ArangoGraphStore,
}

impl QueryGroundingFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for query grounding tests")?;
        let temp_database = TempArangoDatabase::create(&settings).await?;
        settings.arangodb_database = temp_database.name.clone();

        let client = Arc::new(
            ArangoClient::from_settings(&settings).context("failed to build Arango client")?,
        );
        client.ping().await.context("failed to ping temp ArangoDB database")?;
        bootstrap_knowledge_plane(
            &client,
            &ArangoBootstrapOptions {
                collections: true,
                views: false,
                graph: true,
                vector_indexes: false,
                vector_dimensions: 3072,
                vector_index_n_lists: 100,
                vector_index_default_n_probe: 8,
                vector_index_training_iterations: 25,
            },
        )
        .await
        .context("failed to bootstrap temp Arango knowledge plane")?;

        Ok(Self {
            temp_database,
            document_store: ArangoDocumentStore::new(Arc::clone(&client)),
            context_store: ArangoContextStore::new(Arc::clone(&client)),
            graph_store: ArangoGraphStore::new(Arc::clone(&client)),
        })
    }

    async fn cleanup(self) -> Result<()> {
        self.temp_database.drop().await
    }

    async fn seed_chunk(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
        document_id: Uuid,
        revision_id: Uuid,
        chunk_id: Uuid,
        content_text: &str,
    ) -> Result<()> {
        let now = Utc::now();

        self.document_store
            .upsert_document(&KnowledgeDocumentRow {
                key: document_id.to_string(),
                arango_id: None,
                arango_rev: None,
                document_id,
                workspace_id,
                library_id,
                external_key: format!("grounding-{document_id}"),
                title: Some("Grounding Document".to_string()),
                document_state: "active".to_string(),
                active_revision_id: Some(revision_id),
                readable_revision_id: Some(revision_id),
                latest_revision_no: Some(1),
                created_at: now,
                updated_at: now,
                deleted_at: None,
            })
            .await
            .context("failed to insert grounding document")?;

        self.document_store
            .upsert_revision(&KnowledgeRevisionRow {
                key: revision_id.to_string(),
                arango_id: None,
                arango_rev: None,
                revision_id,
                workspace_id,
                library_id,
                document_id,
                revision_number: 1,
                revision_state: "active".to_string(),
                revision_kind: "upload".to_string(),
                storage_ref: Some(format!("memory://grounding/{revision_id}")),
                source_uri: Some(format!("memory://grounding/source/{revision_id}")),
                mime_type: "text/plain".to_string(),
                checksum: format!("checksum-{revision_id}"),
                title: Some("Grounding Revision".to_string()),
                byte_size: i64::try_from(content_text.len()).unwrap_or(i64::MAX),
                normalized_text: Some(content_text.to_string()),
                text_checksum: Some(format!("text-checksum-{revision_id}")),
                text_state: "text_readable".to_string(),
                vector_state: "pending".to_string(),
                graph_state: "pending".to_string(),
                text_readable_at: Some(now),
                vector_ready_at: None,
                graph_ready_at: None,
                superseded_by_revision_id: None,
                created_at: now,
            })
            .await
            .context("failed to insert grounding revision")?;

        self.document_store
            .upsert_chunk(&KnowledgeChunkRow {
                key: chunk_id.to_string(),
                arango_id: None,
                arango_rev: None,
                chunk_id,
                workspace_id,
                library_id,
                document_id,
                revision_id,
                chunk_index: 0,
                content_text: content_text.to_string(),
                normalized_text: content_text.to_string(),
                span_start: Some(0),
                span_end: Some(i32::try_from(content_text.len()).unwrap_or(i32::MAX)),
                token_count: Some(3),
                section_path: vec!["grounding".to_string()],
                heading_trail: vec!["Grounding".to_string()],
                chunk_state: "ready".to_string(),
                text_generation: Some(1),
                vector_generation: None,
            })
            .await
            .context("failed to insert grounding chunk")?;

        Ok(())
    }
}

fn canonical_context_bundle_id(execution_id: Uuid) -> Uuid {
    Uuid::new_v5(&Uuid::NAMESPACE_OID, execution_id.as_bytes())
}

fn sample_query_execution(
    workspace_id: Uuid,
    library_id: Uuid,
    execution_id: Uuid,
    query_text: &str,
) -> QueryExecution {
    QueryExecution {
        id: execution_id,
        workspace_id,
        library_id,
        conversation_id: Uuid::now_v7(),
        context_bundle_id: canonical_context_bundle_id(execution_id),
        request_turn_id: None,
        response_turn_id: None,
        binding_id: None,
        execution_state: "retrieving".to_string(),
        query_text: query_text.to_string(),
        failure_code: None,
        started_at: Utc::now(),
        completed_at: None,
    }
}

fn sample_context_bundle(
    workspace_id: Uuid,
    library_id: Uuid,
    execution: &QueryExecution,
) -> KnowledgeContextBundleRow {
    let now = Utc::now();
    KnowledgeContextBundleRow {
        key: canonical_context_bundle_id(execution.id).to_string(),
        arango_id: None,
        arango_rev: None,
        bundle_id: canonical_context_bundle_id(execution.id),
        workspace_id,
        library_id,
        query_execution_id: Some(execution.id),
        bundle_state: "assembling".to_string(),
        bundle_strategy: "grounded_answer".to_string(),
        requested_mode: "grounded_answer".to_string(),
        resolved_mode: "grounded_answer".to_string(),
        freshness_snapshot: json!({
            "active_text_generation": 7,
            "active_vector_generation": 7,
            "active_graph_generation": 7
        }),
        candidate_summary: json!({
            "chunks": 0,
            "entities": 0,
            "relations": 0,
            "evidence": 0
        }),
        assembly_diagnostics: json!({
            "question": execution.query_text,
            "status": "assembling"
        }),
        created_at: now,
        updated_at: now,
    }
}

fn sample_linked_query_execution(
    workspace_id: Uuid,
    library_id: Uuid,
    execution_id: Uuid,
    conversation_id: Uuid,
    query_text: &str,
    execution_state: &str,
    failure_code: Option<&str>,
    request_turn_id: Option<Uuid>,
    response_turn_id: Option<Uuid>,
    binding_id: Option<Uuid>,
    completed_at: Option<chrono::DateTime<Utc>>,
) -> QueryExecution {
    QueryExecution {
        conversation_id,
        request_turn_id,
        response_turn_id,
        binding_id,
        execution_state: execution_state.to_string(),
        failure_code: failure_code.map(ToString::to_string),
        completed_at,
        ..sample_query_execution(workspace_id, library_id, execution_id, query_text)
    }
}

fn sample_trace(
    workspace_id: Uuid,
    library_id: Uuid,
    execution_id: Uuid,
    bundle_id: Uuid,
) -> KnowledgeRetrievalTraceRow {
    let now = Utc::now();
    KnowledgeRetrievalTraceRow {
        key: Uuid::now_v7().to_string(),
        arango_id: None,
        arango_rev: None,
        trace_id: Uuid::now_v7(),
        workspace_id,
        library_id,
        query_execution_id: Some(execution_id),
        bundle_id,
        trace_state: "ready".to_string(),
        retrieval_strategy: "chunk_lexical_first".to_string(),
        candidate_counts: json!({
            "chunk_candidates": 1,
            "entity_candidates": 1,
            "relation_candidates": 1,
            "evidence_candidates": 1
        }),
        dropped_reasons: json!([
            {
                "kind": "debug_scaffold",
                "note": "no ground-truth drops were generated for this fixture"
            }
        ]),
        timing_breakdown: json!({
            "lexical_ms": 1,
            "entity_ms": 1,
            "relation_ms": 1,
            "evidence_ms": 1,
            "bundle_ms": 1
        }),
        diagnostics_json: json!({
            "top_k": 1,
            "answerable": true,
            "grounding_kind": "hybrid"
        }),
        created_at: now,
        updated_at: now,
    }
}

fn sample_async_operation(
    workspace_id: Uuid,
    library_id: Uuid,
    execution_id: Uuid,
    status: &str,
    failure_code: Option<&str>,
) -> OpsAsyncOperation {
    OpsAsyncOperation {
        id: Uuid::now_v7(),
        workspace_id,
        library_id: Some(library_id),
        operation_kind: "query_execution".to_string(),
        status: status.to_string(),
        surface_kind: Some("rest".to_string()),
        subject_kind: Some("query_execution".to_string()),
        subject_id: Some(execution_id),
        failure_code: failure_code.map(ToString::to_string),
        created_at: Utc::now(),
        completed_at: matches!(status, "ready" | "failed" | "canceled").then(Utc::now),
    }
}

fn sample_audit_subject(
    subject_kind: &str,
    subject_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Option<Uuid>,
) -> AuditEventSubject {
    AuditEventSubject {
        audit_event_id: Uuid::now_v7(),
        subject_kind: subject_kind.to_string(),
        subject_id,
        workspace_id: Some(workspace_id),
        library_id: Some(library_id),
        document_id,
        query_session_id: (subject_kind == "query_session").then_some(subject_id),
        query_execution_id: (subject_kind == "query_execution").then_some(subject_id),
        context_bundle_id: (subject_kind == "knowledge_bundle").then_some(subject_id),
        async_operation_id: (subject_kind == "async_operation").then_some(subject_id),
    }
}

fn sample_chunk_edge(bundle_id: Uuid, chunk_id: Uuid) -> KnowledgeBundleChunkEdgeRow {
    KnowledgeBundleChunkEdgeRow {
        key: format!("{bundle_id}:{chunk_id}"),
        arango_id: None,
        arango_rev: None,
        from: String::new(),
        to: String::new(),
        bundle_id,
        chunk_id,
        rank: 1,
        score: 0.91,
        inclusion_reason: Some("lexical_grounding".to_string()),
        created_at: Utc::now(),
    }
}

fn sample_entity_edge(bundle_id: Uuid, entity_id: Uuid) -> KnowledgeBundleEntityEdgeRow {
    KnowledgeBundleEntityEdgeRow {
        key: format!("{bundle_id}:{entity_id}"),
        arango_id: None,
        arango_rev: None,
        from: String::new(),
        to: String::new(),
        bundle_id,
        entity_id,
        rank: 1,
        score: 0.87,
        inclusion_reason: Some("entity_grounding".to_string()),
        created_at: Utc::now(),
    }
}

fn sample_relation_edge(bundle_id: Uuid, relation_id: Uuid) -> KnowledgeBundleRelationEdgeRow {
    KnowledgeBundleRelationEdgeRow {
        key: format!("{bundle_id}:{relation_id}"),
        arango_id: None,
        arango_rev: None,
        from: String::new(),
        to: String::new(),
        bundle_id,
        relation_id,
        rank: 1,
        score: 0.84,
        inclusion_reason: Some("relation_grounding".to_string()),
        created_at: Utc::now(),
    }
}

fn sample_evidence_edge(bundle_id: Uuid, evidence_id: Uuid) -> KnowledgeBundleEvidenceEdgeRow {
    KnowledgeBundleEvidenceEdgeRow {
        key: format!("{bundle_id}:{evidence_id}"),
        arango_id: None,
        arango_rev: None,
        from: String::new(),
        to: String::new(),
        bundle_id,
        evidence_id,
        rank: 1,
        score: 0.83,
        inclusion_reason: Some("evidence_grounding".to_string()),
        created_at: Utc::now(),
    }
}

fn sample_chunk_reference(bundle_id: Uuid, chunk_id: Uuid) -> KnowledgeBundleChunkReferenceRow {
    KnowledgeBundleChunkReferenceRow {
        key: format!("{bundle_id}:{chunk_id}"),
        bundle_id,
        chunk_id,
        rank: 1,
        score: 0.91,
        inclusion_reason: Some("lexical_grounding".to_string()),
        created_at: Utc::now(),
    }
}

fn sample_entity_reference(bundle_id: Uuid, entity_id: Uuid) -> KnowledgeBundleEntityReferenceRow {
    KnowledgeBundleEntityReferenceRow {
        key: format!("{bundle_id}:{entity_id}"),
        bundle_id,
        entity_id,
        rank: 1,
        score: 0.87,
        inclusion_reason: Some("entity_grounding".to_string()),
        created_at: Utc::now(),
    }
}

fn sample_relation_reference(
    bundle_id: Uuid,
    relation_id: Uuid,
) -> KnowledgeBundleRelationReferenceRow {
    KnowledgeBundleRelationReferenceRow {
        key: format!("{bundle_id}:{relation_id}"),
        bundle_id,
        relation_id,
        rank: 1,
        score: 0.84,
        inclusion_reason: Some("relation_grounding".to_string()),
        created_at: Utc::now(),
    }
}

fn sample_evidence_reference(
    bundle_id: Uuid,
    evidence_id: Uuid,
) -> KnowledgeBundleEvidenceReferenceRow {
    KnowledgeBundleEvidenceReferenceRow {
        key: format!("{bundle_id}:{evidence_id}"),
        bundle_id,
        evidence_id,
        rank: 1,
        score: 0.83,
        inclusion_reason: Some("evidence_grounding".to_string()),
        created_at: Utc::now(),
    }
}

#[test]
fn canonical_query_execution_scaffold_uses_execution_keyed_bundle_ids() {
    let _service = QueryService::new();
    let workspace_id = Uuid::now_v7();
    let library_id = Uuid::now_v7();
    let execution_id = Uuid::now_v7();
    let execution = sample_query_execution(
        workspace_id,
        library_id,
        execution_id,
        "What supports the canonical answer?",
    );
    let bundle = sample_context_bundle(workspace_id, library_id, &execution);

    assert_eq!(execution.context_bundle_id, canonical_context_bundle_id(execution.id));
    assert_eq!(bundle.bundle_id, canonical_context_bundle_id(execution.id));
    assert_eq!(bundle.query_execution_id, Some(execution.id));
    assert_eq!(bundle.bundle_strategy, "grounded_answer");
}

#[test]
fn typed_bundle_reference_rows_cover_all_grounding_kinds() {
    let bundle_id = Uuid::now_v7();
    let query_execution_id = Uuid::now_v7();
    let chunk_id = Uuid::now_v7();
    let entity_id = Uuid::now_v7();
    let relation_id = Uuid::now_v7();
    let evidence_id = Uuid::now_v7();

    let chunk_edge = sample_chunk_edge(bundle_id, chunk_id);
    let entity_edge = sample_entity_edge(bundle_id, entity_id);
    let relation_edge = sample_relation_edge(bundle_id, relation_id);
    let evidence_edge = sample_evidence_edge(bundle_id, evidence_id);
    let chunk_reference = sample_chunk_reference(bundle_id, chunk_id);
    let entity_reference = sample_entity_reference(bundle_id, entity_id);
    let relation_reference = sample_relation_reference(bundle_id, relation_id);
    let evidence_reference = sample_evidence_reference(bundle_id, evidence_id);

    assert_eq!(chunk_edge.bundle_id, bundle_id);
    assert_eq!(chunk_edge.chunk_id, chunk_id);
    assert_eq!(chunk_edge.key, format!("{bundle_id}:{chunk_id}"));
    assert_eq!(entity_edge.bundle_id, bundle_id);
    assert_eq!(entity_edge.entity_id, entity_id);
    assert_eq!(entity_edge.key, format!("{bundle_id}:{entity_id}"));
    assert_eq!(relation_edge.bundle_id, bundle_id);
    assert_eq!(relation_edge.relation_id, relation_id);
    assert_eq!(relation_edge.key, format!("{bundle_id}:{relation_id}"));
    assert_eq!(evidence_edge.bundle_id, bundle_id);
    assert_eq!(evidence_edge.evidence_id, evidence_id);
    assert_eq!(evidence_edge.key, format!("{bundle_id}:{evidence_id}"));

    let reference_set = KnowledgeContextBundleReferenceSetRow {
        bundle: KnowledgeContextBundleRow {
            key: bundle_id.to_string(),
            arango_id: None,
            arango_rev: None,
            bundle_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            query_execution_id: Some(query_execution_id),
            bundle_state: "ready".to_string(),
            bundle_strategy: "grounded_answer".to_string(),
            requested_mode: "grounded_answer".to_string(),
            resolved_mode: "grounded_answer".to_string(),
            freshness_snapshot: json!({
                "active_text_generation": 7,
                "active_vector_generation": 7,
                "active_graph_generation": 7
            }),
            candidate_summary: json!({
                "chunks": 1,
                "entities": 1,
                "relations": 1,
                "evidence": 1
            }),
            assembly_diagnostics: json!({
                "answerable": true,
                "grounding_kind": "hybrid"
            }),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        chunk_references: vec![chunk_reference],
        entity_references: vec![entity_reference],
        relation_references: vec![relation_reference],
        evidence_references: vec![evidence_reference],
    };

    assert_eq!(reference_set.bundle.bundle_id, bundle_id);
    assert_eq!(reference_set.bundle.query_execution_id, Some(query_execution_id));
    assert_eq!(reference_set.bundle.candidate_summary["chunks"], json!(1));
    assert_eq!(reference_set.bundle.candidate_summary["entities"], json!(1));
    assert_eq!(reference_set.bundle.candidate_summary["relations"], json!(1));
    assert_eq!(reference_set.bundle.candidate_summary["evidence"], json!(1));
    assert_eq!(reference_set.bundle.assembly_diagnostics["grounding_kind"], json!("hybrid"));
    assert_eq!(reference_set.chunk_references[0].chunk_id, chunk_id);
    assert_eq!(reference_set.chunk_references[0].bundle_id, bundle_id);
    assert_eq!(
        reference_set.chunk_references[0].inclusion_reason.as_deref(),
        Some("lexical_grounding")
    );
    assert_eq!(reference_set.entity_references[0].entity_id, entity_id);
    assert_eq!(reference_set.entity_references[0].bundle_id, bundle_id);
    assert_eq!(
        reference_set.entity_references[0].inclusion_reason.as_deref(),
        Some("entity_grounding")
    );
    assert_eq!(reference_set.relation_references[0].relation_id, relation_id);
    assert_eq!(reference_set.relation_references[0].bundle_id, bundle_id);
    assert_eq!(
        reference_set.relation_references[0].inclusion_reason.as_deref(),
        Some("relation_grounding")
    );
    assert_eq!(reference_set.evidence_references[0].evidence_id, evidence_id);
    assert_eq!(reference_set.evidence_references[0].bundle_id, bundle_id);
    assert_eq!(
        reference_set.evidence_references[0].inclusion_reason.as_deref(),
        Some("evidence_grounding")
    );
    assert_eq!(reference_set.chunk_references.len(), 1);
    assert_eq!(reference_set.entity_references.len(), 1);
    assert_eq!(reference_set.relation_references.len(), 1);
    assert_eq!(reference_set.evidence_references.len(), 1);
}

#[test]
fn failure_cancellation_and_retry_scaffold_preserve_execution_bundle_linkage() {
    let workspace_id = Uuid::now_v7();
    let library_id = Uuid::now_v7();
    let conversation_id = Uuid::now_v7();
    let request_turn_id = Uuid::now_v7();
    let response_turn_id = Uuid::now_v7();
    let binding_id = Uuid::now_v7();
    let query_text = "Which anchors survive failure, cancellation, and retry?";

    let failed_execution_id = Uuid::now_v7();
    let canceled_execution_id = Uuid::now_v7();
    let retry_execution_id = Uuid::now_v7();

    let failed = sample_linked_query_execution(
        workspace_id,
        library_id,
        failed_execution_id,
        conversation_id,
        query_text,
        "failed",
        Some("provider_timeout"),
        Some(request_turn_id),
        None,
        Some(binding_id),
        Some(Utc::now()),
    );
    let canceled = sample_linked_query_execution(
        workspace_id,
        library_id,
        canceled_execution_id,
        conversation_id,
        query_text,
        "canceled",
        Some("canceled_by_user"),
        Some(request_turn_id),
        None,
        Some(binding_id),
        Some(Utc::now()),
    );
    let retried = sample_linked_query_execution(
        workspace_id,
        library_id,
        retry_execution_id,
        conversation_id,
        query_text,
        "retrieving",
        None,
        Some(request_turn_id),
        Some(response_turn_id),
        Some(binding_id),
        None,
    );

    let failed_bundle = sample_context_bundle(workspace_id, library_id, &failed);
    let canceled_bundle = sample_context_bundle(workspace_id, library_id, &canceled);
    let retried_bundle = sample_context_bundle(workspace_id, library_id, &retried);

    assert_eq!(failed.context_bundle_id, canonical_context_bundle_id(failed.id));
    assert_eq!(canceled.context_bundle_id, canonical_context_bundle_id(canceled.id));
    assert_eq!(retried.context_bundle_id, canonical_context_bundle_id(retried.id));
    assert_eq!(failed.request_turn_id, Some(request_turn_id));
    assert_eq!(canceled.request_turn_id, Some(request_turn_id));
    assert_eq!(retried.request_turn_id, Some(request_turn_id));
    assert_eq!(failed.response_turn_id, None);
    assert_eq!(canceled.response_turn_id, None);
    assert_eq!(retried.response_turn_id, Some(response_turn_id));
    assert_eq!(failed.binding_id, Some(binding_id));
    assert_eq!(canceled.binding_id, Some(binding_id));
    assert_eq!(retried.binding_id, Some(binding_id));
    assert_eq!(failed.execution_state, "failed");
    assert_eq!(canceled.execution_state, "canceled");
    assert_eq!(retried.execution_state, "retrieving");
    assert_eq!(failed.failure_code.as_deref(), Some("provider_timeout"));
    assert_eq!(canceled.failure_code.as_deref(), Some("canceled_by_user"));
    assert_eq!(retried.failure_code, None);
    assert_eq!(failed.query_text, query_text);
    assert_eq!(canceled.query_text, query_text);
    assert_eq!(retried.query_text, query_text);
    assert_eq!(failed_bundle.assembly_diagnostics["question"], json!(query_text));
    assert_eq!(canceled_bundle.assembly_diagnostics["question"], json!(query_text));
    assert_eq!(retried_bundle.assembly_diagnostics["question"], json!(query_text));
    assert_eq!(failed_bundle.candidate_summary["chunks"], json!(0));
    assert_eq!(canceled_bundle.candidate_summary["chunks"], json!(0));
    assert_eq!(retried_bundle.candidate_summary["chunks"], json!(0));
    assert_eq!(failed_bundle.query_execution_id, Some(failed.id));
    assert_eq!(canceled_bundle.query_execution_id, Some(canceled.id));
    assert_eq!(retried_bundle.query_execution_id, Some(retried.id));
    assert_eq!(failed_bundle.bundle_id, failed.context_bundle_id);
    assert_eq!(canceled_bundle.bundle_id, canceled.context_bundle_id);
    assert_eq!(retried_bundle.bundle_id, retried.context_bundle_id);
    assert_eq!(failed_bundle.bundle_strategy, "grounded_answer");
    assert_eq!(canceled_bundle.bundle_strategy, "grounded_answer");
    assert_eq!(retried_bundle.bundle_strategy, "grounded_answer");

    let failed_operation = sample_async_operation(
        workspace_id,
        library_id,
        failed.id,
        "failed",
        failed.failure_code.as_deref(),
    );
    let canceled_operation = sample_async_operation(
        workspace_id,
        library_id,
        canceled.id,
        "failed",
        canceled.failure_code.as_deref(),
    );
    let retried_operation =
        sample_async_operation(workspace_id, library_id, retried.id, "processing", None);

    let failed_execution_subject =
        sample_audit_subject("query_execution", failed.id, workspace_id, library_id, None);
    let failed_bundle_subject = sample_audit_subject(
        "knowledge_bundle",
        failed_bundle.bundle_id,
        workspace_id,
        library_id,
        None,
    );
    let failed_operation_subject = sample_audit_subject(
        "async_operation",
        failed_operation.id,
        workspace_id,
        library_id,
        None,
    );

    assert_eq!(failed_operation.subject_kind.as_deref(), Some("query_execution"));
    assert_eq!(failed_operation.subject_id, Some(failed.id));
    assert_eq!(failed_operation.failure_code.as_deref(), Some("provider_timeout"));
    assert_eq!(canceled_operation.subject_kind.as_deref(), Some("query_execution"));
    assert_eq!(canceled_operation.subject_id, Some(canceled.id));
    assert_eq!(canceled_operation.failure_code.as_deref(), Some("canceled_by_user"));
    assert_eq!(retried_operation.subject_kind.as_deref(), Some("query_execution"));
    assert_eq!(retried_operation.subject_id, Some(retried.id));
    assert_eq!(retried_operation.failure_code, None);
    assert_eq!(failed_execution_subject.query_execution_id, Some(failed.id));
    assert_eq!(failed_bundle_subject.context_bundle_id, Some(failed_bundle.bundle_id));
    assert_eq!(failed_operation_subject.async_operation_id, Some(failed_operation.id));
    assert_eq!(failed_execution_subject.library_id, Some(library_id));
    assert_eq!(failed_bundle_subject.workspace_id, Some(workspace_id));
    assert_eq!(failed_operation_subject.subject_kind, "async_operation");
}

#[tokio::test]
#[ignore = "requires local ArangoDB service with database create/drop access"]
async fn context_bundle_roundtrip_by_query_execution_persists_trace_and_chunk_references()
-> Result<()> {
    let fixture = QueryGroundingFixture::create().await?;
    let result = async {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let execution_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let chunk_id = Uuid::now_v7();
        let entity_id = Uuid::now_v7();
        let relation_id = Uuid::now_v7();
        let evidence_id = Uuid::now_v7();
        let execution = sample_query_execution(
            workspace_id,
            library_id,
            execution_id,
            "Which chunk grounds this answer?",
        );
        let bundle = sample_context_bundle(workspace_id, library_id, &execution);

        fixture
            .seed_chunk(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                chunk_id,
                "grounding anchor chunk",
            )
            .await?;

        fixture
            .context_store
            .upsert_bundle(&bundle)
            .await
            .context("failed to persist grounding context bundle")?;
        fixture
            .context_store
            .upsert_trace(&sample_trace(workspace_id, library_id, execution.id, bundle.bundle_id))
            .await
            .context("failed to persist grounding retrieval trace")?;
        fixture
            .context_store
            .replace_bundle_chunk_edges(
                bundle.bundle_id,
                &[sample_chunk_edge(bundle.bundle_id, chunk_id)],
            )
            .await
            .context("failed to persist grounding chunk references")?;
        fixture
            .context_store
            .replace_bundle_entity_edges(
                bundle.bundle_id,
                &[sample_entity_edge(bundle.bundle_id, entity_id)],
            )
            .await
            .context("failed to persist grounding entity references")?;
        fixture
            .context_store
            .replace_bundle_relation_edges(
                bundle.bundle_id,
                &[sample_relation_edge(bundle.bundle_id, relation_id)],
            )
            .await
            .context("failed to persist grounding relation references")?;
        fixture
            .context_store
            .replace_bundle_evidence_edges(
                bundle.bundle_id,
                &[sample_evidence_edge(bundle.bundle_id, evidence_id)],
            )
            .await
            .context("failed to persist grounding evidence references")?;
        fixture
            .context_store
            .update_bundle_state(
                bundle.bundle_id,
                "ready",
                json!({
                    "active_text_generation": 7,
                    "active_vector_generation": 7,
                    "active_graph_generation": 7
                }),
                json!({
                    "chunks": 1,
                    "entities": 1,
                    "relations": 1,
                    "evidence": 1
                }),
                json!({
                    "answerable": true,
                    "grounding_kind": "hybrid"
                }),
            )
            .await
            .context("failed to update grounding bundle state")?
            .ok_or_else(|| anyhow!("grounding context bundle disappeared during update"))?;

        let persisted_bundle = fixture
            .context_store
            .get_bundle_by_query_execution(execution.id)
            .await
            .context("failed to load context bundle by query execution")?
            .ok_or_else(|| anyhow!("context bundle not found for query execution"))?;
        assert_eq!(persisted_bundle.bundle_id, execution.id);
        assert_eq!(persisted_bundle.bundle_state, "ready");

        let reference_set = fixture
            .context_store
            .get_bundle_reference_set_by_query_execution(execution.id)
            .await
            .context("failed to load materialized context bundle by query execution")?
            .ok_or_else(|| anyhow!("materialized context bundle not found for query execution"))?;
        assert_eq!(reference_set.bundle.query_execution_id, Some(execution.id));
        assert_eq!(reference_set.chunk_references.len(), 1);
        assert_eq!(reference_set.chunk_references[0].chunk_id, chunk_id);
        assert_eq!(reference_set.chunk_references[0].rank, 1);
        assert_eq!(
            reference_set.chunk_references[0].inclusion_reason.as_deref(),
            Some("lexical_grounding")
        );
        assert_eq!(reference_set.entity_references.len(), 1);
        assert_eq!(reference_set.entity_references[0].entity_id, entity_id);
        assert_eq!(reference_set.entity_references[0].rank, 1);
        assert_eq!(
            reference_set.entity_references[0].inclusion_reason.as_deref(),
            Some("entity_grounding")
        );
        assert_eq!(reference_set.relation_references.len(), 1);
        assert_eq!(reference_set.relation_references[0].relation_id, relation_id);
        assert_eq!(reference_set.relation_references[0].rank, 1);
        assert_eq!(
            reference_set.relation_references[0].inclusion_reason.as_deref(),
            Some("relation_grounding")
        );
        assert_eq!(reference_set.evidence_references.len(), 1);
        assert_eq!(reference_set.evidence_references[0].evidence_id, evidence_id);
        assert_eq!(reference_set.evidence_references[0].rank, 1);
        assert_eq!(
            reference_set.evidence_references[0].inclusion_reason.as_deref(),
            Some("evidence_grounding")
        );

        let traces = fixture
            .context_store
            .list_traces_by_query_execution(execution.id)
            .await
            .context("failed to list retrieval traces by query execution")?;
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0].bundle_id, execution.id);
        assert_eq!(traces[0].query_execution_id, Some(execution.id));
        assert_eq!(traces[0].trace_state, "ready");
        assert_eq!(traces[0].retrieval_strategy, "chunk_lexical_first");
        assert_eq!(traces[0].candidate_counts["chunk_candidates"], json!(1));
        assert_eq!(traces[0].candidate_counts["entity_candidates"], json!(1));
        assert_eq!(traces[0].candidate_counts["relation_candidates"], json!(1));
        assert_eq!(traces[0].candidate_counts["evidence_candidates"], json!(1));
        assert_eq!(traces[0].dropped_reasons[0]["kind"], json!("debug_scaffold"));
        assert_eq!(traces[0].timing_breakdown["bundle_ms"], json!(1));
        assert_eq!(traces[0].diagnostics_json["grounding_kind"], json!("hybrid"));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local ArangoDB service with database create/drop access"]
async fn entity_neighborhood_filters_out_context_bundle_vertices() -> Result<()> {
    let fixture = QueryGroundingFixture::create().await?;
    let result = async {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let execution_id = Uuid::now_v7();
        let entity_id = Uuid::now_v7();
        let execution = sample_query_execution(
            workspace_id,
            library_id,
            execution_id,
            "Which neighbors should stay inside the domain graph?",
        );
        let bundle = sample_context_bundle(workspace_id, library_id, &execution);

        fixture
            .graph_store
            .upsert_entity(&NewKnowledgeEntity {
                entity_id,
                workspace_id,
                library_id,
                canonical_label: "Paging Parameter".to_string(),
                aliases: vec!["pageSize".to_string()],
                entity_type: "parameter".to_string(),
                summary: Some("Pagination parameter surfaced for regression coverage.".to_string()),
                confidence: Some(0.99),
                support_count: 1,
                freshness_generation: 1,
                entity_state: "active".to_string(),
                created_at: Some(Utc::now()),
                updated_at: Some(Utc::now()),
            })
            .await
            .context("failed to seed entity for traversal regression")?;

        fixture
            .context_store
            .upsert_bundle(&bundle)
            .await
            .context("failed to persist traversal regression bundle")?;
        fixture
            .context_store
            .replace_bundle_entity_edges(
                bundle.bundle_id,
                &[sample_entity_edge(bundle.bundle_id, entity_id)],
            )
            .await
            .context("failed to persist traversal regression bundle edge")?;

        let rows = fixture
            .graph_store
            .list_entity_neighborhood(entity_id, library_id, 2, 16)
            .await
            .context("failed to list entity neighborhood after bundle edge insert")?;

        assert_eq!(rows.len(), 1, "service should keep only domain vertices in traversal rows");
        assert_eq!(rows[0].vertex_kind, "knowledge_entity");
        assert_eq!(rows[0].vertex_id, entity_id);
        assert_eq!(rows[0].path_length, 0);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
