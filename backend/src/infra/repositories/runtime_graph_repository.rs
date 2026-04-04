#![allow(clippy::missing_errors_doc, clippy::too_many_arguments, clippy::too_many_lines)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::{RuntimeGraphConvergenceCountersRow, RuntimeGraphFilteredArtifactRow};

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphSnapshotRow {
    pub project_id: Uuid,
    pub graph_status: String,
    pub projection_version: i64,
    pub node_count: i32,
    pub edge_count: i32,
    pub provenance_coverage_percent: Option<f64>,
    pub last_built_at: Option<DateTime<Utc>>,
    pub last_error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphNodeRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub canonical_key: String,
    pub label: String,
    pub node_type: String,
    pub aliases_json: serde_json::Value,
    pub summary: Option<String>,
    pub metadata_json: serde_json::Value,
    pub support_count: i32,
    pub projection_version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphEdgeRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub from_node_id: Uuid,
    pub to_node_id: Uuid,
    pub relation_type: String,
    pub canonical_key: String,
    pub summary: Option<String>,
    pub weight: Option<f64>,
    pub support_count: i32,
    pub metadata_json: serde_json::Value,
    pub projection_version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphEvidenceRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub target_kind: String,
    pub target_id: Uuid,
    pub document_id: Option<Uuid>,
    pub chunk_id: Option<Uuid>,
    pub source_file_name: Option<String>,
    pub page_ref: Option<String>,
    pub evidence_text: String,
    pub confidence_score: Option<f64>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

#[must_use]
pub fn runtime_graph_evidence_identity_key(
    target_kind: &str,
    target_id: Uuid,
    document_id: Option<Uuid>,
    revision_id: Option<Uuid>,
    activated_by_attempt_id: Option<Uuid>,
    chunk_id: Option<Uuid>,
    page_ref: Option<&str>,
    source_file_name: Option<&str>,
    evidence_context_key: &str,
) -> String {
    format!(
        "{target_kind}|{target_id}|{}|{}|{}|{}|{}|{}|{evidence_context_key}",
        document_id.map_or_else(|| "-".to_string(), |value| value.to_string()),
        revision_id.map_or_else(|| "-".to_string(), |value| value.to_string()),
        activated_by_attempt_id.map_or_else(|| "-".to_string(), |value| value.to_string()),
        chunk_id.map_or_else(|| "-".to_string(), |value| value.to_string()),
        page_ref.unwrap_or_default(),
        source_file_name.unwrap_or_default(),
    )
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphEvidenceLifecycleRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub target_kind: String,
    pub target_id: Uuid,
    pub document_id: Option<Uuid>,
    pub revision_id: Option<Uuid>,
    pub activated_by_attempt_id: Option<Uuid>,
    pub deactivated_by_mutation_id: Option<Uuid>,
    pub chunk_id: Option<Uuid>,
    pub source_file_name: Option<String>,
    pub page_ref: Option<String>,
    pub evidence_text: String,
    pub confidence_score: Option<f64>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphContributionCountsRow {
    pub node_count: i64,
    pub edge_count: i64,
    pub evidence_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphProjectionCountsRow {
    pub node_count: i64,
    pub edge_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphDocumentLinkRow {
    pub document_id: Uuid,
    pub target_node_id: Uuid,
    pub target_node_type: String,
    pub relation_type: String,
    pub support_count: i64,
}

/// Loads the active runtime graph snapshot for one project.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph snapshot.
pub async fn get_runtime_graph_snapshot(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Option<RuntimeGraphSnapshotRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphSnapshotRow>(
        "select project_id, graph_status, projection_version, node_count, edge_count,
            provenance_coverage_percent, last_built_at, last_error_message, created_at, updated_at
         from runtime_graph_snapshot
         where project_id = $1",
    )
    .bind(project_id)
    .fetch_optional(pool)
    .await
}

/// Upserts a runtime graph snapshot.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating the graph snapshot.
pub async fn upsert_runtime_graph_snapshot(
    pool: &PgPool,
    project_id: Uuid,
    graph_status: &str,
    projection_version: i64,
    node_count: i32,
    edge_count: i32,
    provenance_coverage_percent: Option<f64>,
    last_error_message: Option<&str>,
) -> Result<RuntimeGraphSnapshotRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphSnapshotRow>(
        "insert into runtime_graph_snapshot (
            project_id, graph_status, projection_version, node_count, edge_count,
            provenance_coverage_percent, last_built_at, last_error_message
         ) values ($1, $2, $3, $4, $5, $6, now(), $7)
         on conflict (project_id) do update
         set graph_status = excluded.graph_status,
             projection_version = excluded.projection_version,
             node_count = excluded.node_count,
             edge_count = excluded.edge_count,
             provenance_coverage_percent = excluded.provenance_coverage_percent,
             last_built_at = now(),
             last_error_message = excluded.last_error_message,
             updated_at = now()
         returning project_id, graph_status, projection_version, node_count, edge_count,
            provenance_coverage_percent, last_built_at, last_error_message, created_at, updated_at",
    )
    .bind(project_id)
    .bind(graph_status)
    .bind(projection_version)
    .bind(node_count)
    .bind(edge_count)
    .bind(provenance_coverage_percent)
    .bind(last_error_message)
    .fetch_one(pool)
    .await
}

/// Persists one filtered graph artifact for later diagnostics.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the filtered artifact row.
pub async fn create_runtime_graph_filtered_artifact(
    pool: &PgPool,
    project_id: Uuid,
    ingestion_run_id: Option<Uuid>,
    revision_id: Option<Uuid>,
    target_kind: &str,
    candidate_key: &str,
    source_node_key: Option<&str>,
    target_node_key: Option<&str>,
    relation_type: Option<&str>,
    filter_reason: &str,
    summary: Option<&str>,
    metadata_json: serde_json::Value,
) -> Result<RuntimeGraphFilteredArtifactRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphFilteredArtifactRow>(
        "insert into runtime_graph_filtered_artifact (
            id, project_id, ingestion_run_id, revision_id, target_kind, candidate_key,
            source_node_key, target_node_key, relation_type, filter_reason, summary, metadata_json
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
         returning id, project_id, ingestion_run_id, revision_id, target_kind, candidate_key,
            source_node_key, target_node_key, relation_type, filter_reason, summary, metadata_json, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(ingestion_run_id)
    .bind(revision_id)
    .bind(target_kind)
    .bind(candidate_key)
    .bind(source_node_key)
    .bind(target_node_key)
    .bind(relation_type)
    .bind(filter_reason)
    .bind(summary)
    .bind(metadata_json)
    .fetch_one(pool)
    .await
}

/// Lists filtered graph artifacts for one project.
///
/// # Errors
/// Returns any `SQLx` error raised while querying filtered artifact rows.
pub async fn list_runtime_graph_filtered_artifacts_by_project(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Vec<RuntimeGraphFilteredArtifactRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphFilteredArtifactRow>(
        "select id, project_id, ingestion_run_id, revision_id, target_kind, candidate_key,
            source_node_key, target_node_key, relation_type, filter_reason, summary, metadata_json, created_at
         from runtime_graph_filtered_artifact
         where project_id = $1
         order by created_at desc, id desc",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
}

/// Loads convergence and filtered-artifact counters for one project.
///
/// # Errors
/// Returns any `SQLx` error raised while querying convergence counters.
pub async fn load_runtime_graph_convergence_counters(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<RuntimeGraphConvergenceCountersRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphConvergenceCountersRow>(
        "with visible_runs as (
            select run.status
            from runtime_ingestion_run as run
            where run.project_id = $1
              and (
                    run.document_id is null
                    or not exists (
                        select 1
                        from document
                        where document.id = run.document_id
                          and document.deleted_at is not null
                    )
              )
         ),
         backlog as (
            select
                count(*) filter (where status = 'queued') as queued_document_count,
                count(*) filter (where status = 'processing') as processing_document_count,
                count(*) filter (where status = 'ready_no_graph') as ready_no_graph_count
            from visible_runs
         ),
         mutation_backlog as (
            select
                count(*) filter (
                    where active_mutation_kind in ('update_append', 'update_replace')
                      and active_mutation_status in ('accepted', 'reconciling')
                      and deleted_at is null
                ) as pending_update_count,
                count(*) filter (
                    where active_mutation_kind = 'delete'
                      and active_mutation_status in ('accepted', 'reconciling')
                ) as pending_delete_count
            from document
            where project_id = $1
         ),
         latest_failed_mutation as (
            select active_mutation_kind
            from document
            where project_id = $1
              and active_mutation_status = 'failed'
            order by updated_at desc, id desc
            limit 1
         ),
         filtered as (
            select
                count(distinct artifact_identity) as filtered_artifact_count,
                count(distinct case when filter_reason = 'empty_relation' then artifact_identity end) as filtered_empty_relation_count,
                count(distinct case when filter_reason = 'degenerate_self_loop' then artifact_identity end) as filtered_degenerate_loop_count
            from (
                select
                    concat_ws(
                        ':',
                        coalesce(artifact.revision_id::text, 'none'),
                        coalesce(artifact.ingestion_run_id::text, 'none'),
                        artifact.target_kind,
                        artifact.candidate_key,
                        artifact.filter_reason
                    ) as artifact_identity,
                    artifact.filter_reason
                from runtime_graph_filtered_artifact as artifact
                left join document_revision as revision
                    on revision.id = artifact.revision_id
                left join document as document
                    on document.id = revision.document_id
                where artifact.project_id = $1
                  and (
                        artifact.revision_id is null
                        or (
                            document.deleted_at is null
                            and (
                                document.current_revision_id = revision.id
                                or (
                                    document.current_revision_id is null
                                    and coalesce(revision.status, '') not in ('superseded', 'deleted', 'failed')
                                )
                            )
                        )
                  )
            ) as active_filtered
         )
         select
            coalesce(backlog.queued_document_count, 0) as queued_document_count,
            coalesce(backlog.processing_document_count, 0) as processing_document_count,
            coalesce(backlog.ready_no_graph_count, 0) as ready_no_graph_count,
            coalesce(mutation_backlog.pending_update_count, 0) as pending_update_count,
            coalesce(mutation_backlog.pending_delete_count, 0) as pending_delete_count,
            coalesce(filtered.filtered_artifact_count, 0) as filtered_artifact_count,
            coalesce(filtered.filtered_empty_relation_count, 0) as filtered_empty_relation_count,
            coalesce(filtered.filtered_degenerate_loop_count, 0) as filtered_degenerate_loop_count,
            latest_failed_mutation.active_mutation_kind as latest_failed_mutation_kind
         from backlog
         cross join mutation_backlog
         cross join filtered
         left join latest_failed_mutation on true",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await
}

/// Lists admitted runtime graph nodes for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying admitted graph nodes.
pub async fn list_admitted_runtime_graph_nodes_by_projection(
    pool: &PgPool,
    project_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphNodeRow>(&admitted_runtime_graph_nodes_query(""))
        .bind(project_id)
        .bind(projection_version)
        .fetch_all(pool)
        .await
}

/// Lists admitted runtime graph nodes by id for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying admitted graph nodes.
pub async fn list_admitted_runtime_graph_nodes_by_ids(
    pool: &PgPool,
    project_id: Uuid,
    projection_version: i64,
    node_ids: &[Uuid],
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, RuntimeGraphNodeRow>(&admitted_runtime_graph_nodes_query(
        "and node.id = any($3)",
    ))
    .bind(project_id)
    .bind(projection_version)
    .bind(node_ids)
    .fetch_all(pool)
    .await
}

/// Lists admitted runtime graph edges for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying admitted graph edges.
pub async fn list_admitted_runtime_graph_edges_by_projection(
    pool: &PgPool,
    project_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphEdgeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEdgeRow>(
        "select id, project_id, from_node_id, to_node_id, relation_type, canonical_key,
            summary, weight, support_count, metadata_json, projection_version, created_at, updated_at
         from runtime_graph_edge
         where project_id = $1
           and projection_version = $2
           and btrim(relation_type) <> ''
           and from_node_id <> to_node_id
         order by relation_type asc, created_at asc",
    )
    .bind(project_id)
    .bind(projection_version)
    .fetch_all(pool)
    .await
}

/// Lists admitted runtime graph edges by id for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying admitted graph edges.
pub async fn list_admitted_runtime_graph_edges_by_ids(
    pool: &PgPool,
    project_id: Uuid,
    projection_version: i64,
    edge_ids: &[Uuid],
) -> Result<Vec<RuntimeGraphEdgeRow>, sqlx::Error> {
    if edge_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, RuntimeGraphEdgeRow>(
        "select id, project_id, from_node_id, to_node_id, relation_type, canonical_key,
            summary, weight, support_count, metadata_json, projection_version, created_at, updated_at
         from runtime_graph_edge
         where project_id = $1
           and projection_version = $2
           and id = any($3)
           and btrim(relation_type) <> ''
           and from_node_id <> to_node_id
         order by relation_type asc, created_at asc",
    )
    .bind(project_id)
    .bind(projection_version)
    .bind(edge_ids)
    .fetch_all(pool)
    .await
}

/// Lists admitted runtime graph edges that touch any of the supplied node ids.
///
/// # Errors
/// Returns any `SQLx` error raised while querying admitted graph edges.
pub async fn list_admitted_runtime_graph_edges_by_node_ids(
    pool: &PgPool,
    project_id: Uuid,
    projection_version: i64,
    node_ids: &[Uuid],
) -> Result<Vec<RuntimeGraphEdgeRow>, sqlx::Error> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, RuntimeGraphEdgeRow>(
        "select id, project_id, from_node_id, to_node_id, relation_type, canonical_key,
            summary, weight, support_count, metadata_json, projection_version, created_at, updated_at
         from runtime_graph_edge
         where project_id = $1
           and projection_version = $2
           and (from_node_id = any($3) or to_node_id = any($3))
           and btrim(relation_type) <> ''
           and from_node_id <> to_node_id
         order by relation_type asc, created_at asc",
    )
    .bind(project_id)
    .bind(projection_version)
    .bind(node_ids)
    .fetch_all(pool)
    .await
}

/// Upserts a canonical runtime graph node.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating the graph node.
pub async fn upsert_runtime_graph_node(
    pool: &PgPool,
    project_id: Uuid,
    canonical_key: &str,
    label: &str,
    node_type: &str,
    aliases_json: serde_json::Value,
    summary: Option<&str>,
    metadata_json: serde_json::Value,
    support_count: i32,
    projection_version: i64,
) -> Result<RuntimeGraphNodeRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "insert into runtime_graph_node (
            id, project_id, canonical_key, label, node_type, aliases_json, summary,
            metadata_json, support_count, projection_version
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         on conflict (project_id, canonical_key, projection_version) do update
         set label = excluded.label,
             node_type = excluded.node_type,
             aliases_json = excluded.aliases_json,
             summary = excluded.summary,
             metadata_json = excluded.metadata_json,
             support_count = excluded.support_count,
             updated_at = now()
         returning id, project_id, canonical_key, label, node_type, aliases_json, summary,
            metadata_json, support_count, projection_version, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(canonical_key)
    .bind(label)
    .bind(node_type)
    .bind(aliases_json)
    .bind(summary)
    .bind(metadata_json)
    .bind(support_count)
    .bind(projection_version)
    .fetch_one(pool)
    .await
}

/// Loads one canonical runtime graph node for a projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph node.
pub async fn get_runtime_graph_node_by_key(
    pool: &PgPool,
    project_id: Uuid,
    canonical_key: &str,
    projection_version: i64,
) -> Result<Option<RuntimeGraphNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "select id, project_id, canonical_key, label, node_type, aliases_json, summary,
            metadata_json, support_count, projection_version, created_at, updated_at
         from runtime_graph_node
         where project_id = $1 and canonical_key = $2 and projection_version = $3",
    )
    .bind(project_id)
    .bind(canonical_key)
    .bind(projection_version)
    .fetch_optional(pool)
    .await
}

/// Loads one canonical runtime graph node by id.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph node.
pub async fn get_runtime_graph_node_by_id(
    pool: &PgPool,
    project_id: Uuid,
    id: Uuid,
) -> Result<Option<RuntimeGraphNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "select id, project_id, canonical_key, label, node_type, aliases_json, summary,
            metadata_json, support_count, projection_version, created_at, updated_at
         from runtime_graph_node
         where project_id = $1 and id = $2",
    )
    .bind(project_id)
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Lists canonical runtime graph nodes for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph nodes.
pub async fn list_runtime_graph_nodes_by_projection(
    pool: &PgPool,
    project_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "select id, project_id, canonical_key, label, node_type, aliases_json, summary,
            metadata_json, support_count, projection_version, created_at, updated_at
         from runtime_graph_node
         where project_id = $1 and projection_version = $2
         order by node_type asc, label asc, created_at asc",
    )
    .bind(project_id)
    .bind(projection_version)
    .fetch_all(pool)
    .await
}

/// Upserts a canonical runtime graph edge.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating the graph edge.
pub async fn upsert_runtime_graph_edge(
    pool: &PgPool,
    project_id: Uuid,
    from_node_id: Uuid,
    to_node_id: Uuid,
    relation_type: &str,
    canonical_key: &str,
    summary: Option<&str>,
    weight: Option<f64>,
    support_count: i32,
    metadata_json: serde_json::Value,
    projection_version: i64,
) -> Result<RuntimeGraphEdgeRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEdgeRow>(
        "insert into runtime_graph_edge (
            id, project_id, from_node_id, to_node_id, relation_type, canonical_key, summary,
            weight, support_count, metadata_json, projection_version
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
         on conflict (project_id, canonical_key, projection_version) do update
         set from_node_id = excluded.from_node_id,
             to_node_id = excluded.to_node_id,
             relation_type = excluded.relation_type,
             summary = excluded.summary,
             weight = excluded.weight,
             support_count = excluded.support_count,
             metadata_json = excluded.metadata_json,
             updated_at = now()
         returning id, project_id, from_node_id, to_node_id, relation_type, canonical_key,
            summary, weight, support_count, metadata_json, projection_version, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(from_node_id)
    .bind(to_node_id)
    .bind(relation_type)
    .bind(canonical_key)
    .bind(summary)
    .bind(weight)
    .bind(support_count)
    .bind(metadata_json)
    .bind(projection_version)
    .fetch_one(pool)
    .await
}

/// Loads one canonical runtime graph edge for a projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph edge.
pub async fn get_runtime_graph_edge_by_key(
    pool: &PgPool,
    project_id: Uuid,
    canonical_key: &str,
    projection_version: i64,
) -> Result<Option<RuntimeGraphEdgeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEdgeRow>(
        "select id, project_id, from_node_id, to_node_id, relation_type, canonical_key,
            summary, weight, support_count, metadata_json, projection_version, created_at, updated_at
         from runtime_graph_edge
         where project_id = $1 and canonical_key = $2 and projection_version = $3",
    )
    .bind(project_id)
    .bind(canonical_key)
    .bind(projection_version)
    .fetch_optional(pool)
    .await
}

/// Loads one canonical runtime graph edge by id.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph edge.
pub async fn get_runtime_graph_edge_by_id(
    pool: &PgPool,
    project_id: Uuid,
    id: Uuid,
) -> Result<Option<RuntimeGraphEdgeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEdgeRow>(
        "select id, project_id, from_node_id, to_node_id, relation_type, canonical_key,
            summary, weight, support_count, metadata_json, projection_version, created_at, updated_at
         from runtime_graph_edge
         where project_id = $1 and id = $2",
    )
    .bind(project_id)
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Lists canonical runtime graph edges for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph edges.
pub async fn list_runtime_graph_edges_by_projection(
    pool: &PgPool,
    project_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphEdgeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEdgeRow>(
        "select id, project_id, from_node_id, to_node_id, relation_type, canonical_key,
            summary, weight, support_count, metadata_json, projection_version, created_at, updated_at
         from runtime_graph_edge
         where project_id = $1 and projection_version = $2
         order by relation_type asc, created_at asc",
    )
    .bind(project_id)
    .bind(projection_version)
    .fetch_all(pool)
    .await
}

/// Creates a runtime graph evidence link.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the evidence record.
pub async fn create_runtime_graph_evidence(
    pool: &PgPool,
    project_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
    document_id: Option<Uuid>,
    revision_id: Option<Uuid>,
    activated_by_attempt_id: Option<Uuid>,
    chunk_id: Option<Uuid>,
    source_file_name: Option<&str>,
    page_ref: Option<&str>,
    evidence_text: &str,
    confidence_score: Option<f64>,
    evidence_context_key: &str,
) -> Result<RuntimeGraphEvidenceRow, sqlx::Error> {
    let evidence_identity_key = runtime_graph_evidence_identity_key(
        target_kind,
        target_id,
        document_id,
        revision_id,
        activated_by_attempt_id,
        chunk_id,
        page_ref,
        source_file_name,
        evidence_context_key,
    );
    sqlx::query_as::<_, RuntimeGraphEvidenceRow>(
        "insert into runtime_graph_evidence (
            id, project_id, evidence_identity_key, target_kind, target_id, document_id, revision_id, activated_by_attempt_id,
            chunk_id, source_file_name, page_ref, evidence_text, confidence_score
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
         on conflict (project_id, evidence_identity_key) do update
         set document_id = excluded.document_id,
             revision_id = excluded.revision_id,
             activated_by_attempt_id = excluded.activated_by_attempt_id,
             chunk_id = excluded.chunk_id,
             source_file_name = excluded.source_file_name,
             page_ref = excluded.page_ref,
             evidence_text = excluded.evidence_text,
             confidence_score = excluded.confidence_score,
             is_active = true
         returning id, project_id, target_kind, target_id, document_id, chunk_id, source_file_name,
            page_ref, evidence_text, confidence_score, is_active, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(&evidence_identity_key)
    .bind(target_kind)
    .bind(target_id)
    .bind(document_id)
    .bind(revision_id)
    .bind(activated_by_attempt_id)
    .bind(chunk_id)
    .bind(source_file_name)
    .bind(page_ref)
    .bind(evidence_text)
    .bind(confidence_score)
    .fetch_one(pool)
    .await
}

/// Recalculates support counts for a targeted set of graph nodes.
///
/// # Errors
/// Returns any `SQLx` error raised while updating support counts.
pub const RECALCULATE_RUNTIME_GRAPH_NODE_SUPPORT_COUNTS_BY_IDS_SQL: &str = "with target_nodes as (
            select id, support_count
            from runtime_graph_node
            where project_id = $1
              and projection_version = $2
              and id = any($3)
         ),
         evidence_counts as (
            select evidence.target_id, count(*)::int as support_count
            from runtime_graph_evidence as evidence
            where evidence.project_id = $1
              and evidence.target_kind = 'node'
              and evidence.is_active = true
              and evidence.target_id = any($3)
            group by evidence.target_id
         ),
         desired_counts as (
            select target_nodes.id,
                   coalesce(evidence_counts.support_count, 0) as support_count
            from target_nodes
            left join evidence_counts on evidence_counts.target_id = target_nodes.id
         )
         update runtime_graph_node as node
         set support_count = desired_counts.support_count,
             updated_at = now()
         from desired_counts
         where node.id = desired_counts.id
           and node.support_count is distinct from desired_counts.support_count";

pub async fn recalculate_runtime_graph_node_support_counts_by_ids(
    pool: &PgPool,
    project_id: Uuid,
    projection_version: i64,
    node_ids: &[Uuid],
) -> Result<u64, sqlx::Error> {
    if node_ids.is_empty() {
        return Ok(0);
    }

    let result = sqlx::query(RECALCULATE_RUNTIME_GRAPH_NODE_SUPPORT_COUNTS_BY_IDS_SQL)
        .bind(project_id)
        .bind(projection_version)
        .bind(node_ids)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// Recalculates support counts for a targeted set of graph edges.
///
/// # Errors
/// Returns any `SQLx` error raised while updating support counts.
pub const RECALCULATE_RUNTIME_GRAPH_EDGE_SUPPORT_COUNTS_BY_IDS_SQL: &str = "with target_edges as (
            select id, support_count
            from runtime_graph_edge
            where project_id = $1
              and projection_version = $2
              and id = any($3)
         ),
         evidence_counts as (
            select evidence.target_id, count(*)::int as support_count
            from runtime_graph_evidence as evidence
            where evidence.project_id = $1
              and evidence.target_kind = 'edge'
              and evidence.is_active = true
              and evidence.target_id = any($3)
            group by evidence.target_id
         ),
         desired_counts as (
            select target_edges.id,
                   coalesce(evidence_counts.support_count, 0) as support_count
            from target_edges
            left join evidence_counts on evidence_counts.target_id = target_edges.id
         )
         update runtime_graph_edge as edge
         set support_count = desired_counts.support_count,
             updated_at = now()
         from desired_counts
         where edge.id = desired_counts.id
           and edge.support_count is distinct from desired_counts.support_count";

pub async fn recalculate_runtime_graph_edge_support_counts_by_ids(
    pool: &PgPool,
    project_id: Uuid,
    projection_version: i64,
    edge_ids: &[Uuid],
) -> Result<u64, sqlx::Error> {
    if edge_ids.is_empty() {
        return Ok(0);
    }

    let result = sqlx::query(RECALCULATE_RUNTIME_GRAPH_EDGE_SUPPORT_COUNTS_BY_IDS_SQL)
        .bind(project_id)
        .bind(projection_version)
        .bind(edge_ids)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// Lists runtime graph evidence for one target.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the evidence rows.
pub async fn list_runtime_graph_evidence_by_target(
    pool: &PgPool,
    project_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
) -> Result<Vec<RuntimeGraphEvidenceRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEvidenceRow>(
        "select id, project_id, target_kind, target_id, document_id, chunk_id, source_file_name,
            page_ref, evidence_text, confidence_score, is_active, created_at
         from runtime_graph_evidence
         where project_id = $1 and target_kind = $2 and target_id = $3 and is_active = true
         order by created_at desc",
    )
    .bind(project_id)
    .bind(target_kind)
    .bind(target_id)
    .fetch_all(pool)
    .await
}

/// Lists active runtime graph evidence lifecycle rows for one target.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the evidence rows.
pub async fn list_active_runtime_graph_evidence_lifecycle_by_target(
    pool: &PgPool,
    project_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
) -> Result<Vec<RuntimeGraphEvidenceLifecycleRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEvidenceLifecycleRow>(
        "select id, project_id, target_kind, target_id, document_id, revision_id,
            activated_by_attempt_id, deactivated_by_mutation_id, chunk_id, source_file_name,
            page_ref, evidence_text, confidence_score, is_active, created_at
         from runtime_graph_evidence
         where project_id = $1
           and target_kind = $2
           and target_id = $3
           and is_active = true
         order by created_at desc",
    )
    .bind(project_id)
    .bind(target_kind)
    .bind(target_id)
    .fetch_all(pool)
    .await
}

/// Lists document-to-runtime-graph links for the active projection.
///
/// # Errors
/// Returns any `SQLx` error raised while querying document link rows.
pub async fn list_runtime_graph_document_links_by_projection(
    pool: &PgPool,
    project_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphDocumentLinkRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphDocumentLinkRow>(
        "with active_node_links as (
            select
                evidence.document_id,
                evidence.target_id as target_node_id,
                'entity'::text as target_node_type,
                'supports'::text as relation_type,
                count(*)::bigint as support_count
            from runtime_graph_evidence as evidence
            inner join runtime_graph_node as node
                on node.project_id = evidence.project_id
               and node.id = evidence.target_id
               and node.projection_version = $2
            where evidence.project_id = $1
              and evidence.target_kind = 'node'
              and evidence.is_active = true
              and evidence.document_id is not null
            group by evidence.document_id, evidence.target_id
        ),
        active_edge_links as (
            select
                evidence.document_id,
                evidence.target_id as target_node_id,
                'topic'::text as target_node_type,
                'supports'::text as relation_type,
                count(*)::bigint as support_count
            from runtime_graph_evidence as evidence
            inner join runtime_graph_edge as edge
                on edge.project_id = evidence.project_id
               and edge.id = evidence.target_id
               and edge.projection_version = $2
            where evidence.project_id = $1
              and evidence.target_kind = 'edge'
              and evidence.is_active = true
              and evidence.document_id is not null
            group by evidence.document_id, evidence.target_id
        )
        select document_id, target_node_id, target_node_type, relation_type, support_count
        from (
            select * from active_node_links
            union all
            select * from active_edge_links
        ) as links
        order by support_count desc, document_id asc, target_node_id asc",
    )
    .bind(project_id)
    .bind(projection_version)
    .fetch_all(pool)
    .await
}

/// Marks all active graph evidence for one document as inactive.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the evidence rows.
pub async fn deactivate_runtime_graph_evidence_by_document(
    pool: &PgPool,
    project_id: Uuid,
    document_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "update runtime_graph_evidence
         set is_active = false
         where project_id = $1 and document_id = $2 and is_active = true",
    )
    .bind(project_id)
    .bind(document_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Lists active graph evidence rows for one logical document revision.
///
/// # Errors
/// Returns any `SQLx` error raised while querying revision-scoped evidence rows.
pub async fn list_active_runtime_graph_evidence_by_document_revision(
    pool: &PgPool,
    project_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
) -> Result<Vec<RuntimeGraphEvidenceLifecycleRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEvidenceLifecycleRow>(
        "select id, project_id, target_kind, target_id, document_id, revision_id,
            activated_by_attempt_id, deactivated_by_mutation_id, chunk_id, source_file_name,
            page_ref, evidence_text, confidence_score, is_active, created_at
         from runtime_graph_evidence
         where project_id = $1
           and document_id = $2
           and is_active = true
           and (revision_id = $3 or revision_id is null)
         order by created_at desc",
    )
    .bind(project_id)
    .bind(document_id)
    .bind(revision_id)
    .fetch_all(pool)
    .await
}

/// Lists target ids that still have active evidence outside one document revision.
///
/// # Errors
/// Returns any `SQLx` error raised while querying surviving evidence lineage.
pub async fn list_active_runtime_graph_target_ids_excluding_document_revision(
    pool: &PgPool,
    project_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    target_kind: &str,
    target_ids: &[Uuid],
) -> Result<Vec<Uuid>, sqlx::Error> {
    if target_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_scalar::<_, Uuid>(
        "select distinct target_id
         from runtime_graph_evidence
         where project_id = $1
           and target_kind = $4
           and target_id = any($5)
           and is_active = true
           and not (
                document_id = $2
                and (revision_id = $3 or revision_id is null)
           )
         order by target_id asc",
    )
    .bind(project_id)
    .bind(document_id)
    .bind(revision_id)
    .bind(target_kind)
    .bind(target_ids)
    .fetch_all(pool)
    .await
}

/// Marks active graph evidence for one logical document revision as inactive.
///
/// # Errors
/// Returns any `SQLx` error raised while updating revision-scoped evidence rows.
pub async fn deactivate_runtime_graph_evidence_by_document_revision(
    pool: &PgPool,
    project_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    mutation_id: Option<Uuid>,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "update runtime_graph_evidence
         set is_active = false,
             deactivated_by_mutation_id = coalesce($4, deactivated_by_mutation_id)
         where project_id = $1
           and document_id = $2
           and is_active = true
           and (revision_id = $3 or revision_id is null)",
    )
    .bind(project_id)
    .bind(document_id)
    .bind(revision_id)
    .bind(mutation_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Recalculates graph node/edge support counters from surviving active evidence.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the canonical graph rows.
pub const RECALCULATE_RUNTIME_GRAPH_NODE_SUPPORT_COUNTS_SQL: &str = "with target_nodes as (
            select id, support_count
            from runtime_graph_node
            where project_id = $1
              and projection_version = $2
         ),
         evidence_counts as (
            select evidence.target_id, count(*)::int as support_count
            from runtime_graph_evidence as evidence
            where evidence.project_id = $1
              and evidence.target_kind = 'node'
              and evidence.is_active = true
            group by evidence.target_id
         ),
         desired_counts as (
            select target_nodes.id,
                   coalesce(evidence_counts.support_count, 0) as support_count
            from target_nodes
            left join evidence_counts on evidence_counts.target_id = target_nodes.id
         )
         update runtime_graph_node as node
         set support_count = desired_counts.support_count,
             updated_at = now()
         from desired_counts
         where node.id = desired_counts.id
           and node.support_count is distinct from desired_counts.support_count";

pub const RECALCULATE_RUNTIME_GRAPH_EDGE_SUPPORT_COUNTS_SQL: &str = "with target_edges as (
            select id, support_count
            from runtime_graph_edge
            where project_id = $1
              and projection_version = $2
         ),
         evidence_counts as (
            select evidence.target_id, count(*)::int as support_count
            from runtime_graph_evidence as evidence
            where evidence.project_id = $1
              and evidence.target_kind = 'edge'
              and evidence.is_active = true
            group by evidence.target_id
         ),
         desired_counts as (
            select target_edges.id,
                   coalesce(evidence_counts.support_count, 0) as support_count
            from target_edges
            left join evidence_counts on evidence_counts.target_id = target_edges.id
         )
         update runtime_graph_edge as edge
         set support_count = desired_counts.support_count,
             updated_at = now()
         from desired_counts
         where edge.id = desired_counts.id
           and edge.support_count is distinct from desired_counts.support_count";

pub async fn recalculate_runtime_graph_support_counts(
    pool: &PgPool,
    project_id: Uuid,
    projection_version: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(RECALCULATE_RUNTIME_GRAPH_NODE_SUPPORT_COUNTS_SQL)
        .bind(project_id)
        .bind(projection_version)
        .execute(pool)
        .await?;

    sqlx::query(RECALCULATE_RUNTIME_GRAPH_EDGE_SUPPORT_COUNTS_SQL)
        .bind(project_id)
        .bind(projection_version)
        .execute(pool)
        .await?;

    Ok(())
}

/// Deletes canonical graph edges with zero surviving active evidence.
///
/// # Errors
/// Returns any `SQLx` error raised while pruning unsupported graph edges.
pub async fn delete_runtime_graph_edges_without_support(
    pool: &PgPool,
    project_id: Uuid,
    projection_version: i64,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "delete from runtime_graph_edge
         where project_id = $1
           and projection_version = $2
           and support_count <= 0",
    )
    .bind(project_id)
    .bind(projection_version)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Deletes canonical graph nodes with zero surviving active evidence.
///
/// # Errors
/// Returns any `SQLx` error raised while pruning unsupported graph nodes.
pub async fn delete_runtime_graph_nodes_without_support(
    pool: &PgPool,
    project_id: Uuid,
    projection_version: i64,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "delete from runtime_graph_node
         where project_id = $1
           and projection_version = $2
           and support_count <= 0",
    )
    .bind(project_id)
    .bind(projection_version)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Counts graph contributions that are still linked to one document.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the evidence rows.
pub async fn count_runtime_graph_contributions_by_document(
    pool: &PgPool,
    project_id: Uuid,
    document_id: Uuid,
) -> Result<RuntimeGraphContributionCountsRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphContributionCountsRow>(
        "select
            count(distinct case when target_kind = 'node' then target_id end) as node_count,
            count(distinct case when target_kind = 'edge' then target_id end) as edge_count,
            count(*) as evidence_count
         from runtime_graph_evidence
         where project_id = $1 and document_id = $2 and is_active = true",
    )
    .bind(project_id)
    .bind(document_id)
    .fetch_one(pool)
    .await
}

/// Counts active graph contributions linked to one logical document revision.
///
/// # Errors
/// Returns any `SQLx` error raised while querying revision-scoped evidence counts.
pub async fn count_runtime_graph_contributions_by_document_revision(
    pool: &PgPool,
    project_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
) -> Result<RuntimeGraphContributionCountsRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphContributionCountsRow>(
        "select
            count(distinct case when target_kind = 'node' then target_id end) as node_count,
            count(distinct case when target_kind = 'edge' then target_id end) as edge_count,
            count(*) as evidence_count
         from runtime_graph_evidence
         where project_id = $1
           and document_id = $2
           and is_active = true
           and (revision_id = $3 or revision_id is null)",
    )
    .bind(project_id)
    .bind(document_id)
    .bind(revision_id)
    .fetch_one(pool)
    .await
}

/// Counts admitted canonical graph nodes and relationships for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the canonical graph counts.
pub async fn count_admitted_runtime_graph_projection(
    pool: &PgPool,
    project_id: Uuid,
    projection_version: i64,
) -> Result<RuntimeGraphProjectionCountsRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphProjectionCountsRow>(&admitted_runtime_graph_counts_query())
        .bind(project_id)
        .bind(projection_version)
        .fetch_one(pool)
        .await
}

fn admitted_runtime_graph_nodes_query(extra_filter: &str) -> String {
    format!(
        "with admitted_edges as (
            select edge.from_node_id, edge.to_node_id
            from runtime_graph_edge as edge
            where edge.project_id = $1
              and edge.projection_version = $2
              and btrim(edge.relation_type) <> ''
              and edge.from_node_id <> edge.to_node_id
         ),
         admitted_edge_endpoints as (
            select admitted_edges.from_node_id as node_id
            from admitted_edges
            union
            select admitted_edges.to_node_id as node_id
            from admitted_edges
         )
         select node.id, node.project_id, node.canonical_key, node.label, node.node_type,
            node.aliases_json, node.summary, node.metadata_json, node.support_count,
            node.projection_version, node.created_at, node.updated_at
         from runtime_graph_node as node
         left join admitted_edge_endpoints as admitted on admitted.node_id = node.id
         where node.project_id = $1
           and node.projection_version = $2
           {extra_filter}
           and (
                node.node_type = 'document'
                or admitted.node_id is not null
           )
         order by node.node_type asc, node.label asc, node.created_at asc"
    )
}

fn admitted_runtime_graph_counts_query() -> String {
    "with admitted_edges as (
        select edge.id, edge.from_node_id, edge.to_node_id
        from runtime_graph_edge as edge
        where edge.project_id = $1
          and edge.projection_version = $2
          and btrim(edge.relation_type) <> ''
          and edge.from_node_id <> edge.to_node_id
     ),
     admitted_edge_endpoints as (
        select admitted_edges.from_node_id as node_id
        from admitted_edges
        union
        select admitted_edges.to_node_id as node_id
        from admitted_edges
     )
     select
        (
            select count(*)
            from runtime_graph_node as node
            left join admitted_edge_endpoints as admitted on admitted.node_id = node.id
            where node.project_id = $1
              and node.projection_version = $2
              and (
                    node.node_type = 'document'
                    or admitted.node_id is not null
              )
        ) as node_count,
        (
            select count(*)
            from admitted_edges
        ) as edge_count"
        .to_string()
}
