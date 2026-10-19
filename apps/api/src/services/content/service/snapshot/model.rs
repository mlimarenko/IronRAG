use super::*;

/// Prefix of every per-dim chunk-vector relation/shard.
pub(super) const PER_DIM_CHUNK_VECTOR_PREFIX: &str = "knowledge_chunk_vector_d";
/// Prefix of every per-dim entity-vector shard.
pub(super) const PER_DIM_ENTITY_VECTOR_PREFIX: &str = "knowledge_entity_vector_d";

/// Parse the dim suffix off a per-dim vector shard name.
/// Returns `None` when the name does not match the per-dim shape.
pub(super) fn parse_per_dim_vector_collection_dim(name: &str) -> Option<u64> {
    let suffix = name
        .strip_prefix(PER_DIM_CHUNK_VECTOR_PREFIX)
        .or_else(|| name.strip_prefix(PER_DIM_ENTITY_VECTOR_PREFIX))?;
    parse_per_dim_vector_suffix_dim(suffix)
}

pub(super) fn parse_per_dim_vector_suffix_dim(suffix: &str) -> Option<u64> {
    if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    suffix.parse::<u64>().ok()
}

pub(super) fn is_expected_per_dim_vector_collection_name(name: &str) -> bool {
    expected_per_dim_vector_relation_name(name).as_deref() == Some(name)
}

/// `true` when `name` is a per-dim chunk-vector shard
/// (`knowledge_chunk_vector_d<dim>`). Used to decide whether the restore path
/// should ensure a chunk-side vs entity-side relation.
pub(super) fn is_per_dim_chunk_vector_collection_name(name: &str) -> bool {
    name.strip_prefix(PER_DIM_CHUNK_VECTOR_PREFIX)
        .is_some_and(|suffix| parse_per_dim_vector_suffix_dim(suffix).is_some())
}

pub(super) fn expected_per_dim_vector_relation_name(name: &str) -> Option<String> {
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
///
/// Version 4 preserves revision admission identities of both origins and the
/// readable revision pointer independently of operational mutation/attempt history.
/// Older archives cannot prove this closure and are refused, not reconstructed.
pub const SNAPSHOT_SCHEMA_VERSION: u32 = 4;

/// Scope of a library snapshot.
///
/// A library is an atomic unit from the operator's point of view: its
/// documents, revisions, chunks, graph facts, knowledge entities and
/// relations all describe the same thing and are worthless without each
/// other. The exact scope keeps that domain model whole instead of
/// exposing persistence-tier fragments to operators.
///
/// The exact scope `LibraryData` therefore always includes every
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
    /// on another stack: `ai_provider_catalog`, `ai_model_catalog` (pricing
    /// inline on each row), `ai_model_binding_purpose`, `ai_account` (with
    /// `api_key` stripped), and `ai_binding`. Includes instance-scoped
    /// bindings as well as workspace/library-scoped ones, because deployments
    /// configure embed/answer bindings at instance scope. `iam_principal`
    /// author references are nulled (principals never travel in a snapshot),
    /// and import is non-destructive — `ON CONFLICT DO NOTHING` means an
    /// import only populates an empty target and never clobbers the
    /// deployment's existing AI configuration.
    AiConfig,
    /// Everything owned by a library that is NOT a raw source file —
    /// `PostgreSQL` rows for content, runtime graph, and knowledge data.
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

    /// Enforce dependency ordering. Blobs without `LibraryData` would
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
    /// identity. `PostgreSQL` rows are restored atomically; blob writes are staged
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
    /// Per-dim vector shards must exactly declare every corresponding relation
    /// in `postgres_tables`.
    pub vector_shards: Vec<VectorShardEntry>,
}

#[derive(Debug, Default, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SnapshotSummary {
    #[serde(flatten)]
    pub inventory: SnapshotInventory,
}

/// Why a snapshot import refused, in the vocabulary an operator acts on.
///
/// The async admission path answers `202` long before the restore runs, so the
/// only place a caller learns what happened is the operation record. A single
/// `snapshot_import_failed` there cannot distinguish "you asked me not to
/// overwrite" from "this archive is corrupt" from "the machine fell over", and
/// all three want different next steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotImportFailureKind {
    /// `overwrite=reject` and the target library already holds data.
    TargetPopulated,
    /// The rows this archive carries already exist in this deployment under
    /// another library. Snapshots keep their original primary keys, so the two
    /// copies cannot coexist.
    SourceAlreadyPresent,
    /// The archive declares a schema version this build does not speak.
    UnsupportedSchemaVersion,
    /// The archive is malformed, truncated, or marked as a failed export.
    ArchiveInvalid,
}

impl SnapshotImportFailureKind {
    /// Stable machine-readable code stored on the async operation.
    pub const fn code(self) -> &'static str {
        match self {
            Self::TargetPopulated => "snapshot_target_populated",
            Self::SourceAlreadyPresent => "snapshot_source_already_present",
            Self::UnsupportedSchemaVersion => "snapshot_unsupported_schema_version",
            Self::ArchiveInvalid => "snapshot_archive_invalid",
        }
    }
}

/// Code used when an import fails for a reason that is not one of the
/// classified refusals above — a genuine internal failure.
pub(super) const SNAPSHOT_IMPORT_INTERNAL_FAILURE_CODE: &str = "snapshot_import_failed";

/// A classified refusal raised by the restore path.
///
/// Carried through the `anyhow` chain so the HTTP worker can recover the kind
/// with `downcast_ref` and stamp the operation with a specific failure code
/// instead of a catch-all one.
#[derive(Debug)]
pub(super) struct SnapshotImportRejection {
    pub(super) kind: SnapshotImportFailureKind,
    pub(super) message: String,
    /// The underlying failure, kept as a source so `{:#}` still prints the
    /// database or IO error an operator needs, with the classified message in
    /// front of it rather than instead of it.
    pub(super) source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
}

impl SnapshotImportRejection {
    pub(super) fn new(kind: SnapshotImportFailureKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into(), source: None }
    }

    #[must_use]
    pub(super) fn with_source(
        mut self,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// Build the `anyhow::Error` the restore path returns for this refusal.
    pub(super) fn into_error(self) -> anyhow::Error {
        anyhow::Error::new(self)
    }
}

impl std::fmt::Display for SnapshotImportRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SnapshotImportRejection {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|source| &**source as &(dyn std::error::Error + 'static))
    }
}

/// A failed import, carrying the code the async operation should record.
///
/// The restore runs in a spawned task whose only channel back to the operator
/// is the operation row, so the failure code has to survive the trip out of the
/// service layer rather than being flattened into one catch-all string.
#[derive(Debug)]
pub struct SnapshotImportError {
    failure_code: &'static str,
    error: anyhow::Error,
}

impl SnapshotImportError {
    pub fn new(error: anyhow::Error) -> Self {
        Self { failure_code: snapshot_import_failure_code(&error), error }
    }

    pub const fn failure_code(&self) -> &'static str {
        self.failure_code
    }

    /// Full `anyhow` chain, for the log line that precedes the operation update.
    pub fn chain(&self) -> String {
        format!("{:#}", self.error)
    }
}

impl std::fmt::Display for SnapshotImportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:#}", self.error)
    }
}

impl std::error::Error for SnapshotImportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.error.source()
    }
}

impl From<SnapshotImportError> for ContentServiceError {
    fn from(failure: SnapshotImportError) -> Self {
        Self::from(failure.error)
    }
}

/// Classifies an otherwise-unclassified archive failure as a malformed archive.
///
/// Archive parsing raises dozens of distinct `ensure!`s; rather than restate a
/// kind at each one, the whole read is wrapped here. A refusal that already
/// carries a kind passes through unchanged.
pub(super) fn classify_snapshot_archive_error(error: anyhow::Error) -> anyhow::Error {
    if error.chain().any(|cause| cause.downcast_ref::<SnapshotImportRejection>().is_some()) {
        return error;
    }
    let message = format!("snapshot archive is not readable: {error:#}");
    SnapshotImportRejection::new(SnapshotImportFailureKind::ArchiveInvalid, message).into_error()
}

/// Recovers the classified refusal from an error chain, if there is one.
pub(super) fn snapshot_import_failure_code(error: &anyhow::Error) -> &'static str {
    error
        .chain()
        .find_map(|cause| {
            cause.downcast_ref::<SnapshotImportRejection>().map(|rejection| rejection.kind.code())
        })
        .unwrap_or(SNAPSHOT_IMPORT_INTERNAL_FAILURE_CODE)
}

#[derive(Debug, Default)]
pub struct SnapshotImportReport {
    pub library_id: Uuid,
    pub postgres_rows_by_table: Vec<(String, u64)>,
    pub blobs_restored: u64,
    /// Blobs whose bytes prevalidation proved against the digest a
    /// `content_revision.checksum` names (IRG-32).
    pub blobs_verified: u64,
    /// Blobs whose byte contract prevalidation could not prove. They are
    /// accepted, not refused — the check proves internal consistency, not
    /// authenticity.
    pub unverifiable_blobs: u64,
    pub overwrite_mode: OverwriteMode,
    pub include_kinds: Vec<IncludeKind>,
}
