use uuid::Uuid;

pub const KNOWLEDGE_DOCUMENT_COLLECTION: &str = "knowledge_document";
pub const KNOWLEDGE_REVISION_COLLECTION: &str = "knowledge_revision";
pub const KNOWLEDGE_STRUCTURED_REVISION_COLLECTION: &str = "knowledge_structured_revision";
pub const KNOWLEDGE_STRUCTURED_BLOCK_COLLECTION: &str = "knowledge_structured_block";
pub const KNOWLEDGE_CHUNK_COLLECTION: &str = "knowledge_chunk";
pub const KNOWLEDGE_TECHNICAL_FACT_COLLECTION: &str = "knowledge_technical_fact";
/// Legacy single-dim vector collection. New per-library deployments use
/// [`chunk_vector_collection_for_dim`] / [`entity_vector_collection_for_dim`]
/// which return `knowledge_chunk_vector_d<dim>` / `knowledge_entity_vector_d<dim>`.
/// The constant remains for migration: legacy rows live here until
/// `ironrag-maintenance migrate vector-per-dim` splits them into
/// per-dim collections.
pub const KNOWLEDGE_CHUNK_VECTOR_COLLECTION: &str = "knowledge_chunk_vector";
pub const KNOWLEDGE_ENTITY_VECTOR_COLLECTION: &str = "knowledge_entity_vector";

/// Per-dim chunk-vector collection name. Each distinct embedding-model
/// dimension lives in its own Arango collection so libraries on different
/// embed bindings can coexist without dropping each other's vectors.
pub fn chunk_vector_collection_for_dim(dim: u64) -> String {
    format!("knowledge_chunk_vector_d{dim}")
}

/// Per-dim entity-vector collection name (graph-node vectors). Same
/// per-dim scheme as the chunk-vector side, applied to entity embeddings.
pub fn entity_vector_collection_for_dim(dim: u64) -> String {
    format!("knowledge_entity_vector_d{dim}")
}

/// Per-dim chunk-vector ANN index name (built on each per-dim collection's
/// `vector` field). Index name is unique per collection so concurrent rebuilds
/// against different dims never trample each other.
pub fn chunk_vector_index_for_dim(dim: u64) -> String {
    format!("knowledge_chunk_vector_d{dim}_index")
}

/// Per-dim entity-vector ANN index name.
pub fn entity_vector_index_for_dim(dim: u64) -> String {
    format!("knowledge_entity_vector_d{dim}_index")
}

// --- Per-(library, dim) vector shards -------------------------------------
//
// Physical tenant isolation: each library's vectors live in their own shard,
// keyed by `(dim, library)`. This keeps `APPROX_NEAR_COSINE` scoped to one
// library's (small) vector set — fast ANN with full recall — and makes tenant
// deletion a collection drop, with no cross-tenant data sharing one IVF index.
//
// These names are an Arango-adapter detail. The query/ingest layers address
// vectors only as `(library_id, dim)` through the storage port; the adapter
// maps that to the collection/index names below. A future non-Arango backend
// implements the same port without reusing this naming.
//
// Naming: `knowledge_chunk_vector_d{dim}_l{library_hex}` where `library_hex`
// is the library UUID in simple (hyphen-free) form, so a collection name is
// reversible to `(kind, dim, library_id)` by [`parse_library_vector_shard`].

/// Which vector family a per-(library, dim) shard holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorShardKind {
    Chunk,
    Entity,
}

/// A per-(library, dim) shard name decoded back into its components.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedLibraryVectorShard {
    pub kind: VectorShardKind,
    pub dim: u64,
    pub library_id: Uuid,
}

/// Collection-name-safe token for a library: the UUID in simple (hyphen-free)
/// hex form. Reversible via [`Uuid::parse_str`].
#[must_use]
pub fn library_vector_token(library_id: Uuid) -> String {
    library_id.simple().to_string()
}

/// Per-(library, dim) chunk-vector collection name.
#[must_use]
pub fn chunk_vector_collection_for_library(dim: u64, library_id: Uuid) -> String {
    format!("knowledge_chunk_vector_d{dim}_l{}", library_vector_token(library_id))
}

/// Per-(library, dim) entity-vector collection name.
#[must_use]
pub fn entity_vector_collection_for_library(dim: u64, library_id: Uuid) -> String {
    format!("knowledge_entity_vector_d{dim}_l{}", library_vector_token(library_id))
}

/// Per-(library, dim) chunk-vector ANN index name.
#[must_use]
pub fn chunk_vector_index_for_library(dim: u64, library_id: Uuid) -> String {
    format!("{}_index", chunk_vector_collection_for_library(dim, library_id))
}

/// Per-(library, dim) entity-vector ANN index name.
#[must_use]
pub fn entity_vector_index_for_library(dim: u64, library_id: Uuid) -> String {
    format!("{}_index", entity_vector_collection_for_library(dim, library_id))
}

/// Decode a shared per-dim CHUNK shard name (`knowledge_chunk_vector_d{dim}`)
/// to its dimension. Returns `None` for the per-(library, dim) shards (which
/// carry an `_l{library_hex}` suffix), the legacy unscoped collection, and any
/// non-chunk collection — so the per-library migration can enumerate exactly
/// the shared shards it needs to drain without touching per-library shards.
#[must_use]
pub fn parse_per_dim_chunk_vector_dim(name: &str) -> Option<u64> {
    let dim_part = name.strip_prefix("knowledge_chunk_vector_d")?;
    if dim_part.is_empty() || !dim_part.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    dim_part.parse::<u64>().ok()
}

/// Decode a per-(library, dim) shard collection name back into its components.
/// Returns `None` for legacy single-dim shards (`..._d{dim}` with no `_l`
/// suffix) and any non-shard collection, so callers can distinguish the
/// per-library scheme from the legacy per-dim / unscoped collections.
#[must_use]
pub fn parse_library_vector_shard(name: &str) -> Option<ParsedLibraryVectorShard> {
    let (kind, rest) = name
        .strip_prefix("knowledge_chunk_vector_d")
        .map(|rest| (VectorShardKind::Chunk, rest))
        .or_else(|| {
            name.strip_prefix("knowledge_entity_vector_d")
                .map(|rest| (VectorShardKind::Entity, rest))
        })?;
    // `rest` is `{dim}_l{library_hex}`. Index suffix is not part of a shard
    // collection name, so reject anything carrying `_index`.
    let (dim_part, library_hex) = rest.split_once("_l")?;
    if dim_part.is_empty() || !dim_part.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let dim = dim_part.parse::<u64>().ok()?;
    let library_id = Uuid::parse_str(library_hex).ok()?;
    Some(ParsedLibraryVectorShard { kind, dim, library_id })
}

pub const KNOWLEDGE_ENTITY_COLLECTION: &str = "knowledge_entity";
pub const KNOWLEDGE_ENTITY_CANDIDATE_COLLECTION: &str = "knowledge_entity_candidate";
pub const KNOWLEDGE_RELATION_COLLECTION: &str = "knowledge_relation";
pub const KNOWLEDGE_RELATION_CANDIDATE_COLLECTION: &str = "knowledge_relation_candidate";
pub const KNOWLEDGE_EVIDENCE_COLLECTION: &str = "knowledge_evidence";
pub const KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION: &str = "knowledge_context_bundle";
pub const KNOWLEDGE_RETRIEVAL_TRACE_COLLECTION: &str = "knowledge_retrieval_trace";

pub const KNOWLEDGE_DOCUMENT_REVISION_EDGE: &str = "knowledge_document_revision_edge";
pub const KNOWLEDGE_REVISION_BLOCK_EDGE: &str = "knowledge_revision_block_edge";
pub const KNOWLEDGE_REVISION_CHUNK_EDGE: &str = "knowledge_revision_chunk_edge";
pub const KNOWLEDGE_BLOCK_CHUNK_EDGE: &str = "knowledge_block_chunk_edge";
pub const KNOWLEDGE_CHUNK_MENTIONS_ENTITY_EDGE: &str = "knowledge_chunk_mentions_entity_edge";
pub const KNOWLEDGE_RELATION_SUBJECT_EDGE: &str = "knowledge_relation_subject_edge";
pub const KNOWLEDGE_RELATION_OBJECT_EDGE: &str = "knowledge_relation_object_edge";
pub const KNOWLEDGE_EVIDENCE_SOURCE_EDGE: &str = "knowledge_evidence_source_edge";
pub const KNOWLEDGE_FACT_EVIDENCE_EDGE: &str = "knowledge_fact_evidence_edge";
pub const KNOWLEDGE_EVIDENCE_SUPPORTS_ENTITY_EDGE: &str = "knowledge_evidence_supports_entity_edge";
pub const KNOWLEDGE_EVIDENCE_SUPPORTS_RELATION_EDGE: &str =
    "knowledge_evidence_supports_relation_edge";
pub const KNOWLEDGE_BUNDLE_CHUNK_EDGE: &str = "knowledge_bundle_chunk_edge";
pub const KNOWLEDGE_BUNDLE_ENTITY_EDGE: &str = "knowledge_bundle_entity_edge";
pub const KNOWLEDGE_BUNDLE_RELATION_EDGE: &str = "knowledge_bundle_relation_edge";
pub const KNOWLEDGE_BUNDLE_EVIDENCE_EDGE: &str = "knowledge_bundle_evidence_edge";

pub const KNOWLEDGE_SEARCH_VIEW: &str = "knowledge_search_view";

/// Custom trigram analyzer registered on startup and attached to the
/// `knowledge_document.title` / `file_name` fields. Powers typo-tolerant
/// matches in the `search_chunks` title subquery via NGRAM_MATCH.
/// Keeping the name in one place keeps bootstrap + search_store in sync.
pub const KNOWLEDGE_NGRAM_ANALYZER: &str = "ironrag_ngram";

pub const KNOWLEDGE_GRAPH_NAME: &str = "knowledge_graph";
pub const KNOWLEDGE_CHUNK_VECTOR_INDEX: &str = "knowledge_chunk_vector_index";
pub const KNOWLEDGE_ENTITY_VECTOR_INDEX: &str = "knowledge_entity_vector_index";
pub const KNOWLEDGE_CHUNK_VECTOR_REVISION_GENERATION_INDEX: &str =
    "knowledge_chunk_vector_revision_generation_index";
pub const KNOWLEDGE_CHUNK_VECTOR_CHUNK_MODEL_INDEX: &str =
    "knowledge_chunk_vector_chunk_model_index";
pub const KNOWLEDGE_CHUNK_VECTOR_LIBRARY_INDEX: &str = "knowledge_chunk_vector_library_index";
pub const KNOWLEDGE_REVISION_LIBRARY_VECTOR_STATE_INDEX: &str =
    "knowledge_revision_library_vector_state_index";
pub const KNOWLEDGE_ENTITY_VECTOR_LIBRARY_INDEX: &str = "knowledge_entity_vector_library_index";
pub const KNOWLEDGE_STRUCTURED_REVISION_REVISION_INDEX: &str =
    "knowledge_structured_revision_revision_index";
pub const KNOWLEDGE_STRUCTURED_BLOCK_REVISION_ORDINAL_INDEX: &str =
    "knowledge_structured_block_revision_ordinal_index";
pub const KNOWLEDGE_STRUCTURED_BLOCK_BLOCK_ID_INDEX: &str =
    "knowledge_structured_block_block_id_index";
pub const KNOWLEDGE_TECHNICAL_FACT_REVISION_INDEX: &str = "knowledge_technical_fact_revision_index";
pub const KNOWLEDGE_TECHNICAL_FACT_LITERAL_INDEX: &str = "knowledge_technical_fact_literal_index";
pub const KNOWLEDGE_CHUNK_LIBRARY_DOCUMENT_INDEX: &str = "knowledge_chunk_library_document_index";
pub const KNOWLEDGE_CHUNK_REVISION_INDEX: &str = "knowledge_chunk_revision_index";
pub const KNOWLEDGE_DOCUMENT_LIBRARY_UPDATED_INDEX: &str =
    "knowledge_document_library_updated_index";
pub const KNOWLEDGE_REVISION_REVISION_ID_INDEX: &str = "knowledge_revision_revision_id_index";
pub const KNOWLEDGE_REVISION_DOCUMENT_REVISION_INDEX: &str =
    "knowledge_revision_document_revision_index";
pub const KNOWLEDGE_ENTITY_LIBRARY_SUPPORT_INDEX: &str = "knowledge_entity_library_support_index";
pub const KNOWLEDGE_RELATION_LIBRARY_SUPPORT_INDEX: &str =
    "knowledge_relation_library_support_index";
pub const KNOWLEDGE_CONTEXT_BUNDLE_EXECUTION_INDEX: &str =
    "knowledge_context_bundle_execution_index";
pub const KNOWLEDGE_CONTEXT_BUNDLE_LIBRARY_UPDATED_INDEX: &str =
    "knowledge_context_bundle_library_updated_index";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArangoPersistentIndexSpec {
    pub collection: &'static str,
    pub name: &'static str,
    pub fields: &'static [&'static str],
    pub unique: bool,
    pub sparse: bool,
}

pub const KNOWLEDGE_PERSISTENT_INDEXES: &[ArangoPersistentIndexSpec] = &[
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_DOCUMENT_COLLECTION,
        name: KNOWLEDGE_DOCUMENT_LIBRARY_UPDATED_INDEX,
        fields: &["library_id", "workspace_id", "updated_at", "document_id"],
        unique: false,
        sparse: false,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_REVISION_COLLECTION,
        name: KNOWLEDGE_REVISION_REVISION_ID_INDEX,
        fields: &["revision_id"],
        unique: false,
        sparse: false,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_REVISION_COLLECTION,
        name: KNOWLEDGE_REVISION_DOCUMENT_REVISION_INDEX,
        fields: &["document_id", "revision_number", "revision_id"],
        unique: false,
        sparse: false,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_REVISION_COLLECTION,
        name: KNOWLEDGE_REVISION_LIBRARY_VECTOR_STATE_INDEX,
        fields: &["library_id", "vector_state", "revision_id"],
        unique: false,
        sparse: false,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_STRUCTURED_REVISION_COLLECTION,
        name: KNOWLEDGE_STRUCTURED_REVISION_REVISION_INDEX,
        fields: &["revision_id", "document_id"],
        unique: false,
        sparse: false,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_STRUCTURED_BLOCK_COLLECTION,
        name: KNOWLEDGE_STRUCTURED_BLOCK_REVISION_ORDINAL_INDEX,
        fields: &["revision_id", "ordinal"],
        unique: false,
        sparse: false,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_STRUCTURED_BLOCK_COLLECTION,
        name: KNOWLEDGE_STRUCTURED_BLOCK_BLOCK_ID_INDEX,
        fields: &["block_id"],
        unique: false,
        sparse: false,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_CHUNK_COLLECTION,
        name: KNOWLEDGE_CHUNK_LIBRARY_DOCUMENT_INDEX,
        fields: &["library_id", "document_id", "chunk_state", "chunk_index"],
        unique: false,
        sparse: false,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_CHUNK_COLLECTION,
        name: KNOWLEDGE_CHUNK_REVISION_INDEX,
        fields: &["revision_id", "chunk_index", "chunk_id"],
        unique: false,
        sparse: false,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
        name: KNOWLEDGE_CHUNK_VECTOR_REVISION_GENERATION_INDEX,
        fields: &["revision_id", "embedding_model_key", "vector_kind", "freshness_generation"],
        unique: false,
        sparse: false,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
        name: KNOWLEDGE_CHUNK_VECTOR_CHUNK_MODEL_INDEX,
        fields: &[
            "chunk_id",
            "embedding_model_key",
            "vector_kind",
            "freshness_generation",
            "created_at",
        ],
        unique: false,
        sparse: false,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
        name: KNOWLEDGE_CHUNK_VECTOR_LIBRARY_INDEX,
        fields: &["library_id", "vector_kind", "freshness_generation"],
        unique: false,
        sparse: false,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_ENTITY_VECTOR_COLLECTION,
        name: KNOWLEDGE_ENTITY_VECTOR_LIBRARY_INDEX,
        fields: &["library_id", "embedding_model_key"],
        unique: false,
        sparse: false,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_ENTITY_COLLECTION,
        name: KNOWLEDGE_ENTITY_LIBRARY_SUPPORT_INDEX,
        fields: &["library_id", "support_count", "updated_at", "entity_id"],
        unique: false,
        sparse: false,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_RELATION_COLLECTION,
        name: KNOWLEDGE_RELATION_LIBRARY_SUPPORT_INDEX,
        fields: &["library_id", "support_count", "updated_at", "relation_id"],
        unique: false,
        sparse: false,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION,
        name: KNOWLEDGE_CONTEXT_BUNDLE_EXECUTION_INDEX,
        fields: &["query_execution_id"],
        unique: false,
        sparse: true,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION,
        name: KNOWLEDGE_CONTEXT_BUNDLE_LIBRARY_UPDATED_INDEX,
        fields: &["library_id", "updated_at", "bundle_id"],
        unique: false,
        sparse: false,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_TECHNICAL_FACT_COLLECTION,
        name: KNOWLEDGE_TECHNICAL_FACT_REVISION_INDEX,
        fields: &["revision_id", "fact_kind"],
        unique: false,
        sparse: false,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_TECHNICAL_FACT_COLLECTION,
        name: KNOWLEDGE_TECHNICAL_FACT_LITERAL_INDEX,
        fields: &["canonical_value_exact", "fact_kind"],
        unique: false,
        sparse: false,
    },
    // Edge-collection library_id indexes: edges now carry library_id
    // so snapshot export/clear can filter via the index instead of
    // resolving every edge's endpoint vertex via DOCUMENT().
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_DOCUMENT_REVISION_EDGE,
        name: "idx_edge_doc_rev_library",
        fields: &["library_id"],
        unique: false,
        sparse: true,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_REVISION_CHUNK_EDGE,
        name: "idx_edge_rev_chunk_library",
        fields: &["library_id"],
        unique: false,
        sparse: true,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_REVISION_BLOCK_EDGE,
        name: "idx_edge_rev_block_library",
        fields: &["library_id"],
        unique: false,
        sparse: true,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_BLOCK_CHUNK_EDGE,
        name: "idx_edge_block_chunk_library",
        fields: &["library_id"],
        unique: false,
        sparse: true,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_CHUNK_MENTIONS_ENTITY_EDGE,
        name: "idx_edge_chunk_entity_library",
        fields: &["library_id"],
        unique: false,
        sparse: true,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_RELATION_SUBJECT_EDGE,
        name: "idx_edge_rel_subj_library",
        fields: &["library_id"],
        unique: false,
        sparse: true,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_RELATION_OBJECT_EDGE,
        name: "idx_edge_rel_obj_library",
        fields: &["library_id"],
        unique: false,
        sparse: true,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_EVIDENCE_SOURCE_EDGE,
        name: "idx_edge_evi_src_library",
        fields: &["library_id"],
        unique: false,
        sparse: true,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_FACT_EVIDENCE_EDGE,
        name: "idx_edge_fact_evi_library",
        fields: &["library_id"],
        unique: false,
        sparse: true,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_EVIDENCE_SUPPORTS_ENTITY_EDGE,
        name: "idx_edge_evi_entity_library",
        fields: &["library_id"],
        unique: false,
        sparse: true,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_EVIDENCE_SUPPORTS_RELATION_EDGE,
        name: "idx_edge_evi_rel_library",
        fields: &["library_id"],
        unique: false,
        sparse: true,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_BUNDLE_CHUNK_EDGE,
        name: "idx_edge_bundle_chunk_library",
        fields: &["library_id"],
        unique: false,
        sparse: true,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_BUNDLE_CHUNK_EDGE,
        name: "idx_edge_bundle_chunk_bundle_rank",
        fields: &["bundle_id", "rank", "created_at"],
        unique: false,
        sparse: false,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_BUNDLE_ENTITY_EDGE,
        name: "idx_edge_bundle_entity_library",
        fields: &["library_id"],
        unique: false,
        sparse: true,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_BUNDLE_ENTITY_EDGE,
        name: "idx_edge_bundle_entity_bundle_rank",
        fields: &["bundle_id", "rank", "created_at"],
        unique: false,
        sparse: false,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_BUNDLE_RELATION_EDGE,
        name: "idx_edge_bundle_rel_library",
        fields: &["library_id"],
        unique: false,
        sparse: true,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_BUNDLE_RELATION_EDGE,
        name: "idx_edge_bundle_rel_bundle_rank",
        fields: &["bundle_id", "rank", "created_at"],
        unique: false,
        sparse: false,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_BUNDLE_EVIDENCE_EDGE,
        name: "idx_edge_bundle_evi_library",
        fields: &["library_id"],
        unique: false,
        sparse: true,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_BUNDLE_EVIDENCE_EDGE,
        name: "idx_edge_bundle_evi_bundle_rank",
        fields: &["bundle_id", "rank", "created_at"],
        unique: false,
        sparse: false,
    },
];

pub const DOCUMENT_COLLECTIONS: &[&str] = &[
    KNOWLEDGE_DOCUMENT_COLLECTION,
    KNOWLEDGE_REVISION_COLLECTION,
    KNOWLEDGE_STRUCTURED_REVISION_COLLECTION,
    KNOWLEDGE_STRUCTURED_BLOCK_COLLECTION,
    KNOWLEDGE_CHUNK_COLLECTION,
    KNOWLEDGE_TECHNICAL_FACT_COLLECTION,
    KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
    KNOWLEDGE_ENTITY_VECTOR_COLLECTION,
    KNOWLEDGE_ENTITY_COLLECTION,
    KNOWLEDGE_ENTITY_CANDIDATE_COLLECTION,
    KNOWLEDGE_RELATION_COLLECTION,
    KNOWLEDGE_RELATION_CANDIDATE_COLLECTION,
    KNOWLEDGE_EVIDENCE_COLLECTION,
    KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION,
    KNOWLEDGE_RETRIEVAL_TRACE_COLLECTION,
];

pub const EDGE_COLLECTIONS: &[&str] = &[
    KNOWLEDGE_DOCUMENT_REVISION_EDGE,
    KNOWLEDGE_REVISION_BLOCK_EDGE,
    KNOWLEDGE_REVISION_CHUNK_EDGE,
    KNOWLEDGE_BLOCK_CHUNK_EDGE,
    KNOWLEDGE_CHUNK_MENTIONS_ENTITY_EDGE,
    KNOWLEDGE_RELATION_SUBJECT_EDGE,
    KNOWLEDGE_RELATION_OBJECT_EDGE,
    KNOWLEDGE_EVIDENCE_SOURCE_EDGE,
    KNOWLEDGE_FACT_EVIDENCE_EDGE,
    KNOWLEDGE_EVIDENCE_SUPPORTS_ENTITY_EDGE,
    KNOWLEDGE_EVIDENCE_SUPPORTS_RELATION_EDGE,
    KNOWLEDGE_BUNDLE_CHUNK_EDGE,
    KNOWLEDGE_BUNDLE_ENTITY_EDGE,
    KNOWLEDGE_BUNDLE_RELATION_EDGE,
    KNOWLEDGE_BUNDLE_EVIDENCE_EDGE,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_library_chunk_shard_round_trips() {
        let dim = 3072u64;
        let library_id = Uuid::parse_str("019d97f8-90d6-7e10-9495-d31ddf8e4854").unwrap();
        let name = chunk_vector_collection_for_library(dim, library_id);
        assert_eq!(name, "knowledge_chunk_vector_d3072_l019d97f890d67e109495d31ddf8e4854");
        let parsed = parse_library_vector_shard(&name).expect("chunk shard parses");
        assert_eq!(
            parsed,
            ParsedLibraryVectorShard { kind: VectorShardKind::Chunk, dim, library_id }
        );
    }

    #[test]
    fn per_library_entity_shard_round_trips() {
        let dim = 384u64;
        let library_id = Uuid::parse_str("00000000-0000-4000-8000-000000000abc").unwrap();
        let name = entity_vector_collection_for_library(dim, library_id);
        let parsed = parse_library_vector_shard(&name).expect("entity shard parses");
        assert_eq!(parsed.kind, VectorShardKind::Entity);
        assert_eq!(parsed.dim, dim);
        assert_eq!(parsed.library_id, library_id);
    }

    #[test]
    fn parser_rejects_legacy_per_dim_and_unscoped_collections() {
        // Legacy per-dim shard (no `_l` suffix) is NOT a per-library shard.
        assert!(parse_library_vector_shard("knowledge_chunk_vector_d3072").is_none());
        assert!(parse_library_vector_shard("knowledge_entity_vector_d1536").is_none());
        // Unscoped legacy collection.
        assert!(parse_library_vector_shard("knowledge_chunk_vector").is_none());
        // Index name is not a shard collection name.
        let library_id = Uuid::new_v4();
        let index = chunk_vector_index_for_library(3072, library_id);
        assert!(parse_library_vector_shard(&index).is_none());
        // Non-vector collection.
        assert!(parse_library_vector_shard("knowledge_document").is_none());
        // Non-numeric dim.
        assert!(parse_library_vector_shard("knowledge_chunk_vector_dXX_labc").is_none());
    }

    #[test]
    fn per_dim_chunk_dim_parses_shared_shard_and_rejects_per_library_shard() {
        // Shared per-dim chunk shards parse to their dim, on two dims.
        assert_eq!(parse_per_dim_chunk_vector_dim("knowledge_chunk_vector_d384"), Some(384));
        assert_eq!(parse_per_dim_chunk_vector_dim("knowledge_chunk_vector_d3072"), Some(3072));
        // Per-library shards carry an `_l{hex}` suffix → not a shared source.
        let library_id = Uuid::parse_str("019d97f8-90d6-7e10-9495-d31ddf8e4854").unwrap();
        let per_library = chunk_vector_collection_for_library(3072, library_id);
        assert_eq!(parse_per_dim_chunk_vector_dim(&per_library), None);
        // Legacy unscoped + entity + non-numeric dim all reject.
        assert_eq!(parse_per_dim_chunk_vector_dim("knowledge_chunk_vector"), None);
        assert_eq!(parse_per_dim_chunk_vector_dim("knowledge_entity_vector_d384"), None);
        assert_eq!(parse_per_dim_chunk_vector_dim("knowledge_chunk_vector_dXX"), None);
    }

    #[test]
    fn per_library_index_names_derive_from_collection() {
        let library_id = Uuid::new_v4();
        assert_eq!(
            chunk_vector_index_for_library(768, library_id),
            format!("{}_index", chunk_vector_collection_for_library(768, library_id))
        );
        assert_eq!(
            entity_vector_index_for_library(768, library_id),
            format!("{}_index", entity_vector_collection_for_library(768, library_id))
        );
    }
}
