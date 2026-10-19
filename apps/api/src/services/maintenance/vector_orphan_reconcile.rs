//! `repair orphan-vector-rows` — remove per-dim vector-shard rows whose library
//! no longer exists, delete their orphaned manifest lanes, and re-attach the
//! canonical `on delete cascade` on every physical shard.
//!
//! A physical vector row can outlive its `catalog_library` row when it was
//! written before the shard had its cascade, or when an operator dropped the
//! foreign key. The per-dim ensure path
//! (`ensure_vector_relation_library_cascade`) then refuses to attach the
//! validated constraint and reports every orphaned lane, because deleting rows
//! implicitly would hide data loss. This module is the explicit escape hatch
//! the refusal names: it deletes the orphaned rows and the manifest lanes of
//! absent libraries, decrements `knowledge_vector_relation_manifest` for the
//! lanes that keep a live library, and re-runs the ensure so the cascade is
//! attached on every shard — including shards that carry no orphan — in the
//! same transaction. Idempotent.

use anyhow::Result;
use serde::Serialize;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::postgres::pg_vector_config::{self, OrphanedVectorLane, OrphanedVectorRelation},
};

#[derive(Debug, Clone, Serialize)]
pub struct OrphanedVectorLaneReport {
    pub library_id: Uuid,
    pub embedding_model_key: String,
    pub vector_kind: String,
    pub row_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrphanedVectorRelationReport {
    pub relation_name: String,
    pub lanes: Vec<OrphanedVectorLaneReport>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrphanedVectorRowReport {
    pub dry_run: bool,
    pub relations_with_orphans: usize,
    pub lanes: usize,
    pub rows: i64,
    pub details: Vec<OrphanedVectorRelationReport>,
}

impl From<OrphanedVectorLane> for OrphanedVectorLaneReport {
    fn from(lane: OrphanedVectorLane) -> Self {
        Self {
            library_id: lane.library_id,
            embedding_model_key: lane.embedding_model_key,
            vector_kind: lane.vector_kind,
            row_count: lane.row_count,
        }
    }
}

impl From<OrphanedVectorRelation> for OrphanedVectorRelationReport {
    fn from(relation: OrphanedVectorRelation) -> Self {
        Self {
            relation_name: relation.relation_name,
            lanes: relation.lanes.into_iter().map(OrphanedVectorLaneReport::from).collect(),
        }
    }
}

/// Reconcile orphaned vector-shard rows for every physical shard. `dry_run`
/// reports the lanes without deleting rows or changing DDL; a real run also
/// deletes orphaned manifest lanes and attaches the canonical cascade on every
/// shard, not only the ones that carried an orphan.
pub async fn orphaned_vector_rows(
    state: &AppState,
    dry_run: bool,
) -> Result<OrphanedVectorRowReport> {
    let relations =
        pg_vector_config::reconcile_orphaned_vector_rows(&state.persistence.postgres, dry_run)
            .await?;
    let details = relations.into_iter().map(OrphanedVectorRelationReport::from).collect::<Vec<_>>();
    let lanes = details.iter().map(|relation| relation.lanes.len()).sum();
    let rows =
        details.iter().flat_map(|relation| relation.lanes.iter().map(|lane| lane.row_count)).sum();
    Ok(OrphanedVectorRowReport {
        dry_run,
        relations_with_orphans: details.len(),
        lanes,
        rows,
        details,
    })
}
