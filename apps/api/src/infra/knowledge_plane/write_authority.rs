use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Formal identifier protocol for an unpromoted vector rebuild lane. The
/// suffix is a SHA-256 digest, not an operator/model alias.
pub const VECTOR_REBUILD_STAGING_PROFILE_PREFIX: &str = "embedding-rebuild:v1:";
pub const VECTOR_REBUILD_STAGING_PROFILE_DIGEST_LEN: usize = 64;
/// Cross-process serializer namespace for active vector-plane writes.
///
/// Ordinary writes take a transaction-scoped shared lock; destructive purge
/// and promotion take the exclusive form of the same per-library lock.
pub const VECTOR_PLANE_DATA_ADVISORY_LOCK_PREFIX: &str = "query.vector_plane.data.library";

/// Authority/profile fence for an active vector write.
///
/// Ordinary ingest writes are revision-scoped: the caller re-resolves the
/// current embedding profile before each write, while the leased attempt proves
/// authority over the exact revision. Library-global source generation remains
/// an exact compare-and-advance fence for direct operator and migration writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorWriteFence {
    pub expected_source_truth_version: i64,
    pub embedding_profile_key: String,
    /// Ordinary ingest writes are additionally authorized by the latest
    /// leased attempt for the exact revision. Direct operator writes do not
    /// have an ingest lifecycle and leave this unset.
    pub ingest_attempt: Option<IngestVectorWriteFence>,
    /// Direct operator/API writes advance the cross-replica cache fence.
    /// Ingest batches preserve it because one ingest attempt writes several
    /// batches under the same pre-provider source/profile snapshot.
    pub advance_source_truth_version: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestVectorWriteFence {
    pub attempt_id: Uuid,
    pub revision_id: Uuid,
}

/// Complete authority required by ingest-owned text mutations.
///
/// The workspace/library scope lets adapters acquire the parent catalog row
/// before locking the owning job and attempt. Holding those locks until commit
/// makes the authority check part of the mutation, rather than a racy
/// preflight check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestRevisionWriteAuthority {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub attempt_id: Uuid,
    pub revision_id: Uuid,
}

impl IngestRevisionWriteAuthority {
    #[must_use]
    pub const fn vector_fence(self) -> IngestVectorWriteFence {
        IngestVectorWriteFence { attempt_id: self.attempt_id, revision_id: self.revision_id }
    }
}

/// Result of a short ingest-owned mutation transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestAttemptMutationOutcome<T> {
    Applied(T),
    AuthorityLost,
}

/// Explicit extract-artifact replacement semantics for an ingest-owned state
/// transition. A successful extraction replaces every artifact field,
/// including clearing a previous image checksum with `None`. State-only
/// transitions (for example failure) preserve the last complete artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestRevisionExtractArtifact<'a> {
    StateOnly,
    Replace {
        normalized_text: &'a str,
        text_checksum: &'a str,
        text_readable_at: Option<DateTime<Utc>>,
        image_checksum: Option<&'a str>,
    },
}

/// Resolves the active embedding execution profile for a library.
///
/// Revision-scoped vector writes invoke this while holding the library row
/// lock, so an AI-binding mutation either commits before the profile check or
/// waits until the vector transaction commits.
#[async_trait]
pub trait ActiveEmbeddingProfileSource: Send + Sync {
    async fn active_embedding_profile_key(&self, library_id: Uuid) -> anyhow::Result<String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VectorPlaneDeleteOutcome {
    pub deleted: u64,
    pub source_truth_version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkVectorProfileInventory {
    pub dimensions: Vec<u64>,
    pub active_vector_count: u64,
}

/// Declares whether a lexical chunk query is the user's own text or a
/// retrieval probe derived from it. Only user text may activate the
/// document-identity score lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LexicalQueryOrigin {
    UserQuery,
    DerivedProbe,
}
