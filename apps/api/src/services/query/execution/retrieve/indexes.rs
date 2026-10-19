use anyhow::Context;

use super::{
    AppState, DOCUMENT_IDENTITY_SCORE_FLOOR, DateTime, HashMap, HashSet, KnowledgeDocumentRow,
    KnowledgeLibraryGenerationRow, QueryGraphIndex, Utc, Uuid,
    load_active_runtime_graph_projection, record_stage_elapsed,
};

pub(super) const DIRECT_TABLE_AGGREGATION_SUMMARY_LIMIT: usize = 32;
pub(super) const DIRECT_TABLE_AGGREGATION_ROW_LIMIT: usize = 24;
pub(super) const DIRECT_TABLE_AGGREGATION_CHUNK_LIMIT: usize = 32;
pub(super) const TABLE_SECTION_SIBLING_LIMIT_PER_SECTION: usize = 32;
pub(super) const TABLE_SECTION_SIBLING_CHUNK_LIMIT: usize = 40;
pub(super) const DOCUMENT_EVIDENCE_ANCHOR_CANDIDATE_LIMIT: usize = 3;
pub(super) const DOCUMENT_EVIDENCE_ANCHOR_CHUNKS_PER_DOCUMENT: usize = 3;
pub(super) const DOCUMENT_EVIDENCE_CONTEXT_RESERVATION_LIMIT: usize = 2;
pub(super) const DOCUMENT_IDENTITY_CHUNKS_PER_DOCUMENT: usize = 3;
pub(super) const DOCUMENT_IDENTITY_FOCUSED_CHUNKS_PER_DOCUMENT: usize = 4;
pub(super) const EXACT_LITERAL_CONTEXT_RESERVATION_LIMIT: usize = 3;
pub(super) const DOCUMENT_IDENTITY_FOCUS_PREFIX_CHARS: usize = 6;
pub(super) const LINKED_ANCHOR_CONTEXT_QUERY_CAP: usize = 4;
pub(super) const LINKED_ANCHOR_CONTEXT_CHUNKS_PER_QUERY: usize = 4;
pub(super) const LINKED_ANCHOR_CONTEXT_PREFIX_CHARS: usize = 6;
pub(super) const LINKED_ANCHOR_CONTEXT_QUERY_PREFIX_CHARS: usize = 7;
pub(super) const GRAPH_EVIDENCE_CONTEXT_FETCH_MULTIPLIER: usize = 3;
pub(super) const GRAPH_EVIDENCE_CONTEXT_SCORE_TOKEN_MIN_CHARS: usize = 4;
pub(super) const GRAPH_EVIDENCE_CONTEXT_EVIDENCE_FIELD_WEIGHT: usize = 4;
pub(super) const GRAPH_EVIDENCE_CONTEXT_TARGET_FIELD_WEIGHT: usize = 2;
pub(super) const GRAPH_EVIDENCE_CONTEXT_SOURCE_FIELD_WEIGHT: usize = 1;
pub(super) const GRAPH_EVIDENCE_CONTEXT_LINE_CHARS: usize = 1600;
pub(super) const GRAPH_EVIDENCE_SOURCE_DOCUMENT_PRIORITY_CAP: usize = 12;
pub(super) const MAX_GRAPH_EVIDENCE_DB_TEXT_QUERIES: usize = 5;

pub(crate) async fn load_graph_index(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<QueryGraphIndex> {
    let started = std::time::Instant::now();
    let projection = load_active_runtime_graph_projection(state, library_id)
        .await
        .context("failed to load active runtime graph projection for query")?;
    let mut all_node_positions = HashMap::with_capacity(projection.nodes.len());
    for (position, node) in projection.nodes.iter().enumerate() {
        all_node_positions.insert(node.id, position);
    }

    let mut edge_positions = HashMap::with_capacity(projection.edges.len());
    let mut connected_node_ids = HashSet::with_capacity(projection.edges.len().saturating_mul(2));
    for (position, edge) in projection.edges.iter().enumerate() {
        let Some(&from_position) = all_node_positions.get(&edge.from_node_id) else {
            continue;
        };
        let Some(&to_position) = all_node_positions.get(&edge.to_node_id) else {
            continue;
        };
        let from_node_key = projection.nodes[from_position].identity_key.as_str();
        let to_node_key = projection.nodes[to_position].identity_key.as_str();
        if !state.bulk_ingest_hardening_services.graph_quality_guard.allows_relation(
            from_node_key,
            to_node_key,
            &edge.relation_type,
        ) {
            continue;
        }
        edge_positions.insert(edge.id, position);
        connected_node_ids.insert(edge.from_node_id);
        connected_node_ids.insert(edge.to_node_id);
    }
    let node_positions = projection
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(position, node)| {
            (node.node_type == "document" || connected_node_ids.contains(&node.id))
                .then_some((node.id, position))
        })
        .collect();

    let graph_index = QueryGraphIndex::new(projection, node_positions, edge_positions);
    record_stage_elapsed("retrieve.index_graph", started);
    Ok(graph_index)
}

pub(crate) async fn load_latest_library_generation(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<Option<KnowledgeLibraryGenerationRow>> {
    state
        .application_services
        .knowledge
        .derive_library_generation_rows(state, library_id)
        .await
        .map(|rows| rows.into_iter().next())
        .map_err(|error| {
            anyhow::anyhow!("failed to derive library generations for runtime query: {error}")
        })
}

pub(crate) fn query_graph_status(
    generation: Option<&KnowledgeLibraryGenerationRow>,
) -> &'static str {
    match generation {
        Some(row) if row.active_graph_generation > 0 && row.degraded_state == "ready" => "current",
        Some(row) if row.active_graph_generation > 0 => "partial",
        _ => "empty",
    }
}

const DOCUMENT_INDEX_SQL: &str = "select
        d.id as document_id,
        d.workspace_id,
        d.library_id,
        d.external_key,
        r.title,
        r.source_uri,
        r.document_hint,
        d.document_state::text as document_state,
        h.active_revision_id,
        h.readable_revision_id,
        r.revision_number::bigint as latest_revision_no,
        d.parent_document_id,
        d.document_role,
        d.created_at,
        h.head_updated_at as updated_at,
        d.deleted_at
     from content_document d
     join content_document_head h on h.document_id = d.id
     join knowledge_structured_revision sr
       on sr.revision_id = h.readable_revision_id
      and sr.document_id = d.id
      and sr.workspace_id = d.workspace_id
      and sr.library_id = d.library_id
      and sr.preparation_state = 'prepared'
     join content_revision r
       on r.id = h.readable_revision_id
      and r.document_id = d.id
      and r.workspace_id = d.workspace_id
      and r.library_id = d.library_id
     where d.library_id = $1
       and d.document_state = 'active'
       and d.deleted_at is null
     order by h.head_updated_at desc, d.id desc";

pub(crate) async fn load_document_index(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<HashMap<Uuid, KnowledgeDocumentRow>> {
    let started = std::time::Instant::now();
    let rows = sqlx::query_as::<_, QueryDocumentIndexRow>(DOCUMENT_INDEX_SQL)
        .bind(library_id)
        .fetch_all(&state.persistence.postgres)
        .await
        .context("failed to load runtime query document index from current content heads")?;
    let document_index = rows
        .into_iter()
        .map(query_document_index_row_to_knowledge_document_row)
        .map(|row| (row.document_id, row))
        .collect();
    record_stage_elapsed("retrieve.index_document", started);
    Ok(document_index)
}

#[cfg(test)]
mod document_index_sql_tests {
    use super::*;

    #[test]
    fn document_index_requires_exact_current_readable_prepared_revision() {
        for predicate in [
            "h.document_id = d.id",
            "sr.revision_id = h.readable_revision_id",
            "sr.document_id = d.id",
            "sr.workspace_id = d.workspace_id",
            "sr.library_id = d.library_id",
            "sr.preparation_state = 'prepared'",
            "r.id = h.readable_revision_id",
            "r.document_id = d.id",
            "r.workspace_id = d.workspace_id",
            "r.library_id = d.library_id",
            "d.document_state = 'active'",
            "d.deleted_at is null",
        ] {
            assert!(DOCUMENT_INDEX_SQL.contains(predicate), "missing index scope: {predicate}");
        }
        assert!(DOCUMENT_INDEX_SQL.contains("r.revision_number::bigint as latest_revision_no"));
        assert!(!DOCUMENT_INDEX_SQL.contains("coalesce(h.readable_revision_id"));
        assert!(!DOCUMENT_INDEX_SQL.contains("preparation_state = 'preparing'"));
        assert!(!DOCUMENT_INDEX_SQL.contains("left join content_document_head"));
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(super) struct QueryDocumentIndexRow {
    pub(super) document_id: Uuid,
    pub(super) workspace_id: Uuid,
    pub(super) library_id: Uuid,
    pub(super) external_key: String,
    pub(super) title: Option<String>,
    pub(super) source_uri: Option<String>,
    pub(super) document_hint: Option<String>,
    pub(super) document_state: String,
    pub(super) active_revision_id: Option<Uuid>,
    pub(super) readable_revision_id: Option<Uuid>,
    pub(super) latest_revision_no: Option<i64>,
    pub(super) parent_document_id: Option<Uuid>,
    pub(super) document_role: String,
    pub(super) created_at: DateTime<Utc>,
    pub(super) updated_at: DateTime<Utc>,
    pub(super) deleted_at: Option<DateTime<Utc>>,
}

pub(super) fn query_document_index_row_to_knowledge_document_row(
    row: QueryDocumentIndexRow,
) -> KnowledgeDocumentRow {
    let file_name = Some(row.external_key.clone());
    KnowledgeDocumentRow {
        document_id: row.document_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        external_key: row.external_key,
        file_name,
        title: row.title,
        source_uri: row.source_uri,
        document_hint: row.document_hint,
        document_state: row.document_state,
        active_revision_id: row.active_revision_id,
        readable_revision_id: row.readable_revision_id,
        latest_revision_no: row.latest_revision_no,
        parent_document_id: row.parent_document_id,
        document_role: row.document_role,
        created_at: row.created_at,
        updated_at: row.updated_at,
        deleted_at: row.deleted_at,
    }
}

/// Ceiling on entity-bio fan-out chunks so vector and lexical hits stay within
/// the context window for entities that appear across dozens of documents.
pub(super) const ENTITY_BIO_CHUNK_CAP: usize = 24;
pub(super) const GRAPH_EVIDENCE_CHUNK_CAP: usize = 24;
pub(super) const GRAPH_EVIDENCE_TARGET_CAP: usize = 48;
pub(super) const QUERY_IR_FOCUS_CHUNK_CAP: usize = 32;
pub(super) const QUERY_IR_FOCUS_SOURCE_CONTEXT_RESERVATION_LIMIT: usize = 8;
pub(super) const QUERY_IR_FOCUS_CHUNKS_PER_QUERY: usize = 12;
pub(super) const SETUP_FOCUS_DOCUMENT_CANDIDATE_CAP: usize = 128;
pub(super) const SETUP_FOCUS_DOCUMENT_FETCH_CONCURRENCY: usize = 8;
pub(super) const SETUP_FOCUS_DOCUMENT_SCAN_CHUNKS: i32 = 32;
pub(super) const SETUP_FOCUS_DOCUMENT_FORWARD_CHUNKS: i32 = 24;
pub(super) const SETUP_FOCUS_DOCUMENT_CHUNK_CAP: usize = 24;
pub(super) const SETUP_FOCUS_DOCUMENT_SCORE_BASE: f32 = DOCUMENT_IDENTITY_SCORE_FLOOR * 20.0;
pub(super) const SETUP_VARIANT_DOCUMENT_CANDIDATE_CAP: usize = 64;
pub(super) const SETUP_VARIANT_DOCUMENT_CAP: usize = 12;
pub(super) const SETUP_VARIANT_DOCUMENT_FETCH_CAP: usize = SETUP_VARIANT_DOCUMENT_CAP * 2;
pub(super) const SETUP_VARIANT_CHUNKS_PER_DOCUMENT: usize = 4;
pub(super) const SETUP_VARIANT_CHUNK_CAP: usize = 48;
pub(super) const SETUP_VARIANT_DOCUMENT_FETCH_CONCURRENCY: usize = 8;
pub(super) const LATEST_VERSION_SEMANTIC_DOCUMENT_CANDIDATE_CAP: usize = 128;
pub(super) const LATEST_VERSION_STRUCTURAL_PROBE_DOCUMENT_CAP: usize = 1024;
pub(super) const LATEST_VERSION_STRUCTURAL_PROBE_SCOPED_DOCUMENT_CAP: usize = 256;
pub(super) const LATEST_VERSION_SEMANTIC_CHUNK_SCAN_LIMIT: usize = 48;
pub(super) const LATEST_VERSION_WINDOW_FETCH_BATCH: usize = 32;
pub(super) const LATEST_VERSION_SEMANTIC_DEEP_DOCUMENT_CAP: usize = 1024;
pub(super) const LATEST_VERSION_SEMANTIC_DEEP_CHUNK_SCAN_LIMIT: i32 = 512;
pub(super) const LATEST_VERSION_SEMANTIC_CHUNK_CAP: usize = 40;
pub(super) const LATEST_VERSION_SEMANTIC_UNSCOPED_IDENTITY_DOCUMENT_CAP_MULTIPLIER: usize = 2;
pub(super) const LATEST_VERSION_SEMANTIC_UNSCOPED_ROWS_PER_DOCUMENT: usize = 10;
pub(super) const LATEST_VERSION_STRUCTURAL_MIN_DISTINCT_VERSIONS: usize = 2;
pub(super) const LATEST_VERSION_STRUCTURAL_DENSITY_DOCUMENT_CAP: usize = 96;
pub(super) const LATEST_VERSION_STRUCTURAL_DENSITY_MIN_ROWS: i64 = 2;
pub(super) const LATEST_VERSION_STRUCTURAL_DENSITY_REGEX: &str =
    r"(^|[^0-9.])[0-9]+\.[0-9]+(\.[0-9]+)?([^0-9.]|$)";
pub(super) const VERSIONED_UPDATE_PROCEDURE_DOCUMENT_CANDIDATE_CAP: usize = 12;
pub(super) const VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT: usize = 8;
pub(super) const VERSIONED_UPDATE_PROCEDURE_MATCH_PROBE_CHUNKS_PER_DOCUMENT: usize =
    VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT * 4;
pub(super) const VERSIONED_UPDATE_PROCEDURE_CONTEXT_BACKWARD_CHUNKS: i32 = 1;
pub(super) const VERSIONED_UPDATE_PROCEDURE_SCORE_PRIORITY_MARGIN: f32 = 32.0;
pub(super) const VERSIONED_UPDATE_PROCEDURE_SCORE_BASE: f32 = SETUP_FOCUS_DOCUMENT_SCORE_BASE
    + DOCUMENT_IDENTITY_SCORE_FLOOR
    + VERSIONED_UPDATE_PROCEDURE_SCORE_PRIORITY_MARGIN;
pub(super) const VERSIONED_UPDATE_PROCEDURE_CONTEXT_RESERVATION_LIMIT: usize =
    VERSIONED_UPDATE_PROCEDURE_DOCUMENT_CANDIDATE_CAP
        * VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT;
pub(super) const VERSIONED_UPDATE_PROCEDURE_DOCUMENT_EVIDENCE_RESERVATION_LIMIT: usize = 8;
pub(super) const VERSIONED_UPDATE_PROCEDURE_RESERVED_NEIGHBORS_PER_ANCHOR: usize = 2;
pub(super) const VERSIONED_UPDATE_PROCEDURE_DOCUMENT_FETCH_CONCURRENCY: usize = 4;
pub(super) const VERSIONED_UPDATE_PROCEDURE_EVIDENCE_SEARCH_QUERY_CAP: usize = 6;
pub(super) const VERSIONED_UPDATE_PROCEDURE_EVIDENCE_SEARCH_HIT_LIMIT: usize = 96;
pub(super) const VERSIONED_UPDATE_PROCEDURE_SUBJECT_TITLE_RESERVE_CAP: usize = 2;
pub(super) const VERSIONED_UPDATE_PROCEDURE_EVIDENCE_STRUCTURAL_SCORE_FLOOR: usize = 3;
pub(super) const VERSIONED_UPDATE_PROCEDURE_COMMAND_SIGNAL_SCORE_CAP: usize = 8;
pub(super) const SETUP_FOCUS_CONFIG_PATH_EXTENSIONS: [&str; 8] =
    [".conf", ".ini", ".cfg", ".properties", ".yaml", ".yml", ".toml", ".json"];
pub(super) const RAW_SETUP_FOCUS_CANDIDATE_TOKEN_MAX_DOC_FREQUENCY_FLOOR: usize = 8;
pub(super) const RAW_SETUP_FOCUS_CANDIDATE_TOKEN_MAX_DOC_FREQUENCY_CAP: usize = 512;
pub(super) const RAW_SETUP_FOCUS_CANDIDATE_TOKEN_DOCUMENT_DIVISOR: usize = 50;
pub(super) const RAW_SETUP_FOCUS_STRUCTURAL_DOCUMENT_SCORE_FLOOR: usize = 24;
pub(super) const ARTIFACT_SIBLING_SOURCE_DOCUMENT_CAP: usize = 3;
pub(super) const ARTIFACT_SIBLING_SOURCE_CHUNKS_PER_DOCUMENT: i32 = 4;
pub(super) const ARTIFACT_SIBLING_SOURCE_STRUCTURAL_CHUNKS_PER_DOCUMENT: usize = 8;
pub(super) const ARTIFACT_SIBLING_SOURCE_FOCUS_TERM_CAP: usize = 24;
pub(super) const ARTIFACT_SIBLING_SOURCE_SCORE_BASE: f32 = 1.1;
pub(super) const ARTIFACT_SIBLING_SOURCE_SCORE_STEP: f32 = 0.01;

pub(super) const ENTITY_BIO_CHUNK_SCORE_BASE: f32 = 1.0;
pub(super) const ENTITY_BIO_CHUNK_SCORE_STEP: f32 = 0.001;
pub(super) const GRAPH_EVIDENCE_CHUNK_SCORE_BASE: f32 = 1.25;
pub(super) const GRAPH_EVIDENCE_CHUNK_SCORE_STEP: f32 = 0.001;
pub(super) const QUERY_IR_FOCUS_CHUNK_SCORE_BASE: f32 = 1.5;
pub(super) const QUERY_IR_FOCUS_CHUNK_SCORE_STEP: f32 = 0.001;
pub(super) const CONTENT_ANCHOR_CHUNK_CAP: usize = 24;
pub(super) const CONTENT_ANCHOR_CHUNKS_PER_REVISION: usize = 2;
pub(super) const CONTENT_ANCHOR_SEARCH_TERM_CAP: usize = 24;
pub(super) const CONTENT_ANCHOR_SEARCH_PREFIX_MIN_CHARS: usize = 6;
pub(super) const CONTENT_ANCHOR_SEARCH_PREFIX_CHARS: usize = 5;
pub(super) const CONTENT_ANCHOR_SEQUENCE_CAP: usize = 32;
pub(super) const CONTENT_ANCHOR_TOKEN_MIN_CHARS: usize = 4;
pub(super) const CONTENT_ANCHOR_SEQUENCE_MIN_CHARS: usize = 3;
pub(super) const CONTENT_ANCHOR_MIN_TOKEN_OVERLAP: usize = 2;
pub(super) const CONTENT_ANCHOR_CONTEXT_RESERVATION_LIMIT: usize = 4;
pub(super) const CONTENT_ANCHOR_CHUNK_SCORE_BASE: f32 = 1.75;
pub(super) const CONTENT_ANCHOR_CHUNK_SCORE_STEP: f32 = 0.001;
pub(super) const CONTENT_ANCHOR_EVIDENCE_SCORE_STEP: f32 = 0.000_001;
pub(super) const GRAPH_EVIDENCE_TEXTS_PER_CHUNK: usize = 4;
pub(super) const GRAPH_EVIDENCE_CONTEXT_LINE_CAP: usize = 24;
