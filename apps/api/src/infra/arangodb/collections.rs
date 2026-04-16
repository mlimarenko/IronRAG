pub const KNOWLEDGE_DOCUMENT_COLLECTION: &str = "knowledge_document";
pub const KNOWLEDGE_REVISION_COLLECTION: &str = "knowledge_revision";
pub const KNOWLEDGE_STRUCTURED_REVISION_COLLECTION: &str = "knowledge_structured_revision";
pub const KNOWLEDGE_STRUCTURED_BLOCK_COLLECTION: &str = "knowledge_structured_block";
pub const KNOWLEDGE_CHUNK_COLLECTION: &str = "knowledge_chunk";
pub const KNOWLEDGE_TECHNICAL_FACT_COLLECTION: &str = "knowledge_technical_fact";
pub const KNOWLEDGE_LIBRARY_GENERATION_COLLECTION: &str = "knowledge_library_generation";
pub const KNOWLEDGE_CHUNK_VECTOR_COLLECTION: &str = "knowledge_chunk_vector";
pub const KNOWLEDGE_ENTITY_VECTOR_COLLECTION: &str = "knowledge_entity_vector";
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
pub const KNOWLEDGE_GRAPH_NAME: &str = "knowledge_graph";
pub const KNOWLEDGE_CHUNK_VECTOR_INDEX: &str = "knowledge_chunk_vector_index";
pub const KNOWLEDGE_ENTITY_VECTOR_INDEX: &str = "knowledge_entity_vector_index";
pub const KNOWLEDGE_STRUCTURED_REVISION_REVISION_INDEX: &str =
    "knowledge_structured_revision_revision_index";
pub const KNOWLEDGE_STRUCTURED_BLOCK_REVISION_ORDINAL_INDEX: &str =
    "knowledge_structured_block_revision_ordinal_index";
pub const KNOWLEDGE_TECHNICAL_FACT_REVISION_INDEX: &str = "knowledge_technical_fact_revision_index";
pub const KNOWLEDGE_TECHNICAL_FACT_LITERAL_INDEX: &str = "knowledge_technical_fact_literal_index";
pub const KNOWLEDGE_DOCUMENT_LIBRARY_UPDATED_INDEX: &str =
    "knowledge_document_library_updated_index";
pub const KNOWLEDGE_REVISION_REVISION_ID_INDEX: &str = "knowledge_revision_revision_id_index";
pub const KNOWLEDGE_REVISION_DOCUMENT_REVISION_INDEX: &str =
    "knowledge_revision_document_revision_index";
pub const KNOWLEDGE_LIBRARY_GENERATION_LIBRARY_UPDATED_INDEX: &str =
    "knowledge_library_generation_library_updated_index";
pub const KNOWLEDGE_ENTITY_LIBRARY_SUPPORT_INDEX: &str = "knowledge_entity_library_support_index";
pub const KNOWLEDGE_RELATION_LIBRARY_SUPPORT_INDEX: &str =
    "knowledge_relation_library_support_index";

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
        collection: KNOWLEDGE_LIBRARY_GENERATION_COLLECTION,
        name: KNOWLEDGE_LIBRARY_GENERATION_LIBRARY_UPDATED_INDEX,
        fields: &["library_id", "updated_at", "generation_id"],
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
        collection: KNOWLEDGE_BUNDLE_ENTITY_EDGE,
        name: "idx_edge_bundle_entity_library",
        fields: &["library_id"],
        unique: false,
        sparse: true,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_BUNDLE_RELATION_EDGE,
        name: "idx_edge_bundle_rel_library",
        fields: &["library_id"],
        unique: false,
        sparse: true,
    },
    ArangoPersistentIndexSpec {
        collection: KNOWLEDGE_BUNDLE_EVIDENCE_EDGE,
        name: "idx_edge_bundle_evi_library",
        fields: &["library_id"],
        unique: false,
        sparse: true,
    },
];

pub const DOCUMENT_COLLECTIONS: &[&str] = &[
    KNOWLEDGE_DOCUMENT_COLLECTION,
    KNOWLEDGE_REVISION_COLLECTION,
    KNOWLEDGE_STRUCTURED_REVISION_COLLECTION,
    KNOWLEDGE_STRUCTURED_BLOCK_COLLECTION,
    KNOWLEDGE_CHUNK_COLLECTION,
    KNOWLEDGE_TECHNICAL_FACT_COLLECTION,
    KNOWLEDGE_LIBRARY_GENERATION_COLLECTION,
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
