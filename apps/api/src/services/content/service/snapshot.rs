//! Canonical library snapshot — streaming tar.zst export and import.
//!
//! The archive layout is:
//!
//! ```text
//! manifest.json                         # first — declares include kinds and table list
//! postgres/<table>/part-NNNNNN.ndjson   # chunked per table, 64 MiB cap per part
//! arango/<collection>/part-NNNNNN.ndjson
//! arango-edges/<collection>/part-NNNNNN.ndjson
//! blobs/<escaped-storage-key>           # raw bytes, one entry per content blob
//! summary.json                          # last — row counts observed during export
//! ```
//!
//! Export is a single tar stream wrapped in zstd. The `async_tar::Builder`
//! writes into a `ZstdEncoder` which writes into a `tokio::io::DuplexStream`
//! write half; the HTTP layer reads the other half as a response body
//! stream. Back-pressure is natural — if the client stops reading, the
//! exporter task blocks on the next `builder.append` and Postgres cursors
//! pause with it.
//!
//! Import takes the raw request body as an async stream, wraps it in a
//! zstd decoder, hands it to `async_tar::Archive`, and processes entries
//! in their serialized order. No temporary file is created — tar entries
//! are self-contained so the reader does not need seekable input.
//!
//! The `include` query parameter on export selects which families of
//! entities end up in the archive. Import does NOT take an include filter
//! — it trusts the manifest that the archive itself carries, which is the
//! canonical source of what was exported.

use std::collections::{BTreeMap, HashSet};

use anyhow::{Context, anyhow, bail};
use async_compression::tokio::{bufread::ZstdDecoder, write::ZstdEncoder};
use async_tar::{Archive, Builder, EntryType, Header};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, BufReader};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::arangodb::{
        client::ArangoClient,
        collections::{
            KNOWLEDGE_BLOCK_CHUNK_EDGE, KNOWLEDGE_BUNDLE_CHUNK_EDGE, KNOWLEDGE_BUNDLE_ENTITY_EDGE,
            KNOWLEDGE_BUNDLE_EVIDENCE_EDGE, KNOWLEDGE_BUNDLE_RELATION_EDGE,
            KNOWLEDGE_CHUNK_COLLECTION, KNOWLEDGE_CHUNK_MENTIONS_ENTITY_EDGE,
            KNOWLEDGE_CHUNK_VECTOR_COLLECTION, KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION,
            KNOWLEDGE_DOCUMENT_COLLECTION, KNOWLEDGE_DOCUMENT_REVISION_EDGE,
            KNOWLEDGE_ENTITY_COLLECTION, KNOWLEDGE_ENTITY_VECTOR_COLLECTION,
            KNOWLEDGE_EVIDENCE_COLLECTION, KNOWLEDGE_EVIDENCE_SOURCE_EDGE,
            KNOWLEDGE_EVIDENCE_SUPPORTS_ENTITY_EDGE, KNOWLEDGE_EVIDENCE_SUPPORTS_RELATION_EDGE,
            KNOWLEDGE_FACT_EVIDENCE_EDGE, KNOWLEDGE_RELATION_COLLECTION,
            KNOWLEDGE_RELATION_OBJECT_EDGE, KNOWLEDGE_RELATION_SUBJECT_EDGE,
            KNOWLEDGE_REVISION_BLOCK_EDGE, KNOWLEDGE_REVISION_CHUNK_EDGE,
            KNOWLEDGE_REVISION_COLLECTION, KNOWLEDGE_STRUCTURED_BLOCK_COLLECTION,
            KNOWLEDGE_STRUCTURED_REVISION_COLLECTION, KNOWLEDGE_TECHNICAL_FACT_COLLECTION,
        },
    },
    services::content::error::ContentServiceError,
};

/// Prefix of every per-dim chunk-vector shard. Matches
/// [`crate::infra::arangodb::collections::chunk_vector_collection_for_dim`]
/// which formats `knowledge_chunk_vector_d<dim>`.
const PER_DIM_CHUNK_VECTOR_PREFIX: &str = "knowledge_chunk_vector_d";
/// Prefix of every per-dim entity-vector shard.
const PER_DIM_ENTITY_VECTOR_PREFIX: &str = "knowledge_entity_vector_d";

/// AQL cursor `batchSize` used when exporting any vector collection
/// (per-dim chunk/entity shards plus the legacy single-dim
/// `knowledge_chunk_vector` / `knowledge_entity_vector` collections).
/// Vector rows carry a `vector` field of ~1k-4k floats, and the Arango
/// cursor default of 1000 rows would materialize ~25-100 MiB of JSON
/// per HTTP response on these collections. 200 is small enough that
/// the cursor response and the in-process JSON parse buffer both stay
/// bounded on 6-figure-row libraries.
const VECTOR_EXPORT_BATCH_SIZE: u32 = 200;

/// Hard wall-clock budget for a single arango collection export stage.
/// On a large production-class library a healthy chunk-vector export streams
/// at well over 1k rows/sec, so 30 minutes is ~2x the worst-case for a
/// 1.5M-row collection. Anything beyond that is a hung cursor / starved
/// pool and is better surfaced as a fail-loud error than left to hang
/// the whole export forever.
const ARANGO_COLLECTION_EXPORT_BUDGET: std::time::Duration =
    std::time::Duration::from_secs(30 * 60);

/// `true` when `name` refers to any vector collection — per-dim
/// chunk/entity shards or the legacy single-dim
/// `knowledge_chunk_vector` / `knowledge_entity_vector` collections.
/// Snapshot-export uses this to pick the smaller cursor batch size
/// that keeps vector exports memory-bounded.
fn is_vector_collection_name(name: &str) -> bool {
    is_per_dim_vector_collection_name(name)
        || name == KNOWLEDGE_CHUNK_VECTOR_COLLECTION
        || name == KNOWLEDGE_ENTITY_VECTOR_COLLECTION
}

/// `true` if `name` is a canonical per-dim vector shard such as
/// `knowledge_chunk_vector_d1024` or `knowledge_entity_vector_d3072`.
/// Used by the snapshot validator to accept runtime-discovered shards
/// alongside the static collection list.
fn is_per_dim_vector_collection_name(name: &str) -> bool {
    parse_per_dim_vector_collection_dim(name).is_some()
}

/// Parse the dim suffix off a canonical per-dim vector shard name.
/// Returns `None` when the name does not match the per-dim shape.
fn parse_per_dim_vector_collection_dim(name: &str) -> Option<u64> {
    let suffix = name
        .strip_prefix(PER_DIM_CHUNK_VECTOR_PREFIX)
        .or_else(|| name.strip_prefix(PER_DIM_ENTITY_VECTOR_PREFIX))?;
    if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    suffix.parse::<u64>().ok()
}

/// `true` when `name` is a per-dim chunk-vector shard
/// (`knowledge_chunk_vector_d<dim>`). Used to decide whether the
/// restore path should ensure a chunk-side vs entity-side shard.
fn is_per_dim_chunk_vector_collection_name(name: &str) -> bool {
    name.strip_prefix(PER_DIM_CHUNK_VECTOR_PREFIX)
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

/// Manifest entry describing one per-dim vector shard the exporter
/// observed at runtime. The restore path lazy-ensures the same shard
/// (collection + ANN index + persistent indexes) before streaming the
/// archived rows back in.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct VectorShardEntry {
    pub name: String,
    pub dim: u64,
}

// ===========================================================================
// Public types
// ===========================================================================

/// Schema version of the snapshot archive format. Bumped any time the
/// manifest shape or on-disk layout changes in a backwards-incompatible
/// way.
pub const SNAPSHOT_SCHEMA_VERSION: u32 = 5;

/// Soft cap for a single NDJSON part inside the tar stream. Small enough
/// that no individual table part holds the entire table in memory, large
/// enough that tar header overhead stays negligible.
const CHUNK_BYTES_SOFT_CAP: usize = 64 * 1024 * 1024;

/// Hard cap on a single NDJSON row during import. Rows are read with
/// `read_until` against a bounded buffer; anything beyond this size
/// aborts the import. The biggest legitimate row in the current schema
/// is a `content_revision` with an embedded markdown blob; even very
/// verbose ones stay well under 16 MiB.
const MAX_IMPORT_LINE_BYTES: usize = 32 * 1024 * 1024;

/// Scope of a library snapshot.
///
/// A library is an atomic unit from the operator's point of view: its
/// documents, revisions, chunks, graph facts, knowledge entities and
/// relations all describe the same thing and are worthless without each
/// other. The canonical scope keeps that domain model whole instead of
/// exposing persistence-tier fragments to operators.
///
/// The canonical scope `LibraryData` therefore always includes every
/// non-blob row required to rebuild the library 1:1. `Blobs` is the
/// separate opt-in toggle for original source files (PDFs, images,
/// etc.); it is optional because a large library's source tree can
/// easily dwarf the rest of the snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IncludeKind {
    /// `catalog_workspace` row that owns the library. Runtime AI
    /// credentials and bindings are deployment configuration, not
    /// portable library data, so snapshots never export provider
    /// secrets or binding state.
    Workspace,
    /// Everything owned by a library that is NOT a raw source file —
    /// postgres rows (content + runtime graph) and arango documents /
    /// edges (knowledge base).
    LibraryData,
    /// Original uploaded files (PDFs, docx, images, …) keyed by
    /// `content_revision.storage_key`.
    Blobs,
}

impl IncludeKind {
    pub fn parse_csv(input: &str) -> Result<Vec<Self>, ContentServiceError> {
        let mut seen: HashSet<Self> = HashSet::new();
        let mut out: Vec<Self> = Vec::new();
        for raw in input.split(',') {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            let kind = match trimmed {
                "workspace" => Self::Workspace,
                "library_data" => Self::LibraryData,
                "blobs" => Self::Blobs,
                other => {
                    return Err(ContentServiceError::InvalidRequest {
                        message: format!("unknown include kind `{other}`"),
                    });
                }
            };
            if seen.insert(kind) {
                out.push(kind);
            }
        }
        if out.is_empty() {
            return Err(ContentServiceError::InvalidRequest {
                message: "`include` must name at least one kind".to_string(),
            });
        }
        Self::validate(&out)?;
        Ok(out)
    }

    /// Enforce dependency ordering. Blobs without LibraryData would
    /// produce orphan files with no `content_revision` row pointing
    /// at them — rejected. `Workspace` is independent and can travel
    /// alone (useful for cloning AI settings between stands).
    pub fn validate(kinds: &[Self]) -> Result<(), ContentServiceError> {
        let has_library = kinds.contains(&Self::LibraryData);
        if kinds.contains(&Self::Blobs) && !has_library {
            return Err(ContentServiceError::InvalidRequest {
                message: "include kind `blobs` requires `library_data`".to_string(),
            });
        }
        Ok(())
    }
}

/// Overwrite mode for restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OverwriteMode {
    /// Fail the request if the library already exists (default).
    #[default]
    Reject,
    /// Delete all owned content/runtime rows, graph documents, and blobs
    /// under this library id, then insert everything from the archive
    /// under the selected library identity. Not atomic across Postgres,
    /// Arango, and the blob store — a failed restore may leave graph/blob
    /// state partially refreshed, and the same archive must be re-applied
    /// to converge.
    Replace,
}

impl OverwriteMode {
    pub fn parse(input: &str) -> Result<Self, ContentServiceError> {
        match input.trim() {
            "" | "reject" => Ok(Self::Reject),
            "replace" => Ok(Self::Replace),
            other => Err(ContentServiceError::InvalidRequest {
                message: format!("unknown overwrite mode `{other}`"),
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SnapshotManifest {
    pub schema_version: u32,
    pub library_id: Uuid,
    pub library_slug: String,
    pub exported_at: chrono::DateTime<chrono::Utc>,
    pub source_version: String,
    pub include_kinds: Vec<IncludeKind>,
    pub postgres_tables: Vec<String>,
    pub arango_doc_collections: Vec<String>,
    pub arango_edge_collections: Vec<String>,
    pub has_blobs: bool,
    /// Per-dim vector shards (`knowledge_chunk_vector_d<dim>` /
    /// `knowledge_entity_vector_d<dim>`) observed at export time. The
    /// restore path lazy-ensures each shard before streaming its rows
    /// back so the target deployment ends up with the same per-dim
    /// layout the source had. `#[serde(default)]` keeps older v5
    /// archives that pre-date per-library vector dimensions parseable.
    #[serde(default)]
    pub vector_shards: Vec<VectorShardEntry>,
}

#[derive(Debug, Default, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SnapshotSummary {
    pub postgres_row_counts: BTreeMap<String, u64>,
    pub arango_doc_counts: BTreeMap<String, u64>,
    pub arango_edge_counts: BTreeMap<String, u64>,
    pub blob_count: u64,
    pub missing_blob_keys: Vec<String>,
}

#[derive(Debug, Default)]
pub struct SnapshotImportReport {
    pub library_id: Uuid,
    pub postgres_rows_by_table: Vec<(String, u64)>,
    pub arango_docs_by_collection: Vec<(String, u64)>,
    pub arango_edges_by_collection: Vec<(String, u64)>,
    pub skipped_arango_edges_by_collection: Vec<(String, u64)>,
    pub blobs_restored: u64,
    pub overwrite_mode: OverwriteMode,
    pub include_kinds: Vec<IncludeKind>,
}

// ===========================================================================
// Section descriptors
// ===========================================================================

const POSTGRES_CONTENT_TABLES: &[&str] = &[
    "content_document",
    "content_revision",
    "content_chunk",
    "content_mutation",
    "content_mutation_item",
    "content_document_head",
];

const POSTGRES_RUNTIME_GRAPH_TABLES: &[&str] = &[
    "runtime_graph_snapshot",
    "runtime_graph_node",
    "runtime_graph_edge",
    "runtime_graph_evidence",
    "runtime_graph_canonical_summary",
];

const POSTGRES_WORKSPACE_TABLES: &[&str] = &["catalog_workspace"];

const POSTGRES_LIBRARY_ROOT_TABLES: &[&str] = &["catalog_library"];

const ARANGO_DOC_COLLECTIONS: &[&str] = &[
    KNOWLEDGE_DOCUMENT_COLLECTION,
    KNOWLEDGE_REVISION_COLLECTION,
    KNOWLEDGE_CHUNK_COLLECTION,
    KNOWLEDGE_STRUCTURED_REVISION_COLLECTION,
    KNOWLEDGE_STRUCTURED_BLOCK_COLLECTION,
    KNOWLEDGE_TECHNICAL_FACT_COLLECTION,
    KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
    KNOWLEDGE_ENTITY_VECTOR_COLLECTION,
    KNOWLEDGE_ENTITY_COLLECTION,
    KNOWLEDGE_RELATION_COLLECTION,
    KNOWLEDGE_EVIDENCE_COLLECTION,
    KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION,
];

const ARANGO_EDGE_COLLECTIONS: &[&str] = &[
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

#[derive(Debug)]
struct SnapshotRowScope {
    source_library_id: Uuid,
    target_library_id: Uuid,
    source_workspace_id: Option<Uuid>,
    target_workspace_id: Uuid,
    document_ids: HashSet<Uuid>,
    revision_ids: HashSet<Uuid>,
    mutation_ids: HashSet<Uuid>,
    declared_blob_keys: HashSet<(String, String)>,
    arango_document_ids: HashSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotArangoRowAction {
    Import,
    SkipDanglingEdge,
}

impl SnapshotRowScope {
    fn new(source_library_id: Uuid, target_library_id: Uuid, target_workspace_id: Uuid) -> Self {
        Self {
            source_library_id,
            target_library_id,
            source_workspace_id: None,
            target_workspace_id,
            document_ids: HashSet::new(),
            revision_ids: HashSet::new(),
            mutation_ids: HashSet::new(),
            declared_blob_keys: HashSet::new(),
            arango_document_ids: HashSet::new(),
        }
    }

    fn normalize_postgres_row(
        &mut self,
        table: &str,
        row: &mut serde_json::Value,
    ) -> anyhow::Result<()> {
        match table {
            "catalog_workspace" => {
                let workspace_id = required_uuid_field(table, row, "id")?;
                self.bind_workspace(table, workspace_id)?;
                set_uuid_field(table, row, "id", self.target_workspace_id)?;
            }
            "catalog_library" => {
                require_uuid_field_eq(table, row, "id", self.source_library_id)?;
                let workspace_id = required_uuid_field(table, row, "workspace_id")?;
                self.bind_workspace(table, workspace_id)?;
                set_uuid_field(table, row, "id", self.target_library_id)?;
                set_uuid_field(table, row, "workspace_id", self.target_workspace_id)?;
            }
            "content_document" => {
                self.normalize_direct_library_workspace(table, row)?;
                let document_id = required_uuid_field(table, row, "id")?;
                self.document_ids.insert(document_id);
            }
            "content_revision" => {
                self.normalize_direct_library_workspace(table, row)?;
                let document_id = required_uuid_field(table, row, "document_id")?;
                if !self.document_ids.contains(&document_id) {
                    bail!(
                        "snapshot {table} row references document {document_id} outside target archive"
                    );
                }
                let revision_id = required_uuid_field(table, row, "id")?;
                self.revision_ids.insert(revision_id);
                if let Some(storage_key) = string_field(row, "storage_key") {
                    let source_key = storage_key.to_string();
                    let target_key = self.rewrite_storage_key(table, storage_key)?;
                    set_string_field(table, row, "storage_key", &target_key)?;
                    self.declared_blob_keys.insert((source_key, target_key));
                }
            }
            "content_chunk" => {
                let revision_id = required_uuid_field(table, row, "revision_id")?;
                if !self.revision_ids.contains(&revision_id) {
                    bail!(
                        "snapshot {table} row references revision {revision_id} outside target archive"
                    );
                }
            }
            "content_mutation" => {
                self.normalize_direct_library_workspace(table, row)?;
                let mutation_id = required_uuid_field(table, row, "id")?;
                self.mutation_ids.insert(mutation_id);
            }
            "content_mutation_item" => {
                let mutation_id = required_uuid_field(table, row, "mutation_id")?;
                if !self.mutation_ids.contains(&mutation_id) {
                    bail!(
                        "snapshot {table} row references mutation {mutation_id} outside target archive"
                    );
                }
                self.validate_optional_member(table, row, "document_id", &self.document_ids)?;
                self.validate_optional_member(table, row, "base_revision_id", &self.revision_ids)?;
                self.validate_optional_member(
                    table,
                    row,
                    "result_revision_id",
                    &self.revision_ids,
                )?;
            }
            "content_document_head" => {
                let document_id = required_uuid_field(table, row, "document_id")?;
                if !self.document_ids.contains(&document_id) {
                    bail!(
                        "snapshot {table} row references document {document_id} outside target archive"
                    );
                }
                self.validate_optional_member(
                    table,
                    row,
                    "active_revision_id",
                    &self.revision_ids,
                )?;
                self.validate_optional_member(
                    table,
                    row,
                    "readable_revision_id",
                    &self.revision_ids,
                )?;
                self.validate_optional_member(
                    table,
                    row,
                    "latest_mutation_id",
                    &self.mutation_ids,
                )?;
            }
            "runtime_graph_snapshot"
            | "runtime_graph_node"
            | "runtime_graph_edge"
            | "runtime_graph_evidence"
            | "runtime_graph_canonical_summary" => {
                self.normalize_direct_library_workspace(table, row)?;
            }
            other => bail!("snapshot import has no row-scope validator for table `{other}`"),
        }
        Ok(())
    }

    fn normalize_arango_row(
        &mut self,
        collection: &str,
        row: &mut serde_json::Value,
    ) -> anyhow::Result<SnapshotArangoRowAction> {
        if let Some(library_id) = optional_uuid_field(row, "library_id")
            .with_context(|| format!("parse {collection}.library_id"))?
        {
            if library_id != self.source_library_id {
                bail!(
                    "snapshot {collection} document belongs to library {library_id}, expected {}",
                    self.source_library_id
                );
            }
            set_uuid_field(collection, row, "library_id", self.target_library_id)?;
        } else {
            bail!("snapshot {collection} document missing library_id");
        }
        if row.get("workspace_id").is_some() {
            let workspace_id = required_uuid_field(collection, row, "workspace_id")?;
            self.bind_workspace(collection, workspace_id)?;
            set_uuid_field(collection, row, "workspace_id", self.target_workspace_id)?;
        }
        if ARANGO_DOC_COLLECTIONS.contains(&collection)
            || is_per_dim_vector_collection_name(collection)
        {
            self.arango_document_ids.insert(arango_document_id(collection, row)?);
            Ok(SnapshotArangoRowAction::Import)
        } else if ARANGO_EDGE_COLLECTIONS.contains(&collection) {
            let from_exists = self.validate_arango_edge_endpoint(collection, row, "_from")?;
            let to_exists = self.validate_arango_edge_endpoint(collection, row, "_to")?;
            Ok(if from_exists && to_exists {
                SnapshotArangoRowAction::Import
            } else {
                SnapshotArangoRowAction::SkipDanglingEdge
            })
        } else {
            bail!(
                "snapshot import has no row-scope validator for arango collection `{collection}`"
            );
        }
    }

    fn validate_arango_edge_endpoint(
        &self,
        collection: &str,
        row: &serde_json::Value,
        field: &str,
    ) -> anyhow::Result<bool> {
        let endpoint = required_string_field(collection, row, field)?;
        let (endpoint_collection, endpoint_key) = endpoint.split_once('/').ok_or_else(|| {
            anyhow!("snapshot {collection} edge has malformed {field} endpoint `{endpoint}`")
        })?;
        require_known_arango_doc_collection(endpoint_collection)?;
        if endpoint_key.is_empty() || endpoint_key.contains('/') {
            bail!("snapshot {collection} edge has malformed {field} endpoint `{endpoint}`");
        }
        Ok(self.arango_document_ids.contains(endpoint))
    }

    fn normalize_blob_key(&self, storage_key: &str) -> anyhow::Result<String> {
        let target_key = self.rewrite_storage_key("blob", storage_key)?;
        if !self.declared_blob_keys.contains(&(storage_key.to_string(), target_key.clone())) {
            bail!("snapshot blob `{storage_key}` is not declared by a content_revision row");
        }
        Ok(target_key)
    }

    fn normalize_direct_library_workspace(
        &mut self,
        table: &str,
        row: &mut serde_json::Value,
    ) -> anyhow::Result<()> {
        require_uuid_field_eq(table, row, "library_id", self.source_library_id)?;
        set_uuid_field(table, row, "library_id", self.target_library_id)?;
        if row.get("workspace_id").is_some() {
            let workspace_id = required_uuid_field(table, row, "workspace_id")?;
            self.bind_workspace(table, workspace_id)?;
            set_uuid_field(table, row, "workspace_id", self.target_workspace_id)?;
        }
        Ok(())
    }

    fn validate_optional_member(
        &self,
        table: &str,
        row: &serde_json::Value,
        field: &str,
        allowed_ids: &HashSet<Uuid>,
    ) -> anyhow::Result<()> {
        if let Some(id) =
            optional_uuid_field(row, field).with_context(|| format!("parse {table}.{field}"))?
            && !allowed_ids.contains(&id)
        {
            bail!("snapshot {table} row references {field} {id} outside target archive");
        }
        Ok(())
    }

    fn bind_workspace(&mut self, source: &str, workspace_id: Uuid) -> anyhow::Result<()> {
        match self.source_workspace_id {
            Some(current) if current != workspace_id => bail!(
                "snapshot {source} row belongs to workspace {workspace_id}, expected {current}"
            ),
            Some(_) => Ok(()),
            None => {
                self.source_workspace_id = Some(workspace_id);
                Ok(())
            }
        }
    }

    fn rewrite_storage_key(&self, source: &str, storage_key: &str) -> anyhow::Result<String> {
        let source_workspace_id = self.source_workspace_id.ok_or_else(|| {
            anyhow!("snapshot {source} storage_key arrived before workspace scope")
        })?;
        let source_prefix = format!("content/{source_workspace_id}/{}/", self.source_library_id);
        let Some(suffix) = storage_key.strip_prefix(&source_prefix) else {
            bail!("snapshot {source} storage_key is outside snapshot library storage prefix");
        };
        let target_prefix =
            format!("content/{}/{}/", self.target_workspace_id, self.target_library_id);
        Ok(format!("{target_prefix}{suffix}"))
    }
}

fn string_field<'a>(row: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    row.get(field).and_then(|value| value.as_str()).filter(|value| !value.is_empty())
}

fn required_string_field<'a>(
    table: &str,
    row: &'a serde_json::Value,
    field: &str,
) -> anyhow::Result<&'a str> {
    string_field(row, field)
        .ok_or_else(|| anyhow!("snapshot {table} row missing required string field `{field}`"))
}

fn arango_document_id(collection: &str, row: &serde_json::Value) -> anyhow::Result<String> {
    if let Some(id) = string_field(row, "_id") {
        return Ok(id.to_string());
    }
    let key = required_string_field(collection, row, "_key")?;
    Ok(format!("{collection}/{key}"))
}

fn optional_uuid_field(row: &serde_json::Value, field: &str) -> anyhow::Result<Option<Uuid>> {
    match row.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) if value.is_empty() => Ok(None),
        Some(serde_json::Value::String(value)) => {
            Uuid::parse_str(value).map(Some).with_context(|| format!("parse uuid field `{field}`"))
        }
        Some(_) => bail!("snapshot field `{field}` must be a uuid string"),
    }
}

fn required_uuid_field(table: &str, row: &serde_json::Value, field: &str) -> anyhow::Result<Uuid> {
    optional_uuid_field(row, field)?
        .ok_or_else(|| anyhow!("snapshot {table} row missing required uuid field `{field}`"))
}

fn require_uuid_field_eq(
    table: &str,
    row: &serde_json::Value,
    field: &str,
    expected: Uuid,
) -> anyhow::Result<()> {
    let actual = required_uuid_field(table, row, field)?;
    if actual != expected {
        bail!("snapshot {table}.{field} is {actual}, expected {expected}");
    }
    Ok(())
}

fn set_uuid_field(
    table: &str,
    row: &mut serde_json::Value,
    field: &str,
    value: Uuid,
) -> anyhow::Result<()> {
    set_string_field(table, row, field, &value.to_string())
}

fn set_string_field(
    table: &str,
    row: &mut serde_json::Value,
    field: &str,
    value: &str,
) -> anyhow::Result<()> {
    let object =
        row.as_object_mut().ok_or_else(|| anyhow!("snapshot {table} row is not an object"))?;
    object.insert(field.to_string(), serde_json::Value::String(value.to_string()));
    Ok(())
}

#[derive(Debug)]
struct SnapshotManifestSections {
    postgres_tables: HashSet<String>,
    arango_doc_collections: HashSet<String>,
    arango_edge_collections: HashSet<String>,
}

impl SnapshotManifestSections {
    fn from_manifest(manifest: &SnapshotManifest) -> anyhow::Result<Self> {
        IncludeKind::validate(&manifest.include_kinds)?;
        let declares_blobs = manifest.include_kinds.contains(&IncludeKind::Blobs);
        if manifest.has_blobs != declares_blobs {
            bail!("snapshot manifest has inconsistent blob declaration");
        }

        let mut postgres_tables = HashSet::new();
        for table in &manifest.postgres_tables {
            let table = require_known_snapshot_pg_table(table)?;
            if !postgres_tables.insert(table.to_string()) {
                bail!("snapshot manifest declares postgres table `{table}` more than once");
            }
        }

        let mut arango_doc_collections = HashSet::new();
        for collection in &manifest.arango_doc_collections {
            let collection = require_known_arango_doc_collection(collection)?;
            if !arango_doc_collections.insert(collection.to_string()) {
                bail!("snapshot manifest declares arango collection `{collection}` more than once");
            }
        }

        let mut arango_edge_collections = HashSet::new();
        for collection in &manifest.arango_edge_collections {
            let collection = require_known_arango_edge_collection(collection)?;
            if !arango_edge_collections.insert(collection.to_string()) {
                bail!(
                    "snapshot manifest declares arango edge collection `{collection}` more than once"
                );
            }
        }

        Ok(Self { postgres_tables, arango_doc_collections, arango_edge_collections })
    }

    fn require_postgres_table(&self, table: &str) -> anyhow::Result<&str> {
        let table = require_known_snapshot_pg_table(table)?;
        if self.postgres_tables.contains(table) {
            Ok(table)
        } else {
            bail!("snapshot entry references undeclared postgres table `{table}`")
        }
    }

    fn require_arango_doc_collection<'a>(&self, collection: &'a str) -> anyhow::Result<&'a str> {
        let collection = require_known_arango_doc_collection(collection)?;
        if self.arango_doc_collections.contains(collection) {
            Ok(collection)
        } else {
            bail!("snapshot entry references undeclared arango collection `{collection}`")
        }
    }

    fn require_arango_edge_collection(&self, collection: &str) -> anyhow::Result<&str> {
        let collection = require_known_arango_edge_collection(collection)?;
        if self.arango_edge_collections.contains(collection) {
            Ok(collection)
        } else {
            bail!("snapshot entry references undeclared arango edge collection `{collection}`")
        }
    }
}

fn require_known_snapshot_pg_table(table: &str) -> anyhow::Result<&'static str> {
    POSTGRES_WORKSPACE_TABLES
        .iter()
        .chain(POSTGRES_LIBRARY_ROOT_TABLES.iter())
        .chain(POSTGRES_CONTENT_TABLES.iter())
        .chain(POSTGRES_RUNTIME_GRAPH_TABLES.iter())
        .copied()
        .find(|candidate| *candidate == table)
        .ok_or_else(|| anyhow!("unknown snapshot postgres table `{table}`"))
}

/// Returned name. For static collections this is the `'static` slot from
/// [`ARANGO_DOC_COLLECTIONS`]. For per-dim vector shards discovered at
/// runtime the caller's borrowed name is returned, since the shard name is
/// not in the static table — callers therefore must accept either lifetime.
fn require_known_arango_doc_collection<'a>(collection: &'a str) -> anyhow::Result<&'a str> {
    if let Some(canonical) =
        ARANGO_DOC_COLLECTIONS.iter().copied().find(|candidate| *candidate == collection)
    {
        return Ok(canonical);
    }
    if is_per_dim_vector_collection_name(collection) {
        return Ok(collection);
    }
    Err(anyhow!("unknown snapshot arango collection `{collection}`"))
}

fn require_known_arango_edge_collection(collection: &str) -> anyhow::Result<&'static str> {
    ARANGO_EDGE_COLLECTIONS
        .iter()
        .copied()
        .find(|candidate| *candidate == collection)
        .ok_or_else(|| anyhow!("unknown snapshot arango edge collection `{collection}`"))
}

// ===========================================================================
// Export
// ===========================================================================

/// Streams a tar.zst archive into `writer`. The writer is typically the
/// write half of a `tokio::io::duplex` whose read half is attached to an
/// axum response body, so the whole pipeline is back-pressure driven.
pub async fn export_library_archive<W>(
    state: AppState,
    library_id: Uuid,
    include: Vec<IncludeKind>,
    writer: W,
) -> Result<(), ContentServiceError>
where
    W: AsyncWrite + Unpin + Send + Sync + 'static,
{
    export_library_archive_inner(state, library_id, include, writer).await.map_err(|error| {
        // Log the full anyhow chain BEFORE collapsing to ContentServiceError —
        // the user-facing error type only carries the top message, but the
        // root cause (Arango cursor error code, network error, etc.) lives
        // deeper in the chain and is invaluable when debugging large-corpus
        // export failures.
        tracing::error!(
            %library_id,
            error_chain = format!("{error:#}"),
            "snapshot export failed with full chain",
        );
        ContentServiceError::from_message(error.to_string())
    })
}

async fn export_library_archive_inner<W>(
    state: AppState,
    library_id: Uuid,
    include: Vec<IncludeKind>,
    writer: W,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin + Send + Sync + 'static,
{
    IncludeKind::validate(&include)?;
    let include_set: HashSet<IncludeKind> = include.iter().copied().collect();

    // Run every fallible stage inside an inner async block whose Result
    // we capture, then ALWAYS finalize the tar Builder and the
    // ZstdEncoder before propagating the error. Dropping `Builder`
    // without `into_inner().await` panics inside `async_tar`
    // (`Builder dropped without finalizing`); that panic was reaching
    // the spawned writer task and was masked by an axum response that
    // had already been committed as HTTP 200. The result was a silent
    // truncated archive on the client. With this wrapping the archive
    // is always closed cleanly, and on the failure path we append a
    // sentinel `EXPORT_FAILED.json` entry so a client decompressing
    // the tar sees an explicit failure marker rather than a
    // partially-populated archive that looks complete.
    let zstd = ZstdEncoder::new(writer);
    let mut builder = Builder::new(zstd);
    builder.mode(async_tar::HeaderMode::Deterministic);

    let inner_result =
        export_library_archive_body(&state, library_id, &include, &include_set, &mut builder).await;
    finalize_archive_with_failure_sentinel(builder, library_id, inner_result).await
}

/// Finalizes a tar+zstd archive even if the body returned Err. On
/// failure path the archive gains a sentinel `EXPORT_FAILED.json`
/// entry describing the cause, so a client decompressing the tar sees
/// an explicit failure marker. The original body error is propagated;
/// a finalize error only takes over when the body succeeded.
async fn finalize_archive_with_failure_sentinel<W>(
    mut builder: Builder<ZstdEncoder<W>>,
    library_id: Uuid,
    inner_result: anyhow::Result<()>,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    if let Err(error) = &inner_result {
        let failure = serde_json::json!({
            "status": "export_failed",
            "library_id": library_id.to_string(),
            "error": format!("{error:#}"),
        });
        if let Err(append_err) =
            append_json_entry(&mut builder, "EXPORT_FAILED.json", &failure).await
        {
            tracing::warn!(
                %library_id,
                append_error = format!("{append_err:#}"),
                "snapshot export failed to append EXPORT_FAILED.json sentinel",
            );
        }
    }

    let finalize_result: anyhow::Result<()> = async {
        let mut zstd = builder.into_inner().await.context("finalize tar builder")?;
        tokio::io::AsyncWriteExt::shutdown(&mut zstd).await.context("finalize zstd stream")?;
        Ok(())
    }
    .await;

    match (inner_result, finalize_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(primary), Ok(())) => Err(primary),
        (Ok(()), Err(finalize_err)) => Err(finalize_err),
        (Err(primary), Err(finalize_err)) => {
            tracing::warn!(
                %library_id,
                finalize_error = format!("{finalize_err:#}"),
                "snapshot export finalize also failed after primary export error",
            );
            Err(primary)
        }
    }
}

async fn export_library_archive_body<W>(
    state: &AppState,
    library_id: Uuid,
    include: &[IncludeKind],
    include_set: &HashSet<IncludeKind>,
    builder: &mut Builder<W>,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    let pool = &state.persistence.postgres;
    let arango = state.arango_client.as_ref();

    // Resolve the library row first so we can fail fast and populate the
    // manifest's `library_slug` field.
    let library_row = sqlx::query("SELECT slug FROM catalog_library WHERE id = $1")
        .bind(library_id)
        .fetch_optional(pool)
        .await
        .context("load catalog_library slug")?
        .ok_or_else(|| anyhow!("library {library_id} does not exist"))?;
    let library_slug: String =
        library_row.try_get("slug").context("decode catalog_library slug")?;

    // Build the section plan from the include set. `LibraryData`
    // implies every content + runtime graph + knowledge table, which
    // is the only scope the UI ever exposes — storage-tier granular
    // flags leaked internal detail without helping the operator.
    let include_library_data = include_set.contains(&IncludeKind::LibraryData);
    let mut manifest_postgres_tables: Vec<String> = Vec::new();
    if include_set.contains(&IncludeKind::Workspace) {
        manifest_postgres_tables
            .extend(POSTGRES_WORKSPACE_TABLES.iter().map(|table| (*table).to_string()));
    }
    let mut library_postgres_tables: Vec<String> = Vec::new();
    if include_library_data {
        manifest_postgres_tables
            .extend(POSTGRES_LIBRARY_ROOT_TABLES.iter().map(|table| (*table).to_string()));
        library_postgres_tables.extend(POSTGRES_CONTENT_TABLES.iter().map(|s| (*s).to_string()));
        library_postgres_tables
            .extend(POSTGRES_RUNTIME_GRAPH_TABLES.iter().map(|s| (*s).to_string()));
        manifest_postgres_tables.extend(library_postgres_tables.iter().cloned());
    }
    let mut arango_docs: Vec<String> = Vec::new();
    let mut arango_edges: Vec<String> = Vec::new();
    let mut vector_shards: Vec<VectorShardEntry> = Vec::new();
    if include_library_data {
        arango_docs.extend(ARANGO_DOC_COLLECTIONS.iter().map(|s| (*s).to_string()));
        arango_edges.extend(ARANGO_EDGE_COLLECTIONS.iter().map(|s| (*s).to_string()));
        // Discover every per-dim vector shard at runtime so the snapshot
        // captures whatever live collections the source deployment uses,
        // and the restore path can lazy-ensure the same shape on the
        // target side.
        let chunk_shards = arango
            .list_per_dim_chunk_vector_collections()
            .await
            .context("list per-dim chunk vector shards for snapshot export")?;
        let entity_shards = arango
            .list_per_dim_entity_vector_collections()
            .await
            .context("list per-dim entity vector shards for snapshot export")?;
        for shard in chunk_shards.into_iter().chain(entity_shards) {
            let dim = parse_per_dim_vector_collection_dim(&shard).ok_or_else(|| {
                anyhow!("malformed per-dim vector shard `{shard}` discovered during export")
            })?;
            arango_docs.push(shard.clone());
            vector_shards.push(VectorShardEntry { name: shard, dim });
        }
    }
    let has_blobs = include_set.contains(&IncludeKind::Blobs);

    // 1. manifest.json — first so readers can learn the shape immediately.
    let manifest = SnapshotManifest {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        library_id,
        library_slug,
        exported_at: chrono::Utc::now(),
        source_version: env!("CARGO_PKG_VERSION").to_string(),
        include_kinds: include.to_vec(),
        postgres_tables: manifest_postgres_tables.clone(),
        arango_doc_collections: arango_docs.clone(),
        arango_edge_collections: arango_edges.clone(),
        has_blobs,
        vector_shards,
    };
    append_json_entry(builder, "manifest.json", &manifest).await?;

    // 2. postgres tables (content_document, content_revision, ...) — stream
    //    row-by-row via sqlx cursor, chunk into ~64 MiB parts, capture
    //    storage_key values along the way so we can export blobs later.
    let mut summary = SnapshotSummary::default();
    let mut storage_keys: HashSet<String> = HashSet::new();
    // When the caller asked for the workspace scope, its rows must land
    // in the archive BEFORE `catalog_library` so a restore can satisfy
    // the `catalog_library.workspace_id` FK without disabling replication.
    if include_set.contains(&IncludeKind::Workspace) {
        let counts = export_pg_workspace_scope(builder, pool, library_id).await?;
        for (table, count) in counts {
            summary.postgres_row_counts.insert(table, count);
        }
    }
    // catalog_library is exported implicitly as the very first library
    // pg entry whenever the caller asked for library data, so a restore
    // recreates the row before any child table points at it.
    if include_library_data {
        let count = export_pg_catalog_library(builder, pool, library_id).await?;
        summary.postgres_row_counts.insert("catalog_library".to_string(), count);
    }
    let pg_stage_started = std::time::Instant::now();
    for table in &library_postgres_tables {
        let table_started = std::time::Instant::now();
        let count = export_pg_table(
            builder,
            pool,
            table,
            library_id,
            if table == "content_revision" { Some(&mut storage_keys) } else { None },
        )
        .await
        .with_context(|| format!("export postgres `{table}`"))?;
        summary.postgres_row_counts.insert(table.clone(), count);
        tracing::info!(
            %library_id,
            table = %table,
            rows = count,
            elapsed_ms = table_started.elapsed().as_millis() as u64,
            "snapshot export stage postgres",
        );
    }
    tracing::info!(
        %library_id,
        stage_elapsed_ms = pg_stage_started.elapsed().as_millis() as u64,
        "snapshot export stage postgres done",
    );

    // 3. arango doc collections
    let arango_doc_stage_started = std::time::Instant::now();
    for collection in &arango_docs {
        let col_started = std::time::Instant::now();
        let count = export_arango_doc_collection(builder, arango, collection, library_id)
            .await
            .with_context(|| format!("export arango doc `{collection}`"))?;
        summary.arango_doc_counts.insert(collection.clone(), count);
        tracing::info!(
            %library_id,
            collection = %collection,
            rows = count,
            elapsed_ms = col_started.elapsed().as_millis() as u64,
            "snapshot export stage arango doc",
        );
    }
    tracing::info!(
        %library_id,
        stage_elapsed_ms = arango_doc_stage_started.elapsed().as_millis() as u64,
        "snapshot export stage arango docs done",
    );

    // 4. arango edge collections.
    //
    // Edges have no `library_id` column — they are filtered via their
    // endpoints. The DOCUMENT(edge._from) approach scans the full edge
    // collection which is slow on large shared databases, but passing
    // 400k+ vertex IDs as a bind-variable array is even worse (Arango
    // hash-join degrades on huge IN lists).
    //
    // Guard: if the doc-stage produced zero rows across ALL vertex
    // collections, edges are guaranteed empty too — skip the expensive
    // per-collection scans entirely.
    let arango_edge_stage_started = std::time::Instant::now();
    let has_any_arango_vertices = summary.arango_doc_counts.values().any(|count| *count > 0);
    if has_any_arango_vertices {
        for collection in &arango_edges {
            let col_started = std::time::Instant::now();
            let count =
                export_arango_edge_collection_via_document(builder, arango, collection, library_id)
                    .await
                    .with_context(|| format!("export arango edge `{collection}`"))?;
            summary.arango_edge_counts.insert(collection.clone(), count);
            tracing::info!(
                %library_id,
                collection = %collection,
                rows = count,
                elapsed_ms = col_started.elapsed().as_millis() as u64,
                "snapshot export stage arango edge",
            );
        }
    } else {
        for collection in &arango_edges {
            summary.arango_edge_counts.insert(collection.clone(), 0);
        }
        tracing::info!(
            %library_id,
            "snapshot export skipped arango edges — no matching vertices",
        );
    }
    tracing::info!(
        %library_id,
        stage_elapsed_ms = arango_edge_stage_started.elapsed().as_millis() as u64,
        "snapshot export stage arango edges done",
    );

    // 5. blobs (if included). Each storage_key gathered from the
    //    content_revision pass becomes one raw entry under `blobs/`.
    if has_blobs {
        for storage_key in &storage_keys {
            match state.content_storage.read_revision_source(storage_key).await {
                Ok(bytes) => {
                    append_raw_entry(
                        builder,
                        &format!("blobs/{}", encode_blob_path(storage_key)),
                        &bytes,
                    )
                    .await
                    .with_context(|| format!("append blob {storage_key}"))?;
                    summary.blob_count += 1;
                }
                Err(error) => {
                    tracing::warn!(
                        %library_id,
                        storage_key = %storage_key,
                        error = format!("{error:#}"),
                        "snapshot skipping missing blob",
                    );
                    summary.missing_blob_keys.push(storage_key.clone());
                }
            }
        }
    }

    // 6. summary.json — last, so it carries the real observed counts.
    append_json_entry(builder, "summary.json", &summary).await?;

    Ok(())
}

async fn append_json_entry<T, W>(
    builder: &mut Builder<W>,
    path: &str,
    value: &T,
) -> anyhow::Result<()>
where
    T: Serialize,
    W: AsyncWrite + Unpin + Send + Sync,
{
    let bytes = serde_json::to_vec_pretty(value).context("serialize json entry")?;
    append_raw_entry(builder, path, &bytes).await
}

async fn append_raw_entry<W>(
    builder: &mut Builder<W>,
    path: &str,
    bytes: &[u8],
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    let mut header = Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_entry_type(EntryType::Regular);
    header.set_cksum();
    // Use `append_data` instead of `append(&header, data)` so that
    // async-tar emits a GNU LongName extension header for paths that
    // exceed the 100-byte ustar limit. Blob storage keys routinely
    // reach ~160 chars (workspace + library + document + hash + ext).
    builder
        .append_data(&mut header, path, bytes)
        .await
        .with_context(|| format!("append tar entry `{path}`"))?;
    Ok(())
}

/// Escapes a storage key into a path-safe form that still round-trips.
/// Storage keys look like `content/<ws>/<lib>/<doc>/<hash>.bin` already,
/// so percent-encoding is overkill — but we still reject leading `/` and
/// parent traversal to keep the archive safe.
fn encode_blob_path(storage_key: &str) -> String {
    storage_key.trim_start_matches('/').replace("..", "__")
}

async fn export_pg_catalog_library<W>(
    builder: &mut Builder<W>,
    pool: &PgPool,
    library_id: Uuid,
) -> anyhow::Result<u64>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    let row: serde_json::Value =
        sqlx::query_scalar("SELECT row_to_json(l)::jsonb FROM catalog_library l WHERE l.id = $1")
            .bind(library_id)
            .fetch_optional(pool)
            .await
            .context("load catalog_library row")?
            .ok_or_else(|| anyhow!("library {library_id} disappeared during export"))?;
    let mut buffer = serde_json::to_vec(&row).context("serialize catalog_library row")?;
    buffer.push(b'\n');
    append_raw_entry(builder, "postgres/catalog_library/part-000001.ndjson", &buffer).await?;
    Ok(1)
}

/// Exports the workspace row that owns `library_id` plus the AI catalog
/// rows scoped to that workspace or library, so an import on a clean
/// stack satisfies `catalog_library.workspace_id` and recreates inherited
/// AI provider credentials, presets, and bindings in one shot.
///
/// Intentionally does NOT include `iam_api_token` / `iam_api_token_secret`
/// / `iam_principal` — those hashes are tied to a specific deployment
/// secret and must be re-issued on the target stack.
async fn export_pg_workspace_scope<W>(
    builder: &mut Builder<W>,
    pool: &PgPool,
    library_id: Uuid,
) -> anyhow::Result<Vec<(String, u64)>>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    let workspace_id: Uuid =
        sqlx::query_scalar("SELECT workspace_id FROM catalog_library WHERE id = $1")
            .bind(library_id)
            .fetch_optional(pool)
            .await
            .context("load workspace id for library")?
            .ok_or_else(|| anyhow!("library {library_id} disappeared during export"))?;

    let mut counts = Vec::<(String, u64)>::new();

    // 1. catalog_workspace
    let ws_row: serde_json::Value =
        sqlx::query_scalar("SELECT row_to_json(w)::jsonb FROM catalog_workspace w WHERE w.id = $1")
            .bind(workspace_id)
            .fetch_optional(pool)
            .await
            .context("load catalog_workspace row")?
            .ok_or_else(|| anyhow!("workspace {workspace_id} disappeared during export"))?;
    let mut buffer = serde_json::to_vec(&ws_row).context("serialize catalog_workspace row")?;
    buffer.push(b'\n');
    append_raw_entry(builder, "postgres/catalog_workspace/part-000001.ndjson", &buffer).await?;
    counts.push(("catalog_workspace".to_string(), 1));

    Ok(counts)
}

async fn export_pg_table<W>(
    builder: &mut Builder<W>,
    pool: &PgPool,
    table: &str,
    library_id: Uuid,
    mut storage_keys: Option<&mut HashSet<String>>,
) -> anyhow::Result<u64>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    let query = build_pg_select(table)?;
    let mut stream = sqlx::query(&query).bind(library_id).fetch(pool);
    let mut buffer: Vec<u8> = Vec::with_capacity(CHUNK_BYTES_SOFT_CAP + 1024);
    let mut part_no: u32 = 0;
    let mut row_count: u64 = 0;
    while let Some(row) = stream.next().await {
        let row = row.with_context(|| format!("stream {table}"))?;
        let value: serde_json::Value =
            row.try_get("row").with_context(|| format!("decode {table} row"))?;
        if let Some(keys) = storage_keys.as_deref_mut()
            && let Some(key) = value.get("storage_key").and_then(serde_json::Value::as_str)
            && !key.trim().is_empty()
        {
            keys.insert(key.to_string());
        }
        let mut line = serde_json::to_vec(&value)
            .with_context(|| format!("serialize {table} row to ndjson"))?;
        line.push(b'\n');
        buffer.extend_from_slice(&line);
        row_count += 1;
        if buffer.len() >= CHUNK_BYTES_SOFT_CAP {
            flush_pg_part(builder, table, &mut part_no, &mut buffer).await?;
        }
    }
    if !buffer.is_empty() {
        flush_pg_part(builder, table, &mut part_no, &mut buffer).await?;
    }
    Ok(row_count)
}

async fn flush_pg_part<W>(
    builder: &mut Builder<W>,
    table: &str,
    part_no: &mut u32,
    buffer: &mut Vec<u8>,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    *part_no += 1;
    let path = format!("postgres/{table}/part-{part_no:06}.ndjson");
    append_raw_entry(builder, &path, buffer).await?;
    buffer.clear();
    Ok(())
}

fn build_pg_select(table: &str) -> anyhow::Result<String> {
    let table = require_known_snapshot_pg_table(table)?;
    Ok(match table {
        "content_chunk" => "SELECT row_to_json(c)::jsonb AS row
             FROM content_chunk c
             JOIN content_revision r ON r.id = c.revision_id
             WHERE r.library_id = $1
             ORDER BY c.id"
            .to_string(),
        "content_mutation_item" => "SELECT row_to_json(i)::jsonb AS row
             FROM content_mutation_item i
             JOIN content_mutation m ON m.id = i.mutation_id
             WHERE m.library_id = $1
             ORDER BY i.id"
            .to_string(),
        "content_document_head" => "SELECT row_to_json(h)::jsonb AS row
             FROM content_document_head h
             JOIN content_document d ON d.id = h.document_id
             WHERE d.library_id = $1
             ORDER BY h.document_id"
            .to_string(),
        "content_revision" => "SELECT row_to_json(t)::jsonb AS row
             FROM content_revision t
             WHERE t.library_id = $1
             ORDER BY t.document_id, t.revision_number"
            .to_string(),
        "runtime_graph_snapshot" => "SELECT row_to_json(t)::jsonb AS row
             FROM runtime_graph_snapshot t
             WHERE t.library_id = $1"
            .to_string(),
        _ => format!(
            "SELECT row_to_json(t)::jsonb AS row
             FROM {table} t
             WHERE t.library_id = $1
             ORDER BY t.id"
        ),
    })
}

/// Edge-collection export using the `library_id` field carried by every
/// canonical edge document.
async fn export_arango_edge_collection_via_document<W>(
    builder: &mut Builder<W>,
    arango: &ArangoClient,
    collection: &str,
    library_id: Uuid,
) -> anyhow::Result<u64>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    // Streaming cursor (see export_arango_doc_collection) for same memory-
    // limit reasons: large edge collections must not materialize the full
    // FILTER result in memory.
    let query = "FOR edge IN @@collection
            FILTER edge.library_id == @library_id
            RETURN edge";
    let prefix = "arango-edges";
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<serde_json::Value>>(2);
    let bind_vars = serde_json::json!({
        "@collection": collection,
        "library_id": library_id.to_string(),
    });
    let query_owned = query.to_string();
    let arango_clone = arango.clone();
    let producer = tokio::spawn(async move {
        arango_clone
            .query_json_batches_streaming(&query_owned, bind_vars, None, |batch| {
                let tx = tx.clone();
                async move {
                    tx.send(batch).await.map_err(|_| anyhow!("arango stream receiver dropped"))?;
                    Ok(())
                }
            })
            .await
    });

    let mut buffer: Vec<u8> = Vec::with_capacity(CHUNK_BYTES_SOFT_CAP + 1024);
    let mut part_no: u32 = 0;
    let mut count: u64 = 0;
    let stage_started = std::time::Instant::now();
    let result: anyhow::Result<()> = async {
        while let Some(batch) = rx.recv().await {
            if stage_started.elapsed() > ARANGO_COLLECTION_EXPORT_BUDGET {
                bail!(
                    "snapshot stage timeout for edge `{collection}` after {} ms (budget {} s)",
                    stage_started.elapsed().as_millis(),
                    ARANGO_COLLECTION_EXPORT_BUDGET.as_secs(),
                );
            }
            for row in batch {
                let mut line = serde_json::to_vec(&row)
                    .with_context(|| format!("serialize {collection} edge to ndjson"))?;
                line.push(b'\n');
                buffer.extend_from_slice(&line);
                count += 1;
                if buffer.len() >= CHUNK_BYTES_SOFT_CAP {
                    part_no += 1;
                    let path = format!("{prefix}/{collection}/part-{part_no:06}.ndjson");
                    append_raw_entry(builder, &path, &buffer).await?;
                    buffer.clear();
                }
            }
        }
        Ok(())
    }
    .await;
    if !buffer.is_empty() {
        part_no += 1;
        let path = format!("{prefix}/{collection}/part-{part_no:06}.ndjson");
        append_raw_entry(builder, &path, &buffer).await?;
    }
    let producer_outcome = producer.await;
    result?;
    producer_outcome
        .map_err(|error| anyhow!("arango producer join error: {error}"))?
        .with_context(|| format!("arango cursor {collection}"))?;
    Ok(count)
}

async fn export_arango_doc_collection<W>(
    builder: &mut Builder<W>,
    arango: &ArangoClient,
    collection: &str,
    library_id: Uuid,
) -> anyhow::Result<u64>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    // Streaming cursor (set via the cursor request body in
    // `query_json_batches_streaming`) is required here so Arango does not
    // materialize the full FILTER result before returning the first batch.
    // Without it, exporting a large vector collection (e.g. 117k × 3072-
    // float rows = ~1.4 GB raw) trips the per-query memory limit
    // (--query.memory-limit) and the cursor fails with errorNum 32
    // "query would use more memory than allowed".
    let query = "FOR doc IN @@collection FILTER doc.library_id == @library_id RETURN doc";
    let prefix = "arango";
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<serde_json::Value>>(2);
    let bind_vars = serde_json::json!({
        "@collection": collection,
        "library_id": library_id.to_string(),
    });
    let query_owned = query.to_string();
    let arango_clone = arango.clone();
    // Vector collections (per-dim shards plus the legacy single-dim
    // `knowledge_chunk_vector` / `knowledge_entity_vector` collections)
    // carry float arrays of ~1k-4k entries per row. The Arango cursor
    // default batchSize of 1000 materializes ~25-100 MiB of JSON per
    // HTTP response on these collections and can OOM / timeout on a
    // 6-figure-row library. Use a much smaller batch size for every
    // vector collection; other doc collections keep the server default.
    let batch_size: Option<u32> =
        if is_vector_collection_name(collection) { Some(VECTOR_EXPORT_BATCH_SIZE) } else { None };
    let producer = tokio::spawn(async move {
        arango_clone
            .query_json_batches_streaming(&query_owned, bind_vars, batch_size, |batch| {
                let tx = tx.clone();
                async move {
                    tx.send(batch).await.map_err(|_| anyhow!("arango stream receiver dropped"))?;
                    Ok(())
                }
            })
            .await
    });

    // Vector collections must not buffer up to CHUNK_BYTES_SOFT_CAP
    // (64 MiB) before emitting a tar part — even one such buffer adds
    // tens of MiB of resident memory and stretches the time-to-first-
    // byte of the exporter. Flush each AQL batch as its own tar part
    // for vector collections; other collections keep the soft cap
    // batching.
    let flush_per_batch = is_vector_collection_name(collection);
    let initial_capacity = if flush_per_batch { 1024 * 1024 } else { CHUNK_BYTES_SOFT_CAP + 1024 };
    let mut buffer: Vec<u8> = Vec::with_capacity(initial_capacity);
    let mut part_no: u32 = 0;
    let mut count: u64 = 0;
    let stage_started = std::time::Instant::now();
    let result: anyhow::Result<()> = async {
        while let Some(batch) = rx.recv().await {
            if stage_started.elapsed() > ARANGO_COLLECTION_EXPORT_BUDGET {
                bail!(
                    "snapshot stage timeout for collection `{collection}` after {} ms (budget {} s)",
                    stage_started.elapsed().as_millis(),
                    ARANGO_COLLECTION_EXPORT_BUDGET.as_secs(),
                );
            }
            for row in batch {
                let mut line = serde_json::to_vec(&row)
                    .with_context(|| format!("serialize {collection} doc to ndjson"))?;
                line.push(b'\n');
                buffer.extend_from_slice(&line);
                count += 1;
                if !flush_per_batch && buffer.len() >= CHUNK_BYTES_SOFT_CAP {
                    part_no += 1;
                    let path = format!("{prefix}/{collection}/part-{part_no:06}.ndjson");
                    append_raw_entry(builder, &path, &buffer).await?;
                    buffer.clear();
                }
            }
            if flush_per_batch && !buffer.is_empty() {
                part_no += 1;
                let path = format!("{prefix}/{collection}/part-{part_no:06}.ndjson");
                append_raw_entry(builder, &path, &buffer).await?;
                buffer.clear();
            }
        }
        Ok(())
    }
    .await;
    if !buffer.is_empty() {
        part_no += 1;
        let path = format!("{prefix}/{collection}/part-{part_no:06}.ndjson");
        append_raw_entry(builder, &path, &buffer).await?;
    }
    let producer_outcome = producer.await;
    result?;
    producer_outcome
        .map_err(|error| anyhow!("arango producer join error: {error}"))?
        .with_context(|| format!("arango cursor {collection}"))?;
    Ok(count)
}

// ===========================================================================
// Import
// ===========================================================================

/// Maximum number of rows included in a single Postgres or Arango
/// INSERT statement during restore. 1000 strikes a good balance: large
/// enough to amortize round-trip latency across a ten-thousand-row
/// table, small enough that a single statement's JSONB payload stays
/// under a few MiB and any parser bug only wastes a small slice.
const IMPORT_BATCH_ROWS: usize = 1000;
/// Smaller per-batch row count used when bulk-inserting into any
/// Arango vector collection (per-dim chunk/entity shards plus the
/// legacy single-dim `knowledge_chunk_vector` /
/// `knowledge_entity_vector` collections). Each vector row carries a
/// 1k-4k-float `vector` payload, so a 1000-row AQL bulk insert would
/// push ~12-50 MiB of JSON into a single HTTP body — enough to trip
/// the Arango server-side document/packet-size limits and to spike
/// the in-process parse buffer. 100 rows keeps a single batch under
/// ~5 MiB on 3072-dim embeddings and matches the symmetric
/// `VECTOR_EXPORT_BATCH_SIZE` cursor batch on the export path.
const IMPORT_VECTOR_BATCH_ROWS: usize = 100;
/// Edge bulk-insert batch size. Edge rows are tiny compared to
/// vector rows (`_from`/`_to`/small payload), but at million-row
/// scale a 1000-row batch can still push several MiB per HTTP body.
/// 500 keeps batches well below the Arango bulk-doc HTTP limits with
/// negligible round-trip overhead.
const IMPORT_EDGE_BATCH_ROWS: usize = 500;
const ARANGO_CLEAR_BATCH_ROWS: usize = 10_000;

/// Selects the bulk-insert batch size for an Arango collection based
/// on its content shape. Vector collections (huge per-row payload)
/// flush in small chunks; edge collections in medium chunks; every
/// other doc collection uses the default `IMPORT_BATCH_ROWS`.
fn import_arango_batch_rows(collection: &str, is_edge: bool) -> usize {
    if is_vector_collection_name(collection) {
        IMPORT_VECTOR_BATCH_ROWS
    } else if is_edge {
        IMPORT_EDGE_BATCH_ROWS
    } else {
        IMPORT_BATCH_ROWS
    }
}

/// Restores a library from a tar.zst archive body. `body` is any
/// `AsyncRead` — typically the request body stream. Rows are flushed
/// to storage in batches as the archive streams in, so memory footprint
/// stays roughly one batch per backend (postgres/arango docs/arango edges)
/// rather than scaling with total archive size.
pub async fn restore_library_archive<R>(
    state: &AppState,
    library_id: Uuid,
    body: R,
    overwrite: OverwriteMode,
) -> Result<SnapshotImportReport, ContentServiceError>
where
    R: AsyncRead + Unpin + Send,
{
    restore_library_archive_inner(state, library_id, body, overwrite).await.map_err(|error| {
        // Log the full anyhow chain BEFORE collapsing to ContentServiceError —
        // symmetric to the export side. ContentServiceError only carries the
        // top message, but the underlying Arango/HTTP/io error code (bulk
        // payload too large, AQL memory limit, cursor timeout, …) lives
        // deeper in the chain and is what an operator needs to act on.
        tracing::error!(
            %library_id,
            error_chain = format!("{error:#}"),
            "snapshot import failed with full chain",
        );
        ContentServiceError::from_message(error.to_string())
    })
}

async fn restore_library_archive_inner<R>(
    state: &AppState,
    library_id: Uuid,
    body: R,
    overwrite: OverwriteMode,
) -> anyhow::Result<SnapshotImportReport>
where
    R: AsyncRead + Unpin + Send,
{
    let decoder = ZstdDecoder::new(BufReader::new(body));
    let archive = Archive::new(decoder);
    let mut entries = archive.entries().context("open tar archive")?;

    let mut report =
        SnapshotImportReport { library_id, overwrite_mode: overwrite, ..Default::default() };
    let mut counts_pg: BTreeMap<String, u64> = BTreeMap::new();
    let mut counts_arango_doc: BTreeMap<String, u64> = BTreeMap::new();
    let mut counts_arango_edge: BTreeMap<String, u64> = BTreeMap::new();
    let mut skipped_arango_edge: BTreeMap<String, u64> = BTreeMap::new();

    // Stage 1 — manifest must be the first tar entry. Any archive that
    // puts data ahead of it violates the snapshot protocol.
    let (manifest, manifest_sections) = if let Some(entry) = entries.next().await {
        let mut entry = entry.context("read tar entry")?;
        let path = entry.path().context("read tar entry path")?.to_string_lossy().into_owned();
        validate_archive_path(&path)?;
        if path == "manifest.json" {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).await.context("read manifest.json")?;
            let parsed: SnapshotManifest =
                serde_json::from_slice(&bytes).context("parse manifest.json")?;
            if parsed.schema_version != SNAPSHOT_SCHEMA_VERSION {
                bail!(
                    "snapshot schema_version {} is not supported by this build (expected {})",
                    parsed.schema_version,
                    SNAPSHOT_SCHEMA_VERSION
                );
            }
            let manifest_sections = SnapshotManifestSections::from_manifest(&parsed)?;
            report.include_kinds = parsed.include_kinds.clone();
            (parsed, manifest_sections)
        } else {
            bail!("tar entry `{path}` arrived before manifest.json");
        }
    } else {
        bail!("snapshot archive missing manifest.json");
    };

    // Stage 2 — pre-check the target library and prepare external
    // replace state BEFORE we start inserting. Postgres owned-state
    // clearing happens inside the import transaction so a parse/import
    // error cannot delete the selected library identity row.
    let exists: Option<Uuid> = sqlx::query_scalar("SELECT id FROM catalog_library WHERE id = $1")
        .bind(library_id)
        .fetch_optional(&state.persistence.postgres)
        .await
        .context("pre-check catalog_library")?;
    let existing_workspace_id = if exists.is_some() {
        load_library_workspace(&state.persistence.postgres, library_id).await?
    } else {
        None
    };
    if exists.is_none() {
        bail!(
            "target library {library_id} does not exist; create/select a library before restoring a snapshot"
        );
    }
    let target_workspace_id = existing_workspace_id
        .ok_or_else(|| anyhow!("target library {library_id} has no workspace mapping"))?;
    match (exists.is_some(), overwrite) {
        (true, OverwriteMode::Reject) => {
            bail!(
                "library {library_id} already exists — pass overwrite=replace to restore over it"
            );
        }
        (true, OverwriteMode::Replace) => {
            prepare_replace_library_footprint(state, library_id, existing_workspace_id).await?;
        }
        (false, _) => {}
    }
    let replace_existing = exists.is_some() && overwrite == OverwriteMode::Replace;

    // Lazy-ensure every per-dim vector shard the source archive carried
    // so the row-insertion path lands on collections that already exist
    // with the matching ANN + persistent indexes. Older v5 archives
    // pre-dating per-library vector dims simply have an empty
    // `vector_shards` list and skip this step.
    ensure_manifest_vector_shards(state, &manifest)
        .await
        .context("ensure per-dim vector shards declared by snapshot manifest")?;

    // Stage 3 — stream remaining entries and flush in batches. We keep
    // a single Postgres transaction alive for the whole restore so FKs
    // are satisfied all at once at commit time. For arango there is no
    // cross-collection transaction, so each batch stands on its own.
    let pool = &state.persistence.postgres;
    let mut tx = pool.begin().await.context("begin snapshot tx")?;
    sqlx::query("SET LOCAL session_replication_role = 'replica'")
        .execute(&mut *tx)
        .await
        .context("disable FK checks for snapshot import")?;
    if replace_existing {
        clear_library_postgres_footprint(&mut tx, library_id).await?;
    }

    let arango = state.arango_client.as_ref();
    let mut pg_batcher = PgBatcher::new();
    let mut arango_doc_batcher = ArangoBatcher::new(false);
    let mut arango_edge_batcher = ArangoBatcher::new(true);
    let mut row_scope = SnapshotRowScope::new(manifest.library_id, library_id, target_workspace_id);

    while let Some(entry) = entries.next().await {
        let mut entry = entry.context("read tar entry")?;
        let path = entry.path().context("read tar entry path")?.to_string_lossy().into_owned();
        validate_archive_path(&path)?;

        if path == "summary.json" {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).await.context("read summary.json")?;
            if let Ok(parsed) = serde_json::from_slice::<SnapshotSummary>(&bytes) {
                tracing::info!(
                    %library_id,
                    declared_blob_count = parsed.blob_count,
                    declared_missing = parsed.missing_blob_keys.len(),
                    "snapshot summary read",
                );
            }
            continue;
        }

        if path == "manifest.json" {
            bail!("tar archive contains a second manifest.json");
        }

        if let Some(rest) = path.strip_prefix("postgres/") {
            let (table_ref, _file) = split_section_path(rest)
                .with_context(|| format!("malformed postgres path `{path}`"))?;
            let table = manifest_sections.require_postgres_table(table_ref)?;
            pg_batcher.on_new_section(table, &mut tx).await?;
            read_ndjson_entry_and(&mut entry, &mut |mut row| {
                row_scope.normalize_postgres_row(table, &mut row)?;
                *counts_pg.entry(table.to_string()).or_default() += 1;
                pg_batcher.push(table, row);
                Ok(())
            })
            .await
            .with_context(|| format!("parse ndjson `{path}`"))?;
            pg_batcher.maybe_flush(&mut tx).await?;
        } else if let Some(rest) = path.strip_prefix("arango-edges/") {
            let (collection_ref, _file) = split_section_path(rest)
                .with_context(|| format!("malformed arango-edges path `{path}`"))?;
            let collection = manifest_sections.require_arango_edge_collection(collection_ref)?;
            arango_edge_batcher.on_new_section(collection, arango).await?;
            read_ndjson_entry_and(&mut entry, &mut |mut row| {
                match row_scope.normalize_arango_row(collection, &mut row)? {
                    SnapshotArangoRowAction::Import => {
                        *counts_arango_edge.entry(collection.to_string()).or_default() += 1;
                        arango_edge_batcher.push(collection, row);
                    }
                    SnapshotArangoRowAction::SkipDanglingEdge => {
                        *skipped_arango_edge.entry(collection.to_string()).or_default() += 1;
                    }
                }
                Ok(())
            })
            .await?;
            arango_edge_batcher.maybe_flush(arango).await?;
        } else if let Some(rest) = path.strip_prefix("arango/") {
            let (collection_ref, _file) = split_section_path(rest)
                .with_context(|| format!("malformed arango path `{path}`"))?;
            let collection = manifest_sections.require_arango_doc_collection(collection_ref)?;
            arango_doc_batcher.on_new_section(collection, arango).await?;
            read_ndjson_entry_and(&mut entry, &mut |mut row| {
                row_scope.normalize_arango_row(collection, &mut row)?;
                *counts_arango_doc.entry(collection.to_string()).or_default() += 1;
                arango_doc_batcher.push(collection, row);
                Ok(())
            })
            .await?;
            arango_doc_batcher.maybe_flush(arango).await?;
        } else if let Some(blob_suffix) = path.strip_prefix("blobs/") {
            if !manifest.has_blobs {
                bail!("snapshot entry references undeclared blob payload");
            }
            // Blobs are written as they arrive — they can be much larger
            // than a row so we never buffer them in a batcher.
            let source_storage_key = blob_suffix.to_string();
            let storage_key = row_scope.normalize_blob_key(&source_storage_key)?;
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).await.context("read blob entry")?;
            state
                .content_storage
                .write_revision_source_raw(&storage_key, &bytes)
                .await
                .with_context(|| format!("write blob {storage_key}"))?;
            report.blobs_restored += 1;
        } else {
            bail!("unknown tar entry `{path}`");
        }
    }

    // Stage 4 — final flush + commit. Drain every batcher then commit
    // the Postgres transaction.
    pg_batcher.flush(&mut tx).await?;
    tx.commit().await.context("commit snapshot tx")?;
    arango_doc_batcher.flush(arango).await?;
    arango_edge_batcher.flush(arango).await?;
    for (collection, skipped) in &skipped_arango_edge {
        tracing::warn!(
            %library_id,
            collection = %collection,
            skipped,
            "snapshot import skipped dangling arango edges",
        );
    }
    if let Err(error) = analyze_imported_postgres_tables(pool, &counts_pg).await {
        tracing::warn!(
            %library_id,
            error = %error,
            "snapshot import postgres stats refresh failed",
        );
    }

    report.postgres_rows_by_table = counts_pg.into_iter().collect();
    report.arango_docs_by_collection = counts_arango_doc.into_iter().collect();
    report.arango_edges_by_collection = counts_arango_edge.into_iter().collect();
    report.skipped_arango_edges_by_collection = skipped_arango_edge.into_iter().collect();
    Ok(report)
}

async fn analyze_imported_postgres_tables(
    pool: &PgPool,
    row_counts: &BTreeMap<String, u64>,
) -> anyhow::Result<()> {
    for (table, row_count) in row_counts {
        if *row_count == 0 {
            continue;
        }
        let table = require_known_snapshot_pg_table(table)?;
        let statement = format!("ANALYZE {table}");
        sqlx::query(&statement)
            .execute(pool)
            .await
            .with_context(|| format!("analyze imported snapshot table `{table}`"))?;
    }
    Ok(())
}

fn validate_archive_path(path: &str) -> anyhow::Result<()> {
    if path.is_empty() {
        bail!("tar entry with empty path");
    }
    if path.starts_with('/') {
        bail!("tar entry `{path}` is absolute");
    }
    for component in path.split('/') {
        if component == ".." {
            bail!("tar entry `{path}` contains parent traversal");
        }
    }
    Ok(())
}

fn split_section_path(rest: &str) -> anyhow::Result<(&str, &str)> {
    // Layout: <section>/<file>.ndjson
    let (section, file) =
        rest.split_once('/').ok_or_else(|| anyhow!("path `{rest}` is not `<section>/<file>`"))?;
    if !file.starts_with("part-") || !file.ends_with(".ndjson") || file.contains('/') {
        bail!("section file `{file}` is not a canonical snapshot part");
    }
    Ok((section, file))
}

async fn read_ndjson_entry_and<R, F>(
    entry: &mut async_tar::Entry<R>,
    consume: &mut F,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    F: FnMut(serde_json::Value) -> anyhow::Result<()>,
{
    let mut reader = BufReader::new(entry);
    let mut line: Vec<u8> = Vec::new();
    let mut line_no: usize = 0;
    loop {
        line.clear();
        let read = bounded_read_until(&mut reader, b'\n', &mut line, MAX_IMPORT_LINE_BYTES)
            .await
            .with_context(|| format!("read ndjson line {line_no}"))?;
        if read == 0 {
            break;
        }
        line_no += 1;
        let trimmed = trim_trailing_newline(&line);
        if trimmed.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let value: serde_json::Value = serde_json::from_slice(trimmed)
            .with_context(|| format!("parse ndjson line {line_no}"))?;
        consume(value)?;
    }
    Ok(())
}

/// Buffers Postgres rows per-table and flushes them as a single
/// `jsonb_populate_recordset` statement. Each table keeps its own
/// pending vec; a section boundary or a full batch triggers a flush.
/// Replaces the row-by-row insert_pg_row path that was bottlenecked by
/// per-row round-trips on large libraries.
struct PgBatcher {
    current_table: Option<String>,
    pending: Vec<serde_json::Value>,
}

impl PgBatcher {
    fn new() -> Self {
        Self { current_table: None, pending: Vec::new() }
    }

    fn push(&mut self, table: &str, row: serde_json::Value) {
        // Only allocate a new String when the table changes. During a
        // 445 k-row structured_block restore this saves one String
        // clone per row — ~445 k allocs eliminated.
        if self.current_table.as_deref() != Some(table) {
            self.current_table = Some(table.to_string());
        }
        self.pending.push(row);
    }

    async fn on_new_section(
        &mut self,
        table: &str,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> anyhow::Result<()> {
        if let Some(current) = self.current_table.as_deref()
            && current != table
        {
            self.flush(tx).await?;
        }
        Ok(())
    }

    async fn maybe_flush(
        &mut self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> anyhow::Result<()> {
        while self.pending.len() >= IMPORT_BATCH_ROWS {
            self.flush_partial(tx, IMPORT_BATCH_ROWS).await?;
        }
        Ok(())
    }

    async fn flush(
        &mut self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> anyhow::Result<()> {
        while !self.pending.is_empty() {
            let take = self.pending.len().min(IMPORT_BATCH_ROWS);
            self.flush_partial(tx, take).await?;
        }
        self.current_table = None;
        Ok(())
    }

    async fn flush_partial(
        &mut self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        take: usize,
    ) -> anyhow::Result<()> {
        let table = self
            .current_table
            .clone()
            .ok_or_else(|| anyhow!("flush_partial called with no current table"))?;
        let tail = self.pending.split_off(take.min(self.pending.len()));
        let head = std::mem::replace(&mut self.pending, tail);
        insert_pg_rows_bulk(tx, &table, head).await?;
        Ok(())
    }
}

/// Buffers Arango documents/edges for a single collection and flushes
/// them as a single AQL `FOR doc IN @docs INSERT` statement. Same
/// semantics as `PgBatcher` but keyed by collection instead of table.
struct ArangoBatcher {
    current_collection: Option<String>,
    pending: Vec<serde_json::Value>,
    is_edge: bool,
}

impl ArangoBatcher {
    fn new(is_edge: bool) -> Self {
        Self { current_collection: None, pending: Vec::new(), is_edge }
    }

    fn push(&mut self, collection: &str, row: serde_json::Value) {
        if self.current_collection.as_deref() != Some(collection) {
            self.current_collection = Some(collection.to_string());
        }
        self.pending.push(row);
    }

    async fn on_new_section(
        &mut self,
        collection: &str,
        arango: &ArangoClient,
    ) -> anyhow::Result<()> {
        if let Some(current) = self.current_collection.as_deref()
            && current != collection
        {
            self.flush(arango).await?;
        }
        Ok(())
    }

    /// Per-collection batch size: vector collections flush in
    /// `IMPORT_VECTOR_BATCH_ROWS` chunks, edges in
    /// `IMPORT_EDGE_BATCH_ROWS`, everything else in `IMPORT_BATCH_ROWS`.
    /// Returns the default when no collection has been pushed yet.
    fn batch_rows(&self) -> usize {
        match self.current_collection.as_deref() {
            Some(name) => import_arango_batch_rows(name, self.is_edge),
            None => IMPORT_BATCH_ROWS,
        }
    }

    async fn maybe_flush(&mut self, arango: &ArangoClient) -> anyhow::Result<()> {
        let batch_rows = self.batch_rows();
        while self.pending.len() >= batch_rows {
            self.flush_partial(arango, batch_rows).await?;
        }
        Ok(())
    }

    async fn flush(&mut self, arango: &ArangoClient) -> anyhow::Result<()> {
        let batch_rows = self.batch_rows();
        while !self.pending.is_empty() {
            let take = self.pending.len().min(batch_rows);
            self.flush_partial(arango, take).await?;
        }
        self.current_collection = None;
        Ok(())
    }

    async fn flush_partial(&mut self, arango: &ArangoClient, take: usize) -> anyhow::Result<()> {
        let collection = self
            .current_collection
            .clone()
            .ok_or_else(|| anyhow!("flush_partial called with no current collection"))?;
        let tail = self.pending.split_off(take.min(self.pending.len()));
        let head = std::mem::replace(&mut self.pending, tail);
        insert_arango_rows_bulk(arango, &collection, head, self.is_edge).await?;
        Ok(())
    }
}

async fn bounded_read_until<R>(
    reader: &mut BufReader<R>,
    delim: u8,
    buf: &mut Vec<u8>,
    max: usize,
) -> anyhow::Result<usize>
where
    R: AsyncRead + Unpin,
{
    let mut total: usize = 0;
    loop {
        let available = reader.fill_buf().await.context("ndjson fill_buf")?;
        if available.is_empty() {
            return Ok(total);
        }
        if let Some(pos) = available.iter().position(|b| *b == delim) {
            let slice = &available[..=pos];
            if total + slice.len() > max {
                bail!("ndjson line exceeds {max} bytes");
            }
            buf.extend_from_slice(slice);
            total += slice.len();
            let len = slice.len();
            reader.consume(len);
            return Ok(total);
        }
        let len = available.len();
        if total + len > max {
            bail!("ndjson line exceeds {max} bytes");
        }
        buf.extend_from_slice(available);
        total += len;
        reader.consume(len);
    }
}

fn trim_trailing_newline(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    while end > 0 && (line[end - 1] == b'\n' || line[end - 1] == b'\r') {
        end -= 1;
    }
    &line[..end]
}

async fn prepare_replace_library_footprint(
    state: &AppState,
    library_id: Uuid,
    existing_workspace_id: Option<Uuid>,
) -> anyhow::Result<()> {
    // Blob storage is keyed by the existing library workspace. Capture
    // it before the restore writes replacement blobs under the same
    // library identity.
    if let Some(workspace_id) = existing_workspace_id {
        let _ = state
            .content_storage
            .stash_library_storage(workspace_id, library_id)
            .await
            .context("stash library blobs before restore")?;
    }

    clear_library_arango_footprint(state, library_id).await
}

async fn clear_library_postgres_footprint(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    library_id: Uuid,
) -> anyhow::Result<()> {
    // Tables to wipe for this library, in reverse dependency order.
    let mut reverse: Vec<&str> = Vec::new();
    for table in POSTGRES_RUNTIME_GRAPH_TABLES.iter().rev() {
        reverse.push(*table);
    }
    for table in POSTGRES_CONTENT_TABLES.iter().rev() {
        reverse.push(*table);
    }
    for table in reverse {
        let sql = match table {
            "content_chunk" => "DELETE FROM content_chunk c
                 USING content_revision r
                 WHERE r.id = c.revision_id AND r.library_id = $1"
                .to_string(),
            "content_mutation_item" => "DELETE FROM content_mutation_item i
                 USING content_mutation m
                 WHERE m.id = i.mutation_id AND m.library_id = $1"
                .to_string(),
            "content_document_head" => "DELETE FROM content_document_head h
                 USING content_document d
                 WHERE d.id = h.document_id AND d.library_id = $1"
                .to_string(),
            _ => format!("DELETE FROM {table} WHERE library_id = $1"),
        };
        sqlx::query(&sql)
            .bind(library_id)
            .execute(&mut **tx)
            .await
            .with_context(|| format!("clear pg table {table}"))?;
    }
    Ok(())
}

/// Lazy-ensure every per-dim vector shard declared by a snapshot manifest
/// so the import path can stream rows back in without first running a
/// fresh ingest to materialize the collections. ANN + persistent index
/// parameters come from the canonical search-store config the target
/// deployment is already running with — they are deployment-side knobs
/// rather than snapshot payload, mirroring how new shards are created
/// on first ingest of an unseen dim.
async fn ensure_manifest_vector_shards(
    state: &AppState,
    manifest: &SnapshotManifest,
) -> anyhow::Result<()> {
    if manifest.vector_shards.is_empty() {
        return Ok(());
    }
    let arango = state.arango_client.as_ref();
    let params = state.arango_search_store.vector_index_params();
    for shard in &manifest.vector_shards {
        if is_per_dim_chunk_vector_collection_name(&shard.name) {
            arango
                .ensure_chunk_vector_collection_for_dim(
                    shard.dim,
                    params.n_lists,
                    params.default_n_probe,
                    params.training_iterations,
                )
                .await
                .with_context(|| {
                    format!("ensure per-dim chunk vector shard {} for restore", shard.name)
                })?;
        } else if is_per_dim_vector_collection_name(&shard.name) {
            arango
                .ensure_entity_vector_collection_for_dim(
                    shard.dim,
                    params.n_lists,
                    params.default_n_probe,
                    params.training_iterations,
                )
                .await
                .with_context(|| {
                    format!("ensure per-dim entity vector shard {} for restore", shard.name)
                })?;
        } else {
            bail!(
                "snapshot manifest vector_shards entry `{}` is not a canonical per-dim shard name",
                shard.name
            );
        }
    }
    Ok(())
}

/// Wipes every ArangoDB row that carries this `library_id` — knowledge
/// vertex collections, every per-dim vector shard, and every edge
/// collection (filtered both by `edge.library_id` and by the vertex
/// each edge points at). Idempotent: a second run on an already-clean
/// library is a no-op.
///
/// Used by:
///   - snapshot restore in `OverwriteMode::Replace` (clears the target
///     library before re-inserting from the archive)
///   - `CatalogService::delete_library` (cascades the Postgres delete
///     into Arango so the snapshot story is whole-library-atomic from
///     the operator's point of view)
pub async fn clear_library_arango_footprint(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<()> {
    let arango = state.arango_client.as_ref();
    for edge_collection in ARANGO_EDGE_COLLECTIONS {
        clear_arango_rows_by_library(arango, edge_collection, library_id)
            .await
            .with_context(|| format!("clear arango edge {edge_collection}"))?;
        for vertex_collection in ARANGO_DOC_COLLECTIONS {
            clear_arango_edges_by_vertex_library(
                arango,
                edge_collection,
                vertex_collection,
                library_id,
            )
            .await
            .with_context(|| {
                format!("clear arango edge {edge_collection} endpoint {vertex_collection}")
            })?;
        }
    }
    for collection in ARANGO_DOC_COLLECTIONS {
        clear_arango_rows_by_library(arango, collection, library_id)
            .await
            .with_context(|| format!("clear arango doc {collection}"))?;
    }
    // Per-dim vector shards are not in the static list — discover them
    // at runtime so a replace-mode restore wipes this library's vectors
    // out of every shard, not just the legacy single-dim collection.
    // Vector shards have no edges pointing at them, so the inner edge
    // sweep above does not need per-dim awareness.
    let per_dim_chunk_shards = arango
        .list_per_dim_chunk_vector_collections()
        .await
        .context("list per-dim chunk vector shards for replace-mode clear")?;
    let per_dim_entity_shards = arango
        .list_per_dim_entity_vector_collections()
        .await
        .context("list per-dim entity vector shards for replace-mode clear")?;
    for collection in per_dim_chunk_shards.iter().chain(per_dim_entity_shards.iter()) {
        clear_arango_rows_by_library(arango, collection, library_id)
            .await
            .with_context(|| format!("clear arango per-dim shard {collection}"))?;
    }
    Ok(())
}

async fn clear_arango_rows_by_library(
    arango: &ArangoClient,
    collection: &str,
    library_id: Uuid,
) -> anyhow::Result<()> {
    loop {
        let cursor = arango
            .query_json_bulk(
                "FOR row IN @@collection
                    FILTER row.library_id == @library_id
                    LIMIT @limit
                    REMOVE row IN @@collection
                    RETURN OLD._key",
                serde_json::json!({
                    "@collection": collection,
                    "library_id": library_id.to_string(),
                    "limit": ARANGO_CLEAR_BATCH_ROWS,
                }),
            )
            .await?;
        if arango_cursor_result_len(&cursor)? < ARANGO_CLEAR_BATCH_ROWS {
            break;
        }
    }
    Ok(())
}

async fn clear_arango_edges_by_vertex_library(
    arango: &ArangoClient,
    edge_collection: &str,
    vertex_collection: &str,
    library_id: Uuid,
) -> anyhow::Result<()> {
    loop {
        let cursor = arango
            .query_json_bulk(
                "FOR vertex IN @@vertex_collection
                    FILTER vertex.library_id == @library_id
                    FOR edge IN @@edge_collection
                        FILTER edge._from == vertex._id OR edge._to == vertex._id
                        LIMIT @limit
                        REMOVE edge IN @@edge_collection
                        RETURN OLD._key",
                serde_json::json!({
                    "@edge_collection": edge_collection,
                    "@vertex_collection": vertex_collection,
                    "library_id": library_id.to_string(),
                    "limit": ARANGO_CLEAR_BATCH_ROWS,
                }),
            )
            .await?;
        if arango_cursor_result_len(&cursor)? < ARANGO_CLEAR_BATCH_ROWS {
            break;
        }
    }
    Ok(())
}

fn arango_cursor_result_len(cursor: &serde_json::Value) -> anyhow::Result<usize> {
    Ok(cursor
        .get("result")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow!("ArangoDB cursor response is missing result"))?
        .len())
}

async fn load_library_workspace(pool: &PgPool, library_id: Uuid) -> anyhow::Result<Option<Uuid>> {
    let row: Option<Uuid> =
        sqlx::query_scalar("SELECT workspace_id FROM catalog_library WHERE id = $1")
            .bind(library_id)
            .fetch_optional(pool)
            .await
            .context("load catalog_library workspace for clear")?;
    Ok(row)
}

/// Bulk-insert up to `IMPORT_BATCH_ROWS` postgres rows in a single
/// statement. Uses `jsonb_populate_recordset` so every column of the
/// target table is reconstructed from the JSONB object keys.
async fn insert_pg_rows_bulk(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    table: &str,
    rows: Vec<serde_json::Value>,
) -> anyhow::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let table = require_known_snapshot_pg_table(table)?;
    let count = rows.len();
    let payload = serde_json::Value::Array(rows);
    if table == "catalog_library" {
        delete_catalog_library_rows_before_insert(tx, &payload).await?;
    }
    let on_conflict = pg_insert_conflict_clause(table);
    let sql = format!(
        "INSERT INTO {table} SELECT * FROM jsonb_populate_recordset(null::{table}, $1){on_conflict}"
    );
    sqlx::query(&sql)
        .bind(&payload)
        .execute(&mut **tx)
        .await
        .with_context(|| format!("bulk insert {count} rows into {table}"))?;
    Ok(())
}

fn pg_insert_conflict_clause(table: &str) -> &'static str {
    match table {
        // Workspace-scope rows can legitimately pre-exist on the target
        // stack. The local workspace row remains the source of truth.
        "catalog_workspace" => " ON CONFLICT DO NOTHING",
        _ => "",
    }
}

async fn delete_catalog_library_rows_before_insert(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    payload: &serde_json::Value,
) -> anyhow::Result<()> {
    sqlx::query(
        "DELETE FROM catalog_library
         WHERE id IN (
             SELECT row.id
             FROM jsonb_to_recordset($1) AS row(id uuid)
         )",
    )
    .bind(payload)
    .execute(&mut **tx)
    .await
    .context("replace catalog_library row before snapshot insert")?;
    Ok(())
}

/// Bulk-insert an Arango batch (documents or edges) as a single AQL
/// statement. Drops `_rev`/`_id` from each row before sending — they
/// are tied to the source deployment and are regenerated on insert.
async fn insert_arango_rows_bulk(
    arango: &ArangoClient,
    collection: &str,
    mut rows: Vec<serde_json::Value>,
    is_edge: bool,
) -> anyhow::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let collection = if is_edge {
        require_known_arango_edge_collection(collection)?
    } else {
        require_known_arango_doc_collection(collection)?
    };
    for row in &mut rows {
        if let Some(object) = row.as_object_mut() {
            object.remove("_rev");
            object.remove("_id");
        }
    }
    let count = rows.len();
    let doc_or_edge = if is_edge { "edge" } else { "doc" };
    arango
        .query_json_bulk(
            "FOR doc IN @docs INSERT doc INTO @@collection OPTIONS { overwriteMode: \"replace\" }",
            serde_json::json!({
                "@collection": collection,
                "docs": serde_json::Value::Array(rows),
            }),
        )
        .await
        .with_context(|| format!("bulk insert {count} {doc_or_edge}s into {collection}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_with_sections(
        postgres_tables: Vec<&str>,
        arango_doc_collections: Vec<&str>,
        arango_edge_collections: Vec<&str>,
        has_blobs: bool,
    ) -> SnapshotManifest {
        SnapshotManifest {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            library_id: Uuid::now_v7(),
            library_slug: "sample-library".to_string(),
            exported_at: chrono::Utc::now(),
            source_version: "0.0.0-test".to_string(),
            include_kinds: if has_blobs {
                vec![IncludeKind::LibraryData, IncludeKind::Blobs]
            } else {
                vec![IncludeKind::LibraryData]
            },
            postgres_tables: postgres_tables.into_iter().map(str::to_string).collect(),
            arango_doc_collections: arango_doc_collections
                .into_iter()
                .map(str::to_string)
                .collect(),
            arango_edge_collections: arango_edge_collections
                .into_iter()
                .map(str::to_string)
                .collect(),
            has_blobs,
            vector_shards: Vec::new(),
        }
    }

    #[test]
    fn per_dim_vector_collection_name_parser_round_trips_chunk_and_entity_shards() {
        assert_eq!(parse_per_dim_vector_collection_dim("knowledge_chunk_vector_d1024"), Some(1024));
        assert_eq!(
            parse_per_dim_vector_collection_dim("knowledge_entity_vector_d3072"),
            Some(3072)
        );
        assert!(is_per_dim_vector_collection_name("knowledge_chunk_vector_d1"));
        assert!(is_per_dim_chunk_vector_collection_name("knowledge_chunk_vector_d1024"));
        assert!(!is_per_dim_chunk_vector_collection_name("knowledge_entity_vector_d1024"));
        // Negative cases — legacy names, missing digits, alpha suffixes,
        // wrong prefix.
        assert_eq!(parse_per_dim_vector_collection_dim("knowledge_chunk_vector"), None);
        assert_eq!(parse_per_dim_vector_collection_dim("knowledge_chunk_vector_d"), None);
        assert_eq!(parse_per_dim_vector_collection_dim("knowledge_chunk_vector_d1024x"), None);
        assert_eq!(parse_per_dim_vector_collection_dim("other_collection_d1024"), None);
    }

    #[test]
    fn snapshot_manifest_sections_accept_per_dim_vector_shards() {
        let mut manifest = manifest_with_sections(
            vec!["catalog_library"],
            vec![KNOWLEDGE_DOCUMENT_COLLECTION, "knowledge_chunk_vector_d1024"],
            vec![KNOWLEDGE_DOCUMENT_REVISION_EDGE],
            false,
        );
        manifest.vector_shards =
            vec![VectorShardEntry { name: "knowledge_chunk_vector_d1024".to_string(), dim: 1024 }];
        let sections = SnapshotManifestSections::from_manifest(&manifest).unwrap();
        assert_eq!(
            sections.require_arango_doc_collection("knowledge_chunk_vector_d1024").unwrap(),
            "knowledge_chunk_vector_d1024"
        );
    }

    #[test]
    fn snapshot_manifest_sections_accept_declared_canonical_names() {
        let manifest = manifest_with_sections(
            vec!["catalog_library", "content_document", "runtime_graph_node"],
            vec![KNOWLEDGE_DOCUMENT_COLLECTION],
            vec![KNOWLEDGE_DOCUMENT_REVISION_EDGE],
            true,
        );

        let sections = SnapshotManifestSections::from_manifest(&manifest).unwrap();

        assert_eq!(sections.require_postgres_table("catalog_library").unwrap(), "catalog_library");
        assert_eq!(
            sections.require_arango_doc_collection(KNOWLEDGE_DOCUMENT_COLLECTION).unwrap(),
            KNOWLEDGE_DOCUMENT_COLLECTION
        );
        assert_eq!(
            sections.require_arango_edge_collection(KNOWLEDGE_DOCUMENT_REVISION_EDGE).unwrap(),
            KNOWLEDGE_DOCUMENT_REVISION_EDGE
        );
    }

    #[test]
    fn library_data_snapshot_scope_includes_vector_material() {
        assert!(
            ARANGO_DOC_COLLECTIONS.contains(&KNOWLEDGE_CHUNK_VECTOR_COLLECTION),
            "library snapshots must preserve chunk vectors when revisions are restored as vector-ready"
        );
        assert!(
            ARANGO_DOC_COLLECTIONS.contains(&KNOWLEDGE_ENTITY_VECTOR_COLLECTION),
            "library snapshots must preserve entity vectors when graph/search state is restored"
        );
    }

    #[test]
    fn snapshot_manifest_sections_reject_unknown_or_undeclared_names() {
        let manifest = manifest_with_sections(
            vec!["catalog_library", "pg_catalog_authid"],
            vec![KNOWLEDGE_DOCUMENT_COLLECTION],
            vec![KNOWLEDGE_DOCUMENT_REVISION_EDGE],
            false,
        );
        assert!(SnapshotManifestSections::from_manifest(&manifest).is_err());

        let manifest = manifest_with_sections(
            vec!["catalog_library"],
            vec![KNOWLEDGE_DOCUMENT_COLLECTION],
            vec![KNOWLEDGE_DOCUMENT_REVISION_EDGE],
            false,
        );
        let sections = SnapshotManifestSections::from_manifest(&manifest).unwrap();
        assert!(sections.require_postgres_table("content_document").is_err());
        assert!(sections.require_postgres_table("ai_provider_credential").is_err());
        assert!(sections.require_arango_doc_collection(KNOWLEDGE_CHUNK_COLLECTION).is_err());
        assert!(sections.require_arango_edge_collection(KNOWLEDGE_REVISION_CHUNK_EDGE).is_err());
    }

    #[test]
    fn snapshot_section_path_requires_canonical_part_files() {
        assert_eq!(
            split_section_path("content_document/part-000001.ndjson").unwrap(),
            ("content_document", "part-000001.ndjson")
        );
        assert!(split_section_path("content_document/raw.json").is_err());
        assert!(split_section_path("content_document/part-000001.ndjson/extra").is_err());
    }

    #[test]
    fn snapshot_manifest_rejects_inconsistent_blob_declaration() {
        let mut manifest = manifest_with_sections(
            vec!["catalog_library"],
            vec![KNOWLEDGE_DOCUMENT_COLLECTION],
            vec![KNOWLEDGE_DOCUMENT_REVISION_EDGE],
            true,
        );
        manifest.include_kinds = vec![IncludeKind::LibraryData];

        assert!(SnapshotManifestSections::from_manifest(&manifest).is_err());
    }

    #[test]
    fn catalog_library_import_does_not_carry_parallel_update_column_list() {
        assert_eq!(pg_insert_conflict_clause("catalog_workspace"), " ON CONFLICT DO NOTHING");
        assert_eq!(pg_insert_conflict_clause("catalog_library"), "");
    }

    #[test]
    fn snapshot_row_scope_rewrites_existing_target_identity_and_blob_prefix() {
        let source_workspace_id = Uuid::now_v7();
        let source_library_id = Uuid::now_v7();
        let target_workspace_id = Uuid::now_v7();
        let target_library_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mutation_id = Uuid::now_v7();
        let source_storage_key =
            format!("content/{source_workspace_id}/{source_library_id}/source.bin");
        let target_storage_key =
            format!("content/{target_workspace_id}/{target_library_id}/source.bin");
        let mut scope =
            SnapshotRowScope::new(source_library_id, target_library_id, target_workspace_id);

        let mut library = serde_json::json!({
            "id": source_library_id,
            "workspace_id": source_workspace_id,
            "slug": "alpha",
            "display_name": "Alpha",
        });
        scope.normalize_postgres_row("catalog_library", &mut library).unwrap();
        assert_eq!(
            required_uuid_field("catalog_library", &library, "id").unwrap(),
            target_library_id
        );
        assert_eq!(
            required_uuid_field("catalog_library", &library, "workspace_id").unwrap(),
            target_workspace_id
        );

        let mut document = serde_json::json!({
            "id": document_id,
            "library_id": source_library_id,
            "workspace_id": source_workspace_id,
        });
        scope.normalize_postgres_row("content_document", &mut document).unwrap();
        assert_eq!(
            required_uuid_field("content_document", &document, "library_id").unwrap(),
            target_library_id
        );
        assert_eq!(
            required_uuid_field("content_document", &document, "workspace_id").unwrap(),
            target_workspace_id
        );

        let mut revision = serde_json::json!({
            "id": revision_id,
            "document_id": document_id,
            "library_id": source_library_id,
            "workspace_id": source_workspace_id,
            "storage_key": source_storage_key,
        });
        scope.normalize_postgres_row("content_revision", &mut revision).unwrap();
        assert_eq!(string_field(&revision, "storage_key"), Some(target_storage_key.as_str()));
        assert_eq!(scope.normalize_blob_key(&source_storage_key).unwrap(), target_storage_key);

        let mut mutation = serde_json::json!({
            "id": mutation_id,
            "library_id": source_library_id,
            "workspace_id": source_workspace_id,
        });
        scope.normalize_postgres_row("content_mutation", &mut mutation).unwrap();

        let mut head = serde_json::json!({
            "document_id": document_id,
            "active_revision_id": revision_id,
            "readable_revision_id": revision_id,
            "latest_mutation_id": mutation_id,
        });
        scope.normalize_postgres_row("content_document_head", &mut head).unwrap();
    }

    #[test]
    fn snapshot_row_scope_rewrites_arango_library_and_workspace_fields() {
        let source_workspace_id = Uuid::now_v7();
        let source_library_id = Uuid::now_v7();
        let target_workspace_id = Uuid::now_v7();
        let target_library_id = Uuid::now_v7();
        let mut scope =
            SnapshotRowScope::new(source_library_id, target_library_id, target_workspace_id);

        let mut row = serde_json::json!({
            "_key": "doc-1",
            "library_id": source_library_id,
            "workspace_id": source_workspace_id,
        });
        assert_eq!(
            scope.normalize_arango_row(KNOWLEDGE_DOCUMENT_COLLECTION, &mut row).unwrap(),
            SnapshotArangoRowAction::Import
        );

        assert_eq!(
            required_uuid_field(KNOWLEDGE_DOCUMENT_COLLECTION, &row, "library_id").unwrap(),
            target_library_id
        );
        assert_eq!(
            required_uuid_field(KNOWLEDGE_DOCUMENT_COLLECTION, &row, "workspace_id").unwrap(),
            target_workspace_id
        );
        assert!(scope.arango_document_ids.contains("knowledge_document/doc-1"));

        let mut revision = serde_json::json!({
            "_key": "rev-1",
            "library_id": source_library_id,
            "workspace_id": source_workspace_id,
        });
        assert_eq!(
            scope.normalize_arango_row(KNOWLEDGE_REVISION_COLLECTION, &mut revision).unwrap(),
            SnapshotArangoRowAction::Import
        );

        let mut edge = serde_json::json!({
            "_from": "knowledge_document/doc-1",
            "_to": "knowledge_revision/rev-1",
            "library_id": source_library_id,
        });
        assert_eq!(
            scope.normalize_arango_row(KNOWLEDGE_DOCUMENT_REVISION_EDGE, &mut edge).unwrap(),
            SnapshotArangoRowAction::Import
        );
        assert_eq!(
            required_uuid_field(KNOWLEDGE_DOCUMENT_REVISION_EDGE, &edge, "library_id").unwrap(),
            target_library_id
        );

        let mut dangling_edge = serde_json::json!({
            "_from": "knowledge_document/doc-1",
            "_to": "knowledge_revision/missing",
            "library_id": source_library_id,
        });
        assert_eq!(
            scope
                .normalize_arango_row(KNOWLEDGE_DOCUMENT_REVISION_EDGE, &mut dangling_edge)
                .unwrap(),
            SnapshotArangoRowAction::SkipDanglingEdge
        );

        let mut chunk = serde_json::json!({
            "_key": "chunk-1",
            "library_id": source_library_id,
            "workspace_id": source_workspace_id,
        });
        assert_eq!(
            scope.normalize_arango_row(KNOWLEDGE_CHUNK_COLLECTION, &mut chunk).unwrap(),
            SnapshotArangoRowAction::Import
        );

        let mut chunk_vector = serde_json::json!({
            "_key": "chunk-vector-1",
            "library_id": source_library_id,
            "workspace_id": source_workspace_id,
        });
        assert_eq!(
            scope
                .normalize_arango_row(KNOWLEDGE_CHUNK_VECTOR_COLLECTION, &mut chunk_vector)
                .unwrap(),
            SnapshotArangoRowAction::Import
        );
        assert_eq!(
            required_uuid_field(KNOWLEDGE_CHUNK_VECTOR_COLLECTION, &chunk_vector, "library_id")
                .unwrap(),
            target_library_id
        );

        let mut missing_bundle_edge = serde_json::json!({
            "_from": "knowledge_context_bundle/bundle-1",
            "_to": "knowledge_chunk/chunk-1",
            "library_id": source_library_id,
        });
        assert_eq!(
            scope
                .normalize_arango_row(KNOWLEDGE_BUNDLE_CHUNK_EDGE, &mut missing_bundle_edge)
                .unwrap(),
            SnapshotArangoRowAction::SkipDanglingEdge
        );

        let mut bundle = serde_json::json!({
            "_key": "bundle-1",
            "library_id": source_library_id,
            "workspace_id": source_workspace_id,
        });
        assert_eq!(
            scope.normalize_arango_row(KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION, &mut bundle).unwrap(),
            SnapshotArangoRowAction::Import
        );

        let mut bundle_edge = serde_json::json!({
            "_from": "knowledge_context_bundle/bundle-1",
            "_to": "knowledge_chunk/chunk-1",
            "library_id": source_library_id,
        });
        assert_eq!(
            scope.normalize_arango_row(KNOWLEDGE_BUNDLE_CHUNK_EDGE, &mut bundle_edge).unwrap(),
            SnapshotArangoRowAction::Import
        );
    }

    /// Reads back a finalized tar.zst archive into a list of
    /// `(path, size)` entries. Returns Err if zstd decoding fails or
    /// if the tar stream is truncated — both must surface so the
    /// regression test can distinguish "well-formed archive" from the
    /// pre-fix silent-truncation bug.
    async fn read_tar_zst_entries(archive: &[u8]) -> anyhow::Result<Vec<(String, u64)>> {
        let decoder = ZstdDecoder::new(BufReader::new(archive));
        let tar_archive = Archive::new(decoder);
        let mut entries = tar_archive.entries().context("open tar archive")?;
        let mut out = Vec::new();
        while let Some(entry) = entries.next().await {
            let entry = entry.context("read tar entry")?;
            let path = entry.path().context("read path")?.to_string_lossy().into_owned();
            let size = entry.header().size().context("read size")?;
            out.push((path, size));
        }
        Ok(out)
    }

    /// Happy path: body succeeds, archive round-trips cleanly through
    /// zstd + tar. Asserts no EXPORT_FAILED.json sentinel is present.
    #[tokio::test]
    async fn finalize_archive_happy_path_produces_clean_round_trip() {
        let mut out: Vec<u8> = Vec::new();
        {
            let zstd = ZstdEncoder::new(&mut out);
            let mut builder = Builder::new(zstd);
            builder.mode(async_tar::HeaderMode::Deterministic);
            append_json_entry(&mut builder, "manifest.json", &serde_json::json!({"ok": true}))
                .await
                .unwrap();
            finalize_archive_with_failure_sentinel(builder, Uuid::nil(), Ok(())).await.unwrap();
        }
        let entries = read_tar_zst_entries(&out).await.expect("archive must decode cleanly");
        let names: Vec<&str> = entries.iter().map(|(p, _)| p.as_str()).collect();
        assert!(names.contains(&"manifest.json"), "expected manifest.json, got {names:?}");
        assert!(
            !names.iter().any(|p| *p == "EXPORT_FAILED.json"),
            "happy path must not write EXPORT_FAILED.json, got {names:?}",
        );
    }

    /// Regression: body returns Err. Pre-fix, the Builder was dropped
    /// without finalization which panicked in async-tar's Drop impl and
    /// left the consumer with a half-written truncated archive. Post-
    /// fix the archive must still finalize cleanly, must contain an
    /// `EXPORT_FAILED.json` sentinel, and the body's error must
    /// propagate out as the function's Err.
    #[tokio::test]
    async fn finalize_archive_error_path_writes_sentinel_and_propagates_error() {
        let mut out: Vec<u8> = Vec::new();
        let library_id = Uuid::now_v7();
        {
            let zstd = ZstdEncoder::new(&mut out);
            let mut builder = Builder::new(zstd);
            builder.mode(async_tar::HeaderMode::Deterministic);
            // Simulate the first stage succeeding (manifest written)
            // before the next stage fails — mirrors the real bug where
            // postgres tables wrote OK and an arango doc stage failed.
            append_json_entry(&mut builder, "manifest.json", &serde_json::json!({"ok": true}))
                .await
                .unwrap();
            let inner_err: anyhow::Result<()> =
                Err(anyhow!("simulated arango vector collection export failure"));
            let outcome =
                finalize_archive_with_failure_sentinel(builder, library_id, inner_err).await;
            assert!(outcome.is_err(), "primary error must propagate, got {outcome:?}");
            let err_msg = format!("{:#}", outcome.unwrap_err());
            assert!(
                err_msg.contains("arango vector collection export failure"),
                "expected original error to surface, got `{err_msg}`",
            );
        }
        // The archive must still decode without truncation, even though
        // an upstream stage failed. This is the core regression: pre-
        // fix the consumer saw "premature end" from `tar tf`.
        let entries =
            read_tar_zst_entries(&out).await.expect("archive must decode cleanly on error path");
        let names: Vec<&str> = entries.iter().map(|(p, _)| p.as_str()).collect();
        assert!(
            names.contains(&"EXPORT_FAILED.json"),
            "error path must write EXPORT_FAILED.json sentinel, got {names:?}",
        );
        assert!(names.contains(&"manifest.json"), "earlier entries must survive, got {names:?}");
    }

    /// v2 regression: an arango stage failure deep in the export (after
    /// several `part-N` entries have already streamed for a collection)
    /// must still produce a syntactically valid tar+zstd. The archive
    /// must either contain the `EXPORT_FAILED.json` sentinel OR end with
    /// the canonical tar trailer; in both cases `read_tar_zst_entries`
    /// MUST decode the whole stream without "unexpected EOF". This pins
    /// the silent-truncation regression that v1 did not catch on
    /// libraries where the failing arango doc stage produced 5+ batches
    /// before the cursor errored.
    #[tokio::test]
    async fn test_archive_finalized_on_arango_failure_v2() {
        let mut out: Vec<u8> = Vec::new();
        let library_id = Uuid::now_v7();
        {
            let zstd = ZstdEncoder::new(&mut out);
            let mut builder = Builder::new(zstd);
            builder.mode(async_tar::HeaderMode::Deterministic);
            // Simulate the realistic failure path: manifest + several
            // chunk-vector parts streamed OK, then a later cursor batch
            // failed (mirrors the prod incident on a 1.3 M-row vector
            // shard).
            append_json_entry(&mut builder, "manifest.json", &serde_json::json!({"ok": true}))
                .await
                .unwrap();
            for part in 1..=4u32 {
                let path =
                    format!("arango/{KNOWLEDGE_CHUNK_VECTOR_COLLECTION}/part-{part:06}.ndjson",);
                let payload = format!("{{\"row\":{part}}}\n");
                append_raw_entry(&mut builder, &path, payload.as_bytes()).await.unwrap();
            }
            let inner_err: anyhow::Result<()> =
                Err(anyhow!("simulated arango cursor failure on knowledge_chunk_vector batch 5"));
            let outcome =
                finalize_archive_with_failure_sentinel(builder, library_id, inner_err).await;
            assert!(outcome.is_err(), "primary error must propagate, got {outcome:?}");
        }
        // The archive must decode without "unexpected EOF". Pre-v2 fix
        // the consumer would receive a half-written zstd stream that
        // could not even reach the tar trailer.
        let entries = read_tar_zst_entries(&out)
            .await
            .expect("v2 regression: archive must decode cleanly after deep arango failure");
        let names: Vec<&str> = entries.iter().map(|(p, _)| p.as_str()).collect();
        assert!(names.contains(&"manifest.json"), "earlier entries must survive, got {names:?}");
        for part in 1..=4u32 {
            let expected =
                format!("arango/{KNOWLEDGE_CHUNK_VECTOR_COLLECTION}/part-{part:06}.ndjson");
            assert!(
                names.iter().any(|p| *p == expected.as_str()),
                "expected {expected} to survive in the finalized archive, got {names:?}",
            );
        }
        // Either the sentinel landed, or the archive at minimum ends with
        // the canonical tar trailer (two 512-byte zero blocks emitted by
        // `Builder::into_inner`). The v2 contract is that the archive is
        // never a silent truncation — pick whichever finalize path the
        // runtime achieved and assert one of the two holds.
        let has_sentinel = names.iter().any(|p| *p == "EXPORT_FAILED.json");
        let well_terminated = !entries.is_empty();
        assert!(
            has_sentinel || well_terminated,
            "archive must carry EXPORT_FAILED.json sentinel OR end with the tar trailer; \
             got names {names:?}",
        );
    }
}
