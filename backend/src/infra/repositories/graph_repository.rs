use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct GraphProjectionRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub source_attempt_id: Option<Uuid>,
    pub projection_state: String,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub superseded_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct GraphNodeRow {
    pub id: Uuid,
    pub projection_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub canonical_key: String,
    pub node_kind: String,
    pub display_label: String,
    pub summary: Option<String>,
    pub support_count: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct GraphEdgeRow {
    pub id: Uuid,
    pub projection_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub canonical_key: String,
    pub edge_kind: String,
    pub from_node_id: Uuid,
    pub to_node_id: Uuid,
    pub summary: Option<String>,
    pub support_count: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct GraphNodeEvidenceRow {
    pub node_id: Uuid,
    pub chunk_id: Uuid,
    pub revision_id: Uuid,
    pub attempt_id: Uuid,
    pub candidate_node_id: Option<Uuid>,
    pub evidence_weight: Decimal,
}

#[derive(Debug, Clone, FromRow)]
pub struct GraphEdgeEvidenceRow {
    pub edge_id: Uuid,
    pub chunk_id: Uuid,
    pub revision_id: Uuid,
    pub attempt_id: Uuid,
    pub candidate_edge_id: Option<Uuid>,
    pub evidence_weight: Decimal,
}

pub async fn create_graph_projection(
    pool: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    source_attempt_id: Option<Uuid>,
    projection_state: &str,
) -> Result<GraphProjectionRow, sqlx::Error> {
    sqlx::query_as::<_, GraphProjectionRow>(
        "insert into graph_projection (
            id,
            workspace_id,
            library_id,
            source_attempt_id,
            projection_state,
            started_at,
            completed_at,
            superseded_at
        )
        values ($1, $2, $3, $4, $5::graph_projection_state, now(), null, null)
        returning
            id,
            workspace_id,
            library_id,
            source_attempt_id,
            projection_state::text as projection_state,
            started_at,
            completed_at,
            superseded_at",
    )
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind(library_id)
    .bind(source_attempt_id)
    .bind(projection_state)
    .fetch_one(pool)
    .await
}

pub async fn get_graph_projection_by_id(
    pool: &PgPool,
    projection_id: Uuid,
) -> Result<Option<GraphProjectionRow>, sqlx::Error> {
    sqlx::query_as::<_, GraphProjectionRow>(
        "select
            id,
            workspace_id,
            library_id,
            source_attempt_id,
            projection_state::text as projection_state,
            started_at,
            completed_at,
            superseded_at
         from graph_projection
         where id = $1",
    )
    .bind(projection_id)
    .fetch_optional(pool)
    .await
}

pub async fn list_graph_projections_by_library(
    pool: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
) -> Result<Vec<GraphProjectionRow>, sqlx::Error> {
    sqlx::query_as::<_, GraphProjectionRow>(
        "select
            id,
            workspace_id,
            library_id,
            source_attempt_id,
            projection_state::text as projection_state,
            started_at,
            completed_at,
            superseded_at
         from graph_projection
         where workspace_id = $1 and library_id = $2
         order by started_at desc",
    )
    .bind(workspace_id)
    .bind(library_id)
    .fetch_all(pool)
    .await
}

pub async fn update_graph_projection_state(
    pool: &PgPool,
    projection_id: Uuid,
    projection_state: &str,
    completed_at: Option<DateTime<Utc>>,
    superseded_at: Option<DateTime<Utc>>,
) -> Result<Option<GraphProjectionRow>, sqlx::Error> {
    sqlx::query_as::<_, GraphProjectionRow>(
        "update graph_projection
         set projection_state = $2::graph_projection_state,
             completed_at = $3,
             superseded_at = $4
         where id = $1
         returning
            id,
            workspace_id,
            library_id,
            source_attempt_id,
            projection_state::text as projection_state,
            started_at,
            completed_at,
            superseded_at",
    )
    .bind(projection_id)
    .bind(projection_state)
    .bind(completed_at)
    .bind(superseded_at)
    .fetch_optional(pool)
    .await
}

pub async fn delete_graph_projection(
    pool: &PgPool,
    projection_id: Uuid,
) -> Result<Option<GraphProjectionRow>, sqlx::Error> {
    sqlx::query_as::<_, GraphProjectionRow>(
        "delete from graph_projection
         where id = $1
         returning
            id,
            workspace_id,
            library_id,
            source_attempt_id,
            projection_state::text as projection_state,
            started_at,
            completed_at,
            superseded_at",
    )
    .bind(projection_id)
    .fetch_optional(pool)
    .await
}

pub async fn create_graph_node(
    pool: &PgPool,
    projection_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    canonical_key: &str,
    node_kind: &str,
    display_label: &str,
    summary: Option<&str>,
    support_count: i32,
) -> Result<GraphNodeRow, sqlx::Error> {
    sqlx::query_as::<_, GraphNodeRow>(
        "insert into graph_node (
            id,
            projection_id,
            workspace_id,
            library_id,
            canonical_key,
            node_kind,
            display_label,
            summary,
            support_count,
            created_at,
            updated_at
        )
        values ($1, $2, $3, $4, $5, $6, $7, $8, $9, now(), now())
        returning
            id,
            projection_id,
            workspace_id,
            library_id,
            canonical_key,
            node_kind,
            display_label,
            summary,
            support_count,
            created_at,
            updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(projection_id)
    .bind(workspace_id)
    .bind(library_id)
    .bind(canonical_key)
    .bind(node_kind)
    .bind(display_label)
    .bind(summary)
    .bind(support_count)
    .fetch_one(pool)
    .await
}

pub async fn upsert_graph_node(
    pool: &PgPool,
    projection_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    canonical_key: &str,
    node_kind: &str,
    display_label: &str,
    summary: Option<&str>,
    support_count: i32,
) -> Result<GraphNodeRow, sqlx::Error> {
    sqlx::query_as::<_, GraphNodeRow>(
        "insert into graph_node (
            id,
            projection_id,
            workspace_id,
            library_id,
            canonical_key,
            node_kind,
            display_label,
            summary,
            support_count,
            created_at,
            updated_at
        )
        values ($1, $2, $3, $4, $5, $6, $7, $8, $9, now(), now())
        on conflict (projection_id, canonical_key)
        do update set
            workspace_id = excluded.workspace_id,
            library_id = excluded.library_id,
            node_kind = excluded.node_kind,
            display_label = excluded.display_label,
            summary = excluded.summary,
            support_count = excluded.support_count,
            updated_at = now()
        returning
            id,
            projection_id,
            workspace_id,
            library_id,
            canonical_key,
            node_kind,
            display_label,
            summary,
            support_count,
            created_at,
            updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(projection_id)
    .bind(workspace_id)
    .bind(library_id)
    .bind(canonical_key)
    .bind(node_kind)
    .bind(display_label)
    .bind(summary)
    .bind(support_count)
    .fetch_one(pool)
    .await
}

pub async fn get_graph_node_by_id(
    pool: &PgPool,
    node_id: Uuid,
) -> Result<Option<GraphNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, GraphNodeRow>(
        "select
            id,
            projection_id,
            workspace_id,
            library_id,
            canonical_key,
            node_kind,
            display_label,
            summary,
            support_count,
            created_at,
            updated_at
         from graph_node
         where id = $1",
    )
    .bind(node_id)
    .fetch_optional(pool)
    .await
}

pub async fn get_graph_node_by_key(
    pool: &PgPool,
    projection_id: Uuid,
    canonical_key: &str,
) -> Result<Option<GraphNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, GraphNodeRow>(
        "select
            id,
            projection_id,
            workspace_id,
            library_id,
            canonical_key,
            node_kind,
            display_label,
            summary,
            support_count,
            created_at,
            updated_at
         from graph_node
         where projection_id = $1 and canonical_key = $2",
    )
    .bind(projection_id)
    .bind(canonical_key)
    .fetch_optional(pool)
    .await
}

pub async fn list_graph_nodes_by_projection(
    pool: &PgPool,
    projection_id: Uuid,
) -> Result<Vec<GraphNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, GraphNodeRow>(
        "select
            id,
            projection_id,
            workspace_id,
            library_id,
            canonical_key,
            node_kind,
            display_label,
            summary,
            support_count,
            created_at,
            updated_at
         from graph_node
         where projection_id = $1
         order by node_kind asc, display_label asc, created_at asc",
    )
    .bind(projection_id)
    .fetch_all(pool)
    .await
}

pub async fn list_graph_nodes_by_projection_and_kind(
    pool: &PgPool,
    projection_id: Uuid,
    node_kind: &str,
) -> Result<Vec<GraphNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, GraphNodeRow>(
        "select
            id,
            projection_id,
            workspace_id,
            library_id,
            canonical_key,
            node_kind,
            display_label,
            summary,
            support_count,
            created_at,
            updated_at
         from graph_node
         where projection_id = $1 and node_kind = $2
         order by display_label asc, created_at asc",
    )
    .bind(projection_id)
    .bind(node_kind)
    .fetch_all(pool)
    .await
}

pub async fn update_graph_node(
    pool: &PgPool,
    node_id: Uuid,
    node_kind: &str,
    display_label: &str,
    summary: Option<&str>,
    support_count: i32,
) -> Result<Option<GraphNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, GraphNodeRow>(
        "update graph_node
         set node_kind = $2,
             display_label = $3,
             summary = $4,
             support_count = $5,
             updated_at = now()
         where id = $1
         returning
            id,
            projection_id,
            workspace_id,
            library_id,
            canonical_key,
            node_kind,
            display_label,
            summary,
            support_count,
            created_at,
            updated_at",
    )
    .bind(node_id)
    .bind(node_kind)
    .bind(display_label)
    .bind(summary)
    .bind(support_count)
    .fetch_optional(pool)
    .await
}

pub async fn delete_graph_node(
    pool: &PgPool,
    node_id: Uuid,
) -> Result<Option<GraphNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, GraphNodeRow>(
        "delete from graph_node
         where id = $1
         returning
            id,
            projection_id,
            workspace_id,
            library_id,
            canonical_key,
            node_kind,
            display_label,
            summary,
            support_count,
            created_at,
            updated_at",
    )
    .bind(node_id)
    .fetch_optional(pool)
    .await
}

pub async fn create_graph_edge(
    pool: &PgPool,
    projection_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    canonical_key: &str,
    edge_kind: &str,
    from_node_id: Uuid,
    to_node_id: Uuid,
    summary: Option<&str>,
    support_count: i32,
) -> Result<GraphEdgeRow, sqlx::Error> {
    sqlx::query_as::<_, GraphEdgeRow>(
        "insert into graph_edge (
            id,
            projection_id,
            workspace_id,
            library_id,
            canonical_key,
            edge_kind,
            from_node_id,
            to_node_id,
            summary,
            support_count,
            created_at,
            updated_at
        )
        values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, now(), now())
        returning
            id,
            projection_id,
            workspace_id,
            library_id,
            canonical_key,
            edge_kind,
            from_node_id,
            to_node_id,
            summary,
            support_count,
            created_at,
            updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(projection_id)
    .bind(workspace_id)
    .bind(library_id)
    .bind(canonical_key)
    .bind(edge_kind)
    .bind(from_node_id)
    .bind(to_node_id)
    .bind(summary)
    .bind(support_count)
    .fetch_one(pool)
    .await
}

pub async fn upsert_graph_edge(
    pool: &PgPool,
    projection_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    canonical_key: &str,
    edge_kind: &str,
    from_node_id: Uuid,
    to_node_id: Uuid,
    summary: Option<&str>,
    support_count: i32,
) -> Result<GraphEdgeRow, sqlx::Error> {
    sqlx::query_as::<_, GraphEdgeRow>(
        "insert into graph_edge (
            id,
            projection_id,
            workspace_id,
            library_id,
            canonical_key,
            edge_kind,
            from_node_id,
            to_node_id,
            summary,
            support_count,
            created_at,
            updated_at
        )
        values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, now(), now())
        on conflict (projection_id, canonical_key)
        do update set
            workspace_id = excluded.workspace_id,
            library_id = excluded.library_id,
            edge_kind = excluded.edge_kind,
            from_node_id = excluded.from_node_id,
            to_node_id = excluded.to_node_id,
            summary = excluded.summary,
            support_count = excluded.support_count,
            updated_at = now()
        returning
            id,
            projection_id,
            workspace_id,
            library_id,
            canonical_key,
            edge_kind,
            from_node_id,
            to_node_id,
            summary,
            support_count,
            created_at,
            updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(projection_id)
    .bind(workspace_id)
    .bind(library_id)
    .bind(canonical_key)
    .bind(edge_kind)
    .bind(from_node_id)
    .bind(to_node_id)
    .bind(summary)
    .bind(support_count)
    .fetch_one(pool)
    .await
}

pub async fn get_graph_edge_by_id(
    pool: &PgPool,
    edge_id: Uuid,
) -> Result<Option<GraphEdgeRow>, sqlx::Error> {
    sqlx::query_as::<_, GraphEdgeRow>(
        "select
            id,
            projection_id,
            workspace_id,
            library_id,
            canonical_key,
            edge_kind,
            from_node_id,
            to_node_id,
            summary,
            support_count,
            created_at,
            updated_at
         from graph_edge
         where id = $1",
    )
    .bind(edge_id)
    .fetch_optional(pool)
    .await
}

pub async fn get_graph_edge_by_key(
    pool: &PgPool,
    projection_id: Uuid,
    canonical_key: &str,
) -> Result<Option<GraphEdgeRow>, sqlx::Error> {
    sqlx::query_as::<_, GraphEdgeRow>(
        "select
            id,
            projection_id,
            workspace_id,
            library_id,
            canonical_key,
            edge_kind,
            from_node_id,
            to_node_id,
            summary,
            support_count,
            created_at,
            updated_at
         from graph_edge
         where projection_id = $1 and canonical_key = $2",
    )
    .bind(projection_id)
    .bind(canonical_key)
    .fetch_optional(pool)
    .await
}

pub async fn list_graph_edges_by_projection(
    pool: &PgPool,
    projection_id: Uuid,
) -> Result<Vec<GraphEdgeRow>, sqlx::Error> {
    sqlx::query_as::<_, GraphEdgeRow>(
        "select
            id,
            projection_id,
            workspace_id,
            library_id,
            canonical_key,
            edge_kind,
            from_node_id,
            to_node_id,
            summary,
            support_count,
            created_at,
            updated_at
         from graph_edge
         where projection_id = $1
         order by edge_kind asc, created_at asc",
    )
    .bind(projection_id)
    .fetch_all(pool)
    .await
}

pub async fn list_graph_edges_by_projection_and_kind(
    pool: &PgPool,
    projection_id: Uuid,
    edge_kind: &str,
) -> Result<Vec<GraphEdgeRow>, sqlx::Error> {
    sqlx::query_as::<_, GraphEdgeRow>(
        "select
            id,
            projection_id,
            workspace_id,
            library_id,
            canonical_key,
            edge_kind,
            from_node_id,
            to_node_id,
            summary,
            support_count,
            created_at,
            updated_at
         from graph_edge
         where projection_id = $1 and edge_kind = $2
         order by created_at asc",
    )
    .bind(projection_id)
    .bind(edge_kind)
    .fetch_all(pool)
    .await
}

pub async fn update_graph_edge(
    pool: &PgPool,
    edge_id: Uuid,
    edge_kind: &str,
    from_node_id: Uuid,
    to_node_id: Uuid,
    summary: Option<&str>,
    support_count: i32,
) -> Result<Option<GraphEdgeRow>, sqlx::Error> {
    sqlx::query_as::<_, GraphEdgeRow>(
        "update graph_edge
         set edge_kind = $2,
             from_node_id = $3,
             to_node_id = $4,
             summary = $5,
             support_count = $6,
             updated_at = now()
         where id = $1
         returning
            id,
            projection_id,
            workspace_id,
            library_id,
            canonical_key,
            edge_kind,
            from_node_id,
            to_node_id,
            summary,
            support_count,
            created_at,
            updated_at",
    )
    .bind(edge_id)
    .bind(edge_kind)
    .bind(from_node_id)
    .bind(to_node_id)
    .bind(summary)
    .bind(support_count)
    .fetch_optional(pool)
    .await
}

pub async fn delete_graph_edge(
    pool: &PgPool,
    edge_id: Uuid,
) -> Result<Option<GraphEdgeRow>, sqlx::Error> {
    sqlx::query_as::<_, GraphEdgeRow>(
        "delete from graph_edge
         where id = $1
         returning
            id,
            projection_id,
            workspace_id,
            library_id,
            canonical_key,
            edge_kind,
            from_node_id,
            to_node_id,
            summary,
            support_count,
            created_at,
            updated_at",
    )
    .bind(edge_id)
    .fetch_optional(pool)
    .await
}

pub async fn upsert_graph_node_evidence(
    pool: &PgPool,
    node_id: Uuid,
    chunk_id: Uuid,
    revision_id: Uuid,
    attempt_id: Uuid,
    candidate_node_id: Option<Uuid>,
    evidence_weight: Decimal,
) -> Result<GraphNodeEvidenceRow, sqlx::Error> {
    sqlx::query_as::<_, GraphNodeEvidenceRow>(
        "insert into graph_node_evidence (
            node_id,
            chunk_id,
            revision_id,
            attempt_id,
            candidate_node_id,
            evidence_weight
        )
        values ($1, $2, $3, $4, $5, $6)
        on conflict (node_id, chunk_id, attempt_id)
        do update set
            revision_id = excluded.revision_id,
            candidate_node_id = excluded.candidate_node_id,
            evidence_weight = excluded.evidence_weight
        returning
            node_id,
            chunk_id,
            revision_id,
            attempt_id,
            candidate_node_id,
            evidence_weight",
    )
    .bind(node_id)
    .bind(chunk_id)
    .bind(revision_id)
    .bind(attempt_id)
    .bind(candidate_node_id)
    .bind(evidence_weight)
    .fetch_one(pool)
    .await
}

pub async fn get_graph_node_evidence(
    pool: &PgPool,
    node_id: Uuid,
    chunk_id: Uuid,
    attempt_id: Uuid,
) -> Result<Option<GraphNodeEvidenceRow>, sqlx::Error> {
    sqlx::query_as::<_, GraphNodeEvidenceRow>(
        "select
            node_id,
            chunk_id,
            revision_id,
            attempt_id,
            candidate_node_id,
            evidence_weight
         from graph_node_evidence
         where node_id = $1 and chunk_id = $2 and attempt_id = $3",
    )
    .bind(node_id)
    .bind(chunk_id)
    .bind(attempt_id)
    .fetch_optional(pool)
    .await
}

pub async fn list_graph_node_evidence_by_node(
    pool: &PgPool,
    node_id: Uuid,
) -> Result<Vec<GraphNodeEvidenceRow>, sqlx::Error> {
    sqlx::query_as::<_, GraphNodeEvidenceRow>(
        "select
            node_id,
            chunk_id,
            revision_id,
            attempt_id,
            candidate_node_id,
            evidence_weight
         from graph_node_evidence
         where node_id = $1
         order by chunk_id asc, attempt_id asc",
    )
    .bind(node_id)
    .fetch_all(pool)
    .await
}

pub async fn list_graph_node_evidence_by_chunk(
    pool: &PgPool,
    chunk_id: Uuid,
) -> Result<Vec<GraphNodeEvidenceRow>, sqlx::Error> {
    sqlx::query_as::<_, GraphNodeEvidenceRow>(
        "select
            node_id,
            chunk_id,
            revision_id,
            attempt_id,
            candidate_node_id,
            evidence_weight
         from graph_node_evidence
         where chunk_id = $1
         order by node_id asc, attempt_id asc",
    )
    .bind(chunk_id)
    .fetch_all(pool)
    .await
}

pub async fn delete_graph_node_evidence(
    pool: &PgPool,
    node_id: Uuid,
    chunk_id: Uuid,
    attempt_id: Uuid,
) -> Result<Option<GraphNodeEvidenceRow>, sqlx::Error> {
    sqlx::query_as::<_, GraphNodeEvidenceRow>(
        "delete from graph_node_evidence
         where node_id = $1 and chunk_id = $2 and attempt_id = $3
         returning
            node_id,
            chunk_id,
            revision_id,
            attempt_id,
            candidate_node_id,
            evidence_weight",
    )
    .bind(node_id)
    .bind(chunk_id)
    .bind(attempt_id)
    .fetch_optional(pool)
    .await
}

pub async fn upsert_graph_edge_evidence(
    pool: &PgPool,
    edge_id: Uuid,
    chunk_id: Uuid,
    revision_id: Uuid,
    attempt_id: Uuid,
    candidate_edge_id: Option<Uuid>,
    evidence_weight: Decimal,
) -> Result<GraphEdgeEvidenceRow, sqlx::Error> {
    sqlx::query_as::<_, GraphEdgeEvidenceRow>(
        "insert into graph_edge_evidence (
            edge_id,
            chunk_id,
            revision_id,
            attempt_id,
            candidate_edge_id,
            evidence_weight
        )
        values ($1, $2, $3, $4, $5, $6)
        on conflict (edge_id, chunk_id, attempt_id)
        do update set
            revision_id = excluded.revision_id,
            candidate_edge_id = excluded.candidate_edge_id,
            evidence_weight = excluded.evidence_weight
        returning
            edge_id,
            chunk_id,
            revision_id,
            attempt_id,
            candidate_edge_id,
            evidence_weight",
    )
    .bind(edge_id)
    .bind(chunk_id)
    .bind(revision_id)
    .bind(attempt_id)
    .bind(candidate_edge_id)
    .bind(evidence_weight)
    .fetch_one(pool)
    .await
}

pub async fn get_graph_edge_evidence(
    pool: &PgPool,
    edge_id: Uuid,
    chunk_id: Uuid,
    attempt_id: Uuid,
) -> Result<Option<GraphEdgeEvidenceRow>, sqlx::Error> {
    sqlx::query_as::<_, GraphEdgeEvidenceRow>(
        "select
            edge_id,
            chunk_id,
            revision_id,
            attempt_id,
            candidate_edge_id,
            evidence_weight
         from graph_edge_evidence
         where edge_id = $1 and chunk_id = $2 and attempt_id = $3",
    )
    .bind(edge_id)
    .bind(chunk_id)
    .bind(attempt_id)
    .fetch_optional(pool)
    .await
}

pub async fn list_graph_edge_evidence_by_edge(
    pool: &PgPool,
    edge_id: Uuid,
) -> Result<Vec<GraphEdgeEvidenceRow>, sqlx::Error> {
    sqlx::query_as::<_, GraphEdgeEvidenceRow>(
        "select
            edge_id,
            chunk_id,
            revision_id,
            attempt_id,
            candidate_edge_id,
            evidence_weight
         from graph_edge_evidence
         where edge_id = $1
         order by chunk_id asc, attempt_id asc",
    )
    .bind(edge_id)
    .fetch_all(pool)
    .await
}

pub async fn list_graph_edge_evidence_by_chunk(
    pool: &PgPool,
    chunk_id: Uuid,
) -> Result<Vec<GraphEdgeEvidenceRow>, sqlx::Error> {
    sqlx::query_as::<_, GraphEdgeEvidenceRow>(
        "select
            edge_id,
            chunk_id,
            revision_id,
            attempt_id,
            candidate_edge_id,
            evidence_weight
         from graph_edge_evidence
         where chunk_id = $1
         order by edge_id asc, attempt_id asc",
    )
    .bind(chunk_id)
    .fetch_all(pool)
    .await
}

pub async fn delete_graph_edge_evidence(
    pool: &PgPool,
    edge_id: Uuid,
    chunk_id: Uuid,
    attempt_id: Uuid,
) -> Result<Option<GraphEdgeEvidenceRow>, sqlx::Error> {
    sqlx::query_as::<_, GraphEdgeEvidenceRow>(
        "delete from graph_edge_evidence
         where edge_id = $1 and chunk_id = $2 and attempt_id = $3
         returning
            edge_id,
            chunk_id,
            revision_id,
            attempt_id,
            candidate_edge_id,
            evidence_weight",
    )
    .bind(edge_id)
    .bind(chunk_id)
    .bind(attempt_id)
    .fetch_optional(pool)
    .await
}
