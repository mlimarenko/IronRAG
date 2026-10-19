use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::LazyLock;

use anyhow::Context;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{Executor, FromRow, PgConnection, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::domains::query_ir::literal_text_is_identifier_shaped;
use crate::domains::retrieval::DEFAULT_TEXT_SEARCH_CONFIG;
use crate::infra::postgres::pg_vector_config::{
    PgVectorStorage, pg_hnsw_index_params, read_env_u64,
};
use crate::infra::{
    knowledge_plane::{
        ActiveEmbeddingProfileSource, ChunkLexicalQuery, ChunkVectorProfileInventory,
        IngestAttemptMutationOutcome, IngestVectorWriteFence, LexicalQueryOrigin, SearchStore,
        StagedVectorRebuildPromotion, VECTOR_PLANE_DATA_ADVISORY_LOCK_PREFIX,
        VECTOR_REBUILD_STAGING_PROFILE_DIGEST_LEN, VECTOR_REBUILD_STAGING_PROFILE_PREFIX,
        VectorPlaneDeleteOutcome, VectorSimilaritySearchInput, VectorWriteFence,
    },
    knowledge_rows::{
        KNOWLEDGE_CHUNK_VECTOR_KIND, KNOWLEDGE_ENTITY_VECTOR_KIND, KnowledgeChunkSearchRow,
        KnowledgeChunkVectorRow, KnowledgeChunkVectorSearchRow, KnowledgeEntitySearchRow,
        KnowledgeEntityVectorRow, KnowledgeEntityVectorSearchRow, KnowledgeRelationSearchRow,
        KnowledgeStructuredBlockSearchRow, KnowledgeTechnicalFactSearchRow,
    },
    repositories::ingest_repository,
};

// Entity/relation text and vectors aggregate evidence. Both lanes require
// positive current support and reject any retained non-current contribution.
macro_rules! current_knowledge_graph_target {
    ($support:literal, $target:literal, $library:literal) => {
        concat!(
            "exists (select 1 from ",
            $support,
            " support
             join knowledge_evidence evidence on evidence.evidence_id = support.from_id
             join content_current_revision current_revision
               on current_revision.document_id = evidence.document_id
              and current_revision.revision_id = evidence.revision_id
              and current_revision.library_id = evidence.library_id
             where support.to_id = ",
            $target,
            " and evidence.library_id = ",
            $library,
            ")
             and not exists (select 1 from ",
            $support,
            " support
             left join knowledge_evidence evidence on evidence.evidence_id = support.from_id
             left join content_current_revision current_revision
               on current_revision.document_id = evidence.document_id
              and current_revision.revision_id = evidence.revision_id
              and current_revision.library_id = ",
            $library,
            "
             where support.to_id = ",
            $target,
            " and current_revision.revision_id is null)"
        )
    };
}

/// Late membership check for normalized graph search hits already hydrated in
/// memory. This uses the same support predicate as lexical and vector lanes.
pub(crate) async fn knowledge_graph_targets_are_current(
    pool: &PgPool,
    library_id: Uuid,
    entity_ids: &[Uuid],
    relation_ids: &[Uuid],
) -> Result<bool, sqlx::Error> {
    let entity_visibility = current_knowledge_graph_target!(
        "knowledge_evidence_entity_support",
        "e.entity_id",
        "e.library_id"
    );
    let relation_visibility = current_knowledge_graph_target!(
        "knowledge_evidence_relation_support",
        "r.relation_id",
        "r.library_id"
    );
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select not exists (
            select 1 from unnest($2::uuid[]) requested(id)
            left join knowledge_entity e on e.entity_id = requested.id and e.library_id = $1
            where e.entity_id is null or not ({entity_visibility})
            union all
            select 1 from unnest($3::uuid[]) requested(id)
            left join knowledge_relation r on r.relation_id = requested.id and r.library_id = $1
            where r.relation_id is null or not ({relation_visibility})
         )"
    )))
    .bind(library_id)
    .bind(entity_ids)
    .bind(relation_ids)
    .fetch_one(pool)
    .await
}

#[derive(Clone)]
pub struct PgSearchStore {
    pub pool: PgPool,
}

#[derive(Debug, Clone)]
struct PgVectorSearchLane {
    relation_name: String,
    manifest_row_count: u64,
}

#[derive(Debug, Clone, FromRow)]
struct PgChunkVectorRow {
    vector_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    chunk_id: Uuid,
    revision_id: Uuid,
    embedding_model_key: String,
    vector_kind: String,
    dimensions: i32,
    vector_text: String,
    freshness_generation: i64,
    created_at: DateTime<Utc>,
    occurred_at: Option<DateTime<Utc>>,
    occurred_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
struct PgEntityVectorRow {
    vector_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    entity_id: Uuid,
    embedding_model_key: String,
    vector_kind: String,
    dimensions: i32,
    vector_text: String,
    freshness_generation: i64,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
struct PgChunkSearchRow {
    chunk_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    revision_id: Uuid,
    content_text: String,
    normalized_text: String,
    section_path: Vec<String>,
    heading_trail: Vec<String>,
    score: f64,
    exact_identifier_rarity: f64,
    exact_identifier_terms: Vec<String>,
    rare_exact_identifier_terms: Vec<String>,
    quality_score: Option<f32>,
}

#[derive(Debug, Clone, FromRow)]
struct PgStructuredBlockSearchRow {
    block_id: Uuid,
    document_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    revision_id: Uuid,
    ordinal: i32,
    block_kind: String,
    text: String,
    normalized_text: String,
    section_path: Vec<String>,
    heading_trail: Vec<String>,
    score: f64,
}

#[derive(Debug, Clone, FromRow)]
struct PgTechnicalFactSearchRow {
    fact_id: Uuid,
    document_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    revision_id: Uuid,
    fact_kind: String,
    normalized_value_text: String,
    display_value: String,
    exact_match: bool,
    score: f64,
}

#[derive(Debug, Clone, FromRow)]
struct PgEntitySearchRow {
    entity_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    primary_label: String,
    entity_type: String,
    summary: Option<String>,
    score: f64,
}

#[derive(Debug, Clone, FromRow)]
struct PgRelationSearchRow {
    relation_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    predicate: String,
    normalized_assertion: String,
    summary: Option<String>,
    score: f64,
}

#[derive(Debug, Clone, FromRow)]
struct PgChunkVectorSearchRow {
    vector_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    chunk_id: Uuid,
    revision_id: Uuid,
    embedding_model_key: String,
    vector_kind: String,
    freshness_generation: i64,
    score: f64,
}

#[derive(Debug, Clone, FromRow)]
struct PgEntityVectorSearchRow {
    vector_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    entity_id: Uuid,
    embedding_model_key: String,
    vector_kind: String,
    freshness_generation: i64,
    score: f64,
}

mod lexical;
mod relations;
mod search_store;
mod vector_inventory;
mod vector_mutation;
mod vector_support;

use lexical::*;
pub(crate) use vector_inventory::read_vector_inventory;
pub(crate) use vector_mutation::*;
use vector_support::*;
pub(crate) use vector_support::{
    CHUNK_VECTOR_RELATION_PREFIX, ENTITY_VECTOR_RELATION_PREFIX,
    VECTOR_RELATION_DDL_ADVISORY_LOCK_KEY,
};

pub(crate) use vector_mutation::{
    SupersededChunkVectorSweepReport, advance_vector_source_truth_version,
    delete_ingest_revision_chunk_vectors, delete_projection_chunk_vectors,
    list_physical_vector_relations_in_transaction, lock_exclusive_vector_mutation_source,
    sweep_superseded_chunk_vectors, validate_ingest_revision_vector_coverage,
};

#[cfg(test)]
mod lexical_tests;
#[cfg(test)]
mod vector_mutation_tests;
#[cfg(test)]
mod vector_support_tests;
