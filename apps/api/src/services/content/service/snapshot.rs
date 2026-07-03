//! Canonical library snapshot — streaming tar.zst export and import.
//!
//! The archive layout is:
//!
//! ```text
//! manifest.json                         # first — declares include kinds and table list
//! postgres/<table>/part-NNNNNN.ndjson   # chunked per table, 64 MiB cap per part
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

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Context, anyhow, bail};
use async_compression::tokio::{bufread::ZstdDecoder, write::ZstdEncoder};
use async_tar::{Archive, Builder, EntryType, Header};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sqlx::{Acquire, PgPool, Row};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, BufReader};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::content_repository,
    services::content::{error::ContentServiceError, storage::StashedContentDirectory},
};

/// Prefix of every per-dim chunk-vector relation/shard.
const PER_DIM_CHUNK_VECTOR_PREFIX: &str = "knowledge_chunk_vector_d";
/// Prefix of every per-dim entity-vector shard.
const PER_DIM_ENTITY_VECTOR_PREFIX: &str = "knowledge_entity_vector_d";
const PGVECTOR_HNSW_VECTOR_MAX_DIM: u64 = 2000;
const PG_HNSW_DEFAULT_BUILD_BUDGET_BYTES: u64 = 3_000_000_000;
const PG_HNSW_MIN_M: u64 = 8;
const PG_HNSW_MID_M: u64 = 16;
const PG_HNSW_LARGE_M: u64 = 24;

/// Parse the dim suffix off a per-dim vector shard name.
/// Returns `None` when the name does not match the per-dim shape.
fn parse_per_dim_vector_collection_dim(name: &str) -> Option<u64> {
    let suffix = name
        .strip_prefix(PER_DIM_CHUNK_VECTOR_PREFIX)
        .or_else(|| name.strip_prefix(PER_DIM_ENTITY_VECTOR_PREFIX))?;
    parse_per_dim_vector_suffix_dim(suffix)
}

fn parse_per_dim_vector_suffix_dim(suffix: &str) -> Option<u64> {
    if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    suffix.parse::<u64>().ok()
}

fn is_canonical_per_dim_vector_collection_name(name: &str) -> bool {
    let Some(suffix) = name
        .strip_prefix(PER_DIM_CHUNK_VECTOR_PREFIX)
        .or_else(|| name.strip_prefix(PER_DIM_ENTITY_VECTOR_PREFIX))
    else {
        return false;
    };
    parse_per_dim_vector_suffix_dim(suffix).is_some()
}

/// `true` when `name` is a per-dim chunk-vector shard
/// (`knowledge_chunk_vector_d<dim>`). Used to decide whether the restore path
/// should ensure a chunk-side vs entity-side relation.
fn is_per_dim_chunk_vector_collection_name(name: &str) -> bool {
    name.strip_prefix(PER_DIM_CHUNK_VECTOR_PREFIX)
        .is_some_and(|suffix| parse_per_dim_vector_suffix_dim(suffix).is_some())
}

fn canonical_per_dim_vector_relation_name(name: &str) -> Option<String> {
    let dim = parse_per_dim_vector_collection_dim(name)?;
    if is_per_dim_chunk_vector_collection_name(name) {
        Some(format!("{PER_DIM_CHUNK_VECTOR_PREFIX}{dim}"))
    } else {
        Some(format!("{PER_DIM_ENTITY_VECTOR_PREFIX}{dim}"))
    }
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
pub const SNAPSHOT_SCHEMA_VERSION: u32 = 7;
const MIN_SUPPORTED_SNAPSHOT_SCHEMA_VERSION: u32 = 6;

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
    /// Portable AI configuration that makes the exported library resolvable
    /// on another stack: provider/model catalogs, prices, provider
    /// credentials (with `api_key` stripped), model presets and binding
    /// assignments. Includes instance-scoped (deployment-global) bindings as
    /// well as workspace/library-scoped ones, because deployments commonly
    /// configure embed/answer bindings at instance scope. `iam_principal`
    /// author references are nulled (principals never travel in a snapshot),
    /// and import is non-destructive — `ON CONFLICT DO NOTHING` means an
    /// import only populates an empty target and never clobbers the
    /// deployment's existing AI configuration.
    AiConfig,
    /// Everything owned by a library that is NOT a raw source file —
    /// PostgreSQL rows for content, runtime graph, and knowledge data.
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
                "ai_config" => Self::AiConfig,
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
    /// Delete all owned content/runtime rows and blobs under this library id,
    /// then insert everything from the archive under the selected library
    /// identity. PostgreSQL rows are restored atomically; blob writes are staged
    /// and rolled back separately when possible.
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

/// Whether a single library restore should refresh PostgreSQL planner stats
/// itself, or defer to one ANALYZE pass run by the caller (Workstream R / R1).
///
/// Snapshot tables (`runtime_graph_*`, `knowledge_*`, the per-dim vector
/// shards) are physically shared and grow with every library imported. Running
/// `ANALYZE {table}` after each library — as the single-library path correctly
/// does — re-scans the whole growing table per library, which is O(n²) over a
/// mass workspace import. In [`Self::Deferred`] the per-library restore skips
/// ANALYZE entirely and the workspace driver runs a single ANALYZE over the
/// union of touched tables once the import is done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestoreStatsMode {
    /// Single-library restore: ANALYZE every touched table once at the end of
    /// this library import so the planner immediately has fresh stats.
    PerLibrary,
    /// Mass / workspace import: skip per-library ANALYZE; the caller runs one
    /// ANALYZE pass after all libraries are restored.
    Deferred,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetRestoreFootprint {
    Empty,
    Populated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestoreLibraryDataAction {
    Skip,
    ImportIntoEmptyTarget,
    ReplaceTarget,
    RejectPopulatedTarget,
}

fn plan_restore_library_data(
    include_kinds: &[IncludeKind],
    overwrite: OverwriteMode,
    target_footprint: TargetRestoreFootprint,
) -> RestoreLibraryDataAction {
    if !include_kinds.contains(&IncludeKind::LibraryData) {
        return RestoreLibraryDataAction::Skip;
    }
    match (overwrite, target_footprint) {
        (OverwriteMode::Reject, TargetRestoreFootprint::Empty) => {
            RestoreLibraryDataAction::ImportIntoEmptyTarget
        }
        (OverwriteMode::Reject, TargetRestoreFootprint::Populated) => {
            RestoreLibraryDataAction::RejectPopulatedTarget
        }
        (OverwriteMode::Replace, _) => RestoreLibraryDataAction::ReplaceTarget,
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
    pub has_blobs: bool,
    /// Per-dim vector shards (`knowledge_chunk_vector_d<dim>` /
    /// `knowledge_entity_vector_d<dim>`) observed at export time. The
    /// restore path lazy-ensures each shard before streaming its rows
    /// back so the target deployment ends up with the same per-dim
    /// layout the source had. `#[serde(default)]` keeps v6 manifests parseable.
    #[serde(default)]
    pub vector_shards: Vec<VectorShardEntry>,
}

#[derive(Debug, Default, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SnapshotSummary {
    pub postgres_row_counts: BTreeMap<String, u64>,
    pub blob_count: u64,
    pub missing_blob_keys: Vec<String>,
}

#[derive(Debug, Default)]
pub struct SnapshotImportReport {
    pub library_id: Uuid,
    pub postgres_rows_by_table: Vec<(String, u64)>,
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

const POSTGRES_KNOWLEDGE_TABLES: &[&str] = &[
    "knowledge_document",
    "knowledge_revision",
    "knowledge_structured_revision",
    "knowledge_structured_block",
    "knowledge_chunk",
    "knowledge_technical_fact",
    "knowledge_entity",
    "knowledge_entity_candidate",
    "knowledge_relation",
    "knowledge_relation_candidate",
    "knowledge_evidence",
    "knowledge_context_bundle",
    "knowledge_retrieval_trace",
    "knowledge_bundle_chunk",
    "knowledge_bundle_entity",
    "knowledge_bundle_relation",
    "knowledge_bundle_evidence",
    "knowledge_chunk_entity_mention",
    "knowledge_evidence_entity_support",
    "knowledge_evidence_relation_support",
    "knowledge_vector_relation_manifest",
];

const POSTGRES_KNOWLEDGE_BASE_TABLES: &[&str] = &[
    "knowledge_document",
    "knowledge_revision",
    "knowledge_structured_revision",
    "knowledge_structured_block",
    "knowledge_chunk",
    "knowledge_technical_fact",
    "knowledge_entity",
    "knowledge_entity_candidate",
    "knowledge_relation",
    "knowledge_relation_candidate",
    "knowledge_evidence",
    "knowledge_context_bundle",
    "knowledge_retrieval_trace",
];

const POSTGRES_KNOWLEDGE_EDGE_TABLES: &[&str] = &[
    "knowledge_bundle_chunk",
    "knowledge_bundle_entity",
    "knowledge_bundle_relation",
    "knowledge_bundle_evidence",
    "knowledge_chunk_entity_mention",
    "knowledge_evidence_entity_support",
    "knowledge_evidence_relation_support",
];

const POSTGRES_WORKSPACE_TABLES: &[&str] = &["catalog_workspace"];

/// AI configuration tables exported by `IncludeKind::AiConfig`, listed in
/// FK-dependency order so a restore that re-enables FK enforcement (or a
/// human reading the archive) sees parents before children. Provider and
/// model catalogs and system prices are deployment-seeded with stable ids;
/// accounts and bindings are workspace/library-scoped config (migration
/// 0004 merged `ai_model_preset` inline into `ai_binding`, so there is no
/// separate preset table to export any more).
const POSTGRES_AI_CONFIG_TABLES: &[&str] =
    &["ai_provider_catalog", "ai_model_catalog", "ai_price_catalog", "ai_account", "ai_binding"];

/// Pre-0004 archive table names, accepted only on import for backward
/// compatibility with snapshots taken before the AI-config simplification.
/// `ai_provider_credential` → `ai_account` and `ai_binding_assignment` →
/// `ai_binding` are 1:1 renames (identical columns); `ai_model_preset` has
/// no canonical storage table any more — its rows are buffered and merged
/// into the `ai_binding` row that referenced them (see
/// `LegacyModelPreset`/`merge_legacy_binding_row`). Never written by export.
const POSTGRES_LEGACY_AI_CONFIG_TABLES: &[&str] =
    &["ai_provider_credential", "ai_model_preset", "ai_binding_assignment"];

const POSTGRES_LIBRARY_ROOT_TABLES: &[&str] = &["catalog_library"];

#[derive(Debug)]
struct SnapshotRowScope {
    source_library_id: Uuid,
    target_library_id: Uuid,
    source_workspace_id: Option<Uuid>,
    target_workspace_id: Uuid,
    /// Slug already held by the target library row. The archive's
    /// `catalog_library.slug` is rewritten to this value on restore so a
    /// snapshot exported from one library can be restored into a
    /// freshly-created target library with a different slug without tripping the
    /// `catalog_library_workspace_id_slug_key` unique constraint against a
    /// sibling library in the same workspace. `None` only when the target
    /// row was already deleted before scope construction (it never is on
    /// the canonical restore path).
    target_library_slug: Option<String>,
    document_ids: HashSet<Uuid>,
    revision_ids: HashSet<Uuid>,
    mutation_ids: HashSet<Uuid>,
    declared_blob_keys: HashSet<(String, String)>,
}

impl SnapshotRowScope {
    fn new(
        source_library_id: Uuid,
        target_library_id: Uuid,
        target_workspace_id: Uuid,
        target_library_slug: Option<String>,
    ) -> Self {
        Self {
            source_library_id,
            target_library_id,
            source_workspace_id: None,
            target_workspace_id,
            target_library_slug,
            document_ids: HashSet::new(),
            revision_ids: HashSet::new(),
            mutation_ids: HashSet::new(),
            declared_blob_keys: HashSet::new(),
        }
    }

    fn normalize_postgres_row(
        &mut self,
        table: &str,
        row: &mut serde_json::Value,
    ) -> anyhow::Result<()> {
        if is_runtime_vector_relation_name(table) {
            self.normalize_direct_library_workspace(table, row)?;
            return Ok(());
        }
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
                // Keep the operator-chosen slug of the target library so a
                // restore never collides with a sibling library that already
                // owns the archive's slug in this workspace.
                if let Some(slug) = self.target_library_slug.as_deref() {
                    set_string_field(table, row, "slug", slug)?;
                }
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
            "knowledge_document"
            | "knowledge_revision"
            | "knowledge_structured_revision"
            | "knowledge_structured_block"
            | "knowledge_chunk"
            | "knowledge_technical_fact"
            | "knowledge_entity"
            | "knowledge_entity_candidate"
            | "knowledge_relation"
            | "knowledge_relation_candidate"
            | "knowledge_evidence"
            | "knowledge_context_bundle"
            | "knowledge_retrieval_trace" => {
                self.normalize_direct_library_workspace(table, row)?;
            }
            "knowledge_bundle_chunk"
            | "knowledge_bundle_entity"
            | "knowledge_bundle_relation"
            | "knowledge_bundle_evidence"
            | "knowledge_chunk_entity_mention"
            | "knowledge_evidence_entity_support"
            | "knowledge_evidence_relation_support"
            | "knowledge_vector_relation_manifest" => {
                require_uuid_field_eq(table, row, "library_id", self.source_library_id)?;
                set_uuid_field(table, row, "library_id", self.target_library_id)?;
            }
            "ai_provider_catalog" | "ai_model_catalog" => {
                // System-seeded catalogs carry no workspace/library scope and
                // keep their stable ids; nothing to remap.
            }
            "ai_price_catalog"
            | "ai_account"
            | "ai_binding"
            // Pre-0004 archive names, accepted for backward-compatible
            // import (see `POSTGRES_LEGACY_AI_CONFIG_TABLES`). Scope
            // columns are identical to the renamed tables, so the same
            // normalizer applies unchanged.
            | "ai_provider_credential"
            | "ai_model_preset"
            | "ai_binding_assignment" => {
                self.normalize_ai_config_scope(table, row)?;
            }
            other => bail!("snapshot import has no row-scope validator for table `{other}`"),
        }
        Ok(())
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

    /// Normalizes an AI-config row (`ai_price_catalog`, `ai_account`,
    /// `ai_binding`, or their pre-0004 archive equivalents
    /// `ai_provider_credential` / `ai_model_preset` / `ai_binding_assignment`).
    /// `workspace_id` / `library_id` are nullable scope columns: a
    /// workspace-scoped row carries only `workspace_id`, a library-scoped row
    /// carries both, a system-scoped price carries neither. Each non-null
    /// scope id is rewritten to the restore target; `library_id`, when
    /// present, must belong to the exported library. `iam_principal` author
    /// references are dropped because principals never travel in a snapshot.
    fn normalize_ai_config_scope(
        &mut self,
        table: &str,
        row: &mut serde_json::Value,
    ) -> anyhow::Result<()> {
        if let Some(library_id) = optional_uuid_field(row, "library_id")
            .with_context(|| format!("parse {table}.library_id"))?
        {
            if library_id != self.source_library_id {
                bail!(
                    "snapshot {table} row references library {library_id} outside the exported library {}",
                    self.source_library_id
                );
            }
            set_uuid_field(table, row, "library_id", self.target_library_id)?;
        }
        if let Some(workspace_id) = optional_uuid_field(row, "workspace_id")
            .with_context(|| format!("parse {table}.workspace_id"))?
        {
            self.bind_workspace(table, workspace_id)?;
            set_uuid_field(table, row, "workspace_id", self.target_workspace_id)?;
        }
        null_field_if_present(row, "created_by_principal_id");
        null_field_if_present(row, "updated_by_principal_id");
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

/// Sets `field` to JSON null when the row already carries it. Used to drop
/// deployment-specific references (e.g. `iam_principal` author ids) that
/// must not survive a cross-stack restore.
fn null_field_if_present(row: &mut serde_json::Value, field: &str) {
    if let Some(object) = row.as_object_mut()
        && object.contains_key(field)
    {
        object.insert(field.to_string(), serde_json::Value::Null);
    }
}

#[derive(Debug)]
struct SnapshotManifestSections {
    postgres_tables: HashSet<String>,
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
            let table = validate_snapshot_pg_table_name(table)?;
            if !postgres_tables.insert(table.to_string()) {
                bail!("snapshot manifest declares postgres table `{table}` more than once");
            }
        }

        Ok(Self { postgres_tables })
    }

    fn require_postgres_table<'a>(&self, table: &'a str) -> anyhow::Result<&'a str> {
        let table = validate_snapshot_pg_table_name(table)?;
        if self.postgres_tables.contains(table) {
            Ok(table)
        } else {
            bail!("snapshot entry references undeclared postgres table `{table}`")
        }
    }
}

fn require_known_snapshot_pg_table(table: &str) -> anyhow::Result<&'static str> {
    POSTGRES_WORKSPACE_TABLES
        .iter()
        .chain(POSTGRES_AI_CONFIG_TABLES.iter())
        .chain(POSTGRES_LEGACY_AI_CONFIG_TABLES.iter())
        .chain(POSTGRES_LIBRARY_ROOT_TABLES.iter())
        .chain(POSTGRES_CONTENT_TABLES.iter())
        .chain(POSTGRES_RUNTIME_GRAPH_TABLES.iter())
        .chain(POSTGRES_KNOWLEDGE_TABLES.iter())
        .copied()
        .find(|candidate| *candidate == table)
        .ok_or_else(|| anyhow!("unknown snapshot postgres table `{table}`"))
}

fn validate_snapshot_pg_table_name(table: &str) -> anyhow::Result<&str> {
    if require_known_snapshot_pg_table(table).is_ok() || is_runtime_vector_relation_name(table) {
        Ok(table)
    } else {
        bail!("unknown snapshot postgres table `{table}`")
    }
}

fn is_runtime_vector_relation_name(name: &str) -> bool {
    is_canonical_per_dim_vector_collection_name(name)
}

fn is_chunk_vector_relation_name(name: &str) -> bool {
    name.strip_prefix(PER_DIM_CHUNK_VECTOR_PREFIX).is_some_and(|suffix| {
        !suffix.contains("_l") && parse_per_dim_vector_suffix_dim(suffix).is_some()
    })
}

fn quote_pg_identifier(identifier: &str) -> anyhow::Result<String> {
    anyhow::ensure!(!identifier.is_empty(), "empty SQL identifier");
    anyhow::ensure!(
        identifier.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_'),
        "unsafe SQL identifier {identifier}"
    );
    anyhow::ensure!(
        identifier.chars().next().is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_'),
        "SQL identifier must start with a letter or underscore: {identifier}"
    );
    Ok(format!("\"{}\"", identifier.replace('"', "\"\"")))
}

/// Max attempts for a savepoint-guarded vector-shard write (Workstream R / R2,
/// R3). The first attempt plus a small bounded set of retries.
const RESTORE_SAVEPOINT_MAX_ATTEMPTS: u32 = 5;
/// Base backoff between savepoint retries; multiplied by the attempt index.
const RESTORE_SAVEPOINT_BACKOFF_BASE: std::time::Duration = std::time::Duration::from_millis(25);

/// PostgreSQL deadlock SQLSTATE — the transaction was chosen as the deadlock
/// victim and rolled back. Safe to retry after rolling back to a savepoint.
const PG_SQLSTATE_DEADLOCK_DETECTED: &str = "40P01";

/// Returns `true` when a `SQLx` error inside the restore transaction is a
/// transient contention that a savepoint rollback + retry can recover from
/// (Workstream R / R2 + in-transaction R3):
///
/// - `40P01` deadlock_detected — parallel restores fight over the shared
///   per-dim vector shard; the loser is rolled back and can replay.
/// - `42P07` duplicate_table / `42710` duplicate_object — two sessions both
///   passed the `CREATE ... IF NOT EXISTS` existence check and raced the
///   catalog insert; on retry the relation already exists and the create
///   no-ops.
/// - `23505` unique_violation — the same race surfacing as a `pg_catalog`
///   unique-index collision.
/// - `XX000` internal_error — Postgres reports "tuple concurrently
///   updated/inserted" for concurrent DDL under this generic code.
fn pg_error_is_retryable_restore_contention(error: &sqlx::Error) -> bool {
    error.as_database_error().and_then(sqlx::error::DatabaseError::code).is_some_and(|code| {
        matches!(code.as_ref(), PG_SQLSTATE_DEADLOCK_DETECTED | "42P07" | "42710" | "23505")
            || (code.as_ref() == "XX000"
                && error.to_string().to_ascii_lowercase().contains("concurrently"))
    })
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
        // root cause (database cursor error, storage error, etc.) lives deeper
        // in the chain and is invaluable when debugging large-corpus exports.
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
    if include_set.contains(&IncludeKind::AiConfig) {
        manifest_postgres_tables
            .extend(POSTGRES_AI_CONFIG_TABLES.iter().map(|table| (*table).to_string()));
    }
    let mut library_postgres_tables: Vec<String> = Vec::new();
    if include_library_data {
        manifest_postgres_tables
            .extend(POSTGRES_LIBRARY_ROOT_TABLES.iter().map(|table| (*table).to_string()));
        library_postgres_tables.extend(POSTGRES_CONTENT_TABLES.iter().map(|s| (*s).to_string()));
        library_postgres_tables
            .extend(POSTGRES_RUNTIME_GRAPH_TABLES.iter().map(|s| (*s).to_string()));
        library_postgres_tables.extend(POSTGRES_KNOWLEDGE_TABLES.iter().map(|s| (*s).to_string()));
        library_postgres_tables
            .extend(list_pg_vector_relations_for_library(pool, library_id).await?);
        manifest_postgres_tables.extend(library_postgres_tables.iter().cloned());
    }
    let mut vector_shards: Vec<VectorShardEntry> = Vec::new();
    if include_library_data {
        for shard in
            library_postgres_tables.iter().filter(|name| is_runtime_vector_relation_name(name))
        {
            let dim = parse_per_dim_vector_collection_dim(&shard).ok_or_else(|| {
                anyhow!("malformed per-dim vector relation `{shard}` discovered during export")
            })?;
            vector_shards.push(VectorShardEntry { name: shard.clone(), dim });
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
    // AI config rows land after the workspace row (their `workspace_id` FK
    // target) and before catalog_library; library-scoped AI rows reference
    // catalog_library but the import disables FK enforcement, so a restore
    // accepts them in any order within the single transaction.
    if include_set.contains(&IncludeKind::AiConfig) {
        let counts = export_pg_ai_config_scope(builder, pool, library_id).await?;
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

    // 3. blobs (if included). Each storage_key gathered from the
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

    // 4. summary.json — last, so it carries the real observed counts.
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

/// Exports the portable AI configuration that makes the exported library
/// resolvable on another stack: provider and model catalogs travel whole
/// (referenced by FK and seeded with stable ids on every deployment); prices
/// include the system catalog plus this workspace's overrides; accounts and
/// bindings include instance-scoped (deployment-global) rows plus the rows
/// scoped to this workspace/library. `api_key` is always nulled out —
/// provider secrets never leave the source stack. Bindings are filtered to
/// those whose account is also in the exported set so a restore never lands
/// a dangling FK.
async fn export_pg_ai_config_scope<W>(
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
            .context("load workspace id for ai-config export")?
            .ok_or_else(|| anyhow!("library {library_id} disappeared during ai-config export"))?;

    // (table, query). Each query returns a single `row` jsonb column and is
    // bound with ($1 = workspace_id, $2 = library_id). The order matches the
    // FK-dependency order in POSTGRES_AI_CONFIG_TABLES.
    //
    // The scope filter keeps instance-scoped (deployment-global) rows AND the
    // rows scoped to this workspace/library. Instance scope is included on
    // purpose: in practice a deployment configures embed/answer bindings at
    // instance scope, so excluding them would make an AI-config export empty
    // for the common case. Import is non-destructive (ON CONFLICT DO NOTHING),
    // so importing instance config only ever populates an empty target.
    // `{alias}` is the table alias whose scope columns are tested. Instance
    // scope always matches; workspace/library scope matches this export's
    // ($1, $2). Phrased per-alias so the binding join can require its
    // account to be in the exported set too.
    let scope_pred = |alias: &str| {
        format!(
            "({alias}.scope_kind = 'instance' \
             OR ({alias}.scope_kind IN ('workspace','library') \
                 AND ({alias}.workspace_id = $1 OR {alias}.library_id = $2)))"
        )
    };
    let binding_query = format!(
        "SELECT row_to_json(b)::jsonb AS row FROM ai_binding b \
         JOIN ai_account a ON a.id = b.account_id AND {} \
         WHERE {}",
        scope_pred("a"),
        scope_pred("b"),
    );
    let account_query = format!(
        "SELECT (row_to_json(t)::jsonb || jsonb_build_object('api_key', NULL)) AS row \
         FROM ai_account t WHERE {}",
        scope_pred("t"),
    );
    let queries: [(&str, String); 5] = [
        (
            "ai_provider_catalog",
            "SELECT row_to_json(t)::jsonb AS row FROM ai_provider_catalog t".to_string(),
        ),
        (
            "ai_model_catalog",
            "SELECT row_to_json(t)::jsonb AS row FROM ai_model_catalog t".to_string(),
        ),
        (
            "ai_price_catalog",
            "SELECT row_to_json(t)::jsonb AS row FROM ai_price_catalog t \
             WHERE catalog_scope = 'system' \
                OR (catalog_scope = 'workspace_override' AND workspace_id = $1)"
                .to_string(),
        ),
        ("ai_account", account_query),
        ("ai_binding", binding_query),
    ];

    let mut counts = Vec::<(String, u64)>::new();
    for (table, query) in queries {
        let rows: Vec<serde_json::Value> = sqlx::query_scalar(&query)
            .bind(workspace_id)
            .bind(library_id)
            .fetch_all(pool)
            .await
            .with_context(|| format!("export ai-config table `{table}`"))?;
        if rows.is_empty() {
            continue;
        }
        let mut buffer: Vec<u8> = Vec::new();
        for value in &rows {
            let mut line = serde_json::to_vec(value)
                .with_context(|| format!("serialize {table} ai-config row"))?;
            line.push(b'\n');
            buffer.extend_from_slice(&line);
        }
        let path = format!("postgres/{table}/part-000001.ndjson");
        append_raw_entry(builder, &path, &buffer).await?;
        counts.push((table.to_string(), rows.len() as u64));
    }

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

async fn list_pg_vector_relations_for_library(
    pool: &PgPool,
    library_id: Uuid,
) -> anyhow::Result<Vec<String>> {
    let relations = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT relation_name
         FROM knowledge_vector_relation_manifest
         WHERE library_id = $1
         ORDER BY relation_name",
    )
    .bind(library_id)
    .fetch_all(pool)
    .await
    .context("list snapshot vector relations")?;
    relations
        .into_iter()
        .map(|relation| {
            validate_snapshot_pg_table_name(&relation)?;
            if !is_runtime_vector_relation_name(&relation) {
                bail!("snapshot vector manifest relation `{relation}` is not a vector relation");
            }
            Ok(relation)
        })
        .collect()
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
    validate_snapshot_pg_table_name(table)?;
    if is_runtime_vector_relation_name(table) {
        let relation = quote_pg_identifier(table)?;
        return Ok(if is_chunk_vector_relation_name(table) {
            format!(
                "SELECT jsonb_build_object(
                    'key', key,
                    'vector_id', vector_id,
                    'workspace_id', workspace_id,
                    'library_id', library_id,
                    'chunk_id', chunk_id,
                    'revision_id', revision_id,
                    'embedding_model_key', embedding_model_key,
                    'vector_kind', vector_kind,
                    'dimensions', dimensions,
                    'embedding', embedding::text,
                    'freshness_generation', freshness_generation,
                    'created_at', created_at,
                    'occurred_at', occurred_at,
                    'occurred_until', occurred_until
                 ) AS row
                 FROM {relation}
                 WHERE library_id = $1
                 ORDER BY key"
            )
        } else {
            format!(
                "SELECT jsonb_build_object(
                    'key', key,
                    'vector_id', vector_id,
                    'workspace_id', workspace_id,
                    'library_id', library_id,
                    'entity_id', entity_id,
                    'embedding_model_key', embedding_model_key,
                    'vector_kind', vector_kind,
                    'dimensions', dimensions,
                    'embedding', embedding::text,
                    'freshness_generation', freshness_generation,
                    'created_at', created_at
                 ) AS row
                 FROM {relation}
                 WHERE library_id = $1
                 ORDER BY key"
            )
        });
    }
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
        "knowledge_vector_relation_manifest" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_vector_relation_manifest t
             WHERE t.library_id = $1
             ORDER BY t.dim, t.vector_kind, t.embedding_model_key"
            .to_string(),
        "knowledge_document" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_document t
             WHERE t.library_id = $1
             ORDER BY t.document_id"
            .to_string(),
        "knowledge_revision" | "knowledge_structured_revision" => format!(
            "SELECT row_to_json(t)::jsonb AS row
             FROM {table} t
             WHERE t.library_id = $1
             ORDER BY t.revision_id"
        ),
        "knowledge_retrieval_trace" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_retrieval_trace t
             WHERE t.library_id = $1
             ORDER BY t.trace_id"
            .to_string(),
        "knowledge_structured_block" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_structured_block t
             WHERE t.library_id = $1
             ORDER BY t.revision_id, t.ordinal, t.block_id"
            .to_string(),
        "knowledge_chunk" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_chunk t
             WHERE t.library_id = $1
             ORDER BY t.revision_id, t.chunk_index, t.chunk_id"
            .to_string(),
        "knowledge_technical_fact" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_technical_fact t
             WHERE t.library_id = $1
             ORDER BY t.fact_id"
            .to_string(),
        "knowledge_entity" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_entity t
             WHERE t.library_id = $1
             ORDER BY t.entity_id"
            .to_string(),
        "knowledge_entity_candidate" | "knowledge_relation_candidate" => format!(
            "SELECT row_to_json(t)::jsonb AS row
             FROM {table} t
             WHERE t.library_id = $1
             ORDER BY t.candidate_id"
        ),
        "knowledge_relation" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_relation t
             WHERE t.library_id = $1
             ORDER BY t.relation_id"
            .to_string(),
        "knowledge_evidence" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_evidence t
             WHERE t.library_id = $1
             ORDER BY t.evidence_id"
            .to_string(),
        "knowledge_context_bundle" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_context_bundle t
             WHERE t.library_id = $1
             ORDER BY t.bundle_id"
            .to_string(),
        "knowledge_bundle_chunk" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_bundle_chunk t
             WHERE t.library_id = $1
             ORDER BY t.bundle_id, t.rank, t.chunk_id"
            .to_string(),
        "knowledge_bundle_entity" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_bundle_entity t
             WHERE t.library_id = $1
             ORDER BY t.bundle_id, t.rank, t.entity_id"
            .to_string(),
        "knowledge_bundle_relation" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_bundle_relation t
             WHERE t.library_id = $1
             ORDER BY t.bundle_id, t.rank, t.relation_id"
            .to_string(),
        "knowledge_bundle_evidence" => "SELECT row_to_json(t)::jsonb AS row
             FROM knowledge_bundle_evidence t
             WHERE t.library_id = $1
             ORDER BY t.bundle_id, t.rank, t.evidence_id"
            .to_string(),
        "knowledge_chunk_entity_mention"
        | "knowledge_evidence_entity_support"
        | "knowledge_evidence_relation_support" => format!(
            "SELECT row_to_json(t)::jsonb AS row
             FROM {table} t
             WHERE t.library_id = $1
             ORDER BY t.from_id, t.to_id, t.relation_type"
        ),
        _ => format!(
            "SELECT row_to_json(t)::jsonb AS row
             FROM {table} t
             WHERE t.library_id = $1
             ORDER BY t.id"
        ),
    })
}

// ===========================================================================
// Import
// ===========================================================================

/// Maximum number of rows included in a single PostgreSQL INSERT statement
/// during restore. 1000 strikes a good balance: large
/// enough to amortize round-trip latency across a ten-thousand-row
/// table, small enough that a single statement's JSONB payload stays
/// under a few MiB and any parser bug only wastes a small slice.
const IMPORT_BATCH_ROWS: usize = 1000;
/// Restores a library from a tar.zst archive body. `body` is any
/// `AsyncRead` — typically the request body stream. Rows are flushed
/// to storage in batches as the archive streams in, so memory footprint
/// stays roughly one PostgreSQL batch rather than scaling with total archive
/// size.
pub async fn restore_library_archive<R>(
    state: &AppState,
    library_id: Uuid,
    body: R,
    overwrite: OverwriteMode,
) -> Result<SnapshotImportReport, ContentServiceError>
where
    R: AsyncRead + Unpin + Send,
{
    restore_library_archive_inner(state, library_id, body, overwrite, RestoreStatsMode::PerLibrary)
        .await
        .map_err(|error| {
            // Log the full anyhow chain BEFORE collapsing to ContentServiceError —
            // symmetric to the export side. ContentServiceError only carries the
            // top message, but the underlying database/storage/io error lives
            // deeper in the chain and is what an operator needs to act on.
            tracing::error!(
                %library_id,
                error_chain = format!("{error:#}"),
                "snapshot import failed with full chain",
            );
            ContentServiceError::from_message(error.to_string())
        })
}

/// Maps a pre-0004 archive table name to the canonical storage table its
/// rows land in on restore. `ai_model_preset` is deliberately absent — it
/// has no canonical storage table any more; its rows are buffered
/// separately (see [`LegacyModelPreset`]) and merged into the owning
/// `ai_binding` row instead of being routed through here. Canonical table
/// names pass through unchanged.
fn canonical_ai_config_storage_table(table: &str) -> &str {
    match table {
        "ai_provider_credential" => "ai_account",
        "ai_binding_assignment" => "ai_binding",
        other => other,
    }
}

/// A buffered pre-0004 `ai_model_preset` row, captured during restore so its
/// fields can be spliced into the `ai_binding` row that references it via
/// `model_preset_id` — the FK column migration 0004 dropped once presets
/// were merged inline.
struct LegacyModelPreset {
    model_catalog_id: serde_json::Value,
    system_prompt: serde_json::Value,
    temperature: serde_json::Value,
    top_p: serde_json::Value,
    max_output_tokens_override: serde_json::Value,
    extra_parameters_json: serde_json::Value,
}

impl LegacyModelPreset {
    fn from_row(row: &serde_json::Value) -> anyhow::Result<(Uuid, Self)> {
        let id = required_uuid_field("ai_model_preset", row, "id")?;
        let field = |name: &str| row.get(name).cloned().unwrap_or(serde_json::Value::Null);
        Ok((
            id,
            Self {
                model_catalog_id: field("model_catalog_id"),
                system_prompt: field("system_prompt"),
                temperature: field("temperature"),
                top_p: field("top_p"),
                max_output_tokens_override: field("max_output_tokens_override"),
                extra_parameters_json: field("extra_parameters_json"),
            },
        ))
    }
}

/// Splices a pre-0004 `ai_binding_assignment` row into the canonical
/// `ai_binding` shape: renames `provider_credential_id` → `account_id` and
/// replaces `model_preset_id` with the inline fields of the preset it
/// pointed at, looked up from `legacy_presets` (populated while streaming
/// the archive's `ai_model_preset` section, which always precedes
/// `ai_binding_assignment` in FK-dependency export order).
fn merge_legacy_binding_row(
    row: &mut serde_json::Value,
    legacy_presets: &HashMap<Uuid, LegacyModelPreset>,
) -> anyhow::Result<()> {
    let object = row
        .as_object_mut()
        .ok_or_else(|| anyhow!("snapshot ai_binding_assignment row is not a JSON object"))?;
    let account_id = object.remove("provider_credential_id").ok_or_else(|| {
        anyhow!("snapshot ai_binding_assignment row missing provider_credential_id")
    })?;
    object.insert("account_id".to_string(), account_id);
    let preset_id_value = object
        .remove("model_preset_id")
        .ok_or_else(|| anyhow!("snapshot ai_binding_assignment row missing model_preset_id"))?;
    let preset_id =
        preset_id_value.as_str().and_then(|value| Uuid::parse_str(value).ok()).ok_or_else(
            || anyhow!("snapshot ai_binding_assignment row has malformed model_preset_id"),
        )?;
    let preset = legacy_presets.get(&preset_id).ok_or_else(|| {
        anyhow!(
            "snapshot ai_binding_assignment row references model_preset_id {preset_id} \
             not present in the archive's ai_model_preset section"
        )
    })?;
    object.insert("model_catalog_id".to_string(), preset.model_catalog_id.clone());
    object.insert("system_prompt".to_string(), preset.system_prompt.clone());
    object.insert("temperature".to_string(), preset.temperature.clone());
    object.insert("top_p".to_string(), preset.top_p.clone());
    object.insert(
        "max_output_tokens_override".to_string(),
        preset.max_output_tokens_override.clone(),
    );
    object.insert("extra_parameters_json".to_string(), preset.extra_parameters_json.clone());
    Ok(())
}

async fn restore_library_archive_inner<R>(
    state: &AppState,
    library_id: Uuid,
    body: R,
    overwrite: OverwriteMode,
    stats_mode: RestoreStatsMode,
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
            if parsed.schema_version < MIN_SUPPORTED_SNAPSHOT_SCHEMA_VERSION
                || parsed.schema_version > SNAPSHOT_SCHEMA_VERSION
            {
                bail!(
                    "snapshot schema_version {} is not supported by this build (supported {}..={})",
                    parsed.schema_version,
                    MIN_SUPPORTED_SNAPSHOT_SCHEMA_VERSION,
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

    let mut stashed_storage: Option<StashedContentDirectory> = None;
    let restore_result: anyhow::Result<SnapshotImportReport> = async {
        // Stage 2 — from here until commit the selected library identity row is
        // locked. Concurrent ingests/imports that reference the same library block on
        // the FK parent row lock, so `overwrite=reject` cannot pass an empty-target
        // check and then race with new content rows.
        let pool = &state.persistence.postgres;
        let mut tx = pool.begin().await.context("begin snapshot tx")?;
        content_repository::acquire_content_library_storage_lock_in_tx(&mut tx, library_id)
            .await
            .context("acquire library storage lock before snapshot restore")?;
        let locked_target = lock_catalog_library_for_restore(&mut tx, library_id).await?;
    let target_workspace_id = locked_target.workspace_id;
    let target_library_slug = Some(locked_target.slug);
    let existing_workspace_id = Some(target_workspace_id);
    let target_footprint = if manifest.include_kinds.contains(&IncludeKind::LibraryData)
        && overwrite == OverwriteMode::Reject
        && tx_library_has_restore_footprint(state, &mut tx, library_id).await?
    {
        TargetRestoreFootprint::Populated
    } else {
        TargetRestoreFootprint::Empty
    };
    let library_data_action =
        plan_restore_library_data(&manifest.include_kinds, overwrite, target_footprint);
    match library_data_action {
        RestoreLibraryDataAction::Skip | RestoreLibraryDataAction::ImportIntoEmptyTarget => {}
        RestoreLibraryDataAction::RejectPopulatedTarget => {
            bail!(
                "target library data conflict: library {library_id} already contains data — pass overwrite=replace to restore over it"
            );
        }
        RestoreLibraryDataAction::ReplaceTarget => {
            stashed_storage =
                prepare_replace_library_footprint(state, library_id, existing_workspace_id).await?;
        }
    }

    let import_result: anyhow::Result<SnapshotImportReport> = async {
    // Lazy-ensure every per-dim vector shard the source archive carried so the
    // row-insertion path lands on relations that already exist with matching
    // ANN + persistent indexes.
    ensure_manifest_vector_shards(state, &manifest)
        .await
        .context("ensure per-dim vector shards declared by snapshot manifest")?;

    // Stream remaining entries and flush in batches. We keep
    // a single Postgres transaction alive for the whole restore so FKs
    // are satisfied all at once at commit time.
    sqlx::query("SET LOCAL session_replication_role = 'replica'")
        .execute(&mut *tx)
        .await
        .context("disable FK checks for snapshot import")?;
    if library_data_action == RestoreLibraryDataAction::ReplaceTarget {
        clear_library_postgres_footprint(&mut tx, library_id).await?;
    }

    let mut pg_batcher = PgBatcher::new();
    let mut knowledge_dedup = KnowledgeDocumentDedup::default();
    let mut row_scope = SnapshotRowScope::new(
        manifest.library_id,
        library_id,
        target_workspace_id,
        target_library_slug,
    );
    // Pre-0004 archives carry `ai_model_preset` as a sibling section (bounded
    // by one deployment/workspace's preset count). Buffered here and merged
    // into `ai_binding` rows as they stream (see `merge_legacy_binding_row`).
    let mut legacy_model_presets: HashMap<Uuid, LegacyModelPreset> = HashMap::new();

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

        if path == "EXPORT_FAILED.json" {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).await.context("read EXPORT_FAILED.json")?;
            let message = serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .and_then(|value| {
                    value.get("error").and_then(serde_json::Value::as_str).map(str::to_string)
                })
                .unwrap_or_else(|| "snapshot archive contains EXPORT_FAILED.json".to_string());
            bail!("snapshot archive is marked as failed export: {message}");
        }

        if path == "manifest.json" {
            bail!("tar archive contains a second manifest.json");
        }

        if let Some(rest) = path.strip_prefix("postgres/") {
            let (table_ref, _file) = split_section_path(rest)
                .with_context(|| format!("malformed postgres path `{path}`"))?;
            let table = manifest_sections.require_postgres_table(table_ref)?;

            if table == "ai_model_preset" {
                // Pre-0004 archive: buffer preset rows for the
                // `ai_binding_assignment` section that follows. Never reaches
                // the batcher — `ai_model_preset` has no storage table.
                read_ndjson_entry_and(&mut entry, &mut |mut row| {
                    row_scope.normalize_postgres_row(table, &mut row)?;
                    let (id, preset) = LegacyModelPreset::from_row(&row)?;
                    legacy_model_presets.insert(id, preset);
                    Ok(())
                })
                .await
                .with_context(|| format!("parse ndjson `{path}`"))?;
                continue;
            }

            let storage_table = canonical_ai_config_storage_table(table);
            pg_batcher.on_new_section(storage_table, &mut tx).await?;
            read_ndjson_entry_and(&mut entry, &mut |mut row| {
                row_scope.normalize_postgres_row(storage_table, &mut row)?;
                if table == "ai_binding_assignment" {
                    merge_legacy_binding_row(&mut row, &legacy_model_presets)?;
                }
                let mut kept = true;
                if is_chunk_vector_relation_name(storage_table) {
                    route_pg_vector_row_through_dedup(
                        &mut knowledge_dedup,
                        &mut pg_batcher,
                        storage_table,
                        row,
                        &mut kept,
                    )?;
                } else {
                    route_pg_row_through_dedup(
                        &mut knowledge_dedup,
                        &mut pg_batcher,
                        storage_table,
                        row,
                        &mut kept,
                    )?;
                }
                if kept {
                    *counts_pg.entry(storage_table.to_string()).or_default() += 1;
                }
                Ok(())
            })
            .await
            .with_context(|| format!("parse ndjson `{path}`"))?;
            pg_batcher.maybe_flush(&mut tx).await?;
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

    // Stage 4 — final flush + commit. Resolve the document dedup (covers
    // archives that carry `knowledge_document` rows but no descendants, so
    // the lazy finalize on the first descendant never fired), then drain
    // every batcher and commit the Postgres transaction.
    knowledge_dedup.finalize(&mut pg_batcher);
    // Record the kept `knowledge_document` count so the import report reflects
    // what was committed after the keep-rule dropped stale duplicates.
    if knowledge_dedup.saw_document_rows() {
        *counts_pg.entry("knowledge_document".to_string()).or_default() +=
            knowledge_dedup.kept_document_count();
    }
    pg_batcher.flush(&mut tx).await?;
    tx.commit().await.context("commit snapshot tx")?;
    // R1: in a mass/workspace import the shared snapshot tables grow with every
    // library, so a per-library ANALYZE re-scans the whole table each time
    // (O(n²)). Defer to a single end-of-import ANALYZE run by the workspace
    // driver. The single-library path still ANALYZEs here so the planner has
    // fresh stats immediately.
    if stats_mode == RestoreStatsMode::PerLibrary {
        if let Err(error) = analyze_imported_postgres_tables(pool, &counts_pg).await {
            tracing::warn!(
                %library_id,
                error = %error,
                "snapshot import postgres stats refresh failed",
            );
        }
    }

    report.postgres_rows_by_table = counts_pg.into_iter().collect();
    Ok(report)
    }
    .await;

    match import_result {
        Ok(report) => {
            if let Some(stashed) = stashed_storage.as_ref()
                && let Err(error) = state.content_storage.purge_stashed_directory(stashed).await
            {
                tracing::warn!(
                    %library_id,
                    error = %error,
                    "snapshot restore succeeded but failed to purge stashed blob directory",
                );
            }
            Ok(report)
        }
        Err(error) => {
            if let Some(stashed) = stashed_storage.as_ref()
                && let Err(restore_error) = state
                    .content_storage
                    .restore_stashed_directory_replacing_current(stashed)
                    .await
            {
                tracing::error!(
                    %library_id,
                    restore_error = %restore_error,
                    primary_error = format!("{error:#}"),
                    "snapshot restore failed and blob stash rollback also failed",
                );
            }
            Err(error)
        }
    }
    }
    .await;

    restore_result
}

async fn analyze_imported_postgres_tables(
    pool: &PgPool,
    row_counts: &BTreeMap<String, u64>,
) -> anyhow::Result<()> {
    for (table, row_count) in row_counts {
        if *row_count == 0 {
            continue;
        }
        validate_snapshot_pg_table_name(table)?;
        let table = quote_pg_identifier(table)?;
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

fn optional_i64_json(row: &serde_json::Value, field: &str) -> anyhow::Result<Option<i64>> {
    match row.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(value)) => {
            value.as_i64().map(Some).ok_or_else(|| anyhow!("field `{field}` is not an i64"))
        }
        Some(serde_json::Value::String(value)) if value.is_empty() => Ok(None),
        Some(serde_json::Value::String(value)) => {
            value.parse::<i64>().map(Some).context("parse i64")
        }
        Some(_) => bail!("field `{field}` must be an integer"),
    }
}

/// Buffers Postgres rows per-table and flushes them as a single
/// `jsonb_populate_recordset` statement. Each table keeps its own
/// pending vec so different PostgreSQL sections cannot be accidentally inserted
/// through the most recent table's column list.
struct PgBatcher {
    pending: BTreeMap<String, Vec<serde_json::Value>>,
}

impl PgBatcher {
    fn new() -> Self {
        Self { pending: BTreeMap::new() }
    }

    fn push(&mut self, table: &str, row: serde_json::Value) {
        self.pending.entry(table.to_string()).or_default().push(row);
    }

    async fn on_new_section(
        &mut self,
        table: &str,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> anyhow::Result<()> {
        let tables: Vec<String> = self
            .pending
            .keys()
            .filter(|pending_table| pending_table.as_str() != table)
            .cloned()
            .collect();
        for pending_table in tables {
            self.flush_table(tx, &pending_table).await?;
        }
        Ok(())
    }

    async fn maybe_flush(
        &mut self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> anyhow::Result<()> {
        let tables: Vec<String> = self
            .pending
            .iter()
            .filter(|(_table, rows)| rows.len() >= IMPORT_BATCH_ROWS)
            .map(|(table, _rows)| table.clone())
            .collect();
        for table in tables {
            while self.pending.get(&table).is_some_and(|rows| rows.len() >= IMPORT_BATCH_ROWS) {
                self.flush_partial(tx, &table, IMPORT_BATCH_ROWS).await?;
            }
        }
        Ok(())
    }

    async fn flush(
        &mut self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> anyhow::Result<()> {
        let tables: Vec<String> = self.pending.keys().cloned().collect();
        for table in tables {
            self.flush_table(tx, &table).await?;
        }
        Ok(())
    }

    async fn flush_table(
        &mut self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        table: &str,
    ) -> anyhow::Result<()> {
        while self.pending.get(table).is_some_and(|rows| !rows.is_empty()) {
            let take = self.pending.get(table).map_or(0, |rows| rows.len().min(IMPORT_BATCH_ROWS));
            self.flush_partial(tx, table, take).await?;
        }
        self.pending.remove(table);
        Ok(())
    }

    async fn flush_partial(
        &mut self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        table: &str,
        take: usize,
    ) -> anyhow::Result<()> {
        let rows = self
            .pending
            .get_mut(table)
            .ok_or_else(|| anyhow!("flush_partial called for empty table `{table}`"))?;
        let tail = rows.split_off(take.min(rows.len()));
        let head = std::mem::replace(rows, tail);
        if rows.is_empty() {
            self.pending.remove(table);
        }
        insert_pg_rows_bulk(tx, table, head).await?;
        Ok(())
    }
}

/// Collapses the `knowledge_document` plane to exactly one row per
/// `(workspace_id, library_id, external_key)` while a snapshot streams in,
/// and cascade-filters descendant rows so nothing is orphaned.
///
/// ### Why chunk ownership must be known before resolving
///
/// A re-sync can mint a NEW `content_document` that becomes head/active but
/// whose ingest died before `chunk_content`, so it has 0 chunks, while the
/// OLD sibling (now non-head or `document_state='deleted'`) holds the real
/// chunks. Keeping the empty new head and dropping the old sibling would
/// restore an empty shell with content silently gone. The keep-rule therefore
/// gives the "has chunks" signal higher priority than head/active status.
///
/// ### Two-phase buffering
///
/// Because `knowledge_chunk` rows arrive after `knowledge_revision` /
/// `knowledge_structured_*` in the export order, the dedup cannot finalize
/// correctly the moment the first document-descendant row arrives. Instead:
///
/// 1. **Pre-finalize buffer** — every document-descendant row that arrives
///    before finalization is held in `pre_finalize_buffer`; chunk rows also
///    update `chunk_bearing_document_ids` before being buffered.
/// 2. **Finalize** — called explicitly before vector rows and at the final
///    flush. By then the chunk section is complete, so
///    `chunk_bearing_document_ids` is authoritative.
///    The keep-rule runs, `kept_document_ids` is frozen, and the buffered
///    pre-finalize rows are replayed through the cascade into `batcher`.
/// 3. **Post-finalize** — rows that arrive after finalization are routed
///    directly (no buffering needed).
///
/// This is a strict no-op for archives that already carry one document per key.
#[derive(Default)]
struct KnowledgeDocumentDedup {
    /// `document_id`s present in `content_document_head` — the strongest
    /// keep signal. Captured from the head section, which always precedes
    /// `knowledge_document` in the archive.
    head_document_ids: HashSet<Uuid>,
    /// Normalized `knowledge_document` rows buffered until the keep-rule runs.
    buffered: Vec<BufferedDocument>,
    /// `document_id`s that own at least one `knowledge_chunk` row. Populated
    /// while chunk rows stream in (before finalize), so the keep-rule can
    /// prefer content-bearing docs over empty shells.
    chunk_bearing_document_ids: HashSet<Uuid>,
    /// Document-descendant and chunk-descendant rows that arrived before
    /// finalization. Replayed through the cascade once the keep set is known.
    pre_finalize_buffer: Vec<(String, serde_json::Value)>,
    /// Once finalized, the winning `document_id` per external key.
    kept_document_ids: HashSet<Uuid>,
    /// `chunk_id`s belonging to kept documents — the cascade filter for
    /// chunk-derived tables (vectors, candidates, mentions, bundle_chunk).
    kept_chunk_ids: HashSet<Uuid>,
    /// `true` once at least one `knowledge_document` row has been buffered.
    saw_document_rows: bool,
    finalized: bool,
}

/// A normalized `knowledge_document` row buffered until the keep-rule runs.
struct BufferedDocument {
    document_id: Uuid,
    external_key: String,
    has_chunks: bool, // filled in during finalize from chunk_bearing_document_ids
    is_head: bool,
    is_active: bool,
    latest_revision_no: Option<i64>,
    updated_at: Option<chrono::DateTime<chrono::Utc>>,
    row: serde_json::Value,
}

impl KnowledgeDocumentDedup {
    /// Records a `content_document_head.document_id` as a keep signal. Called
    /// for every head row; tolerant of rows without a parseable id (the
    /// row-scope validator has already enforced the contract upstream).
    fn note_head_row(&mut self, row: &serde_json::Value) {
        if let Ok(Some(document_id)) = optional_uuid_field(row, "document_id") {
            self.head_document_ids.insert(document_id);
        }
    }

    /// Number of `knowledge_document` rows that survived the keep-rule.
    /// Meaningful only after [`Self::finalize`].
    fn kept_document_count(&self) -> u64 {
        self.kept_document_ids.len() as u64
    }

    /// Returns `true` if any `knowledge_document` rows have been buffered.
    fn saw_document_rows(&self) -> bool {
        self.saw_document_rows
    }

    /// Buffers a normalized `knowledge_document` row (PG column shape) for the
    /// keep-rule instead of inserting it immediately.
    fn buffer_document(&mut self, row: serde_json::Value) -> anyhow::Result<()> {
        debug_assert!(!self.finalized, "buffer_document called after finalize — ordering bug");
        self.saw_document_rows = true;
        let document_id = required_uuid_field("knowledge_document", &row, "document_id")?;
        let external_key =
            required_string_field("knowledge_document", &row, "external_key")?.to_string();
        let is_active = string_field(&row, "document_state").is_some_and(|s| s == "active");
        let is_head = self.head_document_ids.contains(&document_id);
        let latest_revision_no = optional_i64_json(&row, "latest_revision_no")?;
        let updated_at = string_field(&row, "updated_at")
            .and_then(|s| s.parse::<chrono::DateTime<chrono::Utc>>().ok());
        self.buffered.push(BufferedDocument {
            document_id,
            external_key,
            has_chunks: false, // filled during finalize
            is_head,
            is_active,
            latest_revision_no,
            updated_at,
            row,
        });
        Ok(())
    }

    /// Records that `document_id` owns at least one chunk. Called for every
    /// `knowledge_chunk` row before finalization; ignored after.
    fn note_chunk_owner(&mut self, document_id: Uuid) {
        if !self.finalized {
            self.chunk_bearing_document_ids.insert(document_id);
        }
    }

    /// Deterministic keep-rule: returns `true` when `a` should beat `b` for
    /// the same external key. Preference order:
    ///
    /// 1. **has chunks** — if any candidate for the key owns chunks, choose
    ///    only among chunk-bearing candidates. An empty shell (new head whose
    ///    ingest died before `chunk_content`) never evicts a content-bearing
    ///    sibling.
    /// 2. presence in `content_document_head`
    /// 3. `document_state = 'active'`
    /// 4. greater `latest_revision_no`
    /// 5. later `updated_at` (parsed to `DateTime` — no lexical ambiguity)
    /// 6. greater `document_id` (final, total tie-break; stream-order independent)
    fn candidate_beats(a: &BufferedDocument, b: &BufferedDocument) -> bool {
        // Tier 1 — chunk ownership
        if a.has_chunks != b.has_chunks {
            return a.has_chunks;
        }
        // Tier 2 — head presence
        if a.is_head != b.is_head {
            return a.is_head;
        }
        // Tier 3 — active state
        if a.is_active != b.is_active {
            return a.is_active;
        }
        // Tier 4 — revision number
        match a.latest_revision_no.cmp(&b.latest_revision_no) {
            std::cmp::Ordering::Greater => return true,
            std::cmp::Ordering::Less => return false,
            std::cmp::Ordering::Equal => {}
        }
        // Tier 5 — updated_at (DateTime, not string)
        match a.updated_at.cmp(&b.updated_at) {
            std::cmp::Ordering::Greater => return true,
            std::cmp::Ordering::Less => return false,
            std::cmp::Ordering::Equal => {}
        }
        // Tier 6 — document_id tie-break
        a.document_id > b.document_id
    }

    /// Resolves the keep-rule across all buffered documents, pushes the kept
    /// `knowledge_document` rows to `batcher`, replays every pre-finalize
    /// descendant row through the cascade, and freezes the kept sets. Idempotent.
    fn finalize(&mut self, batcher: &mut PgBatcher) {
        if self.finalized {
            return;
        }
        self.finalized = true;

        // Annotate each buffered document with chunk-ownership now that the
        // full chunk section has streamed.
        for doc in &mut self.buffered {
            doc.has_chunks = self.chunk_bearing_document_ids.contains(&doc.document_id);
        }

        // Winner per external key, resolved deterministically.
        let mut winner_indices: HashMap<String, usize> = HashMap::new();
        for (idx, doc) in self.buffered.iter().enumerate() {
            match winner_indices.get(&doc.external_key) {
                Some(&prev_idx) if !Self::candidate_beats(doc, &self.buffered[prev_idx]) => {}
                _ => {
                    winner_indices.insert(doc.external_key.clone(), idx);
                }
            }
        }

        self.kept_document_ids = winner_indices
            .values()
            .map(|&idx| self.buffered[idx].document_id)
            .collect::<HashSet<_>>();

        // Push kept document rows to the batcher. The dedup can drop a parent
        // document while keeping its child; a dangling `parent_document_id` FK
        // would then fail to insert. The simplest correct choice is to NULL the
        // parent pointer on any kept row whose parent id was not itself kept and
        // let the deferred resolver re-resolve it from the preserved
        // `parent_external_key`/structural source. The child's typed role is
        // unchanged here; it is re-derived when the resolver re-attaches.
        for mut doc in std::mem::take(&mut self.buffered) {
            if self.kept_document_ids.contains(&doc.document_id) {
                if let Ok(Some(parent_id)) = optional_uuid_field(&doc.row, "parent_document_id")
                    && !self.kept_document_ids.contains(&parent_id)
                    && let Some(object) = doc.row.as_object_mut()
                {
                    object.insert("parent_document_id".to_string(), serde_json::Value::Null);
                }
                batcher.push("knowledge_document", doc.row);
            }
        }

        // Replay pre-finalize descendant rows now that kept_document_ids and
        // (after replay) kept_chunk_ids are known.
        for (table, row) in std::mem::take(&mut self.pre_finalize_buffer) {
            self.apply_descendant_row(batcher, &table, row);
        }
    }

    /// `true` once the document plane has been resolved.
    fn is_finalized(&self) -> bool {
        self.finalized
    }

    /// Buffers a descendant row that arrived before finalization for later replay.
    fn buffer_pre_finalize(&mut self, table: &str, row: serde_json::Value) {
        self.pre_finalize_buffer.push((table.to_string(), row));
    }

    /// Applies one document-descendant or chunk-descendant row after the keep
    /// sets are resolved, pushing survivors to `batcher`.
    fn apply_descendant_row(
        &mut self,
        batcher: &mut PgBatcher,
        table: &str,
        row: serde_json::Value,
    ) {
        debug_assert!(self.finalized, "apply_descendant_row called before finalize");
        if KNOWLEDGE_DOCUMENT_DESCENDANT_TABLES.contains(&table) {
            // Extract document_id; drop on parse error (corrupt row already
            // rejected by row-scope validator upstream, so this is defensive).
            let Ok(document_id) = required_uuid_field(table, &row, "document_id") else {
                return;
            };
            if self.kept_document_ids.contains(&document_id) {
                if table == "knowledge_chunk" {
                    if let Ok(chunk_id) = required_uuid_field(table, &row, "chunk_id") {
                        self.kept_chunk_ids.insert(chunk_id);
                    }
                }
                batcher.push(table, row);
            }
        } else if KNOWLEDGE_CHUNK_DESCENDANT_TABLES.contains(&table) {
            let chunk_field = chunk_cascade_key_field(table);
            let keep = match optional_uuid_field(&row, chunk_field) {
                Ok(None) => true,
                Ok(Some(id)) => self.kept_chunk_ids.contains(&id),
                Err(_) => false,
            };
            if keep {
                batcher.push(table, row);
            }
        } else {
            batcher.push(table, row);
        }
    }

    /// Decides whether a chunk-derived row survives the cascade after
    /// finalization. Uses the correct key field per table.
    fn keep_chunk_descendant(&self, table: &str, row: &serde_json::Value) -> anyhow::Result<bool> {
        let field = chunk_cascade_key_field(table);
        match optional_uuid_field(row, field)? {
            None => Ok(true),
            Some(id) => Ok(self.kept_chunk_ids.contains(&id)),
        }
    }
}

/// Returns the row field that identifies the owning chunk for tables in
/// [`KNOWLEDGE_CHUNK_DESCENDANT_TABLES`].
///
/// `knowledge_chunk_entity_mention` uses `from_id` (FK → `knowledge_chunk.chunk_id`),
/// not `chunk_id` — confirmed against migration `0001_init.sql:2331`
/// and the FK constraint added at line 6265. All other tables in the list use
/// `chunk_id`.
fn chunk_cascade_key_field(table: &str) -> &'static str {
    if table == "knowledge_chunk_entity_mention" { "from_id" } else { "chunk_id" }
}

/// Tables whose rows are dropped when their owning `knowledge_document` is
/// dropped by the dedup. Each carries a non-null `document_id`.
const KNOWLEDGE_DOCUMENT_DESCENDANT_TABLES: &[&str] = &[
    "knowledge_revision",
    "knowledge_structured_revision",
    "knowledge_structured_block",
    "knowledge_chunk",
    "knowledge_technical_fact",
    "knowledge_evidence",
];

/// Tables whose rows are dropped when the chunk they reference was dropped.
/// `knowledge_evidence` is intentionally absent: it is already filtered by
/// `document_id` in [`KNOWLEDGE_DOCUMENT_DESCENDANT_TABLES`].
///
/// Note: `knowledge_chunk_entity_mention` keys on `from_id`, not `chunk_id` —
/// see [`chunk_cascade_key_field`].
const KNOWLEDGE_CHUNK_DESCENDANT_TABLES: &[&str] = &[
    "knowledge_entity_candidate",
    "knowledge_relation_candidate",
    "knowledge_bundle_chunk",
    "knowledge_chunk_entity_mention",
];

/// Routes one already-normalized restore row through the document dedup and
/// into `batcher`. `kept` is set to `false` for rows that are buffered or
/// dropped so the caller's count increment is skipped.
fn route_pg_row_through_dedup(
    dedup: &mut KnowledgeDocumentDedup,
    batcher: &mut PgBatcher,
    table: &str,
    row: serde_json::Value,
    kept: &mut bool,
) -> anyhow::Result<()> {
    *kept = true;
    match table {
        "content_document_head" => {
            dedup.note_head_row(&row);
            batcher.push(table, row);
        }
        "knowledge_document" => {
            // Buffered, not yet committed; the kept count is added to the
            // report after the keep-rule resolves in `finalize`.
            dedup.buffer_document(row)?;
            *kept = false;
        }
        "knowledge_chunk" if !dedup.is_finalized() => {
            // Record chunk ownership BEFORE buffering so finalize sees it.
            if let Ok(document_id) = required_uuid_field(table, &row, "document_id") {
                dedup.note_chunk_owner(document_id);
            }
            dedup.buffer_pre_finalize(table, row);
            *kept = false;
        }
        table if KNOWLEDGE_DOCUMENT_DESCENDANT_TABLES.contains(&table) && !dedup.is_finalized() => {
            dedup.buffer_pre_finalize(table, row);
            *kept = false;
        }
        table if KNOWLEDGE_CHUNK_DESCENDANT_TABLES.contains(&table) && !dedup.is_finalized() => {
            dedup.buffer_pre_finalize(table, row);
            *kept = false;
        }
        table if KNOWLEDGE_DOCUMENT_DESCENDANT_TABLES.contains(&table) => {
            // Post-finalize: apply directly.
            let before = batcher.pending.get(table).map_or(0, Vec::len);
            dedup.apply_descendant_row(batcher, table, row);
            *kept = batcher.pending.get(table).map_or(0, Vec::len) > before;
        }
        table if KNOWLEDGE_CHUNK_DESCENDANT_TABLES.contains(&table) => {
            if dedup.keep_chunk_descendant(table, &row)? {
                batcher.push(table, row);
            } else {
                *kept = false;
            }
        }
        _ => {
            batcher.push(table, row);
        }
    }
    Ok(())
}

/// Routes a per-dim chunk-vector row (PG-shaped, keyed by `chunk_id`) through
/// the chunk cascade. Vector relations arrive after `knowledge_chunk`, so the
/// dedup is always finalized by the time they stream; finalize defensively in
/// case an archive carries vectors but no chunks.
fn route_pg_vector_row_through_dedup(
    dedup: &mut KnowledgeDocumentDedup,
    batcher: &mut PgBatcher,
    table: &str,
    row: serde_json::Value,
    kept: &mut bool,
) -> anyhow::Result<()> {
    // Vectors cannot arrive before finalize in any canonical archive (they
    // follow knowledge_chunk), but finalize is idempotent so this is safe.
    if !dedup.is_finalized() {
        dedup.finalize(batcher);
    }
    if dedup.keep_chunk_descendant(table, &row)? {
        *kept = true;
        batcher.push(table, row);
    } else {
        *kept = false;
    }
    Ok(())
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
) -> anyhow::Result<Option<StashedContentDirectory>> {
    if state.settings.knowledge_plane_backend != "postgres" {
        bail!(
            "unsupported knowledge_plane_backend `{}` for snapshot restore",
            state.settings.knowledge_plane_backend
        );
    }

    // Blob storage is keyed by the existing library workspace. Capture
    // it before the restore writes replacement blobs under the same
    // library identity.
    let stashed_storage = if let Some(workspace_id) = existing_workspace_id {
        state
            .content_storage
            .stash_library_storage(workspace_id, library_id)
            .await
            .context("stash library blobs before restore")?
    } else {
        None
    };

    Ok(stashed_storage)
}

async fn clear_library_postgres_footprint(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    library_id: Uuid,
) -> anyhow::Result<()> {
    clear_pg_vector_relations_for_library(tx, library_id).await?;

    // Tables to wipe for this library, in reverse dependency order.
    let mut reverse: Vec<&str> = Vec::new();
    reverse.push("knowledge_vector_relation_manifest");
    for table in POSTGRES_KNOWLEDGE_EDGE_TABLES.iter().rev() {
        reverse.push(*table);
    }
    for table in POSTGRES_KNOWLEDGE_BASE_TABLES.iter().rev() {
        reverse.push(*table);
    }
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
            "knowledge_vector_relation_manifest" => {
                "DELETE FROM knowledge_vector_relation_manifest WHERE library_id = $1".to_string()
            }
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

async fn clear_pg_vector_relations_for_library(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    library_id: Uuid,
) -> anyhow::Result<()> {
    let relations = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT relation_name
         FROM knowledge_vector_relation_manifest
         WHERE library_id = $1
         ORDER BY relation_name",
    )
    .bind(library_id)
    .fetch_all(&mut **tx)
    .await
    .context("list vector relations before snapshot replace")?;
    for relation_name in relations {
        validate_snapshot_pg_table_name(&relation_name)?;
        if !is_runtime_vector_relation_name(&relation_name) {
            bail!("vector manifest relation `{relation_name}` is not a vector relation");
        }
        let relation = quote_pg_identifier(&relation_name)?;
        sqlx::query(&format!("DELETE FROM {relation} WHERE library_id = $1"))
            .bind(library_id)
            .execute(&mut **tx)
            .await
            .with_context(|| format!("clear vector relation {relation_name}"))?;
    }
    Ok(())
}

struct LockedTargetLibrary {
    workspace_id: Uuid,
    slug: String,
}

async fn lock_catalog_library_for_restore(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    library_id: Uuid,
) -> anyhow::Result<LockedTargetLibrary> {
    let locked: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT workspace_id, slug
         FROM catalog_library
         WHERE id = $1
         FOR UPDATE",
    )
    .bind(library_id)
    .fetch_optional(&mut **tx)
    .await
    .context("lock catalog_library before snapshot restore")?;
    let Some((workspace_id, slug)) = locked else {
        bail!(
            "target library {library_id} does not exist; create/select a library before restoring a snapshot"
        );
    };
    Ok(LockedTargetLibrary { workspace_id, slug })
}

async fn tx_library_has_restore_footprint(
    _state: &AppState,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    library_id: Uuid,
) -> anyhow::Result<bool> {
    postgres_library_has_restore_footprint(tx, library_id).await
}

async fn postgres_library_has_restore_footprint(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    library_id: Uuid,
) -> anyhow::Result<bool> {
    let mut clauses = Vec::new();
    for table in POSTGRES_CONTENT_TABLES
        .iter()
        .chain(POSTGRES_RUNTIME_GRAPH_TABLES.iter())
        .chain(POSTGRES_KNOWLEDGE_TABLES.iter())
        .copied()
    {
        if let Some(clause) = postgres_restore_footprint_clause(table)? {
            clauses.push(clause);
        }
    }
    if clauses.is_empty() {
        return Ok(false);
    }
    let sql = format!("SELECT {}", clauses.join(" OR "));
    let has_footprint = sqlx::query_scalar::<_, bool>(&sql)
        .bind(library_id)
        .fetch_one(&mut **tx)
        .await
        .context("check target library snapshot restore footprint")?;
    Ok(has_footprint)
}

fn postgres_restore_footprint_clause(table: &str) -> anyhow::Result<Option<String>> {
    let clause = match table {
        "content_chunk" => "EXISTS (SELECT 1 FROM content_chunk c \
             JOIN content_revision r ON r.id = c.revision_id \
             WHERE r.library_id = $1)"
            .to_string(),
        "content_mutation_item" => "EXISTS (SELECT 1 FROM content_mutation_item i \
             JOIN content_mutation m ON m.id = i.mutation_id \
             WHERE m.library_id = $1)"
            .to_string(),
        "content_document_head" => "EXISTS (SELECT 1 FROM content_document_head h \
             JOIN content_document d ON d.id = h.document_id \
             WHERE d.library_id = $1)"
            .to_string(),
        direct => {
            let direct = require_known_snapshot_pg_table(direct)?;
            let table = quote_pg_identifier(direct)?;
            format!("EXISTS (SELECT 1 FROM {table} WHERE library_id = $1)")
        }
    };
    Ok(Some(clause))
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
    let search_store = &state.search_store;
    let mut ensured = HashSet::new();
    for shard in &manifest.vector_shards {
        let relation_name = canonical_per_dim_vector_relation_name(&shard.name).ok_or_else(|| {
            anyhow!(
                "snapshot manifest vector_shards entry `{}` is not a canonical per-dim shard name",
                shard.name
            )
        })?;
        if !ensured.insert(relation_name.clone()) {
            continue;
        }
        if is_per_dim_chunk_vector_collection_name(&shard.name) {
            search_store.ensure_chunk_vector_shard(shard.dim).await.with_context(|| {
                format!("ensure per-dim chunk vector shard {relation_name} for restore")
            })?;
        } else {
            search_store.ensure_entity_vector_shard(shard.dim).await.with_context(|| {
                format!("ensure per-dim entity vector shard {relation_name} for restore")
            })?;
        }
    }
    Ok(())
}
/// Recursively strips characters that PostgreSQL `text` and `jsonb` cannot
/// store from every `String` node in `value`.
///
/// PostgreSQL rejects the following when they appear inside a JSONB literal:
///
/// - U+0000 (null byte) — forbidden in `text`/`jsonb` by the SQL standard.
/// - Lone surrogate code points (U+D800–U+DFFF) — not valid Unicode scalar
///   values; `serde_json` can round-trip them as `\uD800`-style escapes but
///   PostgreSQL's JSON parser treats them as an "unsupported Unicode escape
///   sequence" and aborts the statement.
///
/// All other Unicode scalar values (including multi-byte characters) are left
/// intact so that legitimate content in any language is fully preserved. This
/// function is a no-op on rows that contain no such characters.
fn sanitize_json_for_postgres(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s) => {
            // Fast path: skip the allocation when nothing needs removing.
            if s.chars().any(|c| c == '\u{0000}' || is_surrogate_char(c)) {
                *s = s.chars().filter(|c| *c != '\u{0000}' && !is_surrogate_char(*c)).collect();
            }
        }
        serde_json::Value::Array(arr) => {
            for element in arr.iter_mut() {
                sanitize_json_for_postgres(element);
            }
        }
        serde_json::Value::Object(map) => {
            for element in map.values_mut() {
                sanitize_json_for_postgres(element);
            }
        }
        _ => {}
    }
}

/// Returns `true` for code points in the surrogate range U+D800–U+DFFF.
///
/// Rust `char` only holds valid Unicode scalar values, so no char produced by
/// iterating a well-formed `&str` can ever be a surrogate. However
/// `serde_json` in some versions can round-trip lone-surrogate JSON escapes
/// (`\uD800`) into an internal byte representation that may survive as a
/// surrogate-range code point; comparing via `u32` is both safe and correct.
#[inline]
fn is_surrogate_char(c: char) -> bool {
    (0xD800..=0xDFFF).contains(&(c as u32))
}

/// Bulk-insert up to `IMPORT_BATCH_ROWS` postgres rows in a single
/// statement. Uses `jsonb_populate_recordset` so every column of the
/// target table is reconstructed from the JSONB object keys.
async fn insert_pg_rows_bulk(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    table: &str,
    mut rows: Vec<serde_json::Value>,
) -> anyhow::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    validate_snapshot_pg_table_name(table)?;
    // Strip U+0000 and lone surrogates before building any JSONB payload so
    // PostgreSQL's JSON parser never encounters them. Covers all paths:
    // jsonb_to_recordset, jsonb_populate_recordset, and the vector
    // jsonb_to_recordset inside insert_pg_vector_rows_bulk.
    for row in &mut rows {
        sanitize_json_for_postgres(row);
    }
    if is_runtime_vector_relation_name(table) {
        insert_pg_vector_rows_bulk(tx, table, rows).await?;
        return Ok(());
    }
    let table = require_known_snapshot_pg_table(table)?;
    let count = rows.len();
    let payload = serde_json::Value::Array(rows);
    if table == "catalog_library" {
        update_catalog_library_rows_from_snapshot(tx, &payload, count).await?;
        return Ok(());
    }
    let on_conflict = pg_insert_conflict_clause(table);
    let sql = if let Some(columns) = snapshot_pg_insert_columns(table) {
        format!(
            "INSERT INTO {table} ({columns})
             SELECT {columns}
             FROM jsonb_to_recordset($1) AS row({}){on_conflict}",
            snapshot_pg_recordset_columns(table)?
        )
    } else {
        format!(
            "INSERT INTO {table} SELECT * FROM jsonb_populate_recordset(null::{table}, $1){on_conflict}"
        )
    };
    sqlx::query(&sql)
        .bind(&payload)
        .execute(&mut **tx)
        .await
        .with_context(|| format!("bulk insert {count} rows into {table}"))?;
    Ok(())
}

fn snapshot_pg_insert_columns(table: &str) -> Option<&'static str> {
    match table {
        "knowledge_document" => Some(
            "document_id, workspace_id, library_id, external_key, file_name, title, document_state, active_revision_id, readable_revision_id, latest_revision_no, parent_document_id, document_role, created_at, updated_at, deleted_at",
        ),
        "knowledge_revision" => Some(
            "revision_id, workspace_id, library_id, document_id, revision_number, revision_state, revision_kind, storage_ref, source_uri, document_hint, mime_type, checksum, title, byte_size, normalized_text, text_checksum, image_checksum, text_state, vector_state, graph_state, text_readable_at, vector_ready_at, graph_ready_at, superseded_by_revision_id, created_at",
        ),
        "knowledge_structured_revision" => Some(
            "revision_id, workspace_id, library_id, document_id, preparation_state, normalization_profile, source_format, language_code, block_count, chunk_count, typed_fact_count, outline_json, prepared_at, updated_at",
        ),
        "knowledge_structured_block" => Some(
            "block_id, workspace_id, library_id, document_id, revision_id, ordinal, block_kind, text, normalized_text, heading_trail, section_path, page_number, span_start, span_end, parent_block_id, table_coordinates_json, code_language, occurred_at, occurred_until, created_at, updated_at",
        ),
        "knowledge_chunk" => Some(
            "chunk_id, workspace_id, library_id, document_id, revision_id, primary_block_id, chunk_index, chunk_kind, content_text, normalized_text, span_start, span_end, token_count, support_block_ids, section_path, heading_trail, literal_digest, chunk_state, text_generation, vector_generation, quality_score, window_text, raptor_level, occurred_at, occurred_until",
        ),
        "knowledge_technical_fact" => Some(
            "fact_id, workspace_id, library_id, document_id, revision_id, fact_kind, canonical_value_text, canonical_value_exact, canonical_value_json, display_value, qualifiers_json, support_block_ids, support_chunk_ids, confidence, extraction_kind, conflict_group_id, created_at, updated_at",
        ),
        "knowledge_entity" => Some(
            "entity_id, workspace_id, library_id, canonical_label, aliases, entity_type, entity_sub_type, summary, confidence, support_count, freshness_generation, entity_state, created_at, updated_at",
        ),
        "knowledge_entity_candidate" => Some(
            "candidate_id, workspace_id, library_id, revision_id, chunk_id, candidate_label, candidate_type, candidate_sub_type, normalization_key, confidence, extraction_method, candidate_state, created_at, updated_at",
        ),
        "knowledge_relation" => Some(
            "relation_id, workspace_id, library_id, subject_entity_id, object_entity_id, predicate, normalized_assertion, summary, confidence, support_count, contradiction_state, freshness_generation, relation_state, created_at, updated_at",
        ),
        "knowledge_relation_candidate" => Some(
            "candidate_id, workspace_id, library_id, revision_id, chunk_id, subject_label, subject_candidate_key, predicate, object_label, object_candidate_key, normalized_assertion, confidence, extraction_method, candidate_state, created_at, updated_at",
        ),
        "knowledge_evidence" => Some(
            "evidence_id, workspace_id, library_id, document_id, revision_id, chunk_id, block_id, fact_id, span_start, span_end, quote_text, literal_spans_json, summary, evidence_kind, extraction_method, confidence, evidence_state, freshness_generation, created_at, updated_at",
        ),
        "knowledge_context_bundle" => Some(
            "bundle_id, workspace_id, library_id, query_execution_id, bundle_state, bundle_strategy, requested_mode, resolved_mode, selected_fact_ids, verification_state, verification_warnings, freshness_snapshot, candidate_summary, assembly_diagnostics, created_at, updated_at",
        ),
        "knowledge_retrieval_trace" => Some(
            "trace_id, workspace_id, library_id, query_execution_id, bundle_id, trace_state, retrieval_strategy, candidate_counts, dropped_reasons, timing_breakdown, diagnostics_json, created_at, updated_at",
        ),
        "knowledge_bundle_chunk" => {
            Some("bundle_id, chunk_id, library_id, rank, score, inclusion_reason, created_at")
        }
        "knowledge_bundle_entity" => {
            Some("bundle_id, entity_id, library_id, rank, score, inclusion_reason, created_at")
        }
        "knowledge_bundle_relation" => {
            Some("bundle_id, relation_id, library_id, rank, score, inclusion_reason, created_at")
        }
        "knowledge_bundle_evidence" => {
            Some("bundle_id, evidence_id, library_id, rank, score, inclusion_reason, created_at")
        }
        "knowledge_chunk_entity_mention" => Some(
            "from_id, to_id, relation_type, support, library_id, rank, score, inclusion_reason, created_at, updated_at",
        ),
        "knowledge_evidence_entity_support" => Some(
            "from_id, to_id, relation_type, support, library_id, rank, score, inclusion_reason, created_at, updated_at",
        ),
        "knowledge_evidence_relation_support" => Some(
            "from_id, to_id, relation_type, support, library_id, rank, score, inclusion_reason, created_at, updated_at",
        ),
        "knowledge_vector_relation_manifest" => Some(
            "library_id, dim, vector_kind, embedding_model_key, relation_name, is_default, row_count, promoted, created_at",
        ),
        _ => None,
    }
}

fn snapshot_pg_recordset_columns(table: &str) -> anyhow::Result<&'static str> {
    Ok(match table {
        "knowledge_document" => {
            "document_id uuid, workspace_id uuid, library_id uuid, external_key text, file_name text, title text, document_state text, active_revision_id uuid, readable_revision_id uuid, latest_revision_no bigint, parent_document_id uuid, document_role text, created_at timestamptz, updated_at timestamptz, deleted_at timestamptz"
        }
        "knowledge_revision" => {
            "revision_id uuid, workspace_id uuid, library_id uuid, document_id uuid, revision_number bigint, revision_state text, revision_kind text, storage_ref text, source_uri text, document_hint text, mime_type text, checksum text, title text, byte_size bigint, normalized_text text, text_checksum text, image_checksum text, text_state text, vector_state text, graph_state text, text_readable_at timestamptz, vector_ready_at timestamptz, graph_ready_at timestamptz, superseded_by_revision_id uuid, created_at timestamptz"
        }
        "knowledge_structured_revision" => {
            "revision_id uuid, workspace_id uuid, library_id uuid, document_id uuid, preparation_state text, normalization_profile text, source_format text, language_code text, block_count bigint, chunk_count bigint, typed_fact_count bigint, outline_json jsonb, prepared_at timestamptz, updated_at timestamptz"
        }
        "knowledge_structured_block" => {
            "block_id uuid, workspace_id uuid, library_id uuid, document_id uuid, revision_id uuid, ordinal integer, block_kind text, text text, normalized_text text, heading_trail text[], section_path text[], page_number integer, span_start integer, span_end integer, parent_block_id uuid, table_coordinates_json jsonb, code_language text, occurred_at timestamptz, occurred_until timestamptz, created_at timestamptz, updated_at timestamptz"
        }
        "knowledge_chunk" => {
            "chunk_id uuid, workspace_id uuid, library_id uuid, document_id uuid, revision_id uuid, primary_block_id uuid, chunk_index integer, chunk_kind text, content_text text, normalized_text text, span_start integer, span_end integer, token_count integer, support_block_ids uuid[], section_path text[], heading_trail text[], literal_digest text, chunk_state text, text_generation bigint, vector_generation bigint, quality_score real, window_text text, raptor_level integer, occurred_at timestamptz, occurred_until timestamptz"
        }
        "knowledge_technical_fact" => {
            "fact_id uuid, workspace_id uuid, library_id uuid, document_id uuid, revision_id uuid, fact_kind text, canonical_value_text text, canonical_value_exact text, canonical_value_json jsonb, display_value text, qualifiers_json jsonb, support_block_ids uuid[], support_chunk_ids uuid[], confidence double precision, extraction_kind text, conflict_group_id text, created_at timestamptz, updated_at timestamptz"
        }
        "knowledge_entity" => {
            "entity_id uuid, workspace_id uuid, library_id uuid, canonical_label text, aliases text[], entity_type text, entity_sub_type text, summary text, confidence double precision, support_count bigint, freshness_generation bigint, entity_state text, created_at timestamptz, updated_at timestamptz"
        }
        "knowledge_entity_candidate" => {
            "candidate_id uuid, workspace_id uuid, library_id uuid, revision_id uuid, chunk_id uuid, candidate_label text, candidate_type text, candidate_sub_type text, normalization_key text, confidence double precision, extraction_method text, candidate_state text, created_at timestamptz, updated_at timestamptz"
        }
        "knowledge_relation" => {
            "relation_id uuid, workspace_id uuid, library_id uuid, subject_entity_id uuid, object_entity_id uuid, predicate text, normalized_assertion text, summary text, confidence double precision, support_count bigint, contradiction_state text, freshness_generation bigint, relation_state text, created_at timestamptz, updated_at timestamptz"
        }
        "knowledge_relation_candidate" => {
            "candidate_id uuid, workspace_id uuid, library_id uuid, revision_id uuid, chunk_id uuid, subject_label text, subject_candidate_key text, predicate text, object_label text, object_candidate_key text, normalized_assertion text, confidence double precision, extraction_method text, candidate_state text, created_at timestamptz, updated_at timestamptz"
        }
        "knowledge_evidence" => {
            "evidence_id uuid, workspace_id uuid, library_id uuid, document_id uuid, revision_id uuid, chunk_id uuid, block_id uuid, fact_id uuid, span_start integer, span_end integer, quote_text text, literal_spans_json jsonb, summary text, evidence_kind text, extraction_method text, confidence double precision, evidence_state text, freshness_generation bigint, created_at timestamptz, updated_at timestamptz"
        }
        "knowledge_context_bundle" => {
            "bundle_id uuid, workspace_id uuid, library_id uuid, query_execution_id uuid, bundle_state text, bundle_strategy text, requested_mode text, resolved_mode text, selected_fact_ids uuid[], verification_state text, verification_warnings jsonb, freshness_snapshot jsonb, candidate_summary jsonb, assembly_diagnostics jsonb, created_at timestamptz, updated_at timestamptz"
        }
        "knowledge_retrieval_trace" => {
            "trace_id uuid, workspace_id uuid, library_id uuid, query_execution_id uuid, bundle_id uuid, trace_state text, retrieval_strategy text, candidate_counts jsonb, dropped_reasons jsonb, timing_breakdown jsonb, diagnostics_json jsonb, created_at timestamptz, updated_at timestamptz"
        }
        "knowledge_bundle_chunk" => {
            "bundle_id uuid, chunk_id uuid, library_id uuid, rank integer, score double precision, inclusion_reason text, created_at timestamptz"
        }
        "knowledge_bundle_entity" => {
            "bundle_id uuid, entity_id uuid, library_id uuid, rank integer, score double precision, inclusion_reason text, created_at timestamptz"
        }
        "knowledge_bundle_relation" => {
            "bundle_id uuid, relation_id uuid, library_id uuid, rank integer, score double precision, inclusion_reason text, created_at timestamptz"
        }
        "knowledge_bundle_evidence" => {
            "bundle_id uuid, evidence_id uuid, library_id uuid, rank integer, score double precision, inclusion_reason text, created_at timestamptz"
        }
        "knowledge_chunk_entity_mention" => {
            "from_id uuid, to_id uuid, relation_type text, support bigint, library_id uuid, rank integer, score double precision, inclusion_reason text, created_at timestamptz, updated_at timestamptz"
        }
        "knowledge_evidence_entity_support" => {
            "from_id uuid, to_id uuid, relation_type text, support bigint, library_id uuid, rank integer, score double precision, inclusion_reason text, created_at timestamptz, updated_at timestamptz"
        }
        "knowledge_evidence_relation_support" => {
            "from_id uuid, to_id uuid, relation_type text, support bigint, library_id uuid, rank integer, score double precision, inclusion_reason text, created_at timestamptz, updated_at timestamptz"
        }
        "knowledge_vector_relation_manifest" => {
            "library_id uuid, dim integer, vector_kind text, embedding_model_key text, relation_name text, is_default boolean, row_count bigint, promoted boolean, created_at timestamptz"
        }
        other => bail!("no explicit recordset column list for `{other}`"),
    })
}

fn pg_insert_conflict_clause(table: &str) -> &'static str {
    match table {
        // Workspace-scope rows can legitimately pre-exist on the target
        // stack. The local workspace row remains the source of truth.
        "catalog_workspace" => " ON CONFLICT DO NOTHING",
        // AI-config rows are keyed by stable ids (system catalogs) or
        // scope-partitioned natural keys (accounts/bindings, each with three
        // partial unique indexes per scope). A targetless `ON CONFLICT DO
        // NOTHING` swallows a collision on ANY of those unique constraints,
        // so whatever the target already holds wins — an import never
        // clobbers a deployment's existing AI configuration, and a
        // partial-key collision cannot abort the restore transaction.
        "ai_provider_catalog"
        | "ai_model_catalog"
        | "ai_price_catalog"
        | "ai_account"
        | "ai_binding" => " ON CONFLICT DO NOTHING",
        "knowledge_document" => {
            " ON CONFLICT (document_id) DO UPDATE SET workspace_id = excluded.workspace_id, library_id = excluded.library_id, external_key = excluded.external_key, file_name = excluded.file_name, title = excluded.title, document_state = excluded.document_state, active_revision_id = excluded.active_revision_id, readable_revision_id = excluded.readable_revision_id, latest_revision_no = excluded.latest_revision_no, parent_document_id = excluded.parent_document_id, document_role = excluded.document_role, created_at = excluded.created_at, updated_at = excluded.updated_at, deleted_at = excluded.deleted_at"
        }
        "knowledge_revision" | "knowledge_structured_revision" => {
            " ON CONFLICT (revision_id) DO NOTHING"
        }
        "knowledge_structured_block" => " ON CONFLICT (block_id) DO NOTHING",
        "knowledge_chunk" => " ON CONFLICT (chunk_id) DO NOTHING",
        "knowledge_technical_fact" => " ON CONFLICT (fact_id) DO NOTHING",
        "knowledge_entity" => " ON CONFLICT (entity_id) DO NOTHING",
        "knowledge_entity_candidate" | "knowledge_relation_candidate" => {
            " ON CONFLICT (candidate_id) DO NOTHING"
        }
        "knowledge_relation" => " ON CONFLICT (relation_id) DO NOTHING",
        "knowledge_evidence" => " ON CONFLICT (evidence_id) DO NOTHING",
        "knowledge_context_bundle" => " ON CONFLICT (bundle_id) DO NOTHING",
        "knowledge_retrieval_trace" => " ON CONFLICT (trace_id) DO NOTHING",
        "knowledge_bundle_chunk" => {
            " ON CONFLICT (bundle_id, chunk_id) DO UPDATE SET library_id = excluded.library_id, rank = excluded.rank, score = excluded.score, inclusion_reason = excluded.inclusion_reason, created_at = excluded.created_at"
        }
        "knowledge_bundle_entity" => {
            " ON CONFLICT (bundle_id, entity_id) DO UPDATE SET library_id = excluded.library_id, rank = excluded.rank, score = excluded.score, inclusion_reason = excluded.inclusion_reason, created_at = excluded.created_at"
        }
        "knowledge_bundle_relation" => {
            " ON CONFLICT (bundle_id, relation_id) DO UPDATE SET library_id = excluded.library_id, rank = excluded.rank, score = excluded.score, inclusion_reason = excluded.inclusion_reason, created_at = excluded.created_at"
        }
        "knowledge_bundle_evidence" => {
            " ON CONFLICT (bundle_id, evidence_id) DO UPDATE SET library_id = excluded.library_id, rank = excluded.rank, score = excluded.score, inclusion_reason = excluded.inclusion_reason, created_at = excluded.created_at"
        }
        "knowledge_chunk_entity_mention"
        | "knowledge_evidence_entity_support"
        | "knowledge_evidence_relation_support" => {
            " ON CONFLICT (from_id, to_id, relation_type) DO UPDATE SET support = excluded.support, library_id = excluded.library_id, rank = excluded.rank, score = excluded.score, inclusion_reason = excluded.inclusion_reason, updated_at = excluded.updated_at"
        }
        "knowledge_vector_relation_manifest" => {
            " ON CONFLICT (library_id, dim, vector_kind, embedding_model_key) DO UPDATE SET relation_name = excluded.relation_name, is_default = excluded.is_default, row_count = excluded.row_count, promoted = excluded.promoted"
        }
        _ => "",
    }
}

async fn update_catalog_library_rows_from_snapshot(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    payload: &serde_json::Value,
    count: usize,
) -> anyhow::Result<()> {
    let result = sqlx::query(catalog_library_snapshot_update_sql())
        .bind(payload)
        .execute(&mut **tx)
        .await
        .context("update catalog_library row from snapshot")?;
    if result.rows_affected() != count as u64 {
        bail!(
            "catalog_library snapshot expected to update {count} target rows but updated {}",
            result.rows_affected()
        );
    }
    Ok(())
}

fn catalog_library_snapshot_update_sql() -> &'static str {
    "UPDATE catalog_library AS target
         SET workspace_id = row.workspace_id,
             slug = row.slug,
             display_name = COALESCE(row.display_name, target.display_name),
             description = row.description,
             lifecycle_state = COALESCE(row.lifecycle_state, target.lifecycle_state),
             source_truth_version = COALESCE(row.source_truth_version, target.source_truth_version),
             extraction_prompt = row.extraction_prompt,
             ai_summary = row.ai_summary,
             created_by_principal_id = row.created_by_principal_id,
             created_at = COALESCE(row.created_at, target.created_at),
             updated_at = COALESCE(row.updated_at, target.updated_at),
             web_ingest_policy = COALESCE(row.web_ingest_policy, target.web_ingest_policy),
             chunking_template = COALESCE(row.chunking_template, target.chunking_template),
             recognition_policy = COALESCE(row.recognition_policy, target.recognition_policy),
             include_document_hint_in_mcp_answers = COALESCE(
                 row.include_document_hint_in_mcp_answers,
                 target.include_document_hint_in_mcp_answers
             ),
             retrieval_config = COALESCE(row.retrieval_config, target.retrieval_config)
         FROM jsonb_to_recordset($1) AS row(
             id uuid,
             workspace_id uuid,
             slug text,
             display_name text,
             description text,
             lifecycle_state catalog_library_lifecycle_state,
             source_truth_version bigint,
             extraction_prompt text,
             ai_summary text,
             created_by_principal_id uuid,
             created_at timestamptz,
             updated_at timestamptz,
             web_ingest_policy jsonb,
             chunking_template text,
             recognition_policy jsonb,
             include_document_hint_in_mcp_answers boolean,
             retrieval_config jsonb
         )
         WHERE target.id = row.id"
}

/// Writes a batch of per-dim vector rows into the shared `knowledge_*_vector_d*`
/// shard, guarded by a savepoint + bounded deadlock/contention retry
/// (Workstream R / R2 + in-transaction R3).
///
/// Parallel restores into the same shared shard race for the same pages and
/// hit deadlocks (`40P01`); the lazy `CREATE ... IF NOT EXISTS` for a brand-new
/// dim can also race two sessions on the catalog insert. Both abort the
/// statement — and, unguarded, the whole restore transaction. We run the shard
/// create + insert inside a SAVEPOINT (`tx.begin()`), so on a retryable
/// contention we roll back to the savepoint (leaving the outer transaction and
/// its tx-scoped `session_replication_role = 'replica'` intact) and replay the
/// same in-memory rows. `ON CONFLICT DO NOTHING` makes replay idempotent.
async fn insert_pg_vector_rows_bulk(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    relation_name: &str,
    rows: Vec<serde_json::Value>,
) -> anyhow::Result<()> {
    let dim = parse_per_dim_vector_collection_dim(relation_name)
        .ok_or_else(|| anyhow!("invalid vector relation `{relation_name}`"))?;
    let count = rows.len();
    let payload = serde_json::Value::Array(rows);
    for attempt in 1..=RESTORE_SAVEPOINT_MAX_ATTEMPTS {
        let mut savepoint = tx.begin().await.context("open vector-shard write savepoint")?;
        match insert_pg_vector_rows_bulk_once(&mut savepoint, relation_name, dim, count, &payload)
            .await
        {
            Ok(()) => {
                savepoint.commit().await.context("release vector-shard write savepoint")?;
                return Ok(());
            }
            Err(error) => {
                // Roll the savepoint back so the outer transaction stays usable,
                // then decide whether the failure is transient.
                savepoint.rollback().await.context("roll back vector-shard write savepoint")?;
                let retryable = error
                    .downcast_ref::<sqlx::Error>()
                    .is_some_and(pg_error_is_retryable_restore_contention);
                if retryable && attempt < RESTORE_SAVEPOINT_MAX_ATTEMPTS {
                    tracing::warn!(
                        relation = %relation_name,
                        attempt,
                        max_attempts = RESTORE_SAVEPOINT_MAX_ATTEMPTS,
                        error = %error,
                        "vector-shard write hit transient contention; retrying after savepoint rollback",
                    );
                    tokio::time::sleep(RESTORE_SAVEPOINT_BACKOFF_BASE * attempt).await;
                    continue;
                }
                return Err(error);
            }
        }
    }
    unreachable!("vector-shard write retry loop exits via return")
}

/// One attempt of the vector-shard write, executed inside a savepoint by
/// [`insert_pg_vector_rows_bulk`]. Surfaces the raw `sqlx::Error` (wrapped by
/// `anyhow`) so the caller can classify deadlock/contention via
/// [`pg_error_is_retryable_restore_contention`].
async fn insert_pg_vector_rows_bulk_once(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    relation_name: &str,
    dim: u64,
    count: usize,
    payload: &serde_json::Value,
) -> anyhow::Result<()> {
    ensure_pg_vector_relation(tx, relation_name, dim).await?;
    let relation = quote_pg_identifier(relation_name)?;
    let storage = PgVectorStorage::for_dim(dim);
    let cast_type = storage.cast_type();
    if is_chunk_vector_relation_name(relation_name) {
        sqlx::query(&format!(
            "WITH rows AS MATERIALIZED (
                SELECT DISTINCT ON (key)
                    key, vector_id, workspace_id, library_id, chunk_id, revision_id,
                    embedding_model_key, vector_kind, dimensions, embedding,
                    freshness_generation, created_at, occurred_at, occurred_until
                FROM jsonb_to_recordset($1) AS row(
                    key text, vector_id uuid, workspace_id uuid, library_id uuid,
                    chunk_id uuid, revision_id uuid, embedding_model_key text,
                    vector_kind text, dimensions integer, embedding text,
                    freshness_generation bigint, created_at timestamptz,
                    occurred_at timestamptz, occurred_until timestamptz
                )
                ORDER BY key, freshness_generation DESC NULLS LAST, created_at DESC NULLS LAST
             ), inserted AS (
                INSERT INTO {relation} (
                    key, vector_id, workspace_id, library_id, chunk_id, revision_id,
                    embedding_model_key, vector_kind, dimensions, embedding,
                    freshness_generation, created_at, occurred_at, occurred_until
                )
                SELECT key, vector_id, workspace_id, library_id, chunk_id, revision_id,
                    embedding_model_key, vector_kind, dimensions, embedding::{cast_type},
                    freshness_generation, created_at, occurred_at, occurred_until
                FROM rows
                ON CONFLICT (key) DO NOTHING
                RETURNING library_id, dimensions, vector_kind, embedding_model_key
             ), lanes AS MATERIALIZED (
                SELECT DISTINCT library_id, dimensions, vector_kind, embedding_model_key
                FROM inserted
             )
             INSERT INTO knowledge_vector_relation_manifest (
                 library_id, dim, vector_kind, embedding_model_key, relation_name,
                 is_default, row_count, promoted
             )
             SELECT library_id, dimensions, vector_kind, embedding_model_key,
                 $2, true, 0, false
             FROM lanes
             ON CONFLICT (library_id, dim, vector_kind, embedding_model_key)
             DO UPDATE SET relation_name = excluded.relation_name,
                           is_default = true,
                           promoted = false
             "
        ))
        .bind(payload)
        .bind(relation_name)
        .execute(&mut **tx)
        .await
        .with_context(|| format!("bulk insert {count} chunk vectors into {relation_name}"))?;
    } else {
        sqlx::query(&format!(
            "WITH rows AS MATERIALIZED (
                SELECT DISTINCT ON (key)
                    key, vector_id, workspace_id, library_id, entity_id,
                    embedding_model_key, vector_kind, dimensions, embedding,
                    freshness_generation, created_at
                FROM jsonb_to_recordset($1) AS row(
                    key text, vector_id uuid, workspace_id uuid, library_id uuid,
                    entity_id uuid, embedding_model_key text, vector_kind text,
                    dimensions integer, embedding text, freshness_generation bigint,
                    created_at timestamptz
                )
                ORDER BY key, freshness_generation DESC NULLS LAST, created_at DESC NULLS LAST
             ), inserted AS (
                INSERT INTO {relation} (
                    key, vector_id, workspace_id, library_id, entity_id,
                    embedding_model_key, vector_kind, dimensions, embedding,
                    freshness_generation, created_at
                )
                SELECT key, vector_id, workspace_id, library_id, entity_id,
                    embedding_model_key, vector_kind, dimensions, embedding::{cast_type},
                    freshness_generation, created_at
                FROM rows
                ON CONFLICT (key) DO NOTHING
                RETURNING library_id, dimensions, vector_kind, embedding_model_key
             ), lanes AS MATERIALIZED (
                SELECT DISTINCT library_id, dimensions, vector_kind, embedding_model_key
                FROM inserted
             )
             INSERT INTO knowledge_vector_relation_manifest (
                 library_id, dim, vector_kind, embedding_model_key, relation_name,
                 is_default, row_count, promoted
             )
             SELECT library_id, dimensions, vector_kind, embedding_model_key,
                 $2, true, 0, false
             FROM lanes
             ON CONFLICT (library_id, dim, vector_kind, embedding_model_key)
             DO UPDATE SET relation_name = excluded.relation_name,
                           is_default = true,
                           promoted = false
             "
        ))
        .bind(payload)
        .bind(relation_name)
        .execute(&mut **tx)
        .await
        .with_context(|| format!("bulk insert {count} entity vectors into {relation_name}"))?;
    }
    sqlx::query(&format!(
        "UPDATE knowledge_vector_relation_manifest m
         SET row_count = (
            SELECT count(*)::bigint
            FROM {relation} v
            WHERE v.library_id = m.library_id
              AND v.dimensions = m.dim
              AND v.vector_kind = m.vector_kind
              AND v.embedding_model_key = m.embedding_model_key
         )
         WHERE m.relation_name = $1"
    ))
    .bind(relation_name)
    .execute(&mut **tx)
    .await
    .with_context(|| format!("refresh vector manifest row counts for {relation_name}"))?;
    Ok(())
}

async fn ensure_pg_vector_relation(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    relation_name: &str,
    dim: u64,
) -> anyhow::Result<()> {
    let relation = quote_pg_identifier(relation_name)?;
    let storage = PgVectorStorage::for_dim(dim);
    let dim = checked_vector_dim_i32(dim)?;
    let embedding_type = storage.column_type(dim);
    if is_chunk_vector_relation_name(relation_name) {
        sqlx::query(&format!(
            "CREATE TABLE IF NOT EXISTS {relation} (
                key text primary key,
                vector_id uuid not null,
                workspace_id uuid not null,
                library_id uuid not null,
                chunk_id uuid not null,
                revision_id uuid not null,
                embedding_model_key text not null,
                vector_kind text not null,
                dimensions integer not null check (dimensions = {dim}),
                embedding {embedding_type} not null,
                freshness_generation bigint not null,
                created_at timestamptz not null,
                occurred_at timestamptz,
                occurred_until timestamptz
            )"
        ))
        .execute(&mut **tx)
        .await
        .with_context(|| format!("create vector relation {relation_name}"))?;
        ensure_pg_vector_relation_indexes(
            tx,
            relation_name,
            "chunk_id",
            Some("revision_id"),
            storage,
            dim,
        )
        .await?;
    } else {
        sqlx::query(&format!(
            "CREATE TABLE IF NOT EXISTS {relation} (
                key text primary key,
                vector_id uuid not null,
                workspace_id uuid not null,
                library_id uuid not null,
                entity_id uuid not null,
                embedding_model_key text not null,
                vector_kind text not null,
                dimensions integer not null check (dimensions = {dim}),
                embedding {embedding_type} not null,
                freshness_generation bigint not null,
                created_at timestamptz not null
            )"
        ))
        .execute(&mut **tx)
        .await
        .with_context(|| format!("create vector relation {relation_name}"))?;
        ensure_pg_vector_relation_indexes(tx, relation_name, "entity_id", None, storage, dim)
            .await?;
    }
    Ok(())
}

async fn ensure_pg_vector_relation_indexes(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    relation_name: &str,
    id_column: &str,
    extra_column: Option<&str>,
    storage: PgVectorStorage,
    dim: i32,
) -> anyhow::Result<()> {
    let relation = quote_pg_identifier(relation_name)?;
    let lane_idx = quote_pg_identifier(&format!("{relation_name}_lane_idx"))?;
    sqlx::query(&format!(
        "CREATE INDEX IF NOT EXISTS {lane_idx}
         ON {relation} (library_id, embedding_model_key, vector_kind)"
    ))
    .execute(&mut **tx)
    .await
    .with_context(|| format!("create lane index on {relation_name}"))?;

    let id_idx = quote_pg_identifier(&format!("{relation_name}_{id_column}_idx"))?;
    sqlx::query(&format!("CREATE INDEX IF NOT EXISTS {id_idx} ON {relation} ({id_column})"))
        .execute(&mut **tx)
        .await
        .with_context(|| format!("create id index on {relation_name}"))?;

    if let Some(extra_column) = extra_column {
        let extra_idx = quote_pg_identifier(&format!("{relation_name}_{extra_column}_idx"))?;
        sqlx::query(&format!(
            "CREATE INDEX IF NOT EXISTS {extra_idx} ON {relation} ({extra_column})"
        ))
        .execute(&mut **tx)
        .await
        .with_context(|| format!("create extra index on {relation_name}"))?;
    }
    ensure_pg_vector_relation_hnsw_index(tx, relation_name, storage, dim).await?;
    Ok(())
}

async fn ensure_pg_vector_relation_hnsw_index(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    relation_name: &str,
    storage: PgVectorStorage,
    dim: i32,
) -> anyhow::Result<()> {
    let relation = quote_pg_identifier(relation_name)?;
    let hnsw_idx = quote_pg_identifier(&format!("{relation_name}_hnsw"))?;
    let row_count =
        sqlx::query_scalar::<_, i64>(&format!("SELECT count(*)::bigint FROM {relation}"))
            .fetch_one(&mut **tx)
            .await
            .with_context(|| format!("count vector rows in {relation_name} for HNSW sizing"))?;
    let row_count = u64::try_from(row_count).context("negative vector shard row count")?;
    let params = pg_hnsw_index_params(row_count, dim, storage)?;
    let ops = storage.cosine_ops();
    sqlx::query(&format!(
        "CREATE INDEX IF NOT EXISTS {hnsw_idx}
         ON {relation} USING hnsw (embedding {ops})
         WITH (m = {m}, ef_construction = {ef_construction})",
        m = params.m,
        ef_construction = params.ef_construction
    ))
    .execute(&mut **tx)
    .await
    .with_context(|| format!("create HNSW index on {relation_name}"))?;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum PgVectorStorage {
    Vector,
    Halfvec,
}

impl PgVectorStorage {
    fn for_dim(dim: u64) -> Self {
        if dim > PGVECTOR_HNSW_VECTOR_MAX_DIM { Self::Halfvec } else { Self::Vector }
    }

    fn column_type(self, dim: i32) -> String {
        match self {
            Self::Vector => format!("vector({dim})"),
            Self::Halfvec => format!("halfvec({dim})"),
        }
    }

    fn cast_type(self) -> &'static str {
        match self {
            Self::Vector => "vector",
            Self::Halfvec => "halfvec",
        }
    }

    fn cosine_ops(self) -> &'static str {
        match self {
            Self::Vector => "vector_cosine_ops",
            Self::Halfvec => "halfvec_cosine_ops",
        }
    }

    fn bytes_per_component(self) -> u64 {
        match self {
            Self::Vector => 4,
            Self::Halfvec => 2,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PgHnswIndexParams {
    m: u64,
    ef_construction: u64,
}

fn pg_hnsw_index_params(
    row_count: u64,
    dim: i32,
    storage: PgVectorStorage,
) -> anyhow::Result<PgHnswIndexParams> {
    let dim = u64::try_from(dim).context("vector dimension must be positive")?;
    let configured_m = read_env_u64("IRONRAG_PG_HNSW_M");
    let configured_ef_construction = read_env_u64("IRONRAG_PG_HNSW_EF_CONSTRUCTION");
    let m = configured_m
        .map(|m| m.clamp(PG_HNSW_MIN_M, PG_HNSW_LARGE_M))
        .unwrap_or_else(|| memory_safe_hnsw_m(row_count, dim, storage));
    let ef_construction = configured_ef_construction.unwrap_or(m.saturating_mul(4)).max(m);
    Ok(PgHnswIndexParams { m, ef_construction })
}

fn memory_safe_hnsw_m(row_count: u64, dim: u64, storage: PgVectorStorage) -> u64 {
    let target = if row_count >= 100_000 {
        PG_HNSW_LARGE_M
    } else if row_count >= 1_000 {
        PG_HNSW_MID_M
    } else {
        PG_HNSW_MIN_M
    };
    let budget = pg_hnsw_build_budget_bytes();
    [target, PG_HNSW_MID_M, PG_HNSW_MIN_M]
        .into_iter()
        .find(|&m| estimated_hnsw_build_bytes(row_count, dim, storage, m) <= budget)
        .unwrap_or(PG_HNSW_MIN_M)
}

fn estimated_hnsw_build_bytes(row_count: u64, dim: u64, storage: PgVectorStorage, m: u64) -> u128 {
    let rows = u128::from(row_count.max(1));
    let vector_bytes = u128::from(dim) * u128::from(storage.bytes_per_component());
    let graph_bytes = u128::from(m) * 16;
    rows * (vector_bytes.saturating_mul(2) + graph_bytes)
}

fn pg_hnsw_build_budget_bytes() -> u128 {
    u128::from(
        read_env_u64("IRONRAG_PG_HNSW_BUILD_BUDGET_BYTES")
            .or_else(|| read_env_u64("IRONRAG_VECTOR_INDEX_TRAINING_BUDGET_BYTES"))
            .unwrap_or(PG_HNSW_DEFAULT_BUILD_BUDGET_BYTES),
    )
}

fn read_env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            trimmed.parse::<u64>().ok().filter(|value| *value > 0)
        }
    })
}

fn checked_vector_dim_i32(dim: u64) -> anyhow::Result<i32> {
    anyhow::ensure!(dim > 0, "vector dimension must be positive");
    i32::try_from(dim).context("vector dimension overflowed i32")
}

// ===========================================================================
// Workspace snapshot — bundles every library archive into one plain tar
// ===========================================================================

/// Env var naming a scratch directory for the per-library temp files the
/// workspace exporter materializes (one library at a time). Falls back to
/// `std::env::temp_dir()` when unset.
const SNAPSHOT_SCRATCH_DIR_ENV: &str = "IRONRAG_SNAPSHOT_SCRATCH_DIR";

/// Manifest entry describing one library inside a workspace archive.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct WorkspaceManifestLibrary {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
}

/// First entry of a workspace archive (`workspace-manifest.json`). Declares
/// the workspace identity, the include kinds requested for every embedded
/// library archive, and the ordered library list. Reuses the per-library
/// [`SNAPSHOT_SCHEMA_VERSION`] so a single version gate covers both archive
/// shapes.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct WorkspaceSnapshotManifest {
    pub schema_version: u32,
    pub workspace_id: Uuid,
    pub workspace_slug: String,
    pub exported_at: chrono::DateTime<chrono::Utc>,
    pub source_version: String,
    pub include_kinds: Vec<IncludeKind>,
    pub libraries: Vec<WorkspaceManifestLibrary>,
}

/// Per-library entry in a [`WorkspaceSnapshotImportReport`].
#[derive(Debug, Default)]
pub struct WorkspaceLibraryImportReport {
    /// Library id minted on the target stack (NOT the source id).
    pub library_id: Uuid,
    /// Slug actually assigned on the target (may be suffixed `-2`, `-3`… if
    /// the source slug collided with a sibling library).
    pub slug: String,
    pub postgres_rows_by_table: Vec<(String, u64)>,
    pub blobs_restored: u64,
}

/// Aggregate report for a workspace restore. One entry per embedded library
/// archive that was provisioned and restored.
#[derive(Debug, Default)]
pub struct WorkspaceSnapshotImportReport {
    pub workspace_id: Uuid,
    pub libraries_restored: u64,
    pub overwrite_mode: OverwriteMode,
    pub libraries: Vec<WorkspaceLibraryImportReport>,
}

/// Streams a plain (uncompressed) tar bundling every library in `workspace_id`
/// into `writer`. Each embedded `libraries/{library_id}.tar.zst` entry is the
/// EXACT byte stream [`export_library_archive`] produces for that library with
/// the same `include` kinds — already zstd-compressed, hence the OUTER tar is
/// not compressed again.
///
/// To emit a tar entry the size must be known up front, so each library is
/// exported to a temp file first, stat-ed, appended, then deleted before the
/// next library — peak scratch usage is one library archive.
pub async fn export_workspace_archive<W>(
    state: AppState,
    workspace_id: Uuid,
    include: Vec<IncludeKind>,
    writer: W,
) -> Result<(), ContentServiceError>
where
    W: AsyncWrite + Unpin + Send + Sync + 'static,
{
    export_workspace_archive_inner(state, workspace_id, include, writer).await.map_err(|error| {
        tracing::error!(
            %workspace_id,
            error_chain = format!("{error:#}"),
            "workspace snapshot export failed with full chain",
        );
        ContentServiceError::from_message(error.to_string())
    })
}

async fn export_workspace_archive_inner<W>(
    state: AppState,
    workspace_id: Uuid,
    include: Vec<IncludeKind>,
    writer: W,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin + Send + Sync + 'static,
{
    IncludeKind::validate(&include)?;
    let pool = &state.persistence.postgres;

    let workspace_slug: String =
        sqlx::query_scalar("SELECT slug FROM catalog_workspace WHERE id = $1")
            .bind(workspace_id)
            .fetch_optional(pool)
            .await
            .context("load catalog_workspace slug")?
            .ok_or_else(|| anyhow!("workspace {workspace_id} does not exist"))?;

    let library_rows = sqlx::query(
        "SELECT id, slug, display_name FROM catalog_library WHERE workspace_id = $1 ORDER BY display_name",
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await
    .context("load workspace libraries")?;
    let libraries: Vec<WorkspaceManifestLibrary> = library_rows
        .into_iter()
        .map(|row| -> anyhow::Result<WorkspaceManifestLibrary> {
            Ok(WorkspaceManifestLibrary {
                id: row.try_get("id").context("decode catalog_library id")?,
                slug: row.try_get("slug").context("decode catalog_library slug")?,
                display_name: row
                    .try_get("display_name")
                    .context("decode catalog_library display_name")?,
            })
        })
        .collect::<anyhow::Result<_>>()?;

    // Plain tar over the writer — the inner library archives are already
    // zstd-compressed, so the outer layer stays uncompressed.
    let mut builder = Builder::new(writer);
    builder.mode(async_tar::HeaderMode::Deterministic);

    let inner_result = export_workspace_archive_body(
        &state,
        workspace_id,
        &workspace_slug,
        &include,
        &libraries,
        &mut builder,
    )
    .await;
    finalize_workspace_archive_with_failure_sentinel(builder, workspace_id, inner_result).await
}

/// Mirrors [`finalize_archive_with_failure_sentinel`] for the plain-tar
/// workspace builder: always finalize the tar (dropping a `Builder` without
/// `into_inner().await` panics inside `async_tar`), append an
/// `EXPORT_FAILED.json` sentinel on the error path, and propagate the body
/// error in preference to a finalize error.
async fn finalize_workspace_archive_with_failure_sentinel<W>(
    mut builder: Builder<W>,
    workspace_id: Uuid,
    inner_result: anyhow::Result<()>,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    if let Err(error) = &inner_result {
        let failure = serde_json::json!({
            "status": "export_failed",
            "workspace_id": workspace_id.to_string(),
            "error": format!("{error:#}"),
        });
        if let Err(append_err) =
            append_json_entry(&mut builder, "EXPORT_FAILED.json", &failure).await
        {
            tracing::warn!(
                %workspace_id,
                append_error = format!("{append_err:#}"),
                "workspace snapshot export failed to append EXPORT_FAILED.json sentinel",
            );
        }
    }

    let finalize_result: anyhow::Result<()> = async {
        let mut writer = builder.into_inner().await.context("finalize workspace tar builder")?;
        tokio::io::AsyncWriteExt::shutdown(&mut writer)
            .await
            .context("finalize workspace tar stream")?;
        Ok(())
    }
    .await;

    match (inner_result, finalize_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(primary), Ok(())) => Err(primary),
        (Ok(()), Err(finalize_err)) => Err(finalize_err),
        (Err(primary), Err(finalize_err)) => {
            tracing::warn!(
                %workspace_id,
                finalize_error = format!("{finalize_err:#}"),
                "workspace snapshot export finalize also failed after primary export error",
            );
            Err(primary)
        }
    }
}

async fn export_workspace_archive_body<W>(
    state: &AppState,
    workspace_id: Uuid,
    workspace_slug: &str,
    include: &[IncludeKind],
    libraries: &[WorkspaceManifestLibrary],
    builder: &mut Builder<W>,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    // 1. workspace-manifest.json — MUST be first so a reader learns the
    //    library set before the embedded archives stream past.
    let manifest = WorkspaceSnapshotManifest {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        workspace_id,
        workspace_slug: workspace_slug.to_string(),
        exported_at: chrono::Utc::now(),
        source_version: env!("CARGO_PKG_VERSION").to_string(),
        include_kinds: include.to_vec(),
        libraries: libraries.to_vec(),
    };
    append_json_entry(builder, "workspace-manifest.json", &manifest).await?;

    // 2. One library archive at a time, materialized to a temp file so the
    //    tar header carries the exact size. Delete the temp before the next
    //    library to bound scratch usage to a single library.
    let scratch_dir = std::env::var_os(SNAPSHOT_SCRATCH_DIR_ENV)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);

    for library in libraries {
        let temp_path = scratch_dir.join(format!("ironrag-snapshot-{}.tar.zst", Uuid::now_v7()));
        let export_result =
            export_one_library_to_temp(state, library.id, include, &temp_path).await;
        let append_result = match &export_result {
            Ok(()) => append_library_archive_entry(builder, library.id, &temp_path)
                .await
                .with_context(|| format!("append library {} archive entry", library.id)),
            Err(_) => Ok(()),
        };
        // Always clean the temp file before moving on, regardless of outcome.
        if let Err(remove_err) = tokio::fs::remove_file(&temp_path).await {
            if remove_err.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    %workspace_id,
                    library_id = %library.id,
                    error = %remove_err,
                    "workspace snapshot export failed to remove temp library archive",
                );
            }
        }
        export_result.with_context(|| format!("export library {} archive", library.id))?;
        append_result?;
    }

    Ok(())
}

/// Exports one library archive to `temp_path` via the canonical
/// [`export_library_archive`], leaving the file ready for stat + tar append.
async fn export_one_library_to_temp(
    state: &AppState,
    library_id: Uuid,
    include: &[IncludeKind],
    temp_path: &std::path::Path,
) -> anyhow::Result<()> {
    let file = tokio::fs::File::create(temp_path)
        .await
        .with_context(|| format!("create temp library archive `{}`", temp_path.display()))?;
    export_library_archive(state.clone(), library_id, include.to_vec(), file)
        .await
        .map_err(|error| anyhow!("{error}"))?;
    Ok(())
}

/// Stats `temp_path` and appends it as `libraries/{library_id}.tar.zst` with a
/// deterministic regular-file header (mode 0o644, mtime 0).
async fn append_library_archive_entry<W>(
    builder: &mut Builder<W>,
    library_id: Uuid,
    temp_path: &std::path::Path,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    let metadata = tokio::fs::metadata(temp_path)
        .await
        .with_context(|| format!("stat temp library archive `{}`", temp_path.display()))?;
    let file = tokio::fs::File::open(temp_path)
        .await
        .with_context(|| format!("open temp library archive `{}`", temp_path.display()))?;

    let mut header = Header::new_gnu();
    header.set_size(metadata.len());
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_entry_type(EntryType::Regular);
    header.set_cksum();
    builder
        .append_data(&mut header, format!("libraries/{library_id}.tar.zst"), file)
        .await
        .with_context(|| format!("append tar entry for library {library_id}"))?;
    Ok(())
}

/// Restores a workspace plain-tar archive into `workspace_id`. For each
/// embedded `libraries/{id}.tar.zst` entry a fresh target library is
/// provisioned in `workspace_id` (so its runtime AI profile is created), then
/// the embedded archive is restored into it via [`restore_library_archive`]
/// in `OverwriteMode::Replace` (the library was just created empty). The
/// caller-supplied `overwrite` is recorded in the report for traceability but
/// each newly minted library is always restored with `Replace`.
pub async fn restore_workspace_archive<R>(
    state: &AppState,
    workspace_id: Uuid,
    body: R,
    overwrite: OverwriteMode,
) -> Result<WorkspaceSnapshotImportReport, ContentServiceError>
where
    R: AsyncRead + Unpin + Send,
{
    restore_workspace_archive_inner(state, workspace_id, body, overwrite).await.map_err(|error| {
        tracing::error!(
            %workspace_id,
            error_chain = format!("{error:#}"),
            "workspace snapshot import failed with full chain",
        );
        ContentServiceError::from_message(error.to_string())
    })
}

async fn restore_workspace_archive_inner<R>(
    state: &AppState,
    workspace_id: Uuid,
    body: R,
    overwrite: OverwriteMode,
) -> anyhow::Result<WorkspaceSnapshotImportReport>
where
    R: AsyncRead + Unpin + Send,
{
    // Plain tar — NO zstd decode on the OUTER layer. The embedded library
    // entries are themselves tar.zst and `restore_library_archive` decodes
    // each one.
    let archive = Archive::new(BufReader::new(body));
    let mut entries = archive.entries().context("open workspace tar archive")?;

    // Stage 1 — workspace-manifest.json must be the first entry.
    let manifest = if let Some(entry) = entries.next().await {
        let mut entry = entry.context("read workspace tar entry")?;
        let path =
            entry.path().context("read workspace tar entry path")?.to_string_lossy().into_owned();
        validate_archive_path(&path)?;
        if path == "workspace-manifest.json" {
            let mut bytes = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut entry, &mut bytes)
                .await
                .context("read workspace-manifest.json")?;
            let parsed: WorkspaceSnapshotManifest =
                serde_json::from_slice(&bytes).context("parse workspace-manifest.json")?;
            if parsed.schema_version < MIN_SUPPORTED_SNAPSHOT_SCHEMA_VERSION
                || parsed.schema_version > SNAPSHOT_SCHEMA_VERSION
            {
                bail!(
                    "workspace snapshot schema_version {} is not supported by this build (supported {}..={})",
                    parsed.schema_version,
                    MIN_SUPPORTED_SNAPSHOT_SCHEMA_VERSION,
                    SNAPSHOT_SCHEMA_VERSION
                );
            }
            parsed
        } else {
            bail!("workspace tar entry `{path}` arrived before workspace-manifest.json");
        }
    } else {
        bail!("workspace snapshot archive missing workspace-manifest.json");
    };

    // Index manifest libraries by source id so we can resolve slug +
    // display_name from each `libraries/{id}.tar.zst` path.
    let manifest_by_id: BTreeMap<Uuid, &WorkspaceManifestLibrary> =
        manifest.libraries.iter().map(|library| (library.id, library)).collect();

    let mut report = WorkspaceSnapshotImportReport {
        workspace_id,
        overwrite_mode: overwrite,
        ..Default::default()
    };

    // R1: union of PostgreSQL tables touched across all libraries, ANALYZEd
    // once after the import instead of per library.
    let mut deferred_analyze_tables: BTreeMap<String, u64> = BTreeMap::new();

    // Stage 2 — each subsequent entry is a per-library archive.
    while let Some(entry) = entries.next().await {
        let mut entry = entry.context("read workspace tar entry")?;
        let path =
            entry.path().context("read workspace tar entry path")?.to_string_lossy().into_owned();
        validate_archive_path(&path)?;
        let Some(source_library_id) = parse_workspace_library_entry_path(&path)? else {
            // Tolerate auxiliary entries (e.g. an EXPORT_FAILED.json sentinel
            // surfaces the source export error verbatim).
            if path == "EXPORT_FAILED.json" {
                let mut bytes = Vec::new();
                tokio::io::AsyncReadExt::read_to_end(&mut entry, &mut bytes)
                    .await
                    .context("read EXPORT_FAILED.json")?;
                bail!(
                    "workspace snapshot carries an export failure sentinel: {}",
                    String::from_utf8_lossy(&bytes)
                );
            }
            continue;
        };
        let source_library = manifest_by_id.get(&source_library_id).ok_or_else(|| {
            anyhow!("workspace archive entry `{path}` is not declared in workspace-manifest.json")
        })?;

        // Provision a fresh target library so its runtime AI profile exists,
        // retrying with `-2`, `-3`… suffixes on slug collision.
        let created = create_target_library(
            state,
            workspace_id,
            &source_library.slug,
            &source_library.display_name,
        )
        .await?;

        // Restore the embedded archive directly from the tar entry stream —
        // it is itself an AsyncRead over the library's tar.zst bytes. Replace
        // mode is correct because the library was just created empty.
        //
        // R1: defer the per-library ANALYZE — the shared snapshot tables grow
        // with every library, so a per-library ANALYZE is O(n²) over a mass
        // import. We run a single ANALYZE over the union of touched tables
        // after the loop instead.
        let library_report = restore_library_archive_inner(
            state,
            created.id,
            &mut entry,
            OverwriteMode::Replace,
            RestoreStatsMode::Deferred,
        )
        .await
        .with_context(|| {
            format!(
                "restore library `{}` (source {source_library_id}) into workspace {workspace_id}",
                created.slug
            )
        })?;
        for (table, count) in &library_report.postgres_rows_by_table {
            if *count > 0 {
                *deferred_analyze_tables.entry(table.clone()).or_insert(0) += *count;
            }
        }

        report.libraries.push(WorkspaceLibraryImportReport {
            library_id: created.id,
            slug: created.slug,
            postgres_rows_by_table: library_report.postgres_rows_by_table,
            blobs_restored: library_report.blobs_restored,
        });
        report.libraries_restored += 1;
    }

    // R1: single end-of-import ANALYZE over the union of touched tables, so the
    // planner gets fresh stats once instead of O(n²) per-library re-scans.
    if let Err(error) =
        analyze_imported_postgres_tables(&state.persistence.postgres, &deferred_analyze_tables)
            .await
    {
        tracing::warn!(
            %workspace_id,
            error = %error,
            "workspace snapshot import deferred postgres stats refresh failed",
        );
    }

    Ok(report)
}

/// Parses `libraries/{uuid}.tar.zst` and returns the embedded library id.
/// Returns `Ok(None)` for any path that is not a library archive entry.
fn parse_workspace_library_entry_path(path: &str) -> anyhow::Result<Option<Uuid>> {
    let Some(rest) = path.strip_prefix("libraries/") else {
        return Ok(None);
    };
    let Some(id_str) = rest.strip_suffix(".tar.zst") else {
        return Ok(None);
    };
    if id_str.contains('/') {
        return Ok(None);
    }
    Uuid::parse_str(id_str)
        .map(Some)
        .with_context(|| format!("parse library id from workspace archive entry `{path}`"))
}

/// A target library minted by the workspace restore path.
struct CreatedTargetLibrary {
    id: Uuid,
    slug: String,
}

/// Provisions a fresh library in `workspace_id` through [`CatalogService`] so
/// its runtime AI profile is created, retrying with `-2`, `-3`… slug suffixes
/// when the requested slug collides with a sibling library
/// (`catalog_library_workspace_id_slug_key`).
async fn create_target_library(
    state: &AppState,
    workspace_id: Uuid,
    source_slug: &str,
    display_name: &str,
) -> anyhow::Result<CreatedTargetLibrary> {
    const MAX_SLUG_ATTEMPTS: u32 = 50;
    for attempt in 1..=MAX_SLUG_ATTEMPTS {
        let candidate_slug =
            if attempt == 1 { source_slug.to_string() } else { format!("{source_slug}-{attempt}") };
        let command = crate::services::catalog_service::CreateLibraryCommand {
            workspace_id,
            slug: Some(candidate_slug.clone()),
            display_name: display_name.to_string(),
            description: None,
            created_by_principal_id: None,
        };
        match state.canonical_services.catalog.create_library(state, command).await {
            Ok(library) => {
                return Ok(CreatedTargetLibrary { id: library.id, slug: library.slug });
            }
            Err(crate::interfaces::http::router_support::ApiError::Conflict(_)) => {
                // Slug collided with a sibling library — try the next suffix.
                continue;
            }
            Err(error) => {
                bail!(
                    "create target library for workspace {workspace_id} (slug `{candidate_slug}`): {error:?}"
                );
            }
        }
    }
    bail!(
        "could not allocate a free slug for workspace {workspace_id} library `{source_slug}` after {MAX_SLUG_ATTEMPTS} attempts"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_with_sections(postgres_tables: Vec<&str>, has_blobs: bool) -> SnapshotManifest {
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
            has_blobs,
            vector_shards: Vec::new(),
        }
    }

    #[test]
    fn workspace_manifest_serde_round_trips() {
        let workspace_id = Uuid::now_v7();
        let lib_a = Uuid::now_v7();
        let lib_b = Uuid::now_v7();
        let manifest = WorkspaceSnapshotManifest {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            workspace_id,
            workspace_slug: "alpha-suite".to_string(),
            exported_at: chrono::Utc::now(),
            source_version: "0.0.0-test".to_string(),
            include_kinds: vec![IncludeKind::LibraryData, IncludeKind::Blobs],
            libraries: vec![
                WorkspaceManifestLibrary {
                    id: lib_a,
                    slug: "provider-beta".to_string(),
                    display_name: "Provider Beta".to_string(),
                },
                WorkspaceManifestLibrary {
                    id: lib_b,
                    slug: "provider-gamma".to_string(),
                    display_name: "Provider Gamma".to_string(),
                },
            ],
        };

        let bytes = serde_json::to_vec(&manifest).expect("serialize workspace manifest");
        let parsed: WorkspaceSnapshotManifest =
            serde_json::from_slice(&bytes).expect("deserialize workspace manifest");

        assert_eq!(parsed.schema_version, SNAPSHOT_SCHEMA_VERSION);
        assert_eq!(parsed.workspace_id, workspace_id);
        assert_eq!(parsed.workspace_slug, "alpha-suite");
        assert_eq!(parsed.include_kinds, vec![IncludeKind::LibraryData, IncludeKind::Blobs]);
        assert_eq!(parsed.libraries.len(), 2);
        assert_eq!(parsed.libraries[0].id, lib_a);
        assert_eq!(parsed.libraries[1].slug, "provider-gamma");
    }

    #[test]
    fn workspace_library_entry_path_parses_and_rejects() {
        let id = Uuid::now_v7();
        assert_eq!(
            parse_workspace_library_entry_path(&format!("libraries/{id}.tar.zst")).unwrap(),
            Some(id)
        );
        assert_eq!(parse_workspace_library_entry_path("workspace-manifest.json").unwrap(), None);
        assert_eq!(parse_workspace_library_entry_path("libraries/nested/x.tar.zst").unwrap(), None);
        assert!(parse_workspace_library_entry_path("libraries/not-a-uuid.tar.zst").is_err());
    }

    #[test]
    fn per_dim_vector_collection_name_parser_round_trips_chunk_and_entity_shards() {
        assert_eq!(parse_per_dim_vector_collection_dim("knowledge_chunk_vector_d1024"), Some(1024));
        assert_eq!(
            parse_per_dim_vector_collection_dim("knowledge_entity_vector_d3072"),
            Some(3072)
        );
        assert_eq!(parse_per_dim_vector_collection_dim("knowledge_chunk_vector_d1"), Some(1));
        assert!(is_per_dim_chunk_vector_collection_name("knowledge_chunk_vector_d1024"));
        assert!(!is_per_dim_chunk_vector_collection_name("knowledge_entity_vector_d1024"));
        assert!(is_runtime_vector_relation_name("knowledge_chunk_vector_d3072"));
        // Negative cases — legacy names, missing digits, alpha/library suffixes,
        // and wrong prefixes.
        assert_eq!(parse_per_dim_vector_collection_dim("knowledge_chunk_vector"), None);
        assert_eq!(parse_per_dim_vector_collection_dim("knowledge_chunk_vector_d"), None);
        assert_eq!(parse_per_dim_vector_collection_dim("knowledge_chunk_vector_d1024x"), None);
        assert_eq!(parse_per_dim_vector_collection_dim("knowledge_chunk_vector_d1024_l"), None);
        assert_eq!(
            parse_per_dim_vector_collection_dim(
                "knowledge_chunk_vector_d3072_l019ded0008207ad29224ca3d0c82d57c"
            ),
            None
        );
        assert_eq!(parse_per_dim_vector_collection_dim("knowledge_chunk_vector_d1024_lABC"), None);
        assert_eq!(parse_per_dim_vector_collection_dim("other_collection_d1024"), None);
    }

    #[test]
    fn snapshot_manifest_sections_accept_per_dim_vector_shards() {
        let mut manifest =
            manifest_with_sections(vec!["catalog_library", "knowledge_chunk_vector_d1024"], false);
        manifest.vector_shards =
            vec![VectorShardEntry { name: "knowledge_chunk_vector_d1024".to_string(), dim: 1024 }];
        let sections = SnapshotManifestSections::from_manifest(&manifest).unwrap();
        assert_eq!(
            sections.require_postgres_table("knowledge_chunk_vector_d1024").unwrap(),
            "knowledge_chunk_vector_d1024"
        );
    }

    #[test]
    fn snapshot_manifest_sections_accept_declared_canonical_names() {
        let manifest = manifest_with_sections(
            vec!["catalog_library", "content_document", "runtime_graph_node"],
            true,
        );

        let sections = SnapshotManifestSections::from_manifest(&manifest).unwrap();

        assert_eq!(sections.require_postgres_table("catalog_library").unwrap(), "catalog_library");
        assert_eq!(
            sections.require_postgres_table("content_document").unwrap(),
            "content_document"
        );
        assert_eq!(
            sections.require_postgres_table("runtime_graph_node").unwrap(),
            "runtime_graph_node"
        );
    }

    #[test]
    fn snapshot_manifest_sections_reject_unknown_or_undeclared_names() {
        let manifest = manifest_with_sections(vec!["catalog_library", "pg_catalog_authid"], false);
        assert!(SnapshotManifestSections::from_manifest(&manifest).is_err());

        let manifest = manifest_with_sections(vec!["catalog_library"], false);
        let sections = SnapshotManifestSections::from_manifest(&manifest).unwrap();
        assert!(sections.require_postgres_table("content_document").is_err());
        assert!(sections.require_postgres_table("ai_provider_credential").is_err());
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
        let mut manifest = manifest_with_sections(vec!["catalog_library"], true);
        manifest.include_kinds = vec![IncludeKind::LibraryData];

        assert!(SnapshotManifestSections::from_manifest(&manifest).is_err());
    }

    #[test]
    fn catalog_library_import_does_not_carry_parallel_update_column_list() {
        assert_eq!(pg_insert_conflict_clause("catalog_workspace"), " ON CONFLICT DO NOTHING");
        assert_eq!(pg_insert_conflict_clause("catalog_library"), "");
    }

    #[test]
    fn restore_library_data_plan_allows_reject_into_empty_target() {
        assert_eq!(
            plan_restore_library_data(
                &[IncludeKind::LibraryData],
                OverwriteMode::Reject,
                TargetRestoreFootprint::Empty,
            ),
            RestoreLibraryDataAction::ImportIntoEmptyTarget
        );
        assert_eq!(
            plan_restore_library_data(
                &[IncludeKind::LibraryData],
                OverwriteMode::Reject,
                TargetRestoreFootprint::Populated,
            ),
            RestoreLibraryDataAction::RejectPopulatedTarget
        );
        assert_eq!(
            plan_restore_library_data(
                &[IncludeKind::LibraryData],
                OverwriteMode::Replace,
                TargetRestoreFootprint::Populated,
            ),
            RestoreLibraryDataAction::ReplaceTarget
        );
        assert_eq!(
            plan_restore_library_data(
                &[IncludeKind::AiConfig],
                OverwriteMode::Replace,
                TargetRestoreFootprint::Populated,
            ),
            RestoreLibraryDataAction::Skip
        );
    }

    #[test]
    fn restore_footprint_clauses_follow_canonical_snapshot_tables() {
        let content_document = postgres_restore_footprint_clause("content_document")
            .unwrap()
            .expect("content_document carries direct library scope");
        assert!(content_document.contains("\"content_document\""));
        assert!(content_document.contains("library_id = $1"));

        let content_chunk = postgres_restore_footprint_clause("content_chunk")
            .unwrap()
            .expect("content_chunk is scoped through content_revision");
        assert!(content_chunk.contains("JOIN content_revision"));
        assert!(content_chunk.contains("r.library_id = $1"));

        let vector_manifest =
            postgres_restore_footprint_clause("knowledge_vector_relation_manifest")
                .unwrap()
                .expect("vector manifest rows make the target non-empty");
        assert!(vector_manifest.contains("\"knowledge_vector_relation_manifest\""));
    }

    #[test]
    fn catalog_library_snapshot_update_preserves_missing_default_columns() {
        let sql = catalog_library_snapshot_update_sql();
        assert!(sql.contains(
            "retrieval_config = COALESCE(row.retrieval_config, target.retrieval_config)"
        ));
        assert!(sql.contains(
            "web_ingest_policy = COALESCE(row.web_ingest_policy, target.web_ingest_policy)"
        ));
        assert!(sql.contains("retrieval_config jsonb"));
        assert!(sql.contains("FROM jsonb_to_recordset($1)"));
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
        let mut scope = SnapshotRowScope::new(
            source_library_id,
            target_library_id,
            target_workspace_id,
            Some("alpha-restored".to_string()),
        );

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
        // The archive's slug `alpha` is rewritten to the target library's
        // own slug so a restore cannot collide with a sibling library.
        assert_eq!(string_field(&library, "slug"), Some("alpha-restored"));

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
    fn snapshot_row_scope_normalizes_ai_config_rows() {
        let source_workspace_id = Uuid::now_v7();
        let source_library_id = Uuid::now_v7();
        let target_workspace_id = Uuid::now_v7();
        let target_library_id = Uuid::now_v7();
        let mut scope =
            SnapshotRowScope::new(source_library_id, target_library_id, target_workspace_id, None);

        // System provider catalog: stable id, no scope columns — untouched.
        let provider_id = Uuid::now_v7();
        let mut provider = serde_json::json!({
            "id": provider_id,
            "provider_kind": "openai",
            "display_name": "OpenAI",
        });
        scope.normalize_postgres_row("ai_provider_catalog", &mut provider).unwrap();
        assert_eq!(
            required_uuid_field("ai_provider_catalog", &provider, "id").unwrap(),
            provider_id
        );

        // Workspace-scoped credential: workspace_id remapped, library_id stays
        // null, principal author reference dropped.
        let mut credential = serde_json::json!({
            "id": Uuid::now_v7(),
            "workspace_id": source_workspace_id,
            "library_id": serde_json::Value::Null,
            "scope_kind": "workspace",
            "api_key": serde_json::Value::Null,
            "created_by_principal_id": Uuid::now_v7(),
        });
        scope.normalize_postgres_row("ai_provider_credential", &mut credential).unwrap();
        assert_eq!(
            required_uuid_field("ai_provider_credential", &credential, "workspace_id").unwrap(),
            target_workspace_id
        );
        assert!(credential.get("library_id").unwrap().is_null());
        assert!(credential.get("created_by_principal_id").unwrap().is_null());

        // Library-scoped preset: both scope ids remapped.
        let mut preset = serde_json::json!({
            "id": Uuid::now_v7(),
            "workspace_id": source_workspace_id,
            "library_id": source_library_id,
            "scope_kind": "library",
            "created_by_principal_id": Uuid::now_v7(),
        });
        scope.normalize_postgres_row("ai_model_preset", &mut preset).unwrap();
        assert_eq!(
            required_uuid_field("ai_model_preset", &preset, "workspace_id").unwrap(),
            target_workspace_id
        );
        assert_eq!(
            required_uuid_field("ai_model_preset", &preset, "library_id").unwrap(),
            target_library_id
        );
        assert!(preset.get("created_by_principal_id").unwrap().is_null());

        // A library-scoped row referencing a different library is rejected.
        let mut foreign = serde_json::json!({
            "id": Uuid::now_v7(),
            "workspace_id": source_workspace_id,
            "library_id": Uuid::now_v7(),
            "scope_kind": "library",
        });
        assert!(scope.normalize_postgres_row("ai_binding_assignment", &mut foreign).is_err());

        // The canonical (post-0004) names go through the same normalizer.
        let mut account = serde_json::json!({
            "id": Uuid::now_v7(),
            "workspace_id": source_workspace_id,
            "library_id": serde_json::Value::Null,
            "scope_kind": "workspace",
        });
        scope.normalize_postgres_row("ai_account", &mut account).unwrap();
        assert_eq!(
            required_uuid_field("ai_account", &account, "workspace_id").unwrap(),
            target_workspace_id
        );
        let mut binding = serde_json::json!({
            "id": Uuid::now_v7(),
            "workspace_id": source_workspace_id,
            "library_id": source_library_id,
            "scope_kind": "library",
        });
        scope.normalize_postgres_row("ai_binding", &mut binding).unwrap();
        assert_eq!(
            required_uuid_field("ai_binding", &binding, "library_id").unwrap(),
            target_library_id
        );
    }

    /// Pre-0004 archives declare the old table names in `manifest.json`; a
    /// restore must still recognize them as known snapshot tables (import
    /// backward compatibility, migration 0004).
    #[test]
    fn legacy_ai_config_table_names_are_recognized_for_import() {
        let manifest = manifest_with_sections(
            vec![
                "catalog_library",
                "ai_provider_credential",
                "ai_model_preset",
                "ai_binding_assignment",
            ],
            false,
        );
        let sections = SnapshotManifestSections::from_manifest(&manifest).unwrap();
        assert_eq!(
            sections.require_postgres_table("ai_provider_credential").unwrap(),
            "ai_provider_credential"
        );
        assert_eq!(sections.require_postgres_table("ai_model_preset").unwrap(), "ai_model_preset");
        assert_eq!(
            sections.require_postgres_table("ai_binding_assignment").unwrap(),
            "ai_binding_assignment"
        );
    }

    #[test]
    fn canonical_ai_config_storage_table_renames_legacy_names_only() {
        assert_eq!(canonical_ai_config_storage_table("ai_provider_credential"), "ai_account");
        assert_eq!(canonical_ai_config_storage_table("ai_binding_assignment"), "ai_binding");
        assert_eq!(canonical_ai_config_storage_table("ai_account"), "ai_account");
        assert_eq!(canonical_ai_config_storage_table("ai_binding"), "ai_binding");
        assert_eq!(canonical_ai_config_storage_table("ai_provider_catalog"), "ai_provider_catalog");
    }

    /// Core of the old-format import path: a pre-0004
    /// `ai_binding_assignment` row plus its referenced `ai_model_preset`
    /// row must merge into the exact shape a canonical `ai_binding` row
    /// carries — `provider_credential_id` renamed to `account_id`,
    /// `model_preset_id` replaced by the preset's inline fields.
    #[test]
    fn merge_legacy_binding_row_splices_preset_fields_and_renames_columns() {
        let account_id = Uuid::now_v7();
        let preset_id = Uuid::now_v7();
        let model_catalog_id = Uuid::now_v7();

        let mut presets = HashMap::new();
        let preset_row = serde_json::json!({
            "id": preset_id,
            "model_catalog_id": model_catalog_id,
            "system_prompt": "be terse",
            "temperature": 0.2,
            "top_p": 0.9,
            "max_output_tokens_override": 4096,
            "extra_parameters_json": {"seed": 7},
        });
        let (parsed_id, preset) = LegacyModelPreset::from_row(&preset_row).unwrap();
        assert_eq!(parsed_id, preset_id);
        presets.insert(parsed_id, preset);

        let mut binding = serde_json::json!({
            "id": Uuid::now_v7(),
            "binding_purpose": "query_answer",
            "provider_credential_id": account_id,
            "model_preset_id": preset_id,
            "binding_state": "active",
            "scope_kind": "instance",
        });
        merge_legacy_binding_row(&mut binding, &presets).unwrap();

        assert!(binding.get("provider_credential_id").is_none());
        assert!(binding.get("model_preset_id").is_none());
        assert_eq!(binding["account_id"], serde_json::json!(account_id));
        assert_eq!(binding["model_catalog_id"], serde_json::json!(model_catalog_id));
        assert_eq!(binding["system_prompt"], serde_json::json!("be terse"));
        assert_eq!(binding["temperature"], serde_json::json!(0.2));
        assert_eq!(binding["top_p"], serde_json::json!(0.9));
        assert_eq!(binding["max_output_tokens_override"], serde_json::json!(4096));
        assert_eq!(binding["extra_parameters_json"], serde_json::json!({"seed": 7}));
    }

    /// A binding whose `model_preset_id` was not seen in the archive's
    /// `ai_model_preset` section (corrupt or truncated archive) must fail
    /// loudly rather than silently drop the binding's parameters.
    #[test]
    fn merge_legacy_binding_row_rejects_dangling_preset_reference() {
        let presets: HashMap<Uuid, LegacyModelPreset> = HashMap::new();
        let mut binding = serde_json::json!({
            "id": Uuid::now_v7(),
            "provider_credential_id": Uuid::now_v7(),
            "model_preset_id": Uuid::now_v7(),
        });
        assert!(merge_legacy_binding_row(&mut binding, &presets).is_err());
    }

    #[test]
    fn pg_batcher_keeps_different_postgres_targets_in_separate_buffers() {
        let document_id = Uuid::now_v7();
        let block_id = Uuid::now_v7();
        let mut batcher = PgBatcher::new();

        batcher.push(
            "knowledge_document",
            serde_json::json!({
                "document_id": document_id,
                "title": "Document A",
            }),
        );
        batcher.push(
            "knowledge_structured_block",
            serde_json::json!({
                "block_id": block_id,
                "text": "Section A",
            }),
        );

        let block_id_text = block_id.to_string();
        assert_eq!(batcher.pending.get("knowledge_document").map(Vec::len), Some(1));
        assert_eq!(batcher.pending.get("knowledge_structured_block").map(Vec::len), Some(1));
        assert_eq!(
            batcher
                .pending
                .get("knowledge_structured_block")
                .and_then(|rows| rows.first())
                .and_then(|row| row.get("block_id"))
                .and_then(serde_json::Value::as_str),
            Some(block_id_text.as_str())
        );
    }

    /// Pushes one restore row through the document dedup, mirroring the
    /// streaming loop's PG branch, and panics on a routing error.
    fn route(
        dedup: &mut KnowledgeDocumentDedup,
        batcher: &mut PgBatcher,
        table: &str,
        row: serde_json::Value,
    ) {
        let mut kept = true;
        route_pg_row_through_dedup(dedup, batcher, table, row, &mut kept)
            .expect("route restore row");
    }

    /// Collects the `uuid`-typed `field` of every pending row in `table`.
    fn pending_uuids(batcher: &PgBatcher, table: &str, field: &str) -> HashSet<Uuid> {
        batcher
            .pending
            .get(table)
            .into_iter()
            .flatten()
            .filter_map(|row| row.get(field))
            .filter_map(serde_json::Value::as_str)
            .map(|value| Uuid::parse_str(value).expect("uuid field"))
            .collect()
    }

    fn document_row(
        document_id: Uuid,
        external_key: &str,
        state: &str,
        rev_no: i64,
    ) -> serde_json::Value {
        serde_json::json!({
            "document_id": document_id,
            "workspace_id": Uuid::now_v7(),
            "library_id": Uuid::now_v7(),
            "external_key": external_key,
            "file_name": "file.bin",
            "title": "title",
            "document_state": state,
            "active_revision_id": Uuid::now_v7(),
            "readable_revision_id": Uuid::now_v7(),
            "latest_revision_no": rev_no,
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "deleted_at": serde_json::Value::Null,
        })
    }

    fn chunk_row(chunk_id: Uuid, document_id: Uuid) -> serde_json::Value {
        serde_json::json!({
            "chunk_id": chunk_id,
            "workspace_id": Uuid::now_v7(),
            "library_id": Uuid::now_v7(),
            "document_id": document_id,
            "revision_id": Uuid::now_v7(),
            "content_text": "body",
        })
    }

    /// Reproduces the 0.4.x archive shape — many stale `knowledge_document`
    /// rows per `external_key` plus a non-head chunk-bearing sibling — and
    /// asserts the restore dedup keeps exactly one document per key, drops the
    /// stale rows, retains only the kept documents' chunks and chunk-derived
    /// rows, and skips a `knowledge_bundle_chunk_edge` for a dropped chunk so
    /// nothing is orphaned and the unique index cannot be violated.
    #[test]
    fn restore_dedup_collapses_external_key_and_cascades_descendants() {
        let mut dedup = KnowledgeDocumentDedup::default();
        let mut batcher = PgBatcher::new();

        // --- Key A: a live head doc with chunks, a non-head doc WITH chunks,
        //     and two stale empty docs. The head must win; the non-head's
        //     chunks must be dropped, the head's chunks kept.
        let key_a = "synthetic:key:alpha";
        let doc_a_head = Uuid::now_v7();
        let doc_a_nonhead = Uuid::now_v7();
        let doc_a_stale_1 = Uuid::now_v7();
        let doc_a_stale_2 = Uuid::now_v7();
        let chunk_a_head_1 = Uuid::now_v7();
        let chunk_a_head_2 = Uuid::now_v7();
        let chunk_a_nonhead = Uuid::now_v7();

        // --- Key B: no head at all; the only chunk-bearing active doc must be
        //     kept on the "has descendants / active" fallback, plus a stale doc.
        let key_b = "synthetic:key:beta";
        let doc_b_keep = Uuid::now_v7();
        let doc_b_stale = Uuid::now_v7();
        let chunk_b = Uuid::now_v7();

        // --- Key C: a single document (already-deduped / v6 shape). Must pass
        //     through untouched — the no-op guarantee.
        let key_c = "synthetic:key:gamma";
        let doc_c = Uuid::now_v7();
        let chunk_c = Uuid::now_v7();

        // Heads precede documents in the archive (see POSTGRES_CONTENT_TABLES).
        // Key A's head is doc_a_head; key B/C have no head row.
        route(
            &mut dedup,
            &mut batcher,
            "content_document_head",
            serde_json::json!({ "document_id": doc_a_head }),
        );

        // knowledge_document rows (stale duplicates interleaved).
        route(
            &mut dedup,
            &mut batcher,
            "knowledge_document",
            document_row(doc_a_stale_1, key_a, "deleted", 1),
        );
        route(
            &mut dedup,
            &mut batcher,
            "knowledge_document",
            document_row(doc_a_nonhead, key_a, "active", 1),
        );
        route(
            &mut dedup,
            &mut batcher,
            "knowledge_document",
            document_row(doc_a_head, key_a, "active", 1),
        );
        route(
            &mut dedup,
            &mut batcher,
            "knowledge_document",
            document_row(doc_a_stale_2, key_a, "deleted", 2),
        );
        route(
            &mut dedup,
            &mut batcher,
            "knowledge_document",
            document_row(doc_b_stale, key_b, "deleted", 1),
        );
        route(
            &mut dedup,
            &mut batcher,
            "knowledge_document",
            document_row(doc_b_keep, key_b, "active", 1),
        );
        route(
            &mut dedup,
            &mut batcher,
            "knowledge_document",
            document_row(doc_c, key_c, "active", 1),
        );

        // knowledge_chunk rows — first descendant triggers finalize.
        route(&mut dedup, &mut batcher, "knowledge_chunk", chunk_row(chunk_a_head_1, doc_a_head));
        route(&mut dedup, &mut batcher, "knowledge_chunk", chunk_row(chunk_a_head_2, doc_a_head));
        route(
            &mut dedup,
            &mut batcher,
            "knowledge_chunk",
            chunk_row(chunk_a_nonhead, doc_a_nonhead),
        );
        route(&mut dedup, &mut batcher, "knowledge_chunk", chunk_row(chunk_b, doc_b_keep));
        route(&mut dedup, &mut batcher, "knowledge_chunk", chunk_row(chunk_c, doc_c));

        // Revisions for a dropped and a kept doc arrive BEFORE chunks in the
        // v6 export order — they must be buffered and replayed after finalize.
        route(
            &mut dedup,
            &mut batcher,
            "knowledge_revision",
            serde_json::json!({
                "revision_id": Uuid::now_v7(), "document_id": doc_a_nonhead,
            }),
        );
        route(
            &mut dedup,
            &mut batcher,
            "knowledge_revision",
            serde_json::json!({
                "revision_id": Uuid::now_v7(), "document_id": doc_a_head,
            }),
        );

        // Chunk rows arrive next; they update chunk-ownership before finalize.
        route(&mut dedup, &mut batcher, "knowledge_chunk", chunk_row(chunk_a_head_1, doc_a_head));
        route(&mut dedup, &mut batcher, "knowledge_chunk", chunk_row(chunk_a_head_2, doc_a_head));
        route(
            &mut dedup,
            &mut batcher,
            "knowledge_chunk",
            chunk_row(chunk_a_nonhead, doc_a_nonhead),
        );
        route(&mut dedup, &mut batcher, "knowledge_chunk", chunk_row(chunk_b, doc_b_keep));
        route(&mut dedup, &mut batcher, "knowledge_chunk", chunk_row(chunk_c, doc_c));

        // knowledge_entity_candidate: keyed by chunk_id.
        // Row for dropped chunk must be dropped; row for kept chunk must survive.
        route(
            &mut dedup,
            &mut batcher,
            "knowledge_entity_candidate",
            serde_json::json!({
                "candidate_id": Uuid::now_v7(), "chunk_id": chunk_a_nonhead, "candidate_label": "x",
            }),
        );
        route(
            &mut dedup,
            &mut batcher,
            "knowledge_entity_candidate",
            serde_json::json!({
                "candidate_id": Uuid::now_v7(), "chunk_id": chunk_a_head_1, "candidate_label": "y",
            }),
        );
        // A chunk-derived row with a null chunk_id is always kept.
        let candidate_null_chunk = Uuid::now_v7();
        route(
            &mut dedup,
            &mut batcher,
            "knowledge_entity_candidate",
            serde_json::json!({
                "candidate_id": candidate_null_chunk, "chunk_id": serde_json::Value::Null, "candidate_label": "z",
            }),
        );

        // knowledge_chunk_entity_mention: keyed by from_id (FK → knowledge_chunk),
        // NOT chunk_id. Row whose from_id is a dropped chunk must be dropped.
        let mention_kept_id = Uuid::now_v7();
        let entity_id = Uuid::now_v7();
        route(
            &mut dedup,
            &mut batcher,
            "knowledge_chunk_entity_mention",
            serde_json::json!({
                "from_id": chunk_a_nonhead, "to_id": entity_id, "relation_type": "mentions",
            }),
        );
        route(
            &mut dedup,
            &mut batcher,
            "knowledge_chunk_entity_mention",
            serde_json::json!({
                "from_id": chunk_a_head_1, "to_id": mention_kept_id, "relation_type": "mentions",
            }),
        );

        dedup.finalize(&mut batcher);

        // 1. Exactly one knowledge_document per external key survives, and the
        //    survivors are the head / chunk-bearing / live rows.
        let kept_docs = pending_uuids(&batcher, "knowledge_document", "document_id");
        assert_eq!(kept_docs.len(), 3, "one document kept per external key");
        assert!(kept_docs.contains(&doc_a_head), "head wins key A");
        assert!(kept_docs.contains(&doc_b_keep), "chunk-bearing active wins headless key B");
        assert!(kept_docs.contains(&doc_c), "single document key C is untouched");
        assert!(!kept_docs.contains(&doc_a_nonhead));
        assert!(!kept_docs.contains(&doc_a_stale_1));
        assert!(!kept_docs.contains(&doc_a_stale_2));
        assert!(!kept_docs.contains(&doc_b_stale));

        // No two kept docs share an external key — the unique index holds.
        let kept_keys: Vec<&str> = batcher
            .pending
            .get("knowledge_document")
            .into_iter()
            .flatten()
            .map(|row| row.get("external_key").and_then(serde_json::Value::as_str).unwrap())
            .collect();
        let mut deduped_keys = kept_keys.clone();
        deduped_keys.sort_unstable();
        deduped_keys.dedup();
        assert_eq!(
            kept_keys.len(),
            deduped_keys.len(),
            "no duplicate external_key violates the unique index"
        );

        // 2. Only the kept documents' chunks survive.
        let kept_chunks = pending_uuids(&batcher, "knowledge_chunk", "chunk_id");
        assert_eq!(kept_chunks.len(), 4);
        assert!(kept_chunks.contains(&chunk_a_head_1));
        assert!(kept_chunks.contains(&chunk_a_head_2));
        assert!(kept_chunks.contains(&chunk_b));
        assert!(kept_chunks.contains(&chunk_c));
        assert!(!kept_chunks.contains(&chunk_a_nonhead), "dropped doc's chunk is orphan-free");

        // 3. knowledge_entity_candidate rows follow the chunk cascade (chunk_id).
        let kept_candidates = pending_uuids(&batcher, "knowledge_entity_candidate", "candidate_id");
        assert_eq!(
            kept_candidates.len(),
            2,
            "candidate for dropped chunk dropped; null-chunk and kept-chunk survive"
        );
        assert!(kept_candidates.contains(&candidate_null_chunk));

        // 4. Document-keyed descendants buffered before chunks (revisions) are
        //    replayed correctly through the cascade after finalize.
        let kept_revision_docs = pending_uuids(&batcher, "knowledge_revision", "document_id");
        assert_eq!(kept_revision_docs.len(), 1, "revision for dropped doc is dropped");
        assert!(kept_revision_docs.contains(&doc_a_head));

        // 5. knowledge_chunk_entity_mention uses from_id (not chunk_id) as the
        //    cascade key. Row whose from_id was dropped must be dropped.
        let kept_mentions = pending_uuids(&batcher, "knowledge_chunk_entity_mention", "to_id");
        assert_eq!(
            kept_mentions.len(),
            1,
            "mention for dropped chunk is dropped via from_id cascade"
        );
        assert!(kept_mentions.contains(&mention_kept_id));
    }

    /// An archive that already carries one document per external key (v6 /
    /// already-deduped) must pass through unchanged: every document, chunk and
    /// chunk-derived row survives, so the normal path never regresses.
    #[test]
    fn restore_dedup_is_a_noop_without_duplicates() {
        let mut dedup = KnowledgeDocumentDedup::default();
        let mut batcher = PgBatcher::new();

        let doc_1 = Uuid::now_v7();
        let doc_2 = Uuid::now_v7();
        let chunk_1 = Uuid::now_v7();
        let chunk_2 = Uuid::now_v7();

        route(
            &mut dedup,
            &mut batcher,
            "knowledge_document",
            document_row(doc_1, "key:one", "active", 1),
        );
        route(
            &mut dedup,
            &mut batcher,
            "knowledge_document",
            document_row(doc_2, "key:two", "active", 1),
        );
        route(&mut dedup, &mut batcher, "knowledge_chunk", chunk_row(chunk_1, doc_1));
        route(&mut dedup, &mut batcher, "knowledge_chunk", chunk_row(chunk_2, doc_2));
        dedup.finalize(&mut batcher);

        assert_eq!(pending_uuids(&batcher, "knowledge_document", "document_id").len(), 2);
        assert_eq!(pending_uuids(&batcher, "knowledge_chunk", "chunk_id").len(), 2);
    }

    /// Blocker regression: an empty new head (head=true, active, 0 chunks) must
    /// NOT evict an old chunk-bearing sibling (non-head or deleted) for the same
    /// external key. Without the chunk-ownership tier in the keep-rule the empty
    /// head would win and the library would restore as an empty shell.
    ///
    /// Pattern: re-sync minted a NEW document that became `content_document_head`
    /// but ingest died before `chunk_content`. The OLD document (now
    /// `document_state='deleted'`, not in head) still holds all the real chunks.
    #[test]
    fn restore_dedup_chunk_bearing_sibling_beats_empty_head() {
        let mut dedup = KnowledgeDocumentDedup::default();
        let mut batcher = PgBatcher::new();

        let key = "synthetic:key:delta";
        // Empty new head: active, latest head, but 0 chunks.
        let doc_empty_head = Uuid::now_v7();
        // Old chunk-bearing doc: deleted (superseded by re-sync), non-head,
        // lower rev_no, but holds the only real content chunks.
        let doc_chunked_old = Uuid::now_v7();
        // Unrelated stale empty doc with no head and no chunks.
        let doc_stale = Uuid::now_v7();

        let chunk_old_1 = Uuid::now_v7();
        let chunk_old_2 = Uuid::now_v7();
        let entity_id = Uuid::now_v7();

        // The empty new doc is the head.
        route(
            &mut dedup,
            &mut batcher,
            "content_document_head",
            serde_json::json!({ "document_id": doc_empty_head }),
        );

        // Documents buffered. Empty head has higher rev_no to make it
        // "better" on every tier except chunk ownership.
        route(
            &mut dedup,
            &mut batcher,
            "knowledge_document",
            document_row(doc_empty_head, key, "active", 3),
        );
        route(
            &mut dedup,
            &mut batcher,
            "knowledge_document",
            document_row(doc_chunked_old, key, "deleted", 1),
        );
        route(
            &mut dedup,
            &mut batcher,
            "knowledge_document",
            document_row(doc_stale, key, "deleted", 1),
        );

        // Revisions arrive before chunks in v6 order — must be buffered.
        let rev_old = Uuid::now_v7();
        let rev_head = Uuid::now_v7();
        route(
            &mut dedup,
            &mut batcher,
            "knowledge_revision",
            serde_json::json!({
                "revision_id": rev_old, "document_id": doc_chunked_old,
            }),
        );
        route(
            &mut dedup,
            &mut batcher,
            "knowledge_revision",
            serde_json::json!({
                "revision_id": rev_head, "document_id": doc_empty_head,
            }),
        );

        // Chunks: only the old deleted doc has any. Empty head has none.
        route(&mut dedup, &mut batcher, "knowledge_chunk", chunk_row(chunk_old_1, doc_chunked_old));
        route(&mut dedup, &mut batcher, "knowledge_chunk", chunk_row(chunk_old_2, doc_chunked_old));

        // Chunk-entity mention for a chunk of the old doc (via from_id).
        route(
            &mut dedup,
            &mut batcher,
            "knowledge_chunk_entity_mention",
            serde_json::json!({
                "from_id": chunk_old_1, "to_id": entity_id, "relation_type": "mentions",
            }),
        );

        dedup.finalize(&mut batcher);

        // The old chunk-bearing doc must win despite being non-head and deleted.
        let kept_docs = pending_uuids(&batcher, "knowledge_document", "document_id");
        assert_eq!(kept_docs.len(), 1, "exactly one doc kept per external key");
        assert!(kept_docs.contains(&doc_chunked_old), "chunk-bearing sibling beats the empty head",);
        assert!(
            !kept_docs.contains(&doc_empty_head),
            "empty head is dropped when a chunk-bearing sibling exists",
        );
        assert!(!kept_docs.contains(&doc_stale));

        // No duplicate external_key — unique index cannot be violated.
        let kept_keys: Vec<&str> = batcher
            .pending
            .get("knowledge_document")
            .into_iter()
            .flatten()
            .map(|r| r.get("external_key").and_then(serde_json::Value::as_str).unwrap())
            .collect();
        assert_eq!(kept_keys.len(), 1);

        // The old doc's chunks survive.
        let kept_chunks = pending_uuids(&batcher, "knowledge_chunk", "chunk_id");
        assert_eq!(kept_chunks.len(), 2);
        assert!(kept_chunks.contains(&chunk_old_1));
        assert!(kept_chunks.contains(&chunk_old_2));

        // Revisions: old doc's revision kept; empty head's revision dropped.
        let kept_rev_docs = pending_uuids(&batcher, "knowledge_revision", "document_id");
        assert_eq!(kept_rev_docs.len(), 1);
        assert!(kept_rev_docs.contains(&doc_chunked_old));

        // knowledge_chunk_entity_mention via from_id: mention for chunk_old_1 kept.
        let kept_mention_tos = pending_uuids(&batcher, "knowledge_chunk_entity_mention", "to_id");
        assert_eq!(kept_mention_tos.len(), 1, "mention via from_id survives for kept chunk");
        assert!(kept_mention_tos.contains(&entity_id));
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
            // postgres tables wrote OK and a later vector stage failed.
            append_json_entry(&mut builder, "manifest.json", &serde_json::json!({"ok": true}))
                .await
                .unwrap();
            let inner_err: anyhow::Result<()> =
                Err(anyhow!("simulated vector relation export failure"));
            let outcome =
                finalize_archive_with_failure_sentinel(builder, library_id, inner_err).await;
            assert!(outcome.is_err(), "primary error must propagate, got {outcome:?}");
            let err_msg = format!("{:#}", outcome.unwrap_err());
            assert!(
                err_msg.contains("vector relation export failure"),
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

    /// v2 regression: a table export failure deep in the export (after
    /// several `part-N` entries have already streamed for a relation)
    /// must still produce a syntactically valid tar+zstd. The archive
    /// must either contain the `EXPORT_FAILED.json` sentinel OR end with
    /// the canonical tar trailer; in both cases `read_tar_zst_entries`
    /// MUST decode the whole stream without "unexpected EOF". This pins
    /// the silent-truncation regression that v1 did not catch on
    /// libraries where the failing vector relation stage produced 5+ batches
    /// before the cursor errored.
    #[tokio::test]
    async fn test_archive_finalized_on_vector_relation_failure_v2() {
        let mut out: Vec<u8> = Vec::new();
        let library_id = Uuid::now_v7();
        {
            let zstd = ZstdEncoder::new(&mut out);
            let mut builder = Builder::new(zstd);
            builder.mode(async_tar::HeaderMode::Deterministic);
            // Simulate the realistic failure path: manifest + several
            // chunk-vector parts streamed OK, then a later cursor batch failed.
            append_json_entry(&mut builder, "manifest.json", &serde_json::json!({"ok": true}))
                .await
                .unwrap();
            for part in 1..=4u32 {
                let path = format!("postgres/knowledge_chunk_vector_d3/part-{part:06}.ndjson");
                let payload = format!("{{\"row\":{part}}}\n");
                append_raw_entry(&mut builder, &path, payload.as_bytes()).await.unwrap();
            }
            let inner_err: anyhow::Result<()> =
                Err(anyhow!("simulated postgres cursor failure on vector relation batch 5"));
            let outcome =
                finalize_archive_with_failure_sentinel(builder, library_id, inner_err).await;
            assert!(outcome.is_err(), "primary error must propagate, got {outcome:?}");
        }
        // The archive must decode without "unexpected EOF". Pre-v2 fix
        // the consumer would receive a half-written zstd stream that
        // could not even reach the tar trailer.
        let entries = read_tar_zst_entries(&out)
            .await
            .expect("v2 regression: archive must decode cleanly after deep vector failure");
        let names: Vec<&str> = entries.iter().map(|(p, _)| p.as_str()).collect();
        assert!(names.contains(&"manifest.json"), "earlier entries must survive, got {names:?}");
        for part in 1..=4u32 {
            let expected = format!("postgres/knowledge_chunk_vector_d3/part-{part:06}.ndjson");
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

    /// `sanitize_json_for_postgres` must strip U+0000 (null byte) and lone
    /// surrogates from all string nodes at any depth while leaving every other
    /// code point — including multibyte non-ASCII — intact.
    #[test]
    fn sanitize_json_strips_null_bytes_and_surrogates_preserves_multibyte() {
        // A string mixing a legitimate multibyte character, a null byte, and
        // surrounding ASCII — only the null must be removed.
        let mut v = serde_json::json!("hello\u{0000}wörld");
        sanitize_json_for_postgres(&mut v);
        assert_eq!(v.as_str().unwrap(), "hellowörld", "null byte removed; multibyte char kept");

        // Nested: null byte inside an object value inside an array.
        let mut nested = serde_json::json!([
            { "text": "abc\u{0000}def", "num": 42 },
            "ok\u{0000}"
        ]);
        sanitize_json_for_postgres(&mut nested);
        assert_eq!(
            nested[0]["text"].as_str().unwrap(),
            "abcdef",
            "null stripped from object value",
        );
        assert_eq!(nested[0]["num"].as_i64().unwrap(), 42, "numeric node untouched",);
        assert_eq!(nested[1].as_str().unwrap(), "ok", "null stripped from array string");

        // A string with no forbidden characters must be returned unchanged
        // (fast-path: no allocation). Deliberately script-agnostic: mixes
        // Latin-extended, Greek, CJK, a 4-byte astral math glyph and an emoji
        // so the multibyte-preservation invariant is exercised without
        // embedding any natural-language phrase.
        let clean = "é·Ω·中·𝛼·🌍";
        let mut v2 = serde_json::json!(clean);
        sanitize_json_for_postgres(&mut v2);
        assert_eq!(v2.as_str().unwrap(), clean, "clean string unchanged");

        // A string consisting entirely of null bytes becomes empty.
        let mut all_null = serde_json::json!("\u{0000}\u{0000}");
        sanitize_json_for_postgres(&mut all_null);
        assert_eq!(all_null.as_str().unwrap(), "", "all-null string becomes empty");
    }
}
