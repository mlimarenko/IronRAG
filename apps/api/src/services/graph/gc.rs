#![allow(clippy::missing_errors_doc, clippy::too_many_lines)]

use std::sync::Arc;

use anyhow::Context as AnyhowContext;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::{
        arangodb::{
            client::ArangoClient,
            collections::{
                KNOWLEDGE_BUNDLE_ENTITY_EDGE, KNOWLEDGE_BUNDLE_RELATION_EDGE,
                KNOWLEDGE_CHUNK_COLLECTION, KNOWLEDGE_CHUNK_MENTIONS_ENTITY_EDGE,
                KNOWLEDGE_DOCUMENT_COLLECTION, KNOWLEDGE_ENTITY_COLLECTION,
                KNOWLEDGE_EVIDENCE_COLLECTION, KNOWLEDGE_EVIDENCE_SUPPORTS_ENTITY_EDGE,
                KNOWLEDGE_EVIDENCE_SUPPORTS_RELATION_EDGE, KNOWLEDGE_RELATION_COLLECTION,
                KNOWLEDGE_RELATION_OBJECT_EDGE, KNOWLEDGE_RELATION_SUBJECT_EDGE,
                KNOWLEDGE_REVISION_COLLECTION,
            },
        },
        repositories,
    },
};

#[derive(Clone)]
pub struct Context {
    arango_client: Arc<ArangoClient>,
    postgres: PgPool,
    knowledge_plane_backend: String,
}

impl Context {
    #[must_use]
    pub fn new(arango_client: Arc<ArangoClient>, postgres: PgPool) -> Self {
        Self::with_backend(arango_client, postgres, "arango")
    }

    #[must_use]
    pub fn with_backend(
        arango_client: Arc<ArangoClient>,
        postgres: PgPool,
        knowledge_plane_backend: impl Into<String>,
    ) -> Self {
        Self { arango_client, postgres, knowledge_plane_backend: knowledge_plane_backend.into() }
    }

    #[must_use]
    pub fn from_state(state: &AppState) -> Self {
        Self::with_backend(
            Arc::clone(&state.arango_client),
            state.persistence.postgres.clone(),
            state.settings.knowledge_plane_backend.clone(),
        )
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GcReport {
    pub entities_deleted: u32,
    pub relations_deleted: u32,
    #[serde(default)]
    pub evidence_deleted: u32,
    pub libraries_scanned: u32,
}

impl GcReport {
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        Self {
            entities_deleted: self.entities_deleted.saturating_add(other.entities_deleted),
            relations_deleted: self.relations_deleted.saturating_add(other.relations_deleted),
            evidence_deleted: self.evidence_deleted.saturating_add(other.evidence_deleted),
            libraries_scanned: self.libraries_scanned.saturating_add(other.libraries_scanned),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GraphGcError {
    #[error("library {library_id} has {active_jobs} active ingest jobs")]
    ActiveIngest { library_id: Uuid, active_jobs: i64 },
    #[error("failed to inspect active ingest jobs for library {library_id}: {source}")]
    ActiveIngestLookup { library_id: Uuid, source: sqlx::Error },
    #[error("failed to acquire graph GC lock for library {library_id}: {source}")]
    LockAcquire { library_id: Uuid, source: sqlx::Error },
    #[error("failed to release graph GC lock for library {library_id}: {source}")]
    LockRelease { library_id: Uuid, source: sqlx::Error },
    #[error("failed to execute graph GC AQL for library {library_id}: {source}")]
    Arango { library_id: Uuid, source: anyhow::Error },
    #[error("postgres error during graph GC for library {library_id}: {source}")]
    Postgres { library_id: Uuid, source: sqlx::Error },
    #[error("failed to decode graph GC report for library {library_id}: {source}")]
    Decode { library_id: Uuid, source: anyhow::Error },
    #[error("graph GC returned no report row for library {library_id}")]
    MissingReport { library_id: Uuid },
    #[error("unsupported knowledge plane backend `{backend}` for graph GC")]
    UnsupportedBackend { backend: String },
}

pub(crate) const ZOMBIE_NODE_GC_AQL: &str = r#"
WITH knowledge_entity, knowledge_relation, knowledge_evidence, knowledge_chunk, knowledge_revision, knowledge_document
LET zombieEntityIds = (
  FOR entity IN @@entity_collection
    FILTER entity.library_id == @library_id
    LET aliveEvidence = (
      FOR entityEvidence IN @@entity_evidence_edge_collection
        FILTER entityEvidence.library_id == @library_id
          AND entityEvidence._to == entity._id
        FOR evidence IN @@evidence_collection
          FILTER evidence.library_id == @library_id
            AND evidence._id == entityEvidence._from
            AND evidence.evidence_state == "active"
            AND evidence.chunk_id != null
        FOR chunk IN @@chunk_collection
          FILTER chunk.library_id == @library_id
            AND chunk.chunk_id == evidence.chunk_id
            AND chunk.revision_id == evidence.revision_id
            AND chunk.chunk_state == "ready"
        FOR revision IN @@revision_collection
          FILTER revision.library_id == @library_id
            AND revision.revision_id == evidence.revision_id
            AND revision.revision_id == chunk.revision_id
            AND revision.revision_state == "active"
            AND revision.superseded_by_revision_id == null
        FOR document IN @@document_collection
          FILTER document.library_id == @library_id
            AND document.document_id == revision.document_id
            AND document.document_state == "active"
            AND document.deleted_at == null
            AND document.active_revision_id == revision.revision_id
        LIMIT 1
        RETURN evidence._id
    )
    FILTER LENGTH(aliveEvidence) == 0
    RETURN entity._id
)
LET zombieRelationIds = UNIQUE(FLATTEN([
  (
    FOR edge IN @@relation_subject_edge_collection
      FILTER edge.library_id == @library_id
        AND edge._to IN zombieEntityIds
      RETURN edge._from
  ),
  (
    FOR edge IN @@relation_object_edge_collection
      FILTER edge.library_id == @library_id
        AND edge._to IN zombieEntityIds
      RETURN edge._from
  )
]))
LET deletedChunkMentionsEntityEdges = (
  FOR edge IN @@chunk_mentions_entity_edge_collection
    FILTER edge.library_id == @library_id
      AND edge._to IN zombieEntityIds
    REMOVE edge IN @@chunk_mentions_entity_edge_collection
    RETURN OLD._key
)
LET deletedEvidenceSupportsEntityEdges = (
  FOR edge IN @@entity_evidence_edge_collection
    FILTER edge.library_id == @library_id
      AND edge._to IN zombieEntityIds
    REMOVE edge IN @@entity_evidence_edge_collection
    RETURN OLD._key
)
LET deletedRelationSubjectEdges = (
  FOR edge IN @@relation_subject_edge_collection
    FILTER edge.library_id == @library_id
      AND (edge._to IN zombieEntityIds OR edge._from IN zombieRelationIds)
    REMOVE edge IN @@relation_subject_edge_collection
    RETURN OLD._key
)
LET deletedRelationObjectEdges = (
  FOR edge IN @@relation_object_edge_collection
    FILTER edge.library_id == @library_id
      AND (edge._to IN zombieEntityIds OR edge._from IN zombieRelationIds)
    REMOVE edge IN @@relation_object_edge_collection
    RETURN OLD._key
)
LET deletedEvidenceSupportsRelationEdges = (
  FOR edge IN @@relation_evidence_edge_collection
    FILTER edge.library_id == @library_id
      AND edge._to IN zombieRelationIds
    REMOVE edge IN @@relation_evidence_edge_collection
    RETURN OLD._key
)
LET deletedBundleEntityEdges = (
  FOR edge IN @@bundle_entity_edge_collection
    FILTER edge.library_id == @library_id
      AND edge._to IN zombieEntityIds
    REMOVE edge IN @@bundle_entity_edge_collection
    RETURN OLD._key
)
LET deletedBundleRelationEdges = (
  FOR edge IN @@bundle_relation_edge_collection
    FILTER edge.library_id == @library_id
      AND edge._to IN zombieRelationIds
    REMOVE edge IN @@bundle_relation_edge_collection
    RETURN OLD._key
)
LET deletedRelations = (
  FOR relation IN @@relation_collection
    FILTER relation.library_id == @library_id
      AND relation._id IN zombieRelationIds
    REMOVE relation IN @@relation_collection
    RETURN OLD._key
)
LET deletedEntities = (
  FOR entity IN @@entity_collection
    FILTER entity.library_id == @library_id
      AND entity._id IN zombieEntityIds
    REMOVE entity IN @@entity_collection
    RETURN OLD._key
)
RETURN {
  entitiesDeleted: LENGTH(deletedEntities),
  relationsDeleted: LENGTH(deletedRelations),
  librariesScanned: 1
}
"#;

/// Deletes graph entities that no longer have evidence tied to a current document revision.
///
/// # Errors
/// Returns [`GraphGcError`] when the library has active ingest work, the graph lock cannot be
/// acquired or released, or ArangoDB fails to execute/decode the cleanup query.
pub async fn gc_zombie_nodes(library_id: Uuid, ctx: &Context) -> Result<GcReport, GraphGcError> {
    let graph_lock = repositories::acquire_runtime_library_graph_lock(&ctx.postgres, library_id)
        .await
        .map_err(|source| GraphGcError::LockAcquire { library_id, source })?;

    let result = async {
        ensure_library_has_no_active_ingest_jobs(library_id, &ctx.postgres).await?;
        match ctx.knowledge_plane_backend.as_str() {
            "arango" => run_gc_aql(library_id, &ctx.arango_client).await,
            "postgres" => run_gc_postgres(library_id, &ctx.postgres).await,
            backend => Err(GraphGcError::UnsupportedBackend { backend: backend.to_string() }),
        }
    }
    .await;

    let release_result = repositories::release_runtime_library_graph_lock(graph_lock, library_id)
        .await
        .map_err(|source| GraphGcError::LockRelease { library_id, source });

    match (result, release_result) {
        (Ok(report), Ok(())) => Ok(report),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(release_error)) => {
            tracing::error!(
                %library_id,
                ?error,
                ?release_error,
                "graph GC failed and advisory lock release also failed"
            );
            Err(error)
        }
    }
}

async fn ensure_library_has_no_active_ingest_jobs(
    library_id: Uuid,
    postgres: &PgPool,
) -> Result<(), GraphGcError> {
    let active_jobs = sqlx::query_scalar::<_, i64>(
        "select count(*)
         from ingest_job
         where library_id = $1
           and queue_state in ('queued', 'leased')
           and completed_at is null",
    )
    .bind(library_id)
    .fetch_one(postgres)
    .await
    .map_err(|source| GraphGcError::ActiveIngestLookup { library_id, source })?;

    if active_jobs > 0 {
        return Err(GraphGcError::ActiveIngest { library_id, active_jobs });
    }
    Ok(())
}

async fn run_gc_aql(
    library_id: Uuid,
    arango_client: &ArangoClient,
) -> Result<GcReport, GraphGcError> {
    let cursor = arango_client
        .query_json(ZOMBIE_NODE_GC_AQL, gc_bind_vars(library_id))
        .await
        .map_err(|source| GraphGcError::Arango { library_id, source })?;
    let mut reports = decode_many_results::<GcReport>(cursor)
        .map_err(|source| GraphGcError::Decode { library_id, source })?;
    reports.pop().ok_or(GraphGcError::MissingReport { library_id })
}

pub(crate) async fn run_gc_postgres(
    library_id: Uuid,
    postgres: &PgPool,
) -> Result<GcReport, GraphGcError> {
    let mut tx =
        postgres.begin().await.map_err(|source| GraphGcError::Postgres { library_id, source })?;
    let report = run_gc_postgres_transaction(library_id, &mut tx).await?;
    tx.commit().await.map_err(|source| GraphGcError::Postgres { library_id, source })?;
    Ok(report)
}

pub(crate) async fn run_gc_postgres_transaction(
    library_id: Uuid,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<GcReport, GraphGcError> {
    let orphan_evidence_before = delete_orphan_runtime_graph_evidence(library_id, tx).await?;
    let (edge_evidence_deleted, edges_deleted) =
        delete_zombie_runtime_graph_edges(library_id, tx).await?;
    let (node_evidence_deleted, nodes_deleted) =
        delete_zombie_runtime_graph_nodes(library_id, tx).await?;
    let orphan_evidence_after = delete_orphan_runtime_graph_evidence(library_id, tx).await?;

    Ok(GcReport {
        entities_deleted: count_to_u32(nodes_deleted),
        relations_deleted: count_to_u32(edges_deleted),
        evidence_deleted: count_to_u32(
            orphan_evidence_before
                + edge_evidence_deleted
                + node_evidence_deleted
                + orphan_evidence_after,
        ),
        libraries_scanned: 1,
    })
}

async fn delete_orphan_runtime_graph_evidence(
    library_id: Uuid,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<i64, GraphGcError> {
    sqlx::query_scalar::<_, i64>(
        "with deleted as ( \
             delete from runtime_graph_evidence evidence \
             where evidence.library_id = $1 \
               and ( \
                   (evidence.target_kind = 'node' and not exists ( \
                       select 1 from runtime_graph_node node \
                       where node.library_id = evidence.library_id \
                         and node.id = evidence.target_id \
                   )) \
                   or \
                   (evidence.target_kind = 'edge' and not exists ( \
                       select 1 from runtime_graph_edge edge \
                       where edge.library_id = evidence.library_id \
                         and edge.id = evidence.target_id \
                   )) \
               ) \
             returning 1 \
         ) \
         select count(*)::bigint from deleted",
    )
    .bind(library_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|source| GraphGcError::Postgres { library_id, source })
}

async fn delete_zombie_runtime_graph_edges(
    library_id: Uuid,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<(i64, i64), GraphGcError> {
    sqlx::query_as::<_, (i64, i64)>(
        "with zombie_nodes as ( \
             select node.id \
             from runtime_graph_node node \
             where node.library_id = $1 \
               and not exists ( \
                   select 1 from runtime_graph_evidence evidence \
                   where evidence.library_id = node.library_id \
                     and evidence.target_kind = 'node' \
                     and evidence.target_id = node.id \
               ) \
         ), \
         zombie_edges as ( \
             select edge.id \
             from runtime_graph_edge edge \
             where edge.library_id = $1 \
               and ( \
                   not exists ( \
                       select 1 from runtime_graph_evidence evidence \
                       where evidence.library_id = edge.library_id \
                         and evidence.target_kind = 'edge' \
                         and evidence.target_id = edge.id \
                   ) \
                   or exists (select 1 from zombie_nodes node where node.id = edge.from_node_id) \
                   or exists (select 1 from zombie_nodes node where node.id = edge.to_node_id) \
               ) \
         ), \
         deleted_edge_evidence as ( \
             delete from runtime_graph_evidence evidence \
             using zombie_edges \
             where evidence.library_id = $1 \
               and evidence.target_kind = 'edge' \
               and evidence.target_id = zombie_edges.id \
             returning 1 \
         ), \
         deleted_edges as ( \
             delete from runtime_graph_edge edge \
             using zombie_edges \
             where edge.library_id = $1 \
               and edge.id = zombie_edges.id \
             returning 1 \
         ) \
         select \
             (select count(*)::bigint from deleted_edge_evidence), \
             (select count(*)::bigint from deleted_edges)",
    )
    .bind(library_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|source| GraphGcError::Postgres { library_id, source })
}

async fn delete_zombie_runtime_graph_nodes(
    library_id: Uuid,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<(i64, i64), GraphGcError> {
    sqlx::query_as::<_, (i64, i64)>(
        "with zombie_nodes as ( \
             select node.id \
             from runtime_graph_node node \
             where node.library_id = $1 \
               and not exists ( \
                   select 1 from runtime_graph_evidence evidence \
                   where evidence.library_id = node.library_id \
                     and evidence.target_kind = 'node' \
                     and evidence.target_id = node.id \
               ) \
         ), \
         deleted_node_evidence as ( \
             delete from runtime_graph_evidence evidence \
             using zombie_nodes \
             where evidence.library_id = $1 \
               and evidence.target_kind = 'node' \
               and evidence.target_id = zombie_nodes.id \
             returning 1 \
         ), \
         deleted_nodes as ( \
             delete from runtime_graph_node node \
             using zombie_nodes \
             where node.library_id = $1 \
               and node.id = zombie_nodes.id \
             returning 1 \
         ) \
         select \
             (select count(*)::bigint from deleted_node_evidence), \
             (select count(*)::bigint from deleted_nodes)",
    )
    .bind(library_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|source| GraphGcError::Postgres { library_id, source })
}

fn count_to_u32(count: i64) -> u32 {
    u32::try_from(count.max(0)).unwrap_or(u32::MAX)
}

fn gc_bind_vars(library_id: Uuid) -> serde_json::Value {
    serde_json::json!({
        "library_id": library_id,
        "@entity_collection": KNOWLEDGE_ENTITY_COLLECTION,
        "@relation_collection": KNOWLEDGE_RELATION_COLLECTION,
        "@evidence_collection": KNOWLEDGE_EVIDENCE_COLLECTION,
        "@chunk_collection": KNOWLEDGE_CHUNK_COLLECTION,
        "@revision_collection": KNOWLEDGE_REVISION_COLLECTION,
        "@document_collection": KNOWLEDGE_DOCUMENT_COLLECTION,
        "@chunk_mentions_entity_edge_collection": KNOWLEDGE_CHUNK_MENTIONS_ENTITY_EDGE,
        "@relation_subject_edge_collection": KNOWLEDGE_RELATION_SUBJECT_EDGE,
        "@relation_object_edge_collection": KNOWLEDGE_RELATION_OBJECT_EDGE,
        "@entity_evidence_edge_collection": KNOWLEDGE_EVIDENCE_SUPPORTS_ENTITY_EDGE,
        "@relation_evidence_edge_collection": KNOWLEDGE_EVIDENCE_SUPPORTS_RELATION_EDGE,
        "@bundle_entity_edge_collection": KNOWLEDGE_BUNDLE_ENTITY_EDGE,
        "@bundle_relation_edge_collection": KNOWLEDGE_BUNDLE_RELATION_EDGE,
    })
}

fn decode_many_results<T>(cursor: serde_json::Value) -> anyhow::Result<Vec<T>>
where
    T: DeserializeOwned,
{
    let result = cursor
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("ArangoDB cursor response is missing result"))?;
    serde_json::from_value(result).context("failed to decode ArangoDB result rows")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone)]
    struct SyntheticEvidence {
        target_id: Uuid,
        revision_id: Uuid,
        chunk_id: Option<Uuid>,
        evidence_state: &'static str,
    }

    #[derive(Debug, Clone)]
    struct SyntheticChunk {
        chunk_id: Uuid,
        revision_id: Uuid,
        chunk_state: &'static str,
    }

    #[derive(Debug, Clone)]
    struct SyntheticRevision {
        revision_id: Uuid,
        document_id: Uuid,
        revision_state: &'static str,
        superseded_by_revision_id: Option<Uuid>,
    }

    #[derive(Debug, Clone)]
    struct SyntheticDocument {
        document_id: Uuid,
        active_revision_id: Option<Uuid>,
        document_state: &'static str,
        deleted: bool,
    }

    fn synthetic_has_alive_evidence(
        target_id: Uuid,
        evidence_rows: &[SyntheticEvidence],
        chunk_rows: &[SyntheticChunk],
        revision_rows: &[SyntheticRevision],
        document_rows: &[SyntheticDocument],
    ) -> bool {
        evidence_rows
            .iter()
            .filter(|evidence| {
                evidence.target_id == target_id
                    && evidence.evidence_state == "active"
                    && evidence.chunk_id.is_some()
            })
            .any(|evidence| {
                let Some(chunk_id) = evidence.chunk_id else {
                    return false;
                };
                let Some(chunk) = chunk_rows.iter().find(|chunk| {
                    chunk.chunk_id == chunk_id
                        && chunk.revision_id == evidence.revision_id
                        && chunk.chunk_state == "ready"
                }) else {
                    return false;
                };
                let Some(revision) = revision_rows.iter().find(|revision| {
                    revision.revision_id == evidence.revision_id
                        && revision.revision_id == chunk.revision_id
                        && revision.revision_state == "active"
                        && revision.superseded_by_revision_id.is_none()
                }) else {
                    return false;
                };
                document_rows.iter().any(|document| {
                    document.document_id == revision.document_id
                        && document.document_state == "active"
                        && !document.deleted
                        && document.active_revision_id == Some(revision.revision_id)
                })
            })
    }

    #[test]
    fn alive_predicate_requires_current_active_chunk_revision_and_document() {
        let entity_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let chunk_id = Uuid::now_v7();
        let evidence = [SyntheticEvidence {
            target_id: entity_id,
            revision_id,
            chunk_id: Some(chunk_id),
            evidence_state: "active",
        }];
        let chunks = [SyntheticChunk { chunk_id, revision_id, chunk_state: "ready" }];
        let revisions = [SyntheticRevision {
            revision_id,
            document_id,
            revision_state: "active",
            superseded_by_revision_id: None,
        }];
        let documents = [SyntheticDocument {
            document_id,
            active_revision_id: Some(revision_id),
            document_state: "active",
            deleted: false,
        }];

        assert!(synthetic_has_alive_evidence(
            entity_id, &evidence, &chunks, &revisions, &documents
        ));
    }

    #[test]
    fn alive_predicate_rejects_stale_or_deleted_sources() {
        let entity_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let chunk_id = Uuid::now_v7();
        let evidence = [SyntheticEvidence {
            target_id: entity_id,
            revision_id,
            chunk_id: Some(chunk_id),
            evidence_state: "active",
        }];
        let chunks = [SyntheticChunk { chunk_id, revision_id, chunk_state: "ready" }];
        let revisions = [SyntheticRevision {
            revision_id,
            document_id,
            revision_state: "active",
            superseded_by_revision_id: None,
        }];

        for document in [
            SyntheticDocument {
                document_id,
                active_revision_id: Some(Uuid::now_v7()),
                document_state: "active",
                deleted: false,
            },
            SyntheticDocument {
                document_id,
                active_revision_id: Some(revision_id),
                document_state: "deleted",
                deleted: true,
            },
        ] {
            assert!(!synthetic_has_alive_evidence(
                entity_id,
                &evidence,
                &chunks,
                &revisions,
                &[document],
            ));
        }
    }

    #[test]
    fn alive_predicate_rejects_missing_chunk_or_superseded_evidence() {
        let entity_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let chunk_id = Uuid::now_v7();
        let chunks = [SyntheticChunk { chunk_id, revision_id, chunk_state: "ready" }];
        let revisions = [SyntheticRevision {
            revision_id,
            document_id,
            revision_state: "active",
            superseded_by_revision_id: None,
        }];
        let documents = [SyntheticDocument {
            document_id,
            active_revision_id: Some(revision_id),
            document_state: "active",
            deleted: false,
        }];

        for evidence in [
            SyntheticEvidence {
                target_id: entity_id,
                revision_id,
                chunk_id: None,
                evidence_state: "active",
            },
            SyntheticEvidence {
                target_id: entity_id,
                revision_id,
                chunk_id: Some(chunk_id),
                evidence_state: "superseded",
            },
        ] {
            assert!(!synthetic_has_alive_evidence(
                entity_id,
                &[evidence],
                &chunks,
                &revisions,
                &documents,
            ));
        }
    }

    #[test]
    fn aql_predicate_uses_the_canonical_liveness_join() {
        for required in [
            "FOR entity IN @@entity_collection",
            "FOR entityEvidence IN @@entity_evidence_edge_collection",
            "FOR evidence IN @@evidence_collection",
            "FOR chunk IN @@chunk_collection",
            "FOR revision IN @@revision_collection",
            "FOR document IN @@document_collection",
            "evidence.chunk_id != null",
            "chunk.chunk_state == \"ready\"",
            "document.active_revision_id == revision.revision_id",
        ] {
            assert!(ZOMBIE_NODE_GC_AQL.contains(required), "GC AQL must include {required}");
        }
    }

    #[test]
    fn aql_removes_endpoint_relations_and_dangling_graph_edges() {
        for required in [
            "@@relation_subject_edge_collection",
            "@@relation_object_edge_collection",
            "@@entity_evidence_edge_collection",
            "@@relation_evidence_edge_collection",
            "@@chunk_mentions_entity_edge_collection",
            "@@bundle_entity_edge_collection",
            "@@bundle_relation_edge_collection",
            "REMOVE relation IN @@relation_collection",
            "REMOVE entity IN @@entity_collection",
        ] {
            assert!(ZOMBIE_NODE_GC_AQL.contains(required), "GC AQL must include {required}");
        }
    }
}
