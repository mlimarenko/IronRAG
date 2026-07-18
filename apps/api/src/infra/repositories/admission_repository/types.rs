use serde_json::Value;
use sqlx::FromRow;
use thiserror::Error;
use uuid::Uuid;

use super::super::{content_repository, ingest_repository, ops_repository};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionFailpoint {
    AfterMutation,
    AfterDocument,
    AfterAsyncOperation,
    AfterRevision,
    AfterMutationItem,
    AfterJob,
    AfterHead,
    AfterWebRun,
    AfterWebSeed,
    BeforeCommit,
}

#[derive(Debug, Error)]
pub enum AdmissionError {
    #[error("admission request conflicts with an existing idempotency record")]
    IdempotencyConflict { mutation_id: Uuid },
    #[error("target document {document_id} no longer exists")]
    TargetDocumentNotFound { document_id: Uuid },
    #[error("target document {document_id} is deleted")]
    TargetDocumentDeleted { document_id: Uuid },
    #[error("target document {document_id} no longer belongs to the requested scope")]
    TargetDocumentScopeConflict { document_id: Uuid },
    #[error("target document {document_id} head references violate canonical ownership")]
    TargetDocumentHeadIntegrity { document_id: Uuid },
    #[error("legacy admission {mutation_id} is incomplete and cannot be repaired safely: {reason}")]
    IncompleteLegacy { mutation_id: Uuid, reason: &'static str },
    #[error("admission invariant failed: {0}")]
    InvariantViolation(&'static str),
    #[error("document {document_id} is still processing another mutation")]
    ConflictingActiveMutation { document_id: Uuid },
    #[error("an active document with this external key already exists: {existing_document_id}")]
    DuplicateExternalKey { existing_document_id: Uuid },
    #[error("admission failpoint triggered after {0:?}")]
    InjectedFailure(AdmissionFailpoint),
    #[error("admission database operation failed")]
    Database(#[from] sqlx::Error),
    #[error("admission request fingerprint could not be encoded")]
    FingerprintEncoding(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
pub enum AdmissionOutcome<T> {
    Created(T),
    Replayed(T),
    RepairedLegacy(T),
}

impl<T> AdmissionOutcome<T> {
    #[must_use]
    pub const fn bundle(&self) -> &T {
        match self {
            Self::Created(bundle) | Self::Replayed(bundle) | Self::RepairedLegacy(bundle) => bundle,
        }
    }

    #[must_use]
    pub fn into_bundle(self) -> T {
        match self {
            Self::Created(bundle) | Self::Replayed(bundle) | Self::RepairedLegacy(bundle) => bundle,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ContentAdmissionTarget {
    New {
        external_key: Option<String>,
        file_name: Option<String>,
        parent_external_key: Option<String>,
    },
    Existing {
        document_id: Uuid,
    },
}

#[derive(Debug, Clone)]
pub struct RevisionAdmission {
    pub content_source_kind: String,
    pub checksum: String,
    pub mime_type: String,
    pub byte_size: i64,
    pub title: Option<String>,
    pub language_code: Option<String>,
    pub source_uri: Option<String>,
    pub document_hint: Option<String>,
    pub storage_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ContentAdmissionRequest {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub operation_kind: String,
    pub requested_by_principal_id: Option<Uuid>,
    pub request_surface: String,
    pub idempotency_key: Option<String>,
    pub source_identity: Option<String>,
    pub target: ContentAdmissionTarget,
    pub revision: Option<RevisionAdmission>,
    pub parent_async_operation_id: Option<Uuid>,
    pub priority: i32,
}

#[derive(Debug, Clone)]
pub struct WebRunAdmissionRequest {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub seed_url: String,
    pub normalized_seed_url: String,
    pub mode: String,
    pub boundary_policy: String,
    pub max_depth: i32,
    pub max_pages: i32,
    pub crawl_allow_patterns: Value,
    pub crawl_block_patterns: Value,
    pub materialization_allow_patterns: Value,
    pub materialization_block_patterns: Value,
    pub requested_by_principal_id: Option<Uuid>,
    pub request_surface: String,
    pub idempotency_key: Option<String>,
    pub source_identity: Option<String>,
}

/// Existing canonical identities required to admit one fetched web revision
/// into the content-ingest queue. Document/revision creation deliberately
/// remains outside this short transaction; every lifecycle row that makes the
/// revision runnable is committed by the materialization admission `UoW`.
#[derive(Debug, Clone)]
pub struct WebCaptureMaterializationAdmissionRequest {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub mutation_id: Uuid,
    pub document_id: Uuid,
    pub revision_id: Uuid,
    pub requested_by_principal_id: Option<Uuid>,
    pub priority: i32,
}

#[derive(Debug, Clone, FromRow)]
pub struct AdmissionIngestJobRow {
    pub id: Uuid,
    pub mutation_id: Option<Uuid>,
    pub mutation_item_id: Option<Uuid>,
    pub async_operation_id: Option<Uuid>,
    pub knowledge_document_id: Option<Uuid>,
    pub knowledge_revision_id: Option<Uuid>,
    pub job_kind: String,
    pub dedupe_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ContentAdmissionBundle {
    pub mutation: content_repository::ContentMutationRow,
    pub document: content_repository::ContentDocumentRow,
    pub revision: Option<content_repository::ContentRevisionRow>,
    pub item: Option<content_repository::ContentMutationItemRow>,
    pub job: Option<AdmissionIngestJobRow>,
    pub async_operation: ops_repository::OpsAsyncOperationRow,
}

impl ContentAdmissionBundle {
    #[must_use]
    pub fn is_complete(&self) -> bool {
        if self.async_operation.subject_id != Some(self.mutation.id)
            || self.async_operation.subject_kind != "content_mutation"
        {
            return false;
        }
        match (&self.revision, &self.item, &self.job) {
            (None, Some(item), None) => {
                item.mutation_id == self.mutation.id
                    && item.document_id == Some(self.document.id)
                    && item.result_revision_id.is_none()
            }
            (Some(revision), Some(item), Some(job)) => {
                item.mutation_id == self.mutation.id
                    && item.document_id == Some(self.document.id)
                    && item.result_revision_id == Some(revision.id)
                    && job.mutation_id == Some(self.mutation.id)
                    && job.mutation_item_id == Some(item.id)
                    && job.async_operation_id == Some(self.async_operation.id)
                    && job.knowledge_document_id == Some(self.document.id)
                    && job.knowledge_revision_id == Some(revision.id)
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WebCaptureMaterializationAdmissionBundle {
    pub mutation: content_repository::ContentMutationRow,
    pub document: content_repository::ContentDocumentRow,
    pub revision: content_repository::ContentRevisionRow,
    pub item: content_repository::ContentMutationItemRow,
    pub job: ingest_repository::IngestJobRow,
    pub head: content_repository::ContentDocumentHeadRow,
}

impl WebCaptureMaterializationAdmissionBundle {
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.mutation.operation_kind == "web_capture"
            && self.document.workspace_id == self.mutation.workspace_id
            && self.document.library_id == self.mutation.library_id
            && self.revision.document_id == self.document.id
            && self.revision.workspace_id == self.document.workspace_id
            && self.revision.library_id == self.document.library_id
            && self.item.mutation_id == self.mutation.id
            && self.item.document_id == Some(self.document.id)
            && self.item.result_revision_id == Some(self.revision.id)
            && self.job.mutation_id == Some(self.mutation.id)
            && self.job.mutation_item_id == Some(self.item.id)
            && self.job.workspace_id == self.mutation.workspace_id
            && self.job.library_id == self.mutation.library_id
            && self.job.connector_id.is_none()
            && self.job.async_operation_id.is_none()
            && self.job.knowledge_document_id == Some(self.document.id)
            && self.job.knowledge_revision_id == Some(self.revision.id)
            && self.job.job_kind == "content_mutation"
            && self.head.document_id == self.document.id
    }
}

#[derive(Debug, Clone)]
pub struct WebRunAdmissionBundle {
    pub mutation: content_repository::ContentMutationRow,
    pub async_operation: ops_repository::OpsAsyncOperationRow,
    pub run: ingest_repository::WebIngestRunRow,
    pub seed: ingest_repository::WebDiscoveredPageRow,
    pub job: Option<AdmissionIngestJobRow>,
}

impl WebRunAdmissionBundle {
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.run.mutation_id == self.mutation.id
            && self.run.async_operation_id == Some(self.async_operation.id)
            && self.async_operation.subject_kind == "content_web_ingest_run"
            && self.async_operation.subject_id == Some(self.run.id)
            && self.seed.run_id == self.run.id
            && self.seed.normalized_url == self.run.normalized_seed_url
            && self.job.as_ref().is_some_and(|job| {
                job.mutation_id == Some(self.mutation.id)
                    && job.async_operation_id == Some(self.async_operation.id)
                    && job.job_kind == "web_discovery"
            })
    }
}

#[derive(Debug, Clone, FromRow)]
pub(super) struct MutationIdentityRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub operation_kind: String,
    pub requested_by_principal_id: Option<Uuid>,
    pub request_surface: String,
    pub idempotency_key: Option<String>,
    pub source_identity: Option<String>,
    pub mutation_state: String,
    pub requested_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub failure_code: Option<String>,
    pub conflict_code: Option<String>,
    pub request_fingerprint: Option<String>,
}

impl MutationIdentityRow {
    pub(super) fn into_row(self) -> content_repository::ContentMutationRow {
        content_repository::ContentMutationRow {
            id: self.id,
            workspace_id: self.workspace_id,
            library_id: self.library_id,
            operation_kind: self.operation_kind,
            requested_by_principal_id: self.requested_by_principal_id,
            request_surface: self.request_surface,
            idempotency_key: self.idempotency_key,
            source_identity: self.source_identity,
            mutation_state: self.mutation_state,
            requested_at: self.requested_at,
            completed_at: self.completed_at,
            failure_code: self.failure_code,
            conflict_code: self.conflict_code,
        }
    }
}
